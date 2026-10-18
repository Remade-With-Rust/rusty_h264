# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.0](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-decoder-v0.16.0...rusty_h264-decoder-v0.17.0) - 2026-10-18

### Fixed

- the profile census compiles again, and the decoder builds no_std ([#20](https://github.com/Remade-With-Rust/rusty_h264/pull/20))

### Other

- 87% of every allocation was one Box::new that skipped its own pool
- the plane recycle pool was never fed - 4.34 GB of wasted zeroing
- narrow the motion grids to (i16,i16) - 2.90 GB of pool clear removed
- the List-1 grids are B-only, so stop clearing them without B (-36%)
- four more, from the encoder and the PPS-gated grids
- the mvd cache stores what the context can read, not what was parsed
- second pass of the copy hunt — the census's own two blind spots
- carry the POC lookup table, not its per-block expansion (-37 MB)
- narrow the reference-index grids i32 -> i8 (-103 MB of memset)
- overflow-checks cost 22.5% of decode — moved to release-checked
- audit pass 2 — 6% to 89% of in-scope gates, every unit >=85%
- close H-03/04/05/07/13/14/15/16/31/39 + threat model
- use-protection-please first pass — 41 gates x 8 units, depth=deep
- no production panic sites — 47 to 0 (H-18)
- put the 8x8-transform flag in the intra table, so the filter stops asking
- move the derivation's output pointer into a register argument
- build the packed record where it lives (spills -72%)
- mark single-partition B macroblocks uniform (mb_uniform now -86..-96%)
- coded flags a word at a time, and a provable kind row
- pack a uniform macroblock from one block, not sixteen
- mark the B classes whose motion the decoder already knows is uniform
- one motion test per macroblock edge when both sides are uniform
- take the uniformity answer from mb_kind instead of recomputing it
- derive boundary strengths at u8, not i32 (-223 instrs on the decoder's derivation path)
