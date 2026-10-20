//! Motion-compensation scratch and the weighted-prediction write-out.
//!
//! The interpolation kernels live in `rusty_h265-accel`; what stays here is
//! the geometry around them — reusable scratch so no prediction unit
//! allocates, an edge-extended footprint for blocks whose filter support
//! leaves the picture (§8.5.3.3.2), and explicit weighted prediction
//! (§8.5.3.3.4.3), which is rare enough not to want a kernel.

use crate::frame::Plane;
use crate::slice::WeightEntry;
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{vec, vec::Vec};

/// Largest luma prediction block, and the filter margin around it.
const MAX_PB: usize = 64;
const MAX_MARGIN: usize = 7;

/// Buffers reused by every prediction unit in a slice.
///
/// Before this existed, each prediction unit allocated three `Vec`s per plane
/// plus an interpolation temporary — in the function that is 61 % of decode.
pub(crate) struct McScratch {
    /// The 14-bit intermediate for each reference list.
    pub pred: [Vec<i16>; 2],
    /// The horizontal pass's output, consumed by the vertical pass.
    pub tmp: Vec<i16>,
    /// Edge-extended footprint, for blocks that reach outside the picture.
    pub pad: Vec<u16>,
}

impl McScratch {
    pub fn new() -> McScratch {
        McScratch {
            pred: [vec![0; MAX_PB * MAX_PB], vec![0; MAX_PB * MAX_PB]],
            tmp: vec![0; MAX_PB * (MAX_PB + MAX_MARGIN)],
            pad: vec![0; (MAX_PB + MAX_MARGIN) * (MAX_PB + MAX_MARGIN)],
        }
    }

    /// Copies the `fw × fh` footprint at (`x0`, `y0`) out of `plane` into
    /// `pad`, extending the edge sample where the footprint leaves the
    /// picture. Rows are split into a left run, an interior copy and a right
    /// run, so the clamp costs three decisions per row rather than one per
    /// sample.
    pub fn pad_footprint(&mut self, plane: &Plane, x0: i32, y0: i32, fw: usize, fh: usize) {
        crate::prof_scope!(crate::prof::Stage::Pad);
        // `pad` is built at the largest footprint HEVC can ask for -- a 64x64
        // prediction block plus the 7-tap margin, which is exactly
        // `(64 + 7) * (64 + 7)` -- so this can only ever be a no-op. It is a
        // `debug_assert` rather than a runtime resize so the release path does
        // not carry a compare and a call to `Vec::resize` it can never take.
        // `scratch_holds_the_largest_block` is the test that keeps it true.
        debug_assert!(self.pad.len() >= fw * fh, "pad footprint {fw}x{fh} exceeds the scratch");
        let pw = plane.width as i32;
        let ph = plane.height as i32;
        let left = (-x0).clamp(0, fw as i32) as usize;
        let right = ((x0 + fw as i32) - pw).clamp(0, fw as i32) as usize;
        let mid = fw - left - right;
        // Rows above the picture all clamp to row 0, and rows below all clamp
        // to the last one -- so a footprint hanging off an edge asked for the
        // SAME output row several times over, rebuilding it each time: a clamp,
        // a copy and up to two fills per repeat. Building it once and
        // replicating is strictly less work, and blocks at a picture edge are
        // 12% of motion compensation (`MC_EDGE_PAD` = 31,137 a clip).
        // The source column range is the same for every row, so it is resolved
        // once rather than per row.
        let sx_mid = (x0 + left as i32) as usize;
        let sx_all = x0.clamp(0, pw - 1) as usize;
        let last = plane.width - 1;
        let mut prev_sy = usize::MAX;
        for y in 0..fh {
            let sy = (y0 + y as i32).clamp(0, ph - 1) as usize;
            if sy == prev_sy {
                // `copy_within` on the flat buffer: the previous row is already
                // exactly what this one would be.
                let (a, b) = ((y - 1) * fw, y * fw);
                self.pad.copy_within(a..a + fw, b);
                continue;
            }
            prev_sy = sy;
            let rb = sy * plane.stride;
            let row = &plane.data[rb..rb + plane.width];
            let out = &mut self.pad[y * fw..y * fw + fw];
            if mid > 0 {
                out[left..left + mid].copy_from_slice(&row[sx_mid..sx_mid + mid]);
                if left > 0 {
                    out[..left].fill(row[0]);
                }
                if right > 0 {
                    out[left + mid..].fill(row[last]);
                }
            } else {
                // The whole footprint is off one side: one edge sample.
                out.fill(row[sx_all]);
            }
        }
    }
}

/// §8.5.3.3.4.3 explicit weighted prediction, uni or bi.
#[allow(clippy::too_many_arguments)]
pub(crate) fn weighted_write(
    dst: &mut [u16],
    dst_stride: usize,
    pred: &[Vec<i16>; 2],
    used: [bool; 2],
    e0: Option<&WeightEntry>,
    e1: Option<&WeightEntry>,
    denom: u32,
    c: usize,
    w: usize,
    h: usize,
    bit_depth: u8,
) {
    let max = (1i32 << bit_depth) - 1;
    let shift1 = 14i32 - bit_depth as i32;
    let log2wd = denom as i32 + shift1;
    let wt = |e: Option<&WeightEntry>| -> (i32, i32) {
        match e {
            Some(e) if c == 0 => (e.luma_weight, e.luma_offset),
            Some(e) => (e.chroma_weight[c - 1], e.chroma_offset[c - 1]),
            None => (1 << denom, 0),
        }
    };
    match (used[0], used[1]) {
        (true, true) => {
            let (w0, o0) = wt(e0);
            let (w1, o1) = wt(e1);
            crate::accel::pixel::weighted_bi(dst, dst_stride, &pred[0], &pred[1], w, h, w0, w1, (o0 + o1 + 1) << log2wd, log2wd, max);
        }
        (used0, _) => {
            let l = if used0 { 0 } else { 1 };
            let (wv, ov) = wt(if used0 { e0 } else { e1 });
            crate::accel::pixel::weighted_uni(dst, dst_stride, &pred[l], w, h, wv, ov, log2wd, max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane_with(w: usize, h: usize) -> Plane {
        let mut p = Plane::new(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, (y * w + x) as u16 % 255);
            }
        }
        p
    }

    /// The padded footprint must equal a per-sample clamped read — the
    /// definition it replaces.
    #[test]
    fn pad_footprint_matches_clamped_reads() {
        let p = plane_with(19, 11);
        let mut s = McScratch::new();
        for y0 in -9i32..14 {
            for x0 in -9i32..22 {
                for &(fw, fh) in &[(11usize, 11usize), (5, 7), (1, 1), (12, 3)] {
                    s.pad_footprint(&p, x0, y0, fw, fh);
                    for y in 0..fh {
                        for x in 0..fw {
                            let sx = (x0 + x as i32).clamp(0, p.width as i32 - 1) as usize;
                            let sy = (y0 + y as i32).clamp(0, p.height as i32 - 1) as usize;
                            assert_eq!(s.pad[y * fw + x], p.get(sx, sy), "({x0},{y0}) {fw}x{fh} at ({x},{y})");
                        }
                    }
                }
            }
        }
    }

    /// The scratch must be big enough for the largest block HEVC allows, or a
    /// 64x64 prediction unit would reallocate mid-picture.
    #[test]
    fn scratch_holds_the_largest_block() {
        let s = McScratch::new();
        assert!(s.pred[0].len() >= 64 * 64);
        assert!(s.tmp.len() >= 64 * (64 + 7));
        assert!(s.pad.len() >= (64 + 7) * (64 + 7));
    }
}
