//! **Forward/inverse 4x4 transform and quantization** — portable (rip-ASM Phase 5b).
//!
//! Replaces openh264's `WelsDctFourT4`, `WelsIDctFourT4Rec` and `WelsQuantFour4x4`
//! (dct.asm 1,036 + decoder/dct.asm 72 + quant.asm 507 LOC).
//!
//! ## Scalar oracle + hand-written SSE2 (rip-ASM Phase 5b, reopened)
//!
//! The scalar forms below are the ORACLE and the non-x86 path. The first cut
//! shipped them alone and MEASURED SLOWER than the assembly on x86-64 (fast
//! preset 1.253x) — the predicted refutation of "the compiler already did it"
//! for the butterfly/transpose shapes. The `x86` module below is the reopened
//! swap: explicit SSE2 with the scalar twin pinned by `*_matches_scalar`
//! differential tests over full-range inputs. The openh264 assembly this
//! replaced is GONE (ripped 2026-08-12); the scalar twins are the standing
//! oracle. See docs/add_SIMD_rip_ASM.md.
//!
//! Everything here is bit-exact against `common`'s `forward_core` / `inverse_core`
//! and against openh264's `WELS_NEW_QUANT`, pinned by the tests that previously held
//! the assembly to those same references.

/// One-dimensional forward butterfly (a row of `Cf`). Mirrors `common::transform`.
#[inline]
fn fwd_1d(x0: i32, x1: i32, x2: i32, x3: i32) -> (i32, i32, i32, i32) {
    let (t0, t1, t2, t3) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
    (t0 + t1, 2 * t3 + t2, t0 - t1, t3 - 2 * t2)
}

/// One-dimensional inverse butterfly (a row of `Ci`).
#[inline]
fn inv_1d(d0: i32, d1: i32, d2: i32, d3: i32) -> (i32, i32, i32, i32) {
    let (e0, e1) = (d0 + d2, d0 - d2);
    let (e2, e3) = ((d1 >> 1) - d3, d1 + (d3 >> 1));
    (e0 + e3, e1 + e2, e1 - e2, e0 - e3)
}

/// `W = Cf · X · Cfᵀ` over one row-major 4x4 block.
#[inline]
fn forward_core(m: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, b, c, d) = fwd_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = fwd_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
}

/// Inverse core + the final `(x + 32) >> 6`.
///
/// ROW-FIRST, THEN COLUMN — the order is NOT interchangeable. The `>> 1` inside
/// `inv_1d` makes the integer transform non-separable, so a column-first pass diverges
/// by +/-1 on asymmetric blocks (only visible at low QP / high-frequency content).
/// Spec 8.5.12.2 fixes the order and the decoder must agree exactly.
#[inline]
fn inverse_core(m: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, b, c, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
    for v in m.iter_mut() {
        *v = (*v + 32) >> 6;
    }
}

/// The four 4x4 sub-blocks of an 8x8 region, in openh264's order.
const SUBBLOCKS: [(usize, usize); 4] = [(0, 0), (4, 0), (0, 4), (4, 4)];

/// Forward-transform the residual of four 4x4 blocks covering an 8x8 region.
///
/// `dct` receives 64 coefficients, block-major in `SUBBLOCKS` order.
pub fn dct_four_t4_scalar(dct: &mut [i16], src: &[u8], stride_src: usize, pred: &[u8], stride_pred: usize) {
    assert!(dct.len() >= 64);
    assert!(src.len() >= 7 * stride_src + 8 && pred.len() >= 7 * stride_pred + 8);
    for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
        let mut m = [0i32; 16];
        for dy in 0..4 {
            for dx in 0..4 {
                m[dy * 4 + dx] = src[(oy + dy) * stride_src + ox + dx] as i32
                    - pred[(oy + dy) * stride_pred + ox + dx] as i32;
            }
        }
        forward_core(&mut m);
        for i in 0..16 {
            dct[k * 16 + i] = m[i] as i16;
        }
    }
}

/// Inverse-transform four 4x4 coefficient blocks and add them to `pred`, writing the
/// reconstruction into `rec`.
pub fn idct_four_t4_rec_scalar(
    rec: &mut [u8], stride_rec: usize, pred: &[u8], stride_pred: usize, dct: &[i16],
) {
    assert!(dct.len() >= 64);
    assert!(rec.len() >= 7 * stride_rec + 8 && pred.len() >= 7 * stride_pred + 8);
    for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
        let mut m = [0i32; 16];
        for i in 0..16 {
            m[i] = dct[k * 16 + i] as i32;
        }
        inverse_core(&mut m);
        for dy in 0..4 {
            for dx in 0..4 {
                let v = pred[(oy + dy) * stride_pred + ox + dx] as i32 + m[dy * 4 + dx];
                rec[(oy + dy) * stride_rec + ox + dx] = v.clamp(0, 255) as u8;
            }
        }
    }
}

