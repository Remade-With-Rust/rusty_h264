//! CABAC arithmetic *encoder* engine (spec §9.3.4) — the exact forward inverse of
//! the decoder's [`rusty_h264_decoder::cabac`] engine. Promoted verbatim from the
//! decoder's round-trip-validated test encoder (`engine_roundtrip_many` sweeps
//! QP × init-model × 40 seeds), so the range/offset evolution, every
//! `RANGE_LPS`/`STATE_TRANS` transition, and the bypass/terminate/flush paths are
//! already proven bit-exact against [`Cabac`]. This module only wraps that engine
//! in a syntax-facing API (`encode_decision`/`encode_bypass`/`encode_terminate`)
//! and the 460 adaptive context models.
//!
//! Tables (`CTX_INIT`, `RANGE_LPS`, `STATE_TRANS`) are shared with the decoder via
//! [`rusty_h264_common::cabac_tables`] — one source of truth, no drift.

#[allow(unused_imports)]
use alloc::vec::Vec;

use rusty_h264_common::cabac_tables::{CTX_INIT, RANGE_LPS, STATE_TRANS};

/// Initialise the 460 context models `(state, mps)` from `CTX_INIT` (spec §9.3.1.1).
/// Identical to the decoder's `Cabac::new` context init.
fn init_ctx(qp: i32, init_idc: u32, is_i: bool) -> [(u8, u8); 460] {
    let model = if is_i {
        0
    } else {
        ((init_idc + 1) as usize).min(3)
    };
    let q = qp.clamp(0, 51);
    let mut ctx = [(0u8, 0u8); 460];
    for (i, slot) in ctx.iter_mut().enumerate() {
        let (m, n) = CTX_INIT[i][model];
        let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
        *slot = if pre <= 63 {
            ((63 - pre) as u8, 0)
        } else {
            ((pre - 64) as u8, 1)
        };
    }
    ctx
}

