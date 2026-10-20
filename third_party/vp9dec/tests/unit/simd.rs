use super::transform::{
    b_op_simd, b_op_simd_hbd, iadst_i64, iadst_simd, idct_i64, idct_permute_simd, idct_simd,
    sb_op_simd, sh_op_simd,
};
use super::*;
use crate::predict::MAX_INTERMEDIATE_HEIGHT;
use std::arch::x86_64::*;

fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// `block_inter_predict_avx2_w4` must produce exactly the leftmost 4 output columns that the
/// already-sweep-proven 8-wide `block_inter_predict_avx2` produces from the same source and
/// params: each output column's two-pass FIR depends only on its own source-column
/// neighborhood, so a width-4 block's 4 columns equal the leftmost 4 of a width-8 block at
/// the same origin. This pins the width-4 kernel to the width-8 one with no new reference
/// arithmetic (the sweep is the ultimate gate, but this localizes any width-4 lane/offset
/// bug immediately).
#[test]
fn avx2_w4_matches_w8_leftmost_four_columns() {
    if !avx2_enabled() {
        return;
    }
    let ref_width = 40usize;
    let ref_height = 40usize;
    let mut seed = 0x1234_5678u32;
    let ref_data: Vec<u16> = (0..ref_width * ref_height)
        .map(|_| (xorshift32(&mut seed) & 0xFF) as u16)
        .collect();

    // Interior origin (well away from the buffer edges so every read is in bounds); a
    // couple of block heights, all 4 interp filters, and integer + fractional subpel phases.
    let src_row0 = 8i64;
    let src_col0 = 8i64;
    for &h in &[4usize, 8] {
        let intermediate_height = h + 7;
        for interp_filter in 0..4u8 {
            for &fx in &[0usize, 5, 15] {
                for &fy in &[0usize, 8, 11] {
                    let mut pred8 = vec![0i32; 8 * h];
                    let mut pred4 = vec![0i32; 4 * h];
                    // SAFETY: avx2_enabled() confirmed; the interior origin keeps every read
                    // within `ref_data`, satisfying both kernels' documented bounds.
                    unsafe {
                        block_inter_predict_avx2(
                            &ref_data,
                            ref_width,
                            src_row0,
                            src_col0,
                            fx,
                            fy,
                            8,
                            h,
                            intermediate_height,
                            interp_filter,
                            255,
                            &mut pred8,
                        );
                        block_inter_predict_avx2_w4(
                            &ref_data,
                            ref_width,
                            src_row0,
                            src_col0,
                            fx,
                            fy,
                            h,
                            intermediate_height,
                            interp_filter,
                            255,
                            &mut pred4,
                        );
                    }
                    for r in 0..h {
                        for c in 0..4 {
                            assert_eq!(
                                pred4[r * 4 + c],
                                pred8[r * 8 + c],
                                "h={h} filt={interp_filter} fx={fx} fy={fy} at r={r} c={c}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The scaled-reference kernel (`block_inter_predict_scaled_avx2`) must exactly reproduce
/// the scalar `predict::block_inter_predict_scalar` across: widths (4 -- the padded single
/// group -- and the 8-wide-group multiples), scale steps on both axes (upscale 1, the
/// spec-conformant range up to the §8.5.2.3 maximum 32, mixed x/y including one axis
/// unscaled, and fully unscaled 16/16), start positions engaging the left/top edge clamp,
/// the right/bottom clamp, and interior subpel phases, and all bit depths (8/10/12 via
/// `max_val`). The filter
/// index rotates pseudo-randomly across the matrix, covering all 4 filter banks over the
/// run. The (w=64, x_step=32) case lands `span` exactly on the MAX_INTERMEDIATE_HEIGHT
/// scratch bound.
#[test]
fn scaled_inter_predict_avx2_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let ref_w = 48usize;
    let ref_h = 40usize;
    let mut seed = 0x5CA1_ED01u32;
    for &bit_depth in &[8u8, 10, 12] {
        let sample_mask = (1u32 << bit_depth) - 1;
        let mut plane = crate::framebuffer::Plane::new(ref_w, ref_h);
        for yy in 0..ref_h {
            for xx in 0..ref_w {
                plane.set(xx, yy, (xorshift32(&mut seed) & sample_mask) as u16);
            }
        }
        for &(w, h) in &[(4usize, 4usize), (4, 8), (8, 8), (16, 4), (32, 8), (64, 4)] {
            for &(x_step, y_step) in &[
                (1i64, 32i64),
                (8, 8),
                (9, 27),
                (16, 16),
                (16, 24),
                (24, 16),
                (31, 31),
                (32, 32),
            ] {
                for &(x, y) in &[
                    (-97i64, -70i64),                           // heavy left + top clamp
                    (3, 7),                                     // interior, small phases
                    (37 * 16 + 11, 21 * 16 + 5),                // interior / right-bottom clamp
                    ((ref_w as i64) << 4, (ref_h as i64) << 4), // fully past right/bottom
                ] {
                    let interp_filter = (xorshift32(&mut seed) & 3) as u8;
                    let intermediate_height = ((((h as i64 - 1) * y_step + 15) >> 4) + 8) as usize;
                    let span = ((((x & 15) + x_step * (w as i64 - 1)) >> 4) + 8) as usize;
                    assert!(
                        intermediate_height <= MAX_INTERMEDIATE_HEIGHT
                            && span <= MAX_INTERMEDIATE_HEIGHT
                    );
                    let mut pred_scalar = vec![0i32; w * h];
                    crate::predict::block_inter_predict_scalar(
                        &plane,
                        x,
                        y,
                        x_step,
                        y_step,
                        w,
                        h,
                        interp_filter,
                        bit_depth,
                        &mut pred_scalar,
                    );
                    let mut pred_simd = vec![0i32; w * h];
                    // SAFETY: avx2_enabled() confirmed; the plane slice is exactly
                    // ref_w * ref_h and both scratch bounds were just asserted, matching
                    // the kernel's documented contract (it clamps all source reads).
                    unsafe {
                        block_inter_predict_scaled_avx2(
                            plane.as_slice(),
                            ref_w,
                            ref_h,
                            x,
                            y,
                            x_step,
                            y_step,
                            w,
                            h,
                            intermediate_height,
                            interp_filter,
                            (1i32 << bit_depth) - 1,
                            &mut pred_simd,
                        );
                    }
                    assert_eq!(
                        pred_scalar, pred_simd,
                        "bd={bit_depth} w={w} h={h} steps=({x_step},{y_step}) \
                         pos=({x},{y}) filt={interp_filter}"
                    );
                }
            }
        }
    }
}

/// The vector 1D inverse DCT (`idct_permute_simd` + `idct_simd`) must exactly reproduce the
/// scalar `transform::idct_permute` + `transform::idct` on each of its 8 lanes, for every
/// size n = 2..=5. Inputs are kept small (structure-validating) so no intermediate exceeds
/// i32 -- magnitude-independent index/angle/flip bugs still show up, and the i32-vs-i64
/// agreement on realistic ranges is covered end-to-end by the 2D test and the official sweep.
#[test]
fn idct_simd_1d_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x9E3779B9u32;
    for n in 2..=5u32 {
        let n0 = 1usize << n;
        for _ in 0..200 {
            let mut rows = [[0i64; 32]; 8];
            for row in rows.iter_mut() {
                for v in row[..n0].iter_mut() {
                    *v = (xorshift32(&mut seed) & 0x7FF) as i64 - 1024;
                }
            }
            let mut scalar = rows;
            for row in scalar.iter_mut() {
                crate::transform::idct_permute(&mut row[..n0], n);
                crate::transform::idct(&mut row[..n0], n);
            }
            let mut t = [unsafe { _mm256_setzero_si256() }; 32];
            for (k, tk) in t[..n0].iter_mut().enumerate() {
                let lanes: [i32; 8] = std::array::from_fn(|r| rows[r][k] as i32);
                *tk = unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) };
            }
            unsafe {
                idct_permute_simd(&mut t, n);
                idct_simd::<false>(&mut t, n);
            }
            for (k, &tk) in t[..n0].iter().enumerate() {
                let mut lanes = [0i32; 8];
                unsafe { _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk) };
                for (r, &lane) in lanes.iter().enumerate() {
                    assert_eq!(lane as i64, scalar[r][k], "n={n}: row {r}, elem {k}");
                }
            }
        }
    }
}

/// The fused `inverse_transform_dct_dct_reconstruct_avx2` (2D transform + per-pixel residual
/// add + 8-bit clip) must exactly reproduce the scalar `inverse_transform_block` (DctDct)
/// followed by the scalar `clip(pred + residual, 0, 255)` reconstruction, for every size
/// n = 2..=5. Inputs are kept small (±64) so no transform intermediate exceeds i32 even in
/// the worst case, isolating the transpose/driver/mirror/reconstruction structure; the
/// i32-vs-i64 agreement on realistic (spec §8.7.1.1 16-bit-bounded) magnitudes is proven
/// end-to-end by the official sweep.
#[test]
fn inverse_transform_dct_dct_reconstruct_simd_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x2468ACE0u32;
    for n in 2..=5u32 {
        let n0 = 1usize << n;
        let count = n0 * n0;
        for _ in 0..100 {
            let mut dq = vec![0i64; count];
            for v in dq.iter_mut() {
                *v = (xorshift32(&mut seed) & 0x7F) as i64 - 64;
            }
            let mut dq_s = dq.clone();
            crate::transform::inverse_transform_block(
                &mut dq_s,
                n,
                crate::transform::TxType::DctDct,
                false,
            );
            // Two placements: (0, 0) in a tight stride-n0 plane, and a nonzero origin
            // inside a wider plane -- the offset/stride seam of `reconstruct_add_clip`'s
            // raw `(start_y + i) * plane_width + start_x` indexing (exactly what the
            // tile-parallel column strips feed it). Whole-plane equality vs scalar also
            // proves no out-of-block writes.
            for &(start_x, start_y, plane_width) in &[(0usize, 0usize, n0), (4, 2, n0 + 8)] {
                let pred: Vec<u16> = (0..plane_width * (start_y + n0))
                    .map(|_| (xorshift32(&mut seed) & 0xFF) as u16)
                    .collect();
                let mut plane_scalar = pred.clone();
                for i in 0..n0 {
                    for j in 0..n0 {
                        let idx = (start_y + i) * plane_width + start_x + j;
                        let old = plane_scalar[idx] as i64;
                        plane_scalar[idx] = (old + dq_s[i * n0 + j]).clamp(0, 255) as u16;
                    }
                }

                let mut plane_simd = pred.clone();
                unsafe {
                    inverse_transform_dct_dct_reconstruct_avx2(
                        &mut plane_simd,
                        plane_width,
                        start_x,
                        start_y,
                        &dq,
                        n,
                    );
                }
                assert_eq!(
                    plane_scalar, plane_simd,
                    "n={n} at ({start_x},{start_y}) stride {plane_width}: fused SIMD != scalar"
                );
            }
        }
    }
}

/// The HBD 1D inverse DCT (`idct_simd::<true>`, i.e. the `b_op_simd_hbd` butterfly) must
/// exactly reproduce the scalar i64 `transform::idct_permute` + `transform::idct` on each
/// lane, for every size n = 2..=5, at full 12-bit magnitudes: inputs span ±2^19 (the spec
/// §8.7.1.1 `8 + BitDepth` bound), where `t * cos64` products reach ~2^33 -- beyond i32 --
/// so this genuinely exercises the widened multiplies. (All STORED values stay inside i32:
/// the 1D network's L2 gain is ~2^3 and L-inf <= sqrt(32) * L2, keeping intermediates under
/// ~2^25.) As a self-check that these magnitudes discriminate, the 8-bit (`mullo`) network
/// must diverge from the scalar somewhere across the run.
#[test]
fn idct_simd_hbd_1d_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0xB5297A4Du32;
    let mut mullo_diverged = false;
    for n in 2..=5u32 {
        let n0 = 1usize << n;
        for _ in 0..200 {
            let mut rows = [[0i64; 32]; 8];
            for row in rows.iter_mut() {
                for v in row[..n0].iter_mut() {
                    *v = (xorshift32(&mut seed) & 0xF_FFFF) as i64 - (1 << 19);
                }
            }
            let mut scalar = rows;
            for row in scalar.iter_mut() {
                crate::transform::idct_permute(&mut row[..n0], n);
                crate::transform::idct(&mut row[..n0], n);
            }
            let mut t = [unsafe { _mm256_setzero_si256() }; 32];
            for (k, tk) in t[..n0].iter_mut().enumerate() {
                let lanes: [i32; 8] = std::array::from_fn(|r| rows[r][k] as i32);
                *tk = unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) };
            }
            let mut t_mullo = t;
            unsafe {
                idct_permute_simd(&mut t, n);
                idct_simd::<true>(&mut t, n);
                idct_permute_simd(&mut t_mullo, n);
                idct_simd::<false>(&mut t_mullo, n);
            }
            for (k, (&tk, &mk)) in t[..n0].iter().zip(t_mullo[..n0].iter()).enumerate() {
                let mut lanes = [0i32; 8];
                let mut lanes_mullo = [0i32; 8];
                unsafe {
                    _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk);
                    _mm256_storeu_si256(lanes_mullo.as_mut_ptr() as *mut __m256i, mk);
                }
                for (r, (&lane, &lane_mullo)) in lanes.iter().zip(lanes_mullo.iter()).enumerate() {
                    assert_eq!(lane as i64, scalar[r][k], "n={n}: row {r}, elem {k}");
                    mullo_diverged |= lane_mullo as i64 != scalar[r][k];
                }
            }
        }
    }
    assert!(
        mullo_diverged,
        "test inputs never overflowed the i32-product network -- magnitudes too small to \
         exercise the widened HBD path"
    );
}

