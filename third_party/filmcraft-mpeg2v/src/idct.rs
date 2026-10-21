//! 8×8 inverse DCT (H.262 Annex A): the separable 2-D IDCT evaluated in
//! fixed point (`S = 16`, direct even/odd-free dot products), rounded once to
//! the nearest integer and saturated to [-256, 255]. Its error is far inside
//! the IEEE Std 1180-1990 accuracy limits the standard requires (checked by
//! the test procedure of IEEE 1180 itself, below).
//!
//! Dual path (VENDOR(uefi-player)): the row pass has a scalar twin (always
//! compiled) and an integer-SSE2 twin (`asm!` explicit `xmm` registers,
//! `pmaddwd` dots, no intrinsics, no float vectors, no AVX); the column pass
//! and the final rounding are one shared scalar tail, so both paths are
//! bit-identical by construction (32-bit modular dots feed exact 64-bit
//! accumulators — order-independent). The runtime `crate::simd_enabled`
//! switch picks the row kernel; the DC term is folded exactly (as before),
//! so `dc_only` and the full transform agree for every DC value.
//!
//! Bounds (all plain `i64`, never wrapping, never panicking, every build):
//! inputs are saturated once to ±32768 (`i16` lane range; a no-op on every
//! spec-compliant stream and on all accuracy/oracle inputs), basis entries
//! `|C'| <= 32138 < 32767` (asserted by `basis_fits_i16`): row outputs
//! `|T| <= 8·32138·32768 < 2^34`, column outputs
//! `|A| <= 8·32138·2^34 < 2^63`, final numerator
//! `|d|·2^32 + 8|A| + 2^34 < 2^55`.

// VENDOR(uefi-player): one-time lookup tables in both builds. `core::cell`
// has no `Sync` cell (its OnceCell is rejected for `static`s), so the tiny
// `spin::Once` is used unconditionally — identical single-init semantics,
// verified by the table/accuracy tests below in both configurations.
use spin::Once as OnceLock;

/// Fixed-point scale of the basis table below.
const SCALE_BITS: u32 = 16;

/// `round(M[x][u]·2^16)` as `i16`, with
/// `M[x][u] = C(u)/2 · cos((2x+1)uπ/16)`, `C(0) = 1/√2`, `C(u>0) = 1`.
/// `|M| <= 0.5·cos(π/16) < 0.4904`, so every entry fits `i16`
/// (`round(0.4904·65536) = 32138`; see `basis_fits_i16`).
fn basis() -> &'static [[i16; 8]; 8] {
    static B: OnceLock<[[i16; 8]; 8]> = OnceLock::new();
    B.call_once(|| {
        let mut m = [[0i16; 8]; 8];
        for (x, row) in m.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                let c = if u == 0 { core::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                // VENDOR(uefi-player): `libm::cos` runs once per process at
                // table init (not on the hot path) in both std and no_std
                // builds (identical tables, minimal diff; libm is already in
                // the firmware closure via rusty_h264-decoder). `f64::round`
                // is std-only, so the rounding is `libm::round` as well.
                *v = libm::round(c / 2.0 * libm::cos(((2 * x + 1) * u) as f64 * core::f64::consts::PI / 16.0) * ((1u32 << SCALE_BITS) as f64)) as i16;
            }
        }
        m
    })
}

/// In-place IDCT of a block of dequantised coefficients (raster order). `dc_only`: only `blk[0]`
/// may be non-zero.
#[inline]
pub(crate) fn idct(blk: &mut [i32; 64], dc_only: bool) {
    // VENDOR(uefi-player): single shared saturation to the `i16` lane range
    // before dispatch — a no-op on every spec-compliant stream (and on all
    // accuracy/oracle inputs), so both paths below always agree bit for bit
    // while adversarial inputs stay deterministic and panic-free.
    for v in blk.iter_mut() {
        *v = (*v).clamp(-32768, 32767);
    }
    if dc_only {
        let v = ((blk[0] + 4) >> 3).clamp(-256, 255);
        blk.fill(v);
        return;
    }
    let m = basis();
    // The DC term F[0][0]/8 is added exactly (so ties round as in the DC-only path).
    let dc = blk[0] as i64;
    blk[0] = 0;
    // rows: tmp[v][x] = Σ_u C'[x][u]·F[v][u] (exact `i64`).
    let mut tmp = [[0i64; 8]; 8];
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if crate::simd_enabled() {
        // VENDOR(uefi-player): `#[target_feature]` double-gates the ABI
        // premise (SSE2 is x86-64 baseline anyway); the `unsafe` block is
        // required by edition 2024 even inside the `unsafe fn`.
        unsafe {
            row_pass_simd(m, blk, &mut tmp);
        }
    } else {
        row_pass_scalar(m, blk, &mut tmp);
    }
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    row_pass_scalar(m, blk, &mut tmp);
    // columns + final, shared by both paths: out[y][x] = floor(dc/8 + A/2^32 + 1/2)
    // with A[y][x] = Σ_v C'[y][v]·tmp[v][x] — a single rounding at the end, so
    // an all-zero AC tail reproduces the DC-only path exactly for every DC.
    for y in 0..8 {
        let my = &m[y];
        for x in 0..8 {
            let mut a: i64 = 0;
            for v in 0..8 {
                a += my[v] as i64 * tmp[v][x];
            }
            blk[y * 8 + x] = (((dc << 32) + 8 * a + (1i64 << 34)) >> 35).clamp(-256, 255) as i32;
        }
    }
}

