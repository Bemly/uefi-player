//! Rust implementation of a deblocking filter inspired by ITU-T H.263 Annex J.
//! This is intended to be used as a postprocessing step, not as a loop filter.

/// Table J.2/H.263 - Relationship between QUANT and STRENGTH of filter; [0] is not to be used
pub const QUANT_TO_STRENGTH: [u8; 32] = [
    0, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10, 10, 11, 11, 11,
    12, 12, 12,
];

mod scalar_impl {
    /// Figure J.2/H.263 – Parameter d1 as a function of parameter d for deblocking filter mode
    #[inline]
    fn up_down_ramp(x: i16, strength: i16) -> i16 {
        x.signum() * (x.abs() - (2 * (x.abs() - strength)).max(0)).max(0)
    }

    /// Clips x to the range +/- abs(lim)
    #[inline]
    fn clipd1(x: i16, lim: i16) -> i16 {
        x.clamp(-lim.abs(), lim.abs())
    }

    /// Operates the filter on a set of four (clipped) pixel values on a horizontal or
    /// vertical line of the picture, of which A and B belong to one block, and C and D
    /// belong to a neighbouring block which is to the right of or below the first block.
    /// Figure J.1 shows examples for the position of these pixels.
    #[allow(non_snake_case)]
    #[inline]
    pub fn process(A: &mut u8, B: &mut u8, C: &mut u8, D: &mut u8, strength: u8) {
        debug_assert!((1..=12).contains(&strength));

        let (a16, b16, c16, d16) = (*A as i16, *B as i16, *C as i16, *D as i16);

        let d: i16 = (a16 - 4 * b16 + 4 * c16 - d16) / 8;
        let d1: i16 = up_down_ramp(d, strength as i16);
        let d2: i16 = clipd1((a16 - d16) / 4, d1 / 2);

        *A = (a16 - d2) as u8;
        *B = (b16 + d1).clamp(0, 255) as u8;
        *C = (c16 - d1).clamp(0, 255) as u8;
        *D = (d16 + d2) as u8;
    }
}

// VENDOR(uefi-player): `simd_impl` needs the `simd` feature (wide); without
// it the scalar kernel below filters every edge instead (see the twins in
// `deblock_horiz` / `deblock_vert`). `wide` itself is `#![no_std]` — its
// `std` feature only speeds up float `sqrt`, which this integer kernel never
// touches — so this module also builds for firmware
// (`--no-default-features --features simd`).
#[cfg(feature = "simd")]
mod simd_impl {
    // VENDOR(uefi-player): std-only module (wide SIMD); `core::ops` so the
    // import itself is shape-neutral.
    use core::ops::Shr;
    use wide::i16x8;

    /// Same as `scalar::up_down_ramp`, but operates on a vector of 8 values in parallel
    #[inline]
    fn up_down_ramp_simd(x: i16x8, strength: i16) -> i16x8 {
        x.signum()
            * (x.abs() - (2 * (x.abs() - i16x8::splat(strength))).max(i16x8::ZERO)).max(i16x8::ZERO)
    }

    /// Same as `scalar::clipd1`, but operates on a vector of 8 values in parallel
    #[inline]
    fn clipd1_simd(x: i16x8, lim: i16x8) -> i16x8 {
        let la = lim.abs();
        x.clamp(-la, la)
    }

    /// Utility to upcast and convert an array of 8 `u8` values into a `i16x8` vector.
    #[inline]
    fn into_simd16(a: &[u8; 8]) -> i16x8 {
        // Spelled out on purpose: `i16x8::from(a.map(i16::from))` made `deblock` ~40% slower.
        i16x8::from([
            a[0] as i16,
            a[1] as i16,
            a[2] as i16,
            a[3] as i16,
            a[4] as i16,
            a[5] as i16,
            a[6] as i16,
            a[7] as i16,
        ])
    }

