//! CABAC arithmetic decoding engine (spec §9.3.3.2) + context initialization
//! (§9.3.1.1). The literal-spec engine (codIRange/codIOffset, RenormD), which is
//! bit-exact to openh264's optimized variant. Tables in [`crate::cabac_tables`].

use crate::cabac_tables::{CTX_INIT, RANGE_LPS, STATE_TRANS};

/// One context model: probability-state index (0..63) and the MPS value (0/1).
#[derive(Clone, Copy)]
struct Ctx {
    state: u8,
    mps: u8,
}

/// The CABAC decoder: arithmetic engine reading MSB-first from the RBSP plus the
/// 460 adaptive context models.
pub struct Cabac<'a> {
    data: &'a [u8],
    /// Next bit position (in bits) into `data`.
    bit_pos: usize,
    range: u32,
    offset: u32,
    ctx: [Ctx; 460],
}

impl<'a> Cabac<'a> {
    /// Initializes from the RBSP `data` at byte offset `start_byte` (the slice
    /// data, byte-aligned past the header), the slice's `qp` (clamped 0..51),
    /// `cabac_init_idc`, and whether the slice is I/SI (spec §9.3.1).
    pub fn new(data: &'a [u8], start_byte: usize, qp: i32, init_idc: u32, is_i: bool) -> Self {
        let model = if is_i { 0 } else { (init_idc + 1) as usize };
        let q = qp.clamp(0, 51);
        let mut ctx = [Ctx { state: 0, mps: 0 }; 460];
        for (i, c) in ctx.iter_mut().enumerate() {
            let (m, n) = CTX_INIT[i][model];
            let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
            *c = if pre <= 63 {
                Ctx { state: (63 - pre) as u8, mps: 0 }
            } else {
                Ctx { state: (pre - 64) as u8, mps: 1 }
            };
        }
        let mut e = Cabac { data, bit_pos: start_byte * 8, range: 510, offset: 0, ctx };
        e.offset = e.read_bits(9);
        e
    }

    /// Reads one bit MSB-first; zero-fills past the end of the buffer.
    #[inline]
    fn read_bit(&mut self) -> u32 {
        let byte = self.bit_pos / 8;
        if byte >= self.data.len() {
            self.bit_pos += 1;
            return 0;
        }
        let bit = (self.data[byte] >> (7 - (self.bit_pos % 8))) & 1;
        self.bit_pos += 1;
        bit as u32
    }

    #[inline]
    fn read_bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.read_bit();
        }
        v
    }

    /// Renormalization (spec §9.3.3.2.2): keep `range` ≥ 256, refilling `offset`.
    #[inline]
    fn renorm(&mut self) {
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.read_bit();
        }
    }

    /// Decodes a context-coded bin (spec §9.3.3.2.1), updating the context model.
    pub fn decode_decision(&mut self, ctx_idx: usize) -> u32 {
        let state = self.ctx[ctx_idx].state;
        let mps = self.ctx[ctx_idx].mps;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_LPS[state as usize][q] as u32;
        self.range -= lps;
        let bin;
        let (new_state, new_mps);
        if self.offset >= self.range {
            bin = 1 - mps;
            self.offset -= self.range;
            self.range = lps;
            new_mps = if state == 0 { 1 - mps } else { mps };
            new_state = STATE_TRANS[state as usize][0];
        } else {
            bin = mps;
            new_mps = mps;
            new_state = STATE_TRANS[state as usize][1];
        }
        self.ctx[ctx_idx].state = new_state;
        self.ctx[ctx_idx].mps = new_mps;
        self.renorm();
        bin as u32
    }

    /// Decodes a bypass (equiprobable) bin (spec §9.3.3.2.3).
    pub fn decode_bypass(&mut self) -> u32 {
        self.offset = (self.offset << 1) | self.read_bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            1
        } else {
            0
        }
    }

    /// Decodes `n` bypass bins as an unsigned value (MSB first).
    pub fn decode_bypass_bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.decode_bypass();
        }
        v
    }

    /// Decodes the terminate bin (spec §9.3.3.2.4); `true` ends the slice (or
    /// marks I_PCM). No renormalization on terminate.
    pub fn decode_terminate(&mut self) -> bool {
        self.range -= 2;
        if self.offset >= self.range {
            true
        } else {
            self.renorm();
            false
        }
    }

    /// Byte position just past the consumed bits, after the terminating
    /// arithmetic flush — the start of any byte-aligned `pcm` data.
    pub fn byte_pos(&self) -> usize {
        self.bit_pos.div_ceil(8)
    }
}
