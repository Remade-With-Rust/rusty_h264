# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added — `no_std` + `alloc` for `rusty_h264-common` and `rusty_h264-encoder`

The encoder now builds for bare-metal targets (checked on
`riscv32imac-unknown-none-elf`, the ESP32-C6 class, and `thumbv7em-none-eabihf`
in CI). The ladder is the `rusty_zstd` / `rusty_flac` one:

- `std` (default) — parallel GOP encoding (`encode_all`, `RUSTY_THREADS`), the
  `RS_H264_*` / `RFF_*` environment knobs, the stderr censuses and CSV/file
  harvest sinks, the stage profiler, per-thread recycled scratch.
- without `std` — `no_std` + `alloc`. A knob reads as unset (the shipped
  default), a print is a no-op, `encode_all` runs the GOPs in order, and the
  per-frame scratch is allocated per frame instead of recycled. **`libm` is
  required** without `std`: `f64::sqrt`, `powf`, `log2`, `exp2`, `floor`,
  `round` and friends are `std`-only inherent methods, and
  `rusty_h264_common::fmath::{F64Ext, F32Ext}` supplies them from the
  pure-Rust `libm` crate. With `std`, enabling `libm` too makes float-derived
  decisions bit-identical between a host and a chip (the platform libm is not
  guaranteed to agree with itself across machines).

New (optional / target-gated) dependencies on `rusty_h264-common`: `libm`
(feature `libm`), `once_cell` with only `race` + `alloc` (a `Sync` once-cell
for the lazily built tables and cached knobs without `std`), and
`portable-atomic` **only on targets without native 64-bit atomics** (the
diagnostic counters are `AtomicU64`). Public surface: `VlcTables::build()`;
`cavlc::vlc_tables()` and `decode_residual_block()` are now `std`-only
(without `std`, build the tables and use `decode_residual_block_with`).
`prof` and `prometheus-telemetry` imply `std`. The decoder is unchanged and
still `std`-only; the facade forwards `std` (default) and `libm`.

### Changed — `--no-default-features` no longer implies `std`