    /// Same as `scalar::process`, but performs it on 8 independent sets of values in parallel.
    #[allow(non_snake_case)]
    #[inline]
    pub fn process_simd(
        A: &mut [u8; 8],
        B: &mut [u8; 8],
        C: &mut [u8; 8],
        D: &mut [u8; 8],
        strength: u8,
    ) {
        debug_assert!((1..=12).contains(&strength));

        let a16 = into_simd16(A);
        let b16 = into_simd16(B);
        let c16 = into_simd16(C);
        let d16 = into_simd16(D);

        let d: i16x8 = (a16 - 4 * b16 + 4 * c16 - d16).shr(3);
        let d1: i16x8 = up_down_ramp_simd(d, strength as i16);
        let d2: i16x8 = clipd1_simd((a16 - d16).shr(2), d1.shr(1));

        let res_a = a16 - d2;
        let res_b = (b16 + d1).clamp(i16x8::ZERO, i16x8::splat(255));
        let res_c = (c16 - d1).clamp(i16x8::ZERO, i16x8::splat(255));
        let res_d = d16 + d2;

        let res_a = res_a.as_array();
        let res_b = res_b.as_array();
        let res_c = res_c.as_array();
        let res_d = res_d.as_array();

        for i in 0..8 {
            A[i] = res_a[i] as u8;
            B[i] = res_b[i] as u8;
            C[i] = res_c[i] as u8;
            D[i] = res_d[i] as u8;
        }
    }
}

// VENDOR(uefi-player): no_std port — `alloc::Vec` for the `deblock` return.
use alloc::vec::Vec;
// VENDOR(uefi-player): `izip!` drives the `simd`-gated 8-row parallel loop;
// the scalar twin below needs no zipping.
#[cfg(feature = "simd")]
use itertools::izip;
use scalar_impl::process;
// VENDOR(uefi-player): SIMD entry point, `simd`-gated.
#[cfg(feature = "simd")]
use simd_impl::process_simd;

// VENDOR(uefi-player): firmware gate — the single-bit `[accel]` switch backer
// (`scalar` => firmware calls `set_simd_enabled(false)`, any other level =>
// `true` once `ensure_xsave_enabled` has run; see VENDOR.md for the wiring
// contract — this crate never probes CPUID itself, it only obeys). The kernel
// only ever emits SSE2-baseline integer math (x86-64 psABI guaranteed), so the
// switch is config semantics, not a safety gate: ON under `std` (upstream ran
// SIMD unconditionally), OFF under no_std (fail-closed until firmware opts
// in). `Relaxed` suffices — both paths are byte-identical by test, so a torn
// read can at worst mix paths inside one frame, never corrupt it.
#[cfg(feature = "simd")]
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "simd")]
static SIMD_ENABLED: AtomicBool = AtomicBool::new(cfg!(feature = "std"));

/// Enable (`true`) or disable (`false`) the wide-SIMD fast paths at runtime.
/// No-op unless the `simd` feature is on, so callers need no `cfg`.
pub fn set_simd_enabled(en: bool) {
    #[cfg(feature = "simd")]
    SIMD_ENABLED.store(en, Ordering::Relaxed);
    #[cfg(not(feature = "simd"))]
    let _ = en;
}

#[cfg(feature = "simd")]
#[inline]
fn simd_enabled() -> bool {
    SIMD_ENABLED.load(Ordering::Relaxed)
}

