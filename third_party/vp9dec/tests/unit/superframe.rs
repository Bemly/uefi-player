use super::*;

/// Hand-builds a superframe index for the given frame sizes, using 1-byte frame sizes.
fn build_index_1byte(frame_sizes: &[u8]) -> Vec<u8> {
    let marker = 0xc0 | (frame_sizes.len() as u8 - 1);
    let mut index = vec![marker];
    index.extend_from_slice(frame_sizes);
    index.push(marker);
    index
}

#[test]
fn passes_through_data_with_no_superframe_marker() {
    let data = [1u8, 2, 3, 4, 5];
    assert_eq!(split_superframe(&data), vec![&data[..]]);
}

#[test]
fn splits_a_two_frame_superframe() {
    let mut data = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    // frame 0 = data[0..3] (size 3), frame 1 = data[3..5] (size 2)
    data.extend(build_index_1byte(&[3, 2]));

    let frames = split_superframe(&data);
    assert_eq!(frames, vec![&[0xAA, 0xBB, 0xCC][..], &[0xDD, 0xEE][..]]);
}

#[test]
fn splits_a_four_frame_superframe_matching_the_intra_only_vector_layout() {
    let f0 = vec![1u8; 3];
    let f1 = vec![2u8; 5];
    let f2 = vec![3u8; 7];
    let f3 = vec![4u8; 2];
    let mut data = Vec::new();
    data.extend_from_slice(&f0);
    data.extend_from_slice(&f1);
    data.extend_from_slice(&f2);
    data.extend_from_slice(&f3);
    data.extend(build_index_1byte(&[3, 5, 7, 2]));

    let frames = split_superframe(&data);
    assert_eq!(frames, vec![&f0[..], &f1[..], &f2[..], &f3[..]]);
}

#[test]
fn rejects_index_whose_leading_and_trailing_marker_bytes_disagree() {
    let mut data = vec![0xAAu8, 0xBB, 0xCC];
    // Trailing marker (0xc0: frame_size_length=1, num_frames=1) puts the index start
    // right before the leading byte below, which deliberately doesn't repeat 0xc0.
    data.push(0x00);
    data.push(3);
    data.push(0xc0);
    assert_eq!(split_superframe(&data), vec![&data[..]]);
}

#[test]
fn rejects_index_whose_declared_sizes_overflow_the_payload() {
    let mut data = vec![0xAAu8, 0xBB, 0xCC];
    data.extend(build_index_1byte(&[100])); // claims a 100-byte frame; payload is only 3 bytes
    assert_eq!(split_superframe(&data), vec![&data[..]]);
}

#[test]
fn treats_too_short_a_buffer_as_a_single_frame() {
    // Last byte looks like a marker, but the buffer is far too short for the claimed index.
    let data = [0xffu8];
    assert_eq!(split_superframe(&data), vec![&data[..]]);
}
