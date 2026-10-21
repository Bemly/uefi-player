//! Challenge-1 A3: fused (a+b+1)>>1 average + SATD — a **custom Rust-intrinsics
//! kernel** (not vendored; openh264 ships nothing of this shape).
//!
//! The ME's quarter-pel candidates are predicted as the rounded average of two
//! half-pel planes. The old path materialized that average into a 256-byte temp
//! (`avg_rows`), then re-loaded it inside the SATD kernel — a store/reload round
//! trip plus an FFI call per candidate, at ~40% of all sub-pel evaluations. This
//! kernel computes `Σ|H·(src − avg(a,b))|` in one register pass: `vpavgb` produces
//! the exact `(a+b+1)>>1` the spec requires, and the 4×4 Hadamard runs in 16-bit
//! lanes (max |2-D coeff| = 16·255 = 4080 < i16::MAX, so no overflow is possible).
//!
//! **Byte-identity argument:** the butterfly network below computes a row-permuted
//! Sylvester H4 (`[1,1,1,1],[1,-1,1,-1],[1,1,-1,-1],[1,-1,-1,1]` vs the scalar
//! `hadamard_1d`'s `[1,1,1,1],[1,1,-1,-1],[1,-1,-1,1],[1,-1,1,-1]`). For any row
//! permutation P, `(PH)D(PH)ᵀ = P(HDHᵀ)Pᵀ` — the same coefficient multiset — so
//! `Σ|·|` is EXACTLY equal, in integer arithmetic, for every input. The
//! `satd_avg_matches_materialized_scalar` test pins this empirically as well.

use core::arch::x86_64::*;

/// One 4-row×16-col band: four 4×4 blocks side by side, diffs already in i16 lanes.
/// Adds each block's `Σ|H·d|` into `acc`'s eight i32 lanes.
#[inline]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn hadamard4_abs_acc(
    d0: __m256i,
    d1: __m256i,
    d2: __m256i,
    d3: __m256i,
    acc: __m256i,
) -> __m256i {
    // Vertical (across the four rows), lane-wise.
    let t0 = _mm256_add_epi16(d0, d1);
    let t1 = _mm256_sub_epi16(d0, d1);
    let t2 = _mm256_add_epi16(d2, d3);
    let t3 = _mm256_sub_epi16(d2, d3);
    let m0 = _mm256_add_epi16(t0, t2);
    let m1 = _mm256_add_epi16(t1, t3);
    let m2 = _mm256_sub_epi16(t0, t2);
    let m3 = _mm256_sub_epi16(t1, t3);
    // Transpose each 4×4 group (all unpacks are 128-lane-local, so the two lanes'
    // block pairs stay independent).
    let u0 = _mm256_unpacklo_epi16(m0, m1);
    let u1 = _mm256_unpackhi_epi16(m0, m1);
    let v0 = _mm256_unpacklo_epi16(m2, m3);
    let v1 = _mm256_unpackhi_epi16(m2, m3);
    let w0 = _mm256_unpacklo_epi32(u0, v0);
    let w1 = _mm256_unpackhi_epi32(u0, v0);
    let w2 = _mm256_unpacklo_epi32(u1, v1);
    let w3 = _mm256_unpackhi_epi32(u1, v1);
    let r0 = _mm256_unpacklo_epi64(w0, w2);
    let r1 = _mm256_unpackhi_epi64(w0, w2);
    let r2 = _mm256_unpacklo_epi64(w1, w3);
    let r3 = _mm256_unpackhi_epi64(w1, w3);
    // Horizontal (across the four columns, now rows post-transpose), lane-wise.
    let s0 = _mm256_add_epi16(r0, r1);
    let s1 = _mm256_sub_epi16(r0, r1);
    let s2 = _mm256_add_epi16(r2, r3);
    let s3 = _mm256_sub_epi16(r2, r3);
    let f0 = _mm256_add_epi16(s0, s2);
    let f1 = _mm256_add_epi16(s1, s3);
    let f2 = _mm256_sub_epi16(s0, s2);
    let f3 = _mm256_sub_epi16(s1, s3);
    // Σ|coeff| into i32 lanes: |f| ≤ 4080, pairs ≤ 8160 — well inside i32.
    let ones = _mm256_set1_epi16(1);
    let mut a = acc;
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f0), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f1), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f2), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f3), ones));
    a
}

