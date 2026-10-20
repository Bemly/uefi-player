//! Motion vector prediction (spec §6.5 "Motion vector prediction"):
//! `find_mv_refs`/`find_best_ref_mvs`/`append_sub8x8_mvs` and their neighbor-scanning helpers.
//!
//! Also absorbs the former standalone `src/mv.rs` module (W5): its pure helpers (clamping,
//! sign inversion, threshold checks) were factored out on their own specifically because
//! `find_mv_refs`/`find_best_ref_mvs`/`append_sub8x8_mvs` had to stay in `tile.rs` (borrow
//! convenience with `TileDecoder`/`MiGrid`); now that those methods live in this
//! `tile`-submodule alongside them, keeping the pure helpers in a separate top-level module
//! serves no purpose.

use super::TileDecoder;
use crate::common::INTRA_FRAME;
use crate::mv_ref_tables::{
    COUNTER_TO_CONTEXT, IDX_N_COLUMN_TO_SUBBLOCK, MODE_2_COUNTER, MV_REF_BLOCKS,
};
use crate::prob_tables::{NUM_8X8_BLOCKS_HIGH_LOOKUP, NUM_8X8_BLOCKS_WIDE_LOOKUP};

/// A motion vector (`[row, col]`, in units of 1/8 pel).
pub type Mv = [i32; 2];

/// `ZeroMv` (spec §6.4.18 and others). Used from `tile::mode_info` too (`assign_mv`/
/// `inter_block_mode_info`), hence `pub(super)`.
pub(super) const ZERO_MV: Mv = [0, 0];

/// `MVREF_NEIGHBOURS` (spec §3, constants list). The number of neighbor
/// candidates `find_mv_refs` scans.
const MVREF_NEIGHBOURS: usize = 8;

/// `MV_BORDER` (spec §3). The clamp border used by `clamp_mv_ref` at the end
/// of `find_mv_refs`.
const MV_BORDER: i32 = 128;

/// `(BORDERINPIXELS - INTERP_EXTEND) << 3` (spec §6.5.12). The clamp border
/// used by `find_best_ref_mvs` (`BORDERINPIXELS = 160`, `INTERP_EXTEND = 4`).
const MV_PRED_BORDER: i32 = (160 - 4) << 3;

/// `COMPANDED_MVREF_THRESH` (spec §3). The threshold used by `use_mv_hp`.
const COMPANDED_MVREF_THRESH: i32 = 8;

/// `MI_SIZE` (spec §3). The side length in pixels of an 8x8 mode info unit.
const MI_SIZE: i32 = 8;

/// `clamp_mv_row( mvec, border )` (spec §6.5.4).
fn clamp_mv_row(mvec: i32, border: i32, mi_row: u32, bh: u32, mi_rows: u32) -> i32 {
    let mb_to_top_edge = -((mi_row as i32) * MI_SIZE * 8);
    let mb_to_bottom_edge = ((mi_rows as i32) - (bh as i32) - (mi_row as i32)) * MI_SIZE * 8;
    mvec.clamp(mb_to_top_edge - border, mb_to_bottom_edge + border)
}

/// `clamp_mv_col( mvec, border )` (spec §6.5.5).
fn clamp_mv_col(mvec: i32, border: i32, mi_col: u32, bw: u32, mi_cols: u32) -> i32 {
    let mb_to_left_edge = -((mi_col as i32) * MI_SIZE * 8);
    let mb_to_right_edge = ((mi_cols as i32) - (bw as i32) - (mi_col as i32)) * MI_SIZE * 8;
    mvec.clamp(mb_to_left_edge - border, mb_to_right_edge + border)
}

/// `add_mv_ref_list( refList )` (spec §6.5.6). Handles appending to
/// `RefListMv`/`RefMvCount`, including deduplication (a value equal to
/// `RefListMv[0]` is not added) and capping at a maximum of 2 entries.
fn add_mv_ref_list(ref_list_mv: &mut [Mv; 2], ref_mv_count: &mut usize, candidate: Mv) {
    if *ref_mv_count >= 2 {
        return;
    }
    if *ref_mv_count > 0 && candidate == ref_list_mv[0] {
        return;
    }
    ref_list_mv[*ref_mv_count] = candidate;
    *ref_mv_count += 1;
}

/// `scale_mv( refList, refFrame )` (spec §6.5.9). Flips the sign of the
/// motion vector if `ref_frame_sign_bias` differs between the candidate
/// frame and the target reference frame.
fn scale_mv(
    candidate_mv: Mv,
    cand_frame: u8,
    ref_frame: u8,
    ref_frame_sign_bias: &[bool; 4],
) -> Mv {
    if ref_frame_sign_bias[cand_frame as usize] != ref_frame_sign_bias[ref_frame as usize] {
        [-candidate_mv[0], -candidate_mv[1]]
    } else {
        candidate_mv
    }
}

