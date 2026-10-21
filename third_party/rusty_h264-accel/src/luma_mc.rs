//! Half-pel **luma** motion compensation — portable SIMD (rip-ASM Phase 2).
//!
//! Replaces openh264's `McHorVer20*`, `McHorVer02*`, `McHorVer22*` and `PixelAvg*`
//! (4,490 LOC of NASM) with Rust intrinsics. This is the largest SIMD-addressable
//! bucket in the decoder: inter-mc measures **10–17% of decode** in the anatomy.
//!
//! ## The filters (H.264 §8.4.2.2.1)
//!
//! The half-pel interpolator is the 6-tap `(1, −5, 20, 20, −5, 1)`:
//!
//! ```text
//! b (horizontal) = clip(( 6tap_h + 16) >>  5)
//! h (vertical)   = clip(( 6tap_v + 16) >>  5)
//! j (centre)     = clip(( 6tap applied twice, at full precision, + 512) >> 10)
//! quarter-pel    = (a + b + 1) >> 1   -- an average of two of the above
//! ```
//!
//! ## Three facts that shape the implementation
//!
//! **1. The single-pass filters fit in `i16`.** With 8-bit input the 6-tap ranges over
//! `[-2550, 10710]`, so `b` and `h` are computed entirely in 16-bit lanes — 8 pixels per
//! 128-bit register, no widening.
//!
//! **2. The centre filter does NOT.** `j` applies the 6-tap to 6-tap outputs, reaching
//! `±475320`, so the second pass must accumulate in `i32`. Pass one stays `i16` (it is
//! just `b` without the shift), pass two widens.
//!
//! **3. No multiplies are needed.** `20 = 16 + 4` and `5 = 4 + 1`, so every tap is
//! shifts and adds. That keeps the x86 path **SSE2-only** — `_mm_mullo_epi32` is SSE4.1
//! and would have forced either a raised baseline or a second code path.
//!
//! The saturating narrows do the clip for free: `_mm_packus_epi16` on x86, and on NEON
//! `vqrshrun_n_s16::<5>` performs round-add, shift, clamp and narrow in one instruction.
//!
//! Ordering note: the spec defines `j` via vertical-then-horizontal, and the scalar twin
//! in `common::inter` does it that way. This uses **horizontal-then-vertical**, which is
//! identical arithmetic — the convolution is separable and every intermediate is exact —
//! and is far better for SIMD, because the second pass then reads whole rows with no
//! lane shuffling. The tests pin it against the scalar oracle either way.

// ---------------------------------------------------------------------------------
// Scalar references. The oracles, and the fallback on other architectures.
// ---------------------------------------------------------------------------------

#[inline]
fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// 6-tap over six already-loaded samples.
#[inline]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

fn hor20_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2] as i32, src[p - 1] as i32, src[p] as i32,
                src[p + 1] as i32, src[p + 2] as i32, src[p + 3] as i32,
            );
            dst[r * w + c] = clip_u8((f + 16) >> 5);
        }
    }
}

fn ver02_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2 * ts] as i32, src[p - ts] as i32, src[p] as i32,
                src[p + ts] as i32, src[p + 2 * ts] as i32, src[p + 3 * ts] as i32,
            );
            dst[r * w + c] = clip_u8((f + 16) >> 5);
        }
    }
}

fn centre_scalar(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize) {
    // horizontal-first at full precision, then vertical (see module docs).
    // FIXED ARRAY: this is called per MC block, so a `vec!` here would allocate on
    // every block. The largest case is 16 wide x (16+5) rows.
    let mut hor = [0i32; 21 * 16];
    for rr in 0..h + 5 {
        for c in 0..w {
            let p = rr * ts + c;
            hor[rr * w + c] = tap6(
                t[p] as i32, t[p + 1] as i32, t[p + 2] as i32,
                t[p + 3] as i32, t[p + 4] as i32, t[p + 5] as i32,
            );
        }
    }
    for r in 0..h {
        for c in 0..w {
            let g = |k: usize| hor[(r + k) * w + c];
            let f = tap6(g(0), g(1), g(2), g(3), g(4), g(5));
            dst[r * w + c] = clip_u8((f + 512) >> 10);
        }
    }
}

fn pixel_avg_scalar(
    dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
) {
    for r in 0..h {
        for c in 0..w {
            dst[r * w + c] = ((a[r * a_stride + c] as u32 + b[r * b_stride + c] as u32 + 1) >> 1) as u8;
        }
    }
}

/// One-filter horizontal qpel: `pavgb( clip((6tap_h+16)>>5), full[+fdc] )`.
/// Byte-identical to `hor20` into scratch then `pixel_avg` vs the integer plane.
fn hor_qpel_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2] as i32, src[p - 1] as i32, src[p] as i32,
                src[p + 1] as i32, src[p + 2] as i32, src[p + 3] as i32,
            );
            let half = clip_u8((f + 16) >> 5);
            let full = src[p + fdc];
            dst[r * w + c] = ((half as u32 + full as u32 + 1) >> 1) as u8;
        }
    }
}

/// One-filter vertical qpel: `pavgb( clip((6tap_v+16)>>5), full[+fdr*ts] )`.
fn ver_qpel_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2 * ts] as i32, src[p - ts] as i32, src[p] as i32,
                src[p + ts] as i32, src[p + 2 * ts] as i32, src[p + 3 * ts] as i32,
            );
            let half = clip_u8((f + 16) >> 5);
            let full = src[p + fdr * ts];
            dst[r * w + c] = ((half as u32 + full as u32 + 1) >> 1) as u8;
        }
    }
}

