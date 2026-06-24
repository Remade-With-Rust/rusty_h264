# Project notes — pure-Rust H.264

Research notes for a ground-up Rust rebuild of Cisco's OpenH264, for the
**Remade With Rust** (Mata Network) initiative.

## The opportunity

| | Cisco openh264 | ralfbiedert/openh264-rs | **This project** |
|---|---|---|---|
| Language | C++/C/asm | Rust **FFI wrapper** over the C lib | **Pure Rust** |
| Memory safety | No | No ("no additional safety guarantees") | **Yes — safe Rust core** |
| License | BSD-2 | BSD-2 (+ vendored C) | BSD-2 / permissive, no C |

`openh264-rs` does *not* reimplement anything — it vendors the C source and
binds it via `cc`/FFI. The "Remade With Rust" value proposition is the thing it
explicitly doesn't provide: a memory-safe, copyleft-free, dependency-free Rust
implementation you can actually audit and embed.

### Notes from openh264-rs (API shape worth matching)
- Three crates: `openh264` (idiomatic API), `openh264-sys2` (FFI), `gfx` (YUV↔RGB).
- Decoder: `Decoder::new()`, `decoder.decode(&packet) -> DecodedYUV`.
- Encoder: `Encoder::new()`, `encoder.encode(&yuv) -> EncodedBitstream`.
- Helper: `nal_units(&[u8])` iterator splitting an Annex-B bitstream into NALs.
- YUV↔RGB conversion is SIMD-accelerated; BT.601/BT.709 noted as wanted.
- They treat Cisco's `h264dec` reference decoder as the **authoritative oracle**
  for conformance — we should do the same (bit-exact comparison).

## Scope of Cisco openh264 (what "feature complete" means)
- **Constrained Baseline Profile, up to Level 5.2.**
- Max frame size 36,864 macroblocks; arbitrary resolutions (not just 16x16 mult).
- Encoder: rate control + adaptive quant, temporal scalability (≤4 layers),
  multiple slices, simulcast, long-term reference frames.
- Decoder: multi-reference frames, long-term refs, MMCO, flexible slices.
- Codebase layout: `codec/{encoder,decoder,common,console}`, `test` (gtest),
  `res` (YUV + bitstream fixtures).

## Decoder pipeline (the build order for a CBP decoder)
1. **Bitstream / NAL layer** — Annex-B start-code scan, emulation-prevention
   (`00 00 03`) removal, RBSP extraction, Exp-Golomb (ue/se) bit reader.
2. **Parameter sets** — SPS / PPS parsing.
3. **Slice header** parsing (slice type, frame_num, ref pic list config).
4. **Macroblock layer** — mb_type, CBP, prediction modes parsing (CAVLC only —
   CBP has no CABAC, which simplifies things massively).
5. **Entropy decode** — CAVLC residual coefficient decoding.
6. **Prediction** — Intra 4x4 / 16x16 / chroma; Inter motion comp (P-slices),
   quarter-pel luma interpolation, MV prediction.
7. **Reconstruction** — inverse quant, 4x4 integer IDCT, residual + pred add.
8. **Deblocking filter** (in-loop).
9. **DPB** — decoded picture buffer, reference management (sliding window + MMCO),
   POC, output reordering.

CBP = **no B-frames, no CABAC, no interlace** → big simplification vs full H.264.

## Conformance strategy
- Oracle: Cisco `h264dec` (or ffmpeg) — decode same bitstream, compare YUV
  bit-exact, fail loud on mismatch.
- Use JVT/ITU conformance bitstreams + openh264's own `res/` fixtures.
- Property: re-encode → decode round-trips once encoder lands.

## Status (generation 1 — landed)
- Workspace scaffolded mirroring `codec/` (common, encoder, decoder, api, cli) + `bench/`.
- `rusty_h264-common`: MSB-first BitWriter/BitReader, Exp-Golomb ue/se (spec
  Table 9-2/9-3 verified), NAL header, Annex-B framing, RBSP emulation
  prevention/removal — all unit-tested + round-tripped.
- Encoder: SPS/PPS generation (CBP subset), IDR slice header, **all-`I_PCM`
  slice data**. Output is a fully conformant, losslessly-decodable Annex-B
  stream. Chosen as gen-1 because `I_PCM` needs no transform/CAVLC/prediction
  yet still exercises the entire bitstream + framing path end to end.
