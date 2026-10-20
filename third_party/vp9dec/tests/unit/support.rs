pub(crate) use crate::test_support::{BitWriter, BoolEncoder};

/// Builds a compact, internally consistent key-frame header for unit tests that exercise
/// post-header decode stages directly.
pub(crate) fn minimal_new_frame_header(width: u32, height: u32) -> crate::header::NewFrameHeader {
    use crate::header::{
        ColorConfig, FrameType, LoopFilterDeltas, LoopFilterParams, NewFrameHeader,
        QuantizationParams, SegmentationParams,
    };

    let loop_filter_deltas = LoopFilterDeltas::default();
    NewFrameHeader {
        profile: 0,
        frame_type: FrameType::KeyFrame,
        show_frame: true,
        error_resilient_mode: false,
        frame_is_intra: true,
        intra_only: false,
        reset_frame_context: 0,
        ref_frame_idx: [0, 0, 0],
        ref_frame_sign_bias: [false; 4],
        allow_high_precision_mv: false,
        interpolation_filter: crate::prob_tables::SWITCHABLE,
        color_config: Some(ColorConfig {
            bit_depth: 8,
            color_space: 0,
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
        }),
        width,
        height,
        render_width: width,
        render_height: height,
        refresh_frame_flags: 0xFF,
        refresh_frame_context: true,
        frame_parallel_decoding_mode: false,
        frame_context_idx: 0,
        loop_filter: LoopFilterParams {
            level: 0,
            sharpness: 0,
            delta_enabled: false,
            ref_deltas: loop_filter_deltas.ref_deltas,
            mode_deltas: loop_filter_deltas.mode_deltas,
        },
        quantization: QuantizationParams {
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_uv_dc: 0,
            delta_q_uv_ac: 0,
            lossless: true,
        },
        segmentation: SegmentationParams::default(),
        tile_cols_log2: 0,
        tile_rows_log2: 0,
        header_size_in_bytes: 0,
    }
}

/// Minimal lossless/intra-compatible compressed-header state for unit-level tile fixtures.
pub(crate) fn minimal_compressed_header() -> crate::compressed_header::CompressedHeader {
    crate::compressed_header::CompressedHeader {
        tx_mode: crate::prob_tables::ONLY_4X4,
        probs: std::sync::Arc::new(crate::compressed_header::CompressedHeaderProbs::default()),
        reference_mode: crate::prob_tables::SINGLE_REFERENCE,
        comp_fixed_ref: 0,
        comp_var_ref: [0, 0],
    }
}

/// Inter-frame variant of [`minimal_new_frame_header`].
pub(crate) fn minimal_inter_frame_header(width: u32, height: u32) -> crate::header::NewFrameHeader {
    let mut header = minimal_new_frame_header(width, height);
    header.frame_type = crate::header::FrameType::NonKeyFrame;
    header.frame_is_intra = false;
    header.ref_frame_idx = [0, 1, 2];
    header
}
