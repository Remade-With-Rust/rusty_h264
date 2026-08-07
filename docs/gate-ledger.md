# The Gate Ledger

Every gate in the codebase, in the canonical form of great-gate.md §4:

```
GATE := (unit, signal, threshold-form, arms, fallback, ledger-entry)
```

One section per gate. Each entry records the per-class truth table it was
fitted/verified against, the split discipline (fitted on train, judged ONCE on
holdout), each branch's precision and bounded downside, and provisional
branches labeled provisional. Regeneration: the P1 harvest
(`RFF_SIGNALS_CSV`) + the `bdrate` per-clip runs + `gate_optimizer`
(bench/examples/). Entries are append-only; a re-fit gets a new dated entry,
never an edit.

Method line for all P2 entries unless stated otherwise: 24-clip corpus
(20 natural + 4 synthesized gap-class, `video-tests/synth_clips.sh`),
60 frames, QPs 22/27/32/37, gop 30, CLI defaults (CABAC, fast preset,
mb-tree on), BD via `bench/examples/bdrate.rs` (in-process Y-PSNR + SSIM→dB,
cubic BD). Deterministic quantities — one run is the verdict.

---

<!-- entries append below -->

## THE HARNESS — `bench/examples/gatecheck.rs` (Great Gate P4, built 2026-08-06)

Every entry below is a CLAIM that nothing re-checks by itself, and claims decay
fastest when something UPSTREAM changes. Not hypothetical: the AQ grain fix
silently flipped mb-tree's grain verdict (-0.63 -> +4.41 BD-SSIM) and only a
re-run found it.

| tier | what | cost | catches |
|---|---|---|---|
| 0 | escape-hatch hashes (every gate's neutral end reproduces its bytes) | seconds | escape-hatch rot, accidental default flips |
| 1 | **fire-rate census** — (fired, seen) per gate per clip | minutes | *anything upstream moving* — the CANARY |
| 2 | **work counts** — `best_part` / `mb_plan` / `mb_coded` | minutes | the deterministic COST axis |
| 3 | BD (`bdrate`) + pinned ms (`pinvs.ps1`) | hours | the verdict itself; run when 1 or 2 moves |

Counter-first: tiers 0-2 are DETERMINISTIC — one run is the verdict, no
pinning, no z-score, exact comparison. Tier 3 lives OUTSIDE the binary because
deterministic and timed quantities must not share a loop
(`codec-measurement` §13). A moved counter is a CANARY, not a verdict: it says
"re-earn this gate's Tier-3 claim", not "better" or "worse".

Run: `gatecheck --baseline <file>` to record, `--check <file>` to compare
(non-zero exit on drift). Clips via `RUSTY_GATECHECK_CLIPS`/`_DIR`; keep the
set SMALL and DISCRIMINATING — one per gate-relevant class.

**Two defects it found in itself on day one, both recorded because they are the
failure modes any successor will hit:**

1. **Silence recorded as data.** The first cut snapshotted the census ONCE,
   before the arm that exercises the sub-8x8 gates ran, and dutifully wrote
   `0/0` for three gates. Now captured PER ARM.
2. **A regression harness certifying stale code.** The first `--check` after
   the intra-RD grain gate landed printed *"PASS: 185 tracked quantities
   unchanged"* — because the CLI and bdrate had been rebuilt and `gatecheck`
   had not. A false PASS from the tool whose entire job is catching change is
   the worst possible failure. Fixed with `assert_fresh()`: the harness
   compares its own exe mtime against the newest `.rs` under `crates/` and
   EXITS 3 rather than report. `codec-measurement` §10 ("verify the binary is
   fresh") made automatic instead of remembered.



## aq-grain-veto — SHIPPED default-on 2026-08-06 (PROVISIONAL: one textured-grain exemplar)

```
GATE := (unit: frame, signal: median_var × grain_floor × mgain,
         threshold-form: fixed conjunction (per-frame),
         arms: aq_qp_map active / uniform-QP return,
         fallback: RFF_AQ_GRAIN=0 → pre-gate bytes exactly,
         entry: this)
```

**Premise.** AQ's "busy = maskable" fails on grain: noise is busy everywhere but
not maskable texture, and the lv_spread back-off cannot see it (grain reads
LOW-to-mid spread — 0.13 flat / 1.60 textured — while the back-off keys on HIGH
spread). Measured catastrophe: grain_akiyo **+29.45% BD-SSIM** (and +54% BD-PSNR),
the corpus's only AQ loss. Everything natural wins −1.5..−11.2%; screen content
is already byte-identical OFF via the spread ramp (its win signature held).

**The rule** (fire = veto AQ for this frame, uniform QP):

```
median_var < 200  AND  grain_floor > 5.0  AND  mgain < 0.1
```

"Unexplained temporal residual: not texture (var — protects mobile 1346+, city
259+ vs grain ≤ 134), not motion (mgain — protects bus .19+, football .12+),
→ noise." Fitted per-frame on the 24-clip harvest: fires 58/58 grain frames,
0 winner frames except stockholm 1/58; threshold-insensitive across
var<150..250 (the invariance tell). Judged per-frame, not on clip medians —
the median gap for the optimizer's preferred `dcfrac` clause did NOT survive
frame-level checking (overlap with mobile/shields); this conjunction did.

**Mechanism note (found by descent).** The damage is ~all PROPAGATION through
the GOP's I-frame: all-intra AQ on grain is nearly free (−1.6), but one AQ'd
IDR referenced by a GOP of unpredictable noise carries the loss. Hence the
IDR needs the veto most — an IDR has no coding reference, so the batch paths
hand the PREVIOUS SOURCE frame as the probe (`pending_aq_probe`, self-filled
by `encode_direct` on sequential paths; identical frame by construction, so
the documented streaming==batch byte-for-byte invariant holds).

**The bounded residual.** The stream's FIRST IDR fails open on every path
(pure streaming cannot see frame 1 at frame 0; keeping batch==streaming exact
was chosen over probing frames[1] in batch only). Measured: grain BD-SSIM
+10.85% at 2 GOPs (unprotected GOP = 50% of clip) → +5.65% at 4 GOPs (25%)
— ratio 1.92 ≈ the share ratio, so the residual is ∝ first-GOP share and
vanishes on deployment-length streams (<0.5% at 100+ GOPs). Veto fires
116/116 P frames on the 120-frame clip; no clause drift (var 115–134).

**Trial (4-QP BD, full 24-clip matrix, veto on vs the pre-veto binary):**
21/22 non-grain clips numerically IDENTICAL (output unchanged — the
non-beneficiary byte-identity signature); stockholm −9.50 → −9.04 (its one
fired frame); soccer −10.690 → −10.670 (one QP-dependent frame; P-frame
signals read the recon, so fire rates can shift ±1 frame across the ladder).
grain_akiyo +29.45 → +10.85 (→ 0 with length, above). Conformance: veto-fired
stream decodes pixel-exact in ffmpeg; full encoder suite green.

**Provisional because:** n=1 textured-grain exemplar (grain_flat is a no-op
either way); the var<200 clause bounds abstention — grain over MODERATE
texture (var ≥ 200) is out of reach and FAILS OPEN to current behaviour
(the safe direction: a miss = status quo, never a new regression). Re-fit
when real grain footage (Diana harvest) joins the corpus. Downside bound of a
misfire: forgoing that frame's AQ win (observed non-grain misfire rate:
2 frames / 1276).

## rdskip-preset-gate — RECLASSIFIED P3 (missing arm on the shipped path) — 2026-08-06

