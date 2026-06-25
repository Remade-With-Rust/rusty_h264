//! # rusty_h264
//!
//! A ground-up, **pure-Rust** H.264 codec — a *Remade With Rust* rebuild of
//! Cisco's [openh264](https://github.com/cisco/openh264). Unlike the FFI
//! bindings in `openh264-rs`, there is no C in the dependency tree: the codec
//! core is `#![forbid(unsafe_code)]`, BSD-2 licensed, and embeddable anywhere.
//!
//! The encoder produces compressed, all-intra (`I_16x16`) Constrained Baseline
//! streams whose output is bit-exactly decodable by reference decoders (verified
//! against ffmpeg).
//!
//! This facade re-exports the encoder, decoder, and shared types so downstream
//! users depend on a single crate.
//!
//! ```
//! use rusty_h264::{Encoder, EncoderConfig, Decoder, YuvFrame};
//!
//! let mut enc = Encoder::new(EncoderConfig::new(32, 32)).unwrap();
//! let frame = YuvFrame::black(32, 32);
//! let bitstream = enc.encode(&frame);
//!
//! let mut dec = Decoder::new();
//! let decoded = dec.decode(&bitstream).unwrap().unwrap();
//! assert_eq!((decoded.width, decoded.height), (32, 32));
//! ```

pub use rusty_h264_common::{ChromaFormat, NalUnit, NalUnitType, Profile, YuvFrame};
pub use rusty_h264_decoder::{DecodeError, Decoder};
pub use rusty_h264_encoder::{EncodeError, Encoder, EncoderConfig, Preset};

/// The crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
