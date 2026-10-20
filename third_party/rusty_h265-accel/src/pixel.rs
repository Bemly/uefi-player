//! Sample-domain kernels: writing a prediction out of the 14-bit intermediate
//! (§8.5.3.3.4.2 default weighted prediction) and adding a residual into the
//! picture (§8.6.6).
//!
//! # Why saturating adds are exact here
//!
//! Both write-out paths end in the mandatory clip to `[0, maxVal]`, and
//! `maxVal` is at most 1023. Whenever a saturating `i16` add would clamp at
//! ±32767, the exact `i32` result — shifted right by 5, 6 or 7 — is already
//! outside `[0, maxVal]` and the clip produces the same sample. So the kernels
//! stay in `i16` lanes end to end, with no widening, and still match the
//! scalar twin bit for bit. The `*_matches_scalar` tests sweep the saturation
//! boundary deliberately, not just typical values.

use crate::census;

/// Bring-up switch: `RH265_SCALAR_GATE=1` keeps the arms built by the Great
/// Gate campaign — explicit weighted prediction and transform skip — on their
/// scalar twins, so each can be A/B'd inside one binary on the content class
/// whose population justified building it.
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: off).
#[cfg(feature = "std")]
fn scalar_gate() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RH265_SCALAR_GATE").is_some())
}
// VENDOR(uefi-player): no_std => knob reads as unset (shipped default: off).
#[cfg(not(feature = "std"))]
fn scalar_gate() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Scalar reference
// ---------------------------------------------------------------------------

/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream -- the `*_SCALAR` census counters read 0 across the corpus -- so this
/// is the fallback for a shape the kernels decline, not a path decode takes.
/// Inlined it padded the dispatcher, which IS on the hot path, with a body that
/// never runs. Same reason `Cabac::refill_tail` is out of line.
#[cold]
#[inline(never)]
fn put_uni_scalar(dst: &mut [u16], dst_stride: usize, src: &[i16], w: usize, h: usize, bit_depth: u8) {
    let shift = 14u32 - bit_depth as u32;
    let off = 1i32 << (shift - 1);
    let max = (1i32 << bit_depth) - 1;
    for y in 0..h {
        let row = &mut dst[y * dst_stride..y * dst_stride + w];
        let s = &src[y * w..y * w + w];
        for (d, &v) in row.iter_mut().zip(s) {
            *d = (((v as i32 + off) >> shift).clamp(0, max)) as u16;
        }
    }
}

/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream -- the `*_SCALAR` census counters read 0 across the corpus -- so this
/// is the fallback for a shape the kernels decline, not a path decode takes.
/// Inlined it padded the dispatcher, which IS on the hot path, with a body that
/// never runs. Same reason `Cabac::refill_tail` is out of line.
#[cold]
#[inline(never)]
fn put_bi_scalar(dst: &mut [u16], dst_stride: usize, a: &[i16], b: &[i16], w: usize, h: usize, bit_depth: u8) {
    let shift = 15u32 - bit_depth as u32;
    let off = 1i32 << (shift - 1);
    let max = (1i32 << bit_depth) - 1;
    for y in 0..h {
        let row = &mut dst[y * dst_stride..y * dst_stride + w];
        let (pa, pb) = (&a[y * w..y * w + w], &b[y * w..y * w + w]);
        for (d, (&x, &z)) in row.iter_mut().zip(pa.iter().zip(pb)) {
            *d = (((x as i32 + z as i32 + off) >> shift).clamp(0, max)) as u16;
        }
    }
}

/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream -- the `*_SCALAR` census counters read 0 across the corpus -- so this
/// is the fallback for a shape the kernels decline, not a path decode takes.
/// Inlined it padded the dispatcher, which IS on the hot path, with a body that
/// never runs. Same reason `Cabac::refill_tail` is out of line.
#[cold]
#[inline(never)]
fn add_residual_scalar(dst: &mut [u16], dst_stride: usize, res: &[i32], w: usize, h: usize, max: i32) {
    for y in 0..h {
        let row = &mut dst[y * dst_stride..y * dst_stride + w];
        let r = &res[y * w..y * w + w];
        for (d, &v) in row.iter_mut().zip(r) {
            *d = ((*d as i32 + v).clamp(0, max)) as u16;
        }
    }
}

// ---------------------------------------------------------------------------
// x86-64
// ---------------------------------------------------------------------------

/// Full-pel uni-prediction: the reference samples ARE the prediction.
///
/// `copy_shift` writes `s << shift3` with `shift3 = 14 − BitDepth`, and
/// [`put_uni`] then computes `(v + 2^(shift−1)) >> shift` with the same
/// `shift`. Composing them:
///
/// ```text
///   (s·2^k + 2^(k−1)) >> k  ==  s + floor(2^(k−1) / 2^k)  ==  s
/// ```
///
/// exactly, for any `k >= 1`; and the clamp is a no-op because `s` is already a
/// sample. So an integer motion vector with no weighting needs neither kernel —
/// it needs a rectangle copy.
// NOT `#[cold]`, unlike its siblings: `copy_block` has no vector path at all
// -- a per-row `copy_from_slice` is already the best primitive -- so this is
// the ONLY path, taken on every full-pel uni-predicted block. Outlining it
// took `copy_block` from 56 instructions to 1, which is the tell: the work
// moved behind a jump instead of leaving the hot path.
fn copy_block_scalar(dst: &mut [u16], dst_stride: usize, src: &[u16], src_stride: usize, w: usize, h: usize) {
    // The row width is dispatched to a CONSTANT before it is copied.
    //
    // `copy_from_slice` with a runtime length is an opaque `call memcpy`, and
    // these rows are prediction-block widths: 4 to 64 samples, so 8 to 128
    // BYTES. At that size the call is the whole cost -- and `copy_block` runs
    // 71,413 times on a 20-second clip, once per full-pel uni-predicted block
    // per component, each doing `h` of them. A constant length inlines to one
    // to four vector load/store pairs instead.
    macro_rules! rows {
        ($k:literal) => {{
            for y in 0..h {
                let (d, s) = (y * dst_stride, y * src_stride);
                if let (Some(a), Some(b)) = (dst[d..].first_chunk_mut::<$k>(), src[s..].first_chunk::<$k>()) {
                    *a = *b;
                }
            }
            return;
        }};
    }
    match w {
        4 => rows!(4),
        8 => rows!(8),
        16 => rows!(16),
        32 => rows!(32),
        64 => rows!(64),
        _ => {}
    }
    for y in 0..h {
        dst[y * dst_stride..y * dst_stride + w].copy_from_slice(&src[y * src_stride..y * src_stride + w]);
    }
}

/// Full-pel bi-prediction: the rounding average of the two references.
///
/// The same composition one step further. [`put_bi`] shifts by `15 − BitDepth`
/// and rounds by `2^(14 − BitDepth)`, so with both inputs `s << (14 − BitDepth)`:
///
/// ```text
///   (s0·2^k + s1·2^k + 2^k) >> (k+1)  ==  (s0 + s1 + 1) >> 1
/// ```
///
/// which is exactly what `pavgw` computes, in one instruction.
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn avg_block_scalar(dst: &mut [u16], dst_stride: usize, a: &[u16], a_stride: usize, b: &[u16], b_stride: usize, w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            dst[y * dst_stride + x] = ((a[y * a_stride + x] as u32 + b[y * b_stride + x] as u32 + 1) >> 1) as u16;
        }
    }
}

/// Bi-prediction where exactly ONE list is full-pel.
///
/// The general path materialises that list first — `copy_shift` reads a `u16`
/// sample, shifts it left by `k = 14 − BitDepth` and stores an `i16` — and then
/// [`put_bi`] loads it straight back. The shift is one instruction and the
/// round trip is a store and a load per sample, so the whole pass exists to
/// carry a value the write could have shifted itself:
///
/// ```text
///   clamp( ((s << k) + b + 2^k) >> (k+1) )
/// ```
///
/// Folding it removes a full pass over the block. The two remaining full-pel
/// cases already collapse further: both lists full-pel is [`avg_block`], and
/// uni-prediction is [`copy_block`].
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn put_bi_fp_scalar(dst: &mut [u16], dst_stride: usize, s: &[u16], s_stride: usize, b: &[i16], w: usize, h: usize, bit_depth: u8) {
    let k = 14i32 - bit_depth as i32;
    let shift = k + 1;
    let off = 1i32 << (shift - 1);
    let max = (1i32 << bit_depth) - 1;
    for y in 0..h {
        for x in 0..w {
            let a = (s[y * s_stride + x] as i32) << k;
            let v = (a + b[y * w + x] as i32 + off) >> shift;
            dst[y * dst_stride + x] = v.clamp(0, max) as u16;
        }
    }
}