/// openh264's `WELS_NEW_QUANT` over four 4x4 blocks, in place.
///
/// `level = sign(c) * ((|c| + FF[pos]) * MF[pos]) >> 16`, with `pos = (row & 1)*4 + col`
/// — the tables hold two rows and are reused for rows 2/3. NOTE this is openh264's
/// quantizer (dead-zone added BEFORE the multiply, fixed `>>16`), structurally
/// different from our own `(|c|*MF + F) >> qbits`, and deliberately kept as-is.
pub fn quant_four_4x4_scalar(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
    assert!(dct.len() >= 64);
    for blk in 0..4 {
        for row in 0..4 {
            for col in 0..4 {
                let idx = blk * 16 + row * 4 + col;
                let pos = (row & 1) * 4 + col;
                let c = dct[idx] as i32;
                let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                dct[idx] = (if c < 0 { -lvl } else { lvl }) as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dct_matches_reference() {
        for seed in 0..128usize {
            let (mut src, mut pred) = ([0u8; 64], [0u8; 64]);
            for y in 0..8 {
                for x in 0..8 {
                    src[y * 8 + x] = ((y * 31 + x * 17 + seed * 7) & 0xff) as u8;
                    pred[y * 8 + x] = ((y * 13 + x * 41 + seed * 5 + 9) & 0xff) as u8;
                }
            }
            let mut dct = [0i16; 64];
            dct_four_t4_scalar(&mut dct, &src, 8, &pred, 8);
            for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
                let mut want = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        want[dy * 4 + dx] = src[(oy + dy) * 8 + ox + dx] as i32
                            - pred[(oy + dy) * 8 + ox + dx] as i32;
                    }
                }
                forward_core(&mut want);
                for i in 0..16 {
                    assert_eq!(dct[k * 16 + i] as i32, want[i], "seed {seed} blk {k} c {i}");
                }
            }
        }
    }

    #[test]
    fn idct_rec_matches_reference() {
        for seed in 0..128usize {
            let mut pred = [0u8; 64];
            let mut dct = [0i16; 64];
            for i in 0..64 {
                pred[i] = ((i * 7 + seed * 3) & 0xff) as u8;
                dct[i] = (((i as i32 * 53 + seed as i32 * 29) % 4096) - 2048) as i16;
            }
            let mut rec = [0u8; 64];
            idct_four_t4_rec_scalar(&mut rec, 8, &pred, 8, &dct);
            for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
                let mut m = [0i32; 16];
                for i in 0..16 {
                    m[i] = dct[k * 16 + i] as i32;
                }
                inverse_core(&mut m);
                for dy in 0..4 {
                    for dx in 0..4 {
                        let want = (pred[(oy + dy) * 8 + ox + dx] as i32 + m[dy * 4 + dx])
                            .clamp(0, 255) as u8;
                        assert_eq!(rec[(oy + dy) * 8 + ox + dx], want, "seed {seed} blk {k}");
                    }
                }
            }
        }
    }

    /// openh264's WELS_NEW_QUANT, spelled out independently of the implementation.
    #[test]
    fn quant_matches_openh264_c() {
        let ff: [i16; 8] = [80, 85, 80, 85, 90, 95, 90, 95];
        let mf: [i16; 8] = [410, 420, 410, 420, 430, 440, 430, 440];
        for seed in 0..64i32 {
            let mut input = [0i16; 64];
            for (k, v) in input.iter_mut().enumerate() {
                *v = (((k as i32 * 37 + seed * 53) % 2000) - 1000) as i16;
            }
            let mut got = input;
            quant_four_4x4_scalar(&mut got, &ff, &mf);
            for blk in 0..4 {
                for row in 0..4 {
                    for col in 0..4 {
                        let idx = blk * 16 + row * 4 + col;
                        let pos = (row & 1) * 4 + col;
                        let c = input[idx] as i32;
                        let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                        let want = (if c < 0 { -lvl } else { lvl }) as i16;
                        assert_eq!(got[idx], want, "seed {seed} blk {blk} ({row},{col})");
                    }
                }
            }
        }
    }

    /// The inverse transform's row-then-column order is load-bearing: a column-first
    /// pass differs by +/-1 on asymmetric blocks. Pin it so a "tidy-up" cannot swap
    /// them. Searches for a witness rather than asserting one hand-picked input
    /// differs — the first version picked an input where the two orders happened to
    /// agree, so the test passed while proving nothing.
    #[test]
    fn inverse_order_is_row_then_column() {
        fn col_first(mut m: [i32; 16]) -> [i32; 16] {
            for c in 0..4 {
                let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
                m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
            }
            for r in 0..4 {
                let (a, b, c2, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
                m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c2; m[r * 4 + 3] = d;
            }
            for v in m.iter_mut() { *v = (*v + 32) >> 6; }
            m
        }
        let mut witnesses = 0;
        for seed in 0..512i32 {
            let mut m = [0i32; 16];
            for (i, v) in m.iter_mut().enumerate() {
                *v = ((i as i32 * 71 + seed * 137) % 2048) - 1024;
            }
            let mut row = m;
            inverse_core(&mut row);
            if row != col_first(m) { witnesses += 1; }
        }
        assert!(witnesses > 0,
            "no input distinguished the two orders — this test would pass on a swapped              implementation and is therefore worthless as written");
    }
}

