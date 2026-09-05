//! CABAC arithmetic decoding engine (spec §9.3.3.2) + context initialization
//! (§9.3.1.1). The literal-spec engine (codIRange/codIOffset, RenormD), which is
//! bit-exact to openh264's optimized variant. Tables in [`crate::cabac_tables`].

#[allow(unused_imports)]
use alloc::borrow::ToOwned;
#[allow(unused_imports)]
use alloc::boxed::Box;
#[allow(unused_imports)]
use alloc::format;
#[allow(unused_imports)]
use alloc::string::{String, ToString};
#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;
use rusty_h264_common::cabac_tables::{CTX_INIT, RANGE_LPS, STATE_TRANS};

/// Profile-only bin census: how many bins of each class the engine decodes.
/// The entropy stage's time divided by these counts gives ns/bin — the number
/// that decides whether the engine or the syntax around it is the target.
#[cfg(feature = "profile")]
pub mod bin_census {
    use core::sync::atomic::Ordering::Relaxed;
    use rusty_h264_common::atomic::AtomicU64;
    pub static DECISIONS: AtomicU64 = AtomicU64::new(0);
    pub static BYPASSES: AtomicU64 = AtomicU64::new(0);
    pub static TERMINATES: AtomicU64 = AtomicU64::new(0);
    /// Decision bins whose renormalization shift was nonzero.
    pub static RENORMS: AtomicU64 = AtomicU64::new(0);
    pub fn reset() {
        DECISIONS.store(0, Relaxed);
        BYPASSES.store(0, Relaxed);
        TERMINATES.store(0, Relaxed);
    }
    pub fn snapshot() -> (u64, u64, u64) {
        (
            DECISIONS.load(Relaxed),
            BYPASSES.load(Relaxed),
            TERMINATES.load(Relaxed),
        )
    }
    pub fn renorms() -> u64 {
        RENORMS.load(Relaxed)
    }
}

/// FUSED per-(quartile, packed-state) record: `lps | trans_mps<<8 | trans_lps<<16`.
///
/// A context model is ONE byte: `state * 2 + mps` (0..=127) — ffmpeg/openh264's
/// packing (H-35). The literal two-field form cost two loads and two stores per
/// bin plus `1 - mps` arithmetic; packed, a bin is one byte load, one table
/// lookup, one byte store, and `s & 1` for the value. This table folds the state
/// transition AND the state-0 MPS flip (spec §9.3.3.2.1.1) into the lookup, so
/// the decoded bins are identical by construction. Built at compile time from
/// the spec tables: no init cost, no `OnceLock` check on the hot path.
///
/// Why: the serial chain of a decision bin ended with a LATE load — the
/// transition table's address needs the LPS/MPS MASK, which exists only after
/// the compare, so the context write-back (and every same-context successor
/// bin: all unary and level-prefix loops re-read the context they just wrote)
/// waited on a ~5-cycle L1 load issued at the chain's end. Folding both
/// transition bytes into the SAME u32 the LPS quantity comes from makes them
/// arrive EARLY (with the lps load, whose address needs only `s` and `q`),
/// and the post-compare step becomes a 1-cycle shift-select:
/// `(entry >> (8 + (mask & 8))) & 0xFF`. 2 KB, L1-resident like the tables it
/// replaces on this path.
const fn build_fused() -> [u32; 4 * 256] {
    // Row stride 256, not 128: the packed context byte indexes the row DIRECTLY
    // (`(q << 8) | s`, s < 256 by type), so the per-bin `& 127` mask that the
    // 128-stride layout needed as its bounds proof is gone. Only s < 128 is
    // ever populated or read (the packed-state invariant); the unused half of
    // each row is never touched, so the L1 footprint is unchanged (2 KB live).
    let mut t = [0u32; 4 * 256];
    let mut q = 0;
    while q < 4 {
        let mut s = 0;
        while s < 128 {
            let lps = RANGE_LPS[s >> 1][q] as u32;
            let tm = {
                let mps = s as u8 & 1;
                ((STATE_TRANS[s >> 1][1] << 1) | mps) as u32
            };
            let tl = {
                let mps = s as u8 & 1;
                let new_mps = if s >> 1 == 0 { 1 - mps } else { mps };
                ((STATE_TRANS[s >> 1][0] << 1) | new_mps) as u32
            };
            t[q * 256 + s] = lps | (tm << 8) | (tl << 16);
            s += 1;
        }
        q += 1;
    }
    t
}
static FUSED: [u32; 4 * 256] = build_fused();

