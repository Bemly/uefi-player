//! Motion-compensation kernels: the fractional-sample interpolation of
//! §8.5.3.3.3 and the weighted sample prediction of §8.5.3.3.4.
//!
//! # Why every buffer here is `i16`
//!
//! The spec designs the interpolation intermediate to fit 16 bits: the
//! horizontal pass shifts by `BitDepth − 8` and its result, and the final
//! prediction sample `predSampleLX`, are defined over −32768..32767. Ranges,
//! for the widest 8-tap filter (`|taps|` sums to 88 with a positive part of 80):
//!
//! | term | 8-bit | 10-bit |
//! |---|---:|---:|
//! | horizontal sum before shift | ±20 400 | ±81 840 |
//! | after `>> (BitDepth − 8)` | ±20 400 | ±20 460 |
//! | vertical sum of those, before `>> 6` | ±1 795 200 | ±1 800 480 |
//! | after `>> 6` | ±28 050 | ±28 132 |
//! | full-pel `sample << (14 − BitDepth)` | 16 320 | 16 368 |
//!
//! Every stored value fits `i16`, every accumulator fits `i32`. That is what
//! makes `pmaddwd` the natural instruction for this codec: two `i16` lanes
//! multiplied and summed into one `i32` lane, which is exactly a two-tap
//! contribution. It also halves the memory traffic against an `i32`
//! intermediate — and this kernel is 61 % of decode, so that traffic is the
//! decoder's largest single cost.
//!
//! Every *single-pass* value above is proven to fit. The two-pass composite
//! has a wider adversarial bound — a 15×15 sample pattern chosen against the
//! separable filter reaches ±33 150 at 8-bit — but §8.5.3.3.3.1 makes
//! `−32768 ..= 32767` a **bitstream conformance requirement** on
//! `predSampleLX`, so a stream that reaches it is not a conforming stream. The
//! reference decoder stores the same value in an `i16` and would wrap there;
//! these kernels saturate instead, which is the safer of the two behaviours
//! for input nobody should be able to hand us. Within the conforming range the
//! packs are exact.
//!
//! # Footprint convention
//!
//! Every entry point takes `src` pointing at the **top-left of the filter
//! footprint**, not the block: `(xInt − 3, yInt − 3)` for luma, `(xInt − 1,
//! yInt − 1)` for chroma, with `(w + N − 1) × (h + N − 1)` samples readable at
//! `src_stride`. The caller either slices the reference plane directly (the
//! common case, when the footprint is inside the picture) or copies an
//! edge-extended footprint into scratch. Neither the scalar nor the SIMD path
//! contains a per-sample bounds clamp.

use crate::census;

/// Luma 8-tap filters `fL[xFracL]` (Table 8-11); index 0 is the full-pel case.
pub static LUMA_FILTER: [[i16; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [-1, 4, -10, 58, 17, -5, 1, 0],
    [-1, 4, -11, 40, 40, -11, 4, -1],
    [0, 1, -5, 17, 58, -10, 4, -1],
];

/// `(lo, hi)` bracketing the NONZERO taps of each luma filter.
///
/// Read by the 2-D path to skip filtering a row that would be multiplied by
/// zero. It is a table rather than a scan because the scan would run on every
/// 2-D call while the saving accrues on only the ~19% with a zero end tap --
/// the bookkeeping has to cost less than the work it removes, and at this size
/// "less" means a single indexed load.
///
/// Pinned against the filters themselves by `tap_spans_match_the_filters`.
pub static LUMA_SPAN: [(usize, usize); 4] = [(3, 3), (0, 6), (0, 7), (1, 7)];

/// `(lo, hi)` for the chroma filters. Only the full-pel entry is degenerate:
/// every fractional chroma filter has nonzero taps at both ends, so chroma
/// never elides a row.
pub static CHROMA_SPAN: [(usize, usize); 8] = [(1, 1), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3)];

/// Chroma 4-tap filters `fC[xFracC]` (Table 8-12); index 0 is full-pel.
pub static CHROMA_FILTER: [[i16; 4]; 8] = [
    [0, 64, 0, 0],
    [-2, 58, 10, -2],
    [-4, 54, 16, -2],
    [-6, 46, 28, -4],
    [-4, 36, 36, -4],
    [-4, 28, 46, -6],
    [-2, 16, 54, -4],
    [-2, 10, 58, -2],
];

/// Margin the footprint carries on each side, per tap count.
#[inline]
pub const fn margin(taps: usize) -> usize {
    taps / 2 - 1
}

// ---------------------------------------------------------------------------
// Scalar reference — the oracle, and the path on any target without kernels
// ---------------------------------------------------------------------------

/// `#[cold]`: the kernels take every conformant block -- `MC_LUMA_SCALAR` and
/// `MC_CHROMA_SCALAR` read 0 across the corpus -- so this is the fallback for a
/// shape they decline, not a path decode takes. Inlined, all four of these sat
/// inside `interp`, which IS the hot path.
#[cold]
#[inline(never)]
/// Horizontal FIR: `dst[y][x] = (Σ t[i]·src[y][x+i]) >> shift`.
fn fir_h_scalar<const N: usize>(src: &[u16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    for y in 0..h {
        let row = &src[y * stride..];
        let out = &mut dst[y * dst_stride..y * dst_stride + w];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0i32;
            for i in 0..N {
                acc += t[i] as i32 * row[x + i] as i32;
            }
            *o = (acc >> shift) as i16;
        }
    }
}

/// `#[cold]`: the kernels take every conformant block -- `MC_LUMA_SCALAR` and
/// `MC_CHROMA_SCALAR` read 0 across the corpus -- so this is the fallback for a
/// shape they decline, not a path decode takes. Inlined, all four of these sat
/// inside `interp`, which IS the hot path.
#[cold]
#[inline(never)]
/// Vertical FIR over `i16` rows: `dst[y][x] = (Σ t[i]·src[y+i][x]) >> shift`.
fn fir_v_scalar<const N: usize>(src: &[i16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    for y in 0..h {
        let out = &mut dst[y * dst_stride..y * dst_stride + w];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0i32;
            for i in 0..N {
                acc += t[i] as i32 * src[(y + i) * stride + x] as i32;
            }
            *o = (acc >> shift) as i16;
        }
    }
}

/// `#[cold]`: the kernels take every conformant block -- `MC_LUMA_SCALAR` and
/// `MC_CHROMA_SCALAR` read 0 across the corpus -- so this is the fallback for a
/// shape they decline, not a path decode takes. Inlined, all four of these sat
/// inside `interp`, which IS the hot path.
#[cold]
#[inline(never)]
/// Vertical FIR reading `u16` samples (the vertical-only case, where no
/// horizontal pass has widened them yet).
fn fir_v_u16_scalar<const N: usize>(src: &[u16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    for y in 0..h {
        let out = &mut dst[y * dst_stride..y * dst_stride + w];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0i32;
            for i in 0..N {
                acc += t[i] as i32 * src[(y + i) * stride + x] as i32;
            }
            *o = (acc >> shift) as i16;
        }
    }
}

/// `#[cold]`: the kernels take every conformant block -- `MC_LUMA_SCALAR` and
/// `MC_CHROMA_SCALAR` read 0 across the corpus -- so this is the fallback for a
/// shape they decline, not a path decode takes. Inlined, all four of these sat
/// inside `interp`, which IS the hot path.
#[cold]
#[inline(never)]
fn copy_shift_scalar(src: &[u16], stride: usize, w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    for y in 0..h {
        let row = &src[y * stride..y * stride + w];
        let out = &mut dst[y * dst_stride..y * dst_stride + w];
        for (o, &s) in out.iter_mut().zip(row) {
            *o = ((s as i32) << shift) as i16;
        }
    }
}

