//! AVX2 SIMD inverse transforms (DCT and ADST networks, spec §8.7) fused with the
//! reconstruction add+clip; `tile/residual.rs` owns the dispatch and the scalar fallback.
//! The section comments below carry the load-bearing overflow-safety arguments (i32 lane
//! storage vs. product width per bit depth) -- read them before touching any kernel.

use crate::transform::TxType;
// VENDOR(uefi-player): core path works in both std and no_std builds.
use core::arch::x86_64::*;

// ===========================================================================================
// SIMD inverse DCT (SIMD wave 4b; 10/12-bit added later). The scalar `transform::idct` is a
// recursive butterfly network on an i64 array; the functions below mirror it VERBATIM on i32
// vectors, each element vector holding that transform element across 8 rows (8 lanes) -- so 8
// independent 1D row/column IDCTs run in parallel. Lane STORAGE is i32 at every bit depth:
// spec §8.7.1.1 bounds every value stored into T to signed `8 + BitDepth` bits (<= 20 at
// 12-bit), so loads/stores, transposes, `h_op` sums and the final `round2` all fit i32. Only
// the `t * cos64` PRODUCTS inside `b_op` differ by depth: at bit_depth == 8 they fit i32
// (16b * 15b), so `_mm256_mullo_epi32` is exact (`b_op_simd`); at 10/12-bit they reach ~2^33
// and would wrap, so the HBD network (`HBD = true`) computes them with 32x32 -> 64-bit
// widening multiplies and rounds in i64 (`b_op_simd_hbd`). DCT_DCT (both axes DCT) is
// vectorized at every depth; the ADST-containing types (ADST_DCT / DCT_ADST / ADST_ADST, sizes
// 4/8/16 -- 32x32 is DCT-only) take these i32-lane networks at 8-bit and the i64-lane networks
// (see the i64 section below) at 10/12-bit. Only WHT (lossless) stays scalar.
// ===========================================================================================

/// `round2(x, 14)` (`transform.rs::round2`) lane-wise; the b_op products fit i32 (see the module
/// comment above), so `(x + 2^13) >> 14` arithmetic-shifted matches the scalar i64 round2.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn round2_14(x: __m256i) -> __m256i {
    _mm256_srai_epi32(_mm256_add_epi32(x, _mm256_set1_epi32(1 << 13)), 14)
}

/// Vector `B(a, b, angle, flip)` butterfly (`transform.rs::b_op`) on 8 rows at once.
#[inline]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn b_op_simd(t: &mut [__m256i], a: usize, b: usize, angle: i32, flip: bool) {
    let cos = _mm256_set1_epi32(crate::transform::cos64(angle) as i32);
    let sin = _mm256_set1_epi32(crate::transform::sin64(angle) as i32);
    let ta = t[a];
    let tb = t[b];
    // x = ta*cos - tb*sin ; y = ta*sin + tb*cos ; each product fits i32 (mullo is exact there).
    let x = _mm256_sub_epi32(_mm256_mullo_epi32(ta, cos), _mm256_mullo_epi32(tb, sin));
    let y = _mm256_add_epi32(_mm256_mullo_epi32(ta, sin), _mm256_mullo_epi32(tb, cos));
    let na = round2_14(x);
    let nb = round2_14(y);
    if flip {
        t[a] = nb;
        t[b] = na;
    } else {
        t[a] = na;
        t[b] = nb;
    }
}

/// High-bit-depth `B(a, b, angle, flip)`: identical network arithmetic to [`b_op_simd`], but
/// the `t * cos64` products are computed with 32x32 -> 64-bit widening multiplies
/// (`_mm256_mul_epi32`) and rounded in i64, because at 10/12-bit a stored T value spans up to
/// signed `8 + BitDepth <= 20` bits (spec §8.7.1.1) and `t * cos64` (20b * 15b) would wrap
/// `_mm256_mullo_epi32`. Overflow-safe by that same bound: |t| < 2^19 and |cos64| <= 2^14, so
/// |ta*cos - tb*sin| + 2^13 < 2^35 fits i64 exactly. The `>> 14` uses a LOGICAL 64-bit shift
/// (AVX2 has no 64-bit arithmetic shift): logical and arithmetic shifts differ only in the top
/// 14 bits of the 64-bit result, and only the low 32 bits are kept -- exact whenever the true
/// `round2` result fits i32, which §8.7.1.1 guarantees (it is a stored T value).
#[inline]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn b_op_simd_hbd(t: &mut [__m256i], a: usize, b: usize, angle: i32, flip: bool) {
    let cos = _mm256_set1_epi32(crate::transform::cos64(angle) as i32);
    let sin = _mm256_set1_epi32(crate::transform::sin64(angle) as i32);
    let ta = t[a];
    let tb = t[b];
    // `_mm256_mul_epi32` multiplies the (sign-extended) low 32 bits of each 64-bit lane, i.e.
    // i32 lanes 0/2/4/6. The logical 64-bit shift moves lanes 1/3/5/7 into those positions
    // (the zeroed high halves are ignored by the multiply).
    let ta_odd = _mm256_srli_epi64(ta, 32);
    let tb_odd = _mm256_srli_epi64(tb, 32);
    let round = _mm256_set1_epi64x(1 << 13);
    // x = ta*cos - tb*sin ; y = ta*sin + tb*cos, each + 2^13, in exact i64 (per even/odd half).
    let x_even = _mm256_add_epi64(
        _mm256_sub_epi64(_mm256_mul_epi32(ta, cos), _mm256_mul_epi32(tb, sin)),
        round,
    );
    let x_odd = _mm256_add_epi64(
        _mm256_sub_epi64(_mm256_mul_epi32(ta_odd, cos), _mm256_mul_epi32(tb_odd, sin)),
        round,
    );
    let y_even = _mm256_add_epi64(
        _mm256_add_epi64(_mm256_mul_epi32(ta, sin), _mm256_mul_epi32(tb, cos)),
        round,
    );
    let y_odd = _mm256_add_epi64(
        _mm256_add_epi64(_mm256_mul_epi32(ta_odd, sin), _mm256_mul_epi32(tb_odd, cos)),
        round,
    );
    // `round2(., 14)` per 64-bit lane (logical shift, low 32 bits exact -- see above), then
    // re-interleave the even/odd halves back into 8 i32 lanes.
    let combine = |even: __m256i, odd: __m256i| {
        _mm256_blend_epi32(
            _mm256_srli_epi64(even, 14),
            _mm256_slli_epi64(_mm256_srli_epi64(odd, 14), 32),
            0b1010_1010,
        )
    };
    let na = combine(x_even, x_odd);
    let nb = combine(y_even, y_odd);
    if flip {
        t[a] = nb;
        t[b] = na;
    } else {
        t[a] = na;
        t[b] = nb;
    }
}

/// Selects the depth-correct butterfly: `b_op_simd` (i32 products, bit_depth == 8) or
/// `b_op_simd_hbd` (widened i64 products, 10/12-bit). Monomorphized away -- the 8-bit network
/// is unchanged.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn b_op_simd_depth<const HBD: bool>(
    t: &mut [__m256i],
    a: usize,
    b: usize,
    angle: i32,
    flip: bool,
) {
    if HBD {
        b_op_simd_hbd(t, a, b, angle, flip);
    } else {
        b_op_simd(t, a, b, angle, flip);
    }
}

