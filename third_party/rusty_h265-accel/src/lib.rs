//! SIMD kernels for [`rusty_h265`](https://crates.io/crates/rusty_h265).
//!
//! The decoder core is `#![forbid(unsafe_code)]`; this crate is the seam where
//! acceleration is allowed to use `unsafe`, and it is confined to
//! `#[target_feature]` functions whose bounds the safe dispatcher proves.
//!
//! # Shape
//!
//! Every kernel comes in a set:
//!
//! - `*_scalar` — plain safe Rust. **The oracle and the fallback**, compiled on
//!   every target, and the only implementation when the `simd` feature is off.
//! - `*_sse2` / `*_avx2` (x86_64), `*_neon` (aarch64) — the twins. SSE2 and
//!   NEON are *baseline* on their architectures, so they need no runtime
//!   detection and no second code path production might never run; AVX2 is
//!   detected once and cached.
//! - a safe `pub fn` dispatcher that picks one and documents why the pointer
//!   arithmetic inside is in bounds.
//!
//! Every kernel is integer and exact: the SIMD twin performs the same
//! operations in the same order as the scalar one, so the gate is
//! `assert_eq!`, not a tolerance. `*_matches_scalar` tests pin that.
//!
//! # Where the dispatch sits
//!
//! A `#[target_feature]` function cannot be inlined into a caller that lacks
//! the feature, so the call is real. Every entry point here therefore takes a
//! whole **block** (a prediction unit, a transform block, a CTB row of SAO) —
//! the loop lives inside the feature boundary, never outside it.

#![cfg_attr(not(feature = "simd"), forbid(unsafe_code))]
#![deny(unsafe_op_in_unsafe_fn)]
// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): always in scope; `use alloc::…` resolves in both builds.
extern crate alloc;
// VENDOR(uefi-player): core atomics resolve in both builds (std links core;
// the per-module `std::sync::atomic` imports below became this one shape).
use core::sync::atomic::{AtomicBool, Ordering};

pub mod deblock;
pub mod intra;
pub mod itx;
pub mod mc;
pub mod pixel;
pub mod sao;