// ---------------------------------------------------------------------------------
// Public dispatchers. x86-64: SSE2 (baseline, nothing to detect). Other
// arches: the scalar oracle. The vendored assembly these once fronted was
// ripped 2026-08-12 (docs/add_SIMD_rip_ASM.md).
// ---------------------------------------------------------------------------------

/// MEASUREMENT KNOB (`RFF_ABL_RECON=1`): make `idct_four_t4_rec` copy the
/// prediction through (skip inverse transform + residual add) so the recon
/// stage can be priced by ablation on the uninstrumented binary. The scalar
/// twin in `common::predict` carries the same knob. Output is wrong while set.
#[inline]
fn abl_recon() -> bool {
    // ROUTED AT BUILD TIME (routing round 2026-09-05): the shipped arm is the
    // constant below; the env arm exists only under `--features knobs`.
    #[cfg(not(feature = "knobs"))]
    {
        return false;
    }
    #[cfg(all(feature = "knobs", feature = "std"))]
    {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RFF_ABL_RECON").is_some_and(|v| v != "0");
            ON.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
    }
    // VENDOR(uefi-player): no_std => the env knob reads as unset (shipped
    // default off); atomics come from `core` (same type, no_std-safe).
    #[cfg(all(feature = "knobs", not(feature = "std")))]
    {
        use core::sync::atomic::{AtomicU8, Ordering};
        static ON: AtomicU8 = AtomicU8::new(0);
        match ON.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                ON.store(2, Ordering::Relaxed);
                false
            }
        }
    }
}

/// ORACLE ARM (`RFF_TQ_SCALAR=1`): pin the three transform/quant dispatchers to
/// their scalar twins at RUNTIME — the differential/bisection anchor the
/// campaign method requires of every kernel family (`add_SIMD_rip_ASM.md` §3
/// step 1), previously satisfied here only inside `#[cfg(test)]` (H10). Output
/// is byte-identical either way — this is a correctness knob, not a speed knob;
/// same cached-atomic shape and cost class as `abl_recon` above.
#[inline]
pub fn tq_scalar_forced() -> bool {
    // ROUTED AT BUILD TIME (routing round 2026-09-05): the shipped arm is the
    // constant below; the env arm exists only under `--features knobs`.
    #[cfg(not(feature = "knobs"))]
    {
        return false;
    }
    #[cfg(all(feature = "knobs", feature = "std"))]
    {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RFF_TQ_SCALAR").is_some_and(|v| v != "0");
            ON.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
    }
    // VENDOR(uefi-player): no_std => the env knob reads as unset (shipped
    // default off); atomics come from `core` (same type, no_std-safe).
    #[cfg(all(feature = "knobs", not(feature = "std")))]
    {
        use core::sync::atomic::{AtomicU8, Ordering};
        static ON: AtomicU8 = AtomicU8::new(0);
        match ON.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                ON.store(2, Ordering::Relaxed);
                false
            }
        }
    }
}

