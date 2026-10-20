//! Unit tests for the `header` module (split out per the out-of-line test convention).

use super::*;
use crate::unit_test_support::BitWriter;

/// Builds a minimal key frame uncompressed header.
/// profile=0, 8x8, lossless, segmentation disabled, no tile split.
fn build_minimal_keyframe_header() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.push_bits(2, 2); // frame_marker
    w.push_bits(0, 1); // profile_low_bit
    w.push_bits(0, 1); // profile_high_bit -> profile=0
    w.push_flag(false); // show_existing_frame
    w.push_bits(0, 1); // frame_type = KEY_FRAME
    w.push_flag(true); // show_frame
    w.push_flag(false); // error_resilient_mode
    w.push_bits(0x49, 8);
    w.push_bits(0x83, 8);
    w.push_bits(0x42, 8);
    // color_config (profile 0 -> bit_depth=8 is not read)
    w.push_bits(CS_UNKNOWN as u32, 3); // color_space
    w.push_flag(false); // color_range
                        // profile 0 -> subsampling is not read
                        // frame_size
    w.push_bits(7, 16); // frame_width_minus_1 -> width=8
    w.push_bits(7, 16); // frame_height_minus_1 -> height=8
                        // render_size
    w.push_flag(false); // render_and_frame_size_different = 0
                        // error_resilient_mode == 0 -> refresh_frame_context, frame_parallel_decoding_mode
    w.push_flag(true); // refresh_frame_context
    w.push_flag(false); // frame_parallel_decoding_mode
    w.push_bits(0, 2); // frame_context_idx
                       // loop_filter_params
    w.push_bits(0, 6); // loop_filter_level
    w.push_bits(0, 3); // loop_filter_sharpness
    w.push_flag(false); // loop_filter_delta_enabled
                        // quantization_params (all 0 -> lossless)
    w.push_bits(0, 8); // base_q_idx
    w.push_flag(false); // delta_q_y_dc coded?
    w.push_flag(false); // delta_q_uv_dc coded?
    w.push_flag(false); // delta_q_uv_ac coded?
                        // segmentation_params
    w.push_flag(false); // segmentation_enabled
                        // tile_info: width=8 -> MiCols=1, Sb64Cols=1 -> min_log2=0, max_log2=0 -> loop does not run
    w.push_bits(0, 1); // tile_rows_log2 = 0
                       // header_size_in_bytes
    w.push_bits(1, 16);

    w.finish()
}

#[test]
fn parses_minimal_keyframe_header() {
    let data = build_minimal_keyframe_header();
    let (header, _consumed) =
        parse_uncompressed_header(&data, &PersistentState::default()).expect("should parse");
    match header {
        FrameHeader::New(f) => {
            assert_eq!(f.profile, 0);
            assert_eq!(f.frame_type, FrameType::KeyFrame);
            assert!(f.show_frame);
            assert!(!f.error_resilient_mode);
            let cc = f
                .color_config
                .expect("key frame always parses color_config");
            assert_eq!(cc.bit_depth, 8);
            assert_eq!(cc.color_space, CS_UNKNOWN);
            assert_eq!(cc.subsampling_x, 1);
            assert_eq!(cc.subsampling_y, 1);
            assert_eq!(f.width, 8);
            assert_eq!(f.height, 8);
            assert_eq!(f.render_width, 8);
            assert_eq!(f.render_height, 8);
            assert_eq!(f.refresh_frame_flags, 0xFF);
            assert!(f.quantization.lossless);
            assert!(!f.segmentation.enabled);
            assert_eq!(f.tile_cols_log2, 0);
            assert_eq!(f.tile_rows_log2, 0);
            assert_eq!(f.header_size_in_bytes, 1);
            assert_eq!(f.loop_filter.ref_deltas, [1, 0, -1, -1]);
        }
        FrameHeader::ShowExistingFrame { .. } => panic!("unexpected show_existing_frame"),
    }
}

#[test]
fn rejects_bad_frame_marker() {
    let mut w = BitWriter::new();
    w.push_bits(1, 2); // frame_marker != 2
    w.push_bits(0, 30);
    let data = w.finish();
    assert_eq!(
        parse_uncompressed_header(&data, &PersistentState::default()),
        Err(HeaderError::InvalidFrameMarker)
    );
}

