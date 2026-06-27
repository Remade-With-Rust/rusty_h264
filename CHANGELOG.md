# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [0.2.0]

First public release on [crates.io](https://crates.io/crates/rusty_h264).

### Added

- **Decoder**: Constrained Baseline + B-slices (temporal/spatial direct, implicit
  & explicit weighted prediction) + most of High profile over CAVLC (8×8 transform
  & intra, scaling lists). Bit-exact vs Cisco `h264dec` on the clean corpus.
- **Encoder**: Constrained Baseline — intra, P-frames (`P_Skip`/16×16/16×8/8×16),
  quarter-pel motion compensation, rate-aware motion estimation, multi-reference
  DPB, ABR rate control. Bit-exact vs ffmpeg across QP 0–51.
- **CABAC** arithmetic-decoding engine + 460-context initialization (round-trip
  verified); per-syntax parsing is the next milestone.
- **`asm` feature (on by default)**: openh264's BSD-2 SIMD kernels (motion
  compensation, deblocking, transforms), **vendored** into `rusty_h264-accel` so
  the build is self-contained — only `nasm` is required. The `build.rs` is
  target-aware (win64 / macho64 / elf64) and degrades gracefully when `nasm` is
  absent. Build `--no-default-features` for 100%-safe, portable, nasm-free Rust.

### Notes

- The codec crates (`-common`, `-encoder`, `-decoder`, and the `rusty_h264`
  facade) are `#![forbid(unsafe_code)]`. All `unsafe` is quarantined in the
  optional `rusty_h264-accel` crate (asm FFI).
- Bitstream format is Annex-B (start codes).
