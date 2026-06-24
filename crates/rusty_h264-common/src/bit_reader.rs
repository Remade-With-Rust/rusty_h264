//! MSB-first bit reader with H.264 Exp-Golomb decoding.
//!
//! The inverse of [`crate::BitWriter`]. Operates over an RBSP byte slice
//! (emulation-prevention bytes already removed — see [`crate::nal`]).

/// Error returned when a read runs past the end of the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfData;

impl core::fmt::Display for OutOfData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("bit reader ran out of data")
    }
}

impl std::error::Error for OutOfData {}

/// A big-endian, MSB-first bit reader.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// Wraps an RBSP byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Total number of bits in the buffer.
    pub fn bit_len(&self) -> usize {
        self.data.len() * 8
    }

    /// Bits remaining.
    pub fn bits_left(&self) -> usize {
        self.bit_len().saturating_sub(self.pos)
    }

    /// `true` if the read position sits on a byte boundary.
    pub fn is_byte_aligned(&self) -> bool {
        self.pos % 8 == 0
    }

    /// Reads a single bit.
    pub fn read_bit(&mut self) -> Result<bool, OutOfData> {
        if self.pos >= self.bit_len() {
            return Err(OutOfData);
        }
        let byte = self.data[self.pos / 8];
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Ok(bit == 1)
    }

    /// Reads `n` bits (`n` <= 32) as an unsigned value, MSB first. `u(n)`.
    pub fn read_bits(&mut self, n: u32) -> Result<u32, OutOfData> {
        debug_assert!(n <= 32);
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | (self.read_bit()? as u32);
        }
        Ok(v)
    }

    /// Unsigned Exp-Golomb decode, `ue(v)`.
    pub fn read_ue(&mut self) -> Result<u32, OutOfData> {
        let mut leading_zeros = 0u32;
        while !self.read_bit()? {
            leading_zeros += 1;
            if leading_zeros > 32 {
                return Err(OutOfData); // malformed / not representable in u32
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let info = self.read_bits(leading_zeros)?;
        Ok((1u32 << leading_zeros) - 1 + info)
    }

    /// Signed Exp-Golomb decode, `se(v)`.
    pub fn read_se(&mut self) -> Result<i32, OutOfData> {
        let code_num = self.read_ue()?;
        // Inverse of the se->code_num mapping.
        let magnitude = code_num.div_ceil(2) as i32;
        Ok(if code_num % 2 == 1 { magnitude } else { -magnitude })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BitWriter;

    #[test]
    fn roundtrip_ue() {
        for v in [0u32, 1, 2, 3, 4, 7, 8, 255, 256, 65535, u32::MAX - 1] {
            let mut w = BitWriter::new();
            w.write_ue(v);
            w.align_zero();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_ue().unwrap(), v, "ue roundtrip {v}");
        }
    }

    #[test]
    fn roundtrip_se() {
        for v in [0i32, 1, -1, 2, -2, 100, -100, 32767, -32768] {
            let mut w = BitWriter::new();
            w.write_se(v);
            w.align_zero();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_se().unwrap(), v, "se roundtrip {v}");
        }
    }

    #[test]
    fn roundtrip_mixed_stream() {
        let mut w = BitWriter::new();
        w.write_bits(0b1011, 4);
        w.write_ue(42);
        w.write_se(-17);
        w.write_bits(1, 1);
        w.align_zero();
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(4).unwrap(), 0b1011);
        assert_eq!(r.read_ue().unwrap(), 42);
        assert_eq!(r.read_se().unwrap(), -17);
        assert_eq!(r.read_bits(1).unwrap(), 1);
    }

    #[test]
    fn reports_out_of_data() {
        let bytes = [0x80u8];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(8).unwrap(), 0x80);
        assert_eq!(r.read_bit(), Err(OutOfData));
    }
}
