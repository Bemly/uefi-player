//! Unit tests for the crate root (split out per the out-of-line test convention).

use super::*;

#[test]
fn frame_context_reset_keyframe_always_resets_all() {
    // frame_type == KEY_FRAME overrides reset_frame_context entirely.
    for reset_frame_context in 0..=3 {
        assert_eq!(
            frame_context_reset(FrameType::KeyFrame, false, reset_frame_context, 2),
            FrameContextReset::All
        );
    }
}

#[test]
fn frame_context_reset_error_resilient_always_resets_all() {
    // error_resilient_mode overrides reset_frame_context entirely.
    for reset_frame_context in 0..=3 {
        assert_eq!(
            frame_context_reset(FrameType::NonKeyFrame, true, reset_frame_context, 2),
            FrameContextReset::All
        );
    }
}

#[test]
fn frame_context_reset_intra_only_reset_frame_context_3_resets_all() {
    assert_eq!(
        frame_context_reset(FrameType::NonKeyFrame, false, 3, 2),
        FrameContextReset::All
    );
}

#[test]
fn frame_context_reset_intra_only_reset_frame_context_2_resets_only_that_slot() {
    assert_eq!(
        frame_context_reset(FrameType::NonKeyFrame, false, 2, 0),
        FrameContextReset::Slot(0)
    );
    assert_eq!(
        frame_context_reset(FrameType::NonKeyFrame, false, 2, 3),
        FrameContextReset::Slot(3)
    );
}

#[test]
fn frame_context_reset_intra_only_reset_frame_context_0_or_1_resets_nothing() {
    assert_eq!(
        frame_context_reset(FrameType::NonKeyFrame, false, 0, 1),
        FrameContextReset::None
    );
    assert_eq!(
        frame_context_reset(FrameType::NonKeyFrame, false, 1, 1),
        FrameContextReset::None
    );
}

/// `PrevSegmentIds` reset lifecycle (`clear_prev_segment_ids_if_needed`):
/// zeroed by `setup_past_independence()` (spec §7.2) and by the first-frame /
/// size-change condition of `compute_image_size()` (spec §7.2.6); retained for a
/// same-size non-intra non-error-resilient frame.
#[test]
fn prev_segment_ids_reset_lifecycle() {
    // 16x16 -> MiCols = MiRows = 2 (4 entries).
    let dims = (16u32, 16u32);
    let image_size = header::compute_image_size(dims.0, dims.1);
    let seeded = || {
        let mut d = Decoder::new();
        d.prev_frame_dims = Some(dims);
        d.prev_segment_ids = Arc::new(vec![5u8; 4]);
        d
    };

    // Same size, no setup_past_independence: the map is retained.
    let mut d = seeded();
    d.clear_prev_segment_ids_if_needed(dims, &image_size, false);
    assert_eq!(*d.prev_segment_ids, vec![5u8; 4]);

    // setup_past_independence (FrameIsIntra || error_resilient_mode): zeroed even
    // though the size is unchanged.
    let mut d = seeded();
    d.clear_prev_segment_ids_if_needed(dims, &image_size, true);
    assert_eq!(*d.prev_segment_ids, vec![0u8; 4]);

    // Size change (compute_image_size step 1): zeroed (and resized) even without
    // setup_past_independence.
    let mut d = seeded();
    let new_dims = (24u32, 16u32); // MiCols = 3, MiRows = 2 -> 6 entries.
    let new_image_size = header::compute_image_size(new_dims.0, new_dims.1);
    d.clear_prev_segment_ids_if_needed(new_dims, &new_image_size, false);
    assert_eq!(*d.prev_segment_ids, vec![0u8; 6]);

    // First invocation (prev_frame_dims == None): zeroed.
    let mut d = Decoder::new();
    d.prev_segment_ids = Arc::new(vec![5u8; 4]);
    d.clear_prev_segment_ids_if_needed(dims, &image_size, false);
    assert_eq!(*d.prev_segment_ids, vec![0u8; 4]);
}

/// A `FrameContext` distinguishable from `FrameContext::default()`, standing in for a
/// context that has been backward-adapted (spec §8.4) away from its default values.
fn adapted_context() -> FrameContext {
    FrameContext {
        mv_hp_prob: [1, 2],
        ..FrameContext::default()
    }
}

