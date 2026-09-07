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
# SIMD acceleration on by default (pure Rust intrinsics — no assembler, no build script):
rusty_h264 = "0.7"

# …or scalar-only, 100%-safe Rust — no `unsafe` anywhere in the tree:
rusty_h264 = { version = "0.7", default-features = false }
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
  BD-rate for 1.10–1.22× the time; `RUSTY_H264_LEGACY_CAVLC=1` selects
  `EncoderConfig::baseline`, the Constrained Baseline + CAVLC configuration).
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
| `asm` | ✅ | portable Rust SIMD (x86-64 SSE2/AVX2, aarch64 NEON) for MC, deblocking, transforms, SATD/SAD. No assembler or build script; the `unsafe` intrinsics are quarantined in `rusty_h264-accel`. |
| *(none)* | — | `--no-default-features` → scalar-only, 100% safe Rust with no `unsafe` at all. Runs on any Rust target. |

The `asm` kernels are x86-64 only; on other architectures (e.g. arm64 macOS) the
accel crate compiles to an empty lib and the pure-Rust scalar path is selected
automatically, so a default-features build works everywhere.

**The codec core is `#![forbid(unsafe_code)]` either way.** All `unsafe` lives in
the single, optional [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel)
crate. The same acceleration boundary accepts **your own custom kernels or
hand-written ASM** — the safe core never changes when you push for speed.

## Performance

Single core, bit-exact, on the maintainer's machine:

**Decode**, 1800 frames of real 720p content **encoded by x264** (what an encoder puts in
the stream dominates decode cost), vs ffmpeg's *native* `h264` software decoder:

| x264 tool tier | rusty_h264 | ffmpeg native `h264` | gap |
|---|---:|---:|---:|
| baseline / CAVLC (`--preset veryfast`) | **160 Mpx/s** | 263 Mpx/s | **1.66×** |
| main / CABAC (`--preset medium`) | **130 Mpx/s** | 211 Mpx/s | **1.63×** |
| high (`--preset slower`) | **111 Mpx/s** | 191 Mpx/s | **1.77×** |

| encode workload | rusty_h264 | reference |
|---|---:|---:|
| **Encode** INTER, CIF (vs openh264) | **71 Mpx/s** | 115 · 1.6× |
| **Encode** ALL-INTRA, CIF (vs openh264) | **24 Mpx/s** | 88 · 3.6× |


<sub>**Measured 2026-09-06 (0.16.0)** on the same harness and streams as every earlier
figure. The box was shared with a foreign LLM server during this run, so the absolute
Mpx/s of *both* arms sit below the quiet-box 2026-08-05 run (213/146/125 vs
412/294/255), so **read the ratio, not the Mpx/s**. The ratio moved from
1.98×/2.16×/2.06× to roughly 1.66×/1.63×/1.77× over the 0.15.0 and 0.16.0 decoder
rounds — a register-resident CABAC engine and one-call-per-macroblock residual
parser, a SIMD reachability sweep that put every 4×4 IDCT, narrow MC and intra 4×4
path on a kernel, instruction cuts inside the kernels, content-gate reroutes, and a
fused scan-order dequant+IDCT+add kernel — all byte-identical, each landed behind a
pinned CPU-time ABBA clock. 0.16.0 itself shows **no resolved change** against
0.15.0: it is dominated by dead-code removal (the decode binary lost 4.4% of its
instructions), which cuts footprint rather than executed work, and both arms were
2–11% slower in absolute CPU on the busier box. See `CHANGELOG.md` and
`docs/big-oppy-decoder.md`.</sub>

<sub>**These decode figures were measured with `-C target-cpu=x86-64-v3`** (this
workspace's `.cargo/config.toml`). That setting is deliberately **not** shipped to
consumers of the published crates — a library should not impose an ISA floor on its
dependents — so a default `cargo add rusty_h264` build compiles for baseline x86-64 and
will be somewhat slower than the table above. To reproduce these numbers, build with
`RUSTFLAGS="-C target-cpu=x86-64-v3"` (needs AVX2: Intel Haswell 2013+ / AMD Zen
2015+).</sub>

Method: pinned to one core, **CPU time** (not wall), arms ABBA-alternated, 7 pairs,
**7/7 paired with z = 2.65** on every tier; frame counts compared between arms and every
stream verified byte-identical to ffmpeg before timing. Earlier releases quoted
"145 Mpx/s · 0.25×" from a differential harness that has since been refuted and replaced
— see `docs/WHYS-decoder-perf.md`.

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
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional portable SIMD kernels, SSE2/AVX2 + NEON — the one `unsafe` crate |

The workspace mirrors Cisco openh264's `codec/` tree (`common`/`encoder`/
`decoder`/`api`/`console`).

## Using it from `remade_ffmpeg_rs`

Depend on this facade and adapt to the `rff-codec` `Encoder`/`Decoder` traits —
`YuvFrame` (I420 planes) ↔ `VideoFrame`. Note rusty_h264 speaks **Annex-B**
(start codes), so an AVCC↔Annex-B shim is needed for MP4 inputs. `default-features = false` gives the scalar,
fully-safe build if you want zero `unsafe` in your dependency tree.

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
