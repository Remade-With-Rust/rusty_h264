# Benchmarks

Deterministic A/B vs **x264** (intra-only, matched QP), produced by
[`bench/`](../bench). Both encoders' output is decoded by the **same ffmpeg**
for PSNR, so quality is apples-to-apples. rusty_h264 is pure Rust; x264 runs as an
external process (no C in our build).

- Clip: synthetic, CIF 352×288, 30 frames (gradient + moving box). Note it is
  **gradient-heavy**, which favours plane prediction — real footage shows
  smaller (but real) gains.
- Each rusty_h264 stream is also verified **bit-exact against ffmpeg's decoder**.
- `bpp` = bits per pixel (lower is better). PSNR = luma, dB (higher is better).

## Rate-distortion vs x264, by phase

### Baseline — `I_16x16` DC only
| QP | ours bpp | x264 bpp | ours PSNR | x264 PSNR | size ratio |
|---:|---:|---:|---:|---:|:--:|
| 22 | 0.608 | 0.415 | 44.4 | 45.9 | 1.46× |
| 26 | 0.511 | 0.330 | 43.3 | 45.4 | 1.55× |
| 30 | 0.443 | 0.276 | 41.8 | 43.4 | 1.61× |
| 36 | 0.321 | 0.220 | 38.0 | 42.4 | 1.46× |

Rate-distortion gap (bits for equal quality): **~1.85× behind x264**.

### Phase 1a — `I_16x16` all 4 modes (V/H/DC/Plane) + SATD mode decision
| QP | ours bpp | x264 bpp | ours PSNR | x264 PSNR | size ratio |
|---:|---:|---:|---:|---:|:--:|
| 22 | 0.495 | 0.415 | 50.2 | 45.9 | 1.19× |
| 26 | 0.408 | 0.330 | 47.4 | 45.4 | 1.24× |
| 30 | 0.349 | 0.276 | 45.0 | 43.4 | 1.27× |
| 36 | 0.254 | 0.220 | 40.2 | 42.4 | 1.15× |

Smaller files *and* higher quality at matched QP; at QP 22–30 our PSNR now
exceeds x264's. Rate-distortion gap: **~1.1× behind x264** (down from ~1.85×).

### Phase 1b — add `I_4x4` (9 directional modes) + per-MB I_16x16/I_4x4 decision
| QP | ours bpp | x264 bpp | ours PSNR | x264 PSNR | size ratio |
|---:|---:|---:|---:|---:|:--:|
| 26 | 0.357 | 0.330 | 47.5 | 45.4 | 1.08× |
| 30 | 0.316 | 0.276 | 45.0 | 43.4 | 1.14× |
| 36 | 0.226 | 0.220 | 40.0 | 42.4 | 1.03× |

Within **1.03–1.14× of x264's size** at matched QP (higher PSNR at QP 26–30) —
rate-distortion *competitive* with x264 on intra. Validated bit-exact vs ffmpeg
across resolutions, cropped sizes, and gradient/random/mixed content.

### Phase 1c — chroma modes (DC/Horizontal/Vertical/Plane) + chroma mode decision
At QP 26 on the bench clip (structured chroma gradients):

| | rusty_h264 | x264 |
|---|---:|---:|
| Size | **0.281 bpp** | 0.330 bpp |
| Y-PSNR | **47.5 dB** | 45.4 dB |
| Encode speed | 28 Mpx/s | 69 Mpx/s |

**rusty_h264 now out-compresses x264 at matched QP** (0.85× size, +2.1 dB) on this
clip — but at **0.4× the speed**. Caveats: (1) the clip's chroma gradients
flatter our plane modes; (2) matched-QP PSNR partly reflects x264's psychovisual
tuning (which trades PSNR). The slowdown comes from exhaustive mode search
(always-on I_4x4 planning); early-termination in Phase 3 reclaims it. Speed is
now the tradeoff we spent for compression — the reverse of the gen-1 position.

### Phase 2 — in-loop deblocking filter (enabled)
The filter is bit-exact vs ffmpeg (15/15 across resolutions/content). It is a
post-pass on the decoded frame; it does not change bitstream size but improves
edge quality. With it on, size vs x264 (matched QP):

| QP | ours bpp | x264 bpp | size ratio |
|---:|---:|---:|:--:|
| 30 | 0.250 | 0.276 | 0.91× |
| 36 | 0.195 | 0.220 | 0.89× |

