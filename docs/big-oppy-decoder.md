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

Gate 1 status: ROUTER WIRED (2026-08-20), ZERO CONSUMERS — the decoder
computes the route live (`Decoder::content_route()`): three free counters
(NAL bits, skip MBs, step_qp-coded MBs), EMA alpha 1/8 (thresholds are
per-stream-mean-calibrated; raw per-picture reads misroute boundary P
pictures), per-GATE-0-signature thresholds REFIT ON THE DEPLOYED ESTIMATOR:
cavlc 17/17 LOCO-CV, main 15/17, unified 8x8 (high+default) 32/34 = 64/68.
Byte-identity + suite green — the router changes no output bit. Next: first
consumer = LIGHT route per-frame-overhead strip (the 3.18x hole).

Previous offline fit (superseded by the deployed calibration above):
FITTED PER TIER (full fits: gate_fit_per_tier
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

GATE 0 computes nothing — it READS. Three header fields, parsed before any
macroblock work, define the tier exactly (no counters, no thresholds, no
fitting; the tier label is just a name for a flag combination):

| field                    | where                              | read at             |
| ------------------------ | ---------------------------------- | ------------------- |
| profile_idc              | SPS byte 1 (params.rs:121)         | first SPS of stream |
| entropy_coding_mode_flag | PPS (params.rs:308)                | first PPS           |
| transform_8x8_mode_flag  | PPS High extension (params.rs:286) | first PPS           |

| tier signature   | profile_idc | entropy | 8x8 flag | corpus tier          |
| ---------------- | ----------- | ------- | -------- | -------------------- |
| Baseline + CAVLC | 66          | CAVLC   | absent   | cavlc                |
| Main + CABAC     | 77          | CABAC   | absent   | MAIN (current)       |
| High + CABAC     | 100         | CABAC   | set      | MAIN (target) / high |

These same flags already steer the decoder today (this IS gate 0, live):
CAVLC vs CABAC slice loops (lib.rs:866), b_possible = profile_idc != 66
(lib.rs:1062), transform_8x8_mode threaded into every FrameDecoder. Note the
current-MAIN vs target-MAIN signatures differ only in profile_idc + the 8x8
flag — after the swap, gate 0 cannot distinguish MAIN from high by flags
alone; preset differences (refs, partitions, B-pyramid) show up only in
GATE 1's counters, which is exactly why GATE 1 exists.


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

| content type | route (truth)   | our MAIN | their default | ours ns/MB | ffmpeg ns/MB | gap vs ffmpeg |
| ------------ | --------------- | -------- | ------------- | ---------- | ------------ | ------------- |
| static       | LIGHT           | ok       | ok            | 884        | 278          | 3.18x         |
| medium       | MID             | ok       | ok            | 1556       | 704          | 2.21x         |
| detail       | DENSE-INTER     | ok       | ok            | 2489       | 1286         | 1.94x         |
| pan          | DENSE-INTER     | ok       | ok            | 2282       | 1178         | 1.94x         |
| complex      | DENSE-INTER     | ok       | ok            | 2232       | 1096         | 2.04x         |
| fastmotion   | DENSE-INTER     | ok       | ok            | 3084       | 1631         | 1.89x         |
| smooth       | MID             | ok       | ok            | 1343       | 559          | 2.40x         |
| grain        | ENTROPY-EXTREME | ok       | ok            | 7377       | 5031         | 1.47x         |
| screen       | LIGHT           | ok       | ok            | 1039       | 457          | 2.28x         |

FUNCTIONS BY % OF OUR PIPELINE, PER ROUTE (sampled profiler, MAIN-tier
streams, per-route means; rows ordered by LIGHT share; every column sums to
~100 of that route's own decode time):

| function (stage)   | LIGHT | MID  | DENSE | ENTROPY |
| ------------------ | ----- | ---- | ----- | ------- |
| per-MB glue (other) | 47.0  | 29.0 | 20.5  | 2.5     |
| inter-mc           | 20.7  | 20.4 | 14.6  | 2.4     |
| entropy decode     | 15.0  | 23.1 | 38.3  | 78.0    |
| deblock            | 6.1   | 7.9  | 7.1   | 2.5     |
| syntax-parse       | 3.7   | 9.1  | 7.8   | 1.9     |
| dpb-clone          | 3.0   | 2.8  | 1.7   | 0.6     |
| reconstruct        | 1.2   | 2.1  | 3.3   | 3.3     |
| dequant            | 1.0   | 1.9  | 3.8   | 4.6     |
| skip-recon         | 0.7   | 0.2  | 0.0   | 0.0     |
| scatter(store)     | 0.5   | 0.4  | 0.5   | 1.6     |
| intra-pred         | 0.4   | 0.5  | 0.6   | 2.2     |
| pred-buf copy      | 0.3   | 1.0  | 0.9   | 0.1     |
| finalize           | 0.2   | 1.0  | 0.2   | 0.1     |
| neighbors          | 0.1   | 0.4  | 0.3   | 0.0     |
| mv+grid            | 0.1   | 0.2  | 0.2   | 0.0     |

The read, per route: LIGHT's top function is the OVERHEAD ITSELF — 47% per-MB
loop glue + 20.7% MC for mostly-skip copies, only 15% real entropy work; this
is the 3.18x gap in function form and the first consumer's target list.
MID is balanced (the default path earns its name). DENSE is entropy+syntax
46% with MC second. ENTROPY is a CABAC benchmark wearing a codec costume:
78% one function.

Score with the WIRED router (deployed-calibrated thresholds + EMA, 82efc68):
our MAIN 17/17, their default 17/17 — 34/34 steady-state, verified live.
(The offline v1 tree had misrouted shields/stockholm/crew-default to MID;
the deployed calibration's unified 8x8 signature fixed all three — history
kept in gate_fit_per_tier.) Default tier: 17/17 byte-identical. Speed columns: BOTH decoders on the
SAME x264-default streams (concatenated to >=800ms workloads, frame counts
verified both arms, 3 alternating reps, best-of; loaded-box session — the
RATIO is the robust number). Weighted overall ~2.0x. The gap is NOT uniform:
grain 1.5x (entropy-bound, our engine closest), fastmotion/detail/pan ~1.9-2.1x
(kernel-bound, SIMD parity), static 3.2x / screen 2.5x — LIGHT content is our
WORST competitive class: per-frame fixed costs (setup, dpb, grids) dominate
when frames are cheap, and ffmpeg's per-frame overhead is far smaller. The
next competitive lever for LIGHT is per-frame orchestration, not kernels.

BENCHMARK-DRIFT WARNING for the corpus swap: because default streams are
cheaper, every ABSOLUTE number (Mpx/s, truth-table ns/MB) improves ~2-13%
at the swap with ZERO decoder change. Only the vs-ffmpeg RATIO survives the
swap comparably (both decoders get the same streams). Re-baseline sec 1 and
re-harvest the truth table on swap day; never compare absolute numbers
across the swap boundary.

### HIGH