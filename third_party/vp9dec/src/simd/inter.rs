//! AVX2 SIMD mirror of `predict::block_inter_predict`'s scalar two-pass 8-tap subpel
//! convolution (spec §8.5.2.4): specialized kernels for the common UNSCALED case
//! (`x_step == y_step == 16`, i.e. reference frame same size as the current frame) plus a
//! general edge-clamping kernel
//! ([`block_inter_predict_scaled_avx2`]). The latter handles both scaled references (SVC /
//! resize) and unscaled blocks whose 8-tap window crosses a reference edge. `predict.rs`
//! owns the dispatch and the scalar fallback; this module owns only the vector kernels.

use crate::predict::{MAX_BLOCK_DIM, MAX_INTERMEDIATE_HEIGHT};
use crate::subpel::SUBPEL_FILTERS;
// VENDOR(uefi-player): core path works in both std and no_std builds.
use core::arch::x86_64::*;

/// AVX2 mirror of `predict::block_inter_predict`'s scalar two-pass 8-tap FIR.
///
/// Only valid for the unscaled case (`x_step == y_step == 16`), which lets the caller
/// simplify the scalar loop's per-column `filters[(p & 15) as usize]` lookup and
/// `(p >> 4)` row/column arithmetic down to a single filter (`fx`/`fy`) and a flat
/// `src_row0`/`src_col0` origin -- see `predict.rs`'s call site for the derivation
/// (`p & 15` and `p >> 4 - c` are both step-invariant when `step == 16`).
///
/// `w` must be a multiple of 8 (this processes 8 output columns per AVX2 lane group;
/// width-4 blocks use the companion `_w4` kernel or the general edge-clamping kernel).
/// `h <= MAX_BLOCK_DIM`,
/// `intermediate_height <= MAX_INTERMEDIATE_HEIGHT`. Works for all bit depths: `ref_data` is
/// `Plane::as_slice()`'s `u16` buffer (every plane is `u16`-backed, see `framebuffer.rs`), the
/// subpel FIR is bit-depth-agnostic, and the i32 accumulation holds a 12-bit sample through both
/// passes -- only the `clip1` bound differs, so the caller passes `max_val = (1<<bit_depth)-1`
/// and the kernel clips each pass to `0..=max_val`.
///
/// # Safety
/// The caller must have confirmed `avx2_enabled()` (this fn requires the `avx2` target
/// feature to be genuinely available at runtime) and must guarantee that every source
/// pixel this call reads -- rows `src_row0..src_row0 + intermediate_height` and columns
/// `src_col0..src_col0 + w + 7` of `ref_data` (row-major, stride `ref_width`) -- lies
/// within `ref_data`'s bounds. Unlike the scalar path, this function does **not**
/// clamp/border-replicate: reproducing that with AVX2 would need a byte gather (x86 has
/// none), so the caller instead routes an out-of-bounds window to the general
/// edge-clamping AVX2 kernel (see `predict.rs`'s `unscaled_in_bounds` check).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub unsafe fn block_inter_predict_avx2(
    ref_data: &[u16],
    ref_width: usize,
    src_row0: i64,
    src_col0: i64,
    fx: usize,
    fy: usize,
    w: usize,
    h: usize,
    intermediate_height: usize,
    interp_filter: u8,
    max_val: i32,
    pred: &mut [i32],
) {
    debug_assert_eq!(w % 8, 0);
    debug_assert!(w <= MAX_BLOCK_DIM && h <= MAX_BLOCK_DIM);
    debug_assert!(intermediate_height <= MAX_INTERMEDIATE_HEIGHT);
    debug_assert_eq!(pred.len(), w * h);
    debug_assert!(src_row0 >= 0 && src_col0 >= 0);
    debug_assert!(
        ((src_row0 as usize) + intermediate_height - 1) * ref_width + (src_col0 as usize) + w + 6
            < ref_data.len()
    );

    let filters = &SUBPEL_FILTERS[interp_filter as usize];
    let hcoeffs = &filters[fx];
    let vcoeffs = &filters[fy];

    let round_add = _mm256_set1_epi32(64);
    let zero = _mm256_setzero_si256();
    let max_clip = _mm256_set1_epi32(max_val);

    // Same fixed-size scratch as the scalar path's `intermediate`: the spec's two-pass
    // structure needs the full horizontal-filter output (for rows both above and below
    // any given output row) before the vertical pass can run.
    let mut intermediate = [0i32; MAX_INTERMEDIATE_HEIGHT * MAX_BLOCK_DIM];

    // Horizontal pass. `fx` is the one filter used for every row/column here (the
    // x_step==16 simplification), and `ref_y = src_row0 + r` / `ref_x = src_col0 + c + t`
    // need no clamp -- the caller (predict.rs) already proved every such read is
    // in-bounds.
    let ref_base = ref_data.as_ptr();
    for r in 0..intermediate_height {
        let row_ptr = ref_base.add(((src_row0 as usize) + r) * ref_width + src_col0 as usize);
        let mut c = 0usize;
        while c < w {
            let mut acc = zero;
            for (t, &tap) in hcoeffs.iter().enumerate() {
                // 8 u16 samples -> 8 x i32 (zero-extend, matching the scalar `.get(..) as
                // i32` on a u16 sample); reads exactly the columns this chunk's 8 outputs
                // need for this tap, no more (see the Safety section's bound).
                let vals = _mm_loadu_si128(row_ptr.add(c + t) as *const __m128i);
                let widened = _mm256_cvtepu16_epi32(vals);
                let coeff = _mm256_set1_epi32(tap);
                acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(widened, coeff));
            }
            // round2(s, 7) then clip1 (Clip3(0, max_val, .)) -- identical arithmetic and
            // accumulation order to the scalar `round2`/`clip1`, just 8 lanes at once.
            acc = _mm256_srai_epi32(_mm256_add_epi32(acc, round_add), 7);
            acc = _mm256_min_epi32(_mm256_max_epi32(acc, zero), max_clip);
            _mm256_storeu_si256(
                intermediate.as_mut_ptr().add(r * w + c) as *mut __m256i,
                acc,
            );
            c += 8;
        }
    }

    // Vertical pass. `fy` is the one filter used for every output row (same
    // simplification), and `base_row == r` exactly (the y_step==16 case of the scalar
    // loop's `p >> 4`), so tap `t` reads intermediate row `r + t` directly.
    for r in 0..h {
        let mut c = 0usize;
        while c < w {
            let mut acc = zero;
            for (t, &tap) in vcoeffs.iter().enumerate() {
                let p = intermediate.as_ptr().add((r + t) * w + c);
                let vals = _mm256_loadu_si256(p as *const __m256i);
                let coeff = _mm256_set1_epi32(tap);
                acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(vals, coeff));
            }
            acc = _mm256_srai_epi32(_mm256_add_epi32(acc, round_add), 7);
            acc = _mm256_min_epi32(_mm256_max_epi32(acc, zero), max_clip);
            _mm256_storeu_si256(pred.as_mut_ptr().add(r * w + c) as *mut __m256i, acc);
            c += 8;
        }
    }
}

