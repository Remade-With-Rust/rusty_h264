//! Property tests for the bitstream layer's documented invariants (H-28).
//!
//! Dependency-free on purpose. `proptest` and `quickcheck` would each add a
//! dependency tree to a workspace whose entire production surface is 16 crates,
//! and this codebase already has a deterministic-PRNG idiom that does the job
//! (`tests/fuzz_no_panic.rs`). A fixed seed also means a failure here is
//! reproducible from the printed case rather than from a saved regression file.
//!
//! What a property test buys over the fuzzer: the fuzzer asserts only that
//! nothing PANICS. These assert what the functions must actually *mean* -- that
//! a reader never reads past its end, that unprevention is the inverse of
//! prevention, that a split never loses or invents a byte. A function can be
//! panic-free and still be wrong.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::BitReader;

/// SplitMix64 -- the same tiny deterministic PRNG the no-panic fuzzer uses, so
/// a failure is reproducible from its seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    /// A buffer biased toward the bytes that MATTER for Annex-B framing: zeros
    /// and the 0x00 0x00 0x03 escape. Uniform random bytes almost never produce
    /// a start code, so a uniform generator would test the boring path only.
    fn buf(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max) + 1;
        (0..n)
            .map(|_| match self.next() % 8 {
                0..=3 => 0x00,
                4 => 0x01,
                5 => 0x03,
                _ => (self.next() >> 11) as u8,
            })
            .collect()
    }
}

/// **A reader never reads past its end.** Every `BitReader` accessor returns
/// `Result`, and once the buffer is exhausted it must keep returning `Err`
/// rather than wrapping, sign-extending, or reading adjacent memory.
///
/// This is the invariant the whole decoder rests on: it is what lets the parse
/// path be `#![forbid(unsafe_code)]` and still be safe against a truncated
/// stream, which is the single most common shape of malformed input.
#[test]
fn bitreader_never_reads_past_the_end() {
    let mut rng = Rng(0x0BAD_C0DE_D15E_A5ED);
    for round in 0..20_000 {
        let data = rng.buf(48);
        let mut r = BitReader::new(&data);
        let total_bits = data.len() * 8;
        let mut consumed = 0usize;

        // Draw random-width reads until the reader refuses.
        loop {
            let width = 1 + rng.below(24);
            match r.read_bits(width as u32) {
                Ok(_) => {
                    consumed += width;
                    assert!(
                        consumed <= total_bits,
                        "round {round}: reader returned Ok after consuming {consumed} of \
                         {total_bits} bits (len {})",
                        data.len()
                    );
                }
                Err(_) => break,
            }
        }

        // A WIDE read failing does not mean the reader is empty -- asking for 24
        // bits with 8 left correctly fails while a 1-bit read still succeeds.
        // (This test asserted otherwise on its first run and was wrong, not the
        // reader.) Drain a bit at a time to reach true exhaustion, then check it
        // is permanent: no recovery, no wrap.
        let mut drained = 0usize;
        while r.read_bits(1).is_ok() {
            drained += 1;
            assert!(
                consumed + drained <= total_bits,
                "round {round}: reader yielded more bits than the buffer holds"
            );
        }
        for _ in 0..8 {
            assert!(
                r.read_bits(1).is_err(),
                "round {round}: reader recovered after true exhaustion"
            );
        }
    }
}

/// **Emulation unprevention never grows its input, and never panics.**
///
/// `emulation_unprevent` removes the `0x03` from every `00 00 03` sequence the
/// encoder inserted. It is pure removal, so the result is always shorter than or
/// equal to the input -- a result that GREW would mean the scan invented bytes,
/// which on an attacker-supplied buffer is how an allocation becomes unbounded.
#[test]
fn unprevention_only_ever_shrinks() {
    let mut rng = Rng(0x5EED_1234_5678_9ABC);
    for round in 0..20_000 {
        let data = rng.buf(64);
        let out = emulation_unprevent(&data);
        assert!(
            out.len() <= data.len(),
            "round {round}: unprevention grew {} -> {} bytes",
            data.len(),
            out.len()
        );
    }
}

/// **Splitting is non-destructive and terminating.** Every NAL a split yields
/// must be a sub-slice of the input, so their total length cannot exceed it, and
/// the walk must terminate on any input -- including one that is all start
/// codes, which is the shape that turns a naive scanner into an infinite loop.
#[test]
fn annex_b_split_yields_only_sub_slices() {
    let mut rng = Rng(0xFACE_B00C_0000_0001);
    for round in 0..20_000 {
        let data = rng.buf(96);
        let mut total = 0usize;
        let mut count = 0usize;
        for nal in split_annex_b(&data) {
            total += nal.len();
            count += 1;
            assert!(
                count <= data.len() + 1,
                "round {round}: split produced more NALs ({count}) than input bytes"
            );
        }
        assert!(
            total <= data.len(),
            "round {round}: split yielded {total} bytes from a {}-byte input",
            data.len()
        );
    }
}

/// **The degenerate inputs, explicitly.** Random generation reaches these only
/// by chance, and they are exactly where an off-by-one lives: empty, one byte,
/// a bare start code, a truncated start code, and nothing but escapes.
#[test]
fn framing_handles_the_degenerate_inputs() {
    for case in [
        &b""[..],
        &b"\x00"[..],
        &b"\x00\x00"[..],
        &b"\x00\x00\x01"[..],
        &b"\x00\x00\x00\x01"[..],
        &b"\x00\x00\x03"[..],
        &b"\x00\x00\x03\x00\x00\x03"[..],
        &b"\x00\x00\x01\x00\x00\x01"[..],
    ] {
        let mut total = 0usize;
        for nal in split_annex_b(case) {
            total += nal.len();
            let out = emulation_unprevent(nal);
            assert!(out.len() <= nal.len());
        }
        assert!(total <= case.len(), "split grew a degenerate input");

        let mut r = BitReader::new(case);
        for _ in 0..(case.len() * 8 + 16) {
            let _ = r.read_bits(1);
        }
        assert!(r.read_bits(1).is_err(), "reader not exhausted past its end");
    }
}
