//! Inter prediction: prediction-unit syntax (§7.3.8.6, §7.3.8.9), merge mode
//! (§8.5.3.2.2–8.5.3.2.5), AMVP (§8.5.3.2.6–8.5.3.2.7), temporal motion
//! vector prediction (§8.5.3.2.8–8.5.3.2.9), motion storage and motion
//! compensation (§8.5.3.3). A child of `ctu` so it shares `SliceDecoder`.

use super::{PartMode, SliceDecoder};
use crate::cabac::*;
use crate::error::{Error, Result};
use crate::mcscratch::weighted_write;
use crate::pic::{AvailAt, Motion, PicState, PRED_INTER, PRED_SKIP};
use crate::slice::PredWeightTable;
use rusty_h265_accel as accel;

/// A fixed-capacity candidate list.
///
/// The merge list holds at most `MaxNumMergeCand` (five) entries and the
/// motion-vector-predictor list at most three, both bounded by the spec, and
/// neither outlives the function that builds it. A `Vec` for either is a heap
/// allocation and a free **per prediction unit** for data that fits in a
/// register file.
#[derive(Clone, Copy)]
struct Cands<T: Copy + Default, const N: usize> {
    buf: [T; N],
    len: usize,
}

impl<T: Copy + Default, const N: usize> Cands<T, N> {
    fn new() -> Self {
        Cands { buf: [T::default(); N], len: 0 }
    }

    fn push(&mut self, v: T) {
        debug_assert!(self.len < N, "candidate list overflowed its spec bound");
        if self.len < N {
            self.buf[self.len] = v;
            self.len += 1;
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, i: usize) -> Option<&T> {
        self.buf[..self.len].get(i)
    }
}

impl<T: Copy + Default, const N: usize> core::ops::Index<usize> for Cands<T, N> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.buf[..self.len][i]
    }
}

/// Motion of one prediction unit under derivation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PuMv {
    pub mv: [[i32; 2]; 2],
    pub ref_idx: [i32; 2],
    /// bit0 = L0, bit1 = L1
    pub flags: u8,
}

impl PuMv {
    fn from_motion(m: &Motion) -> Self {
        PuMv {
            mv: [[m.mv[0][0] as i32, m.mv[0][1] as i32], [m.mv[1][0] as i32, m.mv[1][1] as i32]],
            ref_idx: [m.ref_idx[0] as i32, m.ref_idx[1] as i32],
            flags: m.pred_flags,
        }
    }
    fn same(&self, o: &PuMv) -> bool {
        if self.flags != o.flags {
            return false;
        }
        for l in 0..2 {
            if self.flags & (1 << l) != 0 && (self.mv[l] != o.mv[l] || self.ref_idx[l] != o.ref_idx[l]) {
                return false;
            }
        }
        true
    }
}

/// `l0CandIdx`/`l1CandIdx` of §8.5.3.2.3. Twelve entries is exact, not a
/// bound: the loop runs to `numOrig * (numOrig - 1)` and entry requires
/// `numOrig < MaxNumMergeCand <= 5`, so `numOrig <= 4` and the index stops at
/// 11. Pairing them into one table of tuples was measured: +22 instructions
/// for no guard change, because the two lookups share one bound and LLVM had
/// already merged the check.
const L0_CAND: [usize; 12] = [0, 1, 0, 2, 1, 2, 0, 3, 1, 3, 2, 3];
const L1_CAND: [usize; 12] = [1, 0, 2, 0, 2, 1, 3, 0, 3, 1, 3, 2];

/// `tx` for every `td` the spec can produce.
///
/// §8.5.3.2.8 clamps `td` to −128..=127, so the division `(16384 + |td|/2) / td`
/// has only 256 possible inputs and belongs in a table. Left as a division it
/// was a hardware `idiv` per scaled motion vector — the same barrier a libm
/// call is, for the same reason: no SIMD form and tens of cycles of latency.
/// Built at compile time from the spec's expression, so the table cannot drift
/// from it.
const fn build_tx() -> [i32; 256] {
    let mut t = [0i32; 256];
    let mut i = 0;
    while i < 256 {
        let td = i as i32 - 128;
        t[i] = if td == 0 { 0 } else { (16384 + (if td < 0 { -td } else { td } >> 1)) / td };
        i += 1;
    }
    t
}

static TX_BY_TD: [i32; 256] = build_tx();

/// §8.5.3.2.7 / 8.5.3.2.8 motion vector scaling by POC distances.
fn scale_mv(mv: [i32; 2], td: i32, tb: i32) -> [i32; 2] {
    let td = td.clamp(-128, 127);
    let tb = tb.clamp(-128, 127);
    if td == 0 {
        return mv;
    }
    let tx = TX_BY_TD[(td + 128) as usize];
    debug_assert_eq!(tx, (16384 + (td.abs() >> 1)) / td);
    let dsf = ((tb * tx + 32) >> 6).clamp(-4096, 4095);
    let s = |v: i32| -> i32 {
        let p = dsf * v;
        let r = (p.abs() + 127) >> 8;
        (if p < 0 { -r } else { r }).clamp(-32768, 32767)
    };
    [s(mv[0]), s(mv[1])]
}

