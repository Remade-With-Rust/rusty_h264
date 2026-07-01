# ASM optimization targets — the kill-list (data-grounded)

Where SIMD asm is worth wiring next, and — just as important — where it is **not**.
Grounded in the 2026 meticulous stage profile (rdtsc profiler, median-of-31,
`profile_decode_meticulous` / `profile_encode`, single core).

## The measured reality: asm is a ~1.1–1.45× lever, not 2×

| workload | safe Rust | asm | speedup |
|---|---:|---:|:--:|
| Decode 1080p | 109 Mpx/s | 145 | **1.34×** |
| Encode INTER | 37.8 | 54.5 | **1.44×** |
| Encode ALL-INTRA | 16.8 | 19.1 | **1.14×** |

The kernels themselves are ~2–2.7× faster (MC 12.5→4.6 ms, deblock 26→12 ms). But
**Amdahl caps the whole-codec gain**: the vectorizable pixel math is only ~30–45% of
the work. The rest — entropy coding, mode-decision control flow, memory management —
does not vectorize. So new asm only pays where a *hot, vectorizable, still-Rust* stage
exists. Everything below is chosen by that test.

## Already asm'd (don't re-do)

MC (luma hor20/ver02/centre, chroma w8), deblock (luma/chroma lt4/eq4 v/h), IDCT+recon
(`idct_four_t4_rec`), forward DCT (`dct_four_t4`), quant (`quant_four_4x4`), SAD 16×16
(inter ME), i16×16 + chroma8×8 intra predictors. See `crates/rusty_h264-accel/src/lib.rs`.

## DO NOT target — un-vectorizable, asm is wasted effort

- **CAVLC entropy** (decode ~14 ms — the #1 decode stage; encode residual coding):
  serial bit-at-a-time parse/write. No SIMD lever. This is *why* decode asm is only
  1.34× — the biggest stage can't be accelerated. Future decode speed lives in the
  **pure-Rust** glue/entropy path (where the 2026 redundancy bricks landed), not asm.
- **Per-MB glue, neighbor derivation, DPB/finalize** (decode ~10 + ~9 ms): control flow
  and memcpy/allocation, not arithmetic.
- **Mode-decision *logic*** (the RD comparisons, λ math): branchy, scalar. Only the
  *pixel-cost primitives* it calls (SATD/SAD) are asm-able — see P1.

## DO target — prioritized by the data

### P1 — Encoder SATD in mode decision  ← the one real untapped lever
**Data:** ALL-INTRA encode got only **1.14×** from asm because its dominant cost — the
SATD search over I4×4/I16×16/I8×8/chroma candidate modes — runs in **Rust**
(`satd_4x4_sum`, `common/src/transform.rs:549`; the per-MB `satd_16x16/8x8/4x4` in
`encoder/src/mb16.rs` build residuals in Rust then Hadamard them).
**Available but UNUSED:** `accel::satd_{4x4,8x8,16x8,8x16,16x16}` (openh264
`WelsSampleSatd*_sse2`) are exported and never called.
**Sites:** `best_i16_satd`, `best_i4x4`/I8×8 search, chroma SATD, inter `mc_satd`
(`mb16.rs` ~1158/1748/1900/2001/351) — pass src+pred pixels straight to the asm kernel
instead of Rust-building residuals + Rust Hadamard.
**⚠️ NOT byte-identical:** our Rust SATD is `Σ|H·d|`; openh264's asm is `(Σ|H·d|+1)>>1`
(≈ half). Swapping shifts the SATD-vs-`λ·rate` balance → **different mode decisions**
(still valid H.264 that decodes under ffmpeg, but the output bytes change). So this is
an **RD-revalidation** brick, not a bit-exact one: swap, then re-run the `bench/` RD
sweep + PSNR corpus and keep only if compression doesn't regress (retune `λ` if needed,
or scale the asm result ×2 to approximately restore the Rust magnitude).
**Expected:** the largest encode win available — SATD is roughly a third of intra encode.

### P2 — Encoder intra-prediction for the candidate search
**Data:** the SATD search predicts *many* candidate modes per MB; the 4×4/8×8 intra
predictors and DC/plane paths are Rust (only i16×16 + chroma8×8 are wired).
**Available:** `vendor/.../encoder/core/x86/intra_pred.asm` (full mode set).
**Action:** wire the missing predictor modes; compounds with P1 (predict-in-asm →
satd-in-asm keeps the whole inner search off the scalar path). Bit-exact if the asm
predictor matches our Rust predictor (openh264 kernels are the reference we mirror —
verify per mode).

### P3 — Sub-block SAD for inter ME partitions
**Data:** inter ME uses `accel::sad_16x16`, but 16×8/8×16 partition search falls to Rust
SAD. `accel::sad_{16x8,8x16}` are exported and unused.
**Action:** route partition-mode SAD through them. Bit-exact (SAD is exact). Small
(inter already 1.44×), low risk — a mop-up.

### P4 — Decoder dequant / VAA activity
- Decoder **dequant** (`dequantize`/`inverse_quant_8x8`, ~2 ms) is Rust; check the
  vendor tree for an inverse-scan/quant kernel. Small.
- **`vaa.asm`** (variance/activity) for adaptive-quant rate control — wire only if the
  RC activity pass shows up hot (currently minor).

## Verdict

- **Decode:** asm is **near its ceiling**. The dominant stages (entropy, glue, finalize)
  are un-vectorizable, so 1.34× is roughly the structural max. Chase decode speed in the
  **pure-Rust** path, not asm.
- **Encode:** **P1 (asm SATD in mode decision) is the one high-value untapped asm target**
  — it directly explains the 1.14× intra result. It is an RD-revalidation change, not a
  byte-identical one; gate it on the RD/PSNR corpus, not the bit-exact oracle.