/// DC prediction and residual add, fused.
///
/// DC prediction writes one constant across the block; the residual add then
/// loads every one of those samples straight back, adds and clamps. Composed:
///
/// ```text
///   dst = clamp(dc + res)
/// ```
///
/// which needs neither the fill nor the load — one pass instead of two, and the
/// predicted block never reaches memory. The caller only skips the fill when a
/// residual is known to follow, so the two stay in step.
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
fn add_residual_const_scalar(dst: &mut [u16], dst_stride: usize, dc: u16, res: &[i32], w: usize, h: usize, max: i32) {
    for y in 0..h {
        for x in 0..w {
            dst[y * dst_stride + x] = (dc as i32 + res[y * w + x]).clamp(0, max) as u16;
        }
    }
}

/// Explicit weighted prediction, uni (§8.5.3.3.4.3).
///
/// `dst = clamp(((x·w + 2^(log2wd−1)) >> log2wd) + o)`.
///
/// Every term fits the `pmaddwd` contraction: the prediction `x` is the 14-bit
/// intermediate, the weight is `(1 << denom) + delta` with `delta` in −128..=127
/// and `denom ≤ 7`, and the rounding term is at most `2^12`. Pairing `(x, 1)`
/// against `(w, round)` therefore computes `x·w + round` exactly in one
/// instruction, which is what makes this worth vectorising at all.
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn weighted_uni_scalar(dst: &mut [u16], dst_stride: usize, src: &[i16], w: usize, h: usize, wt: i32, off: i32, log2wd: i32, max: i32) {
    for y in 0..h {
        for x in 0..w {
            let v = src[y * w + x] as i32;
            let r = if log2wd >= 1 { ((v * wt + (1 << (log2wd - 1))) >> log2wd) + off } else { v * wt + off };
            dst[y * dst_stride + x] = r.clamp(0, max) as u16;
        }
    }
}

/// Explicit weighted prediction, bi (§8.5.3.3.4.3).
///
/// `dst = clamp((x·w0 + z·w1 + ((o0+o1+1) << log2wd)) >> (log2wd + 1))`.
///
/// Interleaving the two predictions puts `x` and `z` in adjacent lanes, so a
/// single `pmaddwd` against `(w0, w1)` is the whole weighted sum.
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn weighted_bi_scalar(dst: &mut [u16], dst_stride: usize, a: &[i16], b: &[i16], w: usize, h: usize, w0: i32, w1: i32, obias: i32, log2wd: i32, max: i32) {
    for y in 0..h {
        for x in 0..w {
            let v = a[y * w + x] as i32 * w0 + b[y * w + x] as i32 * w1 + obias;
            dst[y * dst_stride + x] = (v >> (log2wd + 1)).clamp(0, max) as u16;
        }
    }
}

