//! Sample adaptive offset kernels (§8.7.3), for the interior of a coding tree
//! block — the region where every neighbour is inside the same block and no
//! availability question arises.
//!
//! # Both offset types are table lookups, and neither needs a gather
//!
//! **Band offset** classifies a sample by its top five bits and adds one of
//! four offsets, the others being zero. Rather than index a 32-entry table,
//! the kernel compares the band index against the four active bands and
//! selects — four compares and four masked adds, no gather.
//!
//! **Edge offset** compares a sample with two neighbours along one of four
//! directions. `sign(v − a)` is `cmpgt(a, v) − cmpgt(v, a)`, so the whole
//! category derivation is compares and subtracts; the category is then mapped
//! to an offset the same way, by comparison rather than lookup. Category 2
//! (the plateau) maps to a zero offset, which is why the mapping table has a
//! hole in the middle rather than a branch.

use crate::census;

/// Offsets in `edgeIdx` order, `[0]` being category 1. Category 2 has no
/// offset; the kernels encode that as a zero, which is why they need no
/// branch. Index by `2 + sign(v − a) + sign(v − b)`.
#[inline]
fn edge_offsets(offs: &[i16; 4]) -> [i16; 5] {
    [offs[0], offs[1], 0, offs[2], offs[3]]
}

/// Byte offset of a neighbour direction, in samples.
#[inline]
fn doff(d: (i32, i32), stride: usize) -> isize {
    d.1 as isize * stride as isize + d.0 as isize
}

