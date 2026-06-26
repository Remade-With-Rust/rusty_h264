---
name: asm-campaign-verdict
description: openh264 asm is ~2x faster than our real vectorized kernels (verified); the wiring campaign + its gating constraints
metadata:
  type: project
---

**Verdict (kernel_bench, aligned 16×16): openh264's SSE2 asm is ~2× faster than our
REAL vectorized kernels** — not 1× (our scalar isn't as good as hand-tuned asm) and not
10–36× (that's only vs *naive* scalar, the misleading trap the test kit exists to avoid,
same lesson as the DCT batching):
- SAD16×16: ours 88 M/s (`psadbw` auto-vec via `u8::abs_diff`), asm 210 M/s → **2.4×**
- SATD16×16: ours 11 M/s (`wide` SIMD `satd_4x4_sum`), asm 22 M/s → **2.0×**

This IS the openh264 gap mechanism (2.1× inter): openh264 *is* these asm kernels. So
wiring them is worth ~2× **per kernel** — but end-to-end is diluted: kernels are ~half the
encode (rest is CAVLC, control flow, memory), and the existing DCT/IDCT asm (already wired
for inter luma) moved the whole encode only +3–6%. **Realistic end-to-end ≈ 1.3–1.5×**
(inter ~50 → ~65 Mpx/s, closing toward openh264's ~102), not a 2× flip.

**Foundation built (committed):** accel/build.rs assembles all 23 openh264 asm files;
FFI + byte-exact tests for SAD (16x16/16x8/8x16), SATD (8x8/16x8/8x16/16x16), DCT, IDCT;
`kernel_bench` example is the per-kernel scalar-vs-asm test kit. Run:
`cargo run --release -p rusty_h264-accel --example kernel_bench`.

**Gating constraint for wiring the spatial kernels:** the SSE2 kernels use aligned
(`movdqa`) loads → 16-byte-aligned input, 16-multiple stride. The encoder stays
`forbid(unsafe)` even with `--features asm`, so the aligned buffer must be a safe
`#[repr(align(16))] struct A([u8;256])` (the SOURCE MB copied once per MB ME; the
reference is `movdqu`-tolerant — TODO verify op2 unaligned). Licensing: x264 asm is
**GPL (off-limits)**; openh264 asm is **BSD-2 (ours to use)** — see [[openh264-baseline-build]].

**How to apply / order:** wire in impact order, re-run kernel_bench + the 3-codec
speedtest after each. SAD/SATD (ME) → quant → deblock (needs common-crate plumbing) →
MC sub-pel → intra-pred. Only keep an asm wire if kernel_bench AND the end-to-end
speedtest both improve (don't trust the naive-scalar ratio).