/// Vector `H(a, b, flip)` Hadamard rotation (`transform.rs::h_op`) on 8 rows.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn h_op_simd(t: &mut [__m256i], a: usize, b: usize, flip: bool) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a];
    let y = t[b];
    t[a] = _mm256_add_epi32(x, y);
    t[b] = _mm256_sub_epi32(x, y);
}

/// Vector `idct_permute` (`transform.rs::idct_permute`): bit-reversal reorder of the element
/// vectors (no arithmetic, so trivially the same at i32).
#[inline]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn idct_permute_simd(t: &mut [__m256i], n: u32) {
    let n0 = 1usize << n;
    let mut copy_t = [_mm256_setzero_si256(); 32];
    copy_t[..n0].copy_from_slice(&t[..n0]);
    for (i, dst) in t[..n0].iter_mut().enumerate() {
        *dst = copy_t[crate::transform::brev(n, i)];
    }
}

/// Vector inverse-DCT butterfly network -- a verbatim mirror of `transform.rs::idct` (spec
/// §8.7.1.3), operating on 8-row i32 element vectors. Assumes `idct_permute_simd` ran first.
/// `HBD` selects the butterfly's multiply width (see [`b_op_simd_depth`]); the network
/// structure is identical either way.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn idct_simd<const HBD: bool>(t: &mut [__m256i], n: u32) {
    let n0 = 1i64 << n;
    let n1 = 1i64 << (n - 1);
    let n2 = 1i64 << (n - 2);

    if n == 2 {
        b_op_simd_depth::<HBD>(t, 0, 1, 16, true);
    } else {
        idct_simd::<HBD>(t, n - 1);
    }

    for i in 0..n2 {
        let a = (n1 + i) as usize;
        let b = (n0 - 1 - i) as usize;
        let angle = 32 - crate::transform::brev(5, a) as i32;
        b_op_simd_depth::<HBD>(t, a, b, angle, false);
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            for j in 0..2i64 {
                let a = (n1 + 4 * i + 2 * j) as usize;
                let b = (n1 + 1 + 4 * i + 2 * j) as usize;
                h_op_simd(t, a, b, j == 1);
            }
        }
    }

    if n == 5 {
        for i in 0..2i64 {
            for j in 0..2i64 {
                let a = (n0 - n as i64 + 3 - n2 * j - 4 * i) as usize;
                let b = (n1 + n as i64 - 4 + n2 * j + 4 * i) as usize;
                let angle = 28 - 16 * i as i32 + 56 * j as i32;
                b_op_simd_depth::<HBD>(t, a, b, angle, true);
            }
        }
        let n3 = 1i64 << (n - 3);
        for i in 0..2i64 {
            for j in 0..4i64 {
                let a = (n1 + n3 * j + i) as usize;
                let b = (n1 + n2 - 5 + n3 * j - i) as usize;
                h_op_simd(t, a, b, (j & 1) == 1);
            }
        }
    }

    if n >= 4 {
        let imax_a: i64 = if n == 5 { 1 } else { 0 };
        for i in 0..=imax_a {
            for j in 0..2i64 {
                let a = (n0 - n as i64 + 2 - i - n2 * j) as usize;
                let b = (n1 + n as i64 - 3 + i + n2 * j) as usize;
                let angle = 24 + 48 * j as i32;
                b_op_simd_depth::<HBD>(t, a, b, angle, true);
            }
        }
        let imax_b: i64 = 2 * n as i64 - 7;
        for j in 0..2i64 {
            for i in 0..=imax_b {
                let a = (n1 + n2 * j + i) as usize;
                let b = (n1 + n2 - 1 + n2 * j - i) as usize;
                h_op_simd(t, a, b, (j & 1) == 1);
            }
        }
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            let a = (n0 - n3 - 1 - i) as usize;
            let b = (n1 + n3 + i) as usize;
            b_op_simd_depth::<HBD>(t, a, b, 16, true);
        }
    }

    for i in 0..n1 {
        let a = i as usize;
        let b = (n0 - 1 - i) as usize;
        h_op_simd(t, a, b, false);
    }
}

// -------------------------------------------------------------------------------------------
// SIMD inverse ADST, 8-bit ONLY. The scalar `transform::iadst4/8/16` networks are mirrored
// verbatim on i32 8-lane element vectors, exactly like the DCT above. Overflow safety at
// bit_depth == 8 (from spec §8.7.1.1 / §8.7.2 conformance: every stored T value fits signed
// `8 + BitDepth == 16` bits, so |T| <= 2^15; |cos64| <= 2^14; max |cos64|+|sin64| == 23170):
//   - every `SB` product `T * cos64` is <= 2^29, so `_mm256_mullo_epi32` is exact;
//   - every unrounded `S` value is <= 2^15 * 23170 < 2^30, inside the i32 lanes;
//   - every `SH` sum `S[a] +/- S[b]` (+ the 2^13 round) is < 2^31;
//   - the iadst4 chains peak at (SINPI_1+SINPI_2+SINPI_3+SINPI_4) * 2^15 = 43801 * 2^15 < 2^31
//     (`SINPI_1_9 + SINPI_2_9 == SINPI_4_9` exactly, which bounds the `x0 + x1` intermediate),
//     and its largest product `SINPI_3_9 * (T0 - T2 + T3)` is <= 13377 * 3 * 2^15 < 2^31;
//   - the networks' `B`/`H` ops are bounded as in the DCT case (`b_op_simd`'s own claim).
// At 10/12-bit the spec's bound on the `S` array is `24 + BitDepth` (up to 36) bits -- beyond
// i32 LANE STORAGE, not merely the products -- so the DCT's products-only i64 widening does not
// carry over; the dispatch (`tile/residual.rs`) gates these kernels on bit_depth == 8 and
// routes 10/12-bit ADST to the i64-lane networks in the next section.
// -------------------------------------------------------------------------------------------

/// Vector `SB(a, b, angle, flip)` (`transform.rs::sb_op`, spec §8.7.1.1) on 8 rows: the
/// butterfly rotation into the high-precision `S` array, unrounded. 8-bit only -- see the
/// section comment for why every product and `S` value fits i32 there.
#[inline]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn sb_op_simd(
    s: &mut [__m256i],
    t: &[__m256i],
    a: usize,
    b: usize,
    angle: i32,
    flip: bool,
) {
    let cos = _mm256_set1_epi32(crate::transform::cos64(angle) as i32);
    let sin = _mm256_set1_epi32(crate::transform::sin64(angle) as i32);
    let ta = t[a];
    let tb = t[b];
    let sa = _mm256_sub_epi32(_mm256_mullo_epi32(ta, cos), _mm256_mullo_epi32(tb, sin));
    let sb = _mm256_add_epi32(_mm256_mullo_epi32(ta, sin), _mm256_mullo_epi32(tb, cos));
    if flip {
        s[a] = sb;
        s[b] = sa;
    } else {
        s[a] = sa;
        s[b] = sb;
    }
}

/// Vector `SH(a, b)` (`transform.rs::sh_op`, spec §8.7.1.1) on 8 rows: Hadamard + `round2(14)`
/// from `S` back into `T`. The `S[a] +/- S[b]` sums fit i32 at 8-bit (see the section comment).
#[inline]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn sh_op_simd(t: &mut [__m256i], s: &[__m256i], a: usize, b: usize) {
    t[a] = round2_14(_mm256_add_epi32(s[a], s[b]));
    t[b] = round2_14(_mm256_sub_epi32(s[a], s[b]));
}