The planned fit ("enable RD P_Skip exactly where sub-pel is off") cannot run:
the 24-clip × 4-QP matrix with `tune_rd_skip=1` on the Fast preset read
**0.0000 on every clip and both metrics** — the inert-across-its-whole-grid
tell (§8.117). Cause: the RD-skip machinery (`rdskip_on`, the online free-skip
census, the spliced-bits path) exists ONLY in the CAVLC I/P driver
(`encode_slice_data`, mb16.rs ~4730–4979); the CABAC P driver has none, and
CABAC is now the default entropy coder. `tune_rd_skip` is a dead knob on the
shipped path — capability×population, not a threshold: **build the CABAC
RD-skip arm first (P3), then run this fit.** (Historical −10%-on-Fast numbers
were CAVLC-era.)

**P3 item 2 (2026-08-06): arm BUILT, fit run, verdict PRUNE-to-opt-in.**
Ported the THRESHOLD form into `encode_slice_data_cabac_p` (the CAVLC
trial-encode-and-splice does not transfer to an arithmetic coder; the fast
gate `SSD(skip) ≤ T·λ` is the λ-priced-distortion form the RD B_Skip gate
proved under CABAC — NOTE: B_Skip is census+threshold, not state-snapshot
trials as the P3 catalog first said). Gated on the existing greedy free-skip
census (min_free 60 Fast / 90 Quality). Liveness proven (akiyo −225 bytes at
T=48, `RFF_RDSKIP_T` env arm); fired stream decodes pixel-exact in ffmpeg;
off = byte-identical by guard.

**Fit (9 clips × T∈{24,48,96} × 4-QP, rdskipn param, FRESH bdrate — the first
run was all-zeros on a stale binary, caught by the liveness cross-check):**
best case akiyo −0.28 PSNR/−0.09 SSIM @T=48; but worst class ≤ 0 FAILS at
every T — in_to_tree +1.56 SSIM @48 (+3.33 @96), screen_text +2.92,
FourPeople +1.05, stockholm +0.26; mobile/grain held at 0.0000 by the census.
The CAVLC-era −10% prize is NOT in the threshold arm under the CABAC-default
stack (that win came from the full J-compare and/or a weaker baseline).
`tune_rd_skip` stays opt-in. Possible future brick: the full J-compare via
CABAC state-snapshot trials — ceiling now doubtful; measure before building.

## rdoq-inter — MEASURED WASH; arm built, kept opt-in, no default — 2026-08-06

The missing half-arm was built: the accel inter luma site now carries the
trellis fork (mirroring the I16 site; inter keeps DC, /6 deadzone), and
`cabac_rdoq_p` / `cabac_rdoq_b` (default 0.0 = byte-identical) wire it into
the CABAC P/B drivers. Sweep at the intra-calibrated strength 8, 7 clips
across classes, 4-QP BD:

- **P slices**: ±0.15% washes; stockholm +1.17 loss — the
  reference-structure-adaptive law confirmed (a P frame is a reference; the
  distortion-for-rate trade propagates unweighted).
- **B slices**: ±0.2% washes; foreman's BD-SSIM fit blew up (+183904% — the
  known cubic-fit pathology when the two curves nearly coincide, which is
  itself evidence the arms barely differ; its PSNR column read a sane −0.22).

Verdict: **no win to gate** — a texture dispatch here would route noise. The
knobs stay opt-in; revisit only with propagation-weighted λ for reference
frames. Matches the cabac-state prior ("RD-decision levers wash").

## sub8x8-split — SHIPPED default-on 2026-08-06 (P3 item 3): 10 WIN / 9 neutral / 0 LOSE; cost cut 5.59x -> 3.05x

```
GATE := (unit: frame, signal: FrameSignals::grain_signature(),
         threshold-form: fixed conjunction (shared, 3 consumers),
         arms: 8x8-only P_8x8 / 8x4+4x8+4x4 split search (RD-priced),
         fallback: tune_sub8x8_split=false (default) = byte-identical;
                   RFF_SUB8_GRAIN=0 disables the veto alone,
         entry: this)
```

**Read the RESOLVED section below first** — the gate this entry set out to fit
was not the answer; a mispriced search was. The narrative is kept in order
because the two dead candidates are the evidence that sent the third probe
somewhere better.

**The missing arm exists now.** Encoder previously emitted only `sub_mb_type 0`
(the `unreachable!` in `cb_sub_mb_type_p`); the decoder had parsed all types
since bring-up. Built: `cb_sub_mb_type_p` types 1-3 (exact inverse of
`parse_sub_mb_type_p_cabac`, ctx 21), the sub-partition formation in
`plan_inter_mb` mirroring `decode_p8x8` (plain `predict_mv` median per
sub-partition, COMMIT-before-next chaining, decode order), the mvd cache
layout (`p_sub_partition_layout`), InterPlan widened to 16 mvds + `sub_types`,
and the per-quad search in the CABAC quality path (each quad trials
8x8/8x4/4x8/4x4; REAL sub_mb_type bins priced at lme). Scope: CABAC path,
single-ref (ref_idx is per-quad syntax; best_part searches refs per part —
mixed sub-part refs are unrepresentable). CAVLC emission + multi-ref =
recorded follow-ups; CAVLC drivers generate no split candidates, so that
population is byte-identical by construction.

**Gates passed:** off = byte-identical (the legacy all-8x8 arm keeps its exact
single-truncation pricing — a per-quad `lme as i64` form truncates 4x and
picks different candidates; caught at review). Liveness (foreman quality
+365B different decisions). **Conformance 6/6: pixel-exact ffmpeg decode at
QP 4/27/48 x {foreman, mobile}**, first try. Suites green, both builds.

**Force-on BD (6 clips, quality preset, 4-QP): SIGN-FLIP — the dispatch
trigger.** bus −4.31/−2.95, foreman −3.31/−1.73, mobile −1.32/−2.67 WIN;
akiyo ~0; **tempete +1.69/+1.83, harbour +3.98/+4.63 LOSE** (fine texture:
splits fit noise the coding would repair, paying mvd bits). Class does NOT
separate (mobile wins, tempete/harbour lose — all busy-tex), so the gate needs
a fit: full-corpus quality ladder → gate_optimizer, likely per-frame/per-MB
grain (the per-quad decision already computes each arm's J — a J-margin gate
is the natural first candidate). 6 units is below the fit bar
(combination-space law).

**Gate-fit progress (2026-08-06, same day):** batch-1 corpus extension
sharpened the axis — screen_text **−11.5/−11.3** (sub-8×8's biggest win:
sharp synthetic edges), screen_ui −3.8, panc −1.9, qcif clips win;
**grain_flat +42.1/+15.8 catastrophic**, grain_akiyo +9.4/+13.6, football
+2.95, crew/city/mand +1.1..+1.3 join tempete/harbour as losers. Wins on
SHARP STRUCTURE, losses on STOCHASTIC residual. **J-margin hypothesis
REFUTED** (11-clip harvest, 1.06M quad decisions, margins normalized by λ):
losers' chosen-split margins are as large or LARGER than winners' (grain
median 9–10, football 30.1 vs foreman 6.0; only screen separates at 366) —
the search is SYSTEMATICALLY wrong on stochastic content, not marginally
(fitting noise slashes search-time SATD while the coded residual gains
nothing), so no post-hoc margin threshold can police it — the G3
correlated-arms law. Refutation basis: three varied probes (per-clip BD ×
2 batches, margin distributions, split-rate columns). Next candidates for
the fit: per-clip signals through gate_optimizer on the full 24-clip table;
if none separates, harvest the chosen splits' MV-SCATTER (noise chases
random sub-MVs; structure moves them coherently) — a NEW column, not a
reweighting of J.

