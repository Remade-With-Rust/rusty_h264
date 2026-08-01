# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.6.0] - 2026-08-01

### ⚠️ The default encoder output changed (B-frames only)

Minor rather than patch, for the same reason 0.3.0 was: encoding with stock settings
produces a **different bitstream** than 0.5.x, with no change on your side. B-frame
macroblocks may now use **16x8 / 8x16 partitions** (`tune_b_split`, default ON). Set
`EncoderConfig::tune_b_split = false` for the old bytes. P-only and all-intra output is
unchanged.

`EncoderConfig` gained the public field `tune_b_split`, and `rusty_h264_decoder`
exports `split_access_units` — `Decoder::decode` takes ONE access unit, so a caller that
wants decode-order pictures, or wants to drop each picture instead of accumulating the
stream, now has a supported way to feed it.

> **Note on 0.4.x / 0.5.x:** those releases shipped without changelog entries. This entry
> covers only the work since 0.5.1; the gap is acknowledged rather than reconstructed.

### Fixed — decoder conformance

- **Spatial direct ignored `direct_8x8_inference_flag`.** It read the co-located block at
  its own 4x4 coordinates while the temporal path correctly mapped the 8x8 corner
  (spec 8.4.1.2.1 defines the co-located block once, for both direct modes). Invisible on
  every stream the gate had ever produced, because x264's default partition set has an
  8x8 minimum, so all four 4x4s of a co-located 8x8 carry identical motion. Turn on
  sub-8x8 P partitions (`--partitions all`) and they differ, and B direct mode
  reconstructed from the wrong vector. Found by benchmarking decode of x264
  `--preset slower` streams — a configuration no conformance arm had ever generated.
- The decoder conformance gate gained crossed `sub8` / `sub8pyr` arms: **120 -> 160
  configs**. Both were verified to go RED without the fix.

### Added — encoder: B 16x8 / 8x16 partitions, default ON

x264 spends 13.5% of its B macroblocks on these; we had none. Each half reuses the
existing 16x16 motion search for L0/L1 and prices Bi through a rect-shaped `bi_dist_rect`;
the 9 (p0,p1) pairings are compared against the 16x16 winner. 4-QP per-clip BD (SSIM):
akiyo -0.17%, FourPeople -0.80%, tempete -1.66%, mobile -3.36%, foreman -3.49%,
bus -4.56%, football -7.09%. **Every clip wins on both metrics with no sign flip**, so
there is nothing to dispatch on. Cost ~+17% encode wall on B-heavy content; dominant
(fewer bytes AND higher PSNR at matched QP) on the Fast preset too.

### Changed — decoder speed on real bitstreams

Three byte-identical bricks, all sibling-path parity gaps where the encoder had the fix
and the decoder did not:

- the vendored openh264 `PixelAvgWidthEq16/8/4` asm is now wired into the QUARTER-pel
  average (it was in the objects, never declared, never called) — 16x16 quarter-pel
  1432 -> 762 cycles/call;
- const-width re-stride of motion-compensated output, 4 sites — that stage fell 84%;
- const-width full-pel MC copy — total MC cycles -29%.

Measured against ffmpeg on x264 streams (1800 frames 720p, pinned CPU time, frame-count
parity, arms interleaved): CAVLC **6.12x -> ~4.8x**, CABAC **6.03x -> 5.67x**.

### Added — benchmarks and instruments

- `bench/decode_x264_speedtest.sh` — decode speed on x264-ENCODED streams. Every prior
  decode number was taken on our OWN encoder's bitstreams, which a new MC census showed
  are **100.0% full-pel** (our fast preset is integer-pel only) — so every decode
  benchmark ever run had skipped the entire interpolation path. The gap read 1.65-3.69x
  on our streams and 6.03-6.12x on x264's.
- `bench/x264_headtohead.ps1` — encoder rate/quality/speed Pareto vs x264.
- `bench/decode_speedtest.sh` rebuilt on CPU time, long streams and a frame-count parity
  check that VOIDS the comparison on mismatch. The old differential subtracted two
  numbers of the same size and returned 202, 391, 176, NEGATIVE and 330 Mpx/s for
  identical work.
- `examples/decode_bench.rs` — output-free in-process decode harness (the CLI's output
  path was 38% of what the old benchmark called "decode"), printing the frame count, the
  rep spread, the full stage table and the MC size x phase census.
- Decoder profiler fixes: `InterMc` was scoped on `mc_luma`/`mc_chroma` while the decoder
  calls the `_padded` twins, so motion compensation read **0.0 ms / 0 calls**;
  `DecSetup` and `Dequant` were declared in the `Stage` enum and scoped nowhere.
- `RFF_ABL_DEBLOCK` / `RFF_ABL_DBKERNEL` ablation knobs: deblocking is 31.1% of decode
  and only **5.6%** of that is the SIMD kernels — 25.5% is boundary-strength derivation.


## [0.3.0] - 2026-07-29

### ⚠️ The default encoder output changed

This is why the minor version moves rather than the patch. Encoding with stock
settings now produces a **different bitstream** than 0.2.x did, without any code
change on your side: **CABAC is default-on**, so the **profile default moves from
Constrained Baseline to Main**, and **adaptive quantization is default-on**. If you
need the old bytes — a Baseline-only downstream decoder, or a bisection anchor — set
`RUSTY_H264_LEGACY_CAVLC=1`, which restores the 0.2.x defaults byte-for-byte.

