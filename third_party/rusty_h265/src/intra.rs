//! Intra sample prediction (§8.4.4.2): reference sample substitution,
//! filtering (incl. strong smoothing), planar, DC and the 33 angular modes.
//!
//! The caller gathers the reference samples with their availability (it knows
//! the picture, the z-scan availability and constrained-intra rules); this
//! module owns everything from substitution onward.

use crate::accel;
use crate::tables::{INTRA_PRED_ANGLE, INV_ANGLE};

/// Bring-up switch: `RH265_SCALAR_INTRA=1` keeps prediction on the scalar
/// reference loops, so the kernels can be A/B'd inside one binary. Cached, so
/// the per-block cost is a relaxed load.
// VENDOR(uefi-player): std keeps the cached knob; no_std reads as unset
// (shipped default: kernels on).
#[cfg(feature = "std")]
fn use_kernels() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    !*F.get_or_init(|| std::env::var_os("RH265_SCALAR_INTRA").is_some())
}
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: on).
#[cfg(not(feature = "std"))]
fn use_kernels() -> bool {
    true
}

pub const MODE_PLANAR: u8 = 0;
pub const MODE_DC: u8 = 1;
pub const MODE_HOR: u8 = 10;
pub const MODE_VER: u8 = 26;

/// Reference samples around an N×N block: `left[y] = p[-1][y]` for
/// y = 0..2N, `top[x] = p[x][-1]` for x = 0..2N, `corner = p[-1][-1]`.
/// `*_avail` mark which samples exist (per sample).
pub struct RefSamples {
    pub left: [u16; 64],
    pub top: [u16; 64],
    pub corner: u16,
    pub left_avail: [bool; 64],
    pub top_avail: [bool; 64],
    pub corner_avail: bool,
    /// The projected reference for angular prediction, indices `−N..=2N`.
    ///
    /// A `[0i16; 128]` local here was 256 bytes of memset per angular block,
    /// and 33 of the 35 modes are angular. Nothing reads it before it is
    /// written — the vector kernels overread past the block, but only to
    /// discard those lanes — so carrying it in the reused struct costs one
    /// clear for the whole decode instead of one per block.
    pub refb: [i16; 128],
    /// Narrowed `left` / `top` for the planar kernel, same reasoning.
    pub pl: [i16; 33],
    pub pt: [i16; 33],
}

impl RefSamples {
    pub fn new() -> Self {
        RefSamples {
            left: [0; 64],
            top: [0; 64],
            corner: 0,
            left_avail: [false; 64],
            top_avail: [false; 64],
            corner_avail: false,
            refb: [0; 128],
            pl: [0; 33],
            pt: [0; 33],
        }
    }

    /// Clears just the availability flags for a block of size `n`.
    ///
    /// The sample arrays need no clearing: every entry in `..2n` is either
    /// written by the gather (available) or overwritten by [`substitute`]
    /// (unavailable), so a stale value can never be read.
    pub fn reset(&mut self, n: usize) {
        let n2 = 2 * n;
        self.left_avail[..n2].fill(false);
        self.top_avail[..n2].fill(false);
        self.corner_avail = false;
    }

    /// §8.4.4.2.2: substitute unavailable samples.
    ///
    /// `all` short-circuits the whole process: the caller derives availability
    /// per 4x4 minimum block anyway, so it already knows whether anything was
    /// missing, and in the interior of a picture nothing ever is. Without it
    /// this walks `4n` entries and runs two fill loops to change nothing.
    pub fn substitute(&mut self, n: usize, bit_depth: u8, all: bool) {
        if all {
            return;
        }
        let n2 = 2 * n;
        let any = self.corner_avail || self.left_avail[..n2].iter().any(|&a| a) || self.top_avail[..n2].iter().any(|&a| a);
        if !any {
            let v = 1u16 << (bit_depth - 1);
            self.left[..n2].fill(v);
            self.top[..n2].fill(v);
            self.corner = v;
            return;
        }
        // Search order: p[-1][2N-1] up to p[-1][-1], then p[0][-1] .. p[2N-1][-1].
        if !self.left_avail[n2 - 1] {
            let mut found = None;
            for y in (0..n2 - 1).rev() {
                if self.left_avail[y] {
                    found = Some(self.left[y]);
                    break;
                }
            }
            if found.is_none() && self.corner_avail {
                found = Some(self.corner);
            }
            if found.is_none() {
                for x in 0..n2 {
                    if self.top_avail[x] {
                        found = Some(self.top[x]);
                        break;
                    }
                }
            }
            self.left[n2 - 1] = found.unwrap_or(0);
            self.left_avail[n2 - 1] = true;
        }
        let mut prev = self.left[n2 - 1];
        for y in (0..n2 - 1).rev() {
            if !self.left_avail[y] {
                self.left[y] = prev;
            }
            prev = self.left[y];
        }
        if !self.corner_avail {
            self.corner = prev;
        }
        prev = self.corner;
        for x in 0..n2 {
            if !self.top_avail[x] {
                self.top[x] = prev;
            }
            prev = self.top[x];
        }
    }

