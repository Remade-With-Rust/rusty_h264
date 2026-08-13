# rusty_h264-encoder

[![crates.io](https://img.shields.io/crates/v/rusty_h264-encoder?logo=rust)](https://crates.io/crates/rusty_h264-encoder)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264-encoder?logo=docsdotrs)](https://docs.rs/rusty_h264-encoder)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The encode pipeline** of the pure-Rust [`rusty_h264`](https://crates.io/crates/rusty_h264)
> codec: raw I420 frames in, a conformant Annex-B H.264 stream out. Every frame
> it emits decodes **bit-exactly under ffmpeg across QP 0–51**, intra and inter.
> `#![forbid(unsafe_code)]`, BSD-2, no C in the dependency tree.

**Most users want the facade — [`rusty_h264`](https://crates.io/crates/rusty_h264)**,
which re-exports `Encoder`, `EncoderConfig` and the shared types alongside the
decoder. Depend on this crate directly only if you want the encoder alone.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)** — the H.264 encoder inside
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**.

---

## Install

```sh
cargo add rusty_h264-encoder rusty_h264-common
```

```rust
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};
use rusty_h264_common::YuvFrame;

let mut cfg = EncoderConfig::new(1280, 720);
cfg.preset    = Preset::Balanced;  // Fast | Balanced | Quality
cfg.gop_size  = 30;                // an IDR every 30 frames, P-frames between
cfg.bitrate   = 4_000_000;         // 4 Mbps ABR; 0 = constant QP (cfg.qp)
cfg.framerate = 30.0;

let mut enc = Encoder::new(cfg).unwrap();
for frame in &frames {
    let access_unit = enc.encode(frame);   // Annex-B bytes, ready to write
}
```

## How correctness is enforced

The encoder is gated by a **dual oracle**, and both gates are standing CI tests:

1. **Round-trip** — the encoder's own output is decoded by
   [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) (a
   dev-dependency; there is no build cycle, the decoder never depends on the
   encoder) and asserted **pixel-exact against the encoder's internal
   reconstruction**. Encoder/decoder drift is caught at the macroblock.
2. **Independent reference** — every frame is decoded by **ffmpeg** and asserted
   pixel-exact, across the whole QP 0–51 range, intra and inter.

Speed work on top of that is gated **byte-identical**: a performance brick that
changes a single output byte is reverted, not shipped.

## Coding tools

**Default-on**

- **Intra:** `I_16x16` (all four modes), `I_4x4`, `I_PCM`, chroma 8×8, with
  λ-based RD/SATD mode decision.
- **Inter P-frames:** `P_Skip`, 16×16 / 16×8 / 8×16, quarter-pel motion
  compensation, rate-aware motion estimation with early termination, a
  multi-reference DPB.
- **CABAC entropy coding** (Main profile) — measured **−8.8…−9.0% BD-rate** for
  1.10–1.22× the time on the 4-QP corpus, which is better value than any preset
  step in either encoder, so it ships on by default. Trellis RDOQ is default-on
  for all-intra (−0.5…−1.3%). `RUSTY_H264_LEGACY_CAVLC=1` restores the exact
  prior defaults (Constrained Baseline + CAVLC) byte-for-byte, as an escape
  hatch and bisection anchor.
- **Adaptive quantization** (`aq_strength`, default `1.0`): per-macroblock QP
  finer on flat/low-variance regions (where blocking and banding are visible),
  coarser on busy ones (where the eye masks error). The shift is relative to the
  frame's mean log-variance, rate-compensated, and its effective strength backs
  off automatically on pathological synthetic content — so it never regresses.
  `0.0` = uniform QP, byte-identical to AQ-off.
- **Per-GOP I-frame QP cascade** (`i_qp_offset`, default `-3`) — the calibrated
  equivalent of x264's `ip_ratio`, deepened content-adaptively on predictable
  GOPs.
- In-loop deblocking; **average-bitrate rate control** (complexity model +
  leaky-bucket buffer).

**Opt-in**

| Knob | What it buys |
|---|---|
| `bframes` / `bframes_adaptive` | B-frames (Main profile) with the reorder pipeline, L0-past + L1-future bi-prediction and spatial/temporal direct. Strongly content-dependent, so `bframes_adaptive` measures the clip's temporal predictability and codes B-frames **only** where they help — capturing the win without ever regressing. |
| `transform_8x8` | `I_8x8` intra and the 8×8 inter transform, a 3-way per-macroblock RD choice that wins on smooth / large-structure content. Requires High profile, and is CAVLC-only today (the decoder has no CABAC 8×8 residual yet). |
| `mbtree` + `mbtree_strength` + `mbtree_lookahead` | Temporal AQ: a lookahead propagates each macroblock's future importance backward along motion vectors and lowers the QP of heavily-referenced macroblocks. The complement to `aq_strength`'s spatial AQ. `LookaheadMode` picks the search resolution — `FullRes` (best), `Hybrid` (half-res search, full-res score/refine — ~1.7× for no measured loss), `HalfRes` (~4×, default). |
| `tune_rd_skip` (+ `tune_rd_skip_min_free`, `tune_rd_skip_fast_t`) | Rate-distortion `P_Skip`: skip when `J = SSD + λ·bits` says so, not only when the residual quantizes to exactly zero. Gated on the frame's online free-skip rate so it engages only on the content where it wins. |
| `num_ref_frames` | Multiple reference frames for P-macroblocks (1..=16). |

**Default-on for the `Quality` preset** (`None` follows the preset; `Some(b)`
forces either way):

| Knob | What it buys |
|---|---|
| `sub_8x8` | `P_8x8` sub-partition motion — four 8×8 partitions with their own MVs, a per-MB RD choice against 16×16/16×8/8×16. A net win on real content (12-clip Derf corpus: −0.23% mean BD, large wins on bus/mobile/flower); a 6-channel discovery harvest proved no cheap gate beats default-on. |
| `me_wide` | Adaptive wide motion search — on flat source blocks, where the gradient-descent diamond stalls on a plateau and misses the true MV, cover the ±16 neighbourhood with a grid instead. A per-frame coherence gate keeps it from regressing even on pure pans. |

Bitstream-changing knobs are validated by **4-QP BD-rate per clip with a
worst-clip-≤-0 rule** — never a mean, never a single QP.

## Presets

| `Preset` | Mode decision | Motion | Notes |
|---|---|---|---|
| `Fast` | cheap **SAD** (auto-vectorizes to `psadbw`), `P_16x16`-only inter, `I_16x16`-only intra | **integer-pel** | Maximum throughput. |
| **`Balanced`** | `Fast`'s decision path | **+ sub-pel refinement** | **−42…−50% BD-rate** vs `Fast` (PSNR and SSIM agree) for ~2.3–3.1× the time. On fine detail it beats `Quality` on *both* size and speed. The `Preset` default. |
| `Quality` | full RD — every candidate trial-encoded for real `J = SSD + λ·bits` | + `16x8`/`8x16` sub-partitions, full `I_4x4` search | Smallest files, much slower. |

`EncoderConfig::new()` starts at `Preset::Fast` and all-intra (`gop_size = 1`);
set `preset` and `gop_size` for anything else.

## Features

| Feature | Default | Effect |
|---|:--:|---|
| `asm` | — | Route the hot kernels (DCT/quant, MC, deblock, SATD/SAD) through the portable Rust SIMD (x86-64 SSE2/AVX2, aarch64 NEON) in [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel). The `unsafe` intrinsics are quarantined there, so this crate stays `forbid(unsafe)` even with `--features asm`. |
| `profile` | — | Dev-only `rdtsc` stage profiler (`rusty_h264-common::prof`) — zero cost when off. |

The SIMD SATD kernels are wired into the quality-preset mode decision
**byte-identically**: `Σ|H·d|` is always even, so openh264's `(Σ+1)>>1` result
times 2 recovers our value exactly — worth **1.7×** on quality inter encode with
no bitstream change.

## Where this sits

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | the public, safe facade API — **depend on this** |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, prediction, MC, deblock |
| **[`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder)** | **← you are here** — the encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline (and this crate's round-trip oracle) |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional portable Rust SIMD (x86-64 SSE2/AVX2, aarch64 NEON) — the one `unsafe` crate |

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