// ---------------------------------------------------------------------------------
// x86-64 SSE2
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    /// Load 8 bytes at `p`, zero-extend to 8 lanes of i16.
    #[inline]
        unsafe fn ld8(p: *const u8) -> __m128i {
        _mm_unpacklo_epi8(_mm_loadl_epi64(p as *const __m128i), _mm_setzero_si128())
    }

    /// `a - 5b + 20c + 20d - 5e + f` in i16 lanes, via shifts (20=16+4, 5=4+1).
    #[inline]
        unsafe fn tap6_epi16(a: __m128i, b: __m128i, c: __m128i, d: __m128i, e: __m128i, f: __m128i) -> __m128i {
        // ONE `pmullw` each for the x5 and x20 (kernel round 2026-09-05): the
        // shift-and-add forms were 2 + 3 ops per vector for the same i16 result
        // (b+e <= 510, x5 <= 2550; c+d <= 510, x20 <= 10200 -- no wrap either way).
        let s = _mm_add_epi16(b, e);
        let t = _mm_add_epi16(c, d);
        let five = _mm_mullo_epi16(s, _mm_set1_epi16(5));
        let twenty = _mm_mullo_epi16(t, _mm_set1_epi16(20));
        _mm_add_epi16(_mm_sub_epi16(_mm_add_epi16(a, f), five), twenty)
    }

    /// Same shape in i32 lanes, for the centre filter's second pass.
    #[inline]
        unsafe fn tap6_epi32(a: __m128i, b: __m128i, c: __m128i, d: __m128i, e: __m128i, f: __m128i) -> __m128i {
        let s = _mm_add_epi32(b, e);
        let t = _mm_add_epi32(c, d);
        let five = _mm_add_epi32(_mm_slli_epi32::<2>(s), s);
        let twenty = _mm_add_epi32(_mm_slli_epi32::<4>(t), _mm_slli_epi32::<2>(t));
        _mm_add_epi32(_mm_sub_epi32(_mm_add_epi32(a, f), five), twenty)
    }

    /// `clip((v + 16) >> 5)` for 8 i16 lanes -> 8 packed u8. `packus` does the clip.
    /// `(v + 16) >> 5` == `pmulhrsw(v, 1024)` exactly for every tap value
    /// (-2550..=10710): one SSSE3 op for the add+shift pair (the build is
    /// x86-64-v3, so SSSE3 is a compile-time baseline here).
    #[inline]
        unsafe fn round_shift_pack(v: __m128i) -> __m128i {
        let r = _mm_mulhrs_epi16(v, _mm_set1_epi16(1024));
        _mm_packus_epi16(r, _mm_setzero_si128())
    }

        pub unsafe fn hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2)), ld8(p.sub(1)), ld8(p),
                    ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)),
                );
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, round_shift_pack(v));
                c += 8;
            }
        }
    }

        pub unsafe fn ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, round_shift_pack(v));
                c += 8;
            }
        }
    }

    /// Fused horizontal half + avg vs full-pel at +`fdc` (McHorVer10/30).
    pub unsafe fn hor_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2)), ld8(p.sub(1)), ld8(p),
                    ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)),
                );
                let half = round_shift_pack(v);
                let full = _mm_loadl_epi64(p.add(fdc) as *const __m128i);
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, _mm_avg_epu8(half, full));
                c += 8;
            }
        }
    }

    /// Fused vertical half + avg vs full-pel at +`fdr` rows (McHorVer01/03).
    pub unsafe fn ver_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                let half = round_shift_pack(v);
                let full = _mm_loadl_epi64(p.add(fdr * ts) as *const __m128i);
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, _mm_avg_epu8(half, full));
                c += 8;
            }
        }
    }


        pub unsafe fn centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hor: &mut [i16]) {
        centre_pass1(t, ts, w, h, hor);
        centre_pass2(dst, w, h, hor);
    }

    /// Pass 1: horizontal 6-tap, full precision, into i16 (range [-2550, 10710]).
        pub unsafe fn centre_pass1(t: &[u8], ts: usize, w: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let row = tp.add(rr * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p), ld8(p.add(1)), ld8(p.add(2)),
                    ld8(p.add(3)), ld8(p.add(4)), ld8(p.add(5)),
                );
                _mm_storeu_si128(hor.as_mut_ptr().add(rr * w + c) as *mut __m128i, v);
                c += 8;
            }
        }
    }

    /// Pass 2: vertical 6-tap on the i16 intermediates, accumulating in i32.
        pub unsafe fn centre_pass2(dst: &mut [u8], w: usize, h: usize, hor: &[i16]) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        let round = _mm_set1_epi32(512);
        for r in 0..h {
            let mut c = 0;
            while c < w {
                // sign-extend i16 -> i32 for the low and high halves of each row
                let load = |k: usize| -> (__m128i, __m128i) {
                    let x = _mm_loadu_si128(hp.add((r + k) * w + c) as *const __m128i);
                    (
                        _mm_srai_epi32::<16>(_mm_unpacklo_epi16(x, x)),
                        _mm_srai_epi32::<16>(_mm_unpackhi_epi16(x, x)),
                    )
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                let lo = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a0, b0, c0, d0, e0, f0), round));
                let hi = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a1, b1, c1, d1, e1, f1), round));
                // packs (i32->i16, saturating) then packus (i16->u8, saturating) = clip
                let packed = _mm_packus_epi16(_mm_packs_epi32(lo, hi), _mm_setzero_si128());
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, packed);
                c += 8;
            }
        }
    }

    /// Fused (2,1)/(2,3) tail: centre pass 2 + average with the horizontal
    /// half-pel derived from the SAME pass-1 buffer. b at output row r is
    /// clip((hor[(r + 2 + fdr) * w + c] + 16) >> 5) — pass 1 already computed
    /// the full-precision horizontal 6-tap for every row the vertical filter
    /// needs, and the hor-half IS its rounded form. The separate luma_h pass,
    /// its staging buffer, and the pixel_avg pass all collapse into this loop.
    pub unsafe fn centre_pass2_hq(dst: &mut [u8], w: usize, h: usize, hor: &[i16], fdr: usize) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        let round = _mm_set1_epi32(512);
        let r16 = _mm_set1_epi16(16);
        for r in 0..h {
            let mut c = 0;
            while c < w {
                let load = |k: usize| -> (__m128i, __m128i) {
                    let x = _mm_loadu_si128(hp.add((r + k) * w + c) as *const __m128i);
                    (
                        _mm_srai_epi32::<16>(_mm_unpacklo_epi16(x, x)),
                        _mm_srai_epi32::<16>(_mm_unpackhi_epi16(x, x)),
                    )
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                let lo = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a0, b0, c0, d0, e0, f0), round));
                let hi = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a1, b1, c1, d1, e1, f1), round));
                let j = _mm_packus_epi16(_mm_packs_epi32(lo, hi), _mm_setzero_si128());
                let braw = _mm_loadu_si128(hp.add((r + 2 + fdr) * w + c) as *const __m128i);
                let b8 = _mm_packus_epi16(_mm_srai_epi16::<5>(_mm_add_epi16(braw, r16)), _mm_setzero_si128());
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, _mm_avg_epu8(j, b8));
                c += 8;
            }
        }
    }

    /// Vertical-first pass 1 for the (1,2)/(3,2) fusion: full-precision
    /// VERTICAL 6-tap into i16, `w + 5` columns x `h` rows at stride `w + 8`
    /// (the last 8-lane chunk is re-aligned so no read leaves the tile).
    /// Order-swapped separable filtering is exact — the shipped scalar centre
    /// is vertical-first and the SSE2 centre horizontal-first, byte-identical.
    pub unsafe fn centre_pass1v(t: &[u8], ts: usize, w: usize, h: usize, ver: &mut [i16]) {
        let tp = t.as_ptr();
        let vs = w + 8;
        let wide = w + 5;
        for r in 0..h {
            let mut cc = 0usize;
            loop {
                let c0 = if cc + 8 > wide { wide - 8 } else { cc };
                let p = tp.add(r * ts + c0);
                let v = tap6_epi16(
                    ld8(p), ld8(p.add(ts)), ld8(p.add(2 * ts)),
                    ld8(p.add(3 * ts)), ld8(p.add(4 * ts)), ld8(p.add(5 * ts)),
                );
                _mm_storeu_si128(ver.as_mut_ptr().add(r * vs + c0) as *mut __m128i, v);
                if cc + 8 >= wide {
                    break;
                }
                cc += 8;
            }
        }
    }

    /// Fused (1,2)/(3,2) tail: horizontal 6-tap over the vertical-first pass-1
    /// buffer + average with the vertical half-pel derived from the same
    /// buffer (v at output col c = clip((ver[r][c + 2 + fdc] + 16) >> 5)).
    pub unsafe fn centre_pass2v_hq(dst: &mut [u8], w: usize, h: usize, ver: &[i16], fdc: usize) {
        let vp = ver.as_ptr();
        let dp = dst.as_mut_ptr();
        let vs = w + 8;
        let round = _mm_set1_epi32(512);
        let r16 = _mm_set1_epi16(16);
        for r in 0..h {
            let mut c = 0;
            while c < w {
                let base = vp.add(r * vs + c);
                let load = |k: usize| -> (__m128i, __m128i) {
                    let x = _mm_loadu_si128(base.add(k) as *const __m128i);
                    (
                        _mm_srai_epi32::<16>(_mm_unpacklo_epi16(x, x)),
                        _mm_srai_epi32::<16>(_mm_unpackhi_epi16(x, x)),
                    )
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                let lo = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a0, b0, c0, d0, e0, f0), round));
                let hi = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a1, b1, c1, d1, e1, f1), round));
                let j = _mm_packus_epi16(_mm_packs_epi32(lo, hi), _mm_setzero_si128());
                let vraw = _mm_loadu_si128(base.add(2 + fdc) as *const __m128i);
                let v8 = _mm_packus_epi16(_mm_srai_epi16::<5>(_mm_add_epi16(vraw, r16)), _mm_setzero_si128());
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, _mm_avg_epu8(j, v8));
                c += 8;
            }
        }
    }

    /// Fused HV-diagonal qpel ((1,1)/(3,1)/(1,3)/(3,3)): horizontal half +
    /// vertical half + average in ONE loop — the `a` staging write/read and
    /// the second kernel-call round-trip are gone.
    pub unsafe fn hv_qpel(src: &[u8], hoff: usize, voff: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let mut c = 0;
            while c < w {
                let ph = sp.add(hoff + r * ts + c);
                let hv = tap6_epi16(
                    ld8(ph.sub(2)), ld8(ph.sub(1)), ld8(ph),
                    ld8(ph.add(1)), ld8(ph.add(2)), ld8(ph.add(3)),
                );
                let hhalf = round_shift_pack(hv);
                let pv = sp.add(voff + r * ts + c);
                let vv = tap6_epi16(
                    ld8(pv.sub(2 * ts)), ld8(pv.sub(ts)), ld8(pv),
                    ld8(pv.add(ts)), ld8(pv.add(2 * ts)), ld8(pv.add(3 * ts)),
                );
                let vhalf = round_shift_pack(vv);
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, _mm_avg_epu8(hhalf, vhalf));
                c += 8;
            }
        }
    }

        pub unsafe fn pixel_avg(
        dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
    ) {
        let (dp, ap, bp) = (dst.as_mut_ptr(), a.as_ptr(), b.as_ptr());
        for r in 0..h {
            let (ar, br, dr) = (ap.add(r * a_stride), bp.add(r * b_stride), dp.add(r * w));
            if w == 16 {
                // _mm_avg_epu8 IS (a + b + 1) >> 1 — the exact rounding the spec wants.
                let v = _mm_avg_epu8(_mm_loadu_si128(ar as *const __m128i), _mm_loadu_si128(br as *const __m128i));
                _mm_storeu_si128(dr as *mut __m128i, v);
            } else if w == 8 {
                let v = _mm_avg_epu8(_mm_loadl_epi64(ar as *const __m128i), _mm_loadl_epi64(br as *const __m128i));
                _mm_storel_epi64(dr as *mut __m128i, v);
            } else {
                let va = _mm_cvtsi32_si128(ar.cast::<u32>().read_unaligned() as i32);
                let vb = _mm_cvtsi32_si128(br.cast::<u32>().read_unaligned() as i32);
                let v = _mm_avg_epu8(va, vb);
                dr.cast::<u32>().write_unaligned(_mm_cvtsi128_si32(v) as u32);
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// x86-64 AVX2 — the 16-wide path.
//
// The openh264 assembly this replaces had AVX2 kernels (`McHorVer20_avx2`,
// `McHorVer02_avx2`), and dropping to SSE2-only measured a consistent (if
// floor-limited) ~2% on decode. A 16-wide block is exactly 16 i16 lanes = one 256-bit
// register, so the whole row is one pass instead of two.
//
// The pack is the only subtlety: `_mm256_packus_epi16` works WITHIN 128-bit lanes, so
// the halves come out interleaved and need a `permute4x64` to put them back in order.
// ---------------------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
mod x86_avx2 {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn ld16(p: *const u8) -> __m256i {
        _mm256_cvtepu8_epi16(_mm_loadu_si128(p as *const __m128i))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn tap6(a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i, f: __m256i) -> __m256i {
        let s = _mm256_add_epi16(b, e);
        let t = _mm256_add_epi16(c, d);
        let five = _mm256_mullo_epi16(s, _mm256_set1_epi16(5));
        let twenty = _mm256_mullo_epi16(t, _mm256_set1_epi16(20));
        _mm256_add_epi16(_mm256_sub_epi16(_mm256_add_epi16(a, f), five), twenty)
    }

    /// clip((v + 16) >> 5) for 16 i16 lanes -> 16 packed u8, lane order restored.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn round_shift_pack16(v: __m256i) -> __m128i {
        // `(v + 16) >> 5` == `(v * 1024 + 2^14) >> 15` == pmulhrsw(v, 1024): ONE op
        // for the add+shift pair, exact for every tap value (-2550..=10710).
        let r = _mm256_mulhrs_epi16(v, _mm256_set1_epi16(1024));
        let p = _mm256_packus_epi16(r, _mm256_setzero_si256());
        _mm256_castsi256_si128(_mm256_permute4x64_epi64::<0b1101_1000>(p))
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn hor20_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(ld16(p.sub(2)), ld16(p.sub(1)), ld16(p), ld16(p.add(1)), ld16(p.add(2)), ld16(p.add(3)));
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, round_shift_pack16(v));
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn ver02_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(
                ld16(p.sub(2 * ts)), ld16(p.sub(ts)), ld16(p),
                ld16(p.add(ts)), ld16(p.add(2 * ts)), ld16(p.add(3 * ts)),
            );
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, round_shift_pack16(v));
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn hor_qpel_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize, fdc: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(ld16(p.sub(2)), ld16(p.sub(1)), ld16(p), ld16(p.add(1)), ld16(p.add(2)), ld16(p.add(3)));
            let half = round_shift_pack16(v);
            let full = _mm_loadu_si128(p.add(fdc) as *const __m128i);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_avg_epu8(half, full));
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn ver_qpel_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize, fdr: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(
                ld16(p.sub(2 * ts)), ld16(p.sub(ts)), ld16(p),
                ld16(p.add(ts)), ld16(p.add(2 * ts)), ld16(p.add(3 * ts)),
            );
            let half = round_shift_pack16(v);
            let full = _mm_loadu_si128(p.add(fdr * ts) as *const __m128i);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_avg_epu8(half, full));
        }
    }


    /// Centre pass 1 only (horizontal, full precision into i16). Pass 2 needs i32 and
    /// stays on the SSE2 path, which is already 4-lane-per-half and gains little here.
    /// Fused HV-diagonal qpel at w == 16: both 6-taps + avg in one loop.
    #[target_feature(enable = "avx2")]
    pub unsafe fn hv_qpel_w16(src: &[u8], hoff: usize, voff: usize, ts: usize, dst: &mut [u8], h: usize) {
        let sp = src.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let ph = sp.add(hoff + r * ts);
            let hv = tap6(
                ld16(ph.sub(2)), ld16(ph.sub(1)), ld16(ph),
                ld16(ph.add(1)), ld16(ph.add(2)), ld16(ph.add(3)),
            );
            let hhalf = round_shift_pack16(hv);
            let pv = sp.add(voff + r * ts);
            let vv = tap6(
                ld16(pv.sub(2 * ts)), ld16(pv.sub(ts)), ld16(pv),
                ld16(pv.add(ts)), ld16(pv.add(2 * ts)), ld16(pv.add(3 * ts)),
            );
            let vhalf = round_shift_pack16(vv);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_avg_epu8(hhalf, vhalf));
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn tap6_epi32_256(a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i, f: __m256i) -> __m256i {
        let s = _mm256_add_epi32(b, e);
        let t = _mm256_add_epi32(c, d);
        let five = _mm256_mullo_epi32(s, _mm256_set1_epi32(5));
        let twenty = _mm256_mullo_epi32(t, _mm256_set1_epi32(20));
        _mm256_add_epi32(_mm256_sub_epi32(_mm256_add_epi32(a, f), five), twenty)
    }

    /// One 8-col chunk of centre pass 2: vertical 6-tap over i16 rows in
    /// 8-lane i32, returning the rounded row chunk as 8 i16 (in a __m128i).
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn pass2_chunk(hp: *const i16, stride: usize) -> __m128i {
        let ld = |k: usize| _mm256_cvtepi16_epi32(_mm_loadu_si128(hp.add(k * stride) as *const __m128i));
        let v = tap6_epi32_256(ld(0), ld(1), ld(2), ld(3), ld(4), ld(5));
        let r = _mm256_srai_epi32::<10>(_mm256_add_epi32(v, _mm256_set1_epi32(512)));
        _mm_packs_epi32(_mm256_castsi256_si128(r), _mm256_extracti128_si256::<1>(r))
    }

    /// AVX2 centre pass 2 at w == 16: 8 i32 lanes per op instead of SSE2's 4,
    /// one 16-byte store per row.
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2_w16(dst: &mut [u8], h: usize, hor: &[i16]) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let c0 = pass2_chunk(hp.add(r * 16), 16);
            let c1 = pass2_chunk(hp.add(r * 16 + 8), 16);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_packus_epi16(c0, c1));
        }
    }

    /// AVX2 fused (2,1)/(2,3) tail at w == 16: pass 2 + average with the
    /// hor-half derived from the pass-1 buffer (round_shift_pack16 IS the
    /// (x+16)>>5 clip-pack the derivation needs).
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2_hq_w16(dst: &mut [u8], h: usize, hor: &[i16], fdr: usize) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let c0 = pass2_chunk(hp.add(r * 16), 16);
            let c1 = pass2_chunk(hp.add(r * 16 + 8), 16);
            let j = _mm_packus_epi16(c0, c1);
            let braw = _mm256_loadu_si256(hp.add((r + 2 + fdr) * 16) as *const __m256i);
            let b8 = round_shift_pack16(braw);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_avg_epu8(j, b8));
        }
    }

    /// AVX2 fused (1,2)/(3,2) tail at w == 16 over the vertical-first pass-1
    /// buffer (stride w + 8): horizontal 6-tap chunks + v-half derive + avg.
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2v_hq_w16(dst: &mut [u8], h: usize, ver: &[i16], fdc: usize) {
        let vp = ver.as_ptr();
        let dp = dst.as_mut_ptr();
        let vs = 16 + 8;
        for r in 0..h {
            let base = vp.add(r * vs);
            let chunk = |c: usize| -> __m128i {
                let ld = |k: usize| _mm256_cvtepi16_epi32(_mm_loadu_si128(base.add(c + k) as *const __m128i));
                let v = tap6_epi32_256(ld(0), ld(1), ld(2), ld(3), ld(4), ld(5));
                let rr = _mm256_srai_epi32::<10>(_mm256_add_epi32(v, _mm256_set1_epi32(512)));
                _mm_packs_epi32(_mm256_castsi256_si128(rr), _mm256_extracti128_si256::<1>(rr))
            };
            let j = _mm_packus_epi16(chunk(0), chunk(8));
            let vraw = _mm256_loadu_si256(base.add(2 + fdc) as *const __m256i);
            let v8 = round_shift_pack16(vraw);
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, _mm_avg_epu8(j, v8));
        }
    }

    /// AVX2 centre pass 2 at w == 8: one 8-i32-lane chunk per row — the SSE2
    /// form pays 12 unpack ops and two tap chains for the same 8 columns.
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2_w8(dst: &mut [u8], h: usize, hor: &[i16]) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let c = pass2_chunk(hp.add(r * 8), 8);
            _mm_storel_epi64(dp.add(r * 8) as *mut __m128i, _mm_packus_epi16(c, _mm_setzero_si128()));
        }
    }

    /// AVX2 fused (2,1)/(2,3) tail at w == 8.
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2_hq_w8(dst: &mut [u8], h: usize, hor: &[i16], fdr: usize) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        let r16 = _mm_set1_epi16(16);
        for r in 0..h {
            let j = _mm_packus_epi16(pass2_chunk(hp.add(r * 8), 8), _mm_setzero_si128());
            let braw = _mm_loadu_si128(hp.add((r + 2 + fdr) * 8) as *const __m128i);
            let b8 = _mm_packus_epi16(_mm_srai_epi16::<5>(_mm_add_epi16(braw, r16)), _mm_setzero_si128());
            _mm_storel_epi64(dp.add(r * 8) as *mut __m128i, _mm_avg_epu8(j, b8));
        }
    }

    /// AVX2 fused (1,2)/(3,2) tail at w == 8 (vertical-first pass-1 buffer,
    /// stride w + 8 = 16).
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass2v_hq_w8(dst: &mut [u8], h: usize, ver: &[i16], fdc: usize) {
        let vp = ver.as_ptr();
        let dp = dst.as_mut_ptr();
        let vs = 8 + 8;
        let r16 = _mm_set1_epi16(16);
        for r in 0..h {
            let base = vp.add(r * vs);
            let ld = |k: usize| _mm256_cvtepi16_epi32(_mm_loadu_si128(base.add(k) as *const __m128i));
            let v = tap6_epi32_256(ld(0), ld(1), ld(2), ld(3), ld(4), ld(5));
            let rr = _mm256_srai_epi32::<10>(_mm256_add_epi32(v, _mm256_set1_epi32(512)));
            let j16 = _mm_packs_epi32(_mm256_castsi256_si128(rr), _mm256_extracti128_si256::<1>(rr));
            let j = _mm_packus_epi16(j16, _mm_setzero_si128());
            let vraw = _mm_loadu_si128(base.add(2 + fdc) as *const __m128i);
            let v8 = _mm_packus_epi16(_mm_srai_epi16::<5>(_mm_add_epi16(vraw, r16)), _mm_setzero_si128());
            _mm_storel_epi64(dp.add(r * 8) as *mut __m128i, _mm_avg_epu8(j, v8));
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass1_w16(t: &[u8], ts: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let p = tp.add(rr * ts);
            let v = tap6(ld16(p), ld16(p.add(1)), ld16(p.add(2)), ld16(p.add(3)), ld16(p.add(4)), ld16(p.add(5)));
            _mm256_storeu_si256(hor.as_mut_ptr().add(rr * 16) as *mut __m256i, v);
        }
    }
}