/// End-to-end check of `frame_context_reset`'s output against `FrameContextStore`:
/// seeds all 4 slots with a non-default context, applies each reset outcome, and
/// checks which slots came back to defaults vs. which retained the adapted value.
#[test]
fn frame_context_store_reset_application() {
    let seeded = || {
        let mut store = FrameContextStore::new();
        for i in 0..4 {
            store.save(i, adapted_context());
        }
        store
    };

    // None: every slot keeps the adapted context.
    let store = seeded();
    for i in 0..4 {
        assert_eq!(store.load(i), adapted_context());
    }

    // Slot(2): only slot 2 resets to defaults; the rest keep the adapted context.
    let mut store = seeded();
    if let FrameContextReset::Slot(idx) = frame_context_reset(FrameType::NonKeyFrame, false, 2, 2) {
        store.save(idx, FrameContext::default());
    } else {
        unreachable!();
    }
    for i in 0..4 {
        if i == 2 {
            assert_eq!(store.load(i), FrameContext::default());
        } else {
            assert_eq!(store.load(i), adapted_context());
        }
    }

    // All: every slot resets to defaults (e.g. keyframe).
    let mut store = seeded();
    assert_eq!(
        frame_context_reset(FrameType::KeyFrame, false, 0, 0),
        FrameContextReset::All
    );
    store.reset_all();
    for i in 0..4 {
        assert_eq!(store.load(i), FrameContext::default());
    }
}

/// The per-constituent observation surface ([`DecodedFrame::info`]) reflects the
/// uncompressed header of the frame just decoded.
/// Decodes the first (key) frame of an existing conformance vector rather than
/// building a synthetic header, so this also exercises the real parse path.
#[test]
fn decoded_frame_info_reflects_a_decoded_keyframe() {
    let ivf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join("vp90-2-12-droppable_1.ivf");
    let ivf_bytes = match std::fs::read(&ivf_path) {
        Ok(b) => b,
        Err(_) => {
            eprintln!(
                "[skip] test vector not found, skipping: {}",
                ivf_path.display()
            );
            return;
        }
    };

    let mut reader = ivf::IvfReader::new(&ivf_bytes).expect("failed to parse IVF header");
    let first_frame = reader
        .next()
        .expect("IVF file contains no frames")
        .expect("failed to read first IVF frame");

    let mut decoder = Decoder::new();
    let decoded = decoder
        .decode_frame(first_frame.data)
        .expect("decode_frame failed on first frame");
    assert_eq!(
        decoded.len(),
        1,
        "the first chunk of an IVF stream is a single frame, not a superframe"
    );
    let info = decoded[0]
        .info
        .expect("a newly decoded (non-show_existing) frame always carries info");
    assert!(
        decoded[0].frame.is_some(),
        "key frames have show_frame == 1, so the frame is displayed"
    );
    // The first frame of any IVF stream is a key frame (spec §7.2 conformance requirement).
    assert!(info.frame_is_intra, "key frames have FrameIsIntra == true");
    assert!(
        !info.intra_only,
        "intra_only is only read for non-key frames"
    );
    assert_eq!(
        info.reset_frame_context, 0,
        "reset_frame_context is only read for non-key, non-error-resilient frames"
    );
}

/// Regression test for the Wave 4a `prev_mi_grid`/`prev_segment_ids` sharing change: if a
/// frame's tile decode errors partway through, `decode_one_frame` returns early (via `?`)
/// *before* `prev_frame_dims`/`prev_show_frame`/`prev_mi_grid` are updated, so they're left
/// describing the last *successful* frame. A subsequent frame must still decode cleanly
/// off of that carried-over state instead of panicking (e.g. at the `prev_mi_grid must be
/// Some` expect in `TileDecoder::get_block_mv`, which a naive `Option::take()`-based
/// implementation could hit if the take isn't undone on the error path).
#[test]
fn decode_recovers_after_a_mid_frame_tile_error() {
    use crate::bool_coder::BoolCoderError;

    let ivf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join("vp90-2-12-droppable_1.ivf");
    let ivf_bytes = match std::fs::read(&ivf_path) {
        Ok(b) => b,
        Err(_) => {
            eprintln!(
                "[skip] test vector not found, skipping: {}",
                ivf_path.display()
            );
            return;
        }
    };

    let reader = ivf::IvfReader::new(&ivf_bytes).expect("failed to parse IVF header");
    let frames: Vec<Vec<u8>> = reader
        .take(3)
        .map(|f| f.expect("failed to read IVF frame").data.to_vec())
        .collect();
    assert_eq!(
        frames.len(),
        3,
        "test vector must have at least 3 frames for this test"
    );

    let mut decoder = Decoder::new();
    decoder
        .decode_frame(&frames[0])
        .expect("first (key) frame must decode cleanly");

    // Truncate the second frame to 29 bytes: long enough for the uncompressed +
    // compressed headers to parse, but too short for decode_tiles, which fails with
    // Tile(BoolCoder(EmptyBuffer)) (verified empirically against this vector's frame 1).
    let truncated = &frames[1][..29];
    let err = decoder
        .decode_frame(truncated)
        .expect_err("truncated tile data must be rejected, not silently accepted");
    assert_eq!(
        err,
        DecodeError::Tile(TileError::BoolCoder(BoolCoderError::EmptyBuffer))
    );

    // The regression check itself: a further valid frame must decode without panicking,
    // even though the previous frame errored out mid-decode.
    decoder
        .decode_frame(&frames[2])
        .expect("decode must recover after a mid-frame error on the previous frame");
}