/// src row minus avg row, 16 columns, as 16 i16 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn diff_row16(src: *const u8, a: *const u8, b: *const u8) -> __m256i {
    let s = _mm256_cvtepu8_epi16(_mm_loadu_si128(src as *const __m128i));
    let av = _mm_avg_epu8(
        _mm_loadu_si128(a as *const __m128i),
        _mm_loadu_si128(b as *const __m128i),
    );
    _mm256_sub_epi16(s, _mm256_cvtepu8_epi16(av))
}

/// Rows `r` and `r+4` of an 8-wide block packed as [row r | row r+4] in i16 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn diff_row8x2(
    src: *const u8,
    a: *const u8,
    b: *const u8,
    ss: usize,
    st: usize,
) -> __m256i {
    let s = _mm_unpacklo_epi64(
        _mm_loadl_epi64(src as *const __m128i),
        _mm_loadl_epi64(src.add(4 * ss) as *const __m128i),
    );
    let pa = _mm_unpacklo_epi64(
        _mm_loadl_epi64(a as *const __m128i),
        _mm_loadl_epi64(a.add(4 * st) as *const __m128i),
    );
    let pb = _mm_unpacklo_epi64(
        _mm_loadl_epi64(b as *const __m128i),
        _mm_loadl_epi64(b.add(4 * st) as *const __m128i),
    );
    _mm256_sub_epi16(
        _mm256_cvtepu8_epi16(s),
        _mm256_cvtepu8_epi16(_mm_avg_epu8(pa, pb)),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum_epi32(v: __m256i) -> u32 {
    let s = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b0100_1110));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b1011_0001));
    _mm_cvtsi128_si32(s) as u32
}

/// 16-wide (16×16 / 16×8): one 4-row band per iteration, four blocks per band.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn satd_avg_w16(
    src: *const u8,
    ss: usize,
    a: *const u8,
    b: *const u8,
    st: usize,
    h: usize,
) -> u32 {
    let mut acc = _mm256_setzero_si256();
    let mut r = 0;
    while r < h {
        let d0 = diff_row16(src.add(r * ss), a.add(r * st), b.add(r * st));
        let d1 = diff_row16(src.add((r + 1) * ss), a.add((r + 1) * st), b.add((r + 1) * st));
        let d2 = diff_row16(src.add((r + 2) * ss), a.add((r + 2) * st), b.add((r + 2) * st));
        let d3 = diff_row16(src.add((r + 3) * ss), a.add((r + 3) * st), b.add((r + 3) * st));
        acc = hadamard4_abs_acc(d0, d1, d2, d3, acc);
        r += 4;
    }
    hsum_epi32(acc)
}

/// 8-wide (8×16 / 8×8): one 8-row band per iteration — rows r and r+4 share a
/// vector's two 128-lanes, so the same four-block band kernel applies.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn satd_avg_w8(
    src: *const u8,
    ss: usize,
    a: *const u8,
    b: *const u8,
    st: usize,
    h: usize,
) -> u32 {
    let mut acc = _mm256_setzero_si256();
    let mut r = 0;
    while r < h {
        let d0 = diff_row8x2(src.add(r * ss), a.add(r * st), b.add(r * st), ss, st);
        let d1 = diff_row8x2(src.add((r + 1) * ss), a.add((r + 1) * st), b.add((r + 1) * st), ss, st);
        let d2 = diff_row8x2(src.add((r + 2) * ss), a.add((r + 2) * st), b.add((r + 2) * st), ss, st);
        let d3 = diff_row8x2(src.add((r + 3) * ss), a.add((r + 3) * st), b.add((r + 3) * st), ss, st);
        acc = hadamard4_abs_acc(d0, d1, d2, d3, acc);
        r += 8;
    }
    hsum_epi32(acc)
}

