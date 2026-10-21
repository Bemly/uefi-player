//! SIMD acceleration for rusty_h264 — **pure Rust, no assembly**. The vendored
//! openh264 NASM this crate grew around was fully ripped on 2026-08-12; every
//! kernel is now portable Rust (x86-64 SSE2/AVX2 intrinsics + aarch64 NEON +
//! scalar oracles), no `nasm`, no build script, no FFI.
//!
//! This crate is deliberately **not** `#![forbid(unsafe_code)]`: it is the one place
//! `unsafe` lives, behind safe wrappers, so the codec core stays `forbid(unsafe)`.
//!
//! ## Structure, and where it is going
//!
//! `docs/add_SIMD_rip_ASM.md` is ripping the assembly out kernel by kernel. Two things
//! follow from that, and this file is arranged around them:
//!
//! * **`x86_asm`** holds everything still backed by openh264 NASM. It is gated on
//!   `target_arch = "x86_64"` and shrinks with every phase of the campaign.
//! * **Portable modules** (`chroma_mc`, …) hold Rust intrinsics with an x86-64 path, an
//!   aarch64 NEON path, and a scalar reference that all three are tested bit-identical
//!   against. These compile and run on **every** architecture.
//!
//! Until the campaign finishes, the crate is a mix. The whole crate used to be
//! `#![cfg(target_arch = "x86_64")]` — compiled to nothing on ARM, which is why aarch64
//! ran fully scalar. That gate now sits on the `x86_asm` module alone, so portable
//! kernels reach ARM as they land.
//!
//! **Order matters: replace, then rip.** The vendored assembly measures ~1.94x on decode
//! (paired, N=5, 34/35 reps above 1.0), so deleting a kernel before its portable
//! replacement is bit-identical and no slower would ship a real regression.
//!
//! openh264 asm is BSD-2 licensed; attribution lives in `vendor/LICENSE.openh264`.
#![allow(non_snake_case)]
// VENDOR(uefi-player): no_std port. `--features std` restores exact upstream
// behavior; without it this crate is `#![no_std]` + `alloc` (`default` is
// empty on purpose — see Cargo.toml/VENDOR.md).
#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): always in scope; `use alloc::…` resolves in both builds.
// `macro_use` only for the no_std test build, so upstream's std tests (`vec!`
// etc.) also compile against the no_std default. Never affects firmware.
#[cfg_attr(all(test, not(feature = "std")), macro_use)]
extern crate alloc;
// VENDOR(uefi-player): test-only `eprintln!` no-op, same reason (no stdout in
// no_std; the two call sites only print skip notes). Never shipped.
#[cfg(all(test, not(feature = "std")))]
macro_rules! eprintln {
    ($($t:tt)*) => {{
        let _ = ::core::format_args!($($t)*);
    }};
}
// VENDOR(uefi-player): firmware single-bit SIMD master switch. Upstream has no
// such seam (its only runtime arms are the per-kernel AVX2 probes + the
// knobs-gated `tq_scalar_forced`); the firmware needs one on/off bit driven by
// `config::copy_width()` (`scalar` => off, anything else => on).
// `core` atomics resolve in both builds. Initial is ON unconditionally:
// upstream has the kernels live with no off switch, and `default` is empty
// (see Cargo.toml/VENDOR.md — `common`'s dep edge would otherwise drag `std`
// into the firmware), so host default builds must behave upstream-identically
// without anyone pushing. In firmware `resolve_accel` pushes unconditionally
// at boot before any decode can run, so the default never executes unpushed.
static SIMD_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// Park (`false`) or release (`true`) every decode-reachable SIMD kernel.
///
/// Off routes each entry to its scalar twin (`hpel_fused`/`dequant_4x4` report
/// `false`, `bs_motion_masks*`/`mb_uniform` report `None` — the `common` caller
/// keeps its own scalar fallback either way). Must be called before the first
/// decode; `resolve_accel` does it at boot.
pub fn set_simd_enabled(en: bool) {
    SIMD_ENABLED.store(en, core::sync::atomic::Ordering::Relaxed);
}

/// The firmware switch above. Checked at the top of every decode-reachable entry.
#[inline]
pub(crate) fn simd_enabled() -> bool {
    SIMD_ENABLED.load(core::sync::atomic::Ordering::Relaxed)
}

