//! Fused single-pass half-pel plane builder (x264 `hpel_filter` shape) —
//! AVX2 on x86-64, NEON on aarch64 (X1 port, inline-execution.md 11.15).
//!
//! One pass over the padded full-pel plane produces the H, V and C half-pel rows
//! together, sharing the vertical 6-tap intermediates. The scalar fused builder
//! in `rusty_h264-common` (its oracle) proved the shape byte-identical to the
//! deployed tile walk but lost on throughput because the tiles run asm kernels;
//! this kernel is the SIMD twin the fused shape was retained for.
//!
//! Arithmetic notes (exactness, not approximation):
//! - The vertical 6-tap of u8 taps lies in `[-2550, 10710]`, so it is computed
//!   EXACTLY in i16 as `(a+f) + 5*(4*(c+d) - (b+e))` where every intermediate
//!   stays inside i16 (pairwise u8 sums <= 510; `4*(c+d) - (b+e)` in
//!   `[-510, 2040]`; `*5` <= 10200).
//! - The centre plane's horizontal 6-tap over those i16 intermediates can reach
//!   ~450k, so it widens to i32 (pairwise i16 sums <= 21420 stay exact in i16,
//!   then `20*(c+d) - 5*(b+e) + (a+f)` runs in i32).
//! - `packus` saturation to `0..=255` is exactly the scalar `clip_u8`; NEON's
//!   `vqrshrun_n_s16::<5>` is exactly `(s+16)>>5` then saturate (the same
//!   proven idiom as `luma_mc`'s NEON arms), and `vqrshrn_n_s32::<10>` is
//!   exactly `(s+512)>>10` (the shifted value fits i16: |s|>>10 <= ~440).

/// Cached AVX2 detection (one atomic load after first use).
// VENDOR(uefi-player): alloc prelude for the no_std build (scratch `vec!` below).
#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(target_arch = "x86_64")]
#[inline]
fn has_avx2() -> bool {
    // VENDOR(uefi-player): std keeps the upstream cached probe; no_std assumes
    // max (VENDOR.md) — the firmware pushes the single-bit gate, no second probe.
    #[cfg(feature = "std")]
    {
        use std::sync::OnceLock;
        static C: OnceLock<bool> = OnceLock::new();
        *C.get_or_init(|| std::is_x86_feature_detected!("avx2"))
    }
    #[cfg(not(feature = "std"))]
    {
        true
    }
}

