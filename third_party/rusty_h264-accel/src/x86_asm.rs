//! x86-64 Rust-intrinsics kernels (historical name: this module once wrapped
//! openh264's vendored NASM — **the assembly is fully ripped as of 2026-08-12**;
//! every kernel here and in the sibling modules is pure Rust). The openh264
//! HERITAGE remains in the algorithms and tables (BSD-2; attribution in
//! `LICENSE.openh264` at the crate root).
//!
//! This crate is deliberately **not** `#![forbid(unsafe_code)]`: `unsafe` here
//! is `core::arch` intrinsics behind safe wrappers; the codec core stays
//! `forbid(unsafe)`. Gated on `target_arch = "x86_64"`; other architectures use
//! the portable/scalar paths in the sibling modules.
#![allow(non_snake_case)]

#[path = "mectx.rs"]
mod mectx;
#[path = "satd_avg.rs"]
pub(crate) mod satd_avg;
pub use mectx::MeCtx;
pub use satd_avg::{sad_x4, satd_avg, satd_avg_x4, satd_x4, satd_x4p};


/// Whether the running CPU supports AVX2 (cached). Gates the AVX2 MC kernels —
/// calling a VEX-encoded kernel on a non-AVX2 CPU would fault.
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


















#[cfg(test)]
mod tests {
    use super::*;
    // chroma MC moved to the portable `chroma_mc` module (rip-ASM Phase 1); these
    // pre-existing tests pinned the openh264 asm against scalar and now pin the
    // SSE2/NEON replacement against the same oracle.
    use crate::{mc_chroma_w4, mc_chroma_w8};
    // SATD/SAD moved to the portable `satd_sad` module (rip-ASM Phase 5a); these
    // pre-existing openh264-reference tests now pin the Rust kernels to the same
    // C reference the assembly was held to.
    use crate::{sad_16x16, sad_16x8, sad_8x16, satd_16x16, satd_16x8, satd_4x4, satd_8x16, satd_8x8};

    /// Port of openh264 `WelsSampleSatd4x4_c` — the exact reference the asm matches.
    fn satd_ref(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
        let mut m = [[0i32; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                m[i][j] = a[i * sa + j] as i32 - b[i * sb + j] as i32;
            }
        }
        for row in m.iter_mut() {
            let (s0, s1, s2, s3) =
                (row[0] + row[2], row[1] + row[3], row[0] - row[2], row[1] - row[3]);
            *row = [s0 + s1, s2 + s3, s2 - s3, s0 - s1];
        }
        let mut sum = 0i32;
        for j in 0..4 {
            let (s0, s1, s2, s3) =
                (m[0][j] + m[2][j], m[1][j] + m[3][j], m[0][j] - m[2][j], m[1][j] - m[3][j]);
            let (c0, c1, c2, c3) = (s0 + s1, s2 + s3, s2 - s3, s0 - s1);
            sum += c0.abs() + c1.abs() + c2.abs() + c3.abs();
        }
        (sum + 1) >> 1
    }

    #[test]
    fn mc_chroma_w8_matches_scalar() {
        // Deterministic 9×9 clamped tile; test every eighth-pel (fx,fy) phase.
        let mut tile = [0u8; 9 * 9];
        let mut s = 0x12345u32;
        for v in tile.iter_mut() {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            *v = (s >> 16) as u8;
        }
        for fy in 0..8i32 {
            for fx in 0..8i32 {
                let (wa, wb, wc, wd) =
                    ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
                let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
                let mut got = [0u8; 64];
                mc_chroma_w8(&tile, 9, &mut got, 8, &abcd, 8);
                let mut want = [0u8; 64];
                for r in 0..8 {
                    for c in 0..8 {
                        let p = r * 9 + c;
                        let v = wa * tile[p] as i32
                            + wb * tile[p + 1] as i32
                            + wc * tile[p + 9] as i32
                            + wd * tile[p + 9 + 1] as i32;
                        want[r * 8 + c] = ((v + 32) >> 6) as u8;
                    }
                }
                assert_eq!(got, want, "fx={fx} fy={fy}");
            }
        }
    }

    /// H-38 oracle for the newly-wired 4-wide chroma kernel: every eighth-pel phase
    /// must match the scalar bilinear exactly, at both block heights the decoder
    /// asks for (4-wide blocks are 4 or 2 tall after rect coalescing).
    #[test]
    fn mc_chroma_w4_matches_scalar() {
        let mut tile = [0u8; 5 * 9];
        let mut s = 0xbeef1u32;
        for v in tile.iter_mut() {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            *v = (s >> 16) as u8;
        }
        for h in [2usize, 4, 8] {
            for fy in 0..8i32 {
                for fx in 0..8i32 {
                    let (wa, wb, wc, wd) =
                        ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
                    let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
                    let mut got = [0u8; 32];
                    mc_chroma_w4(&tile, 5, &mut got[..h * 4], 4, &abcd, h);
                    let mut want = [0u8; 32];
                    for r in 0..h {
                        for c in 0..4 {
                            let p = r * 5 + c;
                            let v = wa * tile[p] as i32
                                + wb * tile[p + 1] as i32
                                + wc * tile[p + 5] as i32
                                + wd * tile[p + 5 + 1] as i32;
                            want[r * 4 + c] = ((v + 32) >> 6) as u8;
                        }
                    }
                    assert_eq!(got[..h * 4], want[..h * 4], "h={h} fx={fx} fy={fy}");
                }
            }
        }
    }