/// `use_mv_hp( deltaMv )` (spec §6.5.13). Used from `tile::mode_info` too (`read_mv`), hence
/// `pub(super)`.
pub(super) fn use_mv_hp(delta_mv: Mv) -> bool {
    (delta_mv[0].abs() >> 3) < COMPANDED_MVREF_THRESH
        && (delta_mv[1].abs() >> 3) < COMPANDED_MVREF_THRESH
}

impl TileDecoder {
    /// `is_inside( candidateR, candidateC )` (spec §6.5.2).
    fn is_inside(&self, r: i32, c: i32) -> bool {
        r >= 0
            && (r as u32) < self.mi_rows
            && c >= self.mi_col_start as i32
            && c < self.mi_col_end as i32
    }

    /// `get_block_mv( candidateR, candidateC, refList, usePrev )` (spec §6.5.10).
    /// The return value is `(CandidateMv, CandidateFrame)`.
    fn get_block_mv(&self, row: u32, col: u32, ref_list: usize, use_prev: bool) -> (Mv, u8) {
        if use_prev {
            let grid = self
                .prev_mi_grid
                .as_ref()
                .expect("prev_mi_grid must be Some when use_prev_frame_mvs is true");
            let info = grid.get(row, col);
            (info.mv[ref_list], info.ref_frame[ref_list])
        } else {
            let info = self.mi_grid.get(row, col);
            (info.mv[ref_list], info.ref_frame[ref_list])
        }
    }

    /// `if_same_ref_frame_add_mv( candidateR, candidateC, refFrame, usePrev )` (spec §6.5.7).
    fn if_same_ref_frame_add_mv(
        &self,
        row: u32,
        col: u32,
        ref_frame: u8,
        use_prev: bool,
        ref_list_mv: &mut [Mv; 2],
        ref_mv_count: &mut usize,
    ) {
        for j in 0..2 {
            let (cand_mv, cand_frame) = self.get_block_mv(row, col, j, use_prev);
            if cand_frame == ref_frame {
                add_mv_ref_list(ref_list_mv, ref_mv_count, cand_mv);
                return;
            }
        }
    }

    /// `if_diff_ref_frame_add_mv( candidateR, candidateC, refFrame, usePrev )` (spec §6.5.8).
    fn if_diff_ref_frame_add_mv(
        &self,
        row: u32,
        col: u32,
        ref_frame: u8,
        use_prev: bool,
        ref_list_mv: &mut [Mv; 2],
        ref_mv_count: &mut usize,
    ) {
        let (mv0, frame0) = self.get_block_mv(row, col, 0, use_prev);
        let (mv1, frame1) = self.get_block_mv(row, col, 1, use_prev);
        let mvs_same = mv0 == mv1;
        if frame0 > INTRA_FRAME && frame0 != ref_frame {
            let scaled = scale_mv(mv0, frame0, ref_frame, &self.ref_frame_sign_bias);
            add_mv_ref_list(ref_list_mv, ref_mv_count, scaled);
        }
        if frame1 > INTRA_FRAME && frame1 != ref_frame && !mvs_same {
            let scaled = scale_mv(mv1, frame1, ref_frame, &self.ref_frame_sign_bias);
            add_mv_ref_list(ref_list_mv, ref_mv_count, scaled);
        }
    }