/// Vector `adst_input_permute` (`transform.rs`, spec §8.7.1.4): pure reorder, no arithmetic.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn adst_input_permute_simd(t: &mut [__m256i], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let mut copy_t = [_mm256_setzero_si256(); 16];
    copy_t[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n1 {
        t[2 * i] = copy_t[n0 - 1 - 2 * i];
        t[2 * i + 1] = copy_t[2 * i];
    }
}

/// Vector `adst_output_permute` (`transform.rs`, spec §8.7.1.5): pure reorder, no arithmetic
/// (`n` is 3 or 4).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn adst_output_permute_simd(t: &mut [__m256i], n: u32) {
    if n == 4 {
        let mut copy_t = [_mm256_setzero_si256(); 16];
        copy_t.copy_from_slice(&t[..16]);
        for a in 0..2usize {
            for b in 0..2usize {
                for c in 0..2usize {
                    for d in 0..2usize {
                        let dst = 8 * a + 4 * b + 2 * c + d;
                        let src = 8 * (d ^ c) + 4 * (c ^ b) + 2 * (b ^ a) + a;
                        t[dst] = copy_t[src];
                    }
                }
            }
        }
    } else {
        debug_assert_eq!(n, 3);
        let mut copy_t = [_mm256_setzero_si256(); 8];
        copy_t.copy_from_slice(&t[..8]);
        for a in 0..2usize {
            for b in 0..2usize {
                for c in 0..2usize {
                    let dst = 4 * a + 2 * b + c;
                    let src = 4 * (c ^ b) + 2 * (b ^ a) + a;
                    t[dst] = copy_t[src];
                }
            }
        }
    }
}

/// Vector inverse ADST4 (`transform.rs::iadst4_impl`, spec §8.7.1.6) on 8 rows. Same statement
/// order and evaluation order as the scalar (`v` is `(T0 - T2) + T3`, `o3` is `(x0 + x1) - x3`).
/// i32 is exact here because NO intermediate wraps at all: the section comment's chain bounds
/// keep every product, partial sum, and round2 input below 2^31 in magnitude at 8-bit. (A
/// weaker "wrapped adds cancel if the final value fits" argument would NOT suffice -- the
/// chains end in `round2` shifts, and a shift of a wrapped sum is not the shift of the true
/// sum -- so keep the no-wrap bound intact when touching this network.)
#[target_feature(enable = "avx2")]
unsafe fn iadst4_simd(t: &mut [__m256i]) {
    use crate::transform::{SINPI_1_9, SINPI_2_9, SINPI_3_9, SINPI_4_9};
    let mul = |v: __m256i, c: i64| _mm256_mullo_epi32(v, _mm256_set1_epi32(c as i32));
    let s0 = mul(t[0], SINPI_1_9);
    let s1 = mul(t[0], SINPI_2_9);
    let s2 = mul(t[1], SINPI_3_9);
    let s3 = mul(t[2], SINPI_4_9);
    let s4 = mul(t[2], SINPI_1_9);
    let s5 = mul(t[3], SINPI_2_9);
    let s6 = mul(t[3], SINPI_4_9);
    let v = _mm256_add_epi32(_mm256_sub_epi32(t[0], t[2]), t[3]);
    let s7 = mul(v, SINPI_3_9);
    let x0 = _mm256_add_epi32(_mm256_add_epi32(s0, s3), s5);
    let x1 = _mm256_sub_epi32(_mm256_sub_epi32(s1, s4), s6);
    let x2 = s7;
    let x3 = s2;
    let o0 = _mm256_add_epi32(x0, x3);
    let o1 = _mm256_add_epi32(x1, x3);
    let o2 = x2;
    let o3 = _mm256_sub_epi32(_mm256_add_epi32(x0, x1), x3);
    t[0] = round2_14(o0);
    t[1] = round2_14(o1);
    t[2] = round2_14(o2);
    t[3] = round2_14(o3);
}