    /// §8.4.4.2.3: filtering of neighbouring samples (luma only in 4:2:0).
    fn filter(&mut self, n: usize, mode: u8, bit_depth: u8, strong_intra_smoothing: bool) {
        if mode == MODE_DC || n == 4 {
            return;
        }
        let min_dist = (mode as i32 - 26).abs().min((mode as i32 - 10).abs());
        let thres = match n {
            8 => 7,
            16 => 1,
            _ => 0,
        };
        if min_dist <= thres {
            return;
        }
        let n2 = 2 * n;
        let corner = self.corner as i32;
        let bi_int = strong_intra_smoothing
            && n == 32
            && (corner + self.top[n2 - 1] as i32 - 2 * self.top[n - 1] as i32).abs() < (1 << (bit_depth - 5))
            && (corner + self.left[n2 - 1] as i32 - 2 * self.left[n - 1] as i32).abs() < (1 << (bit_depth - 5));
        if bi_int {
            let l63 = self.left[63] as i32;
            let t63 = self.top[63] as i32;
            for i in 0..63 {
                self.left[i] = (((63 - i as i32) * corner + (i as i32 + 1) * l63 + 32) >> 6) as u16;
                self.top[i] = (((63 - i as i32) * corner + (i as i32 + 1) * t63 + 32) >> 6) as u16;
            }
            return;
        }
        // In place, with a one-sample history -- no double buffer.
        //
        // §8.4.4.2.3 is `(p[i-1] + 2*p[i] + p[i+1] + 2) >> 2`, and the reason
        // this "cannot filter in place" is that `p[i-1]` has already been
        // overwritten by the time index `i` is computed. Carrying the ONE value
        // that is destroyed -- the previous input -- removes that objection: the
        // forward neighbour `p[i+1]` has not been written yet, and `p[i]` is
        // read before it is stored. Filtering the buffer directly retires two
        // `copy_from_slice` calls per filtered block plus the second write pass,
        // and two 128-byte arrays from this struct.
        let corner_u = corner as u16;
        let RefSamples { left, top, .. } = self;
        let new_corner = ((left[0] as i32 + 2 * corner + top[0] as i32 + 2) >> 2) as u16;
        // `p[-1]` is the corner for both runs.
        let (mut lp, mut tp) = (corner_u, corner_u);
        for i in 0..n2 - 1 {
            let (lc, tc) = (left[i], top[i]);
            left[i] = ((left[i + 1] as i32 + 2 * lc as i32 + lp as i32 + 2) >> 2) as u16;
            top[i] = ((top[i + 1] as i32 + 2 * tc as i32 + tp as i32 + 2) >> 2) as u16;
            lp = lc;
            tp = tc;
        }
        // The last sample is unfiltered -- it has no forward neighbour.
        self.corner = new_corner;
    }
}

impl Default for RefSamples {
    fn default() -> Self {
        Self::new()
    }
}

