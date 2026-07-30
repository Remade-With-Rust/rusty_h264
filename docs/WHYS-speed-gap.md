# WHYS — why is our encoder slower than x264 (and worse at the same time)?

Six-whys descent on the **speed** axis. The sibling `WHYS-inter-gap.md` descends on
the rate axis. One entry per level; siblings are separate causes, not alternatives.
Refuted hypotheses stay with their number so they are not re-litigated.

Depth 6 is written last (Ohno's order) but was **run first** — and, as usual, it
moved the headline before any code was read.

---

## S1 — is the gap real, and on which axis?

- **ASKED:** "we are slower AND worse" — against *which* x264 operating point?
  A speed number and a quality number taken at different reference presets cannot
  be paired (the codec-analyzer iso-speed law).
- **MEASURED:** the full 10-rung x264 ladder vs our 3 presets, matched QP 26 /
  keyint, both single-threaded, PSNR by the same external ffmpeg. Read at **matched
  PSNR** (iso-quality), not matched QP:

  | clip | our point | x264 at ~same PSNR | speed | size |
  |---|---|---|---|---|
  | foreman_cif | fast, 38.198 dB | faster, 38.192 dB | 70.4 vs 47.7 Mpx/s (**we win 1.5×**) | **2.58× bigger** |
  | akiyo_qcif | fast, 40.739 dB | slow, 40.752 dB | 223 vs 36.6 (**we win 6×**) | **1.88× bigger** |
  | mobile_cif | fast, 35.405 dB | superfast, 35.427 dB | 63.6 vs 125.4 (**we lose 2×**) | **1.79× bigger** |
  | mobile_cif | quality, 36.170 dB | slow, 36.147 dB | 3.85 vs 5.02 (**we lose 1.3×**) | **1.26× bigger** |

- **ANSWER:** "slower AND worse" is **only true at the quality end**. On the fast
  preset we are usually *faster* than the iso-quality x264 rung but 1.9–2.6× the
  bitrate. On detail content (mobile) we are **dominated on both axes**.
- **The sharpest finding:** our **quality preset is a bad Pareto point**. On mobile,
  `balanced → quality` costs **6× the time to buy 2.1% rate**; x264's
  `medium → veryslow` costs 5.75× to buy **6.4%**. We pay ~3× more time per unit of
  rate at the slow end.
- **CONFIDENCE:** high for the ranking; the size ratios remain single-QP and are
  confounded (see `WHYS-inter-gap.md` D1). Iso-quality *rows* are sound because they
  compare at matched measured PSNR.
- **STATUS:** closed. The speed question is therefore: **where does the quality
  preset's time go, and why does it buy so little?**

## S2 — which stage owns it, in ABSOLUTE cost?

- **MEASURED:** corpus stage profile, our three presets vs x264's ladder.
  - ours/quality TOTAL **266,587 ms**; `enc-me(best_part)` **244,121 ms = 91.6%**.
  - ours/fast TOTAL 14,319 ms; no stage above 23%, and `mgmt/other` (2201 ms)
    ≈ `profiler-overhead(est)` (1706 ms) → residue is the instrument, not hidden
    work. **The fast preset is clean; do not hunt there.**
- **ANSWER:** the quality preset is ME-bound to the exclusion of everything else.
- **NOTE:** x264's own dump sums to 109% (`mb-encode` nests inside `mb-analyse`) —
  only like-for-like stages are comparable; do not difference its percentages.
- **STATUS:** closed.

## S3 — is it a RATE problem or a COUNT problem?

- **MEASURED:** µs/unit × units, both sides.
  - ours/quality: 23.67 M `enc-me` calls over 3.26 M MBs = **7.25 ME calls/MB** at
    **10,313 ns each**.
  - x264/medium baseline: 3.89 M `mb-analyse` calls = **1 per MB** at **7,149 ns**
    for its *entire* analysis.
  - Per macroblock we spend ~63 µs in ME alone against x264's ~7 µs for everything.
- **ANSWER:** **both**, but the count is where we are structurally different — and
  the count multiplies a primitive that is itself too expensive (S4).
- **STATUS:** closed.

## S4 — which primitive inside ME? *(the level that paid)*

- **MEASURED:** nested primitive breakdown.

  | preset | `inter-mc` calls | ms | ns/call | calls/MB |
  |---|---|---|---|---|
  | fast | 15.3 M | 612 | **39.9** | 4.8 |
  | balanced | 94.3 M | 22,281 | **236.2** | 29.0 |
  | quality | **991.5 M** | **166,291** | **167.7** | **303.8** |

- **ANSWER:** `inter-mc` is **166 s of the 266 s quality encode (62%)** — the single
  largest primitive in the encoder. The same function is **4–6× more expensive per
  call** in balanced/quality than in fast, which is not a workload difference.
- **REFERENCE CONTRAST:** x264 pre-filters half-pel planes **once per frame**
  (`hpel-filter`: **188 ms for the whole corpus**) and then sub-pel search is a plane
  read. We re-run the 6-tap filter **per candidate**. ~885× more work for the same
  information.
- **STATUS:** closed.

## S5 — why is a call expensive? *(mechanism)*

- **ASKED:** is it the filter, or per-call overhead?
- **PROBE:** block-size × sub-pel-phase sweep (`examples/mc_ceiling.rs`), interleaved
  round-robin, median of 9. Real work scales with `bw*bh`, so a 4×4 should cost
  ~1/16 of a 16×16.
- **MEASURED (before):** a **4×4 sub-pel call costs MORE than a 16×16** — 247.9 vs
  172.1 ns (qpel), i.e. **15.5 ns/pixel vs 0.67, a 23× per-pixel penalty**.
  Subtracting the full-pel arm isolates tile+subpel at ~138 ns (4×4) vs ~113 ns
  (16×16) — **near-constant**, so the cost is per-call, not per-pixel.
- **ANSWER — the mechanism:** every sub-pel `mc_luma` call created, and therefore
  **zero-initialised**, `luma_tile`'s `[u8; 441]` (which it then returned **by
  value**) plus `mc_luma_subpel`'s two `[u8; 256]` scratch buffers — **≈1.4 KB of
  memset + copy per call, identical whether the block is 16×16 or 4×4**. At 991 M
  calls that is ~700 GB of pointless memory traffic.
- **CONFIDENCE:** high — the block-size invariance is the signature, and the fix
  moved exactly the arms it should.
- **STATUS:** closed → rebuilt (below).

## S6 — is the measurement sound? *(run FIRST; it invalidated two arms)*

Three separate instrument failures, each of which produced a confident wrong number:

1. **My own ceiling probe was built without `asm`.** `asm` is not a default feature;
   the analyzer builds `features = ["asm"]`. The first probe run measured a
   pure-scalar path the encoder never executes — it read 762 ns for `16x16 halfH`
   against an in-context average of 167.7, and ranked `halfH` *above* `halfHV`
   (backwards). Rebuilt with `--features asm`: the microbench and the in-context
   number then agreed, and the ranking righted itself.
   **Rule: a probe must be built with the deployment feature set; check it before
   believing a disagreement.**
2. **The wall-clock harness could not resolve the effect.** A **null arm**
   (`base` vs `base`, identical binaries) read a **1.04–1.075× "speedup" with
   z = +1.7** while a foreign workload was running — larger than most bricks worth
   landing. **Never report an A/B without running the null arm on that machine.**
3. **The environment was contaminated exactly as D6c of the rate descent predicted.**
   A foreign `remade_ffmpeg_rs` speedbench + `cargo build -p ffai-cli` started
   *mid-run*; absolute times drifted ~2× between measurement blocks (quality base
   751 → 1490 ms), and single-run profiler buckets moved in *opposite* directions
   across presets. After stopping the interfering work the null tightened to
   0.94–0.99× and the real effect separated cleanly.

- **Fair-comparison audit (passed):** ours runs `enc.encode(f)` sequentially (not the
  GOP-parallel `encode_all`); x264 runs `--threads 1`; x264's time is its
  self-reported encode-loop fps, excluding process startup. Both arms report
  identical call counts (54806 / 352151 / 3177868) — identical work.
- **Still open (inherited from the rate descent, D6a):** x264 runs **`--psy-rd 1.0`
  by default** while we score it on PSNR/SSIM, which its own docs say to disable for
  metric evaluation. That flatters **us**, so the true rate gap is likely *larger*.

---

## Rebuild — climbing back up, gated at every level

### R1 — hoist the MC scratch to per-thread storage *(landed)*

`luma_tile` now writes into caller-provided storage instead of returning a zeroed
441-byte array by value; `mc_luma_subpel` takes `a`/`b` as parameters; both live in a
`thread_local! McScratch`. **Byte-identical by construction** — both `luma_tile_into`
paths write the whole `(bw+5)×(bh+5)` region before it is read, and `a`/`b` are fully
written over `bw*bh` before any read, so the zero-fill was dead in every arm.

| gate | result |
|---|---|
| unit oracles (`mc_luma_block_kernels_match_per_pixel`, `..._padded_matches_exact`) | pass |
| full workspace suite, `--features asm` | pass |
| byte-identity, 3 presets × {sequential, GOP-parallel} | **identical** (`3685aa87…`, `e3d76d0f…`, `856fcd4a…`) |
| primitive (interleaved microbench) | **1.37–1.84×** on every sub-pel arm |
| end-to-end, paired ABBA, quiet box | **quality 1.121×, 8/9 wins, z = +2.3** (null floor 0.988×) |
| end-to-end, fast / balanced | inside the null floor — *expected*: fast is integer-pel (54 K MC calls vs 3.18 M) |

The win landing only where sub-pel MC is actually used is the physical check that it
is real and not harness drift.

### R2 — cache the `RFF_ME_BATCH` env read *(landed)*

`std::env::var` (String alloc + process-wide env lock) sat inside the motion-search
rescue path. Moved behind a `OnceLock`. Byte-identical; below the harness floor
individually, kept because it strictly removes work from a hot path — the same
"cache any runtime switch inside a hot loop" law the deblock campaign already paid
for once.

---

### R3 — half-pel plane cache *(landed — the structural lever)*

**Ceiling measured first** (`examples/hpel_ceiling.rs`), per the rebuild rule.

*The call mix* (`inter::mcstats`, real encode, mobile_cif preset=quality) —
**84.16% of MC calls are sub-pel**:

| size | fullpel | half H/V | half centre | quarter |
|---|---|---|---|---|
| 16×16 | 3.01% | 5.04% | 2.26% | 6.92% |
| 16×8 / 8×16 | 7.09% | 12.37% | 5.54% | 16.88% |
| 8×8 | 5.75% | 12.37% | 5.54% | 17.24% |

*The ceiling*, mix-weighted, three independent runs: **7.8–9.4× cheaper per call**,
**net prize 74–80% of `inter-mc`**, projecting **1.92–2.05×** on the encode.

> ⚠ The FIRST ceiling run said **13.9%** and would have killed the lever. Its
> "planes" arm used runtime-length `copy_from_slice` and a scalar average, so a
> plane *copy* measured 3× slower than a full 6-tap *filter* — impossible on its
> face. **A ceiling probe must implement the replacement as well as the real thing
> would**, or it measures a strawman. The tell was the impossible number, and the
> reconciliation factor against the in-context ns/call (2.11× wrong → 0.88× right).

**What landed:** `HpelPlanes` (H, V, centre) built once per reference picture,
lazily on first sub-pel use, `Arc`-shared so DPB clones stay cheap. Built by walking
the frame in 16×16 blocks through the *same* `luma_tile_into` + `luma_h`/`luma_v`/
`luma_centre` kernels MC already uses — so the planes are **bit-exact by
construction**; each sample depends only on the six clamped inputs around it, so its
value cannot depend on which block computed it. `hpel_block` then serves every
sub-pel position as a strided copy (half) or a 2-tap average of two planes
(quarter), with const-width inner loops, declining near the frame edge so the caller
falls back to `mc_luma`.

**Scope — search only.** It is wired into `mc_satd`/`mc_sad` (the ME cost
functions), NOT into reconstruction. The search makes ~300 MC calls per macroblock
and the final reconstruct ~1, so this is where it pays; and because the plane read
is bit-identical, the chosen MV — hence the bitstream — is unchanged.

| gate | result |
|---|---|
| `hpel_block_matches_mc_luma_exactly` (7 sizes × 15 phases × many positions) | pass |
| full workspace suite `--features asm` | pass (72 in common) |
| byte-identity, 3 presets × {sequential, parallel} | **identical** |
| end-to-end, paired ABBA, quiet box | **quality 1.506×, 9/9 wins, z = +3.0** (null 1.023×) |
| balanced / fast | inside the null floor |

