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

use crate::error::Result;
use crate::graphics::Screen;

pub mod isa;
pub mod kernels;
pub mod misc;

use self::isa::{rdtsc, read_gates, read_isa, yn};
use self::kernels::*;
use self::misc::*;

macro_rules! tline {
    ($report:expr, $screen:expr, $($arg:tt)*) => {{
        let s = alloc::format!($($arg)*);
        log::info!("{}", s);
        $screen.draw_str(&s);
        $report.push(s);
    }};
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
