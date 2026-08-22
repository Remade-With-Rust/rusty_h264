# Phase 4 — Inter prediction (P-frames)

The temporal-redundancy lever: predict each frame from the previous one, code
only motion + small residual. Targets Constrained Baseline → **P-frames only**
(no B-frames, no CABAC — the structural ceiling).

## Architecture change: multi-frame pipeline

Today the encoder/decoder process each frame independently (all IDR). Inter
needs cross-frame state:

- **Encoder/Decoder hold a reference**: the previous *deblocked* reconstruction
  (coded size). GOP = one IDR, then P-frames.
- **Inter references are deblocked** → the encoder must now deblock its
  reconstruction (it didn't before). Intra prediction *within* the current frame
  still uses pre-deblock current samples; inter prediction uses the deblocked
  reference.
- 1 reference frame (previous picture), `ref_idx = 0`.

## Sub-phases (each validated bit-exact vs ffmpeg)

**4a — Multi-frame + P-slice scaffolding (no motion yet).**
Reference plumbing, encoder deblocking, P-slice header (slice_type P, frame_num,
num_ref_idx, dec_ref_pic_marking), `mb_skip_run` wrapper in slice data. MBs still
coded *intra* (valid in P-slices). De-risks all the syntax/plumbing in isolation.

**4b — Motion compensation + `P_Skip` + `P_L0_16x16` (full-pel).**
The bulk of the win. Reference fetch (full-pel = copy), `P_Skip` (predicted MV,
no residual — huge for static regions), `P_L0_16x16` (one MV + residual),
full-pel motion estimation (diamond/small search), median MV prediction, `mvd`
coding, inter residual (offset/6). ME is encoder-only → affects compression, not
conformance; MC reconstruction must be bit-exact.

**4c — Sub-pel interpolation.** Half-pel 6-tap filter + quarter-pel bilinear.
Precision-critical (must match ffmpeg's exact integer filter). Big match-quality
gain.

**4d — Sub-partitions.** `P_16x8`, `P_8x16`, `P_8x8`. Refinement on top of 4b/4c.

## Validation
Every step: ffmpeg decodes our multi-frame stream bit-exact. The interpolation
filter (4c) and MV prediction are the precision-critical pieces, like CAVLC and
deblocking were.

## Expected payoff
Several-fold size reduction on real sequences (P-frames cost a fraction of an
I-frame). Makes "rusty_h264 vs real x264" a meaningful comparison. Ceiling: no
B-frames/CABAC (Baseline), so ~20–30% behind Main/High on the same content but
on par with x264-baseline.
