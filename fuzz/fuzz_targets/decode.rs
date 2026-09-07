//! The primary target: ONE call to the decoder's single untrusted entry point.
//!
//! `Decoder::decode` takes an Annex-B byte string an attacker chooses. The
//! contract this target enforces is the whole security posture of the crate:
//!
//!   * it must return `Ok(_)` or `Err(DecodeError)` -- **never** unwind, since
//!     the decoder is `#![forbid(unsafe_code)]` and carries no production
//!     `.unwrap()`, so a panic is a bug and a denial-of-service primitive;
//!   * it must not hang or allocate without bound.
//!
//! libFuzzer treats a panic or an abort as a crash, so no assertion is needed
//! here -- the absence of one is the point.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_h264_decoder::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut d = Decoder::new();
    let _ = d.decode(data);
});