Smaller than x264. Like x264's, our deblocking trades a little PSNR for smoother
perceptual quality, so matched-QP PSNR is now a wash-to-slightly-behind while
size is ahead.

### Phase 3 — RD mode decision + encoder speed (trellis attempted)
Encoder-only changes (bitstream unchanged ⇒ still bit-exact vs ffmpeg):
- **Early-termination**: skip the expensive I_4x4 9-mode search when I_16x16
  already predicts the macroblock near-perfectly. Reclaimed encoder throughput
  from ~28 → ~40 Mpx/s (the I_4x4 search added in Phase 1b had ~halved it).
- **λ-based RD mode decision**: the I_16x16-vs-I_4x4 choice now minimizes
  `J = SSD + λ·R` instead of a raw coefficient count.
- **Trellis quantization**: implemented and measured, but **net-negative** on
  this intra codec — greedy per-coefficient rounding fights I_4x4's serial
  intra-prediction feedback (size grew ~8%). Reverted to scalar quant; the
  routine is kept as a tested building block for a future feedback-aware design.

Net: compression held (~0.90× x264 size at QP 26), speed largely reclaimed.

## Phase 4 — inter prediction (P-frames)

### 4a + 4b(P_Skip) — first working inter, bit-exact vs ffmpeg
Multi-frame pipeline (IDR + P-frames), P-slice syntax, reference management,
encoder-side deblocking, **inter-aware deblocking boundary strengths**, and the
`P_Skip` macroblock (copy from the previous deblocked frame when the residual is
free). Validated bit-exact vs ffmpeg across static / brightness-drift / noisy
content at multiple resolutions (9/9).

Compression from `P_Skip` alone (QP 26, vs all-intra `gop=1`):

| Clip | all-intra | with P-frames | |
|---|---:|---:|:--:|
| 6 static frames (176×144) | 23 640 B | 8 432 B | **0.36× (2.8× smaller)** |
| 10 frames, moving box on static bg | 19 566 B | 14 108 B | **0.72×** |

`P_Skip` collapses static regions to ~nothing; moving regions still fall back to
intra until `P_L0_16x16` (motion-compensated residual) + sub-pel land. Even so,
this is the temporal-redundancy lever turning on — the first real video
compression in the codec.

### 4b(ii) + 4c — `P_L0_16x16` with quarter-pel motion compensation
Motion-compensated macroblocks: full-pel diamond + half/quarter-pel motion
search, median MV prediction, `mvd` coding, inter `coded_block_pattern`, inter
residual, and the **6-tap/bilinear quarter-pel luma + eighth-pel bilinear chroma
interpolation** filters. Bit-exact vs ffmpeg across static / sliding / noisy /
zoom motion at multiple sizes (16/16).

Moving-box clip (176×144, 8 frames), QP 26:

| | all-intra | with inter (Skip + P_16x16) | |
|---|---:|---:|:--:|
| size | 15 833 B | 3 308 B | **0.21× (4.8× smaller)** |

Motion compensation now tracks moving regions at quarter-pixel precision, so the
moving box is predicted from the previous frame with a tiny residual. The
temporal lever is fully engaged: this is a real video codec.

### 4d — sub-partitions `P_16x8` / `P_8x16` (two motions per macroblock)
Macroblocks split into two partitions with independent motion vectors, including
the **directional MV predictors** (spec §8.4.1.3.2: the top/left/above-right
neighbor used directly when inter, else median) and per-partition motion search.
The encoder picks 16×16 / 16×8 / 8×16 by SATD (multi-partition modes pay a λ-bias
for their extra `mvd`). Bit-exact vs ffmpeg across split-horizontal /
split-vertical / diagonal / box motion at multiple sizes (12/12); on a
split-motion clip the partition modes are chosen for ~42% of coded MBs.

Split-motion clip (176×144, top half pans right, bottom half pans left), QP 26:

| | all-intra | full inter (Skip + 16×16/16×8/8×16) | |
|---|---:|---:|:--:|
| size | 25 022 B | 4 416 B | **0.18× (5.7× smaller)** |

This completes the Constrained-Baseline inter toolkit (P_8x8's deeper 8×4/4×8/4×4
sub-divisions are the only omitted partition shapes — diminishing returns).

## Rate control (average bitrate)

A frame-level controller that varies the per-frame QP to hit a target bitrate,
combining a **complexity model** (predict a frame's bits at a candidate QP from
recent I/P history) with a **leaky-bucket buffer** (correct accumulated
over/undershoot). The QP rides in each slice's `slice_qp_delta`, so conformant
decoders need no cooperation — and it stays **bit-exact vs ffmpeg** at every
bitrate. Enable with `--bitrate <bps> --fps <f>` (0 = constant-QP).

