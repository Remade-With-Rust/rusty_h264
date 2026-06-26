---
name: encode-phase-breakdown
description: measured per-phase cost of the encode (fast vs quality); CAVLC is 4.6% (a red herring), reconstruction + RDO trials are the real costs
metadata:
  type: project
---

**Measured the encode phase breakdown (352×288, qp26, gop12) by stubbing + Instant timers.**
Settles where the time actually goes — and kills the long-standing "the gap is CAVLC" hunch.

**CAVLC bit-writing = 4.6% of the fast encode** (stubbed `encode_residual_block`'s writes,
kept the param-calc + nnz feedback). Not the bottleneck. The CAVLC primitives are already
openh264-aligned (single-pass `CavlcParamCal`, scan8 nnz cache, u32-flush bit-writer, packed
writes, inverted-CBP lookup) — and we're `forbid(unsafe)`, so the bit-writer is near the safe
ceiling. **Optimizing CAVLC throughput (tables/scan/bit-writer) caps at ~5% — not worth it.**

**Phase breakdown (PROFILE=1 env probe, since reverted):**
- **FAST (the speedtest headline):** encode(recon+cavlc) **58%**, deblock 5%, modedec+ME+overhead **37%**.
  → reconstruction ≈ 53% (58% − 4.6% cavlc). **The headline lever is reconstruction + ME, NOT cavlc.**
- **QUALITY:** 9× slower than fast; **93% is the RDO trials**, final encode only 6%, deblock 1%.
  → A trial computes SSD, which needs MC+DCT+quant+**IDCT+reconstruct** — so the trial cost is
  *reconstruction*, not cavlc. The reconstruction primitives are SHARED with the fast encode.

**Reconstruction primitive state (the 53% fast / 93% quality lever):** inter-luma DCT+quant+IDCT
are asm ✅; **dequant is scalar (before the asm IDCT), chroma transform is fully scalar (8
blocks/MB), the MC→pred_y copy is redundant for 16×16.** These are spread thin — no single big
win — and per the [[asm-campaign-verdict]] SATD lesson, asm-wiring small per-4×4 kernels REGRESSES
(FFI overhead). So recon wins must be **algorithmic** (batch the chroma transform like the luma
scalar `forward_dct_blocks`, fuse, drop the MC copy), each ~1-3%.

**Quality RDO:** `trial_inter`/`trial_intra` do a full re-encode per candidate (`save_mb` →
encode → `mb_ssd` → `load_mb`); winner encoded twice. `trial_intra` (always run, full i16+i4×4
RDO) is the heaviest. **Done: SATD-prune the split trials** (SSD-trial only the SATD-cheaper of
16×8/8×16) → ~3% quality, RD-neutral (committed). Bigger cuts (cheap-estimate intra, eliminate
winner double-encode) are RD-affecting or a real restructure.

**Reconstruction dive (DONE, all byte-exact, inter gap 1.7×→1.6×, ~72 Mpx/s):** the recon
(53% fast) primitive inventory — forward: `forward_core`/`forward_dct_blocks` (SIMD)/`quantize`;
inverse: `dequantize`/`inverse_core`/`inverse_dct_blocks`/`reconstruct_4x4`=`inverse_core`+`add_residual_4x4`.
The SIMD-batched DCT helpers already existed + were verified; several recon paths just didn't use them.
- **Inter chroma transform batched** (`forward_dct_blocks`/`inverse_dct_blocks`) — was the last
  fully-scalar per-block path. **+3%.**
- **MC-direct**: a full-MB (16×16 luma / 8×8 chroma) partition IS the whole pred buffer, so
  `mc_luma`/`mc_chroma` write straight into `pred_y`/`c_pred` — dropped the scratch+repack copy. **+2.6%.**
- **Intra i16 + chroma transforms batched** (independent blocks, fixed prediction). Byte-exact;
  marginal end-to-end (i4×4-dominated content) but helps i16 + quality intra-trials.