/// The fused HBD `inverse_transform_dct_dct_reconstruct_hbd_avx2` (2D transform + residual
/// add + `(1 << bit_depth) - 1` clip) must exactly reproduce the scalar
/// `inverse_transform_block` (DctDct) followed by the scalar `clamp(pred + residual, 0,
/// max_val)` reconstruction, for every size n = 2..=5 at 10-bit AND 12-bit. Dequant inputs
/// span ±2^(bit_depth+5) -- large enough that the column pass's butterfly products routinely
/// exceed i32 at 12-bit (exercising the widened path end-to-end) while every stored
/// intermediate stays inside the i32 lanes (the two-pass L2-gain bound keeps them under
/// ~2^28); predictions span the full `0..(1 << bit_depth)` sample range. ±2^(bit_depth+5)
/// suffices because the 1D network is already pinned at the full signed-`8 + BitDepth`-bit
/// input domain by `idct_simd_hbd_1d_matches_scalar`, and everything the 2D driver adds on
/// top of the two 1D passes (transposes, the final `round2` shift, the reconstruction) is
/// structural, not magnitude-sensitive -- this test's job is that composition, not the
/// arithmetic extremes.
#[test]
fn inverse_transform_dct_dct_reconstruct_hbd_simd_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x0DDB1A5Eu32;
    for &bit_depth in &[10u8, 12] {
        let max_val = (1i64 << bit_depth) - 1;
        let dq_span = 1u32 << (bit_depth + 6); // dequant values in ±2^(bit_depth+5)
        for n in 2..=5u32 {
            let n0 = 1usize << n;
            let count = n0 * n0;
            for _ in 0..100 {
                let mut dq = vec![0i64; count];
                for v in dq.iter_mut() {
                    *v = (xorshift32(&mut seed) % dq_span) as i64 - (dq_span / 2) as i64;
                }
                let mut dq_s = dq.clone();
                crate::transform::inverse_transform_block(
                    &mut dq_s,
                    n,
                    crate::transform::TxType::DctDct,
                    false,
                );
                // Two placements (see the 8-bit DCT test): (0, 0)/stride n0, plus a
                // nonzero origin in a wider plane covering `reconstruct_add_clip`'s
                // offset/stride indexing -- for HBD nothing else covers that seam.
                // Whole-plane equality also proves no out-of-block writes.
                for &(start_x, start_y, plane_width) in &[(0usize, 0usize, n0), (4, 2, n0 + 8)] {
                    let pred: Vec<u16> = (0..plane_width * (start_y + n0))
                        .map(|_| (xorshift32(&mut seed) & (max_val as u32)) as u16)
                        .collect();
                    let mut plane_scalar = pred.clone();
                    for i in 0..n0 {
                        for j in 0..n0 {
                            let idx = (start_y + i) * plane_width + start_x + j;
                            let old = plane_scalar[idx] as i64;
                            plane_scalar[idx] = (old + dq_s[i * n0 + j]).clamp(0, max_val) as u16;
                        }
                    }

                    let mut plane_simd = pred.clone();
                    unsafe {
                        inverse_transform_dct_dct_reconstruct_hbd_avx2(
                            &mut plane_simd,
                            plane_width,
                            start_x,
                            start_y,
                            &dq,
                            n,
                            bit_depth,
                        );
                    }
                    assert_eq!(
                        plane_scalar, plane_simd,
                        "bit_depth={bit_depth} n={n} at ({start_x},{start_y}) stride \
                         {plane_width}: fused HBD SIMD != scalar"
                    );
                }
            }
        }
    }
}