// ---------------------------------------------------------------------------------
// aarch64 NEON
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn ld8(p: *const u8) -> int16x8_t {
        vreinterpretq_s16_u16(vmovl_u8(vld1_u8(p)))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn tap6_s16(a: int16x8_t, b: int16x8_t, c: int16x8_t, d: int16x8_t, e: int16x8_t, f: int16x8_t) -> int16x8_t {
        let s = vaddq_s16(b, e);
        let t = vaddq_s16(c, d);
        vaddq_s16(vsubq_s16(vaddq_s16(a, f), vmulq_n_s16(s, 5)), vmulq_n_s16(t, 20))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn tap6_s32(a: int32x4_t, b: int32x4_t, c: int32x4_t, d: int32x4_t, e: int32x4_t, f: int32x4_t) -> int32x4_t {
        let s = vaddq_s32(b, e);
        let t = vaddq_s32(c, d);
        vaddq_s32(vsubq_s32(vaddq_s32(a, f), vmulq_n_s32(s, 5)), vmulq_n_s32(t, 20))
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(ld8(p.sub(2)), ld8(p.sub(1)), ld8(p), ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)));
                // round-add 16, shift 5, saturate to u8, narrow — one instruction.
                vst1_u8(dp.add(r * w + c), vqrshrun_n_s16::<5>(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                vst1_u8(dp.add(r * w + c), vqrshrun_n_s16::<5>(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn hor_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(ld8(p.sub(2)), ld8(p.sub(1)), ld8(p), ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)));
                let half = vqrshrun_n_s16::<5>(v);
                let full = vld1_u8(p.add(fdc));
                vst1_u8(dp.add(r * w + c), vrhadd_u8(half, full));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn ver_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                let half = vqrshrun_n_s16::<5>(v);
                let full = vld1_u8(p.add(fdr * ts));
                vst1_u8(dp.add(r * w + c), vrhadd_u8(half, full));
                c += 8;
            }
        }
    }


    #[target_feature(enable = "neon")]
    pub unsafe fn centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let row = tp.add(rr * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(ld8(p), ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)), ld8(p.add(4)), ld8(p.add(5)));
                vst1q_s16(hor.as_mut_ptr().add(rr * w + c), v);
                c += 8;
            }
        }
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let mut c = 0;
            while c < w {
                let load = |k: usize| -> (int32x4_t, int32x4_t) {
                    let x = vld1q_s16(hp.add((r + k) * w + c));
                    (vmovl_s16(vget_low_s16(x)), vmovl_high_s16(x))
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                // rounding narrow by 10 (adds 512), then saturate to u8
                let lo = vqrshrn_n_s32::<10>(tap6_s32(a0, b0, c0, d0, e0, f0));
                let hi = vqrshrn_n_s32::<10>(tap6_s32(a1, b1, c1, d1, e1, f1));
                vst1_u8(dp.add(r * w + c), vqmovun_s16(vcombine_s16(lo, hi)));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn pixel_avg(
        dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
    ) {
        let (dp, ap, bp) = (dst.as_mut_ptr(), a.as_ptr(), b.as_ptr());
        for r in 0..h {
            let (ar, br, dr) = (ap.add(r * a_stride), bp.add(r * b_stride), dp.add(r * w));
            if w == 16 {
                // vrhaddq_u8 IS (a + b + 1) >> 1
                vst1q_u8(dr, vrhaddq_u8(vld1q_u8(ar), vld1q_u8(br)));
            } else if w == 8 {
                vst1_u8(dr, vrhadd_u8(vld1_u8(ar), vld1_u8(br)));
            } else {
                let va = vreinterpret_u8_u32(vdup_n_u32(ar.cast::<u32>().read_unaligned()));
                let vb = vreinterpret_u8_u32(vdup_n_u32(br.cast::<u32>().read_unaligned()));
                let v = vrhadd_u8(va, vb);
                let out = vget_lane_u32::<0>(vreinterpret_u32_u8(v));
                dr.cast::<u32>().write_unaligned(out);
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// Safe dispatch
// ---------------------------------------------------------------------------------

/// Horizontal half-pel plane: `clip((6tap_h + 16) >> 5)`, `w` in {8, 16}.
pub fn mc_hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_HOR20.scalar();
        return hor20_scalar(src, off, ts, dst, w, h);
    }
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 && src.len() >= off + (h - 1) * ts + w + 3);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted; 16 lanes is exactly the block width.
            unsafe { x86_avx2::hor20_w16(src, off, ts, dst, h) };
            crate::census::MC_HOR20.wide();
            return;
        }
        if true /* SSE2 is x86-64 baseline; see deblock_simd for why gating costs */ {
            // SAFETY: bounds asserted; taps span off-2 .. off+(h-1)*ts+w+2.
            unsafe { x86::hor20(src, off, ts, dst, w, h) };
            crate::census::MC_HOR20.base();
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
        // SAFETY: as above.
        unsafe { arm::hor20(src, off, ts, dst, w, h) };
        crate::census::MC_HOR20.base();
        return;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::MC_HOR20.scalar();
    hor20_scalar(src, off, ts, dst, w, h);
}

/// Vertical half-pel plane: `clip((6tap_v + 16) >> 5)`, `w` in {8, 16}.
pub fn mc_ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_VER02.scalar();
        return ver02_scalar(src, off, ts, dst, w, h);
    }
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 * ts && src.len() >= off + (h + 2) * ts + w);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: as the SSE2 arm, 16 lanes wide.
            unsafe { x86_avx2::ver02_w16(src, off, ts, dst, h) };
            crate::census::MC_VER02.wide();
            return;
        }
        if true /* SSE2 is x86-64 baseline; see deblock_simd for why gating costs */ {
            // SAFETY: bounds asserted; taps span off-2ts .. off+(h+2)*ts+w.
            unsafe { x86::ver02(src, off, ts, dst, w, h) };
            crate::census::MC_VER02.base();
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
        // SAFETY: as above.
        unsafe { arm::ver02(src, off, ts, dst, w, h) };
        crate::census::MC_VER02.base();
        return;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::MC_VER02.scalar();
    ver02_scalar(src, off, ts, dst, w, h);
}

