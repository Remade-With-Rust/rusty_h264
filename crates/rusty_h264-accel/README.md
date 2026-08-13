# rusty_h264-accel

SIMD kernels for `rusty_h264` — **pure Rust, no assembly**.

This crate is the codec's one `unsafe` boundary: `core::arch` intrinsics
(x86-64 SSE2/AVX2, aarch64 NEON) behind safe wrappers, each pinned to a scalar
twin by differential tests. The codec core stays `#![forbid(unsafe_code)]`.

## History

The crate began as FFI wrappers over ~19,000 lines of vendored openh264 NASM
(BSD-2 — attribution kept in `LICENSE.openh264`, since the algorithms and
tables remain openh264-derived). The rip-ASM campaign
(`docs/add_SIMD_rip_ASM.md`) replaced every kernel with portable Rust,
byte-identical at each step, and the final assembly was deleted on 2026-08-12.
There is no `nasm` dependency and no build script.

Kernel families:

| Area | Module |
|---|---|
| Motion compensation (luma 6-tap/qpel, chroma bilinear, half-pel planes) | `luma_mc`, `chroma_mc`, `hpel` |
| Deblocking (luma/chroma, lt4/eq4, V/H) | `deblock_simd` |
| Cost metrics (SATD/SAD, fused avg+SATD, x4 batches, ME context) | `satd_sad`, `satd_avg`, `mectx` |
| Transform + quant (fwd/inv 4x4, openh264 quantizer) | `transform_quant` |
| Intra prediction (16x16 luma, 8x8 chroma) | `intra_pred` |
| bS derivation / dequant AVX2 helpers | `x86_asm` (historical name; pure Rust) |

## Platform behaviour

x86-64 uses the SSE2 baseline (no runtime detection) with AVX2 selected where
profitable; aarch64 uses NEON where implemented and the scalar twins elsewhere;
every other target runs the scalar oracles. All paths are bit-identical by
construction and by test.
