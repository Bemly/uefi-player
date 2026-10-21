//! H.264 **deblocking loop filter** — portable SIMD (rip-ASM Phase 3).
//!
//! Replaces openh264's `DeblockLuma{Lt4,Eq4}{V,H}_ssse3`, `DeblockChroma{Lt4,Eq4}{V,H}`
//! and the `DeblockLumaTranspose{H2V,V2H}` helpers. Deblock is 5–10% of decode.
//!
//! ## Shape of the problem (H.264 §8.7.2.3–4)
//!
//! Every line across an edge is filtered independently, so N lines vectorise trivially
//! — *if* the samples for one line lie in one lane. For a **horizontal edge** (`_v`
//! kernels, filtering vertically) that is already true: p3..q3 are successive rows, so
//! 16 columns sit in 16 lanes. For a **vertical edge** (`_h` kernels) p3..q3 run along
//! the row, so the block must be transposed first — which is exactly what the assembly
//! did, and why it shipped two transpose kernels.
//!
//! ## Conventions inherited from the assembly
//!
//! * **LUMA and CHROMA use DIFFERENT `tc` conventions, and conflating them is a silent
//!   wrong-pixels bug** — it cost this module a 0/18 gate failure on first submission.
//!     - **Luma**: `tc[i]` is the raw `tc0`; the kernel adds the `ap<beta` / `aq<beta`
//!       increments itself. `-1` means **skip that group** (bS==0), and the vector
//!       kernel must mask those lanes because it filters all lanes at once.
//!     - **Chroma**: the caller has ALREADY applied the spec's `tc = tc0 + 1`, so the
//!       kernel uses the value as-is and must NOT add one again. Skip is encoded as
//!       **`0`**, which needs no mask: clipping delta to `[-0, 0]` yields no change.
//!   Groups are 4 columns for luma, 2 for chroma.
//! * `eq4` is only ever invoked when **all** groups are bS==4.
//! * The `_v` entry points are handed `p3` (luma) / `p1` (chroma) — i.e. the pointer is
//!   already backed up to the first sample the filter reads, not to q0.
//!
//! ## Why everything is `i16`
//!
//! Inputs are 8-bit. The widest intermediate is `2*p3 + 3*p2 + p1 + p0 + q0 + 4`
//! (eq4's p2 output) at `8*255 + 4 = 2044`, and the lt4 delta bottoms out at
//! `-4*255 - 255 = -1275`. Everything fits `i16` with room to spare, so all lanes stay
//! 16-bit and the final `packus` performs the `clip1` for free.
//!
//! SSE2 only — no `_mm_abs_epi16` (SSSE3); `|x|` is `max(x, -x)`, which SSE2 has for
//! signed 16-bit.