/// Applies the deblocking with the given strength to the horizontal block edges.
#[allow(non_snake_case)]
fn deblock_horiz(result: &mut [u8], width: usize, strength: u8) {
    let height = result.len() / width;

    let mut edge_y = 8; // the vertical index of the row with the "C" samples
    while edge_y <= height - 2 {
        // breaking out four rows, one for each of the ABCD samples
        let (_, rest) = result.split_at_mut((edge_y - 2) * width);
        let (row_a, rest) = rest.split_at_mut(width);
        let (row_b, rest) = rest.split_at_mut(width);
        let (row_c, rest) = rest.split_at_mut(width);
        let (row_d, _) = rest.split_at_mut(width);

        // VENDOR(uefi-player): runtime dispatch — SIMD fast path when opted
        // in, full-scalar twin otherwise. With `simd` off only the twin
        // compiles and the branch folds away.
        #[cfg(feature = "simd")]
        if simd_enabled() {
            // the first N*8 samples (horizontally) are handled by the SIMD implementation
            let (row_a_chunks, row_a_rem) = row_a.as_chunks_mut::<8>();
            let (row_b_chunks, row_b_rem) = row_b.as_chunks_mut::<8>();
            let (row_c_chunks, row_c_rem) = row_c.as_chunks_mut::<8>();
            let (row_d_chunks, row_d_rem) = row_d.as_chunks_mut::<8>();

            // luckily the memory layout is advantageous here, no need for transposing
            // chunks into the SIMD lanes
            for (((A, B), C), D) in row_a_chunks
                .iter_mut()
                .zip(row_b_chunks)
                .zip(row_c_chunks)
                .zip(row_d_chunks)
            {
                process_simd(A, B, C, D, strength);
            }

            // the remaining <=7 columns are handled by the scalar implementation
            for (((A, B), C), D) in row_a_rem
                .iter_mut()
                .zip(row_b_rem)
                .zip(row_c_rem)
                .zip(row_d_rem)
            {
                process(A, B, C, D, strength);
            }
        } else {
            deblock_horiz_scalar(row_a, row_b, row_c, row_d, width, strength);
        }
        #[cfg(not(feature = "simd"))]
        deblock_horiz_scalar(row_a, row_b, row_c, row_d, width, strength);

        edge_y += 8;
    }
}

// VENDOR(uefi-player): scalar twin of one horizontal edge — same ABCD edge
// rows, every column filtered by `process` (covers the SIMD chunk range and
// the <=7 remainder alike). Always compiled: the `simd`-off shape and the
// runtime off-ramp both need it.
#[allow(non_snake_case)]
#[inline]
fn deblock_horiz_scalar(
    row_a: &mut [u8],
    row_b: &mut [u8],
    row_c: &mut [u8],
    row_d: &mut [u8],
    width: usize,
    strength: u8,
) {
    for x in 0..width {
        let (mut A, mut B, mut C, mut D) = (row_a[x], row_b[x], row_c[x], row_d[x]);
        process(&mut A, &mut B, &mut C, &mut D, strength);
        row_a[x] = A;
        row_b[x] = B;
        row_c[x] = C;
        row_d[x] = D;
    }
}