/// Width-4 companion to [`block_inter_predict_avx2`]: the same UNSCALED 8-tap two-pass subpel
/// convolution, but 4 output columns per row (a natural 128-bit `_mm_*` lane group) instead of
/// 8. Width-4 blocks (the `4x4` / `4x8` partitions) are not a multiple of the main kernel's
/// 8-wide group, so they had stayed on the scalar path. Same arithmetic, order and clipping as
/// the main kernel (so bit-exact with it and the scalar loop), just 4 lanes wide.
///
/// # Safety
/// Same contract as [`block_inter_predict_avx2`] with `w == 4`: caller must have confirmed
/// `avx2_enabled()` and that every source pixel this reads -- rows
/// `src_row0..src_row0 + intermediate_height` and columns `src_col0..src_col0 + 4 + 7` of
/// `ref_data` (row-major, stride `ref_width`) -- is within bounds (the caller's
/// `unscaled_in_bounds` check covers this and routes reference-edge blocks to the general
/// edge-clamping kernel).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub unsafe fn block_inter_predict_avx2_w4(
    ref_data: &[u16],
    ref_width: usize,
    src_row0: i64,
    src_col0: i64,
    fx: usize,
    fy: usize,
    h: usize,
    intermediate_height: usize,
    interp_filter: u8,
    max_val: i32,
    pred: &mut [i32],
) {
    debug_assert!(h <= MAX_BLOCK_DIM);
    debug_assert!(intermediate_height <= MAX_INTERMEDIATE_HEIGHT);
    debug_assert_eq!(pred.len(), 4 * h);
    debug_assert!(src_row0 >= 0 && src_col0 >= 0);
    debug_assert!(
        ((src_row0 as usize) + intermediate_height - 1) * ref_width + (src_col0 as usize) + 4 + 6
            < ref_data.len()
    );

    let filters = &SUBPEL_FILTERS[interp_filter as usize];
    let hcoeffs = &filters[fx];
    let vcoeffs = &filters[fy];

    let round_add = _mm_set1_epi32(64);
    let zero = _mm_setzero_si128();
    let max_clip = _mm_set1_epi32(max_val);

    // Stride-4 scratch (only the first `intermediate_height * 4` entries are used); the same
    // two-pass structure as the main kernel.
    let mut intermediate = [0i32; MAX_INTERMEDIATE_HEIGHT * 4];

    // Horizontal pass (4 columns; `fx` is the one filter, `src_col0 + t` needs no clamp -- the
    // caller proved it in-bounds).
    let ref_base = ref_data.as_ptr();
    for r in 0..intermediate_height {
        let row_ptr = ref_base.add(((src_row0 as usize) + r) * ref_width + src_col0 as usize);
        let mut acc = zero;
        for (t, &tap) in hcoeffs.iter().enumerate() {
            // 4 u16 -> 4 x i32 (zero-extend, matching the scalar `u16 as i32`): exactly the 4
            // columns this tap needs for the 4 outputs, no more (see the Safety bound).
            let vals = _mm_cvtepu16_epi32(_mm_loadl_epi64(row_ptr.add(t) as *const __m128i));
            acc = _mm_add_epi32(acc, _mm_mullo_epi32(vals, _mm_set1_epi32(tap)));
        }
        acc = _mm_srai_epi32(_mm_add_epi32(acc, round_add), 7);
        acc = _mm_min_epi32(_mm_max_epi32(acc, zero), max_clip);
        _mm_storeu_si128(intermediate.as_mut_ptr().add(r * 4) as *mut __m128i, acc);
    }

    // Vertical pass (4 columns; tap `t` reads intermediate row `r + t` directly, the
    // y_step==16 case).
    for r in 0..h {
        let mut acc = zero;
        for (t, &tap) in vcoeffs.iter().enumerate() {
            let vals = _mm_loadu_si128(intermediate.as_ptr().add((r + t) * 4) as *const __m128i);
            acc = _mm_add_epi32(acc, _mm_mullo_epi32(vals, _mm_set1_epi32(tap)));
        }
        acc = _mm_srai_epi32(_mm_add_epi32(acc, round_add), 7);
        acc = _mm_min_epi32(_mm_max_epi32(acc, zero), max_clip);
        _mm_storeu_si128(pred.as_mut_ptr().add(r * 4) as *mut __m128i, acc);
    }
}