    /// Our scalar `forward_core` butterfly (spec / openh264 `WelsDctT4_c`), on a
    /// 4×4 residual block in raster order.
    fn forward_core(b: &[i32; 16]) -> [i32; 16] {
        let f = |x0: i32, x1: i32, x2: i32, x3: i32| {
            let (t0, t1, t2, t3) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
            (t0 + t1, 2 * t3 + t2, t0 - t1, t3 - 2 * t2)
        };
        let mut m = *b;
        for r in 0..4 {
            let (a, c, d, e) = f(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
            m[r * 4] = a;
            m[r * 4 + 1] = c;
            m[r * 4 + 2] = d;
            m[r * 4 + 3] = e;
        }
        for c in 0..4 {
            let (a, b2, d, e) = f(m[c], m[4 + c], m[8 + c], m[12 + c]);
            m[c] = a;
            m[4 + c] = b2;
            m[8 + c] = d;
            m[12 + c] = e;
        }
        m
    }

    #[test]
    fn dct_four_t4_matches_forward_core() {
        // 8×8 source + prediction tiles (stride 8 for the test).
        for seed in 0..128usize {
            let mut src = [0u8; 64];
            let mut pred = [0u8; 64];
            for y in 0..8 {
                for x in 0..8 {
                    src[y * 8 + x] = ((y * 31 + x * 17 + seed * 7) & 0xff) as u8;
                    pred[y * 8 + x] = ((y * 13 + x * 41 + seed * 5 + 9) & 0xff) as u8;
                }
            }
            let mut dct = [0i16; 64];
            crate::dct_four_t4(&mut dct, &src, 8, &pred, 8);
            // Reference: the four 4×4 sub-blocks at (bx,by) px-units (0,0),(4,0),(0,4),(4,4).
            for (k, (ox, oy)) in [(0, 0), (4, 0), (0, 4), (4, 4)].iter().enumerate() {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        res[dy * 4 + dx] =
                            src[(oy + dy) * 8 + ox + dx] as i32 - pred[(oy + dy) * 8 + ox + dx] as i32;
                    }
                }
                let want = forward_core(&res);
                for i in 0..16 {
                    assert_eq!(
                        dct[k * 16 + i] as i32,
                        want[i],
                        "seed {seed} block {k} coeff {i}"
                    );
                }
            }
        }
    }

    /// Port of openh264 `WelsIDctT4Rec_c` (== our `inverse_core` + add pred + clip),
    /// on one 4×4 block of dequantized coeffs.
    fn idct_rec_block(pred: &[i32; 16], dct: &[i32; 16]) -> [u8; 16] {
        let mut t = [0i32; 16];
        for i in 0..4 {
            let d = &dct[i * 4..i * 4 + 4];
            let (su, de) = (d[0] + d[2], d[0] - d[2]);
            let (sd, dd) = (d[1] + (d[3] >> 1), (d[1] >> 1) - d[3]);
            t[i * 4] = su + sd;
            t[i * 4 + 1] = de + dd;
            t[i * 4 + 2] = de - dd;
            t[i * 4 + 3] = su - sd;
        }
        let mut out = [0u8; 16];
        for i in 0..4 {
            let (sl, dl) = (t[i] + t[8 + i], t[i] - t[8 + i]);
            let (dr, sr) = ((t[4 + i] >> 1) - t[12 + i], t[4 + i] + (t[12 + i] >> 1));
            let r = [sl + sr, dl + dr, dl - dr, sl - sr];
            for k in 0..4 {
                out[k * 4 + i] = (pred[k * 4 + i] + ((r[k] + 32) >> 6)).clamp(0, 255) as u8;
            }
        }
        out
    }

    #[repr(align(16))]
    struct Align16<T>(T);

    #[test]
    fn idct_four_t4_rec_matches_scalar() {
        for seed in 0..128usize {
            let mut pred = [0u8; 64];
            // dct coeffs must be 16-byte aligned (the kernel uses movdqa loads).
            let mut dctw = Align16([0i16; 64]);
            for i in 0..64 {
                pred[i] = ((i * 7 + seed * 3) & 0xff) as u8;
                // dequantized-coeff-like values (signed, modest magnitude)
                dctw.0[i] = (((i as i32 * 53 + seed as i32 * 29) % 4096) - 2048) as i16;
            }
            let dct = &dctw.0;
            let mut rec = [0u8; 64];
            crate::idct_four_t4_rec(&mut rec, 8, &pred, 8, dct);
            // Reference: 4 sub-blocks at (0,0),(4,0),(0,4),(4,4).
            for (k, (ox, oy)) in [(0, 0), (4, 0), (0, 4), (4, 4)].iter().enumerate() {
                let mut pb = [0i32; 16];
                let mut db = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        pb[dy * 4 + dx] = pred[(oy + dy) * 8 + ox + dx] as i32;
                        db[dy * 4 + dx] = dct[k * 16 + dy * 4 + dx] as i32;
                    }
                }
                let want = idct_rec_block(&pb, &db);
                for dy in 0..4 {
                    for dx in 0..4 {
                        assert_eq!(
                            rec[(oy + dy) * 8 + ox + dx],
                            want[dy * 4 + dx],
                            "seed {seed} block {k} ({dx},{dy})"
                        );
                    }
                }
            }
        }
    }

    fn sad_ref(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
        let mut s = 0i32;
        for i in 0..h {
            for j in 0..w {
                s += (a[i * sa + j] as i32 - b[i * sb + j] as i32).abs();
            }
        }
        s
    }
    // openh264's NxM SATD = sum of the constituent 4×4 SATDs (each `(Σ|H·d|+1)>>1`).
    fn satd_region_ref(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
        let mut s = 0i32;
        let mut by = 0;
        while by < h {
            let mut bx = 0;
            while bx < w {
                s += satd_ref(&a[by * sa + bx..], sa, &b[by * sb + bx..], sb);
                bx += 4;
            }
            by += 4;
        }
        s
    }

    #[test]
    fn sad_satd_family_matches_reference() {
        // 16-byte aligned, stride-16 tiles — the SSE2 SAD/SATD kernels use aligned
        // (movdqa) loads, so input must be 16-aligned with a 16-multiple stride
        // (which the encoder's planes are, at 16-aligned MB offsets).
        let (sa, sb) = (16usize, 16usize);
        let mut aw = Align16([0u8; 16 * 16]);
        let mut bw = Align16([0u8; 16 * 16]);
        for seed in 0..96usize {
            for i in 0..16 {
                for j in 0..16 {
                    aw.0[i * sa + j] = ((i * 37 + j * 101 + seed * 3) & 0xff) as u8;
                    bw.0[i * sb + j] = ((i * 53 + j * 17 + seed * 29 + 7) & 0xff) as u8;
                }
            }
            let (a, b): (&[u8], &[u8]) = (&aw.0, &bw.0);
            assert_eq!(sad_16x16(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 16, 16), "sad16x16 {seed}");
            assert_eq!(sad_16x8(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 16, 8), "sad16x8 {seed}");
            assert_eq!(sad_8x16(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 8, 16), "sad8x16 {seed}");
            assert_eq!(satd_8x8(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 8, 8), "satd8x8 {seed}");
            assert_eq!(satd_16x8(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 16, 8), "satd16x8 {seed}");
            assert_eq!(satd_8x16(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 8, 16), "satd8x16 {seed}");
            assert_eq!(satd_16x16(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 16, 16), "satd16x16 {seed}");
        }
    }

    #[test]
    fn quant_four_matches_openh264_c() {
        // openh264 WELS_NEW_QUANT: level = sign(c) * ((|c| + FF[pos]) * MF[pos]) >> 16,
        // pos = (row&1)*4 + col within each 4x4 block.
        #[repr(align(16))]
        struct A16i([i16; 64]);
        let ff: [i16; 8] = [80, 85, 80, 85, 90, 95, 90, 95];
        let mf: [i16; 8] = [410, 420, 410, 420, 430, 440, 430, 440];
        for seed in 0..64i32 {
            let mut input = [0i16; 64];
            for (k, v) in input.iter_mut().enumerate() {
                *v = (((k as i32 * 37 + seed * 53) % 2000) - 1000) as i16;
            }
            let mut dctw = A16i(input);
            crate::quant_four_4x4(&mut dctw.0, &ff, &mf);
            for blk in 0..4 {
                for row in 0..4 {
                    for col in 0..4 {
                        let idx = blk * 16 + row * 4 + col;
                        let pos = (row & 1) * 4 + col;
                        let c = input[idx] as i32;
                        let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                        let want = (if c < 0 { -lvl } else { lvl }) as i16;
                        assert_eq!(dctw.0[idx], want, "seed {seed} blk {blk} ({row},{col})");
                    }
                }
            }
        }
    }

    #[test]
    fn satd_matches_openh264_c_reference() {
        for seed in 0..256u32 {
            let mut a = [0u8; 64];
            let mut b = [0u8; 64];
            for i in 0..4 {
                for j in 0..4 {
                    let s = seed as usize;
                    a[i * 16 + j] = ((i * 37 + j * 101 + s * 3) & 0xff) as u8;
                    b[i * 16 + j] = ((i * 53 + j * 17 + s * 29 + 7) & 0xff) as u8;
                }
            }
            let got = satd_4x4(&a, 16, &b, 16);
            let want = satd_ref(&a, 16, &b, 16);
            assert_eq!(got, want, "seed {seed}: asm {got} != openh264-C ref {want}");
        }
    }
}