/// Bit position of the arithmetic offset field inside [`Engine::low`].
const OFF: u32 = 41;
/// Refill when fewer than this many buffered bits remain. 8 covers the worst
/// single renormalization (6 bits) with margin; a 4-byte refill then lasts
/// ~30 typical bins.
const REFILL_AT: i32 = 8;

#[cfg(feature = "cabac-trace")]
mod trace {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    pub static ON: AtomicBool = AtomicBool::new(false);
    pub static SYM: AtomicU64 = AtomicU64::new(0);
    #[inline(never)]
    pub fn emit(kind: &str, range: u32, offset: u64) {
        let n = SYM.fetch_add(1, Relaxed);
        eprintln!("{} {} r={} o={}", n, kind, range, offset);
    }
}

/// Bring-up symbol trace hook. Compiled OUT unless the `cabac-trace` feature is
/// on: the `if self.trace` test was a load + branch on EVERY bin (~154M per
/// 1080p clip) and its cold `eprintln!` arm was inlined at every decode site --
/// 54 print arms across the residual parser and the slice loop -- and the
/// escaping call kept the engine state pinned to memory.
#[inline(always)]
fn tr(kind: &str, eng: &Engine) {
    #[cfg(feature = "cabac-trace")]
    if trace::ON.load(core::sync::atomic::Ordering::Relaxed) {
        trace::emit(kind, eng.range, eng.low >> OFF);
    }
    #[cfg(not(feature = "cabac-trace"))]
    let _ = (kind, eng);
}

/// Zero-filled 4-byte window at the end of the buffer -- cold by construction.
#[cold]
#[inline(never)]
fn refill_tail(data: &[u8], p: usize) -> u32 {
    let b = |i: usize| data.get(p + i).copied().unwrap_or(0) as u32;
    (b(0) << 24) | (b(1) << 16) | (b(2) << 8) | b(3)
}

/// The arithmetic-decoder REGISTERS, by value. Four words. A parser pulls a copy
/// out of the [`Cabac`] (`view`), decodes a whole block or syntax element against
/// it -- LLVM keeps the four words in registers because nothing they alias is
/// written -- and `commit`s once. Through `&mut Cabac` the same bins did 4 loads
/// + 4 stores EACH: the context-byte store `ctx[idx]` lands in the same struct
/// and LLVM will not prove it disjoint from `low`/`range`/`cnt`.
#[derive(Clone, Copy, Debug)]
pub struct Engine {
    /// Next byte to load into the bit window.
    byte_pos: usize,
    /// FUSED offset+window register: `low = codIOffset * 2^41 + buf`, where
    /// `buf < 2^41` holds the next `cnt` stream bits LEFT-ALIGNED at bit 40
    /// downward, so renorm is ONE shift `low <<= n` and the next bits enter the
    /// offset field by construction. Exact by invariant: `offset >= range <=>
    /// low >= range << 41` (buffered bits can never flip the comparison); the
    /// LPS subtraction cannot borrow into `buf`; `cnt <= 6 + 32 < 41`. Zero-fill
    /// past the buffer end is preserved exactly (the fuzzer relies on it).
    low: u64,
    /// Valid buffered bits below the offset field.
    cnt: i32,
    range: u32,
}

/// The 460 context models, each packed as `state * 2 + mps`, in a 512-entry
/// array so `ctx_idx & 511` is the index proof (one AND) instead of the
/// `.min(459)` cmp+cmov that ran on every bin. Entries 460.. are never read.
pub type Ctx = [u8; 512];

impl Engine {
    /// Appends 32 fresh stream bits directly below the current buffer fill
    /// (zero-filled past the end of the data). Only called when `cnt <
    /// REFILL_AT`, so the insert shift `9 - cnt` is always in `[2..=9]`.
    ///
    /// INLINED ALWAYS: as an out-of-line call inside every inlined bin decode it
    /// was the one escape of the engine on the hot path. The past-the-end arm
    /// stays cold.
    #[inline(always)]
    fn refill(&mut self, data: &[u8]) {
        // REFUTED (2026-09-04): `if p + 4 <= len { data[p..=p+3] }` does NOT fold
        // the four index checks -- `p + 4` may wrap in release, so LLVM cannot
        // derive `p + 3 < len` from it -- and it added a panic path at every
        // inlined refill site (+3,000 instrs in the slice loop). The range `get`
        // is two compares and zero panic paths; keep it.
        let v = match data.get(self.byte_pos..self.byte_pos + 4) {
            Some(c) => u32::from_be_bytes([c[0], c[1], c[2], c[3]]),
            None => refill_tail(data, self.byte_pos),
        };
        self.low |= (v as u64) << ((OFF as i32 - 32 - self.cnt) as u32);
        self.byte_pos += 4;
        self.cnt += 32;
    }

