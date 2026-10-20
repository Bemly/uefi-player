//! Intra prediction kernels (§8.4.4.2.5–8.4.4.2.6): angular, planar and the
//! DC fill.
//!
//! # Why the compiler cannot do these
//!
//! `intra::predict` compiles to ~1000 instructions containing **17 SIMD-register
//! instructions, every one of them a `movups` or `xorps`** — not one packed
//! arithmetic op. Three separate reasons, one per shape:
//!
//! - **Angular, modes below 18.** The main reference is the left column, so the
//!   inner loop walks `y` and stores to `out[y * stride + x]` — a scatter. LLVM
//!   will not vectorise a strided store here. We compute row-major into scratch
//!   and transpose in 8×8 tiles instead, which is 24 unpacks per 64 samples.
//! - **Angular, modes 18 and up.** Loads and stores are contiguous, but the
//!   arithmetic is `i32` because nothing tells the compiler the samples fit
//!   `i16`. The spec does: Main and Main 10 samples are at most 1023, and the
//!   two weights sum to 32, so `(32 − f)·a + f·b ≤ 32 · 1023 = 32,736`. That is
//!   exactly one `pmaddwd` — the same two-tap contraction the MC kernels use.
//! - **Planar and DC.** Both shift by `log2n + 1`, a **variable** shift amount,
//!   which LLVM's cost model treats as a vectorisation blocker.
//!
//! # The bound that makes `i16` legal
//!
//! Every kernel here takes samples already narrowed to `i16`. That is sound for
//! HEVC version 1 (Main, Main 10: `BitDepth ≤ 10`, so a sample is at most 1023)
//! and the debug assertions check it. A future range extension profile with 12+
//! bit samples must take the scalar twin — [`angular`] and friends fall back on
//! their own bounds checks rather than trusting the caller.

use crate::census;

/// The largest block these kernels handle, and the reference buffer around it:
/// indices run −N..=2N, so 3N + 1 entries.
pub const MAX_N: usize = 32;
pub const REF_LEN: usize = 3 * MAX_N + 1;

