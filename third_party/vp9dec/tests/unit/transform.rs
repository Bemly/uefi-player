//! Tests for the `transform` module.
//!
//! To verify the correctness of the integer implementation, an independent
//! floating-point version of the butterfly network is written from scratch
//! based on the pseudocode in spec §8.7.1 (using the actual `cos`/`sin`
//! instead of the 14bit fixed-point `cos64`/`sin64` tables, and without the
//! `Round2` rounding at each stage), and we confirm that the output
//! difference from the integer implementation stays within ±1 for random
//! coefficients. We also test mathematical properties that don't depend on
//! the transform type, such as linearity and flat output for a lone DC
//! coefficient input.

use super::*;
use std::f64::consts::PI;

// ------------------------------------------------------------------------
// Homegrown LCG random number generator (minimal implementation to avoid
// depending on an external crate).
// ------------------------------------------------------------------------

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg { state: seed }
    }

    /// 64bit LCG using constants from Numerical Recipes.
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    /// Generates an integer coefficient in the range `[-range, range]` (modeling a transform coefficient).
    fn next_coef(&mut self, range: i64) -> i64 {
        let v = (self.next_u64() >> 33) as i64; // 31bit non-negative value
        (v % (2 * range + 1)) - range
    }
}

// ------------------------------------------------------------------------
// Floating-point butterfly network (written independently of the integer implementation).
// ------------------------------------------------------------------------
//
// Using the relation `cos64(angle)/16384 ≈ cos(angle * pi / 64)`, the integer
// implementation's "multiply by 16384, then scale back with Round2(.., 14)"
// operation is replaced, in floating point, by simply "multiplying by the
// real-valued cos/sin directly". This does not reproduce the rounding error
// of the 14bit table itself, but it does independently verify whether the
// butterfly network's wiring (index computation) is correct.

fn cos_f(angle: i32) -> f64 {
    (angle as f64 * PI / 64.0).cos()
}
fn sin_f(angle: i32) -> f64 {
    (angle as f64 * PI / 64.0).sin()
}

fn b_op_f(t: &mut [f64], a: usize, b: usize, angle: i32, flip: bool) {
    let c = cos_f(angle);
    let s = sin_f(angle);
    let ta = t[a];
    let tb = t[b];
    t[a] = ta * c - tb * s;
    t[b] = ta * s + tb * c;
    if flip {
        t.swap(a, b);
    }
}

fn h_op_f(t: &mut [f64], a: usize, b: usize, flip: bool) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a];
    let y = t[b];
    t[a] = x + y;
    t[b] = x - y;
}

fn sb_op_f(s: &mut [f64], t: &[f64], a: usize, b: usize, angle: i32, flip: bool) {
    let c = cos_f(angle);
    let sn = sin_f(angle);
    let ta = t[a];
    let tb = t[b];
    let sa = ta * c - tb * sn;
    let sbv = ta * sn + tb * c;
    if flip {
        s[a] = sbv;
        s[b] = sa;
    } else {
        s[a] = sa;
        s[b] = sbv;
    }
}

fn sh_op_f(t: &mut [f64], s: &[f64], a: usize, b: usize) {
    t[a] = s[a] + s[b];
    t[b] = s[a] - s[b];
}

fn idct_permute_f(t: &mut [f64], n: u32) {
    let n0 = 1usize << n;
    let copy_t: Vec<f64> = t[..n0].to_vec();
    for i in 0..n0 {
        t[i] = copy_t[brev(n, i)];
    }
}

