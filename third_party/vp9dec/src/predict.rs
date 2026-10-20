//! Prediction processes: intra prediction (spec §8.5.1) and inter prediction
//! (spec §8.5.2). Both are called per plane by [`crate::tile::TileDecoder`]'s
//! residual path (`src/tile/residual.rs`).
//!
//! [`predict_intra`] implements the 10 intra modes (`DC_PRED` through
//! `TM_PRED`; VP9 has no smooth-style filters, unlike VP8 or AV1). Per
//! transform block the caller passes the [`crate::framebuffer::Plane`] to
//! predict into, the block's top-left `(x, y)`,
//! `have_left`/`have_above`/`not_on_right` (availability at frame/block
//! edges), `tx_size`, the mode, and the clip bounds `max_x`/`max_y` (= the
//! spec's `maxX`/`maxY`, which differ per plane).
//!
//! [`predict_inter`] implements motion compensation: MV selection, edge
//! clamping, and reference-frame scaling (§8.5.2.1-8.5.2.3) feeding the
//! per-block subpel interpolation (§8.5.2.4, `block_inter_predict`), plus
//! compound (two-reference) averaging. `block_inter_predict` dispatches to
//! the AVX2 kernels in `src/simd/inter.rs` (unscaled and reference-scaled) when
//! available; `block_inter_predict_scalar` is the always-kept fallback and
//! the bit-exactness oracle the SIMD unit tests pin against.

use crate::common::{clip3, round2};
use crate::framebuffer::Plane;
use crate::prob_tables::{
    BLOCK_8X8, D117_PRED, D135_PRED, D153_PRED, D207_PRED, D45_PRED, D63_PRED, DC_PRED, H_PRED,
    NUM_8X8_BLOCKS_HIGH_LOOKUP, NUM_8X8_BLOCKS_WIDE_LOOKUP, TM_PRED, TX_4X4, V_PRED,
};
use crate::subpel::{
    INTERP_EXTEND, REF_SCALE_SHIFT, SUBPEL_BITS, SUBPEL_FILTERS, SUBPEL_MASK, SUBPEL_SHIFTS,
};
use crate::tile::Mv;