/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream -- the `*_SCALAR` census counters read 0 across the corpus -- so this
/// is the fallback for a shape the kernels decline, not a path decode takes.
/// Inlined it padded the dispatcher, which IS on the hot path, with a body that
/// never runs. Same reason `Cabac::refill_tail` is out of line.
#[cold]
#[inline(never)]
fn angular_scalar(dst: &mut [u16], dst_stride: usize, n: usize, refb: &[i16], off: usize, angle: i32) {
    for i in 0..n {
        let pos = (i as i32 + 1) * angle;
        let iidx = pos >> 5;
        let ifact = pos & 31;
        let base = (off as i32 + iidx + 1) as usize;
        let row = &mut dst[i * dst_stride..];
        if ifact == 0 {
            for j in 0..n {
                row[j] = refb[base + j] as u16;
            }
        } else {
            for j in 0..n {
                let a = refb[base + j] as i32;
                let b = refb[base + j + 1] as i32;
                row[j] = (((32 - ifact) * a + ifact * b + 16) >> 5) as u16;
            }
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
fn planar_scalar(dst: &mut [u16], dst_stride: usize, n: usize, left: &[i16], top: &[i16], log2n: u32) {
    let tn = top[n] as i32;
    let ln = left[n] as i32;
    for y in 0..n {
        let l = left[y] as i32;
        let k = (n - 1 - y) as i32;
        let c = (y as i32 + 1) * ln + n as i32;
        for x in 0..n {
            let v = (n - 1 - x) as i32 * l + (x as i32 + 1) * tn + k * top[x] as i32 + c;
            dst[y * dst_stride + x] = (v >> (log2n + 1)) as u16;
        }
    }
}

fn dc_fill_scalar(dst: &mut [u16], dst_stride: usize, n: usize, dc: u16) {
    for y in 0..n {
        dst[y * dst_stride..y * dst_stride + n].fill(dc);
    }
}

fn angular_t_scalar(dst: &mut [u16], dst_stride: usize, n: usize, refb: &[i16], off: usize, angle: i32) {
    for i in 0..n {
        let pos = (i as i32 + 1) * angle;
        let iidx = pos >> 5;
        let ifact = pos & 31;
        let base = (off as i32 + iidx + 1) as usize;
        for j in 0..n {
            let a = refb[base + j] as i32;
            let v = if ifact == 0 {
                a
            } else {
                let b = refb[base + j + 1] as i32;
                ((32 - ifact) * a + ifact * b + 16) >> 5
            };
            dst[j * dst_stride + i] = v as u16;
        }
    }
}

fn transpose_scalar(dst: &mut [u16], dst_stride: usize, src: &[u16], src_stride: usize, n: usize) {
    for i in 0..n {
        for j in 0..n {
            dst[j * dst_stride + i] = src[i * src_stride + j];
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod x86 {
    use std::arch::x86_64::*;

    /// One row of an angular prediction: `n` outputs from `refb[base..]`.
    ///
    /// # The two-tap filter never has to leave `i16`
    ///
    /// Written literally, `((32 − f)·a + f·b + 16) >> 5` needs 32-bit products,
    /// which means `pmaddwd` on an even/odd pair of shifted loads and then an
    /// `unpack`/`packs` dance to put the results back in picture order — 13
    /// vector instructions for 8 samples.
    ///
    /// But the weights sum to 32, so
    ///
    /// ```text
    ///   (32 − f)·a + f·b  =  32·a + f·(b − a)
    /// ```
    ///
    /// and 32·a is an exact multiple of the shift, so it factors straight out:
    ///
    /// ```text
    ///   ((32 − f)·a + f·b + 16) >> 5  ==  a + ((f·(b − a) + 16) >> 5)
    /// ```
    ///
    /// This is an identity, not an approximation — `floor((32a + Y)/32)` is
    /// exactly `a + floor(Y/32)`. And every term now fits `i16`: `f ≤ 31` and
    /// `|b − a| ≤ 1023` for Main/Main 10, so `|f·(b − a)| ≤ 31,713`. That
    /// leaves one `pmullw`, no widening, no narrowing, and no shuffles.
    ///
    /// # Safety
    /// `refb.add(base)` must have `n + 8` readable `i16`, and `row` `n`
    /// writable `u16`.
    #[target_feature(enable = "sse2")]
    unsafe fn angular_row_sse2(row: *mut u16, n: usize, refb: *const i16, ifact: i32) {
        let f = _mm_set1_epi16(-(ifact as i16));
        let rnd = _mm_set1_epi16(16);
        // Two vectors per trip: the filter is four instructions, so the loop's
        // own three were nearly half of it.
        let nvec = n / 8;
        for i in 0..nvec / 2 {
            for half in 0..2usize {
                let j = i * 16 + half * 8;
                let p = unsafe { refb.add(j) };
                let a = unsafe { _mm_loadu_si128(p as *const __m128i) };
                let b = unsafe { _mm_loadu_si128(p.add(1) as *const __m128i) };
                let m = _mm_srai_epi16(_mm_add_epi16(_mm_mullo_epi16(_mm_sub_epi16(a, b), f), rnd), 5);
                unsafe { _mm_storeu_si128(row.add(j) as *mut __m128i, _mm_add_epi16(a, m)) };
            }
        }
        if nvec % 2 == 1 {
            let j = (nvec - 1) * 8;
            let p = unsafe { refb.add(j) };
            let a = unsafe { _mm_loadu_si128(p as *const __m128i) };
            let b = unsafe { _mm_loadu_si128(p.add(1) as *const __m128i) };
            let m = _mm_srai_epi16(_mm_add_epi16(_mm_mullo_epi16(_mm_sub_epi16(a, b), f), rnd), 5);
            unsafe { _mm_storeu_si128(row.add(j) as *mut __m128i, _mm_add_epi16(a, m)) };
        }
        let mut j = nvec * 8;
        if j + 4 <= n {
            // Reads 8 lanes and keeps 4; `refb` always has the slack (the
            // reference array is 3N + 1 long and this is at most index 2N + 1).
            let p = unsafe { refb.add(j) };
            let a = unsafe { _mm_loadu_si128(p as *const __m128i) };
            let b = unsafe { _mm_loadu_si128(p.add(1) as *const __m128i) };
            let m = _mm_srai_epi16(_mm_add_epi16(_mm_mullo_epi16(_mm_sub_epi16(a, b), f), rnd), 5);
            unsafe { _mm_storel_epi64(row.add(j) as *mut __m128i, _mm_add_epi16(a, m)) };
            j += 4;
        }
        while j < n {
            let a = unsafe { *refb.add(j) } as i32;
            let b = unsafe { *refb.add(j + 1) } as i32;
            unsafe { *row.add(j) = (((32 - ifact) * a + ifact * b + 16) >> 5) as u16 };
            j += 1;
        }
    }

    /// # Safety
    /// As [`angular_row_sse2`], for every row of the block.
    #[target_feature(enable = "sse2")]
    pub unsafe fn angular_sse2(dst: *mut u16, dst_stride: usize, n: usize, refb: *const i16, off: usize, angle: i32) {
        for i in 0..n {
            let pos = (i as i32 + 1) * angle;
            let iidx = pos >> 5;
            let ifact = pos & 31;
            let base = (off as i32 + iidx + 1) as usize;
            let row = unsafe { dst.add(i * dst_stride) };
            let src = unsafe { refb.add(base) };
            // Route: a whole-sample angle needs no filter at all, only a copy.
            crate::census::route(ifact == 0, &crate::census::RT_INTRA_ANG_COPY, &crate::census::RT_INTRA_ANG_ROW);
            if ifact == 0 {
                // A pure copy: no filter, so no arithmetic to vectorise beyond
                // the move itself.
                let mut j = 0usize;
                while j + 8 <= n {
                    unsafe { _mm_storeu_si128(row.add(j) as *mut __m128i, _mm_loadu_si128(src.add(j) as *const __m128i)) };
                    j += 8;
                }
                while j < n {
                    unsafe { *row.add(j) = *src.add(j) as u16 };
                    j += 1;
                }
            } else {
                unsafe { angular_row_sse2(row, n, src, ifact) };
            }
        }
    }

    /// # Safety
    /// `left` and `top` must have `n + 1` readable `i16`; `dst` an `n × n`
    /// block at `dst_stride`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn planar_sse2(dst: *mut u16, dst_stride: usize, n: usize, left: *const i16, top: *const i16, log2n: u32) {
        let tn = unsafe { *top.add(n) } as i32;
        let ln = unsafe { *left.add(n) } as i32;
        let zero = _mm_setzero_si128();
        let sh = _mm_cvtsi32_si128(log2n as i32 + 1);
        // `ramp[x] = (n − 1 − x, x + 1)` interleaved, so one `pmaddwd` against
        // the weights `(left[y], top[n])` produces both x-dependent products.
        let mut ramp = [0i16; 2 * super::MAX_N];
        for x in 0..n {
            ramp[2 * x] = (n - 1 - x) as i16;
            ramp[2 * x + 1] = (x + 1) as i16;
        }
        for y in 0..n {
            let l = unsafe { *left.add(y) } as i32;
            let k = _mm_set1_epi32((n - 1 - y) as i32);
            let c = _mm_set1_epi32((y as i32 + 1) * ln + n as i32);
            let w = _mm_set1_epi32((l & 0xffff) | (tn << 16));
            let row = unsafe { dst.add(y * dst_stride) };
            // Two vectors per store, for the same reason the AVX2 twin pairs:
            // four `i32` lanes only HALF-fill a 128-bit store, so one vector per
            // trip paid a `packs` and a half-width `storel` for every four
            // samples. Two fill the store exactly and pay that pair once for
            // eight. `packs` does not cross lanes on 128-bit, so unlike the
            // AVX2 version this needs no permute to put the halves in order.
            let term = |x: usize| {
                let rp = unsafe { _mm_loadu_si128(ramp.as_ptr().add(2 * x) as *const __m128i) };
                let xterm = _mm_madd_epi16(rp, w);
                // Zero-extend four `top` samples to `i32`, then `k * top` via
                // the same contraction (the odd lane is zero, so it adds
                // nothing).
                let t = unsafe { _mm_loadl_epi64(top.add(x) as *const __m128i) };
                let t32 = _mm_unpacklo_epi16(t, zero);
                let yterm = _mm_madd_epi16(t32, k);
                _mm_sra_epi32(_mm_add_epi32(_mm_add_epi32(xterm, yterm), c), sh)
            };
            let nvec = n / 4;
            for i in 0..nvec / 2 {
                let x = i * 8;
                let v0 = term(x);
                let v1 = term(x + 4);
                unsafe { _mm_storeu_si128(row.add(x) as *mut __m128i, _mm_packs_epi32(v0, v1)) };
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 4;
                let v = term(x);
                unsafe { _mm_storel_epi64(row.add(x) as *mut __m128i, _mm_packs_epi32(v, v)) };
            }
            let mut x = nvec * 4;
            while x < n {
                let v = (n - 1 - x) as i32 * l + (x as i32 + 1) * tn + (n - 1 - y) as i32 * unsafe { *top.add(x) } as i32 + (y as i32 + 1) * ln + n as i32;
                unsafe { *row.add(x) = (v >> (log2n + 1)) as u16 };
                x += 1;
            }
        }
    }

    /// One row of an angular prediction, AVX2: 16 samples per iteration.
    ///
    /// Same identity as [`angular_row_sse2`] — `a + ((f·(b − a) + 16) >> 5)` —
    /// which is what makes a 256-bit version worth writing at all: staying in
    /// `i16` means the wider register doubles the samples per instruction
    /// instead of merely doubling the lanes of a widened intermediate.
    ///
    /// # Safety
    /// As [`angular_row_sse2`].
    #[target_feature(enable = "avx2")]
    /// The two-tap angular interpolation, in `i16`, with the weight NEGATED.
    ///
    /// HEVC 8.4.4.2.6 specifies `((32 - f)*a + f*b + 16) >> 5`. Two rewrites turn
    /// that into six instructions:
    ///
    /// ```text
    ///   ((32 - f)*a + f*b + 16) >> 5  ==  a + (( f*(b - a) + 16) >> 5)     (i)
    ///                                 ==  a + ((-f*(a - b) + 16) >> 5)     (ii)
    /// ```
    ///
    /// (i) removes a multiply. (ii) looks like a no-op and is not: `a` is used
    /// twice (the difference and the final add) so it has to sit in a register,
    /// while `b` is used once. AT&T `vpsubw src2, src1, dst` computes `src1 - src2`
    /// and only `src2` may be a memory operand -- so in form (i) the single-use
    /// value `b` is `src1` and CANNOT fold, while in form (ii) it is `src2` and
    /// does. One load per vector stops being an instruction.
    ///
    /// ## The headroom this rests on
    ///
    /// `mullo_epi16` keeps the low 16 bits, so `f * (a - b)` must fit `i16`:
    ///
    /// | bit depth | max |a - b| | x31 | fits i16? |
    /// |---|---:|---:|---|
    /// | 8 | 255 | 7,905 | yes |
    /// | 10 | 1,023 | 31,713 | yes, with 1,054 to spare |
    /// | 12 | 4,095 | 126,945 | **NO -- wraps** |
    ///
    /// The decoder rejects `bit_depth > 10` at the SPS (`decoder.rs`, Main/Main 10
    /// only), which is the ONLY reason this is sound. That check is load-bearing
    /// for these kernels and nothing here would fail if it were relaxed for RExt --
    /// the scalar twin computes in `i32` and stays correct, so the twins would
    /// silently disagree only on 12-bit content that no conformance vector in the
    /// corpus contains. `angular_i16_headroom_is_exhausted_at_10_bit` pins it.
    unsafe fn angular_row_avx2(row: *mut u16, n: usize, refb: *const i16, ifact: i32) {
        let f = _mm256_set1_epi16(-(ifact as i16));
        let rnd = _mm256_set1_epi16(16);
        // Two vectors per trip, as in the SSE2 twin.
        let nvec = n / 16;
        for i in 0..nvec / 2 {
            for half in 0..2usize {
                let j = i * 32 + half * 16;
                let p = unsafe { refb.add(j) };
                let a = unsafe { _mm256_loadu_si256(p as *const __m256i) };
                let b = unsafe { _mm256_loadu_si256(p.add(1) as *const __m256i) };
                let m = _mm256_srai_epi16(_mm256_add_epi16(_mm256_mullo_epi16(_mm256_sub_epi16(a, b), f), rnd), 5);
                unsafe { _mm256_storeu_si256(row.add(j) as *mut __m256i, _mm256_add_epi16(a, m)) };
            }
        }
        if nvec % 2 == 1 {
            let j = (nvec - 1) * 16;
            let p = unsafe { refb.add(j) };
            let a = unsafe { _mm256_loadu_si256(p as *const __m256i) };
            let b = unsafe { _mm256_loadu_si256(p.add(1) as *const __m256i) };
            let m = _mm256_srai_epi16(_mm256_add_epi16(_mm256_mullo_epi16(_mm256_sub_epi16(a, b), f), rnd), 5);
            unsafe { _mm256_storeu_si256(row.add(j) as *mut __m256i, _mm256_add_epi16(a, m)) };
        }
        let j = nvec * 16;
        if j < n {
            // SAFETY: the rest of the row, with the same slack.
            unsafe { angular_row_sse2(row.add(j), n - j, refb.add(j), ifact) };
        }
    }

    /// # Safety
    /// As [`angular_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn angular_avx2(dst: *mut u16, dst_stride: usize, n: usize, refb: *const i16, off: usize, angle: i32) {
        for i in 0..n {
            let pos = (i as i32 + 1) * angle;
            let iidx = pos >> 5;
            let ifact = pos & 31;
            let base = (off as i32 + iidx + 1) as usize;
            let row = unsafe { dst.add(i * dst_stride) };
            let src = unsafe { refb.add(base) };
            crate::census::route(ifact == 0, &crate::census::RT_INTRA_ANG_COPY, &crate::census::RT_INTRA_ANG_ROW);
            if ifact == 0 {
                let nvec = n / 16;
                for k in 0..nvec {
                    unsafe { _mm256_storeu_si256(row.add(k * 16) as *mut __m256i, _mm256_loadu_si256(src.add(k * 16) as *const __m256i)) };
                }
                let mut j = nvec * 16;
                while j < n {
                    unsafe { *row.add(j) = *src.add(j) as u16 };
                    j += 1;
                }
            } else {
                unsafe { angular_row_avx2(row, n, src, ifact) };
            }
        }
    }

    /// Planar prediction, AVX2: eight samples per iteration instead of four.
    ///
    /// Planar cannot use the `i16` identity that angular does — the four terms
    /// sum past 2^15 — so the intermediate really is 32-bit and the win is the
    /// wider register: 8 `i32` lanes rather than 4.
    ///
    /// # Safety
    /// As [`planar_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn planar_avx2(dst: *mut u16, dst_stride: usize, n: usize, left: *const i16, top: *const i16, log2n: u32) {
        // This kernel steps eight `i32`; the SSE2 twin steps four and so covers
        // `n == 4`, which this one would send to its scalar tail.
        if n < 8 {
            // SAFETY: same footprint, narrower step.
            return unsafe { planar_sse2(dst, dst_stride, n, left, top, log2n) };
        }
        let tn = unsafe { *top.add(n) } as i32;
        let ln = unsafe { *left.add(n) } as i32;
        let sh = _mm_cvtsi32_si128(log2n as i32 + 1);
        let mut ramp = [0i16; 2 * super::MAX_N];
        for x in 0..n {
            ramp[2 * x] = (n - 1 - x) as i16;
            ramp[2 * x + 1] = (x + 1) as i16;
        }
        let nvec = n / 8;
        for y in 0..n {
            let l = unsafe { *left.add(y) } as i32;
            let k = _mm256_set1_epi32((n - 1 - y) as i32);
            let c = _mm256_set1_epi32((y as i32 + 1) * ln + n as i32);
            let w = _mm256_set1_epi32((l & 0xffff) | (tn << 16));
            let row = unsafe { dst.add(y * dst_stride) };
            // The narrowing is the reason to pair here rather than the loop
            // bookkeeping: eight `i32` lanes only half-fill a 256-bit store, so
            // one vector per trip pays a `packs`, a `permute` and a half-width
            // store for every eight samples. Two vectors fill the store exactly
            // and pay that trio once for sixteen.
            let term = |x: usize| {
                let rp = unsafe { _mm256_loadu_si256(ramp.as_ptr().add(2 * x) as *const __m256i) };
                let xterm = _mm256_madd_epi16(rp, w);
                let t32 = unsafe { _mm256_cvtepu16_epi32(_mm_loadu_si128(top.add(x) as *const __m128i)) };
                let yterm = _mm256_mullo_epi32(t32, k);
                _mm256_sra_epi32(_mm256_add_epi32(_mm256_add_epi32(xterm, yterm), c), sh)
            };
            for i in 0..nvec / 2 {
                let x = i * 16;
                let v0 = term(x);
                let v1 = term(x + 8);
                // `packs` is per 128-bit lane, so the qwords come out as
                // (v0.lo, v1.lo, v0.hi, v1.hi); one permute reorders them.
                let p = _mm256_permute4x64_epi64(_mm256_packs_epi32(v0, v1), 0b11_01_10_00);
                unsafe { _mm256_storeu_si256(row.add(x) as *mut __m256i, p) };
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let v = term(x);
                let p = _mm256_permute4x64_epi64(_mm256_packs_epi32(v, v), 0b0000_1000);
                unsafe { _mm_storeu_si128(row.add(x) as *mut __m128i, _mm256_castsi256_si128(p)) };
            }
            let mut x = nvec * 8;
            while x < n {
                let v = (n - 1 - x) as i32 * l + (x as i32 + 1) * tn + (n - 1 - y) as i32 * unsafe { *top.add(x) } as i32 + (y as i32 + 1) * ln + n as i32;
                unsafe { *row.add(x) = (v >> (log2n + 1)) as u16 };
                x += 1;
            }
        }
    }

    /// DC fill: broadcast one sample across the block.
    ///
    /// `slice::fill` looks like it should lower to a vector store loop, and
    /// across an inlining boundary with a runtime row length it does not — the
    /// emitted `dc_fill` contained **zero** vector instructions. Step 0 of the
    /// vectorise procedure says check the assembly rather than assume; this is
    /// the case where the assumption was wrong in the codec's favour.
    ///
    /// # Safety
    /// `dst` must have `dst_stride * (h - 1) + n` writable `u16`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn dc_fill_sse2(dst: *mut u16, dst_stride: usize, n: usize, dc: u16) {
        let v = _mm_set1_epi16(dc as i16);
        let nvec = n / 8;
        for y in 0..n {
            let row = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                unsafe { _mm_storeu_si128(row.add(i * 8) as *mut __m128i, v) };
            }
            let mut x = nvec * 8;
            // A 4-wide store before the scalar tail. `nvec = n / 8`, so a 4x4
            // block wrote its whole row one sample at a time -- and 4x4 is
            // 28.4% of intra samples on intra-heavy content
            // (`INTRA_N_LT8` = 49,728,768).
            if x + 4 <= n {
                unsafe { _mm_storel_epi64(row.add(x) as *mut __m128i, v) };
                x += 4;
            }
            while x < n {
                unsafe { *row.add(x) = dc };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`dc_fill_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn dc_fill_avx2(dst: *mut u16, dst_stride: usize, n: usize, dc: u16) {
        if n < 16 {
            return unsafe { dc_fill_sse2(dst, dst_stride, n, dc) };
        }
        let v = _mm256_set1_epi16(dc as i16);
        let nvec = n / 16;
        for y in 0..n {
            let row = unsafe { dst.add(y * dst_stride) };
            for i in 0..nvec {
                unsafe { _mm256_storeu_si256(row.add(i * 16) as *mut __m256i, v) };
            }
        }
    }

    /// Angular prediction for modes below 18, AVX2.
    ///
    /// The transpose is the reason this could not simply use wider registers:
    /// an 8×8 tile is 128 bits wide, so a 256-bit row does not fit one tile.
    /// The fix is to do TWO horizontally-adjacent tiles per strip — compute
    /// eight rows sixteen samples wide with one AVX2 op each, then transpose
    /// the two 8×8 halves out of the strip. The row arithmetic halves; the
    /// transposes are unchanged.
    ///
    /// # Safety
    /// As [`angular_t_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn angular_t_avx2(dst: *mut u16, dst_stride: usize, n: usize, refb: *const i16, off: usize, angle: i32) {
        if n < 16 {
            // 4 and 8 are narrower than the strip; the SSE2 path already has a
            // dedicated 4×4 and an 8×8 tile for them.
            return unsafe { angular_t_sse2(dst, dst_stride, n, refb, off, angle) };
        }
        let mut strip = [0u16; 8 * 16];
        let mut i0 = 0usize;
        while i0 < n {
            let mut j0 = 0usize;
            while j0 + 16 <= n {
                for k in 0..8 {
                    let pos = ((i0 + k) as i32 + 1) * angle;
                    let iidx = pos >> 5;
                    let ifact = pos & 31;
                    let src = unsafe { refb.add((off as i32 + iidx + 1) as usize + j0) };
                    let row = unsafe { strip.as_mut_ptr().add(k * 16) };
                    if ifact == 0 {
                        unsafe { _mm256_storeu_si256(row as *mut __m256i, _mm256_loadu_si256(src as *const __m256i)) };
                    } else {
                        unsafe { angular_row_avx2(row, 16, src, ifact) };
                    }
                }
                // SAFETY: both tiles lie inside the n x n block.
                unsafe {
                    tile8x2_avx2(dst.add(j0 * dst_stride + i0), dst.add((j0 + 8) * dst_stride + i0), dst_stride, strip.as_ptr(), 16);
                }
                j0 += 16;
            }
            i0 += 8;
        }
    }

    /// Transpose one 8×8 tile of `u16`: three rounds of unpacks, 16 then 32
    /// then 64 bits wide.
    ///
    /// # Safety
    /// Eight readable rows at `src_stride` and eight writable at `dst_stride`.
    #[target_feature(enable = "sse2")]
    unsafe fn tile8_sse2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize) {
        let mut r = [_mm_setzero_si128(); 8];
        for (k, slot) in r.iter_mut().enumerate() {
            *slot = unsafe { _mm_loadu_si128(src.add(k * src_stride) as *const __m128i) };
        }
        let a: [__m128i; 8] = [
            _mm_unpacklo_epi16(r[0], r[1]),
            _mm_unpackhi_epi16(r[0], r[1]),
            _mm_unpacklo_epi16(r[2], r[3]),
            _mm_unpackhi_epi16(r[2], r[3]),
            _mm_unpacklo_epi16(r[4], r[5]),
            _mm_unpackhi_epi16(r[4], r[5]),
            _mm_unpacklo_epi16(r[6], r[7]),
            _mm_unpackhi_epi16(r[6], r[7]),
        ];
        let b: [__m128i; 8] = [
            _mm_unpacklo_epi32(a[0], a[2]),
            _mm_unpackhi_epi32(a[0], a[2]),
            _mm_unpacklo_epi32(a[1], a[3]),
            _mm_unpackhi_epi32(a[1], a[3]),
            _mm_unpacklo_epi32(a[4], a[6]),
            _mm_unpackhi_epi32(a[4], a[6]),
            _mm_unpacklo_epi32(a[5], a[7]),
            _mm_unpackhi_epi32(a[5], a[7]),
        ];
        let out: [__m128i; 8] = [
            _mm_unpacklo_epi64(b[0], b[4]),
            _mm_unpackhi_epi64(b[0], b[4]),
            _mm_unpacklo_epi64(b[1], b[5]),
            _mm_unpackhi_epi64(b[1], b[5]),
            _mm_unpacklo_epi64(b[2], b[6]),
            _mm_unpackhi_epi64(b[2], b[6]),
            _mm_unpacklo_epi64(b[3], b[7]),
            _mm_unpackhi_epi64(b[3], b[7]),
        ];
        for (k, v) in out.iter().enumerate() {
            unsafe { _mm_storeu_si128(dst.add(k * dst_stride) as *mut __m128i, *v) };
        }
    }

    /// Transpose TWO horizontally-adjacent 8×8 `u16` tiles at once.
    ///
    /// `vpunpck*` on a 256-bit register works inside each 128-bit lane
    /// independently, so one register set carries both tiles through the same
    /// three-stage ladder `tile8_sse2` uses: the low lane transposes the left
    /// tile while the high lane transposes the right one, with no lane
    /// crossing anywhere. Only the stores differ -- the two results land at
    /// unrelated addresses, so the high lane needs an explicit extract.
    ///
    /// Eight loads and twenty-four unpacks against sixteen and forty-eight for
    /// the pair of SSE2 tiles this replaces.
    ///
    /// # Safety
    /// `src` must have 8 rows of 16 samples at `src_stride`; `dst_a` and
    /// `dst_b` must each have 8 rows of 8 at `dst_stride`.
    #[target_feature(enable = "avx2")]
    unsafe fn tile8x2_avx2(dst_a: *mut u16, dst_b: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize) {
        let mut r = [_mm256_setzero_si256(); 8];
        for (k, slot) in r.iter_mut().enumerate() {
            *slot = unsafe { _mm256_loadu_si256(src.add(k * src_stride) as *const __m256i) };
        }
        let a: [__m256i; 8] = [
            _mm256_unpacklo_epi16(r[0], r[1]),
            _mm256_unpackhi_epi16(r[0], r[1]),
            _mm256_unpacklo_epi16(r[2], r[3]),
            _mm256_unpackhi_epi16(r[2], r[3]),
            _mm256_unpacklo_epi16(r[4], r[5]),
            _mm256_unpackhi_epi16(r[4], r[5]),
            _mm256_unpacklo_epi16(r[6], r[7]),
            _mm256_unpackhi_epi16(r[6], r[7]),
        ];
        let b: [__m256i; 8] = [
            _mm256_unpacklo_epi32(a[0], a[2]),
            _mm256_unpackhi_epi32(a[0], a[2]),
            _mm256_unpacklo_epi32(a[1], a[3]),
            _mm256_unpackhi_epi32(a[1], a[3]),
            _mm256_unpacklo_epi32(a[4], a[6]),
            _mm256_unpackhi_epi32(a[4], a[6]),
            _mm256_unpacklo_epi32(a[5], a[7]),
            _mm256_unpackhi_epi32(a[5], a[7]),
        ];
        let out: [__m256i; 8] = [
            _mm256_unpacklo_epi64(b[0], b[4]),
            _mm256_unpackhi_epi64(b[0], b[4]),
            _mm256_unpacklo_epi64(b[1], b[5]),
            _mm256_unpackhi_epi64(b[1], b[5]),
            _mm256_unpacklo_epi64(b[2], b[6]),
            _mm256_unpackhi_epi64(b[2], b[6]),
            _mm256_unpacklo_epi64(b[3], b[7]),
            _mm256_unpackhi_epi64(b[3], b[7]),
        ];
        for (k, v) in out.iter().enumerate() {
            unsafe {
                _mm_storeu_si128(dst_a.add(k * dst_stride) as *mut __m128i, _mm256_castsi256_si128(*v));
                _mm_storeu_si128(dst_b.add(k * dst_stride) as *mut __m128i, _mm256_extracti128_si256(*v, 1));
            }
        }
    }

    /// Transpose an `n × n` block of `u16`, in 8×8 tiles.
    ///
    /// # Safety
    /// Both blocks must be `n × n` at their strides.
    #[target_feature(enable = "sse2")]
    pub unsafe fn transpose_sse2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, n: usize) {
        if n < 8 {
            for i in 0..n {
                for j in 0..n {
                    unsafe { *dst.add(j * dst_stride + i) = *src.add(i * src_stride + j) };
                }
            }
            return;
        }
        let mut i = 0usize;
        while i < n {
            let mut j = 0usize;
            while j < n {
                // SAFETY: both tiles are inside the n x n blocks.
                unsafe { tile8_sse2(dst.add(j * dst_stride + i), dst_stride, src.add(i * src_stride + j), src_stride) };
                j += 8;
            }
            i += 8;
        }
    }

    /// Angular prediction written **column-major** (`dst[j·stride + i]`), for
    /// modes below 18.
    ///
    /// Done tile by tile so the scratch is one 8×8 tile — 128 bytes — rather
    /// than a whole 32×32 block. That matters more than it looks: predicting
    /// into a `[u16; 1024]` stack temp costs a 2 KB zeroing per call, which for
    /// the 4×4 blocks that dominate intra content is 128 bytes of memset per
    /// predicted pixel, and swamped the entire kernel win when measured.
    ///
    /// # Safety
    /// As [`angular_sse2`]; `dst` must be an `n × n` block at `dst_stride`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn angular_t_sse2(dst: *mut u16, dst_stride: usize, n: usize, refb: *const i16, off: usize, angle: i32) {
        if n == 4 {
            // 4×4 is the commonest intra block by a wide margin, so it gets its
            // own path rather than falling back to scalar: four rows computed
            // the usual way, then a 4×4 transpose in two rounds of unpacks.
            let mut rows = [_mm_setzero_si128(); 4];
            let mut tmp = [0u16; 8];
            for (i, slot) in rows.iter_mut().enumerate() {
                let pos = (i as i32 + 1) * angle;
                let iidx = pos >> 5;
                let ifact = pos & 31;
                let src = unsafe { refb.add((off as i32 + iidx + 1) as usize) };
                if ifact == 0 {
                    *slot = unsafe { _mm_loadl_epi64(src as *const __m128i) };
                } else {
                    unsafe { angular_row_sse2(tmp.as_mut_ptr(), 4, src, ifact) };
                    *slot = unsafe { _mm_loadl_epi64(tmp.as_ptr() as *const __m128i) };
                }
            }
            let a = _mm_unpacklo_epi16(rows[0], rows[1]);
            let b = _mm_unpacklo_epi16(rows[2], rows[3]);
            let lo = _mm_unpacklo_epi32(a, b);
            let hi = _mm_unpackhi_epi32(a, b);
            unsafe {
                _mm_storel_epi64(dst as *mut __m128i, lo);
                _mm_storel_epi64(dst.add(dst_stride) as *mut __m128i, _mm_unpackhi_epi64(lo, lo));
                _mm_storel_epi64(dst.add(2 * dst_stride) as *mut __m128i, hi);
                _mm_storel_epi64(dst.add(3 * dst_stride) as *mut __m128i, _mm_unpackhi_epi64(hi, hi));
            }
            return;
        }
        if n < 8 {
            for i in 0..n {
                let pos = (i as i32 + 1) * angle;
                let iidx = pos >> 5;
                let ifact = pos & 31;
                let src = unsafe { refb.add((off as i32 + iidx + 1) as usize) };
                for j in 0..n {
                    let a = unsafe { *src.add(j) } as i32;
                    let v = if ifact == 0 {
                        a
                    } else {
                        let b = unsafe { *src.add(j + 1) } as i32;
                        ((32 - ifact) * a + ifact * b + 16) >> 5
                    };
                    unsafe { *dst.add(j * dst_stride + i) = v as u16 };
                }
            }
            return;
        }
        let mut tile = [0u16; 64];
        let mut i0 = 0usize;
        while i0 < n {
            let mut j0 = 0usize;
            while j0 < n {
                for k in 0..8 {
                    let pos = ((i0 + k) as i32 + 1) * angle;
                    let iidx = pos >> 5;
                    let ifact = pos & 31;
                    let src = unsafe { refb.add((off as i32 + iidx + 1) as usize + j0) };
                    let row = unsafe { tile.as_mut_ptr().add(k * 8) };
                    if ifact == 0 {
                        unsafe { _mm_storeu_si128(row as *mut __m128i, _mm_loadu_si128(src as *const __m128i)) };
                    } else {
                        unsafe { angular_row_sse2(row, 8, src, ifact) };
                    }
                }
                // SAFETY: the tile is 8x8 and lands inside the n x n block.
                unsafe { tile8_sse2(dst.add(j0 * dst_stride + i0), dst_stride, tile.as_ptr(), 8) };
                j0 += 8;
            }
            i0 += 8;
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod arm {
    use std::arch::aarch64::*;

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn angular_neon(dst: *mut u16, dst_stride: usize, n: usize, refb: *const i16, off: usize, angle: i32) {
        for i in 0..n {
            let pos = (i as i32 + 1) * angle;
            let iidx = pos >> 5;
            let ifact = pos & 31;
            let base = (off as i32 + iidx + 1) as usize;
            let row = unsafe { dst.add(i * dst_stride) };
            let src = unsafe { refb.add(base) };
            if ifact == 0 {
                let mut j = 0usize;
                while j + 8 <= n {
                    unsafe { vst1q_u16(row.add(j), vreinterpretq_u16_s16(vld1q_s16(src.add(j)))) };
                    j += 8;
                }
                while j < n {
                    unsafe { *row.add(j) = *src.add(j) as u16 };
                    j += 1;
                }
                continue;
            }
            let wa = vdupq_n_s16((32 - ifact) as i16);
            let wb = vdupq_n_s16(ifact as i16);
            let rnd = vdupq_n_s32(16);
            let mut j = 0usize;
            while j + 8 <= n {
                let a = unsafe { vld1q_s16(src.add(j)) };
                let b = unsafe { vld1q_s16(src.add(j + 1)) };
                let lo = vaddq_s32(vmlal_s16(vmull_s16(vget_low_s16(a), vget_low_s16(wa)), vget_low_s16(b), vget_low_s16(wb)), rnd);
                let hi = vaddq_s32(vmlal_high_s16(vmull_high_s16(a, wa), b, wb), rnd);
                let r = vcombine_s16(vshrn_n_s32(lo, 5), vshrn_n_s32(hi, 5));
                unsafe { vst1q_u16(row.add(j), vreinterpretq_u16_s16(r)) };
                j += 8;
            }
            while j < n {
                let a = unsafe { *src.add(j) } as i32;
                let b = unsafe { *src.add(j + 1) } as i32;
                unsafe { *row.add(j) = (((32 - ifact) * a + ifact * b + 16) >> 5) as u16 };
                j += 1;
            }
        }
    }
}

/// Angular prediction (§8.4.4.2.6) into a **row-major** `n × n` block.
///
/// `refb[off + k]` is the reference at index `k`, `k` running `−n..=2n`. For
/// modes below 18 the caller predicts into scratch and calls [`transpose`];
/// the arithmetic is identical, only the orientation of the result differs.
/// Whether the angular kernels' `i16` multiply is exact for samples bounded by
/// `max`.
///
/// The x86 kernels evaluate `f * (a - b)` with `mullo_epi16`, which keeps the
/// low 16 bits. `f` reaches 31 and `|a - b|` reaches `max`, so the product
/// reaches `31 * max`:
///
/// | bit depth | max | `31 * max` | fits `i16`? |
/// |---|---:|---:|---|
/// | 8 | 255 | 7,905 | yes |
/// | 10 | 1,023 | 31,713 | yes, by 3% |
/// | 12 | 4,095 | 126,945 | **no -- wraps silently** |
///
/// The scalar twin computes in `i32` and the NEON twin widens with
/// `vmull_s16`, so above 10 bits the x86 arms would disagree with BOTH -- a
/// per-architecture split producing different pixels from identical input.
/// Returning false here routes to the scalar twin instead, so enabling RExt
/// costs speed rather than correctness.
pub fn angular_i16_is_exact(max: i32) -> bool {
    31 * max <= i16::MAX as i32
}

pub fn angular(dst: &mut [u16], dst_stride: usize, n: usize, refb: &[i16], off: usize, angle: i32, max: i32) {
    if census::ALWAYS {
        census::bump(if n < 8 { &census::INTRA_N_LT8 } else { &census::INTRA_N_GE8 }, (n * n) as u64);
    }
    // The largest reference index this can touch is `off + n + (n*angle>>5) + 1`,
    // bounded by `off + 2n + 1`; the `+ 8` is the vector overread.
    let ok = (4..=MAX_N).contains(&n) && dst.len() >= dst_stride * (n - 1) + n && refb.len() >= off + 2 * n + 9 && angular_i16_is_exact(max);
    // No debug_assert on `angular_i16_is_exact`: past the i16 headroom (RExt
    // 12-bit and up) falling back to the scalar twin IS the designed path --
    // `angular_is_exact_at_rext_bit_depths` exercises exactly that.
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::INTRA_ANGULAR_SIMD } else { &census::INTRA_ANGULAR_SCALAR }, 1);
        census::bump(&census::SAMPLES_INTRA, (n * n) as u64);
    }
    #[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
    if ok {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the bounds check above covers every load and store, the
        // overread included.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::angular_avx2(dst.as_mut_ptr(), dst_stride, n, refb.as_ptr(), off, angle) },
            _ => return unsafe { x86::angular_sse2(dst.as_mut_ptr(), dst_stride, n, refb.as_ptr(), off, angle) },
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above.
        return unsafe { arm::angular_neon(dst.as_mut_ptr(), dst_stride, n, refb.as_ptr(), off, angle) };
    }
    angular_scalar(dst, dst_stride, n, refb, off, angle);
}

/// Angular prediction for modes below 18, whose main reference is the left
/// column: the same arithmetic as [`angular`] written **column-major**, so the
/// caller needs no scratch block of its own.
pub fn angular_t(dst: &mut [u16], dst_stride: usize, n: usize, refb: &[i16], off: usize, angle: i32, max: i32) {
    let ok = (4..=MAX_N).contains(&n) && dst.len() >= dst_stride * (n - 1) + n && refb.len() >= off + 2 * n + 9 && angular_i16_is_exact(max);
    // No exactness debug_assert: see `angular` -- the scalar fallback past the
    // i16 headroom is the designed path, not a bug.
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::INTRA_ANGULAR_SIMD } else { &census::INTRA_ANGULAR_SCALAR }, 1);
        census::bump(&census::SAMPLES_INTRA, (n * n) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: the bounds check above covers every load and store.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::angular_t_avx2(dst.as_mut_ptr(), dst_stride, n, refb.as_ptr(), off, angle) },
            _ => return unsafe { x86::angular_t_sse2(dst.as_mut_ptr(), dst_stride, n, refb.as_ptr(), off, angle) },
        }
    }
    angular_t_scalar(dst, dst_stride, n, refb, off, angle);
}

