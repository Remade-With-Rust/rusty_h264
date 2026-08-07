# The Great Gate — full content-adaptive dispatch, every content type, every core function

**Mission.** Deploy the adaptive-dispatch law everywhere: every core function that
has (or should have) more than one way to do its job gets a content gate, and the
finish line for each gate is **worst content class ≤ 0, verified per class, never
on average**. The driver is concrete: the YOLO build (Diana, FFai) is failing
downstream on *variable* content — scenes whose character changes mid-stream. A
codec whose quality/speed swings with content hands that variance straight to the
detector. The fix is not a better compromise constant; it is finishing every
unfinished dispatch.

> A negative outcome on one content class is a DISPATCH, not a result. A fixed
> compromise that "averages positive" while one class loses ships a silent
> regression to whoever runs that class — and the YOLO build is that whoever.

---

## 1. The suppressor: what we imported and what it teaches

Copied for testing (verbatim, provenance preserved):

| here | from | what it is |
|---|---|---|
| `_greatgate/suppressor.rs` | `FFai/crates/ffai-carmenta/src/suppress.rs` | The shipped runtime gate: fitted depth-4 decision tree + block-level second pass |
| `_greatgate/suppress_fit_reference.py` | `FFai/.tools-bench/suppress.py` | The offline fit harness: feature harvest → tree fit → unit-weighted judging |
| `_greatgate/suppress_optimizer.rs` | `FFai/crates/ffai-bench/examples/suppress_optimizer.rs` | The rule-search tool (campaign §8.106–§8.117): candidate rules scored on signed net gain, both-splits gate, concentration guard, incremental-on-top-of-shipped scoring, exhaustive conjunction search |

`_greatgate/` is **gitignored** — it is FFai code brought over as private prior
art; it does not build here (it depends on `ffai_core` types) and must not reach
the public repo.

The suppressor's *features* are OCR-specific and transfer nothing. Its
**architecture** is the Great Gate, already proven in production on a different
domain, and it independently converged on the same laws our dispatch skill
distilled from codec campaigns:

1. **Population-relative normalization** — its `w_p90` (width ÷ *this page's* p90
   width) is exactly our per-frame-percentile-threshold law. Absolute thresholds
   die on content whose scale varies 50×; page/frame-relative ones transfer to
   layouts (content) the fit never saw. That normalization was *chosen for
   transfer* over an in-corpus-equivalent form — the standard our gates must meet.
2. **Decide per unit, feed group context as a feature** — its strongest signal
   (52% importance) is the parent *block's* character count handed to a per-*line*
   decision. Same law we learned: decide on the unit the metric counts; supply
   group statistics as features.
3. **Shallow transcribed tree as the gate representation** — depth ≤ 4, thresholds
   from a train split judged once on holdout, transcribed as plain code with one
   doc-comment per feature explaining its physical meaning. No model artifact, no
   runtime fitting. The depth-3 tree kept 64% of a 200-tree boosted model's win.
4. **Unit-weighted objective, not classification accuracy** — it fits against
   character-weighted net gain (our equivalent: bits/BD-SSIM, never "% of MBs
   routed correctly"). A 90%-precision rule can still lose if the 10% is dense.
5. **Precision-gated branches with a recorded ledger** — every branch ships with
   its measured precision, its bounded downside, and *why* it's safe (the
   bibliography branch: gated on a page-wide count, flat plateau 4–8, downside
   bounded at −160 characters). Provisional branches are labeled provisional.
6. **Abstention** — the section-scope branch *abstains* when layout contradicts
   detection rather than guessing. The gate that gives up fitted performance for
   refusing-when-unsure is the one whose train and holdout converge.
7. **Off by default until proven, then a mode** — `FFAI_BODY_ONLY=1`, same as our
   opt-in → default-on ladder with byte-identical fallback.

### 1.5 The optimizer's functions — the evaluation half of the pipeline

`suppress_optimizer.rs` is the third leg: `suppressor.rs` is what SHIPS,
`suppress_fit_reference.py` is what FITS, and the optimizer is what JUDGES —
every candidate rule was tried here before any of them reached Rust. It is the
concrete implementation of the gate-ledger discipline in §4, and its functions
map one-to-one onto what our Phase-2 harness must do:

