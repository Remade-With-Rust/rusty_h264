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

8. **RD-optimized skip** ✅ *done* — P_Skip used to be taken only when the
   residual was *exactly* zero; now it is also chosen when it is RD-cheaper
   (`J = SSD + λ·bits`). The chosen inter mode is **trial-encoded** into a scratch
   writer (real CAVLC bit count + reconstruction SSD) via a per-macroblock state
   snapshot/restore (`save_mb`/`load_mb`/`trial_inter`), then compared against the
   skip's SSD. Bit-exact (P_Skip is valid syntax; the decoder reconstructs the
   prediction). Modest on steady-motion clips (free-skip already catches those),
   −3.7 % at QP36 where small residuals become skippable; the trial-encode
   infrastructure is the foundation for full inter RDO next.

   *Bug found + fixed along the way:* the deblocking boundary-strength derivation
   ignored the **reference index** — spec §8.7.2.1 gives bS 1 when two inter
   blocks reference *different pictures*, which can only happen at `--refs ≥ 2`.
   A latent multi-ref bug (the original multi-ref tests used content that never
   put differing refs across a low-residual edge); `BlockInfo` now carries
   `ref_idx` and bS accounts for it. Refs-1 output is byte-identical.

9. **Full inter RDO** ✅ *done* — every candidate (skip, P_16x16, P_16x8, P_8x16,
   and intra) is now trial-encoded for its real `J = SSD + λ·bits` and the
   minimum wins, replacing the SATD mode heuristic + split penalty + SATD intra
   gate entirely. Motion estimation still finds each shape's MV by SATD; only the
   final mode choice is real RD. **Inter −15 % at QP26 and −22 % at QP36, at
   equal-or-better PSNR** — a strict rate-distortion win and the single biggest
   compression gain in the whole effort (the old heuristic badly mis-weighed
   skip / partition / intra against the actual bit cost; gap to x264 1.4× → 1.15×
   at QP26, and *smaller* than x264 at QP36). Bit-exact; intra-only is
   byte-identical. Cost: ~5 trial-encodes per macroblock (slower encode).

10. **RDO early-termination** ✅ *done* — recovers the speed cost of full RDO with
    no measurable RD loss. Three easy-MB exits, each skipping only trials that
    *cannot* change the pick: (1) **early-skip** — trial the 16×16 first; if a
    zero-residual skip already beats it, this MB is well predicted, so commit skip
    and skip the sub-partition search + intra trial entirely; (2) **sub-partition
    gate** — only run the 16×8 / 8×16 motion search + trials when the 16×16
    residual is heavy (> ~60 coded bits), the tell of a motion boundary;
    (3) **intra gate** — only trial intra (the most expensive candidate) when the
    best inter still needs many bits (> ~200), the tell of a scene cut / occlusion.
    On 50-frame CIF, refs 2: **≈1.7× faster** (QP26 12.2→7.3 s, QP36 11.8→6.9 s)
    at **±0.08 % size / ±0.01 dB PSNR** vs full RDO. Bit-exact (36/36 across
    3 sizes × 4 QP × 3 refs); all-intra byte-identical.

## Tier 4 — Rate control ✅ *done*

**Look-ahead complexity model** (`lookahead.rs` + `rc.rs`). Before a frame is
encoded, a cheap pass scores its complexity — spatial AC SATD for an IDR, the
best small-search motion-compensated residual SATD for a P-frame — so the
controller allocates bits from each frame's *own* complexity instead of a lagging
average of past frames. The model learns `k = bits·Qstep/complexity` per frame
type and allocates `budget ∝ complexity^qcomp` (`qcomp = 0.6`, the constant-bits
↔ constant-quality blend), holding quality steadier across complexity changes.
Encoder-only, so it stays bit-exact.

On a varying-complexity clip (static → fast-motion → static) at a fixed bitrate,
look-ahead lifts **mean PSNR +1.6 dB** vs the reactive controller at the same
rate — the reactive model lags on the motion onset (uses the static average) and
mis-allocates; the look-ahead sees the change immediately. Remaining: rate
adherence still overshoots ~7 % on this clip (a buffer-calibration issue, not
look-ahead-specific); multi-frame look-ahead + mb-tree are future work.

## Tier 5 — Speed (a separate axis from compression)

11. **SIMD the SATD cost kernel** ✅ *done* — the `#![forbid(unsafe_code)]`
    invariant rules out `std::arch` intrinsics, so this uses the pure-Rust
    [`wide`](https://crates.io/crates/wide) crate (safe API; our crates stay
    unsafe-free; no C/C++). `satd_4x4_sum` transforms **four 4×4 blocks at once,
    each in its own SIMD lane** — laying the position-within-block dimension across
    the array of vectors makes *both* Hadamard passes plain across-vector
    butterflies, so there is **no transpose**. Integer math ⇒ bit-identical to the
    scalar SATD (proved by a test and byte-identical output). Paired with a
    **full-pel fast path** in `mc_satd`: the coarse-to-fine diamond walks only whole
    samples, so for interior candidates the prediction is just a reference copy —
    take the residual straight from the reference and skip `mc_luma`'s per-pixel
    sampling. Together: **−14 % (QP26) / −17 % (QP36)** encode time on 50-frame CIF,
    byte-identical, 45/45 bit-exact. With early-termination the encoder is now
    ≈2× the full-RDO baseline.

    Remaining SIMD headroom (left scalar to protect the bit-exact-critical paths):
    the forward/inverse transform and 6-tap `mc_luma` interpolation feed
    reconstruction, so SIMD there must stay exact and is more invasive. **Multi-
    threading** (GOP-parallel for constant-QP, frame-parallel for all-intra) is the
    larger remaining lever and fully safe, but was deferred per the chosen SIMD
    focus.

---

### Working rule for every tier
1. Make the encoder-side change.
2. `cargo test` (unit + the in-tree decoder round-trip).
3. **Re-verify bit-exact vs ffmpeg** across the QP range and intra/inter (the
   conformance bar — a decision change must not alter decodability).
4. Re-run `bench/` vs x264 to quantify the RD/speed delta; record it in
   [benchmarks.md](benchmarks.md).
