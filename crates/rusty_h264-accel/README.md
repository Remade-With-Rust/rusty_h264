# rusty_h264-accel

[![crates.io](https://img.shields.io/crates/v/rusty_h264-accel?logo=rust)](https://crates.io/crates/rusty_h264-accel)
[![docs.rs](https://img.shields.io/docsrs/rusty_h264-accel?logo=docsdotrs)](https://docs.rs/rusty_h264-accel)
[![CI](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml/badge.svg)](https://github.com/remade-with-rust/rusty_h264/actions/workflows/ci.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/license-BSD--2--Clause-blue)](https://github.com/remade-with-rust/rusty_h264/blob/main/LICENSE)
[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network/)

> **The acceleration crate — and the *only* crate in the workspace that contains
> `unsafe`.** It vendors Cisco openh264's BSD-2 x86-64 SIMD assembly kernels,
> assembles them with `nasm`, and exposes them behind safe Rust wrappers so the
> rest of [`rusty_h264`](https://crates.io/crates/rusty_h264) can stay
> `#![forbid(unsafe_code)]` while still running at SIMD speed.

Part of **[Remade With Rust](https://github.com/Remade-With-Rust)** by
**[Mata Network](https://www.mata.network/)**.

---

## The design rule

The codec has one `unsafe` boundary, and this is it.

```
rusty_h264-common / -encoder / -decoder     #![forbid(unsafe_code)]
        │  call only safe wrappers
        ▼
rusty_h264-accel                            the FFI + `unsafe` quarantine
        │  extern "C"
        ▼
vendored openh264 .asm  →  nasm  →  .obj
```

Every kernel here has a **pure-Rust scalar twin** in
[`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) that acts as
its oracle: `satd_asm_compare` / `satd_avg_compare` tests assert the asm and the
scalar path agree, and every speed brick is gated **byte-identical** on full
bitstreams. Turn the feature off and you get 100% safe, portable Rust with
identical output — just slower.

The same boundary is the intended place to plug in **your own kernels**: custom
intrinsics, hand-written assembly, or a pure-Rust implementation tuned for your
micro-architecture. The safe core never sees `unsafe` and never needs to change.

## What's vendored

The `.asm` files under `vendor/` are copied **verbatim** from
[Cisco openh264](https://github.com/cisco/openh264)
(`codec/{common,encoder,decoder}/…/x86/`), which is **BSD-2-Clause** — © 2013
Cisco Systems, see `vendor/LICENSE.openh264`. Vendoring makes this crate
self-contained: **no external openh264 checkout is needed**, only `nasm`.

Kernel families wrapped today:

| Area | Kernels |
|---|---|
| Cost metrics | `WelsSampleSatd{4x4,8x8,16x8,8x16,16x16}_sse2`, `WelsSampleSad{16x16,16x8,8x16}_sse2` |
| Transforms | `WelsDctFourT4_sse2`, `WelsQuantFour4x4_sse2`, inverse DCT + reconstruct |
| Motion compensation | `McHorVer20/02/22` (half-pel horizontal, vertical, centre), chroma MC width-8 |
| Deblocking | luma & chroma `Lt4`/`Eq4`, vertical & horizontal, incl. the H↔V transposes |
| Intra prediction | `WelsI16x16LumaPred{V,H,Dc,Plane}_sse2`, `WelsIChromaPred{V,Plane}_sse2` |

Plus `satd_avg`, `satd_16x16_x4` and `sad_16x16_x4` — **custom fused kernels
written for this project** (not openh264's), which compute a bi-prediction
average and its SATD in one pass, and batch four candidate costs per call for
the motion search.

## Platform behaviour

| Situation | What happens |
|---|---|
| **x86-64 + `nasm` on `PATH`** | Kernels are assembled and linked. Full speed. |
| **x86-64, no `nasm`** | The build script skips assembly with a warning and the crate still compiles as a lib — so it stays publishable and docs.rs-buildable. Enabling `asm` in a *binary* then surfaces a clear link error rather than a build-script panic. |
| **Any other architecture** (e.g. arm64 macOS) | The whole module is gated on `#![cfg(target_arch = "x86_64")]`: the crate compiles to an **empty lib**, `nasm` is never invoked, no x86 objects are linked, and the consumer crates fall back to the pure-Rust scalar path via the internal `accel` cfg. A downstream default-features build works on Apple Silicon unchanged. |

Environment overrides: `NASM` (path to the assembler) and `OPENH264_DIR`
(build against a live openh264 tree instead of `vendor/`, for development).

## You probably don't depend on this directly

Enable it through the facade instead:

```toml
# SIMD on (the default) — needs nasm:
rusty_h264 = "0.2"

# Pure safe Rust, no nasm, no unsafe anywhere:
rusty_h264 = { version = "0.2", default-features = false }
```

The `asm` feature on `rusty_h264`, `rusty_h264-encoder`, `rusty_h264-decoder`
and `rusty_h264-common` is what pulls this crate in.

## Where this sits

| Crate | Role |
|---|---|
| [`rusty_h264`](https://crates.io/crates/rusty_h264) | the public, safe facade API — **depend on this** |
| [`rusty_h264-common`](https://crates.io/crates/rusty_h264-common) | bitstream I/O, transforms, prediction, MC, deblock |
| [`rusty_h264-encoder`](https://crates.io/crates/rusty_h264-encoder) | the encode pipeline |
| [`rusty_h264-decoder`](https://crates.io/crates/rusty_h264-decoder) | the decode pipeline |
| **[`rusty_h264-accel`](https://crates.io/crates/rusty_h264-accel)** | **← you are here** — the SIMD asm + the one `unsafe` boundary |

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
The vendored assembly is BSD-2-Clause © 2013 Cisco Systems (`vendor/LICENSE.openh264`).
No GPL/LGPL anywhere.
