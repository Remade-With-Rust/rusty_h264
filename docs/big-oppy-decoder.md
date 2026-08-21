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
| per-MB glue (othr) | 47.0  | 29.0 | 20.5  | 2.5     |
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

CALVC AND CABAC ANATOMY AND Entry points:
- CAVLC: decode_slice_cavlc_inner reads mb_skip_run — ONE syntax element
  saying "next N MBs are skips" — then loops N times through
  decode_p_skip/decode_b_skip per MB.
- CABAC: one mb_skip_flag bin per MB (P ctx 11-13, B ctx 24-26); runs are
  detected, not read.

#### KEY per-mb-glue functions

The 47% glue bucket, cracked open (INFO scopes, LIGHT MAIN-tier streams;
percentages are of WHOLE decode, scopes overlap — parents contain children):

| function | file | LIGHT share | what it does per MB |
| --- | --- | --- | --- |
| dec-mb-B bodies | mb16.rs decode_slice_cabac_inner | 50.8% | B-slice MB path — on LIGHT this is almost all B_Skip |
| b-mc (in B) | mb16.rs b_mc | 33.6% | bi-pred: TWO MC passes + blend per skipped B MB |
| b-direct (in B) | mb16.rs b_direct derivation | 32.5% | spatial-direct motion derived per skipped B MB |
| per-MB loop glue | both slice loops | 25.0% | neighbor caches, ctx upkeep, dispatch per MB |
| row-hook | mb16.rs row_hook | 22.0% | per-row bS derive + row deblock + EDC flush |
| dec-mb-I bodies | intra path | 9.0% | I-frames (legitimately dense) |
| dec-mb-P bodies | P path incl. decode_p_skip | 6.5% | P_Skip: skip_mv + grid commits + 16x16 copy-MC |
| mc-stage / resid-add | recon helpers | 3.7 / 2.0% | tiny on LIGHT |
| dec-setup / slice-alloc | grid refill | 3.0 / 0.6% | per-picture |

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

#### entropy decode

#### deblock

### HIGH