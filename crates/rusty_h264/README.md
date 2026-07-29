# rusty_h264

[![crates.io](https://img.shields.io/crates/v/rusty_h264?logo=rust)](https://crates.io/crates/rusty_h264)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264?logo=docsdotrs)](https://docs.rs/rusty_h264)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The public facade — this is the crate you depend on.** A ground-up,
> pure-**Rust** H.264 **encoder and decoder** with a `#![forbid(unsafe_code)]`
> codec core, no C in the dependency tree, and a BSD-2 license you can embed
> anywhere. The decoder is validated **bit-exact** against Cisco's `h264dec`
> over openh264's conformance corpus; the encoder is **bit-exact** under ffmpeg
> across QP 0–51.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)** — and the H.264 engine inside
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**,
our memory-safe FFmpeg alternative.

---

## Install

```sh
cargo add rusty_h264
```

```toml
[dependencies]
# SIMD acceleration on by default (needs `nasm` at build time; kernels are vendored):
rusty_h264 = "0.3"

# …or pure, portable, 100%-safe Rust — no nasm, no FFI, no unsafe anywhere:
rusty_h264 = { version = "0.3", default-features = false }
```

## Quick start

```rust
use rusty_h264::{Encoder, EncoderConfig, Decoder, YuvFrame};

let mut enc = Encoder::new(EncoderConfig::new(640, 480)).unwrap();
let frame = YuvFrame::black(640, 480);
let bitstream = enc.encode(&frame);     // one Annex-B access unit

let mut dec = Decoder::new();
let decoded = dec.decode(&bitstream).unwrap().unwrap();
assert_eq!(decoded, frame);             // a flat frame has no residual → exact
```

The codec is lossy in general (that round-trip is exact only because the frame is
flat); quality is governed by QP or the bitrate target. A moving sequence with
P-frames and rate control:

```rust
use rusty_h264::{Encoder, EncoderConfig};

let mut cfg = EncoderConfig::new(640, 480);
cfg.gop_size  = 30;          // an IDR every 30 frames, P-frames between
cfg.bitrate   = 1_000_000;   // 1 Mbps average; 0 = constant-QP (cfg.qp)
cfg.framerate = 30.0;
let mut enc = Encoder::new(cfg).unwrap();
for frame in &frames { let au = enc.encode(frame); /* … */ }
```

Decoding a whole stream in **display order** is one call — it splits access
units, assembles multi-slice pictures and reorders by POC:

```rust
use rusty_h264::Decoder;

let frames = Decoder::new().decode_stream(&stream).unwrap();
```

For streaming use, the lower-level `Decoder::decode` returns one picture per
access unit in **decode** order (pair it with `Decoder::last_poc` to reorder).

## What this crate re-exports

| Item | From | Role |
|---|---|---|
| `Encoder`, `EncoderConfig`, `EncodeError` | [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| `Preset`, `LookaheadMode` | [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | speed/quality trade-offs |
| `Decoder`, `DecodeError` | [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline |
| `YuvFrame`, `Profile`, `ChromaFormat` | [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | shared types (I420 planes) |
| `NalUnit`, `NalUnitType` | [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | Annex-B / NAL layer |
| `VERSION` | this crate | the crate version string |

You never need to name the sub-crates directly — that's the point of the facade.

## Capabilities

**Decoder** — validated bit-exact vs Cisco `h264dec` over openh264's corpus:

- **Constrained Baseline + B-slices** (temporal & spatial direct, implicit &
  explicit weighted prediction, L0/L1/Bi partitions, `B_Skip`/`B_Direct`).
- **Most of High profile (CAVLC):** 8×8 integer transform and 8×8 intra
  prediction, sequence/picture scaling matrices, `transform_size_8x8_flag`,
  second chroma QP offset.
- **CABAC entropy decode (Main profile):** I slices (`I_4x4`, `I_16x16`),
  P slices (`P_Skip`, all partition types + sub-types, mvd, MC, residual) and
  B slices (`B_Skip`, `B_Direct_16x16`, L0/L1/Bi, `B_8x8`, spatial + temporal
  direct) — brought up symbol-by-symbol against an instrumented openh264
  oracle and gated **pixel-exact vs ffmpeg**.
- Full intra (`I_16x16`/`I_4x4`/`I_8x8`/`I_PCM`), quarter-pel MC, in-loop
  deblocking (8×8-aware), multi-reference DPB with POC reordering and MMCO.
- **Fuzzed to never panic or hang** on malformed input.

**Encoder** — every frame decodes bit-exactly under ffmpeg, QP 0–51:

- Intra (`I_16x16`/`I_4x4`, λ-based RD mode decision), inter P-frames
  (`P_Skip`/16×16/16×8/8×16), quarter-pel MC, rate-aware ME, multi-ref DPB.
- **CABAC entropy coding** (Main profile, default-on — measured −8.8…−9.0%
  BD-rate for 1.10–1.22× the time; `RUSTY_H264_LEGACY_CAVLC=1` restores the
  Constrained Baseline + CAVLC bitstream byte-for-byte).
- **Adaptive quantization** (default-on): per-macroblock QP finer on flat
  regions, coarser on busy ones — a perceptual/SSIM win that self-limits on
  pathological content so it never regresses.
- Per-GOP I-frame QP cascade, in-loop deblocking, average-bitrate rate control
  (complexity model + leaky bucket).
- Opt-in tools: B-frames (`bframes`, incl. a content-adaptive enable), the 8×8
  transform (`I_8x8` + inter, High profile), mb-tree temporal AQ with a
  lookahead, RD `P_Skip`. `P_8x8` sub-partition motion and the adaptive wide
  motion search are default-on for the `Quality` preset.
- Three presets — `Fast` (SAD, integer-pel), **`Balanced`** (adds sub-pel
  refinement: −42…−50% BD-rate over `Fast` for ~2.3–3.1× the time), `Quality`
  (full RD trial-encode, sub-partitions, full `I_4x4` search).

## Features

| Feature | Default | Effect |
|---|:--:|---|
| `asm` | ✅ | Vendored openh264 BSD-2 SIMD kernels (x86-64) for MC, deblocking, transforms, SATD/SAD. Needs `nasm` on `PATH`. |
| *(none)* | — | `--no-default-features` → 100% safe, portable Rust. No `nasm`, no FFI, no `unsafe`. Runs on any Rust target. |

The `asm` kernels are x86-64 only; on other architectures (e.g. arm64 macOS) the
accel crate compiles to an empty lib and the pure-Rust scalar path is selected
automatically, so a default-features build works everywhere.

**The codec core is `#![forbid(unsafe_code)]` either way.** All `unsafe` lives in
the single, optional [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel)
crate. The same acceleration boundary accepts **your own custom kernels or
hand-written ASM** — the safe core never changes when you push for speed.

## Performance

Single core, bit-exact, on the maintainer's machine:

| workload | rusty_h264 | reference |
|---|---:|---:|
| **Decode** 1080p — default SIMD kernels | **145 Mpx/s** | ffmpeg-native `h264` ~590 · 0.25× |
| **Decode** 1080p — 100% safe Rust | **109 Mpx/s** | ffmpeg-native `h264` ~590 · 0.18× |
| **Encode** INTER, CIF (vs openh264) | **71 Mpx/s** | 115 · 1.6× |
| **Encode** ALL-INTRA, CIF (vs openh264) | **24 Mpx/s** | 88 · 3.6× |

Decode is benched against ffmpeg's *native* `h264` software decoder — a
deliberately tougher bar than openh264's own `h264dec`. Full methodology,
RD sweeps vs x264 and the reproducible harness:
[`bench/`](https://github.com/remade-with-rust/rusty_h264/tree/main/bench) and
[docs/benchmarks.md](https://github.com/remade-with-rust/rusty_h264/blob/main/docs/benchmarks.md).

## Where this sits

| Crate | Role |
|---|---|
| **[`rusty_h264`](https://crates.io/crates/rusty_h264)** | **← you are here** — the public, safe facade API |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, Exp-Golomb, NAL/Annex-B, transforms, MC, deblock |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional openh264 SIMD asm — the one `unsafe` crate |

The workspace mirrors Cisco openh264's `codec/` tree (`common`/`encoder`/
`decoder`/`api`/`console`).

## Using it from `remade_ffmpeg_rs`

Depend on this facade and adapt to the `rff-codec` `Encoder`/`Decoder` traits —
`YuvFrame` (I420 planes) ↔ `VideoFrame`. Note rusty_h264 speaks **Annex-B**
(start codes), so an AVCC↔Annex-B shim is needed for MP4 inputs. Keep
`default-features = false` in CI if you don't want a `nasm` build dependency
there.

## The Remade With Rust ecosystem

<!-- ORG BOILERPLATE — keep identical across repos -->

**Remade With Rust** is an initiative by **[Mata Network](https://www.mata.network/)**
to rebuild essential C and C++ tools in Rust — for the memory safety, the
predictable performance, and the freedom of a permissive license. Each project
is a reimplementation, not a fork: same wire protocols and file formats, new
code you can actually depend on. No copyleft. No surprises.

| Project | What it is |
|---|---|
| 🎬 **[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)** | **Our FFmpeg alternative.** Drop-in `ffmpeg` and `ffprobe` binaries — demux → decode → filter → encode → mux, rebuilt as composable Rust crates with **zero GPL/LGPL**. Apache-2.0. `rusty_h264` is its H.264 codec. |
| 🧠 **[FFAI](https://github.com/Remade-With-Rust/FFAI)** | **Our sister project: media *for* AI.** "The AI media toolkit, remade with rust." Embedded ASR + TTS (**Mercury**), OCR (**Carmenta**) and vision-language captioning (**Argus**) behind an ffmpeg-style, swap-by-name architecture — no Python, no CUDA. MIT OR Apache-2.0. |
| 🌐 **[Mata Network](https://www.mata.network/)** | **The home page.** *"Stop sacrificing your privacy for convenience."* Sovereign, self-hostable privacy infrastructure — wallet & identity, password manager, contact manager, and a browser extension that stops information leaking as you browse. Remade With Rust is its open-source arm. |

→ All projects: **[github.com/Remade-With-Rust](https://github.com/Remade-With-Rust)**

<!-- /ORG BOILERPLATE -->

## License

BSD-2-Clause — see [LICENSE](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE).
No GPL/LGPL anywhere in the dependency tree, and no C/C++ either (CI-enforceable
via `cargo-deny`). Embed it in closed-source software freely.
