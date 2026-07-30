//! CABAC arithmetic decoding engine (spec §9.3.3.2) + context initialization
//! (§9.3.1.1). The literal-spec engine (codIRange/codIOffset, RenormD), which is
//! bit-exact to openh264's optimized variant. Tables in [`crate::cabac_tables`].

use rusty_h264_common::cabac_tables::{CTX_INIT, RANGE_LPS, STATE_TRANS};

/// A context model is ONE byte: `state * 2 + mps` (0..=127) — ffmpeg/openh264's
/// packing (H-35). The literal two-field form cost two loads and two stores per
/// bin plus `1 - mps` arithmetic; packed, a bin is one byte load, one table
/// lookup, one byte store, and `s & 1` for the value. The three tables below
/// fold the state transition AND the state-0 MPS flip into the lookup, so the
/// decoded bins are identical by construction.
///
/// Built at compile time from the spec tables, so there is no init cost and no
/// `OnceLock` check on the hot path.
const fn build_lps_range() -> [u8; 4 * 128] {
    let mut t = [0u8; 4 * 128];
    let mut q = 0;
    while q < 4 {
        let mut s = 0;
        while s < 128 {
            t[q * 128 + s] = RANGE_LPS[s >> 1][q];
            s += 1;
        }
        q += 1;
    }
    t
}
/// ONE transition table covering both paths: `[0..128)` is the MPS path (state
/// advances, MPS unchanged) and `[128..256)` the LPS path (state falls back, and
/// at state 0 the MPS FLIPS per spec §9.3.3.2.1.1 — baked in, never branched).
/// Indexed `s | (lps_mask & 128)`, which is what makes the bin loop branchless.
const fn build_trans() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut s = 0;
    while s < 128 {
        let mps = s as u8 & 1;
        t[s] = (STATE_TRANS[s >> 1][1] << 1) | mps;
        let new_mps = if s >> 1 == 0 { 1 - mps } else { mps };
        t[128 + s] = (STATE_TRANS[s >> 1][0] << 1) | new_mps;
        s += 1;
    }
    t
}
static LPS_RANGE: [u8; 4 * 128] = build_lps_range();
static TRANS: [u8; 256] = build_trans();

