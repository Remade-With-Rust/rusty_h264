# big-oppy-decoder

## 1. Benchmark vs ffmpeg

1T, pinned CPU time, ABBA ×9, x264-encoded 720p corpus (1800 frames/tier),
byte-identical before timing. Record = 2026-08-12 morning-clean.

| tool tier                  | rusty/ffmpeg | ours Mpx/s | ffmpeg Mpx/s |
| -------------------------- | ------------ | ---------- | ------------ |
| CAVLC (baseline, veryfast) | 2.24×        | 213        | 412          |
| Main (medium)              | 2.04×        | 146        | 294          |
| High (slower)              | 2.04×        | 125        | 255          |

| fact            | value                                                         |
| --------------- | ------------------------------------------------------------- |
| cross-run band  | cavlc 1.98–2.49, main 2.04–2.35, high 2.04–2.25 (box-state)   |
| PGO build       | −3.1% high, −5.3% cavlc (`bench/pgo.sh`)                      |
| 2T vs ffmpeg-2T | 2.75–2.86× wall; our frame-MT busy 1.13–1.20 vs ffmpeg ~1.5   |
| pure-Rust rip   | 1.004× vs last asm build, z=−0.26 (null)                      |
| pure-Rust floor | ~1.4–1.5× (entropy asm gap); wall lever = frame-MT scheduling |

Re-baseline: sustained-quiet ≥10 min, name any hot process, fresh bin builds,
`bash bench/decode_x264_speedtest.sh 9`.

## 2. The gate process

```
STREAM (bytes)
   |
   v
GATE 0 - TOOL TIER          free: stream flags at SPS/PPS/slice parse
   |   entropy_coding_mode, profile, slices, refs, transform_8x8, B
   |   routes: CAVLC | CABAC slice loops, I/P/B paths, multi-slice gating
   v
GATE 1 - PIXEL CLASS        5 trailing counters, 4-way, LOCO-CV 0.804 (gate_fit sheet)
   |   entropy_calls_per_mb <= 2.565 --+-- mb_skip_frac <= 0.157      -> MID
   |                                   +-- mb_skip_frac >  0.157      -> LIGHT
   |   entropy_calls_per_mb >  2.565 --+-- bits_per_mb  >  363.7      -> ENTROPY-EXTREME
   |                                   +-- skiprecon<=0.494 & dequant>1.92 -> DENSE-INTER
   |                                   +-- else                       -> MID
   v
ROUTE -> PIPELINE ANATOMY   measured per route, all tiers (truth table, n=51)
```

| route           | n   | entropy% | inter-MC% | deblock% | ns/MB     | dominant lever                    |
| --------------- | --- | -------- | --------- | -------- | --------- | --------------------------------- |
| LIGHT           | 12  | 3.5–34   | 14–33     | 4–11     | 265–1170  | skip machinery, skip-run batching |
| MID             | 9   | 13–31    | 17–27     | 6–13     | 908–1578  | default path                      |
| DENSE-INTER     | 24  | 20–49    | 10–20     | 5–15     | 1101–3268 | MC ladder, EDC seam               |
| ENTROPY-EXTREME | 6   | 59–82    | 0–6       | 2–6      | 3136–6797 | entropy syntax layer, EDC always  |

Gate 1 status: FITTED PER TIER, NOT WIRED (full fits: gate_fit_per_tier
sheet). 3-variable core suffices on every tier: entropy_calls_per_mb,
bits_per_mb, skiprecon_calls_per_mb (+ a second entropy threshold on cavlc).
cavlc fit EXCLUDES mb_p/b/skip fracs (CABAC-only scopes, invalid there).
9-way fine gate REFUSED (LOCO-CV 0.157, ~2 clips/class).

| split                      | cavlc              | main           | high             |
| -------------------------- | ------------------ | -------------- | ---------------- |
| root: entropy_calls_per_mb | 5.575              | 3.575          | 2.335            |
| LIGHT/MID                  | entropy_calls 2.96 | skiprecon 0.32 | skiprecon 0.3575 |
| EXTREME: bits_per_mb       | 406.65             | 315.43         | 340.11           |
| train / LOCO-CV            | 1.000 / 0.824      | 1.000 / 0.765  | 1.000 / 0.824    |

Solid cells: DENSE-INTER 23/24, ENTROPY-EXTREME 6/6 across tiers. Weakest:
MID on main (0/3, blurs into LIGHT at 17 rows). Thresholds valid at qp26/x264
on the varied axes only; qp sweep + non-x264 provenance before wiring.

## 3. All content types

The decoder's content is the bitstream: pixel class × syntax tools ×
provenance. All three vary independently.

Pixel classes (bench = in the timed corpus today):

| class          | as the decoder feels it                    | bench   |
| -------------- | ------------------------------------------ | ------- |
| static         | skip-run dominated, few coeffs             | —       |
| medium         | balanced P coding                          | ✓       |
| detail         | dense residual, intra mix                  | ✓       |
| pan            | uniform MVs, qpel-rich                     | ✓       |
| complex        | dense residual + motion                    | —       |
| fastmotion     | partition-rich, sub-8x8, qpel              | anatomy |
| smooth         | DC-only residual, B-heavy                  | —       |
| grain          | max-density residual everywhere            | —       |
| screen content | sharp edges, long flat runs, big skip runs | —       |

Syntax/tool axes:

