//! **Per-block 4x4 inverse transform + residual add** for the DECODER (i32 exact).
//!
//! The decoder's dequantised coefficients are i32 (a CAVLC level times a scale
//! factor shifted by qp/6 can exceed i16), so the encoder's i16 `idct_four_t4_rec`
//! cannot serve it bit-exactly. This kernel is the SIMD twin of
//! `common::transform::inverse_core` + add + clip: four coefficient rows in
//! registers, an in-register 4x4 transpose (8 shuffles) around each 1-D pass,
//! `(v + 32) >> 6`, then the prediction added in saturating 16-bit and packed to
//! bytes. No scalar gathers, no stack traffic. Bit-identical to the scalar form
//! on every input: the saturating chain `packs_epi32 -> adds_epi16 -> packus`
//! is monotone and lands on exactly `clamp(pred + res, 0, 255)`.
//!
//! Why not the batched `inverse_dct_blocks`: lane-per-block layout needs a
//! 16 x 8 scalar gather AND scatter per eight blocks -- ~99 instructions per
//! block against ~60 for the scalar butterflies, measured as a LOSS on all-intra
//! content (2026-09-05). Per-block with register transposes is ~68.

#[inline]
fn inv_1d(d0: i32, d1: i32, d2: i32, d3: i32) -> (i32, i32, i32, i32) {
    let (e0, e1) = (d0.wrapping_add(d2), d0.wrapping_sub(d2));
    let (e2, e3) = ((d1 >> 1).wrapping_sub(d3), d1.wrapping_add(d3 >> 1));
    (e0.wrapping_add(e3), e1.wrapping_add(e2), e1.wrapping_sub(e2), e0.wrapping_sub(e3))
}

