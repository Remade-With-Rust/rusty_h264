# rusty_h264-cli

[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The `rusty_h264` command-line tool** — encode raw YUV420p to an Annex-B
> `.264` stream and decode it back. Mirrors openh264's `codec/console` apps.

This crate is `publish = false` — it is the workspace's dev/demo front-end, not
a published library. For a full media CLI (`ffmpeg` / `ffprobe` replacements
with muxing, filtering and every other codec) use
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**,
which embeds this codec.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)**.

---

## Build & run

```sh
git clone https://github.com/remade-with-rust/rusty_h264
cd rusty_h264

# SIMD kernels on (default; needs nasm on PATH)
cargo build --release -p rusty_h264-cli

# …or 100% safe portable Rust, no nasm
cargo build --release -p rusty_h264-cli --no-default-features
```

The binary is named `rusty_h264`.

## Usage

```
rusty_h264 encode --width W --height H [options] --in in.yuv --out out.264
rusty_h264 decode --width W --height H --in in.264 --out out.yuv
```

Input and output YUV is **raw planar 4:2:0 (I420)**, one frame after another.
The bitstream is **Annex-B** (start codes).

```sh
cargo run --release -p rusty_h264-cli -- \
    encode --width 352 --height 288 --gop 30 --qp 26 --in in.yuv --out out.264

cargo run --release -p rusty_h264-cli -- \
    decode --width 352 --height 288 --in out.264 --out roundtrip.yuv
```

### Encode options

| Flag | Default | What it does |
|---|---|---|
| `--width` / `--height` | *required* | Picture size in luma samples (not restricted to multiples of 16). |
| `--qp N` | `26` | Quantization parameter, 0–51. Lower = finer. |
| `--gop N` | `30` | Keyframe interval. `1` = all-intra; `250` ≈ best size. `30` is the sweet spot for per-GOP threading and lands within ~2% of `--gop 250`. |
| `--preset fast\|quality` | `fast` | Speed/quality trade-off. |
| `--bitrate BPS --fps F` | off | Enable average-bitrate rate control instead of constant QP. |
| `--refs N` | `1` | Reference frames for P-macroblocks. |
| `--satd-q F` | `0.5` | Route the top-`F` fraction of highest-variance macroblocks to the rate-faithful SATD mode decision instead of cheap SAD. `0` = pure SAD; `0.5` ≈ −2.3% BD-rate for +6% time; `1` ≈ −4.3% for +13%. |
| `--bframes N\|auto` | `0` | B-frames per anchor gap (Main profile). `auto` codes them **only** on B-favorable smooth-motion content and falls back to P-only on busy content, so B-frames never regress. |
| `--iqp-offset D` | `-3` | Per-GOP I-frame QP cascade (the `ip_ratio` idea) — the GOP's I-frame is coded finer, content-adaptively deeper on predictable GOPs. `0` opts out. |
| `--bqp-offset D` | `2` | B-frame QP offset base; content-adaptively coarser on near-static GOPs (B-frames are non-reference, so their error never propagates). |
| `--aq S` | `1.0` | Adaptive quantization strength — per-MB QP finer on flat regions, coarser on busy ones (a perceptual/SSIM win). Self-limits on pathological content. `0` = off. |

## Benchmarking

The repo ships a reproducible A/B harness that feeds an identical deterministic
clip to this encoder and to a C reference (x264 or Cisco openh264) invoked as a
**separate external process** — never linked into or built by this project:

```sh
cd bench
export RUSTY_H264_BENCH_FFMPEG=/path/to/ffmpeg          # built with libx264
cargo run --release -- --width 352 --height 288 --frames 60 --gop 1             # intra vs x264
cargo run --release -- --width 352 --height 288 --frames 60 --gop 30 --refs 1   # inter, matched 1 ref
cargo run --release -- --ref-codec libopenh264 --gop 1                          # vs Cisco openh264
```

`--refs` is applied to **both** encoders so the race is fair. Decode throughput
is a separate differential head-to-head vs ffmpeg's native `h264` software
decoder:

```sh
bash bench/decode_speedtest.sh          # args: W H N1 N2, e.g. 1920 1080 40 160
```

Methodology: [docs/benchmarks.md](https://github.com/remade-with-rust/rusty_h264/blob/main/docs/benchmarks.md).

## Where this sits

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | the public, safe facade API — **depend on this** |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, prediction, MC, deblock |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional openh264 SIMD asm — the one `unsafe` crate |
| **`rusty_h264-cli`** | **← you are here** — the console front-end (not published) |

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
