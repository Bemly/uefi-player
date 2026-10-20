//! Unit tests for the `loop_filter` module (split out per the out-of-line test convention).

use super::*;

#[test]
fn lvl_lookup_without_deltas_is_flat_level() {
    let lf = LoopFilterParams {
        level: 20,
        sharpness: 0,
        delta_enabled: false,
        ref_deltas: [1, 0, -1, -1],
        mode_deltas: [0, 0],
    };
    let table = build_lvl_lookup(&lf, &SegmentationParams::default());
    for seg in table.iter() {
        for r in seg.iter() {
            for m in r.iter() {
                assert_eq!(*m, 20);
            }
        }
    }
}

#[test]
fn lvl_lookup_applies_intra_ref_delta() {
    // level=40 -> nShift = 40>>5 = 1. Default of ref_deltas[INTRA_FRAME] is 1.
    let lf = LoopFilterParams {
        level: 40,
        sharpness: 0,
        delta_enabled: true,
        ref_deltas: [1, 0, -1, -1],
        mode_deltas: [0, 0],
    };
    let table = build_lvl_lookup(&lf, &SegmentationParams::default());
    // intraLvl = 40 + 1*(1<<1) = 42
    assert_eq!(table[0][INTRA_FRAME as usize][0], 42);
}

#[test]
fn lvl_lookup_seg_lvl_alt_l_absolute_override() {
    let lf = LoopFilterParams {
        level: 20,
        sharpness: 0,
        delta_enabled: false,
        ref_deltas: [1, 0, -1, -1],
        mode_deltas: [0, 0],
    };
    let mut seg = SegmentationParams {
        enabled: true,
        abs_or_delta_update: true,
        ..SegmentationParams::default()
    };
    seg.feature_enabled[3][SEG_LVL_ALT_L] = true;
    seg.feature_data[3][SEG_LVL_ALT_L] = 50;
    let table = build_lvl_lookup(&lf, &seg);
    // Segment 3 uses the absolute override (50); other segments stay flat at 20.
    for m in table[3].iter().flatten() {
        assert_eq!(*m, 50);
    }
    for m in table[0].iter().flatten() {
        assert_eq!(*m, 20);
    }
}

#[test]
fn lvl_lookup_seg_lvl_alt_l_delta_is_clipped() {
    let lf = LoopFilterParams {
        level: 60,
        sharpness: 0,
        delta_enabled: false,
        ref_deltas: [1, 0, -1, -1],
        mode_deltas: [0, 0],
    };
    let mut seg = SegmentationParams {
        enabled: true,
        ..SegmentationParams::default()
    };
    seg.feature_enabled[2][SEG_LVL_ALT_L] = true;
    seg.feature_data[2][SEG_LVL_ALT_L] = 10; // 60 + 10 = 70, clipped to 63.
    let table = build_lvl_lookup(&lf, &seg);
    for m in table[2].iter().flatten() {
        assert_eq!(*m, 63);
    }
}

#[test]
fn adaptive_filter_strength_zero_sharpness() {
    let (limit, blimit, thresh) = adaptive_filter_strength(20, 0, 8);
    assert_eq!(limit, 20);
    assert_eq!(blimit, 2 * (20 + 2) + 20);
    assert_eq!(thresh, 20 >> 4);
}

#[test]
fn narrow_filter_flat_input_is_noop_like() {
    // A perfectly flat input (all the same value) should be unchanged by filtering.
    let mut p = Plane::new(8, 1);
    for x in 0..8 {
        p.set(x, 0, 128);
    }
    narrow_filter(&mut p, 4, 0, 1, 0, false, 8);
    for x in 0..8 {
        assert_eq!(p.get(x, 0), 128);
    }
}

