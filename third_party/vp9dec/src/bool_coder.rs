//! VP9's bool decoder (arithmetic decoder, spec §9.2 "Parsing process for Boolean decoder").
//!
//! Nearly all of the VP9 bitstream except the uncompressed frame header (the
//! compressed header and tile data) is entropy-coded by this bool coder. This
//! implementation follows the spec's pseudocode faithfully and does not reference
//! any existing OSS implementation (clean-room implementation).
//!
//! Spec text referenced (§9.2.1 - §9.2.4):
//!
//! ```text
//! 9.2.1 Initialization process for Boolean decoder
//!   BoolValue = f(8)
//!   BoolRange = 255
//!   BoolMaxBits = 8 * sz - 8
//!   read a marker with read_bool(128), requiring it to be 0
//!
//! 9.2.2 Boolean decoding process (read_bool(p))
//!   split = 1 + (((BoolRange - 1) * p) >> 8)
//!   if BoolValue < split:
//!       BoolRange = split; bool = 0
//!   else:
//!       BoolRange -= split; BoolValue -= split; bool = 1
//!   while BoolRange < 128:
//!       newBit = (BoolMaxBits > 0) ? f(1) : 0   (if read, BoolMaxBits -= 1)
//!       BoolRange *= 2
//!       BoolValue = (BoolValue << 1) + newBit
//!
//! 9.2.3 Exit process for Boolean decoder (exit_bool())
//!   discard the remaining BoolMaxBits worth of padding (expected to be 0)
//!
//! 9.2.4 Parsing process for read_literal(n)
//!   x = 0
//!   for i in 0..n: x = 2 * x + read_bool(128)
//! ```

/// Errors that can occur when initializing the bool decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolCoderError {
    /// `init_bool(sz)` must not be called with sz < 1 (spec §9.2.1).
    EmptyBuffer,
    /// The marker bit read right after initialization must be 0 (spec §9.2.1).
    InvalidMarker,
}

/// VP9's arithmetic decoder (bool decoder).
///
/// `data` is expected to be the `sz`-byte slice from `init_bool( sz )` (the
/// compressed header or tile data itself), passed through as-is.
#[derive(Debug, Clone)]
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    /// Absolute position of the next raw bit to read. `data[0]` is already
    /// consumed as the initial value of BoolValue, so the initial value is 8.
    bit_pos: usize,
    /// BoolValue.
    value: u32,
    /// BoolRange.
    range: u32,
    /// Count of raw bits requested past the end of `data` (the spec's zero-padding: each such
    /// request returns 0 and leaves `bit_pos` pinned at the end). A conformant stream renorms
    /// at most a handful of these at the very end; a desynced (corrupt) stream keeps decoding
    /// symbols off the end and runs this up fast -- the signal `over_read_bits()` exposes so a
    /// tile/compressed-header decode can reject corruption libvpx catches via `has_error`.
    over_read_bits: usize,
}

impl<'a> BoolDecoder<'a> {
    /// Corresponds to `init_bool( sz )`. `data.len()` is the spec's `sz`.
    pub fn new(data: &'a [u8]) -> Result<Self, BoolCoderError> {
        if data.is_empty() {
            return Err(BoolCoderError::EmptyBuffer);
        }
        let mut decoder = Self {
            data,
            bit_pos: 8,
            value: data[0] as u32,
            range: 255,
            over_read_bits: 0,
        };
        // In a spec-conformant stream, the marker bit read right after initialization is always 0.
        if decoder.read_bool(128) {
            return Err(BoolCoderError::InvalidMarker);
        }
        Ok(decoder)
    }

    /// Reads 1 bit from the raw bit stream (MSB first). Reading past the end
    /// returns 0, matching the spec's "newBit = 0 when BoolMaxBits == 0" behavior.
    fn read_bit_raw(&mut self) -> u32 {
        let total_bits = self.data.len() * 8;
        if self.bit_pos >= total_bits {
            self.over_read_bits += 1;
            return 0;
        }
        let byte_index = self.bit_pos / 8;
        let bit_index_from_msb = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;
        ((self.data[byte_index] >> bit_index_from_msb) & 1) as u32
    }

    /// `read_bool( p )` (spec §9.2.2). Decodes 1 bool based on probability p (0..=255, denominator 256).
    pub fn read_bool(&mut self, p: u8) -> bool {
        let split = 1 + (((self.range - 1) * p as u32) >> 8);
        let bit = if self.value < split {
            self.range = split;
            false
        } else {
            self.range -= split;
            self.value -= split;
            true
        };
        while self.range < 128 {
            let new_bit = self.read_bit_raw();
            self.range <<= 1;
            self.value = (self.value << 1) + new_bit;
        }
        bit
    }

    /// `read_literal( n )` (spec §9.2.4). Reads n bits with probability 1/2 (p=128), high bit first.
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bool(128) as u32;
        }
        x
    }

    /// `read_literal( 1 ) == 1`, as a single-bit flag (spec §9.2.4). A convenience wrapper for
    /// the very common single-bit `L(1)` reads scattered through the compressed header.
    pub fn flag(&mut self) -> bool {
        self.read_literal(1) == 1
    }

    /// Decoding process for tree-coded syntax elements (spec §9.3.3, "Tree decoding process").
    ///
    /// `tree` is the spec's tree array itself (leaves are non-positive values
    /// `-value`, internal nodes are non-negative values pointing to the next
    /// index). `prob_of(node)` is a closure, built by the caller, that returns
    /// the probability obtained from the probability selection process (spec
    /// §9.3.2) given a node number (`n >> 1`).
    ///
    /// ```text
    /// do {
    ///     n = T[ n + read_bool( P( n >> 1 ) ) ]
    /// } while ( n > 0 )
    /// return -n
    /// ```
    pub fn read_tree<F>(&mut self, tree: &[i32], mut prob_of: F) -> i32
    where
        F: FnMut(usize) -> u8,
    {
        let mut n: i32 = 0;
        loop {
            let node = (n as usize) >> 1;
            let bit = self.read_bool(prob_of(node)) as i32;
            n = tree[(n + bit) as usize];
            if n <= 0 {
                break;
            }
        }
        -n
    }

    /// Number of raw bits this decoder has requested past the end of its buffer (see the field's
    /// doc). Conformant decodes end with only a small tail of these; a large value means the
    /// arithmetic decoder desynced and ran off the end -- i.e. the tile/header was corrupt.
    pub fn over_read_bits(&self) -> usize {
        self.over_read_bits
    }

    /// Absolute raw-bit position (pinned at `data.len()*8` once the end is reached; further
    /// requests bump [`Self::over_read_bits`] instead). Lets a tile decode measure how much of
    /// its buffer it consumed, to reject a desynced (corrupt) tile that finishes far short.
    pub fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// `exit_bool( )` (spec §9.2.3). Discards the remaining padding bits and finishes.
    ///
    /// The spec requires the padding to be 0, but this implementation does not
    /// validate the value and only advances the internal position to the end
    /// of the buffer, so it does not panic on non-conformant streams.
    pub fn exit_bool(&mut self) {
        self.bit_pos = self.data.len() * 8;
    }
}

#[cfg(test)]
#[path = "../tests/unit/bool_coder.rs"]
mod tests;
