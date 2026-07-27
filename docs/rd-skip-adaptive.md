# The adaptive RD P_Skip decision

**Status:** built, conformance-gated, opt-in (`tune_rd_skip`, default off).
Decision cost optimized 0.73× → ~0.89–0.96×, all byte-identical (§6).
**Result:** on the Fast preset, −10.1% BD-SSIM on akiyo and −4.8% on FourPeople with
**no clip regressing on either metric**. On the sub-pel presets the same feature is
worth ~1%, because sub-pel already captures most of the same win (§5).

---

## 1. The gap

Our P_Skip criterion is **exact-zero residual**: a skip is taken only if the skip
prediction reproduces the source bit-for-bit after quantization. x264's is **RD** —
it compares `J = SSD + λ·R` for skipping against coding and takes whichever is
cheaper, so it also skips macroblocks whose residual is small-but-nonzero.

Measured against x264, our skip rate matches at both extremes and falls **4–24×
short in the middle** — exactly the band where a residual is nonzero but not worth
its bits.

## 2. Why the straight port loses

Implementing the RD criterion for every P macroblock is a **clean content
sign-flip** (Fast preset, 4-QP BD-rate, forced on everywhere):

| clip | free-skip rate | BD-PSNR | BD-SSIM | |
|---|---:|---:|---:|---|
| akiyo_cif | 72.5% | −11.60 | **−13.06** | WIN |
| FourPeople | 58.7% | −11.68 | **−5.58** | WIN |
| foreman_cif | 6.4% | −2.92 | +7.64 | PSNR-only |
| mobile_cif | 1.0% | −0.80 | +1.36 | PSNR-only |
| in_to_tree | 1.0% | +12.40 | **+34.05** | LOSS |
| stockholm | 0.3% | −3.61 | **+95.70** | LOSS |

The failure mode is legible in the metric split: the decision's distortion term is
**SSD**, so it trades structure for mean-squared error. On detailed content that is
a large SSIM loss while PSNR still improves — the "PSNR-only" rows are the decision
gaming its own objective.

## 3. The separating signal

The content's **own free-skip rate** — how much of it is already exactly redundant —
separates the two groups with a wide natural gap: winners ≥58.7%, losers ≤6.4%.
Nothing in between.

The signal was **pinned by a held-out prediction before measuring**: FourPeople
(58.7%) was predicted a WIN on both metrics on the strength of the signal alone, and
measured −11.68 / −5.58. It is a real separator, not a post-hoc fit.

## 4. The dispatch

An **online, within-frame** gate:

* count free skips over the frame as it is encoded;
* once past a learning window (`mb_w·mb_h/8`, floor 64 macroblocks), enable RD skip
  for the remainder of the frame iff the running free-skip rate clears the bar.

Two properties matter:

* **Within-frame, so it stays deterministic under GOP-parallel encode.** Cross-frame
  learned state would make the output depend on how frames were scheduled.
* **The estimate is RUNNING, not frozen at the window boundary.** Freezing samples
  only the top eighth of the frame, which on spatially non-stationary content
  (foliage under flat sky) is not representative — in_to_tree sat at +0.26% SSIM
  under a frozen estimate and went to −0.01% under a running one.

### The calibration

Threshold 60 on Fast — the *smallest* bar at which nothing regresses, so it keeps
the most upside (50 leaves in_to_tree at +0.19%):

| clip | BD-PSNR | BD-SSIM |
|---|---:|---:|
| akiyo | −8.25 | **−10.12** |
| FourPeople | −5.41 | **−4.84** |
| stockholm | −0.75 | −0.99 |
| foreman | −0.10 | −0.13 |
| in_to_tree | −0.24 | −0.01 |
| mobile | 0.00 | 0.00 |

stockholm is the clearest evidence the gate is on the right axis: forced on it is
**+64.3% BD-SSIM**, and at *every* threshold from 10 to 70 it is a flat −3.4% win.
Invariance-to-strength is the tell that the gate is selecting on content rather than
getting lucky — the gate blocks the frames where RD skip is catastrophic and keeps
the minority where it pays.

## 5. The preset caveat — RD skip and sub-pel are SUBSTITUTES

