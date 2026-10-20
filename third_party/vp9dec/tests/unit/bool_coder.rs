use super::*;
use crate::unit_test_support::BoolEncoder;

/// A simple linear congruential generator (LCG) pseudo-random number generator, for tests only.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u32(&mut self) -> u32 {
        // LCG using the Numerical Recipes constants.
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.state >> 32) as u32
    }

    /// Returns a probability value in 0..=255 (the boundary values 0 and 255 can also occur).
    fn next_prob(&mut self) -> u8 {
        (self.next_u32() % 256) as u8
    }

    fn next_bool(&mut self) -> bool {
        self.next_u32() % 2 == 1
    }
}

#[test]
fn empty_buffer_is_rejected() {
    let data: [u8; 0] = [];
    assert_eq!(
        BoolDecoder::new(&data).unwrap_err(),
        BoolCoderError::EmptyBuffer
    );
}

#[test]
fn invalid_marker_is_rejected() {
    // If the first byte is 128 or more, then with split=128 at BoolRange=255,
    // value(=0xFF) >= split, so marker would read as 1, which is invalid.
    let data = [0xFFu8, 0x00];
    assert_eq!(
        BoolDecoder::new(&data).unwrap_err(),
        BoolCoderError::InvalidMarker
    );
}

#[test]
fn roundtrip_fixed_sequence() {
    let bits = [true, false, false, true, true, true, false, false, true];
    let probs = [128u8, 1, 255, 64, 200, 10, 250, 5, 128];

    let mut enc = BoolEncoder::new();
    for (&b, &p) in bits.iter().zip(probs.iter()) {
        enc.write_bool(b, p);
    }
    let buf = enc.finish();

    let mut dec = BoolDecoder::new(&buf).expect("valid bitstream");
    for (&b, &p) in bits.iter().zip(probs.iter()) {
        assert_eq!(dec.read_bool(p), b);
    }
}

#[test]
fn roundtrip_literal() {
    let mut enc = BoolEncoder::new();
    enc.write_literal(0b1011_0110, 8);
    enc.write_literal(0, 4);
    enc.write_literal(0xF, 4);
    let buf = enc.finish();

    let mut dec = BoolDecoder::new(&buf).expect("valid bitstream");
    assert_eq!(dec.read_literal(8), 0b1011_0110);
    assert_eq!(dec.read_literal(4), 0);
    assert_eq!(dec.read_literal(4), 0xF);
}

/// Round-trip test using random bit sequences x probability sequences. Verified across multiple seeds and lengths.
#[test]
fn roundtrip_random_sequences() {
    for seed in [1u64, 2, 42, 1234567, 0xDEAD_BEEF, 999_999_999] {
        for &len in &[0usize, 1, 2, 7, 16, 100, 500, 2000] {
            let mut lcg = Lcg::new(seed ^ len as u64);
            let bits: Vec<bool> = (0..len).map(|_| lcg.next_bool()).collect();
            // Probability 0 is treated as 1 in the split formula (there's a "+1" floor),
            // so the full 0..=255 range can be used as-is.
            let probs: Vec<u8> = (0..len).map(|_| lcg.next_prob()).collect();

            let mut enc = BoolEncoder::new();
            for (&b, &p) in bits.iter().zip(probs.iter()) {
                enc.write_bool(b, p);
            }
            let buf = enc.finish();

            let mut dec = BoolDecoder::new(&buf)
                .unwrap_or_else(|e| panic!("seed={seed} len={len}: init failed: {e:?}"));
            for (i, (&b, &p)) in bits.iter().zip(probs.iter()).enumerate() {
                let got = dec.read_bool(p);
                assert_eq!(got, b, "seed={seed} len={len} index={i} prob={p}: mismatch");
            }
        }
    }
}

#[test]
fn roundtrip_extreme_probabilities() {
    // Verify that a sequence mixing boundary probability values (0, 1, 254, 255) still round-trips correctly.
    let bits = [
        true, true, false, false, true, false, true, false, true, true,
    ];
    let probs = [0u8, 1, 1, 0, 255, 254, 255, 0, 254, 1];

    let mut enc = BoolEncoder::new();
    for (&b, &p) in bits.iter().zip(probs.iter()) {
        enc.write_bool(b, p);
    }
    let buf = enc.finish();

    let mut dec = BoolDecoder::new(&buf).expect("valid bitstream");
    for (&b, &p) in bits.iter().zip(probs.iter()) {
        assert_eq!(dec.read_bool(p), b);
    }
}

#[test]
fn read_tree_decodes_all_leaves() {
    // A 4-value tree equivalent to PARTITION_TYPES: [ -0, 2, -1, 4, -2, -3 ]
    let tree: [i32; 6] = [0, 2, -1, 4, -2, -3];
    let probs = [100u8, 150u8, 200u8];

    // value 0 -> bit sequence [0]
    // value 1 -> bit sequence [1, 0]
    // value 2 -> bit sequence [1, 1, 0]
    // value 3 -> bit sequence [1, 1, 1]
    let mut enc = BoolEncoder::new();
    enc.write_bool(false, probs[0]); // 0
    enc.write_bool(true, probs[0]);
    enc.write_bool(false, probs[1]); // 1
    enc.write_bool(true, probs[0]);
    enc.write_bool(true, probs[1]);
    enc.write_bool(false, probs[2]); // 2
    enc.write_bool(true, probs[0]);
    enc.write_bool(true, probs[1]);
    enc.write_bool(true, probs[2]); // 3
    let buf = enc.finish();

    let mut dec = BoolDecoder::new(&buf).expect("valid bitstream");
    for expected in [0i32, 1, 2, 3] {
        let got = dec.read_tree(&tree, |node| probs[node]);
        assert_eq!(got, expected);
    }
}

#[test]
fn exit_bool_does_not_panic_and_advances_to_end() {
    let mut enc = BoolEncoder::new();
    enc.write_literal(5, 4);
    let buf = enc.finish();

    let mut dec = BoolDecoder::new(&buf).expect("valid bitstream");
    let _ = dec.read_literal(4);
    dec.exit_bool();
    assert_eq!(dec.bit_pos, buf.len() * 8);
}
