//! Eighth-pel **chroma** bilinear motion compensation — portable SIMD.
//!
//! First kernel of the rip-ASM campaign (`docs/add_SIMD_rip_ASM.md`, Phase 1). It
//! replaces openh264's `McChromaWidthEq8_sse2` / `McChromaWidthEq4_mmx` with Rust
//! intrinsics, and is the proving ground for the portable layer: one scalar reference,
//! one x86-64 SSE2 path, one aarch64 NEON path, all three bit-identical by test.
//!
//! ## The operation
//!
//! H.264 §8.4.2.2.2. With fractional position `(fx, fy)` in eighth-pels, the weights
//! are `wa=(8-fx)(8-fy)`, `wb=fx(8-fy)`, `wc=(8-fx)fy`, `wd=fx*fy`, and
//!
//! ```text
//! dst[y][x] = (wa*S[y][x] + wb*S[y][x+1] + wc*S[y+1][x] + wd*S[y+1][x+1] + 32) >> 6
//! ```
//!
//! **The weights sum to exactly 64**, so the accumulator maxes at `64*255 + 32 = 16352`
//! — comfortably inside `u16`. That is what makes the whole kernel 16-bit and is why no
//! widening to 32-bit is needed anywhere.
//!
//! On NEON the `+32 >> 6` is a single rounding-narrow (`vrshrn_n_u16::<6>`), which both
//! rounds and narrows to `u8`; the maximum result is exactly 255 so the truncating
//! narrow cannot lose a value. x86 has no rounding shift, so it adds 32 explicitly.
//!
//! ## Why width 4 does not reuse the width-8 code
//!
//! The 4-wide caller only guarantees **5** readable bytes per row (`src_stride >= 5`),
//! so an 8-byte vector load would read up to 3 bytes past the tile on the final row.
//! The 4-wide path therefore loads exactly 4 bytes at a time via unaligned `u32` reads.
//! This is a real constraint from the call site, not caution: `mc_chroma_w4` is fed a
//! `(bw+1)`-wide clamped halo.

