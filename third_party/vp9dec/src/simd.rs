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

// VENDOR(uefi-player): AVX2 kernels need `std` (runtime feature detection,
// below); without it only the scalar path runs (bit-exact, officially gated),
// so the kernel modules are compiled out.
#[cfg(feature = "std")]
mod inter;
#[cfg(feature = "std")]
mod loop_filter;
#[cfg(feature = "std")]
mod transform;

#[cfg(all(test, feature = "std", target_arch = "x86_64"))]
#[path = "../tests/unit/simd.rs"]
mod tests;

// VENDOR(uefi-player): re-exports only exist with the kernel modules (std).
#[cfg(feature = "std")]
pub use inter::{
    block_inter_predict_avx2, block_inter_predict_avx2_w4, block_inter_predict_scaled_avx2,
};
#[cfg(feature = "std")]
pub use loop_filter::{loop_filter_horiz8_avx2, loop_filter_vert8_avx2};
#[cfg(feature = "std")]
pub use transform::{
    inverse_transform_adst_reconstruct_avx2, inverse_transform_adst_reconstruct_hbd_avx2,
    inverse_transform_dct_dct_reconstruct_avx2, inverse_transform_dct_dct_reconstruct_hbd_avx2,
};

#[cfg(feature = "std")]
use std::sync::OnceLock;

/// Whether the AVX2 fast path should be used: the CPU supports AVX2 and it hasn't been
/// force-disabled. Cached (the feature/env probe cost isn't worth paying per block --
/// `predict::block_inter_predict` calls this once per inter-predicted plane block).
///
/// `VP9DEC_NO_SIMD` (any value, checked once) forces the scalar path even on an
/// AVX2-capable machine. The full official sweep runs once with this unset and once with
/// it set; `tests/simd_scalar_differential_test.rs` also uses isolated processes to compare
/// the two modes on generated conformant streams. The independent ffmpeg cross-decode is a
/// separate acceptance check.
#[cfg(feature = "std")]
pub fn avx2_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("VP9DEC_NO_SIMD").is_none() && is_x86_feature_detected!("avx2")
    })
}

// VENDOR(uefi-player): without `std` there is no `is_x86_feature_detected!`
// runtime probe (nor env knob), so the scalar path is always used. Scalar is
// bit-exact (official sweep runs it), so this loses speed, not correctness.
#[cfg(not(feature = "std"))]
pub fn avx2_enabled() -> bool {
    false
}
