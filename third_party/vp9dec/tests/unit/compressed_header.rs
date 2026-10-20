use super::*;
use crate::prob_tables::ONLY_4X4;
use crate::unit_test_support::{minimal_new_frame_header, BoolEncoder};

/// Builds a minimal key-frame `NewFrameHeader` for these tests: `quantization.lossless` is
/// the value under test; every other field holds what a real key frame's header would
/// (`frame_is_intra = true`, so the four inter-only fields `interpolation_filter`/
/// `ref_frame_sign_bias`/`allow_high_precision_mv` are never actually read by
/// `parse_compressed_header`, but are filled with the same values a real key frame's
/// parse produces -- see `header.rs`'s key-frame branch).
fn key_frame_header(lossless: bool) -> NewFrameHeader {
    let mut header = minimal_new_frame_header(8, 8);
    header.quantization.lossless = lossless;
    header
}

#[test]
fn lossless_frame_forces_only_4x4_and_reads_no_extra_bit() {
    // When lossless == true, tx_mode is always ONLY_4X4 and nothing is
    // read from the bitstream (read_tx_mode's if branch is skipped
    // entirely), so only the subsequent read_coef_probs (txSz is TX_4X4
    // only) and read_skip_prob are encoded.
    let mut enc = BoolEncoder::new();
    enc.write_literal(0, 1); // read_coef_probs: txSz=TX_4X4, update_probs=0
    enc.write_bool(false, 252); // read_skip_prob[0]: update_prob=0
    enc.write_bool(false, 252); // read_skip_prob[1]
    enc.write_bool(false, 252); // read_skip_prob[2]
    let buf = enc.finish();

    let header = parse_compressed_header(&buf, &key_frame_header(true), FrameContext::default())
        .expect("should parse");
    assert_eq!(header.tx_mode, ONLY_4X4);
    assert_eq!(*header.probs, CompressedHeaderProbs::default());
}

#[test]
fn non_lossless_reads_two_bit_tx_mode_without_select() {
    // Reads tx_mode = ALLOW_16X16 (=2); since it's not ALLOW_32X32, no extra bit is read.
    // maxTxSize = TX_16X16, so read_coef_probs runs 3 times for TX_4X4, TX_8X8, TX_16X16.
    let mut enc = BoolEncoder::new();
    enc.write_literal(2, 2); // tx_mode = ALLOW_16X16
    enc.write_literal(0, 1); // txSz=TX_4X4 update_probs=0
    enc.write_literal(0, 1); // txSz=TX_8X8 update_probs=0
    enc.write_literal(0, 1); // txSz=TX_16X16 update_probs=0
    enc.write_bool(false, 252);
    enc.write_bool(false, 252);
    enc.write_bool(false, 252);
    let buf = enc.finish();

    let header = parse_compressed_header(&buf, &key_frame_header(false), FrameContext::default())
        .expect("should parse");
    assert_eq!(header.tx_mode, 2); // ALLOW_16X16
    assert_eq!(*header.probs, CompressedHeaderProbs::default());
}

#[test]
fn tx_mode_select_reads_tx_mode_probs_and_full_coef_range() {
    // tx_mode = ALLOW_32X32 (=3) + tx_mode_select(1) = TX_MODE_SELECT (=4)
    let mut enc = BoolEncoder::new();
    enc.write_literal(3, 2); // tx_mode raw = ALLOW_32X32
    enc.write_literal(1, 1); // tx_mode_select = 1 -> tx_mode = TX_MODE_SELECT
                             // tx_mode_probs(): 8x8(2*1) + 16x16(2*2) + 32x32(2*3) = 12 calls to diff_update_prob
    for _ in 0..12 {
        enc.write_bool(false, 252);
    }
    // read_coef_probs: maxTxSize = TX_32X32, so txSz = 0..=3, 4 iterations
    for _ in 0..4 {
        enc.write_literal(0, 1);
    }
    enc.write_bool(false, 252);
    enc.write_bool(false, 252);
    enc.write_bool(false, 252);
    let buf = enc.finish();

    let header = parse_compressed_header(&buf, &key_frame_header(false), FrameContext::default())
        .expect("should parse");
    assert_eq!(header.tx_mode, TX_MODE_SELECT);
    assert_eq!(*header.probs, CompressedHeaderProbs::default());
}

#[test]
fn diff_update_prob_actually_changes_skip_prob() {
    // Updates skip_prob[0]: update_prob=1, decode_term_subexp()'s first
    // branch (bit=0 -> sub_exp_val (L(4))) yields deltaProb=5, and inv_remap_prob(5, 192) is applied.
    let mut enc = BoolEncoder::new();
    enc.write_literal(0, 1); // read_coef_probs: txSz=TX_4X4, update_probs=0
    enc.write_bool(true, 252); // skip_prob[0]: update_prob=1
    enc.write_literal(0, 1); // decode_term_subexp: bit=0
    enc.write_literal(5, 4); // sub_exp_val = 5
    enc.write_bool(false, 252); // skip_prob[1]: update_prob=0
    enc.write_bool(false, 252); // skip_prob[2]: update_prob=0
    let buf = enc.finish();

    let header = parse_compressed_header(&buf, &key_frame_header(true), FrameContext::default())
        .expect("should parse");
    let expected = inv_remap_prob(5, DEFAULT_SKIP_PROB[0]);
    assert_eq!(header.probs.skip_prob[0], expected);
    assert_ne!(header.probs.skip_prob[0], DEFAULT_SKIP_PROB[0]);
    assert_eq!(header.probs.skip_prob[1], DEFAULT_SKIP_PROB[1]);
    assert_eq!(header.probs.skip_prob[2], DEFAULT_SKIP_PROB[2]);
}

#[test]
fn inv_recenter_nonneg_matches_spec_cases() {
    // v > 2m -> returns v as-is
    assert_eq!(inv_recenter_nonneg(10, 3), 10);
    // v is odd -> m - (v+1)/2
    assert_eq!(inv_recenter_nonneg(3, 5), 5 - 2);
    // v is even -> m + v/2
    assert_eq!(inv_recenter_nonneg(4, 5), 5 + 2);
}

#[test]
fn empty_data_is_rejected() {
    let data: [u8; 0] = [];
    assert_eq!(
        parse_compressed_header(&data, &key_frame_header(true), FrameContext::default())
            .unwrap_err(),
        CompressedHeaderError::BoolCoder(BoolCoderError::EmptyBuffer)
    );
}