/// Track-B batch kernel: SADs of ONE 16×16 source block against FOUR candidate
/// positions in the same plane (shared stride) — the x264 `sad_x4` shape. Each
/// source row is loaded ONCE and two `vpsadbw` cover all four candidates (vs four
/// separate loads+psadbw), amortizing the loads the flat single-SAD experiments
/// proved dominant.
#[target_feature(enable = "avx2")]
unsafe fn sad_16x16_x4_avx2(
    src: *const u8,
    ss: usize,
    r0: *const u8,
    r1: *const u8,
    r2: *const u8,
    r3: *const u8,
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut a01 = _mm256_setzero_si256();
    let mut a23 = _mm256_setzero_si256();
    for r in 0..h {
        let s = _mm_loadu_si128(src.add(r * ss) as *const __m128i);
        let sb = _mm256_set_m128i(s, s);
        let p01 = _mm256_set_m128i(
            _mm_loadu_si128(r1.add(r * rs) as *const __m128i),
            _mm_loadu_si128(r0.add(r * rs) as *const __m128i),
        );
        let p23 = _mm256_set_m128i(
            _mm_loadu_si128(r3.add(r * rs) as *const __m128i),
            _mm_loadu_si128(r2.add(r * rs) as *const __m128i),
        );
        a01 = _mm256_add_epi64(a01, _mm256_sad_epu8(sb, p01));
        a23 = _mm256_add_epi64(a23, _mm256_sad_epu8(sb, p23));
    }
    // Each accumulator holds [q0,q1 | q2,q3] u64: candidate k's row sums live in
    // one 128-lane's two quads.
    let mut buf = [0u64; 4];
    let mut out = [0u32; 4];
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a01);
    out[0] = (buf[0] + buf[1]) as u32;
    out[1] = (buf[2] + buf[3]) as u32;
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a23);
    out[2] = (buf[0] + buf[1]) as u32;
    out[3] = (buf[2] + buf[3]) as u32;
    out
}

/// The SATD sibling of `sad_16x16_x4`: `Σ|H·d|` of one 16×16 source against FOUR
/// candidate positions in the same plane. The source band is converted to i16
/// ONCE per 4-row band and reused by all four candidates' Hadamards.
#[target_feature(enable = "avx2")]
unsafe fn satd_16x16_x4_avx2(
    src: *const u8,
    ss: usize,
    r: [*const u8; 4],
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut acc = [_mm256_setzero_si256(); 4];
    let mut row = 0;
    while row < h {
        // Source band, loaded once for all four candidates.
        let s0 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(row * ss) as *const __m128i));
        let s1 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 1) * ss) as *const __m128i));
        let s2 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 2) * ss) as *const __m128i));
        let s3 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 3) * ss) as *const __m128i));
        for k in 0..4 {
            let p = r[k];
            let d0 = _mm256_sub_epi16(
                s0,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(row * rs) as *const __m128i)),
            );
            let d1 = _mm256_sub_epi16(
                s1,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 1) * rs) as *const __m128i)),
            );
            let d2 = _mm256_sub_epi16(
                s2,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 2) * rs) as *const __m128i)),
            );
            let d3 = _mm256_sub_epi16(
                s3,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 3) * rs) as *const __m128i)),
            );
            acc[k] = hadamard4_abs_acc(d0, d1, d2, d3, acc[k]);
        }
        row += 4;
    }
    [hsum_epi32(acc[0]), hsum_epi32(acc[1]), hsum_epi32(acc[2]), hsum_epi32(acc[3])]
}

/// 8-wide sibling of `satd_16x16_x4_avx2`: bands of 8 rows with the proven
/// `[row r | row r+4]` lane packing (each 128-lane carries an independent pair of
/// 4×4 block rows), so the same band core applies. `h` ∈ {8, 16}.
#[target_feature(enable = "avx2")]
unsafe fn satd_8xh_x4_avx2(
    src: *const u8,
    ss: usize,
    r: [*const u8; 4],
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut acc = [_mm256_setzero_si256(); 4];
    let mut row = 0;
    while row < h {
        let mut s = [_mm256_setzero_si256(); 4];
        for (i, slot) in s.iter_mut().enumerate() {
            *slot = _mm256_cvtepu8_epi16(_mm_unpacklo_epi64(
                _mm_loadl_epi64(src.add((row + i) * ss) as *const __m128i),
                _mm_loadl_epi64(src.add((row + i + 4) * ss) as *const __m128i),
            ));
        }
        for k in 0..4 {
            let p = r[k];
            let mut d = [_mm256_setzero_si256(); 4];
            for (i, slot) in d.iter_mut().enumerate() {
                let c = _mm256_cvtepu8_epi16(_mm_unpacklo_epi64(
                    _mm_loadl_epi64(p.add((row + i) * rs) as *const __m128i),
                    _mm_loadl_epi64(p.add((row + i + 4) * rs) as *const __m128i),
                ));
                *slot = _mm256_sub_epi16(s[i], c);
            }
            acc[k] = hadamard4_abs_acc(d[0], d[1], d[2], d[3], acc[k]);
        }
        row += 8;
    }
    [hsum_epi32(acc[0]), hsum_epi32(acc[1]), hsum_epi32(acc[2]), hsum_epi32(acc[3])]
}