The threshold does **not** transfer across presets. Sub-pel refinement predicts
better, so it lifts the free-skip rate on *all* content; the same absolute bar then
starts admitting content that loses. On Balanced, threshold 60 puts in_to_tree back
to +0.49% SSIM, and the bar has to rise to 90 to clear it — at which point the wins
collapse:

| clip | Fast @60 (BD-SSIM) | Balanced @90 (BD-SSIM) |
|---|---:|---:|
| akiyo | −10.12 | −0.99 |
| FourPeople | −4.84 | −0.20 |
| stockholm | −0.99 | −0.04 |
| in_to_tree | −0.01 | −0.11 |

So the honest reading is that **RD skip and sub-pel harvest largely the same
redundancy**. Sub-pel already shipped as the bigger lever (−42% to −50% BD-rate, and
why `Preset::Balanced` is the default); RD skip is mostly redundant with it. That is
why this stays **opt-in** rather than becoming a default: on the default preset it
buys ~1% for a real speed cost, and on the preset where it buys 10% (Fast) the whole
point of the preset is speed.

`tune_rd_skip_min_free: None` resolves to 60 on Fast and 90 elsewhere; an explicit
`Some(v)` is always honoured so the threshold stays sweepable.

## 6. Cost — and removing the double encode

Interleaved A/B in one process (this box drifts ~20% run to run, more than the
effect), best-of-5 per arm, QP27:

| clip | first cut | shipped | size |
|---|---:|---:|---:|
| akiyo_cif | 0.73× | **0.87–0.92×** | −13.6% |
| FourPeople | 0.90× | **0.94–0.99×** | −7.0% |
| mobile_cif | 1.00× | **1.00×** | 0.00% (byte-identical) |

(Ranges, not points: the interleaved ratio still moves ~0.05 run to run on this box.
Reported from eight alternating runs.)

**Where the gate blocks, the cost is zero and the output is byte-identical to the
default path** — pinned by a test, not just observed. Every step below is likewise
byte-identical: same decisions, less work. The BD-rate is unchanged to the digit
(akiyo −8.252 / −10.120 before and after), which is the real proof, since byte
equality was only spot-checked at one QP while BD-rate spans four.

### 6.1 The skip SSD needs no state mutation

A skip carries no residual, so its reconstruction *is* its prediction. The
`commit_skip` → `mb_ssd` → `load_mb` round trip was paying a full macroblock
save+restore on every candidate, including candidates that went on to code. A
direct `pred_ssd` over the prediction buffers replaces it; the equivalence is
enforced by a `debug_assert` against the old computation rather than assumed.

### 6.2 Encode once and splice, instead of trial-then-repeat

The decision needs `(SSD, bits)` for the coded arm, which means encoding it. The
original shape trial-encoded into a scratch writer, **threw the result away**, and
then encoded the same macroblock again on the path that codes.

The fix is to keep it. The macroblock is encoded once, into scratch, and its
committed state is retained; if the skip loses, those scratch bits *are* the real
bits and are spliced into the slice. That needed `BitWriter::append`, a bit-level
concatenation at arbitrary misalignment (tested across 17 lead offsets × 40
payload lengths). It is sound for CAVLC precisely because a macroblock's
Exp-Golomb/VLC syntax does not depend on its bit position — **an arithmetic coder
could not be spliced this way**, which is why RD skip stays on the CAVLC path.

### 6.3 The snapshot was ten heap allocations

`MbState` is ten `Vec` fields, so `save_mb` allocated ten times — per candidate,
on every candidate. Refilling a reused buffer (`save_mb_into`) leaves the region
size fixed, so after the first call it is a pure copy. This was the single largest
remaining cost: akiyo 0.86× → 0.92×, FourPeople 0.94× → 0.96×.

### 6.4 No sound early-out exists — two bounds, both 0.0%

The remaining waste is the coded arm being encoded and then discarded. Removing it
without changing decisions needs a cheap LOWER BOUND on `J(code) = SSD_c + λ·R_c`
that sometimes exceeds `J(skip)`. Two were built and measured:

1. **Global bit floor.** `SSD_c ≥ 0` and the inter syntax has a 4-bit floor
   (`mb_type` ue(0), two `mvd` se(0), cbp me(0)), so `j_skip ≤ 4λ` proves the skip
   wins. **Fired 0.0%** of candidates at QP22/27/32/37.