/// Planar prediction (§8.4.4.2.5). `left` and `top` must hold `n + 1` samples.
pub fn planar(dst: &mut [u16], dst_stride: usize, n: usize, left: &[i16], top: &[i16], log2n: u32) {
    if census::ALWAYS {
        census::bump(if n < 8 { &census::INTRA_N_LT8 } else { &census::INTRA_N_GE8 }, (n * n) as u64);
    }
    let ok = (4..=MAX_N).contains(&n) && dst.len() >= dst_stride * (n - 1) + n && left.len() > n && top.len() > n;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::INTRA_PLANAR_SIMD } else { &census::INTRA_PLANAR_SCALAR }, 1);
        census::bump(&census::SAMPLES_INTRA, (n * n) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: lengths checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::planar_avx2(dst.as_mut_ptr(), dst_stride, n, left.as_ptr(), top.as_ptr(), log2n) },
            _ => return unsafe { x86::planar_sse2(dst.as_mut_ptr(), dst_stride, n, left.as_ptr(), top.as_ptr(), log2n) },
        }
    }
    planar_scalar(dst, dst_stride, n, left, top, log2n);
}

/// DC prediction's flat fill (§8.4.4.2.5); the three edge fix-ups stay with
/// the caller, which owns the `c_idx` and size conditions.
pub fn dc_fill(dst: &mut [u16], dst_stride: usize, n: usize, dc: u16) {
    let ok = (4..=MAX_N).contains(&n) && dst.len() >= dst_stride * (n - 1) + n;
    debug_assert!(ok);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::INTRA_DC_SIMD } else { &census::INTRA_DC_SCALAR }, 1);
        census::bump(&census::SAMPLES_INTRA, (n * n) as u64);
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: length checked above.
        match crate::isa() {
            crate::Isa::Avx2 => return unsafe { x86::dc_fill_avx2(dst.as_mut_ptr(), dst_stride, n, dc) },
            _ => return unsafe { x86::dc_fill_sse2(dst.as_mut_ptr(), dst_stride, n, dc) },
        }
    }
    dc_fill_scalar(dst, dst_stride, n, dc);
}

