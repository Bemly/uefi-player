use super::*;

#[test]
fn clamp_mv_row_matches_spec_formula() {
    // An 8x8 block with MiRow=0, bh=1, MiRows=8 (64px frame).
    // mbToTopEdge = 0, mbToBottomEdge = (8-1-0)*8*8 = 448
    assert_eq!(clamp_mv_row(1000, 128, 0, 1, 8), 448 + 128);
    assert_eq!(clamp_mv_row(-1000, 128, 0, 1, 8), 0 - 128);
    assert_eq!(clamp_mv_row(10, 128, 0, 1, 8), 10);
}

#[test]
fn add_mv_ref_list_deduplicates_and_caps_at_two() {
    let mut list = [ZERO_MV; 2];
    let mut count = 0usize;
    add_mv_ref_list(&mut list, &mut count, [1, 2]);
    assert_eq!(count, 1);
    add_mv_ref_list(&mut list, &mut count, [1, 2]); // duplicate -> not added
    assert_eq!(count, 1);
    add_mv_ref_list(&mut list, &mut count, [3, 4]);
    assert_eq!(count, 2);
    assert_eq!(list, [[1, 2], [3, 4]]);
    add_mv_ref_list(&mut list, &mut count, [5, 6]); // ignored due to cap of 2
    assert_eq!(count, 2);
    assert_eq!(list, [[1, 2], [3, 4]]);
}

#[test]
fn scale_mv_flips_sign_on_differing_bias() {
    let sign_bias = [false, false, true, false];
    assert_eq!(scale_mv([4, -2], 1, 2, &sign_bias), [-4, 2]);
    assert_eq!(scale_mv([4, -2], 1, 3, &sign_bias), [4, -2]);
}

#[test]
fn use_mv_hp_threshold() {
    assert!(use_mv_hp([63, 0])); // 63>>3 = 7 < 8
    assert!(!use_mv_hp([64, 0])); // 64>>3 = 8, not < 8
}