// VENDOR(uefi-player): one line for the firmware serial log (`accel: h264
// decode isa = …`), mirroring `rusty_h265::accel::describe()` so QEMU 矩阵
// can assert the single-bit push. On means the SIMD twins are live (SSE2
// baseline + AVX2 where the firmware vetted it); off means scalar twins.
pub fn describe() -> &'static str {
    if simd_enabled() {
        "rusty_h264-accel: SIMD"
    } else {
        "rusty_h264-accel: scalar"
    }
}

// VENDOR(uefi-player): runtime-detection policy. std keeps upstream's exact
// probe; no_std assumes max (see VENDOR.md §固件门契约) — the firmware owns the
// only detection set (`probe_max_width` over CPUID/XCR0 after
// `ensure_xsave_enabled`) and pushes the verdict via `set_simd_enabled`
// before first decode. Do NOT grow a second CPUID/XCR0 probe here.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn have_avx2() -> bool {
    #[cfg(feature = "std")]
    {
        std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(feature = "std"))]
    {
        true
    }
}

/// SSE4.1 tier (the `idct4x4` fused kernels). Same policy as [`have_avx2`];
/// split out so the call sites keep naming the tier they need.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn have_sse41() -> bool {
    #[cfg(feature = "std")]
    {
        std::is_x86_feature_detected!("sse4.1")
    }
    #[cfg(not(feature = "std"))]
    {
        true
    }
}

/// NEON is the aarch64 baseline. Same std/no_std split as [`have_avx2`].
#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) fn have_neon() -> bool {
    #[cfg(feature = "std")]
    {
        std::arch::is_aarch64_feature_detected!("neon")
    }
    #[cfg(not(feature = "std"))]
    {
        true
    }
}

// --- portable: every architecture --------------------------------------------------
pub mod census;
mod chroma_mc;
pub use chroma_mc::{mc_chroma_w4, mc_chroma_w8};
mod deblock_simd;
pub use deblock_simd::{
    deblock_chroma_eq4_h, deblock_chroma_eq4_v, deblock_chroma_lt4_h, deblock_chroma_lt4_v,
    deblock_luma_eq4_h, deblock_luma_eq4_v, deblock_luma_lt4_h, deblock_luma_lt4_v,
};
mod luma_mc;
mod satd_sad;
// X1 port (11.15): the fused half-pel builder is portable now — AVX2 on
// x86-64, NEON on aarch64, `false` elsewhere (caller falls back to tiles).
mod hpel;
pub use hpel::hpel_fused;
// Transform/quant: scalar oracle + hand-written SSE2 (the first scalar-only cut
// measured 1.253x vs the assembly; the SSE2 rewrite reads ~1.02x under load —
// see docs/add_SIMD_rip_ASM.md for the ledger).
mod idct4x4;
pub use idct4x4::{flat_add_4x4, idct4x4_add, idct4x4_deq_add, luma_dc_from_scan, nnz_raster_from_z};
mod transform_quant;
pub use transform_quant::{dct_four_t4, idct_four_t4_rec, quant_four_4x4};
// Oracle surface (H10): the scalar twins + the runtime arm knob, public so the
// out-of-process liveness gate (`tests/tq_scalar_arm.rs`) and field bisection
// can reach them. Not a kernel API — census-exempt as oracle machinery.
pub use transform_quant::{
    dct_four_t4_scalar, idct_four_t4_rec_scalar, quant_four_4x4_scalar, tq_scalar_forced,
};
mod intra_pred;
pub use intra_pred::{chroma8x8_pred, i16x16_luma_pred};
pub use satd_sad::{
    sad_16x16, sad_16x8, sad_8x16, satd_16x16, satd_16x8, satd_4x4, satd_8x16, satd_8x8,
};
pub use luma_mc::{mc_centre, mc_hor20, mc_hor_qpel, mc_ver02, mc_ver_qpel, pixel_avg, mc_centre_hq, mc_centre_vq, mc_hv_qpel};

// --- still assembly-backed: x86-64 only ---------------------------------------------
#[cfg(target_arch = "x86_64")]
mod x86_asm;
#[cfg(target_arch = "x86_64")]
pub use x86_asm::*;
