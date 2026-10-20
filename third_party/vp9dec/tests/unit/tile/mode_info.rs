//! Unit tests for the `tile::mode_info` module (split out per the out-of-line test convention).

use super::*;
use crate::compressed_header::CompressedHeaderProbs;
use crate::counts::Counts;
use crate::prob_tables::{BLOCK_32X32, BLOCK_8X8};
use crate::unit_test_support::{
    minimal_compressed_header, minimal_inter_frame_header as minimal_inter_header,
    minimal_new_frame_header as minimal_header, BoolEncoder,
};
use std::sync::Arc;

/// `intra_segment_id()` reads `segment_tree` when `update_map == 1`.
#[test]
fn intra_segment_id_reads_tree_when_update_map() {
    let mut header = minimal_header(8, 8);
    header.segmentation.enabled = true;
    header.segmentation.update_map = true;
    header.segmentation.tree_probs = [128; 7];
    let compressed = minimal_compressed_header();
    let decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    // segment_tree[14] = {2,4,6,8,10,12, 0,-1,-2,-3,-4,-5,-6,-7} (spec §9.3.1). Leaf
    // value 5 is reached via node 0 (bit=1 -> index 4), node 2 (bit=0 -> index 10),
    // node 5 (bit=1 -> index 11, leaf -(-5)=5): bit path [1,0,1].
    let mut enc = BoolEncoder::new();
    enc.write_bool(true, 128);
    enc.write_bool(false, 128);
    enc.write_bool(true, 128);
    let buf = enc.finish();
    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");

    assert_eq!(decoder.intra_segment_id(&mut r), 5);
}

/// `intra_segment_id()` returns 0 without reading any bits when segmentation is
/// disabled or `update_map == 0`.
#[test]
fn intra_segment_id_is_zero_without_update_map() {
    let header = minimal_header(8, 8); // segmentation disabled.
    let compressed = minimal_compressed_header();
    let decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    // An empty tile: if intra_segment_id tried to read a bit, this would panic/error.
    let mut r = BoolDecoder::new(&[0x00]).expect("valid bitstream");
    assert_eq!(decoder.intra_segment_id(&mut r), 0);
}

/// `get_segment_id()` (spec §6.4.14): the predicted id is the minimum over the
/// on-screen `PrevSegmentIds` region covered by the block, clipped at the frame edge.
#[test]
fn get_segment_id_takes_min_over_block_region() {
    // 32x32 -> MiCols=MiRows=4. prev_segment_ids laid out row-major 4x4.
    let header = minimal_header(32, 32);
    let compressed = minimal_compressed_header();
    #[rustfmt::skip]
        let prev_segment_ids = vec![
            3, 3, 3, 3,
            3, 1, 2, 3,
            3, 3, 3, 3,
            3, 3, 3, 3,
        ];
    let decoder = TileDecoder::new_with_prev(
        &header,
        header.color_config.unwrap(),
        &compressed,
        false,
        None,
        [None, None, None],
        Arc::new(prev_segment_ids),
    );
    // BLOCK_32X32 at (0,0) covers the whole 4x4 region -> min is 1.
    assert_eq!(decoder.get_segment_id(0, 0, BLOCK_32X32), 1);
    // BLOCK_8X8 at (1,1) covers only PrevSegmentIds[1][1] = 1.
    assert_eq!(decoder.get_segment_id(1, 1, BLOCK_8X8), 1);
    // BLOCK_8X8 at (0,0) covers only PrevSegmentIds[0][0] = 3.
    assert_eq!(decoder.get_segment_id(0, 0, BLOCK_8X8), 3);
}

/// `inter_segment_id()` (spec §6.4.12): when `seg_id_predicted == 1`, `segment_id` is
/// taken from `get_segment_id()` (the previous frame's map) without reading `segment_id`.
#[test]
fn inter_segment_id_temporal_prediction_uses_prev_map() {
    let mut header = minimal_inter_header(8, 8);
    header.segmentation.enabled = true;
    header.segmentation.update_map = true;
    header.segmentation.temporal_update = true;
    header.segmentation.pred_prob = [64; 3];
    let compressed = minimal_compressed_header();
    let prev_segment_ids = vec![6u8]; // MiCols=MiRows=1 at 8x8.
    let mut decoder = TileDecoder::new_with_prev(
        &header,
        header.color_config.unwrap(),
        &compressed,
        false,
        None,
        [None, None, None],
        Arc::new(prev_segment_ids),
    );

    // seg_id_predicted = 1: only 1 bit is read (no segment_id tree read).
    let mut enc = BoolEncoder::new();
    enc.write_bool(true, 64);
    let buf = enc.finish();
    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");

    assert_eq!(decoder.inter_segment_id(&mut r, 0, 0, BLOCK_8X8), 6);
}

