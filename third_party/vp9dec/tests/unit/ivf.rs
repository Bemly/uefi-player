use super::*;

/// Hand-builds the byte sequence of an IVF file header from the given fields, for tests
/// that need a `frame_count` or per-frame timestamps independent of the actual frames
/// appended (unlike `write_ivf`, which always derives both from `frames`).
fn build_file_header(
    fourcc: &[u8; 4],
    width: u16,
    height: u16,
    timebase_den: u32,
    timebase_num: u32,
    frame_count: u32,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(IVF_FILE_HEADER_SIZE);
    buf.extend_from_slice(b"DKIF");
    buf.extend_from_slice(&0u16.to_le_bytes()); // version
    buf.extend_from_slice(&32u16.to_le_bytes()); // header_length
    buf.extend_from_slice(fourcc);
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.extend_from_slice(&timebase_den.to_le_bytes());
    buf.extend_from_slice(&timebase_num.to_le_bytes());
    buf.extend_from_slice(&frame_count.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // unused
    assert_eq!(buf.len(), IVF_FILE_HEADER_SIZE);
    buf
}

/// Hand-builds the 12-byte header + data for a single frame, for tests.
fn append_frame(buf: &mut Vec<u8>, timestamp: u64, data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&timestamp.to_le_bytes());
    buf.extend_from_slice(data);
}

#[test]
fn parses_file_header_fields() {
    let buf = write_ivf(
        b"VP90",
        352,
        288,
        30,
        1,
        &[vec![0xAA, 0xBB, 0xCC], vec![0x11, 0x22]],
    );

    let reader = IvfReader::new(&buf).expect("valid header");
    let header = reader.header();
    assert_eq!(header.version, 0);
    assert_eq!(header.header_length, 32);
    assert_eq!(&header.fourcc, b"VP90");
    assert_eq!(header.width, 352);
    assert_eq!(header.height, 288);
    assert_eq!(header.timebase_denominator, 30);
    assert_eq!(header.timebase_numerator, 1);
    assert_eq!(header.frame_count, 2);
}

#[test]
fn iterates_frames_in_order() {
    let mut buf = build_file_header(b"VP90", 16, 16, 30, 1, 3);
    append_frame(&mut buf, 0, &[1, 2, 3]);
    append_frame(&mut buf, 33, &[4, 5]);
    append_frame(&mut buf, 66, &[]);

    let reader = IvfReader::new(&buf).expect("valid header");
    let frames: Vec<IvfFrame> = reader.map(|f| f.expect("frame ok")).collect();

    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].timestamp, 0);
    assert_eq!(frames[0].data, &[1, 2, 3]);
    assert_eq!(frames[1].timestamp, 33);
    assert_eq!(frames[1].data, &[4, 5]);
    assert_eq!(frames[2].timestamp, 66);
    assert_eq!(frames[2].data, &[] as &[u8]);
}

#[test]
fn rejects_bad_signature() {
    let mut buf = write_ivf(b"VP90", 16, 16, 30, 1, &[]);
    buf[0] = b'X'; // Corrupt the signature
    assert_eq!(IvfReader::new(&buf).unwrap_err(), IvfError::BadSignature);
}

#[test]
fn rejects_too_short_buffer() {
    let buf = vec![0u8; 10];
    assert_eq!(
        IvfReader::new(&buf).unwrap_err(),
        IvfError::TooShortForFileHeader
    );
}

#[test]
fn reports_truncated_frame_data() {
    let mut buf = build_file_header(b"VP90", 16, 16, 30, 1, 1);
    // Claims a frame size of 10, but only 2 bytes of data actually follow; invalid.
    buf.extend_from_slice(&10u32.to_le_bytes());
    buf.extend_from_slice(&0u64.to_le_bytes());
    buf.extend_from_slice(&[0xAA, 0xBB]);

    let mut reader = IvfReader::new(&buf).expect("valid header");
    assert_eq!(reader.next(), Some(Err(IvfError::TruncatedFrameData)));
}

#[test]
fn handles_empty_stream() {
    let buf = write_ivf(b"VP90", 16, 16, 30, 1, &[]);
    let mut reader = IvfReader::new(&buf).expect("valid header");
    assert_eq!(reader.next(), None);
}