/// 8-wide sibling of `satd_avg_16x16_x4_avx2` (same lane packing, `pavgb` fused).
#[target_feature(enable = "avx2")]
unsafe fn satd_avg_8xh_x4_avx2(
    src: *const u8,
    ss: usize,
    a: [*const u8; 4],
    b: [*const u8; 4],
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut acc = [_mm256_setzero_si256(); 4];
    let mut row = 0;
    while row < h {
        let mut s = [_mm256_setzero_si256(); 4];
        for (i, slot) in s.iter_mut().enumerate() {
            *slot = _mm256_cvtepu8_epi16(_mm_unpacklo_epi64(
                _mm_loadl_epi64(src.add((row + i) * ss) as *const __m128i),
                _mm_loadl_epi64(src.add((row + i + 4) * ss) as *const __m128i),
            ));
        }
        for k in 0..4 {
            let (pa, pb) = (a[k], b[k]);
            let mut d = [_mm256_setzero_si256(); 4];
            for (i, slot) in d.iter_mut().enumerate() {
                let av = _mm_avg_epu8(
                    _mm_unpacklo_epi64(
                        _mm_loadl_epi64(pa.add((row + i) * rs) as *const __m128i),
                        _mm_loadl_epi64(pa.add((row + i + 4) * rs) as *const __m128i),
                    ),
                    _mm_unpacklo_epi64(
                        _mm_loadl_epi64(pb.add((row + i) * rs) as *const __m128i),
                        _mm_loadl_epi64(pb.add((row + i + 4) * rs) as *const __m128i),
                    ),
                );
                *slot = _mm256_sub_epi16(s[i], _mm256_cvtepu8_epi16(av));
            }
            acc[k] = hadamard4_abs_acc(d[0], d[1], d[2], d[3], acc[k]);
        }
        row += 8;
    }
    [hsum_epi32(acc[0]), hsum_epi32(acc[1]), hsum_epi32(acc[2]), hsum_epi32(acc[3])]
}

/// True iff the x4 family covers this ME partition shape.
#[inline]
pub(crate) fn x4_shape(w: usize, h: usize) -> bool {
    matches!((w, h), (16, 16) | (16, 8) | (8, 16) | (8, 8))
}

/// Four SATDs (`Σ|H·d|`) of one `w`×`h` source block vs four INDEPENDENT plane
/// operands (shared stride) — the sub-pel ring's shape, now for every ME
/// partition. `None` without AVX2 or for an uncovered shape.
#[inline]
pub fn satd_x4p(
    src: &[u8],
    ss: usize,
    r: [(&[u8], usize); 4],
    rs: usize,
    w: usize,
    h: usize,
) -> Option<[u32; 4]> {
    if !super::has_avx2() || !x4_shape(w, h) {
        return None;
    }
    assert!(src.len() >= (h - 1) * ss + w);
    for &(p, o) in &r {
        assert!(p.len() >= o + (h - 1) * rs + w);
    }
    let ptrs = [
        // SAFETY (offsets): each `o` bounds-checked above.
        r[0].0[r[0].1..].as_ptr(),
        r[1].0[r[1].1..].as_ptr(),
        r[2].0[r[2].1..].as_ptr(),
        r[3].0[r[3].1..].as_ptr(),
    ];
    // SAFETY: AVX2 checked; every row read inside the asserted bounds (the 8-wide
    // core's `row+i+4` reads are ≤ h-1 by its loop structure).
    unsafe {
        Some(if w == 16 {
            satd_16x16_x4_avx2(src.as_ptr(), ss, ptrs, rs, h)
        } else {
            satd_8xh_x4_avx2(src.as_ptr(), ss, ptrs, rs, h)
        })
    }
}

/// `Σ|H·d|` SATDs of one `w`×`h` source block vs four offsets `o` into `base`
/// (stride `rs`) — the diamond's shape, for every ME partition. The exact
/// scalar-Hadamard value (`satd_px` domain, NOT the `(Σ+1)>>1` the Wels wrappers
/// return). `None` without AVX2 or for an uncovered shape.
#[inline]
pub fn satd_x4(
    src: &[u8],
    ss: usize,
    base: &[u8],
    o: [usize; 4],
    rs: usize,
    w: usize,
    h: usize,
) -> Option<[u32; 4]> {
    if !super::has_avx2() || !x4_shape(w, h) {
        return None;
    }
    assert!(src.len() >= (h - 1) * ss + w);
    for &oi in &o {
        assert!(base.len() >= oi + (h - 1) * rs + w);
    }
    // SAFETY: AVX2 checked; all row reads inside the asserted bounds.
    unsafe {
        let b = base.as_ptr();
        let ptrs = [b.add(o[0]), b.add(o[1]), b.add(o[2]), b.add(o[3])];
        Some(if w == 16 {
            satd_16x16_x4_avx2(src.as_ptr(), ss, ptrs, rs, h)
        } else {
            satd_8xh_x4_avx2(src.as_ptr(), ss, ptrs, rs, h)
        })
    }
}