`default` on the codec crates and the facade is now `["std", "global-alloc",
"asm"]`. The pure-scalar arm is spelled `--no-default-features --features std`
(CI's `pure` job does); a plain `--no-default-features` is the `no_std`
configuration and needs `libm`. `signals::signal_probes_golden` is checked
only without `libm`: its golden hash was taken against the platform libm and
`libm` differs in the last bits by design.

## [0.12.0] - 2026-08-27

### Changed — `asm` (portable SIMD) is now a DEFAULT feature on the codec crates

`rusty_h264-common`, `rusty_h264-encoder` and `rusty_h264-decoder` now enable
`asm` by default, matching the facade and CLI. A plain per-crate build, test
or bench measures the shipping SIMD arm; the pure-scalar arm is explicit:
`--no-default-features` (re-add `global-alloc` as needed). Downstream crates
that depended with `default-features = false` are unaffected. The workspace
manifest now declares the codec crates `default-features = false` in
`[workspace.dependencies]` — required, because Cargo ignores a member's
`default-features = false` on `workspace = true` dependencies.

### Added

- `rusty_h264_common::arms`: `simd_arms()` names the compiled+detected kernel
  arm at runtime; `active_knobs()` lists every live `RS_H264_*`/`RFF_*`
  measurement knob with its effect class. The CLI, the bench harness and
  `decode_bench` print them, so every log records which codec actually ran
  (a scalar build and an accel build are byte-identical — only this line
  tells them apart).
- SSE2 twins for the boundary-strength helpers `mb_uniform` and
  `bs_motion_masks`: on x86-64 the packed-bS fast arm no longer silently
  falls back to the scalar walk when AVX2 is absent (masked-AVX2 VMs,
  pre-Haswell CPUs). Gated by 50k-round AVX2==SSE2 differentials.
- CI: an arm64 job that EXECUTES the NEON differential suites on real
  hardware (previously aarch64 was compile-checked only).

### Fixed

- **Scalar (non-`asm`) builds decoded packed-bS-routed streams with chroma
  deblocking silently OFF** (bug present in 0.11.0; accel builds — including
  the CLI — were never affected). The scalar chroma loops in
  `filter_frame_rows` lacked the `pre_bs` branch the luma loops carry and
  read never-populated zero-init strength arrays. The scalar arm is now
  pixel-exact against ffmpeg on all presets and has its own standing
  conformance probe (`--no-default-features --example dectest`). This closes
  the long-filed "Main-profile chroma-deblock divergence" — the encoder was
  never at fault.
- `RS_H264_DOUBLE_RECON` (an ablation knob) triggered on the variable's mere
  presence, so `RS_H264_DOUBLE_RECON=0` doubled the reconstruction work.
  Now only `=1` enables it.
- The decoder crate carried an unused direct `rusty_h264-accel` dependency
  and cfg-emitting build script (all decoder kernel dispatch routes through
  `rusty_h264-common`); both removed.
- Facade/encoder doc examples and round-trip tests now call `flush()` — with
  the keyint-250 + mb-tree defaults, a single `encode()` legitimately buffers
  and returns no bytes until flushed.

### Changed — encoder: 20 byte-identical instruction wins

Across the five x264-parity areas (bframes / keyint / weightp / trellis /
b-pyramid), all output-identical: RDOQ float-divides 8→0 (qstep table + rate
LUT), `encode_all_bframes` integer divides removed, B-frame detector
preparation halved with a rolling lazy cursor, weighted-prediction estimator
6→1 passes over the current plane, mb-tree anchor-window deep clones replaced
by borrowed refs, flash-veto mean memoization, refine-centre SAD skip, and
scratch pooling in the reference-B tail.

## [0.11.0] - 2026-08-22

The **bounds-check** release. Every `panic_bounds_check` the decoder and the
shared kernels emit in their own code is gone -- 765 sites to **zero** -- and the
decode path got measurably faster doing it. Decode output is byte-identical to
ffmpeg on all 68 corpus streams at every step.

### Changed -- decode speed, from removing bounds checks

Against the previous tag, pinned CPU time, ABBA-alternated, both arms verified
byte-identical, cores-busy 0.91-0.99:

| stream                | ratio  | pairs | z     |
| --------------------- | ------ | ----- | ----- |
| all-intra CAVLC 720p   | 0.897x | 13/13 | -3.61 |
| crowd_run 1080p CAVLC  | 0.917x | 11/11 | -3.32 |
| crowd_run 1080p High   | 0.957x | 11/11 | -3.32 |
| crowd_run 1080p Main   | 0.962x | 11/11 | -3.32 |

46 of 46 paired wins. Against ffmpeg on the x264 720p corpus (9 pairs, 9/9,
z=3.00) the standing gap moves **2.10 / 1.99 / 1.91 -> 1.81 / 1.84 / 1.84** for
CAVLC / Main / High. All three now sit below the historical cross-run band
floor, which is what separates this from box state; ffmpeg's own absolute
numbers rose in the same run, so only the RATIO is comparable across sessions.

The tier ordering inverted: CAVLC was the worst tier and is now the best, which
is where the work landed (`Vlc::read`, `BitReader::read_bit`,
`decode_residual_block_with`, `reconstruct_4x4_*`). The remaining gap is now
uniform across tool tiers.

### Changed -- `rusty_alloc` 1.0.0 — `rusty_alloc` 1.0.0

The process-wide allocator dependency (`rusty_alloc-api`, installed by
`rusty_h264-common`'s default `global-alloc` feature) moves **0.3.2 -> 1.0.1**.
The safe surface we use is unchanged — `rusty_alloc_api::RustyAlloc` still
installs as the `#[global_allocator]` with no code change on our side. Verified:
workspace builds clean, all 22 test binaries pass, and a CLI encode/decode
round-trip decodes **byte-identically** in ffmpeg.

### Added -- `sim_sxs`, a headful side-by-side viewer

`cargo run -p rusty_h264-decoder --features asm --example sim_sxs -- main|high`
opens one window with our decode, ffmpeg's decode of the same stream, and a
third panel showing the pixel difference; `nowin` runs it headless and exits
non-zero on any mismatch. Reports PIXEL-IDENTICAL on 1260 frames of 720p50 at
both Main and High. `minifb` is a **dev-dependency**, so the published crate and
every consumer build are unaffected.

### Fixed -- two profiling harnesses that returned empty tables

`bench/route_shares.py` and `bench/glue_shares.py` both grepped STDOUT for a
`prof `-prefixed row format the binary has not emitted for some time (the
profiler writes to STDERR), and `glue_shares` additionally parsed a throughput
header that no longer exists, so every `calls/MB` read zero. Neither errored --
they produced EMPTY tables, which is the expensive shape of a stale instrument.
Both now parse the shipped format and take macroblock counts from the exact
`px=` counter. Added `bench/entropy_shares.py`, the entropy-side twin.

## [0.10.0] - 2026-08-13

The **pure-Rust** release: the codec no longer contains or requires assembly,
and every SIMD kernel family now covers x86-64 **and** aarch64. Also carries a
run of H.264 conformance fixes found by a syntax-layer audit — most notably
multi-slice CABAC decode, which was broken for every slice after the first.

### Removed — the last assembly, and the `nasm` build dependency

The rip-ASM campaign is **complete**: `crates/rusty_h264-accel/vendor/` (the
final 6 `.asm` files), `build.rs`, the `cc` build-dependency and every
`extern "C"` declaration are gone. Building the codec no longer requires
`nasm`, or any assembler, on any platform. `LICENSE.openh264` stays at the
accel crate root — the algorithms and tables remain openh264-derived (BSD-2);
only the assembly is gone. The `asm` feature keeps its (now historical) name
and gates the pure-Rust SIMD kernels.

Measured: the pure-Rust decoder vs the last asm-linked build is **1.004x
(z=-0.26, 15 interleaved pairs)** — no measurable decode cost. Encoder output
is byte-identical across CABAC / CAVLC / B-frame configurations.

### Added — SIMD on both ISAs for every kernel family

- **NEON** twins for deblocking (lt4/eq4 x luma/chroma x V/H incl. transposes),
  SATD (hadamard bands, 16x16/16x8/8x16/8x8) and the transform/quant trio.
  aarch64 previously compiled the accel crate to an empty lib and ran fully
  scalar; it now has SIMD for MC, SAD, SATD, deblock and transform/quant.
- **SSE2** twins for the transform/quant trio and the 16x16/8x8 intra
  predictors (the families that were still assembly).
- Every kernel keeps its scalar twin as a permanent oracle, pinned by
  differential tests over full-range inputs. x86-64 paths are test-executed;
  the aarch64 paths are compile-verified (zero warnings) and their tests run on
  the first ARM build.
- `bench/pgo.sh` — reproducible profile-guided-optimization build. Measured
  **-3.1% (high) / -5.3% (CAVLC)** whole-decoder, zero code changes.

### Fixed — conformance

- **Multi-slice CABAC decode.** `slice_first_mb` was never set on the CABAC
  path (the CAVLC twin set it), and context-neighbour availability ignored
  slice membership — so every slice after the first desynced. x264
  `--slices 4` streams are now byte-identical to ffmpeg.
- **CABAC I_PCM.** `decode_ipcm` existed but all three CABAC entry points
  refused with `Unsupported`; the engine byte-realign (`pcm_start_byte` /
  `reinit_at`, spec §9.3.1.2) is now wired and gated against ffmpeg.
- **`qpprime_y_zero_transform_bypass_flag`** was parsed and discarded —
  lossless-bypass macroblocks decoded silently wrong. Now a targeted refusal
  at the `step_qp` chokepoint (all-PCM lossless streams still decode).
- **`direct_8x8_inference_flag`** is now 1, as the spec requires at
  level >= 3.0 (every 720p+ stream we emit was formally non-conformant), with
  the encoder's direct derivation switched to corner-colZero in lockstep.
- **`disable_deblocking_filter_idc == 2`** (no filtering across slice
  boundaries) is implemented rather than collapsed to "on".
- **Intra macroblocks in CABAC B slices** (the `m4 == 13` escape) can now be
  emitted — and fixing it exposed a latent bug where `B_Skip` committed motion
  but never reconstruction pixels, drifting the encoder's recon from the
  decoder's.
- CAVLC hardening: out-of-table `coded_block_pattern`, over-long
  `mb_skip_run` and unbounded `mvd` now error instead of silently
  zero-filling or overflowing.

## [0.9.1] - 2026-08-12

### Changed — `rusty_alloc` is default-on

[`rusty_alloc`](https://crates.io/crates/rusty_alloc-api) is now the default
process allocator for the codec:

- `rusty_h264-common` default features = `["global-alloc"]`
- `rusty_h264` (facade) default features = `["asm", "global-alloc"]`

Measured and shipped routes share one allocator. Downstream apps that install
their own `#[global_allocator]` should depend with `default-features = false`
(and re-enable `asm` / other features as needed).

Also includes the decoder glue / frame-MT work from the 0.9.0→0.9.1 window
(byte-identical; picture-thread owns parse+recon; EDC nest default-off).

## [0.9.0] - 2026-08-10

The **rip-ASM** release: ~13,600 lines of vendored NASM removed and replaced with
portable SIMD in safe Rust, plus the Great Gate encoder campaign. Gated throughout —
encoder conformance matrix **304/304** (19 tool configs × 4 QPs × 4 clips, both
reconstructions byte-identical to ffmpeg), decoder conformance **160/160** on x264
streams, full workspace suite green.

### ⚠️ Default encoder output has changed

Same config, different bitstream than 0.8.0. Nothing is broken by this — every
combination is conformant and gated — but a consumer diffing bytes across the upgrade
will see them move:

- **High profile + the 8×8 transform are default-ON**, matching x264's own default.
  Inter-8×8 is default-**OFF**: it owned the two worst BD-rate cells, so it ships as an
  opt-in rather than as part of the flip.
- **B-slice RDOQ is default-on** at 16 (−0.66% to −5.40% BD-SSIM, no losing clip, +1.6%
  CPU). P-slice RDOQ ships as a content dispatch, restricted to grain and screen content.
- **Content-adaptive veto gates** from the Great Gate campaign are live across the mode
  decision. Each is audited on the content it actually acts on; the sub-pel grain veto
  is worth −37.45% BD-SSIM on `grain_akiyo` and abstains cleanly elsewhere.

To pin the old behaviour, set `--transform-8x8 0` and `--profile main`.

### Added — 8×8 transform, end to end

- **CABAC 8×8**: `transform_size_8x8_flag` at both syntax positions plus the
  `ctxBlockCat`-5 residual, so the encoder now emits what the decoder could already read.
- **8×8 with B-frames**: the flag is gated at PLAN time rather than emit time. The
  earlier "8×8 + B emits an invalid slice" guard turned out to be a two-line
  flag-presence bug, not a missing feature.
- **Sub-8×8 partitions and the intra-vs-inter RD trial** are reachable: both sat behind
  the same `fast` flag, which also made the `balanced` preset unreachable in practice.

### Fixed — decoder correctness

- **CAVLC 8×8 residual decoded wrong.** The per-4×4 nnz was written as one aggregate
  value broadcast over all four cells, so any CAVLC stream carrying the 8×8 transform
  mis-predicted downstream coefficient counts. Now per-4×4. This affects real
  third-party streams, not just our own output.
- **Chroma boundary-strength deblock fix**, latent, surfaced by the slice-worker
  threading work.
- The parse/pixel skip bug is now **structurally unavailable** in `decode_p8x8`, with a
  mutation-proven guard rather than a comment.

### Changed — the vendored assembly is gone

The decoder is now **assembly-free**, and the encoder's remaining kernels are our own:

- **Phase 0** — 6,380 LOC of NASM deleted that nothing linked against.
- **Phase 1/2** — chroma and luma motion compensation rewritten as portable SIMD
  (SSE2 + AVX2 + **NEON**), retiring 4,490 more lines and lifting the x86-only gate.
  aarch64 now gets real SIMD instead of the scalar fallback.
- **Phase 3** — deblocking ported at parity (1.000×). A first attempt was byte-identical
  but 1.30–1.37× *slower* and was reverted; the cost turned out to be dispatch, not
  arithmetic.
- **Phase 5a** — SATD/SAD composed from our own Hadamard kernel, 2,734 LOC gone.

Every phase gated byte-identical. SSE2 is de-gated everywhere and `mb_copy.asm` is
dropped.

### Added — `global-alloc` (then defaulted in 0.9.1)

`rusty_h264-common` can install [`rusty_alloc`] as the process-wide allocator behind
the `global-alloc` feature. In 0.9.0 it shipped **opt-in** (library hazard:
`#[global_allocator]` is process-wide and Cargo features unify). **0.9.1 flips the
default on** for measured/shipped routes; escape hatch remains
`default-features = false` on the facade. The CLI and bench harness always ran on
`rusty_alloc`.

Also: an allocation audit by call frequency removed the two per-block offenders.

### Added — decoder threading

E2/E3 slice-worker threading, with a content-adaptive dispatch deciding when the seam is
worth taking (default-on). The previously unnamed decode residue is now named and
measurable: `dec-nal-split`, `dec-rbsp-unescape`, `dec-slice-alloc`, `dec-mb-loop`,
`dec-row-hook`.

### Tooling

- **`bench/ffmpeg_race.ps1`** — the decode-vs-ffmpeg race is now a committed script. The
  previous headline number had been produced ad-hoc with no script, so it could not be
  reproduced; four defects were then found in that harness (disk-bound output, a zeroed
  CPU-time read, ffmpeg running multi-threaded against our pinned single thread) and every
  one of them flattered us.
- The gate ledger, signal truth table and gate baseline are committed under `docs/`.

### Where this leaves us, honestly

All-intra BD-rate vs x264 `medium` improved this cycle (akiyo 12.1% → 9.6%, FourPeople
11.9% → 10.6%, harbour 13.9% → 13.4%). Against x264 `veryfast` at defaults we are still
**behind on 7 of 7 clips** — `balanced` narrows the gap to +8.6…+33% on natural content
from +85…+234%, but it is a narrowing, not a win. **Decode speed did not change in this
release**, and the performance table in the README is unchanged for that reason.

## [0.8.0] - 2026-08-05

*(Entry backfilled 2026-08-10 — 0.8.0 shipped without one.)*

The fusion campaign: a ~25–30% decode-runtime reduction across all x264 tool tiers, in
safe Rust, byte-identical to ffmpeg on every gate throughout.

### ⚠️ Breaking: `deblock::BlockInfo` gained `poc0` / `poc1`

The decoder now passes raw `ref_idx` grids plus small POC tables instead of pre-mapped
per-frame `Vec` shims. Empty maps preserve the old contract for encoder and test callers.

### Decoder — performance

- **Sampled scope profiler** (`RS_H264_PROF_SAMPLE`, golden-ratio-hashed 1-in-N with an
  exact prefix for rare stages) — the instrument whose own tax had blinded every prior
  per-macroblock measurement. Validated against ablation.
- **Per-frame materialization removed**: `GridPool`, a DPB plane pool, `pack_frame`
  recycling, and the deletion of the POC-map shims.
- **Stage-boundary fusion**: parsed-nnz threading, MC direct-to-pred, a DC-only residual
  fast path, sparse CABAC level decode, a dequant-scatter hybrid, reconstruct-into-plane.
- **Motion compensation**: quarter-pel `pixel_avg` on the `pavgb` kernel, the scratch
  borrow hoisted to region scope, `b_mc` full-width direct writes with in-place bi-pred
  blending.
- **Deblocking**: input side de-materialized, a two-list AVX2 set-matching kernel, and a
  fused rolling-window bS precompute → derive-at-decode → row-interleaved filtering,
  reaching x264's single-pass shape.
- **CABAC engine driven to its measured floor**: offset and bit-window fused into one
  `u64` with a single-shift renorm, and a fused LPS+transition entry table. The LPS closed
  form was refuted at domain level (the spec's own generative law mismatches 86/256
  entries) and the renorm-skip branch refuted by counter.
- **Entropy-decouple E1 seam** (default-on): defer-and-flush parse/recon loop fission.

New opt-out knobs, all defaulting to the shipped fast path: `RS_H264_ROWDB`,
`RS_H264_EDC`, `RS_H264_BS_PRE`, `RS_H264_NO_POOL`.

## [0.7.0] - 2026-08-02

### ⚠️ Breaking: `deblock::BlockInfo` gained a field

`BlockInfo` is a public struct with public fields, and it gained `pub kind: &'a [u8]`
(plus the packed-path additions). Code that constructs it literally must add `kind: &[]`
to opt out of the kind-aware fast path. Minor rather than patch for that reason, and
because the decoder's boundary-strength path is now **default-on** (below).

### Decoder — boundary-strength pipeline rebuilt, default-on

- **Packed per-macroblock bS records** (`MbPack`): nnz as a 16-bit mask, motion split
  into `mvx`/`mvy` planes, full two-list (B-slice) support. Built in ONE streaming pass
  per frame instead of ~3600 scattered 24-block gathers.
- **Two AVX2 kernels** in `rusty_h264-accel`: `bs_motion_masks_avx2` (the per-edge
  motion test as two 16-bit masks) and `mb_uniform_avx2` (uniform-motion broadcast
  compare). Both keep scalar twins as permanent oracles and as the non-AVX2 path.
- **Byte-identical** throughout: gated by a per-macroblock oracle comparing packed vs
  blind derivation on real streams, a 4000-case and a 6000-case `*_matches_scalar` test,
  and decoded-YUV `cmp` against ffmpeg on 9 x264 streams across three tool tiers.
- Default-on; `RS_H264_BS_PACKED=0` restores the previous path. Promoted on three
  independent interleaved runs (pinned, CPU time, ABBA) — **12/15 paired, z = 2.32** on
  the deciding run, with a null arm of 1.000 taken in the same session.

### Build — the workspace now targets `x86-64-v3` (AVX2)

`.cargo/config.toml` sets `-C target-cpu=x86-64-v3`. Without it the safe core emitted
**zero AVX2** (0 `ymm` vs 1,463 measured by counting instructions). This costs no
safety — it is a codegen flag, the core remains `#![forbid(unsafe_code)]` — but it does
impose an AVX2 hardware floor on builds made *from this workspace*. It is **not**
propagated to consumers of the published crates.

### Corrected: the published decode benchmark was wrong

Releases up to 0.6.0 quoted decode as "145 Mpx/s vs ffmpeg ~590 · 0.25×". Two defects:
the harness took a *differential* of two same-sized numbers (five runs of identical work
gave 202 / 391 / 176 / **negative** / 330 Mpx/s), and the benchmark's own arm decoded
each stream **twice** while reporting one pass's frame count, inflating the gap ~2×.
Both are fixed, and the standing figures are now paired measurements: **2.34× / 2.70× /
2.49×** behind ffmpeg's native `h264` at x264 `veryfast` / `medium` / `slower`, 9/9 with
z = 3.00. Full campaign log, including the refuted ideas, in `docs/WHYS-decoder-perf.md`.

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
