# rusty_h264

[![crates.io](https://img.shields.io/crates/v/rusty_h264?logo=rust)](https://crates.io/crates/rusty_h264)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264?logo=docsdotrs)](https://docs.rs/rusty_h264)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)

> **rusty_h264** is a ground-up, pure-**Rust** H.264 **encoder and decoder**:
> a `#![forbid(unsafe_code)]` codec core, permissively licensed, with no C and
> zero copyleft strings. Acceleration is **pluggable** — the default path ships
> optimized SIMD kernels (assembled with `nasm`), and the same surface accepts
> **custom kernels or hand-written ASM** so you can push speed further without
> touching the safe core. The decoder is validated **bit-exact** against Cisco’s
> `h264dec` over openh264’s conformance corpus; the encoder is **bit-exact**
> under ffmpeg across the whole QP range.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)** — the H.264 codec inside
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**,
our memory-safe FFmpeg alternative, alongside
**[FFAI](https://github.com/Remade-With-Rust/FFAI)**, the AI media toolkit.
[Jump to the ecosystem ↓](#the-remade-with-rust-ecosystem)

---

## ⚡ The headline

A pure-**safe-Rust** H.264 codec — **encoder *and* decoder** — that is **bit-exact
against the C reference** on both sides, with a clean acceleration surface designed
for custom kernels and ASM:

- **Decoder:** Constrained Baseline **+ B-slices + most of High profile** (8×8
  transform & intra, scaling lists, weighted prediction, temporal & spatial
  direct) — **35 of openh264’s conformance streams decode byte-for-byte identical**
  to Cisco’s `h264dec`. **CABAC entropy decode** (Main profile) is live: I/P/B
  slices incl. I_4x4 + I_16x16 intra, all P/B partition types and spatial/temporal
  direct decode **pixel-exact vs ffmpeg**, verified symbol-by-symbol against an
  instrumented openh264 oracle. The decoder is **fuzzed to never panic or hang**
  on malformed input.
- **Encoder:** Baseline **and Main** — intra, P-frames, quarter-pel MC, in-loop
  deblocking, ABR rate control, with **CABAC entropy coding default-on**
  (−8.8…−9.0% BD-rate for 1.10–1.22× the time), **adaptive quantization**, a
  per-GOP I-frame QP cascade, and opt-in **B-frames**, **8×8 transform**
  (High profile) and **mb-tree** temporal AQ. Every frame decodes **bit-exactly
  under ffmpeg across QP 0–51**.
- **The codec core is `#![forbid(unsafe_code)]`.** All pixel-level work (motion
  compensation, transforms, deblocking, SATD, etc.) lives behind a thin
  acceleration boundary. The default `asm` feature (on by default) supplies
  optimized SIMD kernels; the same boundary accepts **your own custom kernels or
  hand-written ASM**. Drop acceleration entirely with `--no-default-features`
  for 100 % safe, portable Rust (no `nasm`, no FFI, no `unsafe`).
- **Performance is a requirement, so `unsafe` and asm are allowed — deliberately** Builds from this workspace also target **`x86-64-v3` (AVX2)** — a codegen flag, not a code change, so it costs no safety, only a 2013-or-newer CPU floor
  ([details](#isa-baseline-this-workspace-builds-for-x86-64-v3-avx2)).

| | x264 / openh264 (C) | **rusty_h264 (Rust)** |
|---|---|---|
| C/C++ in the dependency tree | all of it | **none** (acceleration is optional and isolated) |
| `unsafe` in the codec core | extensive | **0** — `#![forbid(unsafe_code)]` |
| License | GPL / BSD | **BSD-2** (embed freely) |
| Decoder bit-exact vs `h264dec` | — | **35/35 clean corpus streams** |
| Encoder bit-exact vs ffmpeg | — | **QP 0–51, intra + inter** |
| Custom kernels / ASM | — | **first-class** — plug in your own for extra speed |

### Performance (single core, bit-exact, this machine)

**Decode — measured against ffmpeg's native `h264` software decoder**, the fastest
widely-available SW H.264 decoder and a deliberately *tougher* bar than openh264's own
`h264dec`. 1800 frames of real 720p content (shields / in_to_tree / stockholm),
**encoded by x264** rather than by us, because what an encoder puts in the stream
dominates decode cost:

| x264 tool tier | rusty_h264 | ffmpeg native `h264` | gap |
|---|---:|---:|---:|
| baseline / CAVLC (`--preset veryfast`) | **150 Mpx/s** | 314 Mpx/s | **2.34×** |
| main / CABAC (`--preset medium`) | **107 Mpx/s** | 289 Mpx/s | **2.70×** |
| high (`--preset slower`) | **85 Mpx/s** | 239 Mpx/s | **2.49×** |

| encode workload | rusty_h264 | reference |
|---|---:|---:|
| **Encode** INTER, CIF (vs openh264) | **71 Mpx/s** | 115 · 1.6× |
| **Encode** ALL-INTRA, CIF (vs openh264) | **24 Mpx/s** | 88 · 3.6× |

On a deterministic CIF clip (scrolling gradient + moving box, 60 frames),
matched QP **and matched reference count** (both encoders at 1 ref, baseline
profile), both outputs decoded by the same ffmpeg for PSNR:

| QP 26 | rusty_h264 (Rust) | x264 (C) | size |
|---|---:|---:|:--:|
| **intra** | 0.291 bpp · 44.1 dB | 0.331 bpp · 45.3 dB | **0.88×** |
| **inter** (I+P) | 0.109 bpp · 47.8 dB | 0.105 bpp · 49.8 dB | **1.03×** |

<sub>On **intra**, rusty_h264 produces **smaller files than x264 at matched QP**,
within ~1 dB PSNR (dead-zone tuning) — roughly rate-distortion competitive. On
**inter**, at matched 1-ref the size gap at QP26 is **~1.03×** (near parity —
was mis-reported larger when x264 was silently given 3 reference frames),
rusty_h264 reaches **parity at QP30** (1.01×) and is **smaller than x264 from
QP36 up** (0.83×, 0.78×), after RD-optimized mode decision, rate-aware ME, and
early-termination. x264 stays ahead on PSNR-per-bit (1–3 dB) and exploits
multiple references better (rusty_h264’s multi-ref is bit-exact but not yet
RD-beneficial). rusty_h264 trades a little compression for **memory safety, a
permissive license, and zero C in the build — while matching the reference
decoder bit-for-bit across QP 0–51, intra and inter**.
**This table caps x264 at Baseline to match**, which is what the numbers above
compare. That caveat has since been overtaken on our side: the tools
Constrained Baseline forbids by design — **CABAC and B-frames** — are now built
and conformant here, so the comparison no longer has to be capped.
Methodology + full RD sweep: [`bench/`](bench/), [docs/benchmarks.md](docs/benchmarks.md).</sub>

**Where the encoder stands against x264 today.** Measured over a CIF corpus at
4 QPs (2026-07), the honest summary is that the remaining gap is **feature
coverage and inter coding**, not core efficiency:

- **All-intra: we beat x264** (−0.9% BD-rate at matched intra tooling).
- **At matched feature sets on natural content: ~2% behind.** Each tool we ship
  — CABAC, AQ, mb-tree, B-frames, sub-pel — measurably *subtracts* from the gap.
- **Against x264 `medium` at its defaults: ~30% behind**, which is the price of
  the features we have not built yet rather than of the ones we have.
- The isolated outlier was a **~22% P-16×16 inefficiency on smooth synthetic
  content**, root-caused to the motion-search diamond stalling on flat cost
  surfaces and since addressed by the adaptive wide search and rescue-grid work
  (`me_wide`, default-on for the `Quality` preset).

See [docs/WHYS-inter-gap.md](docs/WHYS-inter-gap.md) for the full descent and
[docs/lets-win-optimize.md](docs/lets-win-optimize.md) for the speed campaign.

---

## What is this?

`rusty_h264` decodes and encodes H.264 in pure, safe Rust — Baseline and Main on
both sides (CAVLC and CABAC), plus most of High profile on decode and the 8×8
transform on encode. Unlike the existing [`openh264-rs`](https://github.com/ralfbiedert/openh264-rs)
bindings — which vendor Cisco’s C source and call it over FFI, offering “no
additional safety guarantees” — there is **no C in the dependency tree** here.
The codec core is `#![forbid(unsafe_code)]`, BSD-2 licensed, and embeddable in
closed-source software with no copyleft obligations. It is a reimplementation
of the algorithms, not a wrapper around the original.

Acceleration is deliberately separated so that the safe core never changes when
you want more speed. The default kernels already deliver a solid ~1.3–1.45×
overall speedup on motion-heavy paths; the same interface lets you drop in
**custom kernels or hand-written ASM** for further gains.

## The Remade With Rust ecosystem

<!-- ORG BOILERPLATE — keep identical across repos -->

**Remade With Rust** is an initiative by **[Mata Network](https://www.mata.network/)**
to rebuild essential C and C++ tools in Rust — for the memory safety, the
predictable performance, and the freedom of a permissive license. Each project
is a reimplementation, not a fork: same wire protocols and file formats, new
code you can actually depend on.

We build the core to production grade and open-source it so the community can
extend it. No copyleft. No surprises. Just the tools we rely on, made faster and
safer.

| Project | What it is |
|---|---|
| 🎬 **[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)** | **Our FFmpeg alternative.** Drop-in `ffmpeg` and `ffprobe` binaries — demux → decode → filter → encode → mux, rebuilt as composable Rust crates with **zero GPL/LGPL**. Apache-2.0. `rusty_h264` is its H.264 codec. |
| 🧠 **[FFAI](https://github.com/Remade-With-Rust/FFAI)** | **Our sister project: media *for* AI.** "The AI media toolkit, remade with rust." Embedded ASR + TTS (**Mercury**), OCR (**Carmenta**) and vision-language captioning (**Argus**) behind an ffmpeg-style, swap-by-name architecture — no Python, no CUDA. MIT OR Apache-2.0. |
| 🌐 **[Mata Network](https://www.mata.network/)** | **The home page.** *"Stop sacrificing your privacy for convenience."* Sovereign, self-hostable privacy infrastructure — wallet & identity, password manager, contact manager, and a browser extension that stops information leaking as you browse. Remade With Rust is its open-source arm. |

→ All projects: **[github.com/Remade-With-Rust](https://github.com/Remade-With-Rust)**

<!-- /ORG BOILERPLATE -->

## Custom kernels & ASM for speed

The acceleration boundary is the intentional place for speed work.

- Default path (`asm` feature, enabled by default): optimized SIMD kernels for
  motion compensation, deblocking, transforms and SATD. Assembled with `nasm`,
  quarantined in the single `rusty_h264-accel` crate. Gives the ~1.3–1.45×
  overall numbers shown above.
- **Custom kernels / ASM**: the same surface accepts your own implementations.
  You can replace individual kernels (or the whole set) with hand-written
  assembly, target-specific intrinsics, or pure-Rust alternatives tuned for
  your workload / micro-architecture. The safe core never sees `unsafe` and
  never needs to be recompiled when you swap kernels.
- Fully safe path: `--no-default-features` disables every acceleration crate.
  Result is 100 % safe, portable Rust with no `nasm`, no FFI and no `unsafe`.

This design keeps the bit-exact guarantees of the core intact while letting
you (or downstream projects such as `remade_ffmpeg`) push the performance
envelope with whatever kernels make sense for the target.

### Where `unsafe` and hand-written assembly are allowed

Performance is a requirement here, so `unsafe` and asm are **permitted where they
are justified** — but deliberately and in one place, not scattered:

- **`rusty_h264-accel` is the designated boundary.** It is the only crate that is
  not `#![forbid(unsafe_code)]`, because it links hand-written assembly through FFI.
  New SIMD, intrinsics or asm belongs here.
- **`common` / `encoder` / `decoder` stay `#![forbid(unsafe_code)]`.** They are the
  bulk of the codec, and keeping them safe is what makes the acceleration boundary
  auditable — the `unsafe` surface is small enough to review in full.
- **Every kernel keeps its scalar twin as the oracle and the fallback**, gated
  byte-identical (integer paths) against it and reachable on any CPU without the ISA.
- **A kernel earns its place by measurement.** Bricks that do not measure faster are
  reverted, and the ones reverted for a *reason* keep their measurement recorded so
  the idea is not re-litigated. Several hand-SIMD attempts here were reverted after
  proving flat — a kernel gated by strided memory loads does not get faster by
  widening it.

The practical order, in decreasing payoff and increasing risk: build-flag ISA
(free, no code change) → algorithmic redundancy removal (byte-identical, safe Rust)
→ auto-vectorization-friendly restructuring → explicit SIMD in `accel` → hand asm.
Reach for the last two only when the profile names the kernel *and* you can say why
the compiler could not do it.

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
rusty_h264 = "0.7"

# …or pure, portable, 100%-safe Rust with no nasm and no unsafe:
rusty_h264 = { version = "0.7", default-features = false }
```

The published crates (all `0.7`, BSD-2):

| Crate | Role | Docs |
|---|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | **the facade — depend on this** | [README](crates/rusty_h264/README.md) · [docs.rs](https://docs.rs/rusty_h264) |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, prediction, MC, deblock | [README](crates/rusty_h264-common/README.md) · [docs.rs](https://docs.rs/rusty_h264-common) |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | encode pipeline | [README](crates/rusty_h264-encoder/README.md) · [docs.rs](https://docs.rs/rusty_h264-encoder) |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | decode pipeline | [README](crates/rusty_h264-decoder/README.md) · [docs.rs](https://docs.rs/rusty_h264-decoder) |
| [`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel) | optional openh264 SIMD asm — the one `unsafe` crate | [README](crates/rusty_h264-accel/README.md) · [docs.rs](https://docs.rs/rusty_h264-accel) |

Not published, but in the repo: [`rusty_h264-cli`](crates/rusty_h264-cli/README.md),
the console encode/decode front-end.

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
- [x] **Decoder speed pass**: rdtsc-accurate stage profiler + byte-identical redundancy bricks (Baseline B-skip, DPB move-not-clone, deblock empty grids). *(The Mpx/s figures once quoted here came from a differential harness later shown to be unsound — see the Performance section for the current paired numbers.)*
- [x] **Encoder asm SATD** wired into the quality-preset mode decision (`2·WelsSampleSatd`, byte-identical via the always-even-Hadamard `×2` identity) — quality inter ME **1.7×**
- [x] **CABAC engine** + context init (round-trip verified)
- [x] **CABAC decode — I, P and B slices** (Main profile): full syntax parse verified
  symbol-by-symbol against an instrumented openh264 oracle, wired into recon —
  **decodes pixel-exact vs ffmpeg**
- [x] **CABAC encode — I, P and B slices**, default-on (−8.8…−9.0% BD-rate);
  trellis RDOQ default-on for all-intra; multi-reference P (`ref_idx_l0`)
- [x] **Decoder hardening**: mutation fuzzing with committed CABAC seeds — zero
  panics, zero hangs; three DoS-class bugs found and regression-gated
- [x] **Encoder B-frames** — conformant, with the content-adaptive `--bframes auto`
  enable that captures the win and never regresses
- [x] **Adaptive quantization** (spatial, default-on) + **mb-tree temporal AQ**
  (opt-in, three lookahead resolutions)
- [x] **8×8 transform in the encoder** — `I_8x8` + inter, content-adaptive dispatch
- [x] **`P_8x8` sub-partition motion** and the **adaptive wide motion search**
  (fixes the diamond stalling on flat cost surfaces) — default-on for `Quality`
- [x] **Decoder bS pipeline (Aug 2026)**: packed per-macroblock boundary-strength records
  (`MbPack`) + two AVX2 kernels (motion masks, uniform-motion test), byte-identical, with
  scalar twins kept as oracles — **12/15 paired, z = 2.32**, default-on
  (`RS_H264_BS_PACKED=0` opts out). Workspace now builds `-C target-cpu=x86-64-v3`: the
  safe core previously emitted **zero AVX2** (0 ymm vs 1,463)
- [ ] CABAC `I_PCM` and High-profile 8×8 CABAC residual (decode)
- [ ] Sub-8×8 shapes (8×4 / 4×8 / 4×4) within a `P_8x8`
- [ ] Full conformance vs the JVT bitstream suite

## License

BSD-2-Clause — see [LICENSE](LICENSE). No GPL/LGPL anywhere in the dependency
tree (no C/C++ either; CI-enforceable via `cargo-deny`).

## About Mata Network

<!-- ORG BOILERPLATE — keep identical across repos -->

**[Mata Network](https://www.mata.network/)** builds sovereign, self-hostable
privacy infrastructure — *"stop sacrificing your privacy for convenience"*:
wallet & identity, a password manager, a contact manager, and a browser
extension that stops your information leaking as you browse.

**Remade With Rust** is our open-source home for the permissively-licensed
building blocks that work depends on — including
[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs) (the
FFmpeg alternative) and [FFAI](https://github.com/Remade-With-Rust/FFAI) (the
AI media toolkit).

→ **[www.mata.network](https://www.mata.network/)**

<!-- /ORG BOILERPLATE -->