/// 8-wide SAD x4 core: rows are 8 bytes, so a row PAIR forms one 16-byte unit
/// (SAD is order-free) and the two-candidates-per-ymm trick applies unchanged.
#[target_feature(enable = "avx2")]
unsafe fn sad_8xh_x4_avx2(
    src: *const u8,
    ss: usize,
    r: [*const u8; 4],
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut a01 = _mm256_setzero_si256();
    let mut a23 = _mm256_setzero_si256();
    let mut row = 0;
    while row < h {
        let s = _mm_unpacklo_epi64(
            _mm_loadl_epi64(src.add(row * ss) as *const __m128i),
            _mm_loadl_epi64(src.add((row + 1) * ss) as *const __m128i),
        );
        let sb = _mm256_set_m128i(s, s);
        let pk = |p: *const u8| {
            _mm_unpacklo_epi64(
                _mm_loadl_epi64(p.add(row * rs) as *const __m128i),
                _mm_loadl_epi64(p.add((row + 1) * rs) as *const __m128i),
            )
        };
        let p01 = _mm256_set_m128i(pk(r[1]), pk(r[0]));
        let p23 = _mm256_set_m128i(pk(r[3]), pk(r[2]));
        a01 = _mm256_add_epi64(a01, _mm256_sad_epu8(sb, p01));
        a23 = _mm256_add_epi64(a23, _mm256_sad_epu8(sb, p23));
        row += 2;
    }
    let mut buf = [0u64; 4];
    let mut out = [0u32; 4];
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a01);
    out[0] = (buf[0] + buf[1]) as u32;
    out[1] = (buf[2] + buf[3]) as u32;
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a23);
    out[2] = (buf[0] + buf[1]) as u32;
    out[3] = (buf[2] + buf[3]) as u32;
    out
}

/// SADs of one `w`×`h` source block vs four offsets `o` into `base` (stride
/// `rs`), for every ME partition shape. Values are exactly `Σ|a−b|` per
/// candidate. `None` without AVX2 or for an uncovered shape.
#[inline]
pub fn sad_x4(
    src: &[u8],
    ss: usize,
    base: &[u8],
    o: [usize; 4],
    rs: usize,
    w: usize,
    h: usize,
) -> Option<[u32; 4]> {
    if !super::has_avx2() || !x4_shape(w, h) {
        return None;
    }
    assert!(src.len() >= (h - 1) * ss + w);
    for &oi in &o {
        assert!(base.len() >= oi + (h - 1) * rs + w);
    }
    // SAFETY: AVX2 checked; every row read of all five operands is inside the
    // asserted bounds (the 8-wide core reads row pairs, h is even for all shapes).
    unsafe {
        let b = base.as_ptr();
        Some(if w == 16 {
            sad_16x16_x4_avx2(
                src.as_ptr(), ss,
                b.add(o[0]), b.add(o[1]), b.add(o[2]), b.add(o[3]),
                rs, h,
            )
        } else {
            sad_8xh_x4_avx2(src.as_ptr(), ss, [b.add(o[0]), b.add(o[1]), b.add(o[2]), b.add(o[3])], rs, h)
        })
    }
}