/// The vector 1D inverse ADST (`iadst_simd`) must exactly reproduce the scalar
/// `transform::iadst4/8/16` on each of its 8 lanes. n == 2 runs at the FULL spec §8.7.1.1
/// input bound (`8 + BitDepth == 16` signed bits, -32768..=32767): every iadst4
/// intermediate provably fits i32 there (peak chain 43801 * 2^15 < 2^31 -- see the
/// inverse-ADST section comment). n == 3/4 use structure-validating ±512 inputs: an
/// adversarial full-range VECTOR is not reachable from a conformant stream (each SB/SH
/// stage re-bounds stored T values to 16 bits, which random full-range stage inputs
/// violate) and would overflow i32 in ways conformant data cannot. The per-op spec-bound
/// coverage is `adst_sb_sh_ops_match_scalar_at_spec_bounds`; the end-to-end
/// conformant-magnitude proof is the official sweep.
#[test]
fn iadst_simd_1d_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0xC0FF_EE11u32;
    for n in 2..=4u32 {
        let n0 = 1usize << n;
        for _ in 0..200 {
            let mut rows = [[0i64; 16]; 8];
            for row in rows.iter_mut() {
                for v in row[..n0].iter_mut() {
                    *v = if n == 2 {
                        (xorshift32(&mut seed) & 0xFFFF) as i64 - 32768
                    } else {
                        (xorshift32(&mut seed) & 0x3FF) as i64 - 512
                    };
                }
            }
            let mut scalar = rows;
            for row in scalar.iter_mut() {
                match n {
                    2 => crate::transform::iadst4((&mut row[..4]).try_into().unwrap()),
                    3 => crate::transform::iadst8((&mut row[..8]).try_into().unwrap()),
                    _ => crate::transform::iadst16((&mut row[..16]).try_into().unwrap()),
                }
            }
            let mut t = [unsafe { _mm256_setzero_si256() }; 32];
            for (k, tk) in t[..n0].iter_mut().enumerate() {
                let lanes: [i32; 8] = std::array::from_fn(|r| rows[r][k] as i32);
                *tk = unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) };
            }
            unsafe { iadst_simd(&mut t, n) };
            for (k, &tk) in t[..n0].iter().enumerate() {
                let mut lanes = [0i32; 8];
                unsafe { _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk) };
                for (r, &lane) in lanes.iter().enumerate() {
                    assert_eq!(lane as i64, scalar[r][k], "n={n}: row {r}, elem {k}");
                }
            }
        }
    }
}

