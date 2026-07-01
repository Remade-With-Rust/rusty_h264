# rusty_h264

[![crates.io](https://img.shields.io/crates/v/rusty_h264?logo=rust)](https://crates.io/crates/rusty_h264)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264?logo=docsdotrs)](https://docs.rs/rusty_h264)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)

> **rusty_h264** is a ground-up, pure-**Rust** H.264 **encoder and decoder** — a
> clean rebuild of [Cisco openh264](https://github.com/cisco/openh264) (BSD-2/C++):
> a `#![forbid(unsafe_code)]` codec core, permissively licensed, with no C and zero
> copyleft strings (the optional SIMD asm is the one isolated `unsafe` crate; drop
> it with `--no-default-features`). The decoder is validated **bit-exact** against
> Cisco's `h264dec` over openh264's conformance corpus; the encoder is **bit-exact**
> under ffmpeg across the whole QP range.

---

## ⚡ The headline

A pure-**safe-Rust** H.264 codec — **encoder *and* decoder** — that is **bit-exact
against the C reference** on both sides:

- **Decoder:** Constrained Baseline **+ B-slices + most of High profile** (8×8
  transform & intra, scaling lists, weighted prediction, temporal & spatial
  direct) — **35 of openh264's conformance streams decode byte-for-byte identical**
  to Cisco's `h264dec`.
- **Encoder:** Constrained Baseline (intra, P-frames, quarter-pel MC, in-loop
  deblocking, ABR rate control) — every frame decodes **bit-exactly under ffmpeg
  across QP 0–51**.
- **The codec is `#![forbid(unsafe_code)]`.** The `asm` feature (**on by default**)
  links openh264's BSD-2 SIMD kernels — vendored, assembled with `nasm`, quarantined
  in the one `unsafe` crate (`rusty_h264-accel`). It gives a **~1.3–1.45× overall
  speedup** on the motion-heavy paths (decode 1.34×, inter encode 1.44×) and ~1.14× on
  intra encode: the kernels themselves are ~2× faster, but H.264 is entropy- and
  mode-decision-bound, so Amdahl caps the whole-codec gain below 2×. Build
  **`--no-default-features` for 100% safe Rust**: no asm, no `nasm`, no FFI, no
  `unsafe`, portable to any Rust target.

| | x264 / openh264 (C) | **rusty_h264 (Rust)** |
|---|---|---|
| C/C++ in the dependency tree | all of it | **none** (asm is the only non-Rust, and optional) |
| `unsafe` in the codec core | extensive | **0** — `#![forbid(unsafe_code)]` |
| License | GPL / BSD | **BSD-2** (embed freely) |
| Decoder bit-exact vs `h264dec` | — | **35/35 clean corpus streams** |
| Encoder bit-exact vs ffmpeg | — | **QP 0–51, intra + inter** |

### Performance (single core, bit-exact, this machine)

| workload | rusty_h264 | reference |
|---|---:|---:|
| **Decode** 1080p — asm kernels | **145 Mpx/s** | ffmpeg-native `h264` ~590 · **0.25×** |
| **Decode** 1080p — 100% safe Rust | **109 Mpx/s** | ffmpeg-native `h264` ~590 · **0.18×** |
| **Encode** INTER, CIF (vs openh264) | **71 Mpx/s** | 115 · 1.6× |
| **Encode** ALL-INTRA, CIF (vs openh264) | **24 Mpx/s** | 88 · 3.6× |

<sub>**Decode** is benched against **ffmpeg's native `h264` software decoder** — the
fastest widely-available SW H.264 decoder and a deliberately *tougher* bar than
openh264's own `h264dec` (historically ~2× our speed, so 0.25× vs ffmpeg ≈ ~0.5× vs
openh264). Reproducible: `bash bench/decode_speedtest.sh` (differential 160f−40f,
best-of-3, single core, decode-to-null). A 2026 profiling pass built an **rdtsc-accurate
stage profiler** and a series of **byte-identical redundancy-elimination bricks** (skip
B-only motion/ref work on Baseline streams, move-not-clone the DPB reference frame,
pass the deblock filter empty grids it won't use) — lifting scalar decode ~94→110 Mpx/s
and asm decode to ~145 Mpx/s, all bit-exact. Earlier algorithmic wins: an
O(bits·candidates)→O(1) table-driven CAVLC and autovectorization-friendly pixel loops.
**Encode** rows are vs Cisco openh264 (same Baseline/CAVLC class); see the note below.</sub>

On a deterministic CIF clip (scrolling gradient + moving box, 60 frames),
matched QP **and matched reference count** (both encoders at 1 ref, baseline
profile), both outputs decoded by the same ffmpeg for PSNR:

| QP 26 | rusty_h264 (Rust) | x264 (C) | size |
|---|---:|---:|:--:|
| **intra** | 0.291 bpp · 44.1 dB | 0.331 bpp · 45.3 dB | **0.88×** |
| **inter** (I+P) | 0.109 bpp · 47.8 dB | 0.105 bpp · 49.8 dB | **1.03×** |

<sub>On **intra**, rusty_h264 produces **smaller files than x264 at matched QP**,
within ~1 dB PSNR (dead-zone tuning) — roughly rate-distortion competitive. On
**inter**, at matched 1-ref the size gap at QP26 is **~1.03×** (near parity — was
mis-reported larger when x264 was silently given 3 reference frames), rusty_h264
reaches **parity at QP30** (1.01×) and is **smaller than x264 from QP36 up**
(0.83×, 0.78×), after RD-optimized mode decision, rate-aware ME, and
early-termination. x264 stays ahead on PSNR-per-bit (1–3 dB) and exploits
multiple references better (rusty_h264's multi-ref is bit-exact but not yet
RD-beneficial). rusty_h264 trades a little compression for **memory safety, a
permissive license, and zero C in the build — while matching the reference
decoder bit-for-bit across QP 0–51, intra and inter**.
**This caps x264 at Baseline to match** — its *default* High profile (B-frames +
CABAC, which Constrained Baseline forbids by design) is ~1.3× smaller than the
numbers above, a mostly **structural** gap, not an implementation one.
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

**Decoder** (validated bit-exact vs Cisco `h264dec` over openh264's corpus):

- **Constrained Baseline** + **B-slices** (temporal & spatial direct, implicit &
  explicit weighted prediction, the L0/L1/Bi partitions, `B_Skip`/`B_Direct`).
- **Most of High profile (CAVLC):** the 8×8 integer transform and 8×8 intra
  prediction, sequence/picture **scaling matrices**, `transform_size_8x8_flag`,
  second chroma QP offset.
- Full intra (`I_16x16`/`I_4x4`/`I_8x8`/`I_PCM`), inter (`P_Skip`/16×16/16×8/8×16/
  `P_8x8`), quarter-pel motion compensation, in-loop deblocking (incl. 8×8-aware),
  multi-reference DPB with POC reordering and MMCO.
- **CABAC** arithmetic engine + 460-context init **implemented and round-trip
  verified** (the per-syntax-element parsing layer is the next milestone).

**Encoder** (every frame decodes bit-exactly under ffmpeg, QP 0–51):

- Full intra with λ-based RD mode decision; inter P-frames (`P_Skip`/16×16/16×8/
  8×16), quarter-pel MC, rate-aware ME, multiple reference frames.
- In-loop deblocking; **average-bitrate rate control** (complexity model +
  leaky-bucket buffer).

**Shared:**

- **The codec is `#![forbid(unsafe_code)]`** — no `unsafe` anywhere in
  common/encoder/decoder. The **`asm` feature (on by default)** links openh264's
  vendored BSD-2 SIMD kernels (motion compensation, deblocking, transforms),
  quarantined in the one `rusty_h264-accel` crate, for a **~1.3–1.45× overall speedup**
  on motion-heavy paths (the kernels are ~2× but entropy/mode-decision dominate); it
  needs `nasm` to build. **`--no-default-features`** drops it for 100% safe, portable Rust.
- **Annex-B bitstream** with RBSP emulation-prevention and Exp-Golomb I/O.
- **Permissive license** (BSD-2-Clause) — embed it in closed-source freely.

## Install

One crate — `rusty_h264` — is the public facade; it re-exports everything you need
(`Encoder`, `Decoder`, `YuvFrame`, …). Add it with:

```sh
cargo add rusty_h264
```

or in `Cargo.toml`:

```toml
[dependencies]
# asm SIMD on by default (needs `nasm` at build time; kernels are vendored):
rusty_h264 = "0.2"

# …or pure, portable, 100%-safe Rust with no nasm and no unsafe:
rusty_h264 = { version = "0.2", default-features = false }
```

The published crates (all `0.2`, BSD-2):

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | **the facade — depend on this** |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, motion comp |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | decode pipeline |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional openh264 SIMD asm (`unsafe`) |

**Dropping it into `remade_ffmpeg`:** depend on the facade and adapt to the
`rff-codec` `Encoder`/`Decoder` traits — `YuvFrame` (I420 planes) ↔ `VideoFrame`,
and note rusty_h264 speaks **Annex-B** (start codes), so an AVCC↔Annex-B shim is
needed for MP4 inputs. Keep `default-features = false` in CI if you don't want a
`nasm` build dependency there.

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
  rusty_h264-common    bitstream I/O, Exp-Golomb, NAL/Annex-B, transforms, MC   (codec/common)
  rusty_h264-encoder   the encode pipeline                                      (codec/encoder)
  rusty_h264-decoder   the decode pipeline                                      (codec/decoder)
  rusty_h264           public, safe facade API  ← depend on this                (codec/api)
  rusty_h264-cli       encode/decode command-line tools                         (codec/console)
  rusty_h264-accel     vendored openh264 BSD-2 SIMD kernels (the one unsafe crate; on by default, needs nasm)
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
cargo run --release -- --width 352 --height 288 --frames 60 --gop 1            # intra vs x264
cargo run --release -- --width 352 --height 288 --frames 60 --gop 30 --refs 1  # inter (I+P), matched 1 ref
cargo run --release -- --width 352 --height 288 --frames 60 --gop 30 --refs 3  # inter, matched 3 refs
cargo run --release -- --ref-codec libopenh264 --gop 1                         # vs Cisco openh264
```

`--refs` is applied to **both** encoders so the race is fair (without it, x264
would use its default of 3 references and rusty_h264 just 1). Output size and PSNR
are exactly reproducible run-to-run; encode time is the median of `--runs`
repetitions (and the C baseline's time includes process startup, so treat it as a
loose bound — see [docs/benchmarks.md](docs/benchmarks.md)).

**Decode** speed is a separate, differential head-to-head vs ffmpeg's native `h264`
software decoder (spawn/init cost cancels between a long and a short stream):

```sh
cargo build --release -p rusty_h264-cli --features asm   # or --no-default-features for safe Rust
bash bench/decode_speedtest.sh                # 720p; args: W H N1 N2 (e.g. 1920 1080 40 160)
```

## Platform support

| Platform | Status |
|---|---|
| Windows | ✅ builds + tests |
| Linux | ✅ builds + tests |
| macOS | ✅ builds + tests |

The `asm` feature (x86-64 SIMD) is **on by default** and needs `nasm` on `PATH`
(`apt install nasm` / `brew install nasm` / [nasm.us](https://nasm.us)); the
kernels are vendored, so no openh264 checkout is required. Build
**`--no-default-features`** for portable, 100%-safe pure Rust with no `nasm` and no
`unsafe` — it runs on any Rust target.

## Roadmap

- [x] Bitstream core, SPS/PPS (incl. High-profile extensions), slice headers
- [x] 4×4 **and 8×8** integer transforms + quantization, scaling matrices, DC Hadamard
- [x] **CAVLC** residual coding — encode + decode (table-driven O(1) decode)
- [x] Intra `I_16x16`/`I_4x4`/`I_8x8`/`I_PCM`, chroma; SATD/RD mode decision
- [x] In-loop deblocking (intra + inter strengths, 8×8-transform-aware)
- [x] **Encoder** P-frames: `P_Skip`/16×16/16×8/8×16, quarter-pel MC, rate-aware ME, multi-ref DPB, ABR rate control
- [x] **Encoder bit-exact vs ffmpeg**, intra + inter, QP 0–51
- [x] **Decoder B-slices**: temporal/spatial direct, implicit/explicit weighted prediction, `B_Skip`/`B_Direct`/B-partitions
- [x] **Decoder High profile (CAVLC)**: 8×8 transform & intra, scaling lists, weighted pred — 35/35 clean corpus streams bit-exact vs `h264dec`
- [x] **openh264 SIMD asm** (MC/deblock/transform) — vendored + self-contained, **on by default** (needs `nasm`)
- [x] **Decoder speed pass**: rdtsc-accurate stage profiler + byte-identical redundancy bricks (Baseline B-skip, DPB move-not-clone, deblock empty grids) — scalar ~94→110, asm ~145 Mpx/s @ 1080p
- [x] **Encoder asm SATD** wired into the quality-preset mode decision (`2·WelsSampleSatd`, byte-identical via the always-even-Hadamard `×2` identity) — quality inter ME **1.7×**
- [x] **CABAC engine** + context init (round-trip verified)
- [ ] **CABAC syntax layer** (mb_type/intra/cbp/qp/residual) — unlocks the `*cabac*` streams
- [ ] Full conformance vs the JVT bitstream suite

## License

BSD-2-Clause — see [LICENSE](LICENSE). No GPL/LGPL anywhere in the dependency
tree (no C/C++ either; CI-enforceable via `cargo-deny`).

## About Mata Network

<!-- ORG BOILERPLATE — keep identical across repos -->

[Mata Network](https://www.mata.network) builds sovereign, self-hostable
infrastructure. **Remade With Rust** is our open-source home for the
permissively-licensed building blocks that work depends on.

<!-- /ORG BOILERPLATE -->
