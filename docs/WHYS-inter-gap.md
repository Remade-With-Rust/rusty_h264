# WHYS — why is our encoder ~1.67× larger than x264 at equal speed?

Six-whys descent on the inter-coding gap. One entry per level; siblings are
separate causes, not alternatives. Refuted hypotheses stay in the log with their
number so they are not re-litigated.

**Status:** D6 opened, measurements deferred — a clean corpus run is in flight and
timing-sensitive work would re-contaminate it (see D6c).

---

## D1 — is the gap real, at matched settings?

- **ASKED:** do we actually compress worse than x264, or are we at a different
  operating point?
- **MEASURED:** full 20-clip corpus, both sides at the **same fixed QP** and same
  keyint. At equal encode speed (x264's preset frontier interpolated to our
  Mpx/s): ours/balanced **1.67× the size at +0.57 dB PSNR**; ours/fast 2.36× at
  +0.18 dB; ours/quality 1.63× at +0.59 dB.
- **ANSWER:** a gap exists, but it is NOT cleanly measured — we sit at a
  higher-rate, higher-quality point, so neither the size ratio nor the dB delta
  is the gap on its own.
- **CONFIDENCE:** medium. Single-QP comparison cannot yield BD-rate; this is the
  single-operating-point trap and the number is confounded by construction.
- **SPAWNED:** D6a (are the two sides configured comparably?), D1a (re-measure as
  BD-rate at matched quality, not one QP).
- **STATUS:** open — D1a not yet run.

## D2 — which stage owns it?

- **ASKED:** where does the encode cost sit, and does the expensive stage explain
  the size gap?
- **MEASURED:** stage profile over the corpus. `enc-me` **62.4%**, enc-inter-code
  10.2%, enc-cavlc-emit 7.9%, mgmt/other 6.2% (named), enc-skip-check 4.0%,
  deblock 3.3%.
- **ANSWER:** ME dominates *time*. CAVLC is at parity with x264 in absolute ms
  (3244 vs 3346). Deblock is 2.4× but only 3.3% of encode.
- **CONFIDENCE:** high — residue decomposed and named.
- **NOTE:** x264's own stage dump sums to **109%** (`mb-encode` nests inside
  `mb-analyse`), so only like-for-like stages are comparable. Do not difference
  its percentages against ours.
- **STATUS:** closed.

## D3 — is the ME cost buying us anything? *(the trap level)*

- **ASKED:** ME is 62% of encode and runs at x264-`medium`'s cost per macroblock
  (7612 vs 7149 ns) while compressing worse than x264-`veryfast` (1353 ns). Is
  the search failing?
- **MEASURED:** exhaustive oracle, ±24 full-pel × every quarter-pel offset, same
  cost function. Our vector lands within **0.4–4.5%** of optimal on ~19
  evaluations/search. **in_to_tree — our worst compression clip at 4.47× — has
  the SMALLEST ME gap (0.42%).**
- **ANSWER:** **REFUTED** — the search is not the cause. If it were, the worst
  clip would show the worst search; it shows the best.
- **CONFIDENCE:** high.
- **STATUS:** closed (refuted).

## D4 — is the ME COST FUNCTION mis-ranking what the search finds?

- **ASKED:** the D3 oracle is optimal *with respect to our own cost function*, so
  it proves the search finds its target, not that the target is right.
- **MEASURED:** recovered x264's motion field by parsing its stream with our own
  decoder (`RFF_MV_DUMP=1`), then scored both fields **reference-neutrally**
  (same original previous frame) restricted to macroblocks that are a single
  16×16 partition on ref 0 in BOTH encoders: x264's vectors predict **1.96%
  (foreman) / 3.94% (mobile) / 4.31% (akiyo) better**.
- **ANSWER:** **REFUTED as the primary cause** — real but far too small for a
  1.67× gap. Motion estimation is exonerated on both counts.
- **CONFIDENCE:** high — but see D6b, three designs were confounded first.
- **SPAWNED:** D5a (partition/mode decision), D5b (inter RDOQ), D5c (rate
  allocation).
- **STATUS:** closed (refuted).

## D5a — partition / mode decision

- **ASKED:** we lack sub-8×8 entirely; x264 `medium` runs `p8x8` partitions and
  `subme 7`. How much of the gap is partition shape?
- **MEASURED:** *nothing yet.*
- **STATUS:** OPEN — prime candidate.

## D5b — inter RDOQ (trellis)

- **ASKED:** x264 `medium` defaults to **`--trellis 1`**; superfast/veryfast set
  `--trellis 0` and slow+ set `--trellis 2`. We have trellis only for all-intra
  CABAC — **the inter path has no RDOQ at all**.
- **WHY IT FITS:** trellis drops coefficients whose bits exceed their distortion
  value, producing exactly the signature we observe — **x264 smaller AND lower
  PSNR than us at the same QP**, while we code everything the quantizer emits.