/// `sb_op_simd` + `sh_op_simd` -- the ADST ops carrying the load-bearing i32 overflow
/// claims -- must exactly match the spec's i64 arithmetic at the FULL spec §8.7.1.1 T
/// bound (-32768..=32767, with the exact corners injected), for every rotation angle the
/// iadst8/16 networks use, in both flip orientations. The SH inputs are the SB outputs of
/// two INDEPENDENT worst-case pairs, so the `S[a] +/- S[b]` sums are exercised at their
/// true maxima (|S| <= 2^15 * 23170 < 2^30, sums < 2^31).
#[test]
fn adst_sb_sh_ops_match_scalar_at_spec_bounds() {
    if !avx2_enabled() {
        return;
    }
    // Union of the `SB` angles in `transform.rs::iadst8_impl` / `iadst16_impl`.
    let angles = [30, 22, 14, 6, 24, 8, 31, 27, 23, 19, 15, 11, 7, 3, 28, 12];
    let mut seed = 0xAD57_0B57u32;
    for &angle in &angles {
        for &flip in &[false, true] {
            for trial in 0..100 {
                // vals = [ta, tb, tc, td], each 8 lanes in the full signed-16-bit range;
                // trial 0 plants the exact corners in the first two lanes.
                let mut vals = [[0i64; 8]; 4];
                for (vi, v) in vals.iter_mut().enumerate() {
                    for (lane, x) in v.iter_mut().enumerate() {
                        *x = if trial == 0 && lane < 2 {
                            if (lane + vi) % 2 == 0 {
                                -32768
                            } else {
                                32767
                            }
                        } else {
                            (xorshift32(&mut seed) & 0xFFFF) as i64 - 32768
                        };
                    }
                }

                // Scalar i64 reference: SB on (ta, tb) -> s0/s1, SB on (tc, td) -> s2/s3
                // (spec §8.7.1.1's SB, with flip), then SH across the two rotations.
                let cos = crate::transform::cos64(angle);
                let sin = crate::transform::sin64(angle);
                let round2_14_i64 = |x: i64| (x + (1i64 << 13)) >> 14;
                let mut expected = [[0i64; 8]; 4];
                for lane in 0..8 {
                    let (ta, tb) = (vals[0][lane], vals[1][lane]);
                    let (tc, td) = (vals[2][lane], vals[3][lane]);
                    let (mut s0, mut s1) = (ta * cos - tb * sin, ta * sin + tb * cos);
                    let (mut s2, mut s3) = (tc * cos - td * sin, tc * sin + td * cos);
                    if flip {
                        std::mem::swap(&mut s0, &mut s1);
                        std::mem::swap(&mut s2, &mut s3);
                    }
                    expected[0][lane] = round2_14_i64(s0 + s2);
                    expected[2][lane] = round2_14_i64(s0 - s2);
                    expected[1][lane] = round2_14_i64(s1 + s3);
                    expected[3][lane] = round2_14_i64(s1 - s3);
                }

                let load = |v: &[i64; 8]| {
                    let lanes: [i32; 8] = std::array::from_fn(|i| v[i] as i32);
                    unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) }
                };
                let mut t = [
                    load(&vals[0]),
                    load(&vals[1]),
                    load(&vals[2]),
                    load(&vals[3]),
                ];
                let mut s = [unsafe { _mm256_setzero_si256() }; 4];
                unsafe {
                    sb_op_simd(&mut s, &t, 0, 1, angle, flip);
                    sb_op_simd(&mut s, &t, 2, 3, angle, flip);
                    sh_op_simd(&mut t, &s, 0, 2);
                    sh_op_simd(&mut t, &s, 1, 3);
                }
                for (k, &tk) in t.iter().enumerate() {
                    let mut lanes = [0i32; 8];
                    unsafe { _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk) };
                    for (lane, &got) in lanes.iter().enumerate() {
                        assert_eq!(
                            got as i64, expected[k][lane],
                            "angle={angle} flip={flip} trial={trial}: t[{k}] lane {lane}"
                        );
                    }
                }
            }
        }
    }
}