### RESOLVED 2026-08-06 — the loser column was a MISPRICED SEARCH, not content

The gate fit was refused twice (class contradictions, then J-margin). The third
probe asked a different question — is the DECISION wrong rather than the
routing — and it was.

**The defect.** `best_part` prices partitions by SATD, i.e. PREDICTION error,
which always falls as partitions get finer. On stochastic residual the
quantizer discards that detail anyway, so the split bought nothing and paid
mvd bits. Textbook wrong-sign proxy (the AV2 law: "D must be quantized-recon
SSE; a prediction-error proxy is not merely imprecise, it is WRONG-SIGN on the
content the feature targets"). The J-margin refutation was the tell in
hindsight: the losers' margins were FATTER than the winners' because the
search was confidently wrong, not marginally wrong.

**The fix** (`tune_sub8_rd`): when the SATD search proposes a split, plan BOTH
arms for real (MC -> transform -> quantize -> reconstruct) and score
`J = SSD_recon + lambda*bits` — the level-aware `sum rdoq_rate(|level|)`
currency the inter-8x8-vs-4x4 decision already used in-tree, PLUS mvd bits
(which do NOT cancel here: the arms carry different mvd counts, and that
difference is the comparison). MB state snapshotted/restored around each trial.

**Probe:** RD-priced vs SATD-priced, harbour **-4.11/-4.28**, foreman
**-0.18/-0.41** — improves the worst loser AND the winner, which no dispatch
can do (a gate can only move losers toward zero).

**Absolute table, 19 clips (RD-priced splits vs no splits). Every clip
improved; SATD-priced 7W/13L/3N became 10W/2L/7N:**

| clip | SATD | RD | | clip | SATD | RD |
|---|---:|---:|---|---|---:|---:|
| screen_text | -11.34 | **-12.40** | | crew | +1.32 | -0.08 |
| bus | -2.95 | **-5.53** | | akiyo | +0.32 | +0.01 |
| screen_ui | -3.21 | **-4.07** | | city | +1.14 | +0.07 |
| mobile | -2.67 | -2.38 | | harbour | **+4.63** | **+0.15** |
| foreman_qcif | -0.34 | **-2.25** | | soccer | +1.30 | -0.27 |
| foreman | -1.73 | -2.14 | | akiyo_qcif | -0.15 | -0.19 |
| panc | -1.44 | -2.01 | | tsrc | -0.14 | -0.15 |
| football | **+2.95** | **-1.59** | | grain_akiyo | +13.63 | +3.01 |
| tempete | +1.83 | **-0.77** | | grain_flat | +15.77 | +5.83 |
| mand | +1.13 | -0.73 | | | | |

**The residual gate — GRAIN, and the trigger already existed.** The two
remaining losers are both grain, and no correct pricing makes fitting noise
worthwhile. The optimizer's top rule (`lv_spread>1.89`, 100% of gain, both
splits) was REFUSED: 228/412 depth-1 rules pass with only two losers
(separation is free), and that threshold incidentally gates off harbour
(lv_spread 1.66). Shipped instead: the `grain_signature()` conjunction already
validated per-frame and default-on for AQ and mb-tree — now factored to ONE
definition on `FrameSignals` with three consumers, so a re-fit cannot drift
them apart. Result: grain_akiyo **0.0000 exact**, grain_flat +0.16 (the same
first-GOP fail-open residual as `aq-grain-veto`, ∝ 1/n_gops); winners
unchanged (bus -5.53, screen_text -12.40, mobile -2.38, harbour +0.15).

**Final: 10 wins, 9 neutral, 0 losers — worst class <= 0.** Suites green.

### THE DUAL VERDICT (2026-08-06) — quality AND cost, per great-gate.md §4

Recorded because the first write-up of this entry reported quality alone, on a
corpus missing every HD clip. Both omissions are now closed.

**Cost, deterministic** (`gate_work()` via `bench/examples/gatecheck.rs`,
30 frames, quality preset — one run, no pinning needed):

| clip | best_part | mb_plan | split-arm won | **RD reverted the SATD pick** |
|---|---:|---:|---:|---:|
| akiyo_cif | 3.67x | 1.44x | 39.1% | **52.7%** |
| foreman_cif | 4.13x | 1.74x | 34.2% | **60.6%** |
| harbour_4cif | 4.45x | 2.56x | 59.5% | **83.1%** |
| screen_text | 3.92x | 1.75x | 28.0% | 28.6% |
| grain_akiyo | **1.00x** | **1.00x** | 0.0% | — (vetoed) |

Two readings that matter. **The revert column is the mispricing, quantified:**
53-83% of the SATD search's split picks are overturned once the coded
macroblock is asked, worst on harbour — the clip that was +4.63%. And **the
grain veto makes the feature free, not merely neutral** (1.00x): a vetoed
frame never runs the split search at all.

**Cost, wall** (`bench/pinvs.ps1`, pinned, CPU time, ABBA, crowd_run 1080p):
**5.59x** (sub8+RD 60.4 s vs base 10.5 s median CPU, base faster 5/5,
z = 2.24). A first attempt on CIF was REFUSED by the harness itself — 328 ms
is ~21 scheduler ticks, so the ratio would have been timer quantisation, not a
measurement; the workload was lengthened rather than the warning ignored.

