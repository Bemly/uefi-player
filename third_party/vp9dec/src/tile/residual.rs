//! `residual()` (spec §6.4.21) and everything it calls directly: per-plane intra/inter
//! prediction dispatch, token reading (spec §6.4.24, §6.4.26), and inverse quantization +
//! inverse transform + reconstruction (spec §8.6.2). Also the per-frame dequant step table
//! builder (`build_dequant_table`, added W4b) since its only caller is `TileDecoder::new_with_prev`
//! and its only reason to exist is feeding this pipeline's dequantization step.

use super::{MiInfo, TileDecoder};
use crate::bool_coder::BoolDecoder;
use crate::common::{get_uv_tx_size as common_get_uv_tx_size, INTRA_FRAME};
use crate::dpb::RefFrameData;
use crate::header::{SegmentationParams, MAX_SEGMENTS};
use crate::predict::predict_intra as predict_intra_block;
use crate::predict::{predict_inter, InterPredictParams, RefPlaneView};
use crate::prob_tables::{
    coefband_8x8plus, mode2txfm_map, pareto, BLOCK_8X8, CAT_PROBS, COEFBAND_4X4, DCT_VAL_CATEGORY6,
    ENERGY_CLASS, EXTRA_BITS, LAST_FRAME, NUM_4X4_BLOCKS_HIGH_LOOKUP, NUM_4X4_BLOCKS_WIDE_LOOKUP,
    SS_SIZE_LOOKUP, TX_16X16, TX_32X32, TX_4X4, TX_8X8, ZERO_TOKEN,
};
use crate::quant::{get_ac_quant, get_dc_quant, get_qindex};
use crate::scan::{get_scan, TxSize};
use crate::transform::{inverse_transform_block, TxType};

/// Builds the per-frame dequant step table, indexed `[segment_id][plane_kind][dc=0/ac=1]`
/// (`plane_kind`: 0 = luma, 1 = chroma -- `get_dc_quant`/`get_ac_quant` only distinguish
/// `plane == 0` from `plane != 0`, so chroma U and V share row 1).
///
/// `get_qindex`/`get_dc_quant`/`get_ac_quant` (spec §8.6.1) depend only on `segment_id` and
/// frame-level header values (`base_q_idx`, the `delta_q_*` fields, `bit_depth`) -- all fixed
/// for the whole frame -- so building this table once here (instead of re-deriving it on every
/// transform block) is exactly equivalent.
pub(super) fn build_dequant_table(
    segmentation: &SegmentationParams,
    base_q_idx: u8,
    bit_depth: u8,
    delta_q_y_dc: i32,
    delta_q_uv_dc: i32,
    delta_q_uv_ac: i32,
) -> [[[i64; 2]; 2]; MAX_SEGMENTS] {
    let mut table = [[[0i64; 2]; 2]; MAX_SEGMENTS];
    for (segment_id, seg_row) in table.iter_mut().enumerate() {
        let qindex = get_qindex(base_q_idx, segmentation, segment_id);
        for (plane_kind, dc_ac) in seg_row.iter_mut().enumerate() {
            let dc_quant = get_dc_quant(bit_depth, qindex, plane_kind, delta_q_y_dc, delta_q_uv_dc);
            let ac_quant = get_ac_quant(bit_depth, qindex, plane_kind, delta_q_uv_ac);
            *dc_ac = [dc_quant as i64, ac_quant as i64];
        }
    }
    table
}

impl TileDecoder {
    /// `get_uv_tx_size( )` (spec §6.4.22).
    fn get_uv_tx_size(&self, mi_size: u8, tx_size: u8) -> u8 {
        common_get_uv_tx_size(mi_size, tx_size, self.subsampling_x, self.subsampling_y)
    }

    /// `get_plane_block_size( subsize, plane )` (spec §6.4.23).
    fn get_plane_block_size(&self, subsize: u8, plane: usize) -> u8 {
        let subx = if plane > 0 { self.subsampling_x } else { 0 } as usize;
        let suby = if plane > 0 { self.subsampling_y } else { 0 } as usize;
        SS_SIZE_LOOKUP[subsize as usize][subx][suby]
    }