Convergence on a 60-frame 176×144 clip @ 30 fps (gop 30), achieved vs target:

| target | achieved | ratio |
|---:|---:|:--:|
| 200 kbps | 213 kbps | 1.07× |
| 500 kbps | 524 kbps | 1.05× |
| 1.0 Mbps | 873 kbps | 0.87× |

Within ~13% where the target is achievable; beyond ~1 Mbps this tiny clip can't
fill the rate even at its QP floor, so the controller correctly pins QP at the
minimum (the target is a ceiling, not a mandate).

### Bug fixes surfaced by low-QP rate control
Driving QP down to hit high bitrates exposed two **pre-existing** correctness
bugs (latent because earlier tests never coded below QP 14), now fixed and
covered by tests:
- **Inverse transform order.** The 4×4 integer inverse transform was applied
  columns-first; the spec (§8.5.12.2) and ffmpeg do **rows-first**. Because the
  `>>1` flooring inside the butterfly makes the integer transform non-separable,
  the two orders diverge by ±1 on asymmetric blocks — invisible except at low QP
  / high-frequency content. (Pinned by `inverse_core_is_row_first`.)
- **CAVLC extended escape.** `level_prefix` was capped at 15 with a 12-bit
  suffix; very large coefficient levels (low QP) need the extended escape
  (`level_prefix ≥ 16`, growing suffix) or they truncate. (Pinned by
  `extreme_levels_use_extended_escape`.)

## Head-to-head vs x264 (current)

Deterministic CIF clip (scrolling gradient + moving box), 60 frames, matched QP,
both encoders' output decoded by the same ffmpeg for PSNR. `bpp` lower is better;
`size` is rusty_h264 ÷ x264.

### Intra (all-I), after Tier-2 dead-zone tuning
| QP | rusty_h264 bpp · PSNR | x264 bpp · PSNR | size |
|---:|---:|---:|:--:|
| 22 | 0.394 · 44.4 dB | 0.417 · 45.7 dB | 0.94× |
| 26 | 0.291 · 44.1 dB | 0.331 · 45.3 dB | 0.88× |
| 30 | 0.256 · 43.1 dB | 0.277 · 43.4 dB | 0.92× |
| 36 | 0.208 · 40.2 dB | 0.221 · 42.2 dB | 0.94× |

rusty_h264 is **smaller than x264 at matched QP** and now within **~1 dB PSNR**
(was ~2–3 dB before dead-zone tuning). On an equal-quality basis it is roughly
**rate-distortion competitive** on intra — near-matched at QP 30 (43.1 vs
43.4 dB at 0.92× the size). See the Tier-2 note below for what changed.

### Inter (I+P, gop 30)
| QP | rusty_h264 bpp · PSNR | x264 bpp · PSNR | size |
|---:|---:|---:|:--:|
| 26 | 0.131 · 48.2 dB | 0.097 · 50.2 dB | 1.36× |
| 36 | 0.078 · 40.6 dB | 0.057 · 43.8 dB | 1.37× |

x264's mature motion estimation (sub-pel refinement, more partition shapes,
trellis) keeps it **~1.2–1.3× ahead on inter** (after Tier-1 rate-aware ME below;
~1.4× before). rusty_h264's P-frames are correct and real, but the ME is a
diamond + sub-pel pass.

### Tier 1 — rate-aware motion estimation
`motion_search` now minimizes `J = SATD + λ·bits(mvd)` (it used to minimize raw
SATD), so a motion vector must *earn* the bits its `mvd` costs against the
predictor. The rate term is a search heuristic only — the chosen MV is still
coded correctly, so the streams stay **bit-exact vs ffmpeg**. Inter, gop 30,
matched-QP, vs the pre-Tier-1 baseline and vs x264:

| QP | size Δ | gap to x264 (before → after) |
|---:|:--:|:--:|
| 26 | −1.4 % (≤0.25 dB) | 1.36× → 1.34× |
| 36 | −8.3 % (≤0.34 dB) | 1.37× → **1.19×** |

The gain grows with QP exactly as theory predicts: when the residual is cheap
(high QP / low bitrate), MV bits are a larger share, so trimming them helps more.
Intra is unaffected (rate-aware ME only touches P-frames). The synthetic clip's
regular global motion understates the benefit — its median predictor already
nails the motion, leaving little for the rate term to fix; irregular real motion
gains more.

