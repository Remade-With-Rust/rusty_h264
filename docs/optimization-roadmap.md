# Optimization roadmap

Now that the codec is **bit-exact with ffmpeg across QP 0–51 (intra + inter +
rate control)**, every item here is an *encoder decision* change: the bitstream
stays a valid Constrained-Baseline stream, so a conforming decoder (ffmpeg) keeps
decoding it correctly. **The non-negotiable invariant for every change: re-verify
bit-exact decode agreement with ffmpeg before claiming a win.** We only spend
encoder cycles deciding *what* to code, never bending *how* it's written.

The benchmark data sets the priorities: equal-quality RD gap is **~15–20 % on
intra and ~1.4× on inter**, so motion estimation is the largest lever.

Hard ceilings we cannot cross inside Constrained Baseline (do not chase): **no
CABAC** (CAVLC is the entropy floor, ~10–15 % structurally off the table) and
**no B-frames**.

---

## Tier 1 — Motion estimation (biggest gap, cheapest wins)

`crates/rusty_h264-encoder/src/mb16.rs` → `motion_search` / `mc_satd`. Today ME
minimizes **SATD only**, seeded from `predMV` and `(0,0)` with a small diamond.

1. **Rate-aware ME cost** ✅ *done* — `motion_search` now minimizes
   `J = SATD + λ·bits(mvd)` (λ_me = √λ_mode, mapping bits into the SATD domain),
   so a far MV must *earn* its `mvd` bits. Bit-exact preserved (the rate term is
   only a search heuristic; the chosen MV is still coded correctly). Measured on
   the bench clip (inter, gop 30), smaller at every QP for ≤0.34 dB PSNR cost —
   a net RD win at mid/high QP where MV overhead is a larger fraction of the bits:

   | QP | size vs matched-QP baseline | gap to x264 |
   |---:|:--:|:--:|
   | 26 | −1.4 % | 1.36× → 1.34× |
   | 36 | −8.3 % | 1.37× → **1.19×** |

   Notes: tried seeding the 16×16 search from raw neighbor MVs too — it pulled
   into worse basins on this clip (QP26 +2.6 %), so reverted; the rate term + the
   partition searches seeding from the 16×16 result was the clean win. The
   synthetic clip's regular global motion *understates* the gain (its median
   predictor is already excellent); irregular real motion benefits more.
2. **More search predictors** — seed from neighbor MVs (left/top/top-right), the
   collocated previous-frame MV, and median; refine the best. Escapes local
   minima for free.
3. **Wider full-pel search** ✅ *done* — coarse-to-fine 4-point diamond from 16 px
   down to 1 px (was a 4-px-start diamond), so fast motion the predictor missed
   is reached. **Fast-motion clip: −20 %** (22 px/frame box + 14 px/frame pan;
   67 467 vs 84 828 B); slow bench clip ~neutral (+1.2 %). Encoder-only, bit-exact
   (27/27). Note: an **8-point** (diagonal) variant was tried and reverted — on
   ambiguous/slow motion the diagonal probes chase equally-good far matches that
   wreck MV-field coherence and the neighbor predictors (+15–20 % on the slow
   clip). Keeping the diamond orthogonal was the fix. A predictor early-out (skip
   the coarse steps when the predictor already matches well) would recover the
   slow clip's last ~1 %; deferred as not worth the calibration yet.
4. **Multiple reference frames** ✅ *done* — an N-frame DPB (sliding window) on
   both sides, per-4×4-block `ref_idx` grids, **ref_idx-aware median MV
   prediction** (spec §8.4.1.3.1, generalized from the single-ref special case),
   ME over all refs (with a `ref_idx` rate term), and `ref_idx` coding (`te(v)`
   for 2 refs, `ue(v)` beyond). Single-reference stays **byte-identical** (no
   `ref_idx` coded, predictor reduces to the old path); multi-ref is **bit-exact
   vs ffmpeg, 27/27** across 2/3/4 refs × sizes × QPs. On an occlusion clip (a
   bar sweeping over a static background) 3 refs is **−27 %** vs 1 ref, with
   `ref_idx > 0` genuinely chosen for the revealed regions. CLI `--refs N`.

## Tier 2 — Quantization (recovers the per-QP quality we leave on the table)

Diagnostic: at matched QP we are *smaller but lower-PSNR* → the quantizer drops
more than it should per QP step.

5. **Trellis quantization** — ⚠️ *tried, reverted.* The objection that it fights
   intra feedback was expected to spare inter, but inter has the *same* problem
   through the **reference chain**: dropping a level degrades the frame as a
   reference, inflating later frames (net bigger *and* lower PSNR; no λ scale
   helped). A real win needs CAVLC-accurate rate + reference-propagation-aware λ
   (mb-tree). `transform.rs::trellis_quant` kept as a building block.
6. **Dead-zone offset tuning** ✅ *done* — `quantize` now takes an explicit
   dead-zone divisor (offset `2^qbits / dz`). For **all-intra** the divisor is 2
   (was 3): rounding up more is a net RD win because better-quantized blocks
   predict their neighbors better. **QP26: −2.4 % size, +1.4 dB PSNR**, PSNR gap
   to x264 ~halved. Gated to `gop ≤ 1` (in I+P the IDR is a reference and the
   larger offset hurts P-frames) so **inter stays byte-identical**. Inter divisor
   6 is already the min-size point.
7. **Adaptive quantization (per-MB QP)** — deferred: it trades PSNR for
   perceptual (SSIM) quality, so our PSNR/size bench can't show the win (it reads
   as a regression). Needs an SSIM metric in the harness first. The `mb_qp_delta`
   syntax already exists (always 0 today) and rate control already moves slice QP.

## Tier 3 — Mode decision

8. **Real RDO for inter** — inter mode/partition choice uses SATD + a λ-bias
   (`mb16.rs::encode_slice_data`); upgrade to `SSD + λ·(real bits)` by
   quantizing+reconstructing each candidate, matching the intra I16-vs-I4 path.
   Costs encode time; more accurate.

## Tier 4 — Rate control

9. **Look-ahead / 2-pass** — current ABR (`rc.rs`) is reactive (frame-level
   feedback). A cheap SATD look-ahead pass to estimate each frame's complexity
   *before* allocating bits gives far better distribution — a large part of
   x264's RC edge.

## Tier 5 — Speed (a separate axis from compression)

10. **SIMD** the hot loops (transform, SAD/SATD, 6-tap interpolation, deblocking)
    and **multithreading** (slice- or frame-parallel). Our slowness is
    "unoptimized," not structural — single-threaded, no SIMD today.

---

### Working rule for every tier
1. Make the encoder-side change.
2. `cargo test` (unit + the in-tree decoder round-trip).
3. **Re-verify bit-exact vs ffmpeg** across the QP range and intra/inter (the
   conformance bar — a decision change must not alter decodability).
4. Re-run `bench/` vs x264 to quantify the RD/speed delta; record it in
   [benchmarks.md](benchmarks.md).