/// Applies the deblocking with the given strength to the vertical block edges.
#[allow(non_snake_case)]
fn deblock_vert(result: &mut [u8], width: usize, strength: u8) {
    // VENDOR(uefi-player): the octet helpers below feed the `simd`-gated SIMD
    // loop; the scalar twin filters the same columns instead.

    // VENDOR(uefi-player): scalar twin of one vertical-edge column quartet:
    // filters the ABCD pixels at `row[x + 4 ..= x + 7]` in a single row.
    // Always compiled: the `simd`-off shape and the runtime off-ramp both
    // need it. The caller below visits the same 8-wide chunks (from x = 2)
    // as the SIMD path, so edge coverage is identical.
    #[allow(non_snake_case)]
    #[inline]
    fn process_vert_quartet(row: &mut [u8], x: usize, strength: u8) {
        let (mut A, mut B, mut C, mut D) = (row[x + 4], row[x + 5], row[x + 6], row[x + 7]);
        process(&mut A, &mut B, &mut C, &mut D, strength);
        row[x + 4] = A;
        row[x + 5] = B;
        row[x + 6] = C;
        row[x + 7] = D;
    }

    // VENDOR(uefi-player): scalar twin of one 8-row vertical pass — same
    // 8-wide chunks from x = 2, same ABCD columns (+4..=7, i.e. edges at
    // multiples of 8), one row at a time. Always compiled, like the quartet.
    // Ten args mirror upstream's 8-tuple octet shape (borrowck can't take an
    // array of `&mut` here); not worth a wrapper struct for one caller.
    #[allow(non_snake_case, clippy::too_many_arguments)]
    #[inline]
    fn process_vert_octet(
        row_0: &mut [u8],
        row_1: &mut [u8],
        row_2: &mut [u8],
        row_3: &mut [u8],
        row_4: &mut [u8],
        row_5: &mut [u8],
        row_6: &mut [u8],
        row_7: &mut [u8],
        width: usize,
        strength: u8,
    ) {
        let mut x = 2;
        while x + 8 <= width {
            process_vert_quartet(row_0, x, strength);
            process_vert_quartet(row_1, x, strength);
            process_vert_quartet(row_2, x, strength);
            process_vert_quartet(row_3, x, strength);
            process_vert_quartet(row_4, x, strength);
            process_vert_quartet(row_5, x, strength);
            process_vert_quartet(row_6, x, strength);
            process_vert_quartet(row_7, x, strength);
            x += 8;
        }
    }

    /// Holds a bundle of 8 mutable references to 8-byte arrays.
    /// This is a tuple instead of an array due to `izip!` usage below.
    #[cfg(feature = "simd")]
    type ByteArrayMutRefOctet<'a> = (
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
        &'a mut [u8; 8],
    );

    /// Indexes into each array in a `ByteArrayMutRefOctet`, returning an array of the values.
    #[cfg(feature = "simd")]
    #[inline]
    fn extract_column(arrays: &ByteArrayMutRefOctet, i: usize) -> [u8; 8] {
        [
            arrays.0[i],
            arrays.1[i],
            arrays.2[i],
            arrays.3[i],
            arrays.4[i],
            arrays.5[i],
            arrays.6[i],
            arrays.7[i],
        ]
    }

    /// Sets a single value in each array in a `ByteArrayMutRefOctet` to the corresponding value in `a`.
    #[cfg(feature = "simd")]
    #[inline]
    fn set_column(arrays: &mut ByteArrayMutRefOctet, i: usize, a: [u8; 8]) {
        arrays.0[i] = a[0];
        arrays.1[i] = a[1];
        arrays.2[i] = a[2];
        arrays.3[i] = a[3];
        arrays.4[i] = a[4];
        arrays.5[i] = a[5];
        arrays.6[i] = a[6];
        arrays.7[i] = a[7];
    }

    // So the `[2..]`s below don't panic, also, not enough pixels to process any vertical edges otherwise.
    if width >= 10 {
        // Handling the top N*8 rows with the SIMD implementation,
        // iterating on 8 (the SIMD width) rows worth of data at a time.
        for rows in result.chunks_exact_mut(width * 8) {
            // Splitting into separate rows (doing it this way to satisfy the borrow checker),
            // each row will supply one SIMD lane.
            let (row_0, rows) = rows.split_at_mut(width);
            let (row_1, rows) = rows.split_at_mut(width);
            let (row_2, rows) = rows.split_at_mut(width);
            let (row_3, rows) = rows.split_at_mut(width);
            let (row_4, rows) = rows.split_at_mut(width);
            let (row_5, rows) = rows.split_at_mut(width);
            let (row_6, rows) = rows.split_at_mut(width);
            let (row_7, rows) = rows.split_at_mut(width);
            debug_assert!(rows.is_empty()); // should have exactly consumed the 8 rows

            // In parallel over each of the 8 rows (this is the SIMD width),
            // iterating over 8-wide chunks (this is the spacing between processed edges),
            // such that the second half of each chunk is the ABCD sample quartet
            // (this offset is done by the `[2..]` part). In the first halves of these
            // chunks are the "middle columns" of the 8x8 blocks, not to be touched.
            // This was easier than iterating over 4-wide chunks and skipping every odd one.
            // VENDOR(uefi-player): runtime dispatch — SIMD transpose path
            // when opted in (upstream verbatim), scalar octet twin otherwise.
            // With `simd` off only the twin compiles and the branch folds away.
            #[cfg(feature = "simd")]
            if simd_enabled() {
                let (row_0_chunks, _) = row_0[2..].as_chunks_mut::<8>();
            let (row_1_chunks, _) = row_1[2..].as_chunks_mut::<8>();
            let (row_2_chunks, _) = row_2[2..].as_chunks_mut::<8>();
            let (row_3_chunks, _) = row_3[2..].as_chunks_mut::<8>();
            let (row_4_chunks, _) = row_4[2..].as_chunks_mut::<8>();
            let (row_5_chunks, _) = row_5[2..].as_chunks_mut::<8>();
            let (row_6_chunks, _) = row_6[2..].as_chunks_mut::<8>();
            let (row_7_chunks, _) = row_7[2..].as_chunks_mut::<8>();
            let parallel_iter = izip!(
                row_0_chunks.iter_mut(),
                row_1_chunks.iter_mut(),
                row_2_chunks.iter_mut(),
                row_3_chunks.iter_mut(),
                row_4_chunks.iter_mut(),
                row_5_chunks.iter_mut(),
                row_6_chunks.iter_mut(),
                row_7_chunks.iter_mut()
            );

            // Transposing the (vertical) sample tuples into SIMD vectors, processing them,
            // then untransposing and storing.
            for mut arrays in parallel_iter {
                let mut As = extract_column(&arrays, 4);
                let mut Bs = extract_column(&arrays, 5);
                let mut Cs = extract_column(&arrays, 6);
                let mut Ds = extract_column(&arrays, 7);

                process_simd(&mut As, &mut Bs, &mut Cs, &mut Ds, strength);

                set_column(&mut arrays, 4, As);
                set_column(&mut arrays, 5, Bs);
                set_column(&mut arrays, 6, Cs);
                set_column(&mut arrays, 7, Ds);
            }
            } else {
                process_vert_octet(
                    row_0, row_1, row_2, row_3, row_4, row_5, row_6, row_7, width, strength,
                );
            }
            #[cfg(not(feature = "simd"))]
            process_vert_octet(
                row_0, row_1, row_2, row_3, row_4, row_5, row_6, row_7, width, strength,
            );
        }

        // The remaining <=7 rows at the bottom are handled by the scalar implementation,
        // with a similar iteration pattern as above, but with one row at a time, not in
        // parallel over an octet of rows.
        for row in result
            .chunks_exact_mut(width * 8)
            .into_remainder()
            .chunks_exact_mut(width)
        {
            let (chunks, _) = row[2..].as_chunks_mut::<8>();
            for chunk in chunks {
                let mut A = chunk[4];
                let mut B = chunk[5];
                let mut C = chunk[6];
                let mut D = chunk[7];
                process(&mut A, &mut B, &mut C, &mut D, strength);
                chunk[4] = A;
                chunk[5] = B;
                chunk[6] = C;
                chunk[7] = D;
            }
        }
    }
}