/// Minimal xorshift32 PRNG for the randomized SIMD-vs-scalar test below -- deterministic
/// (fixed seed) so the test is reproducible, and avoids pulling in a `rand` dev-dep for
/// one test.
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Direct equivalence test for `simd::loop_filter_horiz8_avx2` (SIMD wave 3): for many
/// random pixel windows and random per-lane `eligible`/`is_tx8`/`limit`/`blimit`/`thresh`
/// combinations, the AVX2 kernel's output must exactly match calling this file's own
/// (already spec-conformant) `sample_filtering` scalar function once per lane. This is a
/// stronger, finer-grained bit-exactness proof than the official-vector sweep: it
/// exercises every combination of narrow-selected/wide8-selected/filter_mask-false/
/// ineligible lanes directly, including mixes within a single 8-lane batch, which a
/// handful of real conformance vectors may not happen to hit simultaneously.
#[test]
#[cfg(target_arch = "x86_64")]
fn avx2_horiz8_matches_scalar_sample_filtering() {
    if !crate::simd::avx2_enabled() {
        // No AVX2 on this host -- the kernel is never dispatched to at runtime either
        // (see `avx2_enabled()`'s use at the `superblock_loop_filter` call site), so
        // there's nothing to cross-check here.
        return;
    }

    let width = 8usize;
    let height = 16usize;
    let y0 = 8usize;
    let mut seed = 0xC0FFEEu32;

    for trial in 0..500u32 {
        // Half the trials use a near-flat window (base value +/- {0,1}) so `flat_mask` and
        // `flat_mask2` (threshold 1) actually hold and wide8/wide16 get *selected*, not
        // just computed; the other half are fully random (filter_mask false, hev, narrow).
        let flat = trial % 2 == 1;
        let base = (xorshift32(&mut seed) & 0xFF) as i32;
        let mut plane_scalar = Plane::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let v = if flat {
                    (base + (xorshift32(&mut seed) % 2) as i32).clamp(0, 255) as u16
                } else {
                    (xorshift32(&mut seed) & 0xFF) as u16
                };
                plane_scalar.set(x, y, v);
            }
        }
        let mut plane_simd = plane_scalar.clone();

        let mut eligible = [0i32; 8];
        let mut is_tx8 = [0i32; 8];
        let mut is_tx16 = [0i32; 8];
        let mut limit = [0i32; 8];
        let mut blimit = [0i32; 8];
        let mut thresh = [0i32; 8];
        for lane in 0..8usize {
            let elig = !xorshift32(&mut seed).is_multiple_of(5); // ~80% eligible
                                                                 // filter_size across all three arms (TX_4X4 narrow / TX_8X8 wide8 / TX_16X16
                                                                 // wide2), so a batch mixes narrow-, wide8- and wide16-selected lanes.
            let filter_size = match xorshift32(&mut seed) % 3 {
                1 => TX_8X8,
                2 => TX_16X16,
                _ => TX_4X4,
            };
            eligible[lane] = if elig { -1 } else { 0 };
            is_tx8[lane] = if filter_size == TX_8X8 { -1 } else { 0 };
            is_tx16[lane] = if filter_size == TX_16X16 { -1 } else { 0 };
            // Wide-ranging (not just spec-plausible) limit/blimit/thresh: the kernel and
            // `sample_filtering` are just fixed integer arithmetic over whatever's passed
            // in, so exercising a broad range is a stronger, still-valid check.
            limit[lane] = (xorshift32(&mut seed) % 64) as i32;
            blimit[lane] = (xorshift32(&mut seed) % 200) as i32;
            thresh[lane] = (xorshift32(&mut seed) % 16) as i32;

            if elig {
                sample_filtering(
                    &mut plane_scalar,
                    lane,
                    y0,
                    0,
                    1,
                    limit[lane],
                    blimit[lane],
                    thresh[lane],
                    filter_size,
                    8,
                );
            }
        }

        // SAFETY: avx2_enabled() confirmed above; the plane is 16 rows tall with y0==8, so
        // both the narrow rows y0-4..=y0+3 and the wide16 rows y0-8..=y0+7 (== rows 0..=15)
        // are all in bounds.
        unsafe {
            crate::simd::loop_filter_horiz8_avx2(
                plane_simd.as_mut_slice().as_mut_ptr(),
                width,
                0,
                y0,
                &eligible,
                &is_tx8,
                &is_tx16,
                &limit,
                &blimit,
                &thresh,
                8,
            );
        }

        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    plane_scalar.get(x, y),
                    plane_simd.get(x, y),
                    "trial {trial}: mismatch at column (lane) {x}, row {y}"
                );
            }
        }
    }
}