/// Scalar oracle: `inverse_core` + add + clip, row-major 4x4 into strided planes.
pub fn idct4x4_add_scalar(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    let mut m = *coeffs;
    for r in 0..4 {
        let (a, b, c, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
    for r in 0..4 {
        let src = &pred[p_off + r * p_stride..][..4];
        let dst = &mut rec[r_off + r * r_stride..][..4];
        for c in 0..4 {
            let res = m[r * 4 + c].wrapping_add(32) >> 6;
            dst[c] = (src[c] as i32).wrapping_add(res).clamp(0, 255) as u8;
        }
    }
}

/// Scalar oracle of the flat (DC-only) add.
pub fn flat_add_4x4_scalar(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    for r in 0..4 {
        let src = &pred[p_off + r * p_stride..][..4];
        let dst = &mut rec[r_off + r * r_stride..][..4];
        for c in 0..4 {
            dst[c] = (src[c] as i32).wrapping_add(rval).clamp(0, 255) as u8;
        }
    }
}

/// `rec[4x4] = clip(pred[4x4] + idct(coeffs))`. Panics (like slice indexing) if a
/// 4x4 window at the given offsets/strides does not fit either plane.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn idct4x4_add(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    assert!(p_off + 3 * p_stride + 4 <= pred.len() && r_off + 3 * r_stride + 4 <= rec.len());
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::IDCT4X4_ADD.scalar();
        return idct4x4_add_scalar(coeffs, pred, p_off, p_stride, rec, r_off, r_stride);
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: both 4x4 windows are inside their planes (asserted above); coeffs is a fixed [i32; 16].
        crate::census::IDCT4X4_ADD.base();
        return unsafe { x86::idct4x4_add_sse2(coeffs, pred, p_off, p_stride, rec, r_off, r_stride) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: as above.
        crate::census::IDCT4X4_ADD.base();
        return unsafe { arm::idct4x4_add_neon(coeffs, pred, p_off, p_stride, rec, r_off, r_stride) };
    }
    #[allow(unreachable_code)]
    crate::census::IDCT4X4_ADD.scalar();
    idct4x4_add_scalar(coeffs, pred, p_off, p_stride, rec, r_off, r_stride)
}

/// `rec[4x4] = clip(pred[4x4] + rval)` -- the DC-only residual.
#[inline]
pub fn flat_add_4x4(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    assert!(p_off + 3 * p_stride + 4 <= pred.len() && r_off + 3 * r_stride + 4 <= rec.len());
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::FLAT_ADD_4X4.scalar();
        return flat_add_4x4_scalar(rval, pred, p_off, p_stride, rec, r_off, r_stride);
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: windows asserted in bounds.
        crate::census::FLAT_ADD_4X4.base();
        return unsafe { x86::flat_add_4x4_sse2(rval, pred, p_off, p_stride, rec, r_off, r_stride) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: as above.
        crate::census::FLAT_ADD_4X4.base();
        return unsafe { arm::flat_add_4x4_neon(rval, pred, p_off, p_stride, rec, r_off, r_stride) };
    }
    #[allow(unreachable_code)]
    crate::census::FLAT_ADD_4X4.scalar();
    flat_add_4x4_scalar(rval, pred, p_off, p_stride, rec, r_off, r_stride)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    /// 4x4 i32 transpose: rows in, columns out (8 shuffles).
    #[inline(always)]
    pub(super) unsafe fn transpose4(r0: __m128i, r1: __m128i, r2: __m128i, r3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let t0 = _mm_unpacklo_epi32(r0, r1);
        let t1 = _mm_unpackhi_epi32(r0, r1);
        let t2 = _mm_unpacklo_epi32(r2, r3);
        let t3 = _mm_unpackhi_epi32(r2, r3);
        (
            _mm_unpacklo_epi64(t0, t2),
            _mm_unpackhi_epi64(t0, t2),
            _mm_unpacklo_epi64(t1, t3),
            _mm_unpackhi_epi64(t1, t3),
        )
    }

    /// Lane-wise `inv_1d` over four vectors.
    #[inline(always)]
    pub(super) unsafe fn inv1d(d0: __m128i, d1: __m128i, d2: __m128i, d3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let e0 = _mm_add_epi32(d0, d2);
        let e1 = _mm_sub_epi32(d0, d2);
        let e2 = _mm_sub_epi32(_mm_srai_epi32::<1>(d1), d3);
        let e3 = _mm_add_epi32(d1, _mm_srai_epi32::<1>(d3));
        (_mm_add_epi32(e0, e3), _mm_add_epi32(e1, e2), _mm_sub_epi32(e1, e2), _mm_sub_epi32(e0, e3))
    }

    /// Two rows of 16-bit residual (8 lanes: row a then row b) + the two 4-byte
    /// prediction rows, saturating, packed and stored as 2 x 4 bytes.
    #[inline(always)]
    pub(super) unsafe fn add2rows(res: __m128i, pa: *const u8, pb: *const u8, ra: *mut u8, rb: *mut u8) {
        let zero = _mm_setzero_si128();
        let a = _mm_cvtsi32_si128((pa as *const i32).read_unaligned());
        let b = _mm_cvtsi32_si128((pb as *const i32).read_unaligned());
        let p8 = _mm_unpacklo_epi32(a, b); // 8 bytes: row a, row b
        let p16 = _mm_unpacklo_epi8(p8, zero);
        let s = _mm_adds_epi16(res, p16);
        let o = _mm_packus_epi16(s, s);
        (ra as *mut i32).write_unaligned(_mm_cvtsi128_si32(o));
        (rb as *mut i32).write_unaligned(_mm_cvtsi128_si32(_mm_srli_si128::<4>(o)));
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn idct4x4_add_sse2(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let c = coeffs.as_ptr() as *const __m128i;
        let (r0, r1, r2, r3) = (_mm_loadu_si128(c), _mm_loadu_si128(c.add(1)), _mm_loadu_si128(c.add(2)), _mm_loadu_si128(c.add(3)));
        // Row pass runs ALONG each row = across lanes: transpose so lanes index rows.
        let (c0, c1, c2, c3) = transpose4(r0, r1, r2, r3);
        let (a0, a1, a2, a3) = inv1d(c0, c1, c2, c3);
        // Column pass runs along each column: transpose back, lanes index columns.
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = inv1d(t0, t1, t2, t3);
        let k = _mm_set1_epi32(32);
        let rnd = |v: __m128i| _mm_srai_epi32::<6>(_mm_add_epi32(v, k));
        let r01 = _mm_packs_epi32(rnd(o0), rnd(o1));
        let r23 = _mm_packs_epi32(rnd(o2), rnd(o3));
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        add2rows(r01, pp, pp.add(p_stride), rp, rp.add(r_stride));
        add2rows(r23, pp.add(2 * p_stride), pp.add(3 * p_stride), rp.add(2 * r_stride), rp.add(3 * r_stride));
    }

    #[inline(always)]
    pub(super) unsafe fn flat_add_4x4_sse2(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        // Saturate the flat residual to i16 once; the saturating chain below then
        // lands on clamp(pred + rval, 0, 255) exactly.
        let v = _mm_set1_epi32(rval);
        let res = _mm_packs_epi32(v, v);
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        add2rows(res, pp, pp.add(p_stride), rp, rp.add(r_stride));
        add2rows(res, pp.add(2 * p_stride), pp.add(3 * p_stride), rp.add(2 * r_stride), rp.add(3 * r_stride));
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*;

    #[inline(always)]
    unsafe fn transpose4(r0: int32x4_t, r1: int32x4_t, r2: int32x4_t, r3: int32x4_t) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let t01 = vtrnq_s32(r0, r1);
        let t23 = vtrnq_s32(r2, r3);
        (
            vcombine_s32(vget_low_s32(t01.0), vget_low_s32(t23.0)),
            vcombine_s32(vget_low_s32(t01.1), vget_low_s32(t23.1)),
            vcombine_s32(vget_high_s32(t01.0), vget_high_s32(t23.0)),
            vcombine_s32(vget_high_s32(t01.1), vget_high_s32(t23.1)),
        )
    }

    #[inline(always)]
    unsafe fn inv1d(d0: int32x4_t, d1: int32x4_t, d2: int32x4_t, d3: int32x4_t) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let e0 = vaddq_s32(d0, d2);
        let e1 = vsubq_s32(d0, d2);
        let e2 = vsubq_s32(vshrq_n_s32::<1>(d1), d3);
        let e3 = vaddq_s32(d1, vshrq_n_s32::<1>(d3));
        (vaddq_s32(e0, e3), vaddq_s32(e1, e2), vsubq_s32(e1, e2), vsubq_s32(e0, e3))
    }

    /// One row: 4 x i16 residual + 4 prediction bytes, saturating, 4 bytes out.
    #[inline(always)]
    unsafe fn add_row(res: int16x4_t, p: *const u8, r: *mut u8) {
        let pb = vreinterpret_u8_u32(vld1_dup_u32(p as *const u32));
        let p16 = vreinterpret_s16_u16(vget_low_u16(vmovl_u8(pb)));
        let s = vqadd_s16(res, p16);
        let o = vqmovun_s16(vcombine_s16(s, s));
        vst1_lane_u32::<0>(r as *mut u32, vreinterpret_u32_u8(o));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn idct4x4_add_neon(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let c = coeffs.as_ptr();
        let (r0, r1, r2, r3) = (vld1q_s32(c), vld1q_s32(c.add(4)), vld1q_s32(c.add(8)), vld1q_s32(c.add(12)));
        let (c0, c1, c2, c3) = transpose4(r0, r1, r2, r3);
        let (a0, a1, a2, a3) = inv1d(c0, c1, c2, c3);
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = inv1d(t0, t1, t2, t3);
        // vrshrq_n_s32::<6> is exactly `(v + 32) >> 6`.
        let rows = [o0, o1, o2, o3];
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        for (k, o) in rows.iter().enumerate() {
            let res = vqmovn_s32(vrshrq_n_s32::<6>(*o));
            add_row(res, pp.add(k * p_stride), rp.add(k * r_stride));
        }
    }

    pub(super) unsafe fn flat_add_4x4_neon(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let res = vqmovn_s32(vdupq_n_s32(rval));
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        for k in 0..4 {
            add_row(res, pp.add(k * p_stride), rp.add(k * r_stride));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        *s >> 8
    }

    #[test]
    fn idct4x4_add_matches_scalar() {
        let mut seed = 7u32;
        for trial in 0..4000 {
            // Magnitude ladder: small residuals, dequant-sized, then i16-overflowing.
            let mag: i32 = [4, 64, 1024, 40_000, 300_000][trial % 5];
            let mut co = [0i32; 16];
            for c in co.iter_mut() {
                *c = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            }
            let (ps, rs) = (13usize, 17usize);
            let mut pred = vec![0u8; ps * 4 + 8];
            for p in pred.iter_mut() {
                *p = lcg(&mut seed) as u8;
            }
            let mut a = vec![0u8; rs * 4 + 8];
            let mut b = a.clone();
            idct4x4_add_scalar(&co, &pred, 3, ps, &mut a, 5, rs);
            idct4x4_add(&co, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "trial {trial} mag {mag}");
            let rv = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            flat_add_4x4_scalar(rv, &pred, 3, ps, &mut a, 5, rs);
            flat_add_4x4(rv, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "flat trial {trial}");
        }
    }
}

// ---------------------------------------------------------------------------
// FUSED scan-order dequant + IDCT + add (dense-over-scatter round, 2026-09-05).
// The block arrives in SCAN order straight from the entropy parse; the zig-zag
// un-scan is 8 `shufps` in registers, the dequant is `(v * ls + add) >> sr` per
// row from per-qp constants (no branch on qp), then the IDCT + add above. This
// retires the per-block `un_scan_4x4_dcac` (32 scalar moves) and `dequantize`
// (42 instrs) passes entirely. `AC` = the 15-coefficient AC form (I16 luma AC,
// chroma AC): scan index j is raster position ZIG4[j + 1] and position 0 takes
// the caller's already-dequantised DC.
// ---------------------------------------------------------------------------

const ZIG4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Scalar oracle: un-scan (DC/AC form) + dequant + IDCT + add.
#[allow(clippy::too_many_arguments)]
/// Zig-zag unscan + dequant into raster order -- the half of the fused kernel that
/// is NOT the inverse transform. Shared by the scalar oracle and by the dispatcher's
/// fallback so the two arithmetic paths cannot drift apart.
#[inline]
fn deq_raster_from_scan<const AC: bool>(
    scan: &[i32; 16],
    ls: &[i32; 16],
    add: i32,
    sr: i32,
    dc: i32,
) -> [i32; 16] {
    let mut raster = [0i32; 16];
    if AC {
        for j in 0..15 {
            raster[ZIG4[j + 1]] = scan[j];
        }
    } else {
        for j in 0..16 {
            raster[ZIG4[j]] = scan[j];
        }
    }
    let mut deq = [0i32; 16];
    for i in 0..16 {
        deq[i] = raster[i].wrapping_mul(ls[i]).wrapping_add(add) >> sr;
    }
    if AC {
        deq[0] = dc;
    }
    deq
}

// The dispatcher no longer calls this (its fallback composes deq_raster_from_scan
// with idct4x4_add so the NEON/SSE2 kernel is reached), but the scalar twin STAYS:
// it is the oracle both fused-form tests and the composed-fallback tests compare
// against. Deleting an oracle because the shipping path stopped calling it is how
// a kernel loses its only independent check.
#[cfg_attr(not(test), allow(dead_code))]
pub fn idct4x4_deq_add_scalar<const AC: bool>(scan: &[i32; 16], ls: &[i32; 16], add: i32, sr: i32, dc: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    let deq = deq_raster_from_scan::<AC>(scan, ls, add, sr, dc);
    idct4x4_add_scalar(&deq, pred, p_off, p_stride, rec, r_off, r_stride);
}

/// `rec = clip(pred + idct(dequant(unscan(scan))))`, one call per coded block.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn idct4x4_deq_add<const AC: bool>(scan: &[i32; 16], ls: &[i32; 16], add: i32, sr: i32, dc: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    assert!(p_off + 3 * p_stride + 4 <= pred.len() && r_off + 3 * r_stride + 4 <= rec.len());
    // VENDOR(uefi-player): firmware single-bit gate — off means the fully
    // scalar twin (not the composed fallback, which still uses the SSE2 arm).
    if !crate::simd_enabled() {
        crate::census::IDCT4X4_DEQ_ADD.scalar();
        return idct4x4_deq_add_scalar::<AC>(scan, ls, add, sr, dc, pred, p_off, p_stride, rec, r_off, r_stride);
    }
    #[cfg(target_arch = "x86_64")]
    if crate::have_sse41() { // VENDOR(uefi-player): no_std => assumed max (see lib.rs).
        // SAFETY: both windows asserted in bounds; scan / ls are fixed arrays; SSE4.1 present.
        crate::census::IDCT4X4_DEQ_ADD.base();
        return unsafe { x86_fused::idct4x4_deq_add_sse::<AC>(scan, ls, add, sr, dc, pred, p_off, p_stride, rec, r_off, r_stride) };
    }
    // No FUSED kernel on this target (aarch64, or x86-64 without SSE4.1). Do the
    // cheap half -- a 16-element unscan + dequant -- in scalar, then hand the
    // inverse transform to `idct4x4_add`, which HAS an SSE2 arm and a NEON arm.
    // Falling all the way through to `idct4x4_deq_add_scalar` here threw away a
    // kernel that already exists and is already gated bit-exact: this dispatcher
    // is 11% of ALL kernel-dispatcher calls on real content, so on aarch64 that
    // fallback was the single largest scalar path in the decoder.
    crate::census::IDCT4X4_DEQ_ADD.scalar();
    deq_add_composed::<AC>(scan, ls, add, sr, dc, pred, p_off, p_stride, rec, r_off, r_stride)
}

/// The fallback the dispatcher takes when this target has no FUSED kernel.
///
/// It is a NAMED function rather than an inline tail in `idct4x4_deq_add` for one
/// reason: on x86-64 the SSE4.1 arm always wins, so the dispatcher can never reach
/// this path on the host that runs the tests. A fallback that no test can call is
/// how a fallback silently rots. `deq_add_composed_matches_scalar` calls it directly.
#[allow(clippy::too_many_arguments)]
#[inline]
fn deq_add_composed<const AC: bool>(
    scan: &[i32; 16],
    ls: &[i32; 16],
    add: i32,
    sr: i32,
    dc: i32,
    pred: &[u8],
    p_off: usize,
    p_stride: usize,
    rec: &mut [u8],
    r_off: usize,
    r_stride: usize,
) {
    let deq = deq_raster_from_scan::<AC>(scan, ls, add, sr, dc);
    idct4x4_add(&deq, pred, p_off, p_stride, rec, r_off, r_stride)
}

#[cfg(target_arch = "x86_64")]
mod x86_fused {
    use super::x86::{add2rows, inv1d, transpose4};
    use core::arch::x86_64::*;

    /// `shufps` immediate: result = [a[l0], a[l1], b[l2], b[l3]].
    const fn sh(l0: i32, l1: i32, l2: i32, l3: i32) -> i32 {
        l0 | (l1 << 2) | (l2 << 4) | (l3 << 6)
    }

    #[inline(always)]
    unsafe fn shuf<const IMM: i32>(a: __m128i, b: __m128i) -> __m128i {
        _mm_castps_si128(_mm_shuffle_ps::<IMM>(_mm_castsi128_ps(a), _mm_castsi128_ps(b)))
    }

    /// 4x4 zig-zag un-scan in registers. DC form: raster row r from the scan
    /// vectors s0..s3 (8 shuffles). AC form: scan index j is raster ZIG4[j+1];
    /// row 0 lane 0 is left for the caller's DC (7 shuffles).
    #[inline(always)]
    pub(super) unsafe fn unscan<const AC: bool>(s0: __m128i, s1: __m128i, s2: __m128i, s3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        if AC {
            // rows: [_, a0.0, a1.0, a1.1] [a0.1, a0.3, a1.2, a2.3] [a0.2, a1.3, a2.2, a3.0] [a2.0, a2.1, a3.1, a3.2]
            let r0 = shuf::<{ sh(0, 0, 0, 1) }>(s0, s1);
            let y1 = shuf::<{ sh(2, 2, 3, 3) }>(s1, s2);
            let r1 = shuf::<{ sh(1, 3, 0, 2) }>(s0, y1);
            let x2 = shuf::<{ sh(2, 2, 3, 3) }>(s0, s1);
            let y2 = shuf::<{ sh(2, 2, 0, 0) }>(s2, s3);
            let r2 = shuf::<{ sh(0, 2, 0, 2) }>(x2, y2);
            let r3 = shuf::<{ sh(0, 1, 1, 2) }>(s2, s3);
            (r0, r1, r2, r3)
        } else {
            // rows: [s0.0, s0.1, s1.1, s1.2] [s0.2, s1.0, s1.3, s3.0] [s0.3, s2.0, s2.3, s3.1] [s2.1, s2.2, s3.2, s3.3]
            let r0 = shuf::<{ sh(0, 1, 1, 2) }>(s0, s1);
            let x1 = shuf::<{ sh(2, 2, 0, 0) }>(s0, s1);
            let y1 = shuf::<{ sh(3, 3, 0, 0) }>(s1, s3);
            let r1 = shuf::<{ sh(0, 2, 0, 2) }>(x1, y1);
            let x2 = shuf::<{ sh(3, 3, 0, 0) }>(s0, s2);
            let y2 = shuf::<{ sh(3, 3, 1, 1) }>(s2, s3);
            let r2 = shuf::<{ sh(0, 2, 0, 2) }>(x2, y2);
            let r3 = shuf::<{ sh(1, 2, 2, 3) }>(s2, s3);
            (r0, r1, r2, r3)
        }
    }

    /// DC-form un-scan for the luma-DC kernel (same 8 shuffles).
    #[inline(always)]
    pub(super) unsafe fn unscan_dc(s0: __m128i, s1: __m128i, s2: __m128i, s3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        unscan::<false>(s0, s1, s2, s3)
    }

    #[allow(clippy::too_many_arguments)]
    #[target_feature(enable = "sse4.1")]
    pub(super) unsafe fn idct4x4_deq_add_sse<const AC: bool>(scan: &[i32; 16], ls: &[i32; 16], add: i32, sr: i32, dc: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let s = scan.as_ptr() as *const __m128i;
        let (s0, s1, s2, s3) = (_mm_loadu_si128(s), _mm_loadu_si128(s.add(1)), _mm_loadu_si128(s.add(2)), _mm_loadu_si128(s.add(3)));
        let (r0, r1, r2, r3) = unscan::<AC>(s0, s1, s2, s3);
        // Dequant, branch-free: (v * ls + add) >> sr per raster row.
        let l = ls.as_ptr() as *const __m128i;
        let addv = _mm_set1_epi32(add);
        let cnt = _mm_cvtsi32_si128(sr);
        let dq = |v: __m128i, k: usize| _mm_sra_epi32(_mm_add_epi32(_mm_mullo_epi32(v, _mm_loadu_si128(l.add(k))), addv), cnt);
        let (mut d0, d1, d2, d3) = (dq(r0, 0), dq(r1, 1), dq(r2, 2), dq(r3, 3));
        if AC {
            d0 = _mm_insert_epi32::<0>(d0, dc);
        }
        // IDCT + add (same body as idct4x4_add_sse2).
        let (c0, c1, c2, c3) = transpose4(d0, d1, d2, d3);
        let (a0, a1, a2, a3) = inv1d(c0, c1, c2, c3);
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = inv1d(t0, t1, t2, t3);
        let k = _mm_set1_epi32(32);
        let rnd = |v: __m128i| _mm_srai_epi32::<6>(_mm_add_epi32(v, k));
        let r01 = _mm_packs_epi32(rnd(o0), rnd(o1));
        let r23 = _mm_packs_epi32(rnd(o2), rnd(o3));
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        add2rows(r01, pp, pp.add(p_stride), rp, rp.add(r_stride));
        add2rows(r23, pp.add(2 * p_stride), pp.add(3 * p_stride), rp.add(2 * r_stride), rp.add(3 * r_stride));
    }
}

#[cfg(test)]
mod fused_tests {
    use super::*;
    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        *s >> 8
    }
    fn run<const AC: bool>() {
        let mut seed = 99u32;
        for trial in 0..3000 {
            let mag: i32 = [3, 40, 700, 30_000][trial % 4];
            let mut scan = [0i32; 16];
            for c in scan.iter_mut() {
                *c = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            }
            if AC {
                scan[15] = 0;
            }
            // Per-qp shapes: pre-shifted ls with add=0/sr=0, or ls with (add=1<<k, sr=k+1).
            let ls: [i32; 16] = core::array::from_fn(|_| 10 + (lcg(&mut seed) % 600) as i32);
            let (add, sr) = if trial % 2 == 0 { (0, 0) } else { let k = (lcg(&mut seed) % 4) as i32; (1 << k, k + 1) };
            let dc = (lcg(&mut seed) as i32 % 4001) - 2000;
            let (ps, rs) = (13usize, 17usize);
            let mut pred = vec![0u8; ps * 4 + 8];
            for p in pred.iter_mut() {
                *p = lcg(&mut seed) as u8;
            }
            let mut a = vec![0u8; rs * 4 + 8];
            let mut b = a.clone();
            idct4x4_deq_add_scalar::<AC>(&scan, &ls, add, sr, dc, &pred, 3, ps, &mut a, 5, rs);
            idct4x4_deq_add::<AC>(&scan, &ls, add, sr, dc, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "AC={AC} trial {trial}");
        }
    }
    #[test]
    fn fused_dc_form_matches_scalar() { run::<false>(); }
    #[test]
    fn fused_ac_form_matches_scalar() { run::<true>(); }

    /// The COMPOSED fallback (unscan+dequant in scalar, IDCT+add through the
    /// `idct4x4_add` kernel) must equal the pure scalar oracle. This is the path
    /// aarch64 and pre-SSE4.1 x86-64 actually run, and the dispatcher cannot reach
    /// it on this host, so it is called directly here.
    fn run_composed<const AC: bool>() {
        let mut seed = 4242u32;
        for trial in 0..3000 {
            let mag: i32 = [3, 40, 700, 30_000][trial % 4];
            let mut scan = [0i32; 16];
            for c in scan.iter_mut() {
                *c = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            }
            if AC {
                scan[15] = 0;
            }
            let ls: [i32; 16] = core::array::from_fn(|_| 10 + (lcg(&mut seed) % 600) as i32);
            let (add, sr) = if trial % 2 == 0 {
                (0, 0)
            } else {
                let k = (lcg(&mut seed) % 4) as i32;
                (1 << k, k + 1)
            };
            let dc = (lcg(&mut seed) as i32 % 4001) - 2000;
            let (ps, rs) = (13usize, 17usize);
            let mut pred = vec![0u8; ps * 4 + 8];
            for p in pred.iter_mut() {
                *p = lcg(&mut seed) as u8;
            }
            let mut a = vec![0u8; rs * 4 + 8];
            let mut b = a.clone();
            idct4x4_deq_add_scalar::<AC>(&scan, &ls, add, sr, dc, &pred, 3, ps, &mut a, 5, rs);
            deq_add_composed::<AC>(&scan, &ls, add, sr, dc, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "composed fallback != scalar oracle (trial {trial}, AC={AC})");
        }
    }

    #[test]
    fn composed_fallback_ac_matches_scalar() { run_composed::<true>(); }

    #[test]
    fn composed_fallback_dc_matches_scalar() { run_composed::<false>(); }
}

// ---------------------------------------------------------------------------
// I_16x16 luma DC: scan-order un-scan + 4x4 Hadamard + scale, fused
// (dense-over-scatter round). Was `un_scan_4x4_dcac` (32 moves) + a scalar
// `hadamard_4x4` + per-lane scale (253 instrs) per I16 macroblock.
// ---------------------------------------------------------------------------

/// Scalar oracle: `((hadamard(unscan(scan)) * ls) + add) >> sr`, raster out.
pub fn luma_dc_from_scan_scalar(scan: &[i32; 16], ls: i32, add: i32, sr: i32) -> [i32; 16] {
    let mut m = [0i32; 16];
    for j in 0..16 {
        m[ZIG4[j]] = scan[j];
    }
    let h = |a: i32, b: i32, c: i32, d: i32| {
        (a.wrapping_add(b).wrapping_add(c).wrapping_add(d), a.wrapping_add(b).wrapping_sub(c).wrapping_sub(d), a.wrapping_sub(b).wrapping_sub(c).wrapping_add(d), a.wrapping_sub(b).wrapping_add(c).wrapping_sub(d))
    };
    for r in 0..4 {
        let (a, b, c, d) = h(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = h(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
    core::array::from_fn(|i| m[i].wrapping_mul(ls).wrapping_add(add) >> sr)
}

/// Fused I16 luma DC dequant from the SCAN-order DC block.
#[inline]
pub fn luma_dc_from_scan(scan: &[i32; 16], ls: i32, add: i32, sr: i32) -> [i32; 16] {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::LUMA_DC_SCAN.scalar();
        return luma_dc_from_scan_scalar(scan, ls, add, sr);
    }
    #[cfg(target_arch = "x86_64")]
    if crate::have_sse41() { // VENDOR(uefi-player): no_std => assumed max (see lib.rs).
        // SAFETY: fixed arrays; SSE4.1 present.
        crate::census::LUMA_DC_SCAN.base();
        return unsafe { x86_dc::luma_dc_from_scan_sse(scan, ls, add, sr) };
    }
    crate::census::LUMA_DC_SCAN.scalar();
    luma_dc_from_scan_scalar(scan, ls, add, sr)
}

#[cfg(target_arch = "x86_64")]
mod x86_dc {
    use super::x86::transpose4;
    use core::arch::x86_64::*;

    /// Lane-wise 4-point Hadamard: (a+b+c+d, a+b-c-d, a-b-c+d, a-b+c-d).
    #[inline(always)]
    unsafe fn had4(a: __m128i, b: __m128i, c: __m128i, d: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let s = _mm_add_epi32(a, b);
        let t = _mm_add_epi32(c, d);
        let u = _mm_sub_epi32(a, b);
        let v = _mm_sub_epi32(c, d);
        (_mm_add_epi32(s, t), _mm_sub_epi32(s, t), _mm_sub_epi32(u, v), _mm_add_epi32(u, v))
    }

    #[target_feature(enable = "sse4.1")]
    pub(super) unsafe fn luma_dc_from_scan_sse(scan: &[i32; 16], ls: i32, add: i32, sr: i32) -> [i32; 16] {
        let s = scan.as_ptr() as *const __m128i;
        let (s0, s1, s2, s3) = (_mm_loadu_si128(s), _mm_loadu_si128(s.add(1)), _mm_loadu_si128(s.add(2)), _mm_loadu_si128(s.add(3)));
        let (r0, r1, r2, r3) = super::x86_fused::unscan_dc(s0, s1, s2, s3);
        // Row pass across lanes: transpose, lane-wise Hadamard, transpose back; column pass lane-wise.
        let (c0, c1, c2, c3) = transpose4(r0, r1, r2, r3);
        let (a0, a1, a2, a3) = had4(c0, c1, c2, c3);
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = had4(t0, t1, t2, t3);
        let (lsv, addv, cnt) = (_mm_set1_epi32(ls), _mm_set1_epi32(add), _mm_cvtsi32_si128(sr));
        let sc = |v: __m128i| _mm_sra_epi32(_mm_add_epi32(_mm_mullo_epi32(v, lsv), addv), cnt);
        let mut out = [0i32; 16];
        let o = out.as_mut_ptr() as *mut __m128i;
        _mm_storeu_si128(o, sc(o0));
        _mm_storeu_si128(o.add(1), sc(o1));
        _mm_storeu_si128(o.add(2), sc(o2));
        _mm_storeu_si128(o.add(3), sc(o3));
        out
    }
}

#[cfg(test)]
mod dc_tests {
    use super::*;
    #[test]
    fn luma_dc_from_scan_matches_scalar() {
        let mut seed = 3u32;
        for trial in 0..3000 {
            let mut scan = [0i32; 16];
            for c in scan.iter_mut() {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *c = ((seed >> 8) as i32 % 4001) - 2000;
            }
            let ls = 160 + (trial as i32 % 300) * 16;
            let (add, sr) = if trial % 2 == 0 { (0, 0) } else { let k = trial as i32 % 6; (1 << k, k + 1) };
            assert_eq!(luma_dc_from_scan(&scan, ls, add, sr), luma_dc_from_scan_scalar(&scan, ls, add, sr), "trial {trial}");
        }
    }
}

/// z-order -> raster permutation of a macroblock's 24 nnz bytes (16 luma + 8
/// chroma): per 8-byte group the u16-lane swap (0,2,1,3) -- three shuffles on
/// x86 instead of 24 byte moves (dense-over-scatter round). The decoder crate
/// forbids `unsafe`, so the kernel lives here behind a safe fn.
pub fn nnz_raster_from_z(n: &[u8; 24]) -> [u8; 24] {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        // SAFETY: fixed-size arrays; the 16 + 8 byte loads/stores stay inside them.
        unsafe {
            let luma = _mm_loadu_si128(n.as_ptr() as *const __m128i);
            let chroma = _mm_loadl_epi64(n.as_ptr().add(16) as *const __m128i);
            let l = _mm_shufflehi_epi16::<0b11_01_10_00>(_mm_shufflelo_epi16::<0b11_01_10_00>(luma));
            let c = _mm_shufflelo_epi16::<0b11_01_10_00>(chroma);
            let mut out = [0u8; 24];
            _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, l);
            _mm_storel_epi64(out.as_mut_ptr().add(16) as *mut __m128i, c);
            crate::census::NNZ_RASTER.base();
            return out;
        }
    }
    #[allow(unreachable_code)]
    [
        n[0], n[1], n[4], n[5], n[2], n[3], n[6], n[7], n[8], n[9], n[12], n[13], n[10], n[11], n[14], n[15],
        n[16], n[17], n[20], n[21], n[18], n[19], n[22], n[23],
    ]
}

#[cfg(test)]
mod nnz_tests {
    #[test]
    fn nnz_raster_permutation_matches_scalar() {
        for t in 0..64u8 {
            let n: [u8; 24] = core::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(t));
            let want = [
                n[0], n[1], n[4], n[5], n[2], n[3], n[6], n[7], n[8], n[9], n[12], n[13], n[10], n[11], n[14], n[15],
                n[16], n[17], n[20], n[21], n[18], n[19], n[22], n[23],
            ];
            assert_eq!(super::nnz_raster_from_z(&n), want);
        }
    }
}