/// Vector inverse ADST8 (`transform.rs::iadst8_impl`, spec §8.7.1.7) on 8 rows -- a verbatim
/// mirror of the scalar op sequence.
#[target_feature(enable = "avx2")]
unsafe fn iadst8_simd(t: &mut [__m256i]) {
    adst_input_permute_simd(t, 3);
    let mut s = [_mm256_setzero_si256(); 8];

    for i in 0..4usize {
        sb_op_simd(&mut s, t, 2 * i, 1 + 2 * i, 30 - 8 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_simd(t, &s, i, 4 + i);
    }
    for i in 0..2usize {
        sb_op_simd(&mut s, t, 4 + 3 * i, 5 + i, 24 - 16 * i as i32, true);
    }
    for i in 0..2usize {
        sh_op_simd(t, &s, 4 + i, 6 + i);
    }
    for i in 0..2usize {
        h_op_simd(t, i, 2 + i, false);
    }
    for i in 0..2usize {
        b_op_simd(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }

    adst_output_permute_simd(t, 3);

    let zero = _mm256_setzero_si256();
    for i in 0..4usize {
        t[1 + 2 * i] = _mm256_sub_epi32(zero, t[1 + 2 * i]);
    }
}

/// Vector inverse ADST16 (`transform.rs::iadst16_impl`, spec §8.7.1.8) on 8 rows -- a verbatim
/// mirror of the scalar op sequence.
#[target_feature(enable = "avx2")]
unsafe fn iadst16_simd(t: &mut [__m256i]) {
    adst_input_permute_simd(t, 4);
    let mut s = [_mm256_setzero_si256(); 16];

    for i in 0..8usize {
        sb_op_simd(&mut s, t, 2 * i, 1 + 2 * i, 31 - 4 * i as i32, true);
    }
    for i in 0..8usize {
        sh_op_simd(t, &s, i, 8 + i);
    }
    for i in 0..4usize {
        sb_op_simd(&mut s, t, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_simd(t, &s, 8 + i, 12 + i);
    }
    for i in 0..4usize {
        h_op_simd(t, i, 4 + i, false);
    }
    for i in 0..2usize {
        for j in 0..2usize {
            sb_op_simd(
                &mut s,
                t,
                4 + 8 * i + 3 * j,
                5 + 8 * i + j,
                24 - 16 * j as i32,
                true,
            );
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            sh_op_simd(t, &s, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            h_op_simd(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            let angle = 48 + 64 * (i ^ j) as i32;
            b_op_simd(t, 2 + 4 * j + 8 * i, 3 + 4 * j + 8 * i, angle, false);
        }
    }

    adst_output_permute_simd(t, 4);

    let zero = _mm256_setzero_si256();
    for i in 0..2usize {
        for j in 0..2usize {
            t[1 + 12 * j + 2 * i] = _mm256_sub_epi32(zero, t[1 + 12 * j + 2 * i]);
        }
    }
}

/// Vector `iadst(t, n)` (`transform.rs::iadst`, spec §8.7.1.9): ADST4/8/16 by size. The input/
/// output permutes are inside the size-specific bodies, mirroring the scalar structure.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn iadst_simd(t: &mut [__m256i], n: u32) {
    match n {
        2 => iadst4_simd(t),
        3 => iadst8_simd(t),
        4 => iadst16_simd(t),
        _ => unreachable!("iadst_simd only supports n = 2..=4"),
    }
}

// -------------------------------------------------------------------------------------------
// SIMD inverse ADST + mixed-axis DCT at 10/12-bit: i64 LANES (4 per `__m256i`). The spec
// §8.7.1.1 bound on the ADST's unrounded `S` array is signed `24 + BitDepth` bits -- up to 36
// at 12-bit, beyond i32 LANE STORAGE (not merely the products), so the HBD DCT's
// i32-lanes-with-widened-products trick cannot carry over; these kernels instead mirror the
// scalar networks on i64 element vectors, 4 rows per vector. For a mixed block (AdstDct /
// DctAdst) the DCT axis ALSO runs in i64 (`idct_i64`) so the whole 2D driver is
// self-contained -- no i32/i64 intermediate-layout mixing across the transpose -- and the DCT
// axis is trivially bit-exact (the scalar `idct` is i64 arithmetic verbatim).
//
// Exactness argument: every op below is the scalar's i64 arithmetic, element for element --
//   - `mul_i64` computes each lane * constant exactly mod 2^64 (AVX2 has no 64-bit multiply;
//     the low/cross-product decomposition below is exact mod 2^64, hence exact whenever the
//     true product fits i64);
//   - `round2_i64` is `(x + 2^(n-1)) >> n` with a sign-propagating emulated 64-bit ARITHMETIC
//     shift (AVX2 has only logical 64-bit shifts) -- exact for every i64;
//   - adds/subs are exact mod 2^64, matching two's-complement scalar i64.
// So the kernels equal the scalar wherever the scalar itself does not overflow i64 -- and it
// never does: conformant streams bound stored T values to signed `8 + BitDepth` <= 20 bits and
// unrounded S values to `24 + BitDepth` <= 36 bits, so products |T * cos64| < 2^19 * 2^14 =
// 2^33 and every sum < 2^37; even the unit tests' adversarial full-range inputs (+/-2^19 at
// every array position, unreachable from a conformant stream) compound to < ~2^46 (per-stage
// L-inf gain <= ~2.83 for SB+SH / B, x2 for H). WHT (lossless) stays scalar; TX_32X32 is
// DCT-only (spec §8.7.2) and takes the HBD DCT kernel above, so this path only sees n <= 4.
// -------------------------------------------------------------------------------------------

/// Exact `lane * c` on 4 i64 lanes (low 64 bits). AVX2 has no 64-bit multiply, so decompose:
/// `a*c mod 2^64 = a_lo*c_lo + ((a_hi*c_lo + a_lo*c_hi) << 32)`, with `_lo` the unsigned low
/// dwords and `_hi` the high dwords (signedness is irrelevant mod 2^32). Exact whenever the
/// true product fits i64 -- always here (see the section comment).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn mul_i64(a: __m256i, c: i64) -> __m256i {
    let b = _mm256_set1_epi64x(c);
    let a_hi = _mm256_srli_epi64(a, 32);
    let b_hi = _mm256_srli_epi64(b, 32);
    // Even dwords: a_hi*c_lo resp. a_lo*c_hi (their odd dwords are 0 * something == 0).
    let cross = _mm256_add_epi32(_mm256_mullo_epi32(a_hi, b), _mm256_mullo_epi32(a, b_hi));
    _mm256_add_epi64(_mm256_mul_epu32(a, b), _mm256_slli_epi64(cross, 32))
}

/// `round2(x, n)` (`transform.rs::round2`) on 4 i64 lanes: add `2^(n-1)`, then an emulated
/// 64-bit ARITHMETIC right shift (logical shift + the sign bits OR-ed back into the vacated
/// top `n` bits). Exact for every i64 lane value -- no magnitude precondition.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn round2_i64(x: __m256i, n: u32) -> __m256i {
    let biased = _mm256_add_epi64(x, _mm256_set1_epi64x(1i64 << (n - 1)));
    let sign = _mm256_cmpgt_epi64(_mm256_setzero_si256(), biased);
    _mm256_or_si256(
        _mm256_srl_epi64(biased, _mm_cvtsi32_si128(n as i32)),
        _mm256_sll_epi64(sign, _mm_cvtsi32_si128(64 - n as i32)),
    )
}

/// `B(a, b, angle, flip)` (`transform.rs::b_op`) on 4 i64 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn b_op_i64(t: &mut [__m256i], a: usize, b: usize, angle: i32, flip: bool) {
    let cos = crate::transform::cos64(angle);
    let sin = crate::transform::sin64(angle);
    let ta = t[a];
    let tb = t[b];
    let x = _mm256_sub_epi64(mul_i64(ta, cos), mul_i64(tb, sin));
    let y = _mm256_add_epi64(mul_i64(ta, sin), mul_i64(tb, cos));
    let na = round2_i64(x, 14);
    let nb = round2_i64(y, 14);
    if flip {
        t[a] = nb;
        t[b] = na;
    } else {
        t[a] = na;
        t[b] = nb;
    }
}

/// `H(a, b, flip)` (`transform.rs::h_op`) on 4 i64 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn h_op_i64(t: &mut [__m256i], a: usize, b: usize, flip: bool) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a];
    let y = t[b];
    t[a] = _mm256_add_epi64(x, y);
    t[b] = _mm256_sub_epi64(x, y);
}

/// `SB(a, b, angle, flip)` (`transform.rs::sb_op`) on 4 i64 lanes: the unrounded rotation into
/// the high-precision `S` array -- the values whose `24 + BitDepth`-bit bound forces the i64
/// lanes in the first place.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn sb_op_i64(s: &mut [__m256i], t: &[__m256i], a: usize, b: usize, angle: i32, flip: bool) {
    let cos = crate::transform::cos64(angle);
    let sin = crate::transform::sin64(angle);
    let ta = t[a];
    let tb = t[b];
    let sa = _mm256_sub_epi64(mul_i64(ta, cos), mul_i64(tb, sin));
    let sb = _mm256_add_epi64(mul_i64(ta, sin), mul_i64(tb, cos));
    if flip {
        s[a] = sb;
        s[b] = sa;
    } else {
        s[a] = sa;
        s[b] = sb;
    }
}

/// `SH(a, b)` (`transform.rs::sh_op`) on 4 i64 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn sh_op_i64(t: &mut [__m256i], s: &[__m256i], a: usize, b: usize) {
    t[a] = round2_i64(_mm256_add_epi64(s[a], s[b]), 14);
    t[b] = round2_i64(_mm256_sub_epi64(s[a], s[b]), 14);
}

/// Inverse ADST4 (`transform.rs::iadst4_impl`, spec §8.7.1.6) on 4 i64 lanes -- same statement
/// and evaluation order as the scalar (`v` is `(T0 - T2) + T3`, `o3` is `(x0 + x1) - x3`).
#[target_feature(enable = "avx2")]
unsafe fn iadst4_i64(t: &mut [__m256i]) {
    use crate::transform::{SINPI_1_9, SINPI_2_9, SINPI_3_9, SINPI_4_9};
    let s0 = mul_i64(t[0], SINPI_1_9);
    let s1 = mul_i64(t[0], SINPI_2_9);
    let s2 = mul_i64(t[1], SINPI_3_9);
    let s3 = mul_i64(t[2], SINPI_4_9);
    let s4 = mul_i64(t[2], SINPI_1_9);
    let s5 = mul_i64(t[3], SINPI_2_9);
    let s6 = mul_i64(t[3], SINPI_4_9);
    let v = _mm256_add_epi64(_mm256_sub_epi64(t[0], t[2]), t[3]);
    let s7 = mul_i64(v, SINPI_3_9);
    let x0 = _mm256_add_epi64(_mm256_add_epi64(s0, s3), s5);
    let x1 = _mm256_sub_epi64(_mm256_sub_epi64(s1, s4), s6);
    let x2 = s7;
    let x3 = s2;
    let o0 = _mm256_add_epi64(x0, x3);
    let o1 = _mm256_add_epi64(x1, x3);
    let o2 = x2;
    let o3 = _mm256_sub_epi64(_mm256_add_epi64(x0, x1), x3);
    t[0] = round2_i64(o0, 14);
    t[1] = round2_i64(o1, 14);
    t[2] = round2_i64(o2, 14);
    t[3] = round2_i64(o3, 14);
}