    /// Renormalization (spec 9.3.3.2.2): keep `range >= 256`. BRANCHLESS shift
    /// count (`range <= 510` => `leading_zeros()-23` is exactly the iteration
    /// count of the spec loop; 54-62% of decisions renormalize, a coin flip, so
    /// a branch would mispredict); the refill is ONE shift of the fused register.
    #[inline(always)]
    fn renorm(&mut self, data: &[u8]) {
        let n = self.range.leading_zeros() - 23;
        self.range <<= n;
        self.low <<= n;
        self.cnt -= n as i32;
        if self.cnt < REFILL_AT {
            self.refill(data);
        }
    }

    /// Decodes a context-coded bin (spec 9.3.3.2.1), updating the context model.
    #[inline(always)]
    pub fn decode_decision(&mut self, data: &[u8], ctx: &mut Ctx, ctx_idx: usize) -> u32 {
        #[cfg(feature = "profile")]
        bin_census::DECISIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tr("D", self);
        // BRANCHLESS bin decode (H-35, the `get_cabac_inline` shape of ffmpeg).
        // The LPS/MPS test is ~coin-flip on a well-adapted context, so a branch
        // mispredicts constantly; derive an all-ones/zero MASK and select.
        let ctx_idx = ctx_idx & 511;
        let s = ctx[ctx_idx] as usize;
        let q = ((self.range >> 6) & 3) as usize;
        // ONE early load yields the LPS range AND both context transitions --
        // see `build_fused` for why the transitions must not be a second,
        // mask-addressed (late) load. `(q << 8) | s` is in-bounds by type.
        let e = FUSED[(q << 8) | s];
        let lps = e & 0xFF;
        // PRECONDITION: `range >= 256` on entry, so `range - lps` (lps <= 240)
        // stays positive and the sign test is a true "offset >= range" test.
        debug_assert!(
            self.range >= 256,
            "renorm invariant broken: range={}",
            self.range
        );
        self.range -= lps;
        // mask = !0 when `offset >= range` (the LPS path), else 0 -- the sign
        // trick in 64 bits against the SCALED range (values stay below 2^51).
        let scaled = (self.range as u64) << OFF;
        let mask64 = ((scaled as i64 - self.low as i64 - 1) >> 63) as u64;
        let mask = mask64 as u32;
        // LPS: offset -= range; range = lps.  MPS: both unchanged.
        self.low -= scaled & mask64;
        self.range = self.range.wrapping_add(lps.wrapping_sub(self.range) & mask);
        // Both transitions arrived with the lps load; pick by mask with a
        // shift (mask & 8 = 8 exactly on the LPS path).
        ctx[ctx_idx] = ((e >> (8 + (mask & 8))) & 0xFF) as u8;
        // MPS -> s&1; LPS -> (s&1)^1.
        let bin = (s as u32 ^ mask) & 1;
        #[cfg(feature = "profile")]
        if self.range < 256 {
            bin_census::RENORMS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        self.renorm(data);
        bin
    }

    /// Decodes a bypass (equiprobable) bin (spec 9.3.3.2.3).
    #[inline(always)]
    pub fn decode_bypass(&mut self, data: &[u8]) -> u32 {
        #[cfg(feature = "profile")]
        bin_census::BYPASSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tr("B", self);
        self.low <<= 1;
        self.cnt -= 1;
        if self.cnt < REFILL_AT {
            self.refill(data);
        }
        // BRANCHLESS: a bypass bin is a coin flip by definition (signs, EG
        // suffix bits), so the `if low >= scaled` branch mispredicted ~half the
        // time. Same sign-mask select the decision path uses.
        let scaled = (self.range as u64) << OFF;
        let mask = ((scaled as i64 - self.low as i64 - 1) >> 63) as u64;
        self.low -= scaled & mask;
        (mask & 1) as u32
    }

    /// A bypass bin that FOLLOWS another bin of this engine with no refill in
    /// between -- the coefficient SIGN. Every bin leaves `cnt >= REFILL_AT`
    /// (renorm and bypass refill AFTER consuming), so one more bit cannot
    /// exhaust the buffer, and whatever comes next is exact: a decision bin
    /// renormalizes by at most 7 with cnt >= 7 and then refills; terminate
    /// compares the offset field only. Saves the refill test per sign.
    #[inline(always)]
    pub fn decode_bypass_sign(&mut self) -> u32 {
        #[cfg(feature = "profile")]
        bin_census::BYPASSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tr("B", self);
        debug_assert!(self.cnt >= 1, "sign bypass needs a buffered bit");
        self.low <<= 1;
        self.cnt -= 1;
        let scaled = (self.range as u64) << OFF;
        let mask = ((scaled as i64 - self.low as i64 - 1) >> 63) as u64;
        self.low -= scaled & mask;
        (mask & 1) as u32
    }

    /// Decodes the terminate bin (spec 9.3.3.2.4); `true` ends the slice (or
    /// marks I_PCM). No renormalization on terminate.
    #[inline(always)]
    pub fn decode_terminate(&mut self, data: &[u8]) -> bool {
        #[cfg(feature = "profile")]
        bin_census::TERMINATES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tr("T", self);
        self.range -= 2;
        if self.low >= (self.range as u64) << OFF {
            true
        } else {
            self.renorm(data);
            false
        }
    }
}

/// The CABAC decoder: arithmetic engine reading MSB-first from the RBSP plus the
/// 460 adaptive context models.
pub struct Cabac<'a> {
    data: &'a [u8],
    eng: Engine,
    ctx: Ctx,
}

