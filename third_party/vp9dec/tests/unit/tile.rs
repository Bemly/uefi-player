//! Unit tests for the `tile` module (split out per the out-of-line test convention).

use super::*;
use crate::prob_tables::{BLOCK_4X4, BLOCK_64X64 as B64, KF_UV_MODE_PROBS, KF_Y_MODE_PROBS};
use crate::unit_test_support::{
    minimal_compressed_header, minimal_new_frame_header as minimal_header, BoolEncoder,
};

#[test]
fn get_tile_offset_matches_spec_formula() {
    // MiCols=1, tileSzLog2=0 -> only 1 tile, so offset is 0 and mis.
    assert_eq!(get_tile_offset(0, 1, 0), 0);
    assert_eq!(get_tile_offset(1, 1, 0), 1);
}

#[test]
fn single_skip_block_decodes_without_residual_error() {
    // A frame consisting of a single superblock of 8x8 (1 MI).
    // partition (BLOCK_64X64, hasRows=false, hasCols=false) -> SPLIT with no bit read
    // partition (BLOCK_32X32, hasRows=false, hasCols=false) -> SPLIT
    // partition (BLOCK_16X16, hasRows=false, hasCols=false) -> SPLIT
    // partition (BLOCK_8X8, hasRows=false, hasCols=false) -> SPLIT, but
    //   num8x8=1 so half_block8x8=0, and hasRows/hasCols depend on the check.
    // Since MiCols=MiRows=1, half_block8x8 is always 0, so
    // hasRows = (r+0) < 1 = true (r=0), hasCols = true. So from the top level,
    // has_rows=has_cols=true and the whole tree must be read.
    let header = minimal_header(8, 8);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    let mut enc = BoolEncoder::new();
    // BLOCK_64X64: has_rows=true, has_cols=true (MiRows=MiCols=1, half=32>>... actually
    // num8x8=8, half=4, (0+4)<1 is false so hasRows=hasCols=false -> SPLIT with no bit read.
    // BLOCK_32X32: num8x8=4, half=2, (0+2)<1 false -> hasRows=hasCols=false -> SPLIT (no bit)
    // BLOCK_16X16: num8x8=2, half=1, (0+1)<1 false -> hasRows=hasCols=false -> SPLIT (no bit)
    // BLOCK_8X8: num8x8=1, half=0, (0+0)<1 true -> hasRows=hasCols=true -> read partition_tree
    let ctx = 3 * 4; // bsl(BLOCK_8X8)=0; the actual ctx depends on the above/left context. Left to the closure below.
    let _ = ctx;
    // Choose partition = PARTITION_NONE: bit=0 at the first branch of partition_tree.
    enc.write_bool(false, KF_PARTITION_PROBS[0][0]);
    // intra_frame_mode_info: skip=1
    enc.write_bool(true, CompressedHeaderProbs::default().skip_prob[0]);
    // read_tx_size: tx_mode=ONLY_4X4, so the tree is not read even with allowSelect.
    // default_intra_mode (MiSize=BLOCK_8X8 >= BLOCK_8X8): choose DC_PRED (bit=0 at the first tree branch)
    enc.write_bool(
        false,
        KF_Y_MODE_PROBS[DC_PRED as usize][DC_PRED as usize][0],
    );
    // default_uv_mode: DC_PRED
    enc.write_bool(false, KF_UV_MODE_PROBS[DC_PRED as usize][0]);
    let buf = enc.finish();

    decoder
        .decode_tiles(&buf)
        .expect("skip block should decode without error");

    let info = decoder.mi_grid().get(0, 0);
    assert!(info.skip);
    assert_eq!(info.mi_size, BLOCK_8X8);
    assert_eq!(info.y_mode, DC_PRED);
    assert_eq!(info.uv_mode, DC_PRED);
    assert_eq!(info.tx_size, TX_4X4);
    let _ = B64; // Explicitly use the BLOCK_64X64 alias import (avoid unused warning).
    let _ = BLOCK_4X4;
}

