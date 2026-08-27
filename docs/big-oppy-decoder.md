# big-oppy-decoder

## 1. Benchmark vs ffmpeg

### 2026-08-27 rerun — first numbers from the asm-DEFAULT build (LOADED box)

Same harness, 9 pairs, byte-identical + 1800-frame work parity both arms.
Fresh plain-default build (`asm` now default; arm banner verified
`accel x86-64 SSE2+AVX2`, zero knobs), built in an ISOLATED
`CARGO_TARGET_DIR` and run from copies because a concurrent session was
building in this checkout. ⚠ **The quiet-box precondition was NOT met**
(~88% foreign load: VS Code, faucet, concurrent cargo builds), so this is a
loaded-box data point on today's tree, NOT a replacement record.

| tool tier | rusty/ffmpeg | pairs | z |
| --------- | ------------ | ----- | ---- |
| CAVLC     | 2.061x       | 9/9   | 3.00 |
| Main      | 1.994x       | 9/9   | 3.00 |
| High      | 1.959x       | 9/9   | 3.00 |

Read against the bands, not the record: **Main 1.99 and High 1.96 sit BELOW
the historical cross-run band floor (2.04/2.04)** — the code-is-faster
conclusion survives a loaded box on those tiers. CAVLC 2.06 is above the
08-22 record (1.81) but at its old band floor (1.98); CAVLC is the most
load-sensitive tier here and the record was quiet-box. The 08-22 record
table above STANDS as the record; re-run this section's command on a
sustained-quiet box to move it. (The harness path fix that made this run
possible with binary overrides: `BENCH_BIN`/`CLI_BIN` +
`cygpath -am` in `decode_x264_speedtest.sh`.)

## 1a. Conformance status (2026-08-27)

| arm | vs ffmpeg (pixel-exact, all 3 presets) | note |
| --- | --- | --- |
| accel (`asm`, the DEFAULT on every codec crate since 2026-08-27) | ✓ default High / Main / B+pyramid | re-confirmed 2026-08-27 on the asm-default build (9/9 corpus streams byte-identical, §1 rerun) |
| scalar (plain build, encoder-crate examples) | ✓ **after the 2026-08-27 fix** | was silently WRONG on chroma — see below |

**P0 FIXED (2026-08-27): scalar builds decoded packed-bS streams with chroma
deblock OFF** — shipped in 0.11.0. The scalar chroma loops in
`filter_frame_rows` lacked the `pre_bs` branch the luma loops and the accel arm
carry, and read the never-populated `bs_v`/`bs_h` zero-init instead (on the
precomputed path the derivation is skipped by construction, so those arrays
stay zero). One-closure fix in `deblock.rs::chroma_bs`. Blast radius: any
stream the decoder routes through precomputed strengths, decoded by a scalar
build — every chroma 4x4-edge pair unfiltered (|d| ≤ 5-9, edge-flanking).
**This closed the x264-parity campaign's filed "Main-profile chroma-deblock
divergence"**: the encoder was never at fault, and the old "decoder exonerated"
arm was an accel CLI build that never compiled the broken closure. The
timing tables above are accel builds — unaffected. Scalar-arm conformance
probe: `cargo run --release --no-default-features --example dectest -p
rusty_h264-encoder` (**invocation updated 2026-08-27**: `asm` is now a DEFAULT
feature on all three codec crates — the X2 fix — so the scalar arm needs the
explicit `--no-default-features`; the CLI can never see this class of bug).

## 1a.1 SIMD reachability sweep (2026-08-27)

"We deployed AVX2/SIMD and did not see the win" → 20 candidate mis-wiring
sites audited and dispositioned; full ledger in
`docs/plans/inline-execution.md` §11.19. Landed: `asm` is now DEFAULT on the
codec crates (with the workspace-dep `default-features` fix that makes the
scalar arm real), an arm banner + knob audit in the CLI/bench drivers
(`rusty_h264_common::arms`), the `RS_H264_DOUBLE_RECON` polarity fix, SSE2
twins for `mb_uniform`/`bs_motion_masks` (no more silent packed-bS loss
without AVX2), an ARM-execution CI job, and the decoder's phantom accel dep
removed. Closed by evidence (post-LTO asm census + MC size×phase census):
the span-avg loops ARE vpavgb, the residual family IS ymm-vectorised, the
intra kernels stay encoder-only, sub-8x8 MC is 0.9–2.0% of MC cycles.
⚠ The pre-LTO rlib `.s` LIES under `lto="thin"` — census the final binary.

## 1b. Headful side-by-side viewer

`crates/rusty_h264-decoder/examples/sim_sxs.rs` — our decoder and ffmpeg
decoding the same x264 stream in one window, plus a DIFF panel that is black
when the two agree and glows red where they do not.

```
cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- main
cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- high
cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- <s.264> [scale=N] [max=N]
cargo run ... --example sim_sxs -- main nowin      # no window, prints the verdict, exits non-zero on mismatch
```

Verified pixel-identical on 720p MAIN, 720p HIGH and 1080p crowd_run HIGH.
`minifb` is a DEV-dependency, so the published crate and every consumer build is
unchanged — confirmed by grepping a plain library build for it.

TWO THINGS IT GETS RIGHT THAT ARE EASY TO GET WRONG:

**Display order.** The first cut paired `Decoder::decode(au)` output against
ffmpeg positionally and reported **50.6M differing samples** on a stream the
68-stream gate proves byte-identical. Per-AU `decode` returns DECODE order;
ffmpeg emits DISPLAY order; MAIN and HIGH both carry B-frames. `decode_stream`
reorders by PicOrderCnt and the mismatch vanished. This is the same defect that
once produced a BD-SSIM of 4.9e9% — *any harness that zips decoded frames
positionally is wrong the moment B-frames exist*, and it will look like a codec
bug every time.

**Timing it took four goes, and the sequence is the lesson.**

| version | ffmpeg timing | HIGH reads | verdict |
| ------- | ------------- | ---------- | ------- |
| 1 | one unpinned wall-clock `-f null` | 1.46-2.02, and 0.80 under load | can INVERT the result |
| 2 | removed entirely | n/a | ducked the question |
| 3 | best-of-N, startup-corrected, 1.2 s clip | 2.10-2.19 | stable but clip-mismatched |
| 4 | same, on the 25 s clip | **1.95** | usable |

Three separate defects, each masquerading as the next:

1. ONE SAMPLE OF A NOISY QUANTITY. A single wall-clock subprocess run cannot
   resolve a ~1.9x ratio. Best-of-N fixed it — contention only ever makes a run
   SLOWER, so the minimum is the robust estimator.
2. PROCESS STARTUP CHARGED TO 60 FRAMES. ffmpeg's ~55 ms launch is a third of a
   1.2 s decode and a rounding error against a 25 s one. Measured separately via
   `-frames:v 1` and subtracted — and mostly cured by simply using a longer clip.
3. THE COMPARATOR WAS WRONG. Once stable, the viewer read 2.10-2.19 against a
   "standing 1.84x" and looked biased. It was not: 1.84 is the MEDIAN OVER THE
   3-CLIP TIER CORPUS, and shields alone is a 2.20x clip — confirmed by running
   the pinned harness on shields HIGH by itself: **2.196x, 7/7, z=2.65**. The
   instrument was right and the reference number did not apply to it.

En route, the pinned check first returned **7.24x**, which is impossible: the
`decode_bench` binary was still the `--features asm,profile` build left over from
`route_shares.py`. The profiler costs ~5x at full (unsampled) rate. Rebuilt clean
before believing anything — a harness must build what it tests.

ON THE 25-SECOND CLIP the viewer now reads MAIN 1.90x and HIGH 1.95x against a
pinned 1.84x, and reports PIXEL-IDENTICAL on all 1260 frames of both.

**Memory.** Holding every frame at full resolution for both arms is 166 MB at 60
frames and 3.5 GB at 1260. Frames are now shrunk to panel size as they are
paired; the full-resolution pair is used ONCE for the identity verdict and
dropped. The DRAWN diff is the shrunk one, the VERDICT is full-resolution, so
sub-sampling cannot hide a mismatch.

**Demo clip.** `_xbench/demo25s__{main,high}.264` — 1260 frames of real 720p50
video (the `shields_1800` master, 25 s) at `--preset medium` / `--preset slower`.
`main`/`high` prefer it and fall back to the 60-frame conformance clip. The
corpus clips are 0.6-2.0 s because they are benchmark clips; a 1.2 s loop is a
poor demo and, as above, too short to time against.

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
| static       | LIGHT           | ok       | ok            | 544        | 352          | 1.55x         |
| medium       | MID             | ok       | ok            | 1331       | 806          | 1.65x         |
| detail       | DENSE-INTER     | ok       | ok            | 2950*      | 1871*        | 1.58x         |
| pan          | DENSE-INTER     | ok       | ok            | 2054       | 1309         | 1.57x         |
| complex      | DENSE-INTER     | ok       | ok            | 1941       | 1230         | 1.58x         |
| fastmotion   | DENSE-INTER     | ok       | ok            | 2729       | 1812         | 1.51x         |
| smooth       | MID             | ok       | ok            | 1029       | 579          | 1.78x         |
| grain        | ENTROPY-EXTREME | ok       | ok            | 6772       | 4932         | 1.37x         |
| screen       | LIGHT           | ok       | ok            | 654        | 504          | 1.30x         |

RE-RUN 2026-08-22 (`bash bench/nsmb_rerun.sh <decode_bench>`, 7 ABBA pairs,
pinned CPU time, frame-count parity per clip, class rows = mean of their clips,
gap = ratio of the means). Previous run 2026-08-21 in the same column order:
561/349/1.61, 1348/790/1.71, 2166/1354/1.60, 1984/1212/1.64, 1886/1138/1.66,
2745/1732/1.59, 1126/579/1.94, 6740/4932/1.37, 662/477/1.39.

GAP CHANGE, every class: static -4.0%, medium -3.4%, detail -1.5%, pan -4.3%,
complex -4.9%, fastmotion -5.3%, smooth -8.4%, grain +0.2%, screen -6.6%.
**Eight of nine improved and grain held EXACTLY flat** - which is the expected
result, not a miss: grain is entropy-bound and the CABAC engine was already
measured at its floor. Worst class is still `smooth` (1.78, was 1.94); best is
`screen` (1.30).

*THE CONTROL MOVED ON `detail` - read that row's ns/MB as within-run only. ffmpeg
is the same binary with the same flags, and its column shifted +38.2% on detail
(driven by `mobile_cif`) and +5-8% on pan/complex/screen, against -1.1% at the
previous re-baseline. The GAP column is unaffected (both arms of a clip are
measured in one pinned ABBA run, so drift cancels inside a row), but the absolute
detail figures are NOT comparable to the previous table and must not be read as a
36% slowdown on our side.

The route columns are NOT re-derived here - GATE 1 v1 was not refitted;
only the timing columns moved.

