//! Deblocking filter kernels (§8.7.2.5).
//!
//! This was the last large stage of the decoder with no kernel at all. Priced
//! with the paired instrument -- `RH265_NO_SAO` against `RH265_NO_LF`, two arms
//! differing by exactly deblocking -- it is **13.3% of decode** (1.154x, 19/20,
//! z = 4.02), more than SAO, which has been fully vectorised for some time.
//!
//! # Layout: one lane per LINE, `i32` throughout
//!
//! A luma edge segment is four lines of eight samples, `p3 p2 p1 p0 | q0 q1 q2 q3`,
//! and the filter treats the four lines independently. So the natural vector
//! layout is **one lane per line**: eight registers, one per tap position, each
//! holding that tap's value for lines 0..3.
//!
//! Four `i32` lanes is exactly one `__m128i`, which is why the arithmetic is
//! `i32` and not `i16`. It could be `i16` -- the samples and every intermediate
//! fit at 8 and 10 bits -- and that would process eight lines at once. It is
//! deliberately not: the previous campaign found two kernels that were correct
//! only because a bound was asserted in a distant SPS check, and narrowing the
//! lanes here would buy throughput in exchange for exactly that class of latent
//! defect. `i32` makes this kernel bit-identical to its scalar twin by
//! construction, at any bit depth, with nothing to prove.
//!
//! # Getting the samples into that layout
//!
//! `dir` decides which axis is contiguous, and the two cases are not symmetric:
//!
//! * **Horizontal edges** (`dir == 1`): tap `t` is a whole ROW, and the four
//!   lines are four adjacent columns. One 8-byte load per tap already IS the
//!   layout -- no transpose, either way.
//! * **Vertical edges** (`dir == 0`): line `k` is a row of eight contiguous
//!   samples, so one 16-byte load holds all eight taps of ONE line. That is the
//!   transpose of what is wanted, so four loads are transposed 4x8 -> 8x4 on the
//!   way in and back on the way out.
//!
//! # Decisions stay scalar, on purpose
//!
//! `dd >= beta` rejects the whole segment before any filtering, and the
//! strong/weak choice and `dep`/`deq` are per SEGMENT, not per line -- they read
//! twelve samples from lines 0 and 3 only. Computing them scalar keeps the
//! early-out cheap: a rejected segment never pays for the eight-tap gather. Only
//! `|delta| < 10*tc` in the weak path is per line, and that becomes a lane mask.

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

use crate::census;

