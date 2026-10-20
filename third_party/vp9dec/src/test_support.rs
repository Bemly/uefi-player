//! Encoder-side mirrors of this crate's decoders ([`BoolEncoder`] for
//! [`crate::bool_coder::BoolDecoder`], [`BitWriter`] for the raw bit-level uncompressed-header
//! reader), used to hand-build synthetic bitstreams for round-trip tests.
//!
//! Used by unit and integration tests under `tests/`; integration tests access it via the
//! `test-support` feature, which this crate's own dev-dependency on itself enables. Never
//! compiled into a normal release build.

// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{vec, vec::Vec};

/// A bool encoder for tests only.
///
/// This is an arithmetic encoder performing the inverse operation of
/// [`crate::bool_coder::BoolDecoder`], implemented solely for round-trip tests.
///
/// Implementation approach: because this coder has the structure "the
/// range (BoolRange) is capped at 255 and renormalized by doubling one
/// bit at a time whenever it drops below 128", the encoder side has the
/// classic arithmetic-coding problem where a carry into not-yet-finalized
/// low bits can retroactively affect bits already emitted.
///
/// This implementation does not stream; instead it receives the entire
/// sequence of bools to encode and emits the output all at once, holding
/// the internal state `low` as an "arbitrary-precision binary number" so
/// that carry propagation is handled as exact bignum arithmetic (no
/// approximation or special carry-detection logic needed).
pub struct BoolEncoder {
    /// Binary representation of `low`. `bits[0]` is the most significant
    /// bit (the first bit finalized), `bits.last()` is the least
    /// significant bit (the most recently shifted-in bit from
    /// renormalization). The first 8 bits are initialized to 0 as a
    /// placeholder for the first byte, where BoolValue is stored.
    low_bits: Vec<u8>,
    /// The encoder-side counterpart of BoolRange. It transitions with
    /// exactly the same update formula as the decoder (BoolRange's
    /// transition depends only on the bool value and the probability,
    /// not on the output bit stream).
    range: u32,
}

impl Default for BoolEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl BoolEncoder {
    pub fn new() -> Self {
        let mut enc = Self {
            low_bits: vec![0u8; 8], // Placeholder for the first byte, used for BoolValue
            range: 255,
        };
        // Encode the marker bit (always 0) that is read as part of init_bool.
        enc.write_bool(false, 128);
        enc
    }

    fn add_split(&mut self, split: u32) {
        // Add split to low_bits (least significant bit at the end), propagating any carry upward.
        let mut carry = split;
        let mut idx = self.low_bits.len();
        while carry != 0 {
            if idx == 0 {
                self.low_bits.insert(0, 0);
                idx = 1;
            }
            idx -= 1;
            let sum = self.low_bits[idx] as u32 + (carry & 1);
            self.low_bits[idx] = (sum & 1) as u8;
            carry = (carry >> 1) + (sum >> 1);
        }
    }

    pub fn write_bool(&mut self, bit: bool, p: u8) {
        let split = 1 + (((self.range - 1) * p as u32) >> 8);
        if bit {
            self.add_split(split);
            self.range -= split;
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.low_bits.push(0); // Placeholder for the new least significant bit (may later become 1 via a carry)
            self.range <<= 1;
        }
    }

    pub fn write_literal(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            let bit = (value >> i) & 1 == 1;
            self.write_bool(bit, 128);
        }
    }

    /// Finishes encoding and returns the byte sequence (zero-pads to the
    /// byte boundary, corresponding to `exit_bool`).
    pub fn finish(mut self) -> Vec<u8> {
        while !self.low_bits.len().is_multiple_of(8) {
            self.low_bits.push(0);
        }
        self.low_bits
            .chunks(8)
            .map(|chunk| chunk.iter().fold(0u8, |acc, &b| (acc << 1) | b))
            .collect()
    }
}

/// An MSB-first bit writer for tests. Used to hand-build bitstreams (the inverse of the raw bit
/// reads in [`crate::header`]'s `uncompressed_header` parser).
pub struct BitWriter {
    bytes: Vec<u8>,
    cur: u8,
    cur_bits: u32,
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            cur: 0,
            cur_bits: 0,
        }
    }

    pub fn push_bits(&mut self, value: u32, n: u32) {
        // A value wider than the field would otherwise be silently truncated into a *different*
        // valid encoding (e.g. a 6-bit field turns 64 into 0), failing far from the cause.
        assert!(
            n == 32 || value >> n == 0,
            "{value} does not fit in {n} bits"
        );
        for i in (0..n).rev() {
            let bit = ((value >> i) & 1) as u8;
            self.cur = (self.cur << 1) | bit;
            self.cur_bits += 1;
            if self.cur_bits == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.cur_bits = 0;
            }
        }
    }

    pub fn push_flag(&mut self, value: bool) {
        self.push_bits(value as u32, 1);
    }

    /// s(n): n bits absolute value + 1 bit sign.
    pub fn push_signed(&mut self, value: i32, n: u32) {
        self.push_bits(value.unsigned_abs(), n);
        self.push_flag(value < 0);
    }

    pub fn finish(mut self) -> Vec<u8> {
        while self.cur_bits != 0 {
            self.cur <<= 1;
            self.cur_bits += 1;
            if self.cur_bits == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.cur_bits = 0;
            }
        }
        self.bytes
    }
}