/// AVX2 boundary-strength MOTION MASKS — the vectorised twin of
/// `rusty_h264_common::deblock::bs_motion_masks_scalar`.
///
/// Computes, for one macroblock's 16 blocks laid out in raster order:
///   `left` bit k = block k differs in motion from block k-1
///   `up`   bit k = block k differs in motion from block k-4
/// where "differs" is §8.7.2.1's inter rule:
///   `ref[a] != ref[b] || (ref[a] != NO_REF && (|dmvx| >= 4 || |dmvy| >= 4))`
///
/// This replaces 24 branchy scalar `bs_inter` evaluations — the part the ceiling
/// probe identified as COMPUTE-bound (~370 of the derivation's ~400 ns/MB), as
/// opposed to the gather, which was only ~31 ns/MB.
///
/// LANE-BOUNDARY TRICK, and it is load-bearing: the `left` comparison uses a
/// WITHIN-128-bit-lane byte shift, which corrupts the lanes at each 128-bit boundary
/// — for i16 that is k=0 and k=8, for i32 k=0 and k=4. Every corrupted position has
/// `k % 4 == 0`, and those are exactly the macroblock-edge blocks whose strengths are
/// derived separately against the neighbouring record. So the cheap shift is correct
/// where it matters and garbage only where the result is discarded. The `up`
/// comparison genuinely crosses lanes and uses `permute2x128`.
///
/// Both masks are returned with the don't-care bits FORCED TO ZERO so the scalar and
/// SIMD twins are bit-identical on the whole u16, not merely on the bits consumed.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bs_motion_masks_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    no_ref: i32,
) -> (u16, u16) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    let vx = _mm256_loadu_si256(mvx.as_ptr() as *const __m256i);
    let vy = _mm256_loadu_si256(mvy.as_ptr() as *const __m256i);
    let r0 = _mm256_loadu_si256(ref_id.as_ptr() as *const __m256i);
    let r1 = _mm256_loadu_si256(ref_id.as_ptr().add(8) as *const __m256i);

    let four = _mm256_set1_epi16(4);
    let nr = _mm256_set1_epi32(no_ref);

    // |a - b| >= 4, per i16 lane, as `|a-b| > 3`.
    //
    // NOT the `or(subs(a,b), subs(b,a))` idiom recorded elsewhere in this workspace:
    // that one needs UNSIGNED saturating subtract, where the wrong direction clamps to
    // zero. Motion vectors are SIGNED, so `subs_epi16` leaves the wrong direction
    // negative and the OR yields garbage — the `*_matches_scalar` gate caught exactly
    // that. `abs_epi16(sub_epi16(..))` also matches the scalar twin's overflow
    // behaviour: both wrap in i16, and both map i16::MIN to itself.
    let far = |a: __m256i, b: __m256i| -> __m256i {
        let d = _mm256_abs_epi16(_mm256_sub_epi16(a, b));
        _mm256_cmpgt_epi16(d, _mm256_sub_epi16(four, _mm256_set1_epi16(1)))
    };

    // ---- LEFT (k-1): within-lane shifts; corrupted lanes are all k%4==0. ----
    let vxl = _mm256_bslli_epi128(vx, 2);
    let vyl = _mm256_bslli_epi128(vy, 2);
    let farl = _mm256_or_si256(far(vx, vxl), far(vy, vyl));

    let r0l = _mm256_bslli_epi128(r0, 4);
    let r1l = _mm256_bslli_epi128(r1, 4);
    // ref differs, and the "far" term is gated on ref[a] != NO_REF.
    let neq0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, r0l), _mm256_set1_epi32(-1));
    let neq1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, r1l), _mm256_set1_epi32(-1));
    let live0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, nr), _mm256_set1_epi32(-1));
    let live1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, nr), _mm256_set1_epi32(-1));
    // Narrow the i32 ref predicates to i16 lanes so they combine with `far`.
    // packs_epi32 interleaves the two 128-bit halves, so permute4x64 restores order.
    let pack = |a: __m256i, b: __m256i| {
        _mm256_permute4x64_epi64(_mm256_packs_epi32(a, b), 0b11_01_10_00)
    };
    let left = _mm256_or_si256(
        pack(neq0, neq1),
        _mm256_and_si256(pack(live0, live1), farl),
    );

    // ---- UP (k-4): genuinely crosses the 128-bit boundary. ----
    // i16: shift the whole 256-bit register right by 8 bytes (4 blocks).
    let shift4_i16 = |v: __m256i| {
        let lo = _mm256_permute2x128_si256(v, v, 0x08); // [0, low_lane]
        _mm256_alignr_epi8(v, lo, 8)
    };
    let vxu = shift4_i16(vx);
    let vyu = shift4_i16(vy);
    let faru = _mm256_or_si256(far(vx, vxu), far(vy, vyu));

    // i32: k-4 is exactly one 128-bit lane back.
    let r0u = _mm256_permute2x128_si256(r0, r0, 0x08); // [0, ref0..3]
    let r1u = _mm256_permute2x128_si256(r0, r1, 0x21); // [ref4..7, ref8..11]
    let uneq0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, r0u), _mm256_set1_epi32(-1));
    let uneq1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, r1u), _mm256_set1_epi32(-1));
    let up = _mm256_or_si256(
        pack(uneq0, uneq1),
        _mm256_and_si256(pack(live0, live1), faru),
    );

    // One bit per i16 lane out of a byte-granular movemask: take every other bit.
    let bits = |v: __m256i| -> u16 {
        let m = _mm256_movemask_epi8(v) as u32;
        let mut out = 0u16;
        let mut k = 0;
        while k < 16 {
            out |= (((m >> (k * 2)) & 1) as u16) << k;
            k += 1;
        }
        out
    };
    // Force the don't-care bits to zero so the twins match on the FULL u16.
    (bits(left) & 0xEEEE, bits(up) & 0xFFF0)
}