| function | what it does | codec translation |
|---|---|---|
| `evaluate()` → `RuleResult` | Scores a rule on **signed unit-weighted net gain** (`net_gain_if_dropped`: +chars for a true drop, −chars for a false one), never classification accuracy — summing it IS the rule's exact metric-level effect | Score a gate on signed BD/bits effect per routed unit; a 90%-precision rule still loses if the 10% is dense |
| macro vs micro columns (`macro_pp`, `net_gain`) | **Macro** = per-page mean (a page counts once), matching what the benchmark aggregates; **micro** = raw character total. §8.119: the whole campaign had fitted the wrong one — they pick different rules and flip signs | Macro ↔ per-clip mean BD (a clip counts once, the corpus harness's aggregate); micro ↔ total bits. Fit on the one the verdict reports |
| `macro_train` / `macro_hold` + the both-splits gate | Each split scored over its OWN page denominator; a rule whose splits disagree in SIGN is refused whatever its total | Train/holdout segment splits (§5 step 5); sign agreement is the acceptance bar, not the pooled mean |
| `top3_share()` | Share of a rule's gain carried by its 3 biggest pages; ~100% = the rule is a **page list, not a rule** (§8.114: +4,407 chars refuted — 3 pages of 236 carried all of it). NaN on near-cancellation rather than a fake reading | Per-clip concentration guard on any corpus BD win: report top-N share of the net alongside every delta (the Whisper 946% lesson, already in `codec-tune-quality`) |
| `shipped()` + `run_incremental()` | The live filter transcribed; every candidate scored as **shipped OR candidate**, so the printed delta is what the candidate ADDS. Standalone tables double-count what already ships | Score any new gate on top of the existing dispatch stack (B_Skip 4-term gate, SATD dispatch, bframes auto) — a signal that re-routes what's already routed is worth nothing |
| `build_predicates()` / `rec()` / `score_mask()` / `run_search()` | Exhaustive 3–4-variable conjunction search over precomputed bitmasks, one feature per clause, acceptance gates applied INSIDE the search (both splits positive AND macro > floor). Hand-picked rules top out at 2 variables; precision is what a conjunction buys | The Phase-2 threshold search over the P1 signal vector — enumerate, don't guess; and remember the enumerate-the-combination-space law: with few units most of the space "separates perfectly" by chance |
| `run_block_rules()` + block-mode detection | The same objective at a different UNIT (98% of characters sit in ≥90%-pure blocks, and blocks carry features lines can't have); the money population (dense+wide) is the one shape features get backwards | Unit-choice validation per §4: score the same gate at MB/frame/GOP grain and let the table pick |
| `wide_score()` / `longprose_score()` | Additive scorecards as an alternative gate representation, swept over thresholds | A fallback form when no shallow conjunction separates — still transcribed, still judged once |
| `print_best()` sign-flip report | Rules where micro and macro DISAGREE in sign are flagged explicitly — invisible to every earlier search | The per-clip sign-flip table that triggers dispatch (Tim's rule) — make the harness print it, don't grep for it |
| `load_csv()` | Schema-agnostic loader; derives `macro_gain` from `page_chars` when the column predates the export | The harvest CSV contract for P1's signal vector |

Two traps documented in its header, both properties of the INPUT (a clean table
can still be void):

1. **Check what the harvest was already filtered by** — a sweep of a branch on
   a post-branch export moves nothing; the tell is a constant inert across its
   whole grid (§8.117). Codec form: never sweep a gate on a harvest taken with
   that gate (or an upstream gate) active — the harvest-at-decision-time law.
2. **Sum per page before believing a total** (§8.114) — half the residual sat
   on ~17 pages of 316; a large net gain is as likely to be a page list as a
   rule. Codec form: the per-clip table is mandatory in every evaluation.

Note: the optimizer is the only one of the three imports that is
near-standalone (`clap` + `csv` only, no `ffai_core` types) — it can be adapted
to build here, and the Phase-2 harness should be a port of it, not a rewrite.

**First test with the copy:** re-derive its fit pipeline against a codec truth
table (Phase 2 below) — harvest per-MB/per-GOP features, score candidates
through the optimizer's discipline (signed net gain, both-splits gate, top3
concentration, incremental-on-top-of-shipped), fit a depth-≤4 tree with the
same train/judge-once discipline, transcribe it in the suppressor's style
(one documented struct field per feature, one test per branch).

---

## 2. The content-type taxonomy

Axes first (each axis has a cheap O(pixels) signal), then the named classes.

### Axes and their signals

| axis | signal (O(pixels), harvested at decision time) | already exists? |
|---|---|---|
| Texture / detail | source variance (keyframes), log-variance and its **spread** | yes — AQ ramp |
| Coding difficulty | residual variance vs zero-MV reference (NOT source variance) | yes — lookahead |
| Predictability | 1 − temporal decay; motion-compensated residual **vs distance** (2-gap/1-gap ratio) | yes — mb-tree back-off |
| Motion magnitude / coherence | lookahead MV field: mean magnitude, coherence (tex/motion thresholds `tune_lme_*`) | yes |
| Bi-prediction favorability | absolute bi-residual (the bi/uni *ratio* was proven misleading) | yes — bframes auto |
| Busy-ness | busy-pct (`tune_bskip_busy_pct` family) | yes |
| Scene stability | lookahead cost discontinuity (cuts/fades/flashes) | partial — GOP logic |
| Synthetic vs natural | high-frequency energy shape; flat-run length; palette-like histogram concentration | **no — build in P1** |
| Grain / noise | temporal residual floor at zero motion (noise never predicts) | **no — build in P1** |

Rules that bind every signal: harvest at decision time (a tap placed after the
action measures the action); validate against a brute-force oracle or per-clip
truth table before wiring; thresholds re-calibrated on the *deployed* estimator,
not the offline probe.

### Content classes (with corpus exemplars)

| class | exemplar | known behavior |
|---|---|---|
| Smooth synthetic | tsrc-class clips | All-intra we WIN; inter gap is here; mb-tree −1.8% |
| Busy natural | mandelbrot/park-class | B-frames LOSE (+3.6%) without the auto gate; AQ mild loss on synthetic, win on natural |
| Slow pan / zoom, natural texture | `in_to_tree`, `shields` (720p50) | deblock-heavy, sub-pel-sensitive |
| Structured pan (buildings, edges) | `stockholm` (720p59.94) | bS derivation stress; partition-shape sensitive |
| Static / talking-head | (thin in corpus) | P_Skip-dominant; RD-skip −10% on Fast presets |
| Screen content / graphics / text overlay | `screen_text`, `screen_ui` (synthesized 2026-08-06, `video-tests/synth_clips.sh`) | flat_run/hist_top16 separate it 10× from ALL natural content (docs/signals-truth-table.md) |
| Grain / noise | `grain_akiyo`, `grain_flat` (synthesized 2026-08-06) | grain_floor 6.5× over clean static; motion/texture confound needs the joint read; AQ premise broken from BOTH ends (lv_spread 0.13 on flat grain) |
| **YOLO-feed variable scenes** | **harvest from the failing Diana build** | THE driver — day/night shifts, motion bursts, mixed static+busy in one stream |

Corpus law applies: encode-side gates are judged on y4m sources at 4 QPs
per-clip; decode-side on x264-generated streams (our own streams are 100%
full-pel and hide gaps). The failing YOLO clips join the corpus as first-class
classes — corpus-neutral on a feature with a known physical premise is a corpus
gap, so we extend the corpus rather than call the feature done.

---

## 3. The function inventory — every core function and its gate

Status legend: ✅ gated (dispatch built + verified per-class) · 🔶 built but
opt-in / partially gated · ❌ no gate yet (fixed behavior or missing capability).

### Encoder — decision functions (gates trade quality/speed; judged by 4-QP per-clip BD)

| function | unit | current state | gate signal / plan |
|---|---|---|---|
| **Entropy: CABAC vs CAVLC** | stream/slice | 🔶 config choice; CABAC −5..17% vs CAVLC, 1-ref | Profile-capability dispatch (CABAC whenever profile allows) + per-preset speed rung. The real dispatch here is *inside* CABAC: lambda scale, dz_div, RDOQ — audit each constant for content-dependent optima (one-signal-many-knobs law) |
| Trellis RDOQ (`cabac_rdoq`) | frame type | 🔶 default-on all-intra (−0.5..−1.3%) | Extend to inter frames gated by texture axis; sweep to both ends before characterising |
| **B-frames** | GOP | ✅ `--bframes auto` (−19.6% smooth / never regresses busy) | Done — the model gate. Absolute bi-residual signal; per-GOP flip in the structure table |
| **AQ** (`aq_strength`) | MB | 🔶 built + conformant, opt-in (−1.66% natural, mild synthetic loss) | Log-variance-spread ramp → strength; neutral end = OFF (byte-identical). Finish: default-on once worst class ≤ 0 on the extended corpus incl. grain |
| **mb-tree** | GOP → per-MB QP | 🔶 opt-in; predictability back-off (never regresses) | Finish: default-on ladder; predictability signal already the right axis |
| **8x8 transform** | MB | ✅ level-aware rate + penalty dispatch; win on ALL clips | Done — extend the same dispatch to CABAC 8x8 when it lands |
| **RD P_Skip** | MB | 🔶 adaptive, opt-in; −10% on Fast, ~+1% with sub-pel (SUBSTITUTES) | Preset-aware gate: enable exactly where sub-pel is off. The substitute relationship IS the dispatch axis |
| **Cost metric: SAD vs SATD** | decision site | ✅ dispatched (−4.3%) | Done for integer-pel mode selection; REFUSED for sub-pel (proxy structurally wrong there — recorded) |
| ME: wide search (`me_wide`) | frame | ✅ online frame-level gate (first-N-units measurement) | Done — pattern for any tool feeding a downstream global consumer |
| ME: snap / subpel iters / greedy skip (`tune_me_snap`, `tune_me_subpel_iter`, `tune_greedy_skip*`) | MB / preset | 🔶 tuned constants + partial gates | Audit each for content-dependent optima; map onto the motion-coherence signal family |
| Lookahead ME thresholds (`tune_lme_hi/tex/motion`) | MB | 🔶 fixed thresholds | Convert to per-frame percentiles (population-shaping law) |
| **Partitions 16x8/8x16** | MB | ✅ default-on | Done |
| **Sub-8x8 partitions** | MB | ❌ MISSING (encoder-only; decoder ready; ~4–12% of inter gap) | This is a **missing capability, not a threshold** — no gate can route to an arm that doesn't exist. Build the arm, then gate by texture/motion axis |
| Rate control / QP offsets (`i_qp_offset`, `bframe_qp_offset`) | frame | ❌ fixed constants | Audit under the same law; bit-redistribution knobs need a rate-neutralizer before their BD is trustworthy |

### Decoder — throughput functions (bit-exact by law; gates are pure speed, judged by |z|>2 paired CPU time)

Decode-side "content dispatch" is mostly **by construction** — the bitstream
tells us what the content is. The remaining gates are capability×population:
a population of streams served by a slow path is a missing kernel.

| function | current dispatch | gap / plan |
|---|---|---|
| **CABAC engine** (fused-low, FUSED table) | ✅ per-slice by stream | Serial-dependency work banked; bin-count reduction refuted at engine level |
| **CAVLC** | ❌ no threading seam at all | **The worst 2T ratio (3.53×) is this population unserved.** Build the E-seam for the CAVLC slice loop (flush hooks exist only in CABAC) |
| **EDC parse/recon overlap (E1–E3)** | ✅ default-on, CABAC P+B slices | Extend: CAVLC (above); then **frame-level threading** on the PixelCtx foundation — the ~1.9×/core tier |
| **Deblock bS derivation** | ✅ two-list AVX2 kernel dispatched on `l1_used`; packed pipeline; derive-at-decode | Done — the capability×population case study (kernel was missing for the two-list population) |
| Deblock filter | ✅ row-interleaved (R1–R3); branchy/branchless knob | Branchy-vs-branchless is content-shaped (edge density) — candidate for a stream-stat gate if |z| ever clears 2 |
| **MC luma/chroma** | ✅ full-pel vs sub-pel paths per MV (by construction); pixel_avg accel with true geometry | Sub-8x8 decode path READY (waiting on encoder) |
| Dequant / transform | ✅ AVX dispatch; 4x4/8x8 per stream flags | Done — note the AVX2 dequant is a measured NULL (memory-bound, opt-in off); keep, don't dispatch |
| Intra prediction | ❌ single path | Uniform-gap finding says no localized prize; revisit only with new census |

### 3.5 The exhaustive census (2026-08-06 sweep) — what the first inventory missed

Two full-crate sweeps (encoder; common/decoder/accel/CLI) enumerated ~150
decision sites. The first inventory above captured the *named features*; the
census found the unnamed ones. Full agent reports are in the session record;
this section keeps what changes the plan.

#### New gate targets, ranked

| # | site | what's wrong | gate to build |
|---|---|---|---|
| 1 | decoder `mb16.rs:1010` `threaded =` | MT engages by env × slice-type only — a QCIF P slice and a 4K P slice take the same decision; channel depth fixed at 256 | Gate on frame area (`mb_w*mb_h`), slice MB count, `available_parallelism()` |
| 2 | **scene-cut detection: DOES NOT EXIST** | IDR placement is purely `frame_index % gop_size` (`lib.rs:452`); no lookahead statistic feeds frame-type | Lookahead cost-discontinuity keyframe/mini-GOP decision — the scene-stability axis has NO consumer today |
| 3 | `cabac_init_idc = 0` (`config.rs:432`) | Three init tables exist; doc admits "best table is content-dependent"; one is always chosen. Free per-slice syntax element | Pick per slice/GOP from frame statistics — textbook missing dispatch, zero bitstream cost |
| 4 | split gate = f(QP) only (`mb16.rs:5005`) | The openh264 QP-formula split gate ignores content; the λ-normalized fix (`split_t`, calibrated T=400/600 in its own doc) ships **default 0 = off** | Enable + per-frame percentile the threshold |
| 5 | `hpel_pad = 32` (`inter.rs:657`) | Own doc: payoff "tracks edge-overhang density — fast pans benefit most" — a NAMED content signal shipping as a constant | Pad from previous frame's MV distribution + frame size |
| 6 | `tune_lambda_scale` / `tune_intra_penalty` (`config.rs:408-409`) | Both docs literally name "a content-adaptive dispatcher can vary it per frame" — never built | λ-family dispatch off the texture/motion signals (same family as `me_lambda_scale`, which IS built) |
| 7 | `tune_lme_tex_thresh` texture arm (`config.rs:435`) | Machinery built, disabled: one signal "does NOT clear the monotone bar"; doc names the fix (a second motion-flavoured term) | Two-term gate; sign-flip evidence already tabulated in the doc comment |
| 8 | sub-8×8 scalar fall-through (`satd_avg.rs:342` x4_shape; `inter.rs:402/421/442/482` width gates) | Every kernel table stops at 8×8 — sub-8×8-heavy content pays scalar across the whole MC/cost stack | Missing-kernel-not-threshold: build 4-wide kernels BEFORE enabling encoder sub-8x8, or its cost will be double-charged |
| 9 | decoder `LPAD=16/CPAD=8` (`lib.rs:140`) | Fixed pad decides padded-vs-clamped MC fallback; MVs >~14px take the slow halo path | Max-|MV|-per-picture is known at parse; size pad per stream |
| 10 | packed deblock pass built frame-wide unconditionally (`deblock.rs:1693`) | On an all-intra picture every packed record is waste; `info.kind` already knows the intra fraction | Intra-fraction veto on `pack_frame_into` |
| 11 | EDC deferral always-on for P (`mb16.rs:5895`) | On mostly-Skip P slices the job queue is near-empty — pure overhead | Skip-rate / coded-MB density of previous picture |
| 12 | RC constants (`rc.rs`: QCOMP=0.6, ±18 clamp, ±6 swing, EMA 0.5) | Entire rate-control personality is fixed magic | Audit under the sweep-to-both-ends law once RC matters for Diana |

#### Already-adaptive exemplars the census surfaced (the models to copy)

The B_Skip RD gate (`mb16.rs:8583`) is the richest in-tree gate: a 4-term
dispatch (direction census × busy-pct × direct-win rate × λ-priced distortion)
with online learning. The me_wide rescue has online payoff learning
(disable-if-not-paying, `mb16.rs:2556`). The SATD/SAD choice is per-MB by
per-frame variance percentile. These are the house style for Phase 2 fits.
Caveat found: **every online dispatcher restarts per frame** (GOP-parallel
determinism), so the first `~mb_w·mb_h/8` MBs of every frame run un-dispatched
— a standing cost the frame-level threading campaign must preserve, not break.

#### Dead dials and unwired capability (prune or wire, never leave ambient)

- `tune_rd_skip` machinery + its dispatch signal: **off by default and no
  preset enables it** — the largest dead dial in the encoder.
- `RS_H264_BS_PRE`: only reachable when `rowdb_on()` is false — dead in the
  default configuration.
- `set_precomputed_bs` (encoder-side bS handoff): `#[doc(hidden)]`, zero
  callers.
- Padded-MC family (`inter.rs:1240-1259`): implemented, bit-exact, unwired
  ("~0 on x86-64; kept as a ready option").
- Decoder intra prediction is fully scalar while `i16x16_luma_pred` /
  `chroma8x8_pred` asm exists and is wired only from the encoder.
- Three dead cost constants (`SKIP_RATE_BITS`, `SPLIT_GATE_BITS`,
  `FAST_INTRA_PENALTY_BITS`) whose live twins are inline literals.
- U1 sub-pel online dispatcher: built, measured, refuted, default-off —
  correctly recorded; candidate for deletion.

#### Hygiene batch (fix BEFORE any Phase-1/2 fitting — these poison measurements)

1. **Doc/code default drift on decision knobs** (5 found): `tune_lme_hi` (doc
   None vs code `Some(1.6)`), `tune_lme_motion_thresh` (config 20.0 vs code
   fallback 26.0, doc says both), `inter8_pen` (comment 0 vs code 8),
   `bframe_qp_offset` (doc 2 vs code 3), `adaptive_bcount` ratios (doc 1.8/1.4
   vs code 1.4/1.3). A fit against documented defaults would fit the wrong
   encoder.
2. **CLI cannot select Balanced** (`main.rs:98-102` accepts only fast/quality)
   while `EncoderConfig::new` defaults to Fast and the Preset doc claims
   Balanced is the default — three sources disagree; the documented default is
   unreachable.
3. **`RUSTY_THREADS` parsed two ways** (CLI: `=="1"` boolean; encoder lib:
   numeric count) and the CLI's per-GOP thread scope is unbounded by core count.
4. Four accel entry points call `is_x86_feature_detected!` **per invocation**
   instead of the cached flag — one (`mb_uniform`) runs 6.19M times/corpus.
5. Deblock SSSE3 kernels are never runtime-detected (asm feature implies them);
   `library gop_size` default is 1 (all-intra) vs CLI 30.
6. λ has **three inconsistent forms** (mode: `0.85·s·2^((qp-12)/3)`; ME:
   `√λ` with `lme_scale` on CABAC but NOT CAVLC; RDOQ: no 0.85) and the mvd
   rate model is duplicated with different math in the P and B paths — unify
   before fitting any λ-family gate, or the fit learns the inconsistency.
7. Duplicated probe skeletons (`b2_mgain` / `me_wide_headroom` /
   `global_mc_residual`×2): consolidate into ONE per-frame signal vector
   (Phase 1's deliverable) that all gates read.

---

## 4. The Great Gate architecture — one shape for every gate

Every gate in this codebase gets written to one canonical form (the suppressor's
form), recorded in a **gate ledger**:

```
GATE := (unit, signal, threshold-form, arms, fallback, cost, ledger-entry)
```

- **unit** — chosen where the signal is STABLE and the objective is COMPLETE
  (per-MB / per-frame / per-GOP / per-clip; per-unit RD is blind to propagated
  payoffs — pick the grain where the whole payoff is visible).
- **signal** — O(pixels), harvested at decision time, validated against a
  per-class truth table BEFORE wiring. Instrument ≥3 candidates; the winner is
  the one that predicts the BD *verdict*, not "activity."
- **threshold-form** — per-frame percentile (default), integral time-budget
  controller (quality-tier defaults), or single-sided latch on a wide natural
  gap (per-clip constants; mid-stream flips desync adapted contexts).
- **arms** — the routed algorithm reuses the proven leaf/emit machinery verbatim;
  force-on-everywhere must nearly tie the anchor on 4-QP BD before a dispatch is
  built on it (a big force-on gap predicts a dominated dispatch).
- **fallback** — the neutral end is **byte-identical OFF**, proven with `cmp`,
  not BD ≈ 0.
- **cost** — ⚠ **THE DUAL VERDICT. A gate is a POINT ON A PARETO CURVE, and a
  verdict that reports only one axis is not a verdict.** Every entry carries
  both:
  1. **A deterministic WORK COUNT** — the expensive operations the gate causes
     or removes (`gate_work()`: `best_part` searches, `mb_plan` macroblock
     plans, per coded MB). Counts need no pinning, no ABBA, no z-score; one
     run is the verdict, and the ratio is exact on a box that cannot hold
     still. **This comes FIRST** (counter-before-clock, `codec-measurement`
     §15) — it also SIZES the effect, so an unmeasurable clock delta becomes
     "0.5%, correctly unmeasurable" instead of an argument.
  2. **Milliseconds**, via `bench/pinvs.ps1` — the ONE compliant timing
     harness (pinned, High priority, CPU time via a cached `$p.Handle`, arms
     ABBA-alternated, paired win-rate + z). Never a stopwatch bolted into the
     BD loop: deterministic quantities (bytes, BD, counts) and timed ones must
     not share a loop, or the timing collapses to one un-interleaved sample
     per point (`codec-measurement` §13).
- **ledger-entry** — per-class truth table **over the FULL corpus** (a table
  that omits a resolution class is a claim about the clips you ran, not about
  the corpus — the VP9 partition-gate error: "better on every clip" meaning
  "on these four"), fitted thresholds' provenance (train split, judged once on
  holdout), each branch's precision and bounded downside, the dual-verdict
  cost above, and provisional branches labeled provisional.

Representation rule: gates ship as **transcribed shallow trees (depth ≤ 4)** in
plain code — one documented field per feature stating its physical meaning and
importance, one unit test per branch. No model artifacts, no runtime fitting.

---

## 5. The YOLO-downstream loop (why now, and how we close it)

### First finding (2026-08-06): the failure was a SHIPPED PIN, not a content gap

Diana's report ("CABAC streams fail to decode", content-scaled 7–30% frame
survival) was reproduced and root-caused in one session: `rff-codec-h264` 0.1.0
pins `rusty_h264 = "0.2"`, which cargo's 0.x semver resolves to **0.2.1** —
pre-CABAC-conformance, two campaigns old. Bumping the pin to `"0.8"` (one line;
API drop-in compatible) takes every clip in the report to full-length decode,
**byte-identical to ffmpeg per-frame MD5** on all six entropy-ladder clips plus
the High-profile probes. Republish of `rff-codec-h264` pending.

Two laws this bakes into the plan:

- **A checkmark in the repo is not a checkmark in the shipped population.** The
  gate ledger must record *which published version* each capability landed in,
  and downstream pins are part of the population being served. A wrapper crate
  pinned to 0.x can NEVER pick up a 0.(x+1) fix on its own.
- **Content-dependence in a failure report does not imply a content-adaptive
  fix.** The report's clean entropy scaling (more bits/frame → earlier desync)
  was version skew wearing a content costume. Localize (reproduce on the current
  engine, rule out the integration layer) BEFORE fitting any gate to a
  downstream symptom — the untouched-signal law applied to bug reports.

### The loop

The failure report is "downstream failures at variable content." Before any gate
is tuned against it, the failure must be localized — a signal that didn't move
localizes nothing until proven it could have moved:

0. **Shipped-version parity first**: confirm the downstream build resolves the
   CURRENT engine (lockfile check), and that the published wrapper decodes the
   x264 corpus like the repo does. (This step found and fixed the first bug.)

1. **Harvest the failing clips** from the Diana build (FFai) — the exact streams
   where detection degrades, plus their upstream sources if available.
2. **Rule out decode**: our decoder is bit-exact vs ffmpeg on the corpus; run the
   failing streams through the conformance harness first. If decode differs,
   that's a bug, not a gate.
3. **Localize to encode decisions**: per-clip 4-QP BD (rate/SSIM) on the failing
   content vs x264 at matched features; segment the clips by scene and BD per
   segment — *variable* content means the truth table lives at segment grain.
4. **Extend the corpus** with the failing classes + synthesized screen-content
   and grain clips (corpus-gap law).
5. **Fit the missing gates** (Phase 2) on train segments, judge once on holdout.
6. **Exit criterion**: per-class BD table over the extended corpus with **no
   sign-flips and worst class ≤ 0** for every default-on feature — and the Diana
   build re-run on the re-encoded streams as the end-to-end confirmation.

---

## 6. Phases

- **P0 — Import + corpus (this session).** Suppressor copied to `_greatgate/`
  (done, gitignored). Harvest YOLO failing clips; synthesize screen-content and
  grain classes; wire them into the 4-QP per-clip BD harness.
- **P0.9 — Hygiene batch (from the census, §3.5).** Fix the five doc/code
  default drifts, the CLI Balanced gap, the RUSTY_THREADS split-brain, the
  uncached feature detections, and unify the three λ forms — BEFORE any
  fitting, because fits against a mis-documented encoder fit the wrong encoder.
- **P1 — Signal audit.** Consolidate the duplicated probe skeletons
  (`b2_mgain`, `me_wide_headroom`, `global_mc_residual`×2) into ONE per-frame
  signal vector all gates read. Build the two missing axis signals
  (synthetic-vs-natural, grain floor). Instrument every candidate signal
  against per-class truth tables (force each feature on per clip/segment, BD
  each). Convert fixed `tune_lme_*` thresholds to per-frame percentiles.

  **P1 status (2026-08-06) — landed except the truth-table runs:**
  - `encoder/src/signals.rs`: lazy, memoized `FrameSignals` built once per
    slice; all five drivers read through it. Kills the CABAC-P double
    `global_mc_residual` and shares one per-MB variance walk across the SATD
    percentile, AQ, and the lme texture term. **Gate: 18-arm byte-identity**
    (3 clips × {fast, B, quality} × {CAVLC, CABAC}) vs the pre-P1 binary —
    18/18 match; full encoder suite green (against a clean decoder — see note).
  - New axis signals (harvest-only, no consumer): `flat_run` + `hist_top16`
    (synthetic), `grain_floor` (zero-MV p25 residual floor), plus `lv_spread`
    (the AQ back-off stat, now exposed). **VALIDATED 2026-08-06** on the full
    24-clip harvest (20 corpus + 4 synthesized gap-class clips): both synthetic
    signals separate outright (flat_run 10×); grain_floor separates its class
    6.5× with a documented motion/texture confound for the P2 conjunction
    search. Full table + verdicts: **docs/signals-truth-table.md**. Gap
    classes synthesized via `video-tests/synth_clips.sh`.
  - Harvest tap `RFF_SIGNALS_CSV=<path>`: one row per slice, full vector +
    gate decisions, proven byte-inert when on. Feeds the P2 `gate_optimizer`.
  - `tune_lme_q` / `RFF_LME_Q` (opt-in, default None = byte-identical): the
    per-MB percentile form of the lme texture veto, P slices, CABAC path.
    Liveness proven (mobile bytes change at q=0.3). **BD-gate pending — P2.**
  - **Hygiene finding (P0.9 addendum, found by the gate itself):** the CLI
    defaulted `cabac=false`, overriding the library's CABAC-on default — the
    exact trap the rate-allocation skill records; every CLI benchmark without
    `--cabac 1` measured CAVLC. **FIXED 2026-08-06**: absent flag now follows
    the library default; `--cabac 0` / `RUSTY_H264_LEGACY_CAVLC=1` restores
    the old bytes exactly (verified); new default output == the validated
    `--cabac 1` bytes and decodes pixel-exact in ffmpeg. ⚠ Standing benchmark
    implication: CLI numbers taken before this date measured CAVLC unless they
    passed `--cabac 1`.
  - The 8 encoder round-trip failures against the working-tree decoder were a
    LATENT DEFECT the E2 threading exposed, not an E2 bug: the
    `#[cfg(not(accel))]` chroma deblock fallback never learned to read the
    precomputed bS store — it live-derived from the syntax grids on every call,
    which panics when the caller's view carries none (the E2 worker's
    `PixelCtx`, by design). **FIXED 2026-08-06** (`common/deblock.rs`): the
    fallback now sources co-located luma strengths from `bs_v`/`bs_h` under
    `have_bs`, mirroring the accel arm. Full suites green both with and
    without `--features asm`.
