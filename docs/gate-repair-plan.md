# Gate repair plan — function by function, from the abstention census

**Status:** plan, 2026-08-08. Built on `bench/baselines/gate_census_grain_2026-08-08.txt`.

## Read this first: the gates are not broken

The first census run concluded "four of five veto gates never fire." **That was wrong,
and the fault was the corpus, not the code** — it omitted the grain and screen classes
that three of those gates key on. With the class restored:

| gate | on its class | off its class |
|---|---|---|
| `aq_grain_veto` | **96.7%** (grain_akiyo) | 0.0% |
| `sub8_grain` | **100.0%** of 11,484 | 0.0% |
| `mbtree_grain` | **100.0%** | 0.0% |
| `intra_rd_flip` | **19.1%** of 11,484 | never run (grain-gated by design) |

That is textbook content-adaptive dispatch: fires on the class it was built for, silent
off it. `sub8_split`/`sub8_rd_revert` reading NEVER RUN on grain is the veto **chain**
working — `sub8_grain` suppressed 100% of them upstream.

The decision gates route too: `sub8_split` 26.7→59.8%, `sub8_rd_revert` 33.8→81.4%,
`shape_rd_flip` 0→58.3%, `mbtree_spread` 0→100%.

**So this is a repair plan for four specific defects, not a rewrite.** Ranked by
measured size, not by how interesting they are.

---

## R1 — CLOSED on quality grounds 2026-08-08. Redirected to R1a/R1b.

**Functions:** `best_part`, the RD revert site in `mb16.rs`, and a new instrument
`sub8_regret` (env `RFF_SUB8_REGRET`, verified non-perturbing — byte-identical encode).

### What the pre-check measured

The census gave a disagreement RATE (33.8–81.4%). A rate cannot justify a refit, so the
pre-check recorded the signed MAGNITUDE in the encoder's own currency,
`dj = (j_split − j_flat) / λ`, over 6 content classes × 2 QPs × 2 arms:

| arm | MBs | median regret | p90 | p99 | max |
|---|---|---|---|---|---|
| `--bframes 2` (x264-comparable) | 2,762 | **17.53 λ** | 42.6 | 69.3 | 98.1 |
| `--bframes 0` (census arm) | 2,196 | 13.41 λ | 31.4 | 49.3 | 83.1 |

Threshold was fixed at ~1 λ **before** looking. At 17.5 λ the proxy **is** genuinely
miscalibrated: the magnitude test PASSES.

### Why R1 still closes

Two further tests, both of which it FAILS:

1. **Amdahl.** The RD revert path runs on **1.27% of macroblocks** (harbour_4cif: 2,407
   trials against 190,080 MBs encoded). Nothing behind a 1.27% door is a headline.
2. **Impact — and this is decisive.** The miscalibration costs *nothing* in the shipped
   configuration:
   * `sub8x8` resolves to `cfg.preset == Preset::Quality`, so **`fast` NEVER runs this
     path** and eats none of the error.
   * `quality` runs the RD pass, which **corrects** it. The 17.5 λ regret is therefore
     the VALUE THE RD CORRECTOR DELIVERS, not a loss we are taking.

**So the plan's original claim — that this "plausibly explains the `fast` arm's +0.99%
BD with 8/9 clips regressed" — was WRONG.** `fast` does not execute this code. The
pre-check existed to catch exactly that, and it did.

### R1a — pre-search skip gate (a COST lever, ceiling 1.27% of MBs)

The useful form is not refitting SATD; it is predicting "this split will be reverted"
*before* paying the 8 sub-searches. `sub8_harvest` already exists for this and already
records pre-search features with the RD label — and already **refuted `j8_lme` and
`mbvar`**. `mvdiv` (motion divergence within the quad; a motion BOUNDARY is the thing
splitting actually exploits) is the live candidate.

**Bound it before building.** At 1.27% of macroblocks the ceiling is small; compute what
skipping all 8 sub-searches on the reverted MBs would save as a share of whole encode,
and prune if it is under the noise floor.

### R1b — shape-RD: escape hatch LANDED; the `fast` hypothesis REFUTED