/// `b_op_simd` / `b_op_simd_hbd` -- the `B` butterfly shared by the DCT and ADST networks,
/// whose i32-`mullo` (8-bit) and widened-i64-product (10/12-bit) exactness claims are
/// load-bearing -- must exactly match the spec's i64 arithmetic at the FULL spec §8.7.1.1
/// stored-T bound for its depth (signed 16 bits for `b_op_simd`; the signed-20-bit
/// 12-bit-depth bound for `b_op_simd_hbd`, where the products genuinely exceed i32), with
/// the exact corners injected, for every rotation angle either network uses, in both flip
/// orientations. Per-op companion to `adst_sb_sh_ops_match_scalar_at_spec_bounds` (SB/SH
/// were previously the only ops tested at their bound).
#[test]
fn b_op_matches_scalar_at_spec_bounds() {
    if !avx2_enabled() {
        return;
    }
    // Union of every `B(a, b, angle, flip)` angle in the vector networks: `idct_simd` --
    // the n == 2 base and final-stage angle 16; `32 - brev(5, a)` over the recursion
    // levels (n=3: 28,12; n=4: 30,14,22,6; n=5: 31,15,23,7,27,11,19,3); the n == 5 stage
    // `28 - 16i + 56j` (84,68 beyond the above); the n >= 4 stage `24 + 48j` (24,72) --
    // plus `iadst8_impl` (16) and `iadst16_impl` (`48 + 64*(i^j)`: 48,112).
    let angles = [
        3, 6, 7, 11, 12, 14, 15, 16, 19, 22, 23, 24, 27, 28, 30, 31, 48, 68, 72, 84, 112,
    ];
    let mut seed = 0xB0B0_5EEDu32;
    for &(hbd, bound) in &[(false, 1i64 << 15), (true, 1i64 << 19)] {
        for &angle in &angles {
            for &flip in &[false, true] {
                for trial in 0..100 {
                    // vals = [ta, tb], each 8 lanes across the full signed range
                    // [-bound, bound); trial 0 plants the exact corners in the first
                    // two lanes.
                    let mut vals = [[0i64; 8]; 2];
                    for (vi, v) in vals.iter_mut().enumerate() {
                        for (lane, x) in v.iter_mut().enumerate() {
                            *x = if trial == 0 && lane < 2 {
                                if (lane + vi) % 2 == 0 {
                                    -bound
                                } else {
                                    bound - 1
                                }
                            } else {
                                (xorshift32(&mut seed) as i64 & (2 * bound - 1)) - bound
                            };
                        }
                    }

                    // Scalar i64 reference (`transform.rs::b_op`, spec §8.7.1.1's B):
                    // x = ta*cos - tb*sin, y = ta*sin + tb*cos, round2(., 14), flip-swap.
                    let cos = crate::transform::cos64(angle);
                    let sin = crate::transform::sin64(angle);
                    let round2_14_i64 = |x: i64| (x + (1i64 << 13)) >> 14;
                    let mut expected = [[0i64; 8]; 2];
                    for lane in 0..8 {
                        let (ta, tb) = (vals[0][lane], vals[1][lane]);
                        let (mut na, mut nb) = (
                            round2_14_i64(ta * cos - tb * sin),
                            round2_14_i64(ta * sin + tb * cos),
                        );
                        if flip {
                            std::mem::swap(&mut na, &mut nb);
                        }
                        expected[0][lane] = na;
                        expected[1][lane] = nb;
                    }

                    let load = |v: &[i64; 8]| {
                        let lanes: [i32; 8] = std::array::from_fn(|i| v[i] as i32);
                        unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) }
                    };
                    let mut t = [load(&vals[0]), load(&vals[1])];
                    unsafe {
                        if hbd {
                            b_op_simd_hbd(&mut t, 0, 1, angle, flip);
                        } else {
                            b_op_simd(&mut t, 0, 1, angle, flip);
                        }
                    }
                    for (k, &tk) in t.iter().enumerate() {
                        let mut lanes = [0i32; 8];
                        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk) };
                        for (lane, &got) in lanes.iter().enumerate() {
                            assert_eq!(
                                got as i64, expected[k][lane],
                                "hbd={hbd} angle={angle} flip={flip} trial={trial}: \
                                 t[{k}] lane {lane}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The i64-lane 1D inverse ADST (`iadst_i64`, 4 rows per vector) must exactly reproduce the
/// scalar `transform::iadst4/8/16` on each lane, for every size n = 2..=4, at the FULL spec
/// §8.7.1.1 input bound for BOTH high bit depths (±2^(7+BitDepth): ±2^17 at 10-bit, ±2^19 at
/// 12-bit). Unlike the 8-bit i32-lane kernels, full-range testing of ALL sizes is sound here:
/// the kernel is the scalar's i64 arithmetic op for op (exact-mod-2^64 multiply, emulated
/// arithmetic-shift round2), so it matches wherever the scalar i64 does not overflow -- and
/// even adversarial full-range inputs compound to < ~2^46 (see the i64 section comment in
/// `simd/transform.rs`). Signed-bound corner vectors are planted deterministically alongside
/// the random draws (the random draw alone almost never lands on the corners). Self-check
/// that these magnitudes discriminate: the same inputs through the 8-bit i32-LANE network
/// (`iadst_simd`) must diverge from the scalar in EVERY (bit depth, size) cell (the `S`
/// array needs `24 + BitDepth` bits -- beyond i32 lane storage), the `mullo_diverged`
/// precedent from the HBD DCT test.
#[test]
fn iadst_i64_1d_matches_scalar_at_full_range() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x1D64_AD57u32;
    for &bit_depth in &[10u32, 12] {
        let mask = (1u32 << (bit_depth + 8)) - 1;
        let half = 1i64 << (bit_depth + 7);
        for n in 2..=4u32 {
            let n0 = 1usize << n;
            // Per-(bit_depth, n) so every cell individually proves the i64 lanes are needed.
            let mut i32_lanes_diverged = false;
            // Trials 0/1 plant the signed-bound corners (all -half; alternating -half and
            // half-1); the random draw reaches -half with probability 2^-(bit_depth+8) per
            // element, so corners are otherwise effectively untested.
            for trial in 0..202 {
                let mut rows = [[0i64; 16]; 4];
                for row in rows.iter_mut() {
                    for (i, v) in row[..n0].iter_mut().enumerate() {
                        *v = match trial {
                            0 => -half,
                            1 => {
                                if i % 2 == 0 {
                                    -half
                                } else {
                                    half - 1
                                }
                            }
                            _ => (xorshift32(&mut seed) & mask) as i64 - half,
                        };
                    }
                }
                let mut scalar = rows;
                for row in scalar.iter_mut() {
                    match n {
                        2 => crate::transform::iadst4((&mut row[..4]).try_into().unwrap()),
                        3 => crate::transform::iadst8((&mut row[..8]).try_into().unwrap()),
                        _ => crate::transform::iadst16((&mut row[..16]).try_into().unwrap()),
                    }
                }
                let mut t = [unsafe { _mm256_setzero_si256() }; 16];
                let mut t32 = [unsafe { _mm256_setzero_si256() }; 16];
                for (k, (tk, tk32)) in t[..n0].iter_mut().zip(t32[..n0].iter_mut()).enumerate() {
                    let lanes: [i64; 4] = std::array::from_fn(|r| rows[r][k]);
                    *tk = unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) };
                    let lanes32: [i32; 8] =
                        std::array::from_fn(|r| if r < 4 { rows[r][k] as i32 } else { 0 });
                    *tk32 = unsafe { _mm256_loadu_si256(lanes32.as_ptr() as *const __m256i) };
                }
                unsafe {
                    iadst_i64(&mut t, n);
                    iadst_simd(&mut t32, n);
                }
                for (k, (&tk, &tk32)) in t[..n0].iter().zip(t32[..n0].iter()).enumerate() {
                    let mut lanes = [0i64; 4];
                    let mut lanes32 = [0i32; 8];
                    unsafe {
                        _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk);
                        _mm256_storeu_si256(lanes32.as_mut_ptr() as *mut __m256i, tk32);
                    }
                    for (r, &lane) in lanes.iter().enumerate() {
                        assert_eq!(
                            lane, scalar[r][k],
                            "bd={bit_depth} n={n}: row {r}, elem {k}"
                        );
                        i32_lanes_diverged |= lanes32[r] as i64 != scalar[r][k];
                    }
                }
            }
            assert!(
                i32_lanes_diverged,
                "bd={bit_depth} n={n}: test inputs never overflowed the i32-lane ADST \
                 network -- magnitudes too small to prove the i64 lanes are needed"
            );
        }
    }
}

