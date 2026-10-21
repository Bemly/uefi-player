//! Frame buffers and motion-compensated prediction (H.262 §7.6).

// VENDOR(uefi-player): `alloc` paths resolve identically in std and no_std builds.
use alloc::{vec, vec::Vec};

/// A decoded frame, padded to whole macroblocks.
#[derive(Clone)]
pub(crate) struct Frame {
    pub planes: [Vec<u8>; 3],
    /// Luma width / height (multiples of 16).
    pub w: usize,
    pub h: usize,
    /// Chroma width / height.
    pub cw: usize,
    pub ch: usize,
}

impl Frame {
    pub fn new(w: usize, h: usize, cw: usize, ch: usize) -> Frame {
        Frame { planes: [vec![16; w * h], vec![128; cw * ch], vec![128; cw * ch]], w, h, cw, ch }
    }

    /// Plane `c` (0 Y, 1 Cb, 2 Cr) as a reference: the whole frame, or one field (0 top, 1 bottom).
    #[inline]
    pub fn plane(&self, c: usize, field: Option<usize>) -> RefPlane<'_> {
        let (w, h) = if c == 0 { (self.w, self.h) } else { (self.cw, self.ch) };
        match field {
            None => RefPlane { data: &self.planes[c], off: 0, stride: w, w, h },
            Some(p) => RefPlane { data: &self.planes[c], off: p * w, stride: 2 * w, w, h: h / 2 },
        }
    }
}

/// A plane (or a field of one) to predict from.
#[derive(Clone, Copy)]
pub(crate) struct RefPlane<'a> {
    pub data: &'a [u8],
    pub off: usize,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
}

/// Form the `bw`×`bh` prediction at integer position (`x`, `y`) with half-sample flags into
/// `dst` (row `r` at `dst[doff + r·dstride..]`), or average it with what is there (`avg`).
/// Positions outside the reference are clamped to its edges (not allowed in conforming streams;
/// this keeps corrupt ones safe).
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn predict(dst: &mut [u8], doff: usize, dstride: usize, src: RefPlane, x: i32, y: i32, hx: bool, hy: bool, bw: usize, bh: usize, avg: bool) {
    let (ex, ey) = (hx as i32, hy as i32);
    let inside = x >= 0 && y >= 0 && (x + bw as i32 + ex) as usize <= src.w && (y + bh as i32 + ey) as usize <= src.h;
    let mut tmp = [0u8; 16];
    if inside {
        // VENDOR(uefi-player): one switch check per `predict` call; the row
        // loop below branches on this loop-invariant bool (perfectly
        // predicted), so hot rows pay no atomic dispatch. Without `simd`
        // the true-branches below are compiled out and this folds away.
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        let simd = crate::simd_enabled();
        let s = src.stride;
        let base = src.off + y as usize * s + x as usize;
        for r in 0..bh {
            let p = base + r * s;
            let a = &src.data[p..p + bw + 1 - (!hx) as usize];
            let row = &mut tmp[..bw];
            match (hx, hy) {
                (false, false) => row.copy_from_slice(&a[..bw]),
                (true, false) => {
                    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
                    if simd {
                        avg2_simd(row, a, &a[1..]);
                    } else {
                        avg2_scalar(row, a, &a[1..]);
                    }
                    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
                    avg2_scalar(row, a, &a[1..]);
                }
                (false, true) => {
                    let b = &src.data[p + s..p + s + bw];
                    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
                    if simd {
                        avg2_simd(row, a, b);
                    } else {
                        avg2_scalar(row, a, b);
                    }
                    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
                    avg2_scalar(row, a, b);
                }
                (true, true) => {
                    let b = &src.data[p + s..p + s + bw + 1];
                    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
                    if simd {
                        avg4_simd(row, a, &a[1..], b, &b[1..]);
                    } else {
                        avg4_scalar(row, a, &a[1..], b, &b[1..]);
                    }
                    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
                    avg4_scalar(row, a, &a[1..], b, &b[1..]);
                }
            }
            #[cfg(all(feature = "simd", target_arch = "x86_64"))]
            if simd {
                store_simd(dst, doff + r * dstride, row, avg);
            } else {
                store_scalar(dst, doff + r * dstride, row, avg);
            }
            #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
            store_scalar(dst, doff + r * dstride, row, avg);
        }
        return;
    }
    let get = |xx: i32, yy: i32| -> u16 {
        let xx = xx.clamp(0, src.w as i32 - 1) as usize;
        let yy = yy.clamp(0, src.h as i32 - 1) as usize;
        src.data[src.off + yy * src.stride + xx] as u16
    };
    for r in 0..bh {
        let yy = y + r as i32;
        let row = &mut tmp[..bw];
        for (i, v) in row.iter_mut().enumerate() {
            let xx = x + i as i32;
            *v = match (hx, hy) {
                (false, false) => get(xx, yy),
                (true, false) => (get(xx, yy) + get(xx + 1, yy) + 1) >> 1,
                (false, true) => (get(xx, yy) + get(xx, yy + 1) + 1) >> 1,
                (true, true) => (get(xx, yy) + get(xx + 1, yy) + get(xx, yy + 1) + get(xx + 1, yy + 1) + 2) >> 2,
            } as u8;
        }
        // Edge/clamped path: shared scalar (corrupt-stream slow path).
        store_scalar(dst, doff + r * dstride, row, avg);
    }
}

