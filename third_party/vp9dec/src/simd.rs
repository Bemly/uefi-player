//! AVX2 SIMD kernels, x86_64-only (`core::arch::x86_64` intrinsics, zero dependencies),
//! runtime-detected and force-disabled via [`crate::simd::avx2_enabled`]. The scalar decode paths own
//! every dispatch point and fallback; this module owns only the vector kernels, split by
//! pipeline stage into submodules (all entry points re-exported here, so call sites use
//! `crate::simd::<kernel>`):
//!
//! - `inter`: the inter-prediction two-pass 8-tap subpel convolution (spec §8.5.2.4),
//!   direct-load unscaled kernels (widths 8+ and 4) plus a general edge-clamping kernel
//!   for scaled references and unscaled reference-edge blocks; `predict.rs` dispatches.
//! - `loop_filter`: the deblocking edge filters (spec §8.8.5, narrow / wide8 / wide16) for
//!   both edge orientations; `loop_filter.rs` dispatches.
//! - `transform`: the inverse DCT/ADST transforms fused with reconstruction (spec §8.7);
//!   `tile/residual.rs` dispatches (DCT_DCT and the ADST-containing types at all bit
//!   depths; only lossless WHT stays scalar -- see the landmines in
//!   docs/implementation-notes.md).
//!
//! A NEON mirror for aarch64 would be sibling `#[cfg(target_arch = "aarch64")]` modules
//! behind the same dispatch points -- not implemented.

// VENDOR(uefi-player): the kernels are pure `core::arch::x86_64` integer
// intrinsics (no float vectors, no nt stores -- verified by inspection), so
// they compile under `no_std` too; the runtime gate below
// (`avx2_enabled`) is what keeps them off hardware that cannot run them.
// x86_64-only (scalar owns every other target; a NEON mirror would be
// sibling `#[cfg(target_arch = "aarch64")]` modules -- not implemented).
#[cfg(target_arch = "x86_64")]
mod inter;
#[cfg(target_arch = "x86_64")]
mod loop_filter;
#[cfg(target_arch = "x86_64")]
mod transform;

#[cfg(all(test, feature = "std", target_arch = "x86_64"))]
#[path = "../tests/unit/simd.rs"]
mod tests;

// VENDOR(uefi-player): re-exports follow the kernel modules (x86_64, std or
// no_std); the dispatch points (`predict.rs`, `tile/residual.rs`,
// `loop_filter.rs`) gate on the same `target_arch` condition.
#[cfg(target_arch = "x86_64")]
pub use inter::{
    block_inter_predict_avx2, block_inter_predict_avx2_w4, block_inter_predict_scaled_avx2,
};
#[cfg(target_arch = "x86_64")]
pub use loop_filter::{loop_filter_horiz8_avx2, loop_filter_vert8_avx2};
#[cfg(target_arch = "x86_64")]
pub use transform::{
    inverse_transform_adst_reconstruct_avx2, inverse_transform_adst_reconstruct_hbd_avx2,
    inverse_transform_dct_dct_reconstruct_avx2, inverse_transform_dct_dct_reconstruct_hbd_avx2,
};

#[cfg(feature = "std")]
use std::sync::OnceLock;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(feature = "std"))]
use core::sync::atomic::AtomicU8;

// VENDOR(uefi-player): the `[accel]` single-bit switch, receiving end. The
// firmware drives this from `config::copy_width()` before its first decode
// (`scalar` -> `false`, anything else -> `true`; `resolve_accel` has already
// clamped an explicit over-hardware level, so this knob carries the config
// half of the `min(config, hardware)` semantic and `avx2_enabled` adds the
// hardware half). Never reads `player.toml` here -- one resolved value in,
// no second parse. Defaults to `true` (upstream behavior: SIMD when the CPU
// can run it); the scalar path stays bit-exact either way.
static SIMD_ALLOWED: AtomicBool = AtomicBool::new(true);

/// Set the `[accel]` switch: `false` pins the scalar path (firmware
/// `accel.level = scalar`), `true` lets each decode take the wheel's max
/// (whatever `avx2_enabled` proves runnable). Idempotent; takes effect on
/// the next `avx2_enabled()` call (the hardware probe stays cached, this
/// bit is read fresh every call so a late switch cannot go stale).
pub fn set_simd_allowed(allowed: bool) {
    SIMD_ALLOWED.store(allowed, Ordering::Relaxed);
}

/// Read back the `[accel]` switch (mainly for host harnesses asserting both
/// arms of a scalar/SIMD differential run).
pub fn simd_allowed() -> bool {
    SIMD_ALLOWED.load(Ordering::Relaxed)
}

/// Whether the AVX2 fast path should be used: the `[accel]` switch allows it
/// and the CPU proves AVX2-runnable. The switch bit is read fresh per call
/// (cheap atomic); the hardware probe underneath is cached -- the probe cost
/// isn't worth paying per block (`predict::block_inter_predict` calls this
/// once per inter-predicted plane block).
///
/// `VP9DEC_NO_SIMD` (any value, checked once) forces the scalar path even on
/// an AVX2-capable machine. The full official sweep runs once with this unset
/// and once with it set; `tests/simd_scalar_differential_test.rs` also uses
/// isolated processes to compare the two modes on generated conformant
/// streams. The independent ffmpeg cross-decode is a separate acceptance
/// check.
#[cfg(feature = "std")]
pub fn avx2_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let base = *ENABLED.get_or_init(|| {
        std::env::var_os("VP9DEC_NO_SIMD").is_none() && is_x86_feature_detected!("avx2")
    });
    base && SIMD_ALLOWED.load(Ordering::Relaxed)
}