    /// `residual( )` (spec §6.4.21). The return value is `EobTotal` (spec §6.4.4).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn residual(
        &mut self,
        r: &mut BoolDecoder,
        row: u32,
        col: u32,
        info: &MiInfo,
        avail_u: bool,
        avail_l: bool,
        is_inter: bool,
    ) -> u32 {
        let bsize = if info.mi_size < BLOCK_8X8 {
            BLOCK_8X8
        } else {
            info.mi_size
        };
        let mut eob_total = 0u32;

        for plane in 0..3usize {
            let tx_sz = if plane > 0 {
                self.get_uv_tx_size(info.mi_size, info.tx_size)
            } else {
                info.tx_size
            };
            let step = 1u32 << tx_sz;
            let plane_sz = self.get_plane_block_size(bsize, plane);
            let num4x4w = NUM_4X4_BLOCKS_WIDE_LOOKUP[plane_sz as usize] as u32;
            let num4x4h = NUM_4X4_BLOCKS_HIGH_LOOKUP[plane_sz as usize] as u32;
            let sub_x = if plane > 0 { self.subsampling_x } else { 0 };
            let sub_y = if plane > 0 { self.subsampling_y } else { 0 };
            let base_x = (col * 8) >> sub_x;
            let base_y = (row * 8) >> sub_y;
            let maxx = (self.mi_cols * 8) >> sub_x;
            let maxy = (self.mi_rows * 8) >> sub_y;
            // Note that maxX/maxY passed to predict_intra (spec §8.5.1) are the clip bounds
            // in whole-plane coordinates, which is a different meaning from maxx/maxy above
            // (used for in-block clipping in residual).
            let pred_max_x = ((self.mi_cols * 8) >> sub_x).saturating_sub(1) as usize;
            let pred_max_y = ((self.mi_rows * 8) >> sub_y).saturating_sub(1) as usize;

            if is_inter {
                self.predict_inter_for_plane(
                    plane, row, col, info, base_x, base_y, num4x4w, num4x4h,
                );
            }

            let mut block_idx = 0u32;
            let mut y = 0u32;
            while y < num4x4h {
                let mut x = 0u32;
                while x < num4x4w {
                    let start_x = base_x + 4 * x;
                    let start_y = base_y + 4 * y;
                    let mut nonzero = false;

                    if start_x < maxx && start_y < maxy {
                        if !is_inter {
                            let mode = if plane > 0 {
                                info.uv_mode
                            } else if info.mi_size >= BLOCK_8X8 {
                                info.y_mode
                            } else {
                                info.sub_modes[block_idx as usize]
                            };
                            let have_left = avail_l || x > 0;
                            let have_above = avail_u || y > 0;
                            let not_on_right = x + step < num4x4w;
                            let _t = crate::bench_timing::StageTimer::start(
                                crate::bench_timing::Stage::IntraPredict,
                            );
                            predict_intra_block(
                                &mut self.planes[plane],
                                start_x as usize,
                                start_y as usize,
                                have_left,
                                have_above,
                                not_on_right,
                                tx_sz,
                                mode,
                                pred_max_x,
                                pred_max_y,
                                self.bit_depth,
                            );
                        }

                        if !info.skip {
                            let tx_type = self.compute_tx_type(
                                plane,
                                tx_sz,
                                info.mi_size,
                                info.y_mode,
                                &info.sub_modes,
                                block_idx as usize,
                                is_inter,
                            );
                            let _t = crate::bench_timing::StageTimer::start(
                                crate::bench_timing::Stage::TokenDequantTransform,
                            );
                            nonzero = self.tokens_and_reconstruct(
                                r,
                                plane,
                                start_x as usize,
                                start_y as usize,
                                tx_sz,
                                tx_type,
                                is_inter,
                                info.segment_id,
                            );
                        }
                    }

                    eob_total += nonzero as u32;
                    for i in 0..step {
                        self.above_nonzero_context[plane][((start_x >> 2) + i) as usize] =
                            nonzero as u8;
                        let left_idx = (((start_y >> 2) + i) % 16) as usize;
                        self.left_nonzero_context[plane][left_idx] = nonzero as u8;
                    }
                    block_idx += 1;
                    x += step;
                }
                y += step;
            }
        }
        eob_total
    }

    /// The inter-prediction step of `residual( )` for one plane: `predict_inter()`
    /// (spec §8.5.2, motion compensation / sub-pixel interpolation), including the per-4x4
    /// sub-block loop when `MiSize < BLOCK_8X8`. LANDMINE: the sub-8x8 chroma MV block index
    /// `(y * num4x4w + x)` is bit-exact-correct as written -- a plausible, spec-grounded
    /// "correction" to it once regressed the official 4:2:2 vector (see
    /// docs/implementation-notes.md); verify against the sweep before touching it.
    #[allow(clippy::too_many_arguments)]
    fn predict_inter_for_plane(
        &mut self,
        plane: usize,
        row: u32,
        col: u32,
        info: &MiInfo,
        base_x: u32,
        base_y: u32,
        num4x4w: u32,
        num4x4h: u32,
    ) {
        // SIMD measurement infrastructure (M5 in docs/history.md): times the whole
        // per-plane inter-predict section below (all sub-4x4 predict_inter calls when
        // MiSize < BLOCK_8X8 included), not per-call -- see bench_timing module docs.
        let _t = crate::bench_timing::StageTimer::start(crate::bench_timing::Stage::InterPredict);
        // `predict_inter()` (spec §8.5.2): motion compensation / sub-pixel interpolation.
        let refs: [Option<&RefFrameData>; 2] = [
            if info.ref_frame[0] > INTRA_FRAME {
                self.resolved_refs[(info.ref_frame[0] - LAST_FRAME) as usize].as_deref()
            } else {
                None
            },
            if info.ref_frame[1] > INTRA_FRAME {
                self.resolved_refs[(info.ref_frame[1] - LAST_FRAME) as usize].as_deref()
            } else {
                None
            },
        ];
        // VENDOR(uefi-player): core path works in both std and no_std builds.
        let ref_views: [Option<RefPlaneView>; 2] = core::array::from_fn(|i| {
            refs[i].map(|r| RefPlaneView {
                plane: match plane {
                    0 => &r.y,
                    1 => &r.u,
                    _ => &r.v,
                },
                width: r.width,
                height: r.height,
            })
        });
        let ref_view_refs: [Option<&RefPlaneView>; 2] =
            [ref_views[0].as_ref(), ref_views[1].as_ref()];
        let inter_params = InterPredictParams {
            ref_frame: info.ref_frame,
            block_mvs: &info.sub_mvs,
            interp_filter: info.interp_filter,
            mi_row: row,
            mi_col: col,
            mi_size: info.mi_size,
            mi_rows: self.mi_rows,
            mi_cols: self.mi_cols,
            subsampling_x: self.subsampling_x,
            subsampling_y: self.subsampling_y,
            frame_width: self.frame_width,
            frame_height: self.frame_height,
            bit_depth: self.bit_depth,
            refs: ref_view_refs,
        };

        if info.mi_size < BLOCK_8X8 {
            let mut y = 0u32;
            while y < num4x4h {
                let mut x = 0u32;
                while x < num4x4w {
                    let block_idx = (y * num4x4w + x) as usize;
                    predict_inter(
                        &mut self.planes[plane],
                        plane,
                        (base_x + 4 * x) as usize,
                        (base_y + 4 * y) as usize,
                        4,
                        4,
                        block_idx,
                        &inter_params,
                    );
                    x += 1;
                }
                y += 1;
            }
        } else {
            predict_inter(
                &mut self.planes[plane],
                plane,
                base_x as usize,
                base_y as usize,
                (num4x4w * 4) as usize,
                (num4x4h * 4) as usize,
                0,
                &inter_params,
            );
        }
    }

    /// The part of `get_scan( )` (spec §6.4.25) that determines `TxType`.
    #[allow(clippy::too_many_arguments)]
    fn compute_tx_type(
        &self,
        plane: usize,
        tx_sz: u8,
        mi_size: u8,
        y_mode: u8,
        sub_modes: &[u8; 4],
        block_idx: usize,
        is_inter: bool,
    ) -> TxType {
        if plane > 0 || tx_sz == TX_32X32 {
            TxType::DctDct
        } else if tx_sz == TX_4X4 {
            if self.lossless || is_inter {
                TxType::DctDct
            } else {
                let mode = if mi_size < BLOCK_8X8 {
                    sub_modes[block_idx]
                } else {
                    y_mode
                };
                mode2txfm_map(mode)
            }
        } else {
            // When is_inter, y_mode takes NEARESTMV..NEWMV (10..13), and mode2txfm_map maps
            // all of these to DctDct (spec §10.2).
            mode2txfm_map(y_mode)
        }
    }

    fn tx_sz_to_scan_size(tx_sz: u8) -> TxSize {
        match tx_sz {
            TX_4X4 => TxSize::Tx4x4,
            TX_8X8 => TxSize::Tx8x8,
            TX_16X16 => TxSize::Tx16x16,
            _ => TxSize::Tx32x32,
        }
    }

    /// `tokens( )` (spec §6.4.24) + `reconstruct( )` (spec §8.6.2).
    /// The return value is `nonzero` (`nonzero = c > 0` from spec §6.4.24).
    #[allow(clippy::too_many_arguments)]
    fn tokens_and_reconstruct(
        &mut self,
        r: &mut BoolDecoder,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: u8,
        tx_type: TxType,
        is_inter: bool,
        segment_id: u8,
    ) -> bool {
        let n = (tx_sz as u32) + 2;
        let n0 = 1usize << n;
        let seg_eob = n0 * n0;
        let scan = get_scan(Self::tx_sz_to_scan_size(tx_sz), tx_type);

        let plane_type = if plane > 0 { 1usize } else { 0usize };
        let sub_x = if plane > 0 { self.subsampling_x } else { 0 };
        let sub_y = if plane > 0 { self.subsampling_y } else { 0 };
        let max_x_ctx = (2 * self.mi_cols) >> sub_x;
        let max_y_ctx = (2 * self.mi_rows) >> sub_y;
        let numpts = 1u32 << tx_sz;
        let x4 = (start_x >> 2) as u32;
        let y4 = (start_y >> 2) as u32;

        // The first more_coefs bit decides whether this block has any coefficient tokens at
        // all. Read it before initializing/zeroing the 5 KiB token scratch below; an immediate
        // EOB also needs no dequant scratch or inverse transform because adding an all-zero
        // residual leaves the prediction unchanged.
        let first_ctx = {
            let mut above = 0u32;
            let mut left = 0u32;
            for i in 0..numpts {
                if x4 + i < max_x_ctx {
                    above |= self.above_nonzero_context[plane][(x4 + i) as usize] as u32;
                }
                if y4 + i < max_y_ctx {
                    left |= self.left_nonzero_context[plane][((y4 + i) % 16) as usize] as u32;
                }
            }
            (above + left) as usize
        };
        let first_band = if tx_sz == TX_4X4 {
            COEFBAND_4X4[0] as usize
        } else {
            coefband_8x8plus(0) as usize
        };
        let first_probs = self.probs.coef_probs[tx_sz as usize][plane_type][is_inter as usize]
            [first_band][first_ctx];
        let more_coefs = r.read_bool(first_probs[0]);
        self.counts.more_coefs[tx_sz as usize][plane_type][is_inter as usize][first_band]
            [first_ctx][more_coefs as usize] += 1;
        if !more_coefs {
            return false;
        }

        // Fixed-size scratch (seg_eob <= 1024, the 32x32 max transform): avoids a per-block
        // heap allocation. Only the first seg_eob entries are read below.
        let mut tokens = [0i32; 1024];
        let mut token_cache = [0u8; 1024];
        // The c=0 EOB bit was consumed by the preflight above. Once a non-zero token is read,
        // the normal loop resumes checking EOB before the next coefficient position.
        let mut check_eob = false;
        let mut c = 0usize;

        while c < seg_eob {
            let pos = scan[c] as usize;
            let band = if c == 0 {
                first_band
            } else if tx_sz == TX_4X4 {
                COEFBAND_4X4[c] as usize
            } else {
                coefband_8x8plus(c) as usize
            };

            // Derivation of ctx (spec §9.3.2, shared by more_coefs/token).
            let ctx = if c == 0 {
                first_ctx
            } else {
                let nn = 4usize << tx_sz;
                let i = pos / nn;
                let j = pos % nn;
                let (nb0, nb1) = if i > 0 && j > 0 {
                    let a = (i - 1) * nn + j;
                    let a2 = i * nn + j - 1;
                    match tx_type {
                        TxType::DctAdst => (a, a),
                        TxType::AdstDct => (a2, a2),
                        _ => (a, a2),
                    }
                } else if i > 0 {
                    let a = (i - 1) * nn + j;
                    (a, a)
                } else {
                    let a = i * nn + j - 1;
                    (a, a)
                };
                ((1 + token_cache[nb0] as u32 + token_cache[nb1] as u32) >> 1) as usize
            };

            let probs = if c == 0 {
                first_probs
            } else {
                self.probs.coef_probs[tx_sz as usize][plane_type][is_inter as usize][band][ctx]
            };

            if check_eob {
                // The more_coefs (EOB) count feeds the EOB-node adaptation (spec §8.4.3, the
                // `eob_branch` count in libvpx `decode_coefs`). It is incremented ONLY at
                // positions where the EOB flag is actually read (checkEob == 1) — i.e. the
                // first coefficient and every position after a non-zero token. Positions after
                // a zero token (checkEob == 0) skip the EOB read entirely and must NOT be
                // counted here (libvpx increments `eob_branch_count` only in the outer loop,
                // never in its inner zero-run loop).
                let more_coefs = r.read_bool(probs[0]);
                self.counts.more_coefs[tx_sz as usize][plane_type][is_inter as usize][band][ctx]
                    [more_coefs as usize] += 1;
                if !more_coefs {
                    break;
                }
            }

            let token = r.read_tree(&crate::prob_tables::TOKEN_TREE, |node| {
                if node == 0 {
                    probs[1]
                } else if node == 1 {
                    probs[2]
                } else {
                    pareto(node, probs[2])
                }
            }) as u8;
            self.counts.token[tx_sz as usize][plane_type][is_inter as usize][band][ctx]
                [(token as usize).min(2)] += 1;
            token_cache[pos] = ENERGY_CLASS[token as usize];

            if token == ZERO_TOKEN {
                tokens[pos] = 0;
                check_eob = false;
            } else {
                let coef = self.read_coef(r, token);
                let sign = r.read_literal(1) == 1;
                tokens[pos] = if sign { -coef } else { coef };
                check_eob = true;
            }

            c += 1;
        }

        let nonzero = c > 0;

        self.dequant_transform_reconstruct(
            plane, start_x, start_y, tx_sz, tx_type, segment_id, &tokens,
        );

        nonzero
    }

    /// Inverse quantization + inverse transform + reconstruction (spec §8.6.2) of one transform
    /// block's decoded `tokens` -- the tail of [`Self::tokens_and_reconstruct`], including the
    /// fused AVX2 transform+reconstruct dispatch and the scalar reconstruction loop.
    #[allow(clippy::too_many_arguments)]
    fn dequant_transform_reconstruct(
        &mut self,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: u8,
        tx_type: TxType,
        segment_id: u8,
        tokens: &[i32; 1024],
    ) {
        let n = (tx_sz as u32) + 2;
        let n0 = 1usize << n;
        let seg_eob = n0 * n0;
        let plane_type = if plane > 0 { 1usize } else { 0usize };

        let dq_denom: i64 = if tx_sz == TX_32X32 { 2 } else { 1 };
        // Per-frame dequant table (spec §8.6.1 get_qindex/get_dc_quant/get_ac_quant), built
        // once by `build_dequant_table` -- a per-block table lookup instead of re-deriving.
        let [dc_quant, ac_quant] = self.dequant_table[segment_id as usize][plane_type];
        // Fixed-size scratch (n0*n0 <= 1024, the 32x32 max transform): avoids a per-block
        // heap allocation. Only the first seg_eob (== n0*n0) entries are read below.
        let mut dequant = [0i64; 1024];
        for (idx, &t) in tokens[..seg_eob].iter().enumerate() {
            dequant[idx] = (t as i64 * ac_quant) / dq_denom;
        }
        dequant[0] = (tokens[0] as i64 * dc_quant) / dq_denom;
        // Every non-lossless transform runs a fused AVX2 transform+reconstruct (SIMD wave 4b
        // and follow-ups) that writes clipped pixels straight into the plane, skipping both the
        // i64 write-back and the scalar reconstruction loop below: DCT_DCT at every bit depth
        // (the 10/12-bit variant widens the butterfly products to i64), and the ADST-containing
        // types (ADST_DCT / DCT_ADST / ADST_ADST; sizes 4/8/16 only -- 32x32 is DCT-only) on
        // i32 lanes at 8-bit and on i64 lanes at 10/12-bit (the ADST's unrounded `S` array
        // needs `24 + BitDepth` bits of lane storage -- see the i64 section in
        // `simd/transform.rs`). Only WHT (lossless) transforms into `dequant` and takes the
        // scalar loop. The SIMD paths are bit-exact against it for conformant streams (spec
        // §8.7.1.1's stored-value bounds).
        // VENDOR(uefi-player): AVX2 dispatch needs `std` (see simd.rs); without
        // it `fused` is false and the scalar `inverse_transform_block` always runs.
        #[cfg(all(feature = "std", target_arch = "x86_64"))]
        let fused = !self.lossless && crate::simd::avx2_enabled();
        #[cfg(not(all(feature = "std", target_arch = "x86_64")))]
        let fused = false;

        {
            let _t = crate::bench_timing::StageTimer::start(
                crate::bench_timing::Stage::InverseTransform,
            );
            #[cfg(all(feature = "std", target_arch = "x86_64"))]
            if fused {
                let pw = self.planes[plane].width;
                // The kernels index the raw buffer directly, so translate the absolute column
                // into the plane's storage (`x0` > 0 only for a tile-parallel worker's column
                // strip, whose blocks all satisfy start_x >= x0 -- see `spawn_column_worker`).
                // A violated strip invariant fails loudly on `Plane::get`'s slice bounds in
                // the scalar path, but here it would be an unsafe out-of-bounds WRITE inside
                // the fused kernels -- pin it uniformly in debug.
                debug_assert!(
                    start_x >= self.planes[plane].x0 && start_x - self.planes[plane].x0 + n0 <= pw,
                    "fused-reconstruction block outside its plane strip's columns"
                );
                debug_assert!(
                    start_y + n0 <= self.planes[plane].height,
                    "fused-reconstruction block outside its plane's rows"
                );
                let local_x = start_x - self.planes[plane].x0;
                // SAFETY: avx2_enabled() checked; dequant[..seg_eob] is exactly n0*n0; the block's
                // rows/cols are in bounds (planes -- whole-frame or column strip -- are allocated
                // out to superblock boundaries); the ADST entries only run with n <= 4
                // (`compute_tx_type` returns DctDct for TX_32X32), each at its bit depth.
                unsafe {
                    if tx_type != TxType::DctDct {
                        if self.bit_depth == 8 {
                            crate::simd::inverse_transform_adst_reconstruct_avx2(
                                self.planes[plane].as_mut_slice(),
                                pw,
                                local_x,
                                start_y,
                                &dequant[..seg_eob],
                                n,
                                tx_type,
                            );
                        } else {
                            crate::simd::inverse_transform_adst_reconstruct_hbd_avx2(
                                self.planes[plane].as_mut_slice(),
                                pw,
                                local_x,
                                start_y,
                                &dequant[..seg_eob],
                                n,
                                tx_type,
                                self.bit_depth,
                            );
                        }
                    } else if self.bit_depth == 8 {
                        crate::simd::inverse_transform_dct_dct_reconstruct_avx2(
                            self.planes[plane].as_mut_slice(),
                            pw,
                            local_x,
                            start_y,
                            &dequant[..seg_eob],
                            n,
                        );
                    } else {
                        crate::simd::inverse_transform_dct_dct_reconstruct_hbd_avx2(
                            self.planes[plane].as_mut_slice(),
                            pw,
                            local_x,
                            start_y,
                            &dequant[..seg_eob],
                            n,
                            self.bit_depth,
                        );
                    }
                }
            } else {
                inverse_transform_block(&mut dequant[..seg_eob], n, tx_type, self.lossless);
            }
            #[cfg(not(all(feature = "std", target_arch = "x86_64")))]
            inverse_transform_block(&mut dequant[..seg_eob], n, tx_type, self.lossless);
        }

        if !fused {
            let max_val = (1i64 << self.bit_depth) - 1;
            for i in 0..n0 {
                for j in 0..n0 {
                    let old = self.planes[plane].get(start_x + j, start_y + i) as i64;
                    let new_val = (old + dequant[i * n0 + j]).clamp(0, max_val);
                    self.planes[plane].set(start_x + j, start_y + i, new_val as u16);
                }
            }
        }
    }

    /// `read_coef( token )` (spec §6.4.26).
    fn read_coef(&self, r: &mut BoolDecoder, token: u8) -> i32 {
        let row = &EXTRA_BITS[token as usize];
        let cat = row[0] as usize;
        let num_extra = row[1] as u32;
        let mut coef = row[2] as i32;

        if token == DCT_VAL_CATEGORY6 {
            // The cat6 high-bit loop (`for e in 0..(BitDepth-8)`): 0 iterations at 8-bit,
            // 2/4 at 10/12-bit (profiles 2/3), reading the extra magnitude bits above the
            // 14 base cat6 extra bits. Sweep-verified at all three bit depths.
            for e in 0..(self.bit_depth.saturating_sub(8) as u32) {
                let high_bit = r.read_bool(255) as i32;
                coef += high_bit << (5 + self.bit_depth as u32 - e);
            }
        }

        for e in 0..num_extra {
            let coef_bit = r.read_bool(CAT_PROBS[cat][e as usize]) as i32;
            coef += coef_bit << (num_extra - 1 - e);
        }

        coef
    }
}