/// Transpose an `n × n` block of samples.
pub fn transpose(dst: &mut [u16], dst_stride: usize, src: &[u16], src_stride: usize, n: usize) {
    let ok = (4..=MAX_N).contains(&n) && dst.len() >= dst_stride * (n - 1) + n && src.len() >= src_stride * (n - 1) + n;
    debug_assert!(ok);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if ok {
        // SAFETY: both blocks bounds-checked above.
        return unsafe { x86::transpose_sse2(dst.as_mut_ptr(), dst_stride, src.as_ptr(), src_stride, n) };
    }
    transpose_scalar(dst, dst_stride, src, src_stride, n);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *s
    }

    #[test]
    fn angular_matches_scalar() {
        let mut st = 0x1234_9876u32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &n in &[4usize, 8, 16, 32] {
                let refb: Vec<i16> = (0..REF_LEN + 16).map(|_| (lcg(&mut st) as i32 & max) as i16).collect();
                // Every angle the spec can produce, both signs.
                for angle in [-32i32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32] {
                    let stride = n + 3;
                    let mut a = vec![0u16; stride * n];
                    let mut b = vec![0u16; stride * n];
                    angular_scalar(&mut a, stride, n, &refb, MAX_N, angle);
                    angular(&mut b, stride, n, &refb, MAX_N, angle, max);
                    assert_eq!(a, b, "angular n={n} angle={angle} bd={bd}");
                }
            }
        }
    }

    #[test]
    fn angular_t_matches_scalar() {
        let mut st = 0x77aa_33ccu32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &n in &[4usize, 8, 16, 32] {
                let refb: Vec<i16> = (0..REF_LEN + 16).map(|_| (lcg(&mut st) as i32 & max) as i16).collect();
                for angle in [-32i32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32] {
                    let stride = n + 3;
                    let mut a = vec![0u16; stride * n];
                    let mut b = vec![0u16; stride * n];
                    angular_t_scalar(&mut a, stride, n, &refb, MAX_N, angle);
                    angular_t(&mut b, stride, n, &refb, MAX_N, angle, max);
                    assert_eq!(a, b, "angular_t n={n} angle={angle} bd={bd}");
                    // And it must be the transpose of the row-major kernel.
                    let mut rowmajor = vec![0u16; n * n];
                    angular(&mut rowmajor, n, n, &refb, MAX_N, angle, max);
                    let mut t = vec![0u16; stride * n];
                    transpose(&mut t, stride, &rowmajor, n, n);
                    assert_eq!(a, t, "angular_t is not the transpose n={n} angle={angle}");
                }
            }
        }
    }

    #[test]
    fn planar_matches_scalar() {
        let mut st = 0xabcd_1234u32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &n in &[4usize, 8, 16, 32] {
                let left: Vec<i16> = (0..64).map(|_| (lcg(&mut st) as i32 & max) as i16).collect();
                let top: Vec<i16> = (0..64).map(|_| (lcg(&mut st) as i32 & max) as i16).collect();
                let stride = n + 5;
                let log2n = n.trailing_zeros();
                let mut a = vec![0u16; stride * n];
                let mut b = vec![0u16; stride * n];
                planar_scalar(&mut a, stride, n, &left, &top, log2n);
                planar(&mut b, stride, n, &left, &top, log2n);
                assert_eq!(a, b, "planar n={n} bd={bd}");
            }
        }
    }

    #[test]
    fn transpose_matches_scalar() {
        let mut st = 0x5555_aaaau32;
        for &n in &[4usize, 8, 16, 32] {
            let (ss, ds) = (n + 2, n + 7);
            let src: Vec<u16> = (0..ss * n).map(|_| lcg(&mut st) as u16).collect();
            let mut a = vec![0u16; ds * n];
            let mut b = vec![0u16; ds * n];
            transpose_scalar(&mut a, ds, &src, ss, n);
            transpose(&mut b, ds, &src, ss, n);
            assert_eq!(a, b, "transpose n={n}");
        }
    }

    /// The i16 angular kernels are sound only because the decoder refuses
    /// bit depths above 10. This pins how little room is left.
    ///
    /// At 10-bit the product reaches 31,713 against an i16 ceiling of 32,767 --
    /// 3% of headroom. At 12-bit it is 126,945 and `mullo_epi16` would silently
    /// return the low 16 bits, so the SIMD twins would disagree with the i32
    /// scalar twin on content no HEVC_v1 vector contains. If RExt support is
    /// ever added, these kernels need widening FIRST.
    #[test]
    fn angular_i16_headroom_is_exhausted_at_10_bit() {
        let worst = |bd: u32| 31i32 * ((1i32 << bd) - 1);
        assert!(worst(8) <= i16::MAX as i32, "8-bit must fit");
        assert!(worst(10) <= i16::MAX as i32, "10-bit must fit: {}", worst(10));
        assert!(
            worst(11) > i16::MAX as i32,
            "11-bit unexpectedly fits ({}) -- re-derive the bound before relaxing the SPS check",
            worst(11)
        );
        // And the identity the kernels use, over the whole 10-bit domain and
        // every weight, against the specification's own form.
        for &bd in &[8u32, 10] {
            let max = (1i32 << bd) - 1;
            for f in 1..32i32 {
                for &(a, b) in &[(0, 0), (0, max), (max, 0), (max, max), (max / 3, max / 7)] {
                    let spec = ((32 - f) * a + f * b + 16) >> 5;
                    let flipped = a + ((-f * (a - b) + 16) >> 5);
                    assert_eq!(spec, flipped, "bd={bd} f={f} a={a} b={b}");
                    assert!((-f * (a - b)).abs() <= i16::MAX as i32, "overflow at bd={bd} f={f}");
                }
            }
        }
    }

    /// The angular kernels must stay bit-exact at bit depths where `31 * max`
    /// outgrows `i16`.
    ///
    /// `mullo_epi16` keeps the low 16 bits. At 12-bit the product reaches
    /// 126,945 and wraps; the scalar twin computes in `i32` and the NEON twin
    /// widens with `vmull_s16`, so only the x86 arms would be wrong -- the same
    /// stream decoding differently on two architectures.
    ///
    /// The dispatcher now falls back to scalar when `angular_i16_is_exact` is
    /// false. Delete that term from either `ok` gate and this fails.
    #[test]
    fn angular_is_exact_at_rext_bit_depths() {
        let mut st = 0x7a11_ceedu32;
        let rnd = |s: &mut u32| {
            *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*s >> 8) as i32
        };
        let mut stressed = 0usize;
        for &bd in &[8u32, 10, 12, 14] {
            let max = (1i32 << bd) - 1;
            if !angular_i16_is_exact(max) {
                stressed += 1;
            }
            for &n in &[4usize, 8, 16, 32] {
                for &angle in &[-32i32, -21, -9, -2, 2, 9, 17, 26, 32] {
                    let refb: Vec<i16> = (0..4 * n + 16).map(|_| (rnd(&mut st) & max) as i16).collect();
                    let mut a = vec![0u16; n * n];
                    let mut b = vec![0u16; n * n];
                    angular_scalar(&mut a, n, n, &refb, n, angle);
                    angular(&mut b, n, n, &refb, n, angle, max);
                    assert_eq!(a, b, "angular bd={bd} n={n} angle={angle}");

                    let mut c = vec![0u16; n * n];
                    let mut d = vec![0u16; n * n];
                    angular_t_scalar(&mut c, n, n, &refb, n, angle);
                    angular_t(&mut d, n, n, &refb, n, angle, max);
                    assert_eq!(c, d, "angular_t bd={bd} n={n} angle={angle}");
                }
            }
        }
        assert!(stressed > 0, "no bit depth exceeded the i16 headroom -- the test covers nothing");
    }
}