/// `read_skip()` (spec §6.4.8): when `seg_feature_active( SEG_LVL_SKIP )`, `skip` is
/// forced to 1 without reading a bit or incrementing `counts.skip`.
#[test]
fn read_skip_seg_lvl_skip_forces_without_reading_bit() {
    let mut header = minimal_header(8, 8);
    header.segmentation.enabled = true;
    header.segmentation.feature_enabled[2][SEG_LVL_SKIP] = true;
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    // Empty tile data: if read_skip tried to read a bit, BoolDecoder::new would still
    // succeed on an all-zero buffer, but the returned value would come from the (absent)
    // stream rather than being forced; counts.skip is the reliable signal here.
    let mut r = BoolDecoder::new(&[0x00]).expect("valid bitstream");
    let skip = decoder.read_skip(&mut r, 0, 0, false, false, 2);
    assert!(skip);
    assert_eq!(decoder.counts.skip, Counts::new().skip);
}

/// `read_is_inter()` (spec §6.4.13): when `seg_feature_active( SEG_LVL_REF_FRAME )`,
/// `is_inter` is derived from `FeatureData` without reading a bit or counting.
#[test]
fn read_is_inter_seg_lvl_ref_frame_forces_without_reading_bit() {
    let mut header = minimal_inter_header(8, 8);
    header.segmentation.enabled = true;
    header.segmentation.feature_enabled[1][SEG_LVL_REF_FRAME] = true;
    header.segmentation.feature_data[1][SEG_LVL_REF_FRAME] = LAST_FRAME as i32;
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let n = NeighborRefInfo {
        avail_u: false,
        avail_l: false,
        left_ref_frame: [INTRA_FRAME, REF_NONE],
        above_ref_frame: [INTRA_FRAME, REF_NONE],
        left_intra: true,
        above_intra: true,
        left_single: true,
        above_single: true,
    };

    let mut r = BoolDecoder::new(&[0x00]).expect("valid bitstream");
    assert!(decoder.read_is_inter(&mut r, &n, 1));
    assert_eq!(decoder.counts.is_inter, Counts::new().is_inter);
}

/// `read_ref_frames()` (spec §6.4.17): when `seg_feature_active( SEG_LVL_REF_FRAME )`,
/// `ref_frame` is `[FeatureData, NONE]` (no compound) without reading a bit or counting.
#[test]
fn read_ref_frames_seg_lvl_ref_frame_returns_feature_value() {
    let mut header = minimal_inter_header(8, 8);
    header.segmentation.enabled = true;
    header.segmentation.feature_enabled[4][SEG_LVL_REF_FRAME] = true;
    header.segmentation.feature_data[4][SEG_LVL_REF_FRAME] = GOLDEN_FRAME as i32;
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let n = NeighborRefInfo {
        avail_u: false,
        avail_l: false,
        left_ref_frame: [INTRA_FRAME, REF_NONE],
        above_ref_frame: [INTRA_FRAME, REF_NONE],
        left_intra: true,
        above_intra: true,
        left_single: true,
        above_single: true,
    };

    let mut r = BoolDecoder::new(&[0x00]).expect("valid bitstream");
    assert_eq!(
        decoder.read_ref_frames(&mut r, &n, 4),
        [GOLDEN_FRAME, REF_NONE]
    );
    assert_eq!(decoder.counts.comp_mode, Counts::new().comp_mode);
    assert_eq!(decoder.counts.single_ref, Counts::new().single_ref);
}

// =========================================================================
// Unit tests for MV decoding (spec §6.4.19-6.4.20).
// Encode a known MV with `BoolEncoder` (test_support), decode it with
// `TileDecoder::read_mv`/`read_mv_component` (private methods), and check round-trip equality.
// =========================================================================

#[test]
fn read_mv_component_class0_roundtrip() {
    let header = minimal_inter_header(64, 64);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let probs = CompressedHeaderProbs::default();

    // mv_sign=0(positive), mv_class=MV_CLASS_0, class0_bit=1, class0_fr=2, (since
    // use_hp=false, class0_hp is not read and is taken as 1).
    // mag = ((1<<3)|(2<<1)|1) + 1 = 13 + 1 = 14
    let mut enc = BoolEncoder::new();
    enc.write_bool(false, probs.mv_sign_prob[0]);
    enc.write_bool(false, probs.mv_class_probs[0][0]); // MV_CLASS_0 (tree leaf)
    enc.write_bool(true, probs.mv_class0_bit_prob[0]); // class0_bit = 1
                                                       // class0_fr = 2: bit sequence [1,1,0] of MV_FR_TREE
    enc.write_bool(true, probs.mv_class0_fr_probs[0][1][0]);
    enc.write_bool(true, probs.mv_class0_fr_probs[0][1][1]);
    enc.write_bool(false, probs.mv_class0_fr_probs[0][1][2]);
    let buf = enc.finish();

    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");
    let mag = decoder.read_mv_component(&mut r, 0, false);
    assert_eq!(mag, 14);
}