/// One-filter horizontal qpel (`McHorVer10`/`30`): half-pel 6-tap then `pavgb` vs
/// full-pel at column `+fdc`, in one pass (no 256 B scratch store). `fdc` ∈ {0,1}.
pub fn mc_hor_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_HOR_QPEL.scalar();
        return hor_qpel_scalar(src, off, ts, dst, w, h, fdc);
    }
    debug_assert!(w == 8 || w == 16);
    debug_assert!(fdc <= 1);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 && src.len() >= off + (h - 1) * ts + w + 3 + fdc);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted; 16 lanes = block width.
            unsafe { x86_avx2::hor_qpel_w16(src, off, ts, dst, h, fdc) };
            crate::census::MC_HOR_QPEL.wide();
            return;
        }
        // SAFETY: SSE2 baseline; 8-wide chunks cover w∈{8,16}.
        unsafe { x86::hor_qpel(src, off, ts, dst, w, h, fdc) };
        crate::census::MC_HOR_QPEL.base();
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
        // SAFETY: as above.
        unsafe { arm::hor_qpel(src, off, ts, dst, w, h, fdc) };
        crate::census::MC_HOR_QPEL.base();
        return;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::MC_HOR_QPEL.scalar();
    hor_qpel_scalar(src, off, ts, dst, w, h, fdc);
}