/// The widest instruction set this build may use at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Isa {
    /// Portable Rust. Every target, and the oracle everywhere.
    Scalar,
    /// x86-64 baseline (SSE2) or aarch64 baseline (NEON) — always available.
    Baseline,
    /// SSE4.1 (2007 and later on x86-64).
    ///
    /// A rung, not a rounding: the inverse-transform kernels need `pmulld`,
    /// `pmovsxwd` and `pminsd`/`pmaxsd`, none of which exist in SSE2, and all
    /// of which arrive here. Without this variant every pre-AVX2 machine took
    /// the scalar twin for the whole transform.
    ///
    /// The ordering is load-bearing: kernels ask `isa() >= Isa::Sse41`, and the
    /// `_` arm of an existing `match` on `Isa::Avx2` routes this to the SSE2
    /// kernel, which is correct — SSE4.1 is a superset.
    Sse41,
    /// AVX2 + FMA-era x86-64.
    Avx2,
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod detect {
    use super::Isa;
    // VENDOR(uefi-player): `std::sync::atomic` => `core` (same type, both builds).
    use core::sync::atomic::{AtomicU8, Ordering};

    static CACHE: AtomicU8 = AtomicU8::new(u8::MAX);
    /// A cap set before first use, for harnesses that need to vary the ISA per
    /// ARM. An env var cannot do that: both arms of a paired A/B run in the
    /// same environment, so the comparison silently becomes a null arm.
    static FORCED: AtomicU8 = AtomicU8::new(u8::MAX);

    /// Cap the ISA. Must be called before the first kernel dispatch; later calls
    /// are ignored, because the detected level is cached on first use.
    pub fn force_cap(level: u8) {
        FORCED.store(level, Ordering::Relaxed);
        CACHE.store(u8::MAX, Ordering::Relaxed);
    }

    fn probe() -> u8 {
        // `RH265_ISA` caps the detected level: `baseline`/`sse2`, or `sse41`.
        //
        // Without it the SSE4.1 rung could not be exercised on a developer
        // machine that has AVX2, which is every machine here — and an arm no
        // test can reach is an arm nobody has verified. This is the same reason
        // every kernel keeps a `RH265_SCALAR_*` switch.
        //
        // `scalar` is REFUSED, loudly, and used to be accepted as a synonym for
        // `baseline`. It is not one: level 0 is the SSE2 rung, so
        // `RH265_ISA=scalar` ran vector kernels while announcing that it did
        // not. Measured that way the vector path looked worth 3%; with the
        // `simd` feature actually off it is worth **2.81x** (7,554 ms against
        // 2,685 ms on a 20-second 720p clip). An arm that silently does not do
        // what its name says is worse than no arm at all -- every conclusion
        // drawn from it is wrong in the confident direction. The real scalar
        // build is `--no-default-features`.
        // VENDOR(uefi-player): no_std => the env knob reads as unset (uncapped)
        // and the hardware level is assumed max. The firmware owns the ONLY
        // detection set (`config::probe_max_width` over CPUID/XCR0, after
        // `ensure_xsave_enabled`) and pushes its verdict via `force_isa` /
        // `force_scalar` before the first decode, so `FORCED` below is the
        // real gate. Same values as a host run with a clean environment on a
        // machine the firmware gate already approved. Do NOT grow a second
        // CPUID/XCR0 probe here.
        #[cfg(feature = "std")]
        let cap = match std::env::var("RH265_ISA").as_deref() {
            Ok("scalar") => panic!(
                "RH265_ISA=scalar does not select the scalar kernels -- level 0 is the SSE2 rung.                  Build with --no-default-features for a genuinely scalar decoder."
            ),
            Ok("baseline") | Ok("sse2") => 0,
            Ok("sse41") => 1,
            _ => 2,
        };
        #[cfg(not(feature = "std"))]
        let cap = 2u8;
        #[cfg(feature = "std")]
        let have = if std::is_x86_feature_detected!("avx2") {
            2
        } else if std::is_x86_feature_detected!("sse4.1") {
            1
        } else {
            0
        };
        // VENDOR(uefi-player): no_std => assumed max (see above); `forced`
        // clamps it to what the firmware vetted.
        #[cfg(not(feature = "std"))]
        let have = 2u8;
        let forced = FORCED.load(Ordering::Relaxed);
        // VENDOR(uefi-player): fail loud in debug if the firmware forgot the
        // push — otherwise AVX2 would execute unvetted. Release keeps the
        // assumed-max (documented contract, see VENDOR.md).
        #[cfg(not(feature = "std"))]
        debug_assert!(forced != u8::MAX, "rusty_h265-accel: no_std+simd probed before the firmware pushed an ISA cap (resolve_accel must run before first decode)");
        have.min(cap).min(if forced == u8::MAX { u8::MAX } else { forced })
    }

    pub fn isa() -> Isa {
        // Detected once; afterwards a relaxed byte load. Callers hoist this
        // above their loops regardless (see the module docs).
        match CACHE.load(Ordering::Relaxed) {
            0 => Isa::Baseline,
            1 => Isa::Sse41,
            2 => Isa::Avx2,
            _ => {
                let v = probe();
                CACHE.store(v, Ordering::Relaxed);
                match v {
                    2 => Isa::Avx2,
                    1 => Isa::Sse41,
                    _ => Isa::Baseline,
                }
            }
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod detect {
    use super::Isa;
    /// NEON is mandatory on aarch64.
    pub fn isa() -> Isa {
        Isa::Baseline
    }
}

#[cfg(not(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod detect {
    use super::Isa;
    pub fn isa() -> Isa {
        Isa::Scalar
    }
}

/// The instruction set the kernels will use on this machine. Cached; safe to
/// call often, but hoist it above a hot loop anyway.
#[inline]
pub fn isa() -> Isa {
    // VENDOR(uefi-player): firmware scalar park (see `force_scalar`); every
    // dispatcher treats `Isa::Scalar` as "take the scalar twin".
    if FORCE_SCALAR.load(Ordering::Relaxed) {
        return Isa::Scalar;
    }
    detect::isa()
}

// VENDOR(uefi-player): the firmware off-switch for `[accel] level=scalar`.
// Upstream `force_isa(Scalar)` collapses to the SSE2 rung (level 0 IS that
// rung, see `probe`); parking here is the true runtime off-switch. Applies to
// every build (scalar builds already sit at `Scalar`, where this is a no-op).
// Must be called before the first kernel dispatch; `resolve_accel` does.
static FORCE_SCALAR: AtomicBool = AtomicBool::new(false);

/// Park every dispatcher on its scalar twin, before the first kernel dispatch.
///
/// The firmware calls this when `[accel] level=scalar`; anything else goes
/// through [`force_isa`] (capped by the firmware gate). A later [`force_isa`]
/// cap does NOT lift the park — re-resolve from a clean boot instead.
pub fn force_scalar() {
    FORCE_SCALAR.store(true, Ordering::Relaxed);
}

/// Cap the ISA programmatically, before the first kernel dispatch.
///
/// `RH265_ISA` caps it too, but an environment variable cannot vary per ARM:
/// both arms of a paired A/B run in the same environment, so setting it turns
/// the comparison into a null arm without saying so. A harness that wants to
/// measure AVX2 against SSE4.1 needs this.
pub fn force_isa(cap: Isa) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    detect::force_cap(match cap {
        Isa::Scalar | Isa::Baseline => 0,
        Isa::Sse41 => 1,
        Isa::Avx2 => 2,
    });
    let _ = cap;
}

/// One line for a benchmark's method output, and for the reachability census:
/// which kernels a run will actually execute.
pub fn describe() -> &'static str {
    match isa() {
        Isa::Avx2 => "rusty_h265-accel: AVX2",
        Isa::Sse41 => "rusty_h265-accel: SSE4.1",
        Isa::Baseline => {
            if cfg!(target_arch = "x86_64") {
                "rusty_h265-accel: SSE2"
            } else if cfg!(target_arch = "aarch64") {
                "rusty_h265-accel: NEON"
            } else {
                "rusty_h265-accel: baseline"
            }
        }
        // VENDOR(uefi-player): was "scalar (simd feature off)"; the firmware
        // parks here at runtime via `force_scalar` with simd compiled in.
        Isa::Scalar => "rusty_h265-accel: scalar",
    }
}

