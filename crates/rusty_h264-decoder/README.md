# rusty_h264-decoder

[![crates.io](https://img.shields.io/crates/v/rusty_h264-decoder?logo=rust)](https://crates.io/crates/rusty_h264-decoder)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264-decoder?logo=docsdotrs)](https://docs.rs/rusty_h264-decoder)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The decode pipeline** of the pure-Rust [`rusty_h264`](https://crates.io/crates/rusty_h264)
> codec — Constrained Baseline **+ B-slices + most of High profile**, CAVLC and
> **CABAC**. Validated **bit-exact against Cisco's `h264dec`** over openh264's
> conformance corpus and **pixel-exact vs ffmpeg** on the CABAC paths.
> `#![forbid(unsafe_code)]`, BSD-2, no C in the dependency tree.

**Most users want the facade — [`rusty_h264`](https://crates.io/crates/rusty_h264)**,
which re-exports `Decoder`, `YuvFrame` and the encoder. Depend on this crate
directly only if you want a decoder with no encoder pulled in.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)** — the H.264 decoder inside
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**.

---

## Install

```sh
cargo add rusty_h264-decoder rusty_h264-common
```

```rust
use rusty_h264_decoder::Decoder;

// One call: splits access units, assembles multi-slice pictures,
// and returns frames in DISPLAY order (POC-reordered).
let frames = Decoder::new().decode_stream(&annex_b_bytes)?;

// Or stream it — one picture per access unit, in DECODE order:
let mut dec = Decoder::new();
if let Some(pic) = dec.decode(&access_unit)? {
    let poc = dec.last_poc();   // pair with this to reorder yourself
}
```

Input is **Annex-B** (start codes). `YuvFrame` carries raw planar 4:2:0 (I420).

## What it decodes

- **Constrained Baseline** — `I_16x16` / `I_4x4` / `I_PCM` intra, `P_Skip`,
  P 16×16 / 16×8 / 8×16 / `P_8x8`, quarter-pel motion compensation, in-loop
  deblocking, a multi-reference DPB with POC reordering and MMCO.
- **B-slices** — temporal *and* spatial direct, implicit *and* explicit weighted
  prediction, the L0 / L1 / Bi partitions, `B_Skip` and `B_Direct`.
- **Most of High profile (CAVLC)** — the 8×8 integer transform and 8×8 intra
  prediction, sequence and picture **scaling matrices**,
  `transform_size_8x8_flag`, the second chroma QP offset.
- **CABAC (Main profile)** — I slices (`I_4x4`, `I_16x16` incl. all four 16×16
  modes, luma DC + AC), P slices (`P_Skip`, every partition type and sub-type,
  mvd, MC, residual) and B slices (`B_Skip`, `B_Direct_16x16`, L0/L1/Bi
  16×16 / 16×8 / 8×16, `B_8x8` with per-sub-partition direction, spatial +
  temporal direct). Baseline/Main I + P + B streams decode fully pixel-exact
  end to end.

Not yet: CABAC `I_PCM` (errors gracefully), High-profile 8×8 CABAC residual,
and full JVT-suite conformance.

## How correctness is enforced

- **35 of 35** clean streams from openh264's conformance corpus decode
  **byte-for-byte identical** to Cisco's `h264dec`.
- The CABAC bring-up was verified **symbol-by-symbol** against an instrumented
  openh264 oracle (comparing the arithmetic decoder's `dif`/`rng`/`cnt` state),
  then gated **pixel-exact vs ffmpeg**.
- The reconstruction path is shared with the encoder via
  [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common), so the two
  halves agree bit-for-bit by construction.

## Hardening

A decoder is a parser for hostile input, so it is **fuzzed to never panic and
never hang**. The mutation fuzzer carries committed CABAC seeds covering every
macroblock type and runs thousands of mutations per seed. Three DoS-class bugs
found that way are fixed and regression-gated: an infinite `cabac_unary` loop
(the engine zero-fills past EOF and keeps yielding 1-bins), an out-of-bounds
`cabac_init_idc` context-table index, and an unbounded frame-num-gap allocation
(`log2_max_frame_num` / `log2_max_pic_order_cnt_lsb` are now bounded too).

Errors surface as `DecodeError::{Truncated, MissingParameterSet, Unsupported}` —
never a panic.

## Performance

720p, single core, bit-exact, on **x264-encoded** streams (our own encoder's output is
a narrow slice of H.264 and understates decode cost — its fast preset emits no sub-pel
motion at all, so it skips the entire interpolation path):

| x264 tool tier | rusty_h264 | ffmpeg native `h264` | gap |
|---|---:|---:|---:|
| baseline / CAVLC (`--preset veryfast`) | **213 Mpx/s** | 412 Mpx/s | **1.98×** |
| main / CABAC (`--preset medium`) | **146 Mpx/s** | 294 Mpx/s | **2.16×** |
| high (`--preset slower`) | **125 Mpx/s** | 255 Mpx/s | **2.06×** |


<sub>**Measured 2026-08-05** after a structural-fusion campaign (same harness, same
streams as the previous 2.34×/2.70×/2.49× figures — the change is decoder speed, not
method): per-frame allocation pooling, stage-boundary fusion in the residual/MC paths,
row-interleaved deblocking, a fused-register CABAC engine, and a parse/reconstruct
loop-fission seam — all safe Rust, all byte-identical, each landed behind a paired
win-rate gate (see `docs/WHYS-decoder-perf.md`).</sub>

<sub>**These decode figures were measured with `-C target-cpu=x86-64-v3`** (this
workspace's `.cargo/config.toml`). That setting is deliberately **not** shipped to
consumers of the published crates — a library should not impose an ISA floor on its
dependents — so a default `cargo add rusty_h264` build compiles for baseline x86-64 and
will be somewhat slower than the table above. To reproduce these numbers, build with
`RUSTFLAGS="-C target-cpu=x86-64-v3"` (needs AVX2: Intel Haswell 2013+ / AMD Zen
2015+).</sub>

Pinned to one core, CPU time, ABBA-alternated, 9 pairs, **9/9 with z = 3.00**; frame
counts compared between arms and every stream verified byte-identical to ffmpeg before
timing. (Earlier releases quoted ~145 Mpx/s vs ~590 from a differential harness that
produced 202/391/176/**negative**/330 Mpx/s for identical work; it has been replaced.)

Reference: ffmpeg's *native* `h264` software decoder on the same
machine — the fastest widely-available SW H.264 decoder, and a deliberately
tougher bar than openh264's own `h264dec`. Most of the recent gain came from
**byte-identical redundancy elimination** in the pure-Rust glue rather than new
asm: skipping B-only motion/ref work on Baseline streams (+12%), move-not-clone
on the DPB reference frame, and passing the deblock filter the empty grids it
doesn't use. Reproduce with
[`bench/decode_speedtest.sh`](https://github.com/remade-with-rust/rusty_h264/blob/main/bench/decode_speedtest.sh).

## Features

| Feature | Default | Effect |
|---|:--:|---|
| `asm` | — | Route MC, deblocking and the inverse DCT through the vendored openh264 SIMD asm in [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel). The `unsafe` FFI stays quarantined there; this crate remains `forbid(unsafe)`. x86-64 + `nasm`. |
| `profile` | — | Dev-only `rdtsc` stage profiler (used by the `profile_decode` test) — zero cost when off. |

SIMD is enabled through the facade's default `asm` feature; standalone, the
scalar path is the default.

## Where this sits

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | the public, safe facade API — **depend on this** |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, prediction, MC, deblock |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| **[`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder)** | **← you are here** — the decode pipeline |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional openh264 SIMD asm — the one `unsafe` crate |

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
