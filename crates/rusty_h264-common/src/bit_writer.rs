//! MSB-first bit writer with H.264 Exp-Golomb coding.
//!
//! H.264 packs syntax elements as a big-endian bitstream: the first bit written
//! lands in the most-significant position of the first byte. This writer
//! accumulates bits and exposes the Exp-Golomb (`ue`/`se`) and fixed-length
//! (`u`) codings the bitstream syntax is built from.

/// A growable, MSB-first bit buffer.
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    /// Completed bytes.
    bytes: Vec<u8>,
    /// Bits accumulated for the in-progress byte, left-aligned conceptually:
    /// `cur` holds `nbits` valid bits in its low `nbits` positions.
    cur: u8,
    /// Number of valid bits currently in `cur` (0..=7).
    nbits: u8,
}

impl BitWriter {
    /// Creates an empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bits written so far.
    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8 + self.nbits as usize
    }

    /// `true` if the next bit would start a fresh byte.
    pub fn is_byte_aligned(&self) -> bool {
        self.nbits == 0
    }

    /// Writes a single bit (`true` => 1).
    pub fn write_bit(&mut self, bit: bool) {
        self.cur = (self.cur << 1) | (bit as u8);
        self.nbits += 1;
        if self.nbits == 8 {
            self.bytes.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    /// Writes the low `n` bits of `value`, most-significant first. `n` <= 32.
    ///
    /// This is `u(n)` in the spec's descriptor notation.
    pub fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32, "write_bits supports up to 32 bits");
        for i in (0..n).rev() {
            self.write_bit((value >> i) & 1 == 1);
        }
    }

    /// Unsigned Exp-Golomb code `ue(v)`.
    ///
    /// Encodes `code_num = value` as `[prefix zeros][1][info]` where the total
    /// is the binary of `value + 1`: write `n` leading zeros then `value + 1`
    /// in `n + 1` bits, with `n = floor(log2(value + 1))`.
    pub fn write_ue(&mut self, value: u32) {
        // value + 1 as u64 to avoid overflow at u32::MAX.
        let x = value as u64 + 1;
        let n = 63 - x.leading_zeros(); // floor(log2(x))
        // n leading zero bits.
        for _ in 0..n {
            self.write_bit(false);
        }
        // x in (n + 1) bits.
        for i in (0..=n).rev() {
            self.write_bit((x >> i) & 1 == 1);
        }
    }

    /// Signed Exp-Golomb code `se(v)`.
    ///
    /// Maps the signed value to an unsigned code number then writes `ue`:
    /// `0 -> 0`, `1 -> 1`, `-1 -> 2`, `2 -> 3`, `-2 -> 4`, ...
    pub fn write_se(&mut self, value: i32) {
        let code_num = if value <= 0 {
            (-(value as i64) as u64) * 2
        } else {
            (value as u64) * 2 - 1
        };
        // code_num fits in u32 for all i32 inputs except this is bounded well within u64.
        self.write_ue_u64(code_num);
    }

    /// `ue` for a 64-bit code number (used by `se`).
    fn write_ue_u64(&mut self, code_num: u64) {
        let x = code_num + 1;
        let n = 63 - x.leading_zeros();
        for _ in 0..n {
            self.write_bit(false);
        }
        for i in (0..=n).rev() {
            self.write_bit((x >> i) & 1 == 1);
        }
    }

    /// Writes the `rbsp_trailing_bits()`: a stop bit `1` then zero-pad to a byte.
    pub fn rbsp_trailing_bits(&mut self) {
        self.write_bit(true);
        while self.nbits != 0 {
            self.write_bit(false);
        }
    }

    /// Pads with zero bits to the next byte boundary (no stop bit).
    pub fn align_zero(&mut self) {
        while self.nbits != 0 {
            self.write_bit(false);
        }
    }

    /// Consumes the writer, returning the byte buffer. Panics if not byte aligned;
    /// call [`rbsp_trailing_bits`](Self::rbsp_trailing_bits) or
    /// [`align_zero`](Self::align_zero) first.
    pub fn into_bytes(self) -> Vec<u8> {
        assert!(
            self.is_byte_aligned(),
            "BitWriter::into_bytes called with {} dangling bits",
            self.nbits
        );
        self.bytes
    }

    /// Borrows the completed bytes (excludes any partial trailing byte).
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_bits_is_msb_first() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_bits(0b1, 1);
        w.align_zero();
        // bits 1,0,1,1 then zero-pad -> 1011_0000
        assert_eq!(w.into_bytes(), vec![0b1011_0000]);
    }

    #[test]
    fn ue_known_values() {
        // From H.264 Table 9-2: code_num -> bit string.
        let cases: &[(u32, &str)] = &[
            (0, "1"),
            (1, "010"),
            (2, "011"),
            (3, "00100"),
            (4, "00101"),
            (5, "00110"),
            (6, "00111"),
            (7, "0001000"),
            (8, "0001001"),
        ];
        for &(v, bits) in cases {
            let mut w = BitWriter::new();
            w.write_ue(v);
            assert_eq!(bitstring(&w), bits, "ue({v})");
        }
    }

    #[test]
    fn se_known_values() {
        // H.264 Table 9-3 mapping: se -> code_num.
        let cases: &[(i32, &str)] = &[
            (0, "1"),    // code_num 0
            (1, "010"),  // 1
            (-1, "011"), // 2
            (2, "00100"),
            (-2, "00101"),
            (3, "00110"),
            (-3, "00111"),
        ];
        for &(v, bits) in cases {
            let mut w = BitWriter::new();
            w.write_se(v);
            assert_eq!(bitstring(&w), bits, "se({v})");
        }
    }

    #[test]
    fn ue_max_does_not_overflow() {
        let mut w = BitWriter::new();
        w.write_ue(u32::MAX);
        // value+1 = 2^32 -> 32 leading zeros then 33-bit value => 65 bits total.
        assert_eq!(w.bit_len(), 65);
    }

    #[test]
    fn rbsp_trailing_aligns_to_byte() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.rbsp_trailing_bits();
        // 101 + stop 1 + pad 0000 => 1011_0000
        assert_eq!(w.into_bytes(), vec![0b1011_0000]);
    }

    /// Renders the bits currently buffered (including the partial byte) as a
    /// string of '0'/'1' for assertions.
    fn bitstring(w: &BitWriter) -> String {
        let mut s = String::new();
        for &b in w.as_bytes() {
            for i in (0..8).rev() {
                s.push(if (b >> i) & 1 == 1 { '1' } else { '0' });
            }
        }
        for i in (0..w.nbits).rev() {
            s.push(if (w.cur >> i) & 1 == 1 { '1' } else { '0' });
        }
        s
    }
}