/// Safe dispatcher: AVX2 when present, else the SSE2 twin — never `None` on
/// x86-64 since 2026-08-27 (audit site 18): SSE2 is the x86-64 BASELINE, so
/// before the twin existed, a hypervisor masking AVX2 (or a pre-Haswell CPU)
/// silently dropped the packed-bS arm and everything routed on it back to the
/// scalar per-edge walk. The `Option` shape is kept for the callers.
#[cfg(target_arch = "x86_64")]
pub fn bs_motion_masks(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    no_ref: i32,
) -> Option<(u16, u16)> {
    // VENDOR(uefi-player): firmware single-bit gate — off means the caller's
    // scalar twin (`common` keeps it either way).
    if !crate::simd_enabled() {
        return None;
    }
    if has_avx2() {
        // SAFETY: all three inputs are fixed-size arrays of exactly the width the
        // kernel loads (16 x i16 = 32 B for each mv plane, 16 x i32 = 64 B for refs),
        // so every load below is in bounds by construction.
        Some(unsafe { bs_motion_masks_avx2(mvx, mvy, ref_id, no_ref) })
    } else {
        // SAFETY: as above; SSE2 is the x86-64 baseline (no detection needed).
        Some(unsafe { bs_motion_masks_sse2(mvx, mvy, ref_id, no_ref) })
    }
}

