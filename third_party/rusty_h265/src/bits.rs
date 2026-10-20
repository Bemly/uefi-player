//! MSB-first bit reader with Exp-Golomb decoding over an RBSP (emulation
//! prevention already removed — see [`crate::nal`]). Ported from
//! `rusty_h264`'s reader: the one-load `peek_bits` fast path and the cached
//! `rbsp_stop_one_bit` position are the same design.

/// Error returned when a read runs past the end of the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfData;

impl core::fmt::Display for OutOfData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("bit reader ran out of data")
    }
}

impl core::error::Error for OutOfData {}

/// A big-endian, MSB-first bit reader.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    pos: usize,
    /// Bit position of the RBSP stop bit (the last set bit in the buffer).
    stop_pos: usize,
}

impl<'a> BitReader<'a> {
    /// Wraps an RBSP byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        let stop_pos = data.iter().enumerate().rev().find(|(_, &b)| b != 0).map_or(0, |(bi, &b)| bi * 8 + (7 - b.trailing_zeros() as usize));
        Self { data, pos: 0, stop_pos }
    }

    /// The underlying RBSP buffer.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Current bit position.
    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Current byte position (rounded down).
    pub fn byte_pos(&self) -> usize {
        self.pos / 8
    }

    /// Total number of bits in the buffer.
    pub fn bit_len(&self) -> usize {
        self.data.len() * 8
    }

    /// `more_rbsp_data()` (§7.2): true while the read position is before the
    /// `rbsp_stop_one_bit`.
    #[inline]
    pub fn more_rbsp_data(&self) -> bool {
        self.pos < self.stop_pos
    }

    /// `true` if the read position sits on a byte boundary.
    pub fn is_byte_aligned(&self) -> bool {
        self.pos % 8 == 0
    }

    /// Advances to the next byte boundary.
    pub fn align_to_byte(&mut self) -> Result<(), OutOfData> {
        while self.pos % 8 != 0 {
            self.read_bit()?;
        }
        Ok(())
    }

    /// Reads a single bit.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool, OutOfData> {
        let Some(&byte) = self.data.get(self.pos / 8) else {
            return Err(OutOfData);
        };
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Ok(bit == 1)
    }

    /// Reads a flag, `u(1)`.
    #[inline]
    pub fn read_flag(&mut self) -> Result<bool, OutOfData> {
        self.read_bit()
    }

    /// Reads `n` bits (`n` <= 32) as an unsigned value, MSB first. `u(n)`.
    pub fn read_bits(&mut self, n: u32) -> Result<u32, OutOfData> {
        if n > 32 {
            return Err(OutOfData);
        }
        if n == 0 {
            return Ok(0);
        }
        if n <= 24 {
            let v = self.peek_bits(n);
            self.skip_bits(n)?;
            return Ok(v);
        }
        let hi = self.read_bits(n - 16)?;
        let lo = self.read_bits(16)?;
        Ok((hi << 16) | lo)
    }

    /// Peeks the next `n` bits (`n` ≤ 24) without consuming, zero-filling past
    /// the end of the buffer. O(1): one 4-byte load.
    #[inline]
    pub fn peek_bits(&self, n: u32) -> u32 {
        debug_assert!(n <= 24);
        let byte = self.pos / 8;
        let off = (self.pos % 8) as u32;
        let acc = match self.data.get(byte..byte + 4) {
            Some(c) => u32::from_be_bytes([c[0], c[1], c[2], c[3]]),
            None => {
                ((*self.data.get(byte).unwrap_or(&0) as u32) << 24)
                    | ((*self.data.get(byte + 1).unwrap_or(&0) as u32) << 16)
                    | ((*self.data.get(byte + 2).unwrap_or(&0) as u32) << 8)
                    | (*self.data.get(byte + 3).unwrap_or(&0) as u32)
            }
        };
        if n == 0 {
            return 0;
        }
        (acc >> (32 - off - n)) & ((1u32 << n) - 1)
    }

    /// Consumes `n` bits after a [`peek_bits`](Self::peek_bits). Rejects if
    /// the bits run past the end of the buffer.
    #[inline]
    pub fn skip_bits(&mut self, n: u32) -> Result<(), OutOfData> {
        if self.pos + n as usize > self.bit_len() {
            return Err(OutOfData);
        }
        self.pos += n as usize;
        Ok(())
    }

    /// Unsigned Exp-Golomb decode, `ue(v)`.
    pub fn read_ue(&mut self) -> Result<u32, OutOfData> {
        let window = self.peek_bits(24);
        let lz = window.leading_zeros() - 8;
        if lz <= 11 {
            let total = 2 * lz + 1;
            self.skip_bits(total)?;
            if lz == 0 {
                return Ok(0);
            }
            let info = (window >> (24 - total)) & ((1u32 << lz) - 1);
            return Ok((1u32 << lz) - 1 + info);
        }
        let mut leading_zeros = 0u32;
        while !self.read_bit()? {
            leading_zeros += 1;
            if leading_zeros >= 32 {
                return Err(OutOfData);
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let info = self.read_bits(leading_zeros)?;
        Ok((1u32 << leading_zeros) - 1 + info)
    }

    /// `ue(v)` bounded: rejects values above `max` (a hostile count never
    /// drives an allocation).
    pub fn read_ue_max(&mut self, max: u32) -> Result<u32, OutOfData> {
        let v = self.read_ue()?;
        if v > max {
            return Err(OutOfData);
        }
        Ok(v)
    }

    /// Signed Exp-Golomb decode, `se(v)`.
    pub fn read_se(&mut self) -> Result<i32, OutOfData> {
        let code_num = self.read_ue()?;
        let magnitude = code_num.div_ceil(2) as i32;
        Ok(if code_num % 2 == 1 { magnitude } else { -magnitude })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ue_se_known_codewords() {
        // ue: 1 -> 0, 010 -> 1, 011 -> 2, 00100 -> 3, 00111 -> 6
        let bytes = [0b1010_0110u8, 0b0100_0011, 0b1000_0000];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_ue().unwrap(), 0);
        assert_eq!(r.read_ue().unwrap(), 1);
        assert_eq!(r.read_ue().unwrap(), 2);
        assert_eq!(r.read_ue().unwrap(), 3);
        assert_eq!(r.read_ue().unwrap(), 6);
        // se: code 1 -> +1, 2 -> -1, 3 -> +2
        let bytes = [0b0100_1100u8, 0b1000_0000];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_se().unwrap(), 1);
        assert_eq!(r.read_se().unwrap(), -1);
        assert_eq!(r.read_se().unwrap(), 2);
    }

    #[test]
    fn reports_out_of_data() {
        let bytes = [0x80u8];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(8).unwrap(), 0x80);
        assert_eq!(r.read_bit(), Err(OutOfData));
    }

    #[test]
    fn peek_then_skip_matches_read_bits() {
        let bytes = [0xB5u8, 0x3C, 0xF0, 0x0A, 0x77];
        for start in 0..16u32 {
            for n in 1..=24u32 {
                let mut a = BitReader::new(&bytes);
                a.skip_bits(start).unwrap();
                let peeked = a.peek_bits(n);
                a.skip_bits(n).unwrap();
                let mut b = BitReader::new(&bytes);
                b.skip_bits(start).unwrap();
                assert_eq!(peeked, b.read_bits(n).unwrap(), "start={start} n={n}");
            }
        }
    }

    #[test]
    fn read_32_bits() {
        let bytes = [0xDE, 0xAD, 0xBE, 0xEF, 0x80];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(32).unwrap(), 0xDEAD_BEEF);
        assert!(r.read_bit().unwrap());
    }

    #[test]
    fn more_rbsp_data_stops_at_stop_bit() {
        let bytes = [0b1100_0000u8]; // one data bit, then the stop bit
        let mut r = BitReader::new(&bytes);
        assert!(r.more_rbsp_data());
        r.read_bit().unwrap();
        assert!(!r.more_rbsp_data());
    }
}