#[test]
fn rejects_bad_sync_code() {
    let mut w = BitWriter::new();
    w.push_bits(2, 2);
    w.push_bits(0, 1);
    w.push_bits(0, 1);
    w.push_flag(false); // show_existing_frame
    w.push_bits(0, 1); // KEY_FRAME
    w.push_flag(true);
    w.push_flag(false);
    w.push_bits(0x00, 8); // invalid sync byte
    w.push_bits(0x00, 8);
    w.push_bits(0x00, 8);
    let data = w.finish();
    assert_eq!(
        parse_uncompressed_header(&data, &PersistentState::default()),
        Err(HeaderError::InvalidSyncCode)
    );
}

/// Builds a minimal inter (non-intra-only) frame uncompressed header.
/// profile=0, error_resilient_mode=0, single reference, non-SWITCHABLE filter.
fn build_minimal_inter_frame_header() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.push_bits(2, 2); // frame_marker
    w.push_bits(0, 1); // profile_low_bit
    w.push_bits(0, 1); // profile_high_bit -> profile=0
    w.push_flag(false); // show_existing_frame
    w.push_bits(1, 1); // frame_type = NON_KEY_FRAME
    w.push_flag(true); // show_frame = 1 -> intra_only is not read (0)
    w.push_flag(false); // error_resilient_mode
                        // reset_frame_context (f(2) is read since error_resilient_mode==0)
    w.push_bits(0, 2);
    // refresh_frame_flags
    w.push_bits(0x01, 8);
    // ref_frame_idx[3] + ref_frame_sign_bias[3]
    for _ in 0..3 {
        w.push_bits(0, 3); // ref_frame_idx = 0
        w.push_flag(false); // sign_bias
    }
    // frame_size_with_refs: found_ref=1 (first slot) -> uses ref_frame_sizes[0]
    w.push_flag(true);
    // render_size
    w.push_flag(false);
    // allow_high_precision_mv
    w.push_flag(false);
    // read_interpolation_filter: is_filter_switchable=0, raw=0 (via EIGHTTAP_SMOOTH)
    w.push_flag(false);
    w.push_bits(0, 2);
    // error_resilient_mode==0 -> refresh_frame_context, frame_parallel_decoding_mode
    w.push_flag(true);
    w.push_flag(false);
    w.push_bits(0, 2); // frame_context_idx
                       // loop_filter_params
    w.push_bits(0, 6);
    w.push_bits(0, 3);
    w.push_flag(false);
    // quantization_params (lossless)
    w.push_bits(0, 8);
    w.push_flag(false);
    w.push_flag(false);
    w.push_flag(false);
    // segmentation
    w.push_flag(false);
    // tile_info: width=8 -> no loop
    w.push_bits(0, 1);
    // header_size_in_bytes
    w.push_bits(3, 16);

    w.finish()
}

#[test]
fn parses_inter_frame_using_ref_frame_size() {
    let data = build_minimal_inter_frame_header();
    let mut prev = PersistentState::default();
    prev.ref_frame_sizes[0] = (8, 8);
    let (header, _consumed) = parse_uncompressed_header(&data, &prev).expect("should parse");
    match header {
        FrameHeader::New(f) => {
            assert_eq!(f.frame_type, FrameType::NonKeyFrame);
            assert!(!f.frame_is_intra);
            assert!(!f.intra_only);
            // frame_size_with_refs inherits the size of slot 0, where found_ref=1.
            assert_eq!(f.width, 8);
            assert_eq!(f.height, 8);
            assert_eq!(f.refresh_frame_flags, 0x01);
            assert_eq!(f.ref_frame_idx, [0, 0, 0]);
            assert!(!f.allow_high_precision_mv);
            assert_eq!(f.interpolation_filter, EIGHTTAP_SMOOTH);
            assert_eq!(f.header_size_in_bytes, 3);
            // Since this is non-error-resilient and non-intra, frame_context_idx
            // retains the raw bitstream value.
            assert_eq!(f.frame_context_idx, 0);
        }
        FrameHeader::ShowExistingFrame { .. } => panic!("unexpected"),
    }
}

#[test]
fn effective_frame_context_idx_forces_zero_only_for_intra_or_error_resilient() {
    // Regression check: the raw f(2) value is the single stored field, and
    // effective_frame_context_idx() derives the load/save index by forcing it to 0 exactly
    // when FrameIsIntra || error_resilient_mode. The raw field itself is never mutated by
    // the reset.
    let mut prev = PersistentState::default();
    prev.ref_frame_sizes[0] = (8, 8);
    let (header, _) =
        parse_uncompressed_header(&build_minimal_inter_frame_header(), &prev).expect("parse");
    let FrameHeader::New(base) = header else {
        panic!("expected a New frame header");
    };
    for raw in 0u8..4 {
        for frame_is_intra in [false, true] {
            for error_resilient_mode in [false, true] {
                let mut h = base.clone();
                h.frame_context_idx = raw;
                h.frame_is_intra = frame_is_intra;
                h.error_resilient_mode = error_resilient_mode;
                let expected = if frame_is_intra || error_resilient_mode {
                    0
                } else {
                    raw
                };
                assert_eq!(
                    h.effective_frame_context_idx(),
                    expected,
                    "raw={raw} intra={frame_is_intra} er={error_resilient_mode}"
                );
                assert_eq!(h.frame_context_idx, raw, "raw field must be preserved");
            }
        }
    }
}