// ---------------------------------------------------------------------------
// x86-64: SSE2 is baseline, AVX2 is detected once
// ---------------------------------------------------------------------------

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod x86 {
    use std::arch::x86_64::*;

    /// A shift that vanishes when the caller's shift is zero, so the
    /// specialised path emits no instruction at all.
    #[inline(always)]
    fn shr128<const NOSHIFT: bool>(v: __m128i, sh: __m128i) -> __m128i {
        if NOSHIFT {
            v
        } else {
            unsafe { _mm_sra_epi32(v, sh) }
        }
    }

    #[inline(always)]
    fn shr256<const NOSHIFT: bool>(v: __m256i, sh: __m128i) -> __m256i {
        if NOSHIFT {
            v
        } else {
            unsafe { _mm256_sra_epi32(v, sh) }
        }
    }

    /// Two taps packed into the halves of an `i32`, broadcast — the multiplier
    /// for one `pmaddwd`. `t[2k]` lands in the low `i16` lane, `t[2k+1]` in the
    /// high one, matching the operand order `pmaddwd` pairs them in.
    #[inline]
    fn pair(t: &[i16], k: usize) -> i32 {
        (t[2 * k] as u16 as i32) | ((t[2 * k + 1] as i32) << 16)
    }

    /// Horizontal FIR, SSE2. 8 outputs per iteration.
    ///
    /// `pmaddwd` sums *adjacent* lanes, so one pass over tap-pairs yields the
    /// outputs at even offsets and a second pass, one sample to the right,
    /// yields the odd ones; `unpack` + `packs` interleaves them back into
    /// picture order.
    ///
    /// # Safety
    /// `src` must have `stride * (h - 1) + w + N - 1` readable `u16`, and
    /// `dst` `dst_stride * (h - 1) + w` writable `i16`.
    /// `shift1` is `BitDepth − 8` (§8.5.3.3.3.2), so it is **zero for 8-bit
    /// content** — the case that dominates. Specialising on that removes the
    /// two shifts from the inner loop rather than shifting by nothing, and the
    /// branch is outside both loops.
    #[target_feature(enable = "sse2")]
    pub unsafe fn fir_h_sse2<const N: usize>(src: *const u16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        if shift == 0 {
            return unsafe { fir_h_sse2_impl::<N, true>(src, stride, t, w, h, 0, dst, dst_stride) };
        }
        unsafe { fir_h_sse2_impl::<N, false>(src, stride, t, w, h, shift, dst, dst_stride) }
    }

    /// # Safety
    /// As [`fir_h_sse2`].
    #[target_feature(enable = "sse2")]
    unsafe fn fir_h_sse2_impl<const N: usize, const NOSHIFT: bool>(src: *const u16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let np = N / 2;
        let mut taps = [_mm_setzero_si128(); 4];
        for k in 0..np {
            taps[k] = _mm_set1_epi32(pair(t, k));
        }
        let sh = _mm_cvtsi32_si128(shift as i32);
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let mut even = _mm_setzero_si128();
                let mut odd = _mm_setzero_si128();
                for k in 0..np {
                    let base = unsafe { row.add(x + 2 * k) as *const __m128i };
                    even = _mm_add_epi32(even, _mm_madd_epi16(unsafe { _mm_loadu_si128(base) }, taps[k]));
                    let base_odd = unsafe { row.add(x + 2 * k + 1) as *const __m128i };
                    odd = _mm_add_epi32(odd, _mm_madd_epi16(unsafe { _mm_loadu_si128(base_odd) }, taps[k]));
                }
                if !NOSHIFT {
                    even = _mm_sra_epi32(even, sh);
                    odd = _mm_sra_epi32(odd, sh);
                }
                let lo = _mm_unpacklo_epi32(even, odd);
                let hi = _mm_unpackhi_epi32(even, odd);
                unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_packs_epi32(lo, hi)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *row.add(x + i) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
        }
    }

    /// Horizontal FIR, AVX2. 16 outputs per iteration; the interleave works
    /// out because `unpack` and `packs` are both per-128-bit-lane and the
    /// lanes stay in order.
    ///
    /// # Safety
    /// As [`fir_h_sse2`].
    /// `shift1` is `BitDepth − 8` (§8.5.3.3.3.2), so it is **zero for 8-bit
    /// content** — the case that dominates. Specialising on that removes the
    /// two shifts from the inner loop rather than shifting by nothing, and the
    /// branch is outside both loops.
    #[target_feature(enable = "avx2")]
    pub unsafe fn fir_h_avx2<const N: usize>(src: *const u16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        if shift == 0 {
            return unsafe { fir_h_avx2_impl::<N, true>(src, stride, t, w, h, 0, dst, dst_stride) };
        }
        unsafe { fir_h_avx2_impl::<N, false>(src, stride, t, w, h, shift, dst, dst_stride) }
    }

    /// # Safety
    /// As [`fir_h_sse2`].
    #[target_feature(enable = "avx2")]
    unsafe fn fir_h_avx2_impl<const N: usize, const NOSHIFT: bool>(src: *const u16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let np = N / 2;
        let mut taps = [_mm256_setzero_si256(); 4];
        for k in 0..np {
            taps[k] = _mm256_set1_epi32(pair(t, k));
        }
        let sh = _mm_cvtsi32_si128(shift as i32);
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip: the tap loop does the same work either
            // way, but the loop's own three instructions are then paid once
            // per 32 samples instead of once per 16.
            let nvec = w / 16;
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 32 + half * 16;
                    let mut even = _mm256_setzero_si256();
                    let mut odd = _mm256_setzero_si256();
                    for k in 0..np {
                        let base = unsafe { row.add(x + 2 * k) as *const __m256i };
                        even = _mm256_add_epi32(even, _mm256_madd_epi16(unsafe { _mm256_loadu_si256(base) }, taps[k]));
                        let base_odd = unsafe { row.add(x + 2 * k + 1) as *const __m256i };
                        odd = _mm256_add_epi32(odd, _mm256_madd_epi16(unsafe { _mm256_loadu_si256(base_odd) }, taps[k]));
                    }
                    if !NOSHIFT {
                        even = _mm256_sra_epi32(even, sh);
                        odd = _mm256_sra_epi32(odd, sh);
                    }
                    let lo = _mm256_unpacklo_epi32(even, odd);
                    let hi = _mm256_unpackhi_epi32(even, odd);
                    unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_packs_epi32(lo, hi)) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 16;
                let mut even = _mm256_setzero_si256();
                let mut odd = _mm256_setzero_si256();
                for k in 0..np {
                    let base = unsafe { row.add(x + 2 * k) as *const __m256i };
                    even = _mm256_add_epi32(even, _mm256_madd_epi16(unsafe { _mm256_loadu_si256(base) }, taps[k]));
                    let base_odd = unsafe { row.add(x + 2 * k + 1) as *const __m256i };
                    odd = _mm256_add_epi32(odd, _mm256_madd_epi16(unsafe { _mm256_loadu_si256(base_odd) }, taps[k]));
                }
                if !NOSHIFT {
                    even = _mm256_sra_epi32(even, sh);
                    odd = _mm256_sra_epi32(odd, sh);
                }
                let lo = _mm256_unpacklo_epi32(even, odd);
                let hi = _mm256_unpackhi_epi32(even, odd);
                unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_packs_epi32(lo, hi)) };
            }
            let x = nvec * 16;
            if x < w {
                unsafe { fir_h_sse2_tail(row, t, x, w, shift, out) };
            }
        }
    }

    #[target_feature(enable = "sse2")]
    unsafe fn fir_h_sse2_tail<const N: usize>(row: *const u16, t: &[i16; N], mut x: usize, w: usize, shift: u32, out: *mut i16) {
        // A 4-wide step. This tail was fully scalar and serves EVERY row
        // narrower than eight, so a 4-wide chroma block filtered its whole
        // horizontal pass one sample at a time.
        //
        // `pmaddwd` on a contiguous load contracts adjacent tap pairs, giving
        // the EVEN outputs; the same load offset by one gives the odd ones.
        // Interleaving the low halves yields four consecutive outputs.
        let shv = _mm_cvtsi32_si128(shift as i32);
        // The tap vectors are loop-invariant; building them per trip put
        // `N / 2` broadcasts inside the `x` loop.
        let mut tp = [_mm_setzero_si128(); 4];
        for (k, slot) in tp.iter_mut().enumerate().take(N / 2) {
            *slot = _mm_set1_epi32(pair(t, k));
        }
        // Four `i32` lanes half-fill a 128-bit store, so one group per trip paid
        // a `packs` and a half-width `storel` for every four samples. Two groups
        // fill the store exactly and pay that pair once for eight -- the same
        // reason `planar_sse2` pairs.
        let quad = |x: usize| {
            let mut even = _mm_setzero_si128();
            let mut odd = _mm_setzero_si128();
            for k in 0..N / 2 {
                even = _mm_add_epi32(even, _mm_madd_epi16(unsafe { _mm_loadu_si128(row.add(x + 2 * k) as *const __m128i) }, tp[k]));
                odd = _mm_add_epi32(odd, _mm_madd_epi16(unsafe { _mm_loadu_si128(row.add(x + 2 * k + 1) as *const __m128i) }, tp[k]));
            }
            _mm_unpacklo_epi32(_mm_sra_epi32(even, shv), _mm_sra_epi32(odd, shv))
        };
        while x + 8 <= w {
            let v0 = quad(x);
            let v1 = quad(x + 4);
            unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_packs_epi32(v0, v1)) };
            x += 8;
        }
        if x + 4 <= w {
            let v = quad(x);
            unsafe { _mm_storel_epi64(out.add(x) as *mut __m128i, _mm_packs_epi32(v, v)) };
            x += 4;
        }
        while x < w {
            let mut acc = 0i32;
            for i in 0..N {
                acc += t[i] as i32 * unsafe { *row.add(x + i) } as i32;
            }
            unsafe { *out.add(x) = (acc >> shift) as i16 };
            x += 1;
        }
    }

    /// Vertical FIR over `i16` rows, SSE2. 8 columns per iteration.
    ///
    /// Interleaving rows `i` and `i+1` with `unpack` puts the two samples a
    /// tap-pair needs into adjacent lanes, which is exactly what `pmaddwd`
    /// consumes — no shifted loads and no cross-lane work.
    ///
    /// # Safety
    /// `src` must have `stride * (h + N - 2) + w` readable `i16`; `dst`
    /// `dst_stride * (h - 1) + w` writable.
    /// The vertical-only path shifts by `shift1 = BitDepth − 8`, which is
    /// **zero for 8-bit content**; the two-dimensional path shifts by 6. One
    /// branch outside the loops serves both without shifting by nothing.
    #[target_feature(enable = "sse2")]
    pub unsafe fn fir_v_sse2<const N: usize>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        if shift == 0 {
            return unsafe { fir_v_sse2_impl::<N, true>(src, stride, t, w, h, 0, dst, dst_stride) };
        }
        unsafe { fir_v_sse2_impl::<N, false>(src, stride, t, w, h, shift, dst, dst_stride) }
    }

    /// # Safety
    /// As [`fir_v_sse2`].
    #[target_feature(enable = "sse2")]
    unsafe fn fir_v_sse2_impl<const N: usize, const NOSHIFT: bool>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let np = N / 2;
        let mut taps = [_mm_setzero_si128(); 4];
        for k in 0..np {
            taps[k] = _mm_set1_epi32(pair(t, k));
        }
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = w / 8;
        // Two output rows per pass, sharing their overlapping source rows —
        // `N + 1` loads for two outputs where one-at-a-time needs `2N`. See the
        // AVX2 twin for the full argument.
        let mut y = 0usize;
        while y + 2 <= h {
            let out0 = unsafe { dst.add(y * dst_stride) };
            let out1 = unsafe { dst.add((y + 1) * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let mut r = [_mm_setzero_si128(); 9];
                for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                    *slot = unsafe { _mm_loadu_si128(src.add((y + k) * stride + x) as *const __m128i) };
                }
                let mut lo0 = _mm_setzero_si128();
                let mut hi0 = _mm_setzero_si128();
                let mut lo1 = _mm_setzero_si128();
                let mut hi1 = _mm_setzero_si128();
                for k in 0..np {
                    let (a, b, c) = (r[2 * k], r[2 * k + 1], r[2 * k + 2]);
                    lo0 = _mm_add_epi32(lo0, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), taps[k]));
                    hi0 = _mm_add_epi32(hi0, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), taps[k]));
                    lo1 = _mm_add_epi32(lo1, _mm_madd_epi16(_mm_unpacklo_epi16(b, c), taps[k]));
                    hi1 = _mm_add_epi32(hi1, _mm_madd_epi16(_mm_unpackhi_epi16(b, c), taps[k]));
                }
                unsafe {
                    _mm_storeu_si128(out0.add(x) as *mut __m128i, _mm_packs_epi32(shr128::<NOSHIFT>(lo0, sh), shr128::<NOSHIFT>(hi0, sh)));
                    _mm_storeu_si128(out1.add(x) as *mut __m128i, _mm_packs_epi32(shr128::<NOSHIFT>(lo1, sh), shr128::<NOSHIFT>(hi1, sh)));
                }
            }
            // A 4-wide step before the scalar tail, both output rows at once.
            //
            // This kernel stepped 8 and fell straight to scalar below that, so
            // on the SSE rung a 4-wide chroma block -- routine in 4:2:0 --
            // filtered its whole vertical pass one sample at a time. The AVX2
            // twin gained this step during the narrow-block sweep; its SSE
            // counterpart did not.
            let mut x4 = nvec * 8;
            if x4 + 4 <= w {
                let mut r = [_mm_setzero_si128(); 9];
                for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                    *slot = unsafe { _mm_loadl_epi64(src.add((y + k) * stride + x4) as *const __m128i) };
                }
                let mut lo0 = _mm_setzero_si128();
                let mut lo1 = _mm_setzero_si128();
                for k in 0..np {
                    let tp = _mm_set1_epi32(pair(t, k));
                    lo0 = _mm_add_epi32(lo0, _mm_madd_epi16(_mm_unpacklo_epi16(r[2 * k], r[2 * k + 1]), tp));
                    lo1 = _mm_add_epi32(lo1, _mm_madd_epi16(_mm_unpacklo_epi16(r[2 * k + 1], r[2 * k + 2]), tp));
                }
                let (lo0, lo1) = if NOSHIFT { (lo0, lo1) } else { (_mm_sra_epi32(lo0, sh), _mm_sra_epi32(lo1, sh)) };
                unsafe {
                    _mm_storel_epi64(out0.add(x4) as *mut __m128i, _mm_packs_epi32(lo0, lo0));
                    _mm_storel_epi64(out1.add(x4) as *mut __m128i, _mm_packs_epi32(lo1, lo1));
                }
                x4 += 4;
            }
            for (o, yy) in [(out0, y), (out1, y + 1)] {
                let mut x = x4;
                while x < w {
                    let mut acc = 0i32;
                    for i in 0..N {
                        acc += t[i] as i32 * unsafe { *src.add((yy + i) * stride + x) } as i32;
                    }
                    unsafe { *o.add(x) = (acc >> shift) as i16 };
                    x += 1;
                }
            }
            y += 2;
        }
        while y < h {
            let out = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let mut lo_acc = _mm_setzero_si128();
                let mut hi_acc = _mm_setzero_si128();
                for k in 0..np {
                    let r0 = unsafe { _mm_loadu_si128(src.add((y + 2 * k) * stride + x) as *const __m128i) };
                    let r1 = unsafe { _mm_loadu_si128(src.add((y + 2 * k + 1) * stride + x) as *const __m128i) };
                    lo_acc = _mm_add_epi32(lo_acc, _mm_madd_epi16(_mm_unpacklo_epi16(r0, r1), taps[k]));
                    hi_acc = _mm_add_epi32(hi_acc, _mm_madd_epi16(_mm_unpackhi_epi16(r0, r1), taps[k]));
                }
                lo_acc = shr128::<NOSHIFT>(lo_acc, sh);
                hi_acc = shr128::<NOSHIFT>(hi_acc, sh);
                unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_packs_epi32(lo_acc, hi_acc)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *src.add((y + i) * stride + x) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
            y += 1;
        }
    }

    /// Vertical FIR, AVX2. 16 columns per iteration.
    ///
    /// # Safety
    /// As [`fir_v_sse2`].
    /// The vertical-only path shifts by `shift1 = BitDepth − 8`, which is
    /// **zero for 8-bit content**; the two-dimensional path shifts by 6. One
    /// branch outside the loops serves both without shifting by nothing.
    #[target_feature(enable = "avx2")]
    pub unsafe fn fir_v_avx2<const N: usize>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        if shift == 0 {
            return unsafe { fir_v_avx2_impl::<N, true>(src, stride, t, w, h, 0, dst, dst_stride) };
        }
        unsafe { fir_v_avx2_impl::<N, false>(src, stride, t, w, h, shift, dst, dst_stride) }
    }

    /// # Safety
    /// As [`fir_v_avx2`].
    #[target_feature(enable = "avx2")]
    unsafe fn fir_v_avx2_impl<const N: usize, const NOSHIFT: bool>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let np = N / 2;
        let mut taps = [_mm256_setzero_si256(); 4];
        for k in 0..np {
            taps[k] = _mm256_set1_epi32(pair(t, k));
        }
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = w / 16;
        // TWO output rows per pass.
        //
        // A vertical filter reads `N` source rows per output row, and the
        // windows for output rows `y` and `y + 1` overlap in `N − 1` of them.
        // Emitting one row at a time therefore loads every source row `N`
        // times over. Two at a time loads `N + 1` rows to produce two outputs
        // where before it took `2N`: nine loads instead of sixteen for the
        // 8-tap luma filter.
        //
        // Nothing else is shared, and nothing else is claimed — the tap PAIRS
        // differ between the rows (`(r0,r1) (r2,r3) …` against
        // `(r1,r2) (r3,r4) …`), so the unpacks and multiplies stay one per
        // output. Only the loads were redundant; only the loads go.
        let mut y = 0usize;
        while y + 2 <= h {
            let out0 = unsafe { dst.add(y * dst_stride) };
            let out1 = unsafe { dst.add((y + 1) * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let mut r = [_mm256_setzero_si256(); 9];
                for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                    *slot = unsafe { _mm256_loadu_si256(src.add((y + k) * stride + x) as *const __m256i) };
                }
                let mut lo0 = _mm256_setzero_si256();
                let mut hi0 = _mm256_setzero_si256();
                let mut lo1 = _mm256_setzero_si256();
                let mut hi1 = _mm256_setzero_si256();
                for k in 0..np {
                    let (a, b, c) = (r[2 * k], r[2 * k + 1], r[2 * k + 2]);
                    lo0 = _mm256_add_epi32(lo0, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), taps[k]));
                    hi0 = _mm256_add_epi32(hi0, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), taps[k]));
                    lo1 = _mm256_add_epi32(lo1, _mm256_madd_epi16(_mm256_unpacklo_epi16(b, c), taps[k]));
                    hi1 = _mm256_add_epi32(hi1, _mm256_madd_epi16(_mm256_unpackhi_epi16(b, c), taps[k]));
                }
                unsafe {
                    _mm256_storeu_si256(out0.add(x) as *mut __m256i, _mm256_packs_epi32(shr256::<NOSHIFT>(lo0, sh), shr256::<NOSHIFT>(hi0, sh)));
                    _mm256_storeu_si256(out1.add(x) as *mut __m256i, _mm256_packs_epi32(shr256::<NOSHIFT>(lo1, sh), shr256::<NOSHIFT>(hi1, sh)));
                }
            }
            // The 8-wide step, for BOTH output rows at once.
            //
            // It used to sit inside the per-row loop below, which meant each
            // output row loaded all `N` source rows for itself -- sixteen loads
            // to produce two rows where the 16-wide loop above needs nine. The
            // windows for rows `y` and `y + 1` overlap in `N - 1` rows here for
            // exactly the same reason they do there; only the register width
            // differs. The tap vectors are hoisted for the same reason.
            let mut x8 = nvec * 16;
            if x8 + 8 <= w {
                let mut tp = [_mm_setzero_si128(); 4];
                for (k, slot) in tp.iter_mut().enumerate().take(np) {
                    *slot = _mm_set1_epi32(pair(t, k));
                }
                while x8 + 8 <= w {
                    let mut r = [_mm_setzero_si128(); 9];
                    for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                        *slot = unsafe { _mm_loadu_si128(src.add((y + k) * stride + x8) as *const __m128i) };
                    }
                    let mut lo0 = _mm_setzero_si128();
                    let mut hi0 = _mm_setzero_si128();
                    let mut lo1 = _mm_setzero_si128();
                    let mut hi1 = _mm_setzero_si128();
                    for k in 0..np {
                        let (a, b, c) = (r[2 * k], r[2 * k + 1], r[2 * k + 2]);
                        lo0 = _mm_add_epi32(lo0, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), tp[k]));
                        hi0 = _mm_add_epi32(hi0, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), tp[k]));
                        lo1 = _mm_add_epi32(lo1, _mm_madd_epi16(_mm_unpacklo_epi16(b, c), tp[k]));
                        hi1 = _mm_add_epi32(hi1, _mm_madd_epi16(_mm_unpackhi_epi16(b, c), tp[k]));
                    }
                    let (lo0, hi0) = if NOSHIFT { (lo0, hi0) } else { (_mm_sra_epi32(lo0, sh), _mm_sra_epi32(hi0, sh)) };
                    let (lo1, hi1) = if NOSHIFT { (lo1, hi1) } else { (_mm_sra_epi32(lo1, sh), _mm_sra_epi32(hi1, sh)) };
                    unsafe {
                        _mm_storeu_si128(out0.add(x8) as *mut __m128i, _mm_packs_epi32(lo0, hi0));
                        _mm_storeu_si128(out1.add(x8) as *mut __m128i, _mm_packs_epi32(lo1, hi1));
                    }
                    x8 += 8;
                }
            }
            // The 4-wide step, both rows at once, for the same reason.
            let mut x4 = x8;
            if x4 + 4 <= w {
                let mut r = [_mm_setzero_si128(); 9];
                for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                    *slot = unsafe { _mm_loadl_epi64(src.add((y + k) * stride + x4) as *const __m128i) };
                }
                let mut lo0 = _mm_setzero_si128();
                let mut lo1 = _mm_setzero_si128();
                for k in 0..np {
                    let tp = _mm_set1_epi32(pair(t, k));
                    lo0 = _mm_add_epi32(lo0, _mm_madd_epi16(_mm_unpacklo_epi16(r[2 * k], r[2 * k + 1]), tp));
                    lo1 = _mm_add_epi32(lo1, _mm_madd_epi16(_mm_unpacklo_epi16(r[2 * k + 1], r[2 * k + 2]), tp));
                }
                let (lo0, lo1) = if NOSHIFT { (lo0, lo1) } else { (_mm_sra_epi32(lo0, sh), _mm_sra_epi32(lo1, sh)) };
                unsafe {
                    _mm_storel_epi64(out0.add(x4) as *mut __m128i, _mm_packs_epi32(lo0, lo0));
                    _mm_storel_epi64(out1.add(x4) as *mut __m128i, _mm_packs_epi32(lo1, lo1));
                }
                x4 += 4;
            }
            for (o, yy) in [(out0, y), (out1, y + 1)] {
                let mut x = x4;
                while x < w {
                    let mut acc = 0i32;
                    for i in 0..N {
                        acc += t[i] as i32 * unsafe { *src.add((yy + i) * stride + x) } as i32;
                    }
                    unsafe { *o.add(x) = (acc >> shift) as i16 };
                    x += 1;
                }
            }
            y += 2;
        }
        // Odd height: the last row on its own.
        while y < h {
            let out = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let mut lo_acc = _mm256_setzero_si256();
                let mut hi_acc = _mm256_setzero_si256();
                for k in 0..np {
                    let r0 = unsafe { _mm256_loadu_si256(src.add((y + 2 * k) * stride + x) as *const __m256i) };
                    let r1 = unsafe { _mm256_loadu_si256(src.add((y + 2 * k + 1) * stride + x) as *const __m256i) };
                    lo_acc = _mm256_add_epi32(lo_acc, _mm256_madd_epi16(_mm256_unpacklo_epi16(r0, r1), taps[k]));
                    hi_acc = _mm256_add_epi32(hi_acc, _mm256_madd_epi16(_mm256_unpackhi_epi16(r0, r1), taps[k]));
                }
                lo_acc = shr256::<NOSHIFT>(lo_acc, sh);
                hi_acc = shr256::<NOSHIFT>(hi_acc, sh);
                // `unpack` and `packs` are per-128-bit-lane, and the unpack
                // above split each 256-bit row the same way, so the halves
                // land back in picture order without a permute.
                unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_packs_epi32(lo_acc, hi_acc)) };
            }
            let mut x = nvec * 16;
            // The odd-row path needs the same steps as the two-row body above.
            while x + 8 <= w {
                let mut lo = _mm_setzero_si128();
                let mut hi = _mm_setzero_si128();
                for k in 0..np {
                    let r0 = unsafe { _mm_loadu_si128(src.add((y + 2 * k) * stride + x) as *const __m128i) };
                    let r1 = unsafe { _mm_loadu_si128(src.add((y + 2 * k + 1) * stride + x) as *const __m128i) };
                    let tp = _mm_set1_epi32(pair(t, k));
                    lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(r0, r1), tp));
                    hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(r0, r1), tp));
                }
                let (lo, hi) = if NOSHIFT { (lo, hi) } else { (_mm_sra_epi32(lo, sh), _mm_sra_epi32(hi, sh)) };
                unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_packs_epi32(lo, hi)) };
                x += 8;
            }
            if x + 4 <= w {
                let mut lo = _mm_setzero_si128();
                for k in 0..np {
                    let r0 = unsafe { _mm_loadl_epi64(src.add((y + 2 * k) * stride + x) as *const __m128i) };
                    let r1 = unsafe { _mm_loadl_epi64(src.add((y + 2 * k + 1) * stride + x) as *const __m128i) };
                    lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(r0, r1), _mm_set1_epi32(pair(t, k))));
                }
                let lo = if NOSHIFT { lo } else { _mm_sra_epi32(lo, sh) };
                unsafe { _mm_storel_epi64(out.add(x) as *mut __m128i, _mm_packs_epi32(lo, lo)) };
                x += 4;
            }
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *src.add((y + i) * stride + x) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
            y += 1;
        }
    }

    /// Vertical FIR with the **symmetric taps folded before the multiply**.
    ///
    /// Two of HEVC's filters are palindromes:
    ///
    /// ```text
    ///   luma   f2 = [-1, 4, -11, 40, 40, -11, 4, -1]
    ///   chroma fc4 = [-4, 36, 36, -4]
    /// ```
    ///
    /// so `sum t[k]*x[k]` collapses to `sum_{k < N/2} t[k]*(x[k] + x[N-1-k])`.
    /// For a VERTICAL filter the mirrored operands are whole ROWS, so
    /// `x[k] + x[N-1-k]` is one `paddw` -- no shuffle, no realignment. Half as
    /// many values then enter the multiply, and because `pmaddwd` consumes an
    /// unpacked PAIR, the unpacks halve with them. Per two output rows, per
    /// vector:
    ///
    /// | | general | folded |
    /// |---|---:|---:|
    /// | loads | 9 | 9 |
    /// | `paddw` | 0 | 8 |
    /// | `punpck` | 16 | 8 |
    /// | `pmaddwd` | 16 | 8 |
    /// | `paddd` | 16 | 4 |
    /// | **ALU total** | **48** | **28** |
    ///
    /// The loads are unchanged: the two-row structure already shares them, and
    /// folding does not disturb that. Only arithmetic goes.
    ///
    /// ## Why this is the vertical kernel only, and only on pixels
    ///
    /// The fold happens in `i16`, so `x[k] + x[N-1-k]` must not overflow. Two
    /// separate limits, and only one of them is about bit depth:
    ///
    ///   * **Pixels are fine.** Two samples sum to `2*(2^BitDepth - 1)`, which
    ///     stays inside `i16` for any depth up to 14.
    ///   * **The 2-D path's intermediates are NOT.** After the horizontal pass
    ///     an 8-bit intermediate reaches `255*(4+40+40+4) = 22440`; two of those
    ///     sum to **44880**, well past `i16`. HEVC sized that intermediate to
    ///     fill the type, so there is no headroom to borrow. This is why the
    ///     kernel is reached only from the pure-vertical `(0, fy)` arm, where
    ///     the source is pixels -- the 2-D vertical pass keeps the general form.
    ///
    /// Contracting the mirrored pair with `pmaddwd` instead (taps `(t,t)` on an
    /// unpacked `(x[k], x[N-1-k])`) would dodge the overflow, but it saves
    /// nothing: `pmaddwd` already contracts two lanes, so the op count is
    /// exactly the general kernel's. The saving comes from the `paddw`, and the
    /// `paddw` is what needs the headroom.
    ///
    /// # Safety
    /// As [`fir_v_avx2`]; `t` must be a palindrome and `src` must hold samples
    /// of at most 14 bits.
    #[target_feature(enable = "avx2")]
    pub unsafe fn fir_v_avx2_sym<const N: usize>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        debug_assert!((0..N / 2).all(|k| t[k] == t[N - 1 - k]), "kernel requires palindromic taps");
        let ng = N / 4; // folded tap pairs: 2 for luma, 1 for chroma
        let nh = N / 2; // folded row sums per output
        let mut taps = [_mm256_setzero_si256(); 2];
        for (g, slot) in taps.iter_mut().enumerate().take(ng) {
            *slot = _mm256_set1_epi32(pair(t, g));
        }
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = w / 16;
        let mut y = 0usize;
        while y + 2 <= h {
            let out0 = unsafe { dst.add(y * dst_stride) };
            let out1 = unsafe { dst.add((y + 1) * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let mut r = [_mm256_setzero_si256(); 9];
                for (k, slot) in r.iter_mut().enumerate().take(N + 1) {
                    *slot = unsafe { _mm256_loadu_si256(src.add((y + k) * stride + x) as *const __m256i) };
                }
                // Row y folds r[k] with r[N-1-k]; row y+1 is the same window
                // slid by one, so it folds r[k+1] with r[N-k].
                let mut s0 = [_mm256_setzero_si256(); 4];
                let mut s1 = [_mm256_setzero_si256(); 4];
                for k in 0..nh {
                    s0[k] = _mm256_add_epi16(r[k], r[N - 1 - k]);
                    s1[k] = _mm256_add_epi16(r[k + 1], r[N - k]);
                }
                let (mut lo0, mut hi0) = (_mm256_setzero_si256(), _mm256_setzero_si256());
                let (mut lo1, mut hi1) = (_mm256_setzero_si256(), _mm256_setzero_si256());
                for g in 0..ng {
                    let (p0, q0) = (s0[2 * g], s0[2 * g + 1]);
                    let (p1, q1) = (s1[2 * g], s1[2 * g + 1]);
                    lo0 = _mm256_add_epi32(lo0, _mm256_madd_epi16(_mm256_unpacklo_epi16(p0, q0), taps[g]));
                    hi0 = _mm256_add_epi32(hi0, _mm256_madd_epi16(_mm256_unpackhi_epi16(p0, q0), taps[g]));
                    lo1 = _mm256_add_epi32(lo1, _mm256_madd_epi16(_mm256_unpacklo_epi16(p1, q1), taps[g]));
                    hi1 = _mm256_add_epi32(hi1, _mm256_madd_epi16(_mm256_unpackhi_epi16(p1, q1), taps[g]));
                }
                // `unpack` and `packs` are both per-128-bit-lane, so the halves
                // land back in picture order with no permute -- the same
                // property the general kernel relies on.
                unsafe {
                    _mm256_storeu_si256(out0.add(x) as *mut __m256i, _mm256_packs_epi32(shr256_dyn(lo0, sh, shift), shr256_dyn(hi0, sh, shift)));
                    _mm256_storeu_si256(out1.add(x) as *mut __m256i, _mm256_packs_epi32(shr256_dyn(lo1, sh, shift), shr256_dyn(hi1, sh, shift)));
                }
            }
            for (o, yy) in [(out0, y), (out1, y + 1)] {
                let mut x = nvec * 16;
                while x < w {
                    let mut acc = 0i32;
                    for i in 0..N {
                        acc += t[i] as i32 * unsafe { *src.add((yy + i) * stride + x) } as i32;
                    }
                    unsafe { *o.add(x) = (acc >> shift) as i16 };
                    x += 1;
                }
            }
            y += 2;
        }
        while y < h {
            let out = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let mut s = [_mm256_setzero_si256(); 4];
                for (k, slot) in s.iter_mut().enumerate().take(nh) {
                    let a = unsafe { _mm256_loadu_si256(src.add((y + k) * stride + x) as *const __m256i) };
                    let b = unsafe { _mm256_loadu_si256(src.add((y + N - 1 - k) * stride + x) as *const __m256i) };
                    *slot = _mm256_add_epi16(a, b);
                }
                let (mut lo, mut hi) = (_mm256_setzero_si256(), _mm256_setzero_si256());
                for g in 0..ng {
                    let (p, q) = (s[2 * g], s[2 * g + 1]);
                    lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(p, q), taps[g]));
                    hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(p, q), taps[g]));
                }
                unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_packs_epi32(shr256_dyn(lo, sh, shift), shr256_dyn(hi, sh, shift))) };
            }
            let mut x = nvec * 16;
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *src.add((y + i) * stride + x) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
            y += 1;
        }
    }

    /// `shift` is a runtime value here (the caller does not specialize it), so
    /// the zero case is a branch rather than a const-generic.
    #[inline(always)]
    fn shr256_dyn(v: __m256i, sh: __m128i, shift: u32) -> __m256i {
        if shift == 0 {
            v
        } else {
            unsafe { _mm256_sra_epi32(v, sh) }
        }
    }

    /// Full-pel copy: widen `u16` samples into the 14-bit `i16` intermediate.
    ///
    /// This is the `(0, 0)` fractional position — an INTEGER motion vector —
    /// and it had no kernel at all: both dispatchers called the scalar twin, so
    /// every integer-MV block went one sample at a time. That is not a rare
    /// path; a fast encoder preset can make it the majority of prediction.
    ///
    /// The shift cannot overflow: `shift3 = 14 − BitDepth`, so the result is
    /// the 14-bit intermediate by construction (`255 << 6` and `1023 << 4` both
    /// sit inside `i16`).
    ///
    /// # Safety
    /// `src` must have `stride * (h - 1) + w` readable `u16`, `dst`
    /// `dst_stride * (h - 1) + w` writable `i16`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn copy_shift_sse2(src: *const u16, stride: usize, w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = w / 8;
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let v = unsafe { _mm_loadu_si128(row.add(x) as *const __m128i) };
                    unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_sll_epi16(v, sh)) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let v = unsafe { _mm_loadu_si128(row.add(x) as *const __m128i) };
                unsafe { _mm_storeu_si128(out.add(x) as *mut __m128i, _mm_sll_epi16(v, sh)) };
            }
            let mut x = nvec * 8;
            // A 4-wide step: the full-pel copy of a 4-wide chroma block went
            // one sample at a time.
            if x + 4 <= w {
                let v = unsafe { _mm_loadl_epi64(row.add(x) as *const __m128i) };
                unsafe { _mm_storel_epi64(out.add(x) as *mut __m128i, _mm_sll_epi16(v, sh)) };
                x += 4;
            }
            while x < w {
                unsafe { *out.add(x) = ((*row.add(x) as i32) << shift) as i16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`copy_shift_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn copy_shift_avx2(src: *const u16, stride: usize, w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = w / 16;
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 32 + half * 16;
                    let v = unsafe { _mm256_loadu_si256(row.add(x) as *const __m256i) };
                    unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_sll_epi16(v, sh)) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 16;
                let v = unsafe { _mm256_loadu_si256(row.add(x) as *const __m256i) };
                unsafe { _mm256_storeu_si256(out.add(x) as *mut __m256i, _mm256_sll_epi16(v, sh)) };
            }
        }
        // The width remainder, ONCE for the whole block.
        //
        // Inside the row loop with `h = 1` this made one call per row, each
        // re-deriving the kernel's constants. The remaining columns are the
        // same for every row and the callee already loops over `y`.
        //
        // This is only sound because `copy_shift_sse2` takes BOTH strides
        // explicitly. Its neighbours in `pixel.rs` -- `put_uni_sse2` and
        // friends -- derive the source stride from `w` (`src.add(y * w)`), so
        // the same hoist there would silently stride a narrowed source by the
        // REMAINDER width instead of the real one. Left alone for that reason.
        let x = nvec * 16;
        if x < w {
            // SAFETY: the remaining columns of every row are in bounds.
            unsafe { copy_shift_sse2(src.add(x), stride, w - x, h, shift, dst.add(x), dst_stride) };
        }
    }
}

// ---------------------------------------------------------------------------
// aarch64: NEON is baseline
// ---------------------------------------------------------------------------

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod arm {
    use std::arch::aarch64::*;

    /// Horizontal FIR, NEON. 8 outputs per iteration, accumulating in `i32`.
    ///
    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn fir_h_neon<const N: usize>(src: *const u16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let mut lo = vdupq_n_s32(0);
                let mut hi = vdupq_n_s32(0);
                for i in 0..N {
                    let s = unsafe { vreinterpretq_s16_u16(vld1q_u16(row.add(x + i))) };
                    let tv = vdupq_n_s16(t[i]);
                    lo = vmlal_s16(lo, vget_low_s16(s), vget_low_s16(tv));
                    hi = vmlal_high_s16(hi, s, tv);
                }
                let sh = vdupq_n_s32(-(shift as i32));
                let lo = vshlq_s32(lo, sh);
                let hi = vshlq_s32(hi, sh);
                let packed = vcombine_s16(vqmovn_s32(lo), vqmovn_s32(hi));
                unsafe { vst1q_s16(out.add(x), packed) };
            }
            let mut x = nvec * 8;
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *row.add(x + i) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
        }
    }

    /// Vertical FIR over `i16` rows, NEON.
    ///
    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn fir_v_neon<const N: usize>(src: *const i16, stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        for y in 0..h {
            let out = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let mut lo = vdupq_n_s32(0);
                let mut hi = vdupq_n_s32(0);
                for i in 0..N {
                    let s = unsafe { vld1q_s16(src.add((y + i) * stride + x)) };
                    let tv = vdupq_n_s16(t[i]);
                    lo = vmlal_s16(lo, vget_low_s16(s), vget_low_s16(tv));
                    hi = vmlal_high_s16(hi, s, tv);
                }
                let sh = vdupq_n_s32(-(shift as i32));
                let packed = vcombine_s16(vqmovn_s32(vshlq_s32(lo, sh)), vqmovn_s32(vshlq_s32(hi, sh)));
                unsafe { vst1q_s16(out.add(x), packed) };
            }
            let mut x = nvec * 8;
            while x < w {
                let mut acc = 0i32;
                for i in 0..N {
                    acc += t[i] as i32 * unsafe { *src.add((y + i) * stride + x) } as i32;
                }
                unsafe { *out.add(x) = (acc >> shift) as i16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn copy_shift_neon(src: *const u16, stride: usize, w: usize, h: usize, shift: u32, dst: *mut i16, dst_stride: usize) {
        let sh = vdupq_n_s16(shift as i16);
        let nvec = w / 8;
        for y in 0..h {
            let row = unsafe { src.add(y * stride) };
            let out = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let v = unsafe { vreinterpretq_s16_u16(vld1q_u16(row.add(x))) };
                unsafe { vst1q_s16(out.add(x), vshlq_s16(v, sh)) };
            }
            let mut x = nvec * 8;
            while x < w {
                unsafe { *out.add(x) = ((*row.add(x) as i32) << shift) as i16 };
                x += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatchers — safe, bounds proved here, loop inside the feature boundary
// ---------------------------------------------------------------------------

/// Full-pel copy, with runtime dispatch. See [`x86::copy_shift_sse2`].
#[allow(clippy::too_many_arguments)]
fn copy_shift(p: McPlan, src: &[u16], stride: usize, w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    let ok = w > 0 && h > 0 && src.len() >= stride * (h - 1) + w && dst.len() >= dst_stride * (h - 1) + w;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && p.isa != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::MC_COPY_SIMD } else { &census::MC_COPY_SCALAR }, 1);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: lengths checked above.
        match p.isa {
            crate::Isa::Avx2 => return unsafe { x86::copy_shift_avx2(src.as_ptr(), stride, w, h, shift, dst.as_mut_ptr(), dst_stride) },
            _ => return unsafe { x86::copy_shift_sse2(src.as_ptr(), stride, w, h, shift, dst.as_mut_ptr(), dst_stride) },
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    if ok {
        // SAFETY: as above.
        return unsafe { arm::copy_shift_neon(src.as_ptr(), stride, w, h, shift, dst.as_mut_ptr(), dst_stride) };
    }
    copy_shift_scalar(src, stride, w, h, shift, dst, dst_stride);
}

#[inline]
fn fir_h<const N: usize>(p: McPlan, src: &[u16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    debug_assert!(src.len() >= stride * (h - 1) + w + N - 1);
    debug_assert!(dst.len() >= dst_stride * (h - 1) + w);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        if src.len() >= stride * (h - 1) + w + N - 1 && dst.len() >= dst_stride * (h - 1) + w {
            // SAFETY: the lengths the kernels read and write are the ones
            // asserted immediately above.
            match p.isa {
                crate::Isa::Avx2 => return unsafe { x86::fir_h_avx2::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
                _ => return unsafe { x86::fir_h_sse2::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
            }
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    {
        if src.len() >= stride * (h - 1) + w + N - 1 && dst.len() >= dst_stride * (h - 1) + w {
            // SAFETY: as above.
            return unsafe { arm::fir_h_neon::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) };
        }
    }
    fir_h_scalar::<N>(src, stride, t, w, h, shift, dst, dst_stride);
}

/// Reinterprets sample data as `i16`. Every HEVC sample is below `2^14`, so
/// the bit pattern is the same non-negative integer in both types.
#[cfg(feature = "simd")]
#[inline]
fn as_i16(s: &[u16]) -> &[i16] {
    // SAFETY: `u16` and `i16` have identical size and alignment, and the
    // lifetime and length are carried over unchanged.
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<i16>(), s.len()) }
}

/// Vertical FIR straight off the reference plane (the `fx == 0` case).
#[inline]
/// True when the tap set is a palindrome, so the mirrored operands can be
/// folded with a `paddw` before the multiply. Two of HEVC's filters are:
/// luma `f2` and chroma `fC4`.
///
/// Used to build [`LUMA_SYM`] / [`CHROMA_SYM`] and to pin them in tests; the
/// dispatch reads the table, because scanning the taps on every call is `N/2`
/// comparisons for an answer that is a property of a compile-time constant.
#[cfg(test)]
fn palindromic<const N: usize>(t: &[i16; N]) -> bool {
    (0..N / 2).all(|k| t[k] == t[N - 1 - k])
}

/// Which filters are palindromic, by fractional position.
///
/// Pinned against the filters by `sym_tables_match_the_filters`.
pub static LUMA_SYM: [bool; 4] = [false, false, true, false];
pub static CHROMA_SYM: [bool; 8] = [false, false, false, false, true, false, false, false];

/// The vector arm these kernels use, and whether the symmetric fold is on --
/// one cached answer instead of an `isa()` probe and a `OnceLock` read on every
/// call, on every dispatcher.
///
/// `Isa` is `Ord`, so callers ask `>= Isa::Sse41` or `== Isa::Avx2` against this
/// rather than re-probing. `RH265_SCALAR_GATE` and `RH265_NO_SYM_FOLD` are
/// folded in here for the same reason.
#[derive(Clone, Copy)]
struct McPlan {
    isa: crate::Isa,
    sym: bool,
    /// `RH265_NO_TAP_ELIDE=1` forces the full filter window. It lives here
    /// rather than in its own `OnceLock` so the 2-D arm answers it with the
    /// same read that picks the kernels.
    elide: bool,
}

#[inline(always)]
fn plan() -> McPlan {
    // VENDOR(uefi-player): no_std => env knobs read as unset (`is_none()` is
    // true), and `isa()` still reports via the fallback detector (Scalar when
    // `simd` is off). Same values as a host run with a clean environment.
    #[cfg(feature = "std")]
    {
        use std::sync::OnceLock;
        static P: OnceLock<McPlan> = OnceLock::new();
        *P.get_or_init(|| McPlan {
            isa: crate::isa(),
            sym: std::env::var_os("RH265_NO_SYM_FOLD").is_none(),
            elide: std::env::var_os("RH265_NO_TAP_ELIDE").is_none(),
        })
    }
    #[cfg(not(feature = "std"))]
    {
        McPlan {
            isa: crate::isa(),
            sym: true,
            elide: true,
        }
    }
}

/// Pure-vertical FIR over PIXELS, taking the folded kernel when the taps are
/// symmetric.
///
/// The `bit_depth <= 14` guard is what makes the fold sound: it bounds two
/// summed samples by `2 * (2^14 - 1) = 32766`, inside `i16`. It is a real
/// runtime condition, not a formality -- the identical fold is unavailable to
/// the 2-D path precisely because its operands are intermediates that already
/// fill the type.
fn fir_v_u16_sym<const N: usize>(p: McPlan, src: &[u16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, bit_depth: u8, sym: bool, dst: &mut [i16], dst_stride: usize) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        // `sym` is the caller's table lookup, not a scan of `t`, and the bounds
        // expression is formed once rather than twice (it used to be computed
        // for a `debug_assert` and again for the guard).
        let fits = src.len() >= stride * (h + N - 2) + w && dst.len() >= dst_stride * (h - 1) + w;
        if sym && p.sym && p.isa == crate::Isa::Avx2 && fits && bit_depth <= 14 {
            // SAFETY: bounds checked just above; taps verified palindromic by
            // the table and the samples verified narrow enough for the i16 fold.
            return unsafe { x86::fir_v_avx2_sym::<N>(as_i16(src).as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) };
        }
        // Straight to the general kernel: falling through `fir_v_u16` would
        // probe the ISA a second time for a question already answered.
        if fits {
            // SAFETY: as above.
            match p.isa {
                crate::Isa::Avx2 => return unsafe { x86::fir_v_avx2::<N>(as_i16(src).as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
                crate::Isa::Scalar => {}
                _ => return unsafe { x86::fir_v_sse2::<N>(as_i16(src).as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
            }
        }
    }
    let _ = (bit_depth, sym);
    fir_v_u16::<N>(p, src, stride, t, w, h, shift, dst, dst_stride);
}

fn fir_v_u16<const N: usize>(p: McPlan, src: &[u16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    #[cfg(feature = "simd")]
    {
        if p.isa != crate::Isa::Scalar {
            return fir_v::<N>(p, as_i16(src), stride, t, w, h, shift, dst, dst_stride);
        }
    }
    let _ = p;
    fir_v_u16_scalar::<N>(src, stride, t, w, h, shift, dst, dst_stride);
}

#[inline]
fn fir_v<const N: usize>(p: McPlan, src: &[i16], stride: usize, t: &[i16; N], w: usize, h: usize, shift: u32, dst: &mut [i16], dst_stride: usize) {
    debug_assert!(src.len() >= stride * (h + N - 2) + w);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        if src.len() >= stride * (h + N - 2) + w && dst.len() >= dst_stride * (h - 1) + w {
            // SAFETY: the kernel reads `h + N - 1` rows of `w` and writes `h`.
            match p.isa {
                crate::Isa::Avx2 => return unsafe { x86::fir_v_avx2::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
                _ => return unsafe { x86::fir_v_sse2::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) },
            }
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    {
        if src.len() >= stride * (h + N - 2) + w && dst.len() >= dst_stride * (h - 1) + w {
            // SAFETY: as above.
            return unsafe { arm::fir_v_neon::<N>(src.as_ptr(), stride, t, w, h, shift, dst.as_mut_ptr(), dst_stride) };
        }
    }
    fir_v_scalar::<N>(src, stride, t, w, h, shift, dst, dst_stride);
}

/// One prediction block, `N`-tap, from an in-bounds footprint.
///
/// `src` is the top-left of the footprint (`margin(N)` samples up and left of
/// the block). `dst` receives the 14-bit intermediate at stride `w`.
/// Bring-up switch for the zero-end-tap row elision (`RH265_NO_TAP_ELIDE=1`
/// restores the full `h + N - 1` horizontal pass). The arms are bit-identical,
/// so this exists only to price the removed work.
/// The full-pel arm of [`interp`], kept out of line -- see the call site.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn full_pel(p: McPlan, src: &[u16], stride: usize, m: usize, w: usize, h: usize, bit_depth: u8, dst: &mut [i16]) {
    let shift3 = (14u32).saturating_sub(bit_depth as u32).max(2);
    copy_shift(p, &src[m * stride + m..], stride, w, h, shift3, dst, w);
}

#[allow(clippy::too_many_arguments)]
fn interp<const N: usize, const NF: usize>(
    src: &[u16],
    stride: usize,
    fx: usize,
    fy: usize,
    w: usize,
    h: usize,
    bit_depth: u8,
    taps: &[[i16; N]; NF],
    spans: &[(usize, usize); NF],
    syms: &[bool; NF],
    dst: &mut [i16],
    tmp: &mut [i16],
) {
    // `NF` is the number of fractional positions -- 4 for luma, 8 for chroma,
    // both powers of two -- and the caller derives `fx`/`fy` as `mv & (NF - 1)`.
    // The masks below therefore never change a value; they are what lets each
    // table index carry its own proof, so the four lookups stop emitting a
    // compare and a branch to a panic block apiece on every call.
    let fx = fx & (NF - 1);
    let fy = fy & (NF - 1);
    let m = margin(N);
    let shift1 = (bit_depth as u32).saturating_sub(8).min(4);
    // ONE plan read for the whole call, threaded to every filter below.
    //
    // It was three on the 2-D arm -- `elide_taps()` here, then `plan()` again
    // inside `fir_h` and once more inside `fir_v` -- each a `OnceLock` probe on
    // a function that runs a quarter of a million times a clip, 70% of them
    // down that arm. The bring-up switch `elide_taps` used to own now rides in
    // `McPlan`, so the read that picks the kernels also answers it.
    let p = plan();
    // ONE census probe likewise; it was four on the 2-D path.
    let cen = census::ALWAYS;
    if cen {
        // Width class of the FILTER call, weighted by samples. `fir_v` steps 16
        // and falls to a scalar tail below that; chroma blocks are routinely 4
        // wide.
        census::bump(
            if w < 8 {
                &census::MC_FW_LT8
            } else if w < 16 {
                &census::MC_FW_8
            } else {
                &census::MC_FW_GE16
            },
            (w * h) as u64,
        );
    }
    if cen {
        // Route: the fractional position the bitstream asked for. Four arms,
        // and the content picks — a stream from a full-pel-only encoder takes
        // the first on every block, one from a slow preset almost never does.
        census::arm(match (fx, fy) {
            (0, 0) => &census::RT_MC_FULLPEL,
            (_, 0) => &census::RT_MC_HORIZ,
            (0, _) => &census::RT_MC_VERT,
            _ => &census::RT_MC_2D,
        });
        census::route(shift1 == 0, &census::RT_MC_SHIFT0, &census::RT_MC_SHIFTN);
        // The folded vertical kernel is reachable only from the pure-vertical
        // arm, and only for the one palindromic filter -- so the population is
        // narrow by construction and the counter is what prices it.
        if fx == 0 && fy != 0 {
            census::route(p.sym && syms[fy], &census::RT_MC_VSYM, &census::RT_MC_VGEN);
        }
    }
    match (fx, fy) {
        // Full-pel, out of line and `#[cold]`.
        //
        // `motion_compensate` resolves integer motion vectors before it ever
        // calls here -- see its `fp` scan -- so this arm takes 610 of the
        // 252,001 interpolation calls on a 720p clip, 0.2%. Inlined, its
        // `copy_shift` dispatch and `shift3` sat in the middle of the path the
        // other 99.8% take. `shift3` belongs to this arm alone in any case.
        (0, 0) => full_pel(p, src, stride, m, w, h, bit_depth, dst),
        (fx, 0) => fir_h::<N>(p, &src[m * stride..], stride, &taps[fx], w, h, shift1, dst, w),
        (0, fy) => fir_v_u16_sym::<N>(p, &src[m..], stride, &taps[fy], w, h, shift1, bit_depth, syms[fy], dst, w),
        #[allow(clippy::identity_op)]
        (fx, fy) => {
            // Zero end taps: don't filter a row that gets multiplied by nothing.
            //
            // The 2-D path filters `h + N - 1` rows horizontally so the vertical
            // pass has its full window. But two of the three luma filters end in
            // an exact zero --
            //
            //   f1 = [-1, 4, -10, 58, 17, -5,  1,  0]
            //   f3 = [ 0, 1,  -5, 17, 58, -10, 4, -1]
            //
            // -- so for those the window is really N-1 wide and one whole row of
            // horizontal filtering is computed, stored, reloaded and multiplied
            // by zero. Trimming it is bit-exact by construction: the arithmetic
            // that disappears is `x * 0`.
            //
            // `lo`/`hi` bracket the nonzero taps, so output rows 0..h need source
            // rows `lo ..= h-1+hi` -- `h + hi - lo` of them instead of `h + N - 1`.
            // Starting the horizontal pass at row `lo` means row `j+i` of the
            // scratch is source row `j+i+lo`, so the taps shift down by `lo` to
            // stay aligned; the vacated high taps are zero, which is what makes
            // the vertical pass's last (now ungenerated) row read harmless -- it
            // is multiplied by that zero. `tmp` is still sized for the full
            // `h + N - 1` rows, so the read is in bounds, just stale.
            //
            // This is the 63-73% arm of motion compensation, and it is the same
            // observation as the transform butterfly: the structure was in the
            // constants, and the code was not reading it.
            let t = &taps[fy];
            // `N == 8` is the LUMA instantiation, and luma is the only one that
            // can elide: every fractional chroma filter has nonzero taps at both
            // ends, so `CHROMA_SPAN[1..]` is `(0, N - 1)` throughout and the
            // elision is arithmetically a no-op there. Writing the test against
            // the const parameter lets it fold away for `interp::<4, 8>`, which
            // takes 170,346 of the 252,001 interpolation calls on a 720p clip:
            // gone with it are the `spans` load, the switch test, and -- since
            // `lo` is then the constant 0 -- the whole tap-rotation arm below
            // and its stack array. `chroma_never_elides` pins the premise.
            let (lo, hi) = if N == 8 && p.elide { spans[fy] } else { (0, N - 1) };
            let rows = h + hi - lo;
            if cen {
                census::route(hi - lo < N - 1, &census::RT_MC_TAP_ELIDE, &census::RT_MC_TAP_FULL);
                // The deterministic evidence for the elision: rows of horizontal
                // filtering actually performed. The loop BODY is unchanged, so
                // the instruction counter cannot see this win -- only the trip
                // count moves, and only a work count records it.
                census::bump(&census::MC_FIR_H_ROWS, rows as u64);
            }
            fir_h::<N>(p, &src[lo * stride..], stride, &taps[fx], w, rows, shift1, tmp, w);
            if lo == 0 {
                fir_v::<N>(p, tmp, w, t, w, h, 6, dst, w);
            } else {
                // Only filter 3 starts with a zero, so this rotation is off the
                // common path; building it unconditionally would put a stack
                // write on every 2-D call to save a row on a fifth of them.
                let mut tv = [0i16; N];
                tv[..N - lo].copy_from_slice(&t[lo..]);
                fir_v::<N>(p, tmp, w, &tv, w, h, 6, dst, w);
            }
        }
    }
}