| axis            | values                          | cost consequence                              |
| --------------- | ------------------------------- | --------------------------------------------- |
| entropy coder   | CAVLC / CABAC                   | CABAC ~1.9× slower same content               |
| slice structure | 1 / N per picture               | availability gating; idc==2 bS suppression    |
| frame types     | I / IP / IPB(+pyramid)          | B = b-mc glue 22%; dep-density kills frame-MT |
| refs            | 1 / N / long-term / MMCO / gaps | reorder, marking, placeholder path            |
| partitions      | 16x16-heavy … sub-8x8-rich      | MC call count; coalescing ladder              |
| MC precision    | full-pel … qpel-rich            | interpolation share swings whole profile      |
| transform       | 4x4 / 8x8 mix                   | cat-5 residual path, scaling lists            |
| special MBs     | skip-heavy / I_PCM / lossless   | flips which fast paths matter                 |
| weighted pred   | none / explicit-P / implicit-B  | explicit-B refused (loud)                     |
| bits per MB     | sparse … dense                  | strongest single cost signal                  |

Provenance: own-encoder / x264 per-preset / other encoders / fuzzed. Own
streams are a narrow dialect (were 100% full-pel; hid half the gap).

## 4. Main gate per content — how each type is routed today

| content type          | gate/signal today                | channel it routes into                    |
| PIXEL CLASS (GATE 1)  | 5-counter cost-tier tree (sec 2) | FITTED, NOT WIRED — first gate once wired |
| --------------------- | -------------------------------- | ----------------------------------------- |
| entropy coder         | stream flag                      | separate CAVLC / CABAC slice loops        |
| frame type            | slice header                     | I / P / B decode paths                    |
| skip MBs              | mb_skip_run / skip flag          | P_Skip / B_Skip prediction-copy recon     |
| AC-empty block        | cbf / nnz == 0                   | DC-only residual fast path                |
| sparse vs dense block | nnz ≤ 6                          | scatter path vs dense 16-mul path         |
| partition shape       | mb_type                          | mc_rect coalescing ladder                 |
| MC precision          | mv fractional bits               | full-pel direct vs hpel/qpel kernels      |
| uniform-motion MB     | mb_uniform (AVX2)                | single bS kernel call                     |
| dense-residual frame  | bits/MB > 38.4 ∧ CABAC ∧ ≤5k MB  | EDC defer-and-flush seam (auto)           |
| multi-slice           | first_mb_in_slice                | availability gating (ctx + pixels)        |

## 5. Pipeline anatomy

Sampled profiler, 2026-08-12, trustworthy. Shares of decode.

| stage                  | high (CABAC)         | cavlc    | SIMD state              |
| ---------------------- | -------------------- | -------- | ----------------------- |
| entropy                | 22.3% (+8.3% syntax) | 31.2%    | serial — engine CLOSED  |
| inter-MC (+b-mc glue)  | 19.5% (+22% INFO)    | 14.7%    | SSE2/AVX2/NEON done     |
| per-MB glue / row-hook | ~26% class           | ~similar | structural, not SIMD    |
| deblock                | 6.8%                 | 14.2%    | SSE2/NEON done          |
| dequant + reconstruct  | ~5%                  | ~14%     | hybrid; dense inherent  |
| dpb-clone              | 2.3%                 | 1.9%     | memcpy-bound            |
| intra-pred             | 0.3%                 | 0.9%     | not worth it (measured) |

## 6. Tool Tier Anatomy

### MAIN

Data enters. GATE 0 already told us: CABAC, Main/High profile. GATE 1 is
ONE gate — at most 3 threshold comparisons per stream — landing in one of
4 ROUTES (the cost tiers). Two gates total on the path, four destinations.

OUR GATE (v1, fitted on our MAIN streams):

```
entropy_calls_per_mb <= 3.575 ?
    yes -> skiprecon_calls_per_mb <= 0.32 ?  yes -> MID   no -> LIGHT
    no  -> bits_per_mb <= 315.43 ?           yes -> DENSE-INTER   no -> ENTROPY-EXTREME
```

IMMEDIATE ROUTING, 9 content types — our MAIN streams vs x264-default
streams through the same gate (measured per clip, main_vs_default sheet):

| content type | route (truth)   | our MAIN | their default          | ns/MB MAIN | ns/MB default | speed delta |
| ------------ | --------------- | -------- | ---------------------- | ---------- | ------------- | ----------- |
| static       | LIGHT           | ok       | ok                     | 798        | 696           | -12.8%      |
| medium       | MID             | ok       | ok                     | 1319       | 1275          | -3.3%       |
| detail       | DENSE-INTER     | ok       | shields -> MID WRONG   | 2165       | 2068          | -4.5%       |
| pan          | DENSE-INTER     | ok       | stockholm -> MID WRONG | 1878       | 1832          | -2.4%       |
| complex      | DENSE-INTER     | ok       | crew -> MID WRONG      | 1817       | 1783          | -1.9%       |
| fastmotion   | DENSE-INTER     | ok       | ok                     | 2653       | 2596          | -2.1%       |
| smooth       | MID             | ok       | ok                     | 1199       | 1154          | -3.8%       |
| grain        | ENTROPY-EXTREME | ok       | ok                     | 6648       | 6410          | -3.6%       |
| screen       | LIGHT           | ok       | ok                     | 844        | 823           | -2.5%       |

Score: our MAIN 17/17. Their default 14/17 — the 3 misses are all the same
failure: the 8x8 transform drops entropy_calls_per_mb below our 3.575 root,
so the 720p/4CIF dense clips read as MID. Fix already measured: seed with the
HIGH tree (root 2.335, skiprecon 0.3575, bits 340.11) -> 16/17 on default
(one miss: screen_ui at the LIGHT/MID edge), then refit after the corpus
swap. Default tier: 17/17 byte-identical. Speed columns: decode cost per
macroblock, best-of-3 CPU, class mean (2 clips; smooth 1). Loaded-box
provenance — ordinal use. Their default decodes FASTER on every class (8x8
transform = fewer entropy calls per coefficient); grain stays ~8x static's
cost on both toolsets.

### HIGH