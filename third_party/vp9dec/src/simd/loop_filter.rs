//! AVX2 SIMD mirror of `loop_filter.rs`'s deblocking edge filters (spec §8.8.5: narrow /
//! wide8 / wide16), for both edge orientations: the HORIZONTAL-edge kernel
//! ([`loop_filter_horiz8_avx2`]) and the VERTICAL-edge kernel ([`loop_filter_vert8_avx2`]),
//! which transposes the tap window into the horizontal kernel's layout and reuses its
//! arithmetic. `loop_filter.rs` owns the dispatch, the per-lane mask building, and the
//! scalar fallback; this module owns only the vector kernels.

// VENDOR(uefi-player): core path works in both std and no_std builds.
use core::arch::x86_64::*;

/// AVX2 mirror of `loop_filter.rs`'s narrow (spec §8.8.5.2, `filter4`), wide 8-tap
/// (§8.8.5.3, `log2_size == 3`) and wide 16-tap (§8.8.5.3, `log2_size == 4`, the `TX_16X16`
/// "wide2" filter) deblocking filters, applied to 8 contiguous along-edge positions on a
/// HORIZONTAL edge (loop_filter.rs pass==1: taps run in the row direction, i.e. `dx=0,dy=1`
/// in that file's terms) -- see the current SIMD summary in `docs/implementation-notes.md`.
/// Vertical edges (pass==0) are handled by [`loop_filter_vert8_avx2`], which transposes the tap
/// window into this kernel's row-major layout and reuses this exact arithmetic.
///
/// `plane_base`/`plane_width` is the raw row-major plane buffer (stride == `plane_width`,
/// `u16`-backed regardless of bit depth -- see `framebuffer.rs`. The caller passes `bit_depth`;
/// the kernel scales its constants accordingly and supports 8/10/12-bit samples.
/// `(x0, y0)` is lane 0's position (loop_filter.rs's `sample_filtering(x, y, ...)` for the
/// first of the 8 lanes); the other 7 lanes are the next 7 contiguous columns
/// `x0+1..=x0+7` at the same row `y0` (this orientation's along-edge axis -- see the
/// `debug_assert_eq!`s at the call site proving contiguity).
///
/// `eligible[lane]` / `is_tx8[lane]` / `is_tx16[lane]` are 0/-1 masks (not `bool`, so the
/// caller can build them once and this fn just loads them): `eligible` gates whether the lane
/// is written at all (false = leave the plane untouched -- inactive lane); `is_tx8` /
/// `is_tx16` are whether the lane's `filter_size` is `TX_8X8` / `TX_16X16` (exactly one of
/// TX_4X4 / TX_8X8 / TX_16X16 per eligible lane; both zero means TX_4X4), mirroring
/// `loop_filter.rs::sample_filtering`'s three-way narrow/wide8/wide16 branch. `limit`/`blimit`/
/// `thresh` are each lane's spec §8.8.4 filter strength (only read for eligible lanes).
///
/// # Safety
/// Caller must confirm `avx2_enabled()` and that reading/writing columns `x0..x0+8` is in
/// bounds for the row window this call touches: rows `y0-4..=y0+3` (the `p3..q3` window spec
/// §8.8.5.1's mask reads) always, AND -- whenever any lane's `is_tx16` is set -- the wider
/// rows `y0-8..=y0+7` (the `p7..q7` window the wide16 filter reads/writes; loaded only in that
/// case). Both windows are exactly what the already-proven-bit-exact scalar
/// `compute_filter_mask`/`wide_filter` touch for an eligible lane at this same edge-constant
/// `y0`, so they are in bounds whenever at least one lane is eligible (and, for the wider
/// window, whenever an `is_tx16` lane is eligible -- the caller only sets `is_tx16` on eligible
/// lanes). Planes are allocated out to superblock boundaries (see `framebuffer::Plane`), so
/// the extra columns read/written for non-eligible lanes in the same 8-wide group are valid
/// memory too (the write blends in the original value there, so it is a no-op).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub unsafe fn loop_filter_horiz8_avx2(
    plane_base: *mut u16,
    plane_width: usize,
    x0: usize,
    y0: usize,
    eligible: &[i32; 8],
    is_tx8: &[i32; 8],
    is_tx16: &[i32; 8],
    limit: &[i32; 8],
    blimit: &[i32; 8],
    thresh: &[i32; 8],
    bit_depth: u8,
) {
    let base = plane_base;

    // Loads 8 contiguous u16 samples (columns x0..x0+8) from row `y0 + dy`, widened to
    // 8xi32 -- one AVX2 lane per along-edge position, matching `get_off`'s
    // `plane.get(x, y+k)` for pass==1 (`dx=0,dy=1`).
    let load_row = |dy: i64| -> __m256i {
        let row_ptr = base.offset(((y0 as i64 + dy) * plane_width as i64 + x0 as i64) as isize);
        _mm256_cvtepu16_epi32(_mm_loadu_si128(row_ptr as *const __m128i))
    };
    let p3 = load_row(-4);
    let p2 = load_row(-3);
    let p1 = load_row(-2);
    let p0 = load_row(-1);
    let q0 = load_row(0);
    let q1 = load_row(1);
    let q2 = load_row(2);
    let q3 = load_row(3);

    let abs_diff = |a: __m256i, b: __m256i| _mm256_abs_epi32(_mm256_sub_epi32(a, b));
    let gt = |a: __m256i, b: __m256i| _mm256_cmpgt_epi32(a, b);
    let all_ones = _mm256_set1_epi32(-1);
    let not_mask = |m: __m256i| _mm256_xor_si256(m, all_ones);

    let limit_v = _mm256_loadu_si256(limit.as_ptr() as *const __m256i);
    let blimit_v = _mm256_loadu_si256(blimit.as_ptr() as *const __m256i);
    let thresh_v = _mm256_loadu_si256(thresh.as_ptr() as *const __m256i);
    let eligible_v = _mm256_loadu_si256(eligible.as_ptr() as *const __m256i);
    let is_tx8_v = _mm256_loadu_si256(is_tx8.as_ptr() as *const __m256i);
    let is_tx16_v = _mm256_loadu_si256(is_tx16.as_ptr() as *const __m256i);
    // filter_size >= TX_8X8 (either wide filter is possible): gates flat_mask exactly like the
    // scalar `if filter_size >= TX_8X8`.
    let is_wide_v = _mm256_or_si256(is_tx8_v, is_tx16_v);

    // Spec §8.8.5.1 "Filter mask process".
    let hev_mask = _mm256_or_si256(
        gt(abs_diff(p1, p0), thresh_v),
        gt(abs_diff(q1, q0), thresh_v),
    );

    let mut mask = gt(abs_diff(p3, p2), limit_v);
    mask = _mm256_or_si256(mask, gt(abs_diff(p2, p1), limit_v));
    mask = _mm256_or_si256(mask, gt(abs_diff(p1, p0), limit_v));
    mask = _mm256_or_si256(mask, gt(abs_diff(q1, q0), limit_v));
    mask = _mm256_or_si256(mask, gt(abs_diff(q2, q1), limit_v));
    mask = _mm256_or_si256(mask, gt(abs_diff(q3, q2), limit_v));
    let blimit_sum = _mm256_add_epi32(
        _mm256_mullo_epi32(abs_diff(p0, q0), _mm256_set1_epi32(2)),
        _mm256_srli_epi32(abs_diff(p1, q1), 1),
    );
    mask = _mm256_or_si256(mask, gt(blimit_sum, blimit_v));
    let filter_mask = not_mask(mask);
    let elig_and_fm = _mm256_and_si256(eligible_v, filter_mask);

    // Spec §8.8.5.2 "Narrow filter process" (`filter4`) -- computed unconditionally (cheap,
    // and the common TX_4X4-heavy case selects it; both wide fast paths below reuse it).
    // The 8-bit base (128) and clamp range (-128..=127) scale by `<< (bit_depth - 8)`, matching
    // `loop_filter.rs::narrow_filter`'s `half` / `clamp_hi` / `clamp_lo` (identity at 8-bit).
    let clamp_hi = (128i32 << (bit_depth - 8)) - 1;
    let clamp_lo = -(clamp_hi + 1);
    let c128 = _mm256_set1_epi32(1 << (bit_depth - 1));
    let clamp4 = |v: __m256i| {
        _mm256_min_epi32(
            _mm256_max_epi32(v, _mm256_set1_epi32(clamp_lo)),
            _mm256_set1_epi32(clamp_hi),
        )
    };

    let ps1 = _mm256_sub_epi32(p1, c128);
    let ps0 = _mm256_sub_epi32(p0, c128);
    let qs0 = _mm256_sub_epi32(q0, c128);
    let qs1 = _mm256_sub_epi32(q1, c128);

    let filt_hev = clamp4(_mm256_sub_epi32(ps1, qs1));
    let filt_base = _mm256_blendv_epi8(_mm256_setzero_si256(), filt_hev, hev_mask);
    let filt = clamp4(_mm256_add_epi32(
        filt_base,
        _mm256_mullo_epi32(_mm256_sub_epi32(qs0, ps0), _mm256_set1_epi32(3)),
    ));
    let filter1 = _mm256_srai_epi32(clamp4(_mm256_add_epi32(filt, _mm256_set1_epi32(4))), 3);
    let filter2 = _mm256_srai_epi32(clamp4(_mm256_add_epi32(filt, _mm256_set1_epi32(3))), 3);

    let oq0_narrow = _mm256_add_epi32(clamp4(_mm256_sub_epi32(qs0, filter1)), c128);
    let op0_narrow = _mm256_add_epi32(clamp4(_mm256_add_epi32(ps0, filter2)), c128);

    let round_f = _mm256_srai_epi32(_mm256_add_epi32(filter1, _mm256_set1_epi32(1)), 1);
    let oq1_narrow = _mm256_blendv_epi8(
        _mm256_add_epi32(clamp4(_mm256_sub_epi32(qs1, round_f)), c128),
        q1,
        hev_mask,
    );
    let op1_narrow = _mm256_blendv_epi8(
        _mm256_add_epi32(clamp4(_mm256_add_epi32(ps1, round_f)), c128),
        p1,
        hev_mask,
    );

    // Packs 8xi32 (each guaranteed in 0..=255: narrow's clamp4(..)+128 and wide8's
    // round2(sum-of-8-pixel-values, 3) can't leave that range) down to 8 contiguous u16
    // samples and stores at row `y0 + dy`, columns x0..x0+8 -- the exact reverse of `load_row`.
    let store_row = |dy: i64, v: __m256i| {
        let lo = _mm256_castsi256_si128(v);
        let hi = _mm256_extracti128_si256(v, 1);
        let u16x8 = _mm_packus_epi32(lo, hi);
        let row_ptr = base.offset(((y0 as i64 + dy) * plane_width as i64 + x0 as i64) as isize);
        _mm_storeu_si128(row_ptr as *mut __m128i, u16x8);
    };

    // Fast path 1: no lane is a wide filter (every eligible lane is TX_4X4), so flat_mask,
    // wide8 and wide16 can never be selected -- store only the narrow p1..q1 and return. This
    // is the ONLY path that leaves p2/q2 untouched, matching the scalar narrow filter's write
    // set, and it stays inside the narrow p3..q3 window. Common for detailed, TX_4X4-heavy
    // content. `_mm256_testz_si256(a, a)` is a single-instruction "is `a` all-zero" test.
    if _mm256_testz_si256(is_wide_v, is_wide_v) != 0 {
        let p1_out = _mm256_blendv_epi8(p1, op1_narrow, elig_and_fm);
        let p0_out = _mm256_blendv_epi8(p0, op0_narrow, elig_and_fm);
        let q0_out = _mm256_blendv_epi8(q0, oq0_narrow, elig_and_fm);
        let q1_out = _mm256_blendv_epi8(q1, oq1_narrow, elig_and_fm);
        store_row(-2, p1_out);
        store_row(-1, p0_out);
        store_row(0, q0_out);
        store_row(1, q1_out);
        return;
    }

    // flat_mask, gated on filter_size >= TX_8X8 (is_wide_v) exactly like the scalar
    // `if filter_size >= TX_8X8 { .. }` (else flat_mask stays false -> narrow selected). The
    // flat threshold is 1 at 8-bit and scales `<< (bit_depth - 8)` (scalar's `threshold`).
    let one = _mm256_set1_epi32(1 << (bit_depth - 8));
    let mut fm = gt(abs_diff(p1, p0), one);
    fm = _mm256_or_si256(fm, gt(abs_diff(q1, q0), one));
    fm = _mm256_or_si256(fm, gt(abs_diff(p2, p0), one));
    fm = _mm256_or_si256(fm, gt(abs_diff(q2, q0), one));
    fm = _mm256_or_si256(fm, gt(abs_diff(p3, p0), one));
    fm = _mm256_or_si256(fm, gt(abs_diff(q3, q0), one));
    let flat_mask = _mm256_and_si256(not_mask(fm), is_wide_v);

    // Spec §8.8.5.3 "Wide filter process" (log2_size == 3, the 8-tap "flat" filter).
    // Closed-form weighted sums equivalent to the spec's O(n^2) loop (verified by hand
    // against the spec's `t = get_off(i) + sum_j get_off(clip3(-4,3,i+j))` derivation --
    // reordering the same set of integer terms is exact, not just "close").
    let round2_3 = |t: __m256i| _mm256_srai_epi32(_mm256_add_epi32(t, _mm256_set1_epi32(4)), 3);
    let mul = |v: __m256i, c: i32| _mm256_mullo_epi32(v, _mm256_set1_epi32(c));
    let add = _mm256_add_epi32;
    let sum5 = |a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i| {
        add(add(add(a, b), add(c, d)), e)
    };
    let sum6 = |a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i, f: __m256i| {
        add(sum5(a, b, c, d, e), f)
    };
    let sum7 =
        |a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i, f: __m256i, g: __m256i| {
            add(sum6(a, b, c, d, e, f), g)
        };

    let op2_wide = round2_3(sum5(mul(p3, 3), mul(p2, 2), p1, p0, q0));
    let op1_wide = round2_3(sum6(mul(p3, 2), p2, mul(p1, 2), p0, q0, q1));
    let op0_wide = round2_3(sum7(p3, p2, p1, mul(p0, 2), q0, q1, q2));
    let oq0_wide = round2_3(sum7(p2, p1, p0, mul(q0, 2), q1, q2, q3));
    let oq1_wide = round2_3(sum6(p1, p0, q0, mul(q1, 2), q2, mul(q3, 2)));
    let oq2_wide = round2_3(sum5(p0, q0, q1, mul(q2, 2), mul(q3, 3)));

    // Fast path 2: no lane is TX_16X16, so wide16 can't be selected and the p4..p7/q4..q7
    // window is never read -- blend narrow/wide8 into p2..q2 (wide8's write set) and return,
    // exactly as SIMD wave 3 did. Keeps the common case off wide16's 8 extra loads + 14 sums
    // and inside the narrower rows-y0-4..=y0+3 memory window.
    if _mm256_testz_si256(is_tx16_v, is_tx16_v) != 0 {
        let use_wide8 = _mm256_and_si256(elig_and_fm, flat_mask);
        let use_narrow = _mm256_andnot_si256(flat_mask, elig_and_fm);
        // original -> narrow (where selected) -> wide8 (where selected); the two masks are
        // mutually exclusive, so order doesn't matter for overlap.
        let blend3 = |orig: __m256i, narrow: __m256i, wide: __m256i| {
            _mm256_blendv_epi8(
                _mm256_blendv_epi8(orig, narrow, use_narrow),
                wide,
                use_wide8,
            )
        };
        let p2_out = _mm256_blendv_epi8(p2, op2_wide, use_wide8);
        let p1_out = blend3(p1, op1_narrow, op1_wide);
        let p0_out = blend3(p0, op0_narrow, op0_wide);
        let q0_out = blend3(q0, oq0_narrow, oq0_wide);
        let q1_out = blend3(q1, oq1_narrow, oq1_wide);
        let q2_out = _mm256_blendv_epi8(q2, oq2_wide, use_wide8);
        store_row(-3, p2_out);
        store_row(-2, p1_out);
        store_row(-1, p0_out);
        store_row(0, q0_out);
        store_row(1, q1_out);
        store_row(2, q2_out);
        return;
    }

    // Slow path: at least one eligible lane is TX_16X16. Load the wider p4..p7 / q4..q7 window
    // (rows y0-8..=y0+7). Safe: an eligible TX_16X16 lane's scalar `compute_filter_mask`
    // (flat_mask2) / `wide_filter(4)` reads exactly this window (see the Safety note), so it is
    // in bounds; the group's other columns are valid for the same reason as the p3..q3 window.
    let p4 = load_row(-5);
    let p5 = load_row(-6);
    let p6 = load_row(-7);
    let p7 = load_row(-8);
    let q4 = load_row(4);
    let q5 = load_row(5);
    let q6 = load_row(6);
    let q7 = load_row(7);

    // flat_mask2, gated on filter_size >= TX_16X16 (is_tx16_v) exactly like the scalar
    // `if filter_size >= TX_16X16 { .. }` (else it stays false).
    let mut fm2 = gt(abs_diff(p7, p0), one);
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(q7, q0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(p6, p0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(q6, q0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(p5, p0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(q5, q0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(p4, p0), one));
    fm2 = _mm256_or_si256(fm2, gt(abs_diff(q4, q0), one));
    let flat_mask2 = _mm256_and_si256(not_mask(fm2), is_tx16_v);

    // Selection (spec §8.8.5): narrow if !flat_mask; else wide16 if flat_mask2 (which implies
    // TX_16X16); else wide8. The three are mutually exclusive.
    let use_wide16 = _mm256_and_si256(elig_and_fm, _mm256_and_si256(flat_mask, flat_mask2));
    let use_wide8 = _mm256_andnot_si256(flat_mask2, _mm256_and_si256(elig_and_fm, flat_mask));
    let use_narrow = _mm256_andnot_si256(flat_mask, elig_and_fm);

    // Spec §8.8.5.3 "Wide filter process" (log2_size == 4, the 16-tap "wide2" filter). Closed
    // forms derived from the scalar `wide_filter(4)`'s `t = get(i) + sum_j get(clip3(-8,7,i+j))`:
    // each output's 16 weighted terms sum to 16 (round2 by 16), the running sum peaks at
    // 16*255 = 4080 << i32::MAX (no overflow), and integer addition is associative, so this
    // reordering is exact -- verified against the scalar and the sweep in both SIMD configs.
    let round2_4 = |t: __m256i| _mm256_srai_epi32(_mm256_add_epi32(t, _mm256_set1_epi32(8)), 4);
    let sumn = |terms: &[__m256i]| {
        let mut acc = terms[0];
        let mut i = 1;
        while i < terms.len() {
            acc = add(acc, terms[i]);
            i += 1;
        }
        acc
    };
    let op6_wide2 = round2_4(sumn(&[mul(p7, 7), mul(p6, 2), p5, p4, p3, p2, p1, p0, q0]));
    let op5_wide2 = round2_4(sumn(&[
        mul(p7, 6),
        p6,
        mul(p5, 2),
        p4,
        p3,
        p2,
        p1,
        p0,
        q0,
        q1,
    ]));
    let op4_wide2 = round2_4(sumn(&[
        mul(p7, 5),
        p6,
        p5,
        mul(p4, 2),
        p3,
        p2,
        p1,
        p0,
        q0,
        q1,
        q2,
    ]));
    let op3_wide2 = round2_4(sumn(&[
        mul(p7, 4),
        p6,
        p5,
        p4,
        mul(p3, 2),
        p2,
        p1,
        p0,
        q0,
        q1,
        q2,
        q3,
    ]));
    let op2_wide2 = round2_4(sumn(&[
        mul(p7, 3),
        p6,
        p5,
        p4,
        p3,
        mul(p2, 2),
        p1,
        p0,
        q0,
        q1,
        q2,
        q3,
        q4,
    ]));
    let op1_wide2 = round2_4(sumn(&[
        mul(p7, 2),
        p6,
        p5,
        p4,
        p3,
        p2,
        mul(p1, 2),
        p0,
        q0,
        q1,
        q2,
        q3,
        q4,
        q5,
    ]));
    let op0_wide2 = round2_4(sumn(&[
        p7,
        p6,
        p5,
        p4,
        p3,
        p2,
        p1,
        mul(p0, 2),
        q0,
        q1,
        q2,
        q3,
        q4,
        q5,
        q6,
    ]));
    let oq0_wide2 = round2_4(sumn(&[
        p6,
        p5,
        p4,
        p3,
        p2,
        p1,
        p0,
        mul(q0, 2),
        q1,
        q2,
        q3,
        q4,
        q5,
        q6,
        q7,
    ]));
    let oq1_wide2 = round2_4(sumn(&[
        p5,
        p4,
        p3,
        p2,
        p1,
        p0,
        q0,
        mul(q1, 2),
        q2,
        q3,
        q4,
        q5,
        q6,
        mul(q7, 2),
    ]));
    let oq2_wide2 = round2_4(sumn(&[
        p4,
        p3,
        p2,
        p1,
        p0,
        q0,
        q1,
        mul(q2, 2),
        q3,
        q4,
        q5,
        q6,
        mul(q7, 3),
    ]));
    let oq3_wide2 = round2_4(sumn(&[
        p3,
        p2,
        p1,
        p0,
        q0,
        q1,
        q2,
        mul(q3, 2),
        q4,
        q5,
        q6,
        mul(q7, 4),
    ]));
    let oq4_wide2 = round2_4(sumn(&[
        p2,
        p1,
        p0,
        q0,
        q1,
        q2,
        q3,
        mul(q4, 2),
        q5,
        q6,
        mul(q7, 5),
    ]));
    let oq5_wide2 = round2_4(sumn(&[
        p1,
        p0,
        q0,
        q1,
        q2,
        q3,
        q4,
        mul(q5, 2),
        q6,
        mul(q7, 6),
    ]));
    let oq6_wide2 = round2_4(sumn(&[p0, q0, q1, q2, q3, q4, q5, mul(q6, 2), mul(q7, 7)]));

    // Blend precedence per position: original -> narrow -> wide8 -> wide16 (the use_* are
    // mutually exclusive, so precedence only decides which candidates a position lists).
    // narrow touches p1..q1; wide8 touches p2..q2; wide16 touches p6..q6 (p7/q7 read-only).
    let blend_w16 = |orig: __m256i, w16: __m256i| _mm256_blendv_epi8(orig, w16, use_wide16);
    let blend_w8_w16 = |orig: __m256i, w8: __m256i, w16: __m256i| {
        _mm256_blendv_epi8(_mm256_blendv_epi8(orig, w8, use_wide8), w16, use_wide16)
    };
    let blend_all = |orig: __m256i, narrow: __m256i, w8: __m256i, w16: __m256i| {
        _mm256_blendv_epi8(
            _mm256_blendv_epi8(_mm256_blendv_epi8(orig, narrow, use_narrow), w8, use_wide8),
            w16,
            use_wide16,
        )
    };

    let p6_out = blend_w16(p6, op6_wide2);
    let p5_out = blend_w16(p5, op5_wide2);
    let p4_out = blend_w16(p4, op4_wide2);
    let p3_out = blend_w16(p3, op3_wide2);
    let p2_out = blend_w8_w16(p2, op2_wide, op2_wide2);
    let p1_out = blend_all(p1, op1_narrow, op1_wide, op1_wide2);
    let p0_out = blend_all(p0, op0_narrow, op0_wide, op0_wide2);
    let q0_out = blend_all(q0, oq0_narrow, oq0_wide, oq0_wide2);
    let q1_out = blend_all(q1, oq1_narrow, oq1_wide, oq1_wide2);
    let q2_out = blend_w8_w16(q2, oq2_wide, oq2_wide2);
    let q3_out = blend_w16(q3, oq3_wide2);
    let q4_out = blend_w16(q4, oq4_wide2);
    let q5_out = blend_w16(q5, oq5_wide2);
    let q6_out = blend_w16(q6, oq6_wide2);

    store_row(-7, p6_out);
    store_row(-6, p5_out);
    store_row(-5, p4_out);
    store_row(-4, p3_out);
    store_row(-3, p2_out);
    store_row(-2, p1_out);
    store_row(-1, p0_out);
    store_row(0, q0_out);
    store_row(1, q1_out);
    store_row(2, q2_out);
    store_row(3, q3_out);
    store_row(4, q4_out);
    store_row(5, q5_out);
    store_row(6, q6_out);
}

/// Transposes an 8x8 matrix of `u16` (`r[i]` = input row `i`, 8 lanes) so the returned
/// `out[j]` holds input column `j` across all 8 rows. The standard SSE2 unpack network
/// (interleave by 16-, then 32-, then 64-bit granularity); it is its own inverse. Only SSE2
/// (baseline on x86_64), so callable from any x86_64 context. Used by [`loop_filter_vert8_avx2`]
/// to turn a vertical edge (taps along a row) into the row-major layout the horizontal kernel
/// expects, and to turn the filtered result back.
#[inline]
unsafe fn transpose8x8_u16(r: &[__m128i; 8]) -> [__m128i; 8] {
    let a0 = _mm_unpacklo_epi16(r[0], r[1]);
    let a1 = _mm_unpackhi_epi16(r[0], r[1]);
    let a2 = _mm_unpacklo_epi16(r[2], r[3]);
    let a3 = _mm_unpackhi_epi16(r[2], r[3]);
    let a4 = _mm_unpacklo_epi16(r[4], r[5]);
    let a5 = _mm_unpackhi_epi16(r[4], r[5]);
    let a6 = _mm_unpacklo_epi16(r[6], r[7]);
    let a7 = _mm_unpackhi_epi16(r[6], r[7]);
    let b0 = _mm_unpacklo_epi32(a0, a2);
    let b1 = _mm_unpackhi_epi32(a0, a2);
    let b2 = _mm_unpacklo_epi32(a1, a3);
    let b3 = _mm_unpackhi_epi32(a1, a3);
    let b4 = _mm_unpacklo_epi32(a4, a6);
    let b5 = _mm_unpackhi_epi32(a4, a6);
    let b6 = _mm_unpacklo_epi32(a5, a7);
    let b7 = _mm_unpackhi_epi32(a5, a7);
    [
        _mm_unpacklo_epi64(b0, b4),
        _mm_unpackhi_epi64(b0, b4),
        _mm_unpacklo_epi64(b1, b5),
        _mm_unpackhi_epi64(b1, b5),
        _mm_unpacklo_epi64(b2, b6),
        _mm_unpackhi_epi64(b2, b6),
        _mm_unpacklo_epi64(b3, b7),
        _mm_unpackhi_epi64(b3, b7),
    ]
}

/// AVX2 VERTICAL-edge deblocking filter (`loop_filter.rs` pass==0: taps run along a row, i.e.
/// `dx=1,dy=0`), the transpose of [`loop_filter_horiz8_avx2`]. The 8 along-edge positions here
/// are 8 consecutive ROWS `y0..y0+7` at column `x0` (the along-edge axis is strided by the
/// plane, the taps `x0-8..x0+7` are contiguous within each row) -- so this transposes the tap
/// window into the row-major layout the horizontal kernel wants, runs that already-proven
/// kernel unchanged, and transposes the result back. All the bit-exact filter arithmetic (mask,
/// narrow, wide8, wide16) is thus shared verbatim; only the load/store orientation differs.
///
/// `plane_base`/`plane_width`, the 0/-1 lane masks, and `limit`/`blimit`/`thresh` mean exactly
/// what they do for [`loop_filter_horiz8_avx2`]; `(x0, y0)` is lane 0's position and the other 7
/// lanes are the next 7 rows `y0+1..=y0+7` at the same column `x0`.
///
/// # Safety
/// Caller must confirm `avx2_enabled()` and that the tap window is in bounds: columns
/// `x0-4..=x0+3` across rows `y0..=y0+7` always, AND -- whenever any lane's `is_tx16` is set --
/// the wider columns `x0-8..=x0+7`. This is the transpose of the horizontal kernel's contract
/// (rows<->cols swapped) and holds under the same reasoning: planes are allocated out to
/// superblock boundaries in both dimensions (see `framebuffer::Plane`), the wider window is only
/// read when an eligible `is_tx16` lane forces a 16-aligned interior edge, and non-eligible
/// lanes/columns are written back with their original transposed-in value (a no-op).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub unsafe fn loop_filter_vert8_avx2(
    plane_base: *mut u16,
    plane_width: usize,
    x0: usize,
    y0: usize,
    eligible: &[i32; 8],
    is_tx8: &[i32; 8],
    is_tx16: &[i32; 8],
    limit: &[i32; 8],
    blimit: &[i32; 8],
    thresh: &[i32; 8],
    bit_depth: u8,
) {
    // Wide window (16 tap columns x0-8..x0+7) iff a lane needs the TX_16X16 wide16 filter, else
    // the narrow window (8 tap columns x0-4..x0+3). Mirrors the horizontal kernel's own is_tx16
    // branch and its memory-window contract: the wider columns are only read when an is_tx16
    // lane is present, which the caller sets only for eligible TX_16X16 (16-aligned) lanes.
    let wide = is_tx16.iter().any(|&t| t != 0);
    let center = if wide { 8usize } else { 4usize }; // q0's tap-row in the scratch
    let col0 = x0 - center; // leftmost tap column (p3 for the narrow window, p7 for the wide)

    let base = plane_base;
    // scratch: up to 16 tap-rows x 8 lane-cols, row-major stride 8 -- the layout the horizontal
    // kernel reads (tap axis = scratch rows, along-edge axis = scratch cols). Only the first
    // `_win` rows are filled/used; the narrow case leaves rows 8..15 untouched (never read).
    let mut scratch = [0u16; 16 * 8];

    // Transpose the plane tap-window (8 rows y0..y0+7 x `_win` cols from col0) into scratch, one
    // 8x8 block per 8 columns. Afterwards scratch row `tr` is tap column `col0+tr` across the 8
    // lanes -- so scratch row `center` is q0's column x0, matching a horizontal edge at row
    // `center`.
    let mut rows = [_mm_setzero_si128(); 8];
    for (r, slot) in rows.iter_mut().enumerate() {
        *slot = _mm_loadu_si128(base.add((y0 + r) * plane_width + col0) as *const __m128i);
    }
    let t = transpose8x8_u16(&rows);
    for (tr, &v) in t.iter().enumerate() {
        _mm_storeu_si128(scratch.as_mut_ptr().add(tr * 8) as *mut __m128i, v);
    }
    if wide {
        for (r, slot) in rows.iter_mut().enumerate() {
            *slot = _mm_loadu_si128(base.add((y0 + r) * plane_width + col0 + 8) as *const __m128i);
        }
        let t = transpose8x8_u16(&rows);
        for (tr, &v) in t.iter().enumerate() {
            _mm_storeu_si128(scratch.as_mut_ptr().add((8 + tr) * 8) as *mut __m128i, v);
        }
    }

    // Apply the proven horizontal kernel to the scratch: a width-8 "plane" whose edge is at row
    // `center` (q0). Its own narrow/wide8/wide16 selection and p3..q3 / p7..q7 window reads land
    // exactly on the scratch rows filled above.
    loop_filter_horiz8_avx2(
        scratch.as_mut_ptr(),
        8,
        0,
        center,
        eligible,
        is_tx8,
        is_tx16,
        limit,
        blimit,
        thresh,
        bit_depth,
    );

    // Transpose the (now filtered) scratch back to the plane. Rows the kernel left unwritten keep
    // the original values transposed in above, so writing the whole window back is a no-op there.
    for (tr, slot) in rows.iter_mut().enumerate() {
        *slot = _mm_loadu_si128(scratch.as_ptr().add(tr * 8) as *const __m128i);
    }
    let out = transpose8x8_u16(&rows);
    for (r, &v) in out.iter().enumerate() {
        _mm_storeu_si128(base.add((y0 + r) * plane_width + col0) as *mut __m128i, v);
    }
    if wide {
        for (tr, slot) in rows.iter_mut().enumerate() {
            *slot = _mm_loadu_si128(scratch.as_ptr().add((8 + tr) * 8) as *const __m128i);
        }
        let out = transpose8x8_u16(&rows);
        for (r, &v) in out.iter().enumerate() {
            _mm_storeu_si128(
                base.add((y0 + r) * plane_width + col0 + 8) as *mut __m128i,
                v,
            );
        }
    }
}