/// Builds the three half-pel planes from the edge-padded full-pel plane `f`
/// (`pw`×`ph`, row stride `pw`). Returns `false` (no work done) when no SIMD
/// arm applies (x86-64 without AVX2, other ISAs, or a plane too narrow for the
/// vector interior) — the caller falls back to its scalar/tile path.
///
/// Values are byte-identical to the scalar fused builder (and transitively to
/// the tile walk): same 6-tap integer formulas over the same clamped taps. The
/// aarch64 arm is cfg-only dispatch (NEON is the baseline there — the
/// campaign's convention), pinned by the caller-side A/B (`RS_H264_QPEL`-class
/// oracle at `inter.rs`' fused-hpel test) on first ARM execution.
pub fn hpel_fused(f: &[u8], pw: usize, ph: usize, h: &mut [u8], v: &mut [u8], c: &mut [u8]) -> bool {
    // VENDOR(uefi-player): firmware single-bit gate — off means the caller's
    // scalar fused builder / tile walk (`common` keeps both either way).
    if !crate::simd_enabled() {
        crate::census::HPEL_FUSED.scalar();
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if !has_avx2() || pw < 24 || ph == 0 {
            crate::census::HPEL_FUSED.scalar();
            return false;
        }
        assert!(f.len() >= pw * ph && h.len() >= pw * ph && v.len() >= pw * ph && c.len() >= pw * ph);
        // SAFETY: bounds asserted above; the kernel reads/writes inside `pw*ph` rows
        // plus a `pw+5+16` scratch it allocates itself. Feature-gated + detected.
        unsafe { hpel_fused_avx2(f, pw, ph, h, v, c) }
        crate::census::HPEL_FUSED.wide();
        return true;
    }
    #[cfg(target_arch = "aarch64")]
    {
        if pw < 16 || ph == 0 {
            crate::census::HPEL_FUSED.scalar();
            return false;
        }
        assert!(f.len() >= pw * ph && h.len() >= pw * ph && v.len() >= pw * ph && c.len() >= pw * ph);
        // SAFETY: bounds asserted above; NEON is the aarch64 baseline.
        unsafe { hpel_fused_neon(f, pw, ph, h, v, c) }
        crate::census::HPEL_FUSED.base();
        return true;
    }
    #[allow(unreachable_code)]
    {
        let _ = (f, pw, ph, h, v, c);
        false
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hpel_fused_avx2(f: &[u8], pw: usize, ph: usize, h: &mut [u8], v: &mut [u8], c: &mut [u8]) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let cl = |i: isize, hi: usize| i.clamp(0, hi as isize - 1) as usize;
    // Vertical 6-tap intermediates for this row: vt[j] covers column j-2 (clamped),
    // j in 0..pw+5. i16, exact (see module notes). +16 slack so the last vector
    // load of the centre pass never reads past the buffer.
    let mut vt = vec![0i16; pw + 5 + 16];
    let five = _mm256_set1_epi16(5);
    let rnd16 = _mm256_set1_epi16(16);
    let rnd512 = _mm256_set1_epi32(512);

    for y in 0..ph {
        let (ym2, ym1, y0, yp1, yp2, yp3) = (
            cl(y as isize - 2, ph) * pw,
            cl(y as isize - 1, ph) * pw,
            y * pw,
            cl(y as isize + 1, ph) * pw,
            cl(y as isize + 2, ph) * pw,
            cl(y as isize + 3, ph) * pw,
        );
        // -- vertical pass: vt[j] = 6-tap over rows at column (j-2 clamped) --
        let vt_scalar = |f: &[u8], x: usize| -> i16 {
            (f[ym2 + x] as i32 - 5 * f[ym1 + x] as i32 + 20 * f[y0 + x] as i32
                + 20 * f[yp1 + x] as i32
                - 5 * f[yp2 + x] as i32
                + f[yp3 + x] as i32) as i16
        };
        for j in 0..2 {
            vt[j] = vt_scalar(f, 0);
        }
        // Interior: j in 2..pw+2 maps to direct column x = j-2 in 0..pw.
        let mut x = 0usize;
        while x + 16 <= pw {
            // 16 u8 -> 16 i16 lanes per row tap.
            let ld = |base: usize| {
                _mm256_cvtepu8_epi16(_mm_loadu_si128(f.as_ptr().add(base + x) as *const __m128i))
            };
            let (a, b, cc, d, e, g) = (ld(ym2), ld(ym1), ld(y0), ld(yp1), ld(yp2), ld(yp3));
            let af = _mm256_add_epi16(a, g);
            let be = _mm256_add_epi16(b, e);
            let cd = _mm256_add_epi16(cc, d);
            // (a+f) + 5*(4*(c+d) - (b+e)) == a - 5b + 20c + 20d - 5e + f, all i16-exact.
            let s = _mm256_add_epi16(
                af,
                _mm256_mullo_epi16(five, _mm256_sub_epi16(_mm256_slli_epi16(cd, 2), be)),
            );
            _mm256_storeu_si256(vt.as_mut_ptr().add(2 + x) as *mut __m256i, s);
            x += 16;
        }
        while x < pw {
            vt[2 + x] = vt_scalar(f, x);
            x += 1;
        }
        for j in pw + 2..pw + 5 {
            vt[j] = vt_scalar(f, pw - 1);
        }

        // -- V plane: clip((vt[x+2] + 16) >> 5) --
        let mut x = 0usize;
        while x + 16 <= pw {
            let s = _mm256_loadu_si256(vt.as_ptr().add(2 + x) as *const __m256i);
            let r = _mm256_srai_epi16(_mm256_add_epi16(s, rnd16), 5);
            let p = _mm256_packus_epi16(r, r); // saturate i16 -> u8 == clip_u8
            let p = _mm256_permute4x64_epi64(p, 0b11011000);
            _mm_storeu_si128(
                v.as_mut_ptr().add(y0 + x) as *mut __m128i,
                _mm256_castsi256_si128(p),
            );
            x += 16;
        }
        while x < pw {
            let s = vt[2 + x] as i32;
            v[y0 + x] = ((s + 16) >> 5).clamp(0, 255) as u8;
            x += 1;
        }

        // -- H plane: horizontal 6-tap of the source row (column-clamped halo) --
        let h_scalar = |f: &[u8], x: usize| -> u8 {
            let t = |k: isize| f[y0 + cl(x as isize + k, pw)] as i32;
            let s = t(-2) - 5 * t(-1) + 20 * t(0) + 20 * t(1) - 5 * t(2) + t(3);
            ((s + 16) >> 5).clamp(0, 255) as u8
        };
        for x in 0..2.min(pw) {
            h[y0 + x] = h_scalar(f, x);
        }
        let mut x = 2usize;
        // Direct taps need x-2 >= 0 and x+15+3 <= pw-1.
        while x + 16 + 3 <= pw {
            let ld = |off: isize| {
                _mm256_cvtepu8_epi16(_mm_loadu_si128(
                    f.as_ptr().add((y0 + x) as usize).offset(off) as *const __m128i
                ))
            };
            let (a, b, cc, d, e, g) = (ld(-2), ld(-1), ld(0), ld(1), ld(2), ld(3));
            let af = _mm256_add_epi16(a, g);
            let be = _mm256_add_epi16(b, e);
            let cd = _mm256_add_epi16(cc, d);
            let s = _mm256_add_epi16(
                af,
                _mm256_mullo_epi16(five, _mm256_sub_epi16(_mm256_slli_epi16(cd, 2), be)),
            );
            let r = _mm256_srai_epi16(_mm256_add_epi16(s, rnd16), 5);
            let p = _mm256_packus_epi16(r, r);
            let p = _mm256_permute4x64_epi64(p, 0b11011000);
            _mm_storeu_si128(
                h.as_mut_ptr().add(y0 + x) as *mut __m128i,
                _mm256_castsi256_si128(p),
            );
            x += 16;
        }
        while x < pw {
            h[y0 + x] = h_scalar(f, x);
            x += 1;
        }

        // -- C plane: horizontal 6-tap over vt, i32 accumulation --
        // c[x] uses vt[x..x+6]; vt has pw+5 valid entries (+16 slack for the loads).
        let mut x = 0usize;
        while x + 16 <= pw {
            let ld = |off: usize| _mm256_loadu_si256(vt.as_ptr().add(x + off) as *const __m256i);
            let (a, b, cc, d, e, g) = (ld(0), ld(1), ld(2), ld(3), ld(4), ld(5));
            let af = _mm256_add_epi16(a, g); // <= 21420, i16-exact
            let be = _mm256_add_epi16(b, e);
            let cd = _mm256_add_epi16(cc, d);
            // Widen to i32: s = 20*(c+d) - 5*(b+e) + (a+f), then (s+512)>>10.
            let w20 = _mm256_set1_epi32(20);
            let w5 = _mm256_set1_epi32(5);
            let lo = |t: __m256i| _mm256_cvtepi16_epi32(_mm256_castsi256_si128(t));
            let hi = |t: __m256i| _mm256_cvtepi16_epi32(_mm256_extracti128_si256(t, 1));
            let sum = |af: __m256i, be: __m256i, cd: __m256i| {
                let s = _mm256_add_epi32(
                    _mm256_sub_epi32(_mm256_mullo_epi32(cd, w20), _mm256_mullo_epi32(be, w5)),
                    af,
                );
                _mm256_srai_epi32(_mm256_add_epi32(s, rnd512), 10)
            };
            let rlo = sum(lo(af), lo(be), lo(cd));
            let rhi = sum(hi(af), hi(be), hi(cd));
            // packs i32->i16 within 128-lanes then reorder; packus i16->u8 clips.
            let r16 = _mm256_permute4x64_epi64(_mm256_packs_epi32(rlo, rhi), 0b11011000);
            let p = _mm256_packus_epi16(r16, r16);
            let p = _mm256_permute4x64_epi64(p, 0b11011000);
            _mm_storeu_si128(
                c.as_mut_ptr().add(y0 + x) as *mut __m128i,
                _mm256_castsi256_si128(p),
            );
            x += 16;
        }
        while x < pw {
            let s = vt[x] as i32 - 5 * vt[x + 1] as i32 + 20 * vt[x + 2] as i32
                + 20 * vt[x + 3] as i32
                - 5 * vt[x + 4] as i32
                + vt[x + 5] as i32;
            c[y0 + x] = ((s + 512) >> 10).clamp(0, 255) as u8;
            x += 1;
        }
    }
}

/// NEON twin: 8 i16 lanes per step (the AVX2 arm does 16). Same formulas, same
/// scalar tails, same `vt` layout — only the lane width and the
/// round-shift-saturate primitives differ, and both of those are the proven
/// idioms from `luma_mc`'s NEON arms.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn hpel_fused_neon(f: &[u8], pw: usize, ph: usize, h: &mut [u8], v: &mut [u8], c: &mut [u8]) {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let cl = |i: isize, hi: usize| i.clamp(0, hi as isize - 1) as usize;
    let mut vt = vec![0i16; pw + 5 + 8];

    for y in 0..ph {
        let (ym2, ym1, y0, yp1, yp2, yp3) = (
            cl(y as isize - 2, ph) * pw,
            cl(y as isize - 1, ph) * pw,
            y * pw,
            cl(y as isize + 1, ph) * pw,
            cl(y as isize + 2, ph) * pw,
            cl(y as isize + 3, ph) * pw,
        );
        let vt_scalar = |f: &[u8], x: usize| -> i16 {
            (f[ym2 + x] as i32 - 5 * f[ym1 + x] as i32 + 20 * f[y0 + x] as i32
                + 20 * f[yp1 + x] as i32
                - 5 * f[yp2 + x] as i32
                + f[yp3 + x] as i32) as i16
        };
        for j in 0..2 {
            vt[j] = vt_scalar(f, 0);
        }
        let mut x = 0usize;
        while x + 8 <= pw {
            let ld = |base: usize| {
                vreinterpretq_s16_u16(vmovl_u8(vld1_u8(f.as_ptr().add(base + x))))
            };
            let (a, b, cc, d, e, g) = (ld(ym2), ld(ym1), ld(y0), ld(yp1), ld(yp2), ld(yp3));
            let af = vaddq_s16(a, g);
            let be = vaddq_s16(b, e);
            let cd = vaddq_s16(cc, d);
            // (a+f) + 5*(4*(c+d) - (b+e)), all i16-exact.
            let s = vmlaq_n_s16(af, vsubq_s16(vshlq_n_s16(cd, 2), be), 5);
            vst1q_s16(vt.as_mut_ptr().add(2 + x), s);
            x += 8;
        }
        while x < pw {
            vt[2 + x] = vt_scalar(f, x);
            x += 1;
        }
        for j in pw + 2..pw + 5 {
            vt[j] = vt_scalar(f, pw - 1);
        }

        // -- V plane: clip((vt[x+2] + 16) >> 5) == vqrshrun_n_s16::<5> --
        let mut x = 0usize;
        while x + 8 <= pw {
            let s = vld1q_s16(vt.as_ptr().add(2 + x));
            vst1_u8(v.as_mut_ptr().add(y0 + x), vqrshrun_n_s16::<5>(s));
            x += 8;
        }
        while x < pw {
            let s = vt[2 + x] as i32;
            v[y0 + x] = ((s + 16) >> 5).clamp(0, 255) as u8;
            x += 1;
        }

        // -- H plane --
        let h_scalar = |f: &[u8], x: usize| -> u8 {
            let t = |k: isize| f[y0 + cl(x as isize + k, pw)] as i32;
            let s = t(-2) - 5 * t(-1) + 20 * t(0) + 20 * t(1) - 5 * t(2) + t(3);
            ((s + 16) >> 5).clamp(0, 255) as u8
        };
        for x in 0..2.min(pw) {
            h[y0 + x] = h_scalar(f, x);
        }
        let mut x = 2usize;
        while x + 8 + 3 <= pw {
            let ld = |off: isize| {
                vreinterpretq_s16_u16(vmovl_u8(vld1_u8(
                    f.as_ptr().add(y0 + x).offset(off),
                )))
            };
            let (a, b, cc, d, e, g) = (ld(-2), ld(-1), ld(0), ld(1), ld(2), ld(3));
            let af = vaddq_s16(a, g);
            let be = vaddq_s16(b, e);
            let cd = vaddq_s16(cc, d);
            let s = vmlaq_n_s16(af, vsubq_s16(vshlq_n_s16(cd, 2), be), 5);
            vst1_u8(h.as_mut_ptr().add(y0 + x), vqrshrun_n_s16::<5>(s));
            x += 8;
        }
        while x < pw {
            h[y0 + x] = h_scalar(f, x);
            x += 1;
        }

        // -- C plane: widen to i32, s = 20*(c+d) - 5*(b+e) + (a+f), (s+512)>>10 --
        let mut x = 0usize;
        while x + 8 <= pw {
            let ld = |off: usize| vld1q_s16(vt.as_ptr().add(x + off));
            let (a, b, cc, d, e, g) = (ld(0), ld(1), ld(2), ld(3), ld(4), ld(5));
            let af = vaddq_s16(a, g); // <= 21420, i16-exact
            let be = vaddq_s16(b, e);
            let cd = vaddq_s16(cc, d);
            let half = |af: int16x4_t, be: int16x4_t, cd: int16x4_t| -> int16x4_t {
                let s = vaddw_s16(
                    vmlsl_n_s16(vmull_n_s16(cd, 20), be, 5),
                    af,
                );
                vqrshrn_n_s32::<10>(s)
            };
            let rlo = half(vget_low_s16(af), vget_low_s16(be), vget_low_s16(cd));
            let rhi = half(vget_high_s16(af), vget_high_s16(be), vget_high_s16(cd));
            let r16 = vcombine_s16(rlo, rhi);
            vst1_u8(c.as_mut_ptr().add(y0 + x), vqmovun_s16(r16));
            x += 8;
        }
        while x < pw {
            let s = vt[x] as i32 - 5 * vt[x + 1] as i32 + 20 * vt[x + 2] as i32
                + 20 * vt[x + 3] as i32
                - 5 * vt[x + 4] as i32
                + vt[x + 5] as i32;
            c[y0 + x] = ((s + 512) >> 10).clamp(0, 255) as u8;
            x += 1;
        }
    }
}