/// Applies the deblocking filter to the horizontal and vertical block edges
/// of the given image data with the given strength, assuming 8x8 block size.
#[allow(non_snake_case)]
#[allow(clippy::identity_op)]
pub fn deblock(data: &[u8], width: usize, strength: u8) -> Vec<u8> {
    debug_assert!(data.len().is_multiple_of(width));

    let mut result = data.to_vec();

    // According to the spec, the horizontal deblocking filter is applied first.
    deblock_horiz(result.as_mut(), width, strength);
    deblock_vert(result.as_mut(), width, strength);

    result
}

/// These tests serve more as an explanation/demonstration/checking of how all of the above works,
/// and regression testing, rather than requiring conformance to any externally prescribed results.
#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_process_const() {
        // For constant data of any value (equal ABCD samples), processing with
        // any strength is no-op, since there is no edge at all to iron out.
        for val in 0..=255 {
            for strength in 1..=12 {
                let (mut a, mut b, mut c, mut d) = (val, val, val, val);
                process(&mut a, &mut b, &mut c, &mut d, strength);
                assert_eq!((a, b, c, d), (val, val, val, val));
            }
        }
    }

    #[test]
    fn test_process_symmetric_input() {
        // For "XYYX"-like data of any X and Y values, processing with any strength is also no-op,
        // since there is no edge at the middle to smooth - only a "hill" or a "valley".
        for outer_val in 0..=255 {
            for inner_val in 0..=255 {
                for strength in 1..=12 {
                    let (mut a, mut b, mut c, mut d) = (outer_val, inner_val, inner_val, outer_val);
                    process(&mut a, &mut b, &mut c, &mut d, strength);
                    assert_eq!((a, b, c, d), (outer_val, inner_val, inner_val, outer_val));
                }
            }
        }
    }

    #[test]
    fn test_process() {
        #[rustfmt::skip]
        // Holds `(input, strength, output)` tuples of the test cases for `process`.
        // In both the input and the output, the first two values are in one block,
        // and the last two values are in the neighboring block, forming a line.
        #[allow(clippy::type_complexity)] // sorry, it is what it is
        let data: &[((u8, u8, u8, u8), u8, (u8, u8, u8, u8))] = &[
            // This edge is too small to do anything with it at any strength.
            ((0, 0, 1, 1), 1, (0, 0, 1, 1)),
            ((0, 0, 1, 1), 12, (0, 0, 1, 1)),

            // How smoothing with a strength of 1 behaves:
            ((0, 0, 2, 2), 1, (0, 0, 2, 2)), // Edge too small to do anything with it.
            ((0, 0, 4, 4), 1, (0, 1, 3, 4)), // Edge is smoothed nicely.
            ((0, 0, 6, 6), 1, (0, 0, 6, 6)), // Edge too large for this small strength.
            ((0, 0, 8, 8), 1, (0, 0, 8, 8)), // Edge too large for this small strength.

            // How smoothing with a strength of 2 behaves:
            ((0, 0, 2, 2), 2, (0, 0, 2, 2)), // Edge too small to do anything with it.
            ((0, 0, 4, 4), 2, (0, 1, 3, 4)), // Edge is smoothed nicely.
            ((0, 0, 6, 6), 2, (1, 2, 4, 5)), // Edge is smoothed nicely.
            ((0, 0, 8, 8), 2, (0, 1, 7, 8)), // A harder edge is smoothed just a little.

            // How smoothing with a strength of 3 behaves:
            ((0, 0, 2, 2), 3, (0, 0, 2, 2)), // Edge too small to do anything with it.
            ((0, 0, 4, 4), 3, (0, 1, 3, 4)), // Edge is smoothed nicely.
            ((0, 0, 6, 6), 3, (1, 2, 4, 5)), // Edge is smoothed nicely.
            ((0, 0, 8, 8), 3, (1, 3, 5, 7)), // Edge is smoothed nicely.

            // Increasing strength for the same edge:
            ((0, 0, 10, 10), 1, (0, 0, 10, 10)), // Edge too large for this small strength.
            ((0, 0, 10, 10), 2, (0, 1, 9, 10)),  // Edge is smoothed a little.
            ((0, 0, 10, 10), 3, (1, 3, 7, 9)),   // Edge is smoothed nicely.
            ((0, 0, 10, 10), 4, (1, 3, 7, 9)),   // Edge is smoothed nicely.
            ((0, 0, 10, 10), 12, (1, 3, 7, 9)),  // Edge is smoothed nicely.

            // Same thing with a bit stronger edge:
            ((0, 0, 20, 20), 1, (0, 0, 20, 20)),  // Edge too large for this small strength.
            ((0, 0, 20, 20), 3, (0, 0, 20, 20)),  // Edge too large for this small strength.
            ((0, 0, 20, 20), 5, (1, 3, 17, 19)),  // Edge barely smoothed.
            ((0, 0, 20, 20), 6, (2, 5, 15, 18)),  // Edge smoothed a bit more.
            ((0, 0, 20, 20), 12, (3, 7, 13, 17)), // Edge is almost entirely smoothed.

            // This edge is too large for any strength now to change.
            ((0, 0, 100, 100), 1, (0, 0, 100, 100)),
            ((0, 0, 100, 100), 12, (0, 0, 100, 100)),

            // Linear gradient:
            ((0, 80, 160, 240), 1, (0, 80, 160, 240)),  // not touched
            ((0, 80, 160, 240), 5, (0, 80, 160, 240)),  // not touched
            ((0, 80, 160, 240), 6, (1, 82, 158, 239)),  // flattened slightly
            ((0, 80, 160, 240), 12, (5, 90, 150, 235)), // flattened a bit more

            // Sawtooth pattern:
            ((0, 10, 5, 15), 2, (0, 10, 5, 15)), // not changed yet
            ((0, 10, 5, 15), 4, (2, 6, 9, 13)),  // smoothened out
            ((0, 10, 5, 15), 12, (2, 6, 9, 13)), // smoothened out

            // Step in gradient:
            ((0, 40, 40, 80), 4, (0, 40, 40, 80)),  // not touched
            ((0, 40, 40, 80), 6, (1, 38, 42, 79)),  // smoothened slightly
            ((0, 40, 40, 80), 8, (3, 34, 46, 77)),  // smoothened more
            ((0, 40, 40, 80), 10, (5, 30, 50, 75)), // smoothened well
        ];

        for (input, strength, expected) in data.iter() {
            // Checking the data as is:
            let (mut a, mut b, mut c, mut d) = *input;
            process(&mut a, &mut b, &mut c, &mut d, *strength);
            assert_eq!((a, b, c, d), *expected);

            // Test that it is symmetric wrt. direction (both left-to-right or right-to-left
            // and top-to-bottom or bottom-to-top), so applying in reverse order should be the same:
            let (mut a, mut b, mut c, mut d) = *input;
            process(&mut d, &mut c, &mut b, &mut a, *strength);
            assert_eq!((a, b, c, d), *expected);

            // Test that it is symmetric wrt. values (dark-to-bright or bright-to-dark),
            // so applying to the inverse values should give the inverse result:
            let (mut a, mut b, mut c, mut d) = *input;
            a = 255 - a;
            b = 255 - b;
            c = 255 - c;
            d = 255 - d;
            process(&mut a, &mut b, &mut c, &mut d, *strength);
            assert_eq!((255 - a, 255 - b, 255 - c, 255 - d), *expected);
        }
    }

    #[test]
    fn test_deblock() {
        // A simple 11x17 image to test deblocking on.
        // The first 8 values of the horizontal edge and the first 16 values of the vertical edge
        // will be processed by the SIMD part, and the remaining 3 and 1 values (respectively) will
        // be processed by the scalar part.
        // The 5's in the top left block should not be touched, nor should they affect anything,
        // since they are "in the middle" of the block.
        // The second horizontal edge should not be touched, as the D sample would be out of the frame.
        #[rustfmt::skip]
        let data: &[u8] = &[
            0,  0,  0,  0,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  0,  0,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  5,  5,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  5,  5,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  5,  5,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  5,  5,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  0,  0,  0,  0,  0,  0,  10, 10, 10,
            0,  0,  0,  0,  0,  0,  0,  0,  10, 10, 10,

            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,

            80, 80, 80, 80, 80, 80, 80, 80,  30, 30, 30,
        ];

        // A deblocking filter of strength 4 should nicely smooth
        // the vertical 0-10 edge at the top, should slightly smooth
        // the horizontal 0-20 edge at the left, and should not touch
        // the 10-50, 20-50 and 80-30 edges.
        #[rustfmt::skip]
        let expected_4: &[u8] = &[
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            1,  1,  1,  1,  1,  1,  2,  4,   7,  9, 10,

            19, 19, 19, 19, 19, 19, 19, 19,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,
            20, 20, 20, 20, 20, 20, 20, 20,  50, 50, 50,

            80, 80, 80, 80, 80, 80, 80, 80,  30, 30, 30,
        ];
        let result_4 = deblock(data, 11, 4);
        assert_eq!(result_4, expected_4);

        // A deblocking filter of strength 8 should nicely smooth
        // the vertical 0-10 edge at the top, should also smooth the
        // 0-20 edge at the left, should smooth the 20-50 edge a bit,
        // and should barely change the 10-50 edge on the right.
        // It should still not touch the 80-30 edge at the bottom.
        #[rustfmt::skip]
        let expected_8: &[u8] = &[
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            3,  3,  3,  3,  3,  3,  4,  5,   8,  9, 10,
            7,  7,  7,  7,  7,  7,  7,  8,  10, 11, 11,

            13, 13, 13, 13, 13, 13, 14, 16,  46, 48, 49,
            17, 17, 17, 17, 17, 17, 19, 21,  46, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,
            20, 20, 20, 20, 20, 20, 22, 25,  45, 48, 50,

            80, 80, 80, 80, 80, 80, 80, 80,  30, 30, 30,
        ];
        let result_8 = deblock(data, 11, 8);
        assert_eq!(result_8, expected_8);

        // A deblocking filter of strength 12 should nicely smooth almost
        // all edges, with only 10-50 and 80-30 being a bit less affected.
        #[rustfmt::skip]
        let expected_12: &[u8] = &[
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  0,  0,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            0,  0,  5,  5,  0,  0,  1,  3,   7,  9, 10,
            3,  3,  3,  3,  3,  3,  5,  7,  10, 12, 14,
            7,  7,  7,  7,  7,  7,  9, 11,  15, 17, 19,

            13, 13, 13, 13, 13, 13, 18, 23,  31, 36, 41,
            17, 17, 17, 17, 17, 17, 22, 27,  36, 41, 46,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,
            20, 20, 20, 20, 20, 20, 25, 31,  39, 45, 50,

            80, 80, 80, 80, 80, 80, 77, 74,  36, 33, 30,
        ];
        let result_12 = deblock(data, 11, 12);
        assert_eq!(result_12, expected_12);
    }

    // VENDOR(uefi-player): SIMD/scalar agreement. Runs under every shape with
    // the `simd` feature, flipping the runtime switch so both paths filter
    // the same frame in one binary and must agree byte for byte. Sizes sweep
    // below the vertical fast path (< 10 wide), 8-aligned frames, remainder
    // columns/rows, and heights with no horizontal edge at all; strengths
    // sweep the full 1..=12 range corners. The exact-expectation `test_deblock`
    // above keeps passing untouched under both paths; this test locks the two
    // paths to each other. Only this test writes the switch, and the paths
    // are interchangeable, so parallel test threads cannot disturb each other.
    #[cfg(feature = "simd")]
    #[test]
    fn test_deblock_simd_scalar_agree() {
        let prev = simd_enabled();
        for &(width, height) in &[
            (7usize, 20usize),
            (8, 8),
            (9, 9),
            (10, 10),
            (11, 17),
            (16, 9),
            (24, 24),
        ] {
            // Step edges in both directions so every filter branch fires.
            let data: Vec<u8> = (0..width * height)
                .map(|i| {
                    let (x, y) = (i % width, i / width);
                    ((x / 4 * 37 + y / 8 * 41 + x + y) % 251) as u8
                })
                .collect();
            for strength in [1u8, 4, 8, 12] {
                set_simd_enabled(true);
                let via_simd = deblock(&data, width, strength);
                set_simd_enabled(false);
                let via_scalar = deblock(&data, width, strength);
                assert_eq!(
                    via_simd, via_scalar,
                    "SIMD/scalar mismatch at {width}x{height} strength {strength}"
                );
            }
        }
        set_simd_enabled(prev);
    }
}