/// TWO-LIST boundary-strength motion masks (WHYS Part 16's named lever): the
/// §8.7.2.1 set-matching rule, vectorized. Until this kernel, any macroblock
/// with a List-1 slot fell to the scalar per-edge walk — most B inter MBs.
///
/// The branchless per-lane formula (proven case-equal to the scalar
/// `pk_differs` decision tree, including its slot-COMPACTION cases):
///
/// ```text
///   differs = !( (e0 & e1 & !farStraight) | (c0 & c1 & !farCross) )
///     e0/e1 = ref0/ref1 equal to neighbour's ref0/ref1 (straight)
///     c0/c1 = ref0/ref1 equal to neighbour's ref1/ref0 (crossed)
///     far*  = any |Δmv| ≥ 4 under that pairing
/// ```
///
/// The compaction cases work WITHOUT per-lane branching because unused-slot
/// motion is NEUTRALIZED to zero inside the kernel (`mv &= (ref != NO_REF)`):
/// a missing slot then always compares "near" against another missing slot and
/// its ref comparisons (NO_REF vs X) drive the set logic — exhaustively checked
/// against the scalar twin over random two-list inputs by
/// `bs_motion_masks_two_list_matches_scalar`.
///
/// # Safety
/// AVX2 only; caller (the safe dispatcher below) has verified the feature.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bs_motion_masks_two_list_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref0: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
    no_ref: i32,
) -> (u16, u16) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    let x0 = _mm256_loadu_si256(mvx.as_ptr() as *const __m256i);
    let y0 = _mm256_loadu_si256(mvy.as_ptr() as *const __m256i);
    let x1 = _mm256_loadu_si256(mvx1.as_ptr() as *const __m256i);
    let y1 = _mm256_loadu_si256(mvy1.as_ptr() as *const __m256i);
    let r0lo = _mm256_loadu_si256(ref0.as_ptr() as *const __m256i);
    let r0hi = _mm256_loadu_si256(ref0.as_ptr().add(8) as *const __m256i);
    let r1lo = _mm256_loadu_si256(ref1.as_ptr() as *const __m256i);
    let r1hi = _mm256_loadu_si256(ref1.as_ptr().add(8) as *const __m256i);

    let nr = _mm256_set1_epi32(no_ref);
    let ones = _mm256_set1_epi32(-1);
    // packs_epi32 interleaves 128-bit halves; permute4x64 restores lane order.
    let pack = |a: __m256i, b: __m256i| {
        _mm256_permute4x64_epi64(_mm256_packs_epi32(a, b), 0b11_01_10_00)
    };
    // NEUTRALIZE unused-slot motion: the formula's correctness on compaction
    // cases requires missing slots to carry (0,0) REGARDLESS of what the caller
    // stored (see the doc comment).
    let live0 = pack(
        _mm256_xor_si256(_mm256_cmpeq_epi32(r0lo, nr), ones),
        _mm256_xor_si256(_mm256_cmpeq_epi32(r0hi, nr), ones),
    );
    let live1 = pack(
        _mm256_xor_si256(_mm256_cmpeq_epi32(r1lo, nr), ones),
        _mm256_xor_si256(_mm256_cmpeq_epi32(r1hi, nr), ones),
    );
    let x0 = _mm256_and_si256(x0, live0);
    let y0 = _mm256_and_si256(y0, live0);
    let x1 = _mm256_and_si256(x1, live1);
    let y1 = _mm256_and_si256(y1, live1);

    let three = _mm256_set1_epi16(3);
    let far = |a: __m256i, b: __m256i| -> __m256i {
        let d = _mm256_abs_epi16(_mm256_sub_epi16(a, b));
        _mm256_cmpgt_epi16(d, three)
    };
    // One bit per i16 lane from the byte-granular movemask (same as the
    // single-list kernel).
    let bits = |v: __m256i| -> u16 {
        let m = _mm256_movemask_epi8(v) as u32;
        let mut out = 0u16;
        for k in 0..16 {
            out |= (((m >> (2 * k)) & 1) as u16) << k;
        }
        out
    };
    // differs for one shifted pairing, given the six shifted operands.
    let differs = |x0s: __m256i,
                   y0s: __m256i,
                   x1s: __m256i,
                   y1s: __m256i,
                   e0: __m256i,
                   e1: __m256i,
                   c0: __m256i,
                   c1: __m256i|
     -> __m256i {
        let far_s = _mm256_or_si256(
            _mm256_or_si256(far(x0, x0s), far(y0, y0s)),
            _mm256_or_si256(far(x1, x1s), far(y1, y1s)),
        );
        let far_x = _mm256_or_si256(
            _mm256_or_si256(far(x0, x1s), far(y0, y1s)),
            _mm256_or_si256(far(x1, x0s), far(y1, y0s)),
        );
        let ok_s = _mm256_andnot_si256(far_s, _mm256_and_si256(e0, e1));
        let ok_x = _mm256_andnot_si256(far_x, _mm256_and_si256(c0, c1));
        _mm256_xor_si256(_mm256_or_si256(ok_s, ok_x), _mm256_set1_epi16(-1))
    };

    // ---- LEFT (k-1): within-lane shifts; corrupted lanes are all k%4==0. ----
    let sh16 = |v: __m256i| _mm256_bslli_epi128(v, 2);
    let sh32 = |v: __m256i| _mm256_bslli_epi128(v, 4);
    let e0l = pack(
        _mm256_cmpeq_epi32(r0lo, sh32(r0lo)),
        _mm256_cmpeq_epi32(r0hi, sh32(r0hi)),
    );
    let e1l = pack(
        _mm256_cmpeq_epi32(r1lo, sh32(r1lo)),
        _mm256_cmpeq_epi32(r1hi, sh32(r1hi)),
    );
    let c0l = pack(
        _mm256_cmpeq_epi32(r0lo, sh32(r1lo)),
        _mm256_cmpeq_epi32(r0hi, sh32(r1hi)),
    );
    let c1l = pack(
        _mm256_cmpeq_epi32(r1lo, sh32(r0lo)),
        _mm256_cmpeq_epi32(r1hi, sh32(r0hi)),
    );
    let left = differs(sh16(x0), sh16(y0), sh16(x1), sh16(y1), e0l, e1l, c0l, c1l);

    // ---- UP (k-4): one 128-bit lane back for i32, alignr for i16. ----
    let shu16 = |v: __m256i| {
        let lo = _mm256_permute2x128_si256(v, v, 0x08);
        _mm256_alignr_epi8(v, lo, 8)
    };
    let shu32 = |lo: __m256i, hi: __m256i| -> (__m256i, __m256i) {
        (
            _mm256_permute2x128_si256(lo, lo, 0x08),
            _mm256_permute2x128_si256(lo, hi, 0x21),
        )
    };
    let (r0ulo, r0uhi) = shu32(r0lo, r0hi);
    let (r1ulo, r1uhi) = shu32(r1lo, r1hi);
    let e0u = pack(_mm256_cmpeq_epi32(r0lo, r0ulo), _mm256_cmpeq_epi32(r0hi, r0uhi));
    let e1u = pack(_mm256_cmpeq_epi32(r1lo, r1ulo), _mm256_cmpeq_epi32(r1hi, r1uhi));
    let c0u = pack(_mm256_cmpeq_epi32(r0lo, r1ulo), _mm256_cmpeq_epi32(r0hi, r1uhi));
    let c1u = pack(_mm256_cmpeq_epi32(r1lo, r0ulo), _mm256_cmpeq_epi32(r1hi, r0uhi));
    let up = differs(shu16(x0), shu16(y0), shu16(x1), shu16(y1), e0u, e1u, c0u, c1u);

    (bits(left) & 0xEEEE, bits(up) & 0xFFF0)
}

/// Safe dispatcher for the two-list masks kernel; `None` when AVX2 is absent.
///
/// DELIBERATELY AVX2-only (2026-08-27 audit, site 18): the two-list subset is
/// 3.1% of macroblocks (58M compares vs the uniform check's 557M — the counts
/// in [`mb_uniform_avx2`]'s doc), and its set-matching logic is the most
/// intricate kernel in the family. On a non-AVX2 CPU those macroblocks take
/// the scalar per-edge walk, which is the correct-by-construction oracle path.
/// If a census ever shows a B-heavy non-AVX2 population that matters, the
/// SSE2 twin follows the [`bs_motion_masks_sse2`] recipe.
#[cfg(target_arch = "x86_64")]
pub fn bs_motion_masks_two_list(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref0: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
    no_ref: i32,
) -> Option<(u16, u16)> {
    // VENDOR(uefi-player): firmware single-bit gate — off means the caller's
    // scalar twin (see above).
    if !crate::simd_enabled() {
        return None;
    }
    if !has_avx2() {
        return None;
    }
    // SAFETY: AVX2 presence verified above; all inputs are fixed-size arrays of
    // exactly the widths the kernel loads.
    Some(unsafe { bs_motion_masks_two_list_avx2(mvx, mvy, ref0, mvx1, mvy1, ref1, no_ref) })
}


