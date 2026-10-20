use super::*;

#[test]
fn get_set_roundtrip() {
    let mut p = Plane::new(4, 3);
    p.set(1, 2, 42);
    assert_eq!(p.get(1, 2), 42);
    assert_eq!(p.get(0, 0), 0);
}

#[test]
fn strip_translates_absolute_x_to_its_origin() {
    // A strip over absolute columns [8, 12) of a conceptual 16-wide plane: accessors take
    // absolute x, storage is strip-local (stride == strip width).
    let mut s = Plane::new_strip(4, 2, 8);
    s.set(8, 0, 1);
    s.set(11, 1, 2);
    assert_eq!(s.get(8, 0), 1);
    assert_eq!(s.get(11, 1), 2);
    assert_eq!(s.as_slice()[0], 1);
    // Storage index: row 1 * stride 4 + local col 3.
    assert_eq!(s.as_slice()[4 + 3], 2);
}

#[test]
fn crop_extracts_top_left_region() {
    let mut p = Plane::new(4, 2);
    for y in 0..2 {
        for x in 0..4 {
            p.set(x, y, (y * 4 + x) as u16);
        }
    }
    let cropped = p.crop(2, 2);
    assert_eq!(cropped, vec![0, 1, 4, 5]);
}

#[test]
fn crop_u8_narrows_samples() {
    let mut p = Plane::new(4, 2);
    for y in 0..2 {
        for x in 0..4 {
            p.set(x, y, (y * 4 + x) as u16);
        }
    }
    let cropped = p.crop_u8(2, 2);
    assert_eq!(cropped, vec![0u8, 1, 4, 5]);
}