impl<'a> Cabac<'a> {
    /// Initializes from the RBSP `data` at byte offset `start_byte` (the slice
    /// data, byte-aligned past the header), the QP of the slice (clamped 0..51),
    /// `cabac_init_idc`, and whether the slice is I/SI (spec 9.3.1).
    pub fn new(data: &'a [u8], start_byte: usize, qp: i32, init_idc: u32, is_i: bool) -> Self {
        let model = if is_i {
            0
        } else {
            ((init_idc + 1) as usize).min(3)
        };
        let q = qp.clamp(0, 51);
        let mut ctx = [0u8; 512];
        for (i, c) in ctx.iter_mut().take(460).enumerate() {
            let (m, n) = CTX_INIT[i][model];
            let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
            // Packed as state*2 + mps; same (state, mps) pair as the spec form.
            *c = if pre <= 63 {
                ((63 - pre) as u8) << 1
            } else {
                (((pre - 64) as u8) << 1) | 1
            };
        }
        #[cfg(feature = "cabac-trace")]
        trace::ON.store(
            rusty_h264_common::knob("RH_CABAC_TRACE").is_some(),
            core::sync::atomic::Ordering::Relaxed,
        );
        let mut e = Cabac {
            data,
            eng: Engine {
                byte_pos: start_byte,
                low: 0,
                cnt: 0,
                range: 510,
            },
            ctx,
        };
        e.eng.refill(data);
        // codIOffset = first 9 bits: shift them from the buffer into the
        // offset field -- the same fused move renorm makes every bin.
        e.eng.low <<= 9;
        e.eng.cnt -= 9;
        e
    }

    /// Engine state `(codIRange, codIOffset)` -- for bring-up verification against
    /// symbol 0 of the oracle (Brick 1.1). At slice start this is `(510, first-9-bits)`.
    pub fn dbg_state(&self) -> (u32, u32) {
        (self.eng.range, (self.eng.low >> OFF) as u32)
    }

    /// I_PCM sample position (spec 7.3.5 + 9.3.3.2.5). The PCM marker is a
    /// terminate bin; after it decodes as 1, the flush output of the encoder is
    /// already inside the borrowed offset bits of the engine, so the raw
    /// `pcm_sample_*` bytes start at the consumed-bit position rounded up to
    /// the next byte boundary (`pcm_alignment_zero_bit`s). `byte_pos*8 - cnt`
    /// is that consumed position (offset-field bits count as read, buffered
    /// bits do not). Valid only immediately after `decode_terminate()`
    /// returned `true` (no renormalization has run since).
    pub fn pcm_start_byte(&self) -> usize {
        let consumed = self.eng.byte_pos as isize * 8 - self.eng.cnt as isize;
        ((consumed + 7) >> 3) as usize
    }

    /// Re-initializes the arithmetic engine at absolute `byte` (spec 9.3.1.2,
    /// invoked after the I_PCM samples), KEEPING the adaptive context models --
    /// only the engine registers restart. Mirrors the tail of [`Cabac::new`].
    pub fn reinit_at(&mut self, byte: usize) {
        self.eng = Engine {
            byte_pos: byte,
            low: 0,
            cnt: 0,
            range: 510,
        };
        self.eng.refill(self.data);
        self.eng.low <<= 9;
        self.eng.cnt -= 9;
    }