/// Scalar twin: `(a[i] + b[i] + 1) >> 1` (always compiled).
fn avg2_scalar(row: &mut [u8], a: &[u8], b: &[u8]) {
    for (o, (&x, &y)) in row.iter_mut().zip(a.iter().zip(b)) {
        *o = ((x as u16 + y as u16 + 1) >> 1) as u8;
    }
}

/// Scalar twin: `(a[i] + b[i] + c[i] + d[i] + 2) >> 2` (always compiled).
fn avg4_scalar(row: &mut [u8], a: &[u8], b: &[u8], c: &[u8], d: &[u8]) {
    for (o, (((&w, &x), &y), &z)) in row.iter_mut().zip(a.iter().zip(b).zip(c).zip(d)) {
        *o = ((w as u16 + x as u16 + y as u16 + z as u16 + 2) >> 2) as u8;
    }
}

/// Scalar twin of the averaging half of `store` (always compiled).
fn store_avg_scalar(d: &mut [u8], row: &[u8]) {
    for (o, &p) in d.iter_mut().zip(row) {
        *o = ((*o as u16 + p as u16 + 1) >> 1) as u8;
    }
}

/// SIMD half of the two-tap average `row[i] = (a[i] + b[i] + 1) >> 1`.
/// Width-specialized straight-line kernels: every in-tree caller passes
/// `bw` 8 or 16 (luma 16, chroma `16 >> shx`), so there is no chunk loop —
/// the branchiest code here is one width check, which keeps the surrounding
/// row loop as tight as the scalar twin's.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[inline]
fn avg2_simd(row: &mut [u8], a: &[u8], b: &[u8]) {
    // VENDOR(uefi-player): `unsafe` required by edition 2024 even
    // inside the `unsafe fn`.
    match row.len() {
        16 => unsafe { avg2_16(row.as_mut_ptr(), a.as_ptr(), b.as_ptr()) },
        8 => unsafe { avg2_8(row.as_mut_ptr(), a.as_ptr(), b.as_ptr()) },
        _ => avg2_scalar(row, a, b),
    }
}

/// SIMD half of the four-tap average
/// `row[i] = (a[i] + b[i] + c[i] + d[i] + 2) >> 2`.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[inline]
fn avg4_simd(row: &mut [u8], a: &[u8], b: &[u8], c: &[u8], d: &[u8]) {
    match row.len() {
        16 => unsafe { avg4_16(row.as_mut_ptr(), a.as_ptr(), b.as_ptr(), c.as_ptr(), d.as_ptr()) },
        8 => unsafe { avg4_8(row.as_mut_ptr(), a.as_ptr(), b.as_ptr(), c.as_ptr(), d.as_ptr()) },
        _ => avg4_scalar(row, a, b, c, d),
    }
}