/// One-filter vertical qpel (`McHorVer01`/`03`): half-pel 6-tap then `pavgb` vs
/// full-pel at row `+fdr`, in one pass. `fdr` ∈ {0,1}.
pub fn mc_ver_qpel(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_VER_QPEL.scalar();
        return ver_qpel_scalar(src, off, ts, dst, w, h, fdr);
    }
    debug_assert!(w == 8 || w == 16);
    debug_assert!(fdr <= 1);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 * ts && src.len() >= off + (h + 2) * ts + w);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted.
            unsafe { x86_avx2::ver_qpel_w16(src, off, ts, dst, h, fdr) };
            crate::census::MC_VER_QPEL.wide();
            return;
        }
        // SAFETY: SSE2 baseline.
        unsafe { x86::ver_qpel(src, off, ts, dst, w, h, fdr) };
        crate::census::MC_VER_QPEL.base();
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
        // SAFETY: as above.
        unsafe { arm::ver_qpel(src, off, ts, dst, w, h, fdr) };
        crate::census::MC_VER_QPEL.base();
        return;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::MC_VER_QPEL.scalar();
    ver_qpel_scalar(src, off, ts, dst, w, h, fdr);
}


/// Centre half-pel plane: `clip((6tap applied twice + 512) >> 10)`, `w` in {8, 16}.
pub fn mc_centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_CENTRE.scalar();
        return centre_scalar(t, ts, dst, w, h);
    }
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(t.len() >= (h + 4) * ts + w + 5);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        // (h+5) rows x w cols of i16 intermediates; 16x16 worst case = 21*16.
        let mut hor = [0i16; 21 * 16];
        #[cfg(target_arch = "x86_64")]
        {
            if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
                // Pass 1 is 16 i16 lanes = one register per row; pass 2 now runs
                // 8 i32 lanes per op (was SSE2 4-per-half).
                // SAFETY: bounds asserted; scratch sized for the largest block.
                unsafe {
                    x86_avx2::centre_pass1_w16(t, ts, h, &mut hor);
                    x86_avx2::centre_pass2_w16(dst, h, &hor);
                }
                crate::census::MC_CENTRE.wide();
                return;
            }
            if w == 8 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
                // SAFETY: bounds asserted; one 8-i32-lane chunk per row.
                unsafe {
                    x86::centre_pass1(t, ts, w, h, &mut hor);
                    x86_avx2::centre_pass2_w8(dst, h, &hor);
                }
                crate::census::MC_CENTRE.wide();
                return;
            }
            if true /* SSE2 is x86-64 baseline; see deblock_simd for why gating costs */ {
                // SAFETY: bounds asserted; scratch is sized for the largest block.
                unsafe { x86::centre(t, ts, dst, w, h, &mut hor) };
                crate::census::MC_CENTRE.base();
                return;
            }
        }
        #[cfg(target_arch = "aarch64")]
        if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
            // SAFETY: as above.
            unsafe { arm::centre(t, ts, dst, w, h, &mut hor) };
            crate::census::MC_CENTRE.base();
            return;
        }
        let _ = &mut hor;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::MC_CENTRE.scalar();
    centre_scalar(t, ts, dst, w, h);
}