/// Inverse ADST8 (`transform.rs::iadst8_impl`, spec §8.7.1.7) on 4 i64 lanes -- a verbatim
/// mirror of the scalar op sequence (the permutes reorder whole element vectors, so the
/// existing lane-agnostic `adst_input/output_permute_simd` are shared).
#[target_feature(enable = "avx2")]
unsafe fn iadst8_i64(t: &mut [__m256i]) {
    adst_input_permute_simd(t, 3);
    let mut s = [_mm256_setzero_si256(); 8];

    for i in 0..4usize {
        sb_op_i64(&mut s, t, 2 * i, 1 + 2 * i, 30 - 8 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_i64(t, &s, i, 4 + i);
    }
    for i in 0..2usize {
        sb_op_i64(&mut s, t, 4 + 3 * i, 5 + i, 24 - 16 * i as i32, true);
    }
    for i in 0..2usize {
        sh_op_i64(t, &s, 4 + i, 6 + i);
    }
    for i in 0..2usize {
        h_op_i64(t, i, 2 + i, false);
    }
    for i in 0..2usize {
        b_op_i64(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }

    adst_output_permute_simd(t, 3);

    let zero = _mm256_setzero_si256();
    for i in 0..4usize {
        t[1 + 2 * i] = _mm256_sub_epi64(zero, t[1 + 2 * i]);
    }
}

/// Inverse ADST16 (`transform.rs::iadst16_impl`, spec §8.7.1.8) on 4 i64 lanes -- a verbatim
/// mirror of the scalar op sequence.
#[target_feature(enable = "avx2")]
unsafe fn iadst16_i64(t: &mut [__m256i]) {
    adst_input_permute_simd(t, 4);
    let mut s = [_mm256_setzero_si256(); 16];

    for i in 0..8usize {
        sb_op_i64(&mut s, t, 2 * i, 1 + 2 * i, 31 - 4 * i as i32, true);
    }
    for i in 0..8usize {
        sh_op_i64(t, &s, i, 8 + i);
    }
    for i in 0..4usize {
        sb_op_i64(&mut s, t, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_i64(t, &s, 8 + i, 12 + i);
    }
    for i in 0..4usize {
        h_op_i64(t, i, 4 + i, false);
    }
    for i in 0..2usize {
        for j in 0..2usize {
            sb_op_i64(
                &mut s,
                t,
                4 + 8 * i + 3 * j,
                5 + 8 * i + j,
                24 - 16 * j as i32,
                true,
            );
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            sh_op_i64(t, &s, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            h_op_i64(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            let angle = 48 + 64 * (i ^ j) as i32;
            b_op_i64(t, 2 + 4 * j + 8 * i, 3 + 4 * j + 8 * i, angle, false);
        }
    }

    adst_output_permute_simd(t, 4);

    let zero = _mm256_setzero_si256();
    for i in 0..2usize {
        for j in 0..2usize {
            t[1 + 12 * j + 2 * i] = _mm256_sub_epi64(zero, t[1 + 12 * j + 2 * i]);
        }
    }
}

/// `iadst(t, n)` (`transform.rs::iadst`, spec §8.7.1.9) on 4 i64 lanes: ADST4/8/16 by size.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn iadst_i64(t: &mut [__m256i], n: u32) {
    match n {
        2 => iadst4_i64(t),
        3 => iadst8_i64(t),
        4 => iadst16_i64(t),
        _ => unreachable!("iadst_i64 only supports n = 2..=4"),
    }
}

/// Inverse-DCT butterfly network (`transform.rs::idct`, spec §8.7.1.3) on 4 i64 lanes -- the
/// mixed types' DCT axis. A verbatim structural mirror of [`idct_simd`] with the i64 ops
/// (kept separate so the proven i32-lane DCT kernels' codegen is untouched). Assumes
/// `idct_permute_simd` ran first (that permute reorders whole element vectors, so it is
/// lane-agnostic and shared).
#[target_feature(enable = "avx2")]
pub(super) unsafe fn idct_i64(t: &mut [__m256i], n: u32) {
    let n0 = 1i64 << n;
    let n1 = 1i64 << (n - 1);
    let n2 = 1i64 << (n - 2);

    if n == 2 {
        b_op_i64(t, 0, 1, 16, true);
    } else {
        idct_i64(t, n - 1);
    }

    for i in 0..n2 {
        let a = (n1 + i) as usize;
        let b = (n0 - 1 - i) as usize;
        let angle = 32 - crate::transform::brev(5, a) as i32;
        b_op_i64(t, a, b, angle, false);
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            for j in 0..2i64 {
                let a = (n1 + 4 * i + 2 * j) as usize;
                let b = (n1 + 1 + 4 * i + 2 * j) as usize;
                h_op_i64(t, a, b, j == 1);
            }
        }
    }

    if n == 5 {
        for i in 0..2i64 {
            for j in 0..2i64 {
                let a = (n0 - n as i64 + 3 - n2 * j - 4 * i) as usize;
                let b = (n1 + n as i64 - 4 + n2 * j + 4 * i) as usize;
                let angle = 28 - 16 * i as i32 + 56 * j as i32;
                b_op_i64(t, a, b, angle, true);
            }
        }
        let n3 = 1i64 << (n - 3);
        for i in 0..2i64 {
            for j in 0..4i64 {
                let a = (n1 + n3 * j + i) as usize;
                let b = (n1 + n2 - 5 + n3 * j - i) as usize;
                h_op_i64(t, a, b, (j & 1) == 1);
            }
        }
    }

    if n >= 4 {
        let imax_a: i64 = if n == 5 { 1 } else { 0 };
        for i in 0..=imax_a {
            for j in 0..2i64 {
                let a = (n0 - n as i64 + 2 - i - n2 * j) as usize;
                let b = (n1 + n as i64 - 3 + i + n2 * j) as usize;
                let angle = 24 + 48 * j as i32;
                b_op_i64(t, a, b, angle, true);
            }
        }
        let imax_b: i64 = 2 * n as i64 - 7;
        for j in 0..2i64 {
            for i in 0..=imax_b {
                let a = (n1 + n2 * j + i) as usize;
                let b = (n1 + n2 - 1 + n2 * j - i) as usize;
                h_op_i64(t, a, b, (j & 1) == 1);
            }
        }
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            let a = (n0 - n3 - 1 - i) as usize;
            let b = (n1 + n3 + i) as usize;
            b_op_i64(t, a, b, 16, true);
        }
    }

    for i in 0..n1 {
        let a = i as usize;
        let b = (n0 - 1 - i) as usize;
        h_op_i64(t, a, b, false);
    }
}

/// Transposes a 4x4 i64 matrix (`r[i]` = row i, 4 i64 lanes) so `out[j]` = column j: an
/// unpack{lo,hi}_epi64 pass within the 128-bit halves, then a `permute2x128` pass to gather
/// the halves.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn transpose4x4_i64(r: &[__m256i; 4]) -> [__m256i; 4] {
    let t0 = _mm256_unpacklo_epi64(r[0], r[1]);
    let t1 = _mm256_unpackhi_epi64(r[0], r[1]);
    let t2 = _mm256_unpacklo_epi64(r[2], r[3]);
    let t3 = _mm256_unpackhi_epi64(r[2], r[3]);
    [
        _mm256_permute2x128_si256(t0, t2, 0x20),
        _mm256_permute2x128_si256(t1, t3, 0x20),
        _mm256_permute2x128_si256(t0, t2, 0x31),
        _mm256_permute2x128_si256(t1, t3, 0x31),
    ]
}

/// One separable pass of the i64-lane 2D inverse transform (the 10/12-bit ADST-containing
/// driver): transforms every "row" of `input` (n0 x n0, row-major i64) with the inverse DCT
/// (`ADST == false`) or inverse ADST (`ADST == true`) and writes the result TRANSPOSED into
/// `output`, exactly like [`xform_pass`] -- but 4 rows at a time on 4 i64 lanes. `n <= 4`
/// (ADST exists only for 4x4/8x8/16x16; 32x32 is DCT-only and takes the i32-lane HBD DCT
/// kernel).
#[target_feature(enable = "avx2")]
unsafe fn xform_pass_i64<const ADST: bool>(
    input: &[i64; 256],
    output: &mut [i64; 256],
    n: u32,
    round_shift: Option<u32>,
) {
    debug_assert!((2..=4).contains(&n));
    let n0 = 1usize << n;
    let mut cs = 0usize;
    while cs < n0 {
        // Build element vectors t[0..n0] (t[k] = coefficient k across rows cs..cs+4) by
        // transposing the 4-row chunk one 4-column block at a time.
        let mut t = [_mm256_setzero_si256(); 16];
        let mut g = 0usize;
        while g < n0 {
            let mut rows4 = [_mm256_setzero_si256(); 4];
            for (r, slot) in rows4.iter_mut().enumerate() {
                *slot = _mm256_loadu_si256(input.as_ptr().add((cs + r) * n0 + g) as *const __m256i);
            }
            let cols = transpose4x4_i64(&rows4);
            t[g..g + 4].copy_from_slice(&cols);
            g += 4;
        }
        if ADST {
            iadst_i64(&mut t, n);
        } else {
            idct_permute_simd(&mut t, n);
            idct_i64(&mut t, n);
        }
        for (k, tk) in t[..n0].iter().enumerate() {
            let v = round_shift.map_or(*tk, |sh| round2_i64(*tk, sh));
            _mm256_storeu_si256(output.as_mut_ptr().add(k * n0 + cs) as *mut __m256i, v);
        }
        cs += 4;
    }
}

/// Transposes a 4x4 i32 matrix (`r[i]` = row i) so `out[j]` = column j.
#[inline]
unsafe fn transpose4x4_i32(r: &[__m128i; 4]) -> [__m128i; 4] {
    let a0 = _mm_unpacklo_epi32(r[0], r[1]);
    let a1 = _mm_unpackhi_epi32(r[0], r[1]);
    let a2 = _mm_unpacklo_epi32(r[2], r[3]);
    let a3 = _mm_unpackhi_epi32(r[2], r[3]);
    [
        _mm_unpacklo_epi64(a0, a2),
        _mm_unpackhi_epi64(a0, a2),
        _mm_unpacklo_epi64(a1, a3),
        _mm_unpackhi_epi64(a1, a3),
    ]
}

/// Transposes an 8x8 i32 matrix (`r[i]` = row i) so `out[j]` = column j. Unpack network within
/// each 128-bit lane, then a `permute2x128` pass to swap the diagonal 128-bit blocks.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn transpose8x8_i32(r: &[__m256i; 8]) -> [__m256i; 8] {
    let t0 = _mm256_unpacklo_epi32(r[0], r[1]);
    let t1 = _mm256_unpackhi_epi32(r[0], r[1]);
    let t2 = _mm256_unpacklo_epi32(r[2], r[3]);
    let t3 = _mm256_unpackhi_epi32(r[2], r[3]);
    let t4 = _mm256_unpacklo_epi32(r[4], r[5]);
    let t5 = _mm256_unpackhi_epi32(r[4], r[5]);
    let t6 = _mm256_unpacklo_epi32(r[6], r[7]);
    let t7 = _mm256_unpackhi_epi32(r[6], r[7]);
    let s0 = _mm256_unpacklo_epi64(t0, t2);
    let s1 = _mm256_unpackhi_epi64(t0, t2);
    let s2 = _mm256_unpacklo_epi64(t1, t3);
    let s3 = _mm256_unpackhi_epi64(t1, t3);
    let s4 = _mm256_unpacklo_epi64(t4, t6);
    let s5 = _mm256_unpackhi_epi64(t4, t6);
    let s6 = _mm256_unpacklo_epi64(t5, t7);
    let s7 = _mm256_unpackhi_epi64(t5, t7);
    [
        _mm256_permute2x128_si256(s0, s4, 0x20),
        _mm256_permute2x128_si256(s1, s5, 0x20),
        _mm256_permute2x128_si256(s2, s6, 0x20),
        _mm256_permute2x128_si256(s3, s7, 0x20),
        _mm256_permute2x128_si256(s0, s4, 0x31),
        _mm256_permute2x128_si256(s1, s5, 0x31),
        _mm256_permute2x128_si256(s2, s6, 0x31),
        _mm256_permute2x128_si256(s3, s7, 0x31),
    ]
}