    /// `find_mv_refs( refFrame, block )` (spec §6.5.1). The return value is `(RefListMv, ModeContext)`.
    pub(super) fn find_mv_refs(
        &self,
        row: u32,
        col: u32,
        mi_size: u8,
        ref_frame: u8,
        block: i32,
    ) -> ([Mv; 2], u8) {
        let mut ref_list_mv = [ZERO_MV; 2];
        let mut ref_mv_count = 0usize;
        let mut different_ref_found = false;
        let mut context_counter: u32 = 0;

        let search = &MV_REF_BLOCKS[mi_size as usize];

        for &(dr, dc) in search.iter().take(2) {
            let cr = row as i32 + dr;
            let cc = col as i32 + dc;
            if self.is_inside(cr, cc) {
                different_ref_found = true;
                let cand = self.mi_grid.get(cr as u32, cc as u32);
                context_counter += MODE_2_COUNTER[cand.y_mode as usize] as u32;
                for j in 0..2 {
                    if cand.ref_frame[j] == ref_frame {
                        let idx = if block >= 0 {
                            IDX_N_COLUMN_TO_SUBBLOCK[block as usize][(dc == 0) as usize] as usize
                        } else {
                            3
                        };
                        add_mv_ref_list(&mut ref_list_mv, &mut ref_mv_count, cand.sub_mvs[j][idx]);
                        break;
                    }
                }
            }
        }

        for &(dr, dc) in search.iter().skip(2).take(MVREF_NEIGHBOURS - 2) {
            let cr = row as i32 + dr;
            let cc = col as i32 + dc;
            if self.is_inside(cr, cc) {
                different_ref_found = true;
                self.if_same_ref_frame_add_mv(
                    cr as u32,
                    cc as u32,
                    ref_frame,
                    false,
                    &mut ref_list_mv,
                    &mut ref_mv_count,
                );
            }
        }

        if self.use_prev_frame_mvs {
            self.if_same_ref_frame_add_mv(
                row,
                col,
                ref_frame,
                true,
                &mut ref_list_mv,
                &mut ref_mv_count,
            );
        }

        if different_ref_found {
            for &(dr, dc) in search.iter().take(MVREF_NEIGHBOURS) {
                let cr = row as i32 + dr;
                let cc = col as i32 + dc;
                if self.is_inside(cr, cc) {
                    self.if_diff_ref_frame_add_mv(
                        cr as u32,
                        cc as u32,
                        ref_frame,
                        false,
                        &mut ref_list_mv,
                        &mut ref_mv_count,
                    );
                }
            }
        }

        if self.use_prev_frame_mvs {
            self.if_diff_ref_frame_add_mv(
                row,
                col,
                ref_frame,
                true,
                &mut ref_list_mv,
                &mut ref_mv_count,
            );
        }

        let mode_context = COUNTER_TO_CONTEXT[context_counter.min(18) as usize];

        let bh = NUM_8X8_BLOCKS_HIGH_LOOKUP[mi_size as usize] as u32;
        let bw = NUM_8X8_BLOCKS_WIDE_LOOKUP[mi_size as usize] as u32;
        for mv in ref_list_mv.iter_mut() {
            mv[0] = clamp_mv_row(mv[0], MV_BORDER, row, bh, self.mi_rows);
            mv[1] = clamp_mv_col(mv[1], MV_BORDER, col, bw, self.mi_cols);
        }

        (ref_list_mv, mode_context)
    }

    /// `find_best_ref_mvs( refList )` (spec §6.5.12). The return value is `(NearestMv, NearMv, BestMv)`.
    pub(super) fn find_best_ref_mvs(
        &self,
        row: u32,
        col: u32,
        mi_size: u8,
        ref_list_mv: [Mv; 2],
    ) -> (Mv, Mv, Mv) {
        let bh = NUM_8X8_BLOCKS_HIGH_LOOKUP[mi_size as usize] as u32;
        let bw = NUM_8X8_BLOCKS_WIDE_LOOKUP[mi_size as usize] as u32;
        let mut out = ref_list_mv;
        for mv in out.iter_mut() {
            let mut delta_row = mv[0];
            let mut delta_col = mv[1];
            if !self.allow_high_precision_mv || !use_mv_hp(*mv) {
                if delta_row & 1 != 0 {
                    delta_row += if delta_row > 0 { -1 } else { 1 };
                }
                if delta_col & 1 != 0 {
                    delta_col += if delta_col > 0 { -1 } else { 1 };
                }
            }
            mv[0] = clamp_mv_row(delta_row, MV_PRED_BORDER, row, bh, self.mi_rows);
            mv[1] = clamp_mv_col(delta_col, MV_PRED_BORDER, col, bw, self.mi_cols);
        }
        (out[0], out[1], out[0])
    }

    /// `append_sub8x8_mvs( block, refList )` (spec §6.5.14). The return value is `(NearestMv, NearMv)`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn append_sub8x8_mvs(
        &self,
        row: u32,
        col: u32,
        mi_size: u8,
        block: i32,
        ref_frame: u8,
        ref_list: usize,
        block_mvs: &[[Mv; 4]; 2],
    ) -> (Mv, Mv) {
        let (ref_list_mv, _ctx) = self.find_mv_refs(row, col, mi_size, ref_frame, block);
        // Fixed-size list (never holds more than 2 entries) + length counter, the same
        // pattern `find_mv_refs` uses via `add_mv_ref_list` -- avoids a per-block heap alloc.
        let mut sub8x8 = [ZERO_MV; 2];
        let mut len; // assigned in every arm below before any read.

        if block == 0 {
            // Unconditional, no dedup (unlike the `add_mv_ref_list` calls below).
            sub8x8[0] = ref_list_mv[0];
            sub8x8[1] = ref_list_mv[1];
            len = 2;
        } else if block <= 2 {
            sub8x8[0] = block_mvs[ref_list][0];
            len = 1;
        } else {
            sub8x8[0] = block_mvs[ref_list][2];
            len = 1;
            for &idx in &[1usize, 0] {
                add_mv_ref_list(&mut sub8x8, &mut len, block_mvs[ref_list][idx]);
            }
        }
        for &cand in ref_list_mv.iter().take(2) {
            add_mv_ref_list(&mut sub8x8, &mut len, cand);
        }
        if len < 2 {
            sub8x8[len] = ZERO_MV;
        }
        (sub8x8[0], sub8x8[1])
    }
}

#[cfg(test)]
#[path = "../../tests/unit/tile/mv_pred.rs"]
mod tests;
