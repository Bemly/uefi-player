//! 整机硬件测试：SIMD 支持度实测 + 屏幕/GOP/算力基准，跑完落盘。
//!
//! - SSE/AVX：CPUID 查位 + **真执行一条向量指令验算**（光看 CPUID 不叫测试）。
//!   AVX 执行前必须过 `check_avx_support`（硬件 + OSXSAVE + XCR0），否则 #UD。
//! - 屏幕刷新：GOP VideoFill / BufferToVideo 整屏帧率。
//! - GOP 速度：framebuffer 裸指针直写带宽。
//! - 算力：memcpy 带宽 + 整数流水线吞吐。
//! - 落盘：启动卷根目录写 `HWBENCHnn.TXT`（从 U 盘启动时就是 U 盘），
//!   文件名自动递增，连测多台机器不互相覆盖。
//!
//! x86_64 only：CPUID / RDTSC / SSE-AVX 内在函数。

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};
use core::time::Duration;

use uefi::boot::{self, get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::pi::mp::MpServices;

use crate::diag::avx_checker::check_avx_support;
use crate::diag::common::save_report;
use crate::emit;
use crate::error::Result;
use crate::graphics::Screen;

/// 双记一行：屏显（+串口）与报告正文各一份。
fn line(report: &mut Vec<String>, screen: &mut Screen, s: String) {
    screen.draw_str(&s);
    report.push(s);
}

macro_rules! tline {
    ($report:expr, $screen:expr, $($arg:tt)*) => {{
        let s = alloc::format!($($arg)*);
        log::info!("{}", s);
        $screen.draw_str(&s);
        $report.push(s);
    }};
}

// ---------- CPUID ----------
//
// 全指令集矩阵：leaf1（ECX/EDX）+ leaf7（EBX/ECX/EDX，子叶0）
// + leaf7 子叶1（EAX）+ 扩展叶 0x80000001（ECX/EDX）。
// 只收 SIMD/向量/密码/位操纵相关位，控制流（CET/INVPCID 等）不管。
// 位号依据 Intel SDM Vol.2A CPUID 与 AMD APM Vol.3 附录。
// 读 7 号叶/扩展叶前用最大叶号门控，老 CPU 直接读会回落误报。

/// 分组展示用的 (名字, 是否支持) 表，按叶分组、组内按位号排序。
struct SimdCpuid {
    sse2: bool,
    avx2_hw: bool,
    avx512f_hw: bool,
    osxsave: bool,
    /// 有 7 号叶（可以信任下面的 leaf7_*）。
    has_leaf7: bool,
    /// 有扩展叶（可以信任下面的 ext_*）。
    has_ext: bool,
    leaf7_ebx: u32,
    leaf7_ecx: u32,
    leaf7_edx: u32,
    leaf7s1_eax: u32,
    has_leaf7s1: bool,
    ext_ecx: u32,
    ext_edx: u32,
    leaf1_ecx: u32,
    leaf1_edx: u32,
}

fn cpuid_simd() -> SimdCpuid {
    // __cpuid 在新版 Rust 里是 safe fn，无需 unsafe。
    let c1 = __cpuid(1);
    let max_basic = crate::diag::common::cpuid_max_basic();
    let max_ext = crate::diag::common::cpuid_max_extended();
    let has_leaf7 = max_basic >= 7;
    let has_ext = max_ext >= 0x8000_0001;
    let (mut b7, mut c7, mut d7) = (0, 0, 0);
    if has_leaf7 {
        let r = __cpuid_count(7, 0);
        b7 = r.ebx;
        c7 = r.ecx;
        d7 = r.edx;
    }
    // 子叶 1 是否存在看 7 号叶 EAX（最大子叶号）。
    let has_leaf7s1 = has_leaf7 && __cpuid_count(7, 0).eax >= 1;
    let mut s1eax = 0;
    if has_leaf7s1 {
        s1eax = __cpuid_count(7, 1).eax;
    }
    let (mut xc, mut xd) = (0, 0);
    if has_ext {
        let r = __cpuid(0x8000_0001);
        xc = r.ecx;
        xd = r.edx;
    }
    SimdCpuid {
        sse2: c1.edx & (1 << 26) != 0,
        avx2_hw: b7 & (1 << 5) != 0,
        avx512f_hw: b7 & (1 << 16) != 0,
        osxsave: c1.ecx & (1 << 27) != 0,
        has_leaf7,
        has_ext,
        leaf7_ebx: b7,
        leaf7_ecx: c7,
        leaf7_edx: d7,
        leaf7s1_eax: s1eax,
        has_leaf7s1,
        ext_ecx: xc,
        ext_edx: xd,
        leaf1_ecx: c1.ecx,
        leaf1_edx: c1.edx,
    }
}

/// 按叶分组打印。叶不存在时整组标 N/A，不逐位误报。
fn print_group(
    report: &mut Vec<String>,
    screen: &mut Screen,
    title: &str,
    ok: bool,
    regs: u32,
    bits: &[(u8, &'static str)],
) {
    if !ok {
        tline!(report, screen, "-- {}: N/A (leaf missing) --", title);
        return;
    }
    tline!(report, screen, "-- {} --", title);
    let mut line = String::new();
    for (i, (bit, name)) in bits.iter().enumerate() {
        let on = regs & (1 << bit) != 0;
        line.push_str(&format!("{}:{} ", name, yn(on)));
        // 屏显是等宽点阵，一行塞太多会换行难看，4 个一组。
        if i % 4 == 3 {
            tline!(report, screen, "  {}", line.trim_end());
            line.clear();
        }
    }
    if !line.is_empty() {
        tline!(report, screen, "  {}", line.trim_end());
    }
}

fn yn(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

// ---------- 功能实测 ----------

/// SSE2 整数加法实测。
///
/// 约束（全是反汇编逼出来的）：
/// 1. 只测整数向量：浮点向量在此 target 上曾经全军覆没（标量化/错常量），
///    而 `.cargo/config.toml` 整 crate 摘软浮点后整数路径已验证发射；
///    浮点执行不在本测试覆盖内（CPUID+XCR0 矩阵照常报告支持情况）。
/// 2. 加法本身必须用 `asm!` 显式 `xmm_reg`：内在函数版的 load/add/store
///    会被 LLVM 整链标量化（二进制零条向量指令——整数得假 PASS）。
///    内在函数只配构造操作数（构造路径怎么编都无所谓）。
/// 3. `#[target_feature]` 保留做双保险（compile-time 门控 + 文档作用），
///    真正的 ABI 前提由 `.cargo/config.toml` 的 crate 级 flags 保证。
#[target_feature(enable = "sse2")]
unsafe fn sse_int_add_test(seed: u32) -> bool {
    // edition 2024：unsafe fn 体内也要显式 unsafe 块。
    unsafe {
        // lane 值运行时派生；u128 纯整数类型不进向量 legalize
        //（__m128i 会在 asm 约束处触发 getCopyFromParts 后端错误），
        // paddd 只认 bit pattern，lane 划分靠移位。
        let s = seed;
        let a: u128 = (s as u128)
            | ((s.wrapping_add(1) as u128) << 32)
            | ((s.wrapping_add(2) as u128) << 64)
            | ((s.wrapping_add(3) as u128) << 96);
        let b: u128 = 10 | (20u128 << 32) | (30u128 << 64) | (40u128 << 96);
        let mut acc = a;
        // Intel 语法：paddd acc, rhs  <=>  acc += rhs（lane 级 wrapping，与 wrapping_add 同语义）。
        asm!(
            "paddd {acc}, {rhs}",
            acc = inlateout(xmm_reg) acc,
            rhs = in(xmm_reg) b,
            options(pure, nomem, nostack, preserves_flags),
        );
        let want: u128 = ((s as i32).wrapping_add(10) as u32 as u128)
            | (((s.wrapping_add(1) as i32).wrapping_add(20) as u32 as u128) << 32)
            | (((s.wrapping_add(2) as i32).wrapping_add(30) as u32 as u128) << 64)
            | (((s.wrapping_add(3) as i32).wrapping_add(40) as u32 as u128) << 96);
        acc == want
    }
}

/// AVX2 整数加法实测（256 位整数要 AVX2，调用前必须同时满足
/// `check_avx_support()` 与 CPUID.07h:EBX.AVX2）。约束同 SSE 版。
#[target_feature(enable = "avx2")]
unsafe fn avx2_int_add_test(seed: u32) -> bool {
    use core::arch::x86_64::{_mm256_set_epi32, _mm256_storeu_si256};
    unsafe {
        let a = _mm256_set_epi32(
            seed.wrapping_add(7) as i32,
            seed.wrapping_add(6) as i32,
            seed.wrapping_add(5) as i32,
            seed.wrapping_add(4) as i32,
            seed.wrapping_add(3) as i32,
            seed.wrapping_add(2) as i32,
            seed.wrapping_add(1) as i32,
            seed as i32,
        );
        let b = _mm256_set_epi32(8, 7, 6, 5, 4, 3, 2, 1);
        let mut acc = a;
        // VEX 3 操作数：vpaddd acc, lhs, rhs  <=>  acc = lhs + rhs。
        asm!(
            "vpaddd {acc}, {lhs}, {rhs}",
            acc = inlateout(ymm_reg) acc,
            lhs = in(ymm_reg) a,
            rhs = in(ymm_reg) b,
            options(pure, nomem, nostack, preserves_flags),
        );
        let mut out = [0i32; 8];
        _mm256_storeu_si256(out.as_mut_ptr() as *mut _, acc);
        let s = seed;
        out == [
            (s as i32).wrapping_add(1),
            (s.wrapping_add(1) as i32).wrapping_add(2),
            (s.wrapping_add(2) as i32).wrapping_add(3),
            (s.wrapping_add(3) as i32).wrapping_add(4),
            (s.wrapping_add(4) as i32).wrapping_add(5),
            (s.wrapping_add(5) as i32).wrapping_add(6),
            (s.wrapping_add(6) as i32).wrapping_add(7),
            (s.wrapping_add(7) as i32).wrapping_add(8),
        ]
    }
}

/// AVX512 整数加法实测（512 位整数要 AVX512F，调用前必须同时满足
/// 硬件位 + `check_avx_support()` + XCR0 的 SSE/AVX/OPMASK/ZMM 五位全开，
/// 缺一个执行就 #UD）。约束同 SSE 版（asm! 显式 zmm_reg 防标量化）。
#[target_feature(enable = "avx512f")]
unsafe fn avx512_int_add_test(seed: u32) -> bool {
    use core::arch::x86_64::{_mm512_set1_epi32, _mm512_storeu_si512};
    unsafe {
        let a = _mm512_set1_epi32(seed as i32);
        let b = _mm512_set1_epi32(100);
        let mut x = a;
        let y = b;
        // EVEX 全掩码：vpaddd zmm, zmm, zmm  <=>  16 lane 相加（wrapping）。
        asm!(
            "vpaddd {x}, {x}, {y}",
            x = inlateout(zmm_reg) x,
            y = in(zmm_reg) y,
            options(pure, nomem, nostack, preserves_flags),
        );
        let mut out = [0i32; 16];
        _mm512_storeu_si512(out.as_mut_ptr() as *mut _, x);
        out.iter().all(|&v| v == (seed as i32).wrapping_add(100))
    }
}

// ---------- 计时 ----------

#[inline(always)]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// 100ms Stall 校准 TSC 频率。QEMU/真机都可用，TCG 下测出来偏小但自洽。
fn calibrate_ticks_per_sec() -> u64 {
    boot::stall(Duration::from_millis(100));
    let s = rdtsc();
    boot::stall(Duration::from_millis(100));
    let e = rdtsc();
    (e - s) * 10
}

// ---------- 主入口 ----------

pub fn run(screen: &mut Screen) -> Result {
    let mut report: Vec<String> = Vec::new();

    tline!(report, screen, "== HW Bench ==");
    tline!(report, screen, "target: x86_64-uefi, opt: release");

    // ---- CPU 基本信息 ----
    let cpuid = raw_cpuid::CpuId::new();
    if let Some(brand) = cpuid.get_processor_brand_string() {
        let s = format!("cpu: {}", brand.as_str().trim());
        line(&mut report, screen, s.clone());
        log::info!("{}", s);
    }
    if let Ok(h) = get_handle_for_protocol::<MpServices>() {
        if let Ok(mp) = open_protocol_exclusive::<MpServices>(h) {
            if let Ok(n) = mp.get_number_of_processors() {
                tline!(
                    report,
                    screen,
                    "cores: total {} enabled {}",
                    n.total,
                    n.enabled
                );
            }
        }
    }

    // ---- XSAVE 预置：固件常不开 CR4.OSXSAVE，会让 AVX 系全程 SKIP ----
    // OVMF/EDK2 与多台真机固件实测都不动 CR4.OSXSAVE（=0），而
    // CPUID.1:ECX.OSXSAVE 是该位的镜像，于是 `check_avx_support` 门控恒假、
    // AVX/AVX2/AVX512 行全 SKIP。这里报告**开机采到的固件原状态**（未被
    // 工具污染），再按需自行开启（CPL0 可直接写 CR4/XCR0）。
    let fw_osxsave = crate::diag::common::firmware_osxsave();
    crate::diag::common::ensure_xsave_enabled();
    tline!(
        report,
        screen,
        "xsave: firmware OSXSAVE={} | now={} (tool sets CR4.OSXSAVE+XCR0 when firmware left it off)",
        match fw_osxsave {
            Some(true) => "on",
            Some(false) => "off",
            None => "unk",
        },
        if crate::diag::common::osxsave_on() {
            "on"
        } else {
            "off"
        }
    );

    // ---- SIMD 支持度（全指令集矩阵） ----
    let f = cpuid_simd();
    tline!(report, screen, "-- SIMD cpuid --");
    print_group(
        &mut report,
        screen,
        "leaf1 ECX",
        true,
        f.leaf1_ecx,
        &[
            (0, "SSE3"),
            (1, "PCLMULQDQ"),
            (9, "SSSE3"),
            (12, "FMA"),
            (13, "CMPXCHG16B"),
            (19, "SSE4.1"),
            (20, "SSE4.2"),
            (22, "MOVBE"),
            (23, "POPCNT"),
            (25, "AES-NI"),
            (26, "XSAVE"),
            (27, "OSXSAVE"),
            (28, "AVX"),
            (29, "F16C"),
            (30, "RDRAND"),
        ],
    );
    print_group(
        &mut report,
        screen,
        "leaf1 EDX",
        true,
        f.leaf1_edx,
        &[(23, "MMX"), (24, "FXSR"), (25, "SSE"), (26, "SSE2")],
    );
    print_group(
        &mut report,
        screen,
        "leaf7 EBX",
        f.has_leaf7,
        f.leaf7_ebx,
        &[
            (0, "FSGSBASE"),
            (3, "BMI1"),
            (5, "AVX2"),
            (8, "BMI2"),
            (16, "AVX512F"),
            (17, "AVX512DQ"),
            (18, "RDSEED"),
            (19, "ADX"),
            (20, "SMAP"),
            (21, "AVX512IFMA"),
            (23, "CLFLUSHOPT"),
            (26, "AVX512PF"),
            (27, "AVX512ER"),
            (28, "AVX512CD"),
            (29, "SHA"),
            (30, "AVX512BW"),
            (31, "AVX512VL"),
        ],
    );
    print_group(
        &mut report,
        screen,
        "leaf7 ECX",
        f.has_leaf7,
        f.leaf7_ecx,
        &[
            (1, "AVX512VBMI"),
            (8, "GFNI"),
            (9, "VAES"),
            (10, "VPCLMULQDQ"),
            (11, "AVX512VNNI"),
            (12, "AVX512BITALG"),
            (14, "AVX512VPOPCNTDQ"),
        ],
    );
    print_group(
        &mut report,
        screen,
        "leaf7 EDX",
        f.has_leaf7,
        f.leaf7_edx,
        &[
            (2, "AVX5124VNNIW"),
            (3, "AVX5124FMAPS"),
            (8, "AVX512VP2INTERSECT"),
            (22, "AMX-BF16"),
            (23, "AVX512-FP16"),
            (24, "AMX-TILE"),
            (25, "AMX-INT8"),
        ],
    );
    print_group(
        &mut report,
        screen,
        "leaf7.1 EAX",
        f.has_leaf7s1,
        f.leaf7s1_eax,
        &[(4, "AVX-VNNI"), (5, "AVX512-BF16"), (23, "AVX-IFMA")],
    );
    print_group(
        &mut report,
        screen,
        "ext 80000001h ECX",
        f.has_ext,
        f.ext_ecx,
        &[
            (5, "LZCNT"),
            (6, "SSE4a"),
            (8, "PREFETCHW"),
            (16, "FMA4"),
            (21, "TBM"),
        ],
    );
    print_group(
        &mut report,
        screen,
        "ext 80000001h EDX",
        f.has_ext,
        f.ext_edx,
        &[
            (22, "MMXEXT"),
            (25, "FFXSR"),
            (30, "3DNOWEXT"),
            (31, "3DNOW"),
        ],
    );

    // XCR0：只有 OSXSAVE 开了才敢读，否则 #UD。
    // 除 SSE/AVX 状态位外再看 OPMASK/ZMM 位——AVX512 能否真跑，
    // 硬件位只是必要条件，XCR0 没开对应状态执行就 #UD。
    let mut xcr0: u64 = 0;
    if f.osxsave {
        xcr0 = unsafe { _xgetbv(0) };
        tline!(
            report,
            screen,
            "xcr0: SSE={} AVX={} OPMASK={} ZMM_Hi256={} Hi16_ZMM={} XCR0={:#x}",
            yn(xcr0 & (1 << 1) != 0),
            yn(xcr0 & (1 << 2) != 0),
            yn(xcr0 & (1 << 5) != 0),
            yn(xcr0 & (1 << 6) != 0),
            yn(xcr0 & (1 << 7) != 0),
            xcr0
        );
    } else {
        tline!(report, screen, "xcr0: SKIPPED (no OSXSAVE)");
    }
    // AVX512 可用判定：硬件位 + XCR0 五位全开，缺一个都 #UD。
    const ZMM_MASK: u64 = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 7);
    let avx512_usable = f.avx512f_hw && f.osxsave && xcr0 & ZMM_MASK == ZMM_MASK;
    tline!(
        report,
        screen,
        "AVX512-usable: {}",
        if avx512_usable { "YES" } else { "NO" }
    );

    // ---- SIMD 功能实测 ----
    tline!(report, screen, "-- SIMD exec test --");
    // ---- SIMD 功能实测（纯整数向量；浮点向量在此 target 上必被软浮点化，不测） ----
    tline!(
        report,
        screen,
        "  SSE2 int add : {}",
        if f.sse2 && unsafe { sse_int_add_test(rdtsc() as u32) } {
            "PASS"
        } else {
            "FAIL/SKIP"
        }
    );
    if check_avx_support() && f.avx2_hw {
        let ok = unsafe { avx2_int_add_test(rdtsc() as u32) };
        tline!(
            report,
            screen,
            "  AVX2 int add : {}",
            if ok { "PASS" } else { "FAIL" }
        );
    } else {
        tline!(
            report,
            screen,
            "  AVX2 int add : SKIP (need AVX+XCR0+AVX2-hw)"
        );
    }
    if avx512_usable {
        let ok = unsafe { avx512_int_add_test(rdtsc() as u32) };
        tline!(
            report,
            screen,
            "  AVX512 int add: {}",
            if ok { "PASS" } else { "FAIL" }
        );
    } else {
        tline!(
            report,
            screen,
            "  AVX512 int add: SKIP (need AVX512F+XCR0-ZMM)"
        );
    }

    // ---- 基准 ----
    let tps = calibrate_ticks_per_sec();
    tline!(report, screen, "tsc: {} ticks/sec", tps);

    // ---- GOP 压测（单/多核、带宽、瓶颈归因，见 gop_bench） ----
    crate::diag::gop_bench::run_section(&mut report, screen, tps)?;

    // ---- 内存拷贝带宽 ----
    // 注意：src/dst 固定时 LLVM 会把 20 次折叠成 1 次（曾测出 96 万 MB/s 的笑话），
    // 每轮 black_box 指针逼真干 20 次。
    const MEM_N: u64 = 20;
    const MEM_LEN: usize = 4 * 1024 * 1024;
    let src = vec![0xA5u8; MEM_LEN];
    let mut dst = vec![0u8; MEM_LEN];
    let t0 = rdtsc();
    for _ in 0..MEM_N {
        let s = core::hint::black_box(src.as_ptr());
        let d = core::hint::black_box(dst.as_mut_ptr());
        unsafe {
            core::ptr::copy_nonoverlapping(s, d, MEM_LEN);
        }
    }
    let dt = rdtsc() - t0;
    let bytes = MEM_N * MEM_LEN as u64;
    tline!(
        report,
        screen,
        "memcpy: {:.1} MB/s (chk={})",
        bytes as f64 * tps as f64 / (dt as f64 * 1024.0 * 1024.0),
        dst[MEM_LEN - 1]
    );

    // ---- 整数流水线 ----
    const OPS: u64 = 20_000_000;
    let mut acc: u64 = 0x1234_5678;
    let t0 = rdtsc();
    for i in 0..OPS {
        acc = acc.wrapping_add(i).wrapping_mul(0x9E37_79B9);
    }
    let dt = rdtsc() - t0;
    core::hint::black_box(acc);
    tline!(
        report,
        screen,
        "int ALU: {:.1} Mops/s (chk={:#x})",
        OPS as f64 * tps as f64 / (dt as f64 * 1_000_000.0),
        acc & 0xFFFF
    );

    // ---- 落盘：文件名自动递增（HWBENCH00.TXT、HWBENCH01.TXT…），
    // 连测多台机器不互相覆盖 ----
    let body = report.join("\n") + "\n";
    match save_report("HWBENCH", body.as_bytes()) {
        Ok(s) => tline!(report, screen, "{}", s),
        Err(e) => tline!(report, screen, "SAVE FAILED: {:?}", e),
    }

    emit!(screen, "-- bench done --");
    Ok(())
}