#[test]
fn parses_show_existing_frame() {
    let mut w = BitWriter::new();
    w.push_bits(2, 2); // frame_marker
    w.push_bits(0, 1); // profile_low_bit
    w.push_bits(0, 1); // profile_high_bit
    w.push_flag(true); // show_existing_frame = 1
    w.push_bits(5, 3); // frame_to_show_map_idx = 5
    let data = w.finish();

    let (header, consumed) =
        parse_uncompressed_header(&data, &PersistentState::default()).expect("should parse");
    assert_eq!(
        header,
        FrameHeader::ShowExistingFrame {
            frame_to_show_map_idx: 5
        }
    );
    assert_eq!(consumed, 1);
}

#[test]
fn parses_loop_filter_deltas_and_signed_values() {
    let mut w = BitWriter::new();
    w.push_bits(2, 2);
    w.push_bits(0, 1);
    w.push_bits(0, 1);
    w.push_flag(false); // show_existing_frame
    w.push_bits(0, 1); // KEY_FRAME
    w.push_flag(true);
    w.push_flag(true); // error_resilient_mode = 1
    w.push_bits(0x49, 8);
    w.push_bits(0x83, 8);
    w.push_bits(0x42, 8);
    w.push_bits(CS_UNKNOWN as u32, 3);
    w.push_flag(false);
    w.push_bits(15, 16); // width = 16
    w.push_bits(15, 16); // height = 16
    w.push_flag(false); // render_size same as frame
                        // error_resilient_mode == 1 -> refresh_frame_context/frame_parallel_decoding_mode are not read
    w.push_bits(0, 2); // frame_context_idx
                       // loop_filter_params
    w.push_bits(10, 6); // level
    w.push_bits(3, 3); // sharpness
    w.push_flag(true); // delta_enabled
    w.push_flag(true); // delta_update
                       // update_ref_delta x4
    w.push_flag(true);
    w.push_signed(-3, 6);
    w.push_flag(false);
    w.push_flag(true);
    w.push_signed(2, 6);
    w.push_flag(false);
    // update_mode_delta x2
    w.push_flag(true);
    w.push_signed(-1, 6);
    w.push_flag(false);
    // quantization_params
    w.push_bits(20, 8); // base_q_idx (not lossless)
    w.push_flag(false);
    w.push_flag(false);
    w.push_flag(false);
    // segmentation
    w.push_flag(false);
    // tile_info: width=16 -> MiCols=2, Sb64Cols=1 -> same as above, no loop
    w.push_bits(0, 1);
    w.push_bits(42, 16); // header_size_in_bytes

    let data = w.finish();
    let (header, _) =
        parse_uncompressed_header(&data, &PersistentState::default()).expect("should parse");
    match header {
        FrameHeader::New(f) => {
            assert!(f.error_resilient_mode);
            assert!(!f.refresh_frame_context);
            assert!(f.frame_parallel_decoding_mode);
            assert_eq!(f.loop_filter.level, 10);
            assert_eq!(f.loop_filter.sharpness, 3);
            assert_eq!(f.loop_filter.ref_deltas, [-3, 0, 2, -1]);
            assert_eq!(f.loop_filter.mode_deltas, [-1, 0]);
            assert!(!f.quantization.lossless);
            assert_eq!(f.header_size_in_bytes, 42);
        }
        FrameHeader::ShowExistingFrame { .. } => panic!("unexpected"),
    }
}

