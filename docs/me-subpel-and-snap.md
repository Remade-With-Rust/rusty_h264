# Motion estimation: the sub-pel refine was under-converged

**Status:** both bricks default ON (`tune_me_subpel_iter`, `tune_me_snap`).
**Result:** BD-SSIM −0.16% to −2.39% on every corpus clip, at ~parity speed
(0.95–1.02×). Fast preset byte-identical (it has no sub-pel refine, so both are
no-ops there).

---

## 1. How we got here — the search was exonerated first

`enc-me` is 62% of encode and runs at x264-`medium`'s cost per macroblock while
compressing worse than x264-`veryfast`. The natural assumption is that the motion
search is failing.

An exhaustive oracle (`RFF_ME_ORACLE=1`: ±24 full-pel × every quarter-pel offset,
same cost function) says otherwise — our chosen vector is within **0.4–4.5%** of
optimal on ~19 evaluations/search, and **in_to_tree, our worst compression clip,
has the smallest gap (0.42%)**. The search pattern is not the problem.

The oracle is optimal *with respect to our own cost function*, so it proves the
search finds its target, not that the target is right. What it does definitively
exclude is "search harder" — and that is what redirected the work to the two
bricks below.

## 2. The compression brick — iterate the sub-pel refine

The sub-pel refinement was a **single 8-point pass** at half-pel then quarter-pel.
Walking each step to convergence instead is worth, on its own:

| clip | BD-PSNR | BD-SSIM |
|---|---:|---:|
| foreman | −2.540 | **−2.389** |
| mobile | −0.372 | **−0.414** |

It costs ~22% speed on foreman (0.784×), which brick 3 pays back.

That a single extra convergence loop is worth 2.4% BD-rate says the refinement was
simply stopping early — the cheapest kind of compression win, and one the ME
oracle could not see because it applied *our* sub-pel pass to both arms. A probe
that holds a stage fixed across both arms is blind to that stage.

## 3. The speed brick — snap the diamond centre to integer-pel

The diamond steps by whole pels, but its seed is the neighbour MV predictor, which
is **fractional**. A fractional centre makes every candidate in the entire search
fractional, forcing all of them through `mc_luma`'s 6-tap filter. Measured
(`RFF_MC_COUNT=1`): **84–90% of SATD evaluations interpolated.** Snapping the
centre drops that to 54–57% and buys **1.21–1.30×**.

x264 does not have this problem twice over: it precomputes half-pel planes once
per frame (its `hpel-filter` stage, 0.6% of encode) and reads them.

The identical defect had already been found and fixed in the **stall-rescue grid**
earlier (2.21× → 1.19× on zoom) — the main diamond was simply never given the same
treatment. *When a fix is found for one search path, check every other search path
in the same function.*

Safety: the pre-snap seed is retained and re-compared after refinement, so the
returned vector can never be worse than the seed the search started from.

## 4. Why they ship together

Measured separately (`RUSTY_BDRATE_PARAM=mearm`, bitmask 1=snap, 2=iterate, 3=both):

| arm | foreman BD-SSIM | foreman speed |
|---|---:|---:|
| snap only | **+0.272** (loss) | 1.30× |
| iterate only | −2.389 | 0.78× |
| **both** | **−2.317** | **0.95×** |

Snapping alone is a small compression *loss* traded for speed; iterating alone is
a compression win that costs speed. Together they are a compression win at ~parity
speed — each pays for the other's weakness. Shipping either alone would have been
the worse decision, and the coupled first implementation nearly hid this: the two
were built as one change and had to be **decoupled to attribute the win**, which
is when the snap turned out to be contributing none of it.

## 5. Corpus (Balanced, both on, anchor = both off)

| clip | BD-PSNR | BD-SSIM |
|---|---:|---:|
| foreman | −3.061 | **−2.317** |
| akiyo | −0.601 | −0.537 |
| FourPeople | −0.594 | −0.558 |
| stockholm | −0.365 | −0.245 |
| mobile | −0.278 | −0.317 |
| in_to_tree | −0.183 | −0.159 |

Every clip wins on both metrics — no dispatch needed.

## 6. Gates

Fast preset byte-identical (no sub-pel refine ⇒ both knobs inert), 5/5 skip
conformance, 18/18 workspace binaries. Probes left behind `RFF_ME_ORACLE` and
`RFF_MC_COUNT`, both cached-env gated and verified inert when off.

---

## 7. Are x264's vectors actually better? (−2% to −4%, which settles it)

The ME oracle showed our search finds the optimum *of our own cost function*. That
left the cost function itself unexamined. To test it, our decoder was given an MV
capture (`RFF_MV_DUMP=1`) so it can recover the motion field from **any** conformant
stream, including x264's — no external MV-export tooling.

### Three designs were built and discarded as confounded

1. **Transplant a single x264 vector into our encoder and price it.** Invalid:
   `mvd` is coded *differentially against the neighbours' vectors*, so a lone
   foreign vector prices against the wrong predictor. It read **+106% bits** — pure
   artifact.
2. **Transplant x264's whole field.** Invalid: x264's vectors point into *x264's
   reconstruction*. Against ours they mispredict, and forcing them degrades our
   reference further, compounding over the GOP. It read **+118% size** — also an
   artifact.
3. **Reference-neutral, but unfiltered.** Invalid: x264 `medium` uses multiple
   reference frames and sub-partitions, so scoring a `ref_idx>0` vector against
   frame N−1, or a sub-partition's vector as if it covered the whole macroblock,
   is meaningless. It read x264 **300–600% worse** — artifact again.

The lesson underneath all three: **a motion vector is only meaningful relative to
the reference frame and the neighbour field it was chosen in.** Any comparison that
moves a vector out of that context measures the context, not the vector.

### The valid comparison

Both fields evaluated against the same **original** previous frame (reference-
neutral, no differential-rate coupling), restricted to macroblocks that are a
single 16×16 partition on reference 0 in **both** encoders:

| clip | MVs differing | our SSD | x264 SSD | x264's edge |
|---|---:|---:|---:|---:|
| foreman | 61.6% | 2457.3 | 2409.2 | **−1.96%** |
| mobile | 26.9% | 11600.2 | 11142.9 | **−3.94%** |
| akiyo | 9.2% | 275.2 | 263.4 | **−4.31%** |

x264's vectors are **2–4% better predictors** — real, but nowhere near enough to
explain a 1.67× size gap. Note also that on foreman 61.6% of vectors differ while
the prediction difference is only 2%: most disagreements are between near-equally
good vectors.

### Conclusion for the inter gap

Motion estimation is **exonerated on both counts** — the search finds its target
(§1) and the target is within 2–4% of x264's (§7). Neither searching harder nor
retuning the ME cost function can close a 1.67× gap. The remaining candidates are
partition/mode decision (sub-8×8 is still missing), residual coding (there is no
inter RDOQ — trellis is all-intra CABAC only), and rate allocation: at matched QP
we sit at higher PSNR *and* higher rate than x264, which is the signature of an
encoder that does not RD-optimise what it spends bits on.