- Decoder: parses SPS/PPS/slice headers + reconstructs `I_PCM` MBs, applies
  SPS crop window. Serves as our in-tree conformance oracle.
- End-to-end tests: encode→decode is bit-exact at aligned, cropped (non-/16),
  small, wide, and multi-frame sizes. 25 tests green, 0 warnings.
- `bench/`: deterministic A/B harness. rusty_h264 side runs now (~81 Mpx/s CIF,
  12.07 bits/px = raw 4:2:0 + header overhead, ∞ PSNR/lossless). Cisco side is
  an **external ffmpeg+libopenh264 process** — never built into our tree.

### Decisions locked
- **No C/C++ in our build, ever** (user directive). The codec is a pure-Rust
  *reimplementation*, not FFI bindings. Cisco only appears as an external
  baseline binary in the benchmark.
- Encoder-first (per user). Decoder grows alongside as the oracle.

### Generation 2 — COMPLETE (compressing, bit-exact vs ffmpeg)
The encoder now produces real compressed all-intra `I_16x16` streams that ffmpeg
decodes **bit-exactly** (10/10 across resolutions, cropped dims, gradient +
random content). Apples-to-apples CIF QP26 intra vs x264: **1.38× faster**
(110 vs 80 Mpx/s), 1.55× larger, −2.1 dB PSNR — excellent for DC-only intra.

Key debugging lesson: the last ±1 mismatch vs ffmpeg was **not a bug** — ffmpeg
applies the in-loop deblocking filter by default and we don't yet. Signalling
`disable_deblocking_filter_idc = 1` made our (un-filtered) reconstruction
bit-identical. Implementing the deblocking filter is the path to enabling it.

### Generation 2 layer notes
- **L1 DONE & verified:** 4×4 integer transform (forward/inverse core) + quant/
  dequant in `rusty_h264-common/src/transform.rs`. Spec tables (normAdjust + MF),
  exact DC scaling (16×4=64), quant→dequant round-trips within the quant step
  across QP 0–51. 30 workspace tests green, clippy clean.
- **L2 CAVLC DONE & verified.** Full entropy coder in `cavlc.rs` — exact
  coeff_token (4 nC tables + chroma-DC), total_zeros (+chroma-DC), run_before
  tables transcribed from ffmpeg's `h264_cavlc.c`; level prefix/suffix with
  adaptive suffixLength + escapes; zig-zag scan. Encode/decode are an exact
  inverse pair: 2000 pseudo-random blocks round-trip across all nC contexts and
  block sizes, plus large-level escape + chroma-DC cases.
- **L3a DC Hadamard DONE & verified.** Luma 4×4 + chroma 2×2 secondary
  transforms + their quant in `transform.rs`; end-to-end flat-block tests
  recover the residual. NOTE: the luma DC forward shift (qbits+2) was set so the
  forward∘inverse pair self-reconstructs; the exact scale gets its final
  confirmation against ffmpeg at L5 (a 1-line shift if it disagrees).
- **Remaining for compression:** L3b intra DC prediction (luma 16×16 + chroma
  8×8) + reconstruction; L4 wire I_16x16 (mb_type/CBP + nC neighbor tracking)
  into encoder+decoder replacing I_PCM; L5 ffmpeg bit-true validation + bench.
  40 workspace tests green, clippy clean.

### Original plan (generation 2 — make it actually compress)
1. Forward + inverse 4×4 integer transform & quant/dequant (the I_PCM samples
   become residuals). Round-trip test: transform∘inverse ≈ identity within spec.
2. CAVLC residual coding (encode + decode) — replaces I_PCM payload.
3. Intra prediction (16×16 + 4×4 + chroma) + simple SAD/SATD mode decision.
4. Deblocking filter. Then inter (ME/MC, P-slices, DPB), then rate control.
   Keep the bench harness + round-trip oracle green at every step.

## Crate plan (workspace)
- `h264` (or branded name) — top-level safe API.
- `h264-bitstream` — NAL/RBSP/Exp-Golomb reader.
- `h264-decoder` — the decode pipeline above.
- `h264-encoder` — later.
- `yuv` — color conversion (BT.601/709), feature-gated SIMD.
- Decoder first (deterministic, testable against an oracle); encoder second.