**Landed:** shape-RD had **no escape hatch**. The condition was
`shape_rd_on() || cfg.tune_shape_rd` — an OR, so `RFF_SHAPE_RD=0` could not turn it off.
It was the one shipped gate that could not be neutralised, which broke gatecheck's
Tier-0 contract and made it untestable. `shape_rd_on()` now returns `Option<bool>` and
the env is an override in BOTH directions. Verified: unset is byte-identical to before
(quality 287454), and `RFF_SHAPE_RD=0` now genuinely changes the encode
(quality 287454 -> 291551).

**Hypothesis refuted by the hatch it unblocked.** shape-RD is **INERT on `fast`**:
ON and OFF are byte-identical (335761 both). `shape_cands.push` happens inside the
sub-8×8 block, which is quality-only, so `shape_cands.len() > 1` is never satisfied on
`fast`. shape-RD is therefore NOT the cause of the `fast` regression.

### R1c — OPEN: all four gates are inert on `fast`, yet `fast`'s bytes changed

Established by measurement:

* `fast` output changed across the campaign: **341471 -> 335761** bytes (FourPeople, qp27,
  `--bframes 2`), and it changed in **`a6e45f7` alone** (nothing since).
* **None of the four shipped gates can explain it.** sub-8×8 and its two RD gates are
  `preset == Quality` only; intra-RD is grain-gated; shape-RD is inert on `fast` (above);
  mb-tree cannot run with B-frames. `RFF_SHAPE_RD=0`, `RFF_SUB8X8=0` and
  `RFF_INTRA_RD_ALL=1` each leave `fast` at 335761 — none restores the pre-campaign bytes.
* `a6e45f7` changed **no pre-existing config default** (the diff is all additions).
* Two candidate code changes were checked and **cleared**: `me_lambda_scale`'s default
  path is unchanged (`_ => return cfg.cabac_lambda_scale` in both), and the mode-3
  `(lme*4.0)` penalty was extended, not removed — it evaluates identically when
  `pick_subs == [0u8; 4]`, which is always the case on `fast`.

**So an unconditional change inside `encode_slice_data_cabac_p` (13 hunks) alters the
`fast` partition decision, and it is not gated by anything.** That is the actual source
of the `fast` arm's +0.99% BD with 8/9 clips regressed — a behaviour change that shipped
inside a campaign whose stated unit was content-adaptive dispatch, with no gate and no
escape hatch.

**Next step:** bisect *within* `a6e45f7` by reverting hunks of
`encode_slice_data_cabac_p` until `fast` returns to 341471. The remaining suspects are
the `inter = if c_intra < best_c` change and the `plan_inter_mb` signature/behaviour
change. Cheap and deterministic — byte comparison, no timing.

## R2 — FourPeople-class content: the features lose and no signal sees it

**Functions:** `signals::grain_signature`, `shape_rd_tex_max`, and the signal set in
`crates/rusty_h264-encoder/src/signals.rs`

**What the census says:** on FourPeople (720p low-motion) **every feature is active and
no veto fires** — `sub8_split` 31.4%, `sub8_rd_revert` 69.9%, `shape_rd_flip` 37.8%, no
grain veto. And FourPeople is the campaign's **worst regression: +8.45% (fast) /
+5.67% (quality)**.

**So this is a missing axis, not a broken gate.** Every existing signal says "normal
content, run everything"; the BD says the features lose. The gates cannot abstain
because nothing in the set distinguishes this class.

**Do NOT** add a threshold that happens to separate FourPeople. That is the
`med_var`/`maxtex_plaid` mistake the campaign already made and labelled — a fitted
bound with no mechanism, refuted by one synthesised clip. Instead:

1. **Find the mechanism first.** Per-MB harvest on FourPeople: which decisions do the
   features change, and where does the RD-vs-actual delta go negative? 720p low-motion
   talking-head content is *low residual energy, high temporal correlation* — a
   plausible mechanism is that sub-8×8 and shape-RD buy partition precision that the
   residual cannot pay for, so the mvd/mb_type overhead dominates.
2. **Only then** ask whether an existing signal already separates it (the campaign's
   own lesson: check for a mis-calibrated existing axis before inventing a third).
3. Gate on the mechanism, and **hold out a synthesised clip on the other side of the
   threshold** before believing it (`holdout-both-sides-of-a-threshold`).

**Verify:** the win-signature is a **byte-identical non-beneficiary** — FourPeople must
come out bit-exact with the features nominally on, and the other five classes unchanged.

---

## R3 — mb-tree is dead code in every x264-comparable configuration