/// Spec §8.5.1's `predict_intra` process.
///
/// `x`/`y` are absolute pixel coordinates within `plane`; `max_x`/`max_y` are
/// the spec's `maxX`/`maxY` (= the maximum coordinates that may be
/// referenced within the plane).
#[allow(clippy::too_many_arguments)]
pub fn predict_intra(
    plane: &mut Plane,
    x: usize,
    y: usize,
    have_left: bool,
    have_above: bool,
    not_on_right: bool,
    tx_size: u8,
    mode: u8,
    max_x: usize,
    max_y: usize,
    bit_depth: u8,
) {
    let log2_size = (tx_size as u32) + 2;
    let size = 1usize << log2_size;
    let base = 1i32 << (bit_depth - 1);

    // Fixed-size scratch (size <= 32, the 32x32 max transform): avoids per-block heap
    // allocations. above_row holds the spec's indices -1..=2*size-1, offset by +1 into a
    // 0..=2*size array (max length 2*32+1 == 65).
    let mut above_row_buf = [0i32; 65];
    let above_row = &mut above_row_buf[..2 * size + 1];
    for i in 0..size {
        above_row[i + 1] = if have_above {
            let sx = (x + i).min(max_x);
            plane.get(sx, y - 1) as i32
        } else {
            base - 1
        };
    }
    for i in size..2 * size {
        above_row[i + 1] = if have_above && not_on_right && tx_size == TX_4X4 {
            let sx = (x + i).min(max_x);
            plane.get(sx, y - 1) as i32
        } else {
            above_row[size]
        };
    }
    above_row[0] = if have_above && have_left {
        let sx = x.saturating_sub(1).min(max_x);
        plane.get(sx, y - 1) as i32
    } else if have_above {
        base + 1
    } else {
        base - 1
    };
    // above_row[i+1] corresponds to the spec's aboveRow[i]; aboveRow[-1] is above_row[0].
    let above = |i: i32| -> i32 { above_row[(i + 1) as usize] };

    let mut left_col_buf = [0i32; 32];
    let left_col = &mut left_col_buf[..size];
    for (i, slot) in left_col.iter_mut().enumerate() {
        *slot = if have_left {
            let sy = (y + i).min(max_y);
            plane.get(x - 1, sy) as i32
        } else {
            base + 1
        };
    }

    let mut pred_buf = [0i32; 1024];
    let pred = &mut pred_buf[..size * size];
    let at = |i: usize, j: usize| i * size + j;

    match mode {
        V_PRED => {
            for i in 0..size {
                for j in 0..size {
                    pred[at(i, j)] = above(j as i32);
                }
            }
        }
        H_PRED => {
            for i in 0..size {
                for j in 0..size {
                    pred[at(i, j)] = left_col[i];
                }
            }
        }
        D207_PRED => {
            for j in 0..size {
                pred[at(size - 1, j)] = left_col[size - 1];
            }
            for i in 0..size.saturating_sub(1) {
                pred[at(i, 0)] = round2(left_col[i] + left_col[i + 1], 1);
            }
            for i in 0..size.saturating_sub(2) {
                pred[at(i, 1)] = round2(left_col[i] + 2 * left_col[i + 1] + left_col[i + 2], 2);
            }
            if size >= 2 {
                pred[at(size - 2, 1)] = round2(left_col[size - 2] + 3 * left_col[size - 1], 2);
            }
            if size >= 3 {
                for i in (0..=(size - 2)).rev() {
                    for j in 2..size {
                        pred[at(i, j)] = pred[at(i + 1, j - 2)];
                    }
                }
            }
        }
        D45_PRED => {
            for i in 0..size {
                for j in 0..size {
                    let s = (i + j) as i32;
                    pred[at(i, j)] = if s + 2 < (size as i32) * 2 {
                        round2(above(s) + above(s + 1) * 2 + above(s + 2), 2)
                    } else {
                        above(2 * size as i32 - 1)
                    };
                }
            }
        }
        D63_PRED => {
            for i in 0..size {
                for j in 0..size {
                    let half = (i / 2) as i32 + j as i32;
                    pred[at(i, j)] = if i & 1 == 1 {
                        round2(above(half) + above(half + 1) * 2 + above(half + 2), 2)
                    } else {
                        round2(above(half) + above(half + 1), 1)
                    };
                }
            }
        }
        D117_PRED => {
            for j in 0..size {
                pred[at(0, j)] = round2(above(j as i32 - 1) + above(j as i32), 1);
            }
            pred[at(1, 0)] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
            for j in 1..size {
                pred[at(1, j)] = round2(
                    above(j as i32 - 2) + 2 * above(j as i32 - 1) + above(j as i32),
                    2,
                );
            }
            if size >= 3 {
                pred[at(2, 0)] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for i in 3..size {
                pred[at(i, 0)] = round2(left_col[i - 3] + 2 * left_col[i - 2] + left_col[i - 1], 2);
            }
            for i in 2..size {
                for j in 1..size {
                    pred[at(i, j)] = pred[at(i - 2, j - 1)];
                }
            }
        }
        D135_PRED => {
            pred[at(0, 0)] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
            for j in 1..size {
                pred[at(0, j)] = round2(
                    above(j as i32 - 2) + 2 * above(j as i32 - 1) + above(j as i32),
                    2,
                );
            }
            if size >= 2 {
                pred[at(1, 0)] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for i in 2..size {
                pred[at(i, 0)] = round2(left_col[i - 2] + 2 * left_col[i - 1] + left_col[i], 2);
            }
            for i in 1..size {
                for j in 1..size {
                    pred[at(i, j)] = pred[at(i - 1, j - 1)];
                }
            }
        }
        D153_PRED => {
            pred[at(0, 0)] = round2(left_col[0] + above(-1), 1);
            for i in 1..size {
                pred[at(i, 0)] = round2(left_col[i - 1] + left_col[i], 1);
            }
            if size >= 2 {
                pred[at(0, 1)] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
                pred[at(1, 1)] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for i in 2..size {
                pred[at(i, 1)] = round2(left_col[i - 2] + 2 * left_col[i - 1] + left_col[i], 2);
            }
            for j in 2..size {
                pred[at(0, j)] = round2(
                    above(j as i32 - 3) + 2 * above(j as i32 - 2) + above(j as i32 - 1),
                    2,
                );
            }
            for i in 1..size {
                for j in 2..size {
                    pred[at(i, j)] = pred[at(i - 1, j - 2)];
                }
            }
        }
        TM_PRED => {
            let top_left = above(-1);
            for i in 0..size {
                for j in 0..size {
                    pred[at(i, j)] = clip3(
                        0,
                        (1 << bit_depth) - 1,
                        above(j as i32) + left_col[i] - top_left,
                    );
                }
            }
        }
        DC_PRED => {
            let value = if have_left && have_above {
                let mut sum = 0i32;
                for (k, &l) in left_col.iter().enumerate() {
                    sum += l;
                    sum += above(k as i32);
                }
                (sum + size as i32) >> (log2_size + 1)
            } else if have_left {
                let sum: i32 = left_col.iter().sum();
                (sum + (1 << (log2_size - 1))) >> log2_size
            } else if have_above {
                let sum: i32 = (0..size).map(|k| above(k as i32)).sum();
                (sum + (1 << (log2_size - 1))) >> log2_size
            } else {
                base
            };
            pred.fill(value);
        }
        _ => unreachable!("predict_intra: unknown intra prediction mode {mode}"),
    }

    for i in 0..size {
        for j in 0..size {
            plane.set(x + j, y + i, pred[at(i, j)] as u16);
        }
    }
}

/// A view over one plane of a reference frame (equivalent to the spec's
/// `FrameStore[ refIdx ][ plane ]` + `RefFrameWidth`/`RefFrameHeight`).
/// `width`/`height` are the overall frame size based on luma (plane 0) — this
/// is exactly the `RefFrameWidth[refIdx]` used in the scaling computation of
/// spec §8.5.2.3 (pass the luma size even for chroma planes). `plane` is the
/// reference pixel data already cropped to the relevant plane (`Plane::width`/
/// `height` are directly the spec's `lastX+1`/`lastY+1`).
pub struct RefPlaneView<'a> {
    pub plane: &'a Plane,
    pub width: u32,
    pub height: u32,
}

#[inline]
fn round_mv_comp_q2(value: i32) -> i32 {
    if value < 0 {
        (value - 1) / 2
    } else {
        (value + 1) / 2
    }
}

#[inline]
fn round_mv_comp_q4(value: i32) -> i32 {
    if value < 0 {
        (value - 2) / 4
    } else {
        (value + 2) / 4
    }
}

/// `clip1(x)` = `Clip3( 0, (1<<BitDepth)-1, x )` (spec §4).
#[inline]
fn clip1(x: i32, bit_depth: u8) -> i32 {
    clip3(0, (1i32 << bit_depth) - 1, x)
}

/// Per-block-invariant parameters for `predict_inter` (spec §8.5.2) and its helpers
/// (`select_mv`/`clamp_mv_for_plane`/`scale_mv_for_plane`).
///
/// A single coding block's `residual()` (`src/tile/residual.rs`) calls `predict_inter` once
/// per plane (further split into 4x4 sub-blocks when `MiSize < BLOCK_8X8`); every field here
/// is identical across all of those calls -- only `dst`/`plane`/`x`/`y`/`w`/`h`/`block_idx`
/// (`predict_inter`'s remaining positional parameters) vary call to call.
pub struct InterPredictParams<'a> {
    pub ref_frame: [u8; 2],
    pub block_mvs: &'a [[Mv; 4]; 2],
    pub interp_filter: u8,
    pub mi_row: u32,
    pub mi_col: u32,
    pub mi_size: u8,
    pub mi_rows: u32,
    pub mi_cols: u32,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub bit_depth: u8,
    pub refs: [Option<&'a RefPlaneView<'a>>; 2],
}

/// Motion vector selection process (spec §8.5.2.1 "Motion vector selection process").
fn select_mv(plane: usize, ref_list: usize, block_idx: usize, p: &InterPredictParams) -> Mv {
    let bm = &p.block_mvs[ref_list];
    if plane == 0 || p.mi_size >= BLOCK_8X8 {
        return bm[block_idx];
    }
    match (p.subsampling_x, p.subsampling_y) {
        (0, 0) => bm[block_idx],
        (0, 1) => [
            round_mv_comp_q2(bm[block_idx][0] + bm[block_idx + 2][0]),
            round_mv_comp_q2(bm[block_idx][1] + bm[block_idx + 2][1]),
        ],
        (1, 0) => [
            round_mv_comp_q2(bm[block_idx][0] + bm[block_idx + 1][0]),
            round_mv_comp_q2(bm[block_idx][1] + bm[block_idx + 1][1]),
        ],
        _ => [
            round_mv_comp_q4(bm[0][0] + bm[1][0] + bm[2][0] + bm[3][0]),
            round_mv_comp_q4(bm[0][1] + bm[1][1] + bm[2][1] + bm[3][1]),
        ],
    }
}

/// Motion vector clamping process (spec §8.5.2.2 "Motion vector clamping process").
fn clamp_mv_for_plane(plane: usize, mv: Mv, p: &InterPredictParams) -> Mv {
    let (sx, sy) = if plane == 0 {
        (0i32, 0i32)
    } else {
        (p.subsampling_x as i32, p.subsampling_y as i32)
    };
    let bh = NUM_8X8_BLOCKS_HIGH_LOOKUP[p.mi_size as usize] as i32;
    let bw = NUM_8X8_BLOCKS_WIDE_LOOKUP[p.mi_size as usize] as i32;
    let mi_row = p.mi_row as i32;
    let mi_col = p.mi_col as i32;
    let mi_rows = p.mi_rows as i32;
    let mi_cols = p.mi_cols as i32;

    let mb_to_top_edge = -((mi_row * 8) * 16) >> sy;
    let mb_to_bottom_edge = ((mi_rows - bh - mi_row) * 8 * 16) >> sy;
    let mb_to_left_edge = -((mi_col * 8) * 16) >> sx;
    let mb_to_right_edge = ((mi_cols - bw - mi_col) * 8 * 16) >> sx;

    let spel_left = (INTERP_EXTEND + ((bw * 8) >> sx)) << SUBPEL_BITS;
    let spel_right = spel_left - SUBPEL_SHIFTS;
    let spel_top = (INTERP_EXTEND + ((bh * 8) >> sy)) << SUBPEL_BITS;
    let spel_bottom = spel_top - SUBPEL_SHIFTS;

    [
        clip3(
            mb_to_top_edge - spel_top,
            mb_to_bottom_edge + spel_bottom,
            (2 * mv[0]) >> sy,
        ),
        clip3(
            mb_to_left_edge - spel_left,
            mb_to_right_edge + spel_right,
            (2 * mv[1]) >> sx,
        ),
    ]
}

/// Motion vector scaling process (spec §8.5.2.3 "Motion vector scaling process").
/// Returns `(startX, startY, stepX, stepY)` (in 1/16 pel units). `ref_width`/`ref_height` are
/// per-call (the specific reference's own size, `view.width`/`view.height` at the call site),
/// unlike the rest of `p`.
fn scale_mv_for_plane(
    plane: usize,
    x: usize,
    y: usize,
    clamped_mv: Mv,
    ref_width: u32,
    ref_height: u32,
    p: &InterPredictParams,
) -> (i64, i64, i64, i64) {
    let x_scale = ((ref_width as i64) << REF_SCALE_SHIFT) / p.frame_width as i64;
    let y_scale = ((ref_height as i64) << REF_SCALE_SHIFT) / p.frame_height as i64;
    let base_x = ((x as i64) * x_scale) >> REF_SCALE_SHIFT;
    let base_y = ((y as i64) * y_scale) >> REF_SCALE_SHIFT;
    let luma_x = if plane > 0 {
        (x as u32) << p.subsampling_x
    } else {
        x as u32
    } as i64;
    let luma_y = if plane > 0 {
        (y as u32) << p.subsampling_y
    } else {
        y as u32
    } as i64;
    let frac_x = ((16 * luma_x * x_scale) >> REF_SCALE_SHIFT) & SUBPEL_MASK as i64;
    let frac_y = ((16 * luma_y * y_scale) >> REF_SCALE_SHIFT) & SUBPEL_MASK as i64;
    let d_x = ((clamped_mv[1] as i64 * x_scale) >> REF_SCALE_SHIFT) + frac_x;
    let d_y = ((clamped_mv[0] as i64 * y_scale) >> REF_SCALE_SHIFT) + frac_y;
    let step_x = (16 * x_scale) >> REF_SCALE_SHIFT;
    let step_y = (16 * y_scale) >> REF_SCALE_SHIFT;
    let start_x = (base_x << SUBPEL_BITS) + d_x;
    let start_y = (base_y << SUBPEL_BITS) + d_y;
    (start_x, start_y, step_x, step_y)
}

/// Max output block dimension for a single `predict_inter` call (spec: 64x64 is the
/// largest coding block, and chroma calls are always <= that due to subsampling).
/// `pub(crate)`: shared with `src/simd/inter.rs`'s AVX2 mirror of [`block_inter_predict`].
pub(crate) const MAX_BLOCK_DIM: usize = 64;

/// Max rows needed in [`block_inter_predict`]'s intermediate (horizontal-filter) buffer.
/// `h <= MAX_BLOCK_DIM`, and the vertical step `y_step` is bounded by the spec's reference-
/// scaling conformance requirement (§8.5.2.3: `RefFrameHeight <= 2 * FrameHeight`), which
/// caps `y_step <= 32` (1/16-pel units; see [`scale_mv_for_plane`]); the 8-tap subpel filter
/// needs 8 extra rows of context. `(((MAX_BLOCK_DIM - 1) * 32 + 15) >> 4) + 8 == 134`.
/// The 2x bound is *enforced*, not assumed: `decode_block` (`src/tile.rs`) rejects any block
/// referencing a frame beyond it (`TileError::RefFrameSizeOutOfRange`), so decode-path
/// callers never exceed this scratch size on any input, conformant or not.
/// `pub(crate)`: shared with `src/simd/inter.rs`'s AVX2 mirror of [`block_inter_predict`].
pub(crate) const MAX_INTERMEDIATE_HEIGHT: usize = 134;

/// Per-block inter prediction process (spec §8.5.2.4 "Block inter prediction process").
/// Writes `pred[r*w+c]` (`r`=0..h-1, `c`=0..w-1) into the caller-provided `pred` buffer
/// (length exactly `h*w`) -- avoids a per-call heap allocation (this runs once per sub-8x8
/// chroma 4x4 block, the hottest call site).
#[allow(clippy::too_many_arguments)]
fn block_inter_predict(
    ref_plane: &Plane,
    x: i64,
    y: i64,
    x_step: i64,
    y_step: i64,
    w: usize,
    h: usize,
    interp_filter: u8,
    bit_depth: u8,
    pred: &mut [i32],
) {
    debug_assert!(w <= MAX_BLOCK_DIM && h <= MAX_BLOCK_DIM);
    debug_assert_eq!(pred.len(), w * h);

    // Current SIMD coverage (docs/implementation-notes.md): AVX2 fast path for the common
    // unscaled case (spec §8.5.2.3's x_step == y_step == 16, i.e. reference frame same size as the
    // current frame -- the overwhelming majority of content). When x_step/y_step == 16,
    // `p & 15` in the scalar loops is step-invariant (adding a multiple of 16 never
    // changes the low 4 bits), so there's exactly one filter for the whole call instead
    // of one per column/row; and `p >> 4` reduces to a flat per-call offset (`c` alone
    // for the horizontal pass's column, `r` alone for the vertical pass's row -- see
    // `simd/inter.rs`'s doc comment for the derivation). A block whose source window needs
    // border replication instead routes to the general edge-clamping AVX2 kernel below; x86
    // has no u16 gather, so that kernel first widens the clamped source span to i32. All bit
    // depths use the kernels: the subpel FIR is bit-depth-agnostic and its i32 accumulation
    // holds a 12-bit sample through the two passes; only the `clip1` bound differs, passed as
    // `max_val = (1<<bit_depth)-1`. Width 4 (the `4x4`/`4x8` partitions) dispatches to the
    // 128-bit-wide `block_inter_predict_avx2_w4`; widths 8/16/32/64 to the 256-bit
    // `block_inter_predict_avx2`.
    // VENDOR(uefi-player): AVX2 dispatch needs x86_64 (see simd.rs
    // `avx2_enabled`: `[accel]` switch + firmware CPUID/XCR0 gate under
    // no_std); off-arch this block is compiled out and
    // `block_inter_predict_scalar` below always runs.
    #[cfg(target_arch = "x86_64")]
    {
        let last_x = ref_plane.width as i64 - 1;
        let last_y = ref_plane.height as i64 - 1;
        let intermediate_height = (((h as i64 - 1) * y_step + 15) >> 4) + 8;
        let unscaled = x_step == 16 && y_step == 16;
        let src_row0 = (y >> 4) - 3;
        let src_col0 = (x >> 4) - 3;
        let unscaled_in_bounds = unscaled
            && src_row0 >= 0
            && src_row0 + intermediate_height - 1 <= last_y
            && src_col0 >= 0
            && src_col0 + (w as i64 - 1) + 7 <= last_x;
        if x_step == 16
            && y_step == 16
            && (w == 4 || w.is_multiple_of(8))
            && crate::simd::avx2_enabled()
        {
            let max_val = (1i32 << bit_depth) - 1;
            if unscaled_in_bounds {
                let fx = (x & 15) as usize;
                let fy = (y & 15) as usize;
                // SAFETY: `avx2_enabled()` proved AVX2 support; `unscaled_in_bounds` proves every
                // source pixel the kernel touches (rows src_row0..src_row0+intermediate_height,
                // columns src_col0..src_col0+w+7) is within `ref_plane`, matching the kernels'
                // documented contract.
                unsafe {
                    if w == 4 {
                        crate::simd::block_inter_predict_avx2_w4(
                            ref_plane.as_slice(),
                            ref_plane.width,
                            src_row0,
                            src_col0,
                            fx,
                            fy,
                            h,
                            intermediate_height as usize,
                            interp_filter,
                            max_val,
                            pred,
                        );
                    } else {
                        crate::simd::block_inter_predict_avx2(
                            ref_plane.as_slice(),
                            ref_plane.width,
                            src_row0,
                            src_col0,
                            fx,
                            fy,
                            w,
                            h,
                            intermediate_height as usize,
                            interp_filter,
                            max_val,
                            pred,
                        );
                    }
                }
                return;
            }
        }

        // General edge-clamping path: scaled references (SVC / resize: x_step or y_step != 16)
        // and unscaled blocks whose 8-tap source window crosses a reference edge. Unlike the
        // direct-load unscaled kernels, this one edge-clamps internally (every source read goes
        // through precomputed clamped indices, reproducing the scalar border replication
        // exactly). Steps are <= 32 by construction here: `decode_block`
        // (`src/tile.rs`) rejects any block whose reference exceeds spec §8.5.2.3's 2x bound
        // (`TileError::RefFrameSizeOutOfRange`) before prediction, which caps both quantities
        // below at MAX_INTERMEDIATE_HEIGHT (the same `(((MAX_BLOCK_DIM - 1) * 32 + 15) >> 4)
        // + 8` derivation on each axis). The two bound checks are therefore defensive
        // redundancy for the kernel's fixed scratch sizes, NOT a malformed-stream handler --
        // the scalar fallback's own fixed scratch has the identical limit and would panic on
        // its slice bounds past it.
        if (w == 4 || w.is_multiple_of(8)) && crate::simd::avx2_enabled() {
            let span = (((x & 15) + x_step * (w as i64 - 1)) >> 4) + 8;
            if intermediate_height as usize <= MAX_INTERMEDIATE_HEIGHT
                && span as usize <= MAX_INTERMEDIATE_HEIGHT
            {
                let max_val = (1i32 << bit_depth) - 1;
                // SAFETY: `avx2_enabled()` proved AVX2 support; `ref_plane.as_slice()` is
                // exactly `width * height` samples (`Plane::new`), and the kernel clamps
                // every source read to that rectangle; the two checks above bound its
                // fixed-size scratch indices.
                unsafe {
                    crate::simd::block_inter_predict_scaled_avx2(
                        ref_plane.as_slice(),
                        ref_plane.width,
                        ref_plane.height,
                        x,
                        y,
                        x_step,
                        y_step,
                        w,
                        h,
                        intermediate_height as usize,
                        interp_filter,
                        max_val,
                        pred,
                    );
                }
                return;
            }
        }
    }

    block_inter_predict_scalar(
        ref_plane,
        x,
        y,
        x_step,
        y_step,
        w,
        h,
        interp_filter,
        bit_depth,
        pred,
    );
}

/// Scalar body of [`block_inter_predict`] (the spec §8.5.2.4 two-pass loops, verbatim): the
/// always-kept fallback for every case the AVX2 kernels don't take (`VP9DEC_NO_SIMD=1`,
/// non-x86_64, or an unsupported dispatch shape), and the bit-exactness oracle
/// `tests/unit/simd.rs`'s unit tests pin the kernels against (hence `pub(crate)`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn block_inter_predict_scalar(
    ref_plane: &Plane,
    x: i64,
    y: i64,
    x_step: i64,
    y_step: i64,
    w: usize,
    h: usize,
    interp_filter: u8,
    bit_depth: u8,
    pred: &mut [i32],
) {
    debug_assert!(w <= MAX_BLOCK_DIM && h <= MAX_BLOCK_DIM);
    debug_assert_eq!(pred.len(), w * h);
    let last_x = ref_plane.width as i64 - 1;
    let last_y = ref_plane.height as i64 - 1;
    let intermediate_height = (((h as i64 - 1) * y_step + 15) >> 4) + 8;
    debug_assert!(intermediate_height as usize <= MAX_INTERMEDIATE_HEIGHT);

    let filters = &SUBPEL_FILTERS[interp_filter as usize];

    // Fixed-size scratch, sized for the worst case; only the first `intermediate_height * w`
    // entries are written/read below.
    let mut intermediate = [0i32; MAX_INTERMEDIATE_HEIGHT * MAX_BLOCK_DIM];
    for r in 0..intermediate_height {
        let ref_y = ((y >> 4) + r - 3).clamp(0, last_y) as usize;
        for c in 0..w {
            let p = x + x_step * (c as i64);
            let coeffs = &filters[(p & 15) as usize];
            let mut s = 0i32;
            for (t, &coeff) in coeffs.iter().enumerate() {
                let ref_x = ((p >> 4) + t as i64 - 3).clamp(0, last_x) as usize;
                s += coeff * ref_plane.get(ref_x, ref_y) as i32;
            }
            intermediate[(r as usize) * w + c] = clip1(round2(s, 7), bit_depth);
        }
    }

    for r in 0..h {
        let p = (y & 15) + y_step * (r as i64);
        let coeffs = &filters[(p & 15) as usize];
        let base_row = (p >> 4) as usize;
        for c in 0..w {
            let mut s = 0i32;
            for (t, &coeff) in coeffs.iter().enumerate() {
                s += coeff * intermediate[(base_row + t) * w + c];
            }
            pred[r * w + c] = clip1(round2(s, 7), bit_depth);
        }
    }
}

