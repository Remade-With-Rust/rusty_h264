//! The MULTI-PICTURE target, which is where the deep state lives.
//!
//! `decode` handles one call; `decode_stream` walks a whole Annex-B sequence and
//! therefore exercises everything that spans pictures and is impossible to reach
//! with a single NAL: reference-list construction, MMCO surgery on the DPB, POC
//! arithmetic across an IDR boundary, B-pyramids, and multi-slice pictures whose
//! slices carry DIFFERENT reference lists.
//!
//! That last one is not hypothetical. On 2026-09-07 a refactor that gave each
//! picture a frame-interned reference id broke exactly there, and the thing that
//! caught it was `tests/mslice_idc2.rs` -- a fixture that happened to exist.
//! Coverage-guided fuzzing over this entry point is how that class gets found
//! without needing someone to have written the right fixture first.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_h264_decoder::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut d = Decoder::new();
    let _ = d.decode_stream(data);
});