/// `segmentation_params()` round-trip: `update_map`/`temporal_update`/`update_data` all
/// set, with one feature exercised per `SEG_LVL_*` (signed negative, signed positive,
/// unsigned, and the zero-bit `SEG_LVL_SKIP` case).
#[test]
fn segmentation_params_round_trip_reads_full_payload() {
    let mut w = BitWriter::new();
    w.push_flag(true); // segmentation_enabled
    w.push_flag(true); // segmentation_update_map
    for _ in 0..7 {
        w.push_flag(true); // read_prob: coded
        w.push_bits(200, 8); // segmentation_tree_probs[i] = 200
    }
    w.push_flag(true); // segmentation_temporal_update
    for _ in 0..3 {
        w.push_flag(true);
        w.push_bits(180, 8); // segmentation_pred_prob[i] = 180
    }
    w.push_flag(true); // segmentation_update_data
    w.push_flag(false); // segmentation_abs_or_delta_update = 0 (delta)
    for seg in 0..MAX_SEGMENTS {
        for level in 0..SEG_LVL_MAX {
            if seg == 2 && level == SEG_LVL_ALT_Q {
                w.push_flag(true); // feature_enabled
                w.push_bits(50, 8);
                w.push_flag(true); // feature_sign: negative -> -50
            } else if seg == 5 && level == SEG_LVL_ALT_L {
                w.push_flag(true);
                w.push_bits(30, 6);
                w.push_flag(false); // feature_sign: positive -> 30
            } else if seg == 3 && level == SEG_LVL_REF_FRAME {
                w.push_flag(true);
                w.push_bits(2, 2); // unsigned, no sign bit
            } else if seg == 7 && level == SEG_LVL_SKIP {
                w.push_flag(true); // 0 bits to read, unsigned -> no value/sign bits
            } else {
                w.push_flag(false);
            }
        }
    }
    let data = w.finish();
    let mut r = BitReader::new(&data);
    let seg = parse_segmentation_params(&mut r, false, SegFeaturePersist::default());

    assert!(seg.enabled);
    assert!(seg.update_map);
    assert_eq!(seg.tree_probs, [200; 7]);
    assert!(seg.temporal_update);
    assert_eq!(seg.pred_prob, [180; 3]);
    assert!(!seg.abs_or_delta_update);
    assert!(seg.feature_enabled[2][SEG_LVL_ALT_Q]);
    assert_eq!(seg.feature_data[2][SEG_LVL_ALT_Q], -50);
    assert!(seg.feature_enabled[5][SEG_LVL_ALT_L]);
    assert_eq!(seg.feature_data[5][SEG_LVL_ALT_L], 30);
    assert!(seg.feature_enabled[3][SEG_LVL_REF_FRAME]);
    assert_eq!(seg.feature_data[3][SEG_LVL_REF_FRAME], 2);
    assert!(seg.feature_enabled[7][SEG_LVL_SKIP]);
    assert_eq!(seg.feature_data[7][SEG_LVL_SKIP], 0);
    // Untouched (segment, level) pairs stay disabled/zero.
    assert!(!seg.feature_enabled[0][SEG_LVL_ALT_Q]);
    assert_eq!(seg.feature_data[0][SEG_LVL_ALT_Q], 0);
}

/// When `segmentation_update_data == 0`, `FeatureEnabled`/`FeatureData`/
/// `abs_or_delta_update` persist from the previous frame (spec §7.2.10) — unless `reset`
/// (`setup_past_independence()`) clears them first.
#[test]
fn segmentation_params_persists_feature_data_when_not_updated() {
    let mut prev_enabled = [[false; SEG_LVL_MAX]; MAX_SEGMENTS];
    let mut prev_data = [[0i32; SEG_LVL_MAX]; MAX_SEGMENTS];
    prev_enabled[4][SEG_LVL_ALT_Q] = true;
    prev_data[4][SEG_LVL_ALT_Q] = 77;
    let prev = SegFeaturePersist {
        enabled: prev_enabled,
        data: prev_data,
        abs_or_delta: true,
    };

    // segmentation_enabled=1, update_map=0, update_data=0: nothing is re-signaled.
    let mut w = BitWriter::new();
    w.push_flag(true);
    w.push_flag(false);
    w.push_flag(false);
    let data = w.finish();

    let mut r = BitReader::new(&data);
    let seg = parse_segmentation_params(&mut r, false, prev);
    assert!(seg.enabled);
    assert!(!seg.update_map);
    assert!(seg.abs_or_delta_update); // persisted from prev
    assert!(seg.feature_enabled[4][SEG_LVL_ALT_Q]);
    assert_eq!(seg.feature_data[4][SEG_LVL_ALT_Q], 77);

    // reset == true (setup_past_independence): clears feature state regardless of prev.
    let mut r2 = BitReader::new(&data);
    let seg_reset = parse_segmentation_params(&mut r2, true, prev);
    assert!(!seg_reset.abs_or_delta_update);
    assert!(!seg_reset.feature_enabled[4][SEG_LVL_ALT_Q]);
    assert_eq!(seg_reset.feature_data[4][SEG_LVL_ALT_Q], 0);
}