/// Forward 4x4 transform of an 8x8 region's residual (four blocks, `SUBBLOCKS`
/// order). See [`dct_four_t4_scalar`] for the reference semantics.
pub fn dct_four_t4(dct: &mut [i16], src: &[u8], stride_src: usize, pred: &[u8], stride_pred: usize) {
    assert!(dct.len() >= 64);
    assert!(src.len() >= 7 * stride_src + 8 && pred.len() >= 7 * stride_pred + 8);
    if tq_scalar_forced() {
        return dct_four_t4_scalar(dct, src, stride_src, pred, stride_pred);
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: bounds asserted above; SSE2 is the x86-64 baseline.
    return unsafe { x86::dct_four_t4_sse2(dct, src, stride_src, pred, stride_pred) };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: bounds asserted above; NEON is the aarch64 baseline.
    return unsafe { arm::dct_four_t4_neon(dct, src, stride_src, pred, stride_pred) };
    #[allow(unreachable_code)]
    dct_four_t4_scalar(dct, src, stride_src, pred, stride_pred)
}

/// Inverse 4x4 transform + reconstruct of an 8x8 region. See
/// [`idct_four_t4_rec_scalar`] for the reference semantics.
pub fn idct_four_t4_rec(
    rec: &mut [u8], stride_rec: usize, pred: &[u8], stride_pred: usize, dct: &[i16],
) {
    assert!(dct.len() >= 64);
    assert!(rec.len() >= 7 * stride_rec + 8 && pred.len() >= 7 * stride_pred + 8);
    if abl_recon() {
        for r in 0..8 {
            rec[r * stride_rec..r * stride_rec + 8]
                .copy_from_slice(&pred[r * stride_pred..r * stride_pred + 8]);
        }
        return;
    }
    if tq_scalar_forced() {
        return idct_four_t4_rec_scalar(rec, stride_rec, pred, stride_pred, dct);
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: bounds asserted above; SSE2 is the x86-64 baseline.
        crate::census::IDCT_FOUR_T4.base();
        return unsafe { x86::idct_four_t4_rec_sse2(rec, stride_rec, pred, stride_pred, dct) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: bounds asserted above; NEON is the aarch64 baseline.
        crate::census::IDCT_FOUR_T4.base();
        return unsafe { arm::idct_four_t4_rec_neon(rec, stride_rec, pred, stride_pred, dct) };
    }
    #[allow(unreachable_code)]
    crate::census::IDCT_FOUR_T4.scalar();
    idct_four_t4_rec_scalar(rec, stride_rec, pred, stride_pred, dct)
}

/// openh264 `WELS_NEW_QUANT` over four 4x4 blocks, in place. See
/// [`quant_four_4x4_scalar`] for the reference semantics.
pub fn quant_four_4x4(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
    assert!(dct.len() >= 64);
    if tq_scalar_forced() {
        return quant_four_4x4_scalar(dct, ff, mf);
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: bounds asserted above; SSE2 is the x86-64 baseline.
    return unsafe { x86::quant_four_4x4_sse2(dct, ff, mf) };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: bounds asserted above; NEON is the aarch64 baseline.
    return unsafe { arm::quant_four_4x4_neon(dct, ff, mf) };
    #[allow(unreachable_code)]
    quant_four_4x4_scalar(dct, ff, mf)
}

/// Hand-written SSE2 twins. Every function is pinned to its scalar oracle by the
/// `*_matches_scalar` differential tests below over full-range inputs.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    /// Quant: `((|c| + ff) * mf) >> 16` with sign restore. The unsigned-16
    /// domain makes `pmulhuw` EXACT: `|c| + ff <= 32767 + 32767 = 65534` never
    /// wraps u16, the 32-bit product never wraps, and `pmulhuw` is precisely
    /// the `>> 16`. Even `c = -32768` matches the scalar (its `max(c, -c)` bit
    /// pattern reinterprets as u16 32768, the same value the scalar's i32
    /// `abs()` produces). Layout: 8 consecutive i16 = one row-PAIR of a block,
    /// whose position indices are exactly `ff[0..8]`/`mf[0..8]`.
    pub(super) unsafe fn quant_four_4x4_sse2(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
        let ffv = _mm_loadu_si128(ff.as_ptr() as *const __m128i);
        let mfv = _mm_loadu_si128(mf.as_ptr() as *const __m128i);
        let zero = _mm_setzero_si128();
        for i in 0..8 {
            let p = dct.as_mut_ptr().add(i * 8) as *mut __m128i;
            let c = _mm_loadu_si128(p);
            let a = _mm_max_epi16(c, _mm_sub_epi16(zero, c)); // |c| (u16 view exact)
            let lvl = _mm_mulhi_epu16(_mm_add_epi16(a, ffv), mfv);
            let m = _mm_srai_epi16(c, 15);
            _mm_storeu_si128(p, _mm_sub_epi16(_mm_xor_si128(lvl, m), m));
        }
    }

    /// Forward butterfly, lane-wise over i16 (residual <= +-255 keeps every
    /// intermediate under +-6120 - i16-safe). No shifts, so the row/column
    /// order is interchangeable and both passes use this one shape.
    #[inline(always)]
    unsafe fn fwd_pass(
        x0: __m128i, x1: __m128i, x2: __m128i, x3: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i) {
        let t0 = _mm_add_epi16(x0, x3);
        let t1 = _mm_add_epi16(x1, x2);
        let t2 = _mm_sub_epi16(x1, x2);
        let t3 = _mm_sub_epi16(x0, x3);
        (
            _mm_add_epi16(t0, t1),
            _mm_add_epi16(_mm_add_epi16(t3, t3), t2),
            _mm_sub_epi16(t0, t1),
            _mm_sub_epi16(t3, _mm_add_epi16(t2, t2)),
        )
    }

    /// Transpose a PAIR of 4x4 i16 blocks held as 4 rows of `[L | R]` lanes.
    #[inline(always)]
    unsafe fn transpose_pair(
        a: __m128i, b: __m128i, c: __m128i, d: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i) {
        let t0 = _mm_unpacklo_epi16(a, b); // L00 L10 L01 L11 L02 L12 L03 L13
        let t1 = _mm_unpackhi_epi16(a, b); // R side
        let t2 = _mm_unpacklo_epi16(c, d);
        let t3 = _mm_unpackhi_epi16(c, d);
        let u0 = _mm_unpacklo_epi32(t0, t2); // Lc0 | Lc1
        let u1 = _mm_unpackhi_epi32(t0, t2); // Lc2 | Lc3
        let u2 = _mm_unpacklo_epi32(t1, t3); // Rc0 | Rc1
        let u3 = _mm_unpackhi_epi32(t1, t3); // Rc2 | Rc3
        (
            _mm_unpacklo_epi64(u0, u2), // Lc0 | Rc0
            _mm_unpackhi_epi64(u0, u2), // Lc1 | Rc1
            _mm_unpacklo_epi64(u1, u3), // Lc2 | Rc2
            _mm_unpackhi_epi64(u1, u3), // Lc3 | Rc3
        )
    }

    #[inline(always)]
    unsafe fn load_resid_row(src: *const u8, pred: *const u8) -> __m128i {
        let zero = _mm_setzero_si128();
        let s = _mm_unpacklo_epi8(_mm_loadl_epi64(src as *const __m128i), zero);
        let p = _mm_unpacklo_epi8(_mm_loadl_epi64(pred as *const __m128i), zero);
        _mm_sub_epi16(s, p)
    }

    /// Store one transform row-pair reg `[Lrow | Rrow]` into block-major output.
    #[inline(always)]
    unsafe fn store_row_pair(dct: *mut i16, kl: usize, kr: usize, row: usize, v: __m128i) {
        _mm_storel_epi64(dct.add(kl * 16 + row * 4) as *mut __m128i, v);
        _mm_storel_epi64(
            dct.add(kr * 16 + row * 4) as *mut __m128i,
            _mm_unpackhi_epi64(v, v),
        );
    }

    pub(super) unsafe fn dct_four_t4_sse2(
        dct: &mut [i16], src: &[u8], ss: usize, pred: &[u8], ps: usize,
    ) {
        // Two block-pairs: rows 0..4 feed blocks 0 (left) and 1 (right); rows
        // 4..8 feed blocks 2 and 3 (`SUBBLOCKS` order).
        for (pair, (kl, kr)) in [(0usize, (0usize, 1usize)), (1, (2, 3))] {
            let base = pair * 4;
            let r0 = load_resid_row(src.as_ptr().add(base * ss), pred.as_ptr().add(base * ps));
            let r1 = load_resid_row(src.as_ptr().add((base + 1) * ss), pred.as_ptr().add((base + 1) * ps));
            let r2 = load_resid_row(src.as_ptr().add((base + 2) * ss), pred.as_ptr().add((base + 2) * ps));
            let r3 = load_resid_row(src.as_ptr().add((base + 3) * ss), pred.as_ptr().add((base + 3) * ps));
            // Column pass (lane-wise), then transpose, then the row pass (also
            // lane-wise on the transposed data), then transpose back to raster.
            let (c0, c1, c2, c3) = fwd_pass(r0, r1, r2, r3);
            let (t0, t1, t2, t3) = transpose_pair(c0, c1, c2, c3);
            let (o0, o1, o2, o3) = fwd_pass(t0, t1, t2, t3);
            let (w0, w1, w2, w3) = transpose_pair(o0, o1, o2, o3);
            let d = dct.as_mut_ptr();
            store_row_pair(d, kl, kr, 0, w0);
            store_row_pair(d, kl, kr, 1, w1);
            store_row_pair(d, kl, kr, 2, w2);
            store_row_pair(d, kl, kr, 3, w3);
        }
    }

    /// Inverse butterfly, lane-wise over i32 (the `>> 1` forces the widened
    /// domain and pins the row-then-column order - see the scalar's doc).
    #[inline(always)]
    unsafe fn inv_pass(
        d0: __m128i, d1: __m128i, d2: __m128i, d3: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i) {
        let e0 = _mm_add_epi32(d0, d2);
        let e1 = _mm_sub_epi32(d0, d2);
        let e2 = _mm_sub_epi32(_mm_srai_epi32(d1, 1), d3);
        let e3 = _mm_add_epi32(d1, _mm_srai_epi32(d3, 1));
        (
            _mm_add_epi32(e0, e3),
            _mm_add_epi32(e1, e2),
            _mm_sub_epi32(e1, e2),
            _mm_sub_epi32(e0, e3),
        )
    }

    #[inline(always)]
    unsafe fn transpose4_epi32(
        a: __m128i, b: __m128i, c: __m128i, d: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i) {
        let t0 = _mm_unpacklo_epi32(a, b);
        let t1 = _mm_unpackhi_epi32(a, b);
        let t2 = _mm_unpacklo_epi32(c, d);
        let t3 = _mm_unpackhi_epi32(c, d);
        (
            _mm_unpacklo_epi64(t0, t2),
            _mm_unpackhi_epi64(t0, t2),
            _mm_unpacklo_epi64(t1, t3),
            _mm_unpackhi_epi64(t1, t3),
        )
    }

    /// Sign-extend 4 i16 (low half of `v`) to 4 i32 lanes (SSE2 form).
    #[inline(always)]
    unsafe fn widen_lo(v: __m128i) -> __m128i {
        _mm_srai_epi32(_mm_unpacklo_epi16(v, v), 16)
    }

    pub(super) unsafe fn idct_four_t4_rec_sse2(
        rec: &mut [u8], rs: usize, pred: &[u8], ps: usize, dct: &[i16],
    ) {
        const SUB: [(usize, usize); 4] = [(0, 0), (4, 0), (0, 4), (4, 4)];
        let zero = _mm_setzero_si128();
        for (k, (ox, oy)) in SUB.iter().enumerate() {
            let lo = _mm_loadu_si128(dct.as_ptr().add(k * 16) as *const __m128i);
            let hi = _mm_loadu_si128(dct.as_ptr().add(k * 16 + 8) as *const __m128i);
            let r0 = widen_lo(lo);
            let r1 = widen_lo(_mm_unpackhi_epi64(lo, lo));
            let r2 = widen_lo(hi);
            let r3 = widen_lo(_mm_unpackhi_epi64(hi, hi));
            // ROW pass first (spec order): transpose so rows sit lane-wise,
            // butterfly, transpose back, then the column pass is lane-wise.
            let (t0, t1, t2, t3) = transpose4_epi32(r0, r1, r2, r3);
            let (a0, a1, a2, a3) = inv_pass(t0, t1, t2, t3);
            let (u0, u1, u2, u3) = transpose4_epi32(a0, a1, a2, a3);
            let (b0, b1, b2, b3) = inv_pass(u0, u1, u2, u3);
            let round = _mm_set1_epi32(32);
            for (dy, m) in [b0, b1, b2, b3].into_iter().enumerate() {
                let v = _mm_srai_epi32(_mm_add_epi32(m, round), 6);
                let pp = pred.as_ptr().add((oy + dy) * ps + ox);
                let p32 = {
                    let p8 = _mm_cvtsi32_si128((pp as *const i32).read_unaligned());
                    _mm_unpacklo_epi16(_mm_unpacklo_epi8(p8, zero), zero)
                };
                let sum = _mm_add_epi32(v, p32);
                // clamp 0..255 via packs (i32->i16 saturate) + packus (i16->u8 saturate)
                let p16 = _mm_packs_epi32(sum, sum);
                let p8 = _mm_packus_epi16(p16, p16);
                let out = _mm_cvtsi128_si32(p8) as u32;
                (rec.as_mut_ptr().add((oy + dy) * rs + ox) as *mut u32).write_unaligned(out);
            }
        }
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod arm_tests {
    use super::*;

    fn lcg(state: &mut u64) -> u32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*state >> 33) as u32
    }

    #[test]
    fn neon_trio_matches_scalar_full_range() {
        let mut st = 0x4242u64;
        for round in 0..2000usize {
            let mut ff = [0i16; 8];
            let mut mf = [0i16; 8];
            for i in 0..8 {
                ff[i] = (lcg(&mut st) % 32768) as i16;
                mf[i] = (lcg(&mut st) % 32768) as i16;
            }
            let mut input = [0i16; 64];
            for v in input.iter_mut() {
                *v = lcg(&mut st) as i16;
            }
            if round == 0 {
                input[0] = i16::MIN;
                input[1] = i16::MAX;
            }
            let mut a = input;
            let mut b = input;
            quant_four_4x4_scalar(&mut a, &ff, &mf);
            unsafe { super::arm::quant_four_4x4_neon(&mut b, &ff, &mf) };
            assert_eq!(a, b, "quant round {round}");

            let mut src = [0u8; 64];
            let mut pred = [0u8; 64];
            for i in 0..64 {
                src[i] = lcg(&mut st) as u8;
                pred[i] = lcg(&mut st) as u8;
            }
            let mut da = [0i16; 64];
            let mut db = [0i16; 64];
            dct_four_t4_scalar(&mut da, &src, 8, &pred, 8);
            unsafe { super::arm::dct_four_t4_neon(&mut db, &src, 8, &pred, 8) };
            assert_eq!(da, db, "dct round {round}");

            let mut coeffs = [0i16; 64];
            for v in coeffs.iter_mut() {
                *v = lcg(&mut st) as i16;
            }
            let mut ra = [0u8; 64];
            let mut rb = [0u8; 64];
            idct_four_t4_rec_scalar(&mut ra, 8, &pred, 8, &coeffs);
            unsafe { super::arm::idct_four_t4_rec_neon(&mut rb, 8, &pred, 8, &coeffs) };
            assert_eq!(ra, rb, "idct round {round}");
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod x86_tests {
    use super::*;

    fn lcg(state: &mut u64) -> u32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*state >> 33) as u32
    }

    #[test]
    fn quant_sse2_matches_scalar_full_range() {
        let mut st = 0x1234u64;
        for round in 0..2000usize {
            let mut ff = [0i16; 8];
            let mut mf = [0i16; 8];
            for i in 0..8 {
                ff[i] = (lcg(&mut st) % 32768) as i16;
                mf[i] = (lcg(&mut st) % 32768) as i16;
            }
            let mut input = [0i16; 64];
            for v in input.iter_mut() {
                *v = lcg(&mut st) as i16; // full i16 range, incl. -32768
            }
            if round == 0 {
                input[0] = i16::MIN;
                input[1] = i16::MAX;
                input[2] = 0;
            }
            let mut a = input;
            let mut b = input;
            quant_four_4x4_scalar(&mut a, &ff, &mf);
            unsafe { super::x86::quant_four_4x4_sse2(&mut b, &ff, &mf) };
            assert_eq!(a, b, "round {round}");
        }
    }

    #[test]
    fn dct_sse2_matches_scalar() {
        let mut st = 0xbeefu64;
        for round in 0..2000usize {
            let mut src = [0u8; 64];
            let mut pred = [0u8; 64];
            for i in 0..64 {
                src[i] = lcg(&mut st) as u8;
                pred[i] = lcg(&mut st) as u8;
            }
            if round == 0 {
                src = [255; 64];
                pred = [0; 64];
            }
            let mut a = [0i16; 64];
            let mut b = [0i16; 64];
            dct_four_t4_scalar(&mut a, &src, 8, &pred, 8);
            unsafe { super::x86::dct_four_t4_sse2(&mut b, &src, 8, &pred, 8) };
            assert_eq!(a, b, "round {round}");
        }
    }

    #[test]
    fn idct_sse2_matches_scalar_full_range() {
        let mut st = 0xfeedu64;
        for round in 0..2000usize {
            let mut pred = [0u8; 64];
            let mut dct = [0i16; 64];
            for i in 0..64 {
                pred[i] = lcg(&mut st) as u8;
                dct[i] = lcg(&mut st) as i16; // full range: saturation must match too
            }
            if round == 0 {
                dct = [i16::MAX; 64];
            }
            if round == 1 {
                dct = [i16::MIN; 64];
            }
            let mut a = [0u8; 64];
            let mut b = [0u8; 64];
            idct_four_t4_rec_scalar(&mut a, 8, &pred, 8, &dct);
            unsafe { super::x86::idct_four_t4_rec_sse2(&mut b, 8, &pred, 8, &dct) };
            assert_eq!(a, b, "round {round}");
        }
    }
}

/// aarch64 NEON twins — the same shapes as the SSE2 module (NEON is the
/// aarch64 baseline, nothing to detect). Pinned by the same scalar-oracle
/// differential tests, which are arch-agnostic and run this module on the
/// first aarch64 test build.
#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    /// Quant: same unsigned-16 argument as the SSE2 twin — `|c| + ff` never
    /// wraps u16, and the widening `vmull_u16` + `>> 16` narrow is exactly the
    /// scalar's `((|c|+ff)*mf) >> 16`. `vabsq_s16(-32768) = -32768` whose u16
    /// bit pattern is 32768, matching the scalar's i32 `abs()` — same edge the
    /// SSE2 comment proves.
    pub(super) unsafe fn quant_four_4x4_neon(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
        let ffv = vreinterpretq_u16_s16(vld1q_s16(ff.as_ptr()));
        let mfv = vreinterpretq_u16_s16(vld1q_s16(mf.as_ptr()));
        for i in 0..8 {
            let p = dct.as_mut_ptr().add(i * 8);
            let c = vld1q_s16(p);
            let a = vreinterpretq_u16_s16(vabsq_s16(c));
            let s = vaddq_u16(a, ffv);
            let lo = vshrn_n_u32::<16>(vmull_u16(vget_low_u16(s), vget_low_u16(mfv)));
            let hi = vshrn_n_u32::<16>(vmull_u16(vget_high_u16(s), vget_high_u16(mfv)));
            let lvl = vreinterpretq_s16_u16(vcombine_u16(lo, hi));
            let m = vshrq_n_s16::<15>(c);
            vst1q_s16(p, vsubq_s16(veorq_s16(lvl, m), m));
        }
    }

    #[inline(always)]
    unsafe fn fwd_pass(
        x0: int16x8_t, x1: int16x8_t, x2: int16x8_t, x3: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        let t0 = vaddq_s16(x0, x3);
        let t1 = vaddq_s16(x1, x2);
        let t2 = vsubq_s16(x1, x2);
        let t3 = vsubq_s16(x0, x3);
        (
            vaddq_s16(t0, t1),
            vaddq_s16(vaddq_s16(t3, t3), t2),
            vsubq_s16(t0, t1),
            vsubq_s16(t3, vaddq_s16(t2, t2)),
        )
    }

    /// Transpose a PAIR of 4x4 i16 blocks held as 4 rows of `[L | R]` lanes —
    /// the SSE2 unpack sequence with vzip1q/vzip2q + 64-bit half recombines.
    #[inline(always)]
    unsafe fn transpose_pair(
        a: int16x8_t, b: int16x8_t, c: int16x8_t, d: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        let t0 = vzip1q_s16(a, b);
        let t1 = vzip2q_s16(a, b);
        let t2 = vzip1q_s16(c, d);
        let t3 = vzip2q_s16(c, d);
        let u0 = vreinterpretq_s16_s32(vzip1q_s32(vreinterpretq_s32_s16(t0), vreinterpretq_s32_s16(t2)));
        let u1 = vreinterpretq_s16_s32(vzip2q_s32(vreinterpretq_s32_s16(t0), vreinterpretq_s32_s16(t2)));
        let u2 = vreinterpretq_s16_s32(vzip1q_s32(vreinterpretq_s32_s16(t1), vreinterpretq_s32_s16(t3)));
        let u3 = vreinterpretq_s16_s32(vzip2q_s32(vreinterpretq_s32_s16(t1), vreinterpretq_s32_s16(t3)));
        (
            vcombine_s16(vget_low_s16(u0), vget_low_s16(u2)),
            vcombine_s16(vget_high_s16(u0), vget_high_s16(u2)),
            vcombine_s16(vget_low_s16(u1), vget_low_s16(u3)),
            vcombine_s16(vget_high_s16(u1), vget_high_s16(u3)),
        )
    }

    #[inline(always)]
    unsafe fn load_resid_row(src: *const u8, pred: *const u8) -> int16x8_t {
        let s = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(src)));
        let p = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(pred)));
        vsubq_s16(s, p)
    }

    #[inline(always)]
    unsafe fn store_row_pair(dct: *mut i16, kl: usize, kr: usize, row: usize, v: int16x8_t) {
        vst1_s16(dct.add(kl * 16 + row * 4), vget_low_s16(v));
        vst1_s16(dct.add(kr * 16 + row * 4), vget_high_s16(v));
    }

    pub(super) unsafe fn dct_four_t4_neon(
        dct: &mut [i16], src: &[u8], ss: usize, pred: &[u8], ps: usize,
    ) {
        for (pair, (kl, kr)) in [(0usize, (0usize, 1usize)), (1, (2, 3))] {
            let base = pair * 4;
            let r0 = load_resid_row(src.as_ptr().add(base * ss), pred.as_ptr().add(base * ps));
            let r1 = load_resid_row(src.as_ptr().add((base + 1) * ss), pred.as_ptr().add((base + 1) * ps));
            let r2 = load_resid_row(src.as_ptr().add((base + 2) * ss), pred.as_ptr().add((base + 2) * ps));
            let r3 = load_resid_row(src.as_ptr().add((base + 3) * ss), pred.as_ptr().add((base + 3) * ps));
            let (c0, c1, c2, c3) = fwd_pass(r0, r1, r2, r3);
            let (t0, t1, t2, t3) = transpose_pair(c0, c1, c2, c3);
            let (o0, o1, o2, o3) = fwd_pass(t0, t1, t2, t3);
            let (w0, w1, w2, w3) = transpose_pair(o0, o1, o2, o3);
            let d = dct.as_mut_ptr();
            store_row_pair(d, kl, kr, 0, w0);
            store_row_pair(d, kl, kr, 1, w1);
            store_row_pair(d, kl, kr, 2, w2);
            store_row_pair(d, kl, kr, 3, w3);
        }
    }

    #[inline(always)]
    unsafe fn inv_pass(
        d0: int32x4_t, d1: int32x4_t, d2: int32x4_t, d3: int32x4_t,
    ) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let e0 = vaddq_s32(d0, d2);
        let e1 = vsubq_s32(d0, d2);
        let e2 = vsubq_s32(vshrq_n_s32::<1>(d1), d3);
        let e3 = vaddq_s32(d1, vshrq_n_s32::<1>(d3));
        (
            vaddq_s32(e0, e3),
            vaddq_s32(e1, e2),
            vsubq_s32(e1, e2),
            vsubq_s32(e0, e3),
        )
    }

    #[inline(always)]
    unsafe fn transpose4_s32(
        a: int32x4_t, b: int32x4_t, c: int32x4_t, d: int32x4_t,
    ) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let t0 = vzip1q_s32(a, b);
        let t1 = vzip2q_s32(a, b);
        let t2 = vzip1q_s32(c, d);
        let t3 = vzip2q_s32(c, d);
        (
            vcombine_s32(vget_low_s32(t0), vget_low_s32(t2)),
            vcombine_s32(vget_high_s32(t0), vget_high_s32(t2)),
            vcombine_s32(vget_low_s32(t1), vget_low_s32(t3)),
            vcombine_s32(vget_high_s32(t1), vget_high_s32(t3)),
        )
    }

    pub(super) unsafe fn idct_four_t4_rec_neon(
        rec: &mut [u8], rs: usize, pred: &[u8], ps: usize, dct: &[i16],
    ) {
        const SUB: [(usize, usize); 4] = [(0, 0), (4, 0), (0, 4), (4, 4)];
        for (k, (ox, oy)) in SUB.iter().enumerate() {
            let base = dct.as_ptr().add(k * 16);
            let r0 = vmovl_s16(vld1_s16(base));
            let r1 = vmovl_s16(vld1_s16(base.add(4)));
            let r2 = vmovl_s16(vld1_s16(base.add(8)));
            let r3 = vmovl_s16(vld1_s16(base.add(12)));
            // ROW pass first (spec order), via transpose — same as the SSE2 twin.
            let (t0, t1, t2, t3) = transpose4_s32(r0, r1, r2, r3);
            let (a0, a1, a2, a3) = inv_pass(t0, t1, t2, t3);
            let (u0, u1, u2, u3) = transpose4_s32(a0, a1, a2, a3);
            let (b0, b1, b2, b3) = inv_pass(u0, u1, u2, u3);
            let round = vdupq_n_s32(32);
            for (dy, m) in [b0, b1, b2, b3].into_iter().enumerate() {
                let v = vshrq_n_s32::<6>(vaddq_s32(m, round));
                let pp = pred.as_ptr().add((oy + dy) * ps + ox);
                let pw = pp.cast::<u32>().read_unaligned();
                let p8 = vreinterpret_u8_u32(vset_lane_u32::<0>(pw, vdup_n_u32(0)));
                let p32 = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(vmovl_u8(p8))));
                let sum = vaddq_s32(v, p32);
                // clamp 0..255: saturating narrow i32->i16 then i16->u8.
                let p16 = vqmovn_s32(sum);
                let out8 = vqmovun_s16(vcombine_s16(p16, p16));
                let out = vget_lane_u32::<0>(vreinterpret_u32_u8(out8));
                rec.as_mut_ptr().add((oy + dy) * rs + ox).cast::<u32>().write_unaligned(out);
            }
        }
    }
}