/// Luma prediction block (8-tap, quarter-sample).
///
/// `src` starts at `(xInt − 3, yInt − 3)` and must have `(w + 7) × (h + 7)`
/// samples readable at `stride`. `tmp` needs `w × (h + 7)`.
pub fn interp_luma(src: &[u16], stride: usize, fx: usize, fy: usize, w: usize, h: usize, bit_depth: u8, dst: &mut [i16], tmp: &mut [i16]) {
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar;
        census::bump(if simd { &census::MC_LUMA_SIMD } else { &census::MC_LUMA_SCALAR }, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
        // Luma filter 2 is [-1,4,-11,40,40,-11,4,-1] -- symmetric. 1 and 3 are
        // mirror images of each other and symmetric in neither direction.
        census::bump(
            match fy {
                0 => &census::RT_MC_FY_NONE,
                2 => &census::RT_MC_FY_SYM,
                _ => &census::RT_MC_FY_ASYM,
            },
            (w * h) as u64,
        );
    }
    interp::<8, 4>(src, stride, fx, fy, w, h, bit_depth, &LUMA_FILTER, &LUMA_SPAN, &LUMA_SYM, dst, tmp);
}

/// Chroma prediction block (4-tap, eighth-sample).
///
/// `src` starts at `(xInt − 1, yInt − 1)` with `(w + 3) × (h + 3)` samples.
/// `tmp` needs `w × (h + 3)`.
pub fn interp_chroma(src: &[u16], stride: usize, fx: usize, fy: usize, w: usize, h: usize, bit_depth: u8, dst: &mut [i16], tmp: &mut [i16]) {
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar;
        census::bump(if simd { &census::MC_CHROMA_SIMD } else { &census::MC_CHROMA_SCALAR }, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
        // Chroma filter 4 is [-4,36,36,-4] -- the only symmetric one of the 8.
        census::bump(
            match fy {
                0 => &census::RT_MC_FY_NONE,
                4 => &census::RT_MC_FY_SYM,
                _ => &census::RT_MC_FY_ASYM,
            },
            (w * h) as u64,
        );
    }
    interp::<4, 8>(src, stride, fx, fy, w, h, bit_depth, &CHROMA_FILTER, &CHROMA_SPAN, &CHROMA_SYM, dst, tmp);
}

