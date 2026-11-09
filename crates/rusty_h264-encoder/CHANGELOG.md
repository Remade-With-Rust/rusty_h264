# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.2](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-encoder-v0.17.1...rusty_h264-encoder-v0.17.2) - 2026-11-09

### Other

- release v0.17.2

## [0.17.1](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-encoder-v0.17.0...rusty_h264-encoder-v0.17.1) - 2026-11-09

### Other

- encoder speed against 0.17.0 -- all three presets, CPU time and Ir
- drop needless borrows in the B drivers (clippy -D warnings)
- release the plane pools when an encode finishes
- Merge origin/main (v0.17.0, decoder fixes #24-#26) into perf/encoder-ir-veins
- enc V15-3: mb-tree intra cost from the SATD and SAD kernels
- accel V15-2: exact SSD kernels; mb_ssd (the RD trials' distortion) uses them
- enc V15-1: the frame probes' 16x16 SAD runs on the accel kernel
- enc V14-2: the trial rate model walks nonzero levels only
- enc V14-1: the RD snapshot (MbState) as fixed arrays
- enc V13-3: half-pel planes rebuilt into a retired picture's planes
- enc V13-2: reconstruction planes recycled from retired pictures, not re-zeroed
- enc V9-3: significance map coded from the bitmask, split at the last coefficient
- enc V9-2: residual levels visit only the nonzero positions
- enc V9-1: residual blocks build a nonzero bitmask once (popcnt + lzcnt)
- enc V5-3: CABAC writes four bytes per slow-path entry; carries ripple back
- enc V5-2: one packed byte per CABAC context; slow path as a tail call
- enc V5-1: byte-wise CABAC interval coder (x264 low/queue form)
- enc V3-3: lam_r is resolved once per motion search, not per evaluation
- enc V3-2: one bounds-free table for every mvbits cost kind
- enc V3-1: mc_sad prices every interior shape through the SAD kernels
- enc V2-3: macroblock variance moments from an SSE2/NEON kernel
- enc V2-2: the me_wide coherence probe abandons losing shifts too
- enc V2-1: the B-decision global-motion search abandons losing shifts
- enc V1-3: the B bi-average runs pavgb at the implicit weights' midpoint
- enc V1-2: a spatial-direct MB with uniform motion is one 16x16 MC per list
- enc V1-1: spatial direct does one MC per 8x8 quadrant, not per 4x4
- enc V7-3: SatdGrid -- the rescue grid computes kernel values a row at a time
- enc V7-2: separable rate tables for the rescue grid
- enc V7-1: the me_wide rescue grid batches every P partition shape
- enc V6-3: a fused pavgb+psadbw kernel for the quarter-pel SAD
- enc V6-2: the 16x16 SAD reads the source plane in place
- enc V6-1: an 8x8 SAD kernel arm for sad_strided
- enc V11-3: the B 16x16 arm takes the drivers' mvd predictors
- enc V11-2: b_direct takes the neighbour sets the B drivers already built
- enc V11-1: the B_Direct arm reuses the driver's direct prediction
- enc V12-3: one RFF_POLYTIER read per frame, cached RFF_ME_COH_DBG
- enc V12-2: resolve the four B-slice env overrides once per process
- enc V12-1: cache the UNSET RFF_SUBPEL_PAT, not just the set one

## [0.17.0](https://github.com/Remade-With-Rust/rusty_h264/compare/rusty_h264-encoder-v0.16.0...rusty_h264-encoder-v0.17.0) - 2026-10-28

### Fixed

- the profile census compiles again, and the decoder builds no_std ([#20](https://github.com/Remade-With-Rust/rusty_h264/pull/20))

### Other

- make main green — fix the root cause behind each failing gate
- narrow the motion grids to (i16,i16) - 2.90 GB of pool clear removed
- four more, from the encoder and the PPS-gated grids
- narrow the reference-index grids i32 -> i8 (-103 MB of memset)
- overflow-checks cost 22.5% of decode — moved to release-checked
- audit pass 2 — 6% to 89% of in-scope gates, every unit >=85%
- close H-03/04/05/07/13/14/15/16/31/39 + threat model
- use-protection-please first pass — 41 gates x 8 units, depth=deep
