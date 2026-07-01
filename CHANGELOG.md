# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Performance — decoder + encoder speed upgrade (bit-exact; no API or bitstream change)

A profiling-driven pass built accurate instrumentation and then a series of wins, every
one gated **byte-identical** against the reference (decode) / prior output (encode):

- **Decoder ~1.5× faster** (1080p, single core): **~94 → ~145 Mpx/s** with the asm
  kernels; **~109 Mpx/s** in 100%-safe pure Rust. Wins are redundancy elimination in the
  pure-Rust glue, not new asm: skip B-only motion/ref work on Baseline streams
  (`+12%`), move-not-clone the DPB reference frame + drop a redundant second plane clone
  (`finalize 9.6 → 6.1 ms`), pass the deblock filter the empty grids it doesn't use.
- **Encoder — asm SATD wired into the quality-preset mode decision**: openh264's
  `WelsSampleSatd` kernels now drive the SATD cost (`2·WelsSampleSatd`, **byte-identical**
  — `Σ|H·d|` is always even so the kernel's `(Σ+1)>>1` × 2 recovers it exactly), taking
  **quality inter encode 1.7×** faster and quality intra ~1.1×. The default *fast* preset
  is unchanged (it uses SAD, which already auto-vectorizes to `psadbw`).

### Added (tooling)

- `bench/decode_speedtest.sh` — reproducible decode throughput vs ffmpeg's native
  `h264` software decoder (differential, best-of-3, single core).
- An `rdtsc`-based stage profiler + `profile_decode_meticulous` / dual-preset
  `profile_encode` benchmarks (behind the `profile` feature; zero cost when off).

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