/// Deterministic per-kernel call counters — the byte census that
/// `codec-vectorize-kernel`'s REACHABILITY step demands. A kernel with a test
/// and a benchmark but a zero here is not deployed, whatever the call graph
/// looks like. Enabled with the `census` feature or `RH265_CENSUS=1`.
pub mod census {
    #[cfg(feature = "std")]
    use std::sync::atomic::{AtomicU64, Ordering};
    // VENDOR(uefi-player): no_std => core atomics (U64 is native on x86_64/aarch64).
    #[cfg(not(feature = "std"))]
    use core::sync::atomic::{AtomicU64, Ordering};
    // VENDOR(uefi-player): alloc prelude for the no_std build.
    #[cfg(not(feature = "std"))]
    use alloc::{vec, vec::Vec};

    macro_rules! counters {
        ($($name:ident),* $(,)?) => {
            $(pub static $name: AtomicU64 = AtomicU64::new(0);)*

            /// Every counter with its current value, for reporting.
            pub fn snapshot() -> Vec<(&'static str, u64)> {
                vec![$((stringify!($name), $name.load(Ordering::Relaxed))),*]
            }

            /// Zero every counter.
            pub fn reset() {
                $($name.store(0, Ordering::Relaxed);)*
            }
        };
    }

    counters!(
        MC_LUMA_SCALAR,
        MC_LUMA_SIMD,
        MC_CHROMA_SCALAR,
        MC_CHROMA_SIMD,
        MC_EDGE_PAD,
        MC_COPY_SCALAR,
        PUT_BI_FP_SCALAR,
        PUT_BI_FP_SIMD,
        MC_FULLPEL_UNI,
        MC_FULLPEL_BI,
        MC_COPY_SIMD,
        PUT_UNI_SCALAR,
        PUT_UNI_SIMD,
        PUT_BI_SCALAR,
        PUT_BI_SIMD,
        TX_SKIP_SCALAR,
        TX_SKIP_SIMD,
        PUT_WEIGHTED_SCALAR,
        PUT_WEIGHTED_SIMD,
        ADD_RESIDUAL_SCALAR,
        ADD_RESIDUAL_SIMD,
        ADD_RESIDUAL_DC_SCALAR,
        ADD_RESIDUAL_DC_SIMD,
        SAO_BAND_SCALAR,
        SAO_BAND_SIMD,
        SAO_EDGE_SCALAR,
        SAO_EDGE_SIMD,
        INTRA_ANGULAR_SCALAR,
        INTRA_ANGULAR_SIMD,
        INTRA_PLANAR_SCALAR,
        INTRA_PLANAR_SIMD,
        INTRA_DC_SCALAR,
        INTRA_DC_SIMD,
        SAMPLES_SAO,
        SAMPLES_MC,
        SAMPLES_ADD_RESIDUAL,
        SAMPLES_INTRA,
        // ---- content-adaptive ROUTES (the Great Gate inventory) -----------
        // Every one of these is a population count: which arm the content
        // actually selected. A route with a population and no fast arm is a
        // missing kernel; a route with a zero population is unreachable code.
        // Motion compensation, by fractional position (§8.5.3.3.3).
        RT_MC_FULLPEL,
        RT_MC_HORIZ,
        RT_MC_VERT,
        RT_MC_2D,
        // ...and by the shift the bit depth implies.
        RT_MC_SHIFT0,
        RT_MC_SHIFTN,
        // Residual, by transform kind (§8.6.4) and size.
        MC_FW_LT8,
        MC_FW_8,
        MC_FW_GE16,
        INTRA_N_LT8,
        INTRA_N_GE8,
        PIX_W_LT8,
        PIX_W_8,
        PIX_W_GE16,
        ITX_FUSED_SIMD,
        ITX_FUSED_SCALAR,
        ITX_SHIFT_SIMD,
        ITX_SHIFT_SCALAR,
        ITX_ACCUM_SIMD,
        ITX_ACCUM_SCALAR,
        DEBLOCK_LUMA_SIMD,
        DEBLOCK_LUMA_SCALAR,
        RT_DEBLOCK_SKIP,
        RT_DEBLOCK_STRONG,
        RT_DEBLOCK_WEAK,
        CABAC_BYPASS_CALLS,
        CABAC_BYPASS_BINS,
        // CABAC bin populations, and the per-block work the residual
        // parser does around them. These are the instruments for the CABAC
        // campaign: every win below is a counter that goes down.
        CABAC_CTX_BINS,
        CABAC_TERM_BINS,
        RES_BLOCKS,
        RES_FILL_STORES,
        RES_SCAN_SEARCH,
        RES_SIG_SCANNED,
        RES_SIG_CTX,
        MC_FIR_H_ROWS,
        RT_MC_VSYM,
        RT_MC_VGEN,
        RT_MC_TAP_ELIDE,
        RT_MC_TAP_FULL,
        RT_MC_FY_NONE,
        RT_MC_FY_SYM,
        RT_MC_FY_ASYM,
        RT_TX_DC_ONLY,
        RT_TX_GENERAL,
        RT_TX_BYPASS,
        RT_TX_SKIP,
        RT_TX_DST,
        RT_TX_DCT,
        RT_TX_N4,
        RT_TX_N8,
        RT_TX_N16,
        RT_TX_N32,
        RT_TX_SCALED,
        RT_TX_FLAT,
        // Sparsity: coefficients the last-significant position says are live,
        // against the dense block they sit in.
        RT_TX_NZ_AREA,
        RT_TX_FULL_AREA,
        // Intra, by direction and by whether the references needed work.
        RT_INTRA_ANG_ROW,
        RT_INTRA_ANG_TRANSPOSED,
        RT_INTRA_ANG_COPY,
        RT_INTRA_REF_FILTERED,
        RT_INTRA_REF_PLAIN,
        RT_INTRA_ALL_AVAIL,
        RT_INTRA_SUBSTITUTED,
        RT_INTRA_DC_DEFERRED,
        RT_INTRA_DC_FILLED,
        // Loop filters.
        RT_SAO_OFF,
        RT_SAO_BAND,
        RT_SAO_EDGE,
        RT_SAO_INTERIOR,
        RT_SAO_RING,
        RT_SAO_PIC_BYPASS,
        RT_DEBLOCK_LUMA,
        RT_DEBLOCK_CHROMA,
        // Picture-level capability.
        RT_PIC_TILES,
        RT_PIC_WPP,
        RT_PIC_10BIT,
        RT_PIC_8BIT,
    );

