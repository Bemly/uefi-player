use super::*;

#[test]
fn clip3_clamps_both_bounds_and_keeps_in_range_values() {
    assert_eq!(clip3(0, 255, -1), 0);
    assert_eq!(clip3(0, 255, 42), 42);
    assert_eq!(clip3(0, 255, 256), 255);
}

#[test]
fn round2_matches_the_spec_formula() {
    assert_eq!(round2(7, 0), 7);
    assert_eq!(round2(0, 1), 0);
    assert_eq!(round2(1, 1), 1);
    assert_eq!(round2(3, 3), 0);
    assert_eq!(round2(4, 3), 1);
}