/// `predict_inter()` (spec §8.5.2 "Inter prediction process").
///
/// Writes motion-compensated, sub-pixel-interpolated inter prediction
/// samples into the `w x h` region of `dst` with top-left corner `(x, y)`.
/// `block_idx` is the spec's `blockIdx` (in 4x4 units, representing how much
/// of this block has been predicted so far; only meaningful when
/// `MiSize < BLOCK_8X8`). `p` groups the per-block-invariant parameters (see
/// [`InterPredictParams`]); still `#[allow(too_many_arguments)]` at 8 (down from the
/// pre-W5 20).
#[allow(clippy::too_many_arguments)]
pub fn predict_inter(
    dst: &mut Plane,
    plane: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    block_idx: usize,
    p: &InterPredictParams,
) {
    use crate::prob_tables::INTRA_FRAME;
    let is_compound = p.ref_frame[1] > INTRA_FRAME;
    let n_refs = 1 + is_compound as usize;

    // Fixed-size scratch (w, h <= MAX_BLOCK_DIM): avoids a per-call heap allocation. Only the
    // first w*h entries of each slot are written/read below.
    let mut preds = [[0i32; MAX_BLOCK_DIM * MAX_BLOCK_DIM]; 2];
    for (ref_list, pred_slot) in preds.iter_mut().enumerate().take(n_refs) {
        let mv = select_mv(plane, ref_list, block_idx, p);
        let clamped = clamp_mv_for_plane(plane, mv, p);
        let view = p.refs[ref_list].expect("reference frame slot is missing (DPB not initialized)");
        let (start_x, start_y, step_x, step_y) =
            scale_mv_for_plane(plane, x, y, clamped, view.width, view.height, p);
        block_inter_predict(
            view.plane,
            start_x,
            start_y,
            step_x,
            step_y,
            w,
            h,
            p.interp_filter,
            p.bit_depth,
            &mut pred_slot[..w * h],
        );
    }

    for i in 0..h {
        for j in 0..w {
            let value = if is_compound {
                round2(preds[0][i * w + j] + preds[1][i * w + j], 1)
            } else {
                preds[0][i * w + j]
            };
            dst.set(x + j, y + i, value as u16);
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/predict.rs"]
mod tests;
