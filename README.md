# rusty_h264

[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](LICENSE)
![Platforms: Windows · macOS · Linux](https://img.shields.io/badge/platforms-Windows%20%C2%B7%20macOS%20%C2%B7%20Linux-informational)

> **rusty_h264** is a ground-up, pure-**Rust** H.264 codec — a clean rebuild of
> [Cisco openh264](https://github.com/cisco/openh264) (BSD-2/C++): memory-safe,
> permissively licensed, with zero C in the build and zero copyleft strings.

---

## ⚡ The headline

A complete Constrained Baseline H.264 encoder **in pure, safe Rust** — intra,
inter (P-frames with quarter-pel motion compensation), in-loop deblocking, and
average-bitrate rate control — whose every frame decodes **bit-exactly under
ffmpeg across the entire QP range (0–51)**.

| | x264 (C) | **rusty_h264 (Rust)** |
|---|---|---|
| C/C++ in the dependency tree | all of it | **none** |
| `unsafe` in the codec core | extensive | **0** — `#![forbid(unsafe_code)]` |
| License | GPL (copyleft) | **BSD-2** (embed freely) |
| Bit-exact vs ffmpeg | — | **QP 0–51, intra + inter** |

On a deterministic CIF clip (scrolling gradient + moving box, 60 frames),
matched-QP, both encoders' output decoded by the same ffmpeg for PSNR:

| QP 26 | rusty_h264 (Rust) | x264 (C) | size |
|---|---:|---:|:--:|
| **intra** | 0.298 bpp · 42.7 dB | 0.331 bpp · 45.3 dB | **0.90×** |
| **inter** (I+P) | 0.131 bpp · 48.2 dB | 0.097 bpp · 50.2 dB | 1.36× |

<sub>On **intra**, rusty_h264 produces **smaller files than x264 at matched QP** — at
~2.5 dB lower PSNR, i.e. a different rate-distortion operating point; on an
equal-quality basis x264 keeps a ~15–20% edge. On **inter**, x264's two decades
of motion-estimation tuning keep it ahead on both size and speed. rusty_h264 trades
some compression and encode speed for **memory safety, a permissive license, and
zero C in the build — while matching the reference decoder bit-for-bit**.
Methodology + full RD sweep: [`bench/`](bench/), [docs/benchmarks.md](docs/benchmarks.md).</sub>

---

## What is this?

`rusty_h264` decodes and encodes H.264 (Constrained Baseline Profile) in pure,
safe Rust. Unlike the existing [`openh264-rs`](https://github.com/ralfbiedert/openh264-rs)
bindings — which vendor Cisco's C source and call it over FFI, offering "no
additional safety guarantees" — there is **no C in the dependency tree** here.
The codec core is `#![forbid(unsafe_code)]`, BSD-2 licensed, and embeddable in
closed-source software with no copyleft obligations. It is a reimplementation
of the algorithms, not a wrapper around the original.

## Remade With Rust

<!-- ORG BOILERPLATE — keep identical across repos -->

**Remade With Rust** is an initiative by [Mata Network](https://www.mata.network)
to rebuild essential C and C++ tools in Rust — for the memory safety, the
predictable performance, and the freedom of a permissive license. Each project is a reimplementation, not a fork: same wire protocols and file formats,
new code you can actually depend on.

We build the core to production grade and open-source it so the community can
extend it. No copyleft. No surprises. Just the tools we rely on, made faster and
safer.

→ More projects: **[github.com/remade-with-rust](https://github.com/remade-with-rust)**

<!-- /ORG BOILERPLATE -->

## Features

- **Constrained Baseline Profile** encode + decode (the openh264 feature target).
- **Full intra**: `I_16x16` (4 modes), `I_4x4` (9 directional modes), chroma
  (4 modes), 4×4 integer transform, luma/chroma DC Hadamard, quantization, and
  **CAVLC** entropy coding, with λ-based RD mode decision.
- **Inter / P-frames**: `P_Skip`, `P_L0_16x16`, and `P_16x8`/`P_8x16`
  sub-partitions; **quarter-pel motion compensation** (6-tap luma + bilinear
  chroma), rate-aware motion estimation, ref_idx-aware MV prediction, and
  **multiple reference frames** (`--refs N`, sliding-window DPB).
- **In-loop deblocking filter** with intra and inter boundary strengths.
- **Average-bitrate rate control** — per-frame QP from a complexity model + a
  leaky-bucket buffer (`--bitrate`/`--fps`).
- **Bit-exact against ffmpeg** across the whole QP range (0–51), intra *and*
  inter — the conformance bar this project holds itself to.
- **Annex-B bitstream** with full RBSP emulation-prevention and Exp-Golomb I/O.
- **Permissive license** (BSD-2-Clause) — embed it in closed-source freely.
- **100% safe Rust** core: every codec crate is `#![forbid(unsafe_code)]`. No C,
  no FFI, no `unsafe`.

## Install

```sh
cargo add rusty_h264
```

## Quick start

```rust
use rusty_h264::{Encoder, EncoderConfig, Decoder, YuvFrame};

let mut enc = Encoder::new(EncoderConfig::new(640, 480)).unwrap();
let frame = YuvFrame::black(640, 480);
let bitstream = enc.encode(&frame);     // Annex-B access unit

let mut dec = Decoder::new();
let decoded = dec.decode(&bitstream).unwrap().unwrap();
assert_eq!(decoded, frame);             // a flat frame has no residual → exact
```

The codec is lossy in general (the round-trip is exact only for flat frames like
this one); quality is governed by QP / the bitrate target. To encode a moving
sequence with P-frames and rate control:

```rust
let mut cfg = EncoderConfig::new(640, 480);
cfg.gop_size = 30;            // an IDR every 30 frames, P-frames between
cfg.bitrate = 1_000_000;      // 1 Mbps average; 0 = constant-QP (cfg.qp)
cfg.framerate = 30.0;
let mut enc = Encoder::new(cfg).unwrap();
for frame in &frames { let au = enc.encode(frame); /* … */ }
```

Command-line:

```sh
cargo run -p rusty_h264-cli -- encode --width 352 --height 288 --in in.yuv --out out.264
cargo run -p rusty_h264-cli -- decode --width 352 --height 288 --in out.264 --out roundtrip.yuv
```

## Architecture

The workspace mirrors Cisco openh264's `codec/` tree:

```
crates/
  rusty_h264-common    bitstream I/O, Exp-Golomb, NAL/Annex-B, shared types   (codec/common)
  rusty_h264-encoder   the encode pipeline                                    (codec/encoder)
  rusty_h264-decoder   the decode pipeline (also our in-tree conformance oracle) (codec/decoder)
  rusty_h264           public, safe facade API                                (codec/api)
  rusty_h264-cli       encode/decode command-line tools                       (codec/console)
bench/              deterministic A/B harness vs Cisco (external process)
```

## Benchmarking vs x264 / Cisco

The comparison is produced by [`bench/`](bench/), which feeds an identical,
deterministic synthetic clip to both encoders. **rusty_h264 is pure Rust; the C
baseline (x264 or Cisco openh264) is invoked as a separate external process** (an
`ffmpeg` built with that codec) — it is never linked into or built by this
project.

```sh
cd bench
export RUSTY_H264_BENCH_FFMPEG=/path/to/ffmpeg            # built with libx264
cargo run --release -- --width 352 --height 288 --frames 60 --gop 1   # intra vs x264
cargo run --release -- --width 352 --height 288 --frames 60 --gop 30  # inter (I+P) vs x264
cargo run --release -- --ref-codec libopenh264 --gop 1                # vs Cisco openh264
```

Output size and PSNR are exactly reproducible run-to-run; encode time is the
median of `--runs` repetitions (and the C baseline's time includes process
startup, so treat it as a loose bound — see [docs/benchmarks.md](docs/benchmarks.md)).

## Platform support

| Platform | Status |
|---|---|
| Windows | ✅ builds + tests |
| Linux | ✅ builds + tests |
| macOS | ✅ builds + tests |

Pure Rust, no platform-specific code yet; SIMD acceleration is a later,
feature-gated extension point.

## Roadmap

- [x] Bitstream core: BitWriter/Reader, Exp-Golomb, NAL/Annex-B, emulation prevention
- [x] SPS/PPS, IDR slice header
- [x] Forward/inverse 4×4 integer transform + quantization (+ DC Hadamard)
- [x] CAVLC residual coding (encode + decode)
- [x] Intra `I_16x16` (4 modes), `I_4x4` (9 modes), chroma (4 modes) + SATD mode decision
- [x] In-loop deblocking filter (intra + inter boundary strengths)
- [x] RD mode decision + encoder-speed early-termination
- [x] **Inter prediction**: P-slices, `P_Skip`, `P_L0_16x16`, `P_16x8`/`P_8x16` sub-partitions, quarter-pel motion compensation, rate-aware motion estimation, ref_idx-aware MV prediction, **multiple reference frames** (sliding-window DPB)
- [x] **Rate control**: average-bitrate (complexity model + leaky-bucket buffer), per-frame QP — bit-exact across the full QP range
- [x] **Bit-exact decode agreement with ffmpeg** (the conformance bar) — intra *and* inter, QP 0–51
- [ ] `P_8x8` deeper sub-partitions (8×4 / 4×8 / 4×4) — refinement
- [ ] Full conformance vs JVT bitstream suite

## License

BSD-2-Clause — see [LICENSE](LICENSE). No GPL/LGPL anywhere in the dependency
tree (no C/C++ either; CI-enforceable via `cargo-deny`).

## About Mata Network

<!-- ORG BOILERPLATE — keep identical across repos -->

[Mata Network](https://www.mata.network) builds sovereign, self-hostable
infrastructure. **Remade With Rust** is our open-source home for the
permissively-licensed building blocks that work depends on.

<!-- /ORG BOILERPLATE -->