/// Transform skip (§8.6.2): `r = ((d << 7) + 2^(shift−1)) >> shift`.
///
/// A whole content class lives here. On natural video transform-skip is a
/// rounding error — 2% of blocks — but on screen and graphics content it is the
/// DOMINANT kind: `DBLK_A_MAIN10_VIXS_4` codes 10,757 of its 16,547 transform
/// blocks this way, 65%. Those blocks skip the DCT entirely, so this shift IS
/// their whole inverse transform, and it was a scalar loop.
///
/// This is the Great Gate's decoder rule in one function: a population of
/// streams served by a slow path is a missing kernel, and the population is
/// what tells you it exists — the site looks cold from any single stream.
/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream, so this is the fallback for a shape the kernels decline, not a path
/// decode takes. Marked as a SET with its siblings: outlining ONE exit while
/// others stay inlined buys the argument marshalling and none of the locality.
#[cold]
#[inline(never)]
fn transform_skip_scalar(d: &mut [i32], n: usize, shift: u32) {
    let add = 1i32 << (shift - 1);
    for v in d[..n * n].iter_mut() {
        *v = ((*v << 7) + add) >> shift;
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod x86 {
    use std::arch::x86_64::*;

    /// # Safety
    /// `dst` must have `dst_stride * (h - 1) + w` writable `u16`; `src`
    /// `w * h` readable `i16`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn put_uni_sse2(dst: *mut u16, dst_stride: usize, src: *const i16, w: usize, h: usize, bit_depth: u8) {
        let shift = 14i32 - bit_depth as i32;
        let off = _mm_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm_setzero_si128();
        let sh = _mm_cvtsi32_si128(shift);
        for y in 0..h {
            let s = unsafe { src.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let v = unsafe { _mm_loadu_si128(s.add(x) as *const __m128i) };
                    let v = _mm_adds_epi16(v, off);
                    let v = _mm_sra_epi16(v, sh);
                    let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let v = unsafe { _mm_loadu_si128(s.add(x) as *const __m128i) };
                let v = _mm_adds_epi16(v, off);
                let v = _mm_sra_epi16(v, sh);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            // A 4-wide step before the scalar tail.
            //
            // `nvec = w / 8`, so a 4-wide block skipped both vector loops and
            // ran the scalar tail for every sample. In 4:2:0 the chroma block of
            // an 8x8 luma PU is 4 wide, and the census puts 18.3% of
            // pixel-kernel samples on intra-heavy content in blocks narrower
            // than 8. `movq` moves exactly four `i16`.
            if x + 4 <= w {
                let v = unsafe { _mm_loadl_epi64(s.add(x) as *const __m128i) };
                let v = _mm_sra_epi16(_mm_adds_epi16(v, off), sh);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < w {
                let v = unsafe { *s.add(x) } as i32;
                let r = ((v + (1 << (shift - 1))) >> shift).clamp(0, (1 << bit_depth) - 1);
                unsafe { *d.add(x) = r as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`put_uni_sse2`], with both `a` and `b` holding `w * h` samples.
    #[target_feature(enable = "sse2")]
    pub unsafe fn put_bi_sse2(dst: *mut u16, dst_stride: usize, a: *const i16, b: *const i16, w: usize, h: usize, bit_depth: u8) {
        let shift = 15i32 - bit_depth as i32;
        let off = _mm_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm_setzero_si128();
        let sh = _mm_cvtsi32_si128(shift);
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * w) }, unsafe { b.add(y * w) });
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let va = unsafe { _mm_loadu_si128(pa.add(x) as *const __m128i) };
                    let vb = unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) };
                    let v = _mm_adds_epi16(_mm_adds_epi16(va, vb), off);
                    let v = _mm_sra_epi16(v, sh);
                    let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let va = unsafe { _mm_loadu_si128(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) };
                let v = _mm_adds_epi16(_mm_adds_epi16(va, vb), off);
                let v = _mm_sra_epi16(v, sh);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            if x + 4 <= w {
                let va = unsafe { _mm_loadl_epi64(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadl_epi64(pb.add(x) as *const __m128i) };
                let v = _mm_sra_epi16(_mm_adds_epi16(_mm_adds_epi16(va, vb), off), sh);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < w {
                let v = unsafe { *pa.add(x) } as i32 + unsafe { *pb.add(x) } as i32;
                let r = ((v + (1 << (shift - 1))) >> shift).clamp(0, (1 << bit_depth) - 1);
                unsafe { *d.add(x) = r as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// `dst` must have `dst_stride * (h - 1) + w` writable `u16`; `res`
    /// `w * h` readable `i32`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn add_residual_sse2(dst: *mut u16, dst_stride: usize, res: *const i32, w: usize, h: usize, max: i32) {
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        for y in 0..h {
            let r = unsafe { res.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let r0 = unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) };
                    let r1 = unsafe { _mm_loadu_si128(r.add(x + 4) as *const __m128i) };
                    let rv = _mm_packs_epi32(r0, r1);
                    let dv = unsafe { _mm_loadu_si128(d.add(x) as *const __m128i) };
                    let v = _mm_adds_epi16(dv, rv);
                    let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let r0 = unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) };
                let r1 = unsafe { _mm_loadu_si128(r.add(x + 4) as *const __m128i) };
                let rv = _mm_packs_epi32(r0, r1);
                let dv = unsafe { _mm_loadu_si128(d.add(x) as *const __m128i) };
                let v = _mm_adds_epi16(dv, rv);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            if x + 4 <= w {
                // Four `i32` residuals narrow to `i16` and add to four samples.
                let p = _mm_packs_epi32(unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) }, _mm_setzero_si128());
                let v = _mm_adds_epi16(unsafe { _mm_loadl_epi64(d.add(x) as *const __m128i) }, p);
                let v = _mm_min_epi16(_mm_max_epi16(v, zero), maxv);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < w {
                let v = unsafe { *d.add(x) } as i32 + unsafe { *r.add(x) };
                unsafe { *d.add(x) = v.clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`put_uni_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn put_uni_avx2(dst: *mut u16, dst_stride: usize, src: *const i16, w: usize, h: usize, bit_depth: u8) {
        // Narrow blocks: ONE call for the whole block, not one per row.
        //
        // These kernels step 16 or 32 samples, so for `w < 16` every row fell
        // through to the SSE2 kernel -- a separate call per row, each rebuilding
        // all four broadcast constants. 31% of pixel-kernel samples on
        // intra-heavy content are 8 wide.
        if w < 16 {
            // SAFETY: same footprint, one call instead of `h`.
            return unsafe { put_uni_sse2(dst, dst_stride, src, w, h, bit_depth) };
        }

        let shift = 14i32 - bit_depth as i32;
        let off = _mm256_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm256_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm256_setzero_si256();
        let sh = _mm_cvtsi32_si128(shift);
        for y in 0..h {
            let s = unsafe { src.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip. The body is five instructions and the
            // loop bookkeeping is three, so at one vector per trip nearly 40%
            // of the loop is the loop. Pairing them halves that share; the odd
            // vector, if any, falls through to the tail below.
            let npair = w / 32;
            for i in 0..npair {
                let x = i * 32;
                let a = unsafe { _mm256_loadu_si256(s.add(x) as *const __m256i) };
                let b = unsafe { _mm256_loadu_si256(s.add(x + 16) as *const __m256i) };
                let a = _mm256_sra_epi16(_mm256_adds_epi16(a, off), sh);
                let b = _mm256_sra_epi16(_mm256_adds_epi16(b, off), sh);
                let a = _mm256_min_epi16(_mm256_max_epi16(a, zero), maxv);
                let b = _mm256_min_epi16(_mm256_max_epi16(b, zero), maxv);
                unsafe {
                    _mm256_storeu_si256(d.add(x) as *mut __m256i, a);
                    _mm256_storeu_si256(d.add(x + 16) as *mut __m256i, b);
                }
            }
            let mut x = npair * 32;
            if x + 16 <= w {
                let v = unsafe { _mm256_loadu_si256(s.add(x) as *const __m256i) };
                let v = _mm256_sra_epi16(_mm256_adds_epi16(v, off), sh);
                let v = _mm256_min_epi16(_mm256_max_epi16(v, zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, v) };
                x += 16;
            }
            if x < w {
                // SAFETY: the remaining samples of this row are in bounds.
                unsafe { put_uni_sse2(d.add(x), dst_stride, s.add(x), w - x, 1, bit_depth) };
            }
        }
    }

    /// # Safety
    /// As [`put_bi_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn put_bi_avx2(dst: *mut u16, dst_stride: usize, a: *const i16, b: *const i16, w: usize, h: usize, bit_depth: u8) {
        // Narrow blocks: ONE call for the whole block, not one per row.
        //
        // These kernels step 16 or 32 samples, so for `w < 16` every row fell
        // through to the SSE2 kernel -- a separate call per row, each rebuilding
        // all four broadcast constants. 31% of pixel-kernel samples on
        // intra-heavy content are 8 wide.
        if w < 16 {
            // SAFETY: same footprint, one call instead of `h`.
            return unsafe { put_bi_sse2(dst, dst_stride, a, b, w, h, bit_depth) };
        }

        let shift = 15i32 - bit_depth as i32;
        let off = _mm256_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm256_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm256_setzero_si256();
        let sh = _mm_cvtsi32_si128(shift);
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * w) }, unsafe { b.add(y * w) });
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip. The body is five instructions and the
            // loop bookkeeping is three, so at one vector per trip nearly 40%
            // of the loop is the loop. Pairing them halves that share; the odd
            // vector, if any, falls through to the tail below.
            let npair = w / 32;
            for i in 0..npair {
                let x = i * 32;
                let a = _mm256_adds_epi16(
                    _mm256_adds_epi16(unsafe { _mm256_loadu_si256(pa.add(x) as *const __m256i) }, unsafe { _mm256_loadu_si256(pb.add(x) as *const __m256i) }),
                    off,
                );
                let b = _mm256_adds_epi16(
                    _mm256_adds_epi16(unsafe { _mm256_loadu_si256(pa.add(x + 16) as *const __m256i) }, unsafe {
                        _mm256_loadu_si256(pb.add(x + 16) as *const __m256i)
                    }),
                    off,
                );
                let a = _mm256_min_epi16(_mm256_max_epi16(_mm256_sra_epi16(a, sh), zero), maxv);
                let b = _mm256_min_epi16(_mm256_max_epi16(_mm256_sra_epi16(b, sh), zero), maxv);
                unsafe {
                    _mm256_storeu_si256(d.add(x) as *mut __m256i, a);
                    _mm256_storeu_si256(d.add(x + 16) as *mut __m256i, b);
                }
            }
            let mut x = npair * 32;
            if x + 16 <= w {
                let va = unsafe { _mm256_loadu_si256(pa.add(x) as *const __m256i) };
                let vb = unsafe { _mm256_loadu_si256(pb.add(x) as *const __m256i) };
                let v = _mm256_sra_epi16(_mm256_adds_epi16(_mm256_adds_epi16(va, vb), off), sh);
                let v = _mm256_min_epi16(_mm256_max_epi16(v, zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, v) };
                x += 16;
            }
            if x < w {
                // SAFETY: the remaining samples of this row are in bounds.
                unsafe { put_bi_sse2(d.add(x), dst_stride, pa.add(x), pb.add(x), w - x, 1, bit_depth) };
            }
        }
    }

    /// # Safety
    /// As [`add_residual_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn add_residual_avx2(dst: *mut u16, dst_stride: usize, res: *const i32, w: usize, h: usize, max: i32) {
        // Narrow blocks: ONE call for the whole block, not one per row.
        //
        // These kernels step 16 or 32 samples, so for `w < 16` every row fell
        // through to the SSE2 kernel -- a separate call per row, each rebuilding
        // all four broadcast constants. 31% of pixel-kernel samples on
        // intra-heavy content are 8 wide.
        if w < 16 {
            // SAFETY: same footprint, one call instead of `h`.
            return unsafe { add_residual_sse2(dst, dst_stride, res, w, h, max) };
        }

        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        for y in 0..h {
            let r = unsafe { res.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip; see `put_uni_avx2` for the arithmetic.
            let npair = w / 32;
            for i in 0..npair {
                let x = i * 32;
                // `packs` is per 128-bit lane, so the halves interleave; the
                // permute puts the 16 residuals back in picture order.
                let pa = _mm256_permute4x64_epi64(
                    _mm256_packs_epi32(unsafe { _mm256_loadu_si256(r.add(x) as *const __m256i) }, unsafe { _mm256_loadu_si256(r.add(x + 8) as *const __m256i) }),
                    0b11_01_10_00,
                );
                let pb = _mm256_permute4x64_epi64(
                    _mm256_packs_epi32(unsafe { _mm256_loadu_si256(r.add(x + 16) as *const __m256i) }, unsafe {
                        _mm256_loadu_si256(r.add(x + 24) as *const __m256i)
                    }),
                    0b11_01_10_00,
                );
                let va = _mm256_adds_epi16(unsafe { _mm256_loadu_si256(d.add(x) as *const __m256i) }, pa);
                let vb = _mm256_adds_epi16(unsafe { _mm256_loadu_si256(d.add(x + 16) as *const __m256i) }, pb);
                let va = _mm256_min_epi16(_mm256_max_epi16(va, zero), maxv);
                let vb = _mm256_min_epi16(_mm256_max_epi16(vb, zero), maxv);
                unsafe {
                    _mm256_storeu_si256(d.add(x) as *mut __m256i, va);
                    _mm256_storeu_si256(d.add(x + 16) as *mut __m256i, vb);
                }
            }
            let mut x = npair * 32;
            if x + 16 <= w {
                let r0 = unsafe { _mm256_loadu_si256(r.add(x) as *const __m256i) };
                let r1 = unsafe { _mm256_loadu_si256(r.add(x + 8) as *const __m256i) };
                let packed = _mm256_permute4x64_epi64(_mm256_packs_epi32(r0, r1), 0b11_01_10_00);
                let dv = unsafe { _mm256_loadu_si256(d.add(x) as *const __m256i) };
                let v = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(dv, packed), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, v) };
                x += 16;
            }
            if x < w {
                // SAFETY: the remaining samples of this row are in bounds.
                unsafe { add_residual_sse2(d.add(x), dst_stride, r.add(x), w - x, 1, max) };
            }
        }
    }

    /// # Safety
    /// `a` and `b` must have `stride * (h - 1) + w` readable `u16` each, and
    /// `dst` the same writable.
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn avg_block_avx2(dst: *mut u16, dst_stride: usize, a: *const u16, a_stride: usize, b: *const u16, b_stride: usize, w: usize, h: usize) {
        let nvec = w / 16;
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * a_stride) }, unsafe { b.add(y * b_stride) });
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let va = unsafe { _mm256_loadu_si256(pa.add(x) as *const __m256i) };
                let vb = unsafe { _mm256_loadu_si256(pb.add(x) as *const __m256i) };
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, _mm256_avg_epu16(va, vb)) };
            }
            let mut x = nvec * 16;
            while x + 8 <= w {
                let va = unsafe { _mm_loadu_si128(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) };
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, _mm_avg_epu16(va, vb)) };
                x += 8;
            }
            if x + 4 <= w {
                let va = unsafe { _mm_loadl_epi64(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadl_epi64(pb.add(x) as *const __m128i) };
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, _mm_avg_epu16(va, vb)) };
                x += 4;
            }
            while x < w {
                unsafe { *d.add(x) = ((*pa.add(x) as u32 + *pb.add(x) as u32 + 1) >> 1) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`avg_block_avx2`].
    #[target_feature(enable = "sse2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn avg_block_sse2(dst: *mut u16, dst_stride: usize, a: *const u16, a_stride: usize, b: *const u16, b_stride: usize, w: usize, h: usize) {
        let nvec = w / 8;
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * a_stride) }, unsafe { b.add(y * b_stride) });
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let va = unsafe { _mm_loadu_si128(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) };
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, _mm_avg_epu16(va, vb)) };
            }
            let mut x = nvec * 8;
            while x < w {
                unsafe { *d.add(x) = ((*pa.add(x) as u32 + *pb.add(x) as u32 + 1) >> 1) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// `s` must have `s_stride * (h - 1) + w` readable `u16`, `b` `w * h`
    /// readable `i16`, `dst` the corresponding writable `u16`.
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn put_bi_fp_avx2(dst: *mut u16, dst_stride: usize, s: *const u16, s_stride: usize, b: *const i16, w: usize, h: usize, bit_depth: u8) {
        let k = 14i32 - bit_depth as i32;
        let shift = k + 1;
        let kv = _mm_cvtsi32_si128(k);
        let sh = _mm_cvtsi32_si128(shift);
        let off = _mm256_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm256_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm256_setzero_si256();
        let nvec = w / 16;
        for y in 0..h {
            let ps = unsafe { s.add(y * s_stride) };
            let pb = unsafe { b.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                // The left shift the removed `copy_shift` pass used to do.
                let a = _mm256_sll_epi16(unsafe { _mm256_loadu_si256(ps.add(x) as *const __m256i) }, kv);
                let v = _mm256_adds_epi16(_mm256_adds_epi16(a, unsafe { _mm256_loadu_si256(pb.add(x) as *const __m256i) }), off);
                let v = _mm256_min_epi16(_mm256_max_epi16(_mm256_sra_epi16(v, sh), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, v) };
            }
            let mut x = nvec * 16;
            // Narrow blocks. `nvec = w / 16`, so everything under 16 samples
            // wide fell to the SCALAR tail -- and the census puts 18.3% of
            // pixel-kernel samples on intra-heavy content in blocks narrower
            // than 8, with another 31% exactly 8 wide (4:2:0 chroma of an 8x8
            // luma PU is 4 wide). 128- and 64-bit steps cover both.
            let off8 = _mm_set1_epi16((1i16) << (shift - 1));
            let max8 = _mm_set1_epi16(((1i32 << bit_depth) - 1) as i16);
            let z8 = _mm_setzero_si128();
            while x + 8 <= w {
                let a = _mm_sll_epi16(unsafe { _mm_loadu_si128(ps.add(x) as *const __m128i) }, kv);
                let v = _mm_adds_epi16(_mm_adds_epi16(a, unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) }), off8);
                let v = _mm_min_epi16(_mm_max_epi16(_mm_sra_epi16(v, sh), z8), max8);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
                x += 8;
            }
            if x + 4 <= w {
                let a = _mm_sll_epi16(unsafe { _mm_loadl_epi64(ps.add(x) as *const __m128i) }, kv);
                let v = _mm_adds_epi16(_mm_adds_epi16(a, unsafe { _mm_loadl_epi64(pb.add(x) as *const __m128i) }), off8);
                let v = _mm_min_epi16(_mm_max_epi16(_mm_sra_epi16(v, sh), z8), max8);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < w {
                let a = (unsafe { *ps.add(x) } as i32) << k;
                let v = (a + unsafe { *pb.add(x) } as i32 + (1 << (shift - 1))) >> shift;
                unsafe { *d.add(x) = v.clamp(0, (1 << bit_depth) - 1) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`put_bi_fp_avx2`].
    #[target_feature(enable = "sse2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn put_bi_fp_sse2(dst: *mut u16, dst_stride: usize, s: *const u16, s_stride: usize, b: *const i16, w: usize, h: usize, bit_depth: u8) {
        let k = 14i32 - bit_depth as i32;
        let shift = k + 1;
        let kv = _mm_cvtsi32_si128(k);
        let sh = _mm_cvtsi32_si128(shift);
        let off = _mm_set1_epi16((1i16) << (shift - 1));
        let maxv = _mm_set1_epi16(((1i32 << bit_depth) - 1) as i16);
        let zero = _mm_setzero_si128();
        let nvec = w / 8;
        for y in 0..h {
            let ps = unsafe { s.add(y * s_stride) };
            let pb = unsafe { b.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let a = _mm_sll_epi16(unsafe { _mm_loadu_si128(ps.add(x) as *const __m128i) }, kv);
                let v = _mm_adds_epi16(_mm_adds_epi16(a, unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) }), off);
                let v = _mm_min_epi16(_mm_max_epi16(_mm_sra_epi16(v, sh), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            while x < w {
                let a = (unsafe { *ps.add(x) } as i32) << k;
                let v = (a + unsafe { *pb.add(x) } as i32 + (1 << (shift - 1))) >> shift;
                unsafe { *d.add(x) = v.clamp(0, (1 << bit_depth) - 1) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// `res` must have `w * h` readable `i32`, `dst` `dst_stride * (h - 1) + w`
    /// writable `u16`.
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn add_residual_const_avx2(dst: *mut u16, dst_stride: usize, dc: u16, res: *const i32, w: usize, h: usize, max: i32) {
        let dcv = _mm256_set1_epi16(dc as i16);
        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        let nvec = w / 16;
        for y in 0..h {
            let r = unsafe { res.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let packed = _mm256_permute4x64_epi64(
                    _mm256_packs_epi32(unsafe { _mm256_loadu_si256(r.add(x) as *const __m256i) }, unsafe { _mm256_loadu_si256(r.add(x + 8) as *const __m256i) }),
                    0b11_01_10_00,
                );
                let v = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(dcv, packed), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, v) };
            }
            let mut x = nvec * 16;
            // Narrow blocks. `nvec = w / 16`, so everything under 16 samples
            // wide fell to the SCALAR tail -- and the census puts 18.3% of
            // pixel-kernel samples on intra-heavy content in blocks narrower
            // than 8, with another 31% exactly 8 wide (4:2:0 chroma of an 8x8
            // luma PU is 4 wide). 128- and 64-bit steps cover both.
            let dc8 = _mm_set1_epi16(dc as i16);
            let max8 = _mm_set1_epi16(max as i16);
            let z8 = _mm_setzero_si128();
            while x + 8 <= w {
                let p = _mm_packs_epi32(unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) }, unsafe { _mm_loadu_si128(r.add(x + 4) as *const __m128i) });
                let v = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(dc8, p), z8), max8);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
                x += 8;
            }
            if x + 4 <= w {
                let p = _mm_packs_epi32(unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) }, z8);
                let v = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(dc8, p), z8), max8);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < w {
                unsafe { *d.add(x) = (dc as i32 + *r.add(x)).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`add_residual_const_avx2`].
    #[target_feature(enable = "sse2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn add_residual_const_sse2(dst: *mut u16, dst_stride: usize, dc: u16, res: *const i32, w: usize, h: usize, max: i32) {
        let dcv = _mm_set1_epi16(dc as i16);
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        let nvec = w / 8;
        for y in 0..h {
            let r = unsafe { res.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 8;
                let rv = _mm_packs_epi32(unsafe { _mm_loadu_si128(r.add(x) as *const __m128i) }, unsafe { _mm_loadu_si128(r.add(x + 4) as *const __m128i) });
                let v = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(dcv, rv), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            while x < w {
                unsafe { *d.add(x) = (dc as i32 + *r.add(x)).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// `src` must have `w * h` readable `i16`, `dst` `dst_stride * (h - 1) + w`
    /// writable `u16`. `wt` and the rounding term must fit `i16` (the spec
    /// bounds both — see the scalar twin).
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn weighted_uni_avx2(dst: *mut u16, dst_stride: usize, src: *const i16, w: usize, h: usize, wt: i32, off: i32, log2wd: i32, max: i32) {
        let rnd = if log2wd >= 1 { 1 << (log2wd - 1) } else { 0 };
        // (x, 1) . (wt, rnd)  ==  x*wt + rnd, in one `pmaddwd`.
        let wv = _mm256_set1_epi32((wt & 0xffff) | (rnd << 16));
        let ones = _mm256_set1_epi16(1);
        let offv = _mm256_set1_epi32(off);
        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        let sh = _mm_cvtsi32_si128(log2wd.max(0));
        let nvec = w / 16;
        for y in 0..h {
            let sp = unsafe { src.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let v = unsafe { _mm256_loadu_si256(sp.add(x) as *const __m256i) };
                let lo = _mm256_add_epi32(_mm256_sra_epi32(_mm256_madd_epi16(_mm256_unpacklo_epi16(v, ones), wv), sh), offv);
                let hi = _mm256_add_epi32(_mm256_sra_epi32(_mm256_madd_epi16(_mm256_unpackhi_epi16(v, ones), wv), sh), offv);
                // `unpack` split the 256-bit vector per 128-bit lane and
                // `packs` rejoins it the same way, so no permute is needed.
                let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_packs_epi32(lo, hi), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
            }
            let mut x = nvec * 16;
            // Narrow blocks. `nvec = w / 16`, so everything under 16 samples
            // wide fell to the SCALAR tail -- and the census puts 18.3% of
            // pixel-kernel samples on intra-heavy content in blocks narrower
            // than 8, with another 31% exactly 8 wide (4:2:0 chroma of an 8x8
            // luma PU is 4 wide). 128- and 64-bit steps cover both.
            let wv8 = _mm_set1_epi32((wt & 0xffff) | (rnd << 16));
            let ones8 = _mm_set1_epi16(1);
            let off8 = _mm_set1_epi32(off);
            let max8 = _mm_set1_epi16(max as i16);
            let z8 = _mm_setzero_si128();
            while x + 8 <= w {
                let v = unsafe { _mm_loadu_si128(sp.add(x) as *const __m128i) };
                let lo = _mm_add_epi32(_mm_sra_epi32(_mm_madd_epi16(_mm_unpacklo_epi16(v, ones8), wv8), sh), off8);
                let hi = _mm_add_epi32(_mm_sra_epi32(_mm_madd_epi16(_mm_unpackhi_epi16(v, ones8), wv8), sh), off8);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_packs_epi32(lo, hi), z8), max8);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
                x += 8;
            }
            if x + 4 <= w {
                let v = unsafe { _mm_loadl_epi64(sp.add(x) as *const __m128i) };
                let lo = _mm_add_epi32(_mm_sra_epi32(_mm_madd_epi16(_mm_unpacklo_epi16(v, ones8), wv8), sh), off8);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_packs_epi32(lo, z8), z8), max8);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, r) };
                x += 4;
            }
            while x < w {
                let v = unsafe { *sp.add(x) } as i32;
                let r = if log2wd >= 1 { ((v * wt + rnd) >> log2wd) + off } else { v * wt + off };
                unsafe { *d.add(x) = r.clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`weighted_uni_avx2`], with `a` and `b` both `w * h` readable.
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn weighted_bi_avx2(dst: *mut u16, dst_stride: usize, a: *const i16, b: *const i16, w: usize, h: usize, w0: i32, w1: i32, obias: i32, log2wd: i32, max: i32) {
        // (x, z) . (w0, w1)  ==  x*w0 + z*w1, in one `pmaddwd`.
        let wv = _mm256_set1_epi32((w0 & 0xffff) | (w1 << 16));
        let ov = _mm256_set1_epi32(obias);
        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        let sh = _mm_cvtsi32_si128(log2wd + 1);
        let nvec = w / 16;
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * w) }, unsafe { b.add(y * w) });
            let d = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                let x = i * 16;
                let va = unsafe { _mm256_loadu_si256(pa.add(x) as *const __m256i) };
                let vb = unsafe { _mm256_loadu_si256(pb.add(x) as *const __m256i) };
                let lo = _mm256_sra_epi32(_mm256_add_epi32(_mm256_madd_epi16(_mm256_unpacklo_epi16(va, vb), wv), ov), sh);
                let hi = _mm256_sra_epi32(_mm256_add_epi32(_mm256_madd_epi16(_mm256_unpackhi_epi16(va, vb), wv), ov), sh);
                let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_packs_epi32(lo, hi), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
            }
            let mut x = nvec * 16;
            // The one kernel the previous narrow-block pass missed: it stepped
            // 16 and fell straight to scalar below that.
            let wv8 = _mm_set1_epi32((w0 & 0xffff) | (w1 << 16));
            let ov8 = _mm_set1_epi32(obias);
            let max8 = _mm_set1_epi16(max as i16);
            let z8 = _mm_setzero_si128();
            while x + 8 <= w {
                let va = unsafe { _mm_loadu_si128(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadu_si128(pb.add(x) as *const __m128i) };
                let lo = _mm_sra_epi32(_mm_add_epi32(_mm_madd_epi16(_mm_unpacklo_epi16(va, vb), wv8), ov8), sh);
                let hi = _mm_sra_epi32(_mm_add_epi32(_mm_madd_epi16(_mm_unpackhi_epi16(va, vb), wv8), ov8), sh);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_packs_epi32(lo, hi), z8), max8);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
                x += 8;
            }
            if x + 4 <= w {
                let va = unsafe { _mm_loadl_epi64(pa.add(x) as *const __m128i) };
                let vb = unsafe { _mm_loadl_epi64(pb.add(x) as *const __m128i) };
                let lo = _mm_sra_epi32(_mm_add_epi32(_mm_madd_epi16(_mm_unpacklo_epi16(va, vb), wv8), ov8), sh);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_packs_epi32(lo, lo), z8), max8);
                unsafe { _mm_storel_epi64(d.add(x) as *mut __m128i, r) };
                x += 4;
            }
            while x < w {
                let v = unsafe { *pa.add(x) } as i32 * w0 + unsafe { *pb.add(x) } as i32 * w1 + obias;
                unsafe { *d.add(x) = (v >> (log2wd + 1)).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// `d` must have `count` readable and writable `i32`.
    #[target_feature(enable = "avx2")]
    pub unsafe fn transform_skip_avx2(d: *mut i32, count: usize, shift: u32) {
        let add = _mm256_set1_epi32(1 << (shift - 1));
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = count / 8;
        for i in 0..nvec {
            let p = unsafe { d.add(i * 8) };
            let v = unsafe { _mm256_loadu_si256(p as *const __m256i) };
            let r = _mm256_sra_epi32(_mm256_add_epi32(_mm256_slli_epi32(v, 7), add), sh);
            unsafe { _mm256_storeu_si256(p as *mut __m256i, r) };
        }
        let mut i = nvec * 8;
        while i < count {
            unsafe { *d.add(i) = ((*d.add(i) << 7) + (1 << (shift - 1))) >> shift };
            i += 1;
        }
    }

    /// # Safety
    /// As [`transform_skip_avx2`].
    #[target_feature(enable = "sse2")]
    pub unsafe fn transform_skip_sse2(d: *mut i32, count: usize, shift: u32) {
        let add = _mm_set1_epi32(1 << (shift - 1));
        let sh = _mm_cvtsi32_si128(shift as i32);
        let nvec = count / 4;
        for i in 0..nvec {
            let p = unsafe { d.add(i * 4) };
            let v = unsafe { _mm_loadu_si128(p as *const __m128i) };
            let r = _mm_sra_epi32(_mm_add_epi32(_mm_slli_epi32(v, 7), add), sh);
            unsafe { _mm_storeu_si128(p as *mut __m128i, r) };
        }
        let mut i = nvec * 4;
        while i < count {
            unsafe { *d.add(i) = ((*d.add(i) << 7) + (1 << (shift - 1))) >> shift };
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// aarch64
// ---------------------------------------------------------------------------

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod arm {
    use std::arch::aarch64::*;

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn put_uni_neon(dst: *mut u16, dst_stride: usize, src: *const i16, w: usize, h: usize, bit_depth: u8) {
        let shift = 14i32 - bit_depth as i32;
        let off = vdupq_n_s16(1i16 << (shift - 1));
        let maxv = vdupq_n_s16(((1i32 << bit_depth) - 1) as i16);
        let zero = vdupq_n_s16(0);
        let sh = vdupq_n_s16(-(shift as i16));
        for y in 0..h {
            let s = unsafe { src.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let v = unsafe { vld1q_s16(s.add(x)) };
                let v = vqaddq_s16(v, off);
                let v = vshlq_s16(v, sh);
                let v = vminq_s16(vmaxq_s16(v, zero), maxv);
                unsafe { vst1q_u16(d.add(x), vreinterpretq_u16_s16(v)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *s.add(x) } as i32;
                let r = ((v + (1 << (shift - 1))) >> shift).clamp(0, (1 << bit_depth) - 1);
                unsafe { *d.add(x) = r as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn put_bi_neon(dst: *mut u16, dst_stride: usize, a: *const i16, b: *const i16, w: usize, h: usize, bit_depth: u8) {
        let shift = 15i32 - bit_depth as i32;
        let off = vdupq_n_s16(1i16 << (shift - 1));
        let maxv = vdupq_n_s16(((1i32 << bit_depth) - 1) as i16);
        let zero = vdupq_n_s16(0);
        let sh = vdupq_n_s16(-(shift as i16));
        for y in 0..h {
            let (pa, pb) = (unsafe { a.add(y * w) }, unsafe { b.add(y * w) });
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let va = unsafe { vld1q_s16(pa.add(x)) };
                let vb = unsafe { vld1q_s16(pb.add(x)) };
                let v = vqaddq_s16(vqaddq_s16(va, vb), off);
                let v = vshlq_s16(v, sh);
                let v = vminq_s16(vmaxq_s16(v, zero), maxv);
                unsafe { vst1q_u16(d.add(x), vreinterpretq_u16_s16(v)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *pa.add(x) } as i32 + unsafe { *pb.add(x) } as i32;
                let r = ((v + (1 << (shift - 1))) >> shift).clamp(0, (1 << bit_depth) - 1);
                unsafe { *d.add(x) = r as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn add_residual_neon(dst: *mut u16, dst_stride: usize, res: *const i32, w: usize, h: usize, max: i32) {
        let maxv = vdupq_n_s16(max as i16);
        let zero = vdupq_n_s16(0);
        for y in 0..h {
            let r = unsafe { res.add(y * w) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let r0 = unsafe { vld1q_s32(r.add(x)) };
                let r1 = unsafe { vld1q_s32(r.add(x + 4)) };
                let rv = vcombine_s16(vqmovn_s32(r0), vqmovn_s32(r1));
                let dv = unsafe { vreinterpretq_s16_u16(vld1q_u16(d.add(x))) };
                let v = vminq_s16(vmaxq_s16(vqaddq_s16(dv, rv), zero), maxv);
                unsafe { vst1q_u16(d.add(x), vreinterpretq_u16_s16(v)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *d.add(x) } as i32 + unsafe { *r.add(x) };
                unsafe { *d.add(x) = v.clamp(0, max) as u16 };
                x += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatchers
// ---------------------------------------------------------------------------

/// Uni-predicted block, default weighting (§8.5.3.3.4.2).
pub fn put_uni(dst: &mut [u16], dst_stride: usize, src: &[i16], w: usize, h: usize, bit_depth: u8) {
    if census::ALWAYS {
        // Width class of the block, weighted by samples. The vector loops step
        // 8 or 16 samples, so anything narrower falls to a scalar tail -- and
        // 4:2:0 chroma of an 8x8 luma PU is 4 wide.
        census::bump(
            if w < 8 {
                &census::PIX_W_LT8
            } else if w < 16 {
                &census::PIX_W_8
            } else {
                &census::PIX_W_GE16
            },
            (w * h) as u64,
        );
    }
    let ok = dst.len() >= dst_stride * (h - 1) + w && src.len() >= w * h;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::PUT_UNI_SIMD } else { &census::PUT_UNI_SCALAR }, 1);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: the length check above is exactly what the kernels touch.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::put_uni_avx2(dst.as_mut_ptr(), dst_stride, src.as_ptr(), w, h, bit_depth) },
            _ => return unsafe { x86::put_uni_sse2(dst.as_mut_ptr(), dst_stride, src.as_ptr(), w, h, bit_depth) },
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    if ok {
        // SAFETY: as above.
        return unsafe { arm::put_uni_neon(dst.as_mut_ptr(), dst_stride, src.as_ptr(), w, h, bit_depth) };
    }
    put_uni_scalar(dst, dst_stride, src, w, h, bit_depth);
}

/// Bi-predicted block, default weighting (§8.5.3.3.4.2).
pub fn put_bi(dst: &mut [u16], dst_stride: usize, a: &[i16], b: &[i16], w: usize, h: usize, bit_depth: u8) {
    if census::ALWAYS {
        // Width class of the block, weighted by samples. The vector loops step
        // 8 or 16 samples, so anything narrower falls to a scalar tail -- and
        // 4:2:0 chroma of an 8x8 luma PU is 4 wide.
        census::bump(
            if w < 8 {
                &census::PIX_W_LT8
            } else if w < 16 {
                &census::PIX_W_8
            } else {
                &census::PIX_W_GE16
            },
            (w * h) as u64,
        );
    }
    let ok = dst.len() >= dst_stride * (h - 1) + w && a.len() >= w * h && b.len() >= w * h;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::PUT_BI_SIMD } else { &census::PUT_BI_SCALAR }, 1);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: the length check above covers every access.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::put_bi_avx2(dst.as_mut_ptr(), dst_stride, a.as_ptr(), b.as_ptr(), w, h, bit_depth) },
            _ => return unsafe { x86::put_bi_sse2(dst.as_mut_ptr(), dst_stride, a.as_ptr(), b.as_ptr(), w, h, bit_depth) },
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    if ok {
        // SAFETY: as above.
        return unsafe { arm::put_bi_neon(dst.as_mut_ptr(), dst_stride, a.as_ptr(), b.as_ptr(), w, h, bit_depth) };
    }
    put_bi_scalar(dst, dst_stride, a, b, w, h, bit_depth);
}

/// Adds a transform block's residual into the picture, with the clip of
/// §8.6.6.
pub fn add_residual(dst: &mut [u16], dst_stride: usize, res: &[i32], w: usize, h: usize, max: i32) {
    if census::ALWAYS {
        // Width class of the block, weighted by samples. The vector loops step
        // 8 or 16 samples, so anything narrower falls to a scalar tail -- and
        // 4:2:0 chroma of an 8x8 luma PU is 4 wide.
        census::bump(
            if w < 8 {
                &census::PIX_W_LT8
            } else if w < 16 {
                &census::PIX_W_8
            } else {
                &census::PIX_W_GE16
            },
            (w * h) as u64,
        );
    }
    let ok = dst.len() >= dst_stride * (h - 1) + w && res.len() >= w * h;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::ADD_RESIDUAL_SIMD } else { &census::ADD_RESIDUAL_SCALAR }, 1);
        census::bump(&census::SAMPLES_ADD_RESIDUAL, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: the length check above covers every access.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::add_residual_avx2(dst.as_mut_ptr(), dst_stride, res.as_ptr(), w, h, max) },
            _ => return unsafe { x86::add_residual_sse2(dst.as_mut_ptr(), dst_stride, res.as_ptr(), w, h, max) },
        }
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    if ok {
        // SAFETY: as above.
        return unsafe { arm::add_residual_neon(dst.as_mut_ptr(), dst_stride, res.as_ptr(), w, h, max) };
    }
    add_residual_scalar(dst, dst_stride, res, w, h, max);
}

/// Copy a `w x h` rectangle of samples — full-pel uni-prediction. See
/// [`copy_block_scalar`] for why the two kernels it replaces compose to this.
pub fn copy_block(dst: &mut [u16], dst_stride: usize, src: &[u16], src_stride: usize, w: usize, h: usize) {
    if census::ALWAYS {
        census::bump(&census::MC_FULLPEL_UNI, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
    }
    // A per-row `memcpy` is already the best primitive available; the win is
    // not running the shift-and-clamp pass at all, not the copy itself.
    copy_block_scalar(dst, dst_stride, src, src_stride, w, h);
}

/// Rounding average of two `w x h` rectangles — full-pel bi-prediction.
#[allow(clippy::too_many_arguments)]
pub fn avg_block(dst: &mut [u16], dst_stride: usize, a: &[u16], a_stride: usize, b: &[u16], b_stride: usize, w: usize, h: usize) {
    let ok = w > 0 && h > 0 && dst.len() >= dst_stride * (h - 1) + w && a.len() >= a_stride * (h - 1) + w && b.len() >= b_stride * (h - 1) + w;
    debug_assert!(ok);
    if census::ALWAYS {
        census::bump(&census::MC_FULLPEL_BI, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: lengths checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::avg_block_avx2(dst.as_mut_ptr(), dst_stride, a.as_ptr(), a_stride, b.as_ptr(), b_stride, w, h) },
            _ => return unsafe { x86::avg_block_sse2(dst.as_mut_ptr(), dst_stride, a.as_ptr(), a_stride, b.as_ptr(), b_stride, w, h) },
        }
    }
    avg_block_scalar(dst, dst_stride, a, a_stride, b, b_stride, w, h);
}

/// Bi-prediction with one full-pel list — see [`put_bi_fp_scalar`] for why the
/// `copy_shift` pass it replaces was pure overhead.
#[allow(clippy::too_many_arguments)]
pub fn put_bi_fp(dst: &mut [u16], dst_stride: usize, s: &[u16], s_stride: usize, b: &[i16], w: usize, h: usize, bit_depth: u8) {
    let ok = w > 0 && h > 0 && dst.len() >= dst_stride * (h - 1) + w && s.len() >= s_stride * (h - 1) + w && b.len() >= w * h;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::PUT_BI_FP_SIMD } else { &census::PUT_BI_FP_SCALAR }, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: lengths checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::put_bi_fp_avx2(dst.as_mut_ptr(), dst_stride, s.as_ptr(), s_stride, b.as_ptr(), w, h, bit_depth) },
            _ => return unsafe { x86::put_bi_fp_sse2(dst.as_mut_ptr(), dst_stride, s.as_ptr(), s_stride, b.as_ptr(), w, h, bit_depth) },
        }
    }
    put_bi_fp_scalar(dst, dst_stride, s, s_stride, b, w, h, bit_depth);
}

/// DC prediction fused with the residual add — see [`add_residual_const_scalar`].
#[allow(clippy::too_many_arguments)]
pub fn add_residual_const(dst: &mut [u16], dst_stride: usize, dc: u16, res: &[i32], w: usize, h: usize, max: i32) {
    let ok = w > 0 && h > 0 && dst.len() >= dst_stride * (h - 1) + w && res.len() >= w * h;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::ADD_RESIDUAL_DC_SIMD } else { &census::ADD_RESIDUAL_DC_SCALAR }, 1);
        census::bump(&census::SAMPLES_ADD_RESIDUAL, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: lengths checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::add_residual_const_avx2(dst.as_mut_ptr(), dst_stride, dc, res.as_ptr(), w, h, max) },
            _ => return unsafe { x86::add_residual_const_sse2(dst.as_mut_ptr(), dst_stride, dc, res.as_ptr(), w, h, max) },
        }
    }
    add_residual_const_scalar(dst, dst_stride, dc, res, w, h, max);
}

/// Explicit weighted prediction, uni. See [`weighted_uni_scalar`].
///
/// This path had NO kernel and a large population: on `WP_A_Toshiba_3` every one
/// of 33.9 M motion-compensated samples goes through it. It read as unreachable
/// only because its census counter was declared and never incremented — a dead
/// counter, not a cold path.
#[allow(clippy::too_many_arguments)]
pub fn weighted_uni(dst: &mut [u16], dst_stride: usize, src: &[i16], w: usize, h: usize, wt: i32, off: i32, log2wd: i32, max: i32) {
    // The `pmaddwd` form needs both multiplicands inside `i16`; the spec bounds
    // them, but a stream that violated it must take the scalar twin, not wrap.
    let fits = (-32768..=32767).contains(&wt) && (1..30).contains(&log2wd) && (1i32 << (log2wd - 1)) <= 32767;
    let ok = w > 0 && h > 0 && dst.len() >= dst_stride * (h - 1) + w && src.len() >= w * h;
    if census::ALWAYS {
        // The predicate must match the dispatch below EXACTLY, switch
        // included, or the census reports an arm that never ran.
        let simd = cfg!(feature = "simd") && crate::isa() == crate::Isa::Avx2 && ok && fits && !scalar_gate();
        census::bump(if simd { &census::PUT_WEIGHTED_SIMD } else { &census::PUT_WEIGHTED_SCALAR }, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok && fits && !scalar_gate() && crate::isa() == crate::Isa::Avx2 {
        // SAFETY: lengths and the `i16` bounds are checked above.
        return unsafe { x86::weighted_uni_avx2(dst.as_mut_ptr(), dst_stride, src.as_ptr(), w, h, wt, off, log2wd, max) };
    }
    weighted_uni_scalar(dst, dst_stride, src, w, h, wt, off, log2wd, max);
}

/// Explicit weighted prediction, bi. See [`weighted_bi_scalar`].
#[allow(clippy::too_many_arguments)]
pub fn weighted_bi(dst: &mut [u16], dst_stride: usize, a: &[i16], b: &[i16], w: usize, h: usize, w0: i32, w1: i32, obias: i32, log2wd: i32, max: i32) {
    let fits = (-32768..=32767).contains(&w0) && (-32768..=32767).contains(&w1) && (0..30).contains(&log2wd);
    let ok = w > 0 && h > 0 && dst.len() >= dst_stride * (h - 1) + w && a.len() >= w * h && b.len() >= w * h;
    if census::ALWAYS {
        // The predicate must match the dispatch below EXACTLY, switch
        // included, or the census reports an arm that never ran.
        let simd = cfg!(feature = "simd") && crate::isa() == crate::Isa::Avx2 && ok && fits && !scalar_gate();
        census::bump(if simd { &census::PUT_WEIGHTED_SIMD } else { &census::PUT_WEIGHTED_SCALAR }, 1);
        census::bump(&census::SAMPLES_MC, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok && fits && !scalar_gate() && crate::isa() == crate::Isa::Avx2 {
        // SAFETY: as above.
        return unsafe { x86::weighted_bi_avx2(dst.as_mut_ptr(), dst_stride, a.as_ptr(), b.as_ptr(), w, h, w0, w1, obias, log2wd, max) };
    }
    weighted_bi_scalar(dst, dst_stride, a, b, w, h, w0, w1, obias, log2wd, max);
}

/// Transform skip. See [`transform_skip_scalar`] for why this path matters.
pub fn transform_skip(d: &mut [i32], n: usize, shift: u32) {
    let count = n * n;
    let ok = count > 0 && d.len() >= count && (1..31).contains(&shift);
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok && !scalar_gate();
        census::bump(if simd { &census::TX_SKIP_SIMD } else { &census::TX_SKIP_SCALAR }, 1);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok && !scalar_gate() {
        // SAFETY: length and shift range checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::transform_skip_avx2(d.as_mut_ptr(), count, shift) },
            _ => return unsafe { x86::transform_skip_sse2(d.as_mut_ptr(), count, shift) },
        }
    }
    transform_skip_scalar(d, n, shift);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *s
    }

    /// Values that sweep the whole `i16` range, including the saturation
    /// boundary the module header argues about.
    fn spread(s: &mut u32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| match i % 8 {
                0 => i16::MIN,
                1 => i16::MAX,
                2 => 32_700,
                3 => -32_700,
                _ => (lcg(s) >> 15) as i16,
            })
            .collect()
    }

    #[test]
    fn put_uni_matches_scalar() {
        let mut st = 0x1357_9bdfu32;
        for &bd in &[8u8, 10] {
            for &(w, h) in &[(4, 4), (8, 8), (12, 6), (16, 16), (24, 3), (32, 32), (64, 64), (5, 7)] {
                let stride = w + 9;
                let src = spread(&mut st, w * h);
                let mut a = vec![7u16; stride * h + 16];
                let mut b = a.clone();
                put_uni_scalar(&mut a, stride, &src, w, h, bd);
                put_uni(&mut b, stride, &src, w, h, bd);
                assert_eq!(a, b, "put_uni {w}x{h} bd={bd}");
            }
        }
    }

    #[test]
    fn put_bi_matches_scalar() {
        let mut st = 0x2468_ace0u32;
        for &bd in &[8u8, 10] {
            for &(w, h) in &[(4, 4), (8, 8), (12, 6), (16, 16), (24, 3), (32, 32), (64, 64), (5, 7)] {
                let stride = w + 9;
                let (pa, pb) = (spread(&mut st, w * h), spread(&mut st, w * h));
                let mut a = vec![7u16; stride * h + 16];
                let mut b = a.clone();
                put_bi_scalar(&mut a, stride, &pa, &pb, w, h, bd);
                put_bi(&mut b, stride, &pa, &pb, w, h, bd);
                assert_eq!(a, b, "put_bi {w}x{h} bd={bd}");
            }
        }
    }

    #[test]
    fn add_residual_matches_scalar() {
        let mut st = 0x0f0f_1e1eu32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &(w, h) in &[(4, 4), (8, 8), (16, 16), (32, 32), (5, 3), (12, 12)] {
                let stride = w + 9;
                let res: Vec<i32> = (0..w * h)
                    .map(|i| match i % 6 {
                        0 => 40_000,
                        1 => -40_000,
                        _ => (lcg(&mut st) >> 16) as i32 - 32_768,
                    })
                    .collect();
                let base: Vec<u16> = (0..stride * h + 16).map(|_| (lcg(&mut st) >> 20) as u16 % (max as u16 + 1)).collect();
                let mut a = base.clone();
                let mut b = base;
                add_residual_scalar(&mut a, stride, &res, w, h, max);
                add_residual(&mut b, stride, &res, w, h, max);
                assert_eq!(a, b, "add_residual {w}x{h} bd={bd}");
            }
        }
    }

    /// The whole justification for these two kernels is that they replace a
    /// composition exactly. Filtering a full-pel position and then applying the
    /// uni/bi write must give the same samples as copying / averaging directly
    /// — if it ever does not, the fast path in the decoder is silently wrong.
    #[test]
    fn full_pel_composition_is_identity() {
        let mut st = 0x2468_ace0u32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &(w, h) in &[(4usize, 4usize), (8, 8), (16, 12), (32, 8), (64, 64), (5, 3)] {
                let (ss, ds) = (w + 7, w + 3);
                let a: Vec<u16> = (0..ss * h + 8).map(|_| (lcg(&mut st) as i32 & max) as u16).collect();
                let b: Vec<u16> = (0..ss * h + 8).map(|_| (lcg(&mut st) as i32 & max) as u16).collect();

                // What the general path produces: `s << shift3`, then the write.
                let shift3 = (14u32).saturating_sub(bd as u32).max(2);
                let mut pa: Vec<i16> = vec![0; w * h];
                let mut pb: Vec<i16> = vec![0; w * h];
                for y in 0..h {
                    for x in 0..w {
                        pa[y * w + x] = ((a[y * ss + x] as i32) << shift3) as i16;
                        pb[y * w + x] = ((b[y * ss + x] as i32) << shift3) as i16;
                    }
                }

                let mut want = vec![0u16; ds * h];
                put_uni(&mut want, ds, &pa, w, h, bd);
                let mut got = vec![0u16; ds * h];
                copy_block(&mut got, ds, &a, ss, w, h);
                assert_eq!(want, got, "uni {w}x{h} bd={bd}");

                let mut want = vec![0u16; ds * h];
                put_bi(&mut want, ds, &pa, &pb, w, h, bd);
                let mut got = vec![0u16; ds * h];
                avg_block(&mut got, ds, &a, ss, &b, ss, w, h);
                assert_eq!(want, got, "bi {w}x{h} bd={bd}");
            }
        }
    }

    /// The fold must be exact: shifting into a scratch buffer and running the
    /// general bi write has to give the same samples as the fused kernel.
    #[test]
    fn put_bi_fp_matches_the_composition() {
        let mut st = 0x1357_9bdfu32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            let k = 14u32 - bd as u32;
            for &(w, h) in &[(4usize, 4usize), (8, 8), (16, 16), (32, 12), (64, 4), (7, 5)] {
                let (ss, ds) = (w + 6, w + 2);
                let src: Vec<u16> = (0..ss * h + 8).map(|_| (lcg(&mut st) as i32 & max) as u16).collect();
                let b: Vec<i16> = (0..w * h).map(|_| ((lcg(&mut st) >> 8) as i32 % 20000 - 10000) as i16).collect();

                // The general path: materialise the full-pel list, then put_bi.
                let mut a: Vec<i16> = vec![0; w * h];
                for y in 0..h {
                    for x in 0..w {
                        a[y * w + x] = ((src[y * ss + x] as i32) << k) as i16;
                    }
                }
                let mut want = vec![0u16; ds * h];
                put_bi(&mut want, ds, &a, &b, w, h, bd);

                let mut got = vec![0u16; ds * h];
                put_bi_fp(&mut got, ds, &src, ss, &b, w, h, bd);
                assert_eq!(want, got, "{w}x{h} bd={bd}");
            }
        }
    }

    /// Fusing may not change a sample: filling the block with the DC value and
    /// then adding the residual must equal the fused kernel.
    #[test]
    fn add_residual_const_matches_fill_then_add() {
        let mut st = 0x0f1e_2d3cu32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &n in &[4usize, 8, 16, 32] {
                let ds = n + 5;
                let dc = (lcg(&mut st) as i32 & max) as u16;
                let res: Vec<i32> = (0..n * n).map(|_| (lcg(&mut st) >> 12) as i32 % 2048 - 1024).collect();

                let mut want = vec![0u16; ds * n];
                for y in 0..n {
                    want[y * ds..y * ds + n].fill(dc);
                }
                add_residual(&mut want, ds, &res, n, n, max);

                let mut got = vec![0u16; ds * n];
                add_residual_const(&mut got, ds, dc, &res, n, n, max);
                assert_eq!(want, got, "n={n} bd={bd}");
            }
        }
    }

    #[test]
    fn weighted_matches_scalar() {
        let mut st = 0x5a5a_1234u32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            let shift1 = 14i32 - bd as i32;
            for &denom in &[0i32, 3, 7] {
                let log2wd = denom + shift1;
                for &(w, h) in &[(4usize, 4usize), (8, 8), (16, 16), (32, 24), (64, 8), (9, 3)] {
                    let ds = w + 3;
                    let n = w * h;
                    let a: Vec<i16> = (0..n).map(|_| ((lcg(&mut st) >> 8) as i32 % 30000 - 15000) as i16).collect();
                    let b: Vec<i16> = (0..n).map(|_| ((lcg(&mut st) >> 8) as i32 % 30000 - 15000) as i16).collect();
                    // Spec ranges: weight = (1<<denom) + delta, delta in -128..=127.
                    let w0 = (1 << denom) + ((lcg(&mut st) % 256) as i32 - 128);
                    let w1 = (1 << denom) + ((lcg(&mut st) % 256) as i32 - 128);
                    let o0 = (lcg(&mut st) % 256) as i32 - 128;
                    let o1 = (lcg(&mut st) % 256) as i32 - 128;

                    let mut want = vec![0u16; ds * h];
                    let mut got = vec![0u16; ds * h];
                    weighted_uni_scalar(&mut want, ds, &a, w, h, w0, o0, log2wd, max);
                    weighted_uni(&mut got, ds, &a, w, h, w0, o0, log2wd, max);
                    assert_eq!(want, got, "uni {w}x{h} bd={bd} denom={denom}");

                    let obias = (o0 + o1 + 1) << log2wd;
                    let mut want = vec![0u16; ds * h];
                    let mut got = vec![0u16; ds * h];
                    weighted_bi_scalar(&mut want, ds, &a, &b, w, h, w0, w1, obias, log2wd, max);
                    weighted_bi(&mut got, ds, &a, &b, w, h, w0, w1, obias, log2wd, max);
                    assert_eq!(want, got, "bi {w}x{h} bd={bd} denom={denom}");
                }
            }
        }
    }

    #[test]
    fn transform_skip_matches_scalar() {
        let mut st = 0x7788_99aau32;
        for &bd in &[8u8, 10] {
            let shift = 20u32 - bd as u32;
            for &n in &[4usize, 8, 16, 32] {
                let a: Vec<i32> = (0..n * n).map(|_| (lcg(&mut st) >> 10) as i32 - 2_000_000).collect();
                let mut want = a.clone();
                let mut got = a.clone();
                transform_skip_scalar(&mut want, n, shift);
                transform_skip(&mut got, n, shift);
                assert_eq!(want, got, "n={n} bd={bd}");
            }
        }
    }
}