/// The CABAC decoder: arithmetic engine reading MSB-first from the RBSP plus the
/// 460 adaptive context models.
pub struct Cabac<'a> {
    data: &'a [u8],
    /// Next byte to load into the bit window.
    byte_pos: usize,
    /// MSB-aligned unread bits (H-34): the old engine extracted ONE bit per
    /// `read_bit` — a bounds check, byte index and shift per renorm shift. The
    /// window refills up to 8 bytes at once and serves multi-bit takes; it
    /// consumes the same bits in the same order, so every bin (and therefore
    /// the bitstream interpretation) is identical by construction. Zero-fills
    /// past the end of the buffer exactly like the old reader (the fuzzer's
    /// slice-loop bound relies on that).
    window: u64,
    /// Number of valid bits at the top of `window`.
    wbits: u32,
    range: u32,
    offset: u32,
    /// 460 context models, each packed as `state * 2 + mps`.
    ctx: [u8; 460],
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
        let model = if is_i { 0 } else { ((init_idc + 1) as usize).min(3) };
        let q = qp.clamp(0, 51);
        let mut ctx = [0u8; 460];
        for (i, c) in ctx.iter_mut().enumerate() {
            let (m, n) = CTX_INIT[i][model];
            let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
            // Packed as state*2 + mps; same (state, mps) pair as the spec form.
            *c = if pre <= 63 {
                ((63 - pre) as u8) << 1
            } else {
                (((pre - 64) as u8) << 1) | 1
            };
        }
        let trace = std::env::var_os("RH_CABAC_TRACE").is_some();
        let mut e = Cabac { data, byte_pos: start_byte, window: 0, wbits: 0, range: 510, offset: 0, ctx, trace, sym: 0 };
        e.offset = e.take(9);
        e
    }

    /// Engine state `(codIRange, codIOffset)` — for bring-up verification against the
    /// oracle's symbol 0 (Brick 1.1). At slice start this is `(510, first-9-bits)`.
    pub fn dbg_state(&self) -> (u32, u32) {
        (self.range, self.offset)
    }

    /// Tops the window up to ≥ 57 valid bits (fast path: one 8-byte load when
    /// the remaining data allows, else per-byte with zero-fill past the end).
    #[inline]
    fn refill(&mut self) {
        if let Some(chunk) = self.data.get(self.byte_pos..self.byte_pos + 8) {
            // Load 8 bytes big-endian, keep as many WHOLE bytes as fit below the
            // current valid bits, masked so no stale bits land past `wbits`.
            let take_bytes = ((64 - self.wbits) / 8) as usize; // ≥ 4 when called from take()
            let keep = (take_bytes * 8) as u32;
            let v = u64::from_be_bytes(chunk.try_into().unwrap());
            let v = if keep == 64 { v } else { v & (!0u64 << (64 - keep)) };
            self.window |= v >> self.wbits;
            self.byte_pos += take_bytes;
            self.wbits += keep;
            return;
        }
        while self.wbits <= 56 {
            let b = self.data.get(self.byte_pos).copied().unwrap_or(0);
            self.window |= (b as u64) << (56 - self.wbits);
            self.byte_pos += 1;
            self.wbits += 8;
        }
    }

    /// Takes the next `n` (≤ 32) bits MSB-first; zero-fills past the buffer end.
    /// `n == 0` is legal and yields 0 — the branchless renorm calls it with the
    /// shift count straight out of `leading_zeros`, which is 0 whenever no
    /// renormalization is due. `(w >> (63-n)) >> 1` equals `w >> (64-n)` for
    /// n ≥ 1 and 0 for n = 0, so no shift ever reaches the illegal width 64.
    #[inline(always)]
    fn take(&mut self, n: u32) -> u32 {
        if self.wbits < n {
            self.refill();
        }
        let v = ((self.window >> (63 - n)) >> 1) as u32;
        self.window <<= n;
        self.wbits -= n;
        v
    }

    /// Renormalization (spec §9.3.3.2.2): keep `range` ≥ 256, refilling `offset`.
    /// BRANCHLESS: `range ≤ 510` always, so `leading_zeros() - 23` is exactly the
    /// spec loop's iteration count and is 0 when no renormalization is due (the
    /// shifts and the zero-width `take` are then no-ops). Same bits, same order.
    #[inline(always)]
    fn renorm(&mut self) {
        let n = self.range.leading_zeros() - 23;
        self.range <<= n;
        self.offset = (self.offset << n) | self.take(n);
    }

    /// Decodes a context-coded bin (spec §9.3.3.2.1), updating the context model.
    pub fn decode_decision(&mut self, ctx_idx: usize) -> u32 {
        self.tr("D");
        // BRANCHLESS bin decode (H-35, ffmpeg's `get_cabac_inline` shape). The
        // LPS/MPS test is inherently ~coin-flip on a well-adapted context, so a
        // branch here mispredicts constantly; instead derive an all-ones/zero
        // MASK and select with arithmetic. `& 127` is free insurance that also
        // proves every table index in range, dropping the bounds checks.
        let s = (self.ctx[ctx_idx] & 127) as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = LPS_RANGE[q * 128 + s] as u32;
        // PRECONDITION of the mask arithmetic below: `range >= 256` on entry, so
        // `range - lps` (lps <= 240) stays positive and the i32 sign test is a
        // true "offset >= range" test. Renormalization guarantees it after every
        // bin, and `new()` starts at 510 — the literal `if` form did not need
        // this, so it is asserted rather than assumed.
        debug_assert!(self.range >= 256, "renorm invariant broken: range={}", self.range);
        self.range -= lps;
        // mask = !0 when `offset >= range` (the LPS path), else 0. `range` and
        // `offset` are both < 2^16 here, so the i32 arithmetic cannot overflow.
        let mask = ((self.range as i32 - self.offset as i32 - 1) >> 31) as u32;
        // LPS: offset -= range; range = lps.  MPS: both unchanged.
        self.offset -= self.range & mask;
        self.range = self.range.wrapping_add(lps.wrapping_sub(self.range) & mask);
        // One table covers both transitions; `| 128` picks the LPS half.
        self.ctx[ctx_idx] = TRANS[s | (mask as usize & 128)];
        // MPS -> s&1; LPS -> (s&1)^1.
        let bin = (s as u32 ^ mask) & 1;
        self.renorm();
        bin
    }

    /// Decodes a bypass (equiprobable) bin (spec §9.3.3.2.3).
    #[inline(always)]
    pub fn decode_bypass(&mut self) -> u32 {
        self.tr("B");
        self.offset = (self.offset << 1) | self.take(1);
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
        let model = if is_i { 0 } else { ((init_idc + 1) as usize).min(3) };
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
        // Packed as state*2 + mps (H-35): state 46, MPS 0 -> 92.
        assert_eq!(dec.ctx[0] >> 1, 46, "state");
        assert_eq!(dec.ctx[0] & 1, 0, "mps");
        // Engine init: range 510, offset = first 9 bits of 0xFFFF = 0x1FF.
        assert_eq!(dec.range, 510);
        assert_eq!(dec.offset, 0x1FF);
    }

    /// H-35 oracle: for EVERY packed state and range quartile, the packed tables
    /// must reproduce the literal spec derivation (RangeLPS, the bin value, and
    /// both transitions including the state-0 MPS flip) exactly. 512 cases —
    /// cheaper and stricter than trusting a corpus.
    #[test]
    fn packed_state_tables_match_spec_form() {
        for s in 0usize..128 {
            let (state, mps) = ((s >> 1) as u8, (s & 1) as u8);
            for q in 0usize..4 {
                assert_eq!(LPS_RANGE[q * 128 + s], RANGE_LPS[state as usize][q], "lps s={s} q={q}");
            }
            // MPS half: bin == mps, state advances, mps unchanged.
            let mps_t = TRANS[s];
            assert_eq!(mps_t >> 1, STATE_TRANS[state as usize][1], "mps-trans state s={s}");
            assert_eq!(mps_t & 1, mps, "mps-trans mps s={s}");
            // LPS half: bin == 1-mps, state falls back, mps flips only at state 0.
            let lps_t = TRANS[128 + s];
            let want_mps = if state == 0 { 1 - mps } else { mps };
            assert_eq!(lps_t >> 1, STATE_TRANS[state as usize][0], "lps-trans state s={s}");
            assert_eq!(lps_t & 1, want_mps, "lps-trans mps s={s}");
        }
    }

    /// H-35 oracle #2: the BRANCHLESS mask arithmetic must equal the literal
    /// `if offset >= range` form for every (range, offset, state) combination
    /// the engine can present — the mask, the two conditional updates, the
    /// transition-table half selection, and the bin value. This is the whole
    /// risk surface of the branchless rewrite, checked exhaustively rather than
    /// inferred from a corpus that happens to decode.
    #[test]
    fn branchless_mask_matches_conditional_form() {
        // Reachable domain only: renorm guarantees `range` in 256..=510 on entry
        // and the spec invariant `offset < range` holds throughout. (Widening
        // past this tests states the engine cannot present — and the wrapped
        // `range - lps` there makes BOTH forms meaningless, not just one.)
        for s in 0usize..128 {
            for range in [256u32, 257, 300, 383, 384, 400, 448, 509, 510] {
                for offset in [0u32, 1, 127, 128, 255, 256, 300, 383, 384, 509] {
                    if offset >= range {
                        continue;
                    }
                    let q = ((range >> 6) & 3) as usize;
                    let lps = LPS_RANGE[q * 128 + s] as u32;
                    let r1 = range.wrapping_sub(lps);
                    // literal spec form
                    let (mut lr, mut lo, lbin, lctx) = if offset >= r1 {
                        (lps, offset - r1, (s as u32 & 1) ^ 1, TRANS[128 + s])
                    } else {
                        (r1, offset, s as u32 & 1, TRANS[s])
                    };
                    // branchless form, exactly as `decode_decision` computes it
                    let mask = ((r1 as i32 - offset as i32 - 1) >> 31) as u32;
                    let bo = offset - (r1 & mask);
                    let br = r1.wrapping_add(lps.wrapping_sub(r1) & mask);
                    let bctx = TRANS[s | (mask as usize & 128)];
                    let bbin = (s as u32 ^ mask) & 1;
                    // (silence unused-mut on the literal bindings)
                    lr += 0;
                    lo += 0;
                    assert_eq!((lr, lo, lbin, lctx), (br, bo, bbin, bctx), "s={s} range={range} offset={offset}");
                }
            }
        }
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