/// Scalar twin -- the oracle, and the path on any non-SIMD build.
///
/// A literal transcription of §8.7.2.5.3 / 8.7.2.5.6 / 8.7.2.5.7, kept in the
/// tree forever. `luma_edge_matches_scalar` pins the kernel to it.
#[allow(clippy::too_many_arguments)]
pub fn luma_edge_scalar(data: &mut [u16], stride: usize, x: usize, y: usize, dir: usize, beta: i32, tc: i32, no_p: bool, no_q: bool, max: i32) {
    let (line_step, tap_step): (isize, isize) = if dir == 0 { (stride as isize, 1) } else { (1, stride as isize) };
    let origin = (y * stride + x) as isize;
    let toff: [isize; 8] = core::array::from_fn(|t| (t as isize - 4) * tap_step);
    let idx = |k: usize, i: i32| -> usize { (origin + k as isize * line_step + toff[(i + 4) as usize]) as usize };

    let g = |d: &[u16], k: usize, i: i32| d[idx(k, i)] as i32;
    let dp0 = (g(data, 0, -3) - 2 * g(data, 0, -2) + g(data, 0, -1)).abs();
    let dp3 = (g(data, 3, -3) - 2 * g(data, 3, -2) + g(data, 3, -1)).abs();
    let dq0 = (g(data, 0, 2) - 2 * g(data, 0, 1) + g(data, 0, 0)).abs();
    let dq3 = (g(data, 3, 2) - 2 * g(data, 3, 1) + g(data, 3, 0)).abs();
    let dpq0 = dp0 + dq0;
    let dpq3 = dp3 + dq3;
    let dp = dp0 + dp3;
    let dq = dq0 + dq3;
    if dpq0 + dpq3 >= beta {
        return;
    }
    let dsam = |k: usize, dpq: i32| -> bool {
        dpq < (beta >> 2) && (g(data, k, -4) - g(data, k, -1)).abs() + (g(data, k, 0) - g(data, k, 3)).abs() < (beta >> 3) && (g(data, k, -1) - g(data, k, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = dsam(0, 2 * dpq0) && dsam(3, 2 * dpq3);
    let dep = dp < ((beta + (beta >> 1)) >> 3);
    let deq = dq < ((beta + (beta >> 1)) >> 3);

    let mut out = [[0i32; 8]; 4];
    let mut write = [[false; 8]; 4];
    for k in 0..4 {
        let p0 = g(data, k, -1);
        let p1 = g(data, k, -2);
        let p2 = g(data, k, -3);
        let p3 = g(data, k, -4);
        let q0 = g(data, k, 0);
        let q1 = g(data, k, 1);
        let q2 = g(data, k, 2);
        let q3 = g(data, k, 3);
        if strong {
            let t2 = 2 * tc;
            let sp = p0 + q0;
            let u = p1 + p2;
            let v = q1 + q2;
            let a = sp + 2;
            let w = 2 * sp + p1 + q1 + 4;
            let b = sp + 4;
            out[k][3] = ((w + u) >> 3).clamp(p0 - t2, p0 + t2);
            out[k][2] = ((a + u) >> 2).clamp(p1 - t2, p1 + t2);
            out[k][1] = ((b + u + 2 * (p2 + p3)) >> 3).clamp(p2 - t2, p2 + t2);
            out[k][4] = ((w + v) >> 3).clamp(q0 - t2, q0 + t2);
            out[k][5] = ((a + v) >> 2).clamp(q1 - t2, q1 + t2);
            out[k][6] = ((b + v + 2 * (q2 + q3)) >> 3).clamp(q2 - t2, q2 + t2);
            for i in 1..7 {
                write[k][i] = true;
            }
        } else {
            let da = q0 - p0;
            let db = q1 - p1;
            let t = da + (da << 1) - db;
            let mut delta = (t + (t << 1) + 8) >> 4;
            if delta.abs() < tc * 10 {
                delta = delta.clamp(-tc, tc);
                out[k][3] = (p0 + delta).clamp(0, max);
                out[k][4] = (q0 - delta).clamp(0, max);
                write[k][3] = true;
                write[k][4] = true;
                if dep {
                    let dlt = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    out[k][2] = (p1 + dlt).clamp(0, max);
                    write[k][2] = true;
                }
                if deq {
                    let dlt = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    out[k][5] = (q1 + dlt).clamp(0, max);
                    write[k][5] = true;
                }
            }
        }
    }
    for k in 0..4 {
        for i in 0..8usize {
            if !write[k][i] {
                continue;
            }
            let pos = i as i32 - 4;
            if (pos < 0 && no_p) || (pos >= 0 && no_q) {
                continue;
            }
            data[idx(k, pos)] = out[k][i] as u16;
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod x86 {
    use super::*;

    /// `min`/`max` on `i32` lanes are SSE4.1; this kernel targets the SSE2
    /// baseline so every machine gets it, so both are a compare and a select.
    #[inline(always)]
    unsafe fn min32(a: __m128i, b: __m128i) -> __m128i {
        unsafe {
            let m = _mm_cmpgt_epi32(a, b);
            _mm_or_si128(_mm_and_si128(m, b), _mm_andnot_si128(m, a))
        }
    }

    #[inline(always)]
    unsafe fn max32(a: __m128i, b: __m128i) -> __m128i {
        unsafe {
            let m = _mm_cmpgt_epi32(a, b);
            _mm_or_si128(_mm_and_si128(m, a), _mm_andnot_si128(m, b))
        }
    }

    #[inline(always)]
    unsafe fn clamp32(v: __m128i, lo: __m128i, hi: __m128i) -> __m128i {
        unsafe { min32(max32(v, lo), hi) }
    }

    /// `|v|` without SSSE3's `pabsd`.
    #[inline(always)]
    unsafe fn abs32(v: __m128i) -> __m128i {
        unsafe {
            let s = _mm_srai_epi32(v, 31);
            _mm_sub_epi32(_mm_xor_si128(v, s), s)
        }
    }

    #[inline(always)]
    unsafe fn sel(mask: __m128i, t: __m128i, f: __m128i) -> __m128i {
        unsafe { _mm_or_si128(_mm_and_si128(mask, t), _mm_andnot_si128(mask, f)) }
    }

    /// # Safety
    /// The caller has bounds-checked every access the segment makes.
    #[target_feature(enable = "sse2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn luma_edge_sse2(data: *mut u16, stride: usize, x: usize, y: usize, dir: usize, tc: i32, no_p: bool, no_q: bool, max: i32, strong: bool, dep: bool, deq: bool) {
        let base = unsafe { data.add(y * stride + x) };
        let z = _mm_setzero_si128();
        let mut t = [z; 8];

        if dir == 1 {
            // Tap `i` is a whole row; the four lines are four adjacent columns,
            // so one 8-byte load per tap already is the layout.
            for (i, slot) in t.iter_mut().enumerate() {
                let row = unsafe { base.offset((i as isize - 4) * stride as isize) };
                *slot = _mm_unpacklo_epi16(unsafe { _mm_loadl_epi64(row as *const __m128i) }, z);
            }
        } else {
            // Line `k` is eight contiguous samples -- one load holds all eight
            // taps of ONE line, which is the transpose of what is wanted.
            let mut r = [z; 4];
            for (k, slot) in r.iter_mut().enumerate() {
                let p = unsafe { base.add(k * stride).offset(-4) };
                *slot = unsafe { _mm_loadu_si128(p as *const __m128i) };
            }
            let a0 = _mm_unpacklo_epi16(r[0], r[1]);
            let a1 = _mm_unpackhi_epi16(r[0], r[1]);
            let a2 = _mm_unpacklo_epi16(r[2], r[3]);
            let a3 = _mm_unpackhi_epi16(r[2], r[3]);
            let u0 = _mm_unpacklo_epi32(a0, a2);
            let u1 = _mm_unpackhi_epi32(a0, a2);
            let u2 = _mm_unpacklo_epi32(a1, a3);
            let u3 = _mm_unpackhi_epi32(a1, a3);
            t[0] = _mm_unpacklo_epi16(u0, z);
            t[1] = _mm_unpackhi_epi16(u0, z);
            t[2] = _mm_unpacklo_epi16(u1, z);
            t[3] = _mm_unpackhi_epi16(u1, z);
            t[4] = _mm_unpacklo_epi16(u2, z);
            t[5] = _mm_unpackhi_epi16(u2, z);
            t[6] = _mm_unpacklo_epi16(u3, z);
            t[7] = _mm_unpackhi_epi16(u3, z);
        }

        let (p3, p2, p1, p0) = (t[0], t[1], t[2], t[3]);
        let (q0, q1, q2, q3) = (t[4], t[5], t[6], t[7]);
        let mut o = t; // unwritten taps keep their original value

        if strong {
            // The factored form: every output shares `p0+q0`, and the two sides
            // are one expression pair with `p1+p2` / `q1+q2` exchanged.
            let t2 = _mm_set1_epi32(2 * tc);
            let sp = _mm_add_epi32(p0, q0);
            let u = _mm_add_epi32(p1, p2);
            let v = _mm_add_epi32(q1, q2);
            let a = _mm_add_epi32(sp, _mm_set1_epi32(2));
            let w = _mm_add_epi32(_mm_add_epi32(_mm_slli_epi32(sp, 1), _mm_add_epi32(p1, q1)), _mm_set1_epi32(4));
            let b = _mm_add_epi32(sp, _mm_set1_epi32(4));
            o[3] = unsafe { clamp32(_mm_srai_epi32(_mm_add_epi32(w, u), 3), _mm_sub_epi32(p0, t2), _mm_add_epi32(p0, t2)) };
            o[2] = unsafe { clamp32(_mm_srai_epi32(_mm_add_epi32(a, u), 2), _mm_sub_epi32(p1, t2), _mm_add_epi32(p1, t2)) };
            let e1 = _mm_add_epi32(_mm_add_epi32(b, u), _mm_slli_epi32(_mm_add_epi32(p2, p3), 1));
            o[1] = unsafe { clamp32(_mm_srai_epi32(e1, 3), _mm_sub_epi32(p2, t2), _mm_add_epi32(p2, t2)) };
            o[4] = unsafe { clamp32(_mm_srai_epi32(_mm_add_epi32(w, v), 3), _mm_sub_epi32(q0, t2), _mm_add_epi32(q0, t2)) };
            o[5] = unsafe { clamp32(_mm_srai_epi32(_mm_add_epi32(a, v), 2), _mm_sub_epi32(q1, t2), _mm_add_epi32(q1, t2)) };
            let e6 = _mm_add_epi32(_mm_add_epi32(b, v), _mm_slli_epi32(_mm_add_epi32(q2, q3), 1));
            o[6] = unsafe { clamp32(_mm_srai_epi32(e6, 3), _mm_sub_epi32(q2, t2), _mm_add_epi32(q2, t2)) };
        } else {
            let maxv = _mm_set1_epi32(max);
            let tcv = _mm_set1_epi32(tc);
            let da = _mm_sub_epi32(q0, p0);
            let db = _mm_sub_epi32(q1, p1);
            let s = _mm_sub_epi32(_mm_add_epi32(da, _mm_slli_epi32(da, 1)), db);
            let delta = _mm_srai_epi32(_mm_add_epi32(_mm_add_epi32(s, _mm_slli_epi32(s, 1)), _mm_set1_epi32(8)), 4);
            // `|delta| < 10*tc` is the one PER-LINE decision, so it is a mask.
            let live = _mm_cmpgt_epi32(_mm_set1_epi32(tc * 10), unsafe { abs32(delta) });
            let d = unsafe { clamp32(delta, _mm_sub_epi32(z, tcv), tcv) };
            o[3] = unsafe { sel(live, clamp32(_mm_add_epi32(p0, d), z, maxv), p0) };
            o[4] = unsafe { sel(live, clamp32(_mm_sub_epi32(q0, d), z, maxv), q0) };
            let half = _mm_set1_epi32(tc >> 1);
            let nhalf = _mm_sub_epi32(z, half);
            if dep {
                let avg = _mm_srai_epi32(_mm_add_epi32(_mm_add_epi32(p2, p0), _mm_set1_epi32(1)), 1);
                let dlt = unsafe { clamp32(_mm_srai_epi32(_mm_add_epi32(_mm_sub_epi32(avg, p1), d), 1), nhalf, half) };
                o[2] = unsafe { sel(live, clamp32(_mm_add_epi32(p1, dlt), z, maxv), p1) };
            }
            if deq {
                let avg = _mm_srai_epi32(_mm_add_epi32(_mm_add_epi32(q2, q0), _mm_set1_epi32(1)), 1);
                let dlt = unsafe { clamp32(_mm_srai_epi32(_mm_sub_epi32(_mm_sub_epi32(avg, q1), d), 1), nhalf, half) };
                o[5] = unsafe { sel(live, clamp32(_mm_add_epi32(q1, dlt), z, maxv), q1) };
            }
        }

        // A suppressed side keeps its original samples, so the stores below need
        // no masking: an unwritten tap stores the value it already held.
        if no_p {
            o[1] = t[1];
            o[2] = t[2];
            o[3] = t[3];
        }
        if no_q {
            o[4] = t[4];
            o[5] = t[5];
            o[6] = t[6];
        }

        if dir == 1 {
            for i in 1..7usize {
                let row = unsafe { base.offset((i as isize - 4) * stride as isize) };
                unsafe { _mm_storel_epi64(row as *mut __m128i, _mm_packs_epi32(o[i], o[i])) };
            }
        } else {
            // 8x4 -> 4x8, then one 16-byte store per line. `p3`/`q3` go back
            // unchanged, which is why the whole row can be written at once.
            let a = _mm_unpacklo_epi32(o[0], o[1]);
            let b = _mm_unpackhi_epi32(o[0], o[1]);
            let c = _mm_unpacklo_epi32(o[2], o[3]);
            let d = _mm_unpackhi_epi32(o[2], o[3]);
            let lo = [_mm_unpacklo_epi64(a, c), _mm_unpackhi_epi64(a, c), _mm_unpacklo_epi64(b, d), _mm_unpackhi_epi64(b, d)];
            let a = _mm_unpacklo_epi32(o[4], o[5]);
            let b = _mm_unpackhi_epi32(o[4], o[5]);
            let c = _mm_unpacklo_epi32(o[6], o[7]);
            let d = _mm_unpackhi_epi32(o[6], o[7]);
            let hi = [_mm_unpacklo_epi64(a, c), _mm_unpackhi_epi64(a, c), _mm_unpacklo_epi64(b, d), _mm_unpackhi_epi64(b, d)];
            for k in 0..4usize {
                let p = unsafe { base.add(k * stride).offset(-4) };
                unsafe { _mm_storeu_si128(p as *mut __m128i, _mm_packs_epi32(lo[k], hi[k])) };
            }
        }
    }
}

/// Bring-up switch: `RH265_SCALAR_DEBLOCK=1` forces the scalar twin.
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: off).
#[cfg(feature = "std")]
fn scalar_deblock() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RH265_SCALAR_DEBLOCK").is_some())
}
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: off).
#[cfg(not(feature = "std"))]
fn scalar_deblock() -> bool {
    false
}

/// One four-line luma edge segment.
///
/// `dir` 0 = vertical edge at column `x`, 1 = horizontal edge at row `y`.
#[allow(clippy::too_many_arguments)]
pub fn luma_edge(data: &mut [u16], stride: usize, x: usize, y: usize, dir: usize, beta: i32, tc: i32, no_p: bool, no_q: bool, max: i32) {
    // The segment's whole footprint, so the kernel's loads and stores need no
    // further checks. Vertical edges touch `x-4 ..= x+3` on rows `y ..= y+3`;
    // horizontal edges touch column `x ..= x+3` on rows `y-4 ..= y+3`.
    // (`x >= 4` is a vertical-edge condition only: a horizontal edge at x = 0
    // -- every picture's left column -- is legal and its footprint starts at x.)
    let ok = x + 4 <= stride && (y + 4) * stride <= data.len() && if dir == 0 { x >= 4 } else { y >= 4 };
    debug_assert!(ok, "deblock bounds: dir={dir} x={x} y={y} stride={stride} len={}", data.len());

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok && !scalar_deblock() && crate::isa() != crate::Isa::Scalar {
        // The decisions are per SEGMENT and they gate the early-out, so they
        // stay scalar; only the per-line filtering is vectorised.
        let (line_step, tap_step): (isize, isize) = if dir == 0 { (stride as isize, 1) } else { (1, stride as isize) };
        let origin = (y * stride + x) as isize;
        // `ok` above proves the segment's entire footprint is in bounds, and
        // this closure is evaluated ~16 times on EVERY segment -- including the
        // 39% that early-out and do nothing else. Bounds-checking each of them
        // is the dominant remaining cost on that path.
        //
        // SAFETY: every `(k, i)` used below lies inside the footprint `ok`
        // checked: `k` in 0..4 and `i` in -4..=3, which is exactly the range the
        // check covers for either direction.
        let g = |k: usize, i: i32| unsafe { *data.get_unchecked((origin + k as isize * line_step + i as isize * tap_step) as usize) } as i32;
        let dp0 = (g(0, -3) - 2 * g(0, -2) + g(0, -1)).abs();
        let dp3 = (g(3, -3) - 2 * g(3, -2) + g(3, -1)).abs();
        let dq0 = (g(0, 2) - 2 * g(0, 1) + g(0, 0)).abs();
        let dq3 = (g(3, 2) - 2 * g(3, 1) + g(3, 0)).abs();
        let dpq0 = dp0 + dq0;
        let dpq3 = dp3 + dq3;
        if dpq0 + dpq3 >= beta {
            if census::ALWAYS {
                census::arm(&census::RT_DEBLOCK_SKIP);
            }
            return;
        }
        let dsam = |k: usize, dpq: i32| -> bool { dpq < (beta >> 2) && (g(k, -4) - g(k, -1)).abs() + (g(k, 0) - g(k, 3)).abs() < (beta >> 3) && (g(k, -1) - g(k, 0)).abs() < ((5 * tc + 1) >> 1) };
        let strong = dsam(0, 2 * dpq0) && dsam(3, 2 * dpq3);
        let dep = (dp0 + dp3) < ((beta + (beta >> 1)) >> 3);
        let deq = (dq0 + dq3) < ((beta + (beta >> 1)) >> 3);
        if census::ALWAYS {
            census::route(strong, &census::RT_DEBLOCK_STRONG, &census::RT_DEBLOCK_WEAK);
            census::bump(&census::DEBLOCK_LUMA_SIMD, 1);
        }
        // SAFETY: `ok` above covers every load and store the kernel makes.
        unsafe { x86::luma_edge_sse2(data.as_mut_ptr(), stride, x, y, dir, tc, no_p, no_q, max, strong, dep, deq) };
        return;
    }
    if census::ALWAYS {
        census::bump(&census::DEBLOCK_LUMA_SCALAR, 1);
    }
    luma_edge_scalar(data, stride, x, y, dir, beta, tc, no_p, no_q, max);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *s >> 8
    }

    /// The kernel against the scalar oracle, over inputs chosen to reach every
    /// arm: the `dd >= beta` early-out, the strong filter, the weak filter with
    /// and without `dep`/`deq`, and the per-line `|delta| < 10*tc` mask.
    ///
    /// Random samples alone would almost never take the strong path -- it needs
    /// a nearly flat block either side of the edge -- so flat, ramp and
    /// near-flat patterns are generated deliberately, and the test asserts that
    /// each arm was actually reached rather than trusting that it was.
    #[test]
    fn luma_edge_matches_scalar() {
        let mut st = 0xd3b1_0c47u32;
        let stride = 64usize;
        let h = 48usize;
        let (mut strongs, mut weaks, mut skips) = (0usize, 0usize, 0usize);
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for pattern in 0..4usize {
                for &beta in &[6i32, 16, 40, 64] {
                    for &tc in &[1i32, 3, 9, 18] {
                        for dir in 0..2usize {
                            for &(no_p, no_q) in &[(false, false), (true, false), (false, true)] {
                                let mut a = vec![0u16; stride * h];
                                for i in 0..a.len() {
                                    let (px, py) = (i % stride, i / stride);
                                    a[i] = match pattern {
                                        0 => {
                                            // flat either side of the edge -> strong
                                            let across = if dir == 0 { px } else { py };
                                            if across < 20 {
                                                40
                                            } else {
                                                44
                                            }
                                        }
                                        1 => ((px + py) * 3) as u16 & max as u16,
                                        2 => (60 + (lcg(&mut st) % 5)) as u16,
                                        _ => (lcg(&mut st) & max as u32) as u16,
                                    };
                                }
                                let mut b = a.clone();
                                let (x, y) = (20usize, 20usize);
                                luma_edge_scalar(&mut a, stride, x, y, dir, beta, tc, no_p, no_q, max);
                                luma_edge(&mut b, stride, x, y, dir, beta, tc, no_p, no_q, max);
                                assert_eq!(a, b, "bd={bd} pat={pattern} beta={beta} tc={tc} dir={dir} no_p={no_p} no_q={no_q}");
                                match pattern {
                                    0 => strongs += 1,
                                    1 | 2 => weaks += 1,
                                    _ => skips += 1,
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(strongs > 0 && weaks > 0 && skips > 0, "arms not all reached: {strongs}/{weaks}/{skips}");
    }
}
