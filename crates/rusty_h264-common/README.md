# rusty_h264-common

[![crates.io](https://img.shields.io/crates/v/rusty_h264-common?logo=rust)](https://crates.io/crates/rusty_h264-common)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264-common?logo=docsdotrs)](https://docs.rs/rusty_h264-common)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The foundation both halves of the codec sit on.** Bitstream I/O, Exp-Golomb,
> NAL/Annex-B framing, the integer transforms, intra prediction, motion
> compensation and the deblocking filter — the shared `codec/common` layer of
> the pure-Rust [`rusty_h264`](https://crates.io/crates/rusty_h264) codec.

**Most users want the facade — [`rusty_h264`](https://crates.io/crates/rusty_h264).**
Depend on this crate directly only if you are building on the primitives (a
custom parser, an analyzer, your own codec) rather than encoding or decoding.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)**.

---

## Why it's `forbid(unsafe_code)`

The bit-twiddling core of an H.264 codec — Exp-Golomb reads off a hostile
bitstream, RBSP emulation handling, block-level pixel loops — is *exactly* where
memory-safety bugs hide in the C implementations. So this crate is
`#![forbid(unsafe_code)]`, and the shared reconstruction path here is what makes
the encoder and decoder agree bit-for-bit.

The single exception is opt-in and never shipped: the `profile` feature relaxes
the attribute to unlock the `rdtsc` timer in [`prof`](https://docs.rs/rusty_h264-common/latest/rusty_h264_common/prof/)
for stage measurement. The release build forbids `unsafe`.

## Modules

| Module | What's in it |
|---|---|
| `bit_writer` / `bit_reader` | MSB-first bit packing and parsing, Exp-Golomb (`ue`/`se`), bounded reads that return `OutOfData` instead of panicking |
| `nal` | NAL units, Annex-B framing (`split_annex_b`), RBSP emulation prevention/unprevention |
| `types` | `YuvFrame` (raw planar I420), `Profile`, `ChromaFormat` |
| `transform` | 4×4 and 8×8 integer transforms, quantization/dequantization, luma & chroma DC Hadamard, scaling matrices |
| `predict` | Intra prediction — `I_16x16`, `I_4x4`, `I_8x8`, chroma 8×8 |
| `inter` | Quarter-pel luma motion compensation (6-tap), chroma MC, the interpolation half-pel planes |
| `deblock` | The in-loop deblocking filter, luma + chroma, 8×8-transform-aware |
| `cavlc` | CAVLC residual coding tables — table-driven **O(1)** decode (was O(bits·candidates)) |
| `cabac_tables` | The 460-context CABAC initialization tables shared by both entropy engines |
| `aligned` | `AlignedBytes` — a safe `[u8]` ↔ `[u128]` view (via `bytemuck`) giving 16-byte-aligned plane rows so the SIMD kernels can use aligned loads without `unsafe` here |
| `prof` | Dev-only `rdtsc` stage profiler, behind the `profile` feature — zero cost when off |

Also exported: `ACCEL: bool`, whether the vendored SIMD kernels are actually
compiled in. Benchmarks read it so a harness that silently fell back to the
scalar twin can't report the fast path's number.

## Features

| Feature | Default | Effect |
|---|:--:|---|
| `asm` | — | Route the hot kernels (deblock, MC, transforms, intra pred) through the portable Rust SIMD (x86-64 SSE2/AVX2, aarch64 NEON) in [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel). The `unsafe` intrinsics stay quarantined there; this crate calls only its safe wrappers and remains `forbid(unsafe)`. |
| `profile` | — | Enable the in-process stage profiler. Measurement builds only — never ship it. |

SIMD is enabled through the facade's default `asm` feature; when this crate is
used standalone the scalar path is the default.

## Install

```sh
cargo add rusty_h264-common
```

```rust
use rusty_h264_common::BitReader;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};

// Walk an Annex-B stream and read each NAL's header + first Exp-Golomb field.
for nal in split_annex_b(&stream) {
    let rbsp = emulation_unprevent(nal);
    let mut r = BitReader::new(&rbsp);
    let _forbidden_zero = r.read_bits(1)?;
    let _nal_ref_idc    = r.read_bits(2)?;
    let nal_unit_type   = r.read_bits(5)?;
    println!("nal type {nal_unit_type}, first ue = {}", r.read_ue()?);
}
```

## Where this sits

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | the public, safe facade API — **depend on this** |
| **[`rusty_h264-common`](https://crates.io/crates/rusty_h264-common)** | **← you are here** — bitstream I/O, transforms, prediction, MC, deblock |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional portable Rust SIMD (x86-64 SSE2/AVX2, aarch64 NEON) — the one `unsafe` crate |

The workspace mirrors Cisco openh264's `codec/` tree; this crate is
`codec/common`.

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