/// The i64-lane 1D inverse DCT (`idct_i64` -- the mixed types' DCT axis) must exactly
/// reproduce the scalar `transform::idct_permute` + `transform::idct` on each lane, at the
/// full ±2^(7+BitDepth) input bound for 10-bit AND 12-bit. Covers n = 2..=5 (the network
/// mirrors `idct_simd` verbatim, though the i64 driver only dispatches n <= 4). No
/// i32-divergence self-check here: the i32-LANE HBD DCT is exact at these magnitudes by
/// design (that is the established `b_op_simd_hbd` kernel); the i64 DCT exists for the
/// driver's uniform lane layout, not because i32 lanes fail on the DCT axis.
#[test]
fn idct_i64_1d_matches_scalar_at_full_range() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x1D64_DC70u32;
    for &bit_depth in &[10u32, 12] {
        let mask = (1u32 << (bit_depth + 8)) - 1;
        let half = 1i64 << (bit_depth + 7);
        for n in 2..=5u32 {
            let n0 = 1usize << n;
            // Trials 0/1 plant the signed-bound corners, as in the ADST 1D test above.
            for trial in 0..202 {
                let mut rows = [[0i64; 32]; 4];
                for row in rows.iter_mut() {
                    for (i, v) in row[..n0].iter_mut().enumerate() {
                        *v = match trial {
                            0 => -half,
                            1 => {
                                if i % 2 == 0 {
                                    -half
                                } else {
                                    half - 1
                                }
                            }
                            _ => (xorshift32(&mut seed) & mask) as i64 - half,
                        };
                    }
                }
                let mut scalar = rows;
                for row in scalar.iter_mut() {
                    crate::transform::idct_permute(&mut row[..n0], n);
                    crate::transform::idct(&mut row[..n0], n);
                }
                let mut t = [unsafe { _mm256_setzero_si256() }; 32];
                for (k, tk) in t[..n0].iter_mut().enumerate() {
                    let lanes: [i64; 4] = std::array::from_fn(|r| rows[r][k]);
                    *tk = unsafe { _mm256_loadu_si256(lanes.as_ptr() as *const __m256i) };
                }
                unsafe {
                    idct_permute_simd(&mut t, n);
                    idct_i64(&mut t, n);
                }
                for (k, &tk) in t[..n0].iter().enumerate() {
                    let mut lanes = [0i64; 4];
                    unsafe { _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, tk) };
                    for (r, &lane) in lanes.iter().enumerate() {
                        assert_eq!(
                            lane, scalar[r][k],
                            "bd={bit_depth} n={n}: row {r}, elem {k}"
                        );
                    }
                }
            }
        }
    }
}

