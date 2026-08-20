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

Entry points:
- CAVLC: decode_slice_cavlc_inner reads mb_skip_run — ONE syntax element
  saying "next N MBs are skips" — then loops N times through
  decode_p_skip/decode_b_skip per MB.
- CABAC: one mb_skip_flag bin per MB (P ctx 11-13, B ctx 24-26); runs are
  detected, not read.

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

TAX-LAW FINDINGS from this dig (do not chase these): b:chroma-mc ~= b:luma
on the profile build is nested-scope tax (4 chroma scopes vs 2 luma per bi
region), not real parity; "setmot 4.6%" is mostly DecBSet's own scope pairs
(b_set_motion is already row-fill optimized); "dec-mb-loop glue 30.7%"
carries the child scopes' entry/exit tax. Per-MB stages price honestly only
by ablation or work-count counters.

#### entropy decode

#### deblock

### HIGH