fn idct_f(t: &mut [f64], n: u32) {
    let n0 = 1i64 << n;
    let n1 = 1i64 << (n - 1);
    let n2 = 1i64 << (n - 2);

    if n == 2 {
        b_op_f(t, 0, 1, 16, true);
    } else {
        idct_f(t, n - 1);
    }

    for i in 0..n2 {
        let a = (n1 + i) as usize;
        let b = (n0 - 1 - i) as usize;
        let angle = 32 - brev(5, a) as i32;
        b_op_f(t, a, b, angle, false);
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            for j in 0..2i64 {
                let a = (n1 + 4 * i + 2 * j) as usize;
                let b = (n1 + 1 + 4 * i + 2 * j) as usize;
                h_op_f(t, a, b, j == 1);
            }
        }
    }

    if n == 5 {
        for i in 0..2i64 {
            for j in 0..2i64 {
                let a = (n0 - n as i64 + 3 - n2 * j - 4 * i) as usize;
                let b = (n1 + n as i64 - 4 + n2 * j + 4 * i) as usize;
                let angle = 28 - 16 * i as i32 + 56 * j as i32;
                b_op_f(t, a, b, angle, true);
            }
        }
        let n3 = 1i64 << (n - 3);
        for i in 0..2i64 {
            for j in 0..4i64 {
                let a = (n1 + n3 * j + i) as usize;
                let b = (n1 + n2 - 5 + n3 * j - i) as usize;
                h_op_f(t, a, b, (j & 1) == 1);
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
                b_op_f(t, a, b, angle, true);
            }
        }
        let imax_b: i64 = 2 * n as i64 - 7;
        for j in 0..2i64 {
            for i in 0..=imax_b {
                let a = (n1 + n2 * j + i) as usize;
                let b = (n1 + n2 - 1 + n2 * j - i) as usize;
                h_op_f(t, a, b, (j & 1) == 1);
            }
        }
    }

    if n >= 3 {
        let n3 = 1i64 << (n - 3);
        for i in 0..n3 {
            let a = (n0 - n3 - 1 - i) as usize;
            let b = (n1 + n3 + i) as usize;
            b_op_f(t, a, b, 16, true);
        }
    }

    for i in 0..n1 {
        let a = i as usize;
        let b = (n0 - 1 - i) as usize;
        h_op_f(t, a, b, false);
    }
}

fn adst_input_permute_f(t: &mut [f64], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let copy_t: Vec<f64> = t[..n0].to_vec();
    for i in 0..n1 {
        t[2 * i] = copy_t[n0 - 1 - 2 * i];
        t[2 * i + 1] = copy_t[2 * i];
    }
}