/// General edge-clamping AVX2 mirror of `predict::block_inter_predict_scalar`'s two-pass
/// 8-tap subpel FIR (spec §8.5.2.3-8.5.2.4). Used for scaled references (`x_step`/`y_step`
/// differ from 16 -- SVC / resize) and for unscaled blocks whose filter window crosses a
/// reference edge. Same arithmetic, accumulation order, `round2(7)` and `clip1` as the scalar
/// loops -- bit-exact by construction. What differs structurally from the direct-load unscaled
/// kernels:
///
/// - **Horizontal pass**: the source position `p = x + x_step*c` gives every output column its
///   own subpel phase `p & 15` (filter row) and source column `p >> 4`. Both are ROW-invariant,
///   so they are precomputed once per call -- per-column gather indices plus the filter taps
///   transposed to tap-major vectors -- and each tap's 8 samples are fetched with
///   `_mm256_i32gather_epi32` from a per-row i32 scratch of the (edge-clamped) source span.
/// - **Vertical pass**: `p = (y & 15) + y_step*r` depends only on the output row, so the phase
///   and source row are uniform across columns -- the same shape as the unscaled vertical pass,
///   just with a per-row filter and base row.
/// - **Edge clamping happens INSIDE the kernel**: the scratch is filled through precomputed
///   `clamp(col, 0, last_x)` source columns and each row through `clamp(row, 0, last_y)`,
///   reproducing the scalar border replication exactly -- so unlike the unscaled kernels there
///   is no `in_bounds` scalar fallback; any block position is valid.
///
/// `w` must be 4 or a multiple of 8; width 4 pads its single 8-lane group by replicating
/// column 3 (the pad lanes' gathers stay in bounds and their results are never stored). Works
/// for all bit depths (the same i32-FIR argument as the unscaled kernels; the caller passes
/// `max_val = (1 << bit_depth) - 1`).
///
/// # Safety
/// The caller must have confirmed `avx2_enabled()`, and must guarantee:
/// `ref_data.len() >= ref_width * ref_height` (every source read is clamped into that
/// rectangle, rows `0..ref_height` x columns `0..ref_width`); `intermediate_height ==
/// (((h-1) * y_step + 15) >> 4) + 8 <= MAX_INTERMEDIATE_HEIGHT`; and the horizontal source
/// span `(((x & 15) + x_step * (w-1)) >> 4) + 8 <= MAX_INTERMEDIATE_HEIGHT`. Both bounds hold
/// for any spec-conformant scaling (§8.5.2.3 caps the steps at 32); the caller re-checks them
/// and falls back to the scalar path so a malformed stream cannot reach here with larger
/// values.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub unsafe fn block_inter_predict_scaled_avx2(
    ref_data: &[u16],
    ref_width: usize,
    ref_height: usize,
    x: i64,
    y: i64,
    x_step: i64,
    y_step: i64,
    w: usize,
    h: usize,
    intermediate_height: usize,
    interp_filter: u8,
    max_val: i32,
    pred: &mut [i32],
) {
    debug_assert!(w == 4 || w.is_multiple_of(8));
    debug_assert!(w <= MAX_BLOCK_DIM && h <= MAX_BLOCK_DIM);
    debug_assert!(intermediate_height <= MAX_INTERMEDIATE_HEIGHT);
    debug_assert_eq!(pred.len(), w * h);
    debug_assert!(ref_data.len() >= ref_width * ref_height);
    debug_assert!(x_step >= 0 && y_step >= 0);

    let filters = &SUBPEL_FILTERS[interp_filter as usize];
    let last_x = ref_width as i64 - 1;
    let last_y = ref_height as i64 - 1;

    // Padded width: w rounded up to a full 8-lane group (only w == 4 pads).
    let wp = if w == 4 { 8 } else { w };
    let groups = wp / 8;

    // Row-invariant per-column precompute (the scalar horizontal loop's `p = x + x_step*c`):
    // gather index (the column's tap-window start `(p >> 4) - 3` relative to `col_min`, i.e.
    // `(p >> 4) - (x >> 4)` -- always >= 0 since x_step >= 0) and the column's 8 filter taps,
    // transposed to tap-major so tap t of a group's 8 columns is one vector. Pad columns
    // (c >= w, only for w == 4) replicate column w-1.
    let col_min = (x >> 4) - 3;
    let span = ((((x & 15) + x_step * (w as i64 - 1)) >> 4) + 8) as usize;
    debug_assert!(span <= MAX_INTERMEDIATE_HEIGHT);
    let mut gidx = [[0i32; 8]; MAX_BLOCK_DIM / 8];
    let mut hcoef = [[[0i32; 8]; 8]; MAX_BLOCK_DIM / 8];
    for c in 0..wp {
        let p = x + x_step * (c.min(w - 1) as i64);
        gidx[c / 8][c % 8] = ((p >> 4) - 3 - col_min) as i32;
        for (t, &coeff) in filters[(p & 15) as usize].iter().enumerate() {
            hcoef[c / 8][t][c % 8] = coeff;
        }
    }

    // Clamped source columns for the per-row scratch fill: scratch[i] will hold
    // `ref_row[clamp(col_min + i, 0, last_x)]`, so a gather at index `gidx + t` reads exactly
    // the scalar's `ref_x = clamp((p >> 4) + t - 3, 0, last_x)` sample. Max gather index is
    // `gidx[w-1] + 7 == span - 1`, inside the scratch.
    let mut src_col = [0usize; MAX_INTERMEDIATE_HEIGHT];
    for (i, sc) in src_col[..span].iter_mut().enumerate() {
        *sc = (col_min + i as i64).clamp(0, last_x) as usize;
    }

    let round_add = _mm256_set1_epi32(64);
    let zero = _mm256_setzero_si256();
    let max_clip = _mm256_set1_epi32(max_val);

    // Same fixed-size scratch as the scalar path's `intermediate` (stride `wp`; pad columns
    // hold replicated-column results that the vertical pass computes but never stores).
    let mut intermediate = [0i32; MAX_INTERMEDIATE_HEIGHT * MAX_BLOCK_DIM];
    let mut scratch = [0i32; MAX_INTERMEDIATE_HEIGHT];

    // Horizontal pass: per row, widen the edge-clamped source span to i32 once, then per
    // 8-column group accumulate tap t's gathered samples times its tap-major coefficients --
    // the identical multiply/accumulate/round2(7)/clip1 sequence as the scalar loop.
    for r in 0..intermediate_height {
        let ref_y = ((y >> 4) + r as i64 - 3).clamp(0, last_y) as usize;
        let row = &ref_data[ref_y * ref_width..ref_y * ref_width + ref_width];
        for (dst, &sc) in scratch[..span].iter_mut().zip(src_col[..span].iter()) {
            *dst = row[sc] as i32;
        }
        for (g, (idx_lanes, taps)) in gidx[..groups]
            .iter()
            .zip(hcoef[..groups].iter())
            .enumerate()
        {
            let idx = _mm256_loadu_si256(idx_lanes.as_ptr() as *const __m256i);
            let mut acc = zero;
            for (t, tap_lanes) in taps.iter().enumerate() {
                // The `<4>` gather scale is `size_of::<i32>()` of the `scratch` staging buffer
                // -- a load-bearing coupling: the i32 staging exists precisely because x86 has
                // no u16 gather, so switching `scratch` to u16 would require scale 2 AND a
                // gather instruction that does not exist. The widen-to-i32-then-gather layout
                // is not an optimization choice.
                let vals = _mm256_i32gather_epi32::<4>(scratch.as_ptr().add(t), idx);
                let coeff = _mm256_loadu_si256(tap_lanes.as_ptr() as *const __m256i);
                acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(vals, coeff));
            }
            acc = _mm256_srai_epi32(_mm256_add_epi32(acc, round_add), 7);
            acc = _mm256_min_epi32(_mm256_max_epi32(acc, zero), max_clip);
            _mm256_storeu_si256(
                intermediate.as_mut_ptr().add(r * wp + g * 8) as *mut __m256i,
                acc,
            );
        }
    }

    // Vertical pass: per output row, one filter (`p & 15`) and one base row (`p >> 4`) shared
    // by every column -- the unscaled kernel's vertical pass with per-row values. `base_row +
    // 7 <= intermediate_height - 1` by the intermediate_height formula (`y & 15 <= 15`).
    for r in 0..h {
        let p = (y & 15) + y_step * (r as i64);
        let vcoeffs = &filters[(p & 15) as usize];
        let base_row = (p >> 4) as usize;
        for g in 0..groups {
            let mut acc = zero;
            for (t, &tap) in vcoeffs.iter().enumerate() {
                let ptr = intermediate.as_ptr().add((base_row + t) * wp + g * 8);
                let vals = _mm256_loadu_si256(ptr as *const __m256i);
                acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(vals, _mm256_set1_epi32(tap)));
            }
            acc = _mm256_srai_epi32(_mm256_add_epi32(acc, round_add), 7);
            acc = _mm256_min_epi32(_mm256_max_epi32(acc, zero), max_clip);
            if w == 4 {
                // The single padded group: store only the 4 real columns.
                _mm_storeu_si128(
                    pred.as_mut_ptr().add(r * 4) as *mut __m128i,
                    _mm256_castsi256_si128(acc),
                );
            } else {
                _mm256_storeu_si256(pred.as_mut_ptr().add(r * w + g * 8) as *mut __m256i, acc);
            }
        }
    }
}