- **MEASURED:** *nothing yet.* Planned: encode the corpus with x264
  `--trellis 0` and see how much of the size gap closes. Deterministic, so the
  result is load-independent — but it costs CPU, hence deferred (D6c).
- **STATUS:** OPEN — strongest hypothesis.

## D5c — rate allocation

- **ASKED:** at matched QP we produce higher PSNR *and* higher rate. That is the
  signature of an encoder that does not RD-optimise **what it spends bits on**.
- **MEASURED:** *nothing yet.* Overlaps D5b.
- **STATUS:** open.

---

## D6a — is the reference configured comparably? *(defaults are configuration)*

- **ASKED:** what does x264 turn on by default at `--preset medium --qp N` that we
  do not have, and does any of it change the objective it optimises?
- **MEASURED:** `x264 --fullhelp`. Defaults: **`--psy-rd 1.0:0.0`**,
  `--aq-mode 1`, `--weightp 2`. Preset ladder confirms `medium` = trellis 1
  (superfast/veryfast are the ones that set `--trellis 0`). x264 ships
  **`--tune psnr` = `--aq-mode 0 --no-psy`** and **`--tune ssim` = `--aq-mode 2
  --no-psy`** precisely for metric-based evaluation.
- **ANSWER:** **we score x264 on PSNR and SSIM while it runs psy-rd, which x264's
  own documentation says to disable for those metrics.** psy-rd deliberately
  sacrifices PSNR/SSIM for subjective quality.
- **DIRECTION — this does NOT flatter x264, it flatters US.** Turning psy off
  should *raise* x264's PSNR/SSIM, so **the true gap is likely LARGER than
  1.67×**, not smaller.
- **CONFIDENCE:** high on the defaults (read from `--fullhelp`); the magnitude is
  unmeasured.
- **NOTE:** `--profile baseline` forces `weightp 0`, so the baseline arm is clean
  of weighted prediction. `--aq-mode` requires a rate-control mode that can vary
  QP, so it is likely inert under `--qp` — **verify, do not assume.**
- **STATUS:** open — measurement deferred.

## D6b — were the D4 comparisons sound? *(three were not)*

- **ASKED:** did the motion-field comparison measure what it claimed?
- **MEASURED:** three designs each produced a confident, false number before one
  worked: transplanting a **single** x264 vector read **+106% bits** (`mvd` codes
  differentially against the *neighbours'* vectors → wrong predictor);
  transplanting the **whole field** read **+118% size** (x264's vectors point into
  x264's *reconstruction*, and forcing them degrades our reference, compounding
  over the GOP); comparing reference-neutrally but **unfiltered** read x264
  **300–600% worse** (multi-ref and sub-partition macroblocks scored against the
  wrong frame / as whole macroblocks).
- **ANSWER:** a quantity chosen inside a context — reference frame, neighbour
  field, partition shape — means nothing outside it. Every artifact was caught by
  **physical implausibility**, not inspection.
- **CONFIDENCE:** high. Banked into `codec-analyzer` Learnings.
- **STATUS:** closed.

## D6c — is the measurement environment sound?

- **ASKED:** can the corpus numbers be trusted?
- **MEASURED:** a corpus run showed the **Fast preset byte-identical** (0.0% rate
  change on every clip — both new ME knobs are provable no-ops there) while its
  **speed swung −66% to +4%**. Identical bits cannot encode 3× slower. Process
  sampling found a foreign `speedbench` at 3.1 cores plus a cycling `vp9enc`, and
  an orphaned `analyzer` of mine that had survived a task stop.
- **ANSWER:** that run's timing was contaminated; its noise floor exceeded every
  effect we have been landing. Compression columns (deterministic) were unaffected.
- **ACTIONS:** orphan killed; foreign workload has since finished; box now at 1.73
  of 24 cores; clean run relaunched.
- **RULE ADOPTED:** **do not run timing-sensitive experiments while a corpus run
  is in flight** — this is why D5b/D6a measurement is deferred rather than done
  now.
- **CONFIDENCE:** high.
- **STATUS:** closed.

---

## Planned next measurements (run AFTER the corpus run lands)

1. **D6a magnitude** — x264 at `--tune psnr` and `--tune ssim` vs default, same
   QP/clips. Quantifies how much our headline was flattered.
2. **D5b ceiling** — x264 `--trellis 0` vs `--trellis 1`. If most of the size gap
   closes, inter RDOQ is the lever and D5b is promoted to the rebuild.
3. **D1a** — replace the single-QP comparison with a real 4-QP BD-rate against
   x264 so the gap is stated in a metric that cannot be gamed by operating point.

Ceiling before cost, per the rebuild rule: measure what x264 loses without
trellis (the prize) before building inter RDOQ (the tax).