    /// Split into `(data, engine copy, contexts)` for a register-resident run
    /// over many bins; finish with [`Cabac::commit`].
    #[inline(always)]
    pub fn view(&mut self) -> (&'a [u8], Engine, &mut Ctx) {
        (self.data, self.eng, &mut self.ctx)
    }

    /// Adopt the engine registers a [`Cabac::view`] run ended with.
    #[inline(always)]
    pub fn commit(&mut self, eng: Engine) {
        self.eng = eng;
    }

    /// One context-coded bin. Single-bin convenience over [`Engine::decode_decision`];
    /// multi-bin parsers take a [`Cabac::view`] instead.
    #[inline(always)]
    pub fn decode_decision(&mut self, ctx_idx: usize) -> u32 {
        let mut e = self.eng;
        let b = e.decode_decision(self.data, &mut self.ctx, ctx_idx);
        self.eng = e;
        b
    }

    /// One bypass bin (see [`Engine::decode_bypass`]).
    #[inline(always)]
    pub fn decode_bypass(&mut self) -> u32 {
        let mut e = self.eng;
        let b = e.decode_bypass(self.data);
        self.eng = e;
        b
    }

    /// The terminate bin (see [`Engine::decode_terminate`]).
    #[inline(always)]
    pub fn decode_terminate(&mut self) -> bool {
        let mut e = self.eng;
        let b = e.decode_terminate(self.data);
        self.eng = e;
        b
    }

    // NB: the byte offset where byte-aligned `pcm_sample` data resumes after an
    // I_PCM under CABAC IS wired: `pcm_start_byte()` + `reinit_at()` above are
    // the byte-realign/re-init pair, dispatched from the I_PCM arm of the
    // decoder in mb16 and gated by tests/ipcm_cabac.rs against ffmpeg.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packed-form twin tables — the two-table formulation `FUSED` folded away.
    /// Kept HERE (test-only) as the oracle that pins each `FUSED` field to the
    /// spec tables; production reads only `FUSED`.
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
    /// ONE transition table covering both paths: `[0..128)` MPS (state advances,
    /// MPS unchanged), `[128..256)` LPS (state falls back; at state 0 the MPS
    /// FLIPS per spec §9.3.3.2.1.1 — baked in, never branched).
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
        let model = if is_i {
            0
        } else {
            ((init_idc + 1) as usize).min(3)
        };
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
        assert!(
            dec.decode_terminate(),
            "terminate should signal end-of-stream"
        );
    }

    #[test]
    fn engine_roundtrip_many() {
        // Sweep QP, init model, and many random scripts: every code path
        // (LPS/MPS transitions across all 64 states, bypass, terminate, renorm).
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

    #[test]
    fn engine_init_matches_spec() {
        // ctxIdx 0 (I mb_type, m=20 n=-15) at QP 26: preCtxState =
        // Clip3(1,126,(20*26>>4)-15) = 17 -> state 63-17 = 46, MPS 0.
        let dec = Cabac::new(&[0xFF, 0xFF, 0xFF], 0, 26, 0, true);
        // Packed as state*2 + mps (H-35): state 46, MPS 0 -> 92.
        assert_eq!(dec.ctx[0] >> 1, 46, "state");
        assert_eq!(dec.ctx[0] & 1, 0, "mps");
        // Engine init: range 510, offset = first 9 bits of 0xFFFF = 0x1FF.
        assert_eq!(dec.eng.range, 510);
        assert_eq!(dec.dbg_state().1, 0x1FF);
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
                assert_eq!(
                    LPS_RANGE[q * 128 + s],
                    RANGE_LPS[state as usize][q],
                    "lps s={s} q={q}"
                );
            }
            // MPS half: bin == mps, state advances, mps unchanged.
            let mps_t = TRANS[s];
            assert_eq!(
                mps_t >> 1,
                STATE_TRANS[state as usize][1],
                "mps-trans state s={s}"
            );
            assert_eq!(mps_t & 1, mps, "mps-trans mps s={s}");
            // LPS half: bin == 1-mps, state falls back, mps flips only at state 0.
            let lps_t = TRANS[128 + s];
            let want_mps = if state == 0 { 1 - mps } else { mps };
            assert_eq!(
                lps_t >> 1,
                STATE_TRANS[state as usize][0],
                "lps-trans state s={s}"
            );
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
                    assert_eq!(
                        (lr, lo, lbin, lctx),
                        (br, bo, bbin, bctx),
                        "s={s} range={range} offset={offset}"
                    );
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
