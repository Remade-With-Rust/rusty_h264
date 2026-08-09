//! # rusty_h264
//!
//! A ground-up, **pure-Rust** H.264 codec — a *Remade With Rust* rebuild of
//! Cisco's [openh264](https://github.com/cisco/openh264). Unlike the FFI
//! bindings in `openh264-rs`, there is no C in the dependency tree: the codec
//! core is `#![forbid(unsafe_code)]`, BSD-2 licensed, and embeddable anywhere.
//!
//! The **encoder** produces compressed Constrained Baseline streams (intra
//! `I_16x16`/`I_4x4`/`I_PCM`, inter P-frames with quarter-pel motion
//! compensation, in-loop deblocking, and rate control) that decode bit-exactly
//! under reference decoders. The **decoder** handles the full Constrained
//! Baseline subset and is validated bit-exact against Cisco's `h264dec`.
//!
//! This facade re-exports the encoder, decoder, and shared types so downstream
//! users depend on a single crate.
//!
//! ## Decoding a whole stream
//!
//! [`Decoder::decode_stream`] is the one-call entry point — it splits access
//! units, assembles multi-slice pictures, and returns frames in **display order**:
//!
//! ```
//! use rusty_h264::{Encoder, EncoderConfig, Decoder, YuvFrame};
//!
//! // Encode three frames.
//! let mut enc = Encoder::new(EncoderConfig::new(32, 32)).unwrap();
//! let mut stream = Vec::new();
//! for _ in 0..3 {
//!     stream.extend_from_slice(&enc.encode(&YuvFrame::black(32, 32)));
//! }
//!
//! let frames = Decoder::new().decode_stream(&stream).unwrap();
//! assert_eq!(frames.len(), 3);
//! assert_eq!((frames[0].width, frames[0].height), (32, 32));
//! ```
//!
//! For streaming use, the lower-level [`Decoder::decode`] returns one picture per
//! access unit in decode order (pair it with [`Decoder::last_poc`] to reorder).

pub use rusty_h264_common::{ChromaFormat, NalUnit, NalUnitType, Profile, YuvFrame};
pub use rusty_h264_decoder::{DecodeError, Decoder};
#[cfg(feature = "prometheus-telemetry")]
pub use rusty_h264_encoder::prometheus_telemetry;
pub use rusty_h264_encoder::bitacct;
pub use rusty_h264_encoder::{EncodeError, Encoder, EncoderConfig, LookaheadMode, Preset};
/// Gate-regression instruments (Great Gate P4 — see `bench/examples/gatecheck.rs`):
/// the fire-rate census and the deterministic work counts every gate verdict
/// must report alongside its quality number (the dual-verdict law).
pub use rusty_h264_encoder::{
    diastats_reset, diastats_snapshot, gate_census, gate_census_by_t8, gate_census_dump_csv,
    gate_census_names, gate_census_reset, gate_work, gate_work_names, gopstats,
    temporal_decay_ratio,
};

/// The crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// B-slice mode census (`RFF_BSTATS=1`). Re-exported so the CLI can print it on the
/// same flags a comparison is being judged on.
pub fn bstats_dump() {
    rusty_h264_encoder::mb16::bstats::dump();
}
