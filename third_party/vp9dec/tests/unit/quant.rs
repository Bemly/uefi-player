use super::*;

/// Confirms that each table's element/row count matches the spec's `[3][256]`.
#[test]
fn table_shapes() {
    assert_eq!(DC_QLOOKUP.len(), 3);
    assert_eq!(AC_QLOOKUP.len(), 3);
    for row in DC_QLOOKUP.iter() {
        assert_eq!(row.len(), 256);
    }
    for row in AC_QLOOKUP.iter() {
        assert_eq!(row.len(), 256);
    }
}

/// Spot-checks the boundary values listed in the spec (first/last entry of each row).
#[test]
fn spot_check_known_values() {
    // 8bit
    assert_eq!(dc_q(8, 0), 4);
    assert_eq!(dc_q(8, 255), 1336);
    assert_eq!(ac_q(8, 0), 4);
    assert_eq!(ac_q(8, 255), 1828);
    // 10bit
    assert_eq!(dc_q(10, 0), 4);
    assert_eq!(dc_q(10, 255), 5347);
    assert_eq!(ac_q(10, 255), 7312);
    // 12bit
    assert_eq!(dc_q(12, 0), 4);
    assert_eq!(dc_q(12, 255), 21387);
    assert_eq!(ac_q(12, 255), 29247);
    // Also confirm a mid-table value from the spec (8bit dc, index 100) wasn't mistranscribed.
    assert_eq!(dc_q(8, 100), 93);
}

/// Out-of-range quantization indices are clamped via `Clip3`.
#[test]
fn out_of_range_index_is_clipped() {
    assert_eq!(dc_q(8, -10), dc_q(8, 0));
    assert_eq!(dc_q(8, 1000), dc_q(8, 255));
    assert_eq!(ac_q(8, -1), ac_q(8, 0));
    assert_eq!(ac_q(8, 300), ac_q(8, 255));
}

/// Builds a `SegmentationParams` with `SEG_LVL_ALT_Q` active for segment 0, with the
/// given `data`/`abs_or_delta_update`.
fn seg_lvl_alt_q(data: i32, abs_or_delta_update: bool) -> SegmentationParams {
    let mut seg = SegmentationParams {
        enabled: true,
        abs_or_delta_update,
        ..SegmentationParams::default()
    };
    seg.feature_enabled[0][SEG_LVL_ALT_Q] = true;
    seg.feature_data[0][SEG_LVL_ALT_Q] = data;
    seg
}

/// When segmentation is disabled, `get_qindex` returns `base_q_idx` unchanged.
#[test]
fn qindex_without_segmentation() {
    assert_eq!(get_qindex(120, &SegmentationParams::default(), 0), 120);
}

/// The case `segmentation_abs_or_delta_update == 1` (absolute value specified).
#[test]
fn qindex_absolute_override() {
    assert_eq!(get_qindex(120, &seg_lvl_alt_q(50, true), 0), 50);
    // Out-of-range values are clamped via Clip3.
    assert_eq!(get_qindex(120, &seg_lvl_alt_q(400, true), 0), 255);
    assert_eq!(get_qindex(120, &seg_lvl_alt_q(-10, true), 0), 0);
}

/// The case `segmentation_abs_or_delta_update == 0` (delta specified).
#[test]
fn qindex_delta_override() {
    assert_eq!(get_qindex(100, &seg_lvl_alt_q(20, false), 0), 120);
    assert_eq!(get_qindex(100, &seg_lvl_alt_q(-300, false), 0), 0);
}

/// `get_dc_quant` / `get_ac_quant` switching which delta is applied based on plane.
#[test]
fn dc_ac_quant_plane_selection() {
    let qindex = 100u8;
    // plane 0 (Y) only uses delta_q_y_dc; AC never has a delta.
    assert_eq!(get_dc_quant(8, qindex, 0, 5, -5), dc_q(8, 105));
    assert_eq!(get_ac_quant(8, qindex, 0, -7), ac_q(8, 100));
    // plane 1/2 (U/V) use delta_q_uv_dc / delta_q_uv_ac.
    assert_eq!(get_dc_quant(8, qindex, 1, 5, -5), dc_q(8, 95));
    assert_eq!(get_dc_quant(8, qindex, 2, 5, -5), dc_q(8, 95));
    assert_eq!(get_ac_quant(8, qindex, 1, -7), ac_q(8, 93));
    assert_eq!(get_ac_quant(8, qindex, 2, -7), ac_q(8, 93));
}