    /// `true` only when the crate was built with the `census` feature.
    ///
    /// A `const`, so a bump guarded by it compiles away to nothing when it is
    /// false. [`enabled`] cannot do that: its `OnceLock` read is an atomic load
    /// plus a branch, which is fine per kernel call and is *itself* the cost
    /// being measured on a path as hot as one CABAC bin. Instrument per-bin and
    /// per-coefficient sites with this; use `enabled()` per block or coarser.
    pub const ALWAYS: bool = cfg!(feature = "census");

    /// True when the counters should be updated. Read once and hoisted by the
    /// caller; the kernels themselves check a `const` in release builds where
    /// the feature is off, so an unused census costs nothing.
    #[inline]
    pub fn enabled() -> bool {
        #[cfg(feature = "census")]
        {
            true
        }
        #[cfg(not(feature = "census"))]
        {
            // VENDOR(uefi-player): without std there is no environment, so the
            // knob reads as unset and census stays off (shipped default).
            #[cfg(feature = "std")]
            {
                static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *ON.get_or_init(|| std::env::var_os("RH265_CENSUS").is_some())
            }
            #[cfg(not(feature = "std"))]
            {
                false
            }
        }
    }

    #[inline]
    pub fn bump(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    /// Records which arm of a two-way route the content selected.
    ///
    /// The Great Gate's decoder rule: a route is only a gate once its
    /// POPULATION is measured. `route(cond, a, b)` is the one-liner that makes
    /// every dispatch site say which way it went, so an arm with no population
    /// (unreachable) and a population with no fast arm (a missing kernel) both
    /// show up in the same table.
    #[inline]
    pub fn route(taken: bool, yes: &AtomicU64, no: &AtomicU64) {
        if enabled() {
            bump(if taken { yes } else { no }, 1);
        }
    }

    /// Records one arm of a route with more than two arms.
    #[inline]
    pub fn arm(c: &AtomicU64) {
        if enabled() {
            bump(c, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isa_is_stable_and_described() {
        let a = isa();
        for _ in 0..4 {
            assert_eq!(isa(), a);
        }
        assert!(describe().starts_with("rusty_h265-accel:"));
    }
}