`EncoderConfig` also gained a large number of public fields and `Preset` gained the
`Balanced` variant (now the `Preset` default).

The encoder grew the tools Constrained Baseline forbids. Everything below is gated the
same way: bitstream-changing work on **4-QP BD-rate per clip, worst clip ≤ 0** (never a
mean, never a single QP), speed work **byte-identical**, and all of it round-tripped
through our own decoder plus decoded pixel-exact by ffmpeg.

### Added — CABAC entropy *encoding* (Main profile), default-on

I, P and B slices, sharing the context tables with the decoder via `rusty_h264-common`
and round-trip-validated against the decoder's own arithmetic engine. Measured
**−8.8…−9.0% BD-rate for 1.10–1.22× the time** on the 4-QP corpus — better value than
any preset step in either encoder — so it ships on by default, and the profile default
moves to Main with it. `RUSTY_H264_LEGACY_CAVLC=1` restores the exact prior defaults
(Constrained Baseline + CAVLC) byte-for-byte as an escape hatch and bisection anchor.
Trellis RDOQ is default-on for all-intra (−0.5…−1.3%), including on the asm path.
Multi-reference P coding (`ref_idx_l0`) lifts the former 1-ref cap on both sides.

### Added — B-frames, content-adaptively enabled

A conformant B pipeline: frame reorder, bi-directional ME, per-list MV prediction,
`B_L0`/`B_L1`/`B_Bi` partitions, `B_Direct_16x16` and `B_Skip` via spatial direct, and
implicit weighted bi-prediction. B-frames are strongly content-dependent (−19.6% on
smooth motion, +3.6% on busy content), so `--bframes auto` measures the clip's temporal
predictability per GOP and codes them **only** where they help — capturing the win with
no regression. The per-GOP B-frame QP offset is adaptive on the same signal.

### Added — adaptive quantization (default-on) and mb-tree temporal AQ (opt-in)

**Spatial AQ** modulates per-macroblock QP by content — finer on flat regions where
blocking and banding show, coarser where the eye masks error — relative to the frame's
mean log-variance, rate-compensated. Its effective strength backs off automatically
where the log-variance spread is pathological, which is what took it from opt-in to
regressing nowhere, hence default-on.

**mb-tree** is the temporal complement: a lookahead pass propagates future-reference
importance backward along motion vectors and lowers the QP of heavily-referenced
macroblocks. A predictability back-off keeps it from regressing (−1.8% on one clip,
−0.2% on another, neutral on a third). Three lookahead resolutions trade speed for
accuracy: `FullRes`, `Hybrid` (half-res search, full-res score — ~1.7× for no measured
loss) and `HalfRes` (~4×, the default).

### Added — the 8×8 transform in the encoder (High profile, opt-in)

`I_8x8` intra and the inter `transform_size_8x8_flag`, as a 3-way per-macroblock RD
choice. A level-aware rate estimate plus a penalty term makes the content-adaptive
dispatch a win on every corpus clip. Scalar and asm paths are byte-identical.

### Added — `P_8x8` sub-partition motion and the adaptive wide search

Both default-on for the `Quality` preset, both content-adaptively gated:

- **`P_8x8`** — four 8×8 partitions with their own MVs, a per-MB RD choice against
  16×16/16×8/8×16. Net win on real content (12-clip Derf corpus: −0.23% mean BD, large
  wins on bus/mobile/flower); a 6-channel discovery harvest proved no cheap gate beats
  default-on, with only 0.18% oracle headroom left.
- **`me_wide`** — the gradient-descent diamond *stalls on flat cost surfaces* and misses
  the true MV, which was the root cause of the ~22% P-16×16 inefficiency on smooth
  content. Flat blocks now get a ±16 grid search instead; a per-frame coherence gate
  and an online rescue-payoff gate keep it from regressing even on pure pans.

### Performance — the motion-estimation campaign

Motion estimation was measured at **81% of the wall-clock gap vs x264**, and as a
**per-call** problem (1.68 µs/search vs 0.16 µs) rather than a call-count one. Landed
byte-identical: a fused avg+SATD custom kernel, a fused single-pass half-pel builder,
sub-pel ring memoization (−27% cost evaluations), edge full-pel served from the padded
plane, const-specialized full-pel copies (skip-MC 3.4×), AVX2 dispatch for the
DCT/quant/IDCT/SATD kernels (+12% quality core), and a streaming per-GOP CLI pipeline.
Bitstream-changing motion work (fixed-centre diamond, `sad_16x16_x4`/`satd_16x16_x4`
batch kernels, sub-pel iteration budget) ships behind a content dispatcher tuned so
every corpus loss is zeroed.

### Changed

- The CLI defaults to a 1-second P-frame GOP instead of all-intra, and encodes fully
  streaming (no whole-file buffers).
- New CLI flags: `--cabac`, `--cabac-init`/`--cabac-lambda`/`--cabac-dz`/`--cabac-rdoq`,
  `--bframes N|auto`, `--iqp-offset`, `--bqp-offset`, `--aq`, `--transform-8x8`,
  `--sub8x8`, `--me-wide`, `--mbtree`/`--mbtree-strength`/`--mbtree-lookahead`.
  Note the CLI defaults CABAC **off** (a bare `encode` stays Baseline+CAVLC) while
  `EncoderConfig` defaults it **on**.

### Documentation

Every crate now ships its own README — the crates.io and docs.rs profiles were bare —
and the `-common`, `-encoder` and `-decoder` crates gained the keywords, categories and
accurate descriptions they were published without.

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