- **NOT batchable / already optimal:** `dequantize` + the predb-widen + stores are flat per-element
  loops rustc already auto-vectorizes; MC interp is asm; **i4×4 is sequential** (each block's recon
  feeds the next block's intra prediction) so its transform can't batch.
- **i4×4 mode-search SATD batching: TRIED, REVERTED — net ~4% SLOWER on ALL-INTRA.** Added a
  per-block batched `satd_4x4_each` (SIMD x4) + scored all ≤9 modes' residuals in one pass. The old
  per-mode loop already computes every mode (no early-exit) and scalar `hadamard_4x4` is fast, so the
  SIMD lane-gathering setup overhead exceeds the benefit at the small (≤9) count. **Same small-kernel
  trap as the SATD-FFI revert — don't batch/asm tiny frequent kernels.** `skip_luma/chroma_is_free`
  similarly must stay per-block (their early-return on the first nonzero coeff IS the optimization).

**Bottom line: CAVLC was a dead end (4.6%); the real headline lever was reconstruction. Batching the
scalar transforms + killing the MC copy moved inter 1.7×→1.6× vs openh264, byte-exact.**

**ME/MD vs openh264 (meticulous compare via Explore agents over svc_motion_estimate.cpp / md.cpp /
mv_pred.cpp / sample.cpp) — two improvements TRIED, both REVERTED:**
- **Fast-ME early termination** (openh264's initial-point stop): our diamond already converges in 1-2
  steps on well-predicted MBs, so skipping it saves ~nothing; a *fixed* QP threshold (vs openh264's
  content-adaptive neighbor predicted-SAD) hurt MV-field coherence → ~6% SLOWER, RD flat. Reverted.
- **Quality SATD-cost mode decision** (openh264's `SATD + λ·mvbits` estimate, no trial-encode): the
  inter/intra SATD ranking + split gate WORKS (QP22 2.31→1.53s, **34% faster**, size +4.4%, PSNR −0.05).
  BUT the **skip is the crux**: without it, dropping the non-free skip balloons size (+8%) and is slower
  at high QP (those MBs get coded); WITH a fixed-SAD-threshold greedy skip, PSNR **CRATERS 41.9→22.7**
  — it skips MBs with real error and no residual to correct it, which **drifts catastrophically across
  the inter chain**. openh264 avoids this with a *calibrated predicted-SAD* (tracks coding cost across
  frames via the reference picture's skip SADs) — substantial machinery a fixed threshold can't fake.
  Reverted. **Our exact-RD trial-encode gives the correct skip via the SSD compare and suits our
  codebase; the SATD model is only a clean win with openh264's full skip apparatus.**

**Verdict: our ME/MD cost kernels (SAD/SATD) and diamond are already at openh264 parity; openh264's
search/mode-decision speed tricks (early-term, SATD-RDO) don't transplant cleanly without their
adaptive SAD-predictor infrastructure. The quality preset's exact-RD trials are correct, not wasteful.**

**PORTED openh264's predicted-SAD skip apparatus (`PredictSadSkip`) — it WORKS but the overall model
is a wash. Implemented: per-MB `mb_skip_sad`+`mb_was_skip` grids on FrameEncoder; `pred_skip_sad()` =
median of skip *neighbours'* skip SADs (left/top/top-right, non-skip→0, so the threshold is what skip
neighbours achieved → self-limiting, propagates from free-skips); greedy skip = `skip_sad < pred`;
re-did the SATD-cost mode decision on top. RESULT (352×288 motion clip):**
- **The skip FIXES the drift** — PSNR holds (41.83/35.72 vs old 41.88/35.60), no more 22.7 crater.
- **But QP-mixed**: QP22 **26% faster** (+4.4% size), QP28 **38% SLOWER** (+8% size). The predicted-SAD
  skip is **QP-independent** (it's a prediction-error bound), so at high QP it under-skips vs the old
  **SSD-priced** skip (which IS QP-aware: skip when cheaper than coding) → codes more → more searches.
- **Temporal path** (collocated-ref skip SAD, `pMbSkipSad`): tried, made it WORSE (PSNR 35.72→35.33,
  slower) — reverted.
- Fast preset byte-identical (apparatus is `!fe.fast`-gated); 60 tests green.
**Bottom line: openh264's SATD-RDO + predicted-SAD skip trades compression (+4-8% size) for mode-
decision speed, and only nets faster at LOW qp. Our SSD-priced exact-RD skip is QP-aware and better-
compressing — strictly better for the quality preset's job. The apparatus is faithfully ported &
correct, but the model loses to our trials. Recommend keep exact-RD.**