/// Scalar reference. The oracle every SIMD path is tested against, and the fallback on
/// architectures with no specialised path. Kept forever, not temporarily.
#[inline]
fn mc_chroma_scalar(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    width: usize,
    height: usize,
) {
    let (wa, wb, wc, wd) = (
        abcd[0] as u32,
        abcd[1] as u32,
        abcd[2] as u32,
        abcd[3] as u32,
    );
    for y in 0..height {
        let s0 = y * src_stride;
        let s1 = s0 + src_stride;
        for x in 0..width {
            let v = wa * src[s0 + x] as u32
                + wb * src[s0 + x + 1] as u32
                + wc * src[s1 + x] as u32
                + wd * src[s1 + x + 1] as u32;
            dst[y * dst_stride + x] = ((v + 32) >> 6) as u8;
        }
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn mc_chroma_w8_sse2(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    // `pmaddubsw` form (kernel round 2026-09-05): the two source rows of the
    // bilinear tap are INTERLEAVED as byte pairs (x[i], x[i+1]) and multiplied
    // by the (wa, wb) / (wc, wd) weight pairs in one op each -- 2 madds + 1 add
    // replace 4 `pmullw` + 3 adds and the four zero-extensions. Exact: every
    // pair sum is <= 255 * 64 = 16320 (weights sum to 64), so the i16 result
    // never saturates. ROW REUSE: row y+1 is loaded once and carried as next
    // iteration's top row. `(v + 32) >> 6` is `pmulhrsw(v, 512)` exactly for
    // v in 0..=16320.
    let wab = _mm_set1_epi16(abcd[0] as i16 | ((abcd[1] as i16) << 8));
    let wcd = _mm_set1_epi16(abcd[2] as i16 | ((abcd[3] as i16) << 8));
    let k512 = _mm_set1_epi16(512);
    let sp = src.as_ptr();
    let dp = dst.as_mut_ptr();
    let pair = |r: *const u8| -> __m128i {
        _mm_unpacklo_epi8(_mm_loadl_epi64(r as *const __m128i), _mm_loadl_epi64(r.add(1) as *const __m128i))
    };
    let mut ab = pair(sp);
    for y in 0..height {
        let cd = pair(sp.add((y + 1) * src_stride));
        let v = _mm_add_epi16(_mm_maddubs_epi16(ab, wab), _mm_maddubs_epi16(cd, wcd));
        let r = _mm_mulhrs_epi16(v, k512);
        _mm_storel_epi64(dp.add(y * dst_stride) as *mut __m128i, _mm_packus_epi16(r, r));
        ab = cd;
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn mc_chroma_w4_sse2(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    use core::arch::x86_64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    // Same `pmaddubsw` + row-reuse shape as w8; 4-byte loads keep the read
    // inside the 5-byte-per-row guarantee.
    let wab = _mm_set1_epi16(abcd[0] as i16 | ((abcd[1] as i16) << 8));
    let wcd = _mm_set1_epi16(abcd[2] as i16 | ((abcd[3] as i16) << 8));
    let k512 = _mm_set1_epi16(512);
    let sp = src.as_ptr();
    let dp = dst.as_mut_ptr();
    let pair = |r: *const u8| -> __m128i {
        _mm_unpacklo_epi8(
            _mm_cvtsi32_si128(r.cast::<u32>().read_unaligned() as i32),
            _mm_cvtsi32_si128(r.add(1).cast::<u32>().read_unaligned() as i32),
        )
    };
    let mut ab = pair(sp);
    for y in 0..height {
        let cd = pair(sp.add((y + 1) * src_stride));
        let v = _mm_add_epi16(_mm_maddubs_epi16(ab, wab), _mm_maddubs_epi16(cd, wcd));
        let r = _mm_mulhrs_epi16(v, k512);
        let packed = _mm_packus_epi16(r, r);
        dp.add(y * dst_stride).cast::<u32>().write_unaligned(_mm_cvtsi128_si32(packed) as u32);
        ab = cd;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn mc_chroma_w8_neon(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let (wa, wb) = (abcd[0] as u16, abcd[1] as u16);
    let (wc, wd) = (abcd[2] as u16, abcd[3] as u16);
    let sp = src.as_ptr();
    let dp = dst.as_mut_ptr();
    for y in 0..height {
        let r0 = sp.add(y * src_stride);
        let r1 = r0.add(src_stride);
        let mut v = vmulq_n_u16(vmovl_u8(vld1_u8(r0)), wa);
        v = vmlaq_n_u16(v, vmovl_u8(vld1_u8(r0.add(1))), wb);
        v = vmlaq_n_u16(v, vmovl_u8(vld1_u8(r1)), wc);
        v = vmlaq_n_u16(v, vmovl_u8(vld1_u8(r1.add(1))), wd);
        // rounding shift-right by 6 AND narrow to u8 in one op: (v + 32) >> 6.
        // Max is 16352 >> 6 == 255, so the truncating narrow cannot clip.
        vst1_u8(dp.add(y * dst_stride), vrshrn_n_u16::<6>(v));
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn mc_chroma_w4_neon(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    use core::arch::aarch64::*; // VENDOR(uefi-player): `std::arch` => `core` (same intrinsics, no_std-safe).
    let (wa, wb) = (abcd[0] as u16, abcd[1] as u16);
    let (wc, wd) = (abcd[2] as u16, abcd[3] as u16);
    let sp = src.as_ptr();
    let dp = dst.as_mut_ptr();
    // 4 readable bytes only (see module docs) -> load via u32 lane, not vld1_u8.
    let ld4 = |p: *const u8| -> uint16x8_t {
        let w = p.cast::<u32>().read_unaligned();
        vmovl_u8(vreinterpret_u8_u32(vdup_n_u32(w)))
    };
    for y in 0..height {
        let r0 = sp.add(y * src_stride);
        let r1 = r0.add(src_stride);
        let mut v = vmulq_n_u16(ld4(r0), wa);
        v = vmlaq_n_u16(v, ld4(r0.add(1)), wb);
        v = vmlaq_n_u16(v, ld4(r1), wc);
        v = vmlaq_n_u16(v, ld4(r1.add(1)), wd);
        let narrowed = vrshrn_n_u16::<6>(v);
        let out = vget_lane_u32::<0>(vreinterpret_u32_u8(narrowed));
        dp.add(y * dst_stride).cast::<u32>().write_unaligned(out);
    }
}

/// Eighth-pel chroma bilinear MC, **8 pixels wide**, `height` rows.
///
/// Reads a 9×(height+1) tile from `src`; writes 8×height to `dst`.
pub fn mc_chroma_w8(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    assert!(src_stride >= 9 && src.len() >= height * src_stride + 9);
    assert!(dst.len() >= (height - 1) * dst_stride + 8);
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_CHROMA_W8.scalar();
        return mc_chroma_scalar(src, src_stride, dst, dst_stride, abcd, 8, height);
    }
    #[cfg(target_arch = "x86_64")]
    {
        if true /* SSE2 is x86-64 baseline; see deblock_simd for why gating costs */ {
            // SAFETY: bounds asserted above; the kernel reads 9×(height+1) and writes
            // 8×height, and sse2 is present.
            unsafe { mc_chroma_w8_sse2(src, src_stride, dst, dst_stride, abcd, height) };
            crate::census::MC_CHROMA_W8.base();
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if crate::have_neon() { // VENDOR(uefi-player): no_std => baseline-true (see lib.rs).
            // SAFETY: as above; neon is present.
            unsafe { mc_chroma_w8_neon(src, src_stride, dst, dst_stride, abcd, height) };
            crate::census::MC_CHROMA_W8.base();
            return;
        }
    }
    crate::census::MC_CHROMA_W8.scalar();
    mc_chroma_scalar(src, src_stride, dst, dst_stride, abcd, 8, height);
}

/// Eighth-pel chroma bilinear MC, **4 pixels wide**, `height` rows.
///
/// Reads a 5×(height+1) tile from `src`; writes 4×height to `dst`.
pub fn mc_chroma_w4(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    abcd: &[u8; 4],
    height: usize,
) {
    assert!(src_stride >= 5 && src.len() >= height * src_stride + 5);
    assert!(dst.len() >= (height - 1) * dst_stride + 4);
    // VENDOR(uefi-player): firmware single-bit gate — off means the scalar twin.
    if !crate::simd_enabled() {
        crate::census::MC_CHROMA_W4.scalar();
        return mc_chroma_scalar(src, src_stride, dst, dst_stride, abcd, 4, height);
    }
    #[cfg(target_arch = "x86_64")]
    {
        if true /* SSE2 is x86-64 baseline; see deblock_simd for why gating costs */ {
            // SAFETY: bounds asserted; 4-byte loads stay inside the 5-wide guarantee.
            unsafe { mc_chroma_w4_sse2(src, src_stride, dst, dst_stride, abcd, height) };
            crate::census::MC_CHROMA_W4.base();
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if crate::have_neon() { // VENDOR(uefi-player): no_std => baseline-true (see lib.rs).
            // SAFETY: as above.
            unsafe { mc_chroma_w4_neon(src, src_stride, dst, dst_stride, abcd, height) };
            crate::census::MC_CHROMA_W4.base();
            return;
        }
    }
    crate::census::MC_CHROMA_W4.scalar();
    mc_chroma_scalar(src, src_stride, dst, dst_stride, abcd, 4, height);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random fill — no dev-dependency, reproducible failures.
    fn fill(buf: &mut [u8], mut seed: u32) {
        for b in buf.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (seed >> 24) as u8;
        }
    }

    /// Every legal (fx, fy) pair, every legal height, against the scalar oracle.
    /// This is the gate that lets the assembly be deleted.
    #[test]
    fn simd_matches_scalar_w8() {
        for fx in 0..8u32 {
            for fy in 0..8u32 {
                let abcd = [
                    ((8 - fx) * (8 - fy)) as u8,
                    (fx * (8 - fy)) as u8,
                    ((8 - fx) * fy) as u8,
                    (fx * fy) as u8,
                ];
                assert_eq!(abcd.iter().map(|&x| x as u32).sum::<u32>(), 64);
                for &h in &[2usize, 4, 8, 16] {
                    let stride = 16;
                    let mut src = vec![0u8; (h + 2) * stride + 16];
                    fill(&mut src, 0xC0FFEE ^ (fx * 31 + fy));
                    let (mut a, mut b) = (vec![0u8; h * 8], vec![0u8; h * 8]);
                    mc_chroma_w8(&src, stride, &mut a, 8, &abcd, h);
                    mc_chroma_scalar(&src, stride, &mut b, 8, &abcd, 8, h);
                    assert_eq!(a, b, "w8 mismatch fx={fx} fy={fy} h={h}");
                }
            }
        }
    }

    #[test]
    fn simd_matches_scalar_w4() {
        for fx in 0..8u32 {
            for fy in 0..8u32 {
                let abcd = [
                    ((8 - fx) * (8 - fy)) as u8,
                    (fx * (8 - fy)) as u8,
                    ((8 - fx) * fy) as u8,
                    (fx * fy) as u8,
                ];
                for &h in &[2usize, 4, 8] {
                    let stride = 8;
                    let mut src = vec![0u8; (h + 2) * stride + 8];
                    fill(&mut src, 0xBEEF ^ (fx * 17 + fy));
                    let (mut a, mut b) = (vec![0u8; h * 4], vec![0u8; h * 4]);
                    mc_chroma_w4(&src, stride, &mut a, 4, &abcd, h);
                    mc_chroma_scalar(&src, stride, &mut b, 4, &abcd, 4, h);
                    assert_eq!(a, b, "w4 mismatch fx={fx} fy={fy} h={h}");
                }
            }
        }
    }

    /// Saturating edges: all-zero and all-255 inputs at the extreme weightings.
    #[test]
    fn simd_matches_scalar_at_extremes() {
        for &v in &[0u8, 255] {
            for abcd in [[64, 0, 0, 0], [0, 64, 0, 0], [0, 0, 0, 64], [16, 16, 16, 16]] {
                let (h, stride) = (8usize, 16usize);
                let src = vec![v; (h + 2) * stride + 16];
                let (mut a, mut b) = (vec![0u8; h * 8], vec![0u8; h * 8]);
                mc_chroma_w8(&src, stride, &mut a, 8, &abcd, h);
                mc_chroma_scalar(&src, stride, &mut b, 8, &abcd, 8, h);
                assert_eq!(a, b, "extreme mismatch v={v} abcd={abcd:?}");
                assert!(a.iter().all(|&x| x == v), "bilinear of a constant must be that constant");
            }
        }
    }
}