/// The fused HBD `inverse_transform_adst_reconstruct_hbd_avx2` (i64-lane 2D transform +
/// residual add + `(1 << bit_depth) - 1` clip) must exactly reproduce the scalar
/// `inverse_transform_block` + `clamp(pred + residual, 0, max_val)` reconstruction, for every
/// ADST-containing tx type x every ADST size (4/8/16) x 10-bit AND 12-bit, with dequant
/// inputs at the FULL spec §8.7.1.1 stored-value bound ±2^(7+BitDepth) (the scalar i64 path
/// is the oracle; the i64 lanes make full-range 2D testing sound -- see the i64 section
/// comment -- including the final i32 narrowing, whose inputs stay far inside i32 even at
/// these magnitudes) and predictions across the full sample range. Two placements per the
/// repo convention: (0, 0) in a tight stride-n0 plane, and a nonzero origin inside a wider
/// plane (the offset/stride seam of `reconstruct_add_clip` -- what the tile-parallel column
/// strips feed it). Whole-plane equality also proves no out-of-block writes.
#[test]
fn inverse_transform_adst_reconstruct_hbd_simd_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0xAD57_2BD1u32;
    for &bit_depth in &[10u8, 12] {
        let max_val = (1i64 << bit_depth) - 1;
        let mask = (1u32 << (bit_depth as u32 + 8)) - 1;
        let half = 1i64 << (bit_depth as u32 + 7);
        for &tx_type in &[
            crate::transform::TxType::AdstDct,
            crate::transform::TxType::DctAdst,
            crate::transform::TxType::AdstAdst,
        ] {
            for n in 2..=4u32 {
                let n0 = 1usize << n;
                let count = n0 * n0;
                // Trials 0/1 plant the signed-bound corners, as in the 1D tests above.
                for trial in 0..102 {
                    let mut dq = vec![0i64; count];
                    for (i, v) in dq.iter_mut().enumerate() {
                        *v = match trial {
                            0 => -half,
                            1 => {
                                if i % 2 == 0 {
                                    -half
                                } else {
                                    half - 1
                                }
                            }
                            _ => (xorshift32(&mut seed) & mask) as i64 - half,
                        };
                    }
                    let mut dq_s = dq.clone();
                    crate::transform::inverse_transform_block(&mut dq_s, n, tx_type, false);
                    for &(start_x, start_y, plane_width) in &[(0usize, 0usize, n0), (4, 2, n0 + 8)]
                    {
                        let pred: Vec<u16> = (0..plane_width * (start_y + n0))
                            .map(|_| (xorshift32(&mut seed) & (max_val as u32)) as u16)
                            .collect();
                        let mut plane_scalar = pred.clone();
                        for i in 0..n0 {
                            for j in 0..n0 {
                                let idx = (start_y + i) * plane_width + start_x + j;
                                let old = plane_scalar[idx] as i64;
                                plane_scalar[idx] =
                                    (old + dq_s[i * n0 + j]).clamp(0, max_val) as u16;
                            }
                        }

                        let mut plane_simd = pred.clone();
                        unsafe {
                            inverse_transform_adst_reconstruct_hbd_avx2(
                                &mut plane_simd,
                                plane_width,
                                start_x,
                                start_y,
                                &dq,
                                n,
                                tx_type,
                                bit_depth,
                            );
                        }
                        assert_eq!(
                            plane_scalar, plane_simd,
                            "bit_depth={bit_depth} tx_type={tx_type:?} n={n} at \
                             ({start_x},{start_y}) stride {plane_width}: fused HBD ADST SIMD \
                             != scalar"
                        );
                    }
                }
            }
        }
    }
}