/// `round2(x, shift)` with a runtime shift (the 2D column pass's `min(n+2, 6)`), lane-wise.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn round2_var(x: __m256i, shift: u32) -> __m256i {
    let add = _mm256_set1_epi32(1 << (shift - 1));
    _mm256_sra_epi32(_mm256_add_epi32(x, add), _mm_cvtsi32_si128(shift as i32))
}

/// One separable pass of the 2D inverse transform: transforms every "row" of `input` (n0 x n0,
/// row-major i32) with the inverse DCT (`ADST == false`) or inverse ADST (`ADST == true`) and
/// writes the result TRANSPOSED into `output`, so running this twice (row then column) lands
/// the 2D result back in row-major. `round_shift` is `None` for the row pass and
/// `Some(min(n+2,6))` for the column pass (spec §8.7.2). Vectorizes across 8 rows at a time
/// (4 for n0==4); each element vector holds one transform coefficient across those rows.
/// The ADST variant is 8-bit-only i32 arithmetic (see the inverse-ADST section comment) and is
/// never instantiated together with `HBD`.
#[target_feature(enable = "avx2")]
unsafe fn xform_pass<const HBD: bool, const ADST: bool>(
    input: &[i32; 1024],
    output: &mut [i32; 1024],
    n: u32,
    round_shift: Option<u32>,
) {
    let n0 = 1usize << n;

    if n0 == 4 {
        let mut rows4 = [_mm_setzero_si128(); 4];
        for (r, slot) in rows4.iter_mut().enumerate() {
            *slot = _mm_loadu_si128(input.as_ptr().add(r * 4) as *const __m128i);
        }
        let cols = transpose4x4_i32(&rows4);
        let mut t = [_mm256_setzero_si256(); 32];
        for (c, &col) in cols.iter().enumerate() {
            // The upper 128 bits of `_mm256_castsi128_si256` are formally UNDEFINED. Safe here
            // only because everything downstream on this n0 == 4 path is lane-wise (butterfly
            // add/sub/mul/shift; the ADST/DCT permutes reorder whole element VECTORS, not
            // lanes) and the final store reads only the low 128 bits. Adding any lane-CROSSING
            // op (cross-half permute/shuffle, horizontal reduce) here requires zero-extending
            // instead (`_mm256_zextsi128_si256`).
            t[c] = _mm256_castsi128_si256(col);
        }
        if ADST {
            iadst_simd(&mut t, n);
        } else {
            idct_permute_simd(&mut t, n);
            idct_simd::<HBD>(&mut t, n);
        }
        for (k, tk) in t[..4].iter().enumerate() {
            let v = round_shift.map_or(*tk, |sh| round2_var(*tk, sh));
            _mm_storeu_si128(
                output.as_mut_ptr().add(k * 4) as *mut __m128i,
                _mm256_castsi256_si128(v),
            );
        }
        return;
    }

    let mut cs = 0usize;
    while cs < n0 {
        // Build element vectors t[0..n0] (t[k] = coefficient k across rows cs..cs+8) by
        // transposing the 8-row chunk one 8-column block at a time.
        let mut t = [_mm256_setzero_si256(); 32];
        let mut g = 0usize;
        while g < n0 {
            let mut rows8 = [_mm256_setzero_si256(); 8];
            for (r, slot) in rows8.iter_mut().enumerate() {
                *slot = _mm256_loadu_si256(input.as_ptr().add((cs + r) * n0 + g) as *const __m256i);
            }
            let cols = transpose8x8_i32(&rows8);
            t[g..g + 8].copy_from_slice(&cols);
            g += 8;
        }
        if ADST {
            iadst_simd(&mut t, n);
        } else {
            idct_permute_simd(&mut t, n);
            idct_simd::<HBD>(&mut t, n);
        }
        for (k, tk) in t[..n0].iter().enumerate() {
            let v = round_shift.map_or(*tk, |sh| round2_var(*tk, sh));
            _mm256_storeu_si256(output.as_mut_ptr().add(k * n0 + cs) as *mut __m256i, v);
        }
        cs += 8;
    }
}