/// AVX2 "does this macroblock have uniform motion" test — all 16 blocks sharing one
/// (ref, mv) on both lists.
///
/// Chosen over extending `bs_motion_masks_avx2` to two lists after COUNTING both
/// populations on the main corpus (see docs/WHYS-decoder-parf.md):
///
/// * uniform check runs on ALL 6,190,820 packed macroblocks  -> 557M compares
/// * mask derivation runs on 726,540 (11.7%)                 -> 105M compares
/// * the two-list subset is 203,090 (3.1% of all MBs)        ->  58M compares
///
/// The uniform check is 9.5x the two-list brick's work, and it is a broadcast-compare
/// rather than an order-independent set match — more prize for far less risk.
///
/// It also matters that the SCALAR version SHORT-CIRCUITS: a non-uniform macroblock
/// bails after a block or two, but a UNIFORM one (the common case — Skip plus
/// single-partition inter) walks all 15 comparisons. So the population paying full
/// scalar price is precisely the one this replaces with ~6 vector compares.
///
/// Returns `Some(uniform)`, or `None` when AVX2 is unavailable so the caller keeps
/// its scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mb_uniform_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
) -> bool {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    // 16 x i16 = one register; broadcast lane 0 and compare the whole plane at once.
    let eq16 = |v: &[i16; 16]| -> __m256i {
        let a = _mm256_loadu_si256(v.as_ptr() as *const __m256i);
        _mm256_cmpeq_epi16(a, _mm256_set1_epi16(v[0]))
    };
    // 16 x i32 = two registers; `packs` narrows the two predicate halves to i16 lanes
    // so every plane's result combines in one register.
    let eq32 = |v: &[i32; 16]| -> __m256i {
        let b = _mm256_set1_epi32(v[0]);
        let lo = _mm256_cmpeq_epi32(_mm256_loadu_si256(v.as_ptr() as *const __m256i), b);
        let hi = _mm256_cmpeq_epi32(_mm256_loadu_si256(v.as_ptr().add(8) as *const __m256i), b);
        _mm256_permute4x64_epi64(_mm256_packs_epi32(lo, hi), 0b11_01_10_00)
    };
    let all = _mm256_and_si256(
        _mm256_and_si256(_mm256_and_si256(eq16(mvx), eq16(mvy)), eq32(ref_id)),
        _mm256_and_si256(_mm256_and_si256(eq16(mvx1), eq16(mvy1)), eq32(ref1)),
    );
    // Every lane must have compared equal.
    _mm256_movemask_epi8(all) == -1
}

/// Safe dispatcher for the uniform-motion test: AVX2 when present, else the
/// SSE2 twin — never `None` on x86-64 since 2026-08-27 (audit site 18). This
/// test runs on ALL macroblocks (9.5x the masks kernel's work, per the counts
/// above), so it is the arm a masked-AVX2 VM was losing most of.
#[cfg(target_arch = "x86_64")]
#[allow(clippy::too_many_arguments)]
pub fn mb_uniform(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
) -> Option<bool> {
    // VENDOR(uefi-player): firmware single-bit gate — off means the caller's
    // scalar twin (see above).
    if !crate::simd_enabled() {
        return None;
    }
    if has_avx2() {
        // SAFETY: all six inputs are fixed-size arrays of exactly the width loaded
        // (16 x i16 = 32 B, 16 x i32 = 64 B), so every load is in bounds by construction.
        Some(unsafe { mb_uniform_avx2(mvx, mvy, ref_id, mvx1, mvy1, ref1) })
    } else {
        // SAFETY: as above; SSE2 is the x86-64 baseline.
        Some(unsafe { mb_uniform_sse2(mvx, mvy, ref_id, mvx1, mvy1, ref1) })
    }
}

/// AVX2 inverse quantization of one 4×4 block — `out[i] = f(levels[i] * ls[i])`,
/// the spec §8.5.12.1 flat/weighted dequant the decoder runs on every coded block.
///
/// **Why intrinsics here and not auto-vectorization** (the Step-0 gate, answered
/// empirically): the crate emits **zero `vpmulld`/`pmulld` and 502 scalar `imul`** —
/// LLVM's cost model declines to vectorize 32-bit integer multiply, because
/// `vpmulld` is 2 uops on this microarchitecture. It still wins decisively on uop
/// COUNT: 16 scalar `imul` become 2 `vpmulld`, and the shift/round becomes 2 more
/// vector ops instead of 16 scalar ones.
///
/// **Bit-identical, not merely close.** These are exact integer ops in the same order
/// as the scalar twin — same multiply, same rounding add, same arithmetic shift — so
/// this gates with `assert_eq!`, not a tolerance. `dequant_4x4_matches_scalar` pins it
/// over the full QP range including both sides of the `qp >= 24` branch.
///
/// The shift amount is a RUNTIME value, so this uses `_mm256_sll/sra_epi32` (variable
/// count in an xmm) rather than the immediate-only `_mm256_slli/srai_epi32`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dequant_4x4_avx2(out: &mut [i32; 16], levels: &[i32; 16], ls: &[i32; 16], qp: u8) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let l0 = _mm256_loadu_si256(levels.as_ptr() as *const __m256i);
    let l1 = _mm256_loadu_si256(levels.as_ptr().add(8) as *const __m256i);
    let s0 = _mm256_loadu_si256(ls.as_ptr() as *const __m256i);
    let s1 = _mm256_loadu_si256(ls.as_ptr().add(8) as *const __m256i);
    let p0 = _mm256_mullo_epi32(l0, s0);
    let p1 = _mm256_mullo_epi32(l1, s1);
    let shift = (qp / 6) as i32;
    let (r0, r1) = if qp >= 24 {
        let c = _mm_cvtsi32_si128(shift - 4);
        (_mm256_sll_epi32(p0, c), _mm256_sll_epi32(p1, c))
    } else {
        let add = _mm256_set1_epi32(1 << (3 - shift));
        let c = _mm_cvtsi32_si128(4 - shift);
        (
            _mm256_sra_epi32(_mm256_add_epi32(p0, add), c),
            _mm256_sra_epi32(_mm256_add_epi32(p1, add), c),
        )
    };
    _mm256_storeu_si256(out.as_mut_ptr() as *mut __m256i, r0);
    _mm256_storeu_si256(out.as_mut_ptr().add(8) as *mut __m256i, r1);
}