// ---------------------------------------------------------------------------------
// Scalar reference — mirrors the §8.7.2 per-line filters exactly, plus the tc<0
// skip convention the vector kernels must honour. On x86-64 (SSE2) and aarch64
// (NEON) the dispatch always takes the SIMD arm, so this family is only reached
// there as the oracle in the `*_matches_scalar` tests — hence the per-fn
// dead_code allowance; on every other target it IS the production path.
// ---------------------------------------------------------------------------------

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
#[inline]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
#[inline]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// One luma line, bS<4. `idx(k)` maps k in -4..=3 to a buffer index.
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_lt4_line(px: &mut [u8], idx: &dyn Fn(isize) -> usize, alpha: i32, beta: i32, tc0: i32) {
    if tc0 < 0 {
        return;
    }
    let g = |k: isize| px[idx(k)] as i32;
    let (p0, p1, p2) = (g(-1), g(-2), g(-3));
    let (q0, q1, q2) = (g(0), g(1), g(2));
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let (ap, aq) = ((p2 - p0).abs(), (q2 - q0).abs());
    let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
    let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
    px[idx(-1)] = clip1(p0 + delta);
    px[idx(0)] = clip1(q0 - delta);
    if ap < beta {
        let d = clip3(-tc0, tc0, (p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1);
        px[idx(-2)] = clip1(p1 + d);
    }
    if aq < beta {
        let d = clip3(-tc0, tc0, (q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1);
        px[idx(1)] = clip1(q1 + d);
    }
}

/// One luma line, bS==4.
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_eq4_line(px: &mut [u8], idx: &dyn Fn(isize) -> usize, alpha: i32, beta: i32) {
    let g = |k: isize| px[idx(k)] as i32;
    let (p0, p1, p2, p3) = (g(-1), g(-2), g(-3), g(-4));
    let (q0, q1, q2, q3) = (g(0), g(1), g(2), g(3));
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let (ap, aq) = ((p2 - p0).abs(), (q2 - q0).abs());
    let strong = (p0 - q0).abs() < (alpha >> 2) + 2;
    if strong && ap < beta {
        px[idx(-1)] = clip1((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3);
        px[idx(-2)] = clip1((p2 + p1 + p0 + q0 + 2) >> 2);
        px[idx(-3)] = clip1((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3);
    } else {
        px[idx(-1)] = clip1((2 * p1 + p0 + q1 + 2) >> 2);
    }
    if strong && aq < beta {
        px[idx(0)] = clip1((q2 + 2 * q1 + 2 * q0 + 2 * p0 + p1 + 4) >> 3);
        px[idx(1)] = clip1((q2 + q1 + q0 + p0 + 2) >> 2);
        px[idx(2)] = clip1((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3);
    } else {
        px[idx(0)] = clip1((2 * q1 + q0 + p1 + 2) >> 2);
    }
}

/// `tc` here is the spec's chroma `tc0 + 1`, ALREADY incremented by the caller.
/// `tc == 0` means bS==0 (skip) — equivalent to clipping delta to zero.
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_lt4_line(px: &mut [u8], idx: &dyn Fn(isize) -> usize, alpha: i32, beta: i32, tc: i32) {
    if tc <= 0 {
        return;
    }
    let g = |k: isize| px[idx(k)] as i32;
    let (p0, p1, q0, q1) = (g(-1), g(-2), g(0), g(1));
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
    px[idx(-1)] = clip1(p0 + delta);
    px[idx(0)] = clip1(q0 - delta);
}

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_eq4_line(px: &mut [u8], idx: &dyn Fn(isize) -> usize, alpha: i32, beta: i32) {
    let g = |k: isize| px[idx(k)] as i32;
    let (p0, p1, q0, q1) = (g(-1), g(-2), g(0), g(1));
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    px[idx(-1)] = clip1((2 * p1 + p0 + q1 + 2) >> 2);
    px[idx(0)] = clip1((2 * q1 + q0 + p1 + 2) >> 2);
}

// --- scalar whole-edge drivers (the fallback, and the test oracle) -----------------

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_lt4_v_scalar(p3: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    for c in 0..16 {
        let t = tc[c / 4] as i32;
        let idx = |k: isize| ((4 + k) as usize) * stride + c;
        luma_lt4_line(p3, &idx, alpha, beta, t);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_eq4_v_scalar(p3: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    for c in 0..16 {
        let idx = |k: isize| ((4 + k) as usize) * stride + c;
        luma_eq4_line(p3, &idx, alpha, beta);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_lt4_h_scalar(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    for r in 0..16 {
        let t = tc[r / 4] as i32;
        let idx = |k: isize| r * stride + (4 + k) as usize;
        luma_lt4_line(p4, &idx, alpha, beta, t);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn luma_eq4_h_scalar(p4: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    for r in 0..16 {
        let idx = |k: isize| r * stride + (4 + k) as usize;
        luma_eq4_line(p4, &idx, alpha, beta);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_lt4_v_scalar(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    for c in 0..8 {
        let t = tc[c / 2] as i32;
        let idx = |k: isize| ((2 + k) as usize) * stride + c;
        chroma_lt4_line(p1, &idx, alpha, beta, t);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_eq4_v_scalar(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    for c in 0..8 {
        let idx = |k: isize| ((2 + k) as usize) * stride + c;
        chroma_eq4_line(p1, &idx, alpha, beta);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_lt4_h_scalar(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    for r in 0..8 {
        let t = tc[r / 2] as i32;
        let idx = |k: isize| r * stride + (2 + k) as usize;
        chroma_lt4_line(p1, &idx, alpha, beta, t);
    }
}
#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn chroma_eq4_h_scalar(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    for r in 0..8 {
        let idx = |k: isize| r * stride + (2 + k) as usize;
        chroma_eq4_line(p1, &idx, alpha, beta);
    }
}

// ---------------------------------------------------------------------------------
// Public API — same signatures the assembly wrappers had.
//
// The vector work lives in `sse2`; every entry point falls back to the scalar driver
// when the feature is absent, and on non-x86 targets the scalar driver IS the path
// until a NEON version lands (the filter is branch-heavy per lane, so it is the least
// profitable of the three kernel families to vectorise twice).
// ---------------------------------------------------------------------------------

/// Call the SSE2 kernel on x86-64, the scalar driver everywhere else.
///
/// SSE2 is part of the x86-64 baseline ABI, so there is NOTHING to detect and the
/// kernels are NOT `#[target_feature]`-gated. That matters: a `#[target_feature]`
/// function cannot be inlined into a normal caller, and these entry points are hit
/// ~13M times over a 300-frame clip. The first version paid a real call plus a
/// feature check every time and measured 1.30-1.37x SLOWER than the assembly it
/// replaced; the arithmetic inside was never the problem.
macro_rules! dispatch {
    ($f:ident, $scalar:path, $twin:ident, ($($a:expr),*)) => {{
        // VENDOR(uefi-player): firmware single-bit gate — off means the scalar
        // twin. One choke point covering all 8 public entries (via the `_one`
        // helpers for chroma).
        if !crate::simd_enabled() {
            crate::census::$twin.scalar();
            $scalar($($a),*);
            return;
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: caller-asserted bounds; kernels touch only the documented window.
        { crate::census::$twin.base(); unsafe { sse2::$f($($a),*) } }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above; NEON is the aarch64 baseline (nothing to detect),
        // so like SSE2 there is no #[target_feature] inlining barrier.
        { crate::census::$twin.base(); unsafe { arm::$f($($a),*) } }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        { crate::census::$twin.scalar(); $scalar($($a),*) }
    }};
}

/// Luma, horizontal edge (filter vertically), bS<4. `p3` points at the p3 row.
pub fn deblock_luma_lt4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(p3.len() >= 7 * stride + 16);
    dispatch!(luma_lt4_v, luma_lt4_v_scalar, DBLK_LUMA_LT4_V, (p3, stride, alpha, beta, tc));
}
/// Luma, horizontal edge, bS==4.
pub fn deblock_luma_eq4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(p3.len() >= 7 * stride + 16);
    dispatch!(luma_eq4_v, luma_eq4_v_scalar, DBLK_LUMA_EQ4_V, (p3, stride, alpha, beta));
}
/// Luma, vertical edge (filter horizontally), bS<4. `p4` points at column p3 of row 0.
pub fn deblock_luma_lt4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(p4.len() >= 15 * stride + 8);
    dispatch!(luma_lt4_h, luma_lt4_h_scalar, DBLK_LUMA_LT4_H, (p4, stride, alpha, beta, tc));
}
/// Luma, vertical edge, bS==4.
pub fn deblock_luma_eq4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(p4.len() >= 15 * stride + 8);
    dispatch!(luma_eq4_h, luma_eq4_h_scalar, DBLK_LUMA_EQ4_H, (p4, stride, alpha, beta));
}

/// Chroma (both planes), horizontal edge, bS<4. `*_p1` point at the p1 row.
pub fn deblock_chroma_lt4_v(
    cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4],
) {
    assert!(cb_p1.len() >= 3 * stride + 8 && cr_p1.len() >= 3 * stride + 8);
    chroma_lt4_v_one(cb_p1, stride, alpha, beta, tc);
    chroma_lt4_v_one(cr_p1, stride, alpha, beta, tc);
}
/// Chroma (both planes), horizontal edge, bS==4.
pub fn deblock_chroma_eq4_v(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(cb_p1.len() >= 3 * stride + 8 && cr_p1.len() >= 3 * stride + 8);
    chroma_eq4_v_one(cb_p1, stride, alpha, beta);
    chroma_eq4_v_one(cr_p1, stride, alpha, beta);
}
/// Chroma (both planes), vertical edge, bS<4. `*_p1` point at column p1 of row 0
/// (q0 is at column 2). NOTE: scalar for now — a vertical chroma edge is 8 rows x 2
/// planes, and the transpose needed to vectorise it costs more than the 4 filtered
/// samples per row save. Measured before leaving it this way.
pub fn deblock_chroma_lt4_h(
    cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4],
) {
    assert!(cb_p1.len() >= 7 * stride + 4 && cr_p1.len() >= 7 * stride + 4);
    chroma_lt4_h_one(cb_p1, stride, alpha, beta, tc);
    chroma_lt4_h_one(cr_p1, stride, alpha, beta, tc);
}
/// Chroma (both planes), vertical edge, bS==4.
pub fn deblock_chroma_eq4_h(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(cb_p1.len() >= 7 * stride + 4 && cr_p1.len() >= 7 * stride + 4);
    chroma_eq4_h_one(cb_p1, stride, alpha, beta);
    chroma_eq4_h_one(cr_p1, stride, alpha, beta);
}

// One plane each. `dispatch!` expands to a `return` on the SIMD arm, so these MUST be
// separate functions -- two dispatch! calls in one body would filter Cb and then return
// without ever touching Cr. That mistake shipped 0/18 on the byte-identity gate.
fn chroma_lt4_h_one(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    dispatch!(chroma_lt4_h, chroma_lt4_h_scalar, DBLK_CHR_LT4_H, (p1, stride, alpha, beta, tc));
}
fn chroma_eq4_h_one(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    dispatch!(chroma_eq4_h, chroma_eq4_h_scalar, DBLK_CHR_EQ4_H, (p1, stride, alpha, beta));
}
fn chroma_lt4_v_one(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    dispatch!(chroma_lt4_v, chroma_lt4_v_scalar, DBLK_CHR_LT4_V, (p1, stride, alpha, beta, tc));
}
fn chroma_eq4_v_one(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    dispatch!(chroma_eq4_v, chroma_eq4_v_scalar, DBLK_CHR_EQ4_V, (p1, stride, alpha, beta));
}

// ---------------------------------------------------------------------------------
// SSE2
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    #[inline(always)]
    unsafe fn ld(p: *const u8) -> __m128i {
        _mm_unpacklo_epi8(_mm_loadl_epi64(p as *const __m128i), _mm_setzero_si128())
    }
    #[inline(always)]
    unsafe fn st(p: *mut u8, v: __m128i) {
        _mm_storel_epi64(p as *mut __m128i, _mm_packus_epi16(v, v));
    }
    /// |a - b| for signed 16-bit lanes, SSE2-only (no `_mm_abs_epi16`).
    #[inline(always)]
    unsafe fn absdiff(a: __m128i, b: __m128i) -> __m128i {
        // SSSE3 `pabsw`: 2 ops instead of sub + neg + max (the build is x86-64-v3).
        _mm_abs_epi16(_mm_sub_epi16(a, b))
    }
    /// `v.clamp(-t, t)`
    #[inline(always)]
    unsafe fn clip3v(v: __m128i, t: __m128i) -> __m128i {
        _mm_min_epi16(_mm_max_epi16(v, _mm_sub_epi16(_mm_setzero_si128(), t)), t)
    }
    /// select(mask, a, b) — mask lanes are all-ones or all-zero.
    #[inline(always)]
    unsafe fn sel(mask: __m128i, a: __m128i, b: __m128i) -> __m128i {
        // SSE4.1 `pblendvb`: one op for and/andnot/or (mask lanes are all-ones or zero).
        _mm_blendv_epi8(b, a, mask)
    }

    /// The eight lanes' worth of lt4 luma filtering. Returns the four updated rows.
    #[inline(always)]
    unsafe fn lt4_core(
        p2: __m128i, p1: __m128i, p0: __m128i, q0: __m128i, q1: __m128i, q2: __m128i,
        alpha: __m128i, beta: __m128i, tc0: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i) {
        // tc0 < 0 marks a skipped group (bS==0) — the vector kernel must mask it.
        let live = _mm_cmpgt_epi16(tc0, _mm_set1_epi16(-1));
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1, p0), absdiff(q1, q0));
        let mut m = _mm_cmpgt_epi16(alpha, absdiff(p0, q0));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(beta, dmax));
        m = _mm_and_si128(m, live);

        let apm = _mm_cmpgt_epi16(beta, absdiff(p2, p0));
        let aqm = _mm_cmpgt_epi16(beta, absdiff(q2, q0));
        let one = _mm_set1_epi16(1);
        // tc = tc0 + (ap<beta) + (aq<beta). A set mask lane is -1, so SUBTRACTING the
        // mask itself adds 1. (Subtracting `mask & 1` would subtract, not add — that
        // was the first version's bug, and the differential test caught it.)
        let tc = _mm_sub_epi16(_mm_sub_epi16(tc0, apm), aqm);

        let d = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(_mm_slli_epi16::<2>(_mm_sub_epi16(q0, p0)), _mm_sub_epi16(p1, q1)),
            _mm_set1_epi16(4),
        ));
        let delta = _mm_and_si128(clip3v(d, tc), m);
        let np0 = _mm_add_epi16(p0, delta);
        let nq0 = _mm_sub_epi16(q0, delta);

        // p1/q1 move only where their own |p2-p0| / |q2-q0| test also passed.
        let avg = _mm_srai_epi16::<1>(_mm_add_epi16(_mm_add_epi16(p0, q0), one));
        let dp = _mm_srai_epi16::<1>(_mm_sub_epi16(_mm_add_epi16(p2, avg), _mm_slli_epi16::<1>(p1)));
        let dq = _mm_srai_epi16::<1>(_mm_sub_epi16(_mm_add_epi16(q2, avg), _mm_slli_epi16::<1>(q1)));
        let np1 = _mm_add_epi16(p1, _mm_and_si128(clip3v(dp, tc0), _mm_and_si128(m, apm)));
        let nq1 = _mm_add_epi16(q1, _mm_and_si128(clip3v(dq, tc0), _mm_and_si128(m, aqm)));
        (np1, np0, nq0, nq1)
    }

    /// bS==4 core. Returns p2,p1,p0,q0,q1,q2.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    unsafe fn eq4_core(
        p3: __m128i, p2: __m128i, p1: __m128i, p0: __m128i,
        q0: __m128i, q1: __m128i, q2: __m128i, q3: __m128i,
        alpha: __m128i, beta: __m128i,
    ) -> (__m128i, __m128i, __m128i, __m128i, __m128i, __m128i) {
        let two = _mm_set1_epi16(2);
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1, p0), absdiff(q1, q0));
        let mut m = _mm_cmpgt_epi16(alpha, absdiff(p0, q0));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(beta, dmax));
        // strong = |p0-q0| < (alpha>>2) + 2
        let thr = _mm_add_epi16(_mm_srai_epi16::<2>(alpha), two);
        let strong = _mm_cmpgt_epi16(thr, absdiff(p0, q0));
        let sp = _mm_and_si128(strong, _mm_cmpgt_epi16(beta, absdiff(p2, p0)));
        let sq = _mm_and_si128(strong, _mm_cmpgt_epi16(beta, absdiff(q2, q0)));

        let s2 = |a: __m128i| _mm_slli_epi16::<1>(a);
        // strong p side
        let p0s = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(_mm_add_epi16(p2, s2(p1)), _mm_add_epi16(s2(p0), s2(q0))),
            _mm_add_epi16(q1, _mm_set1_epi16(4)),
        ));
        let p1s = _mm_srai_epi16::<2>(_mm_add_epi16(
            _mm_add_epi16(_mm_add_epi16(p2, p1), _mm_add_epi16(p0, q0)), two,
        ));
        let p2s = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(s2(p3), _mm_add_epi16(_mm_add_epi16(p2, s2(p2)), p1)),
            _mm_add_epi16(_mm_add_epi16(p0, q0), _mm_set1_epi16(4)),
        ));
        // weak p side
        let p0w = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s2(p1), p0), _mm_add_epi16(q1, two)));

        let q0s = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(_mm_add_epi16(q2, s2(q1)), _mm_add_epi16(s2(q0), s2(p0))),
            _mm_add_epi16(p1, _mm_set1_epi16(4)),
        ));
        let q1s = _mm_srai_epi16::<2>(_mm_add_epi16(
            _mm_add_epi16(_mm_add_epi16(q2, q1), _mm_add_epi16(q0, p0)), two,
        ));
        let q2s = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(s2(q3), _mm_add_epi16(_mm_add_epi16(q2, s2(q2)), q1)),
            _mm_add_epi16(_mm_add_epi16(p0, q0), _mm_set1_epi16(4)),
        ));
        let q0w = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s2(q1), q0), _mm_add_epi16(p1, two)));

        let np0 = sel(m, sel(sp, p0s, p0w), p0);
        let np1 = sel(_mm_and_si128(m, sp), p1s, p1);
        let np2 = sel(_mm_and_si128(m, sp), p2s, p2);
        let nq0 = sel(m, sel(sq, q0s, q0w), q0);
        let nq1 = sel(_mm_and_si128(m, sq), q1s, q1);
        let nq2 = sel(_mm_and_si128(m, sq), q2s, q2);
        (np2, np1, np0, nq0, nq1, nq2)
    }

    /// Expand `tc[4]` to 8 i16 lanes, `per` columns per group, starting at group `g0`.
    #[inline(always)]
    unsafe fn tc_lanes(tc: &[i8; 4], per: usize, g0: usize) -> __m128i {
        // In registers (kernel round 2026-09-05): the [i16; 8] round trip through
        // the stack (8 stores + 1 load per edge call) becomes 3-5 ops.
        if per == 4 {
            // [t0 x4 | t1 x4]
            _mm_unpacklo_epi64(_mm_set1_epi16(tc[g0] as i16), _mm_set1_epi16(tc[g0 + 1] as i16))
        } else {
            // per == 2: [t0,t0,t1,t1,t2,t2,t3,t3] from the four sign-extended bytes.
            let x = _mm_cvtepi8_epi16(_mm_cvtsi32_si128(i32::from_le_bytes([tc[0] as u8, tc[1] as u8, tc[2] as u8, tc[3] as u8])));
            _mm_unpacklo_epi16(x, x)
        }
    }

    #[inline(always)]
    pub unsafe fn luma_lt4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let base = p3.as_mut_ptr();
        for half in 0..2 {
            let c = base.add(half * 8);
            let r = |k: usize| c.add(k * stride);
            let (np1, np0, nq0, nq1) = lt4_core(
                ld(r(1)), ld(r(2)), ld(r(3)), ld(r(4)), ld(r(5)), ld(r(6)),
                a, b, tc_lanes(tc, 4, half * 2),
            );
            st(r(2), np1); st(r(3), np0); st(r(4), nq0); st(r(5), nq1);
        }
    }

    #[inline(always)]
    pub unsafe fn luma_eq4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let base = p3.as_mut_ptr();
        for half in 0..2 {
            let c = base.add(half * 8);
            let r = |k: usize| c.add(k * stride);
            let (np2, np1, np0, nq0, nq1, nq2) = eq4_core(
                ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)),
                ld(r(4)), ld(r(5)), ld(r(6)), ld(r(7)), a, b,
            );
            st(r(1), np2); st(r(2), np1); st(r(3), np0);
            st(r(4), nq0); st(r(5), nq1); st(r(6), nq2);
        }
    }

    #[inline(always)]
    pub unsafe fn chroma_lt4_v(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let tcv = tc_lanes(tc, 2, 0);
        // WIN: the tcv>0 term is REDUNDANT in the CHROMA kernels. `delta` is
        // `clip3v(d, tcv) & m`, and clip3v is `min(max(d, -tcv), tcv)`, so at
        // tcv == 0 it already yields 0 on exactly the lanes this term masked.
        // Chroma tc is `tc0 + 1` with tc0 >= 0 (0 marks bS == 0), so tcv is never
        // negative -- the only case where the clip would not collapse.
        // NOT valid in `lt4_core`: there tc0 < 0 marks the skip and
        // tc = tc0 + ap + aq can climb back to >= 0, so its `live` mask is
        // load-bearing. Removing it there fails `luma_v_matches_scalar`.
        let base = p1.as_mut_ptr();
        let r = |k: usize| base.add(k * stride);
        // chroma tc is tc0+1 and both p1/q1 are untouched, so lt4_core's p1/q1 outputs
        // are discarded; feeding p1/q1 as the p2/q2 slots keeps ap/aq inert (they only
        // gate the discarded outputs).
        // tcv is ALREADY tc0+1 (caller-applied); 0 marks bS==0.
        let (p1v, p0v, q0v, q1v) = (ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)));
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, dmax));
        let d = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(_mm_slli_epi16::<2>(_mm_sub_epi16(q0v, p0v)), _mm_sub_epi16(p1v, q1v)),
            _mm_set1_epi16(4),
        ));
        let delta = _mm_and_si128(clip3v(d, tcv), m);
        st(r(1), _mm_add_epi16(p0v, delta));
        st(r(2), _mm_sub_epi16(q0v, delta));
    }

    #[inline(always)]
    pub unsafe fn chroma_eq4_v(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let base = p1.as_mut_ptr();
        let r = |k: usize| base.add(k * stride);
        let (p1v, p0v, q0v, q1v) = (ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)));
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, dmax));
        // WIN: (p1 + q1 + 2) is common to both outputs -- build it once.
        //   np0 = (2*p1 + p0 + q1 + 2) >> 2 == (s + p1 + p0) >> 2
        //   nq0 = (2*q1 + q0 + p1 + 2) >> 2 == (s + q1 + q0) >> 2
        // Two adds per output instead of a shift plus three adds. Max
        // intermediate is 2*255 + 255 + 2 = 767, still inside i16.
        let s = _mm_add_epi16(_mm_add_epi16(p1v, q1v), _mm_set1_epi16(2));
        let np0 = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s, p1v), p0v));
        let nq0 = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s, q1v), q0v));
        st(r(1), sel(m, np0, p0v));
        st(r(2), sel(m, nq0, q0v));
    }

    /// Vertical edges: transpose the 16x8 window, run the `_v` kernel, transpose back.
    /// This is exactly what the assembly's Transpose{H2V,V2H} pair did.
    /// 8x8 byte transpose network. Inputs: eight 8-byte rows (low halves).
    /// Output `v[k]`: low 8 bytes = column 2k, high 8 bytes = column 2k+1.
    /// Applying it again to `[v0, v0>>64, v1, v1>>64, ...]` inverts it.
    #[inline(always)]
    unsafe fn transpose8(a: [__m128i; 8]) -> [__m128i; 4] {
        let t0 = _mm_unpacklo_epi8(a[0], a[1]);
        let t1 = _mm_unpacklo_epi8(a[2], a[3]);
        let t2 = _mm_unpacklo_epi8(a[4], a[5]);
        let t3 = _mm_unpacklo_epi8(a[6], a[7]);
        let u0 = _mm_unpacklo_epi16(t0, t1);
        let u1 = _mm_unpackhi_epi16(t0, t1);
        let u2 = _mm_unpacklo_epi16(t2, t3);
        let u3 = _mm_unpackhi_epi16(t2, t3);
        [
            _mm_unpacklo_epi32(u0, u2), _mm_unpackhi_epi32(u0, u2),
            _mm_unpacklo_epi32(u1, u3), _mm_unpackhi_epi32(u1, u3),
        ]
    }

    /// Vertical edges, IN REGISTERS (kernel round 2026-09-05). The previous form
    /// transposed the 16x8 window into a 128-byte buffer, ran the `_v` kernel on
    /// the buffer (12 loads, 8 stores) and transposed back through memory (16
    /// loads, 16 stores). Each 8-row half now stays in registers: 8 loads,
    /// transpose, filter on the two 8-byte column halves, transpose back, 8
    /// stores. Same network, same filter core, same tc mapping -- bit-identical.
    #[inline(always)]
    unsafe fn luma_h_regs<const EQ4: bool>(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let zero = _mm_setzero_si128();
        let (av, bv) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let base = p4.as_mut_ptr();
        for h in 0..2 {
            let rows: [__m128i; 8] = core::array::from_fn(|i| {
                _mm_loadl_epi64(base.add((h * 8 + i) * stride) as *const __m128i)
            });
            let mut v = transpose8(rows);
            // v[0] = (p3 | p2), v[1] = (p1 | p0), v[2] = (q0 | q1), v[3] = (q2 | q3)
            let lo = |x: __m128i| _mm_unpacklo_epi8(x, zero);
            let hi = |x: __m128i| _mm_unpackhi_epi8(x, zero);
            let (p3, p2, p1, p0) = (lo(v[0]), hi(v[0]), lo(v[1]), hi(v[1]));
            let (q0, q1, q2, q3) = (lo(v[2]), hi(v[2]), lo(v[3]), hi(v[3]));
            if EQ4 {
                let (np2, np1, np0, nq0, nq1, nq2) = eq4_core(p3, p2, p1, p0, q0, q1, q2, q3, av, bv);
                v[0] = _mm_packus_epi16(p3, np2);
                v[1] = _mm_packus_epi16(np1, np0);
                v[2] = _mm_packus_epi16(nq0, nq1);
                v[3] = _mm_packus_epi16(nq2, q3);
            } else {
                let (np1, np0, nq0, nq1) = lt4_core(p2, p1, p0, q0, q1, q2, av, bv, tc_lanes(tc, 4, h * 2));
                // WIN: only p1,p0,q0,q1 change. We are transposed here, so each LANE
                // of these is a ROW -- exactly `store_8x4`'s shape. Writing the four
                // modified columns at `+2` skips the whole second 8x8 transpose and
                // the eight 8-byte stores, which were re-writing p3/p2/q2/q3 with the
                // bytes already in memory. EQ4 keeps the full path: it modifies six
                // of the eight columns, and is 1.4M calls against lt4's 31.9M.
                store_8x4(base.add(h * 8 * stride + 2), stride, np1, np0, nq0, nq1);
                continue;
            }
            let back = transpose8([
                v[0], _mm_srli_si128::<8>(v[0]), v[1], _mm_srli_si128::<8>(v[1]),
                v[2], _mm_srli_si128::<8>(v[2]), v[3], _mm_srli_si128::<8>(v[3]),
            ]);
            for (k, w) in back.iter().enumerate() {
                let r = h * 8 + k * 2;
                _mm_storel_epi64(base.add(r * stride) as *mut __m128i, *w);
                _mm_storel_epi64(base.add((r + 1) * stride) as *mut __m128i, _mm_srli_si128::<8>(*w));
            }
        }
    }

    #[inline(always)]
    pub unsafe fn luma_lt4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        luma_h_regs::<false>(p4, stride, alpha, beta, tc);
    }

    #[inline(always)]
    pub unsafe fn luma_eq4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        luma_h_regs::<true>(p4, stride, alpha, beta, &[0; 4]);
    }

    // -- chroma vertical edges -------------------------------------------------------
    //
    // These were left scalar in the first cut and that, not the transposes, was the
    // bulk of the 1.34x regression: the scalar drivers index through a
    // `&dyn Fn(isize) -> usize`, so every single sample access is a VIRTUAL CALL.
    // Fine for a cold fallback, ruinous on a hot path. A chroma vertical edge is only
    // 8 rows x 4 columns, so one 8x4 transpose puts each column in a register and the
    // same lane-parallel filter used by the `_v` kernels applies unchanged.

    /// 8 rows x 4 cols -> (cols 0|1 packed, cols 2|3 packed), 8 bytes per column half.
    #[inline(always)]
    unsafe fn transpose_8x4(src: *const u8, stride: usize) -> (__m128i, __m128i) {
        let mut a = [_mm_setzero_si128(); 8];
        for (i, slot) in a.iter_mut().enumerate() {
            *slot = _mm_cvtsi32_si128(src.add(i * stride).cast::<u32>().read_unaligned() as i32);
        }
        let t0 = _mm_unpacklo_epi8(a[0], a[1]);
        let t1 = _mm_unpacklo_epi8(a[2], a[3]);
        let t2 = _mm_unpacklo_epi8(a[4], a[5]);
        let t3 = _mm_unpacklo_epi8(a[6], a[7]);
        let u0 = _mm_unpacklo_epi16(t0, t1);
        let u1 = _mm_unpacklo_epi16(t2, t3);
        (_mm_unpacklo_epi32(u0, u1), _mm_unpackhi_epi32(u0, u1))
    }

    /// Inverse of `transpose_8x4`: four filtered columns (i16 lanes) back to 8 rows.
    #[inline(always)]
    /// Store ONLY the two middle columns (p0, q0) of an 8x4 chroma group.
    ///
    /// The chroma filters never modify p1 or q1, so `store_8x4` was packing and
    /// writing two columns that already held the right bytes. One u16 per row at
    /// `+1` covers bytes 1..=2 and leaves p1/q1 alone.
    unsafe fn store_8x2_mid(dst: *mut u8, stride: usize, c1: __m128i, c2: __m128i) {
        let a = _mm_packus_epi16(c1, c2); // [col1 x8 | col2 x8]
        let inter = _mm_unpacklo_epi8(a, _mm_srli_si128::<8>(a)); // (col1,col2) per row
        macro_rules! put {
            ($r:expr) => {
                dst.add($r * stride + 1)
                    .cast::<u16>()
                    .write_unaligned(_mm_extract_epi16::<$r>(inter) as u16)
            };
        }
        put!(0); put!(1); put!(2); put!(3); put!(4); put!(5); put!(6); put!(7);
    }

    unsafe fn store_8x4(dst: *mut u8, stride: usize, c0: __m128i, c1: __m128i, c2: __m128i, c3: __m128i) {
        let a = _mm_packus_epi16(c0, c1); // [col0 x8 | col1 x8]
        let b = _mm_packus_epi16(c2, c3); // [col2 x8 | col3 x8]
        let lo = _mm_unpacklo_epi8(a, _mm_srli_si128::<8>(a)); // (col0,col1) per row
        let hi = _mm_unpacklo_epi8(b, _mm_srli_si128::<8>(b)); // (col2,col3) per row
        let r03 = _mm_unpacklo_epi16(lo, hi); // rows 0..3, 4 bytes each
        let r47 = _mm_unpackhi_epi16(lo, hi); // rows 4..7
        for r in 0..8 {
            let src = if r < 4 { r03 } else { r47 };
            let lane = match r & 3 {
                0 => _mm_cvtsi128_si32(src),
                1 => _mm_cvtsi128_si32(_mm_srli_si128::<4>(src)),
                2 => _mm_cvtsi128_si32(_mm_srli_si128::<8>(src)),
                _ => _mm_cvtsi128_si32(_mm_srli_si128::<12>(src)),
            };
            dst.add(r * stride).cast::<u32>().write_unaligned(lane as u32);
        }
    }

    /// Widen the two packed halves of `transpose_8x4` into p1,p0,q0,q1 i16 lanes.
    #[inline(always)]
    unsafe fn spread_8x4(lo: __m128i, hi: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let z = _mm_setzero_si128();
        // WIN: `unpacklo(srli::<8>(v), 0)` zero-extends v's HIGH eight bytes, which is
        // exactly what `unpackhi(v, 0)` does in one instruction. Two byte-shifts drop
        // out of every 8x4 spread, and the chroma _h kernels do one per plane.
        (
            _mm_unpacklo_epi8(lo, z),
            _mm_unpackhi_epi8(lo, z),
            _mm_unpacklo_epi8(hi, z),
            _mm_unpackhi_epi8(hi, z),
        )
    }

    #[inline(always)]
    pub unsafe fn chroma_lt4_h(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        let tcv = tc_lanes(tc, 2, 0);
        // WIN: the tcv>0 term is REDUNDANT in the CHROMA kernels. `delta` is
        // `clip3v(d, tcv) & m`, and clip3v is `min(max(d, -tcv), tcv)`, so at
        // tcv == 0 it already yields 0 on exactly the lanes this term masked.
        // Chroma tc is `tc0 + 1` with tc0 >= 0 (0 marks bS == 0), so tcv is never
        // negative -- the only case where the clip would not collapse.
        // NOT valid in `lt4_core`: there tc0 < 0 marks the skip and
        // tc = tc0 + ap + aq can climb back to >= 0, so its `live` mask is
        // load-bearing. Removing it there fails `luma_v_matches_scalar`.
        let base = p1.as_mut_ptr();
        let (lo, hi) = transpose_8x4(base, stride);
        let (p1v, p0v, q0v, q1v) = spread_8x4(lo, hi);
        // tcv is ALREADY tc0+1 (caller-applied); 0 marks bS==0. Rows, not columns, are
        // the groups here, and `tc_lanes` indexes lanes -- which after the transpose
        // ARE the rows. Same 2-per-group mapping.
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, dmax));
        let d = _mm_srai_epi16::<3>(_mm_add_epi16(
            _mm_add_epi16(_mm_slli_epi16::<2>(_mm_sub_epi16(q0v, p0v)), _mm_sub_epi16(p1v, q1v)),
            _mm_set1_epi16(4)));
        let delta = _mm_and_si128(clip3v(d, tcv), m);
        store_8x2_mid(base, stride, _mm_add_epi16(p0v, delta), _mm_sub_epi16(q0v, delta));
    }

    #[inline(always)]
    pub unsafe fn chroma_eq4_h(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let base = p1.as_mut_ptr();
        let (lo, hi) = transpose_8x4(base, stride);
        let (p1v, p0v, q0v, q1v) = spread_8x4(lo, hi);
        let (a, b) = (_mm_set1_epi16(alpha as i16), _mm_set1_epi16(beta as i16));
        // WIN: (beta > d1) & (beta > d2) == beta > max(d1, d2). One compare and
        // one AND become one max. Both diffs are >= 0 and < 256, so signed max
        // is exact.
        let dmax = _mm_max_epi16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = _mm_cmpgt_epi16(a, absdiff(p0v, q0v));
        m = _mm_and_si128(m, _mm_cmpgt_epi16(b, dmax));
        // WIN: (p1 + q1 + 2) is common to both outputs -- build it once.
        //   np0 = (2*p1 + p0 + q1 + 2) >> 2 == (s + p1 + p0) >> 2
        //   nq0 = (2*q1 + q0 + p1 + 2) >> 2 == (s + q1 + q0) >> 2
        // Two adds per output instead of a shift plus three adds. Max
        // intermediate is 2*255 + 255 + 2 = 767, still inside i16.
        let s = _mm_add_epi16(_mm_add_epi16(p1v, q1v), _mm_set1_epi16(2));
        let np0 = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s, p1v), p0v));
        let nq0 = _mm_srai_epi16::<2>(_mm_add_epi16(_mm_add_epi16(s, q1v), q0v));
        store_8x2_mid(base, stride, sel(m, np0, p0v), sel(m, nq0, q0v));
    }
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
    /// Smooth-ish data: the filter's guards only pass when neighbours are CLOSE, so a
    /// uniformly random block would leave almost every lane unfiltered and the test
    /// would pass without exercising the arithmetic at all.
    fn fill_smooth(buf: &mut [u8], mut seed: u32, spread: u8) {
        for b in buf.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = 128u8.wrapping_add(((seed >> 24) as u8) % spread.max(1)).wrapping_sub(spread / 2);
        }
    }

    #[test]
    fn luma_v_matches_scalar() {
        for &spread in &[4u8, 12, 40, 255] {
            for seed in 0..8u32 {
                for tcset in [[0i8, 1, 2, 3], [-1, 2, -1, 1], [3, 3, 3, 3], [-1, -1, -1, -1]] {
                    let stride = 24;
                    let mut a = vec![0u8; 8 * stride + 16];
                    if spread == 255 { fill(&mut a, seed) } else { fill_smooth(&mut a, seed, spread) }
                    let mut b = a.clone();
                    deblock_luma_lt4_v(&mut a, stride, 20, 6, &tcset);
                    luma_lt4_v_scalar(&mut b, stride, 20, 6, &tcset);
                    assert_eq!(a, b, "luma lt4 v spread={spread} seed={seed} tc={tcset:?}");

                    let mut c = vec![0u8; 8 * stride + 16];
                    if spread == 255 { fill(&mut c, seed) } else { fill_smooth(&mut c, seed, spread) }
                    let mut d = c.clone();
                    deblock_luma_eq4_v(&mut c, stride, 20, 6);
                    luma_eq4_v_scalar(&mut d, stride, 20, 6);
                    assert_eq!(c, d, "luma eq4 v spread={spread} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn luma_h_matches_scalar() {
        for &spread in &[4u8, 12, 40, 255] {
            for seed in 0..8u32 {
                for tcset in [[0i8, 1, 2, 3], [-1, 2, -1, 1], [3, 3, 3, 3]] {
                    let stride = 24;
                    let mut a = vec![0u8; 16 * stride + 16];
                    if spread == 255 { fill(&mut a, seed) } else { fill_smooth(&mut a, seed, spread) }
                    let mut b = a.clone();
                    deblock_luma_lt4_h(&mut a, stride, 20, 6, &tcset);
                    luma_lt4_h_scalar(&mut b, stride, 20, 6, &tcset);
                    assert_eq!(a, b, "luma lt4 h spread={spread} seed={seed} tc={tcset:?}");

                    let mut c = vec![0u8; 16 * stride + 16];
                    if spread == 255 { fill(&mut c, seed) } else { fill_smooth(&mut c, seed, spread) }
                    let mut d = c.clone();
                    deblock_luma_eq4_h(&mut c, stride, 20, 6);
                    luma_eq4_h_scalar(&mut d, stride, 20, 6);
                    assert_eq!(c, d, "luma eq4 h spread={spread} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn chroma_matches_scalar() {
        for &spread in &[4u8, 12, 40, 255] {
            for seed in 0..8u32 {
                // chroma convention: value is tc0+1 already, 0 = skip
                for tcset in [[1i8, 2, 3, 4], [0, 3, 0, 2], [0, 0, 0, 0]] {
                    let stride = 16;
                    let mut a = vec![0u8; 4 * stride + 8];
                    if spread == 255 { fill(&mut a, seed) } else { fill_smooth(&mut a, seed, spread) }
                    let mut b = a.clone();
                    let mut a2 = a.clone();
                    let mut b2 = a.clone();
                    deblock_chroma_lt4_v(&mut a, &mut a2, stride, 20, 6, &tcset);
                    chroma_lt4_v_scalar(&mut b, stride, 20, 6, &tcset);
                    chroma_lt4_v_scalar(&mut b2, stride, 20, 6, &tcset);
                    assert_eq!(a, b, "chroma lt4 v cb spread={spread} seed={seed}");
                    assert_eq!(a2, b2, "chroma lt4 v cr spread={spread} seed={seed}");

                    let mut c = vec![0u8; 4 * stride + 8];
                    if spread == 255 { fill(&mut c, seed) } else { fill_smooth(&mut c, seed, spread) }
                    let mut d = c.clone();
                    let mut c2 = c.clone();
                    let mut d2 = c.clone();
                    deblock_chroma_eq4_v(&mut c, &mut c2, stride, 20, 6);
                    chroma_eq4_v_scalar(&mut d, stride, 20, 6);
                    chroma_eq4_v_scalar(&mut d2, stride, 20, 6);
                    assert_eq!(c, d, "chroma eq4 v cb spread={spread} seed={seed}");
                    assert_eq!(c2, d2, "chroma eq4 v cr spread={spread} seed={seed}");
                }
            }
        }
    }

    /// Chroma VERTICAL edges (the `_h` kernels). Absent from the first version, which
    /// is why a two-plane dispatch bug reached the byte-identity gate untested.
    #[test]
    fn chroma_h_matches_scalar() {
        for &spread in &[4u8, 12, 40, 255] {
            for seed in 0..8u32 {
                for tcset in [[1i8, 2, 3, 4], [0, 3, 0, 2], [0, 0, 0, 0]] {
                    let stride = 16;
                    let mut cb = vec![0u8; 8 * stride + 8];
                    if spread == 255 { fill(&mut cb, seed) } else { fill_smooth(&mut cb, seed, spread) }
                    let mut cr = cb.clone();
                    let (mut rb, mut rr) = (cb.clone(), cb.clone());
                    deblock_chroma_lt4_h(&mut cb, &mut cr, stride, 20, 6, &tcset);
                    chroma_lt4_h_scalar(&mut rb, stride, 20, 6, &tcset);
                    chroma_lt4_h_scalar(&mut rr, stride, 20, 6, &tcset);
                    assert_eq!(cb, rb, "chroma lt4 h Cb spread={spread} seed={seed}");
                    assert_eq!(cr, rr, "chroma lt4 h Cr spread={spread} seed={seed}");

                    let mut eb = vec![0u8; 8 * stride + 8];
                    if spread == 255 { fill(&mut eb, seed) } else { fill_smooth(&mut eb, seed, spread) }
                    let mut er = eb.clone();
                    let (mut sb, mut sr) = (eb.clone(), eb.clone());
                    deblock_chroma_eq4_h(&mut eb, &mut er, stride, 20, 6);
                    chroma_eq4_h_scalar(&mut sb, stride, 20, 6);
                    chroma_eq4_h_scalar(&mut sr, stride, 20, 6);
                    assert_eq!(eb, sb, "chroma eq4 h Cb spread={spread} seed={seed}");
                    assert_eq!(er, sr, "chroma eq4 h Cr spread={spread} seed={seed}");
                }
            }
        }
    }

    /// Sweep alpha/beta so the guard boundaries themselves are crossed.
    #[test]
    fn threshold_boundaries_match() {
        for alpha in [0i32, 4, 15, 20, 63, 255] {
            for beta in [0i32, 2, 6, 18] {
                let stride = 24;
                let mut a = vec![0u8; 8 * stride + 16];
                fill_smooth(&mut a, 7, 16);
                let mut b = a.clone();
                let tc = [1i8, 2, 0, 3];
                deblock_luma_lt4_v(&mut a, stride, alpha, beta, &tc);
                luma_lt4_v_scalar(&mut b, stride, alpha, beta, &tc);
                assert_eq!(a, b, "lt4 alpha={alpha} beta={beta}");
                let mut c = vec![0u8; 8 * stride + 16];
                fill_smooth(&mut c, 7, 16);
                let mut d = c.clone();
                deblock_luma_eq4_v(&mut c, stride, alpha, beta);
                luma_eq4_v_scalar(&mut d, stride, alpha, beta);
                assert_eq!(c, d, "eq4 alpha={alpha} beta={beta}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// aarch64 NEON — a function-for-function mirror of the SSE2 module above.
// NEON (Advanced SIMD) is architecturally mandatory on AArch64, so like the
// SSE2 baseline there is NOTHING to detect and no `#[target_feature]` inlining
// barrier. The SSE2 unpack sequences map 1:1: unpacklo_epi8/16/32 = vzip1q,
// unpackhi = vzip2q, unpack{lo,hi}_epi64 = vcombine of the get_{low,high}
// halves, srli_si128::<8> = the high half. Every kernel is pinned to the same
// scalar oracle by the `*_matches_scalar` tests, which are arch-agnostic and
// exercise THIS module on the first aarch64 test run.
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).

    #[inline(always)]
    unsafe fn ld(p: *const u8) -> int16x8_t {
        vreinterpretq_s16_u16(vmovl_u8(vld1_u8(p)))
    }
    #[inline(always)]
    unsafe fn st(p: *mut u8, v: int16x8_t) {
        vst1_u8(p, vqmovun_s16(v));
    }
    #[inline(always)]
    unsafe fn absdiff(a: int16x8_t, b: int16x8_t) -> int16x8_t {
        vabdq_s16(a, b)
    }
    /// `v.clamp(-t, t)`
    #[inline(always)]
    unsafe fn clip3v(v: int16x8_t, t: int16x8_t) -> int16x8_t {
        vminq_s16(vmaxq_s16(v, vnegq_s16(t)), t)
    }
    /// Keep `v` where `mask` lanes are set, else 0 (the `_mm_and_si128` form).
    #[inline(always)]
    unsafe fn mask_s16(v: int16x8_t, mask: uint16x8_t) -> int16x8_t {
        vandq_s16(v, vreinterpretq_s16_u16(mask))
    }
    /// Subtracting an all-ones mask lane adds 1 — the same trick the SSE2 core
    /// documents (and whose `mask & 1` mis-spelling its tests caught).
    #[inline(always)]
    unsafe fn add_mask(v: int16x8_t, mask: uint16x8_t) -> int16x8_t {
        vsubq_s16(v, vreinterpretq_s16_u16(mask))
    }

    #[inline(always)]
    unsafe fn lt4_core(
        p2: int16x8_t, p1: int16x8_t, p0: int16x8_t, q0: int16x8_t, q1: int16x8_t, q2: int16x8_t,
        alpha: int16x8_t, beta: int16x8_t, tc0: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        // tc0 < 0 marks a skipped group (bS==0).
        let live = vcgtq_s16(tc0, vdupq_n_s16(-1));
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1, p0), absdiff(q1, q0));
        let mut m = vcgtq_s16(alpha, absdiff(p0, q0));
        m = vandq_u16(m, vcgtq_s16(beta, dmax));
        m = vandq_u16(m, live);

        let apm = vcgtq_s16(beta, absdiff(p2, p0));
        let aqm = vcgtq_s16(beta, absdiff(q2, q0));
        let one = vdupq_n_s16(1);
        // tc = tc0 + (ap<beta) + (aq<beta)
        let tc = add_mask(add_mask(tc0, apm), aqm);

        let d = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vshlq_n_s16::<2>(vsubq_s16(q0, p0)), vsubq_s16(p1, q1)),
            vdupq_n_s16(4),
        ));
        let delta = mask_s16(clip3v(d, tc), m);
        let np0 = vaddq_s16(p0, delta);
        let nq0 = vsubq_s16(q0, delta);

        let avg = vshrq_n_s16::<1>(vaddq_s16(vaddq_s16(p0, q0), one));
        let dp = vshrq_n_s16::<1>(vsubq_s16(vaddq_s16(p2, avg), vshlq_n_s16::<1>(p1)));
        let dq = vshrq_n_s16::<1>(vsubq_s16(vaddq_s16(q2, avg), vshlq_n_s16::<1>(q1)));
        let np1 = vaddq_s16(p1, mask_s16(clip3v(dp, tc0), vandq_u16(m, apm)));
        let nq1 = vaddq_s16(q1, mask_s16(clip3v(dq, tc0), vandq_u16(m, aqm)));
        (np1, np0, nq0, nq1)
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    unsafe fn eq4_core(
        p3: int16x8_t, p2: int16x8_t, p1: int16x8_t, p0: int16x8_t,
        q0: int16x8_t, q1: int16x8_t, q2: int16x8_t, q3: int16x8_t,
        alpha: int16x8_t, beta: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        let two = vdupq_n_s16(2);
        let four = vdupq_n_s16(4);
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1, p0), absdiff(q1, q0));
        let mut m = vcgtq_s16(alpha, absdiff(p0, q0));
        m = vandq_u16(m, vcgtq_s16(beta, dmax));
        let thr = vaddq_s16(vshrq_n_s16::<2>(alpha), two);
        let strong = vcgtq_s16(thr, absdiff(p0, q0));
        let sp = vandq_u16(strong, vcgtq_s16(beta, absdiff(p2, p0)));
        let sq = vandq_u16(strong, vcgtq_s16(beta, absdiff(q2, q0)));

        let p0s = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vaddq_s16(p2, vshlq_n_s16::<1>(p1)), vaddq_s16(vshlq_n_s16::<1>(p0), vshlq_n_s16::<1>(q0))),
            vaddq_s16(q1, four),
        ));
        let p1s = vshrq_n_s16::<2>(vaddq_s16(
            vaddq_s16(vaddq_s16(p2, p1), vaddq_s16(p0, q0)), two,
        ));
        let p2s = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vshlq_n_s16::<1>(p3), vaddq_s16(vaddq_s16(p2, vshlq_n_s16::<1>(p2)), p1)),
            vaddq_s16(vaddq_s16(p0, q0), four),
        ));
        let p0w = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(vshlq_n_s16::<1>(p1), p0), vaddq_s16(q1, two)));

        let q0s = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vaddq_s16(q2, vshlq_n_s16::<1>(q1)), vaddq_s16(vshlq_n_s16::<1>(q0), vshlq_n_s16::<1>(p0))),
            vaddq_s16(p1, four),
        ));
        let q1s = vshrq_n_s16::<2>(vaddq_s16(
            vaddq_s16(vaddq_s16(q2, q1), vaddq_s16(q0, p0)), two,
        ));
        let q2s = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vshlq_n_s16::<1>(q3), vaddq_s16(vaddq_s16(q2, vshlq_n_s16::<1>(q2)), q1)),
            vaddq_s16(vaddq_s16(p0, q0), four),
        ));
        let q0w = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(vshlq_n_s16::<1>(q1), q0), vaddq_s16(p1, two)));

        let np0 = vbslq_s16(m, vbslq_s16(sp, p0s, p0w), p0);
        let np1 = vbslq_s16(vandq_u16(m, sp), p1s, p1);
        let np2 = vbslq_s16(vandq_u16(m, sp), p2s, p2);
        let nq0 = vbslq_s16(m, vbslq_s16(sq, q0s, q0w), q0);
        let nq1 = vbslq_s16(vandq_u16(m, sq), q1s, q1);
        let nq2 = vbslq_s16(vandq_u16(m, sq), q2s, q2);
        (np2, np1, np0, nq0, nq1, nq2)
    }

    #[inline(always)]
    unsafe fn tc_lanes(tc: &[i8; 4], per: usize, g0: usize) -> int16x8_t {
        let mut v = [0i16; 8];
        for (i, slot) in v.iter_mut().enumerate() {
            *slot = tc[g0 + i / per] as i16;
        }
        vld1q_s16(v.as_ptr())
    }

    #[inline(always)]
    pub unsafe fn luma_lt4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        let base = p3.as_mut_ptr();
        for half in 0..2 {
            let c = base.add(half * 8);
            let r = |k: usize| c.add(k * stride);
            let (np1, np0, nq0, nq1) = lt4_core(
                ld(r(1)), ld(r(2)), ld(r(3)), ld(r(4)), ld(r(5)), ld(r(6)),
                a, b, tc_lanes(tc, 4, half * 2),
            );
            st(r(2), np1); st(r(3), np0); st(r(4), nq0); st(r(5), nq1);
        }
    }

    #[inline(always)]
    pub unsafe fn luma_eq4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        let base = p3.as_mut_ptr();
        for half in 0..2 {
            let c = base.add(half * 8);
            let r = |k: usize| c.add(k * stride);
            let (np2, np1, np0, nq0, nq1, nq2) = eq4_core(
                ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)),
                ld(r(4)), ld(r(5)), ld(r(6)), ld(r(7)), a, b,
            );
            st(r(1), np2); st(r(2), np1); st(r(3), np0);
            st(r(4), nq0); st(r(5), nq1); st(r(6), nq2);
        }
    }

    #[inline(always)]
    pub unsafe fn chroma_lt4_v(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        // WIN: the tcv>0 term is REDUNDANT in the CHROMA kernels. `delta` is
        // `clip3v(d, tcv) & m`, and clip3v is `min(max(d, -tcv), tcv)`, so at
        // tcv == 0 it already yields 0 on exactly the lanes this term masked.
        // Chroma tc is `tc0 + 1` with tc0 >= 0 (0 marks bS == 0), so tcv is never
        // negative -- the only case where the clip would not collapse.
        // NOT valid in `lt4_core`: there tc0 < 0 marks the skip and
        // tc = tc0 + ap + aq can climb back to >= 0, so its `live` mask is
        // load-bearing. Removing it there fails `luma_v_matches_scalar`.
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        let base = p1.as_mut_ptr();
        let r = |k: usize| base.add(k * stride);
        // tcv is ALREADY tc0+1 (caller-applied); 0 marks bS==0.
        let tcv = tc_lanes(tc, 2, 0);
        let (p1v, p0v, q0v, q1v) = (ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)));
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));
        m = vandq_u16(m, vcgtq_s16(b, dmax));
        let d = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vshlq_n_s16::<2>(vsubq_s16(q0v, p0v)), vsubq_s16(p1v, q1v)),
            vdupq_n_s16(4),
        ));
        let delta = mask_s16(clip3v(d, tcv), m);
        st(r(1), vaddq_s16(p0v, delta));
        st(r(2), vsubq_s16(q0v, delta));
    }

    #[inline(always)]
    pub unsafe fn chroma_eq4_v(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        let base = p1.as_mut_ptr();
        let r = |k: usize| base.add(k * stride);
        let (p1v, p0v, q0v, q1v) = (ld(r(0)), ld(r(1)), ld(r(2)), ld(r(3)));
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));
        m = vandq_u16(m, vcgtq_s16(b, dmax));
        // WIN (NEON twin): same shared (p1 + q1 + 2) as the SSE2 path.
        let s = vaddq_s16(vaddq_s16(p1v, q1v), vdupq_n_s16(2));
        let np0 = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(s, p1v), p0v));
        let nq0 = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(s, q1v), q0v));
        st(r(1), vbslq_s16(m, np0, p0v));
        st(r(2), vbslq_s16(m, nq0, q0v));
    }

    // -- vertical-edge (H) kernels: transpose, filter with the _v kernel, transpose back.

    #[inline(always)]
    pub unsafe fn luma_lt4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        let mut buf = [0u8; 8 * 16];
        transpose_16x8_to_8x16(p4.as_ptr(), stride, buf.as_mut_ptr());
        luma_lt4_v(&mut buf, 16, alpha, beta, tc);
        transpose_8x16_to_16x8(buf.as_ptr(), p4.as_mut_ptr(), stride);
    }

    #[inline(always)]
    pub unsafe fn luma_eq4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let mut buf = [0u8; 8 * 16];
        transpose_16x8_to_8x16(p4.as_ptr(), stride, buf.as_mut_ptr());
        luma_eq4_v(&mut buf, 16, alpha, beta);
        transpose_8x16_to_16x8(buf.as_ptr(), p4.as_mut_ptr(), stride);
    }

    /// unpackhi_epi64 equivalent (high halves side by side).
    #[inline(always)]
    unsafe fn hi64(a: uint8x16_t, b: uint8x16_t) -> uint8x16_t {
        vcombine_u8(vget_high_u8(a), vget_high_u8(b))
    }

    /// 16 rows x 8 cols -> 8 rows x 16 cols. Two 8x8 byte transposes, side by side.
    #[inline(always)]
    unsafe fn transpose_16x8_to_8x16(src: *const u8, stride: usize, dst: *mut u8) {
        for h in 0..2 {
            let mut a = [vdupq_n_u8(0); 8];
            for (i, slot) in a.iter_mut().enumerate() {
                *slot = vcombine_u8(vld1_u8(src.add((h * 8 + i) * stride)), vdup_n_u8(0));
            }
            let t0 = vzip1q_u8(a[0], a[1]);
            let t1 = vzip1q_u8(a[2], a[3]);
            let t2 = vzip1q_u8(a[4], a[5]);
            let t3 = vzip1q_u8(a[6], a[7]);
            let u0 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t0), vreinterpretq_u16_u8(t1)));
            let u1 = vreinterpretq_u8_u16(vzip2q_u16(vreinterpretq_u16_u8(t0), vreinterpretq_u16_u8(t1)));
            let u2 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t2), vreinterpretq_u16_u8(t3)));
            let u3 = vreinterpretq_u8_u16(vzip2q_u16(vreinterpretq_u16_u8(t2), vreinterpretq_u16_u8(t3)));
            let v = [
                vreinterpretq_u8_u32(vzip1q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u2))),
                vreinterpretq_u8_u32(vzip2q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u2))),
                vreinterpretq_u8_u32(vzip1q_u32(vreinterpretq_u32_u8(u1), vreinterpretq_u32_u8(u3))),
                vreinterpretq_u8_u32(vzip2q_u32(vreinterpretq_u32_u8(u1), vreinterpretq_u32_u8(u3))),
            ];
            for (k, vk) in v.iter().enumerate() {
                let d = dst.add((k * 2) * 16 + h * 8);
                vst1_u8(d, vget_low_u8(*vk));
                vst1_u8(d.add(16), vget_high_u8(*vk));
            }
        }
    }

    /// Inverse of the above.
    #[inline(always)]
    unsafe fn transpose_8x16_to_16x8(src: *const u8, dst: *mut u8, stride: usize) {
        for h in 0..2 {
            let mut a = [vdupq_n_u8(0); 8];
            for (i, slot) in a.iter_mut().enumerate() {
                *slot = vcombine_u8(vld1_u8(src.add(i * 16 + h * 8)), vdup_n_u8(0));
            }
            let t0 = vzip1q_u8(a[0], a[1]);
            let t1 = vzip1q_u8(a[2], a[3]);
            let t2 = vzip1q_u8(a[4], a[5]);
            let t3 = vzip1q_u8(a[6], a[7]);
            let u0 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t0), vreinterpretq_u16_u8(t1)));
            let u1 = vreinterpretq_u8_u16(vzip2q_u16(vreinterpretq_u16_u8(t0), vreinterpretq_u16_u8(t1)));
            let u2 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t2), vreinterpretq_u16_u8(t3)));
            let u3 = vreinterpretq_u8_u16(vzip2q_u16(vreinterpretq_u16_u8(t2), vreinterpretq_u16_u8(t3)));
            let v = [
                vreinterpretq_u8_u32(vzip1q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u2))),
                vreinterpretq_u8_u32(vzip2q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u2))),
                vreinterpretq_u8_u32(vzip1q_u32(vreinterpretq_u32_u8(u1), vreinterpretq_u32_u8(u3))),
                vreinterpretq_u8_u32(vzip2q_u32(vreinterpretq_u32_u8(u1), vreinterpretq_u32_u8(u3))),
            ];
            for (k, vk) in v.iter().enumerate() {
                let r = h * 8 + k * 2;
                vst1_u8(dst.add(r * stride), vget_low_u8(*vk));
                vst1_u8(dst.add((r + 1) * stride), vget_high_u8(*vk));
            }
        }
    }

    /// 8 rows x 4 cols -> (cols 0|1 packed, cols 2|3 packed), 8 bytes per column half.
    #[inline(always)]
    unsafe fn transpose_8x4(src: *const u8, stride: usize) -> (uint8x16_t, uint8x16_t) {
        let mut a = [vdupq_n_u8(0); 8];
        for (i, slot) in a.iter_mut().enumerate() {
            let w = src.add(i * stride).cast::<u32>().read_unaligned();
            *slot = vreinterpretq_u8_u32(vsetq_lane_u32::<0>(w, vdupq_n_u32(0)));
        }
        let t0 = vzip1q_u8(a[0], a[1]);
        let t1 = vzip1q_u8(a[2], a[3]);
        let t2 = vzip1q_u8(a[4], a[5]);
        let t3 = vzip1q_u8(a[6], a[7]);
        let u0 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t0), vreinterpretq_u16_u8(t1)));
        let u1 = vreinterpretq_u8_u16(vzip1q_u16(vreinterpretq_u16_u8(t2), vreinterpretq_u16_u8(t3)));
        (
            vreinterpretq_u8_u32(vzip1q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u1))),
            vreinterpretq_u8_u32(vzip2q_u32(vreinterpretq_u32_u8(u0), vreinterpretq_u32_u8(u1))),
        )
    }

    /// Inverse of `transpose_8x4`: four filtered columns (i16 lanes) back to 8 rows.
    #[inline(always)]
    /// NEON twin of the two-column chroma store (see the SSE2 copy).
    #[inline(always)]
    unsafe fn store_8x2_mid(dst: *mut u8, stride: usize, c1: int16x8_t, c2: int16x8_t) {
        let a = vcombine_u8(vqmovun_s16(c1), vqmovun_s16(c2)); // [col1 x8 | col2 x8]
        let inter = vreinterpretq_u16_u8(vzip1q_u8(a, hi64(a, a)));
        macro_rules! put {
            ($r:expr) => {
                dst.add($r * stride + 1)
                    .cast::<u16>()
                    .write_unaligned(vgetq_lane_u16::<$r>(inter))
            };
        }
        put!(0); put!(1); put!(2); put!(3); put!(4); put!(5); put!(6); put!(7);
    }

    unsafe fn store_8x4(dst: *mut u8, stride: usize, c0: int16x8_t, c1: int16x8_t, c2: int16x8_t, c3: int16x8_t) {
        let a = vcombine_u8(vqmovun_s16(c0), vqmovun_s16(c1)); // [col0 x8 | col1 x8]
        let b = vcombine_u8(vqmovun_s16(c2), vqmovun_s16(c3)); // [col2 x8 | col3 x8]
        let lo = vzip1q_u8(a, hi64(a, a)); // (col0,col1) interleaved per row
        let hi = vzip1q_u8(b, hi64(b, b)); // (col2,col3) per row
        let r03 = vreinterpretq_u32_u16(vzip1q_u16(vreinterpretq_u16_u8(lo), vreinterpretq_u16_u8(hi)));
        let r47 = vreinterpretq_u32_u16(vzip2q_u16(vreinterpretq_u16_u8(lo), vreinterpretq_u16_u8(hi)));
        let words = [
            vgetq_lane_u32::<0>(r03), vgetq_lane_u32::<1>(r03),
            vgetq_lane_u32::<2>(r03), vgetq_lane_u32::<3>(r03),
            vgetq_lane_u32::<0>(r47), vgetq_lane_u32::<1>(r47),
            vgetq_lane_u32::<2>(r47), vgetq_lane_u32::<3>(r47),
        ];
        for (r, w) in words.into_iter().enumerate() {
            dst.add(r * stride).cast::<u32>().write_unaligned(w);
        }
    }

    /// Widen the two packed halves of `transpose_8x4` into p1,p0,q0,q1 i16 lanes.
    #[inline(always)]
    unsafe fn spread_8x4(lo: uint8x16_t, hi: uint8x16_t) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        (
            vreinterpretq_s16_u16(vmovl_u8(vget_low_u8(lo))),
            vreinterpretq_s16_u16(vmovl_u8(vget_high_u8(lo))),
            vreinterpretq_s16_u16(vmovl_u8(vget_low_u8(hi))),
            vreinterpretq_s16_u16(vmovl_u8(vget_high_u8(hi))),
        )
    }

    #[inline(always)]
    pub unsafe fn chroma_lt4_h(p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
        // WIN: the tcv>0 term is REDUNDANT in the CHROMA kernels. `delta` is
        // `clip3v(d, tcv) & m`, and clip3v is `min(max(d, -tcv), tcv)`, so at
        // tcv == 0 it already yields 0 on exactly the lanes this term masked.
        // Chroma tc is `tc0 + 1` with tc0 >= 0 (0 marks bS == 0), so tcv is never
        // negative -- the only case where the clip would not collapse.
        // NOT valid in `lt4_core`: there tc0 < 0 marks the skip and
        // tc = tc0 + ap + aq can climb back to >= 0, so its `live` mask is
        // load-bearing. Removing it there fails `luma_v_matches_scalar`.
        let base = p1.as_mut_ptr();
        let (lo, hi) = transpose_8x4(base, stride);
        let (p1v, p0v, q0v, q1v) = spread_8x4(lo, hi);
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        let tcv = tc_lanes(tc, 2, 0);
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));
        m = vandq_u16(m, vcgtq_s16(b, dmax));
        let d = vshrq_n_s16::<3>(vaddq_s16(
            vaddq_s16(vshlq_n_s16::<2>(vsubq_s16(q0v, p0v)), vsubq_s16(p1v, q1v)),
            vdupq_n_s16(4)));
        let delta = mask_s16(clip3v(d, tcv), m);
        store_8x2_mid(base, stride, vaddq_s16(p0v, delta), vsubq_s16(q0v, delta));
    }

    #[inline(always)]
    pub unsafe fn chroma_eq4_h(p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
        let base = p1.as_mut_ptr();
        let (lo, hi) = transpose_8x4(base, stride);
        let (p1v, p0v, q0v, q1v) = spread_8x4(lo, hi);
        let (a, b) = (vdupq_n_s16(alpha as i16), vdupq_n_s16(beta as i16));
        // WIN (NEON twin): same folded beta compare as the SSE2 path.
        let dmax = vmaxq_s16(absdiff(p1v, p0v), absdiff(q1v, q0v));
        let mut m = vcgtq_s16(a, absdiff(p0v, q0v));
        m = vandq_u16(m, vcgtq_s16(b, dmax));
        // WIN (NEON twin): same shared (p1 + q1 + 2) as the SSE2 path.
        let s = vaddq_s16(vaddq_s16(p1v, q1v), vdupq_n_s16(2));
        let np0 = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(s, p1v), p0v));
        let nq0 = vshrq_n_s16::<2>(vaddq_s16(vaddq_s16(s, q1v), q0v));
        store_8x4(base, stride, p1v, vbslq_s16(m, np0, p0v), vbslq_s16(m, nq0, q0v), q1v);
    }
}