- **P2 — Gate fits.** First deliverable: port `suppress_optimizer.rs` to a
  codec rule-search harness (`gate_optimizer`) reading the P1 signal-vector
  harvest — same objective columns (signed net gain, macro = per-clip mean,
  both-splits gate, top3 concentration, incremental-on-top-of-shipped,
  conjunction search); it is near-standalone (`clap`+`csv`) so this is a port,
  not a rewrite (§1.5). Then for each 🔶 row: score candidates through it, fit
  the depth-≤4 tree on train, judge once on holdout, transcribe
  suppressor-style, ledger entry. Priority order by downstream impact: AQ
  default-on, mb-tree default-on, RD-skip preset gate, RDOQ inter extension.

  **P2 status — CLOSED 2026-08-06 (Diana-dependent items deferred).** Full
  entries in docs/gate-ledger.md; one-line verdicts:
  - `gate_optimizer` PORTED (bench/examples/, dependency-free; per-class
    worst-column, both-splits gate, top3 guard, combination-space
    calibration); `bdrate` extended (aq/mbtree/rdoqp/rdoqb params, RESULT line).
  - **AQ grain veto SHIPPED default-on** (`aq-grain-veto`, provisional n=1):
    grain +29.45% BD-SSIM → 0 on covered frames, 21/22 clips byte-identical,
    residual ∝ first-GOP share (fails open; streaming==batch invariant kept).
  - **mb-tree back-off RE-FITTED and SHIPPED** (`mbtree-backoff-refit`): the
    old ramp was inverted against real content — throttling akiyo_qcif
    (−5.09→−9.24 unlocked) and screen_text (−4.53→−7.06) while still leaking
    tsrc +0.50. New form: single-sided latch at rf 0.03 (tsrc → 0.00 exact,
    17 clips byte-identical, calibration classes re-synthesized via
    `video-tests/synth_clips.sh` recipes).
  - **mb-tree dispatch remains OPEN** (`mbtree-dispatch`): two named losers —
    pan-struct (stockholm +3.10, needs the temporal predictability signal) and
    grain (+4.41, UNMASKED by the AQ fix; candidate = the aq-veto conjunction
    at GOP grain inside `gop_qp_offsets`). Spatial-proxy candidates refused.
  - **RD-skip preset gate RECLASSIFIED P3** (`rdskip-preset-gate`): the
    machinery exists only in the CAVLC driver — a dead knob under the CABAC
    default (all-zero matrix, the inert-grid tell). Build the CABAC arm first.
  - **RDOQ inter: arm BUILT, measured WASH, kept opt-in** (`rdoq-inter`):
    accel trellis fork + `cabac_rdoq_p`/`cabac_rdoq_b`; ±0.2% washes, P-slice
    losses per the reference-structure law — no win to gate.
  - Deferred (Diana): grain-veto re-fit on real footage; YOLO corpus classes.
