# openh264 ME/MC — every primitive, mapped to ours

Same playbook as CAVLC/transform, **with the transform lesson baked in**: profile first
(don't assume), optimize the actually-hot primitive, measure each step, keep only what
wins. Sources: `common/src/mc.cpp` (MC), `encoder/core/src/sample.cpp` (SAD/SATD),
`encoder/core/src/svc_motion_estimate.cpp` (search).

## A. Motion compensation — `McLuma_c` / `McChroma_c` (common/src/mc.cpp)

`McLuma_c` dispatches on the quarter-pel fraction `(mvx&3, mvy&3)` through a 4×4 table of
16 sub-position kernels. The three 6-tap halves are the real work; the rest are bilinear
averages of those.

| openh264 kernel | frac (x,y) | what | ours |
|---|---|---|---|
| `McCopy_c` | (0,0) | full-pel copy | `mc_luma` int path / `interior_fullpel` copy |
| `McHorVer20_c` | (2,0) | **H 6-tap half** `[1,-5,20,20,-5,1]` | inside `luma_sample`/`mc_luma` |
| `McHorVer02_c` | (0,2) | **V 6-tap half** | inside `mc_luma` |
| `McHorVer22_c` | (2,2) | **center** (H 6-tap on V-filtered) | inside `mc_luma` |
| `McHorVer01/03_c` | (0,1)(0,3) | V quarter = avg(int, V-half) | `mc_luma` |
| `McHorVer10/30_c` | (1,0)(3,0) | H quarter = avg(int, H-half) | `mc_luma` |
| `McHorVer11/13/31/33_c` | diag quarter | avg(H-half, V-half) | `mc_luma` |
| `McHorVer12/21/23/32_c` | edge quarter | avg(center, half) | `mc_luma` |
| `PixelAvg_c` | — | `(a+b+1)>>1` averaging | inside `mc_luma` |
| `McChroma_c` | — | 1/8-pel bilinear (4-tap weights) | `mc_chroma` |

Note: openh264 has **width-specialized** kernels (`WidthEq16/8/4`) so the filter loop has a
compile-time trip count; ours is one generic `mc_luma(rw, rh, …)`.

## B. SAD / SATD cost kernels (encoder/core/src/sample.cpp)

| openh264 | ours | notes |
|---|---|---|
| `WelsSampleSad16x16/16x8/8x16/8x8/8x4/4x8/4x4_c` | `mc_sad` (generic `rw×rh`, `Σ abs_diff` → `psadbw` auto-vec) | **fast preset** full-pel cost |
| `WelsSampleSadFour{16x16,…}_c` (**4 SADs/call**, shared src load) | — we call `mc_sad` 4× per diamond step | **OPPORTUNITY: batch the 4 diamond neighbors** |
| `WelsSampleSatd16x16/16x8/8x16/8x8/4x4_c` | `satd_16x16/8x8/4x4` + `satd_4x4_sum` (SIMD Hadamard) | **quality preset** cost |
| `WelsSampleSatdThree4x4` (3 SATDs/call) | — | intra-combined |
| `WelsSampleSatdIntra{16x16,8x8,4x4}Combined3_c` | intra mode loops in `best_i16`/i4x4 | combined intra mode SATD |

## C. ME search (encoder/core/src/svc_motion_estimate.cpp)

| openh264 | ours | notes |
|---|---|---|
| `WelsMotionEstimateSearch` | `motion_search` | top-level |
| `WelsMotionEstimateInitialPoint` | predictor seeding (`predictors[]` + (0,0)) | start point |
| `WelsDiamondSearch` | the coarse-to-fine diamond loop | fast `[16,4]`, quality `[64,32,16,8,4]` |
| `WelsMeSadCostSelect` / cost+`mvbits` | the `cost` closure (`dist + λ·mvbits`) | rate-aware |
| `WelsMotionCrossSearch`/`WelsDiamondCrossSearch` | — (we use orthogonal diamond) | |
| Feature/hash search (`MotionEstimateFeatureFullSearch`) | — (not implemented) | screen-content opt |

## Already done on our side (don't redo)
- Full-pel SAD auto-vectorizes to `psadbw` (the x264 ME instruction) with no `unsafe`.
- `interior_fullpel` fast path: full-pel candidates inside the frame take the residual/SAD
  straight from the reference — **no interpolation** (the common diamond case).
- Fast preset skips sub-pel entirely (`subme=0` equivalent); SATD only in quality.

## Candidate opportunities (to confirm by PROFILE first — transform lesson)
1. **`SadFour`** — batch the 4 diamond-neighbor SADs into one kernel that loads the source
   block once and computes 4 candidate SADs (openh264's `WelsSampleSadFour`). Shares the
   source load + lets the 4 run wide. The diamond's inner `for &(dx,dy)` calls `mc_sad` 4×.
2. **Width-specialized `mc_luma`** — the final interpolation for the chosen MV (and quality
   sub-pel) uses a generic-width filter; openh264 has 16/8/4 specializations.
3. **`mc_chroma`** — bilinear, every inter MB (2 planes). Profile its share.

## Order
**PROFILE the fast-preset encode first** (phase timers: ME-search vs MC-interp vs
transform vs CAVLC vs recon) to see if ME/MC is even hot for the default path and which
primitive dominates. Then execute the confirmed-hot one, measure on the bench gradient
clip, keep only if it wins.

## RESULTS (measured, 352×288 gop12 qp26 fast, single-thread)

Phase profile (temp timers, cumulative ms over 60 frames):

| | before | after mc fast-path |
|---|---|---|
| ENCODE (residual+transform+quant+cavlc+recon, incl. final MC) | 72.3 | **57.8** |
| SKIP (skip predict + transform-test) | 35.0 (mc 12.4 / tt 13.2 / chroma) | **24.6** (mc **2.1**) |
| ME (motion search, `mc_sad`) | 22.0 | 20.4 |
| DEBLOCK | 12.7 | 12.1 |
| INTRA | 3.3 | 3.0 |

**✅ DONE — `mc_luma`/`mc_chroma` full-pel interior fast path (commit 69c4d4b, +15%).**
The fast preset searches full-pel only, but `mc_luma`/`mc_chroma` sampled every pixel
through `luma_sample`/`at()` with per-pixel bounds-clamping even for integer MVs — so the
P_Skip prediction and every inter MB's final prediction paid interpolation cost for a
verbatim copy. Added the `interior_fullpel` row-copy fast path the SAD/SATD kernels
already had. **140.8→122.3 ms, 43.2→49.7 Mpx/s, byte-identical.**

### Remaining ME/MC items — assessed (don't chase byte-identical no-ops, per the transform lesson)
- **ME search `mc_sad` (20.4ms)** — already `psadbw`-auto-vectorized + interior fast path.
  `SadFour` (batch 4 diamond neighbors) is the openh264 mirror, but the source block is
  already L1-hot across the 4 calls, so like the DCT batching it's unlikely to beat
  auto-vec. **Measure before committing.**
- **Skip transform-test (tt 11.4ms + chroma)** — `skip_luma_is_free` = 16× `forward_core`
  (near-optimal scalar). A cheaper SAD-threshold skip test is openh264's approach but
  **not byte-identical** (changes the skip decision) — a quality/rate tradeoff to raise
  with the user, not a free win.
- **Width-specialized `mc_luma`** — only matters for the *sub-pel* path (quality preset);
  the fast preset is now a copy. Low priority.
- **`mc_chroma` bilinear SIMD** — sub-pel chroma only; the full-pel case is now a copy.