// VENDOR(uefi-player): integer-SSE2 twins of the averaging kernels above.
// `pavgb(a, b)` is exactly `(a + b + 1) >> 1` per byte; the four-tap form
// widens to words (`punpcklbw`), adds (`paddw`, `+ 2` included — the sum tops
// at `4·255 + 2`, exact in 16 bits), shifts (`psrlw`) and narrows
// (`packuswb`, values `<= 255` so nothing saturates). `movq`/`movdqu`
// throughout: no alignment requirement, no `nt` stores (small reused row
// buffers — temporal locality, hence no `sfence` either). SSE2 only: no AVX,
// no XSAVE involvement; the firmware `[accel]` switch is config semantics.
// Width-specialized straight-line bodies (8/16, the only in-tree widths):
// no chunk loops, so the dispatchers above stay branch-lean.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn avg2_16(dst: *mut u8, a: *const u8, b: *const u8) {
    unsafe {
        core::arch::asm!(
            "movdqu {t0}, [{ap}]",
            "movdqu {t1}, [{bp}]",
            "pavgb {t0}, {t1}",
            "movdqu [{dp}], {t0}",
            ap = in(reg) a,
            bp = in(reg) b,
            dp = in(reg) dst,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn avg2_8(dst: *mut u8, a: *const u8, b: *const u8) {
    unsafe {
        core::arch::asm!(
            "movq {t0}, [{ap}]",
            "movq {t1}, [{bp}]",
            "pavgb {t0}, {t1}",
            "movq [{dp}], {t0}",
            ap = in(reg) a,
            bp = in(reg) b,
            dp = in(reg) dst,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn avg4_16(dst: *mut u8, a: *const u8, b: *const u8, c: *const u8, d: *const u8) {
    unsafe {
        core::arch::asm!(
            // `+ 2` rounding splat materialized in-template (`pcmpeqd` →
            // all-ones, `>> 15` → 1, doubled → 2 per word): a `lateout`
            // temp may share its register with an `in(xmm_reg)` operand
            // (the `lateout` contract reads inputs first), so a late-read
            // vector input gets clobbered by the earlier loads — GPR
            // pointer inputs are immune (different register class),
            // vector inputs are not.
            "pcmpeqd {two}, {two}",
            "psrlw {two}, 15",
            "paddw {two}, {two}",
            "pxor {z}, {z}",
            "movdqu {t0}, [{a0p}]",
            "movdqu {t1}, [{a1p}]",
            "movdqu {t2}, [{b0p}]",
            "movdqu {t3}, [{b1p}]",
            "movdqa {l0}, {t0}",
            "punpcklbw {l0}, {z}",
            "punpckhbw {t0}, {z}",
            "movdqa {l1}, {t1}",
            "punpcklbw {l1}, {z}",
            "punpckhbw {t1}, {z}",
            "movdqa {l2}, {t2}",
            "punpcklbw {l2}, {z}",
            "punpckhbw {t2}, {z}",
            "movdqa {l3}, {t3}",
            "punpcklbw {l3}, {z}",
            "punpckhbw {t3}, {z}",
            "paddw {l0}, {l1}",
            "paddw {l2}, {l3}",
            "paddw {l0}, {l2}",
            "paddw {l0}, {two}",
            "psrlw {l0}, 2",
            "paddw {t0}, {t1}",
            "paddw {t2}, {t3}",
            "paddw {t0}, {t2}",
            "paddw {t0}, {two}",
            "psrlw {t0}, 2",
            "packuswb {l0}, {t0}",
            "movdqu [{dp}], {l0}",
            a0p = in(reg) a,
            a1p = in(reg) b,
            b0p = in(reg) c,
            b1p = in(reg) d,
            dp = in(reg) dst,
            two = lateout(xmm_reg) _,
            z = lateout(xmm_reg) _,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            t2 = lateout(xmm_reg) _,
            t3 = lateout(xmm_reg) _,
            l0 = lateout(xmm_reg) _,
            l1 = lateout(xmm_reg) _,
            l2 = lateout(xmm_reg) _,
            l3 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn avg4_8(dst: *mut u8, a: *const u8, b: *const u8, c: *const u8, d: *const u8) {
    unsafe {
        core::arch::asm!(
            "pcmpeqd {two}, {two}",
            "psrlw {two}, 15",
            "paddw {two}, {two}",
            "pxor {z}, {z}",
            "movq {t0}, [{a0p}]",
            "movq {t1}, [{a1p}]",
            "movq {t2}, [{b0p}]",
            "movq {t3}, [{b1p}]",
            "punpcklbw {t0}, {z}",
            "punpcklbw {t1}, {z}",
            "punpcklbw {t2}, {z}",
            "punpcklbw {t3}, {z}",
            "paddw {t0}, {t1}",
            "paddw {t2}, {t3}",
            "paddw {t0}, {t2}",
            "paddw {t0}, {two}",
            "psrlw {t0}, 2",
            "packuswb {t0}, {t0}",
            "movq [{dp}], {t0}",
            a0p = in(reg) a,
            a1p = in(reg) b,
            b0p = in(reg) c,
            b1p = in(reg) d,
            dp = in(reg) dst,
            two = lateout(xmm_reg) _,
            z = lateout(xmm_reg) _,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            t2 = lateout(xmm_reg) _,
            t3 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// In-place two-tap average for `store(..., avg = true)`, same `pavgb`
/// shape as `avg2` (reads the destination it writes).
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn store_avg_16(d: *mut u8, row: *const u8) {
    unsafe {
        core::arch::asm!(
            "movdqu {t0}, [{dp}]",
            "movdqu {t1}, [{rp}]",
            "pavgb {t0}, {t1}",
            "movdqu [{dp}], {t0}",
            dp = in(reg) d,
            rp = in(reg) row,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn store_avg_8(d: *mut u8, row: *const u8) {
    unsafe {
        core::arch::asm!(
            "movq {t0}, [{dp}]",
            "movq {t1}, [{rp}]",
            "pavgb {t0}, {t1}",
            "movq [{dp}], {t0}",
            dp = in(reg) d,
            rp = in(reg) row,
            t0 = lateout(xmm_reg) _,
            t1 = lateout(xmm_reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// Scalar half of `store` (always compiled; also serves the shared
/// edge/clamped path, which never takes the fast path).
#[inline(always)]
fn store_scalar(dst: &mut [u8], at: usize, row: &[u8], avg: bool) {
    let d = &mut dst[at..at + row.len()];
    if avg {
        store_avg_scalar(d, row);
    } else {
        d.copy_from_slice(row);
    }
}

/// SIMD half of `store`: in-place two-tap average, same `pavgb` shape as
/// `avg2` (reads the destination it writes).
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[inline(always)]
fn store_simd(dst: &mut [u8], at: usize, row: &[u8], avg: bool) {
    let d = &mut dst[at..at + row.len()];
    if avg {
        match d.len() {
            16 => unsafe { store_avg_16(d.as_mut_ptr(), row.as_ptr()) },
            8 => unsafe { store_avg_8(d.as_mut_ptr(), row.as_ptr()) },
            _ => store_avg_scalar(d, row),
        }
    } else {
        d.copy_from_slice(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        let mut f = Frame::new(32, 32, 16, 16);
        for (i, v) in f.planes[0].iter_mut().enumerate() {
            *v = (i % 32 * 3 + i / 32 * 5) as u8;
        }
        f
    }

    #[test]
    fn half_sample_interpolation_rounds_up() {
        let f = frame();
        let p = f.plane(0, None);
        let mut d = [0u8; 4];
        predict(&mut d, 0, 2, p, 1, 1, true, true, 2, 2, false);
        let s = |x: usize, y: usize| f.planes[0][y * 32 + x] as u16;
        assert_eq!(d[0] as u16, (s(1, 1) + s(2, 1) + s(1, 2) + s(2, 2) + 2) >> 2);
        predict(&mut d, 0, 2, p, 1, 1, true, false, 2, 2, false);
        assert_eq!(d[3] as u16, (s(2, 2) + s(3, 2) + 1) >> 1);
    }

    // VENDOR(uefi-player): dual-path interlock. Runs under every shape with
    // the `simd` feature, flipping the runtime switch so both averaging
    // kernels render the same predictions in one binary and must agree bit
    // for bit (the edge/clamped path is shared, so this guards the `pavgb`
    // fast path). Other tests never touch the switch, and the two paths are
    // interchangeable, so parallel test threads cannot disturb each other.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    fn both_predict(dst: &mut [u8], doff: usize, dstride: usize, src: RefPlane, x: i32, y: i32, hx: bool, hy: bool, bw: usize, bh: usize, avg: bool) -> Vec<u8> {
        let prev = crate::simd_enabled();
        let mut a = dst.to_vec();
        crate::set_simd_enabled(true);
        predict(&mut a, doff, dstride, src, x, y, hx, hy, bw, bh, avg);
        let mut s = dst.to_vec();
        crate::set_simd_enabled(false);
        predict(&mut s, doff, dstride, src, x, y, hx, hy, bw, bh, avg);
        crate::set_simd_enabled(prev);
        assert_eq!(a, s, "simd/scalar at x={x} y={y} hx={hx} hy={hy} bw={bw} bh={bh} avg={avg}");
        a
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn simd_predict_matches_scalar() {
        let f = frame();
        let p = f.plane(0, None);
        // Widths sweep every chunking class (8-wide main loop, scalar tail),
        // heights sweep 1..16, positions cover inside + all four clamped edges.
        for &(hx, hy) in &[(false, false), (true, false), (false, true), (true, true)] {
            for avg in [false, true] {
                for bw in 1..=16usize {
                    for bh in [1usize, 2, 3, 7, 8, 15, 16] {
                        for &(x, y) in &[(-3, -2), (-1, 0), (0, 0), (5, 7), (17, 19), (31, 31), (24, 33), (40, 40)] {
                            let mut d = [0xABu8; 512];
                            both_predict(&mut d, 3, 32, p, x, y, hx, hy, bw, bh, avg);
                        }
                    }
                }
            }
        }
        // Field views + chroma planes take the same kernels.
        let mut d = [0u8; 64];
        both_predict(&mut d, 0, 8, f.plane(0, Some(1)), 3, 2, true, true, 8, 4, true);
        both_predict(&mut d, 0, 8, f.plane(1, None), 3, 2, true, false, 8, 4, true);
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn both_paths_match_exact_hand_values() {
        // 8-row luma pattern 10/20/…/80 (hand-rolled frame below): every
        // expectation below is literal integer arithmetic, no oracle.
        let mut f = Frame::new(8, 8, 4, 4);
        for (i, v) in f.planes[0].iter_mut().enumerate() {
            *v = [10u8, 20, 30, 40, 50, 60, 70, 80][i % 8];
        }
        let p = f.plane(0, None);
        // (10+20+1)>>1 = 15, (20+30+1)>>1 = 25.
        let mut d = [0u8; 4];
        let got = both_predict(&mut d, 0, 2, p, 0, 0, true, false, 2, 2, false);
        assert_eq!(&got[..], &[15, 25, 15, 25]);
        // vertical: rows are identical, so (10+10+1)>>1 = 10, (20+20+1)>>1 = 20.
        let got = both_predict(&mut d, 0, 2, p, 0, 0, false, true, 2, 2, false);
        assert_eq!(&got[..], &[10, 20, 10, 20]);
        // four-tap: (10+20+10+20+2)>>2 = 62>>2 = 15; next (20+30+20+30+2)>>2 = 102>>2 = 25.
        let got = both_predict(&mut d, 0, 2, p, 0, 0, true, true, 2, 2, false);
        assert_eq!(&got[..], &[15, 25, 15, 25]);
        // store-average on zeros: (0+15+1)>>1 = 8, (0+25+1)>>1 = 13.
        let mut d = [0u8; 4];
        let got = both_predict(&mut d, 0, 2, p, 0, 0, true, false, 2, 2, true);
        assert_eq!(&got[..], &[8, 13, 8, 13]);
    }

    #[test]
    fn fields_and_clamped_edges() {
        let f = frame();
        let mut d = [0u8; 4];
        // bottom field row 0 = frame row 1
        predict(&mut d, 0, 2, f.plane(0, Some(1)), 0, 0, false, false, 2, 2, false);
        assert_eq!(d, [f.planes[0][32], f.planes[0][33], f.planes[0][96], f.planes[0][97]]);
        // outside: clamped to the corner
        predict(&mut d, 0, 2, f.plane(0, None), -10, -10, false, false, 2, 2, false);
        assert_eq!(d, [f.planes[0][0]; 4]);
        // averaging
        let mut d = [10u8; 4];
        predict(&mut d, 0, 2, f.plane(0, None), -10, -10, false, false, 2, 2, true);
        assert_eq!(d[0], ((10 + f.planes[0][0] as u16 + 1) >> 1) as u8);
    }
}