### Tier 1 — wider full-pel search
The motion search is now coarse-to-fine, a 4-point diamond stepping from 16 px
down to 1 px (it used to start at 4 px), so fast motion the predictor misses is
actually reached. The slow bench clip can't show this (its motion is ≤4 px/frame,
already in range), so this is measured on a deliberately fast clip — a box moving
22 px/frame over a background panning 14 px/frame:

| | narrow (old) | wide (new) | |
|---|---:|---:|:--:|
| fast-motion clip, QP 26 | 84 828 B | 67 467 B | **−20 %** |
| slow bench clip, QP 26 | 98 283 B | 99 455 B | +1.2 % |

Big win where there's real fast motion, ~neutral otherwise — net positive for
general content (still bit-exact, 27/27). An 8-point (diagonal) search was tried
and reverted: on ambiguous/slow motion the diagonals chase equally-good far
matches that wreck MV-field coherence (+15–20 % on the slow clip).

### Tier 1 — multiple reference frames
P-macroblocks may now reference any of the last N decoded pictures
(`--refs N`, default 1). Implemented end-to-end: an N-frame DPB with sliding-
window management on both sides, per-block `ref_idx` grids, **ref_idx-aware
median MV prediction**, motion search over all references, and `ref_idx` coding
(`te(v)` / `ue(v)`). Single-reference output is byte-identical; multi-ref is
**bit-exact vs ffmpeg (27/27)** across 2/3/4 refs.

The bench clip (steady motion, previous frame always best) shows nothing — the
payoff is on occlusion/periodic motion, so it's measured on a clip where an
opaque bar sweeps across a static background, revealing regions last seen 2–3
frames earlier:

| refs | size | non-zero ref_idx |
|---:|---:|---:|
| 1 | 21 535 B | 0 |
| 2 | 20 648 B | 156 |
| 3 | **15 741 B (−27 %)** | 212 |

The encoder genuinely selects an older reference for the revealed background.

**Tier 1 (motion estimation) is complete**: rate-aware cost, coarse-to-fine
search, and multiple references — all bit-exact.

### Tier 2 — quantization
- **All-intra dead-zone tuning ✅** (`quantize` deadzone divisor 3 → 2 for
  all-intra). The rounding offset is encoder-only, so any value stays bit-exact.
  Counter-intuitively, rounding *up* more is a net RD win on intra: better-
  quantized blocks predict their spatial neighbors better, shrinking downstream
  residuals. Result at QP26: **−2.4 % size and +1.4 dB PSNR** vs the old offset,
  cutting the PSNR gap to x264 roughly in half. Gated to all-intra (`gop ≤ 1`):
  in an I+P stream the IDR is a reference, and the larger offset there hurts the
  P-frames, so **inter output is byte-identical**. (`DZI=1` over-rounds and is
  catastrophic; `2` is the sweet spot.)
- **Trellis quantization on inter — tried and reverted.** The simplified RDOQ
  (per-coefficient scalar-or-lower) is net-negative in a tight P-prediction
  chain: dropping a level degrades the frame *as a reference*, inflating every
  later frame. No λ scale helped (≤0.25 was a no-op, ≥0.5 strictly worse). A real
  win needs CAVLC-accurate rate modeling + reference-propagation-aware λ
  (mb-tree) — x264-level infrastructure, deferred.
- **Inter dead-zone is already optimal** (divisor 6 is the min-size point at
  QP26; smaller costs bits, larger backfires via the reference chain).

## Speed

Honest accounting: rusty_h264 is single-threaded with no SIMD; x264 is a
two-decade-optimized C encoder. At CIF, rusty_h264 encodes all-intra at ~35 Mpx/s.
The harness spawns ffmpeg as a separate process, so x264's measured time
includes process startup (~50 ms) — even so, at 60 frames x264 matches or beats
rusty_h264's throughput, and on inter (where rusty_h264's per-MB motion search over
three partition shapes dominates) x264 is far faster. **We do not claim a speed
advantage**; SIMD and multithreading are open optimization headroom. The trade
rusty_h264 makes is memory safety + a permissive license + bit-exact conformance,
not raw throughput.

## Methodology

```sh
cd bench
export RUSTY_H264_BENCH_FFMPEG=/path/to/ffmpeg   # built with libx264
# intra:  --gop 1   |   inter: --gop 30
cargo run --release -- --width 352 --height 288 --frames 60 --qp 26 --gop 1 --ref-codec libx264
```
