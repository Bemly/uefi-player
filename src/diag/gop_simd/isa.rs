//! `gop_simd::isa`：CPUID/XCR0 门控（`Isa` 原始位 + `Gates` 可用矩阵）。
//! 由主入口 `run_section` 开头快照一次，后续变体只查表不重复 `cpuid`。
//! 拆分仅为满足单文件 ≤1000 行，不改行为。

use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

use crate::diag::avx_checker::check_avx_support;

#[inline(always)]
pub(crate) fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

pub(crate) fn yn(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

// ---------- ISA 门控 ----------
// ---------- ISA 门控 ----------

pub(crate) struct Isa {
    pub(crate) l1c: u32,
    pub(crate) l1d: u32,
    pub(crate) b7: u32,
    pub(crate) c7: u32,
    pub(crate) d7: u32,
    pub(crate) s1: u32,
    pub(crate) xc: u32,
    pub(crate) xd: u32,
    pub(crate) has7: bool,
    pub(crate) hass1: bool,
    pub(crate) hasext: bool,
}

pub(crate) fn read_isa() -> Isa {
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
pub(crate) struct Gates {
    // ---- 128 位世代 ----
    pub(crate) mmx: bool,
    pub(crate) sse: bool,
    pub(crate) sse2: bool,
    pub(crate) sse3: bool,
    pub(crate) ssse3: bool,
    pub(crate) sse41: bool,
    pub(crate) sse42: bool,
    // ---- 密码/GFNI ----
    pub(crate) aes: bool,
    pub(crate) pclmulqdq: bool,
    pub(crate) sha: bool,
    pub(crate) gfni: bool,
    // ---- 位操纵/杂项 ----
    pub(crate) popcnt: bool,
    pub(crate) rdrand: bool,
    pub(crate) rdseed: bool,
    pub(crate) movbe: bool,
    pub(crate) bmi1: bool,
    pub(crate) bmi2: bool,
    pub(crate) lzcnt: bool,
    pub(crate) tzcnt: bool,
    pub(crate) clflushopt: bool,
    pub(crate) clwb: bool,
    pub(crate) prefetchw: bool,
    // ---- AVX 体系 ----
    pub(crate) avx_usable: bool,
    pub(crate) avx2_usable: bool,
    pub(crate) avx512_usable: bool,
    pub(crate) avx512bw_hw: bool,
    pub(crate) avx512vl_hw: bool,
    pub(crate) avx512vbmi_hw: bool,
    pub(crate) avx512vpopcntdq_hw: bool,
    pub(crate) vaes_hw: bool,
    pub(crate) vpclmulqdq_hw: bool,
    // ---- AMD 世代 ----
    pub(crate) sse4a: bool,
    pub(crate) d3now: bool,
    pub(crate) d3nowa: bool,
    pub(crate) xop: bool,
    pub(crate) tbm: bool,
}

pub(crate) fn read_gates(isa: &Isa) -> Gates {
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
