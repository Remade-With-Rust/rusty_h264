# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.1] - 2026-07-01

### Added — CABAC entropy decode (Main profile)

The CABAC arithmetic decoder (whose engine landed in 0.2.0) now drives a full per-syntax
macroblock parse, brought up symbol-by-symbol against an instrumented openh264 oracle and
gated **pixel-exact vs ffmpeg**:

- **I slices** — I_4x4 and I_16x16 (all four 16×16 intra modes, luma DC + AC).
- **P slices** — `P_Skip`, all partition types (16×16 / 16×8 / 8×16 / 8×8 + sub-types),
  mvd, motion compensation, residual.
- **B slices** — `B_Skip`, `B_Direct_16x16`, L0/L1/Bi 16×16/16×8/8×16, `B_8x8` with
  per-sub-partition direction, spatial + temporal direct.

Baseline/Main-profile I + P + B streams decode fully pixel-exact end to end. (Not yet:
CABAC I_PCM — errors gracefully today; High-profile 8×8 CABAC residual.)

### Security — decoder is panic- and hang-proof on hostile input

Fuzzing the CABAC paths (unreachable from our CAVLC-only encoder, so previously unfuzzed)
fixed three DoS-class bugs on malformed input, all regression-gated:

- an **infinite `cabac_unary` loop** (the arithmetic engine zero-fills past EOF and keeps
  yielding 1-bins → no terminator),
- a **`cabac_init_idc` out-of-bounds** context-table index (panic on a spec-out-of-range
  value parsed as unbounded `ue`),
- an **unbounded frame-num-gap allocation** (one full frame per missing `frame_num`; also
  bounded `log2_max_frame_num` / `log2_max_pic_order_cnt_lsb`).

The mutation fuzzer now carries committed CABAC seeds covering every MB type and runs
thousands of mutations per seed with **zero panics and zero hangs**.

### Fixed

- **Builds on non-x86_64 targets (e.g. arm64 macOS) with the default `asm` feature.**
  The optional openh264 SIMD kernels (`rusty_h264-accel`) are x86-64-only. They are now
  gated on `target_arch = "x86_64"`: the accel crate compiles to an empty lib and its
  build script never invokes `nasm` (nor links x86 objects) off x86-64, and the
  encoder/decoder/common crates fall back to their pure-Rust scalar path via a new
  internal `accel` cfg (= `asm` feature **and** x86-64). Downstream crates that enable
  `asm` by default (e.g. `rff`'s `h264-asm`) now build unchanged on Apple Silicon. SIMD
  on x86-64 is unaffected — `accel` there is exactly the old `asm`-feature path.

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