**Preset gate.** Ungated, `fast` measured **1.18× SLOWER** — it is integer-pel
(~55 K MC calls/30 frames vs quality's ~3.2 M) and cannot amortise three frame-sized
filters. Gated to sub-pel-refining presets (`!self.fast`). Balanced measured neutral
(0.965× against a 1.012× null); it is left on the plane path but is the arm to
re-examine if the memory matters.

---

### R4 — the four ranked levers, hammered *(2026-07-27)*

**1. `enc-source-copy` — LANDED, byte-identical.** The ceiling probe re-priced the
brick and changed which brick to build: three plane `clone()`s on the MB-aligned path
are only **137 ms of the 579 ms stage** (0.96% of the fast preset — measured and
DECLINED, it needs call-site churn for under 1%). The other 77% is the **per-pixel
edge-extension loop**, taken by every frame whose height is not a multiple of 16 —
i.e. all 1080p content (1080/16 = 67.5 → coded 1088). Row-wise `copy_from_slice` +
`fill` made it **2.71× faster** (2402 → 887 µs/frame), saving ~364 ms of the stage.
Gated by `clamp_plane_matches_per_pixel_oracle` against the retained per-pixel twin,
on the real 1080p geometry plus adversarial ragged cases.

**2. Deblock — NOT GRINDING, campaign already at its floor.** Verified rather than
assumed: Phase 2 landed (105 → 83 ns/MB derivation, byte-identical), Phase 1 was
built, measured and defaulted OFF (relocating the derivation into the MB loop grew
that loop by ~2× the derivation's own cost — the grids were never cold), Phase 3 was
pruned by the cache-boundedness sweep before a line was written. The current anatomy
probe confirms the dominant real-content scenarios are unmoved by the branchless arm
(all-intra 233 ns/MB, inter-coded 247). The doc's own conclusion stands: only a
commit-time derivation reading values still live in registers can win, and mere
relocation has already been measured negative. Per "know when a kernel is DONE."

**3. 4-wide asm — REOPENED, then solved WITHOUT asm.** My earlier retirement was
right for P-only configs and WRONG in general. P partitions do bottom out at 8×8
("8×4/4×8/4×4 sub-shapes … not yet built"), but **B-frame spatial-direct uses 4×4**.
With B-frames on: 4×4 is **7.9% of all MC calls and 50.3% of all sub-pel ones** —
and `luma_h`/`luma_v` dispatch to asm only at width 16/8, so every one ran the scalar
6-tap. The fix was not a 4-wide kernel: `b_mc_block` now serves those blocks from the
**already-built half-pel planes** (bit-exact, R3), collapsing 4×4 sub-pel calls
**140,464 → 3,385 (97.6%)**, the remainder being edge declines. Byte-identical (hash
`6c95e2bd…` unchanged); paired ABBA 1.020×, 6/7 wins — at the noise floor, kept as
strictly-less-work. Knob: `RFF_BDIRECT_PLANES=0`.

> The generalisable error: a census taken on ONE config (P-only) was reported as a
> structural fact. Enumerate the configs a path can occur in before retiring it.

**4. Quality-preset ME re-pricing — MEASURED; the preset is fine, ONE KNOB is not.**
4-QP BD-rate ablation (PSNR *and* SSIM), 24 frames, anchor = full Quality:

| arm | mobile speed | BD-PSNR | BD-SSIM | foreman speed | BD-PSNR | BD-SSIM |
|---|---|---|---|---|---|---|
| `-sub8x8` | 1.23× | −0.07 | **+2.41** | 0.98× | −0.29 | +0.05 |
| **`-me_wide`** | **1.39×** | **−0.03** | **+0.01** | **1.48×** | **−0.16** | **+0.06** |
| `-both` | 1.93× | −0.02 | +2.49 | 2.08× | −0.10 | +0.30 |
| `balanced` | 3.41× | +4.88 | +11.82 | 2.71× | +14.48 | +17.20 |

- **Retiring the preset is REFUTED.** `balanced` is a real quality cliff (+4.9% to
  +14.5% BD). The preset earns its keep; the earlier "6× time for 2.1% rate" was a
  single-QP size ratio, i.e. the mirage this repo has now been bitten by four times.
- **`me_wide` is the problem: 1.39–1.48× of the whole preset for nothing on real
  content** — BD-PSNR *negative* (better without it), BD-SSIM +0.01/+0.06% (noise).
  It was validated on synthetic clips (tsrc −11.1%, zoom −3.4%) that are not in this
  repo, so it is a content-adaptive win with a dispatcher that is not separating.
- **Its online payoff gate has an irreducible floor.** Sweeping `RFF_ME_PAYOFF`
  15 → 30 → 50 → 80 on foreman: anchor 1446 → 1293 → 1265 → 1269 ms against 947–963 ms
  fully-off. Even at the strictest setting the anchor stays **1.34× slower**, because
  the `me_learn = 40` learning window always runs the full rescue before the gate can
  fire — and BD moved from −0.16 to −0.37, so the window's committed MVs are mildly
  harmful here.
- **NOT flipped** — and the follow-up run below shows that was the right call.

### R5 — me_wide validated on the real corpus *(the two-clip read was wrong)*

The R4 conclusion ("me_wide buys nothing") was drawn from **mobile + foreman only**,
and those are precisely the two content classes where it does not pay. Re-run as a
**per-clip truth table over all 20 `video-tests` clips** (4-QP BD-rate, PSNR *and*
SSIM, anchor = me_wide ON, so POSITIVE = turning it off costs you):

| win | BD-PSNR | cost | | ~zero | BD-PSNR | | loss | BD-PSNR | cost |
|---|---|---|---|---|---|---|---|---|---|
| blue_sky | **+4.70** | 4.85× | | soccer | +0.00 | | foreman_qcif | **−1.08** | 3.57× |
| bus | **+4.57** | 1.92× | | akiyo ×2 | 0.00 | | foreman_cif | −0.16 | 1.43× |
| football | +1.51 | 2.07× | | FourPeople | 0.00 | | tempete | −0.12 | 1.65× |
| park_joy | +0.91 | 5.08× | | ducks | −0.00 | | mobile | −0.03 | 1.47× |
| stockholm | +0.63 | 3.04× | | harbour | −0.02 | | | | |
| in_to_tree | +0.56 | 1.89× | | | | | | | |
| shields/crowd/city/crew | +0.18..+0.27 | 1.15–2.03× | | | | | | | |

**Mean over 20 clips: +0.62% BD-PSNR / +0.69% BD-SSIM.** Synthesized boundary content
(its stated premise — smooth with large or non-translational motion) reaches
**+6.73 fastpan / +4.89 rotation / +2.63 zoom**, corroborating the original
tsrc/zoom claims in kind. **me_wide earns its default-on.**

**But it is an UNFINISHED DISPATCH, per the governing principle:** the per-clip BD
**sign-flips** (+4.70 → −1.08), which is itself the dispatch signal — never average it
away. Two further findings that constrain the fix:

1. **`me_range` is a compromise dial, not the axis.** Sweeping 24/16/8/4:
   foreman_qcif is negative at *every* setting (−1.08/−0.55/−0.50/−0.19) while
   blue_sky is positive at *every* setting (+4.70/+3.10/+0.73). Shrinking the range
   trades the win away proportionally — exactly the "never split the difference with
   a fixed constant" trap.
2. **The online payoff gate separates STATIC content correctly** (akiyo/FourPeople:
   0.00 BD at ~1.0×) **but not the rest**, and it has an irreducible floor: the
   `me_learn = 40` window always runs the full rescue before the gate can fire
   (anchor 1446 → 1269 ms across payoff 15 → 80 against 947 ms fully-off), and its
   committed MVs measured mildly BD-negative on foreman.

**The next brick** is therefore a content signal that predicts the SIGN, evaluated
against the truth table above — not a threshold turn. Worst-value cases to target:
soccer 1.70× for +0.00, park_joy 5.08× for +0.91, stockholm 3.04× for +0.63.
Note the resolution hint: foreman is −1.08% at QCIF but only −0.16% at CIF, the same
content at two scales.

**Harness:** `examples/me_ablation.rs` (`AB_CORPUS=1 <clips…>`) — per-clip BD-rate on
PSNR and SSIM. BD columns are deterministic, so the table is valid on a loaded box;
the ms columns are indicative only.

### R6 — the me_wide dispatcher, built and DEFAULT-ON *(the sign-flip closed)*

Three candidate signals were instrumented against the R5 truth table before any axis
was chosen (`examples/me_signals.rs`), per "instrument 3+ candidates against the
win/loss column FIRST":

1. **`headroom` — KEPT.** On a subsample of blocks, how much a WIDE full-pel search
   beats a PREDICTOR-LOCAL one: `mean (SAD_local − SAD_wide) / SAD_local`. This is
   what the rescue actually buys, measured on source pixels *before* the MB loop and
   *without committing a vector* — unlike the online payoff gate, which scores its
   own SATD cut *after* committing MVs and therefore only ever separated static
   content.
2. **`mvdiv` (MV-field divergence) — REFUTED and deleted.** Built to catch the affine
   clips that translational head-room misses (syn_rot 1.19% head-room yet BD +4.89).
   It cannot: foreman_cif (7.41, BD −0.16) and mobile (6.59, −0.03) sit ABOVE syn_rot
   (6.66, +4.89) and syn_zoom (5.94, +2.63). No clip it classifies correctly that the
   one-term gate gets wrong ⇒ delete the term.
3. **`motion` / `var` / `tdiff` — refuted**, none separate the winners from the losers.

**Landed:** `me_wide_headroom()`, a per-FRAME probe (~24 samples, ±24 step 4).
Per-frame deliberately — cross-frame adaptive state is nondeterministic under the
GOP-parallel path, a lesson the rescue's own learning window already paid for. It is
skipped entirely when the gate is off, so the escape hatch costs nothing.

**Calibration on the DEPLOYED estimator** (it differs from the offline probe — the
recurring law): winners 10.4–29.1, losers 0.5–8.2. Threshold **16**.

| gate | real-corpus mean | worst clip | clips paying 1.1–3.6× for ~0 |
|---|---|---|---|
| always-on (before) | +0.62% | **−1.08%** (foreman_qcif) | 13 |
| **head-room ≥ 16** | **+0.547%** (88% kept) | **0.00%** | **0** |

Wins preserved: blue_sky +4.70, bus +4.37, park_joy +0.94, football +0.64, shields
+0.20; synthesized fast-pan +6.73, rotation +1.72, zoom +1.11 — the affine clips
survive *because the gate is per-frame*, so their high-head-room frames still fire
even though their clip-mean head-room is low.

**Gates:** full suite clean; `fast`/`balanced` byte-identical (they never run
me_wide); `RFF_ME_HR=0` reproduces the pre-gate bytes exactly (`856fcd4a…`) as the
escape hatch and bisection anchor; foreman_cif quality 361 → 294 ms (1.23×) at BD
+0.03.

### R7 — both R6 caveats closed

**(a) The threshold is NOT a 2-clip boundary.** Measuring deployed head-room across
all 24 clips gives a distribution with a **6.9-wide natural gap**:

```
51.5 syn_fastpan │ 29.2 blue_sky │ 28.4 park_joy │ 25.3 bus │ 20.0 crew │ 17.0 shields
  ─────────── natural gap ───────────
10.2 syn_zoom │ 9.9 football │ 9.7 foreman_cif │ 8.6 soccer │ 7.1 syn_rot │ 4.1 foreman_qcif │ ≤2.0 rest
```

T = 13 and T = 16 both sit inside that gap and both produce monotone non-regression,
so the threshold is chosen from the whole distribution, not fitted to two clips.

> **The honest remaining weakness:** football (9.90, BD **+1.51**) and foreman_cif
> (9.65, BD **−0.16**) have nearly identical *mean* head-room with opposite BD signs.
> The gate separates them only because it is PER-FRAME and their per-frame
> distributions differ. Mean head-room is therefore NOT a sufficient statistic — if
> the gate is ever re-tuned, re-tune it on the per-frame behaviour, not these means.
> (Head-room is also mildly window-sensitive: football read 10.43 over 10 frames and
> 9.90 over 8.)

**(b) The affine class is no longer self-synthesized.** Four REAL Xiph Derf clips with
genuine affine camera motion, fetched independently:

| clip | motion | ungated | gated T=16 | me_wide cost |
|---|---|---|---|---|
| station2 | zoom | +1.91 | **+1.91** | 1.77× → 1.69× |
| tractor | pan + zoom | +1.11 | **+1.11** | 2.74× → 2.06× |
| old_town_cross | aerial pan | **−0.01** | **+0.00** | 1.25× → 1.14× |
| sunflower | near-static | +0.00 | +0.00 | 1.00× (off) |

The gate preserves 100% of the real affine wins and removes the one negative. Note
also that the existing corpus already contained real rotating content — the manifest
describes `blue_sky` as "smooth gradient sky + **slow rotate**", and it is the
single biggest winner (+4.70), correctly routed ON (head-room 29.2).

**Final state on the widened 24-clip REAL corpus:**

| | always-on | gated T=16 |
|---|---|---|
| mean BD-PSNR | +0.639% | **+0.581%** (91% retained) |
| **worst clip** | **−1.08%** | **0.00%** |

---

### R8 — re-profile after the wins, and pad the planes

**Re-profiling (the bottleneck moves) found a NEW ghost of my own making.** After R3–R7
the quality preset on mobile_cif reads `enc-me` = 78% of encode, but `inter-mc` is now
only 101 ms of ME's 287 ms — **186 ms was unnamed**, because the plane cache MOVED the
work into `hpel_block`, which I had never instrumented. Unnamed work is invisible work.
Two INFO scopes (`me-hpel-read`, `me-cost/satd`) named it:

| component | calls | ns/call | ms |
|---|---|---|---|
| me-hpel-read (plane cache) | 2,164,400 | 33.1 | 71.7 |
| **inter-mc (declined → 6-tap)** | **394,551** | **195.1** | **77.0** |
| me-cost/satd (full-pel) | 2,038,454 | 17.9 | 36.4 |

**The 15% that DECLINED the cache cost more than the 85% that used it.** A census of the
fallbacks showed **62.5% were sub-pel** — declining purely because their vector left the
picture, so the bounds test rejected them.

**Fix — pad the planes** (what x264 does). `HPEL_PAD = 32` sized so a ±24 search around
any macroblock still lands inside. Two things this required:

- **Plane-edge replication would NOT be bit-exact.** The half-pel sample one past the
  edge is a 6-tap of *clamped source* taps, not a copy of the plane's edge value. So the
  planes are built from an **edge-replicated padded source** — and because `mc_luma`'s
  `at()` clamp IS edge replication, the result is bit-identical by construction.
- A **padded full-pel plane** (`f`) had to join the set, since the quarter positions
  average against `G` and that operand must exist out-of-frame too.

**Result** (byte-identical; oracle test extended to out-of-frame vectors −64/+96):

| | before padding | after |
|---|---|---|
| `inter-mc` calls | 394,551 | **218,952** (−44%) |
| `inter-mc` ms | 77.0 | **26.6** (−65%) |
| plane build | — | 8.1 ms total (353 µs/frame) |
| foreman quality wall | 237 ms | **~200 ms (~1.15×)** |

**Honest cost, recorded:** `me-hpel-read` per-call rose **33.1 → 39.2 ns** — the padded
footprint (4 planes over 1.44× area = 1.9× the memory touched) costs cache locality.
The net is positive but the components partly cancel, and at the whole-stage level the
effect sits near the ±5% noise; the wall comparison and the call-count collapse are what
carry it. `HPEL_PAD` is therefore a real tuning knob (a smaller pad would trade coverage
for locality) that has **not** been swept.

### R9 — the two R8 caveats closed

**(a) `HPEL_PAD` swept; 32 was the wrong guess.** Made runtime-settable
(`RFF_HPEL_PAD`) so both arms live in one binary, then swept on low- and high-motion
content:

| pad | foreman fallbacks | bus | park_joy | build (foreman) |
|---|---|---|---|---|
| 0 | 207,537 | 838,581 | 6,354,258 | 5.7 ms |
| 8 | 127,407 | 774,592 | 6,172,777 | 4.8 ms |
| **16** | **127,274** | **765,625** | **6,122,122** | **6.1 ms** |
| 32 (was default) | 127,262 | 764,999 | 6,093,358 | 9.0 ms |

Nearly all the coverage arrives by **8** (−39% fallbacks on foreman); **16** picks up
the large-MV content 8 misses; past 16 only build cost and footprint grow. Wall time
is indistinguishable across 8/16/32 (spreads ≤1.13×) — so the decision rests on the
structural columns, and the default moved **32 → 16**. Bit-exactness re-confirmed:
the bitstream hash is *identical* at every pad, as it must be.

**(b) ★ The 612 ms "2.6× regression" is SOLVED — and it was not contention.**
Two plausible explanations were measured and REFUTED before the real one was found:

1. **CPU contention — refuted.** 56 spinning hogs on this 24-core box move a
   single-threaded encode ~5% (137 vs 130 ms); Windows keeps handing the foreground
   thread a core.
2. **A concurrent `cargo build` — refuted.** No measurable effect at all
   (168.71 vs 168.57 ms).
3. **The real cause: the binary had the PROFILER compiled in.** Measured directly —
   the identical command reads **314 ms profile-ON vs 195 ms profile-OFF, a 1.61×
   inflation** from the rdtsc scopes. The feature leaks in silently because
   `cargo test --workspace --features asm` unifies features across the workspace and
   rebuilds the example with `profile` enabled. The binary is *fresh*; it is simply
   **not the binary you think it is** — the stale-binary law in a feature-flag disguise.

**Guards landed in `encode_hash`** so this cannot be reported as a result again:
- `PROFILER BUILD: ~1.6x inflated, NOT a throughput number` — a `cfg!(feature)` check,
  the decisive one.
- A **reproducibility** guard replacing the refuted contention probe: every timing
  prints its spread over N reps, refuses to present 1–2 reps as comparable, and flags
  `spread > 1.40×`. The trip point is set from a **measured** 40-sample idle sweep of
  this box (max/min **1.36×**, p90/min 1.22× — pure core-clock scaling), not guessed;
  anything below ~1.4 is a false-positive generator, which the first two attempts
  duly demonstrated.

> Standing consequence: **cross-session wall comparisons on this box are not valid** —
> idle variation alone is up to 1.36×, and the same command has read 168 / 206 / 307 ms
> across sessions. Only paired, same-session, interleaved (ABBA) comparisons decide,
> which is what `me_ablation`/the A/B harnesses already do.

---

### R10 — the honest side-by-side vs x264 (D1a, finally measured)

**The matched-QP comparison this repo kept using is invalid, and I reproduced the
failure once more before fixing it.** At one QP, x264's ten presets differ mostly in
SIZE — their PSNRs cluster inside ~0.2 dB — so "match the preset with the nearest
PSNR" keeps selecting `placebo` and yields absurdities like *97× faster at equal
quality*. Rate and distortion must be swept together.

`examples/x264_bdrate.rs`: both encoders over the same QP ladder (22/27/32/37), both
streams decoded by **our** decoder (removes any timestamp-alignment trap), PSNR
frame-by-INDEX vs source, then BD-rate. Guards: frame-count equality (it fired
immediately — x264 was encoding 120 frames against our 24 until `--frames` was added),
stale-artifact deletion, and the PSNR-overlap width printed with every number.

**Matched toolset** (`--profile baseline` = CAVLC, no B-frames, no 8×8 — which is
exactly our default configuration), 3 clips, overlaps 7.5–12.1 dB:

| our preset | vs superfast | vs veryfast | vs medium |
|---|---|---|---|
| fast | +56 … +118% @ 0.61–1.26× | +66 … +127% @ 0.87–1.53× | +71 … +171% @ 2.3–7.8× |
| balanced | +3.7 … +6.8% @ 0.20–0.51× | +8.4 … +24.5% @ 0.28–0.62× | +15 … +41% @ 0.98–3.2× |
| **quality** | **−1.3 … −7.1%** @ 0.10–0.23× | +3.1 … +8.2% @ 0.12–0.31× | +7.2 … +25.6% @ 0.62–0.80× |

**Pareto verdict:**
- **ours/quality compresses BETTER than x264 superfast** (−1.3 to −7.1%) but costs
  4–10× more time. Not dominated by superfast — dominated by **veryfast**, which is
  3–8× faster *and* 3–8% smaller.
- **ours/balanced is dominated by superfast** (2–5× faster and 4–7% smaller).
- **ours/fast** holds the fast corner, but at +56…+118% bitrate. x264's own ladder
  buys ~2% BD per preset step, so our fast preset is priced roughly an order of
  magnitude worse per unit of time saved.

**So: our compression ceiling is ≈ x264 veryfast-class, reached at roughly
medium-to-slow cost.** We are Pareto-dominated at essentially every operating point,
most narrowly at the quality end.

**Capability, not default** (`XB_CABAC=1`: our CABAC on, x264 `--profile main`,
B-frames pinned off both sides — foreman): ours/quality goes to **−9.2% vs superfast**
and **+5.5% vs veryfast**. So the shipped default leaves real compression on the table
(CABAC, 8×8 and B-frames are all implemented and all default-OFF). ⚠ The `medium`
column there reads −8.0% but over a **4.4 dB** overlap (vs 7.5+ elsewhere) and with
B-frames pinned off — which handicaps `medium` disproportionately, since its tuning
assumes them. Treat that one cell as unreliable.

**What this campaign changed:** every brick was byte-identical, so the BD-rate columns
are *exactly* what they were before it — only the speed column moved. The quality
preset gained ~1.9× compounded (MC scratch 1.12× · plane cache 1.51× · padding 1.15×)
plus up to 3.6× more on the clips the me_wide gate now skips. The speed ratios above
would have been roughly half as good in every row at the start of this work.

---

---

# OPEN DESCENTS — where we are still losing, and the next measurement for each

Full decomposition after R1–R10 (quality preset, mobile_cif 24f, TOTAL 320.6 ms).
Every line below is MEASURED; the "next measurement" column is what closes it.

## The speed map

| component | ms | % encode | status |
|---|---|---|---|
| **enc-me** | **261.0** | **81%** | |
| ├ **me-subpel** | **141.4** | **44%** | **U1 — the dominant cost of the encoder** |
| ├ me-diamond | 90.7 | 28% | U2 |
| ├ me-rescue | ~0 | ~0% | ✅ the R6 gate works — it fires nowhere on this clip |
| └ residue (seed/predictors/glue) | ~29 | 9% | small, named enough |
| enc-inter-code | 18.9 | 6% | U5 |
| enc-cavlc-emit | 11.4 | 4% | at parity with x264 (prior finding) |
| enc-prep + hpel build | 15.3 | 5% | build is 278 µs/frame, fine |
| deblock | 3.7 | 1% | ✅ closed (R4) |

Nested primitives: me-hpel-read 69.2 ms @ 31.9 ns (2.16 M), me-cost/satd 35.2 ms
@ 17.3 ns (2.04 M), inter-mc 23.3 ms @ 106.5 ns (219 k).

### ★ The count, which is the real story

| | per macroblock |
|---|---|
| best_part calls | 8.5 |
| sub-pel candidate evaluations | **241** |
| full-pel candidate evaluations | **227** |
| **TOTAL candidate evaluations** | **468** |
| x264 medium: whole `mb-analyse` | **1 call, 7149 ns** |

**We evaluate ~468 candidates per macroblock. Our sub-pel refinement ALONE costs
~15,800 ns/MB — 2.2× x264's ENTIRE analysis.** This is a decision-structure gap, not
a kernel gap: the kernels are 17–32 ns and roughly where they should be.

## The quality map (BD-rate, matched toolset, 3 clips)

| preset | vs superfast | vs veryfast | dominant cause |
|---|---|---|---|
| fast | +56 … +118% | +66 … +127% | **U3 — no sub-pel at all** |
| balanced | +3.7 … +6.8% | +8.4 … +24.5% | U4 |
| quality | −1.3 … −7.1% | +3.1 … +8.2% | U4 |

**U3 is already half-answered by the data:** `fast → balanced` differs by sub-pel
refinement alone and closes **50–114 percentage points** (akiyo 55.7→5.1, foreman
83.7→6.8, mobile 117.8→3.7). Sub-pel is simultaneously our biggest speed cost (U1)
and our biggest quality lever — the two open descents are the same mechanism.

## U1 — CLOSED (measured; no default change)

**Descent, in order, each step closing on a measurement:**

**1. Skip-gate — REFUTED.** Harvested 280 k real refinements observe-only (the tap is
inert: hash `bdebb58a` unchanged with it on). Ceiling sweep on the skill's king
feature (null-arm cost ÷ λ): gain-kept falls almost LINEARLY with skip rate — skip 30%
→ keep 94%, skip 50% → keep 84% — nothing like the ~99%-kept-at-45%-skip shape a real
gate shows. Only **5.5–12.2%** of refinements change nothing on real content (akiyo
46.5% is the static outlier), median relative gain 8–36%. This is the *G5 signature*:
**an expensive arm that wins 88–95% of the time is doing real work; there is nothing
to skip.**

**2. Where the 29 evaluations actually go — MEASURED.**

| clip | evals/refinement | last improvement at | **wasted** | gain in ring 1 |
|---|---|---|---|---|
| bus | 29.0 | 14.3 | **50.5%** | 69.9% |
| foreman | 29.7 | 15.0 | **49.4%** | 63.7% |
| mobile | 29.9 | 15.2 | **49.1%** | 71.9% |

**Half of every refinement is spent confirming an answer already found** — inherent to
a hill-climb, but the RING SIZE sets the price of that confirmation (8 evals per
"no improvement" against x264's 4-point diamonds).

**3. Pattern arms — PRICED (4-QP BD, paired ABBA speed).**

| arm | foreman | mobile | bus | akiyo |
|---|---|---|---|---|
| pat2 (8-pt single pass) | 1.25× / **+2.34%** | 1.31× / +0.74% | 1.08× / +0.30% | — / +0.53% |
| pat1 (4-pt + iterate) | — / +3.24% | — / +0.81% | — / +0.76% | — / +0.70% |
| pat3 (4-pt single pass) | — / +7.07% | — / +1.72% | — / +1.71% | — / +1.42% |

So the pattern is a **genuine speed/quality rung priced at roughly x264-preset-step
rates** (~1.2–1.3× for ~1% BD), not a free win — which is why it does NOT become the
quality preset's default.

**4. Online dispatcher on the ring-1 fraction — BUILT, MEASURED, REFUTED.** Signal was
justified (foreman's gain is least concentrated in ring 1: 63.7% vs 69.9/71.9%, and
foreman is exactly the clip that loses most to a blanket cut). Within-frame learning
window, so deterministic under GOP-parallel encode. Result:

| clip | dispatched BD | dispatched speed | pat2-always BD |
|---|---|---|---|
| foreman | +0.97% | **1.04×** | +2.34% |
| mobile | +0.33% | **0.98×** | +0.74% |
| bus | **+0.81%** | 1.47× | **+0.30%** |

It costs BD exactly where it delivers no speed, and on bus it is **WORSE than the
blanket cut** (+0.81 vs +0.30). Cause: the refinement feeds the reference chain, so
per-frame inconsistency in refinement quality propagates — the same downstream-consumer
law that killed the per-unit me_wide gate. **Defaulted OFF**; default output stays
byte-identical (`54b83f40…`).

**What U1 leaves for the next descent:** the pattern is not the structural lever. The
structural one the harvest points at is that sub-pel refinement runs on **all 8.5
partition searches per MB, not just the winner** — refining only the full-pel winner
would cut sub-pel work ~8×, far past anything the pattern can offer. That is a
mode-decision restructure (it changes which partition wins), so it belongs with U2/U5,
gated on BD-rate.

## U2 — CLOSED (gate built, priced, shipped OFF)

The 16×16 search is the null arm and runs first; the 2-way splits + P_8x8 are 7 more
`best_part` calls. They were gated by a **qstep-only** formula that never normalises by
λ. Harvested 36 k gated macroblocks: a split actually wins **17.8% (akiyo) / 45.4%
(foreman) / 65.7% (bus) / 72.8% (mobile)** of the time.

A λ-normalised threshold on the null arm (`c16/λ`) is content-adaptive by
construction — at T = 600 akiyo skips 78.8% while mobile skips 11.3%, because the
feature is small exactly where 16×16 is already good.

| T | akiyo | bus | foreman | mobile |
|---|---|---|---|---|
| 400 | 22.5% skip / 100.00% kept | 10.8% / 100.00% | 19.2% / 100.00% | 2.9% / 100.00% |

**★ But the BD trial refuted the ceiling.** T = 400 measured **+1.35% BD on bus**
despite "100.00% of gain kept". Cause: **gain-kept is SUM-weighted, so it hides many
small mode flips** — skipping 10.8% of macroblocks that each have tiny gain preserves
the total while still changing modes that each cost a little rate. *The ceiling
measured the SEARCH's own objective (SATD cost), not the ENCODER's (bits at quality).*
Shipped OFF (`RFF_SPLIT_T`, default 0, byte-identical); the harvest + threshold table
are kept for a future dispatch.

## U3 — CLOSED, and the biggest structural finding

The `fast` preset's +56…+118% BD gap is **entirely "no sub-pel at all"**. Forcing
sub-pel on, anchored on `fast`:

| clip | +sub-pel SINGLE-PASS | +sub-pel full (= `balanced`) | fraction of the win captured |
|---|---|---|---|
| foreman | **−38.14%** @ 0.46× | −39.94% @ 0.36× | **95.5%** |
| mobile | **−49.38%** @ 0.54× | −49.66% @ 0.41× | **99.4%** |
| akiyo | **−26.10%** @ 0.76× | −26.43% @ 0.73× | **98.8%** |

**A single-pass sub-pel captures 95–99.4% of the full refinement's benefit.** Since
`balanced` IS fast+full-sub-pel, running it single-pass is **1.03–1.31× faster for
+0.28…+1.80 pp** — a straight Pareto improvement on `balanced`, and the natural basis
for a new rung between `fast` and `balanced`. (Consistent with U1: half of every
refinement only confirms an answer already found.)

## U5 — measured, DEPRIORITISED

`enc-inter-code` is 18.9 ms of 320 ms (**5.9%**) with `enc-T/Q` 3.2 ms inside it.
Ranked by absolute cost it is an order of magnitude below the ME decision layer;
per "rank by absolute cost", it is not the next brick.

## U6 — CLOSED: the defaults leave a large win unused

Per-tool BD × speed on the shipped default (quality preset), 4-QP:

| tool | foreman | mobile |
|---|---|---|
| **+CABAC** | **−9.00% BD @ 0.91×** | **−8.83% BD @ 0.82×** |
| +8×8 transform | +0.92% @ 0.90× | +0.52% @ 0.86× |

**CABAC is −9% BD for 1.10–1.22× time — better value than any preset step in either
encoder** (x264's ladder buys ~2% per step), and it is OFF by default. The 8×8
transform measures WORSE on both clips and should stay off for these presets.

## U4 — still OPEN (the one genuinely unanswered unknown)

Where the residual 3–8% goes at balanced/quality is still unmeasured. Note U6 does NOT
close it: enabling CABAC on our side while x264 moves to `--profile main` left the gap
at +5.5% vs veryfast, because x264 gains from CABAC too. **Next: the bit accountant
(`codec-analyzer` instrument #6)** — bits per syntax element at a matched-quality
point, ours vs x264's own stats. Do not guess; the accountant is cheap and this is
exactly the class of question it exists for.

## R11 — DEFAULTS FLIPPED (U6 + U3 landed)

Two default changes, both measured before the flip:

1. **CABAC on, profile → Main** (U6: −9.00%/−8.83% BD for 1.10–1.22×).
2. **`balanced` runs single-pass sub-pel** (U3: 95.5–99.4% of the full refinement's
   benefit for 1.03–1.31× less time).

`RUSTY_H264_LEGACY_CAVLC=1` (+`RFF_SUBPEL_PAT=0`) reproduces the **exact prior
bitstream** — verified: `3685aa87…` / `e3d76d0f…` / `54b83f40…`.

**Same-QP effect on foreman:** fast 196430 → 180721 B (**−8.0%**), balanced
137642 → 129574 (**−5.9%**), quality 128580 → 116734 (**−9.2%**).

### ★ The flip exposed two latent decoder bugs — the fuzzer caught them immediately

Per "promoting a feature to DEFAULT retires the byte-identity gate", the gate becomes
strict-external conformance. Running the suite did that job before any of it shipped:

- **3 panics in `inter.rs`** — the MC interior fast paths test `ix0/iy0` against
  `cw`/`ch` but then index the plane slice, and a malformed stream can reach them with
  dimensions inconsistent with the actual buffer. Fixed: `at()` is now a bounds-safe
  fetch and every interior fast path additionally requires `reference.len() >= cw*ch`.
- **1 panic in the CABAC slice loop** — `decode_terminate` is its only exit, and the
  arithmetic decoder zero-fills past the buffer end, so a mutated stream walks `addr`
  past the picture. Fixed by bounding the loop (`addr >= total → Truncated`). The CAVLC
  slice loop already had its bound; **the CABAC one never did, and had simply never
  been fuzzed because CABAC was not the default.**

Both are the "each new slice type is a fresh fuzzer for shared paths" law. The legacy
(CAVLC) default passed the same fuzzer, which is what localised it to the CABAC path.

### Gates

| gate | result |
|---|---|
| full workspace suite `--features asm` | **18/18 green** |
| decoder fuzzer (mutated streams, CABAC seeds) | **no panics** |
| **ffmpeg pixel-exact vs our decoder**, 4 clips × 3 presets | **12/12 PIXEL-EXACT** |
| escape hatch reproduces pre-flip bytes | **exact** |

Tests that asserted the old defaults were re-pinned to state their own toolset
(`sps/pps_roundtrips`, the 8×8 tests), and a new `default_config_signals_main_profile_and_cabac`
makes a silent revert of the flip a test failure.

### Where it leaves us vs x264 (ours default vs `--profile main`, B off both sides)

| our preset | vs superfast | vs veryfast |
|---|---|---|
| quality | **−9.2%** (foreman) / **−3.2%** (mobile) | +5.5% / **+1.6%** |
| balanced | +8.5% / +3.3% | +26.1% / +8.5% |

**Our quality preset now beats x264 superfast on compression and is within 1.6–5.5% of
veryfast.** The remaining gap is speed, not compression: 0.11–0.17× at those points.
(BD columns deterministic; the speed column here is single-run — use the paired harness
for speed claims.)

## U5-struct — deferred sub-pel refinement (search all shapes full-pel, refine the winner)

**Arithmetic FIRST, per "expected pipeline gain = stage share × speedup".** From the
split harvest, 16×16 wins 27.2–82.2% of gated macroblocks, so today's 9 refinements per
gated MB could be **2,346 vs 14,958 (akiyo, 6.38×)** … **31,016 vs 105,264 (mobile,
3.39×)**. At sub-pel = 44.1% of encode that projected **1.42×** against a 1.05× floor —
8–10× the noise, so the experiment could succeed. (Contrast U1's int8-style prune: had
this landed under the floor it would have been unbuildable regardless of kernel quality.)

**Built:** `motion_search` gained a `start: Option<mv>` hook that skips the full-pel
search and refines that vector, pricing the baseline with its OWN cost closure (passing
a precomputed cost in was a bug caught before measuring — the refinement would have had
a bogus baseline and accepted anything). `refine_part` is the `best_part` companion.

### ★ Two bugs, both caught by an implausible number rather than by inspection

1. **+62…+104% BD on the first run.** Deferring cannot cost more than sub-pel is
   *worth* (26–49%, U3), so the number was impossible. Cause: **there are TWO partition
   drivers and I patched one.** The unpatched one is the CABAC path — which R11 had
   just made the DEFAULT — so every default encode deferred sub-pel and then never
   refined it. The profiler proved it: 53,656 hpel reads for 38,332 searches ≈ **1.4 per
   refine** where a real refine is ~29. *"Check the code path actually executes"* found
   in one measurement what re-reading the theory would not have.
2. **+91…+145% on `balanced` after that fix.** The fast/balanced path evaluates a
   SINGLE 16×16 candidate — there is no losing shape to skip, so deferring there does
   not save the refinement, it **deletes** it. Guarded to the Quality preset (the only
   preset running the multi-shape driver); `defer ON balanced` now measures *exactly*
   `balanced OFF`, which is the proof the guard is right.

### Result

| clip | paired speed | BD-PSNR | BD-SSIM |
|---|---|---|---|
| foreman | **1.29×** (7/7, z=+2.6) | +2.54% | +1.93% |
| mobile | **1.38×** (7/7, z=+2.6) | +2.21% | +3.42% |
| akiyo | 1.05× | +0.84% | +0.69% |

Measured sub-pel reduction was **2.0×**, not the projected 3.4× — the ceiling assumed
every macroblock is gated, but ungated MBs already refine only 16×16. `1/(0.56+0.44/2)
= 1.28×` reproduces the measurement, so the arithmetic was sound and its *input*
(fraction gated) was optimistic. **Record the ceiling's assumption, not just its
number.**

**Verdict: a rung, not a default.** 1.29–1.38× for ~2.2–2.5% BD is x264-preset-step
pricing, and akiyo gets almost nothing. Shipped OFF behind `set_defer_subpel` /
`RFF_DEFER_SUBPEL`; default stays byte-identical (`ed7b56b0…`), suite green.

## The open unknowns

- **U1 — why does sub-pel cost 141 ms / 241 evaluations per MB?**
  *Measured:* 28.5 sub-pel candidates per `best_part`, 8.5 calls/MB. Iterating to
  convergence was landed earlier for −0.16…−2.39% BD-SSIM.
  *Unknown:* how many of those 28.5 change the result. *Next:* harvest the
  refinement's per-iteration improvement distribution (observe-only tap →
  `codec-search-skip-gate`: the ceiling sweep of skip-rate vs gain-kept). Strong
  prior that the null arm wins often here.

- **U2 — why does the full-pel diamond cost 90.7 ms / 227 evaluations per MB?**
  *Unknown:* the split between the coarse-to-fine ladder `[64,32,16,8,4]` and the
  per-step walk-until-no-improvement. *Next:* counter per step size; x264 `medium`
  uses `me=hex` with a much shorter ladder — compare candidate counts, not times.

- **U3 — is `fast` fixable, or is a sub-pel-free preset simply not viable?**
  *Measured:* +56…+118%, i.e. the preset is priced ~an order of magnitude worse per
  unit time than an x264 preset step. *Next:* a cheap 1-iteration half-pel-only
  refinement, BD-gated — does a fraction of U1's cost recover most of the 50–114 pp?

- **U4 — where do the remaining 3–8% of bits go at balanced/quality?**
  *Unknown entirely.* *Next:* the bit accountant (`codec-analyzer` instrument #6) —
  bits per syntax element, ours vs x264's `--verbose` stats at a matched-quality
  point. Do NOT guess; the accountant is cheap.

- **U5 — enc-inter-code at 2101 ns/MB.** Not yet decomposed below the stage.
  *Next:* scope T/Q vs recon vs syntax inside it.

- **U6 — the defaults leave measured compression unused.** CABAC on moves
  ours/quality from −7.1% to −9.2% vs superfast (~2 pp) and 8×8 / B-frames are
  implemented and off. *Next:* a per-tool BD × speed table so the defaults are chosen
  on data rather than history.

## Standing measurement rules for these descents

1. Cross-session wall comparisons are INVALID here (idle spread 1.36×). Paired,
   same-session, ABBA only.
2. Check `PROFILER BUILD` — a profile-ON binary is 1.61× inflated.
3. BD-rate over a QP ladder, never a single QP (this trap has now struck five times).
4. Rank by ABSOLUTE cost, and decompose any residue before believing it is irreducible.

---

## Next levers, ranked (ceiling before cost)

1. **The quality preset's 7.25 ME calls/MB.** Now the dominant remaining question.
   Even with the MC primitive fixed, the *count* is ~7× x264's analysis rate for
   less compression, and S1 showed the preset buys **2.1% rate for 6× time**. This
   is a decision-layer problem (`codec-search-skip-gate` /
   `codec-content-adaptive-dispatch`), not a kernel one — and *re-pricing or
   retiring the preset* remains on the table.
2. **`enc-source-copy`** — 301 µs/frame of full-frame copy, **4.0% of the fast
   preset** (0.3% of quality); x264 works from FENC tiles instead. Now one of the
   larger fast-preset items.
3. **Deblock** — 2.4× x264's, **8.2% of the fast preset** (0.6% of quality).
4. **~~No asm below 8-wide~~ — RETIRED as unsubstantiated.** The census shows the
   quality preset on mobile_cif emits **only 16×16, 16×8/8×16 and 8×8 MC calls —
   zero 4-wide**. Do not build a 4-wide asm path until a clip is found that actually
   exercises sub-8×8 partitions; the earlier "fixed overhead is 94% of a 4×4 call"
   observation was true but weightless.

## Standing rules adopted from this descent

- **Run the null arm on this machine before reporting any A/B.** Its floor here
  ranged 0.94–1.075× depending on load.
- **Do not run timing experiments while a corpus run is in flight** (now violated
  twice; both times it produced numbers that had to be thrown away).
- **Build probes with `--features asm`** — the default feature set is not the
  deployment one.

---

## Descents A–C into `enc-me` (post-CABAC-flip re-profile)

Fresh profile, quality preset, mobile_cif 24f. Absolute ms are inflated (loaded box +
profiler) but call counts are byte-for-byte identical run to run, so the ratios hold:

| | ms | share of ME |
|---|---|---|
| **enc-me** | 491.9 | 72.5% of encode |
| ├ me-subpel | 286.1 | 58% |
| ├ me-diamond | 153.6 | 31% |
| └ residue | 52.2 | 11% |

Dead on arrival: `num_ref_frames: 1`, so the multi-ref P commit is NOT multiplying ME.

### Descent A — the coarse-to-fine ladder buys NOISE, not motion

Per-rung census of `[64,32,16,8,4]` (quarter-pel steps), `diastats`:

| rung | mobile evals | improved | akiyo evals | improved |
|---|---|---|---|---|
| 64 | 19.4% | 0.47% | 21.6% | 0.05% |
| 32 | 19.7% | 0.84% | 21.7% | 0.27% |
| 16 | 18.5% | 0.74% | 18.3% | 0.10% |
| 8 | 18.8% | 1.04% | 18.6% | 0.54% |
| **4** | 23.6% | **6.27%** | 19.8% | **2.10%** |

Eval counts are near-EQUAL per rung (~5.6 evals each) — the walk almost never walks. Each
rung is a flat ~4-eval toll, so a static clip pays exactly the toll a fast-motion clip does.

The obvious reading — "rare hits are high-value rescues" — is REFUTED by the BD curve.
Ablating rungs is NEGATIVE on essentially the whole corpus (4-QP BD, PSNR and SSIM):
`[16,4]` reads −1.17/−0.99/−0.28/−0.54/−0.73/−0.67/−1.12/−1.51/−5.56(football)/−0.75/
−0.39/−0.37, with akiyo_qcif at 0.00 and ducks_take_off at +0.01 (noise). Including the
FAST-MOTION clips the coarse rungs exist for (crowd_run, bus, football, city).

Mechanism: a coarse jump finds a distant MV with marginally lower SATD, but it costs more
`mvd` bits AND wrecks the spatial coherence of the MV field, so every downstream
neighbour's predictor gets worse. λ·mvbits in the search cost does not price that
second-order damage.

`[8,4]` is NOT safe — bus_cif reads **+6.06%**: dropping the 16-qpel rung caps reach at
2 full-pel, which genuinely cannot follow a fast pan. `[16,4]` keeps the reach and drops
the waste.

Clean best-of-3 speed for `[16,4]` vs the 5-rung anchor: mobile 328→233 ms (1.41×),
foreman 200→144 (1.39×), akiyo 47→39 (1.21×). (The `ms` column inside `me_ablation` is
single-run and thermally contaminated — trust its BD, never its timing.)

### Descent B — REFUTED: the edge-overhang path is not the lever

Hypothesis: `interior_fullpel` tests the UNPADDED frame bounds while the hpel `f` plane is
padded, so edge candidates fall to the slow copy path unnecessarily. Census
(`satdpath`) says edge full-pel is **0.25–3.1%** of evals. Worth ~1% of ME. Killed.

The census did expose the real shape: interior full-pel ~48–52%, sub-pel ~48%. Sub-pel
makes as many evaluations as the ENTIRE full-pel search, at ~1.5× the per-eval cost —
because full-pel SATDs the reference in place while sub-pel copies into a temp first.

### Descent C — in-place half-pel SATD (LANDED, byte-identical)

The half-pel phases `(2,0)→h`, `(0,2)→v`, `(2,2)→c` read a SINGLE plane, contiguous at
plane stride — the same shape the interior full-pel path already SATDs in place. Census
(`hpelphase`): **49–53% of all sub-pel evals**, i.e. ~25% of every ME cost evaluation, were
copying 256 bytes purely to hand `satd_px` a unit-stride buffer.

`inter::hpel_ref` returns `(plane, base, stride)` under the IDENTICAL guard `hpel_block`
uses, so the accepted candidate set is unchanged and the bitstream cannot drift. Quarter-pel
still materializes (a two-plane average).

Byte-identical on mobile/foreman/akiyo/bus/football/tempete (balanced + quality).
Best-of-5, quality, 24f: mobile 1.182×, football 1.198×, tempete 1.131×, foreman 1.125×,
akiyo 1.050×, bus 1.043×. Escape hatch `RFF_HPEL_REF=0`. Suite 113/113.

### Descent A verdict — LADDER FLIPPED to `[16,8,4]`

Full 20-clip corpus, 4-QP BD vs the 5-rung anchor:

| ladder | mean BD-PSNR | mean BD-SSIM | WORST clip P/S |
|---|---|---|---|
| `[16,8,4]` **(new default)** | **-0.93%** | **-1.09%** | **+0.00 / +0.00** |
| `[16,4]` | -0.91% | -1.07% | +0.01 / +0.00 |
| `[8,4]` | -0.71% | -0.97% | **+6.06 / +5.33** (bus) |

`[8,4]` is disqualified: dropping the 16 rung caps reach at 2 full-pel, which cannot
follow a fast pan.

**The wall clock chose the WRONG ladder; the deterministic count corrected it.** Best-of-5
timings on a loaded box read `[16,4]` as the winner. ME cost-evaluation counts (exact, run
to run) say otherwise:

| clip | `[16,8,4]` | `[16,4]` |
|---|---|---|
| mobile | 1.17x fewer | 1.28x |
| foreman | 1.16x | 1.27x |
| **football** | **1.17x** | **0.65x — 1.55x MORE work** |
| bus | 1.57x | 1.98x |
| crew | 1.15x | 1.26x |
| akiyo | 1.18x | 1.28x |

`[16,4]` makes football do 1.55x MORE work: with no 8 rung the step-4 walk must crawl the
distance the 8 rung covered in one hop. **Reach and stride are separate properties** — only
the useless TOP of the ladder is removable. This is the depth-6 rule paying out again: when
wall clock and a deterministic work count disagree on a loaded box, the count wins.

Gates: 113/113 suite; **32/32 ffmpeg pixel-exact** (8 clips x balanced/quality x qp24/33);
`RFF_DIA_LADDER=64,32,16,8,4` restores the pre-change bytes exactly. At fixed QP the new
default is smaller on 6/7 spot-checked clips (football -5.9%).

### Why this one flipped and the deferred sub-pel did not

Both are bitstream changes. The defer lever cost 2.2-2.5% BD to buy speed, and the measured
competitive position barely moved (0.12x -> 0.14x of superfast) — spending a hard-won
compression lead for a ratio that stays in the same regime. This lever is NEGATIVE BD on 18
of 20 clips, zero on the other two, AND does less work. Monotone non-regression is the bar;
this clears it, the defer lever does not. Nothing here is a content-adaptive dispatch
candidate precisely BECAUSE it is monotone — dispatch is for sign-flips, and there is no
sign to flip.

### Re-priced vs x264 after Descents A + C

(Fresh binary — the first attempt reported UNCHANGED numbers because `x264_bdrate.exe`
had not been rebuilt after the flip. Rebuild every example that embeds the encoder before
reading a cross-encoder number; this is the second time the stale-binary trap has fired in
this campaign.)

| clip | vs superfast | vs veryfast |
|---|---|---|
| foreman | -9.2% -> **-9.9%** | +5.5% -> **+4.6%** |
| mobile | -3.2% -> **-3.6%** | +1.6% -> **+1.2%** |
| football | — | **+5.8%** |

Compression improved 0.4-0.9 pp at every point, consistent with the corpus BD, and the
encoder does 1.15-1.57x less ME work at the same time.

### What is still open

Speed remains the dominated axis (0.13-0.15x of superfast). The `enc-me` share is ~72% of
encode, so the arithmetic ceiling on ALL remaining ME work is `1/(1-0.72)` = 3.6x — enough
to reach roughly veryfast's speed, not to beat it, and we would still be +1.2..+5.8% behind
on BD there. Beating veryfast needs the candidate-COUNT restructure (we evaluate ~468
candidates/MB against x264's single analyse call), not cheaper candidates. U4 — where the
residual 3-8% of bits go at balanced/quality — remains the one genuinely open unknown and
still needs the bit accountant (analyzer instrument #6).

---

## Descent D — into `me-subpel` (41% of encode after A+C)

Re-profiled first, because the bottleneck moves: after Descents A+C the same
profile build went 678.5 -> 444.6 ms (1.53x) and `me-subpel` became 59% of ME /
41% of encode, against `me-diamond`'s 28%.

### D-1 — the ring census, and why its hit rates MISLED

Sub-pel refines with an 8-point ring around a MOVING centre, for steps [2,1],
walked until no improvement. Census by ring position and by iteration:

| | mobile | foreman | football |
|---|---|---|---|
| axis positions improve | 9.6-17.8% | 7.7-15.1% | 9.5-14.3% |
| DIAGONAL positions improve | 0.94-2.1% | 1.7-6.5% | 1.9-3.0% |
| iteration 1 (55% of evals) | 13.1% | 11.2% | 10.8% |
| iteration 2 (35-40% of evals) | **1.5%** | **2.5%** | **2.4%** |

Identical signature to Descent A's coarse rungs — and the conclusion is the
OPPOSITE. Priced on a real 4-QP BD curve (foreman), every work-dropping pattern
LOSES:

| pattern | BD-PSNR | BD-SSIM | ms |
|---|---|---|---|
| ring8 + iterate (anchor) | — | — | 921 |
| ring4 + iterate | **+4.24%** | +4.10% | 844 |
| ring8 single-pass | +2.63% | +2.23% | 871 |
| ring4 single-pass | +7.84% | +7.47% | 712 |

**A low improvement rate is not evidence of low value.** The diamond's coarse
rungs were harmful because a distant jump wrecks MV-field coherence; a sub-pel
diagonal is a legitimate NEARBY position, so the same statistic carries the
opposite meaning. The census hit-rate column predicts nothing on its own — only
the BD curve decides. Recorded so this is not re-litigated.

### D-2 — the redundancy the moving centre creates (LANDED, byte-identical)

Since the ring re-centres on each improvement, iteration N+1's ring necessarily
re-contains the previous centre and several previous ring points. Census of
evaluations that re-price an MV the SAME refinement already priced:

mobile **43.5%**, football **43.4%**, foreman **40.0%**, akiyo **26.6%**.

`cost()` is pure in `mv` (rate from mv-centre; distortion from fixed
reference/source/block captures), so memoizing is EXACT — identical costs,
identical comparisons, identical chosen MV. A miss simply recomputes, so the
table's hit rate is a SPEED property, never a correctness one.

64-entry direct-mapped on the MV's low bits, tagged with the full MV so a
collision is a miss and not a wrong answer.

Deterministic work removed (total `mc_satd` calls, exact run to run):

| clip | OFF | ON | |
|---|---|---|---|
| mobile | 4,775,858 | 3,453,286 | **1.383x fewer** |
| football | 4,490,745 | 3,351,932 | 1.340x |
| foreman | 2,816,897 | 2,124,579 | 1.326x |
| akiyo | 197,021 | 165,500 | 1.190x |

Paired ABBA interleaved (loaded box — sequential timing is void here), 12
rounds/clip: mobile **1.097x (12/12, z=+3.5)**, foreman 1.080x (10/12, z=+2.3),
football 1.068x (12/12, z=+3.5), akiyo 1.066x (8/12, z=+1.2).

**Why 1.07-1.10x and not the 1.33x the eval count implies** — closed, not
hand-waved. Mobile's sub-pel is 64% of all evals; 43.5% of those are redundant =
27.8% of all evals predicted, and 27.7% were measured removed. So the memo
captures essentially ALL the theoretical redundancy and collisions are
negligible. The shortfall is that **the deleted evaluations were the CHEAPEST
ones**: a re-priced MV was just evaluated, so its reference rows were still hot
in L1. Removing cache-warm SATDs saves less than removing average ones. This is
the microbench-vs-in-context law with the sign reversed — the work you eliminate
is not drawn uniformly from the stage's cost.

Gates: byte-identical on 6 clips x balanced/quality; 113/113 suite; 24/24 ffmpeg
pixel-exact.

---

## Descent E — re-probing a prune I made on ONE measurement

Descent B pruned the reconstruction-MC lever on a call-count census (28.3% of
`mc_luma` calls sub-pel => "prize ~1.6%"). The three-probe rule says a refutation
needs more evidence than a confirmation, because nothing revisits it. Re-probed:

### E-1 — the prune was priced on the WRONG DENOMINATOR, twice

**Wrong denominator #1: calls, not time.** A full-pel 16x16 is a row copy; a
quarter-pel is a per-pixel 6-tap. Per-call cost differs ~2-5x, so a call census
cannot price a time lever. Weighted by cycles: sub-pel is **80.4% / 83.8%** of
`mc_luma` time (mobile / foreman), not 28.3%.

**Wrong denominator #2: the population was never identified.** `prof inter-mc`
counted ~17 calls per macroblock while reconstruction needs 1-4, so the stage was
never mostly recon. Tagging call sites:

| site | mobile | foreman |
|---|---|---|
| **search-fallback** | **76.35%** | **78.87%** |
| recon / skip / other | 23.65% | 21.13% |

So the recon lever really is ~1.2% of encode — **prune CONFIRMED, reasoning was
wrong**. The number was right by coincidence. The real finding was the 124,613
search-fallback calls sitting under the same stage name.

### E-2 — REFUTED: the pad is not why they decline

Hypothesis: `hpel_ref`/`hpel_block` decline near the frame edge, so a wider
`HPEL_PAD` would convert fallbacks into plane reads. Swept pad 16/24/32/48/64:
the fallback count is **IDENTICAL at every pad** (mobile 124,613 at 16 AND at
64). Not a bounds issue at all. Recorded so the pad knob is not re-swept for this.

The real mechanism: `hpel_block` returns `false` for FULL-PEL *before* its bounds
check ("the full-pel copy path is already optimal"). Interior full-pel is handled
zero-copy upstream, so what reached the fallback was **edge full-pel** — the
population Descent B dismissed on call counts.

### E-3 — serve edge full-pel from the padded `f` plane (LANDED, byte-identical)

`f` IS the padded, edge-replicated reference, so it reproduces `mc_luma`'s clamp
exactly and can be read in place like any other phase. One match arm.

Deterministic search-fallback `mc_luma` calls: mobile **124,613 -> 8**, foreman
113,279 -> 3,238, football 154,934 -> 17,662.

Paired ABBA, median of paired ratios (the mean is outlier-contaminated — mobile
read "0.873x" on 10 rounds while winning 5/10, which is internally inconsistent;
20 rounds settled it at 1.025x):

| clip | wins | z | median ratio |
|---|---|---|---|
| bus | 20/20 | +4.5 | **1.281x** |
| football | 10/10 | +3.2 | 1.115x |
| foreman | 9/10 | +2.5 | 1.045x |
| crew | 16/20 | +2.7 | 1.016x |
| mobile | 11/20 | +0.4 | 1.025x (neutral) |

Never a regression; neutral at worst. Gates: byte-identical on 8 clips x
balanced/quality (binaries verified distinct by md5); 113/113 suite; 24/24 ffmpeg
pixel-exact.

### E-4 — the process lesson (cost me two false results this descent)

**A failed build silently reuses the old binary, and a byte-identity diff then
compares an artifact with itself and PASSES.** It happened here: a `#[cfg]` guard
I displaced broke the non-profile build, and the gate reported "BYTE-IDENTICAL"
across two runs of the same stale `.exe`. Worse, the shell gate `grep -E "^error"
&& echo OK` has INVERTED polarity — grep exits 0 when it FINDS errors, so it
printed OK precisely when the build failed. Every A/B must (a) fail loudly on a
non-zero build, and (b) prove the two binaries differ (md5) before comparing their
output. This is the third stale-artifact incident in this campaign.

---

## Descent F — the other 23%, and a prune that was wrong at the PIPELINE level

### F-1 — resolve `untagged` (Track A)

After E-3 the site split moved, so it was re-measured rather than re-reasoned.
Tagging the remaining `mc_luma` populations:

| site | mobile | foreman | football |
|---|---|---|---|
| recon | 66.6% | 56.2% | 41.2% |
| skip-check | 33.4% | 35.0% | 23.7% |
| search-fallback | 0.03% | 8.9% | 35.0% |

`skip-check` is **exactly 5,940 calls on every clip** (396 MB x 15 inter frames) —
one per macroblock, content-independent, irreducible in COUNT but not in cost.

Priced against the whole encode — the denominator Descent B never used —
`mc_luma` is **3.8-5.2%**, so the entire remaining stage caps at ~1.05x.

### F-2 — recon + skip-check through the plane cache (LANDED, byte-identical)

`mc_luma_cached` tries `hpel_block`, then the padded `f` plane, then falls back.
Both are proven bit-identical to `mc_luma`. Paired ABBA, 16 rounds, median:
mobile **1.030x** (14/16, z=+3.0), foreman **1.040x** (16/16, z=+4.0), football
1.024x (14/16, z=+3.0), crew 1.035x (15/16, z=+3.5). Byte-identical on 8 clips.

### F-3 — HPEL_PAD 16 -> 32: a prune REVERSED at the level above

Two separate errors, both mine, both instructive.

**(a) The refutation expired when its baseline moved.** The original pad sweep
found the fallback count identical at pad 16 and 64 — true, but measured on a
population dominated by FULL-PEL declines, which E-3 now serves. Re-swept after
E-3, pad 32 removes essentially every remaining fallback (football 17,662 -> 175,
foreman 3,238 -> 0, crowd_run 2,651 -> 0).

**(b) I then pruned it AGAIN on component arithmetic, and that was wrong too.**
The estimate mixed an rdtsc cycle census with profiler milliseconds through an
assumed 3 GHz clock, read a 23-sample build cost off single runs, and never
measured the level above. Predicted: "+0.2% on football, a 5.4 ms LOSS on 1080p."

Measured at the PIPELINE, paired ABBA, median of paired ratios:

| clip | wins | z | median pad16/pad32 |
|---|---|---|---|
| bus | 14/14 | +3.7 | **1.113x** |
| blue_sky 1080p | 6/6 | — | 1.054x |
| park_joy 1080p | 11/12 | +2.9 | 1.050x |
| football | 11/14 | +2.1 | 1.026x |
| foreman | 8/14 | +0.5 | 1.015x (ns) |
| crowd_run 1080p | 8/12 | +1.2 | 1.005x (ns) |

football is 5x better than predicted, 1080p is NEUTRAL not a loss, and bus — the
biggest winner at 1.113x — was never in the component estimate at all. A 6-round
sample had crowd_run at 0.989x; 12 rounds moved it to 1.005x, so the "one negative
clip" was small-sample noise.

Byte-identical at pad 16/32/64: a wider pad grows the planes but never changes a
value read. Default flipped to 32; `RFF_HPEL_PAD=16` restores the old planes.

**The standing lesson (three prunes wrong in one campaign):** component
arithmetic is for deciding what to MEASURE, never for deciding what to SHIP. Every
prune needs one probe at the level above the change. Descent B pruned recon MC on
call counts (wrong denominator); E-2 pruned the pad on a stale baseline; F-3
pruned it again on mixed-unit component math. The conclusions were, respectively,
right-for-the-wrong-reason, expired, and simply wrong.

---

## Descent G — the per-call reframing, and Track A of the ME campaign *(2026-07-29)*

**The matched-tap x264 comparison rewrote this file's closing conclusion.** With both
encoders carrying matching rdtsc stage taps (`video-tests/x264_instrument.py` as our
prof.rs twin), 24f foreman qp27, both `--profile main`, 1 ref, no B, keyint 60,
single-thread, each scaled by its own measured inflation (ours 2.63×, x264 2.37×):
ME is **81% of the whole gap** (58.5 of 72.3 ms), and it is a **per-call** problem —
ours 39,996 searches at **1.68 µs** vs x264's 53,527 at **0.16 µs**. x264 searches
1.34× MORE than we do. This RETIRES the "~468 candidates/MB vs one analyse call"
framing (the counts are comparable at matched settings); the campaign plan is
[lets-win-optimize.md](lets-win-optimize.md).

Track A (byte-identical) results:

- **A6 audit — nothing to wire.** All four ME SATD shapes (16×16/16×8/8×16/8×8)
  already runtime-dispatch `_avx2`; no scalar SATD call site survives on the ME path.
- **A2 (LANDED, byte-identical).** The cost closure re-derived per CANDIDATE what is
  constant per SEARCH: the plane-cache `OnceLock` (twice on the quarter-pel arm), the
  `RFF_HPEL_REF` OnceLock, and the source-row slice base. `mc_satd_hp` takes them as
  parameters, hoisted once per search — same arms, same order.
- **A3 (LANDED, byte-identical) — the accel crate's first NON-vendored kernel.**
  Quarter-pel candidates (~40-50% of sub-pel evals) materialized a 256-byte
  `(a+b+1)>>1` average, then re-loaded it through an FFI SATD. `satd_avg` (Rust AVX2
  intrinsics, `rusty_h264-accel/src/satd_avg.rs`) computes `Σ|H·(src−avg(a,b))|` in
  one register pass (`vpavgb` + 16-bit-lane Hadamard; max coeff 4080 < i16::MAX so
  overflow is impossible). `hpel_qpel_refs` resolves the two plane operands under
  hpel_block's exact guard. The kernel's butterfly is a row-permuted Sylvester H4, so
  `Σ|·|` equals the scalar exactly — pinned by `satd_avg_matches_materialized_scalar`
  (16k random + extremal configs, zero tolerance). Knob: `RFF_SATD_AVG=0`.
- **Gates:** bitstream hashes unchanged on foreman AND mobile, all three presets,
  sequential + GOP-parallel (`e11235654539ba44` etc.); knob-off identical; workspace
  suite green `--features asm`; `--no-default-features` builds clean.
- **Paired ABBA (quality preset, whole encode, base vs A2+A3):**
  foreman **1.048× median, 9/10 wins (z=+2.5)**; mobile **1.07× median, 10/10 wins
  (z=+3.2)**. (The unpaired session walls read 265→100 ms, but untouched `fast` also
  read 139→48 ms — the box was contended at baseline; only the ABBA is valid.)
- **A1/A4/A5 assessed, deferred with reasons:** the per-eval `λ·rate` f64 mul is
  latency-hidden behind the SATD load chain and a per-search table build would cost
  ~what it saves (~1%, under the ABBA floor); seed batching touches 4-5 evals/search;
  the memo's hit rate is unchanged by A2/A3 (only the value of a hit shrank slightly).

**What remains is Track B** — the search restructure (fixed-centre ring passes +
`sad/satd_x4` batch kernels, SAD-for-full-pel with content dispatch), bitstream-
changing, per-clip 4-QP BD-gated, projected ME 67 → ~15 ms against Track A's ~20%.

### G-audit — attribution + tidy pass *(same day)*

The first ABBA measured A2+A3 MIXED; the audit isolated them (mobile, quality):
**A2 alone** (base vs a2 exes) **7/8, median 1.043×**; **A3 alone** (`RFF_SATD_AVG`
off↔on, ONE binary — the cleanest instrument) **8/8, median 1.04×**. Components
multiply to ≈ the combined 1.07×, so the attribution reconciles.

Three self-inflicted issues found by the audit and fixed, all gated byte-identical
(all six clip×preset hashes unchanged, suites green, no-default-features clean):

1. **Duplicated quarter-pel operand table** — `hpel_block` and `hpel_qpel_refs` each
   carried the 12-arm `(fx,fy)→(plane,offset)` match; drift between them would have
   been a silent bitstream change no test pins. `hpel_block`'s quarter arm now
   consumes `hpel_qpel_refs` — ONE table.
2. **`mc_satd` deleted** (~95 duplicated lines). Its only remaining caller was the
   rescue inside `motion_search`, which now reuses the A2-hoisted invariants — and
   thereby gains the fused-kernel path too.
3. **Per-eval OnceLock reintroduced by A3** — `satd_avg_enabled()` fired per
   quarter-pel candidate, exactly the glue class A2 removed. Now hoisted per search
   (`sa_on`), folded under `hr_on` so `RFF_HPEL_REF=0` remains the master
   restore-the-full-copy-path anchor.

Final tidied build vs baseline: 6/8, median 1.046× on a noisier session
(0.989–1.128 spread) — consistent with 1.07× within the visible floor, and the tidy
is deterministically not-more-work than the pre-tidy build.

---

## Descent H — Track B opened: B2 SAD-full-pel (built, calibrated, VERDICT: unfinished dispatch) *(2026-07-29)*

**Built:** `RFF_ME_SADFP` / `set_me_sadfp` — the non-fast search's full-pel phase
(seeds/snap/diamond) prices candidates in the SAD domain, winner + pre-snap seed
repriced in SATD before rescue/sub-pel (x264's split on every preset). Default OFF =
byte-identical (hashes verified). λ for the SAD domain via `RFF_ME_SADL`.

**Three findings, in the order they were forced:**

1. **The naive cut measured 0.58–0.95× — SLOWER — and the rescue was NOT the cause**
   (refuted directly: still 0.77–0.85× with me_wide off both arms). The profiler
   found two real mechanisms: (a) `mc_sad` is the fast preset's function and had
   NONE of the SATD path's accumulated wins — inter-mc fallbacks +61%; (b) sub-pel
   runs +27% longer from SAD-chosen starts (1913 → 2419 ns/search), because our
   ring iterates to CONVERGENCE — x264 pays no such tax since its subme budget is
   FIXED. **Law: swapping a cost metric in one phase re-prices every downstream
   phase that iterates adaptively; budget-bounded consumers are immune,
   convergence-driven ones are not.**
2. **B2.1 (LANDED):** `mc_sad_hp` — the SAD twin of `mc_satd_hp` with the same
   dispatch ladder (in-place strided `psadbw` via the asm kernels' stride args,
   E-3's padded-`f` edge full-pel, fused avg+SAD for quarter phases). Fallbacks
   collapsed 78.7K → 48.8K (= the SATD path's level). Self-check: the B2-on hash is
   IDENTICAL before/after B2.1 (`372f7a908886d77e`) — the parity fix is
   value-exact. Net speed after B2.1: ~0.97× (still not a speed win).
3. **λ recalibration (`RFF_ME_SADL=0.5`, SATD≈2×SAD scale) flipped the BD table**
   — foreman +0.23 → −0.46 — because the lighter rate term lets the diamond chase
   real motion further (bus keeps −3.6 even with the rescue OFF: SAD-fp substitutes
   for much of me_wide's win there).

**The 16-clip truth table (λ=0.5, 4-QP BD-PSNR/SSIM vs SATD-fp anchor):**
wins: bus **−1.71/−1.60**, football **−1.90/−1.83**, foreman −0.46/−0.22,
foreman_qcif −0.19, mobile −0.11/−0.29, akiyo −0.10, shields −0.07, FourPeople
−0.06, stockholm/akiyo_qcif ~0. losses: **crew +0.91/+1.96**, city +0.35/+0.22,
in_to_tree +0.24, harbour +0.19, tempete +0.13/+0.35, soccer +0.07.
Mean ≈ −0.17% but **crew is a real regression → SIGN-FLIP → per the dispatch
principle this ships OPT-IN, not default**, and the next brick is the content
signal separating bus/football from crew/city (high-motion translational vs
complex/chaotic?). Speed is ~0.97–1.0× — B2 today is a BD lever on motion content,
not a speed lever; its speed value unlocks only paired with a BOUNDED sub-pel
budget (which is itself the next speed brick).

**vs x264 (the standing scoreboard, foreman, B2@λ0.5 ON):** quality
**−10.5% vs superfast** (was −9.9%), **+4.0% vs veryfast** (was +4.6%) — the corpus
BD flows straight through the cross-encoder table.

### H-2 — B3 (sub-pel iteration budget) built and priced; the B2 family's verdict

`RFF_SP_MAXIT` / `set_sp_maxit` (0 = unlimited = byte-identical, verified). Priced
alone and paired with B2@λ0.5 (AB_SPCAP, 4-QP BD, 6 clips):

- **Cap alone is not free**: foreman +0.81 (cap2) / +0.43 (cap3) — its sub-pel gain
  is the least ring-1-concentrated (consistent with Descent D). No speed to show
  for it at the paired level.
- **B2+cap3** keeps the motion wins (bus −2.27, football −1.75) at near-neutral
  foreman (+0.03) — but gives back some of uncapped B2's foreman −0.46.
- **Paired ABBA (the honest wall): the SPEED sign-flips with the SAME clips as
  BD.** bus **1.079× (7/8)** — faster AND −2.27% BD, a both-axes Pareto win;
  foreman **0.94 (0/8)**, mobile 0.958 (1/8) — slower for ~nothing.

**Verdict: the B2/B3 family is a content-DISPATCHED tool, not a preset default.**
On fast-translational content it wins both axes simultaneously; on flash/fine-
detail content it loses both. Signal design is the next brick, and one candidate is
already REFUTED by this data: `me_wide_headroom` would misroute crew (headroom 20.0
= high, but B2 reads +0.91 there — SAD overprices the DC shifts of crew's camera
flashes that the Hadamard partially discounts). The signal must separate
translational motion from illumination-change/fine-detail — per-frame mean-luma
delta vs the reference is the first candidate to instrument against the truth
table, per the R6 methodology (3+ candidate signals before choosing the axis).

Knobs shipped (all default-off / byte-identical): `RFF_ME_SADFP`, `RFF_ME_SADL`
(use 0.5), `RFF_SP_MAXIT`; setters `set_me_sadfp` / `set_sp_maxit`; harness modes
`AB_SADFP`(+`AB_SADFP_WIDE`), `AB_SPCAP`.

### H-3 — the B2 dispatcher, built and CLOSED *(2026-07-29)*

Per the R6 methodology: five candidate signals instrumented offline against the
16-clip truth table (`examples/b2_signals.rs`) BEFORE choosing an axis.

- **The axis is `mgain`** — mean `(SAD@0MV − best SAD over a ±8 step-4 grid)/SAD@0MV`
  — the mechanism-true quantity (how much plain translational full-pel search
  improves on zero motion = the surface B2's SAD diamond exploits). Offline it
  separated every meaningful win (bus .323, football/foreman .164/.165, shields
  .361) from every loss (city .110, crew .070, harbour .036, tempete .008).
  `me_wide_headroom` was confirmed unusable (crew: headroom high, B2 loses).
- **Deployed** (recon reference, ~24 sampled MBs/frame, per-frame so deterministic
  under GOP-parallel; both P drivers in lockstep): bus min .185 / football med .208
  / foreman med .164 ON-dominant; city/tempete/mobile/harbour OFF. `RFF_ME_SADT`
  default 0.13.
- **One residual justified the second term** (the truth-table law): dispatched-only
  crew still read +0.54 — its high-mgain motion frames route ON and its FLASHES
  are exactly where SAD misranks (DC-dominated residual the Hadamard discounts).
  The `dcfrac` veto (`|ΣΔ|/SAD@0MV`, computed free inside the same probe):
  deployed, crew's harmful ON-frames read dc **0.843–0.859** vs ≤ **0.478** for
  every good ON-frame on bus/football/foreman — a 1.76× natural gap;
  `RFF_ME_SADDC` default 0.6 sits mid-gap.
- **The final dispatched 16-clip table** (mode 1, λ=0.5): wins kept — bus −1.71,
  football −1.84, foreman −0.44, shields −0.22, stockholm −0.02; every former
  loss at **0.00** (crew, city, tempete, in_to_tree, mobile, akiyo×2, FourPeople);
  residual tail: soccer +0.09/+0.14, harbour +0.06, foreman_qcif SSIM +0.30 (PSNR
  −0.02). **Corpus mean −0.26% (better than force-on's −0.17 — the losses are
  gone), worst clip +0.09 vs force-on's +0.91.**
- **Not yet a default flip:** the monotone bar is worst ≤ 0.00 and soccer's +0.09
  misses it narrowly. Next calibration pass: sweep `RFF_ME_SADT` 0.13→0.17 on the
  deployed estimator (soccer routes ON more than its offline clip-mean 0.071
  suggested; football's min-frame 0.120 bounds the sweep from above). Modes:
  `RFF_ME_SADFP` 0=off (byte-identical, shipped default) / **1=dispatched (the
  recommended arm)** / 2=force (truth-table A/B); `set_me_sadfp_mode`.

★ Instrument lesson, fourth strike: the first "dispatched" corpus run reproduced
the force-on table DIGIT FOR DIGIT — `me_ablation.exe` was stale (built before the
dispatcher existed). An impossible result (crew +0.91 under a gate that routes crew
OFF) was again the cheapest broken-instrument detector. Check the binary mtime
before believing any A/B.

### H-4 — DEFAULT FLIPPED + the sad_x4 fixed-centre diamond *(2026-07-29)*

**The SADT sweep refuted itself, informatively:** at T=0.155 soccer read WORSE
(+0.18 vs +0.09) despite routing FEWER frames ON — a non-monotone response that
marks the +0.09 tail as BD-fit noise, not a real regression (a real effect shrinks
when its cause is removed). T stays 0.13; the flip bar is cleared on that evidence.

**Defaults flipped:** `RFF_ME_SADFP` unset → **mode 1 (dispatched)**, `RFF_ME_SADL`
default → 0.5. `RFF_ME_SADFP=0` reproduces the pre-B2 bytes exactly (verified:
foreman quality `e11235654539ba44`). New default foreman quality hash
`8d5b432bc1d36257` (85,509 B, −198 B at qp27).

**B2-FC + `sad_16x16_x4` (the batch kernel, custom AVX2, `satd_avg.rs`):** on
SAD-routed frames the diamond runs FIXED-CENTRE passes with all four candidates
batched through one kernel call — each source row loaded once, two `vpsadbw` per
row cover four candidates. Argmin-of-4 (first-wins) replaces first-improver
cascade; dispatched-OFF frames never take the path. `RFF_ME_FC=0` anchor. Oracle
`sad_x4_matches_scalar` (24k lanes, exact).

**Fixed-centre IMPROVED every BD win** (argmin beats compounded first-improver):
bus −1.71 → **−2.61**, football −1.84 → **−1.98**, foreman −0.44 → **−0.59**;
crew stays 0.00 (routed OFF). Paired ABBA vs the escape hatch: bus **1.189×
(6/8)** — the first BOTH-AXES win at the whole-encode level — foreman 0.97 for
−0.59 BD, OFF-content ~1.0.

**Gates:** workspace suite green; `conf_ffmpeg` STRICT external conformance
**12/12 pixel-exact** (foreman/bus/football × 4 QPs); escape hatch byte-exact;
sequential == GOP-parallel hashes.

**vs x264 after the flip (quality preset):** foreman **−10.6% vs superfast /
+3.9% vs veryfast** (session start: −9.9 / +4.6); bus **−1.4% vs superfast /
+7.0% vs veryfast**. (Speed columns from this loaded session are junk; the paired
ABBAs above are the speed evidence.)

### H-5 — FC everywhere (satd_x4), the fused hpel builder, and campaign-2 audit *(2026-07-29)*

- **`satd_16x16_x4`** (custom AVX2, reuses the `hadamard4_abs_acc` core): four
  candidates' SATDs with each source band converted to i16 ONCE. FC (fixed-centre
  argmin diamond) extended to SATD-routed frames — gated at mode 0 so its own BD
  effect is isolated (`AB_FC`, `set_me_fc`): **monotone non-regression** — bus
  −1.93, football −0.52, crew −0.22, foreman −0.11, mobile 0.00, tempete −0.02
  (worst residual +0.06 SSIM, noise). The argmin structure itself carries much of
  the B2-family win. Default-on; fast preset untouched.
- **The FULL-RESTORE anchor is now `RFF_ME_SADFP=0 RFF_ME_FC=0`** — verified to
  reproduce the pre-campaign bytes exactly (`e11235654539ba44`).
- **Campaign 3 landed: the fused single-pass hpel builder** (`build_hpel_fused`) —
  x264's `hpel_filter` shape: one row pass writes H, V, C from shared vertical
  i32 intermediates; kills the tile walk's 1.7× redundant halo reads, per-tile
  dispatch, and triple copy-out. BYTE-IDENTICAL (`fused_hpel_builder_matches_tiles`
  across padded-dim geometries incl. partial tiles; tile walk kept as oracle +
  `RFF_HPEL_FUSED=0`).
- **Campaign 2 audited: the zero-block early-outs ALREADY EXIST** (v1+v2 inter
  paths skip dequant/IDCT/recon per uncoded 8×8 quad and for cbp_chroma==0) — the
  remaining T/Q ratio needs the ATTRIBUTION pass, not a quick brick. Recorded so
  it is not re-hunted.
- **Deferred with reasons:** sub-pel ring batching (first-pass-only coverage at
  16×16, quarter-phase avg pairs need satd_avg_x4, dense instrumentation
  interactions — weakest prize/complexity of the set); campaign 7 residue naming
  (open front, needs its own tap session).
- **Gates:** all oracles pass (satd_x4 lanes exact; fused builder byte-equal);
  suite green; `conf_ffmpeg` **12/12 pixel-exact** on the FC-everywhere default;
  paired campaign wall vs the pre-B2 anchor: bus **1.108× (7/8)**, foreman 1.031,
  mobile 0.994 — speed neutral-to-positive while the BD stack is strongly
  positive.

### H-6 — the four open items, hammered *(2026-07-29, goal session)*

**① Re-profile (fresh stage table, quality foreman, deployment defaults):** sub-pel
is STILL the dominant stage (~39-42% of encode; me-cost 60.9 ns in-context over
1.29M evals); diamond ~13%, hpel-read collapsed. And the re-profile caught a
regression of MY OWN: the fused hpel builder ran **2.2× SLOWER than the tile walk**
(950 vs 617 µs/frame) — the tiles call the SSE2/AVX2 `mc_hor20/ver02/centre` asm
kernels, whose throughput beats the fused pass's redundancy savings; an interior
clamp-split did not close it. **Default flipped back to tiles** per
revert-if-not-faster; the fused builder + byte-identity oracle stay in-tree as the
base for a future AVX2 fused kernel (`RFF_HPEL_FUSED=1`). Campaign 3 is therefore
re-OPEN, blocked on that kernel.

**② Residue NAMED:** the (default) CABAC driver was missing the `EncMbLoop` and
`EncEmit` taps the CAVLC twin has. Landed them: the arithmetic-coder emit is
**~7.4% of encode at ~2.3 µs/MB**; per-MB glue outside named stages bounds at ~3%
after the residue-equals-tax discount. The original table's mysterious 14.2 ms
"OTHER" was mostly this untapped entropy + instrument tax.

**③ Sub-pel ring FC (`satd_16x16_x4p` + fixed-centre half-pel pass) — BUILT,
gated, OPT-IN.** Two x4p calls cover the 8-ring (h/h/v/v + c/c/c/c plane reads
from an integer centre); quarter-step and declined passes keep the cascade. Gate:
BD foreman −0.24/bus −0.17 vs football +0.07/crew +0.18 (mixed, not
monotone-clean); paired speed ~1.033× at 4-5/8 — under the floor. Cause is
structural: the batch covers only the FIRST pass (~8 of ~25 evals) and forfeits
memo hits. Ships `RFF_SP_FC` default OFF; the completion path is batching the
quarter-step too (`satd_avg_x4`) so the whole refinement runs fixed-centre.

**④ T/Q attribution — CLOSED, the 5.1× was a costume.** x264's tap
(`x264_macroblock_encode`) excludes entropy (their CABAC write has its own tap,
absent from the original table — it sat in THEIR residue exactly as ours sat in
ours). As shares of each encoder's own time: our enc-inter-code ≈ **6.0%** vs
their mb-encode ≈ **6.6%** — T/Q+recon is at PARITY; the absolute ratio was the
global wall gap. Campaign 2 needs no dedicated bricks; it closes as ME closes.

Standing state: default hash `bd9b405c93dc4fc4` (post-FC-SATD); full-restore
anchor `RFF_ME_SADFP=0 RFF_ME_FC=0` still reproduces `e11235654539ba44` exactly.

### H-7 — sub-pel ripped open: the anatomy, and the completed FC ring *(2026-07-29)*

**The function-level verdict vs x264-veryfast:** per-eval we are at KERNEL PARITY
(half-pel in-place Wels SATD ~15-20 ns ≈ their pixel_satd asm; quarter-pel fused
`satd_avg` ~20-25 ns ≈ same shape; resolve/memo/λ glue ~5-8 ns vs their ~2 ns).
**The entire loss is STRUCTURE: ~24 evals/refined-MV (ring8 × converge × 2 steps)
vs their ~8-9 (4-point × 1 iteration × 2 steps)** — and blanket count cuts are
BD-priced at +2.6..+7.8 (Descent D, stands). The winnable lever is their OTHER
property: fixed-centre batched evaluation.

**Built: `satd_avg_16x16_x4`** (four fused avg+SATDs, source band converted once —
completes the x4 kernel family) and the quarter-step batched arm: every ±1 offset
makes a component odd, so ALL 8 ring candidates are plane-pair averages and two
x4 calls cover the ring; mixed-parity centres decline to the cascade by the
all-Some guard. The WHOLE 16×16 refinement now runs fixed-centre under
`RFF_SP_FC`.

**Gate:** oracle exact (3 kernel tests); default hash untouched. BD (completed
SP-FC): foreman **−0.17/−0.41**, bus **−0.31/−0.55**, worst +0.10/−0.07
(crew, mixed-sign — improved from the half-only +0.18). Near-monotone. Speed:
THIS session's box degraded to ±30%/round — medians foreman 1.145 (5/8), bus
1.006, mobile 0.908 are all inside that floor; **no honest speed verdict is
possible today**. Ships OPT-IN; the flip decision needs one paired run on a quiet
box (`RFF_SP_FC=1`, expect the BD table above to hold deterministically).

### H-8 — the x4 family bridged to every ME partition shape *(2026-07-29)*

The batch kernels covered only 16×16 while the quality preset searches and refines
16×8/8×16/8×8 too. Generalized the whole family — `sad_x4`, `satd_x4` (diamond),
`satd_x4p`, `satd_avg_x4` (ring) — via an h-parametrized 16-wide core plus 8-wide
cores using the proven `[row r | row r+4]` lane packing (SAD's 8-wide core pairs
consecutive rows instead — SAD is row-order-free). One `x4_shape` predicate gates
every wrapper; all four ME shapes now batch in the FC diamond (default) and the
FC ring (`RFF_SP_FC`).

**Gates:** all-shapes oracle (`x4_family_all_shapes_match_scalar`, 96k lane-checks
byte-exact — plain SATD pinned via `reference(src, a, a)` since avg(a,a)=a);
suites green; `conf_ffmpeg` **12/12 pixel-exact**; full-restore anchor
(`RFF_ME_SADFP=0 RFF_ME_FC=0`) still `e11235654539ba44`. New default hash
`92626b659cde023f` (85,351 B — another −365 B vs pre-campaign at qp27).

**FC BD gate with partition coverage** (vs cascade, both mode 0): bus **−2.27**
(16×16-only was −1.93), football **−0.98** (was −0.52), crew −0.29, mobile −0.06,
foreman +0.13 — foreman has flip-flopped ±0.15 across every FC variant (fit noise
around zero); the mean is −0.69 and the motion clips gain outright. Kept
default-on.

### H-9 — the SP-FC flip question CLOSED: a prune, with the mechanism named *(2026-07-29)*

The standing item was "SP-FC is BD-ready, speed needs a quiet box." Today's runs
resolved it — against the flip, decisively (win rates 0-2/12 are outside ANY noise
floor, unlike the earlier inconclusive medians):

1. **BD (all-shapes ring): fully MONOTONE** — foreman −0.35/−0.27, football
   −0.26/−0.23, bus −0.07/−0.25, worst crew SSIM +0.03. The quality bar was
   cleared.
2. **Speed: 10-25% SLOWER** (bus 0.767 median 1/12, foreman 0.904, mobile 0.904).
   First suspect (8-wide batch kernels lose to per-candidate Wels asm) was fixed —
   8-wide shapes now evaluate per-candidate inside the SAME argmin, proven
   value-identical (default `92626b659cde023f` and sp_fc `945cabd59f87f1c4` hashes
   both unchanged) — and the regression REMAINED.
3. **The real mechanism: fixed-centre INFLATES ring evals.** An 8-point argmin
   pass pays 8 evaluations per re-centre; the cascade re-centres mid-ring
   (effective ~4-5 evals/move) and memoizes revisits (27-44%). On a 4-point
   diamond the inflation is small and batching covers it; on an 8-point ring it
   cannot, on any shape mix measured. **Law: fixed-centre argmin is a WIN on
   narrow rings (4-pt diamond: BD-positive at ~neutral speed) and a LOSS on wide
   rings (8-pt sub-pel: BD-positive but 10-25% slower).**

Verdicts: **diamond FC stays default-on** (mean BD −0.69, speed neutral within
today's floor: foreman 1.01, bus 0.951 vs cascade); **ring FC stays OPT-IN**
(`RFF_SP_FC` — a small pure-BD lever for anyone who wants −0.0..−0.35 at ~15%
time). The sub-pel eval-count gap vs x264 remains what the anatomy said: a
BUDGET question (their fixed subme iterations), which is preset-ladder design,
not batching.

### H-10 — the sub-pel effort ladder (`set_subme`), priced *(2026-07-29)*

The eval-count gap vs x264 (~24 vs ~8-9/MV) is now a PRICED BUDGET DIAL, not a
blanket cut. `set_subme(1..=5)` maps one level onto (ring pattern × B3 iteration
budget); env twins `RFF_SUBPEL_PAT` + `RFF_SP_MAXIT`. Priced on 5 clips × 4-QP BD
(deterministic) + the deterministic per-search sub-pel work (foreman, profile):

| rung | shape | mean BD-PSNR (5 clips) | worst clip | subpel work vs subme5 |
|---|---|---|---|---|
| subme5 | ring8 uncapped (quality default) | anchor | — | 1.00× |
| subme4 | ring8 ≤3 iter | +0.20% | foreman/bus +0.33 | 1.07× less — **dominated by subme3** |
| **subme3** | ring8 ≤2 iter | **+0.23%** | foreman +0.53 | **1.92× less** |
| subme2 | ring8 single pass (= balanced) | +0.99% | foreman +2.31 | 2.95× less |
| subme1 | ring4 single pass | +2.75% | foreman +6.98 | 5.70× less |

**subme3 is the star rung:** half the sub-pel cost (≈1.24× whole-encode at
subpel's ~40% share) for +0.23% mean BD — ~4× better pricing than an x264 preset
step (~2%/step). Not made the quality default (foreman +0.53 breaches the
monotone bar); it is the recommended user dial between balanced and quality.
subme4 is recorded as dominated (subme3 gives the same BD for half the work).
Presets unchanged; hashes unchanged at default (subme5 = the defaults exactly).

### H-11 — six-whys on "0.17× vs superfast: are we missing a function?" *(2026-07-29)*

**D6 first (is the comparison like-for-like?): NO — and that is the finding.**
The original 1.68 µs-vs-0.16 µs per-search table compared OUR quality search
(every partition shape, SATD everywhere, converge-to-exhaustion sub-pel) against
x264's budget search. x264 **superfast does not search P sub-partitions AT ALL**
(P16×16 only) and runs subme1-class refinement. The gap is not one missed
function; it is three multiplicative POLICY choices plus one real engineering
residue, now separated by experiment (same-invocation fair ratios, foreman):

| arm | ms/24f | vs superfast | BD vs superfast |
|---|---:|---:|---|
| ours quality (default) | 117.8 | 0.17× (5.9×) | **−10.5%** |
| + `RFF_SPLIT_T=∞` (P16×16-only = superfast's SHAPE) | **65.2** | 0.28× (3.6×) | **−0.9%** (still ahead!) |
| + SAD-fp forced + single-pass sub-pel (≈ superfast's EFFORT) | (walls junk — box collapsed; BD valid) | — | +1.9% |

**The decomposition of 5.9×:**
1. **1.81× = partition searches** — deliberate spend; buys −0.9% → −10.5%. POLICY.
2. **~1.6–2× = per-search effort** (subme5 vs their subme1/2, SATD vs SAD
   full-pel) — POLICY, already dialable (`set_subme`, B2 dispatch); at iso-effort
   our BD advantage over superfast inverts to +1.9%, i.e. the effort EARNS its BD.
3. **~1.8–2× residual = the real engineering gap**: per-eval glue (me-cost
   in-context ~30 ns vs ~15 ns kernel — dispatch match, hpel_ref guards, memo
   tags, λ f64 mul, wrapper asserts, FFI, ×2) × ~1M evals, plus eval-count
   structure. THIS is the codec-eliminate-redundancy target left standing.

**Answer to "are we getting murdered?": we are BUYING.** At superfast's own shape
we are 3.6× slower but still compress better (−0.9%); nobody on x264's fast
ladder dominates our quality point. The murder-looking 5.9× is two priced
policies (partitions ×1.81, effort ×~1.8) stacked on a ~1.8× glue residue.

**Next bricks, routed:** (a) redundancy — the ~15 ns/eval glue anatomy (in-context
vs kernel ns/call, the skip-MC method, on `mc_satd_hp`'s dispatch chain);
(b) adaptive — the partition-split dispatch (U2's harvest exists; the T=400
failure taught the sum-weighted-ceiling trap; the R6 methodology applies: per-clip
truth table for split-gating, content signal = c16/λ percentile per frame);
(c) a `superfast`-class PRESET (P16×16-only + subme2 + SAD-fp force) — measured
today at −0.9%..+1.9% BD vs superfast — would compete on x264's own turf.

### H-12 — the superfast-class rung shipped; the effort cut measured and REJECTED *(2026-07-29)*

`set_turbo(true)` = Quality at superfast's partition shape (P16×16-only,
`RFF_SPLIT_T=∞`), sub-pel + B2 dispatch left at defaults. Fair-run numbers
(foreman): **1.81× faster than default quality, 0.28× superfast / 0.44× veryfast,
and STILL −0.9% BD vs superfast** — competing on x264's fastest turf via
configuration alone.

The fuller composition (+ subme2 + SAD-fp force ≈ superfast's EFFORT) was
measured and REJECTED from the rung: on a box whose drift x264's own arm exposed
(2.4×), the ratios still resolved — 0.27× vs shape-only's 0.28× (no speed gained)
at +1.9% foreman / +8.4% bus BD (quality lost). The effort knobs stay available
for manual composition. Bus-class (split-heavy) content pays +8.4% at this rung —
confirmation that the per-frame SPLIT DISPATCH (H-11 brick b) is the no-tax
answer, not a fixed threshold.

**The remaining gap, now fully accounted:** glue residue (~1.8×, the
codec-eliminate-redundancy anatomy on `mc_satd_hp`'s ~15 ns/eval dispatch chain)
and the split dispatch. Both are specced in H-11; each is a session.

### H-13 — the last two H-11 bricks executed; both CLOSED, one by refutation *(2026-07-29)*

**(b) The split dispatch — BUILT, gated, REFUTED as a free dispatch.** Per-frame
routing on the existing `b2_mgain` probe (near-static frames skip the 16×8/8×16
searches), structurally verified: foreman/bus byte-identical at any sane T (their
min frame mgain 0.061/0.185 ≫ T), akiyo routed off and faster. The BD gate then
killed the premise: **akiyo +2.45% / akiyo_qcif +2.02% / FourPeople +2.00% for
only 1.10–1.15×** — partition splits EARN ~2% BD even on the most static content
measured (low WIN-RATE ≠ low VALUE; the U2 harvest's 17.8% akiyo win-rate was
never the right statistic). This is the THIRD death of the split-gate idea (U2
T=400 bus, the sum-weighted ceiling, now the mgain axis) — recorded as a
REFUTED CLASS: any gate on partition splits is a priced speed rung, never free.
Shipped as opt-in (`RFF_SPLIT_MG`/`set_split_mg`, default 0 = byte-identical;
akiyo hash restored and verified). `set_turbo` remains the honest shape rung.

**(a) The glue residue — CLOSED as quantified papercuts at the byte-identical
floor.** The ~1.8× decomposes into per-eval items of 2–4 ns each (dispatch match,
hpel_ref guards + bounds, memo tags, λ f64 mul, accel wrapper asserts, FFI hop,
×2 scale) against a 15–18 ns kernel — no single item clears the ~5% stage-median
brick floor, and this campaign's graveyard already holds the class's flat bricks
(branchless mvbits, in-place skip, single-SAD AVX2, A1's λ table twice). The
collectible part WAS collected (A2 hoists, A3/x4 fusion, FC batching, B2.1
parity). What remains requires either an unsafe fast-path API in the encoder
crate (against the `forbid(unsafe)` covenant) or further batch-structure — both
priced, neither free. Per "know when a kernel is DONE": the ME per-eval path is
at its safe-Rust floor.

**FINAL STANDING (the gap, fully dispositioned):** 5.9× vs superfast =
**1.81× partitions** (earns −0.9→−10.5% BD everywhere incl. akiyo — buy it or
dial `set_turbo`) × **~1.8× search effort** (earns its BD; dialable via
`set_subme`/B2) × **~1.8× glue** (at the safe-Rust floor; the residual true debt).
Nobody on x264's fast ladder dominates any of our quality-family points.

### H-14 — three more rounds inside ME (user-directed; unsafe now authorized) *(2026-07-29)*

**R1 — two missed functions in the rescue gate, LANDED byte-identical** (foreman
AND bus hashes proven vs a stash-built pre-brick binary):
1. The rw×rh variance pass ran EAGERLY per search while `me_fast=true` made its
   value irrelevant to the gate — now lazy behind the short-circuit.
2. The rescue threshold re-priced `best` with a full extra SATD — `dist` is
   recoverable EXACTLY as `best_c − (λ·rate(best))` (every path assigning best_c
   uses that formula; the B2 reprice guarantees SATD domain).
Wall: flat (median 1.001) — and the WHY is itself a finding: the rescue gate sits
behind `me_wide`, which the R6 headroom dispatcher routes OFF on foreman — the
dead code was already dead there, alive only on bus-class (me_wide-ON) content.
Kept as strictly-less-work.

**R2 — the reconciliation REVISES H-13's "papercut floor" verdict.** Profile-OFF
arithmetic (foreman quality, calm box, wall 98.4 ms): ME ≈ 1.5 µs/search, sub-pel
≈ 1.1 µs of it over ~28 evals ⇒ **~39 ns/eval in-context against a 16-18 ns
kernel — ~23 ns/eval of GLUE, ≈ half of sub-pel, ≈ 25% of ME (~15 ms/24f).**
H-13 called the glue "2-4 ns papercuts" by summing named items; the in-context
measurement says the chain is worth ~23 ns TOGETHER (memo slot+tag, per-candidate
hpel_ref re-resolve + bounds, phase match, closure hop, wrapper asserts, FFI).
Not papercuts — one resolvable dispatch chain.

**R3 — the kernel that collects it, now authorized: `MeCtx` (unsafe in accel,
encoder stays `forbid(unsafe)`).** A per-search context constructed with ONE
validation of the plane geometry (f/h/v/c cover the padded search window), whose
`eval(mv)` does integer bounds + raw-offset phase pick + direct kernel — no
per-eval slice re-derivation, asserts, or match ladder. Prize: ~23 ns × ~1.1M
sub-pel evals ≈ **~15 ms/24f ≈ 12-15% of encode** — the largest single named
lever left anywhere in ME, sized and specced for the next session's build+gates
(scalar-twin oracle, byte-identity, paired ABBA).

### H-15 — MeCtx BUILT and LANDED: the glue chain collected *(2026-07-29)*

`rusty_h264_accel::MeCtx` (accel/src/mectx.rs — `unsafe` stays quarantined there;
the encoder keeps `forbid(unsafe_code)`): a per-search evaluation context that
validates the plane geometry ONCE (plane lengths vs pw·ph, the candidate window
with the quarter-phase +1 slack), chooses the kernel function pointers ONCE
(Wels SATD by shape×AVX2; fused-avg core by width), and whose `eval(mv)` does
only: two shifts/masks, four integer compares, one offset multiply, a phase
pick, and the raw kernel call. Full/half phases read the f/h/v/c planes in
place via the Wels FFI (×2 identity); quarter phases run the fused `satd_avg`
cores; the operand table mirrors `hpel_qpel_refs`. Out-of-window candidates
fall back to `mc_satd_hp` — equal values there, so byte-identity holds
REGARDLESS of which path serves an eval. Knob: `RFF_MECTX=0`.

**Gates:** all hashes byte-identical with MeCtx live (foreman fast/balanced/
quality + bus quality — millions of evals value-proven end-to-end); suites
green; no-default-features clean. **Paired ABBA (one binary, knob):
bus 1.134× (9/10, z=+2.5), mobile 1.088× (9/10), foreman 1.033 median under
wild rounds** — the H-14 R2 prediction (~23 ns/eval ≈ 12-15% of encode) landed
at 9-13% where the box could resolve it.

**Standing after H-15:** the last non-policy factor of the superfast gap is
~collected. The envelope: quality −10.5% BD vs superfast with MeCtx's ~10%
banked; `set_turbo` ≈ 0.30-0.32× superfast at −0.9% BD; the remaining wall
distance to the fast ladder is POLICY (partitions + effort, both priced dials)
plus the non-ME stages (CABAC emit 7.4%, threads).

### H-16 — the CABAC emit ripped open: output path REFUTED as the cost; the coder core is *(2026-07-29)*

**Anatomy:** the emit (7.4% of encode, ~1.9 µs/MB profile-ON) had one glaring
structural suspect — `bits: Vec<u8>` storing ONE BIT PER BYTE (a push per coded
bit from a `Vec::new()` realloc chain, plus a full MSB repack in `into_bytes`).
Rewrote it as a packed byte accumulator (x264's output shape): byte-exact by
construction, every hash unchanged (foreman ×3 presets + bus), round-trip suite
green. **Measured: FLAT** — wall 1.014 median, stage medians overlapping
(1751-1997 → 1853-1910 ns/MB). The bit-Vec was ~1 ns/push amortized and the
repack ~250 ns/MB — "data movement the compiler/allocator already streams is NOT
redundancy" claims its fifth confirmation. KEPT as a byte-identical
simplification (no repack pass, no realloc chain, strictly-not-more work).

**The real cost is the ARITHMETIC CORE:** ~60-80 bins/MB × ~20-25 ns real —
`encode_decision`'s per-bin chain (RANGE_LPS + STATE_TRANS loads, MPS branch,
then `renorm`'s while-loop with a THREE-WAY branch and a `put_bit` PER OUTPUT
BIT). x264's coder does the same spec math at ~5-8 ns/bin via the byte-wise
shape: wider `low`, renorm as ONE `clz` shift, carry deferred through a byte
buffer (the 0xFF chain), bypass bins batchable k-at-a-time because bypass keeps
`range` constant. **Next brick, sized:** the byte-wise coder core rewrite —
~2-3× on the emit stage ≈ 3-5% of encode, spec-exact output (x264 proves the
shape), gated by the existing round-trip suite + hash gates + conf_ffmpeg.
A delicate ~100-line rewrite; one focused session.

### H-17 — pred-buf opened: the runtime-width copy trap, four more instances *(2026-07-29)*

Campaign 5's stage (PRED-BUF, ~4.3% real, x264 equivalent = zero) decomposed: the
scope wraps the chosen MB's final prediction build (per-partition MC + MV
prediction + motion-grid commits) — mostly REAL planning work, EXCEPT the
partition re-stride: non-16×16 partitions MC into a tmp then re-strided
**per-pixel** into `pred_y`/`c_pred` — a bounds-checked store per pixel, the
exact runtime-width codegen trap the skip-MC brick already beat (16× there).
Fixed with const-width row copies (8/16 luma, 4/8 chroma) at all FOUR sites
(the v2 CAVLC path AND the default `plan_inter_mb` path — the two-driver
lockstep rule again). Byte-identical: all hashes unchanged (foreman ×3 + bus),
suites green. Wall unmeasurable this session (box 2× degraded); kept as
strictly-less-work per the skip-MC precedent. The stage's remainder is real
per-MB planning — no copy left to elide.

### H-18 — the three queued items, executed *(2026-07-29)*

**(2) Quiet-box re-baseline — BLOCKED, reported not guessed.** The box never
settled (best-of-7 spread 1.50; the x264_bdrate run read OUR quality at 174 ms
where the same binary read 118 ms earlier — and x264's OWN arms fell 121→101
Mpx/s in the same run, which is the tell). Cross-session walls are invalid here
by standing rule, so the banked bricks (MeCtx, H-14/16/17/18) remain WEIGHED
INDIVIDUALLY (each paired-ABBA'd or proven strictly-less-work at landing) and
UNWEIGHED COLLECTIVELY. The deterministic half is unchanged and valid:
**quality −10.5% BD vs superfast, +4.0% vs veryfast** (all bricks byte-identical
by construction, so BD cannot have moved). One quiet session owes the wall
column — the arithmetic expectation is quality ≈ 100-105 ms/24f.

**(3) `enc-inter-code` decompose — ALREADY CLOSED by the H-6 taps; verified.**
15.2 ms profile-ON = pred-buf 8.8 (58%) + T/Q 2.9 (19%) + recon 2.4 (16%) +
1.1 residue (**7%**). Not an unopened blob: it is three named children, each
already dispositioned — pred-buf's copy traps fixed (H-17), T/Q proven at
PARITY with x264's mb-encode by share (H-6), recon on the asm IDCT. The 7%
residue is per-MB glue below the brick floor. Item closed with no brick owed.

**(1) CABAC core — first brick landed; the byte-wise rewrite re-scoped.** The
461-entry context table was a heap `Vec` indexed 60-80×/MB: a pointer load +
bounds check per bin, with `encode_decision` indexing it up to THREE times.
Now an inline `[(u8,u8); 460]` accessed through ONE slot borrow for
read+write-back. Byte-identical (all hashes, round-trips, suites green).
The wide-`low` byte-wise engine (H-16's spec) stays queued as its own session:
it is a ~200-line port whose delicate parts are the first-byte suppression, the
0xFF carry chain, and flush alignment — and this session's box cannot resolve
its verdict, so building it blind would violate the measure-before-keep rule.

### H-19 — THE BIT ACCOUNTANT built (analyzer instrument #6) and its first read *(2026-07-29)*

The last unbuilt instrument from the original plan (U4 specced it; nothing ever
built it). `bitacct.rs` + `CabacEncoder::pos()`: buckets are EXACT deltas of the
coder's emitted-bit position, so they sum to the real payload — the property that
separates an instrument from a model. Observe-only, env/API-gated
(`RFF_BITACCT`/`set_enabled`), byte-identical when off (verified).

**First read — foreman qp27, quality, 24f (P-frame syntax; 9108 P-MBs tapped):**

| element | share | bits/MB |
|---|---:|---:|
| **residual luma** | **63.0%** | 37.5 |
| **mvd** | **19.5%** | 11.6 |
| cbp | **6.8%** | 4.1 |
| mb_qp_delta | **3.8%** | 2.3 |
| residual chroma | 3.1% | 1.9 |
| mb_type/sub_type | 2.5% | 1.5 |
| intra MBs (whole) | 0.6% | — |
| mb_skip_flag | 0.6% | 0.4 |
| ref_idx | 0.0% (1 ref) | — |

**x264-comparable rollup: MOTION 19.5% · TEXTURE 66.1% · MISC 14.4%.**
Reconciliation **82.8%** — and the residue is NAMED, not mysterious: the IDR
frame's 396 MBs go through the I-slice emitter, which has no taps yet (the tapped
element counts prove it: mb_skip fired 9108 = 23 P-frames × 396 exactly), plus
slice headers/NAL/flush.

**The lead this opens:** our MISC at 14.4% is high against x264's typical
~8-10% for P-frames, and it decomposes into two specific items —
**cbp 6.8%** and **mb_qp_delta 3.8%** (the AQ signalling tax, 2.3 bits/MB
on every coded MB). Together that is ~10.6% of the payload on syntax that is
neither motion nor texture. Next measurements, in order: (1) tap the I-slice
emitter to close reconciliation to ~99%; (2) price the AQ signalling — 4-QP BD
with `aq_strength=0` (AQ was validated on SSIM, never on its RATE cost);
(3) compare cbp/mb_type context modelling against x264's at a matched point,
since the syntax is identical and only the contexts differ.

### H-20 — ★ THE ACCOUNTANT'S FIRST KILL: we already BEAT veryfast; the "+4%" was a metric-tuning artifact *(2026-07-29)*

**(1) I-slice + terminate taps landed → reconciliation 82.8% → 99.7%.** The
residue is now 1,742 bits over 24 frames (slice headers + NAL + flush) — the
instrument is sound. Full P+I split: residual luma 52.3%, **intra MB body 17.3%**
(417 MBs — the IDR frame is 1.7% of MBs but 17% of BITS), mvd 16.2%, cbp 5.7%,
mb_qp_delta 3.2%, chroma 2.6%, mb_type 2.1%, skip 0.5%, end_of_slice 0.1%.

**(2) AQ's rate priced — and it exposed a MEASUREMENT bias, not just a cost.**
4-QP BD, anchor = AQ ON (default), 4 clips:

| clip | BD-PSNR (AQ off) | BD-SSIM (AQ off) |
|---|---:|---:|
| foreman | **−5.50** | +7.92 |
| akiyo | −2.57 | +8.37 |
| mobile | −2.20 | +7.00 |
| bus | −1.84 | **+15.99** |

AQ *earns* its 3.2% signalling tax many times over on SSIM (+7…+16%) and *costs*
1.8–5.5% on PSNR — the textbook AQ trade, now measured on OUR encoder for the
first time (it shipped validated on SSIM alone; its rate side was never priced).

**★ The consequence, measured, not inferred:** `x264_bdrate` scores **PSNR**, and
our default is SSIM-tuned, so every headline gap in this document was reading a
tuning difference as a deficit. Same harness, `XB_AQ=0` (foreman):

| | AQ on (default) | **AQ off (PSNR-matched)** |
|---|---:|---:|
| vs x264 superfast | −10.5% | **−15.4%** |
| vs x264 veryfast | **+4.0%** | **−1.6%** |

**We do not trail veryfast on compression — we beat it by 1.6% BD-PSNR at a
PSNR-matched configuration, and beat superfast by 15.4%.** The campaign's
standing "+4% behind veryfast" was our own perceptual tuning being scored by a
fidelity metric. Both numbers are true; they answer different questions, and the
document must now say which. Standing rule adopted: **quote BD with its metric
AND the tuning state of both encoders, or the number is not comparable.**
Follow-ups: report the SSIM-scored table too (x264 `--tune ssim` on their side is
the matched arm); leave the SSIM-tuned config as the shipped default — it is the
better encoder for humans, and now we can say so with the PSNR number in hand.

### H-21 — item 3: our syntax overhead measured against x264's, like-for-like *(2026-07-29)*

Instrumented x264 itself (`_ref_x264/encoder/encoder.c`, guarded `#ifdef
X264_PROF` so the stock throughput binary stays untapped — it already buckets
`i_mv_bits`/`i_tex_bits`/`i_misc_bits`, so the patch only totals and prints
them). Matched run: veryfast, `--profile main --qp 27 --keyint 60 --ref 1
--bframes 0 --threads 1 --frames 24`, foreman.

**x264: MOTION 21.5% · TEXTURE 75.7% · MISC 2.8%** (total 469,168 bits).

**★ The definitions differ and that matters** — x264's `i_mv_bits` is
`pos(texture start) − pos(MB start)`, i.e. ALL non-residual MB syntax
(mb_type + ref + mvd + cbp + mb_qp_delta), not just motion; its `misc` is
headers/NAL. Mapping our buckets onto that definition:

| class (x264's definition) | ours | x264 |
|---|---:|---:|
| non-residual MB syntax | **27.7%** | **21.5%** |
| residual/texture | ~72.2% (incl. intra body) | 75.7% |
| headers/NAL/terminate | ~0.4% | 2.8% |

**We spend ~6 percentage points more of the payload on non-residual syntax than
x264 at a matched configuration — ~29% more, proportionally.** And the share
comparison UNDERSTATES it: our stream carries more total bits at this QP (682k
vs 469k, at higher PSNR), which should *dilute* a fixed syntax cost, not inflate
it. Inside our 27.7%: mvd 16.2%, **cbp 5.7%**, **mb_qp_delta 3.2%**, mb_type
2.1%, skip 0.5%.

**Next bricks (bitstream-legal — identical syntax, different context choices):**
(a) audit our `cb_cbp` ctxIdxInc derivation against the spec's neighbour rule —
5.7% on a 4-bin element is the outlier; (b) the mvd context/bypass split
(16.2% on 1 ref); (c) split our intra-body bucket into modes vs residual so the
texture line is exact. Each is measurable with the accountant now in place.

### H-22 — the three bitstream-legal bricks: one REFUTED, one measured, one partial *(2026-07-29)*

**(a) `cb_cbp` ctxIdxInc — AUDITED against spec 9.3.3.1.1.4: CORRECT. Refuted as
a defect.** Each luma bin's (A,B) neighbour pair verified individually:
bin0 → (left MB blk1, top MB blk2) = `l(1<<1) + 2·t(1<<2)`; bin1 → (own blk0,
top MB blk3); bin2 → (left MB blk3, own blk0); bin3 → (own blk2, own blk1) —
all matching the spec's "condTermFlagN = 1 iff the neighbour 8×8's CBP bit is
zero", with unavailable neighbours correctly yielding 0 and skipped MBs
correctly carrying cbp 0. Chroma bins likewise. So the 5.7% is INHERENT cost of
a 4-6 bin element, not mis-modelling — and the corroborating evidence was
already on the record: `conf_ffmpeg` decodes us pixel-exact, which a wrong
ctxIdxInc could not survive. **A clean audit result: the outlier was a red
herring; do not re-open it.**

**(b) mvd context-vs-bypass split — MEASURED, and it points back at the SEARCH.**
New sub-bucket: the UEG3 suffix + sign bit are bypass-coded (incompressible by
construction). Foreman qp27: **32,099 of 110,403 mvd bits = 29% are BYPASS**
(3.4 bits/MB, 22,838 bypass strings). So nearly a third of our motion cost is
not a context-modelling question at all — it is vector MAGNITUDE: values ≥8
quarter-pels overflow the context-coded prefix into the exponential-Golomb tail.
**That re-frames the 16.2% mvd share as a motion-field problem, not a coder
problem** — and connects to the ME campaign's open question about MV-field
coherence (Descent A found coarse diamond rungs harm neighbour predictors). The
actionable child is an MV-cost/λ study or a predictor improvement, NOT entropy
work.

**(c) intra-body split — bucket added, tap PARTIAL.** `IntraResid` exists and is
excluded from the additive total, but the tap inside `emit_intra_body_cabac`
(around its luma-DC/AC + chroma residual calls) is not yet placed, so the
printed "non-residual syntax 45.0%" still counts intra residual as syntax. The
corrected figure from H-21's arithmetic (intra body excluded) stands at **~27.7%
ours vs 21.5% x264**. Finishing the tap is ~10 lines at the three `cb_residual`
call sites in the intra body and makes the texture line exact.

### H-23 — mvd's bypass tail: three iterations to root cause *(2026-07-29)*

**Iteration 1 — DECOMPOSE. The tail is not what its name suggests.** Split the
bypass bucket by mechanism (foreman qp27, 110,403 mvd bits total):

| component | bits | strings | reading |
|---|---:|---:|---|
| **sign bits** | **20,447** | 20,447 | one per NON-ZERO mvd component |
| EG3 suffix (|d| ≥ 9 qpel) | 11,652 | 2,391 | genuinely large vectors |
| context-coded prefix | 78,304 | — | ~3.8 bits per non-zero component |

So "large vectors" are only 1.7% of the payload; the dominant cost is the
**COUNT of non-zero mvd components — 20,447 of them, each dragging sign + prefix
≈ 4.8 bits ⇒ ~14% of the entire bitstream is "our vector differed from its
predictor".** The question was never the tail; it is how often we leave the
predictor.

**Iteration 2 — λ: REFUTED as the cause.** `cabac_lambda_scale` has always been
1.0 (the CAVLC-era value) so under-pricing motion was the obvious suspect. 4-QP
BD sweep: ×1.5 → −0.17/+0.10/+0.09, ×2.0 → −0.15/−0.51/−0.05, ×3.0 → **+0.60/
+0.52/−0.13** (foreman/bus/akiyo). Optimum is shallow and near 1.0–2.0, and 3.0
clearly degrades. **λ is already calibrated; the rate term's WEIGHT is not the
defect.** (Recorded so it is not re-swept.)

**Iteration 3 — the SHAPE is the defect.** λ scales the model uniformly; it
cannot fix a model that is FLAT where it should rise. Our `mvbits` is the
Exp-Golomb length `1 + 2·floor(log2(codenum+1))` — a STEP function, constant
inside each power-of-two bracket, so the search prices d=4 and d=7 identically
and takes the far end of a bracket for free. x264 deliberately uses a smooth
curve, `2·log2(|d|+1) + 0.718 + (d≠0)`. Implemented it as a table (4× resolution
folded into λ so only SHAPE changes), knob `RFF_MVCOST` / `set_mv_smooth`,
default OFF = byte-identical (verified).

**4-QP BD, smooth vs step:** bus **−1.31**, football **−0.24**, mobile −0.01,
foreman +0.23, akiyo +0.11. **Mean −0.24%, and a clean SIGN-FLIP whose sides are
physically meaningful**: the winners are the high-motion clips (where |mvd| is
large and the flat brackets bite), the losers are low-motion (where every vector
sits in the first bracket and the smooth curve only adds noise). Per the
governing principle a sign-flip is a DISPATCH, not a keep-or-prune — and the
signal already exists in the encoder: the per-frame `b2_mgain` motion probe that
routes B2. **Ships OPT-IN now; the dispatch is the next brick, and it is
expected to bank the bus/football wins at zero cost elsewhere.**

### H-24 — the mv-cost dispatch: built, and foreman is the recurring boundary *(2026-07-29)*

Routed the smooth-vs-step mv-cost shape on the SAME per-frame `b2_mgain` probe
that routes B2 (mode 1; `set_mv_smooth_mode` / `RFF_MVCOST`, `RFF_MVCOST_T`).
Default 0 = Exp-Golomb step = byte-identical (verified).

| clip | force-on (H-23) | **dispatched T=0.10** | dispatched T=0.19 |
|---|---:|---:|---:|
| bus | −1.31 | **−1.31** | −1.23 |
| football | −0.24 | **−0.24** | −0.27 |
| mobile | −0.01 | **+0.00** (routed off) | — |
| akiyo | +0.11 | **+0.00** (routed off) | — |
| foreman | +0.23 | **+0.18** | +0.16 |

**The dispatch does exactly what it should on 4 of 5 clips**: akiyo and mobile
route OFF to an exact +0.00, bus and football keep their full wins. **Mean
−0.27%.** But **foreman does not clear at any threshold** — its harmful frames
sit ABOVE the cutoff (median mgain 0.164, max 0.238, overlapping football's
0.208), so raising T sacrifices bus's win faster than it recovers foreman.
mgain separates "is there motion" but not "does this frame's motion leave the
first mvd bracket", which is the quantity that actually decides the shape's
value.

**Verdict: ships OPT-IN (mode 1), default unchanged.** Mean −0.27% is real, but
so is foreman's +0.16-0.18 — it is consistent across thresholds and variants,
so unlike H-8's ±0.15 flip-flop it cannot be written off as fit noise, and the
monotone-non-regression bar is the bar. **foreman has now been the boundary clip
for THREE dispatches (B2, split, mv-cost) — the standing signal family (mgain,
head-room, dcfrac) is exhausted on it; the next dispatch attempt here needs a
signal derived from the mvd DISTRIBUTION itself** (e.g. the lookahead's own
predicted-|mvd| histogram — available pre-encode, and it measures the exact
quantity the cost shape trades on).

### H-25 — the measured mvd cost table, and foreman's TRUE root cause *(2026-07-29)*

**Shipped the H-24 dispatch DEFAULT-ON first** (owner's call: mean −0.27% over
minimax; `RFF_MVCOST=0` restores the prior bytes exactly, verified; conf_ffmpeg
8/8; new quality hash `d656214626e60fa8`).

**Then replaced guesses with measurement.** New harvest instrument: average REAL
CABAC bits per |mvd| component from the production emitter (`add_mvd_sample`,
`BA_MVDTAB`). The truth, merged over 5 clips: d=0 → **1.11** bits, d=1 → 2.85,
d=4 → 5.83, d=16 → 7.49. Both analytic models are wrong in OPPOSITE directions:
the step model's leave-the-predictor delta is 2.0 and its mid-range (d=4..7) is
overpriced ~1.2 bits; the smooth curve's leave delta is **3.0 vs a true 1.74** —
it makes leaving the predictor look ~2× costlier than reality, which is exactly
why foreman (small, frequent vectors) lost under it. Baked the measured curve as
`MVD_TRUE_COST4` (mode 3, EG3-slope extension past |d|=64; regenerate via the
harvest, never hand-edit).

**Verdict: mode 3 ≡ the dispatch on every clip** (bus −1.31, football −0.24,
akiyo/mobile EXACT 0.00 — argmin-stable — foreman +0.18). **And that equality is
the finding: foreman still loses under the TRUE costs, so its residual is NOT
model shape.** With truth available, foreman *prefers the step's over-pricing* —
i.e. its optimum sits ABOVE per-vector truth. That is the MV-FIELD COHERENCE
effect (Descent A's law, resurfacing): cheaper vectors → a more diverse field →
every neighbour's median predictor degrades → downstream mvds grow. A per-vector
cost model CANNOT express a field-level externality. **The real foreman fix is a
search-side coherence term (price a candidate's expected damage to its
neighbours' predictors), not any cost curve** — specced as the next brick class;
the dispatch (default-on) remains the correct shipping shape meanwhile, since it
buys the field-sensitive content exactly nothing and the motion content −1.3%.

### H-26 — the coherence bias explored; the shipped dispatch hardened *(2026-07-29)*

**Instrument bug found by its own impossible result:** the first bias sweep
returned IDENTICAL tables — `mv_smooth_mode()`'s match passed only `0..=2`, so
"mode 3" had been silently running as the dispatch (including H-25's mode-3
verdict). Fixed; the truth table's REAL first test: bus **−1.46** (better than
smooth), football −0.07, foreman +0.24 — the field-coherence inference SURVIVED
its honest test.

**The bias sweep (truth + b bits on d≠0):** foreman reaches noise-zero at b=1.0
(+0.04/−0.13) — the field externality's average is real and ~1 bit — but the
variants (smooth / truth / truth+1.0) shuffle within **±0.2 BD fit-noise** of
each other under dispatch (bus prefers truth, football prefers smooth, nothing
dominates). Shipping a lateral move on noise violates the doctrine, so **the
dispatch keeps its originally-gated smooth ON-model**; the measured and biased
tables stay as research modes 2/3 (`RFF_MVCOST_BIAS`).

**Two hardenings shipped:** (1) the mode-range fix; (2) the **dcfrac flash veto
on the mv-cost route** (crew's flash frames were routing ON in the shipped
config; now crew reads +0.00/−0.03, bus regated −1.31 unchanged, escape hatch
byte-exact). **The foreman/crew knot is proven threshold-inseparable on mgain**
(foreman min-frame 0.061 vs crew median 0.059 — 0.002 apart): the H-24
conclusion stands sharpened — the next dispatch signal here must come from the
mvd DISTRIBUTION itself, and the harvest instrument to build it now exists.

### H-27 — the mvd-distribution signal: VALIDATED offline, its online deployment REFUTED *(2026-07-29)*

**The signal is right.** Per-clip truth (from the emitter harvest): mean |mvd|
among non-zero components separates the mv-cost verdicts CLEANLY — losers
foreman 4.13 / akiyo 1.51 / mobile 2.21 vs winners bus 6.72 / football 8.61
(crew 5.95, flash-confounded, veto territory). T≈5 splits every clip correctly —
the axis mgain could not resolve (H-24/H-26: foreman min 0.061 vs crew med
0.059).

**The deployment failed, structurally.** Built the rdskip-precedent shape:
learn mean|mvd| from the frame's first ~192 search estimates, freeze the route
for the rest (within-frame ⇒ GOP-parallel-deterministic). 7-clip BD INVERTED
every expectation: expected winners bus +0.14 / football +0.27 / crew +0.28 all
LOSE; foreman −0.22 wins. **A mid-frame COST-MODEL switch changes the MV field's
character mid-reference — the reference-chain disturbance dominates the model
choice in both directions.** rdskip's online gate works because a per-MB skip is
local; a cost-model swap is global to every subsequent predictor. REVERTED to
the H-26 shipped config (probe-routed, frozen before the frame; verified
`d656214626e60fa8`).

**The surviving path for the signal:** it must be known BEFORE the frame's first
search — i.e. from the LOOKAHEAD's motion estimates (the module exists; mbtree
already runs block MC there). Recorded as the deployment spec; the offline
validation above is the calibration target (T≈5 qpel on coded-mvd statistics).

**Mission 2 (coherence term) closed with its measurement:** the scalar form
(+1.0 bit on every d≠0 — `RFF_MVCOST_BIAS`, mode 3) is the implemented term;
b=1.0 neutralizes foreman (+0.04) and the H-26 sweep showed all richer variants
shuffle within ±0.2 fit-noise on this corpus — a finer field term cannot be
resolved by 24-frame CIF BD and awaits a bigger corpus, not more code.

### H-28 — the byte-wise CABAC engine: RE-PRICED and PRUNED on arithmetic *(2026-07-29)*

H-16 sized the engine rewrite at "2-3× the emit ≈ 3-5% of encode" from the
pre-anatomy view that the renorm loop dominated the per-bin cost. Two facts,
both now in evidence, retire that estimate:

1. **The renorm loop body averages <1 iteration per bin.** Shifts per bin =
   emitted bits ÷ bins ≈ 683k ÷ ~720k ≈ **0.95** (CABAC near 1 bit/bin at these
   QPs). The clz-batched renorm collapses a loop that already runs ~once —
   saving loop-ENTRY mechanics (~3-5 ns/bin), not a hot loop.
2. **The two flat bricks already taken on this path** (H-16 packed output: flat
   at wall AND stage; H-18 inline ctx array: below floor) — and the campaign's
   own law: *two flat bricks in a row on a mature path = stop; the remaining
   cost is inherent.*

Revised prize: ~3-5 ns × ~720k bins ≈ **1-1.5% of encode** — under every floor
this box has shown all day, and under the brick bar even on a calm one. A
delicate ~200-line carry-chain rewrite for a sub-floor prize fails the
measure-before-keep test by construction. **PRUNED with the arithmetic recorded;
the emit's ~20-25 ns/bin is the adaptive-context state machine itself (table
loads + data-dependent branches), which is INHERENT to CABAC** — the reference
encoders pay it too (x264's cabac_encode_decision is the same shape, merely
~2× leaner per the H-6 share comparison, which the ctx-array and packed-output
bricks already narrowed).

**The three missions, dispositioned:** (1) mvd-distribution signal — validated
offline (T≈5 splits every clip), online deployment refuted structurally,
lookahead deployment specced with its calibration target; (2) coherence term —
implemented and measured (~1 bit, mode 3 / `RFF_MVCOST_BIAS`), finer forms
unresolvable in this corpus's fit-noise; (3) byte-wise engine — re-priced to
1-1.5% and pruned by the two-flat-bricks law. The entropy path is CLOSED as a
campaign front; the open fronts that remain are the lookahead-deployed mvd
signal and the calm-box wall re-baseline.

### H-29 — both closing items resolved: the signal's grain law, and THE RE-BASELINE *(2026-07-29)*

**(1) The mvd-distribution signal: deployment REFUTED at frame granularity —
now a five-experiment LAW.** Built two emit-stat deployments (per-GOP cumulative,
then per-frame window with degenerate-first-frame and flash guards). Both
inverted on boundary content: GOP-cumulative mis-routed foreman's early frames
(fresh-IDR predictors inflate mvds → +0.42); the per-frame window flip-flopped
foreman's straddling frames (+0.43) while bus (−1.52..−2.16) — entirely one side
of T — gained. Adding H-24/H-26/H-27: **every frame-granular routing of the
mv-cost model (probe, online, GOP-stats, frame-stats) pays a reference-chain
mixing tax on threshold-straddling content that exceeds the signal's benefit.
The signal is CLIP-valid, FRAME-invalid; the correct grain is per-encode**
(API knob — `set_mv_smooth_mode(2/3)` exists — or a 2-pass decision). Reverted
to the shipped probe config (verified `d656214626e60fa8`). The shipped dispatch
remains the least-bad frame-granular form (foreman +0.18, the mixing floor).

**(2) THE RE-BASELINE — calm box (spread 1.06-1.09, best-of-7), everything
banked, fair same-run arms:**

| | campaign start | TODAY | movement |
|---|---:|---:|---|
| ours/quality wall | 98.3 ms/24f | **87.0 ms** (90.5 best-of-7 seq) | **−11.5%** |
| vs superfast (SSIM-tuned default) | −10.5% BD @ 0.17× | **−10.3% BD @ 0.18×** | BD held through ~30 bricks |
| vs veryfast (SSIM-tuned default) | +4.0% @ 0.27× | +4.2% @ **0.29×** | — |
| **vs veryfast, PSNR-matched** | (unmeasured then) | **−1.6% BD @ 0.30×** | **we win compression** |
| vs superfast, PSNR-matched | — | **−15.3% BD** | — |
| turbo rung (`set_turbo`) | n/a | −0.9% BD @ ~0.55× superfast-shape wall | competes on their turf |

The banked bricks (MeCtx 9-13%, H-14 rescue, H-17 re-strides, H-16/18
simplifications) weigh in at **−11.5% wall** while every BD column held or
improved — the campaign's speed work was byte-identical by construction and the
re-baseline proves it end-to-end. Standing: **no x264 fast-ladder point
dominates any of ours; PSNR-matched we out-compress veryfast outright at 0.30×
its speed; the remaining wall multiple is priced policy (partitions, effort)
plus x264's half-decade of asm.** This function is CLOSED.

## Descent H-30 — threading: the 3.5-4× that was already built, and the race it was hiding

- ASKED (Tim): do 2-pass routing / threading improve speed without compromising
  compression?
- SHAPE: they are opposites. **Threading** = pure speed, compression untouched
  *by construction* (GOP-parallel `encode_all` must be byte-identical to
  sequential). **2-pass routing** = a compression lever that *costs* a pass —
  it cannot improve speed; its prize is the per-encode mv-cost grain (H-29's
  law), worth ~0.1-0.3% corpus mean over the shipped dispatch.
- MEASURED (thread_bench, foreman 120f, gop30 = 4 GOPs): balanced 3.6×,
  quality ~3.5-4× (spread 3.3-6.6 on a drifting box; cap = min(cores, GOPs)).
  x264's own frame-threading on the same short clip: superfast 1.9×,
  veryfast 2.6× — threaded-vs-threaded the wall gap NARROWS (quality ~350 ms
  vs veryfast ~150 ms ≈ 2.3×, from 3.3× single-threaded).
- ★ THE CATCH: the identity assert FAILED on the quality preset. The H-24
  mv-cost routing decision lived in a process-global `AtomicBool`
  (`MV_SMOOTH_FRAME`) — safe sequentially, RACING across GOP workers. The
  campaign's hash gate never saw it because `encode_hash` runs a single GOP →
  one worker → no interleaving. **Law: a "per-frame" value in a process-global
  is a latent race the moment two frames are in flight; the seq==parallel gate
  is only as strong as the GOP count it runs with.** Fix: the decision rides
  the per-frame state next to `sadfp` (same probe, same lifetime). Sequential
  bytes proven unchanged pre==post (quality `ccea36ed3534d50c`); quality
  seq==parallel across repeated multi-GOP runs; multi-GOP quality-preset
  regression test added. Commit `16e9327`.
- STATUS: threading VERIFIED as the free speed lever (was built, now actually
  safe); 2-pass routing correctly classified as compression-side future work.

## Descent H-31 — the six-target hammer (side-by-side exploration list, all dispositioned)

**dec #1+#2 MERGED AND LANDED (@a54195a): the "CABAC residue" was an MC call
storm, and coalescing it is 1.86× on real-world streams, byte-identical.**
Naming taps (Entropy on `parse_residual_cabac`, Syntax on mvd/type/cbp parsers)
refuted the CABAC hypothesis: residual parse = 3.3%, syntax = 1.1% of decode.
The residue was 2.41M MC kernel entries: the CABAC P recon loop paid 48
calls/MB regardless of partitioning, and spatial B-direct paid one bi-pred
`b_mc` per 4×4 (~96 entries per direct MB) though colZeroFlag admits only two
motion values. Both merged via partition-shaped rect coalescing (the filters
are per-output-pixel → BIT-identical; YUV cmp proved it on x264 + own
streams). MC entries 2.41M → 263k (9.2×); ABBA 4/4 rounds 1.86-1.89×.
LAW: name the residue BEFORE attacking it — the attack plan changed entirely
(entropy engine work would have returned ~nothing).

**enc entropy-emit PRUNED (D6 artifact, 3 probes):** per-MB 1750 vs their
1455 ns (1.2×, not 3.2×); per-BIT throughput parity (~6 MB/s both); writer
already openh264-shaped. The side-by-side's 3.2× = 1.67× more non-skip MBs
(their mbtree buys skips) × ~2× bytes at matched QP (CAVLC density). An emit
"gap" that is actually upstream compression, not emit speed.

**enc sub-pel CLOSED as policy:** per-eval at kernel floor (~70ns prof-taxed,
memo+FC+MeCtx banked); the residual per-search ratio is EVAL COUNT — our
walk-to-convergence vs their fixed subme budget. That count IS the BD edge
(−1.6% PSNR-matched); the cap is already shipped as the BD-gated subme
ladder + SP_MAXIT rungs. No byte-identical work left above noise.

**enc hpel builder LANDED (@4e2c825): AVX2 fused single pass, build 1.66×,
byte-identical** (oracle-extended; hash unchanged all arms). Bucket 52.9 →
31.8 ms; the filter portion ~2.7×; remainder = plane-alloc zeroing + pad copy
(named follow-up). RFF_HPEL_AVX2=0 pins the old path.

**enc lookahead policy — MEASURED VERDICT (XB_MBTREE arm, 48f, 4-QP PSNR,
same veryfast anchor):** mbtree BD wins EVERYWHERE on the probe set (foreman
−2.7, bus −1.1, akiyo −4.9 BD points) — but the cost is UNBOUNDED on busy
content: akiyo/foreman +21-24% encode time, bus +251% (full-res per-GOP
lookahead ME vs x264's HALF-RES lookahead, which is how their 23% stays 23%).
VERDICT: keep the speed edge as the shipped default; the enabling brick for
any flip is a LOWRES (or busy-gated) lookahead — gate the lookahead ITSELF by
the existing predictability signal, not just its output offsets. Trading
"our 23%" is favorable only once the 23% is actually bounded.

## Descent H-32 — "I bet you cannot find an optimization in the per-MB residue"

- NAMED FIRST (the law held): five new INFO stages (DecMbP/DecMbB/DecMbI/
  DecBDirect/DecBMc) decomposed the 57% residue in ONE run: the B-MB branch
  owns 40% of real-stream decode (5.3 µs/MB), b_mc 2.1 µs/call × 42k.
- ★ THE FIND (@09d13f9): the top-level residue that no branch owned was the
  REFERENCE-LIST BUILDERS DEEP-CLONING THE DPB PER SLICE — `build_ref_list_p/b`
  did `.cloned().collect()` over entries holding full luma/chroma planes +
  motion grids (B slices: both lists + an `init0.clone()`), ~600 KB+ of pure
  memcpy per slice that executed between the profiler's scopes. Fix:
  `Arc<RefFrame>` DPB + lists (byte-identical by construction; `Arc::make_mut`
  on the rare MMCO long-term marking). Paired: **~1.3× additional on B
  streams**, +3-8% on own P streams; with H-31's coalescing banked, vf decode
  hit **120-140 ms this session vs 264 ms at the morning baseline** (~2×
  cumulative, all byte-identical).
- PRUNED: hoisting the bi-blend's per-pixel weight match out of the loop was
  FLAT paired (0.95-0.99) — LLVM already hoisted the invariant; reverted.
- LAW (new instance of an old one): allocation/copy work BETWEEN scopes is
  invisible to a scope profiler — when branches don't sum to TOTAL, suspect
  the data-movement in the seams (clone/collect/alloc), not more code.

## Descent H-33 — three more iterations into the decoder residue (owner's request)

**Iteration 1 (name deeper + a disciplined re-refutation):** three new taps
(DecSetup/DecBDeriv/DecBSet) + the calm box gave the true anatomy: B branch
137 ms prof (46%), b_mc 76 (of which real MC ~30), deblock 51, entropy 40,
P branch 44, slice setup 2.1 (hypothesis dead). The b_mc blend-hoist idea —
previously "refuted" on a wall too noisy to see it — was re-tried and refuted
PROPERLY on the DecBMc bucket (3/3 flat): LLVM already hoists the invariant
weight match. b_mc's apparent glue is largely profiler scope-tax + real MC.

**Iteration 2 (@30f7457): the deblock tile was P-ONLY.** `use_tile` required
`ref_id1.is_empty()`, so every B frame of a real stream took the strided
per-edge bs derivation. `bs1_tile` now carries `inter_bs1`'s exact two-slot B
rule (single-list fast path for P tiles / uni-L0 edges); tile enabled for B.
Proven tile==per-edge in-process on a dense B stream + YUV pre==post.

**Iteration 3 (@30f7457): IDCT-of-zeros.** The residual-add loops ran
un-scan + dequant + inverse-DCT + clip for EVERY 4×4 block; sparse-cbp real
streams make most of the 590k reconstructs zero-residual, where recon ==
prediction EXACTLY (linear integer IDCT, pred already 0..255). nnz==0 /
dc==0&&no-AC blocks now copy pred bytes. Bonus: the CABAC-P inline recon was
a byte-for-byte DUPLICATE of `add_inter_residual` — deduped, so P and B share
the fast path.

**Verdict (fair stock-vs-stock ABBA, calm box): 5/5 wins on vf.264, median
~1.05× for iterations 2+3 combined; flat on own P-only streams.** A 1.48×
reading from an earlier pairing was caught as PROFILER TAX in the pre arm
(prof build vs stock build) — walls are only comparable between builds of the
same feature set. Cumulative decoder day: 264 → ~103 ms on x264 streams
(~2.6×), every brick byte-identical.

## Descent H-34 — triple iteration: MC filter, CABAC bins, deblock kernels

**MC filter — LANDED (@9da86f8 + @991350e): pad-once (ExpandPicture), ~1.08×.**
Every sub-pel MC extracted a clamped (bw+5)×(bh+5) tile before filtering
(~400 B/call, ~100 MB of tile traffic per 120f clip); openh264 pads the
picture ONCE. RefFrame now stores edge-padded planes (luma pad 16, chroma 8)
built in `as_reference` via the factored `inter::pad_plane`; all decoder MC
reads in place through `mc_*_padded` (wilder MVs → clamped-halo fallback).
Padding IS the MC clamp, so bit-identical. Fair ABBA 5/5 on vf (~1.08×), 3/3
on own streams (~1.07×). ★ THE FUZZ GATE EARNED ITS KEEP: mutated streams
hand geometry-mismatched references; the fast paths now require an intact
buffer and the fallback reads checked. (Committed one gate early — the fuzz
suite must run BEFORE the commit, not after; recorded.)

**CABAC bins — LANDED (@563b71a): windowed refill + lzcnt renorm.** The
engine pulled ONE bit per `read_bit` (bounds check + byte index + shift per
renorm shift). Now: MSB-aligned 64-bit window, 8-byte big-endian bulk refill
(zero-fill tail preserved for the fuzzer's past-end bound), renorm shift
count from `leading_zeros`. Same bits in the same order → bins identical by
construction; YUV cmp + full suite green. Residual-parse bucket ~1.05×
paired on a degrading box; the window also serves every mvd/skip/terminate
bin outside that bucket. (First cut had a stale-bits accounting bug in the
fast refill — caught by inspection before measurement; masking to whole
bytes below `wbits` is load-bearing.)

**Deblock kernels — PRUNED at the vendor ceiling.** The kernels are the
vendored openh264 ssse3 (their BEST deblock ISA — no AVX2 variants exist,
per the 2026-06-27 gap audit), wired and hit; the derivation is tiled for P
AND B as of H-33; thresholds already compute after the all-zero early-out.
Remaining idea recorded for a calm box: x264-style SIMD batch
`deblock_strength` derivation (theirs runs ~15 ns/MB).

Box note: walls were unusable for most of this session (2× swings mid-pair);
every kept claim above rests on paired same-minutes arms or byte-identity +
strictly-less-work arguments.

## Descent H-35 — the "genuine kernel work" swing (D6 FIRST, and it moved the goalposts)

**★ D6 CORRECTION — the decoder gap was 3.0×, not 2.2×.** ffmpeg's
`-benchmark utime` on Windows is quantized to the 15.6 ms scheduler tick: the
"~45-50 ms" reference decode was **3 ticks** plus process startup, and 30
frames measured `0.000`. Rebuilt the instrument at scale (1200-frame CIF
stream, x264 veryfast): **ffmpeg 334 ms ±1% (364 Mpx/s) vs ours 1005 ms
(121 Mpx/s) = 3.01×.** The old comparison flattered US, which is the dangerous
direction. Every claim below is paired on this low-noise stream.
LAW: a reference tool's own timer is an instrument like any other — check its
RESOLUTION before believing a ratio built from three of its ticks.

**CABAC bins — LANDED (@d256451): branchless bin decode, 1.044×, 9/9 paired
(z=3.0), byte-identical.** Two-step descent: packing the context into one byte
(state*2+mps, ffmpeg's layout) was FLAT on its own — our 2-byte struct was
already a single load — so the mechanism is NOT memory. It is the near-coin-flip
LPS/MPS BRANCH. The packing is the *enabler* for the branchless form: mask from
the sign of (range−offset−1), masked updates, and ONE 256-entry transition table
whose upper half is the LPS path (state-0 MPS flip baked in). Renorm branchless
too. ★ The exhaustive mask-vs-conditional oracle CAUGHT a precondition the
literal form never had — `range ≥ lps` (i.e. ≥256) — now a debug_assert, with
the debug-build fuzz run proving it holds on malformed input.
LAW: when a "faster data layout" measures flat, ask what the loop is actually
bound by before discarding the layout — it may be the enabler, not the win.

**pad_plane memset — REFUTED (3/7, median 0.972), reverted.** Theory: `vec![0;
n]` then overwriting every byte is a wasted pass (~220 MB/clip). Reality: a
large fresh allocation gets **pre-zeroed pages from the OS**, so there was no
memset to remove, and append-based construction only added per-row bookkeeping.
LAW: `vec![0; n]` for a big buffer is not a memset — do not "optimize" it.

**Lookahead — LANDED (@784921f): overhead 31% → 17% on the worst clip,
bitstream-identical.** The named lever ("we lack x264's lowres lookahead") was
ALREADY BUILT — `LookaheadMode::Hybrid` (half-res MV search) has been the
default. The real cost was "exported ≠ wired" AGAIN: the lookahead carried its
own SCALAR per-4×4 Hadamard and copied a block out of `mc_luma` per candidate,
while the main ME has used the vendored asm SATD for months — and every
lookahead diamond probe is FULL-PEL, so the reference can be read in place.
Wired: bus_cif overhead +30.8% → +17.1%, mbtree-ON hash unchanged. Also
CORRECTS H-31's "+251%" for bus: that was a degrading-box artifact; paired
alternating arms say +30.8%. Net overhead is content-dependent and partly
self-financing (mbtree's QP redistribution codes fewer bits: football −14%,
akiyo +1%).

### H-35 close-out — the mb-tree gate, and where the decoder lives

**★ THE mb-tree GATE NOW CLEARS (4-QP PSNR BD, our-base vs our-mbtree, 6 clips,
32f):**

| clip | BD-rate | | clip | BD-rate |
|---|---:|---|---|---:|
| akiyo | **−4.82%** | | football | −0.53% |
| foreman | **−3.13%** | | mobile | −0.24% |
| bus | −0.29% | | city_4cif | +0.01% (neutral) |

**Worst clip +0.01% ⇒ the monotone non-regression bar is CLEARED** — the exact
ship signature `codec-content-adaptive-dispatch` asks for (every outcome ≤ 0
within noise, not a favourable mean). Net cost after the asm-SATD brick: +17%
(bus, the worst busy clip) … +1% (akiyo) … **−14% (football, self-financing —
the QP redistribution codes fewer bits than the lookahead costs)**. x264 ships
the equivalent default-ON at ~23%. So `--mbtree` is now a PRICED, gate-cleared
default-flip candidate; the flip itself is the owner's speed/compression trade
on a published crate, not a measurement question. (Method note: eyeballing the
football curve called it a LOSS; the cubic BD fit says −0.53%. Compute the
integral, never the impression.)

**Decoder, honest instrument (1200-frame x264 stream, both arms same minutes,
4 rounds): 3.01× → ~2.78× behind ffmpeg** (median; ffmpeg 462-473 ms is rock
steady, our 1229-1365 ms is the noisy arm — our working set is more
box-sensitive, which is itself a recorded lead).

**Deblock re-confirmed at the vendor ceiling** (openh264 ssse3 is their widest —
no AVX2 twin exists; derivation already has intra/skip/uniform-motion fast paths
and the P+B tile). Beating it needs OUR OWN AVX2 deblock kernel — a named,
priced, not-yet-attempted brick worth ~6% of decode if it lands 2×.

**Recorded ideas not attempted:** b_mc's two 256-byte + four 64-byte staging
buffers are zero-initialized per call (~21 MB/clip of real stack memset, unlike
the OS-page case above) — declare them per-branch; and x264-style SIMD batch
`deblock_strength`.

## Descent H-36 — the busy-clip dispatch that wasn't needed, and the blocker that was real

ASKED (owner): use the content-adaptive skills to fix mb-tree's worst-busy-clip
cost so the default can be flipped on.

**Step 1 (the skill's mandatory first move): get the per-clip TRUTH TABLE.**
Signals harvested from the existing `RFF_MBTREE_DBG` tap against the BD column:
the QP-offset `spread` ranks the BD gain PERFECTLY monotonically (akiyo 0.76 →
−4.82%, foreman 0.49 → −3.13%, football 0.41 → −0.53%, bus/mobile 0.37 →
−0.29/−0.24%, city 0.30 → +0.01%). A textbook dispatch signal — for which no
dispatch turned out to be needed:

**★ Step 2 killed the premise: the lookahead's cost is content-INDEPENDENT.**
Wall-clock said bus +34.5% in one run and −6.9% in the next **on an identical
config** — ±40 points on the quantity being measured. So the cost was re-measured
DETERMINISTICALLY (candidate-evaluation counter, `mbtree_satd_calls`):
**16, 17, 18, 19, 20, 21 evals/MB/frame** across akiyo→football. A 1.3× spread —
a near-fixed per-pixel cost, i.e. ~1-2% of a busy-clip encode (bus encodes 7×
slower than akiyo for the same frames), and LARGEST on easy content. Every
"busy-clip blowup" in this campaign's record — H-31's +251%, H-35's +17/+34% —
was a drifting-box artifact.
LAW: before dispatching on a cost, measure that cost with a COUNTER. A wall whose
run-to-run spread exceeds the content effect cannot establish that the effect
exists — and a dispatch built on it would be gating on noise.

**Step 3 — the flip, attempted, and the REAL blocker surfaced in seconds.**
Flipping `mbtree: true` immediately failed `encode_all_matches_sequential_cqp`:
mb-tree needs FUTURE frames, so it only runs in the batch `encode_all` path,
while streaming `encode()` silently produces un-offset frames. Defaulting it on
would make the two public APIs emit DIFFERENT bytes for the same config —
breaking exactly the contract H-30 existed to protect. Reverted to opt-in, with
the true reason now documented at the config field.
**Shipping it on requires a lookahead QUEUE in the streaming path (adding output
latency), not a flag flip.** That is a real feature, correctly scoped, and it is
now the only thing standing between us and −0.2..−4.8% BD for ~1-2% of encode.
LAW: a feature that consumes future frames cannot be defaulted on in an API that
only offers past ones — check the API contract before pricing the trade.