// VENDOR(uefi-player): without `std` there is no `is_x86_feature_detected!`
// (nor env knob -- `VP9DEC_NO_SIMD` reads as unset, shipped-defaults per the
// h264-common knob doctrine), so the gate is a firmware CPUID/XCR0 probe
// with the same semantics as uefi-player `src/diag/common.rs`
// (`ensure_xsave_enabled`) + `src/diag/avx_checker.rs`
// (`check_avx_support`): CR4.OSXSAVE + XCR0 AVX state + the AVX2 hardware
// bit. No second CPUID design is invented here; if the firmware gates move,
// mirror them here (and vice versa).
#[cfg(not(feature = "std"))]
pub fn avx2_enabled() -> bool {
    if !SIMD_ALLOWED.load(Ordering::Relaxed) {
        return false;
    }
    static HW_CACHE: AtomicU8 = AtomicU8::new(0); // 0 = unknown, 1 = no, 2 = yes
    match HW_CACHE.load(Ordering::Relaxed) {
        2 => true,
        1 => false,
        _ => {
            let ok = hw_avx2_probe();
            HW_CACHE.store(if ok { 2 } else { 1 }, Ordering::Relaxed);
            ok
        }
    }
}

// VENDOR(uefi-player): the no_std hardware half of `avx2_enabled` (see the
// gate comment above). UEFI discipline (root AGENTS.md + docs/simd.md):
// AVX/AVX2 runs only after the `check_avx_support` gate (CR4.OSXSAVE + XCR0);
// the kernels this guards are integer-only (no float vectors) with plain
// loads/stores (no nt, so no sfence/alignment gate needed).
#[cfg(not(feature = "std"))]
fn hw_avx2_probe() -> bool {
    // On UEFI we run at CPL0, so idempotently enable XSAVE first (semantic
    // mirror of the firmware helper -- a cross-crate call is impossible).
    // Without this, `XGETBV` below would `#UD` under OVMF's default
    // CR4.OSXSAVE=0; the firmware also ensures at boot (`resolve_accel`)
    // and before its first decode, this just makes the wheel self-safe.
    // Host userspace must NEVER touch CR4/XCR0 (would `#GP`) -- the OS owns
    // them and always enables XSAVE there, so read-only is correct.
    #[cfg(target_os = "uefi")]
    ensure_xsave_enabled();
    unsafe {
        let c1 = core::arch::x86_64::__cpuid(1);
        // AVX hardware (bit 28) + OSXSAVE present (bit 27).
        if c1.ecx & (1 << 28) == 0 || c1.ecx & (1 << 27) == 0 {
            return false;
        }
        // XCR0 AVX (ymm) state enabled -- the `check_avx_support` gate.
        if core::arch::x86_64::_xgetbv(0) & (1 << 2) == 0 {
            return false;
        }
        // AVX2 hardware (leaf 7 EBX bit 5; leaf 7 itself gated on the max
        // basic leaf, same as the firmware `probe_max_width`).
        if core::arch::x86_64::__cpuid(0).eax < 7 {
            return false;
        }
        core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 5) != 0
    }
}

// VENDOR(uefi-player): semantic mirror of uefi-player
// `src/diag/common.rs::ensure_xsave_enabled` (same leaves, same bits, same
// order: XSAVE presence -> CR4.OSXSAVE -> XCR0 SSE/AVX[/AVX512]). UEFI-only
// (CPL0 writes); host no_std builds never call this (see `hw_avx2_probe`).
// If the firmware helper changes its gates, change this identically.
#[cfg(all(not(feature = "std"), target_os = "uefi", target_arch = "x86_64"))]
fn ensure_xsave_enabled() {
    use core::arch::{
        asm,
        x86_64::{__cpuid, __cpuid_count},
    };

    let c1 = __cpuid(1);
    if c1.ecx & (1 << 26) == 0 {
        return; // hardware has no XSAVE; enabling is meaningless
    }
    let has_avx = c1.ecx & (1 << 28) != 0;
    let has_avx512 = __cpuid(0).eax >= 7 && (__cpuid_count(7, 0).ebx & (1 << 16)) != 0;

    let cr4: u64;
    unsafe {
        asm!("mov {0}, cr4", out(reg) cr4, options(nostack, preserves_flags));
    }
    if cr4 & (1 << 18) == 0 {
        unsafe {
            asm!("mov cr4, {0}", in(reg) cr4 | (1u64 << 18), options(nostack));
        }
    }

    let mut lo: u32;
    let mut hi: u32;
    unsafe {
        asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi,
            options(nostack, preserves_flags));
    }
    let mut xcr0 = ((hi as u64) << 32) | lo as u64;
    // bit1=SSE(xmm), bit2=AVX(ymm), bit5/6/7=AVX512(opmask/zmm).
    let want = (1u64 << 1)
        | if has_avx { 1u64 << 2 } else { 0 }
        | if has_avx512 {
            (1 << 5) | (1 << 6) | (1 << 7)
        } else {
            0
        };
    if xcr0 & want != want {
        xcr0 |= want;
        lo = xcr0 as u32;
        hi = (xcr0 >> 32) as u32;
        unsafe {
            asm!("xsetbv", in("ecx") 0u32, in("eax") lo, in("edx") hi, options(nostack));
        }
    }
}