/// Fused centre-adjacent quarter-pel, horizontal flavour ((2,1) fdr=0,
/// (2,3) fdr=1): ONE pass-1 + one fused pass-2/avg instead of the 3-kernel
/// 2-staging compose. Byte-identical by construction: the hor-half is the
/// rounded form of the pass-1 rows the centre already computes.
pub fn mc_centre_hq(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_CENTRE_HQ.scalar();
        return centre_hq_scalar(t, ts, dst, w, h, fdr);
    }
    debug_assert!(w == 8 || w == 16);
    debug_assert!(fdr <= 1);
    assert!(dst.len() >= w * h);
    assert!(t.len() >= (h + 4) * ts + w + 5);
    #[cfg(target_arch = "x86_64")]
    {
        let mut hor = [0i16; 21 * 16];
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted; scratch sized for the largest block.
            unsafe {
                x86_avx2::centre_pass1_w16(t, ts, h, &mut hor);
                x86_avx2::centre_pass2_hq_w16(dst, h, &hor, fdr);
            }
            crate::census::MC_CENTRE_HQ.wide();
            return;
        }
        if w == 8 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted.
            unsafe {
                x86::centre_pass1(t, ts, w, h, &mut hor);
                x86_avx2::centre_pass2_hq_w8(dst, h, &hor, fdr);
            }
            crate::census::MC_CENTRE_HQ.wide();
            return;
        }
        // SAFETY: bounds asserted; scratch sized for the largest block.
        unsafe {
            x86::centre_pass1(t, ts, w, h, &mut hor);
            x86::centre_pass2_hq(dst, w, h, &hor, fdr);
        }
        crate::census::MC_CENTRE_HQ.base();
        return;
    }
    #[allow(unreachable_code)]
    crate::census::MC_CENTRE_HQ.scalar();
    centre_hq_scalar(t, ts, dst, w, h, fdr);
}