2. **Exact per-macroblock header floor.** The bits actually written before `cbp`
   (`mb_type` + `sub_mb_type` + `ref_idx` + this macroblock's real `mvd`s) are an
   exact lower bound, typically 15–20 bits rather than 4 — a ~5× tighter bound,
   measured by instrumenting the emit. **Also fired 0.0%**, at every QP, on both
   clips. (A `debug_assert` confirmed the bound never exceeded the actual bits, so
   the 0.0% is real and not a broken probe.)

The diagnosis is the same both times, and it is structural: **skips do not win
because `SSD(skip)` is small — they win because the coded arm's RATE is large.**
Coded macroblocks run to hundreds of bits, so a bound built from a ~20-bit floor is
never within reach of `j_skip`. No tightening of the rate floor can close a gap
that large, which retires the whole "split `plan_inter_mb` to get the header cost
early" plan — the probe cost one instrumented run and saved that refactor.

### 6.5 The search-skip gate — measured, priced, and defaulted OFF

If no sound bound exists, the only way to stop encoding the discarded arm is to
stop *pricing* it: a search-skip gate that takes the skip whenever the null arm's
cost is below a threshold, `SSD(skip) ≤ λ·T`. Not byte-identical, so BD-rate gated.

Harvesting every decision gives the trade directly (QP27):

| T | akiyo: encodes killed / decisions changed | FourPeople |
|---:|---:|---:|
| 40 | 7.5% / 0.15% | 24.4% / 0.18% |
| 80 | 21.4% / 0.98% | 51.8% / 1.29% |
| 120 | 34.2% / 2.39% | 70.6% / 2.74% |
| 180 | 51.7% / 6.54% | 84.4% / 6.34% |

And the net frontier against **no** RD skip — the number that actually decides:

| T | akiyo BD-SSIM / speed | FourPeople BD-SSIM / speed |
|---:|---:|---:|
| 0 (exact) | **−10.12** / 0.88× | **−4.84** / 0.93× |
| 40 | −9.63 / 0.90× | −4.47 / 0.98× |
| 80 | −8.09 / 0.94× | −2.87 / 0.99× |
| 120 | −6.47 / 0.94× | −1.12 / 1.06× |
| 180 | −4.51 / 1.05× | **+0.83 (regresses)** / 1.04× |

Every point prices at roughly **0.3% BD-SSIM per 1 point of speed** — 3–10× worse
than this encoder's other speed knobs, and it is spending the very win the feature
exists to deliver. So the gate ships **off by default** (`tune_rd_skip_fast_t:
None`), available as a documented speed dial for anyone who wants that trade.

### 6.6 Why the pricing is so bad — propagation, not decisions

At T=40 only **0.15%** of decisions change, yet BD-SSIM costs **0.49%**. That
disproportion is the real finding. Holding T=80 and varying only the GOP length:

| GOP | P-frames | BD-SSIM cost |
|---:|---:|---:|
| 5 | 80% | +0.21 |
| 15 | 93% | +0.98 |
| 30 | 97% | **+2.33** |

If the cost were per-decision it would track the P-frame fraction — a **1.21×**
ratio from gop5 to gop30. It is **11.2×**, scaling with chain length instead. Each
wrongly-taken skip contaminates the reference for the rest of the GOP, so its
error is paid again by every frame that predicts from it.

**Transferable:** an approximate decision gate inside a long-GOP P-chain carries a
propagation multiplier that per-decision accounting completely misses. A gate that
looks nearly free at 0.15% of decisions is not — always price it by BD-rate at the
deployment GOP length, never by how often it fires. (This is also why the same
gate is harmless in an all-intra configuration and ruinous here.)

### 6.7 What the prize actually was

Of the original waste, the byte-identical work took the decision from 0.73× to
~0.89×/0.96× — the double encode, the state round trip, and the snapshot's ten
allocations were all real and all recovered at zero quality cost. The residue
(55–66% of candidates encoding then discarding) is **not recoverable without
changing decisions**, and changing decisions is mispriced by an order of magnitude
because of propagation. The prize was mostly collected; what is left is measured,
exposed as a knob, and correctly declined.

## 7. Gates

* `crates/rusty_h264-common/src/bit_writer.rs` — `append` splice equivalence at
  every source/destination bit misalignment.
* `crates/rusty_h264-encoder/tests/rd_skip_conformance.rs` — streams decode cleanly
  at 4 QPs × CAVLC/CABAC × both regimes; the gate demonstrably fires on
  high-free-skip content and is **byte-identical to the default path** when it
  blocks; no reconstruction drift over a 16-frame GOP.
* BD-rate over 4 QPs on **PSNR and SSIM**, per clip. The corpus ships only when the
  worst clip is ≤ ~0 — never on a positive mean.

## 8. Transferable lesson

*A metric split IS the diagnosis.* Every losing clip here improved on PSNR while
regressing on SSIM, which named the mechanism (an SSD-based decision trading
structure for MSE) before any parameter was swept. Had this been gated on PSNR
alone it would have shipped as a win on all six clips while making four of them
visibly worse.

And the second one: *when a threshold can only fix the loser by destroying the
winners, the signal has lost its separating power* — on Balanced, pushing 60→90
fixed in_to_tree but took akiyo from −10.1% to −1.0%. That is not a tuning problem
to grind on; it is the measurement telling you the feature is redundant with
something already in the pipeline.

---

## 9. Audit — the same class of approximation was already shipping

§6.6's propagation multiplier is a property of approximate decisions in a P-chain,
not of this feature, so it prompted an audit: what else decides a skip
approximately inside the chain? One answer, in the **default Quality preset**:
the greedy P_Skip (openh264 `PredictSadSkip`), which takes a skip when its luma
SAD is under a threshold derived from skip neighbours' median SAD — a
SAD-thresholded skip, never priced against the coded alternative.

### It does NOT have the propagation problem

Its source comment claims "no inter-chain drift", and that holds up. Holding the
feature fixed and varying only GOP length (foreman, Quality):

| GOP | BD-SSIM cost | |
|---:|---:|---|
| 5 | +0.64 | |
| 15 | +1.17 | |
| 30 | +1.23 | saturates |

gop5 → gop30 is **1.93×** against a 1.21× P-fraction baseline, and it *saturates*
between 15 and 30 — versus **11.2×** and still climbing for the T-gate in §6.6. The
self-calibration is what does it: the threshold is what neighbours actually
achieved, so the error cannot run away. A well-designed approximation.

### But it was a live, unpriced regression

| clip | shipped (ungated) | dispatched |
|---|---:|---:|
| akiyo | −0.585 | −0.236 |
| FourPeople | −0.324 | −0.121 |
| **foreman** | **+1.233** | **0.000** |
| in_to_tree | +0.213 | +0.034 |
| stockholm | +0.052 | −0.076 |
| mobile | +0.014 | 0.000 |

**+1.23% BD-SSIM on foreman, in the default Quality preset.** The signature is the
one from §2 — PSNR improves while SSIM regresses on moving/detailed content —
because a SAD-thresholded skip, like an SSD-thresholded one, trades structure for
absolute error.

### The fix was already built

The free-skip rates that separate RD skip separate this too, with the same wide
gap: winners akiyo 72.5% / FourPeople 58.7%, losers foreman 6.4% / mobile 1.0% /
in_to_tree 1.0% / stockholm 0.3%. So the greedy skip now runs behind the **same
online within-frame dispatch** (`tune_greedy_skip_min_free`, default 85 — higher
than Fast's 60 because Quality has sub-pel, exactly the scale shift of §5), added
to both the CAVLC and CABAC paths.

The regression goes to 0.00, stockholm flips to a win, nothing regresses, and about
half the akiyo win is the price. That trade is the right one: the bar is monotone
non-regression per content, not a positive mean.

### Transferable

**A propagation finding is a reason to audit, not just to decline.** The gate in
§6.5 was declined; the audit it motivated found a real regression that had been
shipping in a default preset. And note the audit's first result was *negative* —
the greedy skip passed the propagation test — yet running the corpus breadth-first
anyway is what surfaced the actual defect. Test the hypothesis you formed, then
look at the whole corpus regardless of whether it confirms.

