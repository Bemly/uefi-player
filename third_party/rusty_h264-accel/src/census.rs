//! Per-twin CALL census: for every scalar/kernel twin, which tier actually ran.
//!
//! This answers one question and only one: **on real content, what share of calls
//! to each dispatcher reached a SIMD kernel, and what share fell to the scalar
//! twin?** Byte-identity cannot see the difference (the two paths agree by
//! design), a profiler cannot see it (both paths are "the same stage"), and an
//! A/B reads FLAT when an arm is not wired at all. A call count is deterministic,
//! needs one run, and is immune to load.
//!
//! Three buckets per twin, because these dispatchers are TIERED:
//!
//! * `wide`   — the widest kernel (AVX2 on x86-64).
//! * `base`   — the baseline-ISA kernel (SSE2 on x86-64, NEON on aarch64).
//! * `scalar` — the scalar twin. **Anything above zero here on a shipping arch
//!   is a finding**, and the whole point of the instrument.
//!
//! ## The label is part of the instrument
//!
//! A bucket printed as "scalar" that actually counted kernel calls produced a
//! "33% scalar" finding in this repo on 2026-09-05 that did not exist. So each
//! counter is bumped **at the branch it names**, on the line that commits to
//! that tier, never at the top of the function and never inferred afterwards.
//!
//! Off by default: without `--features census` every method below is an empty
//! inline fn on a unit struct, so the call sites cost nothing and need no `cfg`.

#[cfg(feature = "census")]
mod imp {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub struct Twin {
        pub name: &'static str,
        pub wide_n: AtomicU64,
        pub base_n: AtomicU64,
        pub scalar_n: AtomicU64,
    }

    impl Twin {
        pub const fn new(name: &'static str) -> Self {
            Twin {
                name,
                wide_n: AtomicU64::new(0),
                base_n: AtomicU64::new(0),
                scalar_n: AtomicU64::new(0),
            }
        }
        /// Widest kernel tier (AVX2).
        #[inline]
        pub fn wide(&self) {
            self.wide_n.fetch_add(1, Relaxed);
        }
        /// Baseline-ISA kernel tier (SSE2 / NEON).
        #[inline]
        pub fn base(&self) {
            self.base_n.fetch_add(1, Relaxed);
        }
        /// The scalar twin.
        #[inline]
        pub fn scalar(&self) {
            self.scalar_n.fetch_add(1, Relaxed);
        }
        pub fn read(&self) -> (u64, u64, u64) {
            (
                self.wide_n.load(Relaxed),
                self.base_n.load(Relaxed),
                self.scalar_n.load(Relaxed),
            )
        }
        pub fn reset(&self) {
            self.wide_n.store(0, Relaxed);
            self.base_n.store(0, Relaxed);
            self.scalar_n.store(0, Relaxed);
        }
    }
}

#[cfg(not(feature = "census"))]
mod imp {
    /// Zero-cost stub: every method compiles away, so instrumented dispatchers
    /// carry no `cfg` and no branch when the feature is off.
    pub struct Twin {
        pub name: &'static str,
    }
    impl Twin {
        pub const fn new(name: &'static str) -> Self {
            Twin { name }
        }
        #[inline(always)]
        pub fn wide(&self) {}
        #[inline(always)]
        pub fn base(&self) {}
        #[inline(always)]
        pub fn scalar(&self) {}
        pub fn read(&self) -> (u64, u64, u64) {
            (0, 0, 0)
        }
        pub fn reset(&self) {}
    }
}

pub use imp::Twin;

/// Declare the twins and the registry in one place, so a counter cannot exist
/// without appearing in the report (and vice versa).
macro_rules! twins {
    ($($id:ident => $label:expr),* $(,)?) => {
        $(pub static $id: Twin = Twin::new($label);)*
        /// Every twin, in report order.
        pub static ALL: &[&Twin] = &[$(&$id),*];
    };
}

twins! {
    // ---- luma motion compensation -------------------------------------------
    MC_HOR20        => "mc_hor20 (luma half-pel H)",
    MC_VER02        => "mc_ver02 (luma half-pel V)",
    MC_HOR_QPEL     => "mc_hor_qpel (luma quarter H)",
    MC_VER_QPEL     => "mc_ver_qpel (luma quarter V)",
    MC_CENTRE       => "mc_centre (luma centre HV)",
    MC_CENTRE_HQ    => "mc_centre_hq (fused centre-adj H)",
    MC_CENTRE_VQ    => "mc_centre_vq (fused centre-adj V)",
    MC_HV_QPEL      => "mc_hv_qpel (fused HV quarter)",
    PIXEL_AVG       => "pixel_avg (bi-pred average)",
    HPEL_FUSED      => "hpel_fused (half-pel plane)",
    // ---- chroma motion compensation -----------------------------------------
    MC_CHROMA_W8    => "mc_chroma_w8",
    MC_CHROMA_W4    => "mc_chroma_w4",
    // ---- residual reconstruction --------------------------------------------
    IDCT4X4_ADD     => "idct4x4_add (IDCT+add)",
    FLAT_ADD_4X4    => "flat_add_4x4 (DC-only add)",
    IDCT4X4_DEQ_ADD => "idct4x4_deq_add (FUSED deq+IDCT+add)",
    LUMA_DC_SCAN    => "luma_dc_from_scan (I16 luma DC)",
    NNZ_RASTER      => "nnz_raster_from_z",
    IDCT_FOUR_T4    => "idct_four_t4_rec (4x4 batch)",
    DEQUANT_4X4     => "dequant_4x4",
    // ---- deblocking ----------------------------------------------------------
    DBLK_LUMA_LT4_V => "deblock_luma_lt4_v",
    DBLK_LUMA_EQ4_V => "deblock_luma_eq4_v",
    DBLK_LUMA_LT4_H => "deblock_luma_lt4_h",
    DBLK_LUMA_EQ4_H => "deblock_luma_eq4_h",
    DBLK_CHR_LT4_V  => "deblock_chroma_lt4_v",
    DBLK_CHR_EQ4_V  => "deblock_chroma_eq4_v",
    DBLK_CHR_LT4_H  => "deblock_chroma_lt4_h",
    DBLK_CHR_EQ4_H  => "deblock_chroma_eq4_h",
    // ---- boundary-strength derivation ---------------------------------------
    BS_MASKS        => "bs_motion_masks (single-list)",
    BS_MASKS_2L     => "bs_motion_masks_two_list",
    MB_UNIFORM      => "mb_uniform",
    // ---- boundary-strength DERIVATION (scalar side of the deblock stage) ----
    DRV_MB_KIND     => "derive_mb_kind (MbBs form)",
    DRV_MB_KIND_INTO=> "derive_mb_kind_into (i32 form)",
    DRV_MB_GENERAL  => "derive_mb_general (blind fallback)",
    DRV_MB_BS       => "derive_mb_bs (blind tile)",
    DRV_MB_PACKED   => "derive_mb_packed",
    DRV_GATHER_TILE => "gather_tile",
    DRV_MB_RECORDS  => "derive_mb_records",
    DRV_PACK_MB     => "pack_mb",
}

/// Zero every counter.
pub fn reset() {
    for t in ALL {
        t.reset();
    }
}

/// Visit every twin as `(label, wide, base, scalar)`.
pub fn each(f: &mut dyn FnMut(&'static str, u64, u64, u64)) {
    for t in ALL {
        let (w, b, s) = t.read();
        f(t.name, w, b, s);
    }
}