/// Fused centre-adjacent quarter-pel, vertical flavour ((1,2) fdc=0,
/// (3,2) fdc=1): vertical-first pass 1, then fused horizontal pass-2/avg.
pub fn mc_centre_vq(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_CENTRE_VQ.scalar();
        return centre_vq_scalar(t, ts, dst, w, h, fdc);
    }
    debug_assert!(w == 8 || w == 16);
    debug_assert!(fdc <= 1);
    assert!(dst.len() >= w * h);
    assert!(t.len() >= (h + 4) * ts + w + 5);
    #[cfg(target_arch = "x86_64")]
    {
        // (w+8)-stride x h rows of i16; 16x16 worst case = 24*16.
        let mut ver = [0i16; 24 * 16];
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted; pass1v re-aligns its last chunk so no
            // read leaves the tile; scratch sized for the largest block.
            unsafe {
                x86::centre_pass1v(t, ts, w, h, &mut ver);
                x86_avx2::centre_pass2v_hq_w16(dst, h, &ver, fdc);
            }
            crate::census::MC_CENTRE_VQ.wide();
            return;
        }
        if w == 8 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: as above.
            unsafe {
                x86::centre_pass1v(t, ts, w, h, &mut ver);
                x86_avx2::centre_pass2v_hq_w8(dst, h, &ver, fdc);
            }
            crate::census::MC_CENTRE_VQ.wide();
            return;
        }
        // SAFETY: as above.
        unsafe {
            x86::centre_pass1v(t, ts, w, h, &mut ver);
            x86::centre_pass2v_hq(dst, w, h, &ver, fdc);
        }
        crate::census::MC_CENTRE_VQ.base();
        return;
    }
    #[allow(unreachable_code)]
    crate::census::MC_CENTRE_VQ.scalar();
    centre_vq_scalar(t, ts, dst, w, h, fdc);
}

/// Fused HV-diagonal qpel: hor-half at `(hdr, hdc)`, ver-half at `(vdr, vdc)`,
/// averaged — one loop, no staging. Offsets follow `avg_full`'s convention.
#[allow(clippy::too_many_arguments)]
pub fn mc_hv_qpel(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hdr: usize, hdc: usize, vdr: usize, vdc: usize) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_HV_QPEL.scalar();
        return hv_qpel_scalar(t, ts, dst, w, h, hdr, hdc, vdr, vdc);
    }
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(t.len() >= (h + 4) * ts + w + 5);
    let hoff = (2 + hdr) * ts + 2 + hdc;
    let voff = (2 + vdr) * ts + 2 + vdc;
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && crate::have_avx2() /* VENDOR(uefi-player): no_std => assumed max */ {
            // SAFETY: bounds asserted; 16 lanes = block width.
            unsafe { x86_avx2::hv_qpel_w16(t, hoff, voff, ts, dst, h) };
            crate::census::MC_HV_QPEL.wide();
            return;
        }
        // SAFETY: bounds asserted; SSE2 baseline.
        unsafe { x86::hv_qpel(t, hoff, voff, ts, dst, w, h) };
        crate::census::MC_HV_QPEL.base();
        return;
    }
    #[allow(unreachable_code)]
    crate::census::MC_HV_QPEL.scalar();
    hv_qpel_scalar(t, ts, dst, w, h, hdr, hdc, vdr, vdc);
}

/// Scalar oracle for `mc_hv_qpel` (also the non-x86 path).
#[allow(clippy::too_many_arguments)]
fn hv_qpel_scalar(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hdr: usize, hdc: usize, vdr: usize, vdc: usize) {
    for r in 0..h {
        let hb = (2 + r + hdr) * ts + 2 + hdc;
        let vb = (2 + r + vdr) * ts + 2 + vdc;
        for c in 0..w {
            let hp = hb + c;
            let fh = t[hp - 2] as i32 - 5 * t[hp - 1] as i32 + 20 * t[hp] as i32
                + 20 * t[hp + 1] as i32
                - 5 * t[hp + 2] as i32
                + t[hp + 3] as i32;
            let hh = ((fh + 16) >> 5).clamp(0, 255);
            let vp = vb + c;
            let fv = t[vp - 2 * ts] as i32 - 5 * t[vp - ts] as i32 + 20 * t[vp] as i32
                + 20 * t[vp + ts] as i32
                - 5 * t[vp + 2 * ts] as i32
                + t[vp + 3 * ts] as i32;
            let vv = ((fv + 16) >> 5).clamp(0, 255);
            dst[r * w + c] = ((hh + vv + 1) >> 1) as u8;
        }
    }
}

/// Scalar oracle for `mc_centre_hq` (also the non-x86 path).
fn centre_hq_scalar(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, fdr: usize) {
    // Stack scratch, not Vec: this fn is the live path on ISAs without a
    // SIMD arm (future aarch64), where a heap alloc per MC call would be a
    // performance trap. 16x16 worst case fits.
    let mut j = [0u8; 256];
    let j = &mut j[..w * h];
    centre_scalar(t, ts, j, w, h);
    for r in 0..h {
        let base = (2 + r + fdr) * ts + 2;
        for c in 0..w {
            let p = base + c;
            let f = t[p - 2] as i32 - 5 * t[p - 1] as i32 + 20 * t[p] as i32 + 20 * t[p + 1] as i32
                - 5 * t[p + 2] as i32
                + t[p + 3] as i32;
            let b = (f + 16) >> 5;
            let b = b.clamp(0, 255);
            dst[r * w + c] = ((j[r * w + c] as i32 + b + 1) >> 1) as u8;
        }
    }
}

/// Scalar oracle for `mc_centre_vq` (also the non-x86 path).
fn centre_vq_scalar(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, fdc: usize) {
    let mut j = [0u8; 256];
    let j = &mut j[..w * h];
    centre_scalar(t, ts, j, w, h);
    for r in 0..h {
        let base = (2 + r) * ts + 2 + fdc;
        for c in 0..w {
            let p = base + c;
            let f = t[p - 2 * ts] as i32 - 5 * t[p - ts] as i32 + 20 * t[p] as i32
                + 20 * t[p + ts] as i32
                - 5 * t[p + 2 * ts] as i32
                + t[p + 3 * ts] as i32;
            let v = ((f + 16) >> 5).clamp(0, 255);
            dst[r * w + c] = ((j[r * w + c] as i32 + v + 1) >> 1) as u8;
        }
    }
}