/// Direct equivalence test for `simd::loop_filter_vert8_avx2` (SIMD wave 4), the transpose of
/// `avx2_horiz8_matches_scalar_sample_filtering` above. The vertical kernel transposes the tap
/// window, reuses the horizontal kernel, and transposes back; this checks the round trip is
/// bit-exact against the scalar `sample_filtering` on a vertical edge (`dx=1,dy=0`) across the
/// same narrow/wide8/wide16/ineligible mix. The plane is 8 rows (the 8 lanes) by 16 columns, so
/// both the narrow tap window (x0-4..=x0+3) and the wide16 window (x0-8..=x0+7, == cols 0..=15
/// with x0==8) are in bounds.
#[test]
#[cfg(target_arch = "x86_64")]
fn avx2_vert8_matches_scalar_sample_filtering() {
    if !crate::simd::avx2_enabled() {
        return;
    }

    let width = 16usize; // taps x0-8..=x0+7 == columns 0..=15
    let height = 8usize; // 8 along-edge lanes == rows 0..=7
    let x0 = 8usize;
    let mut seed = 0x1234BEEFu32;

    for trial in 0..500u32 {
        let flat = trial % 2 == 1;
        let base = (xorshift32(&mut seed) & 0xFF) as i32;
        let mut plane_scalar = Plane::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let v = if flat {
                    (base + (xorshift32(&mut seed) % 2) as i32).clamp(0, 255) as u16
                } else {
                    (xorshift32(&mut seed) & 0xFF) as u16
                };
                plane_scalar.set(x, y, v);
            }
        }
        let mut plane_simd = plane_scalar.clone();

        let mut eligible = [0i32; 8];
        let mut is_tx8 = [0i32; 8];
        let mut is_tx16 = [0i32; 8];
        let mut limit = [0i32; 8];
        let mut blimit = [0i32; 8];
        let mut thresh = [0i32; 8];
        for lane in 0..8usize {
            let elig = !xorshift32(&mut seed).is_multiple_of(5);
            let filter_size = match xorshift32(&mut seed) % 3 {
                1 => TX_8X8,
                2 => TX_16X16,
                _ => TX_4X4,
            };
            eligible[lane] = if elig { -1 } else { 0 };
            is_tx8[lane] = if filter_size == TX_8X8 { -1 } else { 0 };
            is_tx16[lane] = if filter_size == TX_16X16 { -1 } else { 0 };
            limit[lane] = (xorshift32(&mut seed) % 64) as i32;
            blimit[lane] = (xorshift32(&mut seed) % 200) as i32;
            thresh[lane] = (xorshift32(&mut seed) % 16) as i32;

            if elig {
                // Vertical edge: position (x0, lane), taps along the row (dx=1, dy=0).
                sample_filtering(
                    &mut plane_scalar,
                    x0,
                    lane,
                    1,
                    0,
                    limit[lane],
                    blimit[lane],
                    thresh[lane],
                    filter_size,
                    8,
                );
            }
        }

        // SAFETY: avx2_enabled() confirmed above; the plane is 16 columns wide with x0==8, so
        // both the narrow columns x0-4..=x0+3 and the wide16 columns x0-8..=x0+7 (== columns
        // 0..=15) are in bounds across all 8 rows (y0==0, rows 0..=7).
        unsafe {
            crate::simd::loop_filter_vert8_avx2(
                plane_simd.as_mut_slice().as_mut_ptr(),
                width,
                x0,
                0,
                &eligible,
                &is_tx8,
                &is_tx16,
                &limit,
                &blimit,
                &thresh,
                8,
            );
        }

        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    plane_scalar.get(x, y),
                    plane_simd.get(x, y),
                    "trial {trial}: mismatch at column {x}, row (lane) {y}"
                );
            }
        }
    }
}
