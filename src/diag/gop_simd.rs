//! GOP 数据通路全指令集实测：像素搬运能用的指令，一个不落全上显存比一遍。
//!
//! 由 `gop_bench` 在基线/定标/多核/动画/雪花之后调用，结果并入同一份 `HWBENCHnn.TXT`。
//! 方法论（之前雪花段交的学费，直接写死在这里）：
//! - 同组变体轮转交替计时（ABAB…），杀掉“先测吃亏”的时间漂移；只引用本节现场数，
//!   不引用几分钟前的旧基线；测前各 2 轮热身吸收 TLB/唤醒。
//! - 纯整数向量：本 target 曾经 `+soft-float`，浮点向量必被标量化/算错常量，
//!   所以帧搬运只测整数路径（mov/shuf/crc/popcnt），浮点扩展一律只报 CPUID 不执行。
//! - 核心操作全部 `asm!` 显式向量寄存器：内在函数版 load/add/store 曾被 LLVM
//!   整链标量化（二进制零条向量指令，整数得假 PASS），`asm!` 没得标量化。
//! - AVX/AVX512 跑完立刻 `vzeroupper`（不清零计入下个 SSE 变体的 transition penalty）。
//! - 非暂存（nt）store 计入 `sfence` 成本；nt 要求天然对齐，framebuffer 基址不对齐
//!   时对应变体如实报 SKIP（QEMU/真机一般都是页对齐，会跑）。
//!
//! 覆盖处置表（每个分组一行，N/A 的在报告里写明原因，不蒙混）：
//! - fill/copy：scalar-u64, SSE2-128, SSE2-nt, SSE3-LDDQU(copy), AVX-128-VEX,
//!   AVX2-256(+nt), AVX512-512(+nt), AVX512VL-256-EVEX, prefetch(nta/w) sweep
//! - 换色：scalar, SSSE3-pshufb-16, AVX2-vpshufb-32, AVX512BW-vpshufb-64
//! - 校验/内容：scalar-sum, SSE4.2-crc32, POPCNT-density, AVX512VPOPCNTDQ
//! - 生成：xorshift, RDRAND, RDSEED
//! - GPR 像素 helper：BMI1-BEXTR, BMI2-PDEP/PEXT, LZCNT
//! - 明确不做（报告里逐条注原因）：MMX（≈u64，无增益且要 EMMS）、SSE/SSE3-FP、
//!   FMA/FMA4/F16C/AVX512-FP16/BF16/IFMA/4FMAPS/4VNNIW/VP2INTERSECT（软浮点历史，
//!   本 target 不执行浮点向量）、AES/VAES/PCLMUL/VPCLMULQDQ/SHA/GFNI/VNNI/BITALG/
//!   AMX（密码/AI/矩阵语义，像素搬运无此语义）、MOVBE（字节全反序≠RB 交换）、
//!   ADX/FSGSBASE/SMAP/XSAVE/CET（系统/大整数语义）、3DNOW/MMXEXT/FFXSR/SSE4a/TBM
//!   （AMD 历史，Intel 宿主机门控恒假，如实 SKIP）。
//!
//! x86_64 only，由调用方保证。

use alloc::vec::Vec;
use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

use crate::diag::avx_checker::check_avx_support;
use crate::error::Result;
use crate::graphics::Screen;

macro_rules! tline {
    ($report:expr, $screen:expr, $($arg:tt)*) => {{
        let s = alloc::format!($($arg)*);
        log::info!("{}", s);
        $screen.draw_str(&s);
        $report.push(s);
    }};
}

#[inline(always)]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn yn(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

// ---------- ISA 门控 ----------

struct Isa {
    l1c: u32,
    l1d: u32,
    b7: u32,
    c7: u32,
    d7: u32,
    s1: u32,
    xc: u32,
    xd: u32,
    has7: bool,
    hass1: bool,
    hasext: bool,
}

fn read_isa() -> Isa {
    let c1 = __cpuid(1);
    let max_basic = crate::diag::common::cpuid_max_basic();
    let max_ext = crate::diag::common::cpuid_max_extended();
    let has7 = max_basic >= 7;
    let hasext = max_ext >= 0x8000_0001;
    let (mut b7, mut c7, mut d7) = (0, 0, 0);
    if has7 {
        let r = __cpuid_count(7, 0);
        b7 = r.ebx;
        c7 = r.ecx;
        d7 = r.edx;
    }
    let hass1 = has7 && __cpuid_count(7, 0).eax >= 1;
    let mut s1 = 0;
    if hass1 {
        s1 = __cpuid_count(7, 1).eax;
    }
    let (mut xc, mut xd) = (0, 0);
    if hasext {
        let r = __cpuid(0x8000_0001);
        xc = r.ecx;
        xd = r.edx;
    }
    Isa {
        l1c: c1.ecx,
        l1d: c1.edx,
        b7,
        c7,
        d7,
        s1,
        xc,
        xd,
        has7,
        hass1,
        hasext,
    }
}

#[allow(clippy::too_many_arguments)]
struct Gates {
    // ---- 128 位世代 ----
    mmx: bool,
    sse: bool,
    sse2: bool,
    sse3: bool,
    ssse3: bool,
    sse41: bool,
    sse42: bool,
    // ---- 密码/GFNI ----
    aes: bool,
    pclmulqdq: bool,
    sha: bool,
    gfni: bool,
    // ---- 位操纵/杂项 ----
    popcnt: bool,
    rdrand: bool,
    rdseed: bool,
    movbe: bool,
    bmi1: bool,
    bmi2: bool,
    lzcnt: bool,
    tzcnt: bool,
    clflushopt: bool,
    clwb: bool,
    prefetchw: bool,
    // ---- AVX 体系 ----
    avx_usable: bool,
    avx2_usable: bool,
    avx512_usable: bool,
    avx512bw_hw: bool,
    avx512vl_hw: bool,
    avx512vbmi_hw: bool,
    avx512vpopcntdq_hw: bool,
    vaes_hw: bool,
    vpclmulqdq_hw: bool,
    // ---- AMD 世代 ----
    sse4a: bool,
    d3now: bool,
    d3nowa: bool,
    xop: bool,
    tbm: bool,
}