/// `(a + b + 1) >> 1` of two planes — the quarter-pel average. `w` in {4, 8, 16}.
pub fn pixel_avg(
    dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
) {
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::PIXEL_AVG.scalar();
        return pixel_avg_scalar(dst, a, a_stride, b, b_stride, w, h);
    }
    debug_assert!(w == 4 || w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(a.len() >= (h - 1) * a_stride + w && b.len() >= (h - 1) * b_stride + w);
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: lengths asserted for both sources at their strides and the dst.
        unsafe { x86::pixel_avg(dst, a, a_stride, b, b_stride, w, h) };
        crate::census::PIXEL_AVG.base();
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if crate::have_neon() /* VENDOR(uefi-player): no_std => baseline-true */ {
        // SAFETY: as above.
        unsafe { arm::pixel_avg(dst, a, a_stride, b, b_stride, w, h) };
        crate::census::PIXEL_AVG.base();
        return;
    }
    #[allow(unreachable_code)] // reachable only on ISAs without a SIMD arm above
    crate::census::PIXEL_AVG.scalar();
    pixel_avg_scalar(dst, a, a_stride, b, b_stride, w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(buf: &mut [u8], mut seed: u32) {
        for b in buf.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (seed >> 24) as u8;
        }
    }

    #[test]
    fn hor20_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let off = 2 * ts + 2;
                    let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                    fill(&mut src, 0x1234 + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_hor20(&src, off, ts, &mut a, w, h);
                    hor20_scalar(&src, off, ts, &mut b, w, h);
                    assert_eq!(a, b, "hor20 w={w} h={h} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn ver02_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let off = 2 * ts + 2;
                    let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                    fill(&mut src, 0xABCD + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_ver02(&src, off, ts, &mut a, w, h);
                    ver02_scalar(&src, off, ts, &mut b, w, h);
                    assert_eq!(a, b, "ver02 w={w} h={h} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn centre_hq_vq_match_scalar_oracles() {
        // The fused centre-adjacent kernels vs the compose-form scalar oracles
        // (centre + independently-computed half + avg), every flavour/shift.
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for fshift in 0..2usize {
                    for seed in 0..6u32 {
                        let ts = w + 16;
                        let mut src = vec![0u8; (h + 8) * ts + w + 16];
                        fill(&mut src, 0x77 + seed * 31 + fshift as u32);
                        let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                        mc_centre_hq(&src, ts, &mut a, w, h, fshift);
                        centre_hq_scalar(&src, ts, &mut b, w, h, fshift);
                        assert_eq!(a, b, "centre_hq w={w} h={h} fdr={fshift} seed={seed}");
                        mc_centre_vq(&src, ts, &mut a, w, h, fshift);
                        centre_vq_scalar(&src, ts, &mut b, w, h, fshift);
                        assert_eq!(a, b, "centre_vq w={w} h={h} fdc={fshift} seed={seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn hv_qpel_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for shifts in 0..4usize {
                    let (hdr, vdc) = (shifts & 1, (shifts >> 1) & 1);
                    for seed in 0..4u32 {
                        let ts = w + 16;
                        let mut src = vec![0u8; (h + 8) * ts + w + 16];
                        fill(&mut src, 0x51 + seed * 17 + shifts as u32);
                        let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                        mc_hv_qpel(&src, ts, &mut a, w, h, hdr, 0, 0, vdc);
                        hv_qpel_scalar(&src, ts, &mut b, w, h, hdr, 0, 0, vdc);
                        assert_eq!(a, b, "hv_qpel w={w} h={h} hdr={hdr} vdc={vdc} seed={seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn centre_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let mut t = vec![0u8; (h + 6) * ts + w + 8];
                    fill(&mut t, 0x5EED + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_centre(&t, ts, &mut a, w, h);
                    centre_scalar(&t, ts, &mut b, w, h);
                    assert_eq!(a, b, "centre w={w} h={h} seed={seed}");
                }
            }
        }
    }

    /// The centre filter is the one that can overflow i16 and the one whose clip
    /// actually bites, so hammer it at the extremes the random fill never reaches.
    #[test]
    fn centre_saturates_like_scalar() {
        for &w in &[8usize, 16] {
            let (h, ts) = (8usize, w + 16);
            for pattern in 0..3 {
                let mut t = vec![0u8; (h + 6) * ts + w + 8];
                for (i, v) in t.iter_mut().enumerate() {
                    *v = match pattern {
                        0 => 255,
                        1 => if (i / ts) % 2 == 0 { 255 } else { 0 },
                        _ => if (i % ts) % 2 == 0 { 255 } else { 0 },
                    };
                }
                let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                mc_centre(&t, ts, &mut a, w, h);
                centre_scalar(&t, ts, &mut b, w, h);
                assert_eq!(a, b, "centre saturation w={w} pattern={pattern}");
            }
        }
    }

    #[test]
    fn hor_qpel_matches_compose() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for fdc in 0..=1usize {
                    for seed in 0..6u32 {
                        let ts = w + 16;
                        let off = 2 * ts + 2;
                        let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                        fill(&mut src, 0x917E_0000 + seed + fdc as u32);
                        let (mut fused, mut half, mut compose) =
                            (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
                        mc_hor_qpel(&src, off, ts, &mut fused, w, h, fdc);
                        mc_hor20(&src, off, ts, &mut half, w, h);
                        pixel_avg(&mut compose, &half, w, &src[off + fdc..], ts, w, h);
                        assert_eq!(fused, compose, "hor_qpel w={w} h={h} fdc={fdc} seed={seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn ver_qpel_matches_compose() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for fdr in 0..=1usize {
                    for seed in 0..6u32 {
                        let ts = w + 16;
                        let off = 2 * ts + 2;
                        let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                        fill(&mut src, 0x79E1_0000 + seed + fdr as u32);
                        let (mut fused, mut half, mut compose) =
                            (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
                        mc_ver_qpel(&src, off, ts, &mut fused, w, h, fdr);
                        mc_ver02(&src, off, ts, &mut half, w, h);
                        pixel_avg(&mut compose, &half, w, &src[off + fdr * ts..], ts, w, h);
                        assert_eq!(fused, compose, "ver_qpel w={w} h={h} fdr={fdr} seed={seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn pixel_avg_matches_scalar() {
        for &w in &[4usize, 8, 16] {
            for &h in &[2usize, 4, 8, 16] {
                let (sa, sb) = (w + 7, w + 3);
                let mut a = vec![0u8; h * sa + 16];
                let mut b = vec![0u8; h * sb + 16];
                fill(&mut a, 0x11);
                fill(&mut b, 0x22);
                let (mut x, mut y) = (vec![0u8; w * h], vec![0u8; w * h]);
                pixel_avg(&mut x, &a, sa, &b, sb, w, h);
                pixel_avg_scalar(&mut y, &a, sa, &b, sb, w, h);
                assert_eq!(x, y, "pixel_avg w={w} h={h}");
            }
        }
    }
}