/// The two separable passes of the AVX2 DCT_DCT 2D inverse transform (spec §8.7.2):
/// converts `dequant` (n0 x n0 row-major i64) to i32, runs the row pass (-> R^T) then the column
/// pass (-> O, with `round2(min(n+2,6))`), and writes O row-major into `out`. Bit-exact with the
/// scalar path for conformant streams (see the SIMD-inverse-DCT module comment): spec §8.7.1.1
/// keeps every stored value inside i32 at any depth, so the narrower lane arithmetic is
/// identical -- provided `HBD` matches the content (widened butterfly products for 10/12-bit).
/// `dequant.len()` must be `n0*n0` (`2 <= n <= 5`).
#[target_feature(enable = "avx2")]
unsafe fn idct_2d_dct<const HBD: bool>(dequant: &[i64], n: u32, out: &mut [i32; 1024]) {
    let count = (1usize << n) * (1usize << n);
    let mut buf = [0i32; 1024];
    let mut buf2 = [0i32; 1024];
    for (dst, &src) in buf[..count].iter_mut().zip(dequant[..count].iter()) {
        *dst = src as i32;
    }
    xform_pass::<HBD, false>(&buf, &mut buf2, n, None); // row pass -> buf2 == R^T
    xform_pass::<HBD, false>(&buf2, out, n, Some((n + 2).min(6))); // column pass -> out == O
}

/// AVX2 8-bit DCT_DCT inverse transform + reconstruction (SIMD wave 4b), fused: transforms
/// `dequant` and adds the residual straight into the plane with the 8-bit clip
/// (`clip(pred + residual, 0, 255)`), skipping both the i64 write-back a standalone transform
/// would do and the scalar per-pixel reconstruction loop. The n0 x n0 block sits at
/// `(start_x, start_y)` in the row-major u16 `plane_data` (stride `plane_width`). Bit-exact with
/// the scalar transform-then-reconstruct at `bit_depth == 8`.
///
/// # Safety
/// `avx2_enabled()` must hold; `dequant.len()` must be `n0*n0` (`2 <= n <= 5`); and the block's
/// rows `start_y..start_y+n0` x columns `start_x..start_x+n0` must be in bounds for `plane_data`.
#[target_feature(enable = "avx2")]
pub unsafe fn inverse_transform_dct_dct_reconstruct_avx2(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    dequant: &[i64],
    n: u32,
) {
    idct_dct_reconstruct::<false>(plane_data, plane_width, start_x, start_y, dequant, n, 255);
}

/// High-bit-depth (10/12-bit) companion to [`inverse_transform_dct_dct_reconstruct_avx2`]: the
/// same fused DCT_DCT inverse transform + reconstruction, but with the butterfly products
/// widened to i64 (see `b_op_simd_hbd` -- at 10/12-bit they overflow the 8-bit kernel's i32
/// `mullo`) and the reconstruction clip at `(1 << bit_depth) - 1`. Bit-exact with the scalar
/// transform-then-reconstruct for conformant 10/12-bit streams (spec §8.7.1.1's `8 + BitDepth`
/// bound keeps all stored intermediates inside the i32 lanes).
///
/// # Safety
/// Same contract as [`inverse_transform_dct_dct_reconstruct_avx2`]; `bit_depth` must be the
/// stream's bit depth (10 or 12).
#[target_feature(enable = "avx2")]
pub unsafe fn inverse_transform_dct_dct_reconstruct_hbd_avx2(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    dequant: &[i64],
    n: u32,
    bit_depth: u8,
) {
    let max_val = (1i32 << bit_depth) - 1;
    idct_dct_reconstruct::<true>(
        plane_data,
        plane_width,
        start_x,
        start_y,
        dequant,
        n,
        max_val,
    );
}

/// AVX2 8-bit inverse transform + reconstruction for the ADST-containing transform types
/// (`AdstDct` / `DctAdst` / `AdstAdst`, spec §8.7.1.4-8.7.1.9 + §8.7.2), fused into the plane
/// exactly like [`inverse_transform_dct_dct_reconstruct_avx2`]: the ADST axis runs the vector
/// `iadst_simd`, the DCT axis (of the mixed types) the proven `idct_simd`, and the residual is
/// added with the 8-bit clip. ADST exists only for 4x4/8x8/16x16 (`n <= 4`; 32x32 is DCT-only,
/// spec §8.7.2). 8-bit only -- see the inverse-ADST section comment; the dispatch routes
/// 10/12-bit ADST to [`inverse_transform_adst_reconstruct_hbd_avx2`] and keeps lossless WHT
/// scalar.
///
/// # Safety
/// Same contract as [`inverse_transform_dct_dct_reconstruct_avx2`], with `2 <= n <= 4`;
/// `tx_type` must not be `DctDct` (that type has its own fused entries) and the stream must be
/// 8-bit.
#[target_feature(enable = "avx2")]
pub unsafe fn inverse_transform_adst_reconstruct_avx2(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    dequant: &[i64],
    n: u32,
    tx_type: TxType,
) {
    debug_assert!((2..=4).contains(&n));
    debug_assert!(tx_type != TxType::DctDct);
    let n0 = 1usize << n;
    let count = n0 * n0;
    let mut buf = [0i32; 1024];
    let mut buf2 = [0i32; 1024];
    let mut o = [0i32; 1024];
    for (dst, &src) in buf[..count].iter_mut().zip(dequant[..count].iter()) {
        *dst = src as i32;
    }
    let shift = (n + 2).min(6);
    // Row pass -> buf2 == R^T. The scalar 2D driver (`transform.rs::inverse_transform_block`)
    // runs ADST rows for DctAdst | AdstAdst and DCT rows otherwise; columns are the complement.
    match tx_type {
        TxType::DctAdst | TxType::AdstAdst => xform_pass::<false, true>(&buf, &mut buf2, n, None),
        _ => xform_pass::<false, false>(&buf, &mut buf2, n, None),
    }
    // Column pass -> o == O (row-major), with the final `round2(min(n+2, 6))`.
    match tx_type {
        TxType::AdstDct | TxType::AdstAdst => {
            xform_pass::<false, true>(&buf2, &mut o, n, Some(shift))
        }
        _ => xform_pass::<false, false>(&buf2, &mut o, n, Some(shift)),
    }
    reconstruct_add_clip(plane_data, plane_width, start_x, start_y, &o, n0, 255);
}