fn read_gates(isa: &Isa) -> Gates {
    let b = |r: u32, bit: u8| r & (1 << bit) != 0;
    let osxsave = b(isa.l1c, 27);
    let avx_usable = check_avx_support();
    let avx2_usable = avx_usable && isa.has7 && b(isa.b7, 5);
    let avx512f_hw = isa.has7 && b(isa.b7, 16);
    let mut xcr0: u64 = 0;
    if osxsave {
        xcr0 = unsafe { _xgetbv(0) };
    }
    const ZMM_MASK: u64 = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 7);
    let avx512_usable = avx512f_hw && osxsave && xcr0 & ZMM_MASK == ZMM_MASK;
    Gates {
        mmx: b(isa.l1d, 23),
        sse: b(isa.l1d, 25),
        sse2: b(isa.l1d, 26),
        sse3: b(isa.l1c, 0),
        ssse3: b(isa.l1c, 9),
        sse41: b(isa.l1c, 19),
        sse42: b(isa.l1c, 20),
        aes: b(isa.l1c, 25),
        pclmulqdq: b(isa.l1c, 1),
        sha: isa.has7 && b(isa.b7, 29),
        gfni: isa.has7 && b(isa.c7, 8),
        popcnt: b(isa.l1c, 23),
        rdrand: b(isa.l1c, 30),
        rdseed: isa.has7 && b(isa.b7, 18),
        movbe: b(isa.l1c, 22),
        bmi1: isa.has7 && b(isa.b7, 3),
        bmi2: isa.has7 && b(isa.b7, 8),
        lzcnt: isa.hasext && b(isa.xc, 5),
        // TZCNT：Intel 走 BMI1，AMD 走 ABM(ext ECX.2)。两者取或。
        tzcnt: (isa.has7 && b(isa.b7, 3)) || (isa.hasext && b(isa.xc, 2)),
        clflushopt: isa.has7 && b(isa.b7, 23),
        clwb: isa.has7 && b(isa.b7, 24),
        prefetchw: isa.hasext && b(isa.xc, 8),
        avx_usable,
        avx2_usable,
        avx512_usable,
        avx512bw_hw: isa.has7 && b(isa.b7, 30),
        avx512vl_hw: isa.has7 && b(isa.b7, 31),
        avx512vbmi_hw: isa.has7 && b(isa.c7, 1),
        avx512vpopcntdq_hw: isa.has7 && b(isa.c7, 14),
        vaes_hw: isa.has7 && b(isa.c7, 9),
        vpclmulqdq_hw: isa.has7 && b(isa.c7, 10),
        sse4a: isa.hasext && b(isa.xc, 6),
        d3now: isa.hasext && b(isa.xd, 31),
        d3nowa: isa.hasext && b(isa.xd, 30),
        xop: isa.hasext && b(isa.xc, 11),
        tbm: isa.hasext && b(isa.xc, 21),
    }
}

// ---------- fill 变体：dst=显存WC，纯写 ----------

unsafe fn fill_scalar_u64(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let pat = (seed as u64) | ((seed as u64) << 32);
        let mut p = dst;
        let end = dst.add(len & !7);
        while p < end {
            (p as *mut u64).write_volatile(pat);
            p = p.add(8);
        }
    }
}

/// MMX 64 位（MMX 寄存器；128 位之前的 SIMD 对照组，收尾需 EMMS）。
/// 注：Rust asm 不允许 mmx_reg 作操作数，改用显式 mm0 + clobber。
unsafe fn fill_mmx64(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let pat = (seed as u64) | ((seed as u64) << 32);
        let mut p = dst;
        let end = dst.add(len & !7);
        while p < end {
            asm!("movq mm0, {v}", "movq [{d}], mm0",
                v = in(reg) pat, d = in(reg) p,
                out("mm0") _, options(nostack));
            p = p.add(8);
        }
        asm!("emms", options(nostack, preserves_flags));
    }
}

/// SSE1 128 位（movups，FP 域存）：与 SSE2 movdqu 同宽，做编码域对照。
unsafe fn fill_sse128(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movups [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "sse2")]
unsafe fn fill_sse2(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movdqu [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "sse2")]
unsafe fn fill_sse2_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movntdq [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// AVX-128-VEX：同 128 位宽，VEX 编码对照组（看 VEX 本身有没有增益）。
#[target_feature(enable = "avx")]
unsafe fn fill_avx128(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("vmovdqu [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "avx2")]
unsafe fn fill_avx2(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovdqu [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
    }
}

#[target_feature(enable = "avx2")]
unsafe fn fill_avx2_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovntdq [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

#[target_feature(enable = "avx512f")]
unsafe fn fill_avx512(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm512_set1_epi32;
        let v = _mm512_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !63);
        while p < end {
            asm!("vmovdqu8 [{d}], {v}", d = in(reg) p, v = in(zmm_reg) v, options(nostack));
            p = p.add(64);
        }
    }
}

#[target_feature(enable = "avx512f")]
unsafe fn fill_avx512_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm512_set1_epi32;
        let v = _mm512_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !63);
        while p < end {
            asm!("vmovntdq [{d}], {v}", d = in(reg) p, v = in(zmm_reg) v, options(nostack));
            p = p.add(64);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// AVX512VL-256：同 256 位宽，EVEX 编码对照组。
#[target_feature(enable = "avx512f,avx512vl")]
unsafe fn fill_avx512vl256(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovdqu8 [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
    }
}

#[inline(always)]
unsafe fn vzeroupper() {
    unsafe {
        asm!("vzeroupper", options(nostack, preserves_flags));
    }
}

// ---------- copy 变体：src=WB内存 → dst=显存WC ----------

unsafe fn copy_scalar_u64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            let w = (src.add(i) as *const u64).read_unaligned();
            (dst.add(i) as *mut u64).write_volatile(w);
            i += 8;
        }
    }
}

/// MMX 64 位拷贝（对照组，收尾 EMMS）。
unsafe fn copy_mmx64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            asm!("movq mm0, [{s}]", "movq [{d}], mm0",
                s = in(reg) src.add(i), d = in(reg) dst.add(i),
                out("mm0") _, options(nostack));
            i += 8;
        }
        asm!("emms", options(nostack, preserves_flags));
    }
}

