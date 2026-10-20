use super::*;
use crate::prob_tables::TX_4X4 as TX4;

fn make_plane(width: usize, height: usize, fill: u8) -> Plane {
    let mut p = Plane::new(width, height);
    for y in 0..height {
        for x in 0..width {
            p.set(x, y, fill as u16);
        }
    }
    p
}

#[test]
fn dc_pred_with_no_neighbors_uses_bit_depth_midpoint() {
    let mut plane = Plane::new(8, 8);
    predict_intra(&mut plane, 0, 0, false, false, false, TX4, DC_PRED, 7, 7, 8);
    assert_eq!(plane.get(0, 0), 128);
    assert_eq!(plane.get(3, 3), 128);
}

#[test]
fn v_pred_copies_above_row() {
    let mut plane = make_plane(8, 8, 0);
    for x in 0..4 {
        plane.set(x, 3, 50 + x as u16);
    }
    predict_intra(&mut plane, 0, 4, false, true, false, TX4, V_PRED, 7, 7, 8);
    for x in 0..4 {
        assert_eq!(plane.get(x, 4), 50 + x as u16);
        assert_eq!(plane.get(x, 5), 50 + x as u16);
    }
}

#[test]
fn h_pred_copies_left_col() {
    let mut plane = make_plane(8, 8, 0);
    for y in 0..4 {
        plane.set(3, y, 60 + y as u16);
    }
    predict_intra(&mut plane, 4, 0, true, false, false, TX4, H_PRED, 7, 7, 8);
    for y in 0..4 {
        assert_eq!(plane.get(4, y), 60 + y as u16);
        assert_eq!(plane.get(5, y), 60 + y as u16);
    }
}

#[test]
fn tm_pred_clips_to_valid_range() {
    let mut plane = make_plane(8, 8, 0);
    for x in 0..4 {
        plane.set(4 + x, 3, 255);
    }
    for y in 0..4 {
        plane.set(3, 4 + y, 255);
    }
    plane.set(3, 3, 0);
    predict_intra(&mut plane, 4, 4, true, true, false, TX4, TM_PRED, 7, 7, 8);
    // 255 + 255 - 0 is clipped to 255.
    assert_eq!(plane.get(4, 4), 255);
}