/// FOUR fused avg+SATDs at once — the quarter-pel ring's shape: each candidate is
/// `Σ|H·(src − (a_k+b_k+1)>>1)|` with its own plane pair. Source band converted
/// to i16 once for all four.
#[target_feature(enable = "avx2")]
unsafe fn satd_avg_16x16_x4_avx2(
    src: *const u8,
    ss: usize,
    a: [*const u8; 4],
    b: [*const u8; 4],
    rs: usize,
    h: usize,
) -> [u32; 4] {
    let mut acc = [_mm256_setzero_si256(); 4];
    let mut row = 0;
    while row < h {
        let s0 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(row * ss) as *const __m128i));
        let s1 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 1) * ss) as *const __m128i));
        let s2 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 2) * ss) as *const __m128i));
        let s3 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 3) * ss) as *const __m128i));
        for k in 0..4 {
            let (pa, pb) = (a[k], b[k]);
            let avg = |r: usize| {
                _mm256_cvtepu8_epi16(_mm_avg_epu8(
                    _mm_loadu_si128(pa.add(r * rs) as *const __m128i),
                    _mm_loadu_si128(pb.add(r * rs) as *const __m128i),
                ))
            };
            let d0 = _mm256_sub_epi16(s0, avg(row));
            let d1 = _mm256_sub_epi16(s1, avg(row + 1));
            let d2 = _mm256_sub_epi16(s2, avg(row + 2));
            let d3 = _mm256_sub_epi16(s3, avg(row + 3));
            acc[k] = hadamard4_abs_acc(d0, d1, d2, d3, acc[k]);
        }
        row += 4;
    }
    [hsum_epi32(acc[0]), hsum_epi32(acc[1]), hsum_epi32(acc[2]), hsum_epi32(acc[3])]
}

/// Four fused avg+SATDs of one `w`×`h` source block: `(plane_a, off_a, plane_b,
/// off_b)` per candidate, shared stride — the quarter-pel ring for every ME
/// partition shape. `None` without AVX2 or for an uncovered shape.
#[inline]
pub fn satd_avg_x4(
    src: &[u8],
    ss: usize,
    pairs: [(&[u8], usize, &[u8], usize); 4],
    rs: usize,
    w: usize,
    h: usize,
) -> Option<[u32; 4]> {
    if !super::has_avx2() || !x4_shape(w, h) {
        return None;
    }
    assert!(src.len() >= (h - 1) * ss + w);
    for &(pa, oa, pb, ob) in &pairs {
        assert!(pa.len() >= oa + (h - 1) * rs + w && pb.len() >= ob + (h - 1) * rs + w);
    }
    let ap = [
        pairs[0].0[pairs[0].1..].as_ptr(),
        pairs[1].0[pairs[1].1..].as_ptr(),
        pairs[2].0[pairs[2].1..].as_ptr(),
        pairs[3].0[pairs[3].1..].as_ptr(),
    ];
    let bp = [
        pairs[0].2[pairs[0].3..].as_ptr(),
        pairs[1].2[pairs[1].3..].as_ptr(),
        pairs[2].2[pairs[2].3..].as_ptr(),
        pairs[3].2[pairs[3].3..].as_ptr(),
    ];
    // SAFETY: AVX2 checked; every row read inside the asserted bounds.
    unsafe {
        Some(if w == 16 {
            satd_avg_16x16_x4_avx2(src.as_ptr(), ss, ap, bp, rs, h)
        } else {
            satd_avg_8xh_x4_avx2(src.as_ptr(), ss, ap, bp, rs, h)
        })
    }
}

/// Fused `SATD(src, (a+b+1)>>1)` of a `w`×`h` block — `Σ|H·d|`, the SAME value
/// `satd_px` computes on the materialized average (NOT the `(Σ+1)>>1` the
/// `WelsSampleSatd*` wrappers return). `None` when AVX2 is unavailable or the
/// size is unsupported — the caller then materializes and takes the old path,
/// so a non-AVX2 machine is byte-identical by construction.
#[inline]
pub fn satd_avg(
    src: &[u8],
    src_stride: usize,
    a: &[u8],
    b: &[u8],
    stride: usize,
    w: usize,
    h: usize,
) -> Option<u32> {
    if !super::has_avx2() || !matches!((w, h), (16, 16) | (16, 8) | (8, 16) | (8, 8)) {
        return None;
    }
    assert!(src.len() >= (h - 1) * src_stride + w);
    assert!(a.len() >= (h - 1) * stride + w && b.len() >= (h - 1) * stride + w);
    // SAFETY: AVX2 checked above; every row read is inside the asserted bounds
    // ((h-1)·stride + w for all three operands; the 8-wide packer's `r+4`/`+4·st`
    // rows are ≤ (h-1) by the loop bound).
    unsafe {
        Some(match w {
            16 => satd_avg_w16(src.as_ptr(), src_stride, a.as_ptr(), b.as_ptr(), stride, h),
            _ => satd_avg_w8(src.as_ptr(), src_stride, a.as_ptr(), b.as_ptr(), stride, h),
        })
    }
}