#[cfg(test)]
mod bench_combine {
    //! Prices the combine step, to decide whether fusing it into the
    //! interpolation's final pass is worth three kernel variants per ISA rung.
    //!
    //! Not a `#[bench]`: run it explicitly.
    //!   cargo test --release -p rusty_h265-accel -- --ignored --nocapture combine
    use std::time::Instant;

    /// Sizes weighted the way the census says a real clip uses them: 93% of
    /// motion-compensated samples are in blocks 16 or wider, and every luma
    /// block brings two chroma blocks at half its dimensions.
    const SIZES: [(usize, usize, u64); 4] = [(32, 32, 3), (16, 16, 6), (16, 8, 3), (8, 8, 6)];

    #[test]
    #[ignore = "timing, not correctness"]
    fn combine_cost() {
        let bd = 8u8;
        for (w, h, weight) in SIZES {
            let src = vec![1234i16; w * h];
            let mut dst = vec![0u16; w * h * 2];
            let iters = 200_000u64;
            // Warm.
            for _ in 0..1000 {
                super::put_uni(&mut dst, w, &src, w, h, bd);
            }
            let t = Instant::now();
            for _ in 0..iters {
                super::put_uni(&mut dst, w, &src, w, h, bd);
                std::hint::black_box(&dst);
            }
            let ns = t.elapsed().as_nanos() as f64 / iters as f64;
            println!("put_uni {w:>2}x{h:<2} weight {weight}  {ns:7.1} ns/call  {:5.2} ns/sample", ns / (w * h) as f64);
        }
    }
}
