use super::*;

#[test]
fn reads_msb_first() {
    // 0b1011_0010
    let data = [0b1011_0010u8];
    let mut r = BitReader::new(&data);
    assert_eq!(r.f(1), 1);
    assert_eq!(r.f(1), 0);
    assert_eq!(r.f(1), 1);
    assert_eq!(r.f(1), 1);
    assert_eq!(r.f(4), 0b0010);
}

#[test]
fn reads_multi_bit_values_spanning_bytes() {
    // Read the 16-bit value 0x1234 as a bit stream equivalent to big-endian.
    let data = 0x1234u16.to_be_bytes();
    let mut r = BitReader::new(&data);
    assert_eq!(r.f(16), 0x1234);
}

#[test]
fn reads_signed_value() {
    // value=5 (0b0101), sign=1 -> -5
    let data = [0b0101_1000u8];
    let mut r = BitReader::new(&data);
    assert_eq!(r.s(4), -5);
}

#[test]
fn reads_signed_value_positive() {
    // value=5 (0b0101), sign=0 -> 5
    let data = [0b0101_0000u8];
    let mut r = BitReader::new(&data);
    assert_eq!(r.s(4), 5);
}

#[test]
fn out_of_range_reads_return_zero() {
    let data: [u8; 0] = [];
    let mut r = BitReader::new(&data);
    assert_eq!(r.f(8), 0);
}

#[test]
fn byte_position_ceil_rounds_up() {
    let data = [0u8; 4];
    let mut r = BitReader::new(&data);
    let _ = r.f(3);
    assert_eq!(r.byte_position_ceil(), 1);
    let _ = r.f(5);
    assert_eq!(r.byte_position_ceil(), 1);
    let _ = r.f(1);
    assert_eq!(r.byte_position_ceil(), 2);
}
