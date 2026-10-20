//! Transform tree (§7.3.8.8), transform unit (§7.3.8.10), residual coding
//! (§7.3.8.11) and the reconstruction path: intra prediction glue, scaling
//! and inverse transform, residual add. A child of `ctu` so it shares the
//! `SliceDecoder` fields.

use super::{wrap_qp, PartMode, SliceDecoder};
use crate::cabac::*;
use crate::error::{Error, Result};
use crate::intra;
use crate::itx::{self, TransformKind};
use crate::pic::{PicState, PRED_INTRA};
use crate::tables::{scan_set, CHROMA_QP_420};
use rusty_h265_accel as accel;

impl<'a> SliceDecoder<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn transform_tree(&mut self, x0: usize, y0: usize, xbase: usize, ybase: usize, log2: usize, depth: u8, blk_idx: usize, parent_cbf_cb: bool, parent_cbf_cr: bool) -> Result<()> {
        let max_tb = self.sps.log2_max_tb_size as usize;
        let min_tb = self.sps.log2_min_tb_size as usize;
        let split = if log2 <= max_tb && log2 > min_tb && depth < self.max_trafo_depth && !(self.intra_split && depth == 0) {
            self.cab.decode(CTX_SPLIT_TRANSFORM + 5 - log2) == 1
        } else {
            let inter_split = self.sps.max_transform_hierarchy_depth_inter == 0 && !self.cu_intra && self.part_mode != PartMode::Part2Nx2N && depth == 0;
            log2 > max_tb || (self.intra_split && depth == 0) || inter_split
        };
        let (mut cbf_cb, mut cbf_cr) = (false, false);
        if log2 > 2 {
            if depth == 0 || parent_cbf_cb {
                cbf_cb = self.cab.decode(CTX_CBF_CHROMA + depth as usize) == 1;
            }
            if depth == 0 || parent_cbf_cr {
                cbf_cr = self.cab.decode(CTX_CBF_CHROMA + depth as usize) == 1;
            }
        } else {
            cbf_cb = parent_cbf_cb;
            cbf_cr = parent_cbf_cr;
        }
        if split {
            let half = 1usize << (log2 - 1);
            self.transform_tree(x0, y0, x0, y0, log2 - 1, depth + 1, 0, cbf_cb, cbf_cr)?;
            self.transform_tree(x0 + half, y0, x0, y0, log2 - 1, depth + 1, 1, cbf_cb, cbf_cr)?;
            self.transform_tree(x0, y0 + half, x0, y0, log2 - 1, depth + 1, 2, cbf_cb, cbf_cr)?;
            self.transform_tree(x0 + half, y0 + half, x0, y0, log2 - 1, depth + 1, 3, cbf_cb, cbf_cr)?;
            return Ok(());
        }
        let mut cbf_luma = true;
        if self.cu_intra || depth != 0 || cbf_cb || cbf_cr {
            cbf_luma = self.cab.decode(CTX_CBF_LUMA + if depth == 0 { 1 } else { 0 }) == 1;
        }
        self.transform_unit(x0, y0, xbase, ybase, log2, blk_idx, cbf_luma, cbf_cb, cbf_cr)
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_unit(&mut self, x0: usize, y0: usize, xbase: usize, ybase: usize, log2: usize, blk_idx: usize, cbf_luma: bool, cbf_cb: bool, cbf_cr: bool) -> Result<()> {
        let n = 1usize << log2;
        let cbf_chroma = cbf_cb || cbf_cr;
        if (cbf_luma || cbf_chroma) && self.pps.cu_qp_delta_enabled && !self.is_cu_qp_delta_coded {
            // cu_qp_delta_abs: prefix TU cMax 5 (ctx 0 then 1), suffix EG0.
            let mut v = 0u32;
            while v < 5 && self.cab.decode(CTX_CU_QP_DELTA + if v == 0 { 0 } else { 1 }) == 1 {
                v += 1;
            }
            if v == 5 {
                v += self.eg_k(0)?;
            }
            let mut delta = v as i32;
            if delta != 0 && self.cab.bypass() == 1 {
                delta = -delta;
            }
            self.is_cu_qp_delta_coded = true;
            self.cu_qp_delta_val = delta;
            let lo = -(26 + self.sps.qp_bd_offset_y / 2);
            let hi = 25 + self.sps.qp_bd_offset_y / 2;
            if delta < lo || delta > hi {
                return Err(Error::invalid("CuQpDeltaVal out of range"));
            }
            self.qp_y = wrap_qp(self.qp_y_pred + delta, self.sps.qp_bd_offset_y);
            self.set_cu_qp();
        }
        self.mark_edges(x0, y0, n, n, 0);
        // Luma
        let luma_mode = self.st.intra_mode[self.st.idx4(x0, y0)];
        let mut dc_luma = None;
        if self.cu_intra {
            dc_luma = self.intra_predict(0, x0, y0, n, luma_mode, cbf_luma);
        }
        if cbf_luma {
            self.residual_block(x0, y0, log2, 0, luma_mode, dc_luma)?;
            let w4 = self.st.w4;
            PicState::fill4(&mut self.st.nz, w4, x0, y0, n, n, 1);
        }
        // Chroma (4:2:0)
        let (xc, yc, log2c, do_chroma) = if log2 > 2 {
            (x0 / 2, y0 / 2, log2 - 1, true)
        } else if blk_idx == 3 {
            (xbase / 2, ybase / 2, 2, true)
        } else {
            (0, 0, 0, false)
        };
        if do_chroma {
            let nc = 1usize << log2c;
            let cmode = self.intra_chroma_mode;
            for c in 1..3usize {
                let cbf = if c == 1 { cbf_cb } else { cbf_cr };
                let mut dc_c = None;
                if self.cu_intra {
                    dc_c = self.intra_predict(c, xc, yc, nc, cmode, cbf);
                }
                if cbf {
                    self.residual_block(xc * 2, yc * 2, log2c, c, cmode, dc_c)?;
                }
            }
        }
        Ok(())
    }

    // ---- intra prediction glue (§8.4.4.2.1) ----

    /// Predicts the N×N block of component `c` at component coordinates (xb, yb).
    fn intra_predict(&mut self, c: usize, xb: usize, yb: usize, n: usize, mode: u8, residual_follows: bool) -> Option<u16> {
        crate::prof_scope!(crate::prof::Stage::Intra);
        if self.ablate.intra {
            return None;
        }
        let ss = if c > 0 { 1 } else { 0 };
        let xl = (xb << ss) as i32;
        let yl = (yb << ss) as i32;
        let constrained = self.pps.constrained_intra_pred;
        // Reused across blocks; only the availability flags are cleared.
        let refs = &mut self.iref;
        refs.reset(n);
        // The reference-sample gather asks about up to 4n + 1 neighbours of
        // one block, so the current-block half of §6.4.1 is hoisted hardest
        // here.
        let ac = self.st.avail_at(xl, yl);
        let avail = |st: &PicState, xn: i32, yn: i32| -> bool { st.avail_n(&ac, xn, yn) && (!constrained || st.pred_mode[st.idx4(xn as usize, yn as usize)] == PRED_INTRA) };
        // Every reference sample present? The gather derives this per run for
        // free, and it lets `substitute` return immediately.
        let mut all_avail = true;
        {
            let plane = &self.pic.planes[c];
            let st = &*self.st;
            // §6.4.1 availability is derived per minimum block (4x4 luma), so
            // it is constant along a run of `4 >> ss` reference samples. The
            // old shape asked per sample: for a 32x32 block that was 129
            // z-scan/slice/tile derivations instead of 33.
            // `run` is 4 or 2 — a power of two — so the wrap below is a mask,
            // not a `%`. Written as `%` it was a hardware divide per run.
            let run = 4 >> ss;
            let rmask = run - 1;
            // Gather a SPAN of consecutive available runs at a time, not a run.
            //
            // Availability is derived per run, but the copy does not have to be.
            // A run is four samples (two for chroma), and
            // `top[k..end].copy_from_slice(..)` on a slice that short is a
            // runtime-length `memcpy` CALL -- up to sixteen of them per
            // transform block per component, which `memcpy_census.py` ranked as
            // the densest remaining copy site in the decoder. Runs are almost
            // always all available (the picture interior), so accumulating them
            // and flushing at each transition turns those sixteen 8-byte calls
            // into ONE of up to 128 bytes, and leaves the edge cases as the only
            // place more than one flush happens.
            //
            // `reset` still pre-clears the availability flags, so a span that is
            // never flushed is already false. Removing that pre-clear and
            // writing the false runs instead was tried and measured 0.982x on
            // intra-heavy content: it replaced two long fills with thirty-two
            // short ones, which is this same defect in the other direction.
            let mut k = 0;
            let mut span: Option<usize> = None;
            while k < 2 * n {
                let end = (k + run - ((yb + k) & rmask)).min(2 * n);
                if avail(st, (xb as i32 - 1) << ss, ((yb + k) as i32) << ss) {
                    span.get_or_insert(k);
                } else {
                    all_avail = false;
                    if let Some(a) = span.take() {
                        // The left column is strided, so the samples are a loop
                        // either way; it is the flag write that coalesces.
                        for j in a..k {
                            refs.left[j] = plane.get(xb - 1, yb + j);
                        }
                        refs.left_avail[a..k].fill(true);
                    }
                }
                k = end;
            }
            if let Some(a) = span.take() {
                for j in a..2 * n {
                    refs.left[j] = plane.get(xb - 1, yb + j);
                }
                refs.left_avail[a..2 * n].fill(true);
            }
            // NOT hoisted out of the loop: at `yb == 0` there is no row above,
            // and `(yb - 1) * stride` underflows. No run is available there, so
            // the slice is never USED -- but it must not be FORMED either.
            let top_row = || &plane.data[(yb - 1) * plane.stride..];
            let mut k = 0;
            let mut span: Option<usize> = None;
            while k < 2 * n {
                let end = (k + run - ((xb + k) & rmask)).min(2 * n);
                if avail(st, ((xb + k) as i32) << ss, (yb as i32 - 1) << ss) {
                    span.get_or_insert(k);
                } else {
                    all_avail = false;
                    if let Some(a) = span.take() {
                        refs.top[a..k].copy_from_slice(&top_row()[xb + a..xb + k]);
                        refs.top_avail[a..k].fill(true);
                    }
                }
                k = end;
            }
            if let Some(a) = span.take() {
                refs.top[a..2 * n].copy_from_slice(&top_row()[xb + a..xb + 2 * n]);
                refs.top_avail[a..2 * n].fill(true);
            }
            if avail(st, (xb as i32 - 1) << ss, (yb as i32 - 1) << ss) {
                refs.corner = plane.get(xb - 1, yb - 1);
                refs.corner_avail = true;
            } else {
                all_avail = false;
            }
        }
        let bit_depth = if c == 0 { self.sps.bit_depth_luma } else { self.sps.bit_depth_chroma };
        // Route: every reference present (the picture interior), or some
        // missing and §8.4.4.2.2 substitution needed (edges, slice and tile
        // boundaries, constrained intra).
        if accel::census::ALWAYS {
            accel::census::route(all_avail, &accel::census::RT_INTRA_ALL_AVAIL, &accel::census::RT_INTRA_SUBSTITUTED);
        }
        refs.substitute(n, bit_depth, all_avail);
        let plane = &mut self.pic.planes[c];
        let stride = plane.stride;
        let off = yb * stride + xb;
        intra::predict(refs, n, mode, c, bit_depth, self.sps.strong_intra_smoothing_enabled, &mut plane.data[off..], stride, residual_follows)
    }

    // ---- residual coding (§7.3.8.11) + reconstruction ----

    /// Parses one transform block's coefficients (luma-domain coordinates
    /// `x0, y0`; for chroma these are the chroma block's coordinates × 2),
    /// scales, inverse-transforms and adds to the picture.
    fn residual_block(&mut self, x0: usize, y0: usize, log2: usize, c_idx: usize, pred_mode_intra: u8, dc: Option<u16>) -> Result<()> {
        let n = 1usize << log2;
        let nn = n * n;
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::RES_BLOCKS, 1);
        }
        // The clear of `self.coeffs` used to be here, before anything was
        // known about the block. It is now below, once the last-significant
        // position says how much of the block is live.
        let cab = &mut self.cab;
        let mut transform_skip = false;
        if self.pps.transform_skip_enabled && !self.cu_transquant_bypass && log2 <= self.pps.log2_max_transform_skip_block_size as usize {
            transform_skip = cab.decode(CTX_TRANSFORM_SKIP + if c_idx > 0 { 1 } else { 0 }) == 1;
        }
        // last_sig_coeff_{x,y}_prefix / suffix
        let (ctx_off, ctx_shift) = if c_idx == 0 { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
        let cmax = (log2 << 1) - 1;
        // Only `px >> ctx_shift` varies across the run; the base is fixed.
        let (bx, by) = (CTX_LAST_X_PREFIX + ctx_off, CTX_LAST_Y_PREFIX + ctx_off);
        let mut px = 0usize;
        while px < cmax && cab.decode(bx + (px >> ctx_shift)) == 1 {
            px += 1;
        }
        let mut py = 0usize;
        while py < cmax && cab.decode(by + (py >> ctx_shift)) == 1 {
            py += 1;
        }
        let mut last_x = px;
        if px > 3 {
            let nb = (px >> 1) - 1;
            let suffix = cab.bypass_bits(nb as u32) as usize;
            last_x = ((2 + (px & 1)) << nb) + suffix;
        }
        let mut last_y = py;
        if py > 3 {
            let nb = (py >> 1) - 1;
            let suffix = cab.bypass_bits(nb as u32) as usize;
            last_y = ((2 + (py & 1)) << nb) + suffix;
        }
        if last_x >= n || last_y >= n {
            return Err(Error::invalid("last significant coefficient outside the block"));
        }
        // scanIdx (§7.4.9.11)
        let scan_idx = if self.cu_intra && (log2 == 2 || (log2 == 3 && c_idx == 0)) {
            if (6..=14).contains(&pred_mode_intra) {
                2
            } else if (22..=30).contains(&pred_mode_intra) {
                1
            } else {
                0
            }
        } else {
            0
        };
        if scan_idx == 2 {
            core::mem::swap(&mut last_x, &mut last_y);
        }
        let log2sb = log2 - 2;
        let nsb = 1usize << log2sb;
        // One selection for all five tables (see `ScanSet`).
        let sc = scan_set(log2sb, scan_idx);
        let sb_scan = sc.sb;
        let pos_scan = sc.pos;
        // Two table lookups where there were two linear searches of the
        // forward scan. `last_x` and `last_y` were range-checked against `n`
        // just above, so both indices are inside their table and the
        // "not found" arms the searches carried were unreachable.
        let last_sb = sc.sb_inv[(last_y >> 2) * nsb + (last_x >> 2)] as usize;
        let last_pos = sc.pos_inv[(last_y & 3) * 4 + (last_x & 3)] as usize;
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::RES_SCAN_SEARCH, 2);
        }
        // Clear only what can be written, and read back.
        //
        // The parse can only write into sub-blocks at or before `last_sb` in
        // the scan, so `scan_bbox` bounds the written region exactly; and for a
        // real transform the consumers (`dequant`, then stage 1 of the inverse
        // transform) read only inside `nz_w x nz_h`, which sits inside that
        // same box. Transform-skip and transquant-bypass are the two kinds that
        // pass the block through untransformed and therefore read every sample,
        // so those still clear all of it.
        //
        // On a 720p stream that is 11.4 M i32 stores down to the ~3.7 M the
        // last-significant position says are live; on intra-heavy content,
        // 178.8 M down to ~22.6 M.
        let (fw, fh) = if transform_skip || self.cu_transquant_bypass {
            (n, n)
        } else {
            let (bw, bh) = sc.sb_bbox[last_sb];
            ((bw as usize) << 2, (bh as usize) << 2)
        };
        if fw == n {
            self.coeffs[..fh * n].fill(0);
        } else {
            for y in 0..fh {
                self.coeffs[y * n..y * n + fw].fill(0);
            }
        }
        // Reborrow once for the whole parse. Indexing `self.coeffs` inside the
        // coefficient loop reloaded the slice's POINTER AND LENGTH from the
        // struct on every coefficient -- two loads each, plus a bounds check
        // against a length the loop cannot change. Bound here, they are
        // loop-invariant. (`cab` above borrows a different field, so the two
        // reborrows are disjoint.)
        let co = &mut self.coeffs[..nn];
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::RES_FILL_STORES, (fw * fh) as u64);
        }

        // One `u64` in place of a 64-byte array of `bool`: the sub-block flags
        // are a bitmap, and clearing a register beats clearing 64 bytes on
        // every one of 3.0 M transform blocks. Bit `xs * 8 + ys`.
        let mut csbf = 0u64;
        // The non-zero rectangle actually written, which bounds the work the
        // inverse transform has to do (see `itx`'s module docs).
        let (mut nz_w, mut nz_h) = (0usize, 0usize);
        let mut c1: usize = 1;
        let sig_base = CTX_SIG + if c_idx == 0 { 0 } else { 27 };
        let gt1_base = CTX_GT1 + if c_idx == 0 { 0 } else { 16 };
        let gt2_base = CTX_GT2 + if c_idx == 0 { 0 } else { 4 };
        let sign_hiding = self.pps.sign_data_hiding_enabled && !self.cu_transquant_bypass;

        for i in (0..=last_sb).rev() {
            let (xs, ys) = (sb_scan[i].0 as usize, sb_scan[i].1 as usize);
            let right = xs + 1 < nsb && (csbf >> ((xs + 1) * 8 + ys)) & 1 != 0;
            let below = ys + 1 < nsb && (csbf >> (xs * 8 + ys + 1)) & 1 != 0;
            let mut infer_sb_dc = false;
            let coded = if i < last_sb && i > 0 {
                let ctx = CTX_CSBF + (right || below) as usize + if c_idx == 0 { 0 } else { 2 };
                infer_sb_dc = true;
                cab.decode(ctx) == 1
            } else {
                true
            };
            csbf |= (coded as u64) << (xs * 8 + ys);
            let prev_csbf = right as usize | ((below as usize) << 1);
            // Everything the significance context needs from this sub-block,
            // hoisted out of the per-coefficient loop below. `sig_row[n]` is
            // the position-dependent term keyed by SCAN POSITION, so the loop
            // never has to recover (xP, yP) or (xC, yC); `sig_off` is the
            // sub-block/size/component term, which the old shape recomputed for
            // every one of 2.3 M scanned positions (18.8 M on intra content).
            let (sig_row, sig_off) = if log2 == 2 {
                (sc.sig_4x4, 0usize)
            } else {
                let mut o = if xs > 0 || ys > 0 { 3 } else { 0 };
                if c_idx == 0 {
                    o += if log2 == 3 {
                        if scan_idx == 0 {
                            9
                        } else {
                            15
                        }
                    } else {
                        21
                    };
                } else {
                    // The +3 above is luma-only.
                    o = if log2 == 3 { 9 } else { 12 };
                }
                (&sc.sig_nb[prev_csbf & 3], o)
            };
            // The DC of the DC sub-block is context 0 (§9.3.4.2.5). For a 4x4
            // block the map already reads 0 there, so one test serves both.
            let dc_sb = xs == 0 && ys == 0;
            // significant_coeff_flag
            let mut sig_pos = [0u8; 16];
            let mut nsig = 0usize;
            let start_n: i32 = if i == last_sb {
                sig_pos[0] = last_pos as u8;
                nsig = 1;
                last_pos as i32 - 1
            } else {
                15
            };
            if coded {
                let mut nn_ = start_n;
                while nn_ >= 0 {
                    if accel::census::ALWAYS {
                        accel::census::bump(&accel::census::RES_SIG_SCANNED, 1);
                    }
                    let np = nn_ as usize;
                    let s = if np > 0 || !infer_sb_dc {
                        if accel::census::ALWAYS {
                            accel::census::bump(&accel::census::RES_SIG_CTX, 1);
                        }
                        let sig_ctx = if dc_sb && np == 0 { 0 } else { sig_row[np & 15] as usize + sig_off };
                        let b = cab.decode(sig_base + sig_ctx) == 1;
                        if b {
                            infer_sb_dc = false;
                        }
                        b
                    } else {
                        // n == 0 with inferSbDcSigCoeffFlag: inferred 1
                        true
                    };
                    if s {
                        sig_pos[nsig & 15] = np as u8;
                        nsig += 1;
                    }
                    nn_ -= 1;
                }
            }
            if nsig == 0 {
                continue;
            }
            // coeff_abs_level_greater1_flag (§9.3.4.2.6)
            let ctx_set = (if i == 0 || c_idx > 0 { 0 } else { 2 }) + if c1 == 0 { 1 } else { 0 };
            c1 = 1;
            // A 16-bit set instead of a 16-byte array, cleared and written per
            // sub-block.
            let mut g1 = 0u16;
            // 16 is outside a sub-block's 0..=15 scan positions, so it serves
            // as "none" and the per-coefficient test below is one compare
            // rather than an `Option` discriminant plus a payload compare.
            let mut first_g2: usize = 16;
            let num_c1 = nsig.min(8);
            // The context SET is fixed for the whole run; only `c1` moves.
            let g1_ctx = gt1_base + ctx_set * 4;
            for &np in sig_pos.iter().take(num_c1) {
                let b = cab.decode(g1_ctx + c1) == 1;
                g1 |= (b as u16) << np;
                if b {
                    c1 = 0;
                    if first_g2 == 16 {
                        first_g2 = np as usize;
                    }
                } else if (1..3).contains(&c1) {
                    c1 += 1;
                }
            }
            let mut g2 = false;
            if first_g2 != 16 {
                g2 = cab.decode(gt2_base + ctx_set) == 1;
            }
            let first_sig = sig_pos[(nsig - 1) & 15] as usize;
            let last_sig = sig_pos[0] as usize;
            let sign_hidden = sign_hiding && last_sig - first_sig > 3;
            let nsigns = if sign_hidden { nsig - 1 } else { nsig };
            // Pre-align the sign bits so each coefficient reads the top one.
            //
            // The old form recovered bit `nsigns - 1 - k` per coefficient: two
            // subtracts, a variable shift and a mask, plus a `k < nsigns`
            // guard. Left-aligned, the sign is the sign bit of an `i32` and the
            // guard disappears -- past `nsigns` the shifts have brought in
            // zeros, which is exactly "not negative".
            //
            // `wrapping_shl` masks its operand to 0..=31, so `nsigns == 0`
            // shifts by 0; `signs` is then 0 anyway, so the result still reads
            // as all-positive.
            let mut sbits = cab.bypass_bits(nsigns as u32).wrapping_shl(32 - nsigns as u32);
            // coeff_abs_level_remaining
            let mut rice = 0u32;
            let mut sum_abs = 0i32;
            for k in 0..nsig {
                let np = sig_pos[k & 15] as usize;
                let is_g2_pos = first_g2 == np;
                let base = 1 + ((g1 >> np) & 1) as i32 + (is_g2_pos && g2) as i32;
                let threshold = if k < 8 {
                    if is_g2_pos {
                        3
                    } else {
                        2
                    }
                } else {
                    1
                };
                let mut abs = base;
                if base == threshold {
                    let rem = Self::coeff_remaining(cab, rice)?;
                    abs += rem;
                    if abs > 3 * (1 << rice) {
                        rice = (rice + 1).min(4);
                    }
                }
                let neg = (sbits as i32) < 0;
                sbits <<= 1;
                let mut v = if neg { -abs } else { abs };
                if sign_hidden {
                    sum_abs += abs;
                    if k == nsig - 1 && sum_abs & 1 == 1 {
                        v = -v;
                    }
                }
                let (xp, yp) = (pos_scan[np & 15].0 as usize, pos_scan[np & 15].1 as usize);
                let xc = (xs << 2) + xp;
                let yc = (ys << 2) + yp;
                nz_w = nz_w.max(xc + 1);
                nz_h = nz_h.max(yc + 1);
                co[yc * n + xc] = v.clamp(-32768, 32767);
            }
        }
        self.reconstruct_residual(x0, y0, log2, c_idx, transform_skip, nz_w, nz_h, dc)
    }

    /// `coeff_abs_level_remaining` (§9.3.3.11), HM's equivalent form.
    fn coeff_remaining(cab: &mut Cabac, rice: u32) -> Result<i32> {
        let prefix = cab.bypass_ones(32);
        if prefix >= 32 {
            return Err(Error::invalid("coeff_abs_level_remaining prefix"));
        }
        if prefix < 3 {
            Ok(((prefix << rice) + cab.bypass_bits(rice)) as i32)
        } else {
            let l = prefix - 3;
            if l + rice > 31 {
                return Err(Error::invalid("coeff_abs_level_remaining suffix"));
            }
            Ok(((((1u32 << l) + 2) << rice) + cab.bypass_bits(l + rice)) as i32)
        }
    }

    /// §8.6.2–8.6.4 on `self.coeffs`, then adds the residual to the picture.
    #[allow(clippy::too_many_arguments)]
    fn reconstruct_residual(&mut self, x0: usize, y0: usize, log2: usize, c_idx: usize, transform_skip: bool, nz_w: usize, nz_h: usize, dc: Option<u16>) -> Result<()> {
        crate::prof_scope!(crate::prof::Stage::Transform);
        if self.ablate.residual {
            return Ok(());
        }
        let n = 1usize << log2;
        let nn = n * n;
        let sps = self.sps;
        let bit_depth = if c_idx == 0 { sps.bit_depth_luma } else { sps.bit_depth_chroma };
        let kind = if self.cu_transquant_bypass {
            TransformKind::Bypass
        } else if transform_skip {
            TransformKind::Skip
        } else if self.cu_intra && c_idx == 0 && n == 4 {
            TransformKind::Dst
        } else {
            TransformKind::Dct
        };
        if kind != TransformKind::Bypass {
            let qp = if c_idx == 0 {
                self.qp_y + sps.qp_bd_offset_y
            } else {
                let off = if c_idx == 1 {
                    self.pps.cb_qp_offset + self.sh.cb_qp_offset
                } else {
                    self.pps.cr_qp_offset + self.sh.cr_qp_offset
                };
                let qpi = (self.qp_y + off).clamp(-sps.qp_bd_offset_c, 57);
                let qpc = if qpi < 0 { qpi } else { CHROMA_QP_420[qpi as usize] as i32 };
                qpc + sps.qp_bd_offset_c
            };
            let m: Option<&[u8]> = match self.scaling {
                Some(sf) if !(transform_skip && n > 4) => {
                    let size_id = log2 - 2;
                    let matrix_id = if self.cu_intra { 0 } else { 3 } + c_idx;
                    Some(sf.get(size_id, matrix_id))
                }
                _ => None,
            };
            // Route: a signalled scaling list, or the flat default.
            //
            // `ALWAYS`, not `route`: `route` calls `enabled()`, whose `OnceLock`
            // read is an atomic load plus a branch, and this site runs on every
            // transform block -- 3.0 M of them on intra-heavy content. Per-block
            // and finer sites are compile-time; per-picture ones keep the
            // runtime switch.
            if accel::census::ALWAYS {
                accel::census::route(m.is_some(), &accel::census::RT_TX_SCALED, &accel::census::RT_TX_FLAT);
            }
            itx::dequant(&mut self.coeffs[..nn], n, nz_w.clamp(1, n), nz_h.clamp(1, n), qp, bit_depth, m);
        }
        if accel::census::ALWAYS {
            use accel::census as cx;
            // Route: which inverse transform the block needs.
            cx::arm(match kind {
                TransformKind::Bypass => &cx::RT_TX_BYPASS,
                TransformKind::Skip => &cx::RT_TX_SKIP,
                TransformKind::Dst => &cx::RT_TX_DST,
                TransformKind::Dct => &cx::RT_TX_DCT,
            });
            // Route: block size — each is a different kernel shape.
            cx::arm(match n {
                4 => &cx::RT_TX_N4,
                8 => &cx::RT_TX_N8,
                16 => &cx::RT_TX_N16,
                _ => &cx::RT_TX_N32,
            });
            // Route: sparsity. Not a branch but a continuous population — the
            // ratio is how much of each block the last-significant position
            // actually spared us.
            cx::bump(&cx::RT_TX_NZ_AREA, (nz_w.clamp(1, n) * nz_h.clamp(1, n)) as u64);
            cx::bump(&cx::RT_TX_FULL_AREA, (n * n) as u64);
        }
        itx::inverse_transform(&mut self.coeffs[..nn], &mut self.itx_tmp, n, nz_w, nz_h, bit_depth, kind);
        let ss = if c_idx > 0 { 1 } else { 0 };
        let max = (1i32 << bit_depth) - 1;
        let (xb, yb) = (x0 >> ss, y0 >> ss);
        // Disjoint field borrows: the coefficients are read, the picture written.
        let coeffs = &self.coeffs[..nn];
        let plane = &mut self.pic.planes[c_idx];
        let stride = plane.stride;
        match dc {
            // The prediction was a single value the fill never wrote; adding
            // the residual to it directly is the whole reconstruction.
            Some(v) => accel::pixel::add_residual_const(&mut plane.data[yb * stride + xb..], stride, v, coeffs, n, n, max),
            None => accel::pixel::add_residual(&mut plane.data[yb * stride + xb..], stride, coeffs, n, n, max),
        }
        Ok(())
    }
}
