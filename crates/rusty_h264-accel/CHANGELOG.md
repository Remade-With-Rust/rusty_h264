# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.2](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-accel-v0.17.1...rusty_h264-accel-v0.17.2) - 2026-11-09

### Other

- *(decoder)* 44 byte-identical instruction-count wins across 15 veins

## [0.17.1](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-accel-v0.17.0...rusty_h264-accel-v0.17.1) - 2026-11-09

### Other

- Merge origin/main (v0.17.0, decoder fixes #24-#26) into perf/encoder-ir-veins
- accel V15-2: exact SSD kernels; mb_ssd (the RD trials' distortion) uses them
- accel V10-3: band Hadamard's last stage as 2*max (exact half-sums)
- accel V10-2: band SATD carries one accumulator and halves once
- accel V10-1: single-block SIMD 4x4 SATD (was scalar)
- enc V2-3: macroblock variance moments from an SSE2/NEON kernel
- enc V7-3: SatdGrid -- the rescue grid computes kernel values a row at a time
- enc V6-3: a fused pavgb+psadbw kernel for the quarter-pel SAD
- enc V6-1: an 8x8 SAD kernel arm for sad_strided

## [0.17.0](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-accel-v0.16.0...rusty_h264-accel-v0.17.0) - 2026-10-28

### Other

- overflow-checks cost 22.5% of decode — moved to release-checked
- audit pass 2 — 6% to 89% of in-scope gates, every unit >=85%
- close H-03/04/05/07/13/14/15/16/31/39 + threat model
- use-protection-please first pass — 41 gates x 8 units, depth=deep
- an intra record is one boolean, and the filter stops touching a TLS
- pack a uniform macroblock from one block, not sixteen
- one motion test per macroblock edge when both sides are uniform