#[cfg(test)]
mod tests {
    /// The chroma shortcut in `interp` assumes no fractional chroma filter can
    /// elide a row. If a span ever gains a zero end tap, the shortcut silently
    /// stops taking it -- so assert the premise rather than the consequence.
    #[test]
    fn chroma_never_elides() {
        for (fy, &(lo, hi)) in super::CHROMA_SPAN.iter().enumerate().skip(1) {
            assert_eq!((lo, hi), (0, 3), "CHROMA_SPAN[{fy}] gained a zero end tap");
        }
        // ...and luma still does, or the elision arm is dead code.
        assert!(super::LUMA_SPAN[1..].iter().any(|&(lo, hi)| hi - lo < 7), "no luma filter elides");
    }

    use super::*;

    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state
    }

    fn random_plane(state: &mut u32, n: usize, max: u16) -> Vec<u16> {
        (0..n).map(|_| (lcg(state) >> 13) as u16 % (max + 1)).collect()
    }

    /// Every kernel against its scalar twin, over every block size HEVC can
    /// ask for, both bit depths, and every fractional position.
    /// The folded vertical kernel against the scalar oracle, over the shapes the
    /// interp sweep does NOT reach.
    ///
    /// `interp_luma_matches_scalar` exercises `(fx, fy) = (0, 2)` at several
    /// widths, but every height in its list is even -- so the folded kernel's
    /// odd-height tail, the branch that handles the last row on its own, ran in
    /// no test at all. A kernel whose tail is unexercised passes the suite
    /// exactly like one that is correct.
    ///
    /// This sweeps every height 1..=17 and widths that straddle the 16-lane
    /// vector boundary, so the two-row body, the odd-row tail and the scalar
    /// column remainder are all covered, for both palindromic filters and both
    /// shift regimes.
    #[test]
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    fn fir_v_sym_matches_scalar() {
        if crate::isa() != crate::Isa::Avx2 {
            return; // the folded kernel is AVX2-only; the general path is the fallback
        }
        let mut st = 0x5717_c0deu32;
        let mut checked = 0usize;
        for &bd in &[8u8, 10, 12] {
            let max = (1u16 << bd) - 1;
            let shift = (bd as u32).saturating_sub(8).min(4);
            for w in [1usize, 4, 7, 16, 17, 31, 32, 48] {
                for h in 1usize..=17 {
                    let stride = w + 13;
                    let src = random_plane(&mut st, stride * (h + 8) + 16, max);
                    // luma f2 and chroma fC4 are the palindromes.
                    let mut a = vec![0i16; w * h];
                    let mut b = vec![0i16; w * h];
                    fir_v_u16_scalar::<8>(&src, stride, &LUMA_FILTER[2], w, h, shift, &mut a, w);
                    // SAFETY: `src` holds `stride * (h + 7) + w` samples, taps
                    // are palindromic, and `bd <= 12 <= 14`.
                    unsafe {
                        x86::fir_v_avx2_sym::<8>(as_i16(&src).as_ptr(), stride, &LUMA_FILTER[2], w, h, shift, b.as_mut_ptr(), w);
                    }
                    assert_eq!(a, b, "luma f2 {w}x{h} bd={bd}");

                    let mut c = vec![0i16; w * h];
                    let mut d = vec![0i16; w * h];
                    fir_v_u16_scalar::<4>(&src, stride, &CHROMA_FILTER[4], w, h, shift, &mut c, w);
                    // SAFETY: as above; the 4-tap window is strictly smaller.
                    unsafe {
                        x86::fir_v_avx2_sym::<4>(as_i16(&src).as_ptr(), stride, &CHROMA_FILTER[4], w, h, shift, d.as_mut_ptr(), w);
                    }
                    assert_eq!(c, d, "chroma fC4 {w}x{h} bd={bd}");
                    checked += 2;
                }
            }
        }
        assert!(checked > 300, "sweep collapsed to {checked} cases");
    }

    /// The fold is only valid for a palindrome, and only two HEVC filters are.
    /// If a future edit widened the predicate to a filter that is not symmetric,
    /// every affected block would decode wrong; this pins which ones qualify.
    /// The span table must agree with the filters it describes.
    ///
    /// The 2-D path trusts `LUMA_SPAN`/`CHROMA_SPAN` to say which rows can be
    /// skipped. If a filter were edited and the table not, the decoder would
    /// silently drop a row that carries a nonzero tap -- wrong pixels, no
    /// panic, and only the conformance corpus would catch it.
    #[test]
    fn tap_spans_match_the_filters() {
        fn span(t: &[i16]) -> (usize, usize) {
            (t.iter().position(|&c| c != 0).unwrap(), t.iter().rposition(|&c| c != 0).unwrap())
        }
        for (i, f) in LUMA_FILTER.iter().enumerate() {
            assert_eq!(span(f), LUMA_SPAN[i], "LUMA_SPAN[{i}] disagrees with {f:?}");
        }
        for (i, f) in CHROMA_FILTER.iter().enumerate() {
            assert_eq!(span(f), CHROMA_SPAN[i], "CHROMA_SPAN[{i}] disagrees with {f:?}");
        }
        // The elision only pays where a span is actually short; record which.
        let short: Vec<usize> = (1..4).filter(|&i| LUMA_SPAN[i].1 - LUMA_SPAN[i].0 < 7).collect();
        assert_eq!(short, vec![1, 3], "the zero-end-tap luma filters moved");
    }

    #[test]
    fn exactly_two_hevc_filters_are_palindromic() {
        let luma: Vec<usize> = (0..4).filter(|&i| palindromic(&LUMA_FILTER[i])).collect();
        let chroma: Vec<usize> = (0..8).filter(|&i| palindromic(&CHROMA_FILTER[i])).collect();
        // Index 0 is the full-pel identity `[0,0,0,64,0,0,0,0]` / `[0,64,0,0]`,
        // which is not a palindrome and never reaches the FIR anyway.
        assert_eq!(luma, vec![2], "luma palindromes moved: {luma:?}");
        assert_eq!(chroma, vec![4], "chroma palindromes moved: {chroma:?}");
        // And the fold's premise: two samples must fit i16 after adding.
        assert!(2 * ((1u32 << 14) - 1) <= i16::MAX as u32, "the bit_depth <= 14 guard is not sufficient");
    }

    #[test]
    fn interp_luma_matches_scalar() {
        let mut st = 0x1234_5678u32;
        for &bd in &[8u8, 10] {
            let max = (1u16 << bd) - 1;
            for &(w, h) in &[(4, 4), (8, 4), (4, 8), (8, 8), (16, 8), (12, 16), (16, 16), (24, 8), (32, 32), (64, 64), (48, 16)] {
                let stride = w + 7 + 5;
                let src = random_plane(&mut st, stride * (h + 7) + 8, max);
                for fy in 0..4 {
                    for fx in 0..4 {
                        let mut a = vec![0i16; w * h];
                        let mut b = vec![0i16; w * h];
                        let mut t1 = vec![0i16; w * (h + 7)];
                        let mut t2 = vec![0i16; w * (h + 7)];
                        // scalar reference, bypassing dispatch
                        interp_scalar_ref::<8>(&src, stride, fx, fy, w, h, bd, &LUMA_FILTER, &mut a, &mut t1);
                        interp_luma(&src, stride, fx, fy, w, h, bd, &mut b, &mut t2);
                        assert_eq!(a, b, "luma {w}x{h} bd={bd} frac=({fx},{fy})");
                    }
                }
            }
        }
    }

    #[test]
    fn interp_chroma_matches_scalar() {
        let mut st = 0x9e37_79b9u32;
        for &bd in &[8u8, 10] {
            let max = (1u16 << bd) - 1;
            for &(w, h) in &[(2, 2), (4, 2), (2, 4), (4, 4), (8, 4), (6, 8), (8, 8), (16, 16), (32, 32), (12, 4)] {
                let stride = w + 3 + 5;
                let src = random_plane(&mut st, stride * (h + 3) + 8, max);
                for fy in 0..8 {
                    for fx in 0..8 {
                        let mut a = vec![0i16; w * h];
                        let mut b = vec![0i16; w * h];
                        let mut t1 = vec![0i16; w * (h + 3)];
                        let mut t2 = vec![0i16; w * (h + 3)];
                        interp_scalar_ref::<4>(&src, stride, fx, fy, w, h, bd, &CHROMA_FILTER, &mut a, &mut t1);
                        interp_chroma(&src, stride, fx, fy, w, h, bd, &mut b, &mut t2);
                        assert_eq!(a, b, "chroma {w}x{h} bd={bd} frac=({fx},{fy})");
                    }
                }
            }
        }
    }

    /// The oracle: the same arithmetic with no dispatch and no kernels.
    fn interp_scalar_ref<const N: usize>(src: &[u16], stride: usize, fx: usize, fy: usize, w: usize, h: usize, bit_depth: u8, taps: &[[i16; N]], dst: &mut [i16], tmp: &mut [i16]) {
        let m = margin(N);
        let shift1 = (bit_depth as u32).saturating_sub(8).min(4);
        let shift3 = (14u32).saturating_sub(bit_depth as u32).max(2);
        match (fx, fy) {
            (0, 0) => copy_shift_scalar(&src[m * stride + m..], stride, w, h, shift3, dst, w),
            (fx, 0) => fir_h_scalar::<N>(&src[m * stride..], stride, &taps[fx], w, h, shift1, dst, w),
            (0, fy) => fir_v_u16_scalar::<N>(&src[m..], stride, &taps[fy], w, h, shift1, dst, w),
            (fx, fy) => {
                let rows = h + N - 1;
                fir_h_scalar::<N>(src, stride, &taps[fx], w, rows, shift1, tmp, w);
                fir_v_scalar::<N>(tmp, w, &taps[fy], w, h, 6, dst, w);
            }
        }
    }

    /// The range table in the module header, checked rather than asserted.
    /// Single-pass values must fit `i16` unconditionally; the two-pass
    /// composite is bounded by the conformance requirement instead, and this
    /// records how far outside `i16` an adversarial (non-conforming) input
    /// could reach, so the saturation note in the header stays honest.
    #[test]
    fn intermediates_stay_in_i16() {
        for taps in LUMA_FILTER.iter().chain(std::iter::empty()) {
            let pos: i32 = taps.iter().filter(|&&t| t > 0).map(|&t| t as i32).sum();
            let neg: i32 = taps.iter().filter(|&&t| t < 0).map(|&t| t as i32).sum();
            for &bd in &[8u8, 10, 12] {
                let max = (1i32 << bd) - 1;
                let shift1 = (bd as i32 - 8).min(4);
                let hi = (max * pos) >> shift1;
                let lo = (max * neg) >> shift1;
                assert!(hi <= i16::MAX as i32 && lo >= i16::MIN as i32, "one pass, bd={bd}: {lo}..{hi}");
                let shift3 = (14 - bd as i32).max(2);
                assert!((max << shift3) <= i16::MAX as i32, "full-pel, bd={bd}");
            }
        }
        // The composite bound quoted in the module header.
        let f = LUMA_FILTER[2];
        let pos: i32 = f.iter().filter(|&&t| t > 0).map(|&t| t as i32).sum();
        let neg: i32 = f.iter().filter(|&&t| t < 0).map(|&t| t as i32).sum();
        let composite = (255 * (pos * pos + neg * neg)) >> 6;
        assert_eq!(composite, 33_150, "the header's adversarial bound");
        assert!(composite > i16::MAX as i32, "and it is outside i16, hence the conformance clause");
    }

    /// The symmetry tables must agree with the filters they describe.
    ///
    /// The dispatch reads these instead of scanning the taps, so a filter edit
    /// that did not update them would silently take (or miss) the folded kernel.
    #[test]
    fn sym_tables_match_the_filters() {
        for (i, f) in LUMA_FILTER.iter().enumerate() {
            assert_eq!(palindromic(f), LUMA_SYM[i], "LUMA_SYM[{i}] disagrees with {f:?}");
        }
        for (i, f) in CHROMA_FILTER.iter().enumerate() {
            assert_eq!(palindromic(f), CHROMA_SYM[i], "CHROMA_SYM[{i}] disagrees with {f:?}");
        }
        assert_eq!(LUMA_SYM.iter().filter(|&&b| b).count(), 1, "exactly one luma filter is a palindrome");
        assert_eq!(CHROMA_SYM.iter().filter(|&&b| b).count(), 1, "exactly one chroma filter is a palindrome");
    }
}
