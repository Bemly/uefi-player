//! H-14 R3: `MeCtx` — the per-search motion-estimation evaluation context.
//!
//! The reconciliation (WHYS H-14 R2) measured ~23 ns/eval of GLUE around a
//! 16-18 ns SATD kernel: per-candidate slice re-derivation, bounds asserts,
//! phase match ladder, wrapper hops. This context collects that chain: the
//! plane geometry is validated ONCE at construction, the kernel function
//! pointers are chosen ONCE (size × ISA), and `eval` does only integer bounds,
//! a phase pick, one offset multiply, and the raw kernel call.
//!
//! The `unsafe` stays quarantined in this crate; the encoder remains
//! `#![forbid(unsafe_code)]`. Every value returned is EXACTLY what the safe
//! path computes (`2·WelsSampleSatd` for full/half phases, `Σ|H·d|` fused-avg
//! for quarter phases), so an eval served here instead of there cannot change
//! the bitstream — pinned by `mectx_matches_safe_path`.

use super::satd_avg::{satd_avg_w16, satd_avg_w8};

type WelsSatd = unsafe extern "C" fn(*const u8, i32, *const u8, i32) -> i32;
type AvgSatd = unsafe fn(*const u8, usize, *const u8, *const u8, usize, usize) -> u32;

/// Plane indices into [`MeCtx::planes`].
const PF: usize = 0;
const PH: usize = 1;
const PV: usize = 2;
const PC: usize = 3;

pub struct MeCtx<'a> {
    src: &'a [u8],
    cw: usize,
    planes: [&'a [u8]; 4], // f, h, v, c — each pw×ph, stride == pw
    stride: usize,
    pad: isize,
    /// Valid top-left range for a candidate's padded plane position, precomputed
    /// with the same `+1` slack the safe `hpel_ref`/`hpel_qpel_refs` guards use
    /// (the quarter phases read one sample past the block on either axis).
    px_max: isize,
    py_max: isize,
    lx: isize,
    ly: isize,
    h: usize,
    satd: WelsSatd,
    avg: AvgSatd,
}

impl<'a> MeCtx<'a> {
    /// Validates the whole search geometry once. Returns `None` when AVX2 is
    /// unavailable, the shape is uncovered, or any slice is short — the caller
    /// then uses the safe per-eval path for the entire search.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        src: &'a [u8],
        cw: usize,
        f: &'a [u8],
        h_pl: &'a [u8],
        v: &'a [u8],
        c: &'a [u8],
        stride: usize,
        pad: usize,
        pw: usize,
        ph: usize,
        lx: usize,
        ly: usize,
        w: usize,
        h: usize,
    ) -> Option<Self> {
        if !super::has_avx2() || stride != pw {
            return None;
        }
        let (satd, avg): (WelsSatd, AvgSatd) = match (w, h) {
            // Portable AVX2 kernels (rip-ASM Phase 5a) behind the same extern "C"
            // signature the openh264 symbols had, so this table is unchanged in shape.
            (16, 16) => (crate::satd_sad::cshim::satd16x16 as WelsSatd, satd_avg_w16 as AvgSatd),
            (16, 8) => (crate::satd_sad::cshim::satd16x8 as WelsSatd, satd_avg_w16 as AvgSatd),
            (8, 16) => (crate::satd_sad::cshim::satd8x16 as WelsSatd, satd_avg_w8 as AvgSatd),
            (8, 8) => (crate::satd_sad::cshim::satd8x8 as WelsSatd, satd_avg_w8 as AvgSatd),
            _ => return None,
        };
        if src.len() < (h - 1) * cw + w {
            return None;
        }
        let need = pw.checked_mul(ph)?;
        if f.len() < need || h_pl.len() < need || v.len() < need || c.len() < need {
            return None;
        }
        // px/py are padded-plane coordinates of the candidate's top-left sample;
        // the +1 covers the quarter phases' shifted operand (offset 1 or stride).
        let px_max = pw as isize - w as isize - 1;
        let py_max = ph as isize - h as isize - 1;
        if px_max < 0 || py_max < 0 {
            return None;
        }
        Some(MeCtx {
            src,
            cw,
            planes: [f, h_pl, v, c],
            stride,
            pad: pad as isize,
            px_max,
            py_max,
            lx: lx as isize,
            ly: ly as isize,
            h,
            satd,
            avg,
        })
    }

    /// SATD distortion of candidate `(mvx, mvy)` (quarter-pel units) — the exact
    /// value the safe dispatch computes — or `None` when the candidate leaves the
    /// validated window (caller falls back; identical value there too).
    #[inline]
    pub fn eval(&self, mvx: i32, mvy: i32) -> Option<u32> {
        let px = self.lx + (mvx >> 2) as isize + self.pad;
        let py = self.ly + (mvy >> 2) as isize + self.pad;
        if px < 0 || py < 0 || px > self.px_max || py > self.py_max {
            return None;
        }
        let base = py as usize * self.stride + px as usize;
        let (fx, fy) = (mvx & 3, mvy & 3);
        let st = self.stride;
        // SAFETY (whole match): `base + oa/ob + (h-1)·stride + w (+1 slack)` is
        // inside every plane by the constructor's `pw·ph` length check together
        // with the px/py window test above; `src` covers `(h-1)·cw + w`.
        unsafe {
            if fx & 1 == 0 && fy & 1 == 0 {
                // Full- and half-pel: one plane, read in place.
                let p = match (fx, fy) {
                    (0, 0) => self.planes[PF],
                    (2, 0) => self.planes[PH],
                    (0, 2) => self.planes[PV],
                    _ => self.planes[PC], // (2, 2)
                };
                let v = (self.satd)(
                    self.src.as_ptr(),
                    self.cw as i32,
                    p.as_ptr().add(base),
                    st as i32,
                );
                return Some(2 * v as u32);
            }
            // Quarter phases: the spec's (a+b+1)>>1 of two planes, operand table
            // identical to `hpel_qpel_refs` (pinned by the oracle test).
            let (pa, oa, pb, ob) = match (fx, fy) {
                (1, 0) => (PF, 0, PH, 0),
                (3, 0) => (PF, 1, PH, 0),
                (0, 1) => (PF, 0, PV, 0),
                (0, 3) => (PF, st, PV, 0),
                (1, 1) => (PH, 0, PV, 0),
                (3, 1) => (PH, 0, PV, 1),
                (1, 3) => (PH, st, PV, 0),
                (3, 3) => (PH, st, PV, 1),
                (2, 1) => (PH, 0, PC, 0),
                (2, 3) => (PH, st, PC, 0),
                (1, 2) => (PV, 0, PC, 0),
                _ => (PV, 1, PC, 0), // (3, 2)
            };
            Some((self.avg)(
                self.src.as_ptr(),
                self.cw,
                self.planes[pa].as_ptr().add(base + oa),
                self.planes[pb].as_ptr().add(base + ob),
                st,
                self.h,
            ))
        }
    }
}