/// The fused `inverse_transform_adst_reconstruct_avx2` (2D transform + residual add + 8-bit
/// clip) must exactly reproduce the scalar `inverse_transform_block` + `clip(pred +
/// residual, 0, 255)`, for every ADST-containing tx type and every ADST size (4/8/16).
/// n == 2 runs dequant magnitudes of ±8192 -- NOT the conformant input ceiling (which is
/// ±2^15, covered per-pass by the full-range 1D test `iadst_simd_1d_matches_scalar`), but
/// the largest range for which the 4x4 2D composition is arithmetically i32-safe without
/// invoking any conformance bound between the passes (worst chain 43801 * (2.73 * 8192)
/// < 2^31). n == 3/4 use ±32 (like the DCT 2D test: small enough that no intermediate can
/// overflow i32 even with adversarial -- not-conformant -- magnitude compounding across
/// the two passes; the conformant-magnitude proof is the official sweep).
#[test]
fn inverse_transform_adst_reconstruct_simd_matches_scalar() {
    if !avx2_enabled() {
        return;
    }
    let mut seed = 0x5EED_AD57u32;
    for &tx_type in &[
        crate::transform::TxType::AdstDct,
        crate::transform::TxType::DctAdst,
        crate::transform::TxType::AdstAdst,
    ] {
        for n in 2..=4u32 {
            let n0 = 1usize << n;
            let count = n0 * n0;
            let span: u32 = if n == 2 { 16384 } else { 64 }; // dequant in ±8192 / ±32
            for _ in 0..100 {
                let mut dq = vec![0i64; count];
                for v in dq.iter_mut() {
                    *v = (xorshift32(&mut seed) % span) as i64 - (span / 2) as i64;
                }
                let mut dq_s = dq.clone();
                crate::transform::inverse_transform_block(&mut dq_s, n, tx_type, false);
                // Two placements (see the 8-bit DCT test): (0, 0)/stride n0, plus a
                // nonzero origin in a wider plane covering `reconstruct_add_clip`'s
                // offset/stride indexing. Whole-plane equality also proves no
                // out-of-block writes.
                for &(start_x, start_y, plane_width) in &[(0usize, 0usize, n0), (4, 2, n0 + 8)] {
                    let pred: Vec<u16> = (0..plane_width * (start_y + n0))
                        .map(|_| (xorshift32(&mut seed) & 0xFF) as u16)
                        .collect();
                    let mut plane_scalar = pred.clone();
                    for i in 0..n0 {
                        for j in 0..n0 {
                            let idx = (start_y + i) * plane_width + start_x + j;
                            let old = plane_scalar[idx] as i64;
                            plane_scalar[idx] = (old + dq_s[i * n0 + j]).clamp(0, 255) as u16;
                        }
                    }

                    let mut plane_simd = pred.clone();
                    unsafe {
                        inverse_transform_adst_reconstruct_avx2(
                            &mut plane_simd,
                            plane_width,
                            start_x,
                            start_y,
                            &dq,
                            n,
                            tx_type,
                        );
                    }
                    assert_eq!(
                        plane_scalar, plane_simd,
                        "tx_type={tx_type:?} n={n} at ({start_x},{start_y}) stride \
                         {plane_width}: fused ADST SIMD != scalar"
                    );
                }
            }
        }
    }
}