/// High-bit-depth (10/12-bit) companion to [`inverse_transform_adst_reconstruct_avx2`]: the
/// same fused inverse transform + reconstruction for the ADST-containing types, but on the
/// i64-lane network (see the i64 section comment -- at 10/12-bit the ADST's unrounded `S`
/// array exceeds i32 lane storage) with the clip at `(1 << bit_depth) - 1`. Both axes of a
/// mixed block run in i64 (`xform_pass_i64`), so the driver is self-contained. The final
/// column-pass outputs are narrowed to i32 for the shared `reconstruct_add_clip`: exact,
/// because they are post-`round2(min(n+2, 6))` stored values that spec §8.7.1.1 bounds to
/// signed `8 + BitDepth` <= 20 bits on a conformant stream (and even adversarial full-range
/// inputs keep them under ~2^28: per-pass L-inf gain <= ~2^6, then >> at least 4). On
/// NON-conformant streams this narrowing may wrap where the scalar i64 does not -- the same
/// documented caveat as the i32-lane DCT kernels (SIMD vs scalar may legitimately diverge on
/// garbage input).
///
/// # Safety
/// Same contract as [`inverse_transform_adst_reconstruct_avx2`] (so `2 <= n <= 4` and
/// `tx_type != DctDct`); `bit_depth` must be the stream's bit depth (10 or 12).
#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub unsafe fn inverse_transform_adst_reconstruct_hbd_avx2(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    dequant: &[i64],
    n: u32,
    tx_type: TxType,
    bit_depth: u8,
) {
    // Release-mode assert (not debug_assert): a violated `n <= 4` here would compute
    // out-of-bounds raw-pointer offsets into the [i64; 256] buffers below (UB), unlike
    // the i32 sibling whose [i32; 1024] buffers would merely panic on slice indexing.
    // Unreachable from the current dispatch (`compute_tx_type` forces DctDct for
    // TX_32X32), so this is one predictable branch per block guarding a future
    // dispatch change.
    assert!((2..=4).contains(&n));
    debug_assert!(tx_type != TxType::DctDct);
    let n0 = 1usize << n;
    let count = n0 * n0;
    let mut buf = [0i64; 256];
    let mut buf2 = [0i64; 256];
    buf[..count].copy_from_slice(&dequant[..count]);
    let shift = (n + 2).min(6);
    // Row pass -> buf2 == R^T (same row/column tx-type split as the scalar 2D driver and the
    // 8-bit entry above), then column pass -> buf == O (row-major) with the final round2.
    match tx_type {
        TxType::DctAdst | TxType::AdstAdst => xform_pass_i64::<true>(&buf, &mut buf2, n, None),
        _ => xform_pass_i64::<false>(&buf, &mut buf2, n, None),
    }
    match tx_type {
        TxType::AdstDct | TxType::AdstAdst => {
            xform_pass_i64::<true>(&buf2, &mut buf, n, Some(shift))
        }
        _ => xform_pass_i64::<false>(&buf2, &mut buf, n, Some(shift)),
    }
    // Narrow to the i32 residual layout the shared reconstruction takes (exact -- see above).
    let mut o = [0i32; 1024];
    for (dst, &src) in o[..count].iter_mut().zip(buf[..count].iter()) {
        *dst = src as i32;
    }
    let max_val = (1i32 << bit_depth) - 1;
    reconstruct_add_clip(plane_data, plane_width, start_x, start_y, &o, n0, max_val);
}

/// Shared body of the two fused DCT_DCT entries above: 2D transform (`idct_2d_dct::<HBD>`),
/// then the fused per-pixel reconstruction (`reconstruct_add_clip`).
#[target_feature(enable = "avx2")]
unsafe fn idct_dct_reconstruct<const HBD: bool>(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    dequant: &[i64],
    n: u32,
    max_val: i32,
) {
    let n0 = 1usize << n;
    let mut o = [0i32; 1024];
    idct_2d_dct::<HBD>(dequant, n, &mut o);
    reconstruct_add_clip(plane_data, plane_width, start_x, start_y, &o, n0, max_val);
}

/// Per-pixel `clip(pred + residual, 0, max_val)` of the n0 x n0 residual block `o` (row-major)
/// straight into the u16 plane -- the fused reconstruction shared by the DCT_DCT and ADST
/// entries.
#[target_feature(enable = "avx2")]
unsafe fn reconstruct_add_clip(
    plane_data: &mut [u16],
    plane_width: usize,
    start_x: usize,
    start_y: usize,
    o: &[i32; 1024],
    n0: usize,
    max_val: i32,
) {
    let base = plane_data.as_mut_ptr();
    let zero = _mm256_setzero_si256();
    let max = _mm256_set1_epi32(max_val);

    if n0 == 4 {
        for i in 0..4 {
            let row = base.add((start_y + i) * plane_width + start_x);
            let pred = _mm_cvtepu16_epi32(_mm_loadl_epi64(row as *const __m128i));
            let resid = _mm_loadu_si128(o.as_ptr().add(i * 4) as *const __m128i);
            let sum = _mm_add_epi32(pred, resid);
            let clipped = _mm_min_epi32(
                _mm_max_epi32(sum, _mm256_castsi256_si128(zero)),
                _mm256_castsi256_si128(max),
            );
            _mm_storel_epi64(row as *mut __m128i, _mm_packus_epi32(clipped, clipped));
        }
        return;
    }

    for i in 0..n0 {
        let mut j = 0usize;
        while j < n0 {
            let row = base.add((start_y + i) * plane_width + start_x + j);
            let pred = _mm256_cvtepu16_epi32(_mm_loadu_si128(row as *const __m128i));
            let resid = _mm256_loadu_si256(o.as_ptr().add(i * n0 + j) as *const __m256i);
            let sum = _mm256_add_epi32(pred, resid);
            let clipped = _mm256_min_epi32(_mm256_max_epi32(sum, zero), max);
            let packed = _mm_packus_epi32(
                _mm256_castsi256_si128(clipped),
                _mm256_extracti128_si256(clipped, 1),
            );
            _mm_storeu_si128(row as *mut __m128i, packed);
            j += 8;
        }
    }
}