- **P3 — Missing arms.** Sub-8x8 encoder search (the one gap no gate can route
  around); CAVLC E-seam on the decode side. Both are capability×population
  items: build the arm, then gate it.

  **P3 catalog (2026-08-06) — five items, recon-grounded, in execution order.**

  1. ✅ **mb-tree grain veto at GOP grain** — SHIPPED 2026-08-06 (ledger
     `mbtree-grain-veto`): the aq-veto conjunction on the GOP's first source
     pair in `gop_qp_offsets`; grain +4.41 → 0.0000 exact, 5 non-grain
     clips numerically identical, suites green, `RFF_MBTREE_GRAIN=0` escape.
  2. ✅ **CABAC RD-skip port** — BUILT + FIT RUN 2026-08-06, verdict
     PRUNE-to-opt-in (ledger `rdskip-preset-gate`): threshold arm
     (`SSD(skip) ≤ T·λ` + free-skip census — the B_Skip form; correction: the
     B gate is census+threshold, not state-snapshot trials) is live and
     conformant, but worst class ≤ 0 fails at every swept T (in_to_tree
     +1.56, screen_text +2.92 SSIM) and the CAVLC-era −10% prize is absent
     under the CABAC-default stack. Stays opt-in; full J-compare = doubtful
     future brick.
  3. ✅ **Sub-8×8 encoder search** — ARM BUILT + CONFORMANT 2026-08-06
     (ledger `sub8x8-split`): plan/emit/search live on the CABAC quality path
     (single-ref), 6/6 pixel-exact ffmpeg at QP extremes, off = byte-identical.
     The loser column turned out to be a MISPRICED SEARCH, not content:
     `best_part` prices by SATD (prediction error, always falls as partitions
     get finer) where the decision needs quantized-recon SSE. Re-pricing
     (`tune_sub8_rd`) improved EVERY clip — 7W/13L/3N → 10W/2L/7N — and the
     two remaining losers were grain, closed by the existing
     `grain_signature()` (now ONE definition, three consumers: AQ, mb-tree,
     sub-8x8). **Final: 10 wins, 9 neutral, 0 losers.** Still opt-in: the RD
     arm costs 2 extra MB plans per split candidate. ⚠ The same SATD proxy
     prices the DEFAULT-ON 16x8/8x16 and intra-vs-inter decisions — next probe.
     Original step list preserved below.
     Recon: encoder P_8x8 emits ONLY `sub_mb_type 0` (`cb_sub_mb_type_p` has
     `unreachable!` for 1–3); decoder parses all types, `decode_p8x8` ready.
     Steps, strictly ordered:
     a. **Kernels first** (census #8): the kernel tables stop at 8×8
        (`satd_avg.rs` x4_shape, `inter.rs` width gates) — build/wire 4-wide
        MC+cost kernels BEFORE the search or its cost is double-charged and
        the Pareto lies.
     b. **Emission**: CABAC `cb_sub_mb_type_p` types 1–3 (ctx base 21,
        mirroring `parse_sub_mb_type_p_cabac`), CAVLC `sub_mb_type` ue path,
        per-sub-partition mvd/prediction exactly as the decoder derives them.
     c. **Search**: 8×4/4×8/4×4 arms inside the existing P_8x8 RD trial,
        reusing `best_part`; charge REAL `sub_mb_type` bits (the
        uncharged-syntax lesson).
     d. **Gates**: env-driven mechanism first, bit-exact round-trip across QP
        extremes; conformance MATRIX (every preset × CAVLC/CABAC — the A×B
        interaction lesson) + strict ffmpeg decode; 4-QP per-clip BD; then a
        content gate via gate_optimizer only if per-clip signs flip.
  4. **mb-tree pan loser** (stockholm +3.10). Add the temporal predictability
     axis to the P1 signal vector (2-gap/1-gap motion-compensated residual
     ratio — machinery in `adaptive_bcount`), re-harvest, gate_optimizer fit
     at GOP grain, transcribe if it clears both splits + worst class ≤ 0.
  5. **CAVLC E-seam** (decoder; the 3.53× 2T population unserved). Add flush
     hooks to the CAVLC slice loop and route through the existing
     `EdcJob`/worker (E1–E3). **⚠ WAIT-ON-E2**: sits directly on the
     uncommitted E2/E3 threading work in `decoder/mb16.rs` — land that first.
     Gates: 160/160 conformance + byte-identical x264-corpus decode + |z|>2
     paired CPU time on separate physical cores.

  Items 1–4 are encoder-only and proceed in parallel with the in-flight
  decoder work. Housekeeping pending a call: commit-split of the P1/P2 bricks
  (currently mixed with the E2 decoder changes in the working tree).
- **P4 — The gate ledger + regression harness.** ✅ BUILT 2026-08-06:
  `bench/examples/gatecheck.rs` (tiers 0-2 deterministic in-binary, tier 3 =
  `bdrate` + `pinvs.ps1`), gate fire-rate census + work counters exported from
  the encoder, staleness guard. See docs/gate-ledger.md "THE HARNESS".
  Remaining: wire `--check` into CI and widen the tracked clip set. One document, every gate, its
  per-class table regenerated by a single harness run; CI gate = no sign-flip
  appears on any tracked class. Frame-level decode threading proceeds as its own
  campaign on the PixelCtx foundation.

**Measurement law (binding on every phase)** — from `codec-measurement`: pinned
CPU time (this box runs at 100%), ABBA interleaved arms, work-count parity
(count what the clock is charged for), |z|>2 to bank speed, 4-QP per-clip BD to
bank quality, 160/160 conformance + byte-identical x264-corpus decode for any
decoder change, exe-mtime printed at every gate, separate *physical* cores for
any MT arm, and the null arm before believing any win.

**★ COST IS A DISPATCH AXIS TOO (added 2026-08-06).** The Great Gate dispatches
QUALITY by content. The best_part campaign found the same machinery works on
COST — an expensive tool can be gated by an online per-frame payoff census
rather than switched off wholesale (already shipped twice in-tree: me_wide's
payoff learner, the free-skip census gating RD-skip). Two hard constraints,
both paid for:

- **Weight the census by VALUE, never by a COUNT of decisions.** A frame where
  a tenth of the candidates survive can carry a large win if those few are
  worth a lot (the suppressor's cardinal lesson, re-learned).
- **A local census cannot gate a PROPAGATING payoff.** Sub-8x8 earns -2.43%
  BD-SSIM on crowd_run while its mean local RD saving is under 2 lambda,
  because the value is in the reconstruction every later frame predicts from.
  Both census objectives destroyed that win (retaining 22% and 4.6%). Before
  building a cost census, ask whether the tool's payoff is LOCAL — if it feeds
  the reference chain, the census is measuring the wrong thing and the speed
  must come from making the work cheaper, not from skipping it.

**★ STRUCTURAL SIGNALS BEAT CONTENT SIGNALS WHEN BOTH APPLY (added 2026-08-06).**
The two dispatches that shipped from the best_part campaign key on SHAPE
(`rw < 8 || rh < 8`), not content: zero cost to evaluate, no corpus fit, no
holdout, nothing to re-calibrate when an estimator moves, and they cannot
sign-flip on unseen content. Five content-signal probes failed on the same
decision. When a structural signal is available, prefer it — and note that the
harvest tap (`diastats`) that revealed it is the ME-stage analogue of the P1
signal vector: a per-rung hit-rate table IS a ceiling sweep.

**★ THE DUAL VERDICT (binding, added 2026-08-06).** The Great Gate is the
show-stopper optimization tool for this codec, so *every* verdict it issues —
gate, feature, or default flip — reports **quality AND cost, on the FULL
corpus, in the same pass**:

| axis | instrument | order |
|---|---|---|
| quality | 4-QP per-clip BD (PSNR + SSIM), every class | — |
| cost, deterministic | `gate_work()` counts: `best_part`, `mb_plan` per coded MB | **first** |
| cost, wall | `bench/pinvs.ps1` (pinned CPU, ABBA, paired z) | confirms |

Rules this makes non-negotiable:
- **A one-axis verdict is not a verdict.** "10 wins, 0 losers" with no cost
  column is an unfinished measurement, not a result. (Paid for on 2026-08-06:
  sub-8x8's quality table was reported before a single timing existed, and on
  a corpus missing every HD clip.)
- **Never default-on an unmeasured cost.** `codec-measurement` §18 — a trellis
  quantizer shipped default-on at a published "+3.1% encode time" and cost
  **+144%** on real footage, because the cost was validated on the wrong
  corpus while the quality number got all the scrutiny.
- **Counts before clocks, always.** The count is exact on this box; the clock
  is not. A count also PRICES the change (`removed_work x per-unit cost`),
  which is how a sub-noise effect gets banked honestly instead of argued.
- **The corpus is the whole corpus.** Trimming a ladder to save time makes the
  resulting table a statement about the subset. If a class is missing, say so
  in the entry before anyone reads a verdict into it.