FUNCTIONS BY % OF OUR PIPELINE, PER ROUTE (sampled profiler, MAIN-tier
streams, per-route means; rows ordered by LIGHT share; every column sums to
~100 of that route's own decode time):

| function (stage)   | LIGHT | MID  | DENSE | ENTROPY |
| ------------------ | ----- | ---- | ----- | ------- |
| per-MB glue (othr) | 49.6  | 33.4 | 21.8  | 2.9     |
| entropy decode     | 20.5  | 21.1 | 40.9  | 78.0    |
| inter-mc           | 7.4   | 18.1 | 12.0  | 1.9     |
| deblock            | 7.3   | 7.6  | 7.0   | 2.5     |
| syntax-parse       | 5.5   | 9.6  | 8.0   | 1.8     |
| dpb-clone          | 3.9   | 2.9  | 1.5   | 0.5     |
| skip-recon         | 2.1   | 0.5  | 0.2   | 0.0     |
| reconstruct        | 1.2   | 1.5  | 2.3   | 2.9     |
| dequant            | 1.0   | 1.9  | 4.1   | 7.2     |
| intra-pred         | 0.7   | 0.5  | 0.6   | 2.1     |
| pred-buf copy      | 0.4   | 1.0  | 0.9   | 0.1     |
| neighbors          | 0.2   | 0.4  | 0.3   | 0.0     |
| finalize           | 0.2   | 1.2  | 0.2   | 0.1     |
| mv+grid            | 0.0   | 0.1  | 0.1   | 0.0     |
| scatter(store)     | 0.0   | 0.0  | 0.0   | 0.0     |

REFRESHED 2026-08-22 (`bench/route_shares.py`, `--features asm,profile`;
1-in-64 sampled, 3 passes, per-stage median, nested scopes never summed,
othr = 100 - sum(named); every column sums to 100.0 exactly).

**THE HARNESS WAS BROKEN AND RETURNED NOTHING.** `route_shares.py` grepped
STDOUT for a `prof `-prefixed line format the binary has not emitted for some
time, while the profiler writes to STDERR in a different shape - so every clip
printed `NO TOTAL` and the refresh produced an EMPTY table rather than an error.
A stale harness that fails silently is the stale-instrument law in its most
expensive form; fixed here (reads stderr, regex matches the shipped format) with
a note at the site.

WHAT MOVED vs 2026-08-21: **`reconstruct` fell in every route** - 1.9 to 1.2
(LIGHT), 2.4 to 1.5 (MID), 3.6 to 2.3 (DENSE), 4.5 to 2.9 (ENTROPY) - which is
the bounds-check campaign's `reconstruct_4x4_*` row-slicing showing up exactly
where it should. `skip-recon` and `dpb-clone` also fell. `entropy decode` ROSE as
a share in every route (19.7->20.5, 20.5->21.1, 38.7->40.9, 75.8->78.0) because
it is the one stage that did NOT get cheaper: the CABAC engine is at its floor,
so as everything around it shrinks its share grows. That is the shape of a
remaining gap that is now entropy-dominated.

CALVC AND CABAC ANATOMY AND Entry points:
- CAVLC: decode_slice_cavlc_inner reads mb_skip_run — ONE syntax element
  saying "next N MBs are skips" — then loops N times through
  decode_p_skip/decode_b_skip per MB.
- CABAC: one mb_skip_flag bin per MB (P ctx 11-13, B ctx 24-26); runs are
  detected, not read.

#### KEY per-mb-glue functions

The 48.7% glue bucket, cracked open as a CONTAINMENT TREE (INFO scopes,
LIGHT MAIN-tier streams, refreshed 2026-08-21 via bench/glue_shares.py).
These scopes OVERLAP — a parent contains its children — so they do NOT
sum. Each parent is followed by its RESIDUE: the part of that scope no
child names, which is the actual glue and the only thing worth attacking.
`calls/MB` uses real macroblocks (px/256) and is EXACT; ms is sampled and
carries the probe's tax, so where a count and a time disagree the count
wins.

| function                                | file                       | LIGHT | calls/MB |
| --------------------------------------- | -------------------------- | ----- | -------- |
| per-MB loop (ALL MB work)               | both slice loops           | 87.5% |        - |
| |- RESIDUE = true loop glue             | (unnamed)                  |  3.3% |        - |
| |- dec-mb-B bodies                      | mb16.rs CABAC B arm        | 35.2% |     0.74 |
| |  |- RESIDUE  <== biggest              | (unnamed)                  | 21.3% |        - |
| |  |- b-mc                              | mb16.rs b_mc               |  9.0% |     0.20 |
| |  |  |- b:luma-mc                      | in b_mc                    |  3.5% |     0.20 |
| |  |  |- b:chroma-mc                    | in b_mc                    |  2.8% |     0.20 |
| |  |  |- b:blend                        | in b_mc                    |  1.3% |     0.09 |
| |  |  |- b:weights                      | in b_mc                    |  0.3% |     0.20 |
| |  |  `- RESIDUE                        | (unnamed)                  |  1.1% |        - |
| |  |- b-direct                          | mb16.rs b_direct*          |  3.0% |     0.03 |
| |  |  `- b-deriv                        | in b_direct                |  0.1% |     0.03 |
| |  `- b-setmotion                       | mb16.rs b_set_motion       |  1.9% |     0.20 |
| |- row-hook                             | mb16.rs row_hook           | 28.3% |     0.04 |
| |  |- deb:derive  <== biggest leaf      | deblock.rs bS derivation   | 17.0% |     1.04 |
| |  |- deb:pack  (never fires)           | deblock.rs pack_frame_into |  0.0% |     0.00 |
| |  `- RESIDUE = row deblock + EDC flush | (unnamed)                  | 11.3% |        - |
| |- dec-mb-I bodies                      | intra path                 | 11.6% |     0.02 |
| |- dec-mb-P bodies                      | P path incl. decode_p_skip |  9.2% |     0.05 |
| |- ent:levels                           | cabac.rs level decode      |  8.2% |     0.52 |
| |- ent:sigmap                           | cabac.rs significance map  |  6.9% |     0.52 |
| |- ent:cbf                              | cabac.rs coded_block_flag  |  2.4% |     0.94 |
| |- mc-stage                             | recon helpers              |  4.5% |     0.05 |
| |- resid-add                            | recon helpers              |  2.9% |     0.07 |
| `- state-cache                          | mb16.rs nzc/mn export      |  0.1% |     0.07 |
| dec-setup                               | grid refill (per picture)  |  4.7% |        - |
| dec-slice-alloc                         | per-slice scratch          |  0.5% |        - |
| dec-rbsp-unescape                       | nal.rs                     |  0.2% |        - |
| dec-nal-split                           | nal.rs                     |  0.1% |        - |

| component of dec-mb-B                  |    ms | %decode | ns/call |    calls |
| -------------------------------------- | ----- | ------- | ------- | -------- |
| b:mvd-parse (motion parse + B recon)   | 18.84 |   12.3% |     606 |   31,078 |
| unnamed B per-MB glue                  | 16.37 |   10.6% |     103 |  158,400 |
| b:skip-cold (decode_b_skip)            | 11.58 |    7.5% |     225 |   51,462 |
| b:resid (cbp + coeffs + residual add)  |  8.28 |    5.4% |     266 |   31,078 |
| b:type-parse                           |  1.68 |    1.1% |      54 |   31,128 |
| b:fill-cache                           |  0.70 |    0.5% |      23 |   31,078 |
| b:skip-hot (forced path, near-free)    |  0.73 |    0.5% |      10 |   75,810 |
| = dec-mb-B                             | 58.18 |   37.8% |     367 |  158,400 |

READ IT LIKE THIS. Only 31,078 of 158,400 B macroblocks are NON-SKIP, and
those 20% carry b:mvd-parse + b:resid + b:type-parse + b:fill-cache =
29.5 ms of the 58.2. The 127,272 skips cost 12.3 ms total, and 75,810 of
them go through b:skip-hot at TEN nanoseconds — that is the skip-band /
forced-derivation campaign, and it is done. The remaining 16.4 ms of
unnamed glue is ~103 ns on EVERY B macroblock (skip-flag decision,
edc_flush, decode_terminate, the mb_qp/mb_skip/mb_direct writes).

So the B-arm work list is: the non-skip body first (b:mvd-parse at
606 ns/MB is the single largest), then the flat per-MB glue, and NOT the
skip path.

B-ARM WORK LIST, 20 TARGETS, AND WHAT HAPPENED (2026-08-21). EXECUTED
and byte-identity-gated on both EDC arms:

|  # | target                                            | where            |
| -- | ------------------------------------------------- | ---------------- |
|  1 | `if list==0 {mmvd0} else {mmvd1}[i]` COPIED a      | 3 sites in the   |
|    | 64-byte array before indexing -> take a reference | B recon loops    |
|  2 | B_8x8 bi sub-parts gathered each list separately   | mv_neighbors_    |
|    | -> `mv_neighbors_both`, the fusion the 16x16 path  | both             |
|    | already had                                        |                  |
|  3 | `b_sub_uses(st,list)` resolved 5x per sub-part      | B_8x8 recon      |
|  4 | `pred_y`/`c_pred`: 384 B zeroed per non-skip B      | hoisted to slice |
|    | inter MB and then fully overwritten by MC          | scope            |
|  5 | mvd ctx re-tested BOTH refc neighbours once per     | parse_mvd_       |
|    | COMPONENT - 4 loads for 2 answers                  | partition        |
|  6 | `CACHE30[zb]` and `G_SCAN4[zb]` looked up twice     | same             |
|    | each per block in the scatter                      |                  |
|  7 | full-MB partition scatter -> `fill` (16 indexed     | same             |
|    | stores -> 1 fill, x2 arrays)                       |                  |
|  8 | `edc_regions.is_some()` re-asked per PARTITION      | rec_mode hoist,  |
|    | -> hoisted per MB; 1T now calls b_mc directly       | 2 sites          |
|  9 | two separate bounds-checked writes for              | B skip arm       |
|    | mb_skip/mb_direct -> one tuple store                |                  |

MEASURED, ACCEL BUILDS, vs bc55627: FourPeople-main +0.7% (21/31,
z=1.98), akiyo-main +1.0%, blue_sky-main +1.0%, screen_ui-main flat.
Pooled 66/124, z=0.72 - POSITIVE BUT NOT SIGNIFICANT. Reported as
measured; the identity gate is what makes them safe to keep.

ASSESSED AND NOT EXECUTED, with the reason (these are the other 11):

| target                        | verdict                                  |
| ----------------------------- | ---------------------------------------- |
| mvdc/refc 300 B per non-skip  | REFUSED. Hoisting needs the -1/0 sentinel |
| MB (hoist like pred_y)        | for slots no `fill!` writes; interior     |
|                               | slots read by a later partition would go  |
|                               | stale across macroblocks. ~0.4% for a     |
|                               | real correctness risk.                    |
| mmvd/mref 160 B per MB        | same sentinel argument.                   |
| b_set_motion row fusion       | already row-sliced; rows are strided by   |
|                               | w4, so they cannot merge.                 |
| b:type-parse (1.68 ms)        | 54 ns/call is a CABAC unary decode - work,|
|                               | not glue.                                 |
| b:fill-cache (0.70 ms)        | 0.5% of decode. Not worth the risk.       |
| skip-flag ctx / edc_flush /   | ~103 ns per B MB and MOSTLY CABAC         |
| decode_terminate / span_flush | decisions + a terminate - irreducible     |
|                               | entropy work, not glue.                   |
| extend b_skip_hot's forced    | the 51,462 cold skips fail the bzero      |
| path to more skips            | triple because their neighbours really    |
|                               | are non-zero; edge MBs are only 6.6%.     |
| deb:derive (17.0%)            | **INSTRUMENT, NOT WORK.** 1.04 calls/MB   |
|                               | at 202 ns/call to copy 32 bytes and run   |
|                               | two 16-byte compares is not credible; the |
|                               | profiler-tax law (per-MB scopes at ~50M   |
|                               | entries) already covers this. Price it by |
|                               | ABLATION before anyone attacks it.        |

THE CONCLUSION THAT MATTERS. Naming the residue changed the plan: the B
arm is NOT mostly removable glue. Of dec-mb-B's 58.2 ms, 29.5 ms is the
non-skip body (20% of B macroblocks), most of that inside b_mc, which
three prior campaigns already worked. The skip side is finished - 75,810
of 127,272 skips run at TEN nanoseconds. What is left in this arm is
real MC and real CABAC. The next big decoder win is NOT here.

THE THREE TARGETS THIS TABLE NOW NAMES:

1. **dec-mb-B RESIDUE, 21.3%** - the largest unnamed block in the
   decoder. b-mc + b-direct + b-setmotion account for only 13.9 of the B
   arm's 35.2; the other 21.3 is the B arm's own body with NO scope on
   it. Everything the b-mc/b-direct campaigns did was to the named 14%.
   Scope it before optimising it - we are currently guessing.
2. **deb:derive, 17.0% at 1.04 calls/MB** - boundary-strength derivation,
   once per macroblock, and the single largest NAMED leaf outside
   entropy. It was not on the old table at all.
3. **row-hook RESIDUE, 11.3%** - the row deblock filter proper plus the
   EDC flush, also unscoped.

The per-MB loop's own dispatch glue is now only **3.3%** - batches 7-10
took that from the top of the list to near the bottom, which is why the
next win has to come from inside the B arm or the deblock path, not from
the loop.

NOTE, deb:pack recorded ZERO calls on all four LIGHT streams: the packed
bS derivation never runs on this content, so deb:derive's 17.0% is
entirely the per-MB path. Either the packed path's gate is wrong for B
content or it is dead weight - worth one look.

THE FINDING THAT REDIRECTS THE BRICK: on MAIN-tier LIGHT content the skip
cost is NOT P_Skip (already lean: one skip_mv + grid commit + one copy-MC).
It is B_Skip doing FULL work per skipped MB — spatial-direct derivation plus
bi-prediction (two MC reads + a blend) to produce what static content makes
a near-copy. b-mc + b-direct together ~= the whole competitive hole.

mb_skip_run — the batch the bitstream already hands us

Batch design (byte-identity provable — skip recon is deterministic):
1. P_Skip runs: skip_mv depends on left/top neighbors; interior of a run
   settles to a fixed MV (static: (0,0)). Derive once, verify invariant per
   MB (cheap equality), batch grid commits (fill ranges) and batch MC: at
   pmv==(0,0) the "MC" is contiguous row copies from ref[0] — memcpy per
   run-row instead of 16x16 tiles per MB.
2. B_Skip runs (the fat target): spatial-direct MV derives from the SAME
   neighbor set along a run; on static content colocated is static too, so
   the derived motion is constant along the run. Derive once, re-verify
   cheaply, run ONE batched bi-pred per run of uniform motion — collapsing
   b-direct (32.5%) and most of b-mc (33.6%) for LIGHT streams.
3. Fallback: any MB whose derivation diverges exits the batch to the
   per-MB path. Gate: LIGHT-corpus A/B + full byte-identity; the route
   (Decoder::content_route) arms the batching, per-MB checks keep it exact.

Status: P_Skip band SHIPPED (step 1). edc_flush coalesces maximal same-row
runs of (0,0)-MV skips into ONE band copy per run — straight row memcpy from
padded ref[0], luma + chroma, bypassing per-MB mc_luma_padded/mc_chroma_padded
and the weighting pass. Engages unweighted OR under identity ref0 weights
(x264 weightp=2 puts a pred_weight_table in EVERY P slice; outside fades the
entries are identity, an exact pixel no-op — without this check the whole
x264 corpus banded 0%). Kill-switch: RS_H264_NO_SKIPBAND=1 (bench/pinenv.ps1
runs the paired one-binary A/B).

| stream (tier)         | banded skip MBs | MBs/run | clock (CPU, ABBA, 31 pairs)   |
|-----------------------|-----------------|---------|-------------------------------|
| akiyo cavlc           | 98.3%           | 6.8     | -11.1%  26/31 wins  z=3.77    |
| screen_text default   | 93.5%           | 18.7    | -6.9%   22/31 wins  z=2.33    |
| FourPeople default    | 91.4%           | 9.1     | -1.6%   z=0.90 (B-dominated)  |
| akiyo default         | 92.9%           | 6.3     |                               |
| shields default       | 40.9%           | 50.8    |                               |
| blue_sky default      | 2.5%            |         | +1.1% z=-0.77 (neutral: the   |
|                       |                 |         | guard costs nothing off-path) |

Byte-identity: 68/68 tt streams, BOTH arms, == ffmpeg. Suite green.
Counters (RS_H264_EDC_STATS=1 SKIPRUN line) are the deterministic primary.
FourPeople confirms the FINDING above: P_Skip is banded 91% yet the clock
barely moves on x264-default streams — B_Skip (step 2) owns that hole.

Step 2 SHIPPED: B_Skip ZERO-BI FAST PATH. When spatial direct derives
(0,0)/(0,0) bi motion, colZeroFlag is irrelevant (it only zeroes MVs that
are already zero) — so the colocated probing, region split, b_mc staging
and the pred round-trip collapse into ONE fused row-average from the two
padded refs straight into rec (exactly (a+b+1)>>1 = pavgb; b_mc's bi blend
applies only implicit weights, so None or (32,32) is the whole identity
condition). Sizing counters killed the alternative (P eq-MV bands:
peq_fp a few hundred MBs everywhere = dead) and crowned this one
(bsk_zbi: FourPeople 117k, akiyo 28k, blue_sky 91k — 3.2x the P band
population). Kill-switch: RS_H264_NO_BSKIPFAST=1.

| stream (default tier) | fast-path MBs | clock (CPU, ABBA, 31 pairs)     |
|-----------------------|---------------|---------------------------------|
| screen_text           | 14,913        | -17.4%  30/31 wins  z=5.21      |
| akiyo                 | 28,061        | -10.0%  28/31 wins  z=4.49      |
| FourPeople            | 113,907       | -7.6%   27/31 wins  z=4.13      |
| blue_sky              | 76,754        | -2.4%   z=1.26 (right dir)      |
| foreman               | (few)         | +2.1%   z=-0.26 (neutral)       |

Byte-identity: 68/68, both arms, == ffmpeg. Suite green. The derivation is
SHARED (b_direct_refs_mvs) between the fast path and decode_b_direct_n — one
implementation, no drift.

Step 2b: ZERO-UNI arm (one active list at (0,0) -> straight memcpy; b_mc's
uni arms apply no weights so no guard needed) + fall-through fix: the probe's
b_direct_nbrs result is REUSED by the fallback (decode_b_direct_n direct
call), killing a double grid-walk that showed as a +1.5% lean on
fall-through-heavy stockholm (now 1.0000 dead neutral). FourPeople after:
-8.9% (15/15, z=3.87).

Step 3 (last pass, counter-primary — each item is sub-2% on the clock but
exact and free elsewhere):
- P_Skip identity-weight skip: singles no longer run the 384-pixel apply
  loop when the slice table is identity (cached per slice, weights_id0).
  blue_sky alone: 72k singles x 384 pointless weight ops removed.
- P_Skip full-pel single: direct offset copy from padded ref, no staging
  (SKIP_FP_FULL/LUMA counters; blue_sky 12.8k, clock-neutral at 1.0000).
- B_Skip FULL-PEL arm (the pan case): nonzero full-pel direct MVs probe the
  four colZero corners; uniform czg -> offset copy/avg straight to rec
  (BSKB_FP counter). shields takes 13,562 of its 13,763 census (98.5%);
  shields clock -1.3% z=1.26, stockholm moved neutral->-1.3%, FourPeople
  holds -11.1% 15/15 z=3.87. Kill-switches: RS_H264_NO_SKIPFP (P singles),
  RS_H264_NO_BSKIPFAST (whole B family).

Step 4: PARSE-SIDE run forcing (the theorem applied to skip_mv itself).
decode_p_skip skips the 3-neighbor gather when forced: mb_x==0 (left
off-frame -> unavailability rule) or previous P_Skip at addr-1 committed
(0,0) (left is in-slice ref0/(0,0) -> zero-MV rule, or out-of-slice ->
unavailable -> same rule). Counters: 89.6% of akiyo-cavlc skip MVs forced,
92.7% screen_text, 82.9% FourPeople. Grid commits rewritten from
LUMA_4X4_SCAN_XY zigzag scatter to row-order fills (5 sites, P+B).
Knob RS_H264_NO_RUNMV. Identity 68/68 both arms; suite green.

ABLATION FINDING (the real product of step 4): eliminating ~90% of skip_mv
derivations + descattering the commits moved the clock 0 to -2% lean
(cross-binary, arms >= 1.7s CPU: screen +1.8%, akiyo cavlc +2.2%,
FourPeople 1.000, all |z|<1.3). Skip-parse glue is NOT the whale of the
mgmt/other bucket (48-61% profiled, ~25-40% real after scope tax). By
elimination the whale is the MB-syntax ENGINE layer: CABAC bin decode for
mb_skip_flag/mb_type (invisible to the entropy scope - screen_text shows
2,448 entropy calls for ~21k MBs), slice-loop dispatch, and the EDC queue.
That is kernel/engine territory - a different campaign, not per-MB glue.

Step 5: the KIND-ROUTING WIN (found by a failed experiment). Classifying
fast-path B_Skips as Skip kind was tried — provably legal (identity 68/68,
kind-verify oracle clean) — and measured 1.8% SLOWER (z=-2.69) with +1.07M
Blk::loads: the census's "9 loads vs 24 blind" economics predate the rowdb
PACKED derivation, where pack_mb runs unconditionally and the "blind" arm
derives from those records with zero fresh gathers. Reverted — and the same
stale economics turned out to be live in the SHIPPED P_Skip kind arm.
derive_bs_row now routes Skip/InterUniform through the packed arm (Intra
keeps the kind arm: pure constants, no loads). Counter: Blk::loads in bS
derivation drop to ZERO (akiyo cavlc 419,757 -> 0; FourPeople 478,930 -> 0).
Clocks: akiyo cavlc -3.5% (26/31, z=3.77), FourPeople -1.7% (z=1.62),
screen_text -2.0% (z=0.90). Knob RS_H264_KIND_LOADS=1 restores old routing.

Remaining skip leads (all priced below the next non-skip lever):
MT-worker seam; frac eq-MV run batching (blue_sky 42k,
interpolation-bound); B_Skip grid-commit range fills.

#### KEY entropy decode functions

The 42% entropy bucket, cracked open as a CONTAINMENT TREE, the entropy-side
twin of the glue table above (INFO scopes, DENSE-route MAIN-tier streams,
`bench/entropy_shares.py`, 2026-08-22). WHY DENSE and not LIGHT: entropy is
20.5% of LIGHT but 40.9% of DENSE and 78.0% of ENTROPY - LIGHT is where the
per-MB glue lives, DENSE is where the entropy stage lives.

THE COLUMNS DO NOT TOTAL 100, AND THAT IS THE POINT. These scopes OVERLAP:
`entropy/cavlc` wraps `parse_residual_cabac` and the three `ent:*` scopes sit
INSIDE it. The four indented rows sum to the PARENT, not to the decode:

    16.2 + 12.1 + 6.3 + 7.5  =  42.1  ~=  42.0   (the parent row)

`%decode` is share of TOTAL decode time, so entropy decode is 42% of it and the
OTHER 58% is the rest of the pipeline - glue 21.8, inter-mc 12.0, syntax-parse
8.0, deblock 7.0, dequant 4.1, and the small stages (see the per-route table in
section MAIN, whose DENSE column DOES sum to 100). `%stage` renormalises the
children against entropy alone, and that column sums to 100 by construction.
`calls/MB` uses real macroblocks (px/256) and is EXACT.

| function                              | file                 | %decode | %stage | calls/MB |
| ------------------------------------- | -------------------- | ------- | ------ | -------- |
| entropy decode (parse_residual_cabac) | mb16.rs (+ cavlc.rs) |  42.0%  | 100.0% |     7.31 |
| |- ent:sigmap  <== biggest leaf       | significance map     |  16.2%  |  38.6% |     4.33 |
| |- ent:levels                         | level decode         |  12.1%  |  28.8% |     4.33 |
| |- ent:cbf                            | coded_block_flag     |   6.3%  |  15.0% |     7.31 |
| `- RESIDUE = unnamed parse glue       | (unnamed)            |   7.5%  |  17.9% |        - |

| component of entropy decode  |    ms | %decode | ns/call |     calls |
| ---------------------------- | ----- | ------- | ------- | --------- |
| ent:sigmap                   | 88.07 |   16.2% |     136 |   649,336 |
| ent:levels                   | 67.56 |   12.1% |      99 |   649,336 |
| RESIDUE (unnamed parse glue) | 46.91 |    7.5% |       - |         - |
| ent:cbf                      | 34.03 |    6.3% |      31 | 1,096,321 |
| = entropy decode (the PARENT)| 236.6 |   42.0% |     204 | 1,096,321 |

READ IT LIKE THIS. `ent:cbf` runs on EVERY parsed block - 7.31 calls/MB, the same
count as the parent - and costs 31 ns. `ent:sigmap` and `ent:levels` run 4.33
times per MB. **So 2.98 of every 7.31 parsed blocks (41%) decode one
coded_block_flag bin and stop.** The 31 ns gate is already doing most of the
work of the whole stage: it removes 41% of the blocks before either expensive
child is entered.

That reframes the target. The two big leaves are 28.3% of DENSE decode between
them, and the CABAC BIN ENGINE IS AT ITS FLOOR (bad5285: window itemised =
parity, two engine plays built and refuted, 154M bins at ~2-3 ns/bin, ffmpeg
class). So ns/bin is not the lever - CALL COUNT is. The questions worth asking
are why 4.33 blocks per macroblock reach the significance map, and whether more
of them could be settled the way the cbf gate settles its 41%.

THE RESIDUE IS SMALL HERE, AND THAT IS THE DIFFERENCE FROM THE GLUE TABLE. The
glue tree left a 21.3% unnamed residue in `dec-mb-B` that turned into a
seven-scope campaign. This tree names 82% of its stage; 7.5% is unattributed
setup (scan-table selection, the ctx arithmetic between phases, the `out[]`
scatter). There is no hidden mechanism here - the stage is its three named
phases, and two of them are the work.

METHOD NOTE, AND IT MATTERS FOR ANYONE RE-RUNNING THIS. At 3 passes the CHILDREN
were stable (sigmap 14.9 / 12.1 / 13.3) and calls/MB was EXACTLY identical every
run (7.31, 4.33, 4.33) - but the PARENT swung 32.1 / 37.3 / 44.7, and the
RESIDUE, being parent-minus-children, inherited all of it: **5.7% one run, 16.5%
the next.** A differential of two same-sized noisy numbers is the least stable
thing a sampled profiler can produce. At 7 passes (the harness default) two
independent runs agreed to 0.4 points on every row and the parent matched
`route_shares.py`'s independent 40.9%. If a re-run gives a wild residue, raise
`PASSES` before believing it.

#### KEY inter-mc functions

CHROMA UV-PAIR FUSION (dec-mb-B dig, first win): U and V share every piece
of chroma-MC geometry (mv, frac weights, stride, coords, bounds) — only the
plane base differs. mc_chroma_padded_pair pays setup + range check ONCE for
both planes (kernels unchanged); b_mc_chroma is one pair call per LIST, and
the P_Skip frac-single chroma is one pair call. Deterministic counter:
inter-mc kernel invocations on FourPeople 252,703 -> 191,164 (-24.4%).
Clocks (cross-binary, 31 pairs each): FourPeople +1.3%, stockholm +1.1%,
blue_sky +1.3% (21/31, z=1.98) — uniform lean, no regression; counter-
primary per the sub-2% discipline. Identity 68/68, suite green.

FORCED ZERO-BI BITMAP (dec-mb-B dig, second win): a per-picture never-
cleared bitmap records ref0/(0,0)-both-lists zero-bi fast B_Skips; an MB
whose left+top+topright are all recorded derives (0,0)/(0,0) bi WITHOUT
b_direct_nbrs (min-positive ref over three ref0s is 0; median of three
(0,0)s is (0,0)) — 6 neighbor gathers + rid + median + implicit-weights
lookup (now slice-cached, iw00) skipped. False only ever means "derive
normally", so no MB path carries a clearing duty; a slice guard keeps
prior-slice entries from impersonating available neighbors. Counters:
FourPeople 79,183/119,351 fast B_Skips forced (66%), screen_text 83%,
blue_sky 53%. Clocks: +0.9-1.2% consistent lean, no regression — counter-
primary. Identity 68/68, suite green. Confirms the parse-glue ablation a
third time: derivation glue is real work removed but not the clock whale.

UV-PAIR EXTENSION TO P (third win): the pair fusion now covers the hot P
paths — coalesce_p_inter_mc's mc_rect + both recon_p_inter replay twins.
FourPeople kernel calls 191,164 -> 168,705 (cumulative -33% from 252,703);
crowd_run (fastmotion, 2.16M calls) +0.4% lean. Identity 68/68, suite
green. CENSUS REFUTATION: bmc_bi_fp counted full-pel-bi b_mc regions at
only 1.5-6.4k per stream — the fused offset-average luma idea is NOT worth
code; killed by counter before building (the cheap way to lose).

WEIGHT-PASS CHOKE POINT (fourth win): weight_partition early-returned on
weights None but not on IDENTITY — every coded P inter MB on x264 streams
still paid the per-ref apply loops. weights_l0id caches list_identity(0)
(ALL refs, luma+chroma) per slice; one check inside weight_partition
covers all seven call sites. Counters: crowd_run skips 89,376 passes,
blue_sky 52,160, FourPeople 9,904. Clock neutral (crowd -0.4% lean) —
counter-primary. Identity 68/68 (RS_H264_NO_SKIPFP arm), suite green.

DEC-MB-I CRACKED OPEN (fifth win + the honest map): the 5.8us/call read
was tax-inflated 2.5x — real all-intra cost is 1.73us/MB and the all-intra
gap vs ffmpeg is 1.86x, the SAME as the global gap (intra MBs are ~17
coded blocks of real work, not a pathology). The structural find: the
inter residual ladder (zero / DC-only / sparse-scatter / dense) was NEVER
PORTED to intra. Census on all-intra FourPeople: 63.4% of I_4x4 blocks
are ALL-ZERO, 11.3% DC-only, 24.8% sparse — only 0.5% needed the dense
path every block was taking. Ladder ported to both entropy arms (CABAC +
CAVLC I_4x4); zero-residual shortcut added to FOUR 8x8 sites (intra I_8x8
+ inter t8 + worker twin): t8_zero = 234k blocks/stream. Identity: 68/68
+ the all-intra stream == ffmpeg; suite green. Clocks: all-intra +0.5%
(the 4x4/8x8 kernels already early-out internally — the ladder trims the
surround), but SHIELDS +2.3% (12/15, z=2.32) — the inter-t8 zero shortcut
lands on the detail class. Intra whale after tax adjustment = the CABAC
residual bin engine (~43% of real all-intra time), already at its refuted
frontier (branchless fused-table, ~4 ns/bin, WHYS Part 21): the remaining
per-bin gap vs ffmpeg (~2 ns) is the u64-window vs 16-bit-lazy-refill
architecture, a documented trade — not a glue fix.

DC-ONLY COLLAPSE, ALL BLOCK PATHS (sixth win — the CAVLC/16x16 routing
sweep the skip campaign never reached): I_16x16 AC blocks and BOTH chroma
recon twins ran dense dequant + FULL IDCT on zero-AC blocks whose residual
is the (already-transformed) DC alone — nonzero DC, so the kernels'
internal all-zero check never fired. Every such block is now one flat add
(reconstruct_4x4_dc_into). Also: CAVLC I_8x8 got the zero-residual arm the
CABAC twin got earlier. Populations (I16_DCONLY): all-intra FourPeople
2,458,844 blocks (chroma dominates: 8/MB), screen_text cavlc 12.9k,
mobile 7.3k. Clocks: ALL-INTRA -8.7% (29/31, z=4.85 — biggest clock win
since the B_Skip fast path), screen_text default -3.9% (12/15, z=2.32),
FourPeople -1.4%, akiyo cavlc flat. Identity 68/68 + all-intra == ffmpeg;
suite green.

TWIN CONVERGENCE (the structural fix): the bug class above — routing
improvements landing in one entropy coder's recon twin and not the other —
is now STRUCTURALLY IMPOSSIBLE for intra. Four shared primitives own the
pixel halves, called by both entropy arms: recon_i4_block (ladder),
recon_i8_block (zero arm), recon_i16_luma (DC-only collapse),
recon_chroma_blocks (DC-only collapse). The parse halves stay per-coder
(that IS the coder difference); a future recon improvement lands in ONE
place, like D14 did for inter via add_inter_residual. Dispatch counters
identical pre/post refactor on all tiers; identity 68/68 + all-intra;
suite green; clock neutral (+2.8% lean).

MC INTERPOLATION SIMD DEPTH — MAPPED AND PRICED (2026-08-20 eve):
The census (profile build, "MC census" table — cycles, not calls):
QUARTER-PEL OWNS 76-80% of MC cycles on every motion stream (crowd 80.2%,
blue_sky 77.3%, shields 76.4%); half-ctr is expensive per call (880-1030
cyc) but small share; full-pel 1-3%; sub-8x8 does not chart. Coverage
matrix: SSE2 covers w8+w16 everywhere (no chroma-style hole); AVX2 covers
w16 only (hor20/ver02/hor_qpel/ver_qpel/ver02_avg/centre-pass1);
pixel_avg is pavgb-optimal already.

REFUTED: AVX2 w8 two-rows-per-vector (the obvious depth move, 26-49% of
MC cycles run 8-wide). Built + byte-identical (68/68, kernel tests 35/35)
but the clock said no: hor family -2.1% lean (loads don't halve, only
ALU; the widen chain lengthens), ver family dead neutral (1.003) despite
7-shared-loads-per-2-rows. The SSE2 8-wide 6-tap is at the practical
floor; per-call cost is dominated by the MULTI-PASS qpel structure (two
6-tap passes + avg per diagonal position, staged through buffers) and
caller plumbing. Reverted — neutral complexity is negative value.

THE REAL REMAINING MC LEVER (a campaign, not a patch): fused
per-position qpel kernels — ffmpeg ships ~16 specialized kernels, one
per (fx,fy), each a single pass with no intermediate staging; we compose
each position from 2-3 primitive passes. Fusing the diagonal positions
(the bulk of "quarter") removes a full staging round-trip per call.
Paired with the CABAC engine window redesign, these are the two
remaining structural levers; everything else is at floor.

QPEL FUSION SHIPPED (the campaign the census called for): two structural
fusions on the quarter-pel bucket that owns 76-80% of MC cycles.
(1) CENTRE-ADJACENT ((2,1)/(2,3)/(1,2)/(3,2)): the half-pel operand is the
ROUNDED FORM of the centre's own full-precision pass-1 rows/cols, so the
3-kernel 2-staging compose (luma_h|v + centre + pixel_avg) collapses to
one pass-1 + one fused pass-2/avg (mc_centre_hq / mc_centre_vq; the
vertical flavour uses a vertical-first pass-1 — order swap exact, proven
by the shipped scalar centre being vertical-first). (2) HV-DIAGONAL
((1,1)/(3,1)/(1,3)/(3,3)): both 6-taps + avg in ONE loop (mc_hv_qpel,
AVX2 w16 + SSE2), no `a` staging, no second call. RS_H264_QPEL_COMPOSE=1
restores the compose path as oracle for all fused arms.

EVIDENCE (census A/B on crowd, identical call counts, controls flat):
8x8 quarter 265.0 -> 224.6 cyc/call (-15.2%), 16x8/8x16 403.9 -> 351.9
(-12.9%), 16x16 496.5 -> 473.7 (-4.6%); MC TOTAL 701M -> 643M cycles
(-8.3% of all MC); half-ctr/half-HV control buckets unchanged. Decode
clocks: crowd +1.9%, blue_sky +1.0%, shields +0.7% leans, no regression.
Identity 68/68; kernel differentials 37/37 (fused vs scalar compose
oracles, every flavour/shift/width); suite green.

QPEL NEIGHBOR SWEEP + AVX2 PASS-2 (campaign close): the neighbor audit
found (a) mcstats profile-gated, no release tax; (b) mc_ver02_avg now
DEAD in dispatch after the HV fusion (kept exported + tested, decoder no
longer calls it); (c) w4 scalar six-taps real but <2% of MC by census
(x264 default rarely emits sub-8x8) — not worth kernels; (d) the ONE
real remaining item: centre pass-2 was SSE2-only and after the fusion it
serves (2,2) PLUS all four centre-adjacent tails. AVX2 pass-2 family
built (8 i32 lanes/op, one 16B store/row; the b/v-derive pack IS
round_shift_pack16). Census A/B: 16x16 half-ctr 634.6 -> 435.8 cyc/call
(-31.3%), 16x16 quarter 451.5 -> 357.3 (-20.9%), 8x8 rows flat
(controls). CAMPAIGN CUMULATIVE: MC total 701M -> 539M cycles on crowd =
-23.0% of ALL motion compensation. Decode clocks vs pre-campaign HEAD:
blue_sky +3.3% (20/31, z=1.62), crowd +1.4% (20/31, z=1.62) — combined
40/62, z=2.29, over the bar; shields FLAT (1.000), which corroborates:
its pan motion lives in the one-filter positions fused long before this
campaign, exactly what its bucket mix predicts.

CENTRE PASS-2 DEEP DIG (campaign coda): (1) w8 pass-2 AVX2 built —
unlike the refuted hor20-w8 (loads don't halve), pass-2's SSE2 form pays
12 unpack ops + two tap chains per 8 columns which the one-chunk AVX2
form deletes: INTERLEAVED census (5 ABBA pairs) 8x8 half-ctr -21%
median B-wins-5/5, w16 half-ctr re-verified -23% drift-honest. (2)
INSTRUMENT LESSON: a single-shot census A/B showed the w8 arm as a
REGRESSION with the untouched control bucket +22% — box drift between
runs; the census obeys the interleave law like every other clock. ABBA
census pairs with per-pair ratios are the standing protocol. (3) Scalar
oracles' per-call Vec allocs swapped for stack scratch (the live path on
future aarch64 — a heap-alloc-per-MB trap defused before it shipped).

CABAC ENGINE — RE-PRICED AND CLOSED (the "window redesign" verdict):
(1) The u64-window vs ffmpeg-16-bit-lazy-refill theory, ITEMIZED per bin:
our fused early-load table beats their second state lookup; our 3-cycle
lzcnt renorm beats their 5-cycle norm-shift table load; their sentinel
refill check saves ~1 ALU op vs our cnt bookkeeping. Net ~parity — the
window swap is a high-risk ~2-3%-of-engine play touching init, I_PCM
realign (pcm_start_byte reads cnt), and the fuzzer zero-fill invariant.
REFUSED on ROI. (2) Bin census: crowd 154.0M bins/decode (82.5%
decisions, 16.9% bypass, 54-62% of decisions renormalize) — back-of-
envelope ns/bin including the syntax layer already lands ~2-3, near
ffmpeg-class. (3) The two remaining engine plays were BUILT, verified
bin-exact (68/68 identity + suite), and REFUTED dead-neutral (crowd
1.000, all-intra 1.004): register-hoisted single-context runs (LLVM
already caches the ctx byte in the inlined loops — nothing in
renorm/refill aliases the array) and exp-Golomb bypass chunking (hoisted
comparator + refill; populations too thin at default QP). Reverted —
neutral complexity is negative value. THE ENGINE IS AT ITS FLOOR: the
remaining decode gap vs ffmpeg is DISTRIBUTED (per-frame overheads, loop
machinery), not concentrated in any kernel or the bin engine.

B-MC GLUE: SPAN-BATCHED GRID COMMITS (the b-mc hammer): runs of zero-bi
fast B_Skips defer their grid commits (all-constant: ref0/ref0, (0,0),
inter, coded, DC mode, zero nnz) into a row span, range-filled at flush —
fills of 4N replace N x 28 per-MB fills. Engagement: FourPeople 113,907
MBs in 34,724 spans (3.3/span), screen_text 6.2/span. Clock: screen_text
+7.0% (25/31, z=3.41), FourPeople flat. Byte-identity 68/68; suite green.

SPAN RECON BANDING (the second dive — THE BIGGEST CLOCK WIN OF THE
CAMPAIGN): the span machinery deferred only the grid commits; deferring
the RECON too lets the flush average rows of 16n from both padded refs
in one pass — n per-MB recon calls (8 guard derefs, per-MB row-loop
setup, 16-byte rows) become one banded pass (2 guard derefs per span,
clean wide pavgb rows, sequential writes). The one risky reader (the
bak_y top-row backup that intra reads) is written during FILTERING,
downstream of the derive flush — safe by construction. Clocks vs the
pre-span binary: screen_text +32.1% (31/31, z=5.57), FourPeople +13.6%
(30/31, z=5.21), akiyo +12.5% (26/31, z=3.77); crowd/foreman dead
neutral (low span coverage). Identity 68/68; suite green.

LONG SPANS VIA THE PATCHED GATHER (third dive): spans averaged only 3.3
MBs against 90%+ run-continuity because every NON-FORCED fast MB flushed
before its neighbour gather. But a pending span lives on the CURRENT row
— it can only ever occupy the LEFT neighbour, whose values are the
deferral constants. Substituting a synthetic left (available, ref 0,
(0,0)) when the span ends at mb_x-1 keeps the span alive through
non-forced members: spans 3.3 -> 8.2 MBs (FourPeople), 6.2 -> 18.6
(screen_text), identical MB coverage. Marginal clock: FourPeople +7.8%
(26/31, z=3.77). Note the patch is REQUIRED for correctness of the
no-flush path: a deferred left's coded_y reads false, so the unpatched
gather would derive from a NONE neighbour.

B-MC CAMPAIGN CUMULATIVE (grid spans + banded recon + long spans):
screen_text +42.0%, FourPeople +17.3% — both 15/15 sweeps, z=3.87.

FIVE MORE (the coded_y dive): (1) P_SKIP GRID SPANS — decode_p_skip's
per-MB commit (set_mb_mv + coded_y/modes_y + kind) deferred into pzspan,
mirroring the B machinery; akiyo-cavlc 36,804 MBs at 6.8/span,
screen-cavlc 19.0/span; span_flush() covers both spans at every reader.
(2) ZERO-UNI SPAN KIND + (3) FULL-PEL SPAN KIND — BzKind enum
{ZeroBi, ZeroUni(list,ri), Fp{refs,mvs}}; fp pushes PREVALIDATE windows +
mv%8 per MB (contiguous tiles => union window valid), flush dispatches
avg/copy/offset bands; the synthetic-left gather patch GENERALIZED to all
kinds (the left member's values are determined by the pending kind — the
first cut chained only ZeroBi and shields sat at 1.01/span). shields fp
12,970 MBs banded at 2.5/span; FourPeople span coverage 113,907 ->
119,383. (4) REF_POC LUT — dpb-clone's per-block bounds+Option+pointer
chase (57k blocks/ref at 720p) replaced by a 32-entry POC table,
identical output by construction. (5) COMMIT_INTER_GRID ROW FILLS — the
last per-4x4 scatter (mv+grid was 4.2% on cavlc tier) now range-fills.
Clocks for the batch: screen-cavlc +6.5% (24/31, z=3.05), FourPeople
+3.9% (21/31, z=1.98), shields +1.9%, akiyo-cavlc +0.7%. Identity 68/68
(twice); suite green.

FIVE MORE (batch 3, the deep-loop dive): (1) P (0,0) RECON RIDES THE PZ
SPAN — pz_flush band-copies directly (recon flag decided at PUSH time:
begin_slice can change weights before a cross-slice flush), eliminating
one EdcJob::Skip push per banded skip AND the flush-time run-rediscovery
scan. (2) B-SKIP mb_ref/mb_ref1 ZERO-FILLS DELETED with a proof: every
reader is a `> 0` ctx test (-1 and 0 both false) or the mvd-sum's `>= 0`
gate, and a skip's mvd is (0,0) — exclusion and inclusion-of-zero give
the same sum; 64 bytes per B skip were pure waste. (3) CARRIED mbx/mby
in BOTH slice loops — the per-MB div+mod pair (20-40 cycles each) is one
compare-and-wrap; the CAVLC skip-run's mid-iteration row crossing needs
its own wrap (the FF-DIFF gate caught the miss in one cycle). (4)
row_hook_at(carried row) — kills row_hook's own per-MB division on the
44/45 mid-row early-outs. (5) wait_refs row-progress flag cached as a
field — one test instead of fn call + OnceLock per MB in 1T. Clocks:
screen-cavlc +2.2%, FourPeople +1.7%, akiyo-cavlc +1.0% leans; work
removal deterministic per win. Identity 68/68; suite green.

THE LAST PER-MB SLAB (skip-flag bin / terminate bin / span bookkeeping):
The two BINS are spec-mandated serial arithmetic at engine floor (one
ctx-coded decision + one terminate per MB; the engine itself was closed
at bad5285) — hammered AROUND them instead: (1) knob OnceLock derefs
(no_bskipfast per B skip, no_runmv per P skip) cached as fields; (2)
skip-ctx Option chains replaced with direct bool arithmetic; (3) HOT-
PREFIX SPLIT — b_skip_hot() is the forced-run continuation alone,
#[inline(always)] at both loop call sites, so the 79k forced MBs per
FourPeople decode never pay the cold-body call; the forced arm was
REMOVED from decode_b_skip (single implementation). Counter parity
exact (all dispatch counters byte-identical pre/post). Clocks:
FourPeople +1.4%, screen_text +1.7% leans. Span-key packing priced at
~2 ops/MB and skipped. Per skip MB the remaining costs are now: two
spec bins, three bitmap loads, one span-match, one push — the floor.

B-DIRECT FIVE (the derivation family): (1) COL-PROBE CACHE — col_zero's
per-probe Option + live + long_term + w4 branch chain hoisted to
set_b_context (col_ok/col_w4 fields); the 1T fast path is two grid loads
+ the threshold test. (2) CZ-RELEVANCE GATE — colZeroFlag can only
change a ref-0 list with a NONZERO predicted MV; when neither list
qualifies the probing is skipped entirely in decode_b_direct_n AND the
fp fast-arm. Byte-identical even where old czg split rects: the m
values are equal either way, tiles of the same math (and FEWER b_mc
calls — inter-mc count drops 1,168 on crowd, 635 on FourPeople:
countable). (3) coalesce_region MONOMORPHIZED (&dyn Fn -> generics).
(4) FUSED dual-list b_direct_nbrs — A/B/C availability (bounds + coded
+ slice) computed once for both lists, C-fallback shared (position-
driven, so the per-list gathers always fell back together anyway).
(5) b_mc_or_record's edc_regions Option test hoisted out of the rect
loop. Identity 68/68; suite green; clocks FourPeople +1.3% lean, crowd
flat — op removal countable per win.

B-DIRECT FIVE MORE (batch 6): (1) B_8x8 DERIVATION HOIST — decode_b_
direct_n split into derive + b_direct_region; direct 8x8 subs derive
rid/median ONCE per MB (only czg is per-sub). (2) FORCED DIRECT-16 via
the zero-bi bitmap (same triple as b_skip_hot, weight-gate-free — the
region half runs normal b_mc): gather + derivation skipped for direct-16
MBs inside skip runs, and their commit EXTENDS the chains (bskb_forced
79,183 -> 79,206 on FourPeople; small there, structural everywhere).
(3) Bi-PARTITION FUSED GATHER — mv_neighbors_both generalized to
partition geometry; Bi partitions do availability once for both lists.
(4) PROBE-TIME UNIFORMITY — cz_mixed tracked during the fill; uniform
MBs skip the 16-bool coalesce scan and its recursion outright.
(5) CAVLC PARITY for the forced direct-16 (decode_b_mb's arm).
REFUTED: a predict_mv equal-neighbours shortcut — 8 added compares on
the common non-matching path (inter partitions) leaned crowd -0.3% /
FourPeople -0.5%; bisected out in one cycle. Final: crowd flat,
FourPeople +1.0% lean; identity 68/68; suites green.

PER-MB LOOP GLUE, TEN (batch 7): (1) more_rbsp_data STOP-POS CACHE —
the RBSP stop bit is a constant of the buffer but was rediscovered with
a reverse byte scan TWICE PER CAVLC MB; now one compare. (2)+(3)
rowdb_on/rowhook_eager cached as fields (atomic loads per MB gone).
(4) the CABAC loop's per-MB Truncated bound hoisted to slice entry
(every in-loop path already breaks before continuing; debug_assert
keeps the invariant). (5) edc_flush split into an #[inline(always)]
empty-guard + outlined drain (called per B MB). (6) bz/pz_flush same
split (Option guard inline). (7) wait_refs_for_mb #[inline(always)].
(8) parse_mb_skip/mb_type helpers #[inline]. (9) CAVLC RUN SEGMENTS —
the run length is one syntax element, and after the first skip commits
(0,0) the rest is FORCED: one span extension + one mb_qp fill per ROW
SEGMENT replaces per-MB call + span match + store (worker-gated: jobs
must ride the channel there). (10) the run loop's per-MB addr bound
removed (validated against total upfront). PRICED OUT: left/top
Option->bool churn and mb_skip/mb_direct flag-merge (LLVM niche-packs;
~14-site churn for neutral). Counters exact parity; identity 68/68;
suites green. Clocks: akiyo-cavlc +3.5%, screen-cavlc +4.3%,
FourPeople +2.2% — combined 58/93, z=2.39, over the bar.

RESIDUAL-PLANE GLUE (batch 10) — the theme: conditionally-live residual
planes were UNCONDITIONALLY zeroed, and one family was re-zeroed on
every call. (1) PInterJob/BJob `luma8`/`luma_scan`/`cac` become Option:
the job literal `luma8: luma8.unwrap_or([[0i32;64];4])` re-created the
exact 1 KB memset batch 8 had just removed, on EVERY coded inter MB of
every non-High stream (3 sites). (2) luma_scan (1 KB) materialised only
when a 4x4 luma block is really coded — t8 MBs and cbp_luma==0 skip it.
(3) cac (512 B) only when cbp_chroma==2. (4) I16 q_blocks (1 KB) only
when CBP-luma != 0 — DC-only I16 is common and zeroed 1 KB for nothing.
(5) I4 luma_scan/cac likewise. Consumers take Option<&_> and fall back
to shared `static` zero planes; every read was already gated on a zero
count, so absent == zeros BY CONSTRUCTION. (6) motion-grid gathers
row-sliced at 3 sites (16 strided bounds-checked loads -> 4 slice
copies) + refs.len() hoisted.

LAW BANKED — `get_or_insert(EXPR)` EVALUATES EAGERLY. Four pre-existing
batch-8 sites (`luma8.get_or_insert([[0i32;64];4])`) therefore built a
1 KB array on EVERY call, not just the first, inside a 4-iteration
loop; my first cut of this batch copied a 1 KB static the same way and
measured FLAT (akiyo z=0.18). Switching all 8 sites to
`get_or_insert_with(|| ...)` moved the same stream to z=2.69. When a
lazy-looking API takes a value rather than a closure, it is not lazy.

REFUTED — a "skip the PInterJob in 1T" bypass. EDC is DEFAULT ON
(`edc_on()`, opt out RS_H264_EDC=0), so `!self.edc_active` can never
hold and the branch was unreachable; the 68/68 gate passed while
proving nothing about it (the gate-must-prove-the-tool-ran law, third
instance). Removed, with a comment at the site so it is not retried.

Clocks (ACCEL builds, vs batch 9): akiyo-cavlc +1.4% z=2.69,
shields-main +1.1% z=2.69, blue_sky-1080p-high +2.4% z=2.69,
FourPeople-cavlc +0.7%, FourPeople-high +0.5% — all five positive,
pooled 107/155 arms, z=4.7. Identity 68/68 on BOTH EDC arms.

SETUP/DERIVE GLUE (batch 9): (1) deblock precomputed path: an
all-zero stored MbBs (the dominant class on P content — every flat/skip
MB) now skips the whole per-MB body with two 16-byte compares; every
edge group was early-outing anyway, so control flow past the check is
byte-identical. (2) CAVLC VLC tables fetched once per MB function and
threaded to the ~26 residual-block calls (decode_residual_block_with)
instead of an OnceLock acquire per block. (3) bzero pooled. (4) CABAC
slice scratch pooled: cat/cbp/cmode/nzc/cbf_dc/skip/ref/mvd (+ B-side
ref1/mvd1/direct) were fresh vec![..] at EVERY slice entry — ~407 KB per
P slice, ~700 KB per B slice at 720p — now GridPool-carried and refilled
in place. Clocks (both arms on rusty_alloc-api 1.0.0 — see below):
FourPeople-high +3.5% z=1.98, FourPeople-cavlc +8.2% 27/31 z=4.13,
akiyo-cavlc +7.4% 24/31 z=3.05. Identity 68/68. RE-VERIFIED on ACCEL
builds (the scalar arms above were a build-config artefact — see the
retraction below): akiyo-cavlc +5.6% z=2.33, FourPeople-high +5.6%
z=1.98.

MEASUREMENT TRAP BANKED (and a RETRACTION): the first clocks of this
batch read 0.6x on EVERY stream, z=-5.57, and I blamed a concurrent
rusty_alloc-api 0.3.2 -> 1.0.0 bump. THAT WAS WRONG. A pinned
single-variable A/B (same code, asm on, allocator the only difference)
reads 1.007x z=0.18 — the allocator is innocent. The real cause: the
campaign's fast snapshots were built `--features asm`, which is NOT a
default feature, while every binary I built that evening was a plain
`cargo build` = SCALAR kernels: byte-identical output, ~2x slower. The
comparison was accel-vs-scalar wearing an allocator's clothes.

LAW: every bench binary must be built `--features asm` (verify with
`llvm-objdump -d X.exe | grep -c ymm`). Correctness gates cannot see a
missing kernel feature — 68/68 identity, the decoder suite and the
common suite ALL pass on a scalar build. When a whole-program number
moves ~2x with no mechanism, suspect BUILD CONFIG before dependencies.

CORRECTION (2026-08-27): "byte-identical output, ~2x slower" was itself
only half-audited — it held for the streams the identity gate fed it.
The scalar build was NOT byte-identical on packed-bS-routed streams: its
chroma-deblock arm silently skipped every edge (see §1a). The 68/68 gate
runs the accel CLI, so it certifies ONLY the accel arm; the same cut of
the law in reverse: an identity gate certifies the feature arm it
BUILT, nothing else. Scalar-arm conformance now has its own probe
(`dectest` — since the 2026-08-27 asm-default flip it must be run with
`--no-default-features`) and the fix is in.

LOOP/RESIDUAL GLUE, TEN MORE (batch 8): (1) decode_residual_block now
RETURNS its total_coeff — it always knew it from the coeff_token parse,
and seven call sites were re-counting with 16-element scans (the
round-trip test now pins the returned count too). (2)+(3) CAVLC I16-AC
and chroma-AC zero-skip: an empty AC block leaves the fresh-zero raster
block untouched (un-scanning 16 zeros wrote zeros on zeros). (4) CABAC
I_4x4 recon reads the PARSE's per-block counts (i4n) instead of
re-scanning. (5) CABAC chroma staging: one count, un-scan zero-skipped.
(6)+(7) gather_i4/gather_i8 top rows via row-slice loads — the
bak-vs-rec source branch ran per PIXEL (8-16 times per block), now per
row segment. (8)(9)(10) luma8 Option-gated at all four sites — a 1KB
memset per coded inter/intra MB on every non-t8 tier, now a None.
Clocks: akiyo-cavlc +2.8% (22/31, z=2.33 — after a drift-flipped first
read, replicated per the law), screen-cavlc +1.8%, all-intra +1.8%,
FourPeople flat. Identity 68/68 + all-intra; suites green (77 common).

THE FLUSH DISCIPLINE (two bugs the ARM-DIFF gate caught, both worth
laws): (1) the CABAC loop calls edc_flush PER B MACROBLOCK ("B stays
inline"), so a flush hook there killed every span silently — counters
(spans == span_mbs) exposed it. (2) H-48: the CABAC loop routes NOTHING
through decode_b_mb — direct-16/L0/L1/Bi/B_8x8/intra all read grids
INLINE; a flush placed on the CAVLC-only decode_b_mb path left stale
neighbours feeding spatial-direct MVs: WRONG PIXELS WITH AN IDENTICAL MB
MAP (MVs are invisible in the map — localize with per-frame hashes, not
the map alone). Also: the b_set_motion ABLATION returned an impossible
number (full work FASTER, z=3.7) because skipping the commit corrupts
grids and INFLATES deblock — an ablation is only valid when downstream
work does not depend on the ablated values.

TAX-LAW FINDINGS from this dig (do not chase these): b:chroma-mc ~= b:luma
on the profile build is nested-scope tax (4 chroma scopes vs 2 luma per bi
region), not real parity; "setmot 4.6%" is mostly DecBSet's own scope pairs
(b_set_motion is already row-fill optimized); "dec-mb-loop glue 30.7%"
carries the child scopes' entry/exit tax. Per-MB stages price honestly only
by ablation or work-count counters.

#### deb:derive

DEB:DERIVE CRACKED OPEN (2026-08-21). The old table's flag on this scope --
"INSTRUMENT, NOT WORK... 1.04 calls/MB at 202 ns/call to copy 32 bytes is not
credible; price it by ABLATION" -- was half right, and the half it got wrong is
the useful part. `deb:derive` is TWO SCOPES SHARING ONE LABEL:

| site                                        | granularity | what it does                       |
| ------------------------------------------- | ----------- | ---------------------------------- |
| `mb16.rs derive_bs_row`                     | per ROW     | the actual bS derivation           |
| `deblock.rs filter_frame_rows` (Stage::DebDerive) | per MB | reads the PRECOMPUTED strengths    |

The "32 bytes + two 16-byte compares" is the per-MB one; the 1.04 calls/MB is
the two summed (1/MB + 1/mb_w). Nothing was mispriced by the profiler -- the
label was aggregating two different functions, which is why the arithmetic
would not close. NO ABLATION WAS NEEDED.

CENSUS FIRST (`RS_H264_EDC_STATS=1`, DBSDERIVE line, shipped accel build):

| stream              | MBs     | flat  | allzero | kindarm | kindguard | t8mb   |
| ------------------- | ------- | ----- | ------- | ------- | --------- | ------ |
| screen_text default |  23,760 | 96.8% |  88.9%  |       0 |     5,821 |     20 |
| FourPeople main     | 216,000 | 90.2% |  63.5%  |       0 |    47,899 |      0 |
| akiyo cavlc         |  47,520 | 82.1% |  78.1%  |       0 |    43,803 |      0 |
| blue_sky high       | 489,600 | 77.1% |  32.4%  |       0 |   110,175 | 52,239 |
| shields high        | 216,000 | 56.8% |  18.6%  |       0 |    45,716 | 46,160 |
| crowd_run main      | 489,600 | 38.7% |  11.8%  |       0 |    47,093 |      0 |

Three readings drove everything: `kindarm=0` on 6/6 (the arm a per-MB OnceLock
guard protects NEVER fires), `t8mb=0` on every cavlc/main stream (the `nnz_dbr`
maintenance is dead there), and `flat`/`allzero` dominating LIGHT content.

TWELVE WINS, byte-identity 68/68 on BOTH arms, counters exact-parity after each:

|  # | win                                                    | work removed              |
| -- | ------------------------------------------------------ | ------------------------- |
|  1 | `kind_loads()` OnceLock deref was a per-MB MATCH GUARD  | 5.8k-110k derefs/decode   |
|  2 | `nnz_dbr` 4-row memcpy gated on a new `any_t8` flag     | 1.5-7.8 MB memcpy/decode  |
|  3 | per-row t8 scan gated on the same flag                  | 47k-490k loads/decode     |
|  4 | idc2 `slice_bounds.iter().any()` -> per-picture flag    | 2160-4080 scans/decode    |
|  5 | flat-aware MbBs narrowing (only edge 0 is ever written) | 24 of 32 widen+store, 82-97% of MBs |
|  6 | `pack_mb` row slices (was 16 strided indexes/grid)      | panic_bounds_check 6 -> 1 |
|  7 | ...which unblocked DSE of the dead `MbPack::default()`  | vector stores 17 -> 6     |
|  8 | POC-map presence hoisted out of the 16-block loop       | 16 -> 1 is_empty/MB       |
|  9 | branchless `nnz_mask`                                   | 16 data-dep branches/MB   |
| 10 | all-zero early-out hoisted ABOVE the 128 B zero-init    | 11.8-88.9% of MBs         |
| 11 | `mb_t8` grid load sunk below that early-out             | 1 bounds-checked load/MB  |
| 12 | `pk_differs` HOT-PREFIX SPLIT (32 call sites, `#[inline]` but too big) | ~1.69M calls/decode |

`pack_mb` ASM, once per macroblock: **580 -> 346 instructions (-40.3%)**,
panic_bounds_check 6 -> 1, prologue vector stores 17 -> 6. `pk_differs` no
longer exists as an out-of-line symbol; `derive_mb_records` call sites to it
fell 32 -> 10 (the outlined two-list tail only).

CLOCK (deblock.rs half only -- wins 6-12 -- isolated by stashing that one file,
so the in-flight mb16.rs work is identical in both arms; pinned CORE 22, CPU
time, ABBA, 15 pairs, busy 0.95-0.99):

| stream              | ratio new/old | wins  | z     |
| ------------------- | ------------- | ----- | ----- |
| akiyo cavlc         | 0.959 (+4.3%) | 15/15 | -3.87 |
| screen_text default | 0.980 (+2.0%) | 13/15 | -2.84 |
| FourPeople main     | 1.000         | 10/15 | -1.29 |

Pooled 38/45, z=+4.62. Wins 1-5 (mb16.rs) are counter-verified only -- they
cannot be isolated from the in-flight mb16.rs work by stashing.

LAWS BANKED.
1. **A profiler LABEL can name two functions at different granularities, and
   then its ns/call is a ratio of unlike things.** The tell is exactly what this
   table recorded for weeks: arithmetic that will not close. Check the scope's
   call SITES before ablating -- `grep` for the Stage enum, not just the name.
2. **`cargo rustc -- --emit asm` REPLACES the default emit list**, so no rlib or
   exe is produced and the artifact tree is left mis-tracked. It cost a phantom
   6-stream gate failure (a CONTIGUOUS TAIL of the corpus, with the CLI missing
   afterwards -- a tail, not a scatter, is the signature). Always emit asm into
   a separate `CARGO_TARGET_DIR`.
3. **`powershell -File script.ps1 -Args @('a','b')` does NOT evaluate the array**
   -- it arrives as a literal string and every later named parameter shifts.
   It produced a confident "1.373x REGRESSION" from arms that never received
   their arguments. Invoke with `& .\script.ps1` from inside PowerShell.
4. **`pinvs.ps1` hardcodes affinity to core 2.** With a foreign process pinning
   that core, busy read 0.05 (95% descheduled) and per-pair ratios spanned
   0.72-1.54. Probe cores first; core 22 gave busy 0.97 and the same comparison
   resolved at z=-3.87. Affinity restricts, it does not reserve -- so WHICH core
   is a parameter, not a constant.
5. **Read the SIBLINGS before deleting an initialisation.** Skipping the List-1
   arrays in `MbPack::default()` when `has1 == false` looks free -- `pk_differs`
   only reads them behind `ref1[k] != NO_REF`. But `mb_uniform` hands
   `mvx1`/`mvy1`/`ref1` to the AVX2 kernel UNCONDITIONALLY. Garbage there gives
   wrong uniformity -> wrong bS -> wrong pixels, and garbage that happens to be
   uniform still passes the corpus.
6. **Single-write construction can LOSE to mutate-in-place.** Building the
   record in locals and moving it out measured 603 instrs / 424 B frame; keeping
   `MbPack::default()` + field writes and merely removing the panic edges let
   LLVM DSE the dead fill by itself: 346 instrs / 136 B frame. The structurally
   prettier version was 74% worse. The asm decided it, not the reasoning.

NOT TAKEN: `deb:pack` still records ZERO calls (the packed frame path never runs
on this content) -- unchanged by this batch, still worth one look.

#### deblock

ROW-HOOK RESIDUE CRACKED OPEN (2026-08-21). The 11.3% row-hook residue is the
row deblock filter proper (`filter_frame_rows`, one row per call) plus the EDC
flush. Census first, via a NEW runtime-gated `filtstat` module in deblock.rs —
the existing `census` there is `--features profile` only and therefore cannot
answer a question about the binary that actually ships.

| stream              | MBs at loops | allzero | luma tested | filtered | chroma tested |
| ------------------- | ------------ | ------- | ----------- | -------- | ------------- |
| screen_text default |        2,645 |  88.9%  |      20,929 |   34.1%  |        10,429 |
| FourPeople main     |       78,934 |  63.5%  |     630,551 |   32.6%  |       314,815 |
| akiyo cavlc         |       10,406 |  78.1%  |      83,208 |   57.5%  |        41,584 |
| crowd_run main      |      431,820 |  11.8%  |   3,448,672 |   66.5%  |     1,721,392 |

`luma_tested / mb = 7.99` (all eight groups, every MB) but only 32.6% filter;
`chroma_tested / mb = 3.99`, and those four RE-TEST the same `bs_v[cxe/2]` /
`bs_h[cye/2]` entries the luma loops had just scanned. `thresh` equalled
`luma_filtered + chroma_filtered` EXACTLY — every filtering edge rederived
thresholds although all six internal edges share the macroblock's own QP.

ELEVEN WINS, byte-identity 68/68 both arms, counters exact-parity after each:
lazy per-edge widening (the eager 32-entry u8 to i32 copy per MB is GONE: 2.53M
widen pairs to 1.42M on FourPeople); one 8-entry non-zero MASK per MB replacing
twelve 4-element `.all()` scans; chroma reusing that mask; the cheap
two-16-byte all-zero early-out kept AHEAD of the mask (the dominant class must
not pay for a mask it discards — my first cut got this backwards); `qp_cur`,
`qpc_cur`, internal luma thresholds and internal chroma thresholds all
per-macroblock (`get_or_insert_with`, never the eager `get_or_insert`); eq4 as
one 16-byte compare; and the `rowhook_eager()` / `rowdb_on()` knob reads in
`row_hook` and `publish_progress` taken from the CACHED fields.

`thresholds()` INVOCATIONS (counted at the call itself): akiyo 74,758 to
42,570 (-43.1%), crowd_run 3,594,559 to 2,095,583 (-41.7%), shields -26.7%,
FourPeople -24.2%.

CLOCK (deblock.rs isolated by stashing that one file; core 22, CPU time, ABBA,
15 pairs, busy 0.95-0.99): akiyo-cavlc 0.947x (+5.3%, 15/15, z=-3.87),
FourPeople-main 1.000x, crowd_run-main 1.007x. Cumulative with the deb:derive
batch, whose own contribution was +4.3% on akiyo — so the filter work added
about a point and regressed nothing.

FILTER_FRAME_ROWS: PANICS TO ZERO.

|                        | instrs | panic_bounds_check | frame |
| ---------------------- | ------ | ------------------ | ----- |
| before                 |  3,685 |                 26 | 2,456 |
| after                  |  2,535 |              **0** | 1,112 |
| `derive_mb_general`    |    996 |              **0** | 1,896 |

(Counts are the ACCEL arm — recounted 2026-08-27 after the chroma fix: still
bounds=0, instrs 3,880 with the later batches folded in. The SCALAR arm of the
same function reads bounds=18: it carries the strided line filters, the contig
twins and the fix branch, and has never been panic-hunted — it is a different
function body under one name; scope any future count to its arm.)

THE ROUTE THERE MATTERS MORE THAN THE NUMBER. Three separate index reshapings —
row slices, explicit-length `&x[a..][..n]`, literal chroma indices — left the
count stuck at 25/26 while static instructions ROSE 3,685 to 3,875. A single
up-front `assert!` meant to establish the invariant did not fold anything
either: +78 instructions, +10 panic paths, zero benefit (reverted). What worked
was (a) OUTLINING the derivation the decoder never runs — `derive_mb_general`,
`#[inline(never)]` — which took 26 to 6 because thirteen of the panics lived in
dead code, then (b) giving LLVM its proof AT THE POINT OF USE: the `mb_x > 0`
guard moved INSIDE the closure that indexes `mb_x - 1`, and `& 3` on an index it
would not const-fold out of a two-element loop.

THE MISSING ORACLE (the real find). Writing a test for the blind non-tile arm,
it FAILED — and failed IDENTICALLY at HEAD, before any of this work. Cause: the
fixture randomised `inter` per BLOCK, but `mb_type` is a per-MACROBLOCK element,
so `pack_mb` samples block 0 while the blind scan reads all sixteen. On a
macroblock-coherent (legal) fixture both arms agree. No defect — but that arm
had NO test at all: `bs_arms_agree` pins the per-edge primitives,
`packed_matches_tile` pins the packed derivation, and the decoder never enters
the blind arm so the 68-stream gate is blind to it.
`blind_arm_matches_tile_arm` now closes it, and asserts the fixture actually
filters pixels so it cannot pass vacuously.

POSTSCRIPT (2026-08-27): this section's own code carried a THIRD blind arm the
whole time. The chroma loops above are `cfg`-forked — accel arm and scalar arm
— and only the ACCEL arm (plus both luma loops) consults `pre_bs`; the scalar
chroma arm read the co-located `bs_v`/`bs_h` entries, which the precomputed
path never populates. Result: scalar builds decoded every packed-bS stream
with chroma deblock OFF, shipped in 0.11.0, invisible to the 68/68 identity
gate (an accel binary) and mis-filed as an ENCODER defect in the x264-parity
campaign. Fixed by giving the scalar `chroma_bs` closure the same
`pre_bs`-first branch; ffmpeg full-pixel exact on default/Main/B+pyramid
streams after (§1a). The row-hook census tables above are unaffected — they
were measured on the accel binary.

#### dec-mb-I bodies

TWELVE WINS (2026-08-21). Fixtures: `_xbench/tt_intra_{high,cavlc}.264`,
x264 keyint=1 720p60, both byte-identical vs ffmpeg. Populations on
tt_intra_high: 609,945 zero / 122,925 DC-only / 248,653 sparse / 669 dense
I_4x4 blocks, 151,259 zero-residual 8x8, 1,830,506 DC-only I16 sub-blocks.

Parse side: `mb_nzc[t]` / `[l]` returned `[u8;24]` BY VALUE — a 24-byte stack
copy for the ~10 bytes read (8 sites); `if t8 { break }` sat INSIDE the
sixteen-block recon loop it should guard; `luma_scan.as_ref().unwrap_or(..)` was
re-resolved on every one of those sixteen blocks; `top_ok`/`left_ok` were
computed AFTER the mode loop and then again; and `predict_i4_mode_fast` gives
the twelve interior blocks (of sixteen) the macroblock-level availability
instead of recomputing two `nbr_in_slice` products and two `intra_nbr_ok` tests
~1M times per all-intra decode. `constrained_intra` DEFERS to the original
there — the general form tests this macroblock's own `inter_y` cells for
interior blocks, which the MB-level flags do not model.

Recon side: `gather_i4`'s left column (4 checked strided loads to one span);
`recon_i4_block`'s zero path (the 63.4% arm); `recon_i8_block`'s zero path; and
the one that mattered — **`recon_i8_block`'s coded path wrote PER PIXEL, 64
individually bounds-checked stores per 8x8 block**, now eight row copies. Plus
`predb` written once instead of zeroed-then-filled, `recon_i16_luma`'s 16-sample
left column, and its `modes_y`/`coded_y` sixteen indexed stores to four fills.

CLOCK: tt_intra_high 0.987x (+1.3%, 11/15), tt_intra_cavlc 1.000x. The SPLIT is
the corroboration, not the magnitude: `recon_i8_block` only runs on t8 content
(t8_zero 151,259 on high, ZERO on cavlc), so the fix must show on one and not
the other — and it does.

LAW: a whole-function STATIC instruction count cannot answer a loop question.
It read `recon_i8_block` as +5 panic SITES while replacing 64 stores with 8
copies, and `recon_i16_luma` as +15.7% purely because a second loop appeared.
The clock disagreed with it and the clock was right.

#### dec-mb-P bodies

TEN WINS (2026-08-21), measured on the emitted assembly:

| helper                | instrs        |        | panics   |
| --------------------- | ------------- | ------ | -------- |
| `recon_p_inter_nores` | 972 to 708    | -27.2% | 12 to 0  |
| `parse_mvd_partition` | 269 to 241    | -10.4% |  4 to 0  |
| `recon_p_inter`       | 1017 to 1005  |  -1.2% |  5 to 0  |
| `coalesce_p_inter_mc` | 502 to 519    |  +3.4% |  1 to 0  |
| TOTAL (8 helpers)     | 4725 to 4438  |  -6.1% | 48 to 26 |

The standout is structural, not arithmetic: **`weight_partition`'s identity
early-out lives INSIDE the function**, so an x264 stream — which carries a
`pred_weight_table` in EVERY P slice, identity outside fades — paid sixteen
calls per macroblock to be told there was nothing to do. Hoisting the test to
its callers removes all sixteen. The rest: `recon_p_inter_nores` grid writes to
row fills; `self.refs.len() - 1` re-loaded on each of sixteen iterations;
32 separately-checked plane rows to 3 spans; `mb_ref[l][bi]` is TWO bounds
checks (Vec + array) so the record is bound once; `ref_idx_y` row-sliced at the
three gather sites (the MV row was already slice-copied, the ref row was not);
`recon_p_skip`'s luma plane write; `rect_eq`'s index masked `& 15`; and
`parse_mvd_partition`'s `CACHE30`/`G_SCAN4` masked plus a `clamp(6, 29)` — a
semantic no-op, every `CACHE30` value lies in [7,28] — which makes the
`refc[s-6]` / `mvdc[s-1]` indexes into the 30-entry caches provable.

CLOCK: neutral on all four streams — FourPeople-main 1.015x (7/15),
akiyo-cavlc 0.994x, crowd_run-main 1.000x, shields-high 1.000x; pooled 34/60,
z=+1.03. Expected and reported as such: dec-mb-P is 9.2% of LIGHT and these are
sub-1% instruction reductions inside it, so the ASSEMBLY is the verdict here and
the clock is the no-regression check (codec-measurement 15).

REFUTED AND REVERTED: row-slicing `weight_partition`'s INNER loops measured
+28% instructions — the extents are runtime values, so the slice bounds cost
more than the per-sample checks they replaced. A comment sits at the site so it
is not retried.

TWIN WARNING, live: `recon_p_inter_nores`, `weight_partition` and
`recon_p_skip` each exist TWICE (main path + EDC worker `PixelCtx`). Every fix
above went to both, and `PixelCtx` gained the `weights_l0id` field the main path
already had. A single-site fix here is exactly the routing rot the
twin-convergence law was written for.

#### panics

BOUNDS-CHECK CAMPAIGN (2026-08-21). `panic_bounds_check` call sites across both
library crates, counted on the emitted assembly (`CARGO_TARGET_DIR=target-asm
cargo rustc ... -- --emit asm`, per-symbol attribution between `_ZN...:` and
`.seh_endproc`): **765 to 623, -18.6%**, every step gated 68/68 byte-identical.

TO ZERO: `filter_frame_rows` (26), `derive_mb_general` (17), `derive_mb_kind_into`
(16), `recon_p_inter_nores` (12), `reconstruct_4x4_into` (10), `decode_ipcm` (11),
`reconstruct_4x4_dc_into` (8), `recon_p_inter` (5), `parse_mvd_partition` (4),
`coalesce_p_inter_mc` (1), and every `nzc` access. REDUCED: `derive_bs_row` 35 to
6, `decode_intra_mb` 38 to 24, `inter_finish` 33 to 26, `add_inter_residual` 12 to
5, `parse_residual_cabac` 10 to 5, `col_zero` 7 to 3, `decode_residual_block_with`
7 to 5.

CLOCK (HEAD vs working tree, both arms byte-identical output, core 22, CPU time,
ABBA, busy 0.91-0.99):

| stream            | ratio  | pairs | z     |
| ----------------- | ------ | ----- | ----- |
| tt_intra_cavlc    | 0.907x | 15/15 | -3.87 |
| tt_intra_high     | 0.938x | 15/15 | -3.87 |
| crowd_run cavlc   | 0.951x |  9/9  | -3.00 |
| crowd_run main    | 0.978x |  8/9  | -2.33 |

The ORDERING is the corroboration: CAVLC beats CABAC and intra beats inter,
which is exactly where the fixes sit (`reconstruct_4x4_*` on every intra 4x4 and
every inter residual block; `decode_residual_block_with` is CAVLC-only). akiyo
and the other CIF clips are NOT reported: both arms finish under the harness's
own 15 s resolution floor, so their 1.000x readings are inadmissible rather than
neutral.

WHAT ACTUALLY FOLDS A CHECK — four shapes, and the rule that separates them.

1. ROW SLICES, but only where the extent is a LITERAL. `decode_ipcm` wrote 384
   plane samples and 104 context cells one bounds-checked index at a time; every
   extent there is 16/8/4/2, so one slice per row folds the inner index and it
   went 11 to 0 at -6.1% instructions. The same edit against a RUNTIME extent
   loses: `luma_centre` (six vertical taps + `dst`, extents `bw`/`bh`) removed one
   panic of eight and cost **+9.3%**; `weight_partition`'s inner rows cost +28%.
   Both reverted with the number at the site.
2. STATE THE CEILING an index is already inside. `decode_residual_block_with`
   derives every bound from a `max_coeff` parameter that arrives unconstrained,
   while all its arrays are 16 wide — one `max_coeff.min(16)` (a no-op: every
   call site passes the literal 4, 15 or 16) took 7 to 5 at -3.3%. Same shape as
   `CACHE30[..].clamp(6, 29)` in `parse_mvd_partition`.
3. `.get` PER PARALLEL ARRAY. `col_zero` guarded `idx < ref_idx.len()` and then
   indexed `mv`, `ref_idx1` and `mv1` — separate Vecs whose lengths that guard
   said nothing about, so three panic paths survived the guard that looked like
   it covered them. 7 to 3, -9.6%, and it keeps the fuzz contract.
4. OUTLINE THE DEAD PATH. `derive_mb_general` (`#[inline(never)]`) took
   `filter_frame_rows` 26 to 6 because thirteen panics lived in code the decoder
   never runs.

A WINDOW IS NOT A ROW SLICE, and the two refutations bound it. `recon_i8_block`
already used a window (`&mut rec_y[base..base + 7 * cw + 8]`, then eight
sub-slices): replacing it with eight direct `rec_y[d..][..8]` slices left the
count EXACTLY unchanged at 12 and cost +3.0% — neither form folds, but the window
amortises the base address across the rows. `gather_tile` is the other end: a
window there cost **+57.6%** (908 to 1431). A window pays only when the rows it
spans are written in one tight loop.

THE TWIN LAW FIRED AGAIN, and an assertion is what caught it.
`add_inter_residual`'s 64-store 8x8 loop exists TWICE (main path + EDC worker);
the edit's `count == 1` assertion failed rather than silently fixing one copy.
Its sibling `nnz_y`/`coded_y` anchors are correctly unique — the worker copy
deliberately never writes `nnz_y`. Assert the occurrence count on every anchor in
this file.

THE LAW THAT DECIDED FOUR KEEPS. A whole-function STATIC instruction count cannot
answer a loop question. `add_inter_residual` read 12 to 5 panics at **+0.6%
instructions** while replacing 64 individually-checked stores with 8 row copies;
the static count rises because a second loop structure appears. Kept on that
basis, and the clock agreed (+2.2 to +4.9% on the streams that exercise it).

SECOND PASS: 623 to 557 (-27.2% from the 765 start). Driven by a better
INSTRUMENT — parsing the CodeView INLINE-SITE CHAIN (`.cv_inline_site_id N within
P inlined_at F L`) instead of `.cv_loc` alone. `.cv_loc` reports the INNERMOST
frame, so two thirds of the panics attributed to `index.rs:272` / `index.rs:278`
(the stdlib slice impls) and named nothing. Walking the chain to the outermost
own-source frame turns the census into a ranked worklist of OUR lines, and it
sees inside `decode_slice_cabac_inner` — 14k instructions with no symbol of its
own for any of the callees inlined into it.

| function                     | panics    | instrs         |
| ---------------------------- | --------- | -------------- |
| `inter_finish`               | 26 to 3   | 1502 to 1172 (-22.0%) |
| `recon_i8_block`             | 12 to 5   |  742 to  652 (-12.1%) |
| `recon_i16_luma`             |  7 to 2   |  447 to  378 (-15.4%) |
| `decode_slice_cabac_inner`   | 157 to 140| 14199 to 14087 |
| `recon_i4_block`             |  8 to 6   |  589 to  569 |
| `split_access_units`         |  3 to 1   |              |

A FIFTH SHAPE, and it beats the span form this campaign had been using: a STRIDED
WALK. `gather_i4` carried a comment claiming its span made `col[i * cw] <= 3 * cw
< len` provable — it does not, and the identical shape left FIVE live checks on
`recon_i16_luma`'s sixteen-sample column. `left.iter_mut().zip(rec_y[base..]
.iter().step_by(cw))` does the stride with no index at all and `zip` bounds the
count. Three columns converted; it is what took `recon_i8_block` and
`recon_i16_luma` down, and the false claim is corrected at the site.

THE DENSEST SINGLE SITE was a two-grid gather: sixteen iterations reading `mv_y`
and `ref_idx_y` at the SAME index, where `mv_y`'s bound proves nothing about
`ref_idx_y`, so all thirty-two loads checked. Row slices over both took
`inter_finish` from 21 to 3.

MASKS THAT PAY TWICE. `nc_pred` and `nnz_cache_set` index a 5x5 grid in a
`[u8; 25]` from 4x4 block coordinates every caller already restricts to 0..3, and
both are inlined into the intra path, the inter path AND both slice loops — so
two unprovable indexes were replicated at every call site. `& 3` (and `& 1` for
the 3x3 chroma twins) is a no-op that puts the worst case at 24.

CLOCK: NEUTRAL, and reported as such. crowd_run-cavlc 1.010x and crowd_run-main
1.022x both at z=-0.30 (5/11); the two all-intra clips disagreed in SIGN
(tt_intra_high z=-3.05 at a 1.000x median, tt_intra_cavlc z=+1.39) which is the
below-the-floor signature, not a result. Also note the all-intra clips could not
have shown the biggest win at all — `inter_finish` does not run on them. The
same binary read 641 ms in the previous session and 688 ms in this one, so only
the within-run interleave is admissible here; cross-run deltas are drift.

NEXT, and priced: 28 sites index `refs[refi0 as usize]` / `refs1[refi1 as usize]`
across the B-MC paths and their EDC twin, 12 of them in the two `with_mc_scratch`
closures. Decoder-side B is fully covered by the byte-identity gate, so the edit
is gateable — it is deferred for size, not risk.

THIRD PASS: 557 to 487 (-36.3% from the 765 start; common 176, decoder 311).

FIX THE INSTRUMENT FIRST — AGAIN. The chain walker above picked the OUTERMOST
own-source frame, which for a panic inside an inlined callee names the CALL SITE:
the top of the census read `14x self.decode_mb(...)`, `13x self.decode_i8x8(...)`,
which localises nothing. Picking the INNERMOST own-source frame instead (skip the
stdlib `index.rs` frames, stop at the first line that is ours) names the actual
indexing expression. The two rankings share almost no entries — the second one is
a worklist, the first was a call graph.

WHAT THAT SURFACED, in order: the three hottest sites in the decoder were not in
any function this campaign had looked at.

| site                                     | fix                    | panics |
| ---------------------------------------- | ---------------------- | ------ |
| `cabac.rs` `decode_decision`'s `ctx`      | `ctx_idx.min(459)`     | 2/call |
| `bit_reader.rs` `read_bit`'s `data`       | `.get`                 |  1     |
| `intra_nbr_ok`'s `inter_y`                | `.get`, `unwrap_or(true)` | 11  |

`decode_decision` is `#[inline(always)]` and runs ~154M times per clip. Its `& 127`
comment claimed to prove "every table index in range" — it proves the FUSED index,
and says nothing about `ctx_idx` itself, so BOTH the context load and the
write-back at the end of the function carried a check. `ctx` is `[u8; 460]`, so
`.min(459)` is a no-op. `read_bit` is the sharper lesson: its `pos >= bit_len()`
guard DOES imply the index is in range, because `bit_len()` is `data.len() * 8` —
but that multiply can overflow in principle, which is exactly why LLVM refuses to
fold it. A guard being CORRECT is not the same as a guard being USABLE.

THE REST, all shapes already banked: four parallel grids at one index in
`mv_neighbors_both` (the `coded_y` guard bounds none of the other three); the
`modes_y` pair in `predict_i4_mode` (out-of-range falls back to DC, which is what
every unavailable-neighbour path there already returns); `czg`, a fixed `[[bool;
4]; 4]` indexed by loop bounds that are always <= 4 but are RUNTIME values, masked
`& 3` at all five sites; the `(by + sy) * w4 + (bx + sx)` 2x2 write family, four
sites converted to row fills by one regex pass; `avg_pel`'s three-independent-
slices-per-pixel loop zipped (`avg_full` to ZERO); and `split_annex_b`, the same
`get(i..i + 3)` the decoder's own start-code scanner got.

CLOCK: leans positive, at the edge of resolution. crowd_run-main 0.993x (8/11,
z=-1.51) and crowd_run-cavlc 1.000x median (9/11, z=-2.11) — 17 of 22 paired wins
in the same direction, one clip clearing the |z| > 2 bar and one not. Reported as
"small and consistent, no regression", NOT as a percentage.

STILL OPEN, priced: `gather_tile`'s 33 (refuted TWICE — the cost is the
conditional List-1 slices, so it is at its floor for this technique);
`build_hpel_fused`'s 32 (encoder-only, reached only when the AVX2 kernel declines);
the 28 `refs[refi0 as usize]` sites across B-MC and its twin; and ~18 in the
`bs_branchy`/`inter_bs1` blind arm the decoder never enters.

FOURTH PASS: 487 to 480, and the value here is one REFUTATION and one twin.

GATHER_TILE IS SETTLED — three techniques, three refutations. `Blk::load` reads
SIX parallel grids at one index, 33 panic paths, and it is called 24 times per
macroblock, so any per-field fallback is paid 144 times:

| technique                                | panics | instrs |
| ---------------------------------------- | ------ | ------ |
| baseline                                 |   33   |   983  |
| caller-side window slice of all six grids |   26   |  1431 (+57.6%) |
| `.get` + `let ... else { return }`        |    0   |  2410 (+145%)  |
| `.get` + branchless `unwrap_or`           |    0   |  1521 (+54.7%) |

The let-else form is the instructive one: it reaches ZERO panics and is the WORST
arm, because 24 early-exit paths cost far more than the branchless selects that
replace them. Going branchless recovers most of that and is still +54.7%. Three
independent shapes converging says these checks are at their floor; only an AoS
layout (one array of block records instead of six parallel grids) could move it,
and that is a data-structure change, not a fold. The table is now IN the source at
`Blk::load` so it is not bought a fourth time.

THE TWIN LAW, third instance this campaign. `pack_mb`'s `MbKind::InterUniform`
arm is character-for-character the arm fixed in `derive_mb_kind_into` at the top
of this campaign (48 whole-grid nnz loads for sixteen own cells, 16 panics to 0,
-22% there). It was missed because the two live in different functions with no
shared helper — grep the BODY, not the function name.

Also landed: `Vlc::read`'s `entry[peek_bits(width)]`, the lookup behind every
CAVLC symbol — `entry` is `1 << width` long and the peek cannot exceed it, but
nothing relates the two. Indexing fallibly is free here because a miss yields
`packed == 0` and `len == 0` is ALREADY the corrupt-codeword path two lines down.

FIFTH PASS AND CLOSE-OUT: 480 to 461. **765 to 461 overall, -39.7%.**

Landed here: the weight lookup HOISTED out of the per-sample loops (`apply_luma`
re-read `self.luma[list][refi]` — an array index, a Vec deref and an element
index — on every one of up to 256 luma plus 128 chroma samples per call; the
weight is a property of the PARTITION, not the pixel, and the loop shape is
untouched because row-slicing it is refuted at the same site); `expand_plane`'s
row segment plus two fills; `pk_differs_two_list`'s `k & 15` over `MbPack`'s fixed
`[_; 16]` fields; the second `bzero` neighbour triple via `get(..=addr)`; and
`top_y_px` falling back to 128, the spec's own value for an unavailable sample.

CUMULATIVE CLOCK — final vs HEAD (914152b), both arms verified byte-identical
output, core 22, CPU time, ABBA:

| stream            | ratio  | pairs | z     |
| ----------------- | ------ | ----- | ----- |
| tt_intra_cavlc    | 0.929x | 13/13 | -3.61 |
| crowd_run cavlc   | 0.938x | 11/11 | -3.32 |
| tt_intra_high     | 0.968x | 13/13 | -3.61 |
| crowd_run high    | 0.985x | 11/11 | -3.32 |
| crowd_run main    | 0.984x |  8/11 | -1.51 |

Four of five clear the |z| > 2 bar, all five point the same way, 56 of 59 paired
wins. CAVLC content gains most (+6.6 to +7.6%) and CABAC least (+1.5 to +1.6%),
which is the expected ordering: the CAVLC parse owns `Vlc::read`, `read_bit` and
`decode_residual_block_with`, while the CABAC path's own win was capped once
`decode_decision`'s two context checks were folded.

WHERE THE REMAINING 461 ARE, and why they are staying:

| cluster                              | count | verdict                          |
| ------------------------------------ | ----- | -------------------------------- |
| `Blk::load` / `gather_tile`          |   39  | REFUTED 3 ways, table in source   |
| `build_hpel_fused` + half-pel filters |   28  | runtime extents = refuted shape; encoder-only and doubly cold |
| B-MC `refs[refi as usize]`           |   28  | mechanical but wide, in the codebase's most bug-prone area, no expected speed gain |
| `bs_branchy` / `inter_bs1` blind arm |   18  | decoder never enters it           |
| long tail                            |  ~348 | 2-3 per site across ~140 sites    |

The campaign has reached the point where the distribution is FLAT: after the four
named clusters, no single line carries more than three. That is the signal to
stop hunting clusters — further progress is a grind at 2-3 checks per edit, and
the clock has stopped resolving individual batches for several passes now.

SIXTH PASS: 461 to 437 (-42.9% from 765). Two methodological findings, and a
correction to how this campaign has been reading its own results.

UNBUNDLE A REFUTATION BEFORE YOU TRUST IT. Two of this campaign's recorded
refutations bundled several edits and reverted all of them together, and BOTH
were hiding a win:

- `luma_centre` (+9.3%, "row-slicing the six vertical taps AND `dst`"): the six
  strided source slices were the cost. The DESTINATION row alone is
  **323 to 266 instructions, -17.6%**.
- `weight_partition` (+28%, "row-slicing the inner loops"): slicing was the wrong
  SHAPE, not the wrong goal. `pred_y` is a fixed `[u8; 256]` and `c_pred` a
  `[[u8; 64]; 2]`, so `& 255` / `& 63` prove the indexes outright — both twins to
  ZERO panics, instructions DOWN.

A refutation is only ever of the shape you tested. Record which edits it covered.

WIDTH IS A VARIABLE THE luma_centre REFUTATION NEVER VARIED. Six strided row
slices per output row lose at bw = 4 (they replace 24 checks) and win overwhelmingly
at pw = frame width (they replace ~6 * pw). `build_hpel_fused` has the identical
tap geometry at 1920 wide, and there the same edit is right — together with
stating `vt`/`hb`'s length at the literal `pw + 5`, which is what makes
`x + 5 < pw + 5` provable where a `Vec`'s runtime length cannot be.

THE CORRECTION. This batch is 24 sites and clocks NEUTRAL — crowd_run-main
0.990x (z=-1.51), crowd_run-cavlc 1.007x (z=-0.30). That is not a
disappointment, it is the expected result, and it retires a loose claim:

> panic reduction does not buy speed. Removing PER-ELEMENT WORK ON A HOT PATH
> buys speed, and it happens to show up as panic reduction.

Every measured win this campaign produced came from the second thing:
`reconstruct_4x4_into`/`_dc_into` (row slices on every intra 4x4 and every inter
residual block), `add_inter_residual` (64 checked stores to 8 row copies),
`decode_residual_block_with` (a ceiling on the CAVLC parse), `decode_decision`'s
context pair (~154M calls), `read_bit`. Everything landed on cold or narrow code
— the blind arm, `build_hpel_fused` (encoder, default-off, the tile walk ships),
`expand_plane` (whose only caller is a test) — moved the COUNT and nothing else.

Both are worth doing; only one is worth measuring. The census ranks by count, so
it stops being a performance worklist once the hot per-element work is gone —
which, after six passes, it is.

SEVENTH PASS — "we're not shipping panics". Reframed as a CORRECTNESS standard
rather than a speed lever, and driven hard: **437 to 204, and 765 to 204 overall
(-73.3%)**, six gates, all 68/68 byte-identical.

The work is mechanical once the census names the line. Families cleared, largest
first: `Option::map` into a per-macroblock array (14 sites) becomes `and_then` +
`get`; `CACHE30` is `[usize; 16]` holding 7..28 so `& 15` plus `.clamp(6, 29)`
proves every `refc[s - 1]` / `rc[s - 6]`; **28 per-macroblock `Vec[addr]` WRITES**
become `get_mut`; the `refs`/`refs1` cluster (~20 sites, the one deferred twice
for being wide) resolves at each function's entry, and `b_mc_chroma` carries
`Option<&RefPic>` instead of a bool so "present" and "in range" become the same
fact; the whole CAVLC ENCODER table set (`INV_CBP_*` `[u8; 48]`,
`COEFF_TOKEN_LEN` `[[u8; 68]; 4]`, `RUN_LEN` `[[u8; 15]; 7]`, `TOTAL_ZEROS_LEN`
`[[u8; 16]; 15]`) is fixed-size and takes clamps; the blind deblock arm gets six
`*_at` accessors; and the chroma bilinear MC — three twins — becomes two
`windows(2)` rows.

`windows(n)` IS THE SHARPEST TOOL FOUND. It hands back a slice of EXACTLY n, so
every tap folds outright, where `src[c] ..= src[c + 5]` needed LLVM to relate
`c < bw` to a separate `bw + 5` and it would not. It cleared `luma_h`, both
6-tap hpel loops, and all three chroma bilinear sites.

AND `.get_mut` IS A DROP-IN — the objection that deferring the `nnz` writes would
change the `?` error path was answered by not deferring anything. That single
realisation was worth 35 sites.

**A REAL REGRESSION, MEASURED AND REPORTED: crowd_run-main 1.021x (9/11 pairs,
z=+2.11) — ~2% SLOWER on CABAC content.** crowd_run-cavlc is neutral (1.000x,
z=-0.90). The cause is almost certainly the 28 `Vec[addr]` writes in
`decode_slice_cabac_inner`: each `get_mut` is a branch on the hottest loop in the
decoder, and there are 28 of them per macroblock.

THE FIX WAS APPLIED, AND IT ONLY HALF WORKED — the useful part is WHY.

Every one of those grids is `refill(.., total, ..)`, so binding each as
`&mut v[..total]` inside a scope around the loop ties its length to `total`.
That alone folded NOTHING, because **`debug_assert!(addr < total)` compiles OUT
in release** — nothing carried the invariant across the loop's back-edge. Making
it a real `if addr >= total { break; }` at the loop head (it cannot fire; every
path that advances `addr` already breaks) then folded all fifteen writes and took
`decode_slice_cabac_inner` 13,785 to 13,600 instructions. ONE never-taken branch
per macroblock in place of fifteen per-write ones. `mb_direct`/`mb_ref1`/`mb_mvd1`
are deliberately NOT bound: they are `Vec::new()` on non-B slices, so slicing them
to `total` would be the very panic this campaign removes.

AND THE DIAGNOSIS WAS WRONG. The reborrow form measures IDENTICAL to the
`get_mut` form (1.000x, z=-0.90), so the writes were never the cost. Against the
pre-batch binary the current tree reads 1.014x (7/11, z=+0.90) — down from 1.021x
(9/11, z=+2.11) but now BELOW the significance bar, so it can no longer be
claimed as a confirmed regression. What is honest to say: there is a ~1-2% cost on
CABAC content, distributed across the batch rather than localised, sitting at the
edge of what this box can resolve, with two measurements leaning the same way.
The reborrow is kept anyway — fewer instructions, and one loop-head check is the
right shape regardless of what the clock can see.

EIGHTH PASS: 204 to 144. **765 to 144 overall, -81.2%.** Ten gates this session,
every one 68/68 byte-identical.

WHERE THE REBORROW ARGUMENT BROKE. Binding each grid to `&mut v[..total]` plus a
real `if addr >= total { break; }` at the loop head DID fold the writes near the
head — and did NOT fold the ones ~1,200 lines and many branches deeper. LLVM does
not carry a value range that far. The reborrow is kept (it is worth 185
instructions and is the right shape), but the deep writes went to `get_mut`, which
the clock had already shown costs NOTHING here (reborrow form vs `get_mut` form
measured 1.000x, z=-0.90 — the two are indistinguishable).

A LATENT BUG THE CAMPAIGN FOUND IN ITS OWN WORK. Two masks applied in an earlier
pass were WRONG: `nnzs[(...) & 31]` on a **24**-wide array, and `preds[p & 3]` on
a `[BPred; 2]`. Neither can misbehave (the values were always in range) but
neither proved anything either — a mask is only a proof when it is the array's
own bound. Both fixed (`.min(23)`, `& 1`). **Check the array's declared size when
you write the mask; do not pattern-match the shape of a nearby one.**

CLOCK, and an honest non-result. Across four separate measurements against the
pre-batch binary the CABAC verdict went 1.021x (z=+2.11), 1.014x (z=+0.90),
1.022x (z=-0.30), while CAVLC read 1.000x, 0.990x. Only the first cleared the
|z| > 2 bar and no later run reproduced it. Per the banked law, verdicts that flip
in the same effect band indict the METHOD, not the code: this is ~1-2% sitting
under this box's resolution floor. **No regression is claimed, and none is
denied — the instrument cannot tell at this magnitude.**

REMAINING 144, and what each needs:

| cluster                          | count | needs                                  |
| -------------------------------- | ----- | -------------------------------------- |
| `Blk::load` / `gather_tile`      |   39  | an OWNER DECISION (see below)           |
| half-pel strided taps (`luma_v`, `luma_centre`) | ~11 | the refuted shape at bw = 4; unexamined at other widths |
| `frame_mt.rs`                    |   11  | untouched this campaign                 |
| tail                             |  ~83  | 1-2 per site                            |

`Blk::load` is the only one that is not a grind. Three safe techniques cost
55-145% instructions there. The choices are (a) accept ~55% on a per-macroblock
function, (b) restructure the six parallel grids into ONE array of records —
which fixes it properly and improves locality, but is a design change to a public
API deliberately shaped to avoid a per-frame materialisation, or (c) `unsafe`
behind a length validated once per frame at the `BlockInfo` boundary. (b) is the
right answer for a decoder that eats untrusted input; it is not a change to make
unilaterally.

NINTH PASS: 144 to 75. **765 to 75, -90.2%.** Fourteen gates this session.

**THE GATE CAUGHT A REAL DEFECT I INTRODUCED — the only one all session, and it
was a mask.** `out[(pos[k & 63] as usize) & 15] = level;` in
`parse_residual_cabac`. `out` is `&mut [i32]` — a RUNTIME-length slice of
maxPos+1 — so for an 8x8 block (maxPos = 63) the `& 15` folded coefficients
16..63 onto positions 0..15 and decoded the block wrong. It surfaced as
FF-DIFF on exactly the two 8x8 streams (`tempete_cif__high`, `__default`) and
nowhere else, which is the signature pointing straight at the transform size.

The rule this campaign already wrote — *a mask is a proof only when it IS the
array's own bound* — needed its corollary, and now has it AT THE SITE:
**a runtime-length slice has no such constant, so never mask an index into one.**
Two earlier wrong masks (`& 31` on a 24-wide array, `& 3` on a 2-element one)
were harmless because the values happened to be in range; this one was not. The
difference is that those indexed FIXED arrays and this indexed a SLICE.

The rest of the pass is the same grind, now nearly done: the branchless per-tap
`.get` on `luma_v`/`luma_centre` (6 to 0 and 7 to 1 panics, and INSTRUCTIONS DOWN
2.8% / 4.5% — a third shape for strided taps that beats both the original and the
row-slice that was refuted at +9.3%); the padded-plane publish and its top-pad
replication as row segments and whole-row copies; the last of the `refs`/`refs1`
and `bzero` sites; and the per-block grid quartet (`mv_y`/`inter_y`/`ref_idx_y`/
`coded_y`) written through four disjoint-field `get_mut`s under one test.

REMAINING 75: **39** in `Blk::load` (refuted three ways, needs an owner decision),
**11** in `frame_mt.rs` (untouched), and 25 in a genuine tail.

TENTH PASS — CLOSE-OUT. **765 to 44, -94.2%.** Seventeen gates, every one 68/68
byte-identical. The decoder crate is at **5**; the common crate's **39 are
exactly the `Blk::load` cluster** and nothing else.

FINAL CLOCK, current tree vs HEAD (914152b), both arms byte-identical output,
core 22, CPU time, ABBA:

| stream            | ratio  | pairs | z     |
| ----------------- | ------ | ----- | ----- |
| tt_intra_cavlc    | 0.895x | 13/13 | -3.61 |
| crowd_run cavlc   | 0.917x | 11/11 | -3.32 |
| crowd_run main    | 0.961x | 11/11 | -3.32 |

**35 of 35 paired wins, all three clearing |z| > 2.** Better than the mid-session
reading (0.929 / 0.938 / 0.984), and CABAC main moved +1.6% to +4.1% — so the
"~1-2% CABAC cost" flagged in the seventh pass is GONE. That is the right
epilogue to it: the effect was never confirmed, three later measurements
disagreed with the one that cleared the bar, and the final number is a clean win.

WHAT CLOSED IT: `frame_mt.rs` bound its picture record ONCE instead of indexing
`pics[next_submit]` six times; the CABAC `mc_rect` twins, the last `refs1` sites
and the last colocated read took the `.get`-per-parallel-array shape; and the
hpel builder's `vt`/`hb` writes became three ZIPPED segments rather than three
indexed loops.

THE LAST 5 IN THE DECODER are deliberate: `out[pos[k & 63] as usize]` (a
runtime-length slice — masking it is the defect the gate caught), plus four sites
where the index comes through a helper whose bound is not visible locally.

THE STANDING DECISION. `Blk::load`'s 39 is the whole remainder and the only thing
between this campaign and single digits. Three safe techniques were measured and
all cost 55-145% instructions on a per-macroblock function; the table is in the
source. The choices remain (a) accept ~55%, (b) restructure six parallel grids
into one array of records — the right answer for a decoder eating untrusted
input, and it improves locality, but it changes a public API deliberately shaped
to avoid a per-frame materialisation, or (c) `unsafe` behind a length validated
once per frame at the `BlockInfo` boundary.

CAMPAIGN CLOSED: **765 to ZERO own-code `panic_bounds_check` sites.**

One site remains in the emitted assembly and it is not ours — a monomorphised
`<alloc::vec::Vec<T> as Clone>::clone` from the standard library. Every bounds
check the decoder and the shared kernels write themselves is gone. Twenty-one
gates, every one 68/68 byte-identical, plus the full test suite green.

FINAL CLOCK vs HEAD (914152b), both arms byte-identical output, core 22, CPU
time, ABBA:

| stream            | ratio  | pairs | z     |
| ----------------- | ------ | ----- | ----- |
| tt_intra_cavlc    | 0.897x | 13/13 | -3.61 |
| crowd_run cavlc   | 0.917x | 11/11 | -3.32 |
| crowd_run high    | 0.957x | 11/11 | -3.32 |
| crowd_run main    | 0.962x | 11/11 | -3.32 |

**46 of 46 paired wins.** +11.5% all-intra CAVLC, +9.1% 1080p CAVLC, +4.0-4.5%
CABAC.

THE STANDING DECISION RESOLVED — BY MEASUREMENT, NOT ARGUMENT. `Blk::load`'s 39
were held back for passes on the grounds that all three safe techniques cost
55-145% instructions on a per-macroblock function. That reasoning was never
tested. Taking the branchless form and CLOCKING it: 1.000x (z=-0.90) and 1.008x
(z=+0.30). The +54.7% is real in the assembly and INVISIBLE on the clock, because
`gather_tile` is reached only from `MbKind::Inter` — the residue after the Intra,
Skip and InterUniform fast paths — which is rare enough that 538 extra
instructions per such macroblock do not register. **A static instruction count
cannot answer a dynamic question, and this time the law pointed AT the safe
option rather than away from it.** Two dead helpers (`rid`/`rid1`) fell out and
were removed after checking all four cfgs.

THE LAST FIX IS THE MOST INSTRUCTIVE. `out[pos[k & 63] as usize]` sat untouched
for two passes, labelled DELIBERATE, because an `& 15` there had corrupted 8x8
decode and been caught by the gate. That was the right lesson drawn from the
wrong distinction: the danger was never "bounding a runtime-length slice", it was
**masking** one. A mask silently RELOCATES an out-of-range write; `.get_mut` can
only drop it. Once the two are told apart, the site takes `.get_mut` safely and
the campaign reaches zero. Recorded at the site.

#### entropy decode

### HIGH