/// `#[cold]`: the SIMD guard above it succeeds on every block of a conformant
/// stream -- the `*_SCALAR` census counters read 0 across the corpus -- so this
/// is the fallback for a shape the kernels decline, not a path decode takes.
/// Inlined it padded the dispatcher, which IS on the hot path, with a body that
/// never runs. Same reason `Cabac::refill_tail` is out of line.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn sao_band_scalar(dst: &mut [u16], src: &[u16], stride: usize, x0: usize, y0: usize, w: usize, h: usize, shift: u32, band: &[i16; 32], max: i32) {
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = y * stride + x;
            let v = src[i] as i32;
            let off = band[(v >> shift) as usize] as i32;
            if off != 0 {
                dst[i] = (v + off).clamp(0, max) as u16;
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
#[allow(clippy::too_many_arguments)]
fn sao_edge_scalar(dst: &mut [u16], src: &[u16], stride: usize, x0: usize, y0: usize, w: usize, h: usize, da: (i32, i32), db: (i32, i32), offs: &[i16; 4], max: i32) {
    let table = edge_offsets(offs);
    let (oa, ob) = (doff(da, stride), doff(db, stride));
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = y * stride + x;
            let v = src[i] as i32;
            let a = src[(i as isize + oa) as usize] as i32;
            let b = src[(i as isize + ob) as usize] as i32;
            let e = (2 + (v - a).signum() + (v - b).signum()) as usize;
            let off = table[e] as i32;
            if off != 0 {
                dst[i] = (v + off).clamp(0, max) as u16;
            }
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod x86 {
    use std::arch::x86_64::*;

    /// # Safety
    /// `src` must have `src_stride * (h - 1) + w` readable `u16` and `dst` the
    /// same writable at `dst_stride`.
    #[target_feature(enable = "sse2")]
    pub unsafe fn band_sse2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, shift: u32, bands: [(i16, i16); 4], max: i32) {
        let sh = _mm_cvtsi32_si128(shift as i32);
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        let idx: [__m128i; 4] = [_mm_set1_epi16(bands[0].0), _mm_set1_epi16(bands[1].0), _mm_set1_epi16(bands[2].0), _mm_set1_epi16(bands[3].0)];
        let val: [__m128i; 4] = [_mm_set1_epi16(bands[0].1), _mm_set1_epi16(bands[1].1), _mm_set1_epi16(bands[2].1), _mm_set1_epi16(bands[3].1)];
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let v = unsafe { _mm_loadu_si128(s.add(x) as *const __m128i) };
                    let b = _mm_srl_epi16(v, sh);
                    let mut off = zero;
                    for k in 0..4 {
                        off = _mm_or_si128(off, _mm_and_si128(_mm_cmpeq_epi16(b, idx[k]), val[k]));
                    }
                    let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let v = unsafe { _mm_loadu_si128(s.add(x) as *const __m128i) };
                let b = _mm_srl_epi16(v, sh);
                let mut off = zero;
                for k in 0..4 {
                    off = _mm_or_si128(off, _mm_and_si128(_mm_cmpeq_epi16(b, idx[k]), val[k]));
                }
                let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *s.add(x) } as i32;
                let bi = (v >> shift) as i16;
                let mut o = 0i32;
                for k in 0..4 {
                    if bands[k].0 == bi {
                        o = bands[k].1 as i32;
                    }
                }
                unsafe { *d.add(x) = (v + o).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// Band offset for the SSE4.1 rung: the four active bands are CONSECUTIVE
    /// from `pos`, so `b - pos` indexes a `pshufb` table directly.
    ///
    /// The SSE2 twin has to spell that out as four `cmpeq`/`and`/`or` triples,
    /// twelve instructions per vector, because `pshufb` is SSSE3 and
    /// `pminuw` SSE4.1. This is the same identity `band_avx2` uses, at half the
    /// width.
    ///
    /// # Safety
    /// As [`band_sse2`].
    #[target_feature(enable = "sse4.1")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn band_sse41(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, shift: u32, pos: i16, offs: [i16; 4], max: i32) {
        let sh = _mm_cvtsi32_si128(shift as i32);
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        let wrap = pos + 4 > 32;
        let mut ent = [super::LUT_BIAS as i8; 16];
        for (k, e) in ent.iter_mut().enumerate().take(4) {
            *e = (offs[k] + super::LUT_BIAS) as i8;
        }
        let lut = unsafe { _mm_loadu_si128(ent.as_ptr() as *const __m128i) };
        let posv = _mm_set1_epi16(pos);
        let mask31 = _mm_set1_epi16(31);
        let clamp = _mm_set1_epi16(15);
        let hi = _mm_set1_epi16(-32768); // 0x8000: zero the odd bytes
        let bias = _mm_set1_epi16(super::LUT_BIAS);
        let nvec = w / 8;
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip, as in `band_sse2` and `band_avx2`: the
            // body is seven instructions and the loop bookkeeping three, so
            // pairing halves the bookkeeping's share. Written one-per-trip
            // first, this kernel measured 3.125 instructions per output --
            // WORSE than the SSE2 twin it replaces, which pairs.
            let one = |x: usize| {
                let v = unsafe { _mm_loadu_si128(s.add(x) as *const __m128i) };
                let bnd = _mm_srl_epi16(v, sh);
                // (b - pos) mod 32, then anything past the four active bands
                // folded onto index 15, whose entry is the bias.
                let d0 = _mm_sub_epi16(bnd, posv);
                let dlt = if wrap { _mm_and_si128(d0, mask31) } else { d0 };
                let idx = _mm_or_si128(_mm_min_epu16(dlt, clamp), hi);
                let off = _mm_sub_epi16(_mm_shuffle_epi8(lut, idx), bias);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
            };
            for i in 0..nvec / 2 {
                one(i * 16);
                one(i * 16 + 8);
            }
            if nvec % 2 == 1 {
                one((nvec - 1) * 8);
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *s.add(x) } as i32;
                let bnd = (v >> shift) as i16;
                let dl = if wrap { (bnd - pos) & 31 } else { bnd - pos };
                let o = if (0..4).contains(&dl) { offs[dl as usize] as i32 } else { 0 };
                unsafe { *d.add(x) = (v + o).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`band_sse2`], and the neighbour offsets must stay in bounds — the
    /// caller restricts this to the interior of a coding tree block.
    #[target_feature(enable = "sse2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn edge_sse2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, da: isize, db: isize, table: [i16; 5], max: i32) {
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        let two = _mm_set1_epi16(2);
        let cat: [__m128i; 5] = [_mm_set1_epi16(0), _mm_set1_epi16(1), _mm_set1_epi16(2), _mm_set1_epi16(3), _mm_set1_epi16(4)];
        let val: [__m128i; 5] = [
            _mm_set1_epi16(table[0]),
            _mm_set1_epi16(table[1]),
            _mm_set1_epi16(table[2]),
            _mm_set1_epi16(table[3]),
            _mm_set1_epi16(table[4]),
        ];
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let p = unsafe { s.add(x) };
                    let v = unsafe { _mm_loadu_si128(p as *const __m128i) };
                    let a = unsafe { _mm_loadu_si128(p.offset(da) as *const __m128i) };
                    let b = unsafe { _mm_loadu_si128(p.offset(db) as *const __m128i) };
                    // sign(v − n) = cmpgt(n, v) − cmpgt(v, n), each being 0 or −1.
                    let sa = _mm_sub_epi16(_mm_cmpgt_epi16(a, v), _mm_cmpgt_epi16(v, a));
                    let sb = _mm_sub_epi16(_mm_cmpgt_epi16(b, v), _mm_cmpgt_epi16(v, b));
                    let e = _mm_add_epi16(_mm_add_epi16(two, sa), sb);
                    let mut off = zero;
                    for k in 0..5 {
                        off = _mm_or_si128(off, _mm_and_si128(_mm_cmpeq_epi16(e, cat[k]), val[k]));
                    }
                    let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let p = unsafe { s.add(x) };
                let v = unsafe { _mm_loadu_si128(p as *const __m128i) };
                let a = unsafe { _mm_loadu_si128(p.offset(da) as *const __m128i) };
                let b = unsafe { _mm_loadu_si128(p.offset(db) as *const __m128i) };
                // sign(v − n) = cmpgt(n, v) − cmpgt(v, n), each being 0 or −1.
                let sa = _mm_sub_epi16(_mm_cmpgt_epi16(a, v), _mm_cmpgt_epi16(v, a));
                let sb = _mm_sub_epi16(_mm_cmpgt_epi16(b, v), _mm_cmpgt_epi16(v, b));
                let e = _mm_add_epi16(_mm_add_epi16(two, sa), sb);
                let mut off = zero;
                for k in 0..5 {
                    off = _mm_or_si128(off, _mm_and_si128(_mm_cmpeq_epi16(e, cat[k]), val[k]));
                }
                let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
            }
            let mut x = nvec * 8;
            while x < w {
                let p = unsafe { s.add(x) };
                let v = unsafe { *p } as i32;
                let a = unsafe { *p.offset(da) } as i32;
                let b = unsafe { *p.offset(db) } as i32;
                let e = (2 + (v - a).signum() + (v - b).signum()) as usize;
                unsafe { *d.add(x) = (v + table[e] as i32).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As [`band_sse2`], and the neighbour offsets must stay in bounds — the
    /// caller restricts this to the interior of a coding tree block.
    #[target_feature(enable = "ssse3")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn edge_ssse3(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, da: isize, db: isize, table: [i16; 5], max: i32) {
        let maxv = _mm_set1_epi16(max as i16);
        let zero = _mm_setzero_si128();
        let two = _mm_set1_epi16(2);
        // The five category offsets as BYTES in a `pshufb` table, biased so
        // they are unsigned. `e | 0x8000` sets each odd byte's high bit, which
        // makes `pshufb` zero that byte, so every `i16` receives `lut[e]` in
        // its low half and nothing in its high half.
        //
        // The same identity `edge_avx2` uses. It replaces five
        // `cmpeq`/`and`/`or` triples -- fifteen instructions per vector -- with
        // an `or`, a `pshufb` and a `sub`. `sao_edge` already refuses the whole
        // SIMD path unless `offsets_fit_lut` holds, so the byte table is always
        // representable.
        let mut ent = [super::LUT_BIAS as i8; 16];
        for (k, e) in ent.iter_mut().enumerate().take(5) {
            *e = (table[k] + super::LUT_BIAS) as i8;
        }
        let lut = unsafe { _mm_loadu_si128(ent.as_ptr() as *const __m128i) };
        let hi = _mm_set1_epi16(-32768);
        let bias = _mm_set1_epi16(super::LUT_BIAS);
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            // Two vectors per trip: same arithmetic, but the loop's own
            // three instructions are paid once per two vectors.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 16 + half * 8;
                    let p = unsafe { s.add(x) };
                    let v = unsafe { _mm_loadu_si128(p as *const __m128i) };
                    let a = unsafe { _mm_loadu_si128(p.offset(da) as *const __m128i) };
                    let b = unsafe { _mm_loadu_si128(p.offset(db) as *const __m128i) };
                    // sign(v − n) = cmpgt(n, v) − cmpgt(v, n), each being 0 or −1.
                    let sa = _mm_sub_epi16(_mm_cmpgt_epi16(a, v), _mm_cmpgt_epi16(v, a));
                    let sb = _mm_sub_epi16(_mm_cmpgt_epi16(b, v), _mm_cmpgt_epi16(v, b));
                    let e = _mm_add_epi16(_mm_add_epi16(two, sa), sb);
                    let off = _mm_sub_epi16(_mm_shuffle_epi8(lut, _mm_or_si128(e, hi)), bias);
                    let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                    unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 8;
                let p = unsafe { s.add(x) };
                let v = unsafe { _mm_loadu_si128(p as *const __m128i) };
                let a = unsafe { _mm_loadu_si128(p.offset(da) as *const __m128i) };
                let b = unsafe { _mm_loadu_si128(p.offset(db) as *const __m128i) };
                // sign(v − n) = cmpgt(n, v) − cmpgt(v, n), each being 0 or −1.
                let sa = _mm_sub_epi16(_mm_cmpgt_epi16(a, v), _mm_cmpgt_epi16(v, a));
                let sb = _mm_sub_epi16(_mm_cmpgt_epi16(b, v), _mm_cmpgt_epi16(v, b));
                let e = _mm_add_epi16(_mm_add_epi16(two, sa), sb);
                let off = _mm_sub_epi16(_mm_shuffle_epi8(lut, _mm_or_si128(e, hi)), bias);
                let r = _mm_min_epi16(_mm_max_epi16(_mm_adds_epi16(v, off), zero), maxv);
                unsafe { _mm_storeu_si128(d.add(x) as *mut __m128i, r) };
            }
            let mut x = nvec * 8;
            while x < w {
                let p = unsafe { s.add(x) };
                let v = unsafe { *p } as i32;
                let a = unsafe { *p.offset(da) } as i32;
                let b = unsafe { *p.offset(db) } as i32;
                let e = (2 + (v - a).signum() + (v - b).signum()) as usize;
                unsafe { *d.add(x) = (v + table[e] as i32).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// A 16-entry byte lookup for `vpshufb`, broadcast to both 128-bit lanes.
    ///
    /// `vpshufb` indexes bytes, and our indices live in `i16` lanes: setting
    /// the high bit of each odd byte makes the instruction emit zero there, so
    /// each `i16` lane comes back holding just `table[index]`.
    ///
    /// The entries are **biased by +32**. SAO offsets are at most ±31 (§7.4.9.3
    /// bounds `sao_offset_abs` by `(1 << (min(bitDepth,10) − 5)) − 1`), so
    /// `offset + 32` fits a byte, and one `vpsubw` at the end recovers the sign
    /// — cheaper than the shift pair a sign-extension would need. An index with
    /// no offset stores the bias itself, which subtracts to zero.
    use super::LUT_BIAS;

    #[target_feature(enable = "avx2")]
    unsafe fn lut16(entries: [i8; 16]) -> __m256i {
        let lo = unsafe { _mm_loadu_si128(entries.as_ptr() as *const __m128i) };
        _mm256_broadcastsi128_si256(lo)
    }

    /// # Safety
    /// As [`band_sse2`]. `pos` is `sao_band_position`; the four offsets apply
    /// to bands `pos..pos+3` modulo 32.
    /// `sao_band_position` is a 5-bit field, so the four active bands MAY wrap
    /// past band 31 — but only when `pos > 28`, which is four of the thirty-two
    /// possible values and vanishingly rare in practice. The wrapping case needs
    /// a `(b − pos) mod 32`; the other 28 do not, because a negative difference
    /// read as unsigned is huge and the clamp already folds it onto the
    /// zero-offset entry. Specialising lifts one instruction per vector out of
    /// the common path.
    ///
    /// # Safety
    /// As [`band_sse2`].
    #[target_feature(enable = "avx2")]
    pub unsafe fn band_avx2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, shift: u32, pos: i16, offs: [i16; 4], max: i32) {
        if pos + 4 <= 32 {
            return unsafe { band_avx2_impl::<false>(dst, dst_stride, src, src_stride, w, h, shift, pos, offs, max) };
        }
        unsafe { band_avx2_impl::<true>(dst, dst_stride, src, src_stride, w, h, shift, pos, offs, max) }
    }

    /// Folds `(b − pos)` into a shuffle index. `WRAP` says the four bands cross
    /// the end of the table and the difference needs masking to 5 bits.
    #[inline(always)]
    fn band_index<const WRAP: bool>(dlt: __m256i, mask31: __m256i) -> __m256i {
        if WRAP {
            unsafe { _mm256_and_si256(dlt, mask31) }
        } else {
            dlt
        }
    }

    /// # Safety
    /// As [`band_avx2`].
    #[target_feature(enable = "avx2")]
    unsafe fn band_avx2_impl<const WRAP: bool>(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, shift: u32, pos: i16, offs: [i16; 4], max: i32) {
        let sh = _mm_cvtsi32_si128(shift as i32);
        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        // The four active bands are CONSECUTIVE from `pos`, so the band index
        // does not need four equality tests — the distance from `pos` IS the
        // table index. That turns 4 compares + 4 masks + 3 ors into one shuffle.
        let mut ent = [LUT_BIAS as i8; 16];
        for k in 0..4 {
            ent[k] = (offs[k] + LUT_BIAS) as i8;
        }
        let lut = unsafe { lut16(ent) };
        let posv = _mm256_set1_epi16(pos);
        let mask31 = _mm256_set1_epi16(31);
        let clamp = _mm256_set1_epi16(15);
        let hi = _mm256_set1_epi16(-32768); // 0x8000: zero the odd bytes
        let bias = _mm256_set1_epi16(LUT_BIAS);
        // A counted loop, not `while x + 16 <= w`: the latter makes LLVM carry
        // a second induction variable for the bound test (`movq`/`addq`/`cmpq`
        // on top of the real one). The trip count is loop-invariant, so hoist it.
        let nvec = w / 16;
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip: the body is six instructions and the loop
            // bookkeeping three, so pairing them halves the bookkeeping's share.
            for i in 0..nvec / 2 {
                for half in 0..2usize {
                    let x = i * 32 + half * 16;
                    let v = unsafe { _mm256_loadu_si256(s.add(x) as *const __m256i) };
                    let b = _mm256_srl_epi16(v, sh);
                    // (b − pos) mod 32, then anything past the four active
                    // bands folded onto index 15, whose entry is the bias.
                    let dlt = band_index::<WRAP>(_mm256_sub_epi16(b, posv), mask31);
                    let idx = _mm256_or_si256(_mm256_min_epu16(dlt, clamp), hi);
                    let off = _mm256_sub_epi16(_mm256_shuffle_epi8(lut, idx), bias);
                    let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(v, off), zero), maxv);
                    unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 16;
                let v = unsafe { _mm256_loadu_si256(s.add(x) as *const __m256i) };
                let b = _mm256_srl_epi16(v, sh);
                let dlt = band_index::<WRAP>(_mm256_sub_epi16(b, posv), mask31);
                let idx = _mm256_or_si256(_mm256_min_epu16(dlt, clamp), hi);
                let off = _mm256_sub_epi16(_mm256_shuffle_epi8(lut, idx), bias);
                let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(v, off), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
            }
        }
        // The width remainder, ONCE for the whole block -- as in `edge_avx2`.
        //
        // `band_sse41`, not `band_sse2`: both finish correctly, but `band_sse2`
        // takes the four active bands as `[(index, offset); 4]` while this
        // kernel holds `(pos, offs)`, so calling it marshalled the array onto
        // the stack with eight `movw`s and reloaded all seven vector constants
        // afterwards. `band_sse41` takes exactly `(pos, offs)`.
        let x = nvec * 16;
        if x < w {
            // SAFETY: the remaining columns of every row are in bounds.
            unsafe { band_sse41(dst.add(x), dst_stride, src.add(x), src_stride, w - x, h, shift, pos, offs, max) };
        }
    }

    /// # Safety
    /// As [`edge_sse2`].
    #[target_feature(enable = "avx2")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn edge_avx2(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, da: isize, db: isize, table: [i16; 5], max: i32) {
        let maxv = _mm256_set1_epi16(max as i16);
        let zero = _mm256_setzero_si256();
        let two = _mm256_set1_epi16(2);
        // `edgeIdx` is already 0..4, so it indexes the shuffle table directly —
        // no compares at all. Five entries, the rest unreachable.
        let mut ent = [LUT_BIAS as i8; 16];
        for k in 0..5 {
            ent[k] = (table[k] + LUT_BIAS) as i8;
        }
        let lut = unsafe { lut16(ent) };
        let hi = _mm256_set1_epi16(-32768);
        let bias = _mm256_set1_epi16(LUT_BIAS);
        let nvec = w / 16;
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            // Two vectors per trip, as in `band_avx2`.
            let body = nvec / 2;
            for i in 0..body {
                for half in 0..2usize {
                    let x = i * 32 + half * 16;
                    let p = unsafe { s.add(x) };
                    let v = unsafe { _mm256_loadu_si256(p as *const __m256i) };
                    let a = unsafe { _mm256_loadu_si256(p.offset(da) as *const __m256i) };
                    let b = unsafe { _mm256_loadu_si256(p.offset(db) as *const __m256i) };
                    let sa = _mm256_sub_epi16(_mm256_cmpgt_epi16(a, v), _mm256_cmpgt_epi16(v, a));
                    let sb = _mm256_sub_epi16(_mm256_cmpgt_epi16(b, v), _mm256_cmpgt_epi16(v, b));
                    let e = _mm256_add_epi16(_mm256_add_epi16(two, sa), sb);
                    let off = _mm256_sub_epi16(_mm256_shuffle_epi8(lut, _mm256_or_si256(e, hi)), bias);
                    let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(v, off), zero), maxv);
                    unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
                }
            }
            if nvec % 2 == 1 {
                let x = (nvec - 1) * 16;
                let p = unsafe { s.add(x) };
                let v = unsafe { _mm256_loadu_si256(p as *const __m256i) };
                let a = unsafe { _mm256_loadu_si256(p.offset(da) as *const __m256i) };
                let b = unsafe { _mm256_loadu_si256(p.offset(db) as *const __m256i) };
                let sa = _mm256_sub_epi16(_mm256_cmpgt_epi16(a, v), _mm256_cmpgt_epi16(v, a));
                let sb = _mm256_sub_epi16(_mm256_cmpgt_epi16(b, v), _mm256_cmpgt_epi16(v, b));
                let e = _mm256_add_epi16(_mm256_add_epi16(two, sa), sb);
                let off = _mm256_sub_epi16(_mm256_shuffle_epi8(lut, _mm256_or_si256(e, hi)), bias);
                let r = _mm256_min_epi16(_mm256_max_epi16(_mm256_adds_epi16(v, off), zero), maxv);
                unsafe { _mm256_storeu_si256(d.add(x) as *mut __m256i, r) };
            }
        }
        // The width remainder, ONCE for the whole block.
        //
        // This used to sit inside the row loop with `h = 1`, so a block of
        // height `h` made `h` calls, each re-deriving the kernel's constants
        // and paying the call. The remaining columns are the same for every
        // row and the callee already loops over `y`, so one call with the real
        // height is the same work with the setup paid once.
        //
        // `edge_ssse3`, not `edge_sse2`: identical signature, but it selects
        // the offset with one `pshufb` where the SSE2 kernel needs five
        // `cmpeq`/`and`/`or` triples -- 2.812 instructions per output against
        // 4.562. AVX2 implies SSSE3, so it is always available here.
        let x = nvec * 16;
        if x < w {
            // SAFETY: the remaining columns of every row are in bounds.
            unsafe { edge_ssse3(dst.add(x), dst_stride, src.add(x), src_stride, w - x, h, da, db, table, max) };
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod arm {
    use std::arch::aarch64::*;

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    pub unsafe fn band_neon(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, shift: u32, bands: [(i16, i16); 4], max: i32) {
        let maxv = vdupq_n_s16(max as i16);
        let zero = vdupq_n_s16(0);
        let sh = vdupq_n_s16(-(shift as i16));
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let v = unsafe { vreinterpretq_s16_u16(vld1q_u16(s.add(x))) };
                let b = vshlq_s16(v, sh);
                let mut off = zero;
                for k in 0..4 {
                    let m = vreinterpretq_s16_u16(vceqq_s16(b, vdupq_n_s16(bands[k].0)));
                    off = vorrq_s16(off, vandq_s16(m, vdupq_n_s16(bands[k].1)));
                }
                let r = vminq_s16(vmaxq_s16(vqaddq_s16(v, off), zero), maxv);
                unsafe { vst1q_u16(d.add(x), vreinterpretq_u16_s16(r)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let v = unsafe { *s.add(x) } as i32;
                let bi = (v >> shift) as i16;
                let mut o = 0i32;
                for k in 0..4 {
                    if bands[k].0 == bi {
                        o = bands[k].1 as i32;
                    }
                }
                unsafe { *d.add(x) = (v + o).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }

    /// # Safety
    /// As the SSE2 twin.
    #[target_feature(enable = "neon")]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn edge_neon(dst: *mut u16, dst_stride: usize, src: *const u16, src_stride: usize, w: usize, h: usize, da: isize, db: isize, table: [i16; 5], max: i32) {
        let maxv = vdupq_n_s16(max as i16);
        let zero = vdupq_n_s16(0);
        for y in 0..h {
            let s = unsafe { src.add(y * src_stride) };
            let d = unsafe { dst.add(y * dst_stride) };
            let nvec = w / 8;
            for i in 0..nvec {
                let x = i * 8;
                let p = unsafe { s.add(x) };
                let v = unsafe { vreinterpretq_s16_u16(vld1q_u16(p)) };
                let a = unsafe { vreinterpretq_s16_u16(vld1q_u16(p.offset(da))) };
                let b = unsafe { vreinterpretq_s16_u16(vld1q_u16(p.offset(db))) };
                let sa = vsubq_s16(vreinterpretq_s16_u16(vcgtq_s16(a, v)), vreinterpretq_s16_u16(vcgtq_s16(v, a)));
                let sb = vsubq_s16(vreinterpretq_s16_u16(vcgtq_s16(b, v)), vreinterpretq_s16_u16(vcgtq_s16(v, b)));
                let e = vaddq_s16(vaddq_s16(vdupq_n_s16(2), sa), sb);
                let mut off = zero;
                for k in 0..5 {
                    let m = vreinterpretq_s16_u16(vceqq_s16(e, vdupq_n_s16(k as i16)));
                    off = vorrq_s16(off, vandq_s16(m, vdupq_n_s16(table[k])));
                }
                let r = vminq_s16(vmaxq_s16(vqaddq_s16(v, off), zero), maxv);
                unsafe { vst1q_u16(d.add(x), vreinterpretq_u16_s16(r)) };
            }
            let mut x = nvec * 8;
            while x < w {
                let p = unsafe { s.add(x) };
                let v = unsafe { *p } as i32;
                let a = unsafe { *p.offset(da) } as i32;
                let b = unsafe { *p.offset(db) } as i32;
                let e = (2 + (v - a).signum() + (v - b).signum()) as usize;
                unsafe { *d.add(x) = (v + table[e] as i32).clamp(0, max) as u16 };
                x += 1;
            }
        }
    }
}

/// Band offset over the rectangle at `(x0, y0)` of two same-shaped planes,
/// `band` holding the 32-entry offset table (only four entries are non-zero).
///
/// Both slices are the WHOLE plane and the rectangle is named by its origin,
/// rather than the caller passing `&plane[origin..]`. Band offset would survive
/// either convention, but [`sao_edge`] reads a halo *before* the origin, so
/// they share this one to keep the two call sites identical.
#[allow(clippy::too_many_arguments)]
/// The bias that makes SAO offsets non-negative inside the `pshufb` table.
///
/// `pshufb` selects bytes, so the offsets are carried as `i8`. They are signed,
/// so they are stored biased by this and un-biased after the shuffle.
pub const LUT_BIAS: i16 = 32;

/// Whether every offset survives the round trip through the `pshufb` table --
/// the precondition the LUT kernels rest on.
///
/// The bound is the UNSIGNED byte range, not `i8`. `pshufb` selects bytes, and
/// the kernel ORs `0x8000` into the index so the odd byte of each `i16` lane
/// reads as zero -- which means the entry comes back as an unsigned byte in
/// `0..=255` and is then un-biased with `sub_epi16`. An entry of `-32` is the
/// byte `0xE0`, which reads back as `224`, and `224 - 32 = 192`, not `-64`.
///
/// So the requirement is `0 <= offset + LUT_BIAS <= 255`, i.e. offsets no more
/// negative than `-32`. At 10-bit the most negative offset is `-31` -- ONE
/// unit of margin. (Checking the `i8` range instead admits `-64`, which decodes
/// as `+192`: an error of exactly 256 in every affected pixel.)
///
/// It is not a formality. HEVC scales SAO offsets by the bit depth
/// (`SaoOffsetVal = offset_abs << (BitDepth - Min(BitDepth, 10))`, and
/// `ctu.rs` implements exactly that), so:
///
/// | bit depth | cMax | shift | offset range | biased | in `0..=255`? |
/// |---|---:|---:|---|---|---|
/// | 8 | 7 | 0 | +/-7 | 25..39 | yes |
/// | 10 | 31 | 0 | +/-31 | 1..63 | yes, by ONE |
/// | 12 | 31 | 2 | +/-124 | **-92 .. 156** | **no -- -92 reads back as +164** |
///
/// The decoder rejects `bit_depth > 10` at the SPS, so this cannot fire today.
/// But the PARSER above is written for the general case, so the day RExt is
/// enabled the offsets grow and every SAO offset silently becomes a different
/// number -- no panic, no failing test, because no HEVC_v1 vector is 12-bit.
///
/// Testing the actual offsets rather than the bit depth keeps the precondition
/// and its check the same statement.
fn offsets_fit_lut(offs: &[i16]) -> bool {
    offs.iter().all(|&o| {
        let b = o as i32 + LUT_BIAS as i32;
        (0..=u8::MAX as i32).contains(&b)
    })
}

pub fn sao_band(dst: &mut [u16], src: &[u16], stride: usize, x0: usize, y0: usize, w: usize, h: usize, shift: u32, pos: u8, band: &[i16; 32], max: i32) {
    let in_bounds = w > 0 && h > 0 && {
        let need = (y0 + h - 1) * stride + x0 + w;
        dst.len() >= need && src.len() >= need
    };
    debug_assert!(in_bounds);
    // Offsets too large for the i8 LUT (RExt magnitudes) route to the scalar
    // twin by design, so they are part of the gate but not of the assertion.
    let ok = in_bounds && offsets_fit_lut(band);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::SAO_BAND_SIMD } else { &census::SAO_BAND_SCALAR }, 1);
        census::bump(&census::SAMPLES_SAO, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
    if ok {
        // The four active bands, as (index, offset) pairs.
        let mut bands = [(-1i16, 0i16); 4];
        for k in 0..4usize {
            bands[k] = ((pos as i16 + k as i16) & 31, band[(pos as usize + k) & 31]);
        }
        let offs = [bands[0].1, bands[1].1, bands[2].1, bands[3].1];
        let o = y0 * stride + x0;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the length check above covers every access.
            match crate::isa() {
                crate::Isa::Avx2 => return unsafe { x86::band_avx2(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, shift, pos as i16, offs, max) },
                crate::Isa::Sse41 => return unsafe { x86::band_sse41(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, shift, pos as i16, offs, max) },
                _ => return unsafe { x86::band_sse2(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, shift, bands, max) },
            }
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above.
        return unsafe { arm::band_neon(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, shift, bands, max) };
    }
    sao_band_scalar(dst, src, stride, x0, y0, w, h, shift, band, max);
}

/// Edge offset over the rectangle at `(x0, y0)`.
///
/// Every sample reads two neighbours, so `src` must hold a one-sample halo
/// around the rectangle in the two directions asked for — which is exactly what
/// the interior of a coding tree block gives you. The bounds check below proves
/// it from `da` / `db` rather than assuming a full ring, so a caller filtering
/// the top-left corner of a picture with a vertical direction still gets the
/// kernel.
#[allow(clippy::too_many_arguments)]
pub fn sao_edge(dst: &mut [u16], src: &[u16], stride: usize, x0: usize, y0: usize, w: usize, h: usize, da: (i32, i32), db: (i32, i32), offs: &[i16; 4], max: i32) {
    let (oa, ob) = (doff(da, stride), doff(db, stride));
    let in_bounds = w > 0 && h > 0 && {
        let first = (y0 * stride + x0) as isize;
        let last = ((y0 + h - 1) * stride + x0 + w - 1) as isize;
        let lo = first + oa.min(ob).min(0);
        let hi = last + oa.max(ob).max(0);
        lo >= 0 && (hi as usize) < src.len() && (last as usize) < dst.len()
    };
    debug_assert!(in_bounds);
    // As in `sao_band`: offsets that don't fit the LUT take the scalar twin.
    let ok = in_bounds && offsets_fit_lut(offs);
    if census::ALWAYS {
        let simd = cfg!(feature = "simd") && crate::isa() != crate::Isa::Scalar && ok;
        census::bump(if simd { &census::SAO_EDGE_SIMD } else { &census::SAO_EDGE_SCALAR }, 1);
        census::bump(&census::SAMPLES_SAO, (w * h) as u64);
    }
    #[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
    if ok {
        let table = edge_offsets(offs);
        let o = y0 * stride + x0;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the check above proves every sample AND both neighbour
            // reads land inside `src`, and every write inside `dst`.
            match crate::isa() {
                crate::Isa::Avx2 => return unsafe { x86::edge_avx2(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, oa, ob, table, max) },
                // SSE4.1 implies SSSE3, which is what the `pshufb` offset
                // table needs. The SSE2 twin has to spell the same selection
                // out as five compare/mask/merge triples.
                crate::Isa::Sse41 => return unsafe { x86::edge_ssse3(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, oa, ob, table, max) },
                _ => return unsafe { x86::edge_sse2(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, oa, ob, table, max) },
            }
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above.
        return unsafe { arm::edge_neon(dst.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, oa, ob, table, max) };
    }
    sao_edge_scalar(dst, src, stride, x0, y0, w, h, da, db, offs, max);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *s
    }

    #[test]
    fn band_matches_scalar() {
        let mut st = 0xfeed_beefu32;
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            let shift = bd as u32 - 5;
            for &(w, h) in &[(8usize, 8usize), (16, 16), (13, 5), (32, 32), (64, 8), (3, 3)] {
                let stride = w + 11;
                let src: Vec<u16> = (0..stride * (h + 2) + 8).map(|_| (lcg(&mut st) >> 12) as u16 & max as u16).collect();
                let mut band = [0i16; 32];
                let pos = (lcg(&mut st) % 32) as usize;
                for k in 0..4 {
                    band[(pos + k) & 31] = ((lcg(&mut st) % 15) as i16) - 7;
                }
                let mut a: Vec<u16> = src.clone();
                let mut b = src.clone();
                sao_band_scalar(&mut a, &src, stride, 1, 1, w, h, shift, &band, max);
                sao_band(&mut b, &src, stride, 1, 1, w, h, shift, pos as u8, &band, max);
                assert_eq!(a, b, "band {w}x{h} bd={bd} pos={pos}");
                // As in `edge_matches_scalar`: `sao_band` dispatches on the
                // HOST's ISA, so the SSE4.1 rung is never executed by the line
                // above on an AVX2 machine. Call it directly.
                #[cfg(all(feature = "simd", target_arch = "x86_64"))]
                if std::is_x86_feature_detected!("sse4.1") {
                    let mut offs = [0i16; 4];
                    for (k, o) in offs.iter_mut().enumerate() {
                        *o = band[(pos + k) & 31];
                    }
                    let mut c = src.clone();
                    let o = stride + 1;
                    // SAFETY: same footprint the dispatcher checks.
                    unsafe { x86::band_sse41(c.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, shift, pos as i16, offs, max) };
                    assert_eq!(a, c, "band sse41 {w}x{h} bd={bd} pos={pos}");
                }
            }
        }
    }

    #[test]
    fn edge_matches_scalar() {
        let mut st = 0x0bad_c0deu32;
        let dirs = [((-1i32, 0i32), (1i32, 0i32)), ((0, -1), (0, 1)), ((-1, -1), (1, 1)), ((1, -1), (-1, 1))];
        for &bd in &[8u8, 10] {
            let max = (1i32 << bd) - 1;
            for &(w, h) in &[(8usize, 8usize), (16, 16), (13, 5), (32, 32), (64, 8), (3, 3)] {
                let stride = w + 11;
                // one row/column of halo around the rectangle
                let total = stride * (h + 2) + 8;
                let src: Vec<u16> = (0..total).map(|_| (lcg(&mut st) >> 12) as u16 & max as u16).collect();
                for &(da, db) in &dirs {
                    let mut offs = [0i16; 4];
                    for o in offs.iter_mut() {
                        *o = ((lcg(&mut st) % 15) as i16) - 7;
                    }
                    let mut a = src.clone();
                    let mut b = src.clone();
                    sao_edge_scalar(&mut a, &src, stride, 1, 1, w, h, da, db, &offs, max);
                    sao_edge(&mut b, &src, stride, 1, 1, w, h, da, db, &offs, max);
                    assert_eq!(a, b, "edge {w}x{h} bd={bd} dir={da:?}");
                    // `sao_edge` dispatches on the HOST's ISA, so on an AVX2
                    // box the SSE rungs would never be executed by the line
                    // above. Call them directly, or a kernel ships having been
                    // proven by nothing.
                    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
                    {
                        let table = edge_offsets(&offs);
                        let (oa, ob) = (doff(da, stride), doff(db, stride));
                        let o = stride + 1;
                        for (name, f) in [
                            ("sse2", x86::edge_sse2 as unsafe fn(*mut u16, usize, *const u16, usize, usize, usize, isize, isize, [i16; 5], i32)),
                            ("ssse3", x86::edge_ssse3 as unsafe fn(*mut u16, usize, *const u16, usize, usize, usize, isize, isize, [i16; 5], i32)),
                        ] {
                            if name == "ssse3" && !std::is_x86_feature_detected!("ssse3") {
                                continue;
                            }
                            let mut c = src.clone();
                            // SAFETY: same footprint the dispatcher checks.
                            unsafe { f(c.as_mut_ptr().add(o), stride, src.as_ptr().add(o), stride, w, h, oa, ob, table, max) };
                            assert_eq!(a, c, "edge {name} {w}x{h} bd={bd} dir={da:?}");
                        }
                    }
                }
            }
        }
    }

    /// SAO must stay bit-exact at bit depths where the offsets outgrow the
    /// `pshufb` table's `i8` entries.
    ///
    /// `edge_matches_scalar` above draws offsets from `[-7, 7]` -- the 8-bit
    /// range -- so it can never reach the failing case. HEVC scales offsets by
    /// the bit depth (`offset_abs << (BitDepth - Min(BitDepth, 10))`), so at
    /// 12-bit they reach +/-124, and `124 + LUT_BIAS = 156` does not fit `i8`:
    /// `as i8` wraps it to -100 and every offset silently becomes a different
    /// number.
    ///
    /// The dispatcher now routes to the scalar twin when the offsets do not
    /// fit, so this passes by FALLBACK, not by the kernel coping. Delete
    /// `offsets_fit_lut` from either `ok` gate and this test fails -- which is
    /// the only thing that makes it a gate.
    #[test]
    fn sao_is_exact_at_rext_offset_magnitudes() {
        let mut st = 0x12e5_7a11u32;
        let dirs = [((-1i32, 0i32), (1i32, 0i32)), ((0, -1), (0, 1)), ((-1, -1), (1, 1)), ((1, -1), (-1, 1))];
        let mut stressed = 0usize;
        for &bd in &[8u8, 10, 12, 14] {
            let max = (1i32 << bd) - 1;
            // Exactly the parser's arithmetic in `ctu.rs`.
            let cmax = (1i32 << (bd.min(10) - 5)) - 1;
            let shift = bd - bd.min(10);
            for &(w, h) in &[(16usize, 16usize), (13, 5), (32, 8)] {
                let stride = w + 11;
                let total = stride * (h + 2) + 8;
                let src: Vec<u16> = (0..total).map(|_| (lcg(&mut st) >> 8) as u16 & max as u16).collect();

                // ---- edge -------------------------------------------------
                for &(da, db) in &dirs {
                    let mut offs = [0i16; 4];
                    for o in offs.iter_mut() {
                        let a = (lcg(&mut st) as i32 % (cmax + 1)) << shift;
                        *o = if lcg(&mut st) & 1 == 0 { a as i16 } else { -a as i16 };
                    }
                    if !offsets_fit_lut(&offs) {
                        stressed += 1;
                    }
                    let mut a = src.clone();
                    let mut b = src.clone();
                    sao_edge_scalar(&mut a, &src, stride, 1, 1, w, h, da, db, &offs, max);
                    sao_edge(&mut b, &src, stride, 1, 1, w, h, da, db, &offs, max);
                    assert_eq!(a, b, "edge bd={bd} {w}x{h} offs={offs:?}");
                }

                // ---- band -------------------------------------------------
                let mut band = [0i16; 32];
                let pos = (lcg(&mut st) % 32) as u8;
                for k in 0..4usize {
                    let a = (lcg(&mut st) as i32 % (cmax + 1)) << shift;
                    band[(pos as usize + k) & 31] = if lcg(&mut st) & 1 == 0 { a as i16 } else { -a as i16 };
                }
                if !offsets_fit_lut(&band) {
                    stressed += 1;
                }
                let bshift = bd as u32 - 5;
                let mut a = src.clone();
                let mut b = src.clone();
                sao_band_scalar(&mut a, &src, stride, 1, 1, w, h, bshift, &band, max);
                sao_band(&mut b, &src, stride, 1, 1, w, h, bshift, pos, &band, max);
                assert_eq!(a, b, "band bd={bd} {w}x{h} pos={pos}");
            }
        }
        assert!(stressed > 0, "no case exceeded the i8 table -- the test never reached the condition it exists to cover");
    }
}