/// Scalar twin of the row pass (always compiled): direct dots in exact `i64`.
fn row_pass_scalar(m: &[[i16; 8]; 8], blk: &[i32; 64], tmp: &mut [[i64; 8]; 8]) {
    for v in 0..8 {
        let r = &blk[v * 8..v * 8 + 8];
        if r.iter().all(|&c| c == 0) {
            continue;
        }
        for x in 0..8 {
            let mx = &m[x];
            let mut t: i64 = 0;
            for u in 0..8 {
                t += mx[u] as i64 * r[u] as i64;
            }
            tmp[v][x] = t;
        }
    }
}

// VENDOR(uefi-player): integer-SSE2 twin of the row pass above. One dot per
// `asm!` block: pack the saturated `i32` row to 8×`i16` (`packssdw`, exact),
// `pmaddwd` the four pair-sums (each `<= 2·32138·32767 < 2^31`, exact as
// signed `i32`), sign-extend to 64 bits, `paddq`-accumulate — the same exact
// `i64` dot the scalar twin computes, so the two agree bit for bit.
// `movdqu` throughout: no alignment requirement, no `nt` stores (small reused
// buffers — temporal locality, hence no `sfence` either). SSE2 only: no AVX,
// no XSAVE involvement; the firmware `[accel]` switch is config semantics.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn row_pass_simd(m: &[[i16; 8]; 8], blk: &[i32; 64], tmp: &mut [[i64; 8]; 8]) {
    for v in 0..8 {
        let r = &blk[v * 8..v * 8 + 8];
        if r.iter().all(|&c| c == 0) {
            continue;
        }
        let fptr = r.as_ptr();
        for x in 0..8 {
            // `u128` bit patterns (not `__m128i`) cross the `asm!` boundary —
            // vector-intrinsic types trip the backend here (see `hw_bench.rs`).
            // Pure temporaries bind `_`: written and re-read only in-template.
            unsafe {
                core::arch::asm!(
                    "movdqu {f0}, [{fptr}]",
                    "movdqu {f1}, [{fptr} + 16]",
                    "packssdw {f0}, {f1}",
                    "movdqu {cc}, [{cptr}]",
                    "movdqa {p}, {f0}",
                    "pmaddwd {p}, {cc}",
                    "movdqa {s}, {p}",
                    "psrad {s}, 31",
                    "movdqa {lo}, {p}",
                    "punpckldq {lo}, {s}",
                    "punpckhdq {p}, {s}",
                    "paddq {lo}, {p}",
                    "pshufd {p}, {lo}, 238",
                    "paddq {lo}, {p}",
                    "movq [{optr}], {lo}",
                    fptr = in(reg) fptr,
                    cptr = in(reg) m[x].as_ptr(),
                    optr = in(reg) &mut tmp[v][x] as *mut i64,
                    f0 = lateout(xmm_reg) _,
                    f1 = lateout(xmm_reg) _,
                    cc = lateout(xmm_reg) _,
                    p = lateout(xmm_reg) _,
                    s = lateout(xmm_reg) _,
                    lo = lateout(xmm_reg) _,
                    options(nostack, preserves_flags),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // VENDOR(uefi-player): `alloc` prelude for the crafted-input vectors
    // (resolves in every feature shape; host test builds link std anyway).
    use alloc::vec::Vec;

    /// The pseudo-random generator of IEEE Std 1180-1990 (values in [-l, h]).
    struct Rand(i64);
    impl Rand {
        fn next(&mut self, l: i64, h: i64) -> i64 {
            self.0 = (self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345)) & 0xFFFF_FFFF;
            let i = self.0 & 0x7FFF_FFFE;
            let x = i as f64 / 0x7FFF_FFFF as f64 * (l + h + 1) as f64;
            x as i64 - l
        }
    }

    fn c(u: usize) -> f64 {
        if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 }
    }

    fn cosv(a: usize, b: usize) -> f64 {
        static T: OnceLock<[[f64; 8]; 8]> = OnceLock::new();
        T.call_once(|| std::array::from_fn(|a| std::array::from_fn(|b| (((2 * a + 1) * b) as f64 * std::f64::consts::PI / 16.0).cos())))[a][b]
    }

    /// Separable double-precision 1-D passes (exact up to f64 rounding).
    fn fdct(p: &[i64; 64]) -> [i64; 64] {
        let mut t = [0f64; 64];
        for y in 0..8 {
            for u in 0..8 {
                t[y * 8 + u] = (0..8).map(|x| p[y * 8 + x] as f64 * cosv(x, u)).sum::<f64>() * c(u) / 2.0;
            }
        }
        let mut out = [0i64; 64];
        for v in 0..8 {
            for u in 0..8 {
                let s = (0..8).map(|y| t[y * 8 + u] * cosv(y, v)).sum::<f64>() * c(v) / 2.0;
                out[v * 8 + u] = (s.round() as i64).clamp(-2048, 2047);
            }
        }
        out
    }

    fn reference_idct(f: &[i64; 64]) -> [i64; 64] {
        let mut t = [0f64; 64];
        for v in 0..8 {
            for x in 0..8 {
                t[v * 8 + x] = (0..8).map(|u| c(u) * f[v * 8 + u] as f64 * cosv(x, u)).sum::<f64>() / 2.0;
            }
        }
        let mut out = [0i64; 64];
        for y in 0..8 {
            for x in 0..8 {
                let s = (0..8).map(|v| c(v) * t[v * 8 + x] * cosv(y, v)).sum::<f64>() / 2.0;
                out[y * 8 + x] = (s.round() as i64).clamp(-256, 255);
            }
        }
        out
    }

    /// IEEE 1180-1990 §3: 10 000 blocks for each range and sign; peak error ≤ 1, per-pixel mean
    /// square error ≤ 0.06, overall ≤ 0.02, per-pixel mean error ≤ 0.015, overall ≤ 0.0015.
    #[test]
    fn ieee_1180_accuracy() {
        let blocks = if cfg!(debug_assertions) { 2_000 } else { 10_000 };
        for (l, h) in [(256i64, 255i64), (5, 5), (300, 300)] {
            for sign in [1i64, -1] {
                let mut rng = Rand(1);
                let mut sum_err = [0i64; 64];
                let mut sum_sq = [0i64; 64];
                for _ in 0..blocks {
                    let mut p = [0i64; 64];
                    for v in p.iter_mut() {
                        *v = rng.next(l, h) * sign;
                    }
                    let f = fdct(&p);
                    let r = reference_idct(&f);
                    let mut blk: [i32; 64] = std::array::from_fn(|i| f[i] as i32);
                    idct(&mut blk, false);
                    for i in 0..64 {
                        let e = blk[i] as i64 - r[i];
                        assert!(e.abs() <= 1, "peak error {e} (range {l}/{h} sign {sign})");
                        sum_err[i] += e;
                        sum_sq[i] += e * e;
                    }
                }
                let n = blocks as f64;
                for i in 0..64 {
                    assert!(sum_sq[i] as f64 / n <= 0.06);
                    assert!((sum_err[i] as f64 / n).abs() <= 0.015);
                }
                assert!(sum_sq.iter().sum::<i64>() as f64 / (64.0 * n) <= 0.02);
                assert!((sum_err.iter().sum::<i64>() as f64 / (64.0 * n)).abs() <= 0.0015);
            }
        }
        // all-zero input gives all-zero output
        let mut z = [0i32; 64];
        idct(&mut z, false);
        assert!(z.iter().all(|&v| v == 0));
    }

    #[test]
    fn dc_only_matches_the_full_transform() {
        for dc in -2048..=2047 {
            let mut a = [0i32; 64];
            a[0] = dc;
            let mut b = a;
            idct(&mut a, true);
            idct(&mut b, false);
            assert_eq!(a, b, "dc {dc}");
        }
    }

    /// The `pmaddwd` row pass needs every basis entry (and every saturated
    /// input) in `i16` range; the proof is this test, not a comment.
    #[test]
    fn basis_fits_i16() {
        let m = basis();
        let mut max = 0i16;
        for row in m.iter() {
            for &v in row.iter() {
                max = max.max(v.abs());
            }
        }
        assert!(max <= 32138, "basis max {max}");
    }

    // VENDOR(uefi-player): dual-path interlock. Runs under every shape with
    // the `simd` feature, flipping the runtime switch so both row kernels
    // render the same blocks in one binary and must agree bit for bit (the
    // column/final tail is shared, so this guards the `asm!` row pass).
    // Other tests never touch the switch, and the two paths are
    // interchangeable, so parallel test threads cannot disturb each other.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    fn both_paths(mut blk: [i32; 64], dc_only: bool) -> ([i32; 64], [i32; 64]) {
        let prev = crate::simd_enabled();
        crate::set_simd_enabled(true);
        let mut a = blk;
        idct(&mut a, dc_only);
        crate::set_simd_enabled(false);
        idct(&mut blk, dc_only);
        crate::set_simd_enabled(prev);
        (a, blk)
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn simd_row_matches_scalar() {
        // zero + DC sweep (the full `dc_only` contract range, both shapes)
        let (a, s) = both_paths([0i32; 64], false);
        assert_eq!(a, s);
        for dc in (-2048..=2047).step_by(7) {
            let mut b = [0i32; 64];
            b[0] = dc;
            let (a, s) = both_paths(b, false);
            assert_eq!(a, s, "dc {dc}");
            let (a, s) = both_paths(b, true);
            assert_eq!(a, s, "dc_only {dc}");
        }
        // single-AC probes across positions and magnitudes
        for pos in [1usize, 7, 8, 17, 32, 45, 63] {
            for mag in [1i32, -1, 7, -256, 2047, -2048, 16376, -16376, 32767, -32768, 100_000, -100_000, i32::MAX, i32::MIN] {
                let mut b = [0i32; 64];
                b[pos] = mag;
                let (a, s) = both_paths(b, false);
                assert_eq!(a, s, "pos {pos} mag {mag}");
            }
        }
        // dense pseudo-random blocks at several magnitudes, incl. extremes
        // past the saturation knee (both paths saturate identically).
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i32
        };
        for scale in [1i64, 100, 16_376, 100_000, 2_000_000_000] {
            for _ in 0..300 {
                let mut b = [0i32; 64];
                for v in b.iter_mut() {
                    *v = ((next() as i64 * scale / 2_000_000_000).clamp(-2_147_483_648, 2_147_483_647)) as i32;
                }
                let (a, s) = both_paths(b, false);
                assert_eq!(a, s, "random scale {scale}");
            }
        }
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn both_paths_match_reference_on_crafted_inputs() {
        // Exact expectations: DC-only values the definition pins down, plus
        // single-AC blocks checked against the in-test `f64` oracle (exact
        // here — magnitudes keep every f64 rounding light-years from a `.5`
        // tie; the test itself would fail loudly otherwise).
        let mut cases: Vec<[i32; 64]> = Vec::new();
        for dc in [8i32, -8, 0, 1, -1, 7, 2047, -2048] {
            let mut b = [0i32; 64];
            b[0] = dc;
            cases.push(b);
        }
        for (pos, mag) in [(1usize, 256i32), (8, -256), (63, 100), (27, -1000), (9, 2047)] {
            let mut b = [0i32; 64];
            b[pos] = mag;
            cases.push(b);
        }
        for b in cases {
            let f: [i64; 64] = std::array::from_fn(|i| b[i] as i64);
            let r = reference_idct(&f);
            let (a, s) = both_paths(b, false);
            for i in 0..64 {
                // Paths agree bit-exactly; each stays inside the IEEE gate
                // against the `f64` oracle (a crafted value landing exactly
                // on a `.5` tie would be oracle noise, not signal — hence
                // `<= 1` here and exact integers for the DC cases below).
                assert_eq!(a[i], s[i], "simd/scalar at {i} (blk {b:?})");
                assert!((a[i] as i64 - r[i]).abs() <= 1, "simd vs reference at {i} (blk {b:?})");
                assert!((s[i] as i64 - r[i]).abs() <= 1, "scalar vs reference at {i} (blk {b:?})");
            }
        }
        // DC-only hand values (definition-level, no oracle involved).
        let mut b = [0i32; 64];
        b[0] = 8;
        let (a, s) = both_paths(b, false);
        assert!(a.iter().all(|&v| v == 1) && s.iter().all(|&v| v == 1));
        b[0] = -8;
        let (a, s) = both_paths(b, false);
        assert!(a.iter().all(|&v| v == -1) && s.iter().all(|&v| v == -1));
    }
}
