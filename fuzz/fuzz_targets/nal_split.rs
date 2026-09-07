//! The byte layer BELOW the decoder: Annex-B framing and emulation prevention.
//!
//! `split_annex_b` and `emulation_unprevent` are the first code any untrusted
//! byte touches, and they are pure slice arithmetic over adversarial input --
//! start-code scanning, and removing the `00 00 03` sequences the encoder
//! inserted. Off-by-one country.
//!
//! Fuzzed separately from `decode` because a bug here is reachable through every
//! entry point at once, and because a target this narrow gets far more
//! executions per second than one that runs a whole decode.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};

fuzz_target!(|data: &[u8]| {
    for nal in split_annex_b(data) {
        let _ = emulation_unprevent(nal);
    }
});