#[test]
fn read_mv_component_negative_class0_roundtrip() {
    let header = minimal_inter_header(64, 64);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let probs = CompressedHeaderProbs::default();

    let mut enc = BoolEncoder::new();
    enc.write_bool(true, probs.mv_sign_prob[1]); // sign = negative
    enc.write_bool(false, probs.mv_class_probs[1][0]); // MV_CLASS_0
    enc.write_bool(false, probs.mv_class0_bit_prob[1]); // class0_bit = 0
                                                        // class0_fr = 0: bit sequence [0]
    enc.write_bool(false, probs.mv_class0_fr_probs[1][0][0]);
    let buf = enc.finish();

    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");
    // mag = ((0<<3)|(0<<1)|1) + 1 = 2, and since the sign is negative, -2.
    let mag = decoder.read_mv_component(&mut r, 1, false);
    assert_eq!(mag, -2);
}

#[test]
fn read_mv_component_higher_class_roundtrip() {
    let header = minimal_inter_header(64, 64);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let probs = CompressedHeaderProbs::default();

    // mv_class = MV_CLASS_1 (value 1): in the tree [0,2,-1,4,...], bit0=1,bit1=0 -> leaf -1 (value 1).
    let mut enc = BoolEncoder::new();
    enc.write_bool(false, probs.mv_sign_prob[0]); // positive
    enc.write_bool(true, probs.mv_class_probs[0][0]);
    enc.write_bool(false, probs.mv_class_probs[0][1]);
    // mv_class=1 -> read 1 bit of d (mv_bit). Let d=1.
    enc.write_bool(true, probs.mv_bits_prob[0][0]);
    // mv_fr = 1: bit sequence [1,0]
    enc.write_bool(true, probs.mv_fr_probs[0][0]);
    enc.write_bool(false, probs.mv_fr_probs[0][1]);
    let buf = enc.finish();

    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");
    // mag = CLASS0_SIZE << (1+2) = 2<<3 = 16; d=1 (bit0=1), fr=1, hp(forced 1)
    // mag += ((1<<3)|(1<<1)|1) + 1 = (8|2|1)+1 = 11+1 = 12 -> total 16+12 = 28
    let mag = decoder.read_mv_component(&mut r, 0, false);
    assert_eq!(mag, 28);
}

#[test]
fn read_mv_full_roundtrip_with_best_mv_offset() {
    // read_mv(ref) = BestMv + diffMv. mv_joint = MV_JOINT_HNZVNZ (both components nonzero).
    let header = minimal_inter_header(64, 64);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);
    let probs = CompressedHeaderProbs::default();
    let best_mv: Mv = [10, -20];

    let mut enc = BoolEncoder::new();
    // mv_joint tree: bit sequence [1,1,1] for MV_JOINT_HNZVNZ(=3)
    enc.write_bool(true, probs.mv_joint_probs[0]);
    enc.write_bool(true, probs.mv_joint_probs[1]);
    enc.write_bool(true, probs.mv_joint_probs[2]);
    // comp 0 (row): mag=14 (same class0 pattern as the earlier test, positive sign)
    enc.write_bool(false, probs.mv_sign_prob[0]);
    enc.write_bool(false, probs.mv_class_probs[0][0]);
    enc.write_bool(true, probs.mv_class0_bit_prob[0]);
    enc.write_bool(true, probs.mv_class0_fr_probs[0][1][0]);
    enc.write_bool(true, probs.mv_class0_fr_probs[0][1][1]);
    enc.write_bool(false, probs.mv_class0_fr_probs[0][1][2]);
    // comp 1 (col): mag=2, negative sign -> -2
    enc.write_bool(true, probs.mv_sign_prob[1]);
    enc.write_bool(false, probs.mv_class_probs[1][0]);
    enc.write_bool(false, probs.mv_class0_bit_prob[1]);
    enc.write_bool(false, probs.mv_class0_fr_probs[1][0][0]);
    let buf = enc.finish();

    let mut r = BoolDecoder::new(&buf).expect("valid bitstream");
    // allow_high_precision_mv=false, so use_hp is always false (regardless of use_mv_hp's result).
    let mv = decoder.read_mv(&mut r, best_mv);
    assert_eq!(mv, [10 + 14, -20 + (-2)]);
}
