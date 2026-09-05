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

/// The CABAC arithmetic encoder: the low/range interval coder (spec §9.3.4) plus
/// the 460 adaptive context models. `bits` accumulates the raw output bit stream
/// (delayed by `put_bit`'s carry/outstanding logic); `into_bytes` packs it
/// MSB-first once the final `encode_terminate(true)` has flushed the coder.
pub struct CabacEncoder {
    low: u32,
    range: u32,
    outstanding: u32,
    first: bool,
    /// H-16: PACKED output. The old `bits: Vec<u8>` stored ONE BIT PER BYTE —
    /// a `Vec::push` per coded bit (~700K/stream, from `Vec::new()`'s realloc
    /// chain) plus a full repack walk in `into_bytes`. Bits now accumulate in
    /// `acc` and flush per BYTE; the bit sequence and MSB-first packing are
    /// unchanged, so the output bytes are identical by construction (pinned by
    /// the round-trip suite + the full-encode hash gate).
    acc: u32,
    nacc: u32,
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
            outstanding: 0,
            first: true,
            acc: 0,
            nacc: 0,
            out,
            ctx: init_ctx(qp, init_idc, is_i),
            bins: 0,
        }
    }

    /// Emit a resolved bit plus any carry-delayed `outstanding` bits (spec's
    /// bit-with-carry PutBit).
    #[inline]
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
        let inv = 1 - b;
        while self.outstanding > 0 {
            self.push_packed(inv);
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
        let lps = RANGE_LPS[state as usize][q] as u32;
        self.range -= lps;
        if bin != mps as u32 {
            self.low += self.range;
            self.range = lps;
            let nm = if state == 0 { 1 - mps } else { mps };
            *slot = (STATE_TRANS[state as usize][0], nm);
        } else {
            slot.0 = STATE_TRANS[state as usize][1];
        }
        self.renorm();
        self.bins += 1;
    }

    /// EncodeBypass (§9.3.4.3.2) — code one equiprobable bin (no context).
    pub fn encode_bypass(&mut self, bin: u32) {
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
            self.renorm();
        } else {
            self.low += self.range;
            self.range = 2;
            self.renorm();
            // EncodeFlush bit output.
            self.put_bit((self.low >> 9) & 1);
            let v = ((self.low >> 7) & 3) | 1;
            self.push_packed((v >> 1) & 1);
            self.push_packed(v & 1);
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
        (self.out.len() as u64) * 8 + self.nacc as u64 + self.outstanding as u64
    }

    /// Pack the accumulated bits MSB-first into bytes. Call after the final
    /// `encode_terminate(true)`. The output is byte-aligned (EncodeFlush guarantees
    /// the closing `1` + alignment), ready to append after the byte-aligned slice
    /// header.
    pub fn into_bytes(mut self) -> Vec<u8> {
        if self.nacc > 0 {
            // Left-align the tail exactly as the old MSB-first repack did.
            self.out.push((self.acc << (8 - self.nacc)) as u8);
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