/// RenormE after an LPS, as one shift: indexed by `lps >> 3`, the number of
/// doublings that brings the LPS range (6..=240 for the adaptive states 0..=62)
/// back into [256, 510] (x264's `cabac_renorm_shift`, first 32 entries; the
/// shift is uniform within each bucket of eight). `lps_shift_renormalises`
/// pins it for every state and quarter.
const LPS_SHIFT: [u8; 32] = [
    6, 5, 4, 4, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

/// The CABAC arithmetic encoder: the low/range interval coder (spec §9.3.4) plus
/// the 460 adaptive context models. `bits` accumulates the raw output bit stream
/// (delayed by `put_bit`'s carry/outstanding logic); `into_bytes` packs it
/// MSB-first once the final `encode_terminate(true)` has flushed the coder.
///
/// V5 brick 1 (encoder Ir campaign): the interval coder is BYTE-WISE (x264's
/// `low`/`queue` form). RenormE's bit-at-a-time loop — a compare chain and a
/// `PutBit` call per output bit, each walking the outstanding run — became one
/// table-driven shift, and bytes leave once per eight shifts with carries
/// resolved a byte at a time. The code value is the same integer, so every
/// bit is the same bit: `byte_engine_matches_spec_engine` drives this engine
/// and the spec's bit-wise one (kept in the tests as the oracle) through the
/// same random symbol streams and requires identical bytes AND an identical
/// `pos()` after every symbol.
pub struct CabacEncoder {
    /// Pending code bits: the spec's 10-bit `codILow` window plus `queue + 8`
    /// resolved-but-unwritten bits above it, plus one carry bit above those.
    /// Unlike the spec's register nothing is subtracted on output — a carry
    /// simply propagates upward, into `out` when the byte is written.
    low: u32,
    /// `codIRange`: 9 bits, in [256, 510] between symbols.
    range: u32,
    /// Pending-bit count minus 8. Starts at -9: one bit for the spec's
    /// suppressed first `PutBit` (a carry position that can never be 1 — it
    /// would mean an interval wider than the initial one) plus a byte to fill.
    /// A byte is written when it reaches 0, so it is negative between symbols.
    queue: i32,
    /// Complete bytes of value 0xFF held back because a later carry may still
    /// turn them into 0x00 (adding one to the byte before them) — the
    /// byte-wise form of the spec's `bitsOutstanding`.
    outstanding: u32,
    /// `queue` threshold for the slow path: 0 normally, `i32::MIN` while
    /// `lead` is set, so every shift is inspected until `lead` clears.
    trigger: i32,
    /// True until the shift at which the spec engine makes its first `PutBit`.
    /// Before it, the spec's `pos()` counts the (to-be-suppressed) first bit
    /// as outstanding, so ours adds it too — the accountant's clock stays
    /// exactly the spec engine's. During `lead` every shifted bit is 1, so it
    /// clears at the first shift that brings a 0 into the window's top bit.
    lead: bool,
    /// Written bytes (H-16: packed output, recycled per slice).
    out: Vec<u8>,
    /// H-18: the 460 context models INLINE (was a heap `Vec`): every bin does
    /// `ctx[ctx_idx]` read-modify-write, so the Vec cost a pointer load + a
    /// bounds check per bin on top of the array access. Fixed-size = the length
    /// is a constant the optimizer folds against. Byte-identical (same values,
    /// same order); ~920 bytes lives in the encoder struct.
    ctx: [(u8, u8); 460],
    /// Running count of bins emitted — the RD bit-cost proxy (each context/bypass
    /// bin is ~1 coded bit; adaptive contexts make it fractional, but the *count*
    /// is the cheap monotone cost surrogate the mode decision can use).
    pub bins: u64,
}

impl CabacEncoder {
    /// New encoder with contexts initialised for `qp` / `init_idc` / slice type.
    pub fn new(qp: i32, init_idc: u32, is_i: bool) -> Self {
        Self::new_with_out(qp, init_idc, is_i, Vec::with_capacity(4096))
    }

    /// As [`new`](Self::new), reusing a recycled output buffer — the payload
    /// Vec grows to slice size every slice, so its capacity is worth keeping
    /// (11.11; pair with `into_bytes` + the caller's recycle).
    pub fn new_with_out(qp: i32, init_idc: u32, is_i: bool, mut out: Vec<u8>) -> Self {
        // Prometheus entropy tap: every real slice encoder opens a segment
        // tagged with the context-init inputs (the tables CASC wants to beat).
        #[cfg(feature = "prometheus-telemetry")]
        crate::telemetry::begin_slice(qp, init_idc, is_i);
        out.clear();
        CabacEncoder {
            low: 0,
            range: 510,
            queue: -9,
            outstanding: 0,
            trigger: i32::MIN,
            lead: true,
            out,
            ctx: init_ctx(qp, init_idc, is_i),
            bins: 0,
        }
    }

    /// The slow path of every shift: tracks `lead` while it is set, and writes
    /// the oldest pending byte once eight bits have accumulated. Out of line —
    /// it runs about once per eight shifts, and keeping it out keeps the bin
    /// coders' frames small.
    #[inline(never)]
    fn slow(&mut self) {
        if self.lead {
            // Still in the spec's initial outstanding run iff every pending
            // bit from the window's top bit up is 1 (the carry bit above them
            // is 0 by the interval bound).
            if self.low < (1u32 << (self.queue + 18)) - 512 {
                self.lead = false;
                self.trigger = 0;
            }
            if self.queue < 0 {
                return;
            }
        }
        self.put_byte();
    }

    /// Write the oldest pending byte (x264's `cabac_putbyte`): 0xFF is held
    /// back as outstanding; anything else first settles the carry — into the
    /// byte before the outstanding run, and turning the run into 0x00 (carry)
    /// or 0xFF (none) — then is written. A byte receives at most one carry and
    /// is never 0xFF when it does, so the add cannot overflow.
    #[inline]
    fn put_byte(&mut self) {
        let out = self.low >> (self.queue + 10);
        self.low &= (0x400u32 << self.queue) - 1;
        self.queue -= 8;
        if out & 0xff == 0xff {
            self.outstanding += 1;
        } else {
            let carry = out >> 8;
            if carry != 0 {
                // Never at the first byte: that carry is the spec's suppressed
                // first bit, which is always 0.
                if let Some(last) = self.out.last_mut() {
                    *last += 1;
                }
            }
            let fill = (carry as u8).wrapping_sub(1);
            for _ in 0..self.outstanding {
                self.out.push(fill);
            }
            self.outstanding = 0;
            self.out.push(out as u8);
        }
    }

    /// EncodeDecision (§9.3.4.3.1) — code one context-adaptive bin and update the model.
    pub fn encode_decision(&mut self, ctx_idx: usize, bin: u32) {
        // H-18: ONE bounds-checked slot borrow for the read AND the write-back
        // (was up to three separate indexings per bin); the table lookups then
        // index fixed-size arrays with an in-range state. Byte-identical.
        let slot = &mut self.ctx[ctx_idx];
        let (state, mps) = *slot;
        // Prometheus entropy tap: the model state BEFORE the bin — exactly
        // what the shipping coder knew. Observe-only; emit path unchanged.
        #[cfg(feature = "prometheus-telemetry")]
        crate::telemetry::record(ctx_idx as u16, state, mps, bin as u8);
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_LPS[state as usize][q];
        let range = self.range - lps as u32;
        if bin != mps as u32 {
            // LPS: the range becomes `lps` (>= 6), renormalised in one shift.
            let nm = if state == 0 { 1 - mps } else { mps };
            *slot = (STATE_TRANS[state as usize][0], nm);
            let shift = LPS_SHIFT[(lps >> 3) as usize] as u32;
            self.low = (self.low + range) << shift;
            self.range = (lps as u32) << shift;
            self.queue += shift as i32;
            if self.queue >= self.trigger {
                self.slow();
            }
        } else {
            // MPS: `range` stays >= 128, so at most one doubling.
            slot.0 = STATE_TRANS[state as usize][1];
            if range < 256 {
                self.low <<= 1;
                self.range = range << 1;
                self.queue += 1;
                if self.queue >= self.trigger {
                    self.slow();
                }
            } else {
                self.range = range;
            }
        }
        self.bins += 1;
    }

    /// EncodeBypass (§9.3.4.3.2) — code one equiprobable bin (no context).
    pub fn encode_bypass(&mut self, bin: u32) {
        self.low <<= 1;
        if bin != 0 {
            self.low += self.range;
        }
        self.queue += 1;
        if self.queue >= self.trigger {
            self.slow();
        }
        self.bins += 1;
    }

    /// Code `n` bypass bins of `val`, MSB first (unsigned bypass strings) — the
    /// EG suffix tail of `cb_exp_bypass` (mvd UEG3 and coeff-level UEG0). Was
    /// "reserved for CABAC-4" from 2026-08 until the H9 pass found CABAC-4 had
    /// shipped its own inline copy of this loop; wired 2026-08-26.
    pub fn encode_bypass_bits(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            self.encode_bypass((val >> i) & 1);
        }
    }

    /// EncodeTerminate (§9.3.4.5) — code the `end_of_slice_flag`. `end == false`
    /// between MBs (more to come); `end == true` on the last MB, which also runs
    /// EncodeFlush (§9.3.4.6) to close the stream. After `end == true` call
    /// [`into_bytes`](Self::into_bytes).
    pub fn encode_terminate(&mut self, end: bool) {
        self.range -= 2;
        if !end {
            if self.range < 256 {
                self.low <<= 1;
                self.range <<= 1;
                self.queue += 1;
                if self.queue >= self.trigger {
                    self.slow();
                }
            }
        } else {
            // codILow += codIRange; codIRange = 2; RenormE = seven doublings.
            self.low = (self.low + self.range) << 7;
            self.range = 2 << 7;
            self.queue += 7;
            if self.queue >= self.trigger {
                self.slow();
            }
            // EncodeFlush: PutBit(codILow >> 9 & 1), then WriteBits(((codILow
            // >> 7) & 3) | 1, 2) — i.e. every pending bit down to window bit 7,
            // with bit 7 forced to 1 (it doubles as rbsp_stop_one_bit). Above
            // them sits the final carry, which settles the outstanding run.
            let nbits = (self.queue + 11) as u32;
            let v = (self.low >> 7) | 1;
            let carry = v >> nbits;
            if carry != 0 {
                if let Some(last) = self.out.last_mut() {
                    *last += 1;
                }
            }
            let fill = (carry as u8).wrapping_sub(1);
            for _ in 0..self.outstanding {
                self.out.push(fill);
            }
            self.outstanding = 0;
            self.lead = false;
            // `nbits` is 3..=10: write a whole byte when there is one and keep
            // the rest as the pending tail (`pos()` counts it, `into_bytes`
            // writes it left-aligned).
            let mut rem = nbits;
            if rem >= 8 {
                rem -= 8;
                self.out.push((v >> rem) as u8);
            }
            self.low = v & ((1u32 << rem) - 1);
            self.queue = rem as i32 - 8;
            self.trigger = i32::MAX;
        }
        self.bins += 1;
    }

    /// EXACT emitted-bit position — the bit accountant's clock (analyzer
    /// instrument #6). Counts bytes already flushed, bits held in the packing
    /// accumulator, and carry-delayed `outstanding` bits (each is determined:
    /// it will be emitted as the inverse of the next resolved bit). Deltas of
    /// this across a syntax element are that element's real coded bits, so the
    /// per-element buckets SUM EXACTLY to the slice payload — which is what
    /// makes the accountant reconcilable against the file rather than a model.
    #[inline]
    pub fn pos(&self) -> u64 {
        // Written bytes, held-back 0xFF bytes, and the `queue + 8` pending bits
        // above the window — plus the spec's not-yet-suppressed first bit
        // while `lead` holds (see the field).
        ((self.out.len() as u64 + self.outstanding as u64) * 8 + self.lead as u64)
            .wrapping_add((self.queue + 8) as i64 as u64)
    }

    /// Pack the accumulated bits MSB-first into bytes. Call after the final
    /// `encode_terminate(true)`. The output is byte-aligned (EncodeFlush guarantees
    /// the closing `1` + alignment), ready to append after the byte-aligned slice
    /// header.
    pub fn into_bytes(mut self) -> Vec<u8> {
        let rem = self.queue + 8;
        if rem > 0 {
            // The flush's pending tail, left-aligned (alignment zeros after
            // the stop bit), exactly as the old MSB-first repack did.
            self.out.push((self.low << (8 - rem)) as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;
    use rusty_h264_decoder::cabac_test::Cabac;

    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    /// The promoted engine must still round-trip through the decoder exactly — the
    /// same sweep the decoder validated its own test encoder with, now against this
    /// production module.
    fn roundtrip(qp: i32, init_idc: u32, is_i: bool, seed: u32, n: usize) {
        let mut rng = Rng(seed);
        let mut script: Vec<(u8, usize, u32)> = Vec::with_capacity(n);
        let mut enc = CabacEncoder::new(qp, init_idc, is_i);
        for _ in 0..n {
            let r = rng.next();
            let kind = (r & 1) as u8;
            let ctx = (r >> 1) as usize % 460;
            let bin = (r >> 12) & 1;
            script.push((kind, ctx, bin));
            if kind == 0 {
                enc.encode_decision(ctx, bin);
            } else {
                enc.encode_bypass(bin);
            }
        }
        enc.encode_terminate(true);
        let bytes = enc.into_bytes();

        let mut dec = Cabac::new(&bytes, 0, qp, init_idc, is_i);
        for (i, &(kind, ctx, bin)) in script.iter().enumerate() {
            let got = if kind == 0 {
                dec.decode_decision(ctx)
            } else {
                dec.decode_bypass()
            };
            assert_eq!(got, bin, "bin {i} (kind {kind}, ctx {ctx}) mismatched");
        }
        assert!(
            dec.decode_terminate(),
            "terminate should signal end-of-stream"
        );
    }

    /// The spec's bit-wise engine (§9.3.4.2-6: RenormE, PutBit with
    /// `firstBitFlag`/`bitsOutstanding`, EncodeBypass, EncodeFlush), exactly as
    /// it shipped before V5 brick 1 — the oracle the byte-wise engine must
    /// reproduce bit for bit, `pos()` included.
    struct SpecEngine {
        low: u32,
        range: u32,
        outstanding: u32,
        first: bool,
        acc: u32,
        nacc: u32,
        out: Vec<u8>,
        ctx: [(u8, u8); 460],
    }

    impl SpecEngine {
        fn new(qp: i32, init_idc: u32, is_i: bool) -> Self {
            SpecEngine {
                low: 0,
                range: 510,
                outstanding: 0,
                first: true,
                acc: 0,
                nacc: 0,
                out: Vec::new(),
                ctx: init_ctx(qp, init_idc, is_i),
            }
        }
        fn push_packed(&mut self, b: u32) {
            self.acc = (self.acc << 1) | b;
            self.nacc += 1;
            if self.nacc == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.nacc = 0;
            }
        }
        fn put_bit(&mut self, b: u32) {
            if self.first {
                self.first = false;
            } else {
                self.push_packed(b);
            }
            while self.outstanding > 0 {
                self.push_packed(1 - b);
                self.outstanding -= 1;
            }
        }
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
        fn decision(&mut self, ctx_idx: usize, bin: u32) {
            let (state, mps) = self.ctx[ctx_idx];
            let lps = RANGE_LPS[state as usize][((self.range >> 6) & 3) as usize] as u32;
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
        fn bypass(&mut self, bin: u32) {
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
        fn terminate(&mut self, end: bool) {
            self.range -= 2;
            if !end {
                self.renorm();
            } else {
                self.low += self.range;
                self.range = 2;
                self.renorm();
                self.put_bit((self.low >> 9) & 1);
                let v = ((self.low >> 7) & 3) | 1;
                self.push_packed((v >> 1) & 1);
                self.push_packed(v & 1);
            }
        }
        fn pos(&self) -> u64 {
            (self.out.len() as u64) * 8 + self.nacc as u64 + self.outstanding as u64
        }
        fn into_bytes(mut self) -> Vec<u8> {
            if self.nacc > 0 {
                self.out.push((self.acc << (8 - self.nacc)) as u8);
            }
            self.out
        }
    }

    /// One random symbol stream through both engines: identical `pos()` after
    /// every symbol, identical bytes at the end. `skew` biases the decision
    /// bins (long MPS runs, then LPS storms); bypass runs of equal bins build
    /// the 0xFF bytes whose carries the outstanding logic must settle.
    fn spec_vs_byte(qp: i32, init_idc: u32, is_i: bool, seed: u32, n: usize, skew: u32) {
        let mut rng = Rng(seed | 1);
        let mut a = CabacEncoder::new(qp, init_idc, is_i);
        let mut b = SpecEngine::new(qp, init_idc, is_i);
        assert_eq!(a.pos(), b.pos(), "pos at start");
        let mut run_bin = 0u32;
        let mut run = 0u32;
        for i in 0..n {
            let r = rng.next();
            match r % 16 {
                0..=9 => {
                    let ctx = (r >> 4) as usize % 460;
                    let mps = a.ctx[ctx].1 as u32;
                    let bin = if (r >> 16) % 16 < skew { mps } else { (r >> 21) & 1 };
                    a.encode_decision(ctx, bin);
                    b.decision(ctx, bin);
                }
                10..=13 => {
                    if run == 0 {
                        run = (r >> 8) % 24;
                        run_bin = (r >> 20) & 1;
                    }
                    let bin = if run > 0 {
                        run -= 1;
                        run_bin
                    } else {
                        (r >> 22) & 1
                    };
                    a.encode_bypass(bin);
                    b.bypass(bin);
                }
                14 => {
                    let (v, k) = ((r >> 8) & 0xffff, (r >> 4) % 17);
                    a.encode_bypass_bits(v, k);
                    for j in (0..k).rev() {
                        b.bypass((v >> j) & 1);
                    }
                }
                _ => {
                    a.encode_terminate(false);
                    b.terminate(false);
                }
            }
            assert_eq!(a.pos(), b.pos(), "pos after symbol {i} (seed {seed})");
        }
        a.encode_terminate(true);
        b.terminate(true);
        assert_eq!(a.pos(), b.pos(), "pos after flush (seed {seed})");
        assert_eq!(a.into_bytes(), b.into_bytes(), "bytes (seed {seed}, n {n})");
    }

    #[test]
    fn byte_engine_matches_spec_engine() {
        // Short streams (0..=40 symbols) put the flush inside the spec's
        // initial outstanding run; long ones exercise carries through runs of
        // held-back 0xFF bytes.
        for seed in 1..=4000u32 {
            let n = (seed % 41) as usize;
            spec_vs_byte(26, seed % 3, seed % 4 == 0, seed.wrapping_mul(0x9E37_79B9), n, seed % 17);
        }
        for &qp in &[0, 12, 26, 37, 51] {
            for seed in 1..=60u32 {
                for &skew in &[0u32, 8, 13, 15, 16] {
                    spec_vs_byte(qp, seed % 3, seed % 5 == 0, seed.wrapping_mul(2654435761) ^ skew, 3000, skew);
                }
            }
        }
    }

    #[test]
    fn lps_shift_renormalises() {
        for st in 0..63 {
            for q in 0..4 {
                let lps = RANGE_LPS[st][q];
                let r = (lps as u32) << LPS_SHIFT[(lps >> 3) as usize];
                assert!((256..=510).contains(&r), "state {st} q {q}: lps {lps} -> {r}");
            }
        }
    }

    #[test]
    fn engine_roundtrip_many() {
        for &qp in &[0, 12, 26, 37, 51] {
            for &(idc, is_i) in &[(0u32, true), (0, false), (1, false), (2, false)] {
                for seed in 1..=40u32 {
                    roundtrip(
                        qp,
                        idc,
                        is_i,
                        seed.wrapping_mul(2654435761),
                        seed as usize * 53,
                    );
                }
            }
        }
    }
}