#[test]
fn non_skip_block_with_immediate_eob_decodes_successfully() {
    // A non-skip 8x8 (1 MI) block. Since lossless, tx_size is always TX_4X4, and the Y
    // plane has 2x2=4 4x4 transform blocks, while U/V each have 1.
    // Setting the first more_coefs to false in each block (no coefficients) exercises the
    // immediate-EOB path: it must still update the EOB adaptation count while leaving the
    // already-predicted pixels unchanged.
    let header = minimal_header(8, 8);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    let mut enc = BoolEncoder::new();
    enc.write_bool(false, KF_PARTITION_PROBS[0][0]); // PARTITION_NONE
    enc.write_bool(false, CompressedHeaderProbs::default().skip_prob[0]); // skip = 0
    enc.write_bool(
        false,
        KF_Y_MODE_PROBS[DC_PRED as usize][DC_PRED as usize][0],
    ); // DC_PRED
    enc.write_bool(false, KF_UV_MODE_PROBS[DC_PRED as usize][0]); // DC_PRED

    // DEFAULT_COEF_PROBS[TX_4X4][plane>0][is_inter=0][band=0][ctx=0][0]
    let y_more_coefs_prob = CompressedHeaderProbs::default().coef_probs[0][0][0][0][0][0];
    let uv_more_coefs_prob = CompressedHeaderProbs::default().coef_probs[0][1][0][0][0][0];
    for _ in 0..4 {
        enc.write_bool(false, y_more_coefs_prob); // Y's 4 4x4 blocks: more_coefs=0
    }
    enc.write_bool(false, uv_more_coefs_prob); // U
    enc.write_bool(false, uv_more_coefs_prob); // V
    let buf = enc.finish();

    decoder
        .decode_tiles(&buf)
        .expect("all-zero residual block should decode without error");

    let info = decoder.mi_grid().get(0, 0);
    assert!(!info.skip);
    // Since all coefficients are zero, the reconstructed pixel value should remain the
    // predicted value (DC_PRED, and since no reference is available, the bit_depth
    // midpoint 128).
    assert_eq!(decoder.planes()[0].get(0, 0), 128);
    assert_eq!(decoder.counts().more_coefs[0][0][0][0][0][0], 4);
    assert_eq!(decoder.counts().more_coefs[0][1][0][0][0][0], 2);
    assert_eq!(decoder.counts().token[0][0][0][0][0], [0; 3]);
    assert_eq!(decoder.counts().token[0][1][0][0][0], [0; 3]);
}

#[test]
fn column_worker_strips_are_sized_and_merged_by_absolute_position() {
    // 200x80 (4:2:0): MiCols=25, Sb64Cols=4 -> padded planes 256 (luma) / 128 (chroma) wide,
    // padded grid 32 columns. Two tile columns as get_tile_offset would split them: [0,16)
    // and [16,25) (the last tile ends at MiCols).
    let header = minimal_header(200, 80);
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    let mut w0 = decoder.spawn_column_worker(0, 16);
    let mut w1 = decoder.spawn_column_worker(16, 25);

    // Strip sizing: a middle column gets exactly its tile's span; the LAST column's strip
    // extends into the superblock-rounded padding (its edge blocks write past MiCols*8).
    assert_eq!((w0.planes[0].x0, w0.planes[0].width), (0, 128));
    assert_eq!((w1.planes[0].x0, w1.planes[0].width), (128, 256 - 128));
    assert_eq!((w0.planes[1].x0, w0.planes[1].width), (0, 64));
    assert_eq!((w1.planes[1].x0, w1.planes[1].width), (64, 128 - 64));
    assert_eq!(w0.mi_grid.cols(), 16);
    assert_eq!(w1.mi_grid.cols(), 32 - 16);

    // Workers address planes/mi_grid by ABSOLUTE coordinates; the merge must land each
    // strip back at its absolute position and sum the counts.
    w0.planes[0].set(5, 3, 111);
    w1.planes[0].set(130, 7, 222);
    w0.mi_grid.get_mut(2, 3).y_mode = 5;
    w1.mi_grid.get_mut(2, 20).y_mode = 6;
    w0.counts.partition[0][0] = 1;
    w1.counts.partition[0][0] = 2;
    decoder.merge_column_worker(&w0);
    decoder.merge_column_worker(&w1);
    assert_eq!(decoder.planes[0].get(5, 3), 111);
    assert_eq!(decoder.planes[0].get(130, 7), 222);
    assert_eq!(decoder.mi_grid.get(2, 3).y_mode, 5);
    assert_eq!(decoder.mi_grid.get(2, 20).y_mode, 6);
    assert_eq!(decoder.counts.partition[0][0], 3);
}

#[test]
fn invalid_tile_size_is_rejected() {
    let header = minimal_header(64, 64);
    // 64x64 -> MiCols=8, Sb64Cols=1, so there is still only 1 tile, but force
    // tile_cols_log2 to 1 to require a tile size field.
    let mut header = header;
    header.tile_cols_log2 = 1;
    let compressed = minimal_compressed_header();
    let mut decoder = TileDecoder::new(&header, header.color_config.unwrap(), &compressed);

    // Less than 4 bytes, so not even the tile size field can be read.
    let buf = [0u8; 2];
    let err = decoder.decode_tiles(&buf).unwrap_err();
    assert_eq!(err, TileError::InvalidTileSize);
}