/// Safe dispatcher. `None` = no AVX2, caller keeps its scalar twin (the oracle).
#[cfg(target_arch = "x86_64")]
pub fn dequant_4x4(out: &mut [i32; 16], levels: &[i32; 16], ls: &[i32; 16], qp: u8) -> bool {
    // VENDOR(uefi-player): firmware single-bit gate — off means the caller's
    // scalar twin (`common` keeps it either way).
    if !crate::simd_enabled() {
        return false;
    }
    if crate::have_avx2() { // VENDOR(uefi-player): no_std => assumed max (see lib.rs).
        // SAFETY: all three are fixed [i32; 16] arrays — exactly the 2×256-bit the
        // kernel loads/stores. Every access is in bounds by construction.
        unsafe { dequant_4x4_avx2(out, levels, ls, qp) };
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------------
// SSE2 twins of the boundary-strength helpers (2026-08-27, reachability audit
// site 18). SSE2 is the x86-64 BASELINE: these need no runtime detection, so
// with them in place `mb_uniform` and `bs_motion_masks` NEVER decline on
// x86-64 — previously any environment without AVX2 (hypervisors masking it,
// pre-Haswell CPUs) silently dropped the packed-bS fast arm and the
// kind-routing built on it, with byte-identical output hiding the loss.
// Two-list stays AVX2-only (3.1% of MBs; see its dispatcher note).
//
// Correctness chain: common's dispatcher tests pin scalar == AVX2, and
// `sse2_twin_tests` below pins AVX2 == SSE2 on the same input distribution —
// so scalar == SSE2 transitively, on the dont-care-forced-to-zero contract.
// ---------------------------------------------------------------------------------

/// SSE2 twin of [`mb_uniform_avx2`]: same broadcast-compare over all six
/// planes, in 128-bit halves/quarters.
///
/// # Safety
/// Inputs are fixed-size arrays of exactly the widths loaded (16 x i16 = 32 B,
/// 16 x i32 = 64 B). SSE2 is the x86-64 baseline.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn mb_uniform_sse2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
) -> bool {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    // 16 x i16 = two registers; broadcast lane 0, compare both halves.
    let eq16 = |v: &[i16; 16]| -> __m128i {
        let b = _mm_set1_epi16(v[0]);
        let lo = _mm_cmpeq_epi16(_mm_loadu_si128(v.as_ptr() as *const __m128i), b);
        let hi = _mm_cmpeq_epi16(_mm_loadu_si128(v.as_ptr().add(8) as *const __m128i), b);
        _mm_and_si128(lo, hi)
    };
    // 16 x i32 = four registers; broadcast lane 0, compare all four.
    let eq32 = |v: &[i32; 16]| -> __m128i {
        let b = _mm_set1_epi32(v[0]);
        let m = |o: usize| _mm_cmpeq_epi32(_mm_loadu_si128(v.as_ptr().add(o) as *const __m128i), b);
        _mm_and_si128(_mm_and_si128(m(0), m(4)), _mm_and_si128(m(8), m(12)))
    };
    let all = _mm_and_si128(
        _mm_and_si128(_mm_and_si128(eq16(mvx), eq16(mvy)), eq32(ref_id)),
        _mm_and_si128(_mm_and_si128(eq16(mvx1), eq16(mvy1)), eq32(ref1)),
    );
    _mm_movemask_epi8(all) == 0xFFFF
}

/// SSE2 twin of [`bs_motion_masks_avx2`] — identical §8.7.2.1 logic, 128-bit
/// registers. The AVX2 kernel's lane-boundary trick maps 1:1: its within-lane
/// byte shifts corrupt k=0/k=8 (i16) and k=0/4/8/12 (i32); here each 128-bit
/// register IS one of those lanes, so the same positions corrupt and the same
/// `& 0xEEEE` / `& 0xFFF0` discard them. Dont-care bits forced to zero, so
/// the three twins (scalar/AVX2/SSE2) match on the FULL u16.
///
/// # Safety
/// Inputs are fixed-size arrays of exactly the widths loaded. SSE2 is the
/// x86-64 baseline.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn bs_motion_masks_sse2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    no_ref: i32,
) -> (u16, u16) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let vx_lo = _mm_loadu_si128(mvx.as_ptr() as *const __m128i);
    let vx_hi = _mm_loadu_si128(mvx.as_ptr().add(8) as *const __m128i);
    let vy_lo = _mm_loadu_si128(mvy.as_ptr() as *const __m128i);
    let vy_hi = _mm_loadu_si128(mvy.as_ptr().add(8) as *const __m128i);
    let ra = _mm_loadu_si128(ref_id.as_ptr() as *const __m128i); // k0..3
    let rb = _mm_loadu_si128(ref_id.as_ptr().add(4) as *const __m128i); // k4..7
    let rc = _mm_loadu_si128(ref_id.as_ptr().add(8) as *const __m128i); // k8..11
    let rd = _mm_loadu_si128(ref_id.as_ptr().add(12) as *const __m128i); // k12..15
    let zero = _mm_setzero_si128();
    let three = _mm_set1_epi16(3);
    let nr = _mm_set1_epi32(no_ref);
    let ones = _mm_set1_epi32(-1);

    // |a - b| >= 4 per i16 lane, as `|a-b| > 3`. SSE2 has no `abs_epi16`;
    // `max(d, 0-d)` reproduces it EXACTLY, including the i16::MIN -> i16::MIN
    // wrap the AVX2 kernel documents as matching the scalar twin's overflow.
    let far = |a: __m128i, b: __m128i| -> __m128i {
        let d = _mm_sub_epi16(a, b);
        let ad = _mm_max_epi16(d, _mm_sub_epi16(zero, d));
        _mm_cmpgt_epi16(ad, three)
    };

    // ---- LEFT (k-1): byte shifts corrupt k=0 (lo half) and k=8 (hi half),
    // both k%4==0, both discarded by the final `& 0xEEEE`.
    let farl_lo = _mm_or_si128(
        far(vx_lo, _mm_slli_si128(vx_lo, 2)),
        far(vy_lo, _mm_slli_si128(vy_lo, 2)),
    );
    let farl_hi = _mm_or_si128(
        far(vx_hi, _mm_slli_si128(vx_hi, 2)),
        far(vy_hi, _mm_slli_si128(vy_hi, 2)),
    );
    // refs, k-1 within each 4-block register: corrupts k0/k4/k8/k12 (discarded).
    let neql = |r: __m128i| _mm_xor_si128(_mm_cmpeq_epi32(r, _mm_slli_si128(r, 4)), ones);
    // The "far" term is gated on ref[a] != NO_REF (the current block's ref).
    let live = |r: __m128i| _mm_xor_si128(_mm_cmpeq_epi32(r, nr), ones);
    // Narrow i32 predicates to i16 lanes; within 128 bits `packs` keeps order.
    let live_lo = _mm_packs_epi32(live(ra), live(rb));
    let live_hi = _mm_packs_epi32(live(rc), live(rd));
    let left_lo = _mm_or_si128(
        _mm_packs_epi32(neql(ra), neql(rb)),
        _mm_and_si128(live_lo, farl_lo),
    );
    let left_hi = _mm_or_si128(
        _mm_packs_epi32(neql(rc), neql(rd)),
        _mm_and_si128(live_hi, farl_hi),
    );

    // ---- UP (k-4): the lo half's k0..3 have no in-register neighbour
    // (garbage, discarded by `& 0xFFF0`); k4..7 read k0..3; the hi half reads
    // the straddling window k4..k11.
    let vxu_lo = _mm_slli_si128(vx_lo, 8);
    let vyu_lo = _mm_slli_si128(vy_lo, 8);
    let up_win = |lo: __m128i, hi: __m128i| _mm_unpacklo_epi64(_mm_srli_si128(lo, 8), hi);
    let vxu_hi = up_win(vx_lo, vx_hi);
    let vyu_hi = up_win(vy_lo, vy_hi);
    let faru_lo = _mm_or_si128(far(vx_lo, vxu_lo), far(vy_lo, vyu_lo));
    let faru_hi = _mm_or_si128(far(vx_hi, vxu_hi), far(vy_hi, vyu_hi));
    // refs, k-4 = exactly one register back; k0..3's comparand is arbitrary
    // (self => neq 0) — those bits are discarded anyway.
    let uneq = |r: __m128i, rup: __m128i| _mm_xor_si128(_mm_cmpeq_epi32(r, rup), ones);
    let up_lo = _mm_or_si128(
        _mm_packs_epi32(uneq(ra, ra), uneq(rb, ra)),
        _mm_and_si128(live_lo, faru_lo),
    );
    let up_hi = _mm_or_si128(
        _mm_packs_epi32(uneq(rc, rb), uneq(rd, rc)),
        _mm_and_si128(live_hi, faru_hi),
    );

    // One bit per i16 lane: saturate the 0/-1 i16 predicates to i8 and take
    // one byte-mask (packs_epi16 keeps lo lanes 0..7 then hi lanes 8..15).
    let bits = |lo: __m128i, hi: __m128i| -> u16 { _mm_movemask_epi8(_mm_packs_epi16(lo, hi)) as u16 };
    (bits(left_lo, left_hi) & 0xEEEE, bits(up_lo, up_hi) & 0xFFF0)
}