**Function:** `lookahead_active` (`crates/rusty_h264-encoder/src/lib.rs:456`)

```rust
self.cfg.mbtree && self.cfg.bframes == 0 && self.cfg.bitrate == 0
```

**What was measured:** encoding FourPeople with `--mbtree` on vs off is **byte-identical
(+0.00% BD)** — because the BD ladder uses `--bframes 2` and mb-tree cannot run with
B-frames at all. `mbtree: true` is the library default (`config.rs:523`).

**So:** a default-on feature that is mutually exclusive with another default-on feature,
and the exclusion is silent. Every x264-comparable arm enables B-frames, so mb-tree has
contributed nothing to any competitive measurement — including the three GOP-level
gates built to route it (`mbtree_grain`, `mbtree_backoff`, `mbtree_spread`).

**Do:** decide explicitly, and make the decision visible.
* If mb-tree stays: it needs a B-frame-compatible lookahead, which is real work — or
* If it does not: default it OFF, and have `Encoder::new` **warn** when `mbtree && bframes > 0`
  rather than silently ignoring it.

Either way the silent-exclusion has to go. A feature that cannot execute in the shipped
configuration should not read as enabled.

**Verify:** a test asserting that `mbtree=true, bframes=2` either warns or is rejected —
never silently inert.

---

## R4 — CAVLC has no decoder routing at all

**Function:** `edc_dispatch` (`crates/rusty_h264-decoder/src/mb16.rs:6414`)

```rust
cabac && bits_per_mb > BITS_MIN && mb_w * mb_h <= MAX_MBS
```

**Why the `cabac` clause is there:** without it the rule scored **net −52.30, worst class
−40.85** once CAVLC entered the corpus, because CAVLC's bits/MB is inflated by a weaker
entropy coder rather than by more pixel work. Excluding CAVLC was *locally correct*.

**But the consequence is structural:** CAVLC receives no threading dispatch, and it is
the tier that read **2.230×** vs ffmpeg on 2026-08-08 against a 0.8.0 band of 1.98–2.11
— the only tier outside its band. (The ASM port was exonerated at 1.031 within a 22.8%
floor, so the port does not own it.)

**Do:** the clause is not the bug — the *signal* is. bits/MB conflates entropy
efficiency with pixel work. Replace it with a signal that measures pixel work directly
and is entropy-coder-agnostic: **coded-block density** or non-zero coefficient count per
MB, both already tracked (`nnz`). Then CAVLC can be routed on its merits instead of
excluded by proxy.

**Verify:** re-fit on a corpus containing both entropy coders, and require the worst
class ≥ 0 on **both** — the failure that produced the `cabac` clause must not return.

---

## R5 — process: the census must match the quality configuration

**Function:** `bench/examples/gatecheck.rs`

Two defects found by using it:

1. **Config mismatch.** The census ran at `bframes=0` (library default); the BD ladder
   used `--bframes 2`. They described different programs, and that is exactly how the
   mb-tree false lead in R3 was generated. gatecheck must either take the arm config as
   input or record it in the baseline so a mismatch is visible.
2. **The corpus is load-bearing and was missing.** `grain_akiyo` and `screen_text` are
   synthesised by `video-tests/synth_clips.sh` and were absent from the tree, so
   overriding `RUSTY_GATECHECK_CLIPS` silently dropped the grain class — which produced
   a confidently wrong conclusion. gatecheck should **fail loudly** when a
   gate-relevant class is missing from the clip set rather than reporting 0%.

**Do:** add a class tag per clip and per gate; refuse to report a gate whose trigger
class is not represented. Record the arm config in the baseline. Then wire
`gatecheck --check` into CI (still-open P4 item).

**Verify:** deleting the grain clip must make the run *fail*, not produce zeros.

---

## Ordering

R5 first — it is cheap and everything else's evidence depends on it. Then R1 (largest
measured effect, and its pre-check may close it for free). Then R2 (the only outstanding
quality defect). R3 and R4 are decisions more than implementations and can go in
parallel.

## What this plan will not do

Add a threshold that separates FourPeople without a mechanism. The campaign has already
paid for that lesson twice — `med_var` (refuted by a synthesised clip at 2583) and
shape-RD's own guard, which its comment disowns. A fitted bound that survives
cross-validation still breaks on the axis the corpus did not vary.