/// Is this component's explicit weighting the identity?
///
/// A `pred_weight_table` in the slice header routes EVERY prediction in that
/// slice through §8.5.3.3.4.3, and x265 emits one for P slices by default — so
/// on ordinary content 59 % of prediction writes took the weighted path
/// (110,604 of 187,466 on the 720p bench stream) while carrying the neutral
/// weights `w = 1 << denom, o = 0`.
///
/// With those values the weighted form collapses to the default one exactly:
///
/// ```text
///   uni:  ((x·2^d + 2^(d+s−1)) >> (d+s)) + 0  ==  (x + 2^(s−1)) >> s
///   bi:   (2^d(x+z) + 2^(d+s)) >> (d+s+1)     ==  ((x+z) + 2^s) >> (s+1)
/// ```
///
/// which are `put_uni` and `put_bi` term for term. Detecting it routes the
/// block to the cheaper kernel — and re-opens the full-pel fast path, which is
/// gated on "no weighting" and was therefore closed for every P slice.
/// Bring-up switch: `RH265_NO_NEUTRAL_WP=1` forces every weighted slice down
/// the weighted path, so the gate can be A/B'd inside one binary.
///
/// Read once per prediction unit by the caller, not once per COMPONENT inside
/// `neutral_weights` -- a `OnceLock` read is an atomic load and a branch, and
/// on a weighted slice this ran three times for every unit.
// VENDOR(uefi-player): std keeps the cached knob; no_std reads as unset (off).
#[cfg(feature = "std")]
fn neutral_wp_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RH265_NO_NEUTRAL_WP").is_some())
}
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: off).
#[cfg(not(feature = "std"))]
fn neutral_wp_off() -> bool {
    false
}

fn neutral_weights(t: &PredWeightTable, pu: &PuMv, used: [bool; 2], c: usize, off: bool) -> bool {
    if off {
        return false;
    }
    let denom = if c == 0 { t.luma_log2_weight_denom } else { t.chroma_log2_weight_denom };
    let unit = 1i32 << denom;
    for l in 0..2usize {
        if !used[l] {
            continue;
        }
        let idx = pu.ref_idx[l] as usize;
        let e = if l == 0 { t.l0.get(idx) } else { t.l1.get(idx) };
        let Some(e) = e else { return false };
        let (w, o) = if c == 0 {
            (e.luma_weight, e.luma_offset)
        } else {
            (e.chroma_weight[c - 1], e.chroma_offset[c - 1])
        };
        if w != unit || o != 0 {
            return false;
        }
    }
    true
}

impl<'a> SliceDecoder<'a> {
    /// Parses and predicts every PU of an inter CU. Returns `merge_flag` of a 2N×2N PU.
    pub(super) fn inter_prediction_units(&mut self, x0: usize, y0: usize, n: usize, depth: u8) -> Result<bool> {
        let h = n / 2;
        let q = n / 4;
        // A partition mode yields at most FOUR rectangles, so this is a fixed
        // array and a count — it used to be a `Vec`, which is a heap allocation
        // and a free per inter coding unit purely to iterate one to four
        // tuples.
        let mut parts = [(0usize, 0usize, 0usize, 0usize); 4];
        let np = match self.part_mode {
            PartMode::Part2Nx2N => {
                parts[0] = (x0, y0, n, n);
                1
            }
            PartMode::Part2NxN => {
                parts[..2].copy_from_slice(&[(x0, y0, n, h), (x0, y0 + h, n, h)]);
                2
            }
            PartMode::PartNx2N => {
                parts[..2].copy_from_slice(&[(x0, y0, h, n), (x0 + h, y0, h, n)]);
                2
            }
            PartMode::Part2NxnU => {
                parts[..2].copy_from_slice(&[(x0, y0, n, q), (x0, y0 + q, n, n - q)]);
                2
            }
            PartMode::Part2NxnD => {
                parts[..2].copy_from_slice(&[(x0, y0, n, n - q), (x0, y0 + n - q, n, q)]);
                2
            }
            PartMode::PartnLx2N => {
                parts[..2].copy_from_slice(&[(x0, y0, q, n), (x0 + q, y0, n - q, n)]);
                2
            }
            PartMode::PartnRx2N => {
                parts[..2].copy_from_slice(&[(x0, y0, n - q, n), (x0 + n - q, y0, q, n)]);
                2
            }
            PartMode::PartNxN => {
                parts.copy_from_slice(&[(x0, y0, h, h), (x0 + h, y0, h, h), (x0, y0 + h, h, h), (x0 + h, y0 + h, h, h)]);
                4
            }
        };
        let mut merge = false;
        for (i, &(x, y, w, hh)) in parts[..np].iter().enumerate() {
            if i > 0 {
                self.mark_edges(x, y, w, hh, 1);
            }
            merge = self.prediction_unit(x0, y0, n, x, y, w, hh, i, depth, false)?;
        }
        Ok(merge && self.part_mode == PartMode::Part2Nx2N)
    }