/// AVX2 == SSE2 differential for the bS helpers (the scalar == AVX2 half of
/// the chain lives in rusty_h264-common's dispatcher tests). Same randomized
/// distribution as the common tests, PLUS i16 extremes pinning the `abs` wrap.
#[cfg(test)]
mod sse2_twin_tests {
    use super::*;

    const NO_REF: i32 = -1;

    fn rnd(seed: &mut u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed
    }

    fn random_case(seed: &mut u32) -> ([i16; 16], [i16; 16], [i32; 16]) {
        let mut mvx = [0i16; 16];
        let mut mvy = [0i16; 16];
        let mut refs = [0i32; 16];
        for k in 0..16 {
            let r = rnd(seed);
            refs[k] = match r & 3 {
                0 => NO_REF,
                1 => 10,
                2 => 20,
                _ => 30,
            };
            // Motion near the +/-4 threshold, with occasional i16 extremes to
            // pin the max(d, -d) == abs_epi16 wrap equivalence.
            mvx[k] = match (r >> 2) & 15 {
                0 => i16::MIN,
                1 => i16::MAX,
                _ => ((r >> 8) % 11) as i16 - 5,
            };
            mvy[k] = match (r >> 6) & 15 {
                0 => i16::MIN,
                1 => i16::MAX,
                _ => ((r >> 12) % 11) as i16 - 5,
            };
        }
        (mvx, mvy, refs)
    }

    #[test]
    fn bs_motion_masks_sse2_matches_avx2() {
        if !has_avx2() {
            eprintln!("skipped: no AVX2 on this host (the SSE2 arm IS the shipping arm here)");
            return;
        }
        let mut seed = 0x1234_5678u32;
        for round in 0..50_000usize {
            let (mvx, mvy, refs) = random_case(&mut seed);
            let a = unsafe { bs_motion_masks_avx2(&mvx, &mvy, &refs, NO_REF) };
            let s = unsafe { bs_motion_masks_sse2(&mvx, &mvy, &refs, NO_REF) };
            assert_eq!(a, s, "round {round} mvx={mvx:?} mvy={mvy:?} refs={refs:?}");
        }
    }

    #[test]
    fn mb_uniform_sse2_matches_avx2() {
        if !has_avx2() {
            eprintln!("skipped: no AVX2 on this host (the SSE2 arm IS the shipping arm here)");
            return;
        }
        let mut seed = 0x8765_4321u32;
        for round in 0..50_000usize {
            let (mvx, mvy, refs) = random_case(&mut seed);
            let (mvx1, mvy1, refs1) = random_case(&mut seed);
            let a = unsafe { mb_uniform_avx2(&mvx, &mvy, &refs, &mvx1, &mvy1, &refs1) };
            let s = unsafe { mb_uniform_sse2(&mvx, &mvy, &refs, &mvx1, &mvy1, &refs1) };
            assert_eq!(a, s, "round {round}");
            // Random inputs are almost never uniform; force the TRUE branch
            // too (the common case in real streams: Skip + single-partition).
            let um = [mvx[0]; 16];
            let uy = [mvy[0]; 16];
            let ur = [refs[0]; 16];
            let a = unsafe { mb_uniform_avx2(&um, &uy, &ur, &um, &uy, &ur) };
            let s = unsafe { mb_uniform_sse2(&um, &uy, &ur, &um, &uy, &ur) };
            assert!(a && s, "round {round}: uniform case must be true on both arms");
        }
    }
}
