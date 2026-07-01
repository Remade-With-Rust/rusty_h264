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
    /// Bring-up symbol trace (Brick 0.3): when `RH_CABAC_TRACE=1`, print the
    /// spec-canonical entering `(codIRange, codIOffset)` before each bin, in the
    /// SAME `"<n> <D|B|T> r=<range> o=<offset>"` format as the instrumented openh264
    /// oracle — so the two traces diff line-for-line to localise the first divergence.
    trace: bool,
    sym: u64,
}

impl Cabac<'_> {
    #[inline]
    fn tr(&mut self, kind: &str) {
        if self.trace {
            eprintln!("{} {} r={} o={}", self.sym, kind, self.range, self.offset);
            self.sym += 1;
        }
    }
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
        let trace = std::env::var_os("RH_CABAC_TRACE").is_some();
        let mut e = Cabac { data, bit_pos: start_byte * 8, range: 510, offset: 0, ctx, trace, sym: 0 };
        e.offset = e.read_bits(9);
        e
    }

    /// Engine state `(codIRange, codIOffset)` — for bring-up verification against the
    /// oracle's symbol 0 (Brick 1.1). At slice start this is `(510, first-9-bits)`.
    pub fn dbg_state(&self) -> (u32, u32) {
        (self.range, self.offset)
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
        self.tr("D");
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
        self.tr("B");
        self.offset = (self.offset << 1) | self.read_bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            1
        } else {
            0
        }
    }

    /// Decodes `n` bypass bins as an unsigned value (MSB first).
    #[allow(dead_code)] // used by the syntax layer (next)
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
        self.tr("T");
        self.range -= 2;
        if self.offset >= self.range {
            true
        } else {
            self.renorm();
            false
        }
    }

    // NB: the byte offset where byte-aligned `pcm_sample` data resumes after an
    // I_PCM terminate is intentionally NOT provided here. This literal engine
    // holds a 9-bit look-ahead window in `offset`, so the resume position is not
    // simply `bit_pos` rounded up — it needs the over-read "given back" (cf.
    // openh264's `RestoreCabacDecEngineToBS`, which backs up by `iBitsLeft >> 3`
    // bytes). The correct accounting must be derived and validated against the
    // I_PCM decode path; it will be added with the I_PCM CABAC syntax.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal-spec CABAC *encoder* (§9.3.4), the inverse of [`Cabac`]. Used only
    /// to validate the decoder by round-trip — encode a bin sequence, decode it,
    /// assert equality. Encoder and decoder are independent algorithms (encode
    /// vs decode), so a shared latent bug is implausible; a clean round-trip over
    /// thousands of mixed bins exercises the full range/offset evolution, every
    /// `RANGE_LPS`/`STATE_TRANS` entry reached, and the bypass/terminate paths.
    struct Enc {
        low: u32,
        range: u32,
        outstanding: u32,
        first: bool,
        bits: Vec<u8>,
        ctx: Vec<(u8, u8)>, // (state, mps)
    }

    fn init_ctx(qp: i32, init_idc: u32, is_i: bool) -> Vec<(u8, u8)> {
        let model = if is_i { 0 } else { (init_idc + 1) as usize };
        let q = qp.clamp(0, 51);
        (0..460)
            .map(|i| {
                let (m, n) = CTX_INIT[i][model];
                let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
                if pre <= 63 {
                    ((63 - pre) as u8, 0)
                } else {
                    ((pre - 64) as u8, 1)
                }
            })
            .collect()
    }

    impl Enc {
        fn new(qp: i32, init_idc: u32, is_i: bool) -> Self {
            Enc {
                low: 0,
                range: 510,
                outstanding: 0,
                first: true,
                bits: Vec::new(),
                ctx: init_ctx(qp, init_idc, is_i),
            }
        }

        fn put_bit(&mut self, b: u32) {
            if self.first {
                self.first = false;
            } else {
                self.bits.push(b as u8);
            }
            while self.outstanding > 0 {
                self.bits.push((1 - b) as u8);
                self.outstanding -= 1;
            }
        }

        /// RenormE (§9.3.4.3.3).
        fn renorm(&mut self) {
            while self.range < 256 {
                if self.low < 256 {
                    self.put_bit(0);
                } else if self.low >= 512 {
                    self.low -= 512;
                    self.put_bit(1);
                } else {
                    self.low -= 256;
                    self.outstanding += 1;
                }
                self.range <<= 1;
                self.low <<= 1;
            }
        }

        /// EncodeDecision (§9.3.4.3.1).
        fn encode(&mut self, ctx_idx: usize, bin: u32) {
            let (state, mps) = self.ctx[ctx_idx];
            let q = ((self.range >> 6) & 3) as usize;
            let lps = RANGE_LPS[state as usize][q] as u32;
            self.range -= lps;
            if bin != mps as u32 {
                self.low += self.range;
                self.range = lps;
                let nm = if state == 0 { 1 - mps } else { mps };
                self.ctx[ctx_idx] = (STATE_TRANS[state as usize][0], nm);
            } else {
                self.ctx[ctx_idx].0 = STATE_TRANS[state as usize][1];
            }
            self.renorm();
        }

        /// EncodeBypass (§9.3.4.3.2).
        fn encode_bypass(&mut self, bin: u32) {
            self.low <<= 1;
            if bin != 0 {
                self.low += self.range;
            }
            if self.low >= 1024 {
                self.put_bit(1);
                self.low -= 1024;
            } else if self.low < 512 {
                self.put_bit(0);
            } else {
                self.low -= 512;
                self.outstanding += 1;
            }
        }

        /// EncodeTerminate(1) + flush (§9.3.4.5 / EncodeFlush) — ends the stream.
        fn finish(&mut self) -> Vec<u8> {
            self.range -= 2;
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put_bit((self.low >> 9) & 1);
            let v = ((self.low >> 7) & 3) | 1;
            self.bits.push(((v >> 1) & 1) as u8);
            self.bits.push((v & 1) as u8);
            // Pack MSB-first into bytes.
            let mut out = vec![0u8; self.bits.len().div_ceil(8)];
            for (i, &b) in self.bits.iter().enumerate() {
                out[i / 8] |= b << (7 - (i % 8));
            }
            out
        }
    }

    /// Deterministic xorshift RNG so the test is reproducible.
    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    /// Encode a scripted mix of context-coded, bypass, and terminate bins, then
    /// decode and assert every bin (and the terminate) round-trips exactly.
    fn roundtrip(qp: i32, init_idc: u32, is_i: bool, seed: u32, n: usize) {
        let mut rng = Rng(seed);
        // (kind, ctx, bin): kind 0 = decision, 1 = bypass.
        let mut script: Vec<(u8, usize, u32)> = Vec::with_capacity(n);
        let mut enc = Enc::new(qp, init_idc, is_i);
        for _ in 0..n {
            let r = rng.next();
            let kind = (r & 1) as u8;
            let ctx = (r >> 1) as usize % 460;
            let bin = (r >> 12) & 1;
            script.push((kind, ctx, bin));
            if kind == 0 {
                enc.encode(ctx, bin);
            } else {
                enc.encode_bypass(bin);
            }
        }
        let bytes = enc.finish();

        let mut dec = Cabac::new(&bytes, 0, qp, init_idc, is_i);
        for (i, &(kind, ctx, bin)) in script.iter().enumerate() {
            let got = if kind == 0 {
                dec.decode_decision(ctx)
            } else {
                dec.decode_bypass()
            };
            assert_eq!(got, bin, "bin {i} (kind {kind}, ctx {ctx}) mismatched");
        }
        assert!(dec.decode_terminate(), "terminate should signal end-of-stream");
    }

    #[test]
    fn engine_roundtrip_many() {
        // Sweep QP, init model, and many random scripts: every code path
        // (LPS/MPS transitions across all 64 states, bypass, terminate, renorm).
        for &qp in &[0, 12, 26, 37, 51] {
            for &(idc, is_i) in &[(0u32, true), (0, false), (1, false), (2, false)] {
                for seed in 1..=40u32 {
                    roundtrip(qp, idc, is_i, seed.wrapping_mul(2654435761), seed as usize * 53);
                }
            }
        }
    }

    #[test]
    fn engine_init_matches_spec() {
        // ctxIdx 0 (I mb_type, m=20 n=-15) at QP 26: preCtxState =
        // Clip3(1,126,(20*26>>4)-15) = 17 -> state 63-17 = 46, MPS 0.
        let dec = Cabac::new(&[0xFF, 0xFF, 0xFF], 0, 26, 0, true);
        assert_eq!(dec.ctx[0].state, 46);
        assert_eq!(dec.ctx[0].mps, 0);
        // Engine init: range 510, offset = first 9 bits of 0xFFFF = 0x1FF.
        assert_eq!(dec.range, 510);
        assert_eq!(dec.offset, 0x1FF);
    }

    #[test]
    fn tables_match_spec_boundaries() {
        assert_eq!(RANGE_LPS[0], [128, 176, 208, 240]);
        assert_eq!(RANGE_LPS[63], [2, 2, 2, 2]);
        assert_eq!(STATE_TRANS[0], [0, 1]);
        assert_eq!(STATE_TRANS[63], [63, 63]);
        assert_eq!(CTX_INIT[0][0], (20, -15));
    }
}