    /// §7.3.8.6 prediction_unit() + §8.5.3 decoding: returns `merge_flag`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prediction_unit(&mut self, xcb: usize, ycb: usize, ncb: usize, xp: usize, yp: usize, w: usize, h: usize, part_idx: usize, depth: u8, skip: bool) -> Result<bool> {
        crate::prof_scope!(crate::prof::Stage::Inter);
        let merge_idx = |s: &mut Self| -> usize {
            if s.sh.max_num_merge_cand <= 1 {
                return 0;
            }
            if s.cab.decode(CTX_MERGE_IDX) == 0 {
                return 0;
            }
            // The `merge_idx` suffix is truncated unary over the remaining
            // candidates (the first bin, above, is the context-coded one).
            1 + s.cab.bypass_ones((s.sh.max_num_merge_cand - 2) as u32) as usize
        };
        let merge = skip || self.cab.decode(CTX_MERGE_FLAG) == 1;
        let pu = if merge {
            let idx = merge_idx(self);
            self.merge_motion(xcb, ycb, ncb, xp, yp, w, h, part_idx, idx)?
        } else {
            // inter_pred_idc: 0 = L0, 1 = L1, 2 = BI
            let pred_idc = if self.sh.slice_type.is_b() {
                if w + h != 12 {
                    if self.cab.decode(CTX_INTER_PRED_IDC + depth as usize) == 1 {
                        2
                    } else {
                        self.cab.decode(CTX_INTER_PRED_IDC + 4)
                    }
                } else {
                    self.cab.decode(CTX_INTER_PRED_IDC + 4)
                }
            } else {
                0
            };
            let ref_idx = |s: &mut Self, num: u8| -> i32 {
                if num <= 1 {
                    return 0;
                }
                let cmax = num as i32 - 1;
                let mut i = 0;
                while i < cmax {
                    let bin = if i < 2 { s.cab.decode(CTX_REF_IDX + i as usize) } else { s.cab.bypass() };
                    if bin == 0 {
                        break;
                    }
                    i += 1;
                }
                i
            };
            let mut pu = PuMv::default();
            let mut mvd = [[0i32; 2]; 2];
            let mut mvp_flag = [0u32; 2];
            if pred_idc != 1 {
                pu.ref_idx[0] = ref_idx(self, self.sh.num_ref_idx_l0_active);
                mvd[0] = self.mvd_coding()?;
                mvp_flag[0] = self.cab.decode(CTX_MVP_FLAG);
                pu.flags |= 1;
            }
            if pred_idc != 0 {
                pu.ref_idx[1] = ref_idx(self, self.sh.num_ref_idx_l1_active);
                if !(self.sh.mvd_l1_zero && pred_idc == 2) {
                    mvd[1] = self.mvd_coding()?;
                }
                mvp_flag[1] = self.cab.decode(CTX_MVP_FLAG);
                pu.flags |= 2;
            }
            for l in 0..2 {
                if pu.flags & (1 << l) == 0 {
                    pu.ref_idx[l] = -1;
                    continue;
                }
                let nrefs = if l == 0 { self.refs.l0.len() } else { self.refs.l1.len() };
                if pu.ref_idx[l] as usize >= nrefs {
                    return Err(Error::invalid("ref_idx beyond the reference list"));
                }
                let mvp = self.amvp(xcb, ycb, ncb, xp, yp, w, h, part_idx, l, pu.ref_idx[l], mvp_flag[l]);
                for c in 0..2 {
                    let u = (mvp[c] + mvd[l][c] + 65536) & 0xffff;
                    pu.mv[l][c] = if u >= 32768 { u - 65536 } else { u };
                }
            }
            pu
        };
        self.store_motion(xp, yp, w, h, &pu);
        self.motion_compensate(xp, yp, w, h, &pu)?;
        Ok(merge)
    }

    /// §7.3.8.9 mvd_coding().
    fn mvd_coding(&mut self) -> Result<[i32; 2]> {
        let gt0 = [self.cab.decode(CTX_MVD_GT0) == 1, self.cab.decode(CTX_MVD_GT0) == 1];
        let mut gt1 = [false; 2];
        for c in 0..2 {
            if gt0[c] {
                gt1[c] = self.cab.decode(CTX_MVD_GT1) == 1;
            }
        }
        let mut mvd = [0i32; 2];
        for c in 0..2 {
            if gt0[c] {
                let mut abs = 1;
                if gt1[c] {
                    abs = 2 + self.eg_k(1)? as i32;
                }
                if self.cab.bypass() == 1 {
                    abs = -abs;
                }
                mvd[c] = abs;
            }
        }
        Ok(mvd)
    }

    /// §6.4.2 prediction block availability + "not intra".
    #[allow(clippy::too_many_arguments)]
    /// §6.4.2 prediction-block availability, returning the neighbour's 4x4
    /// index when it is available.
    ///
    /// The index is what every caller wants next: `merge_motion` reads the
    /// neighbour's motion at it, and `amvp` reads it TWICE (once in the
    /// unscaled pass, once in the scaled one). Returning it retires a multiply
    /// by the runtime stride per read -- five candidates per prediction block
    /// in `merge_motion`, five positions read up to twice each in `amvp`.
    #[allow(clippy::too_many_arguments)]
    fn pb_avail(&self, ac: &AvailAt, xcb: usize, ycb: usize, ncb: usize, w: usize, h: usize, part_idx: usize, xn: i32, yn: i32) -> Option<usize> {
        if xn < 0 || yn < 0 || xn >= self.st.width as i32 || yn >= self.st.height as i32 {
            return None;
        }
        let (xnu, ynu) = (xn as usize, yn as usize);
        let same_cb = xnu >= xcb && xnu < xcb + ncb && ynu >= ycb && ynu < ycb + ncb;
        // The prediction block's own §6.4.1 half is hoisted by the caller: this
        // is asked about five candidate neighbours of one block.
        let avail = if !same_cb {
            self.st.avail_n(ac, xn, yn)
        } else {
            !(w * 2 == ncb && h * 2 == ncb && part_idx == 1 && ycb + h <= ynu && xcb + w > xnu)
        };
        if !avail {
            return None;
        }
        let i = self.st.idx4(xnu, ynu);
        let m = self.st.pred_mode[i];
        (m == PRED_INTER || m == PRED_SKIP).then_some(i)
    }

    /// The neighbour's motion at an index [`pb_avail`] already computed.
    #[inline]
    fn motion_at_idx(&self, i: usize) -> PuMv {
        PuMv::from_motion(&self.st.motion[i])
    }

    /// §8.5.3.2.2–8.5.3.2.5: the merge candidate at `merge_idx`.
    #[allow(clippy::too_many_arguments)]
    fn merge_motion(&mut self, xcb: usize, ycb: usize, ncb: usize, xp0: usize, yp0: usize, w0: usize, h0: usize, part_idx0: usize, merge_idx: usize) -> Result<PuMv> {
        let plevel = self.pps.log2_parallel_merge_level as usize;
        let (xp, yp, w, h, part_idx) = if plevel > 2 && ncb == 8 { (xcb, ycb, ncb, ncb, 0) } else { (xp0, yp0, w0, h0, part_idx0) };
        let pm = self.part_mode;
        let same_mer = |xn: i32, yn: i32| -> bool { (xp >> plevel) as i32 == xn >> plevel && (yp >> plevel) as i32 == yn >> plevel };
        let (xi, yi, wi, hi) = (xp as i32, yp as i32, w as i32, h as i32);
        let ac = self.st.avail_at(xi, yi);
        // Five spatial-or-temporal candidates at most; six for headroom.
        let mut cands: Cands<PuMv, 6> = Cands::new();
        // A1
        let (xa1, ya1) = (xi - 1, yi + hi - 1);
        let vert_second = part_idx == 1 && matches!(pm, PartMode::PartNx2N | PartMode::PartnLx2N | PartMode::PartnRx2N);
        let a1 = if !same_mer(xa1, ya1) && !vert_second {
            self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, xa1, ya1).map(|i| self.motion_at_idx(i))
        } else {
            None
        };
        if let Some(m) = a1 {
            cands.push(m);
        }
        // B1
        let (xb1, yb1) = (xi + wi - 1, yi - 1);
        let horz_second = part_idx == 1 && matches!(pm, PartMode::Part2NxN | PartMode::Part2NxnU | PartMode::Part2NxnD);
        let b1 = if !same_mer(xb1, yb1) && !horz_second {
            self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, xb1, yb1).map(|i| self.motion_at_idx(i))
        } else {
            None
        };
        if let Some(m) = b1 {
            if !a1.is_some_and(|a| a.same(&m)) {
                cands.push(m);
            }
        }
        // B0
        let (xb0, yb0) = (xi + wi, yi - 1);
        let b0_i = if same_mer(xb0, yb0) { None } else { self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, xb0, yb0) };
        if let Some(i) = b0_i {
            let m = self.motion_at_idx(i);
            if !b1.is_some_and(|b| b.same(&m)) {
                cands.push(m);
            }
        }
        // A0
        let (xa0, ya0) = (xi - 1, yi + hi);
        let a0_i = if same_mer(xa0, ya0) { None } else { self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, xa0, ya0) };
        if let Some(i) = a0_i {
            let m = self.motion_at_idx(i);
            if !a1.is_some_and(|a| a.same(&m)) {
                cands.push(m);
            }
        }
        // B2 — only when fewer than four spatial candidates survived pruning
        // (availableFlagX are the post-pruning flags in §8.5.3.2.3).
        if cands.len() != 4 {
            let (xb2, yb2) = (xi - 1, yi - 1);
            let b2_i = if same_mer(xb2, yb2) { None } else { self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, xb2, yb2) };
            if let Some(i) = b2_i {
                let m = self.motion_at_idx(i);
                if !a1.is_some_and(|a| a.same(&m)) && !b1.is_some_and(|b| b.same(&m)) {
                    cands.push(m);
                }
            }
        }
        // temporal
        if cands.len() < self.sh.max_num_merge_cand as usize && self.sh.temporal_mvp_enabled {
            let mut t = PuMv::default();
            if let Some(mv) = self.temporal_mv(xp, yp, w, h, 0, 0) {
                t.mv[0] = mv;
                t.ref_idx[0] = 0;
                t.flags |= 1;
            }
            if self.sh.slice_type.is_b() {
                if let Some(mv) = self.temporal_mv(xp, yp, w, h, 1, 0) {
                    t.mv[1] = mv;
                    t.ref_idx[1] = 0;
                    t.flags |= 2;
                }
            }
            if t.flags != 0 {
                if t.flags & 1 == 0 {
                    t.ref_idx[0] = -1;
                }
                if t.flags & 2 == 0 {
                    t.ref_idx[1] = -1;
                }
                cands.push(t);
            }
        }
        let max = self.sh.max_num_merge_cand as usize;
        // combined bi-predictive candidates
        let num_orig = cands.len();
        if self.sh.slice_type.is_b() && num_orig > 1 && num_orig < max {
            let mut comb_idx = 0;
            while comb_idx < num_orig * (num_orig - 1) && cands.len() < max {
                let l0c = cands[L0_CAND[comb_idx]];
                let l1c = cands[L1_CAND[comb_idx]];
                if l0c.flags & 1 != 0 && l1c.flags & 2 != 0 {
                    let p0 = self.refs.l0[l0c.ref_idx[0] as usize].poc;
                    let p1 = self.refs.l1[l1c.ref_idx[1] as usize].poc;
                    if p0 != p1 || l0c.mv[0] != l1c.mv[1] {
                        cands.push(PuMv {
                            mv: [l0c.mv[0], l1c.mv[1]],
                            ref_idx: [l0c.ref_idx[0], l1c.ref_idx[1]],
                            flags: 3,
                        });
                    }
                }
                comb_idx += 1;
            }
        }
        // zero candidates
        let num_ref = if self.sh.slice_type.is_b() {
            self.refs.l0.len().min(self.refs.l1.len())
        } else {
            self.refs.l0.len()
        };
        let mut zero_idx = 0;
        while cands.len() < max {
            let r = if zero_idx < num_ref { zero_idx as i32 } else { 0 };
            if self.sh.slice_type.is_b() {
                cands.push(PuMv {
                    mv: [[0, 0]; 2],
                    ref_idx: [r, r],
                    flags: 3,
                });
            } else {
                cands.push(PuMv {
                    mv: [[0, 0]; 2],
                    ref_idx: [r, -1],
                    flags: 1,
                });
            }
            zero_idx += 1;
        }
        let mut m = *cands.get(merge_idx).ok_or_else(|| Error::invalid("merge_idx beyond the candidate list"))?;
        if m.flags == 3 && w0 + h0 == 12 {
            m.flags = 1;
            m.ref_idx[1] = -1;
        }
        Ok(m)
    }

    /// §8.5.3.2.8 temporal luma motion vector prediction for list `lx` and
    /// target reference index `ref_idx`.
    fn temporal_mv(&self, xp: usize, yp: usize, w: usize, h: usize, lx: usize, ref_idx: i32) -> Option<[i32; 2]> {
        let col = self.col.as_ref()?;
        let log2_ctb = self.st.log2_ctb;
        // bottom right
        let xbr = xp + w;
        let ybr = yp + h;
        let mut r = None;
        if (yp >> log2_ctb) == (ybr >> log2_ctb) && ybr < self.st.height && xbr < self.st.width {
            r = self.col_mv(col, (xbr >> 4) << 4, (ybr >> 4) << 4, lx, ref_idx);
        }
        if r.is_none() {
            let xc = xp + (w >> 1);
            let yc = yp + (h >> 1);
            r = self.col_mv(col, (xc >> 4) << 4, (yc >> 4) << 4, lx, ref_idx);
        }
        r
    }

    /// §8.5.3.2.9 collocated motion vectors.
    fn col_mv(&self, col: &crate::decoder::RefPic, x: usize, y: usize, lx: usize, ref_idx: i32) -> Option<[i32; 2]> {
        let pic = &col.pic;
        if pic.motion16_w == 0 {
            return None;
        }
        let m = pic.motion16.get((y >> 4) * pic.motion16_w + (x >> 4))?;
        if m.pred_flags == 0 {
            return None;
        }
        let (mv_col, ref_poc_col, lt_col) = if m.pred_flags & 1 == 0 {
            (m.mv[1], m.ref_poc[1], m.ref_lt & 2 != 0)
        } else if m.pred_flags & 2 == 0 {
            (m.mv[0], m.ref_poc[0], m.ref_lt & 1 != 0)
        } else {
            let n = if self.no_backward_pred {
                lx
            } else if self.sh.collocated_from_l0 {
                1
            } else {
                0
            };
            (m.mv[n], m.ref_poc[n], m.ref_lt & (1 << n) != 0)
        };
        let target = if lx == 0 { &self.refs.l0[ref_idx as usize] } else { &self.refs.l1[ref_idx as usize] };
        if target.is_long_term != lt_col {
            return None;
        }
        let mv = [mv_col[0] as i32, mv_col[1] as i32];
        let col_poc_diff = col.poc - ref_poc_col;
        let cur_poc_diff = self.poc - target.poc;
        if target.is_long_term || col_poc_diff == cur_poc_diff || col_poc_diff == 0 {
            Some(mv)
        } else {
            Some(scale_mv(mv, col_poc_diff, cur_poc_diff))
        }
    }

    /// §8.5.3.2.6 / 8.5.3.2.7: the motion vector predictor for list `lx`.
    #[allow(clippy::too_many_arguments)]
    fn amvp(&self, xcb: usize, ycb: usize, ncb: usize, xp: usize, yp: usize, w: usize, h: usize, part_idx: usize, lx: usize, ref_idx: i32, mvp_flag: u32) -> [i32; 2] {
        let ly = 1 - lx;
        let list = |l: usize| if l == 0 { &self.refs.l0 } else { &self.refs.l1 };
        let target = &list(lx)[ref_idx as usize];
        let (xi, yi, wi, hi) = (xp as i32, yp as i32, w as i32, h as i32);
        let a_pos = [(xi - 1, yi + hi), (xi - 1, yi + hi - 1)];
        let b_pos = [(xi + wi, yi - 1), (xi + wi - 1, yi - 1), (xi - 1, yi - 1)];
        let ac = self.st.avail_at(xi, yi);
        // Fetch each candidate's motion once. The two passes below (unscaled,
        // then scaled) each re-derived it from coordinates, so an available
        // position paid for `idx4` and a `PuMv::from_motion` twice.
        let avail = |p: (i32, i32)| self.pb_avail(&ac, xcb, ycb, ncb, w, h, part_idx, p.0, p.1).map(|i| self.motion_at_idx(i));
        // same-picture match without scaling
        let direct = |m: &PuMv| -> Option<[i32; 2]> {
            if m.flags & (1 << lx) != 0 && list(lx)[m.ref_idx[lx] as usize].poc == target.poc {
                return Some(m.mv[lx]);
            }
            if m.flags & (1 << ly) != 0 && list(ly)[m.ref_idx[ly] as usize].poc == target.poc {
                return Some(m.mv[ly]);
            }
            None
        };
        // long-term-equality match with scaling
        let scaled = |m: &PuMv| -> Option<[i32; 2]> {
            for l in [lx, ly] {
                if m.flags & (1 << l) != 0 {
                    let r = &list(l)[m.ref_idx[l] as usize];
                    if r.is_long_term == target.is_long_term {
                        let mv = m.mv[l];
                        if r.is_long_term || r.poc == target.poc {
                            return Some(mv);
                        }
                        return Some(scale_mv(mv, self.poc - r.poc, self.poc - target.poc));
                    }
                }
            }
            None
        };
        let cand_a = [avail(a_pos[0]), avail(a_pos[1])];
        let is_scaled = cand_a[0].is_some() || cand_a[1].is_some();
        let mut mv_a = None;
        for m in cand_a.iter().flatten() {
            if mv_a.is_none() {
                mv_a = direct(m);
            }
        }
        for m in cand_a.iter().flatten() {
            if mv_a.is_none() {
                mv_a = scaled(m);
            }
        }
        let cand_b = [avail(b_pos[0]), avail(b_pos[1]), avail(b_pos[2])];
        let mut mv_b = None;
        for m in cand_b.iter().flatten() {
            if mv_b.is_none() {
                mv_b = direct(m);
            }
        }
        if !is_scaled && mv_b.is_some() {
            mv_a = mv_b;
        }
        if !is_scaled {
            mv_b = None;
            for m in cand_b.iter().flatten() {
                if mv_b.is_none() {
                    mv_b = scaled(m);
                }
            }
        }
        let mut list_mvp: Cands<[i32; 2], 3> = Cands::new();
        if let Some(a) = mv_a {
            list_mvp.push(a);
        }
        if let Some(b) = mv_b {
            if mv_a != Some(b) {
                list_mvp.push(b);
            }
        }
        if list_mvp.len() < 2 && self.sh.temporal_mvp_enabled {
            if let Some(t) = self.temporal_mv(xp, yp, w, h, lx, ref_idx) {
                list_mvp.push(t);
            }
        }
        while list_mvp.len() < 2 {
            list_mvp.push([0, 0]);
        }
        list_mvp[mvp_flag as usize]
    }

    /// Records the PU's motion per 4×4 (for neighbours, deblocking and TMVP).
    fn store_motion(&mut self, xp: usize, yp: usize, w: usize, h: usize, pu: &PuMv) {
        let mut m = Motion {
            pred_flags: pu.flags,
            ..Default::default()
        };
        for l in 0..2 {
            if pu.flags & (1 << l) != 0 {
                m.mv[l] = [pu.mv[l][0] as i16, pu.mv[l][1] as i16];
                m.ref_idx[l] = pu.ref_idx[l] as i8;
                let r = if l == 0 { &self.refs.l0[pu.ref_idx[l] as usize] } else { &self.refs.l1[pu.ref_idx[l] as usize] };
                m.ref_poc[l] = r.poc;
                if r.is_long_term {
                    m.ref_lt |= 1 << l;
                }
            } else {
                m.ref_idx[l] = -1;
            }
        }
        let w4 = self.st.w4;
        PicState::fill4(&mut self.st.motion, w4, xp, yp, w, h, m);
    }

    /// §8.5.3.3: motion-compensated prediction of the PU into the picture.
    ///
    /// The kernels in `rusty_h265-accel` want an in-bounds filter footprint and
    /// nothing else: no per-sample coordinate clamp, no allocation, and a
    /// 16-bit intermediate. This function supplies exactly that — it slices the
    /// reference plane directly when the footprint is inside the picture (the
    /// overwhelming majority of blocks), and copies an edge-extended footprint
    /// into scratch when it is not (§8.5.3.3.2's coordinate clipping).
    fn motion_compensate(&mut self, xp: usize, yp: usize, w: usize, h: usize, pu: &PuMv) -> Result<()> {
        crate::prof_scope!(crate::prof::Stage::Mc);
        if self.ablate.mc {
            return Ok(());
        }
        // Disjoint field borrows: the reference lists and slice header are
        // shared, the scratch and the picture being written are exclusive.
        let refs = self.refs;
        let sh = self.sh;
        let sps = self.sps;
        let scratch = &mut self.scratch;
        let pic = &mut self.pic;

        let weights = sh.pred_weight.as_ref();
        // Each list's reference picture, resolved once for the whole unit. It
        // was looked up three times per COMPONENT -- in the full-pel scan, in
        // the interpolation loop, and again through the `refp` closure at the
        // write -- each a bounds-checked index into the reference list.
        let wp_off = weights.is_some() && neutral_wp_off();
        let rp = [
            (pu.flags & 1 != 0).then(|| &refs.l0[pu.ref_idx[0] as usize]),
            (pu.flags & 2 != 0).then(|| &refs.l1[pu.ref_idx[1] as usize]),
        ];
        for c in 0..3usize {
            let ss = if c == 0 { 0 } else { 1 };
            let (bw, bh) = (w >> ss, h >> ss);
            if bw == 0 || bh == 0 {
                continue;
            }
            let (xb, yb) = (xp >> ss, yp >> ss);
            let bit_depth = if c == 0 { sps.bit_depth_luma } else { sps.bit_depth_chroma };
            let taps = if c == 0 { 8usize } else { 4 };
            let m = taps / 2 - 1;
            let (fw, fh) = (bw + taps - 1, bh + taps - 1);

            // The fractional split is a property of the COMPONENT, not of the
            // list: it was recomputed inside both per-list loops below.
            let (frac_bits, frac_mask) = if c == 0 { (2u32, 3i32) } else { (3u32, 7i32) };

            // Geometry first, for every list, so the full-pel fast path below
            // can decide before any interpolation happens.
            let mut used = [false; 2];
            let mut geo = [(0i32, 0i32, 0usize, 0usize); 2];
            for l in 0..2usize {
                if pu.flags & (1 << l) == 0 {
                    continue;
                }
                used[l] = true;
                let mv = pu.mv[l];
                geo[l] = (
                    xb as i32 + (mv[0] >> frac_bits),
                    yb as i32 + (mv[1] >> frac_bits),
                    (mv[0] & frac_mask) as usize,
                    (mv[1] & frac_mask) as usize,
                );
            }

            // Full-pel: decide PER LIST, not for the pair.
            //
            // An integer motion vector needs no filter, and what the general
            // path then does with it collapses in every combination:
            //
            //   uni            `copy_shift` then `put_uni` is the identity
            //   bi, both       the pair is a rounding average (`pavgw`)
            //   bi, one        the `<< k` folds into the bi write
            //
            // so the list is recorded here and never interpolated. See
            // `accel::pixel::copy_block` / `put_bi_fp` for the algebra, and
            // `full_pel_composition_is_identity` /
            // `put_bi_fp_matches_the_composition` for the proofs. The test is
            // on the BLOCK, not the filter footprint — no halo is read.
            // Route: a signalled weight table whose values are the identity
            // is not weighting. Collapsing it here re-opens both the full-pel
            // fast path below and the default uni/bi writes.
            let weights = match weights {
                Some(t) if neutral_weights(t, pu, used, c, wp_off) => None,
                other => other,
            };
            let mut fp = [None; 2];
            if weights.is_none() && bit_depth <= 12 {
                for l in 0..2usize {
                    let Some(r) = rp[l] else { continue };
                    let (xi, yi, fx, fy) = geo[l];
                    let p = &r.pic.planes[c];
                    if fx == 0 && fy == 0 && xi >= 0 && yi >= 0 && xi as usize + bw <= p.width && yi as usize + bh <= p.height {
                        fp[l] = Some((yi as usize * p.stride + xi as usize, p.stride));
                    }
                }
            }

            for l in 0..2usize {
                if fp[l].is_some() {
                    continue; // resolved above; no filter, no scratch
                }
                let Some(r) = rp[l] else { continue };
                let plane = &r.pic.planes[c];
                // `geo[l]` already holds this: the old shape recomputed the
                // whole integer/fractional split -- two shifts, two masks and
                // two adds -- and threw the first copy away.
                let (xi, yi, fx, fy) = geo[l];
                let (x0, y0) = (xi - m as i32, yi - m as i32);

                let interior = x0 >= 0 && y0 >= 0 && (x0 as usize + fw) <= plane.width && (y0 as usize + fh) <= plane.height;
                let (src, stride): (&[u16], usize) = if interior {
                    (&plane.data[y0 as usize * plane.stride + x0 as usize..], plane.stride)
                } else {
                    if accel::census::ALWAYS {
                        accel::census::bump(&accel::census::MC_EDGE_PAD, 1);
                    }
                    scratch.pad_footprint(plane, x0, y0, fw, fh);
                    (&scratch.pad[..], fw)
                };
                let pred = &mut scratch.pred[l];
                if c == 0 {
                    accel::mc::interp_luma(src, stride, fx, fy, bw, bh, bit_depth, pred, &mut scratch.tmp);
                } else {
                    accel::mc::interp_chroma(src, stride, fx, fy, bw, bh, bit_depth, pred, &mut scratch.tmp);
                }
            }

            // NOT a profiler scope. Timing the combine step directly needs a
            // scope on ~2 M calls a clip, and at 545 ns of measured per-scope
            // cost that is 1,067 ms of tax against a 436 ms reading -- the
            // instrument would be 2.4x the quantity. Priced by census and
            // arithmetic instead (codec-measurement §6).
            let plane = &mut pic.planes[c];
            let stride = plane.stride;
            let off = yb * stride + xb;
            let dst = &mut plane.data[off..];
            let refp = |l: usize| -> &[u16] { &rp[l].unwrap().pic.planes[c].data };
            match (used[0], used[1], weights) {
                (true, false, None) | (false, true, None) => {
                    let l = if used[0] { 0 } else { 1 };
                    match fp[l] {
                        Some((o, ss2)) => accel::pixel::copy_block(dst, stride, &refp(l)[o..], ss2, bw, bh),
                        None => accel::pixel::put_uni(dst, stride, &scratch.pred[l], bw, bh, bit_depth),
                    }
                }
                (true, true, None) => match (fp[0], fp[1]) {
                    (Some((o0, s0)), Some((o1, s1))) => accel::pixel::avg_block(dst, stride, &refp(0)[o0..], s0, &refp(1)[o1..], s1, bw, bh),
                    (Some((o0, s0)), None) => accel::pixel::put_bi_fp(dst, stride, &refp(0)[o0..], s0, &scratch.pred[1], bw, bh, bit_depth),
                    (None, Some((o1, s1))) => accel::pixel::put_bi_fp(dst, stride, &refp(1)[o1..], s1, &scratch.pred[0], bw, bh, bit_depth),
                    (None, None) => accel::pixel::put_bi(dst, stride, &scratch.pred[0], &scratch.pred[1], bw, bh, bit_depth),
                },
                // Explicit weighted prediction (§8.5.3.3.4.3) — rare enough
                // that it stays scalar; the census counter records how rare.
                (_, _, Some(t)) => {
                    let denom = if c == 0 { t.luma_log2_weight_denom } else { t.chroma_log2_weight_denom };
                    let e0 = used[0].then(|| &t.l0[pu.ref_idx[0] as usize]);
                    let e1 = used[1].then(|| &t.l1[pu.ref_idx[1] as usize]);
                    weighted_write(dst, stride, &scratch.pred, used, e0, e1, denom, c, bw, bh, bit_depth);
                }
                (false, false, None) => {}
            }
        }
        Ok(())
    }
}