**Verdict: quality YES, cost NO — stays opt-in.** 5.59x is not a preset step,
it is a different encoder. Note what the revert rate says about reducing it:
the RD trial changes the answer on 53-83% of candidates, so by the
search-skip-gate law (high win-rate = productive stage) this is NOT a search
to gate away. The cost must come out of the implementation instead:
(1) cache the winning plan for the emit path, which re-plans it today (one of
the three plans per split MB is pure duplication); (2) the 4-wide MC/cost
kernels (census #8) — sub-8x8 currently takes the scalar fall-through.

**Corpus coverage.** Quality table is 19 clips + the 8 HD clips re-run
separately (`sub8_abs_hd.log`); the HD arm confirms the same direction
(crowd_run -2.50, shields -0.21, in_to_tree -0.16, FourPeople -0.16,
blue_sky -0.01, stockholm +0.35). No class is now unmeasured.

**Default-on preconditions (updated):** the QUALITY case is now made; the
SPEED case is measured and is the blocker (5.59x). (1) the RD arm costs two extra full macroblock
plans per split candidate — probe-grade; the obvious first optimization is
caching the winning plan for the emit path (which re-plans it today), then the
4-wide MC/cost kernels (census #8). (2) CAVLC emission if that population
should share the win. Until then: opt-in (`tune_sub8x8_split` +
`tune_sub8_rd`), off = byte-identical.

**Dead machinery already in the tree.** `FrameEncoder::trial_intra` is a
complete trial-encode RD evaluator (snapshot -> `encode_mb` into scratch ->
real bits + `mb_ssd` recon SSD -> restore) and has **zero callers** — for the
intra-vs-inter site the work may be WIRING, not building ("exported != wired",
the same law as the SATD asm kernel that sat uncalled for months). Related
drift: `Preset::Quality`'s doc claims "every candidate trial-encoded for real
J = SSD + lambda*bits", which is not what the partition/intra decisions do —
hygiene-batch class.

**Harness trigger (why P4 comes first).** Re-pricing touches `best_part` and
the mode decision — shared upstream that EVERY gate sits on. Today already
demonstrated the hazard: the AQ grain fix silently flipped mb-tree's grain
verdict (-0.63 -> +4.41) and only a re-run caught it. Any constant harvested
in SATD currency (`split_t` T=400/600) is void once pricing moves.

**Transferable — the defect is NOT sub-8x8-specific.** `best_part` prices
every partition comparison. Still SATD-priced and DEFAULT-ON: 16x16-vs-16x8-vs-
8x16-vs-P_8x8, and intra-vs-inter. Severity should scale with the finer arm's
freedom to fit noise (4x4 catastrophic, 16x8 probably mild) — but that is a
hypothesis, and `tune_intra_penalty = 24.0` is a fitted correction for exactly
this proxy's bias, with the P2 lambda sweep already finding 0 better on all
three clips. Next probe: the same RD arm on those decisions.

## intra-rd-grain — SHIPPED opt-in 2026-08-06 (P3 RD-pricing probe #2)

```
GATE := (unit: frame, signal: FrameSignals::grain_signature() (4th consumer),
         threshold-form: fixed conjunction,
         arms: intra-vs-inter by RD (SSD_recon + lambda*bits, both candidates
               planned for real) / by SATD + fitted tune_intra_penalty,
         fallback: tune_intra_rd=false (default) = byte-identical;
                   RFF_INTRA_RD_ALL=1 removes the grain gate,
         entry: this)
```

**Premise.** The sub-8x8 result said the defect is not sub-8x8-specific:
`best_part` prices EVERY partition comparison by SATD, and the intra-vs-inter
decision adds a second proxy on the other side (`best_i16_satd`) plus a FITTED
CORRECTION for the proxy's bias (`tune_intra_penalty = 24.0`). A constant whose
job is to bias one arm because the currency mis-ranks them.

**The evaluator already existed and was dead.** `FrameEncoder::trial_intra` —
snapshot -> `encode_mb` into scratch -> real bits + `mb_ssd` recon SSD ->
restore — had ZERO callers. Wiring, not building ("exported != wired").

**Quality (4-QP, quality preset, anchor = SATD+penalty):** grain_akiyo
**-4.73 PSNR / -5.22 SSIM**; tempete -0.58/-0.60; foreman -0.22/-0.03;
akiyo -0.08/-0.16; harbour -0.13/-0.09; mobile -0.02/+0.03; screen_text
+0.53/-1.48 (**metrics disagree — not a win, not counted**).

**Cost + the flip census (why this is a GRAIN fix, not a general win):**

| clip | bytes | best_part | mb_plan | RD overturns SATD |
|---|---:|---:|---:|---:|
| grain_akiyo | -4.97% | 1.00x | 2.53x | **18.4%** |
| screen_text | -0.48% | 0.99x | 1.92x | 12.6% |
| foreman | +0.09% | 1.00x | 1.99x | 1.1% |
| harbour | -0.04% | 1.00x | 2.00x | 0.3% |
| akiyo | 0.00% | 1.00x | 2.00x | **0.0%** |

The proxy is essentially RIGHT on natural content (0-1% of decisions flip) and
badly wrong on noise. Ungated wall cost: **1.71x CPU** (pinvs, crowd_run
1080p, 17.3 s vs 10.1 s, 5/5, z=2.24) — 1.71x for ~nothing off grain.

**Shipped GRAIN-GATED**, the fourth consumer of one `grain_signature()`
definition (AQ, mb-tree, sub-8x8 splits, and now intra-vs-inter — grain breaks
four separate premises for the same physical reason). Verified: grain keeps
-4.73/-5.22; foreman, harbour, tempete all **0.0000 exact** (byte-identical,
zero cost).

**Deliberately forgone, recorded rather than fitted:** tempete's -0.60 and
screen_text's ambiguous case. Widening the gate to catch them is a future fit
needing more clips — expanding a gate on one clip's evidence is the
combination-space trap. Prefer the gate that abstains.

**Open follow-up:** `tune_intra_penalty = 24.0` is a correction for the proxy
this probe replaces. On grain-gated frames the penalty is now double-counting
and should be swept there; the P2 lambda campaign already found 0 better on all
three clips it tested.

## best_part / ME cost — SHIPPED (two shape dispatches) + centre-2 PRUNED five ways — 2026-08-06

Opened because sub-8x8 shipped default-on at a measured **5.59x CPU**. The
question was "find a better way than brute forcing", and the campaign split the
cost into three centres before touching anything:

| centre | measured | contents |
|---|---|---|
| 1 cost per search | evals 2.2x | ladder, sub-pel pattern, seed alloc, kernels |
| 2 **number of searches** | **calls 4.1x** | the 9-shapes-per-quad brute force |
| 3 the RD trials | plans 1.74x | `plan_inter_mb`, not best_part at all |

**CPU tracked CALLS (4.1x), not evals (2.2x)** — so centre 2 looked like the
root. It is not reachable. See the prune below.

### SHIPPED — two shape dispatches (structural signal, no fit, no corpus risk)

`diastats` is the ME harvest tap (the stage's analogue of the P1 signal
vector); its per-rung table wrote both gates:

| rung | reach | share of ALL evals | hit rate |
|---|---|---:|---:|
| s0 | 4 px | 30.0% | **0.97%** |
| s1 | 2 px | 31.6% | 2.20% |
| s2 | 1 px | 38.3% | 6.42% |

...with IDENTICAL shares whether the split search ran or not — i.e. every 4x4
walked the same 4-pixel-reach ladder as an unpredicted 16x16, despite being
seeded from its parent's converged MV.

1. **Shape-aware diamond ladder** (`dia_sub_mask`, default fine rung only):
   **-44.5% full-pel evals**, and BD *improves* on 4 of 6 clips (foreman
   -2.14->-2.21, harbour +0.146->+0.073, mobile, tempete). Not a trade — coarse
   rungs on a tiny block chase spurious far matches that wreck the MV field its
   neighbours predict from, the same mechanism the diagonal-probe note records,
   never applied to rung REACH.
2. **Shape-aware sub-pel pattern** (default 2 = 8-point ring, single pass).
   Swept all four: the RING carries the gain, the ITERATION is confirmation —
   pattern 2 BEATS the full walk on bus (-5.64 vs -5.49) at ~8 evals instead of
   ~29. Dropping the ring too (patterns 1/3) is where quality actually goes.
3. **Stack seed buffer** — `best_part` heap-allocated a `Vec` on every search;
   at 208k searches/clip that is 208k allocations for at most 3 MVs. Verified
   **byte-identical** by reverting the hunk and diffing hashes (2dfdfd... both).

### PRUNED — centre 2 cannot be reached (five probes, two grains)

**Per-QUAD skip gate** — a pre-search feature must predict "this quad's split
will lose". Ratio = (% wins lost)/(% searches skipped); the reference gate shape
(LRF) is ~0.02, i.e. skip 45% and keep 99%:

| feature | measures | ratio |
|---|---|---|
| `j8/lambda` (null-arm cost, the king feature) | difficulty | 0.90-1.00 |
| `mbvar` | texture | 0.59-0.91 |
| `mvdiv` = \|mv_quad - mv16\| | **motion boundary** | 0.87-0.99 |

Uniform failure on every clip — **no sign flip, so not a dispatch, a genuine
prune**. The informative one is `mvdiv`: quads whose MV EXACTLY matches the
parent (a third to half of all quads) benefit from splitting at the same rate as
diverging ones. Whether splitting helps is a property of the quad's RESIDUAL,
which does not exist until you split.

**Per-FRAME payoff census** — the grain the harvest pointed at, since split
survival varies 2.4x by content (harbour 13.7%, mobile 33.2%). Shipped shape of
me_wide's payoff learner and the free-skip census. Two objectives, both fail:

| objective | crowd_run win retained |
|---|---|
| COUNT of surviving splits (minpay 15%) | 22% |
| VALUE, mean RD J saved per searched MB | **4.6%** |

The count version was my error (the suppressor's cardinal lesson: unit-weighted
objective, never a tally). But the value version is WORSE, and that is the
finding: crowd_run earns -2.43% BD from splitting while its mean local J saving
is under 2 lambda. **The payoff PROPAGATES** — better recon feeds every later
frame's prediction — and per-unit RD is blind to it (the law that killed the
per-block OBMC dispatch). A census measuring local payoff cannot see the value
it is deciding about. Kept default-OFF (`RFF_SUB8_MINPAY=0`) with the machinery
and this note in place.

### Result and what remains

Cost 5.59x -> **~3.8-4.9x** (ratios taken across a session in which the baseline
arm drifted 11.6 -> 15.0 s for identical work, so they are directionally sound
and not all commensurable — the clean same-session numbers are the eval counts
and the byte-identity checks). Quality: BD improved or held on 5 of 6 clips.
Suites green both crates, ffmpeg PIXEL-EXACT.

### CENSUS #8 CLOSED — by composition, not new intrinsics (2026-08-06)

`satd_px` asm-dispatched (16,16)|(16,8)|(8,16)|(8,8)|(4,4) — but **not (8,4) or
(4,8)**, which the sub-8x8 split arm made hot, and openh264 ships no kernel for
either shape. It did not need one: SATD here is DEFINED as the sum of 4x4
Hadamards, so 8x4 and 4x8 are exactly two `satd_4x4` calls; each wrapper
returns (Σ+1)>>1 and every 4x4 Σ is even, so summing halves and doubling once
is bit-exact.

**4.61x -> 3.05x** (7 pairs, z=2.65), **byte-identical** (hash unchanged,
2dfdfd...), suites green. The largest single cut of the campaign, from calling
a kernel that was ALREADY compiled and ALREADY bound but never dispatched for
those shapes — "exported != wired", the same law as the SATD kernel that sat
uncalled for months and the `_avx2` twins found unbound in the vendored objects.

**Campaign total: 5.59x -> 3.05x** (45% of the feature's cost removed) with
quality improved or held on 5 of 6 clips.

**`MeCtx::new` — pruned by inspection.** It returns `None` immediately for any
shape outside the four covered ones, so it is NOT a per-call cost for
sub-partitions. The flip side is the real finding above: those evaluations had
no fast evaluator at all and were paying the safe dispatch path.

### Remaining, SPECIFIED not built

**Centre 3 — the duplicate plan.** The winner is planned in the RD trial and
again in emit: 3 plans per split macroblock where 2 suffice. Recoverable by
planning the winner LAST (skip the final restore) and caching it, which also
lands on the majority path since RD reverts to all-8x8 53-83% of the time.
NOT built: the cache is valid only while nothing between the trial and emit
mutates macroblock state — true on the default path, FALSE under `sp_defer`
(re-refines and rewrites MVs) and under `shape_rd` (re-plans every candidate).
It needs a guard, not a reorder, and a silent state bug here is a conformance
issue rather than a quality tweak. Bounded prize: ~1/3 of the plan cost on
split MBs, and plans are the smallest of the three centres (1.74x vs calls
4.1x).

**`MeCtx` for sub-8x8 shapes.** Its table maps shapes to *avx2* Wels kernels
and openh264 ships only sse2/sse41 at 4x4, so extending it means mixing ISA
tiers in that table. Lower priority now that `satd_px` covers the shapes.

## shape-rd (16x8 / 8x16 / P_8x8 shape decision by RD) — SHIPPED default-on behind a texture GUARD — 2026-08-06

The third SATD-priced default-on site (P3 RD-pricing probe #3). Built with the
same arm as the others: plan each candidate shape for real, score
`J = SSD_recon + lambda*bits`.

**A currency bug first, caught by an impossible number.** The first run read
**+17% to +56% BD** — a better cost function cannot lose 50%, so it was chased
rather than recorded (codec-measurement §7). Cause: the RD J was written back
into `best_c`, which the intra test then compared against a SATD-domain
`c_intra`. Mixed scales, so intra won nearly every macroblock. Fixed by making
intra compete in the same currency: one decision, one currency.

**Quality, re-measured on the DEPLOYED estimator** (the ladder/sub-pel/SATD
changes are all bitstream-changing, so the pre-campaign table was void):

| clip | BD-PSNR | BD-SSIM |
|---|---:|---:|
| harbour | -2.57 | **-1.95** |
| tempete | -1.66 | -1.15 |
| screen_text | -1.01 | -1.04 |
| bus | -1.21 | -0.76 |
| foreman | -0.64 | -0.52 |
| **mobile** | -0.44 | **+1.99** |

**Cost: NOT RESOLVABLE.** 7 pairs on 720p read a 1.083x median ratio at
**z = 0.38**, with the medians inverting against the ratio — the honest reading
is "somewhere near free, and the instrument cannot separate it from zero". No
number recorded.

**Verdict: DEFAULT-ON behind a texture GUARD (not a model).** 13 clips measured.

| clip | med_var | BD-PSNR | BD-SSIM | |
|---|---:|---:|---:|---|
| football_cif | 449 | -5.20 | **-3.90** | win |
| harbour_4cif | 663 | -2.57 | -1.95 | win |
| crew_4cif | 52 | -2.01 | -1.81 | win |
| maxtex_plaid* | 2583 | -1.82 | -1.87 | win, GUARDED OFF |
| tempete_cif | 785 | -1.66 | -1.15 | win |
| city_4cif | 295 | -1.68 | -1.41 | win |
| bus_cif | 694 | -1.21 | -0.76 | win |
| screen_text* | 0 | -1.01 | -1.04 | win |
| foreman_qcif | 793 | -0.90 | -0.65 | win |
| maxtex_mandel* | 42 | -0.82 | -0.68 | win |
| foreman_cif | 281 | -0.64 | -0.52 | win |
| akiyo_cif | 61 | -0.26 | -0.15 | win |
| **mobile_cif** | 1494 | -0.40 | **+1.99** | LOSS, guarded to 0.0000 |

(*synthesized). **12 of 13 win on BOTH metrics; 13 of 13 win on PSNR.** With the
guard: mobile and plaid read exactly `0.0000, 0.0000` -- the byte-identical
non-beneficiary win-signature -- and every other clip keeps its FULL win,
unshaved. Worst class = 0.0000. Conformance re-verified pixel-exact vs ffmpeg on
the new default bitstream.

### FOUR REFUTED MECHANISMS (the honest part -- refutations are results)

The guard fires on `median_var > 1000`, but **that axis is NOT causal**, and the
record must say so or someone will build on it:

1. **Texture-causes-it — REFUTED by construction.** The corpus had exactly one
   clip above the line, so the threshold was fitted on n=1. Synthesizing a
   second (`maxtex_plaid`, dense structured plaid under pan, median_var **2583**,
   *above* mobile) predicted a loss and delivered a **-1.87 WIN**. The
   prediction test the campaign's own method demanded is what killed the story
   -- ON-side holdout passed 5/5 (foreman_qcif at 793 wins -0.65) and the
   OFF-side prediction still failed. *A holdout that only samples one side of a
   threshold cannot validate it.*
2. **`dcfrac` — REFUTED as config-dependent.** It separated cleanly (losers
   <=0.111, winners >=0.176) but ONLY under the per-MB-lambda arm. Under the
   shipped lambda, screen_text (dcfrac 0.024, the LOWEST) is a clean winner. An
   axis whose meaning flips with an unrelated config change was chance.
3. **AQ interaction — REFUTED.** mobile still loses **+1.64** at `aq=0`. AQ
   explains at most a fifth of the 1.99.
4. **Chroma currency — REFUTED, monotonically backwards.** `mb_ssd` sums chroma
   1:1 with luma while the graders (`ssim_y`, Y-PSNR) are luma-only, and mobile
   is the most chroma-rich clip -- a clean hypothesis in the same family as the
   two currency bugs this campaign already fixed. Down-weighting chroma made it
   **worse**, monotonically: 1.99 -> 2.06 (w 0.5) -> 2.07 (w 0.25) -> **2.11**
   (w 0). Knob kept (`RFF_RD_CHROMA_W`, default 1.0 = byte-identical).

**So the guard is a BOUND, not an explanation.** Within the 24-clip natural
truth table exactly one clip exceeds the threshold and it regresses, so the
guard can only ever forgo a win, never create one. Its measured price is
plaid's -1.87 on synthetic single-frequency texture. Accepted, and flagged for
deletion the moment a real mechanism appears. What mobile actually is: the one
clip where minimizing SSE (which shape-RD does BY CONSTRUCTION, hence 13/13 on
PSNR) destroys local structure SSIM scores and SSE cannot see.

**Cost: NOT RESOLVABLE** -- 7 pairs on 720p read 1.083x at **z = 0.38** with the
medians inverting against the ratio. Near free; no number recorded.

## rd-lambda-mb (per-MB RD lambda) — REFUTED, knob kept — 2026-08-06

Found by audit, not by a symptom: `lambda` is built ONCE per slice from the
FRAME qp ([mb16.rs] slice prologue), but AQ (default strength **1.0**, no
separate enable flag) and mb-tree rewrite `fe.qp` per macroblock. Every inter RD
site -- including **sub8-RD, which is DEFAULT-ON** -- therefore priced
`SSD + lambda_frame*bits` against a block quantized at `qp_mb`, mispricing rate
by `2^((qp_frame-qp_mb)/3)`, worst exactly on the high-variance macroblocks AQ
moves furthest. Textbook-wrong currency, same family as the SATD-vs-recon-SSE
bug.

**And the measurement refused it.** Arm B = old frame-lambda; negative = old
better: bus **-0.17/-0.12**, foreman -0.02/-0.02, screen_text +0.10/**-1.44**.
The theoretically-correct form is a wash-to-loss on the default path and gives
up 1.44% BD-SSIM on screen_text. Plausible mechanism: AQ already applies a
perceptual discount to high-variance macroblocks; making lambda rate-averse
there too DOUBLE-COUNTS it, and the frame lambda was accidentally compensating.

`tune_rd_lambda_mb` defaults **false**, kept as a pinnable A/B arm. **Law
reaffirmed: a currency bug proven by reading the code is still a HYPOTHESIS
until the corpus rules on it.** Two of this campaign's currency fixes were large
wins; this one, structurally identical, is a loss.

*Harness note:* the first run of this A/B printed **all zeros on six clips** --
`SCALES=1.0` set arm B to `true` while the new default was already `true`, so
both arms were the same config. Caught by the null-result smell. The rule stands:
an A/B arm must PIN its value, never inherit a default.

## mbtree-backoff-refit — SHIPPED default 2026-08-06 (ramp → single-sided latch at 0.03)

```
GATE := (unit: GOP, signal: residual_frac (1 − mean (intra−inter)/intra over the
         GOP's lookahead), threshold-form: single-sided latch on a wide natural
         gap, arms: mb-tree full strength / zero offsets (byte-identical to off),
         fallback: RFF_MBTREE_RESMIN overrides (0 = no back-off), entry: this)
```

**Finding.** The shipped back-off (eff = strength·min(rf/0.10, 1)) had the
right axis and the wrong shape+threshold — INVERTED against the real corpus:
it throttled mb-tree's biggest winners (akiyo_qcif rf 0.046 → eff 0.44;
screen_text rf 0.04 → eff 0.35) while its one true protectee — tsrc-class
synthetic, rf 0.023–0.025 — still leaked +0.50% BD-SSIM through the ramp's
partial strength. Synthesized the original calibration classes
(tsrc/mand/panc via ffmpeg) and swept RESMIN {0.10, 0.05, 0}: every throttled
clip wins MORE at 0 — including panc-class (−2.38 → −3.83), the class the
ramp was built to protect — except tsrc (+0.50 → +3.53, the only true
regressor). Populations are disjoint with a 1.56× gap (tsrc ≤ 0.025 |
winners ≥ 0.039) → the latch: **OFF below rf 0.03 (zero offsets,
byte-identical), FULL strength above.**

**Trial (24-clip 4-QP matrix + calibration classes):** akiyo_qcif −5.09 →
−9.24, akiyo_cif −9.60 → −9.96, screen_text −4.53 → −7.06 BD-SSIM;
tsrc-class 0.0000 exact (latched); panc −1.95 / mand −1.61 stay winners;
17 corpus clips byte-identical (foreman +0.2784 to 4dp). Suites green both
builds. NOTE: this matrix runs on the post-aq-grain-veto baseline; stockholm/
crew/soccer/grain deltas vs the older table are that baseline shift, not the
latch (their rf ≥ 0.3 → full strength under both shapes).

## mbtree-grain-veto — SHIPPED default-on 2026-08-06 (P3 item 1; PROVISIONAL: same n=1 exemplar as aq-grain-veto)

```
GATE := (unit: GOP, signal: median_var x grain_floor x mgain (source-vs-source,
         the GOP's first frame pair), threshold-form: fixed conjunction,
         arms: mb-tree lookahead+offsets / zero offsets (byte-identical to off),
         fallback: RFF_MBTREE_GRAIN=0 -> pre-gate bytes exactly, entry: this)
```

**Premise.** Propagation credit is fiction on noise — nothing persists, so
mb-tree redistributes on false gradients. Measured +4.41% BD-SSIM on
grain_akiyo once the AQ grain veto stopped masking it (rf 0.81 puts grain far
above the residual-frac latch — a different failure axis than tsrc's).

**The rule** — the aq-grain-veto conjunction verbatim, evaluated once per GOP
in `gop_qp_offsets` on `full[1]` vs `full[0]` (planes already built there;
both inside the GOP, so a boundary scene cut cannot sit between the probe
pair; grain is stationary — per-frame floor spread 7.7–8.3 over 120 frames).
Thresholds transfer from the AQ fit: its IDR arm already validated this
conjunction on source-vs-source signals. Fires → zero offsets and the whole
lookahead is skipped (a speed bonus on grain).

**Trial:** grain_akiyo mbtree BD +4.41 → **0.0000 exact** (grain_flat 0.0000);
akiyo_cif/foreman/tsrc_class/screen_text/stockholm numerically IDENTICAL to
the post-backoff-refit table (latch fired nowhere else). Suites green.
Interplay: AQ-on-grain first-GOP residual re-reads +12.07 (was +10.85)
against the cleaner latched baseline — same fail-open mechanism, still
∝ first-GOP share. Single-frame GOPs and streams fail open (no probe pair).

## mbtree-dispatch — CLOSED, SHIPPED default-on 2026-08-06 (removed 7 live regressions; +26.82 vs +23.25)

```
GATE := (unit: GOP/clip, signal: TBD (temporal predictability), threshold-form: TBD,
         arms: mbtree on / off, fallback: off = byte-identical, entry: PROVISIONAL)
```

**Truth table (BD-SSIM %, mbtree on vs off, −ve = win).** Sign-flips across
classes — the dispatch trigger:

| class | verdicts |
|---|---|
| static | akiyo −9.60, akiyo_qcif −5.09, FourPeople −2.67 — the big winners |
| SCREEN | screen_text −4.53, screen_ui −4.93 — winners |
| motion-local | football −1.39, soccer −0.59 win; foreman ±0 |
| **pan-struct** | **stockholm +4.14** ✗, city +0.27 ✗ |
| pan-natural | shields +1.06 ✗, in_to_tree +0.69 ✗, blue_sky ±0 |
| busy-motion | ducks +1.20 ✗, crowd_run +0.35 ✗, park_joy ±0 |
| GRAIN | grain_akiyo +0.63 ✗ (measured pre-AQ-grain-veto — re-measure) |

The shipped predictability back-off does NOT hold the pan classes (stockholm
−4.14 is a real regression under the current default-on-in-CLI configuration).

**Optimizer candidate (recorded, NOT shipped):** `lv_spread>2.28 &
flat_run>1.16` — fires on 8 units, net +27.1 of +29.8 perfect, both splits
positive, worst fired class +0.01. **Refused for transcription**: both clauses
are SPATIAL statistics standing proxy for a TEMPORAL phenomenon (propagation /
predictability) — the signal-must-measure-what-the-tool-stresses law
(codec-content-adaptive-dispatch; the pyramid-depth fit failed exactly this way,
8/8 fitted → 2/6 held out). With 249/398 depth-1 rules passing the both-splits
gate on this corpus, a passing separation is the default outcome here.

**Next step:** add the temporal predictability axis to the P1 signal vector
(the 2-gap/1-gap motion-compensated residual ratio — machinery exists in the
`adaptive_bcount` lookahead), re-harvest, re-fit at the GOP grain (the unit
where mb-tree's objective is complete), THEN transcribe.

**Interim:** worst class ≤ 0 is VIOLATED by the current default (stockholm
+3.10 on the post-veto baseline); if a default change is wanted before the
temporal fit lands, the safe direction per the plan is opt-in, not a
spatial-proxy gate.

### SHIPPED default-on 2026-08-06 — the DIFFERENTIATION LATCH (1 variable, not 3)

```
GATE := (unit: GOP, signal: sd(mb-tree's own centered propagation offsets),
         threshold-form: sd < 1.0 -> zero the offsets, arms: mb-tree on / off,
         fallback: zeroed == mb-tree OFF byte-identically, cost: nil (sd is a
         reduction over offsets already computed), entry: SHIPPED)
```

**COMPLETE ACCOUNTING — 23 clips, BOTH arms, ONE base (2026-08-07).** Every
earlier tally in this entry was partial or on a stale base; this is the verdict.

|  | total BD-SSIM | regressions |
|---|---:|---:|
| always-on (what shipped before) | -24.03 | **8** |
| **gated (shipped now)** | **-25.95** | **0** |

**Better by +1.92 AND every regression removed.** 6 clips fire (identical in
both arms, -25.95); 17 abstain to exactly 0.0000.

Of the 17 abstentions the gate **avoids +9.79 of losses** (stockholm +3.83,
shields +2.30, blue_sky +1.41, ducks +1.12, bus +0.80, crowd_run +0.28, harbour
+0.05, mobile +0.00) and **forgoes -7.87 of wins** (football -2.09, mand -2.02,
tempete -1.53, in_to_tree -1.33, soccer -0.38, park_joy -0.19, crew -0.17, city
-0.13, foreman -0.03).

⚠ **The margin is thin and the forgone value is large.** The gate is right by
the finish line (worst class <= 0, and net positive besides), but it leaves
**7.87 BD-SSIM on the table across 9 clips** — a better gate exists. Three
corrections this accounting forced on earlier claims in this entry:
- "**7 regressions removed**" was wrong; it is **5** among the originally-named
  clips. `in_to_tree` (+0.69 -> **-1.33**) and `city` (+0.27 -> **-0.13**)
  FLIPPED SIGN when today's sub8x8/intra-RD/shape-RD defaults landed. Two new
  regressions appeared that the old table did not have (`blue_sky` +1.41,
  `harbour` +0.05), so the count is 8 on the current base.
- `mand_class` and `football_cif` are the two largest forgone wins (-2.02,
  -2.09) and **neither is in the mb-tree truth table** the rule was fitted on.
  A rule fitted on a 16-clip table was priced against content it never saw.
- The 16-day-old `mbtree-state` memory calls tsrc mb-tree's biggest win
  (-1.80%). STALE: the back-off refit deliberately latches tsrc to 0.0000, and
  it measures 0.0000 in both arms here.

**`sd` BOUNDS the losers; it does not EXPLAIN the winners** — the same shape as
the shape-rd texture guard. football (sd 0.611) sits BETWEEN bus (0.534, +0.80)
and shields (0.713, +2.30); mand (0.542) and tempete (0.708) sit in the same
band. **No cut on this axis recovers them.** Recovering that 7.87 needs a second
axis that separates within the low-sd band, not a moved threshold.

**Composition verified** with the older `residual_frac` back-off: `panc` fires
at sd 4.4-4.6 and WINS -0.63 (its historical +1.12 regression is gone on the
current base), `tsrc` is fully latched by the back-off in both arms. No hole
between the two gates.

### How the 3-variable candidate died — the run is the whole story

The optimizer extracted, at a **perfect +28.800 of +28.800** on the 16-clip
table, both splits positive, worst fired class +0.000, zero losers fired:

```
spread > 1.0  OR  (headroom > 10 AND tdecay > 1.3)
```

The two extra clauses existed only to recover football (+1.39) and soccer
(+0.59), which win but sit below the spread line. **Then it met clip 17.**
`bus_cif` — a FAST PAN, the exact class this gate exists to protect — has
headroom 24.37 and tdecay 1.62, so the rule fired it, and it **LOST +0.68
BD-SSIM**. Arm 2 dropped.

Per-variable verdict from the run:

| variable | verdict |
|---|---|
| `spread` (mb-tree's own offset dispersion) | **CONFIRMED** — fires 5, all wins; all 7 losers below the line |
| `headroom` | **REFUTED** — 24.37 on bus_cif, which loses |
| `tdecay` | **REFUTED** — 1.62 on bus_cif; the axis also INVERTED (pans have LOW decay: global ME compensates pure translation perfectly at any gap, so stockholm reads 1.05 and shields 1.07, BELOW the winners) |

The surviving rule scores **lower on the fitting set** (+26.82 vs +28.80) and is
the one that holds. That is the combination-space calibration vindicated
exactly: the optimizer reported **182/308 depth-1 and 13212/43092 depth-2 rules
passing**, so a perfect score was the DEFAULT outcome and carried no
information. Rank chose the dead rule; mechanism chose the live one.

**Why `spread` is not another proxy.** The two fits refused here before
(`lv_spread & flat_run`, and `flat_run` again in this run — every top-ranked
rule the optimizer returned) are SPATIAL statistics correlating with panning on
this corpus. `sd` is mb-tree's OWN output: it measures whether the tool has
anything to differentiate. mb-tree buys something only when propagation is
DIFFERENTIAL (a static scene with a moving subject; screen content with dead
regions); on a smooth pan every block propagates equally, the centered offsets
are noise, and zeroing them is exactly mb-tree off. The signal is free — a
reduction over offsets already computed.

**Escape hatch + coverage:** `mbtree_spread_min` config field (`0.0` = ungated),
`RFF_MBTREE_SDMIN` env override, census slot `mbtree_spread` (akiyo 0/1 applies,
foreman 1/1 latches). New test `mbtree_spread_latch_is_mbtree_off` asserts a
latched-off encode is BYTE-IDENTICAL to mb-tree off — without that, an
abstaining gate is a third behaviour nobody measured. Two pre-existing liveness
tests were correctly tripped by the latch on a 96x64 synthetic and now pin
`mbtree_spread_min = 0.0` to test the mechanism.

### Superseded: the candidate as extracted

The blocking temporal axis was built (`temporal_decay_ratio`, exported;
`bench/examples/tdecay.rs`) — the 2-gap/1-gap global-MC residual ratio. It
INVERTED the stated hypothesis: pans have the LOWEST decay (stockholm 1.05,
shields 1.07), not the highest, because global ME compensates pure translation
perfectly at any gap. It does not separate alone (screen_text 1.17 and football
1.37 WIN between losers), so it was not fitted on.

The inversion pointed at the real mechanism. On a smooth pan EVERYTHING
propagates equally, so mb-tree's per-MB offsets carry no information and merely
redistribute rate; it wins where propagation is DIFFERENTIAL. The signal for
that is the dispersion of mb-tree's OWN propagation-derived offsets — already
computed as `spread` under `RFF_MBTREE_DBG`, so it costs nothing.

| spread | clips | verdict |
|---|---|---|
| 2.80, 2.13 | screen_ui, screen_text | win -4.93, -4.53 |
| 1.20, 1.07, 1.07 | akiyo, akiyo_qcif, FourPeople | win -9.60, -5.09, -2.67 |
| 0.73 .. 0.37 | 11 clips | **all 6 losers live here** |

**THE RULE TO TEST:**

```
mb-tree FIRES  iff   spread > 1.0
                OR  (headroom > 10 AND tdecay > 1.3)
```

Fires 9, abstains 7. **net +28.800 of a +28.800 perfect** (fire exactly the
winners), train +18.190, holdout +10.610, worst fired class +0.000, **zero
losers fired** — all six regressions abstain to byte-identical.

Per-clause physical justification (the calibration demands this, see below):
- `spread > 1.0` — mb-tree's OWN output dispersion. Not a proxy: it measures
  directly whether the tool has anything to differentiate. Below the line its
  offsets are noise around a centered mean.
- `headroom > 10 AND tdecay > 1.3` — the recovery arm for motion-local winners
  (football, soccer) that sit below the spread line. `headroom` says exploitable
  motion structure exists; `tdecay > 1.3` says prediction degrades with gap,
  i.e. NOT a pure translational pan — which is exactly what excludes shields
  (headroom 35.82 but tdecay 1.07) and stockholm (tdecay 1.05).

**NOT YET SHIPPABLE — the calibration forbids it.** The optimizer reports
**182/308 depth-1 and 13212/43092 depth-2 rules passing** on this corpus, so a
perfect separation is the DEFAULT outcome and carries no information; every rule
it ranked top was built on `flat_run`, the same spatial-proxy family refused
above. This candidate was selected on MECHANISM, not rank.

**Required test before transcription (both sides of every threshold —
`holdout-both-sides-of-a-threshold`, the law the shape-rd guard was caught by):**
1. Clips with `spread > 1.0` NOT in this table: predicted WIN. Any loser refutes.
2. Clips with `spread < 1.0` and high headroom/tdecay: predicted WIN via arm 2.
3. **The refutation arm:** synthesize/find content with `spread > 1.0` that is a
   PAN. The rule predicts a win; a loss refutes `spread` as causal exactly as
   `maxtex_plaid` refuted `median_var`.
4. Re-measure at GOP grain (mb-tree's objective unit), not clip grain.

**Update (2026-08-06, post aq-grain-veto + backoff-refit):** with AQ fixed on
grain, mb-tree's OWN grain loss was unmasked (grain_akiyo −0.63 → +4.41).
**RESOLVED by `mbtree-grain-veto` (P3 item 1, above): grain → 0.0000 exact.**
The open dispatch now has ONE remaining loser: pan-struct (stockholm +3.10),
which still needs the temporal predictability signal (P3 item 4).

## e2-seam-dispatch — CANDIDATE, runtime-only, perfect fit, awaiting a build — 2026-08-07

```
GATE := (unit: decode configuration, signals: cores / profile / bits-per-MB,
         form: cores>6 AND high-profile AND bits_per_mb<65,
         arms: E2 worker seam / inline, fallback: inline = BYTE-IDENTICAL,
         cost: three integer comparisons once per slice, entry: CANDIDATE)
```

The decoder E2 worker seam loses wall clock in most configurations and WINS in a
few. That is a dispatch, not a verdict — and unlike every other threading result
this session, the optimizer's calibration says the separation carries
information: **depth-1 0/30 passed, depth-2 9/296 (3.0%)**. Contrast
`mbtree-dispatch`, where 182/308 depth-1 rules passed and a clean separation was
the DEFAULT outcome.

**Units are CONFIGURATIONS, not clips** — profile x resolution x core count, 12
cells, each an interleaved `pinmtx` measurement. That reframing is what made the
problem tractable: as clips it looked like noise, as configurations it separates.

**THE RULE (all three signals available at RUNTIME):**

| | value |
|---|---|
| net | **+5.30 of +5.30 perfect** |
| train / holdout | +4.90 / +0.40 (both positive) |
| precision / recall | 1.00 / 1.00 |
| fires | 3 — stockholm_high_8c +4.50, shields_high_8c +0.40, intotree_high_8c +0.40 |
| holds | 9, all to 0.00 byte-identical; worst avoided **-10.20** |

It equals the optimizer's top rule (`cores>6 & pixel_share>15.6`) without needing
pixel share, which is only measurable by offline ablation and could never gate.

**CLAUSE ABLATION — every clause is load-bearing:**

| rule | net | losers fired |
|---|---:|---:|
| **full** | **+5.30** | **0** |
| drop `cores>6` | +4.60 | 1 |
| drop `high` | -4.90 | 1 |
| drop `bits<65` | -0.20 | 1 |
| always-on | **-45.00** | **9** |

**Per-clause physical justification:**
- `cores>6` — the seam is a TWO-THREAD design (one worker). Extra cores add no
  parallelism, only headroom so the worker is not competing with the parse
  thread. Measured: every 4-core cell loses or ties; the same clip/profile wins
  at 8.
- `high` — the 8x8 transform makes the pixel work CHUNKIER per macroblock, so
  more work crosses the seam per unit of overhead. main-720p loses -7.9 to -10.2
  at both core counts.
- `bits_per_mb<65` — coefficient density. Above it PARSE dominates and the pixel
  share collapses (crowd_run 1080p at 16.9 Mbps measures 10.2% vs 15.7-18.8% for
  the 720p clips). Counted during parse; no ablation needed.

**Threshold margins:** bits_per_mb fires at up to 47.7 and excludes from 86.5 —
a wide empty gap, not shaved against a datapoint. `cores` fires at 8 against a
nearest tested non-firing 4.

### NOT SHIPPABLE YET — three honest gaps

1. **n=12 units, 3 winners, and the holdout contains ONE winner (+0.40).** The
   headline +4.50 is a single clip. Thin.
2. **cores 5/6/7 are UNTESTED.** The threshold sits in a 4-to-8 gap with nothing
   measured inside it — the same one-sided-threshold flaw that
   `holdout-both-sides-of-a-threshold` records.
3. **The prize is small.** +5.30 total across 12 configurations: ~4.5% on one
   clip, 0.4% on two. Set against the CAVLC arm's byte-identity risk
   (`decode_inter` recons inline; CABAC builds its `EdcJob` inline in its own
   slice loop, so there is no shared parse/recon split to reuse), the build is a
   judgement call, not an obvious yes.

**Next, cheapest first:** fill the 5/6/7-core cells; add more high-profile
content at 8 cores to thicken the holdout; only then decide the CAVLC build.
