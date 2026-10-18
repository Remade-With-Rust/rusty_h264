# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.0](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-encoder-v0.16.0...rusty_h264-encoder-v0.17.0) - 2026-10-18

### Fixed

- the profile census compiles again, and the decoder builds no_std ([#20](https://github.com/Remade-With-Rust/rusty_h264/pull/20))

### Other

- narrow the motion grids to (i16,i16) - 2.90 GB of pool clear removed
- four more, from the encoder and the PPS-gated grids
- narrow the reference-index grids i32 -> i8 (-103 MB of memset)
- overflow-checks cost 22.5% of decode — moved to release-checked
- audit pass 2 — 6% to 89% of in-scope gates, every unit >=85%
- close H-03/04/05/07/13/14/15/16/31/39 + threat model
- use-protection-please first pass — 41 gates x 8 units, depth=deep