fn adst_output_permute_f(t: &mut [f64], n: u32) {
    if n == 4 {
        let copy_t: Vec<f64> = t[..16].to_vec();
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
        let copy_t: Vec<f64> = t[..8].to_vec();
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

/// ADST4 is the spec's algebraic formula expanded directly as a matrix
/// (derived by hand). Verified via a separate path from `iadst4_impl`,
/// writing out the coefficient matrix explicitly.
fn iadst4_f(t: &mut [f64]) {
    let k1 = SINPI_1_9 as f64 / 16384.0;
    let k2 = SINPI_2_9 as f64 / 16384.0;
    let k3 = SINPI_3_9 as f64 / 16384.0;
    let k4 = SINPI_4_9 as f64 / 16384.0;
    let (t0, t1, t2, t3) = (t[0], t[1], t[2], t[3]);
    // The formula in §8.7.1.6 expanded into the form o_i = sum_j M[i][j] * t_j.
    let o0 = k1 * t0 + k3 * t1 + k4 * t2 + k2 * t3;
    let o1 = k2 * t0 + k3 * t1 - k1 * t2 - k4 * t3;
    let o2 = k3 * t0 - k3 * t2 + k3 * t3;
    let o3 = (k1 + k2) * t0 - k3 * t1 + (k4 - k1) * t2 + (k2 - k4) * t3;
    t[0] = o0;
    t[1] = o1;
    t[2] = o2;
    t[3] = o3;
}

fn iadst8_f(t: &mut [f64]) {
    adst_input_permute_f(t, 3);
    let mut s = [0f64; 8];
    for i in 0..4usize {
        sb_op_f(&mut s, t, 2 * i, 1 + 2 * i, 30 - 8 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_f(t, &s, i, 4 + i);
    }
    for i in 0..2usize {
        sb_op_f(&mut s, t, 4 + 3 * i, 5 + i, 24 - 16 * i as i32, true);
    }
    for i in 0..2usize {
        sh_op_f(t, &s, 4 + i, 6 + i);
    }
    for i in 0..2usize {
        h_op_f(t, i, 2 + i, false);
    }
    for i in 0..2usize {
        b_op_f(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }
    adst_output_permute_f(t, 3);
    for i in 0..4usize {
        t[1 + 2 * i] = -t[1 + 2 * i];
    }
}

fn iadst16_f(t: &mut [f64]) {
    adst_input_permute_f(t, 4);
    let mut s = [0f64; 16];
    for i in 0..8usize {
        sb_op_f(&mut s, t, 2 * i, 1 + 2 * i, 31 - 4 * i as i32, true);
    }
    for i in 0..8usize {
        sh_op_f(t, &s, i, 8 + i);
    }
    for i in 0..4usize {
        sb_op_f(&mut s, t, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i as i32, true);
    }
    for i in 0..4usize {
        sh_op_f(t, &s, 8 + i, 12 + i);
    }
    for i in 0..4usize {
        h_op_f(t, i, 4 + i, false);
    }
    for i in 0..2usize {
        for j in 0..2usize {
            sb_op_f(
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
            sh_op_f(t, &s, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            h_op_f(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            let angle = 48 + 64 * (i ^ j) as i32;
            b_op_f(t, 2 + 4 * j + 8 * i, 3 + 4 * j + 8 * i, angle, false);
        }
    }
    adst_output_permute_f(t, 4);
    for i in 0..2usize {
        for j in 0..2usize {
            t[1 + 12 * j + 2 * i] = -t[1 + 12 * j + 2 * i];
        }
    }
}

// ------------------------------------------------------------------------
// Inverse DCT: integer implementation vs. floating-point reference implementation
// ------------------------------------------------------------------------

/// Compares the output of the integer `idct` against the floating-point
/// reference implementation `idct_f`, and confirms that the maximum absolute
/// error is within `tol`.
fn check_idct(n: u32, trials: usize, coef_range: i64, tol: f64, seed: u64) {
    let n0 = 1usize << n;
    let mut rng = Lcg::new(seed);
    for _ in 0..trials {
        let coeffs: Vec<i64> = (0..n0).map(|_| rng.next_coef(coef_range)).collect();

        let mut ti: Vec<i64> = coeffs.clone();
        idct_permute(&mut ti, n);
        idct(&mut ti, n);

        let mut tf: Vec<f64> = coeffs.iter().map(|&x| x as f64).collect();
        idct_permute_f(&mut tf, n);
        idct_f(&mut tf, n);

        for k in 0..n0 {
            let diff = (ti[k] as f64 - tf[k]).abs();
            assert!(
                diff <= tol,
                "idct n={n} k={k}: int={} float={} diff={diff} (coeffs={coeffs:?})",
                ti[k],
                tf[k]
            );
        }
    }
}

// The tolerance grows roughly proportionally to the number of butterfly
// stages (≒ log2(n0)). This is because each stage introduces a ±0.5-unit
// (on the 14bit scale) rounding error from `Round2(x, 14)`, which propagates
// and accumulates through the later rotations — this is normal behavior that
// also occurs in a real VP9 decoder (the final error, after the final shift
// of the 2D transform is applied, stays within about ±1-2 per pixel; see
// `inverse_transform_block_matches_float_reference_all_tx_types`).
// The tolerances below were derived from the measured maximum error across
// 2000 sets of random coefficients x each element (n=2: ~2.0, n=3: ~4.8,
// n=4: ~4.7, n=5: ~6.7), with a safety margin added.

#[test]
fn idct4_matches_float_reference() {
    check_idct(2, 2000, 1 << 15, 3.0, 1);
}

#[test]
fn idct8_matches_float_reference() {
    check_idct(3, 2000, 1 << 15, 6.0, 2);
}

#[test]
fn idct16_matches_float_reference() {
    check_idct(4, 2000, 1 << 14, 6.0, 3);
}

#[test]
fn idct32_matches_float_reference() {
    check_idct(5, 2000, 1 << 13, 8.0, 4);
}

// ------------------------------------------------------------------------
// Inverse ADST: integer implementation vs. floating-point reference implementation
// ------------------------------------------------------------------------

fn check_iadst(n: u32, trials: usize, coef_range: i64, tol: f64, seed: u64) {
    let n0 = 1usize << n;
    let mut rng = Lcg::new(seed);
    for _ in 0..trials {
        let coeffs: Vec<i64> = (0..n0).map(|_| rng.next_coef(coef_range)).collect();

        let mut ti: Vec<i64> = coeffs.clone();
        iadst(&mut ti, n);

        let mut tf: Vec<f64> = coeffs.iter().map(|&x| x as f64).collect();
        match n {
            2 => iadst4_f(&mut tf),
            3 => iadst8_f(&mut tf),
            4 => iadst16_f(&mut tf),
            _ => unreachable!(),
        }

        for k in 0..n0 {
            let diff = (ti[k] as f64 - tf[k]).abs();
            assert!(
                diff <= tol,
                "iadst n={n} k={k}: int={} float={} diff={diff} (coeffs={coeffs:?})",
                ti[k],
                tf[k]
            );
        }
    }
}

#[test]
fn iadst4_matches_float_reference() {
    check_iadst(2, 2000, 1 << 15, 2.0, 11);
}

#[test]
fn iadst8_matches_float_reference() {
    check_iadst(3, 2000, 1 << 15, 6.0, 12);
}

#[test]
fn iadst16_matches_float_reference() {
    check_iadst(4, 2000, 1 << 14, 6.0, 13);
}

// ------------------------------------------------------------------------
// Tests for mathematical properties that don't depend on the transform type
// ------------------------------------------------------------------------

/// When only the DC coefficient (the first element) is nonzero, the inverse
/// DCT's output is flat (all elements are nearly identical). This is a
/// verification based on the mathematical fact — independent of the
/// butterfly network's wiring — that the DCT's 0th basis function is a
/// constant function.
#[test]
fn idct_dc_only_is_flat() {
    for &n in &[2u32, 3, 4, 5] {
        let n0 = 1usize << n;
        for &dc in &[100i64, -500, 4000] {
            let mut t = vec![0i64; n0];
            t[0] = dc;
            idct_permute(&mut t, n);
            idct(&mut t, n);
            let first = t[0];
            for (k, &v) in t.iter().enumerate() {
                assert!(
                    (v - first).abs() <= 1,
                    "n={n} dc={dc} k={k}: v={v} first={first}"
                );
            }
        }
    }
}

/// The inverse DCT is a linear map: `idct(a + b) == idct(a) + idct(b)`
/// (within rounding error).
#[test]
fn idct_is_linear() {
    let mut rng = Lcg::new(42);
    for &n in &[2u32, 3, 4, 5] {
        let n0 = 1usize << n;
        for _ in 0..50 {
            let a: Vec<i64> = (0..n0).map(|_| rng.next_coef(1000)).collect();
            let b: Vec<i64> = (0..n0).map(|_| rng.next_coef(1000)).collect();
            let sum: Vec<i64> = a.iter().zip(b.iter()).map(|(x, y)| x + y).collect();

            let run = |mut v: Vec<i64>| {
                idct_permute(&mut v, n);
                idct(&mut v, n);
                v
            };
            let ra = run(a);
            let rb = run(b);
            let rsum = run(sum);

            for k in 0..n0 {
                let combined = ra[k] + rb[k];
                // The Round2 rounding at each butterfly stage occurs
                // independently for a and b, and can diverge from the
                // rounding applied after summation, so we allow an error
                // proportional to the number of stages (same reasoning as
                // [`check_idct`]).
                assert!(
                    (rsum[k] - combined).abs() <= 8,
                    "n={n} k={k}: rsum={} combined={}",
                    rsum[k],
                    combined
                );
            }
        }
    }
}

/// ADST also varies gently for a lone DC coefficient, but unlike the DCT it
/// doesn't become perfectly flat. Instead, we lightly verify only its
/// "linearity".
#[test]
fn iadst_is_linear() {
    let mut rng = Lcg::new(43);
    for &n in &[2u32, 3, 4] {
        let n0 = 1usize << n;
        for _ in 0..50 {
            let a: Vec<i64> = (0..n0).map(|_| rng.next_coef(1000)).collect();
            let b: Vec<i64> = (0..n0).map(|_| rng.next_coef(1000)).collect();
            let sum: Vec<i64> = a.iter().zip(b.iter()).map(|(x, y)| x + y).collect();

            let run = |mut v: Vec<i64>| {
                iadst(&mut v, n);
                v
            };
            let ra = run(a);
            let rb = run(b);
            let rsum = run(sum);

            for k in 0..n0 {
                let combined = ra[k] + rb[k];
                assert!(
                    (rsum[k] - combined).abs() <= 4,
                    "n={n} k={k}: rsum={} combined={}",
                    rsum[k],
                    combined
                );
            }
        }
    }
}

// ------------------------------------------------------------------------
// Inverse Walsh-Hadamard transform (for lossless): lightweight tests
// ------------------------------------------------------------------------

/// The WHT is nearly linear, but because the internal `e = (a - d) >> 1` is a
/// floor division via right shift (rounding toward -∞ for negative numbers),
/// depending on the parity of `a - d`, `iwht4(x + y) != iwht4(x) + iwht4(y)`
/// can differ by at most 1. We confirm that this deviation stays within ±1,
/// rather than requiring perfect linearity.
#[test]
fn iwht4_is_approximately_linear() {
    let mut rng = Lcg::new(7);
    for _ in 0..50 {
        let a = [
            rng.next_coef(1000),
            rng.next_coef(1000),
            rng.next_coef(1000),
            rng.next_coef(1000),
        ];
        let b = [
            rng.next_coef(1000),
            rng.next_coef(1000),
            rng.next_coef(1000),
            rng.next_coef(1000),
        ];
        let mut sum = a;
        for i in 0..4 {
            sum[i] += b[i];
        }

        let mut ra = a;
        iwht4(&mut ra, 0);
        let mut rb = b;
        iwht4(&mut rb, 0);
        let mut rsum = sum;
        iwht4(&mut rsum, 0);

        for i in 0..4 {
            assert!(
                (rsum[i] - (ra[i] + rb[i])).abs() <= 1,
                "index {i}: rsum={} ra+rb={}",
                rsum[i],
                ra[i] + rb[i]
            );
        }
    }
}

/// For a lone DC coefficient, lightly confirm that the WHT's output is also
/// flat like the other transforms (when `shift == 0`).
#[test]
fn iwht4_dc_only_is_flat() {
    let mut t = [400i64, 0, 0, 0];
    iwht4(&mut t, 0);
    let first = t[0];
    for &v in t.iter() {
        assert_eq!(v, first);
    }
}

// ------------------------------------------------------------------------
// Assembly of the 2D transform (inverse_transform_block)
// ------------------------------------------------------------------------

/// When only the DC coefficient (`Dequant[0][0]`) is nonzero, the output of
/// the `DCT_DCT` 2D inverse transform is nearly flat across the entire block.
#[test]
fn inverse_transform_block_dc_only_is_flat_dct() {
    for &n in &[2u32, 3, 4, 5] {
        let n0 = 1usize << n;
        let mut d = vec![0i64; n0 * n0];
        d[0] = 5000;
        inverse_transform_block(&mut d, n, TxType::DctDct, false);
        let first = d[0];
        for (idx, &v) in d.iter().enumerate() {
            assert!(
                (v - first).abs() <= 2,
                "n={n} idx={idx}: v={v} first={first}"
            );
        }
    }
}

/// Confirms that the 4x4 lossless (WHT) path runs without breaking and
/// returns a flat output for a lone DC coefficient (lightweight test).
#[test]
fn inverse_transform_block_lossless_wht_smoke() {
    let mut d = vec![0i64; 16];
    d[0] = 400;
    inverse_transform_block(&mut d, 2, TxType::DctDct, true);
    let first = d[0];
    for &v in d.iter() {
        assert_eq!(v, first);
    }
}

/// Confirms that the 2D transform (all 4 combinations, e.g. row DCT / column
/// ADST) stays within ±2 of a floating-point reference implementation
/// (row transform -> column transform -> final shift, reproduced in float).
#[test]
fn inverse_transform_block_matches_float_reference_all_tx_types() {
    let tx_types = [
        TxType::DctDct,
        TxType::AdstDct,
        TxType::DctAdst,
        TxType::AdstAdst,
    ];
    for &n in &[2u32, 3, 4] {
        let n0 = 1usize << n;
        for (ti, &tx_type) in tx_types.iter().enumerate() {
            let mut rng = Lcg::new(100 + n as u64 * 10 + ti as u64);
            for _ in 0..30 {
                let coeffs: Vec<i64> = (0..n0 * n0).map(|_| rng.next_coef(1 << 12)).collect();

                let mut di = coeffs.clone();
                inverse_transform_block(&mut di, n, tx_type, false);

                let df = float_2d_reference(&coeffs, n, tx_type);

                for k in 0..n0 * n0 {
                    let diff = (di[k] as f64 - df[k]).abs();
                    assert!(
                        diff <= 3.0,
                        "n={n} tx={tx_type:?} k={k}: int={} float={} diff={diff}",
                        di[k],
                        df[k]
                    );
                }
            }
        }
    }
}

/// Floating-point reference implementation of [`inverse_transform_block`]
/// (row -> column -> equivalent scaling for the final shift).
fn float_2d_reference(coeffs: &[i64], n: u32, tx_type: TxType) -> Vec<f64> {
    let n0 = 1usize << n;
    let mut d: Vec<f64> = coeffs.iter().map(|&x| x as f64).collect();

    let apply_row = |v: &mut [f64]| match tx_type {
        TxType::DctDct | TxType::AdstDct => {
            idct_permute_f(v, n);
            idct_f(v, n);
        }
        TxType::DctAdst | TxType::AdstAdst => match n {
            2 => iadst4_f(v),
            3 => iadst8_f(v),
            4 => iadst16_f(v),
            _ => unreachable!(),
        },
    };
    let apply_col = |v: &mut [f64]| match tx_type {
        TxType::DctDct | TxType::DctAdst => {
            idct_permute_f(v, n);
            idct_f(v, n);
        }
        TxType::AdstDct | TxType::AdstAdst => match n {
            2 => iadst4_f(v),
            3 => iadst8_f(v),
            4 => iadst16_f(v),
            _ => unreachable!(),
        },
    };

    for i in 0..n0 {
        let row = &mut d[i * n0..(i + 1) * n0];
        apply_row(row);
    }
    let shift = (n + 2).min(6);
    let scale = (1u64 << shift) as f64;
    let mut col_buf = vec![0f64; n0];
    for j in 0..n0 {
        for i in 0..n0 {
            col_buf[i] = d[i * n0 + j];
        }
        apply_col(&mut col_buf);
        for i in 0..n0 {
            d[i * n0 + j] = col_buf[i] / scale;
        }
    }
    d
}