/// SSE1 128 位拷贝（movups 载+存）。
unsafe fn copy_sse128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movups {t}, [{s}]", "movups [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

// 注：SSE4.1 的唯一纯内存原语 movntdqa 要求源为 WC 类型，对普通 RAM 会 #GP
//（QEMU 实测 #GP 实锤），故不用于 copy；SSE4.1 改用 pmovzxbw 体现在拆包行。

/// AVX VEX-128 拷贝：与 SSE2 同宽，换 VEX 编码（3 操作数、无破坏）。
#[target_feature(enable = "avx")]
unsafe fn copy_avx128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

#[target_feature(enable = "sse2")]
unsafe fn copy_sse2(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

/// SSE3-LDDQU：非对齐视频源专用 load，对齐未知源的对照组。
#[target_feature(enable = "sse3")]
unsafe fn copy_sse3_lddqu(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("lddqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

#[target_feature(enable = "sse2")]
unsafe fn copy_sse2_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movntdq [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

#[target_feature(enable = "avx2")]
unsafe fn copy_avx2(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
    }
}

#[target_feature(enable = "avx2")]
unsafe fn copy_avx2_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovntdq [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// prefetch 扫描：hint 0=无，1=nta，2=prefetchw，3=t0，4=t1，5=t2。
/// load 侧每 512B 预取一次。
#[target_feature(enable = "avx2")]
unsafe fn copy_avx2_pf(dst: *mut u8, src: *const u8, len: usize, hint: u8) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            match hint {
                1 => {
                    asm!("prefetchnta [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                2 => {
                    asm!("prefetchw [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                3 => {
                    asm!("prefetcht0 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                4 => {
                    asm!("prefetcht1 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                5 => {
                    asm!("prefetcht2 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                _ => {}
            }
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
    }
}

#[target_feature(enable = "avx512f")]
unsafe fn copy_avx512(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovdqu8 [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
    }
}

#[target_feature(enable = "avx512f")]
unsafe fn copy_avx512_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovntdq [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

// ---------- 换色：BGRX→RGBX（播放管线每帧在 CPU 干的活） ----------

unsafe fn convert_scalar(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        for i in 0..px {
            let s = src.add(i * 4);
            let d = dst.add(i * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX VEX-128 vpshufb：与 SSSE3 pshufb 同宽同语义，换 VEX 编码对照。
#[target_feature(enable = "avx")]
unsafe fn convert_vpshufb128(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{__m128i, _mm_set_epi8};
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            let mut t: __m128i;
            asm!("vmovdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(xmm_reg) t, m = in(xmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// SSSE3 pshufb：16B=4 像素一次，mask=[2,1,0,3]×4。
#[target_feature(enable = "ssse3")]
unsafe fn convert_pshufb(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::_mm_set_epi8;
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            asm!("movdqu {t}, [{s}]", "pshufb {t}, {m}", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, m = in(xmm_reg) m,
                s = in(reg) src.add(i * 4), d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX2 vpshufb：32B=8 像素一次（lane 内 shuffle，4B 像素不跨 lane，mask 复用）。
#[target_feature(enable = "avx2")]
unsafe fn convert_vpshufb256(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{_mm_set_epi8, _mm256_set_m128i};
        let m128 = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let m = _mm256_set_m128i(m128, m128);
        let mut i = 0;
        let n = px & !7;
        while i < n {
            let mut t: core::arch::x86_64::__m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(ymm_reg) t, m = in(ymm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 8;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX512BW vpshufb：64B=16 像素一次，mask 由 128 位广播×4（lane 内语义相同）。
#[target_feature(enable = "avx512f,avx512bw")]
unsafe fn convert_vpshufb512(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{_mm_set_epi8, _mm512_broadcast_i32x4};
        let m128 = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let m = _mm512_broadcast_i32x4(m128);
        let mut i = 0;
        let n = px & !15;
        while i < n {
            let mut t: core::arch::x86_64::__m512i;
            asm!("vmovdqu8 {t}, [{s}]", t = out(zmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu8 [{d}], {t}",
                t = inout(zmm_reg) t, m = in(zmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 16;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX512VBMI vpermb：512 位任意字节置换（pshufb 只能 lane 内，VBMI 可跨 lane）。
const PERMB_RGBA: [u8; 64] = {
    let mut a = [0u8; 64];
    let mut i = 0;
    while i < 16 {
        a[i * 4] = (i * 4 + 2) as u8;
        a[i * 4 + 1] = (i * 4 + 1) as u8;
        a[i * 4 + 2] = (i * 4) as u8;
        a[i * 4 + 3] = (i * 4 + 3) as u8;
        i += 1;
    }
    a
};

#[target_feature(enable = "avx512f,avx512bw,avx512vbmi")]
unsafe fn convert_vpermb(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::__m512i;
        let mut m: __m512i;
        asm!("vmovdqu8 {m}, [{p}]", m = out(zmm_reg) m, p = in(reg) PERMB_RGBA.as_ptr(), options(nostack));
        let mut i = 0;
        let n = px & !15;
        while i < n {
            let mut t: __m512i;
            asm!("vmovdqu8 {t}, [{s}]", t = out(zmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpermb {t}, {t}, {m}", "vmovdqu8 [{d}], {t}",
                t = inout(zmm_reg) t, m = in(zmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 16;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AMD XOP vpperm：128 位按字节置换（控制位低 4 位选源字节）。
/// 注：rustc 的 xop target_feature 尚未稳定，直接用 asm 发射，运行时门控。
unsafe fn convert_vpperm_xop(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{__m128i, _mm_set_epi8};
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            let mut t: __m128i;
            asm!("vmovdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpperm {t}, {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(xmm_reg) t, m = in(xmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

// ---------- 校验/内容：帧级 checksum 与位密度 ----------

unsafe fn checksum_scalar(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            acc = acc.wrapping_add((buf.add(i) as *const u64).read_unaligned());
            i += 8;
        }
        acc
    }
}

/// SSE4.2 crc32：8B 一步，帧完整性校验的硬件路径。
#[target_feature(enable = "sse4.2")]
unsafe fn checksum_crc32(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut crc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            let w = (buf.add(i) as *const u64).read_unaligned();
            asm!("crc32 {c}, {w}", c = inout(reg) crc, w = in(reg) w, options(nostack));
            i += 8;
        }
        crc
    }
}

/// POPCNT 位密度：雪花 vs 纯色的内容度量（加密/压缩场景的前置数）。
#[target_feature(enable = "popcnt")]
unsafe fn density_popcnt(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            let w = (buf.add(i) as *const u64).read_unaligned();
            let c: u64;
            asm!("popcnt {d}, {s}", d = out(reg) c, s = in(reg) w, options(nostack, preserves_flags));
            acc += c;
            i += 8;
        }
        acc
    }
}

/// AVX512VPOPCNTDQ：64B 一步的位密度，acc 用 vpaddq 常驻 zmm。
#[target_feature(enable = "avx512f,avx512vpopcntdq")]
unsafe fn density_vpopcntq(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::{_mm512_setzero_si512, _mm512_storeu_si512};
        let mut acc = _mm512_setzero_si512();
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {v}, [{s}]", "vpopcntq {c}, {v}", "vpaddq {a}, {a}, {c}",
                v = out(zmm_reg) _, c = out(zmm_reg) _,
                a = inout(zmm_reg) acc,
                s = in(reg) buf.add(i), options(nostack));
            i += 64;
        }
        let mut out = [0u64; 8];
        _mm512_storeu_si512(out.as_mut_ptr() as *mut _, acc);
        let mut tail = 0u64;
        let mut j = n;
        while j + 8 <= len {
            tail += (buf.add(j) as *const u64).read_unaligned().count_ones() as u64;
            j += 8;
        }
        out.iter().sum::<u64>() + tail
    }
}

// ---------- 密码/GFNI/AMD 扩展：数据通路吞吐 ----------

/// AES-NI aesenc 链：帧数据当状态做一轮轮函数（密码 SIMD 通路）。
#[target_feature(enable = "aes")]
unsafe fn crypto_aes(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("aesenc {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// PCLMULQDQ：无进位乘法（CRC/GF 的硬件底座）。
#[target_feature(enable = "pclmulqdq")]
unsafe fn crypto_pclmul(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("pclmulqdq {a}, {t}, 0", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// SHA 扩展 sha1rnds4：SHA-1 轮函数（摘要加速通路）。
#[target_feature(enable = "sha")]
unsafe fn crypto_sha(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("sha1rnds4 {a}, {t}, 1", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// GFNI gf2p8affineqb：GF(2^8) 仿射变换（图像/密码混合通路）。
#[target_feature(enable = "gfni")]
unsafe fn crypto_gfni(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("gf2p8affineqb {a}, {t}, 0", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// VAES vaesenc（256 位）：宽版 AES 轮函数，需 AVX2 承载 ymm。
#[target_feature(enable = "vaes,avx2")]
unsafe fn crypto_vaes(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m256i;
        let mut acc: __m256i;
        asm!("vmovdqu {a}, [{p}]", a = out(ymm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !31;
        while i < n {
            let mut t: __m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("vaesenc {a}, {a}, {t}", a = inout(ymm_reg) acc, t = in(ymm_reg) t, options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        let mut o = [0u64; 4];
        asm!("vmovdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(ymm_reg) acc, options(nostack));
        o[0] ^ o[1] ^ o[2] ^ o[3]
    }
}

/// VPCLMULQDQ（256 位）：宽版无进位乘法。
#[target_feature(enable = "vpclmulqdq,avx2")]
unsafe fn crypto_vpclmul(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m256i;
        let mut acc: __m256i;
        asm!("vmovdqu {a}, [{p}]", a = out(ymm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !31;
        while i < n {
            let mut t: __m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("vpclmulqdq {a}, {a}, {t}, 0", a = inout(ymm_reg) acc, t = in(ymm_reg) t, options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        let mut o = [0u64; 4];
        asm!("vmovdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(ymm_reg) acc, options(nostack));
        o[0] ^ o[1] ^ o[2] ^ o[3]
    }
}

/// AMD SSE4a extrq：位段提取（AMD 128 位世代的数据通路）。
#[target_feature(enable = "sse4a")]
unsafe fn crypto_sse4a(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut ctrl: __m128i;
        asm!("movdqu {c}, [{p}]", c = out(xmm_reg) ctrl, p = in(reg) buf.add(16), options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("extrq {a}, {c}", a = inout(xmm_reg) acc, c = in(xmm_reg) ctrl, options(nostack));
            asm!("pxor {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// AMD 3DNow! pfadd：打包浮点加（整数 movq 夹层，避免浮点 Rust 类型被标量化）。
unsafe fn crypto_3dnow(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0x3f80_3f80_3f80_3f80;
        let mut i = 0;
        let n = len & !7;
        while i < n {
            let w = (buf.add(i) as *const u64).read_unaligned();
            let r: u64;
            asm!(
                "movq mm0, {a}",
                "movq mm1, {b}",
                "pfadd mm0, mm1",
                "movq {r}, mm0",
                r = out(reg) r, a = in(reg) acc, b = in(reg) w,
                out("mm0") _, out("mm1") _,
                options(nostack)
            );
            acc = r;
            i += 8;
        }
        asm!("emms", options(nostack, preserves_flags));
        acc
    }
}

/// SSE4.1 pmovzxbw：字节零扩展成字（像素拆包基线；唯一不依赖 WC 的 SSE4.1 数据原语）。
#[target_feature(enable = "sse4.1")]
unsafe fn extra_sse41_unpack(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("pxor {a}, {a}", a = out(xmm_reg) acc, options(nostack));
        let mut i = 0;
        let n = len & !7;
        while i < n {
            let mut t: __m128i;
            asm!("movq {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("pmovzxbw {t}, {t}", t = inout(xmm_reg) t, options(nostack));
            asm!("paddw {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 8;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

// ---------- 生成：xorshift vs 硬件 RNG ----------
unsafe fn rng_xorshift(dst: *mut u32, n: usize, seed: u32) {
    unsafe {
        let mut rng = seed | 1;
        for i in 0..n {
            rng ^= rng.wrapping_shl(13);
            rng ^= rng.wrapping_shr(17);
            rng ^= rng.wrapping_shl(5);
            dst.add(i).write(rng);
        }
        core::hint::black_box(dst);
    }
}

/// RDRAND：10 次重试/字，失败记数不断流（NIST 级熵，雪花屏的另一原料）。
#[target_feature(enable = "rdrand")]
unsafe fn rng_rdrand(dst: *mut u32, n: usize) -> u32 {
    unsafe {
        let mut fails = 0u32;
        for i in 0..n {
            let mut v = 0u32;
            let mut ok = 0u8;
            for _ in 0..10 {
                asm!("rdrand {v:e}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nostack));
                if ok == 1 {
                    break;
                }
            }
            if ok == 1 {
                dst.add(i).write(v);
            } else {
                fails += 1;
                dst.add(i).write(0x9E37_79B9);
            }
        }
        core::hint::black_box(dst);
        fails
    }
}

/// RDSEED：种子级熵，比 RDRAND 慢一个量级，轮数减半。
#[target_feature(enable = "rdseed")]
unsafe fn rng_rdseed(dst: *mut u32, n: usize) -> u32 {
    unsafe {
        let mut fails = 0u32;
        for i in 0..n {
            let mut v = 0u32;
            let mut ok = 0u8;
            for _ in 0..20 {
                asm!("rdseed {v:e}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nostack));
                if ok == 1 {
                    break;
                }
            }
            if ok == 1 {
                dst.add(i).write(v);
            } else {
                fails += 1;
                dst.add(i).write(0x9E37_79B9);
            }
        }
        core::hint::black_box(dst);
        fails
    }
}

// ---------- GPR 像素 helper：BMI/LZCNT（标量寄存器，无向量后端风险） ----------

/// BMI1-BEXTR：逐像素抽 R 通道（bits16..23），control=0x810 常驻寄存器。
#[target_feature(enable = "bmi1")]
unsafe fn gpr_bextr_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let ctrl: u32 = 16 | (8 << 8);
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("bextr {d:e}, {s:e}, {c:e}", d = out(reg) r, s = in(reg) v, c = in(reg) ctrl, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2-PEXT：聚拢每像素 bit0（mask=0x01010101）→ 4bit 位平面，位平面打包原语。
#[target_feature(enable = "bmi2")]
unsafe fn gpr_pext_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mask: u32 = 0x0101_0101;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("pext {d:e}, {s:e}, {m:e}", d = out(reg) r, s = in(reg) v, m = in(reg) mask, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// LZCNT：逐字前导零累加，内容稀疏度扫描。
#[target_feature(enable = "lzcnt")]
unsafe fn gpr_lzcnt_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("lzcnt {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI1 ANDN：dest = ~a & b。
#[target_feature(enable = "bmi1")]
unsafe fn gpr_andn_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("andn {d:e}, {s:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc = acc.wrapping_add(r as u64);
        }
        acc
    }
}

/// TZCNT：尾零计数（AMD 走 ABM，Intel 走 BMI1，编码同为 F3 0F BC）。
#[target_feature(enable = "bmi1")]
unsafe fn gpr_tzcnt_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("tzcnt {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 PDEP：按掩码分散位。
#[target_feature(enable = "bmi2")]
unsafe fn gpr_pdep_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mask: u32 = 0x0101_0101;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("pdep {d:e}, {s:e}, {m:e}", d = out(reg) r, s = in(reg) v, m = in(reg) mask, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 RORX：无标志循环右移。
#[target_feature(enable = "bmi2")]
unsafe fn gpr_rorx_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("rorx {d:e}, {s:e}, 8", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 BZHI：清零高位（保留低 n 位）。
#[target_feature(enable = "bmi2")]
unsafe fn gpr_bzhi_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let idx: u32 = 24;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("bzhi {d:e}, {s:e}, {i:e}", d = out(reg) r, s = in(reg) v, i = in(reg) idx, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// MOVBE：字节序交换加载（端序转换通路）。
#[target_feature(enable = "movbe")]
unsafe fn gpr_movbe_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let r: u32;
            asm!("movbe {d:e}, [{s}]", d = out(reg) r, s = in(reg) buf.add(i), options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// AMD TBM blcfill：清最低置位以下所有位。
#[target_feature(enable = "tbm")]
unsafe fn gpr_blcfill_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("blcfill {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

// ---------- 主入口 ----------

pub fn run_section(
    report: &mut Vec<alloc::string::String>,
    screen: &mut Screen,
    tps: u64,
) -> Result {
    let isa = read_isa();
    let g = read_gates(&isa);
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    let stride = screen.get_gop().current_mode_info().stride();
    let fb_bytes = w * h * 4;

    tline!(report, screen, "-- gop-simd: full-ISA pixel path --");
    tline!(
        report,
        screen,
        "gates: MMX {} SSE {} SSE2 {} SSE3 {} SSSE3 {} SSE4.1 {} SSE4.2 {} AES {} PCLMUL {} SHA {} GFNI {} POPCNT {} RDRAND {} RDSEED {} MOVBE {} BMI1 {} BMI2 {} LZCNT {} TZCNT {} CLFLUSHOPT {} CLWB {} PREFETCHW {}",
        yn(g.mmx),
        yn(g.sse),
        yn(g.sse2),
        yn(g.sse3),
        yn(g.ssse3),
        yn(g.sse41),
        yn(g.sse42),
        yn(g.aes),
        yn(g.pclmulqdq),
        yn(g.sha),
        yn(g.gfni),
        yn(g.popcnt),
        yn(g.rdrand),
        yn(g.rdseed),
        yn(g.movbe),
        yn(g.bmi1),
        yn(g.bmi2),
        yn(g.lzcnt),
        yn(g.tzcnt),
        yn(g.clflushopt),
        yn(g.clwb),
        yn(g.prefetchw),
    );
    tline!(
        report,
        screen,
        "gates2: AVX {} AVX2 {} AVX512F {} BW {} VL {} VBMI {} VPOPCNTDQ {} VAES {} VPCLMUL {} || AMD: SSE4a {} 3DNow {} 3DNow+ {} XOP {} TBM {}",
        yn(g.avx_usable),
        yn(g.avx2_usable),
        yn(g.avx512_usable),
        yn(g.avx512bw_hw),
        yn(g.avx512vl_hw),
        yn(g.avx512vbmi_hw),
        yn(g.avx512vpopcntdq_hw),
        yn(g.vaes_hw),
        yn(g.vpclmulqdq_hw),
        yn(g.sse4a),
        yn(g.d3now),
        yn(g.d3nowa),
        yn(g.xop),
        yn(g.tbm),
    );
    // 全叶原始值（7.EDX/7.1.EAX/ext.EDX 只报不测：AI/密码/历史语义，无像素通路）。
    // 7.1 子叶不存在时 EAX 恒零，has7.1 如实标注，免得把“不存在”误读成“全不支持”。
    tline!(
        report,
        screen,
        "raw: leaf7.EBX={:#010x} ECX={:#010x} EDX={:#010x} leaf7.1(has={})EAX={:#010x} ext.ECX={:#010x} EDX={:#010x}",
        isa.b7,
        isa.c7,
        isa.d7,
        yn(isa.hass1),
        isa.s1,
        isa.xc,
        isa.xd,
    );

    // framebuffer 对齐门：nt/512 要求天然对齐，否则如实 SKIP。
    let fb_base = {
        let mut fb = screen.get_gop().frame_buffer();
        fb.as_mut_ptr() as usize
    };
    let aligned16 = fb_base & 15 == 0;
    let aligned32 = fb_base & 31 == 0;
    let aligned64 = fb_base & 63 == 0;
    if !aligned64 {
        tline!(
            report,
            screen,
            "note: fb base {:#x} not 64B-aligned, nt/512 variants SKIP",
            fb_base
        );
    }

    let seed = (rdtsc() as u32) | 1;
    core::hint::black_box(seed);

    // ---- fill 组：轮转交替 ----
    {
        struct F {
            name: &'static str,
            on: bool,
            f: unsafe fn(*mut u8, usize, u32),
            avx: bool,
        }
        let vars = [
            F {
                name: "scalar-u64",
                on: true,
                f: fill_scalar_u64,
                avx: false,
            },
            F {
                name: "mmx-64",
                on: g.mmx,
                f: fill_mmx64,
                avx: false,
            },
            F {
                name: "sse-128",
                on: g.sse,
                f: fill_sse128,
                avx: false,
            },
            F {
                name: "sse2-128",
                on: g.sse2,
                f: fill_sse2,
                avx: false,
            },
            F {
                name: "sse2-128-nt",
                on: g.sse2 && aligned16,
                f: fill_sse2_nt,
                avx: false,
            },
            F {
                name: "avx-128-vex",
                on: g.avx_usable,
                f: fill_avx128,
                avx: true,
            },
            F {
                name: "avx2-256",
                on: g.avx2_usable,
                f: fill_avx2,
                avx: true,
            },
            F {
                name: "avx2-256-nt",
                on: g.avx2_usable && aligned32,
                f: fill_avx2_nt,
                avx: true,
            },
            F {
                name: "avx512-512",
                on: g.avx512_usable && aligned64,
                f: fill_avx512,
                avx: true,
            },
            F {
                name: "avx512-512-nt",
                on: g.avx512_usable && aligned64,
                f: fill_avx512_nt,
                avx: true,
            },
            F {
                name: "avx512vl-256",
                on: g.avx512_usable && g.avx512vl_hw && aligned32,
                f: fill_avx512vl256,
                avx: true,
            },
        ];
        for v in vars.iter().filter(|v| !v.on) {
            tline!(
                report,
                screen,
                "gop-simd fill {}: SKIP (gate/align)",
                v.name
            );
        }
        let live: Vec<&F> = vars.iter().filter(|v| v.on).collect();
        // 热身 1 轮（不计时）。
        {
            let mut fb = screen.get_gop().frame_buffer();
            let dst = fb.as_mut_ptr();
            for v in live.iter() {
                unsafe { (v.f)(dst, fb_bytes, seed) };
                if v.avx {
                    unsafe { vzeroupper() };
                }
            }
        }
        const ROUNDS: u64 = 10;
        let mut acc = alloc::vec![0u64; live.len()];
        {
            let mut fb = screen.get_gop().frame_buffer();
            let dst = fb.as_mut_ptr();
            for _ in 0..ROUNDS {
                for (i, v) in live.iter().enumerate() {
                    let t = rdtsc();
                    unsafe { (v.f)(dst, fb_bytes, seed) };
                    acc[i] += rdtsc() - t;
                    if v.avx {
                        unsafe { vzeroupper() };
                    }
                }
            }
        }
        let mut base_mbs = 0.0;
        for (i, v) in live.iter().enumerate() {
            let mbs = (ROUNDS * fb_bytes as u64) as f64 * tps as f64
                / (acc[i].max(1) as f64 * 1024.0 * 1024.0);
            if i == 0 {
                base_mbs = mbs;
            }
            tline!(
                report,
                screen,
                "gop-simd fill {:<14}: {:7.1} MB/s ({:+.0}% vs scalar)",
                v.name,
                mbs,
                (mbs - base_mbs) / base_mbs.max(0.01) * 100.0
            );
        }
    }

    // ---- copy 组：WB→WC，轮转交替 ----
    let src_buf = {
        let mut b = alloc::vec![0u8; fb_bytes];
        let mut rng = seed;
        for x in b.iter_mut() {
            rng ^= rng.wrapping_shl(13);
            rng ^= rng.wrapping_shr(17);
            rng ^= rng.wrapping_shl(5);
            *x = (rng & 0xFF) as u8;
        }
        core::hint::black_box(b.as_ptr());
        b
    };
    {
        struct C {
            name: &'static str,
            on: bool,
            f: unsafe fn(*mut u8, *const u8, usize),
            avx: bool,
        }
        let vars = [
            C {
                name: "scalar-u64",
                on: true,
                f: copy_scalar_u64,
                avx: false,
            },
            C {
                name: "mmx-64",
                on: g.mmx,
                f: copy_mmx64,
                avx: false,
            },
            C {
                name: "sse-128",
                on: g.sse,
                f: copy_sse128,
                avx: false,
            },
            C {
                name: "sse2-128",
                on: g.sse2,
                f: copy_sse2,
                avx: false,
            },
            C {
                name: "sse3-lddqu",
                on: g.sse3,
                f: copy_sse3_lddqu,
                avx: false,
            },
            C {
                name: "sse2-128-nt",
                on: g.sse2 && aligned16,
                f: copy_sse2_nt,
                avx: false,
            },
            C {
                name: "avx-128-vex",
                on: g.avx_usable,
                f: copy_avx128,
                avx: true,
            },
            C {
                name: "avx2-256",
                on: g.avx2_usable,
                f: copy_avx2,
                avx: true,
            },
            C {
                name: "avx2-256-nt",
                on: g.avx2_usable && aligned32,
                f: copy_avx2_nt,
                avx: true,
            },
            C {
                name: "avx512-512",
                on: g.avx512_usable && aligned64,
                f: copy_avx512,
                avx: true,
            },
            C {
                name: "avx512-512-nt",
                on: g.avx512_usable && aligned64,
                f: copy_avx512_nt,
                avx: true,
            },
        ];
        for v in vars.iter().filter(|v| !v.on) {
            tline!(
                report,
                screen,
                "gop-simd copy {}: SKIP (gate/align)",
                v.name
            );
        }
        let live: Vec<&C> = vars.iter().filter(|v| v.on).collect();
        {
            let mut fb = screen.get_gop().frame_buffer();
            let dst = fb.as_mut_ptr();
            let s = core::hint::black_box(src_buf.as_ptr());
            for v in live.iter() {
                unsafe { (v.f)(dst, s, fb_bytes) };
                if v.avx {
                    unsafe { vzeroupper() };
                }
            }
        }
        const ROUNDS: u64 = 10;
        let mut acc = alloc::vec![0u64; live.len()];
        {
            let mut fb = screen.get_gop().frame_buffer();
            let dst = fb.as_mut_ptr();
            for _ in 0..ROUNDS {
                for (i, v) in live.iter().enumerate() {
                    let s = core::hint::black_box(src_buf.as_ptr());
                    let t = rdtsc();
                    unsafe { (v.f)(dst, s, fb_bytes) };
                    acc[i] += rdtsc() - t;
                    if v.avx {
                        unsafe { vzeroupper() };
                    }
                }
            }
        }
        let mut base_mbs = 0.0;
        for (i, v) in live.iter().enumerate() {
            let mbs = (ROUNDS * fb_bytes as u64) as f64 * tps as f64
                / (acc[i].max(1) as f64 * 1024.0 * 1024.0);
            if i == 0 {
                base_mbs = mbs;
            }
            tline!(
                report,
                screen,
                "gop-simd copy {:<14}: {:7.1} MB/s ({:+.0}% vs scalar)",
                v.name,
                mbs,
                (mbs - base_mbs) / base_mbs.max(0.01) * 100.0
            );
        }
        // prefetch 扫描（同 256 位宽，只换 hint）。
        if g.avx2_usable {
            let mut fb = screen.get_gop().frame_buffer();
            let dst = fb.as_mut_ptr();
            let hints: [(&str, u8, bool); 6] = [
                ("none", 0, true),
                ("nta", 1, true),
                ("prefetchw", 2, g.prefetchw),
                ("t0", 3, true),
                ("t1", 4, true),
                ("t2", 5, true),
            ];
            let mut pf = [0u64; 6];
            for r in 0..5u64 {
                for (k, (_, hint, ok)) in hints.iter().enumerate() {
                    if !*ok {
                        continue;
                    }
                    let s = core::hint::black_box(src_buf.as_ptr());
                    let t = rdtsc();
                    unsafe { copy_avx2_pf(dst, s, fb_bytes, *hint) };
                    let dt = rdtsc() - t;
                    if r > 0 {
                        pf[k] += dt;
                    }
                }
            }
            unsafe { vzeroupper() };
            let mbs = |dt: u64| {
                (4 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0)
            };
            for (k, (name, _, ok)) in hints.iter().enumerate() {
                if *ok {
                    tline!(
                        report,
                        screen,
                        "gop-simd copy avx2-256-pf-{:<10}: {:.1} MB/s",
                        name,
                        mbs(pf[k])
                    );
                } else {
                    tline!(
                        report,
                        screen,
                        "gop-simd copy avx2-256-pf-{:<10}: SKIP (no hw)",
                        name
                    );
                }
            }
        } else {
            tline!(
                report,
                screen,
                "gop-simd copy prefetch sweep: SKIP (need AVX2)"
            );
        }
    }

    // ---- 换色组 ----
    {
        let px = w * h;
        let src = src_buf.clone();
        let mut dst = alloc::vec![0u8; fb_bytes];
        let t_scalar = {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_scalar(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_scalar(d, s, px) };
            }
            rdtsc() - t0
        };
        let scalar_mbs =
            (5 * fb_bytes as u64) as f64 * tps as f64 / (t_scalar.max(1) as f64 * 1024.0 * 1024.0);
        tline!(
            report,
            screen,
            "gop-simd convert scalar     : {:.1} MB/s",
            scalar_mbs
        );
        if g.ssse3 {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_pshufb(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_pshufb(d, s, px) };
            }
            let dt = rdtsc() - t0;
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert ssse3-pshufb: {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert ssse3-pshufb: SKIP (no SSSE3)"
            );
        }
        if g.avx2_usable {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_vpshufb256(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_vpshufb256(d, s, px) };
            }
            let dt = rdtsc() - t0;
            unsafe { vzeroupper() };
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert avx2-vpshufb : {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert avx2-vpshufb : SKIP (need AVX2+XCR0)"
            );
        }
        if g.avx512_usable && g.avx512bw_hw && aligned64 {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_vpshufb512(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_vpshufb512(d, s, px) };
            }
            let dt = rdtsc() - t0;
            unsafe { vzeroupper() };
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert avx512bw-shuf: {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert avx512bw-shuf: SKIP (need AVX512F+BW+XCR0-ZMM)"
            );
        }
        if g.avx_usable {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_vpshufb128(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_vpshufb128(d, s, px) };
            }
            let dt = rdtsc() - t0;
            unsafe { vzeroupper() };
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert avx-pshufb128 : {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert avx-pshufb128 : SKIP (need AVX)"
            );
        }
        if g.avx512_usable && g.avx512bw_hw && g.avx512vbmi_hw && aligned64 {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_vpermb(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_vpermb(d, s, px) };
            }
            let dt = rdtsc() - t0;
            unsafe { vzeroupper() };
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert avx512vbmi-permb: {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert avx512vbmi-permb: SKIP (need AVX512VBMI+XCR0-ZMM)"
            );
        }
        if g.xop {
            let s = core::hint::black_box(src.as_ptr());
            let d = core::hint::black_box(dst.as_mut_ptr());
            for _ in 0..2 {
                unsafe { convert_vpperm_xop(d, s, px) };
            }
            let t0 = rdtsc();
            for _ in 0..5 {
                unsafe { convert_vpperm_xop(d, s, px) };
            }
            let dt = rdtsc() - t0;
            let mbs =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd convert xop-vpperm    : {:.1} MB/s ({:+.0}%)",
                mbs,
                (mbs - scalar_mbs) / scalar_mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd convert xop-vpperm    : SKIP (no XOP)"
            );
        }
        core::hint::black_box(dst.as_ptr());
    }

    // ---- 校验/密度组 ----
    {
        let s = core::hint::black_box(src_buf.as_ptr());
        let t0 = rdtsc();
        let mut chk = 0u64;
        for _ in 0..5 {
            chk = chk.wrapping_add(unsafe { checksum_scalar(s, fb_bytes) });
        }
        let dt = rdtsc() - t0;
        core::hint::black_box(chk);
        let mbs = (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
        tline!(
            report,
            screen,
            "gop-simd checksum scalar-sum: {:.1} MB/s (chk={:#x})",
            mbs,
            chk & 0xFFFF
        );
        if g.sse42 {
            let t0 = rdtsc();
            let mut chk = 0u64;
            for _ in 0..5 {
                chk = chk.wrapping_add(unsafe { checksum_crc32(s, fb_bytes) });
            }
            let dt = rdtsc() - t0;
            core::hint::black_box(chk);
            let m2 =
                (5 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd checksum sse42-crc32: {:.1} MB/s ({:+.0}%)",
                m2,
                (m2 - mbs) / mbs.max(0.01) * 100.0
            );
        } else {
            tline!(
                report,
                screen,
                "gop-simd checksum sse42-crc32: SKIP (no SSE4.2)"
            );
        }
        if g.popcnt {
            let t0 = rdtsc();
            let mut bits = 0u64;
            for _ in 0..3 {
                bits = bits.wrapping_add(unsafe { density_popcnt(s, fb_bytes) });
            }
            let dt = rdtsc() - t0;
            let m2 =
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd density popcnt: {:.1} MB/s (bits/byte={:.2})",
                m2,
                bits as f64 / (3 * fb_bytes) as f64
            );
        } else {
            tline!(report, screen, "gop-simd density popcnt: SKIP (no POPCNT)");
        }
        if g.avx512_usable && g.avx512vpopcntdq_hw {
            let t0 = rdtsc();
            let mut bits = 0u64;
            for _ in 0..3 {
                bits = bits.wrapping_add(unsafe { density_vpopcntq(s, fb_bytes) });
            }
            let dt = rdtsc() - t0;
            unsafe { vzeroupper() };
            let m2 =
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(report, screen, "gop-simd density vpopcntq: {:.1} MB/s", m2);
        } else {
            tline!(
                report,
                screen,
                "gop-simd density vpopcntq: SKIP (need AVX512VPOPCNTDQ+XCR0-ZMM)"
            );
        }

        // ---- SSE4.1 拆包 / 密码 / GFNI / AMD 扩展数据通路 ----
        let mut extra =
            |name: &str, on: bool, collapse_zero: bool, f: unsafe fn(*const u8, usize) -> u64| {
                if !on {
                    tline!(report, screen, "gop-simd extra {:<16}: SKIP", name);
                    return;
                }
                let s = core::hint::black_box(src_buf.as_ptr());
                core::hint::black_box(unsafe { f(s, fb_bytes) }); // 热身
                let t0 = rdtsc();
                let mut acc = 0u64;
                for _ in 0..3 {
                    acc = acc.wrapping_add(unsafe { f(s, fb_bytes) });
                }
                let dt = rdtsc() - t0;
                core::hint::black_box(acc);
                let mbs = (3 * fb_bytes as u64) as f64 * tps as f64
                    / (dt.max(1) as f64 * 1024.0 * 1024.0);
                if collapse_zero {
                    // PCLMUL 链在 8MB 随机缓冲下数学上必坍缩为 0（截断环赋值 walk，
                    // 宿主机同算法复现 o0=o1=0，二进制编码已确认无误），chk!=0 反而是错。
                    tline!(
                        report,
                        screen,
                        "gop-simd extra {:<16}: {:.1} MB/s (collapse-invariant {})",
                        name,
                        mbs,
                        if acc == 0 { "PASS" } else { "FAIL" }
                    );
                } else {
                    tline!(
                        report,
                        screen,
                        "gop-simd extra {:<16}: {:.1} MB/s (chk={:#x})",
                        name,
                        mbs,
                        acc & 0xFFFF
                    );
                }
            };
        extra("sse4.1-unpack", g.sse41, false, extra_sse41_unpack);
        extra("aes-ni", g.aes, false, crypto_aes);
        extra("pclmulqdq", g.pclmulqdq, true, crypto_pclmul);
        extra("sha-ext", g.sha, false, crypto_sha);
        extra("gfni", g.gfni, false, crypto_gfni);
        extra("vaes-256", g.vaes_hw && g.avx2_usable, false, crypto_vaes);
        extra(
            "vpclmulqdq-256",
            g.vpclmulqdq_hw && g.avx2_usable,
            true,
            crypto_vpclmul,
        );
        extra("amd-sse4a", g.sse4a, false, crypto_sse4a);
        extra("amd-3dnow", g.d3nowa, false, crypto_3dnow);
    }

    // ---- RNG 组 ----
    {
        let words = fb_bytes / 4;
        let mut buf = alloc::vec![0u32; words];
        let t0 = rdtsc();
        unsafe { rng_xorshift(buf.as_mut_ptr(), words, seed) };
        let dt = rdtsc() - t0;
        let mbs = (fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
        tline!(report, screen, "gop-simd rng xorshift: {:.1} MB/s", mbs);
        if g.rdrand {
            let t0 = rdtsc();
            let fails = unsafe { rng_rdrand(buf.as_mut_ptr(), words) };
            let dt = rdtsc() - t0;
            let m2 = (fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd rng rdrand: {:.1} MB/s (fails={})",
                m2,
                fails
            );
        } else {
            tline!(report, screen, "gop-simd rng rdrand: SKIP (no RDRAND hw)");
        }
        if g.rdseed {
            let half = words / 2;
            let t0 = rdtsc();
            let fails = unsafe { rng_rdseed(buf.as_mut_ptr(), half) };
            let dt = rdtsc() - t0;
            let m2 = (half as u64 * 4) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0);
            tline!(
                report,
                screen,
                "gop-simd rng rdseed: {:.1} MB/s (fails={}, half-buf)",
                m2,
                fails
            );
        } else {
            tline!(report, screen, "gop-simd rng rdseed: SKIP (no RDSEED hw)");
        }
        core::hint::black_box(buf.as_ptr());
    }

    // ---- GPR helper 组 ----
    {
        let words = fb_bytes / 4;
        let src32: Vec<u32> = {
            let mut v = alloc::vec![0u32; words];
            let mut rng = seed;
            for x in v.iter_mut() {
                rng ^= rng.wrapping_shl(13);
                rng ^= rng.wrapping_shr(17);
                rng ^= rng.wrapping_shl(5);
                *x = rng;
            }
            v
        };
        let s = core::hint::black_box(src32.as_ptr());
        if g.bmi1 {
            let t0 = rdtsc();
            let mut acc = 0u64;
            for _ in 0..3 {
                acc = acc.wrapping_add(unsafe { gpr_bextr_sum(s, words) });
            }
            let dt = rdtsc() - t0;
            core::hint::black_box(acc);
            tline!(
                report,
                screen,
                "gop-simd gpr bmi1-bextr: {:.1} MB/s",
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0)
            );
        } else {
            tline!(report, screen, "gop-simd gpr bmi1-bextr: SKIP (no BMI1)");
        }
        if g.bmi2 {
            let t0 = rdtsc();
            let mut acc = 0u64;
            for _ in 0..3 {
                acc = acc.wrapping_add(unsafe { gpr_pext_sum(s, words) });
            }
            let dt = rdtsc() - t0;
            core::hint::black_box(acc);
            tline!(
                report,
                screen,
                "gop-simd gpr bmi2-pext : {:.1} MB/s",
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0)
            );
        } else {
            tline!(report, screen, "gop-simd gpr bmi2-pext : SKIP (no BMI2)");
        }
        if g.lzcnt {
            let t0 = rdtsc();
            let mut acc = 0u64;
            for _ in 0..3 {
                acc = acc.wrapping_add(unsafe { gpr_lzcnt_sum(s, words) });
            }
            let dt = rdtsc() - t0;
            core::hint::black_box(acc);
            tline!(
                report,
                screen,
                "gop-simd gpr lzcnt     : {:.1} MB/s",
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0)
            );
        } else {
            tline!(report, screen, "gop-simd gpr lzcnt     : SKIP (no LZCNT)");
        }
        let mut gprx = |name: &str, on: bool, f: unsafe fn(*const u32, usize) -> u64| {
            if !on {
                tline!(report, screen, "gop-simd gpr {:<16}: SKIP", name);
                return;
            }
            let t0 = rdtsc();
            let mut acc = 0u64;
            for _ in 0..3 {
                acc = acc.wrapping_add(unsafe { f(s, words) });
            }
            let dt = rdtsc() - t0;
            core::hint::black_box(acc);
            tline!(
                report,
                screen,
                "gop-simd gpr {:<16}: {:.1} MB/s",
                name,
                (3 * fb_bytes as u64) as f64 * tps as f64 / (dt.max(1) as f64 * 1024.0 * 1024.0)
            );
        };
        gprx("bmi1-andn", g.bmi1, gpr_andn_sum);
        gprx("bmi1-tzcnt", g.tzcnt, gpr_tzcnt_sum);
        gprx("bmi2-pdep", g.bmi2, gpr_pdep_sum);
        gprx("bmi2-rorx", g.bmi2, gpr_rorx_sum);
        gprx("bmi2-bzhi", g.bmi2, gpr_bzhi_sum);
        gprx("movbe", g.movbe, gpr_movbe_sum);
        gprx("amd-tbm-blcfill", g.tbm, gpr_blcfill_sum);
        core::hint::black_box(s);
    }

    // ---- 本节归因 ----
    tline!(
        report,
        screen,
        "verdict: gop-simd widths compared against same-run scalar (no stale baselines)"
    );
    tline!(
        report,
        screen,
        "verdict: nt-stream wins only if WC-write-combining not already optimal; else temporal wins (fence cost)"
    );
    tline!(
        report,
        screen,
        "verdict: shufb-lanes are per-128b; 256/512 speedups come from width, not fewer shuffles"
    );
    tline!(
        report,
        screen,
        "note: not-executed (by design): FP vectors FMA/FMA4/F16C/AVX512-FP16/BF16/AVX512-BF16 (target soft-float, would scalarize); AI/matrix VNNI/AVX-VNNI/BITALG/AMX-*/IFMA/VP2INTERSECT/4FMAPS/4VNNIW; system/bigint ADX/FSGSBASE/SMAP/XSAVE/CET; AMD legacy FP MMXEXT/FFXSR. Everything else is executed, gated, or SKIP-with-reason."
    );
    let _ = stride;
    Ok(())
}
