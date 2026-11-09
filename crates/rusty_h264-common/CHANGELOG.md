# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.0](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-common-v0.16.0...rusty_h264-common-v0.17.0) - 2026-10-28

### Other

- *(miri)* make the H-23 gate finish — fix one real test bug, skip the sweeps
- make main green — fix the root cause behind each failing gate
- bump rusty_alloc-api to =2.2.6 ([#19](https://github.com/Remade-With-Rust/rusty_h264/pull/19))
- bump rusty_alloc-api to =2.2.5 ([#17](https://github.com/Remade-With-Rust/rusty_h264/pull/17))
- 87% of every allocation was one Box::new that skipped its own pool
- narrow the motion grids to (i16,i16) - 2.90 GB of pool clear removed
- narrow the reference-index grids i32 -> i8 (-103 MB of memset)
- overflow-checks cost 22.5% of decode — moved to release-checked
- audit pass 2 — 6% to 89% of in-scope gates, every unit >=85%
- close H-03/04/05/07/13/14/15/16/31/39 + threat model
- use-protection-please first pass — 41 gates x 8 units, depth=deep
- put the 8x8-transform flag in the intra table, so the filter stops asking
- record one more refutation on the derivation's argument list
- move the derivation's output pointer into a register argument
- build the packed record where it lives (spills -72%)
- add a spill column to the row-hook census, and record what it settled
- outline the per-lane edge arms, now the minority case
- the filter keeps boundary strengths at byte width
- an intra record is one boolean, and the filter stops touching a TLS
- coded flags a word at a time, and a provable kind row
- pack a uniform macroblock from one block, not sixteen
- one motion test per macroblock edge when both sides are uniform
- take the uniformity answer from mb_kind instead of recomputing it
- fold the paired motion tests, share the edge nnz mask, split the hot prefix
- derive boundary strengths at u8, not i32 (-223 instrs on the decoder's derivation path)