/// Predicts an N×N block into `out` (row stride `stride`). `refs` must have
/// been substituted; filtering is applied here for `c_idx == 0`.
///
/// Returns `Some(dc)` when it declined to write anything because the whole
/// block is that one value and the caller said a residual follows: the fill and
/// the residual add then collapse into `accel::pixel::add_residual_const`, one
/// pass instead of two. Only offered when the DC predictor really is flat — the
/// §8.4.4.2.5 edge fix-ups make luma blocks below 32×32 non-constant.
#[allow(clippy::too_many_arguments)]
pub fn predict(refs: &mut RefSamples, n: usize, mode: u8, c_idx: usize, bit_depth: u8, strong_intra_smoothing: bool, out: &mut [u16], stride: usize, residual_follows: bool) -> Option<u16> {
    if c_idx == 0 {
        // Route: §8.4.4.2.3 smoothing depends on mode, size and bit depth —
        // the filter runs on some blocks and not others.
        let before = refs.corner;
        refs.filter(n, mode, bit_depth, strong_intra_smoothing);
        // Compile-time gate: `route` reads a `OnceLock`, and this runs on
        // every luma intra block -- 3.0 M of them on intra-heavy content.
        if accel::census::ALWAYS {
            accel::census::route(refs.corner != before, &accel::census::RT_INTRA_REF_FILTERED, &accel::census::RT_INTRA_REF_PLAIN);
        }
    }
    let log2n = n.trailing_zeros() as usize;
    let max = (1i32 << bit_depth) - 1;
    let kern = use_kernels();
    match mode {
        MODE_PLANAR => {
            let tn = refs.top[n] as i32;
            let ln = refs.left[n] as i32;
            if kern {
                // The kernel wants `i16`; samples are at most 1023 in Main and
                // Main 10, so the narrowing is exact. n + 1 entries each.
                for i in 0..=n {
                    refs.pl[i] = refs.left[i] as i16;
                    refs.pt[i] = refs.top[i] as i16;
                }
                accel::intra::planar(out, stride, n, &refs.pl, &refs.pt, log2n as u32);
            } else {
                for y in 0..n {
                    for x in 0..n {
                        let v = ((n - 1 - x) as i32 * refs.left[y] as i32 + (x + 1) as i32 * tn + (n - 1 - y) as i32 * refs.top[x] as i32 + (y + 1) as i32 * ln + n as i32) >> (log2n + 1);
                        out[y * stride + x] = v as u16;
                    }
                }
            }
        }
        MODE_DC => {
            let mut sum = n as i32;
            for i in 0..n {
                sum += refs.top[i] as i32 + refs.left[i] as i32;
            }
            let dc = sum >> (log2n + 1);
            // Flat only without the edge fix-ups below.
            // Route: fuse the flat DC into the residual add, or fill it.
            let defer = residual_follows && (c_idx != 0 || n >= 32);
            if accel::census::ALWAYS {
                accel::census::route(defer, &accel::census::RT_INTRA_DC_DEFERRED, &accel::census::RT_INTRA_DC_FILLED);
            }
            if defer {
                return Some(dc as u16);
            }
            if kern {
                accel::intra::dc_fill(out, stride, n, dc as u16);
            } else {
                for y in 0..n {
                    for x in 0..n {
                        out[y * stride + x] = dc as u16;
                    }
                }
            }
            if c_idx == 0 && n < 32 {
                out[0] = ((refs.left[0] as i32 + 2 * dc + refs.top[0] as i32 + 2) >> 2) as u16;
                for x in 1..n {
                    out[x] = ((refs.top[x] as i32 + 3 * dc + 2) >> 2) as u16;
                }
                for y in 1..n {
                    out[y * stride] = ((refs.left[y] as i32 + 3 * dc + 2) >> 2) as u16;
                }
            }
        }
        _ => {
            // Disjoint field borrows: the projected reference is written while
            // the neighbour arrays are read, and both live in `refs`.
            let RefSamples { left, top, corner, refb, .. } = &mut *refs;
            let corner = *corner;
            // Route: modes 18+ walk the top row (contiguous stores); below
            // 18 they walk the left column, which is a scatter and needs the
            // transposing kernel.
            if accel::census::ALWAYS {
                accel::census::route(mode >= 18, &accel::census::RT_INTRA_ANG_ROW, &accel::census::RT_INTRA_ANG_TRANSPOSED);
            }
            let angle = INTRA_PRED_ANGLE[mode as usize];
            // ref[] indexed from -N..=2N via an offset of N. `i16` because the
            // kernel contracts two taps with one `pmaddwd`; the widest index
            // the kernel touches is `off + 2n + 8`, hence the slack. Carried in
            // `refs` rather than declared here — see the field's doc comment.
            let off = n as i32;
            let r = |i: i32| -> usize { (i + off) as usize };
            if mode >= 18 {
                // main reference = top row, side = left column
                // Built as slice copies rather than indexed stores: the
                // projected reference is `2n + 1` samples and an intra block
                // can be as small as 4x4, so at 4x4 this setup is nine stores
                // against sixteen predicted samples. Written as a `zip` over
                // subslices LLVM emits a vector copy; written through an index
                // closure it emitted one store per sample.
                refb[r(0)] = corner as i16;
                let base = r(1);
                for (d, &s) in refb[base..base + n].iter_mut().zip(&top[..n]) {
                    *d = s as i16;
                }
                if angle < 0 {
                    let last = (n as i32 * angle) >> 5;
                    if last < -1 {
                        let inv = INV_ANGLE[(mode - 11) as usize];
                        let mut x = -1;
                        while x >= last {
                            let idx = -1 + ((x * inv + 128) >> 8);
                            refb[r(x)] = if idx < 0 { corner as i16 } else { left[idx as usize] as i16 };
                            x -= 1;
                        }
                    }
                } else {
                    for (d, &s) in refb[base + n..base + 2 * n].iter_mut().zip(&top[n..2 * n]) {
                        *d = s as i16;
                    }
                }
                if kern {
                    // Rows are contiguous in the output, so the kernel writes
                    // straight through.
                    accel::intra::angular(out, stride, n, refb, n, angle, max);
                } else {
                    for y in 0..n {
                        let pos = (y as i32 + 1) * angle;
                        let iidx = pos >> 5;
                        let ifact = pos & 31;
                        for x in 0..n {
                            let a = refb[r(x as i32 + iidx + 1)] as i32;
                            let v = if ifact != 0 {
                                let b = refb[r(x as i32 + iidx + 2)] as i32;
                                ((32 - ifact) * a + ifact * b + 16) >> 5
                            } else {
                                a
                            };
                            out[y * stride + x] = v as u16;
                        }
                    }
                }
                if mode == MODE_VER && c_idx == 0 && n < 32 {
                    for y in 0..n {
                        let v = top[0] as i32 + ((left[y] as i32 - corner as i32) >> 1);
                        out[y * stride] = v.clamp(0, max) as u16;
                    }
                }
            } else {
                // main reference = left column, side = top row
                // Built as slice copies rather than indexed stores: the
                // projected reference is `2n + 1` samples and an intra block
                // can be as small as 4x4, so at 4x4 this setup is nine stores
                // against sixteen predicted samples. Written as a `zip` over
                // subslices LLVM emits a vector copy; written through an index
                // closure it emitted one store per sample.
                refb[r(0)] = corner as i16;
                let base = r(1);
                for (d, &s) in refb[base..base + n].iter_mut().zip(&left[..n]) {
                    *d = s as i16;
                }
                if angle < 0 {
                    let last = (n as i32 * angle) >> 5;
                    if last < -1 {
                        let inv = INV_ANGLE[(mode - 11) as usize];
                        let mut x = -1;
                        while x >= last {
                            let idx = -1 + ((x * inv + 128) >> 8);
                            refb[r(x)] = if idx < 0 { corner as i16 } else { top[idx as usize] as i16 };
                            x -= 1;
                        }
                    }
                } else {
                    for (d, &s) in refb[base + n..base + 2 * n].iter_mut().zip(&left[n..2 * n]) {
                        *d = s as i16;
                    }
                }
                if kern {
                    // Here the output walks `y` for a fixed `x`, so writing
                    // straight through would be a scatter — the reason LLVM
                    // refuses this loop. The kernel does the same arithmetic
                    // and transposes 8×8 tiles internally, so no scratch block
                    // is allocated here.
                    accel::intra::angular_t(out, stride, n, refb, n, angle, max);
                } else {
                    for x in 0..n {
                        let pos = (x as i32 + 1) * angle;
                        let iidx = pos >> 5;
                        let ifact = pos & 31;
                        for y in 0..n {
                            let a = refb[r(y as i32 + iidx + 1)] as i32;
                            let v = if ifact != 0 {
                                let b = refb[r(y as i32 + iidx + 2)] as i32;
                                ((32 - ifact) * a + ifact * b + 16) >> 5
                            } else {
                                a
                            };
                            out[y * stride + x] = v as u16;
                        }
                    }
                }
                if mode == MODE_HOR && c_idx == 0 && n < 32 {
                    for x in 0..n {
                        let v = left[0] as i32 + ((top[x] as i32 - corner as i32) >> 1);
                        out[x] = v.clamp(0, max) as u16;
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_fills_from_first_available() {
        let mut r = RefSamples::new();
        for x in 0..8 {
            r.top[x] = 100 + x as u16;
            r.top_avail[x] = true;
        }
        r.substitute(4, 8, false);
        assert!(r.left[..8].iter().all(|&v| v == 100));
        assert_eq!(r.corner, 100);
        let mut r = RefSamples::new();
        r.substitute(8, 10, false);
        assert!(r.left[..16].iter().all(|&v| v == 512));
    }

    #[test]
    fn dc_and_planar_flat_input() {
        let mut r = RefSamples::new();
        r.left = [50; 64];
        r.top = [50; 64];
        r.corner = 50;
        let mut out = [0u16; 64];
        predict(&mut r, 8, MODE_DC, 0, 8, false, &mut out, 8, false);
        assert!(out.iter().all(|&v| v == 50));
        predict(&mut r, 8, MODE_PLANAR, 0, 8, false, &mut out, 8, false);
        assert!(out.iter().all(|&v| v == 50));
        for m in 2..35 {
            predict(&mut r, 8, m, 1, 8, false, &mut out, 8, false);
            assert!(out.iter().all(|&v| v == 50), "mode {m}");
        }
    }

    #[test]
    fn vertical_copies_top_row_for_chroma() {
        let mut r = RefSamples::new();
        for x in 0..16 {
            r.top[x] = x as u16 * 3;
        }
        let mut out = [0u16; 16];
        predict(&mut r, 4, MODE_VER, 1, 8, false, &mut out, 4, false);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(out[y * 4 + x], x as u16 * 3);
            }
        }
    }
}
