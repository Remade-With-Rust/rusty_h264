# Let's Win — the function-by-function battle plan vs x264 veryfast

**Mission:** close the 3.8× wall-clock gap (98.3 ms vs 26.0 ms, foreman_cif 24f qp27,
matched `--profile main`, 1 ref, no B, keyint 60, single-thread) using every mechanism
available: safe Rust restructure, Rust intrinsics, and custom NASM kernels in
`rusty_h264-accel`. Every challenge in the instrumented table gets a campaign below.

**The single finding this plan is built on:** motion estimation is **81% of the gap**
(58.5 of 72.3 ms) — and it is a **per-call** problem, not a call-count problem:

| | ours | x264 |
|---|---:|---:|
| ME calls | 39,996 | 53,527 |
| per search | **1.68 µs** | **0.16 µs** |

x264 runs 1.34× MORE searches than we do, each **10.3× cheaper**. This **retires the
"~468 candidates/MB vs one analyse call" framing** that closes `WHYS-speed-gap.md` —
at matched settings the call counts are comparable and the entire deficit is inside
one search. The count-restructure levers (deferred sub-pel, split gates) were correctly
priced as BD-expensive rungs; the per-call levers below are where the war is won.

**Licensing rule for this whole plan:** x264 is GPL. We take its *structure* (kernel
shapes, table ideas, pipeline order — all documented in its own papers and in our
`docs/x264-structural-port.md`), never its code. Custom asm is written by us or
vendored from openh264 (BSD-2) into `rusty_h264-accel`.

---

## 0. Ground rules (paid for in blood; non-negotiable)

1. **Byte-identical first.** Every Track-A brick gates on full-bitstream `cmp` across
   ≥6 clips × balanced/quality, suite green `--features asm`, ffmpeg pixel-exact.
   Bitstream-changing bricks (Track B) gate on 4-QP BD-rate per clip — **monotone
   non-regression, worst clip ≤ 0**, never the mean, never one QP (the single-QP
   mirage has struck this repo five times).
2. **Paired ABBA, same session, null arm, md5-distinct binaries.** Cross-session
   walls are invalid on this box (idle spread 1.36×). A profile-ON binary is 1.61×
   inflated — check the `PROFILER BUILD` banner. Deterministic eval/call counts
   are the verdict when the wall can't resolve (the ladder-flip lesson).
3. **Anatomy before bricks.** Each campaign opens with a measurement that decomposes
   its stage one level below the current taps. Component arithmetic decides what to
   MEASURE, never what to SHIP (three prunes went wrong that way in one campaign).
4. **Attribution check before believing any cross-encoder stage ratio.** The same
   work can live in a different stage in x264 (bS derivation lives in mb-encode, not
   deblock; entropy has no tap of its own in the x264 table at all). Grep
   `video-tests/x264_instrument.py` tap placement before chasing a ratio.
5. **One brick per commit, measured before/after in the message, revert if flat.**
   Prior flat bricks (buffer hops, AVX2 4×4 transform, in-place skip freecheck,
   hand AVX2 16×16 SAD) stay retired — do not re-litigate them.

---

## 1. Challenge 1 — MOTION ESTIMATION: 67.2 ms vs 8.69 ms (7.7×; 81% of the war)

Sub-decomposition already measured: **sub-pel refinement 41.8 ms, full-pel diamond
16.8 ms**, residue ~8.6 ms (seeds/predictors/glue).

### 1.0 The anatomy (do this first — it prices every brick below)

Decompose 1.68 µs/search into `evals × ns/eval + glue`, per phase (seed, diamond,
sub-pel), using the existing `diastats`/`spstats` counters plus one new profile-OFF
anatomy bench (the skip-MC/scatter method): replicate one real search's exact call
sequence and time each component. Reconcile: `Σ components ≈ 1.68 µs` or the residue
is glue we haven't named. Then build the same number for x264's search from its
instrumented taps (`me` + `subme` spans ÷ calls).

Known mechanism inventory — what x264 does per search that we don't:

| x264 mechanism | our current state |
|---|---|
| full-pel costs are **SAD** (`psadbw`, ~5 ns); SATD only at sub-pel | quality preset prices **every** candidate with 16×16 SATD (`mc_satd`) |
| `p_cost_mv`: λ·mvbits **precomputed table**, indexed by mvd | float `lambda_me * rate as f64` mul + cast **per eval** ([mb16.rs:1729](../crates/rusty_h264-encoder/src/mb16.rs#L1729)) |
| `sad_x3/x4`, `satd_x3/x4`: 3–4 candidates per kernel call, source loaded once | one candidate per call; source re-addressed per eval |
| FENC tile: source MB at const stride 16, unit-stride loads | strided frame reads per eval (fast preset builds `asrc` for 16×16 only) |
| fixed-centre ring passes (hex/dia) → batchable | **cascading greedy** — `best` moves mid-ring ([mb16.rs:1795](../crates/rusty_h264-encoder/src/mb16.rs#L1795), [mb16.rs:2093](../crates/rusty_h264-encoder/src/mb16.rs#L2093)) |
| qpel candidates averaged inside the SATD kernel | quarter-pel **materializes** a 2-plane average into a temp, then SATDs it |

### 1.A Track A — byte-identical bricks (do in this order)

- **A1. λ·rate lookup table.** `rate = mvbits(dx)+mvbits(dy)` is a small integer
  (≤ ~66). Precompute `LR[r] = (lambda_me * r as f64) as i64` once per search (or per
  frame per λ) — the per-eval cost becomes `dist + LR[rate]`. **Exactly** the same
  arithmetic, so byte-identical by construction. Removes an fp mul + cast + f64
  convert from the innermost loop. (`codec-eliminate-redundancy` move #2.)
- **A2. Hoist the per-eval dispatch.** The `cost` closure re-derives plane refs,
  bounds guards, and the hpel dispatch (`hpel_ref` / `hpel_block` / fallback match)
  per candidate. Resolve once per search: reference plane base pointers, padded-`f`
  base, stride, and the interior/edge classification for the search window — then
  the per-eval path is index math + one kernel call. Audit with the in-context
  ns/call vs kernel ns/call column (the skip-MC method: a 2–5× gap = named glue).
- **A3. Fused average-SATD kernel — custom kernel #1.** Quarter-pel evals are ~17%
  of MC calls and currently do: 256-byte 2-plane average into scratch → SATD read.
  Write `satd16x16_avg(a_ptr, a_stride, b_ptr, b_stride, src) = SATD((a+b+1)>>1, src)`
  as Rust AVX2 intrinsics (or NASM in `rusty_h264-accel`): `vpavgb` the two plane
  rows in-register and feed the existing Hadamard. Named reason auto-vec can't do it:
  the average lives in a different function from the SATD, with a store/reload
  between. `(a+b+1)>>1 == vpavgb` exactly → byte-identical; scalar twin stays as
  oracle (`*_matches_scalar` over random strided blocks). This is the Descent-C
  in-place win applied to the remaining (quarter-pel) half.
- **A4. Batch the SEED evaluations.** The `(0,0)` + predictor seeds are fixed,
  independent candidates ([mb16.rs:1736-1746](../crates/rusty_h264-encoder/src/mb16.rs#L1736-L1746))
  — evaluating their distortions in one x4 batch and then comparing in the original
  order is byte-identical. Small (4–5 evals/search) but it debuts the x4 kernel the
  Track-B restructure needs, against the byte-identity gate where it's provable.
- **A5. Re-audit the sub-pel memo under the new per-eval cost.** After A1–A3 the
  saved work per hit shrinks; the 64-entry memo's tag check may flip from win to tax
  (the 88.9%-redundant-dedup law: removal pays only while the work costs more than
  the check). Instrument hit rate + re-ABBA; delete if flat.
- **A6. SATD width audit.** Confirm the quality path actually dispatches
  `WelsSampleSatd16x16_avx2` (bound 2026-07-17) for every ME shape — 16×8/8×16/8×8
  included — and that no Rust-side scalar SATD call site survives (the
  "exported ≠ wired" law; it happened once already).

**Track A ceiling (honest):** these do not change eval counts. If they take the
per-eval from ~65 ns to ~30–35 ns, ME goes 67 → ~35 ms. Real, but not parity.

> **STATUS 2026-07-29 — Track A executed** (full log: `WHYS-speed-gap.md` Descent G):
> **A6** audited — all four ME SATD shapes already dispatch `_avx2`, nothing to wire.
> **A2 LANDED** (byte-identical): `mc_satd_hp` with the per-search invariants hoisted.
> **A3 LANDED** (byte-identical): `rusty_h264_accel::satd_avg` — fused avg+SATD AVX2
> kernel (the accel crate's first custom, non-vendored kernel) + `hpel_qpel_refs`;
> oracle `satd_avg_matches_materialized_scalar`, knob `RFF_SATD_AVG=0`.
> **Measured** (paired ABBA, quality preset, whole encode): foreman **1.048×**
> (9/10, z=+2.5), mobile **1.07×** (10/10, z=+3.2); attributed — A2 alone 1.043×
> (7/8), A3 alone 1.04× (8/8, one-binary knob A/B). Hashes unchanged everywhere.
> **Audited + tidied** (byte-identical, WHYS G-audit): quarter-pel operand table
> single-sourced in `hpel_qpel_refs`; `mc_satd` deleted (rescue reuses the hoists +
> gains the fused path); `sa_on` hoisted per search (no per-eval OnceLock).
> **A1/A4/A5 deferred**: λ-mul latency-hidden (~1%, under floor); seeds are 4-5
> evals/search; memo hit-rate unchanged. Track B is the open front.

### 1.B Track B — the search restructure (bitstream-changing, BD-gated, the big one)

The 10.3× cannot fully close while every candidate is a 16×16 SATD priced one at a
time around a moving centre. Two levers, both x264-shaped, both changing which MV
wins on some blocks — so both ride the 4-QP per-clip BD gate with `RFF_*` escape
hatches, exactly like the ladder flip (which shipped: −0.93% BD *and* 1.15–1.57×
less work — precedent that Track B can be BD-*positive*):

- **B1. Fixed-centre ring passes + `sad_x4`/`satd_x4` — custom kernel #2.** Change
  the diamond and sub-pel loops to evaluate the whole ring against the pass's
  starting centre, then take the argmin (first-wins tie-break), then re-centre.
  This unlocks batching: one kernel call computes 4 candidates' distortions with the
  source loaded once. Write `sad16x16_x4` / `satd16x16_x4` (Rust AVX2 or NASM;
  openh264 ships neither — x264's existence proof says the shape pays; we write our
  own). Prior art says the *amortization* is the win, not SIMD width — the flat
  hand-AVX2 single-SAD proved the loads dominate, and x4 batching is precisely the
  fix for that (4 candidate windows overlap, so their rows share cache lines loaded
  once). Expect the visit-sequence change to be BD-noise (same local minimum almost
  always) — but measure, per clip, don't assume.
- **B2. SAD-for-full-pel on the quality preset, SATD from sub-pel on.** This is
  x264's own cost split and the single biggest per-eval lever (~3–4× on the full-pel
  phase). It is also exactly the class the SAD→SATD dispatch memory warns about
  (`inter-coding gap`: SATD cost dispatch was worth −4.3% BD on integer-pel mode
  selection). So: dispatch it — SAD full-pel everywhere, SATD full-pel retained on
  the content/blocks where the proxy error measured large (the
  `codec-content-adaptive-dispatch` three moves; the existing headroom-probe
  machinery is the template). A sign-flip in the per-clip table = build the
  dispatcher, not a compromise.
- **B3. Ring-pass budget from the census, priced as rungs.** Descent D already
  priced blanket ring cuts (+2.6…+7.8% BD — rejected as defaults). Do not re-run
  that; instead re-price pat2/pat1 AFTER B1-B2 land, because the rung pricing
  changes when the per-eval cost drops (a cheaper eval makes the full ring cheaper
  to keep — the correct default may become "keep ring8, iterate" at every preset).

**Track B ceiling:** per-eval ~10–15 ns full-pel SAD batched + SATD confined to
sub-pel ≈ x264's shape. ME 67 → **~12–18 ms**. Combined with Track A's glue kills,
parity-class ME is arithmetically reachable at these call counts.

> **STATUS 2026-07-29 — B2 built + calibrated** (full log: WHYS Descent H):
> `RFF_ME_SADFP` (+ `RFF_ME_SADL`, keep 0.5) with `mc_sad_hp` at full parity with
> the SATD dispatch ladder. Off = byte-identical. 16-clip truth table at λ=0.5:
> **sign-flip** — bus −1.71 / football −1.90 / foreman −0.46 vs **crew +0.91**,
> city +0.35 → ships OPT-IN; the next brick is the dispatch signal (per-clip/frame
> motion class), not a threshold turn. Speed ~0.97–1.0×: the SAD saving is eaten by
> the convergence-driven sub-pel iterating longer from SAD starts — **B2's speed
> unlocks only with a bounded sub-pel budget (B3's re-pricing, now the open speed
> brick), while its BD win on motion content is real today.**
> vs x264 (foreman, B2 on): quality −10.5% vs superfast / +4.0% vs veryfast.

---

## 2. Challenge 2 — TRANSFORM/QUANT + RECON: 8.77 ms vs 1.71 ms (5.1×)

Prior anatomy proved scatter (CAVLC build) and the T/Q widen loop are TIGHT, and the
DCT/quant/IDCT kernels are AVX2-bound and byte-identical. Yet 5.1× stands. Campaign:

1. **Attribution first** (ground rule 4): list what our `enc-T/Q + recon` tap wraps
   vs x264's — ours may include coefficient scan/CBP derivation and recon commits
   that x264 books under mb-encode or entropy. Produce the like-for-like ratio
   before optimizing anything.
2. **Zero-block early-outs (byte-identical).** For a 4×4/8×8 block whose quantized
   levels are all zero: dequant, IDCT, and the recon *add* are identity — recon =
   prediction, exactly. Audit the inter path for full T/Q/IT/recon work on blocks
   that quantize to zero (at qp27 inter, MOST blocks do). If the skip isn't taken at
   every level (block / 8×8 group / MB via CBP), add it: `if all_zero { copy pred }`.
   This is x264's decimation *structure* without the lossy decimation decision.
3. **Batch geometry.** `WelsDctFourT4` does four 4×4s per call — verify all 16 luma
   blocks flow through 4 calls (not 16), and quant likewise (`QuantFour4x4`). Recon
   IDCT same. Any per-4×4 Rust loop around a Four-kernel is glue to delete.
4. **In-context vs kernel ns/call sweep** over dct/quant/iq/idct/recon-add — the
   skip-MC method. Any 2×+ gap = a runtime-length copy or dispatch to
   const-specialize (`bw==16` fast lanes; the trap already found twice).

Prize: 8.77 → ~3–4 ms. (The i16 end-to-end coefficient path is refuted — do not
re-litigate `coded_path_v2`.)

## 3. Challenge 3 — HALF-PEL PLANE BUILD: 3.49 ms vs 0.60 ms (5.9×)

R3 builds the planes by walking 16×16 blocks through `luma_tile_into` +
`luma_h`/`luma_v`/`luma_centre` — every block re-reads a 21×21 tile (5-px overlap
re-filtered on every seam) plus per-block dispatch. x264's `hpel_filter` is ONE fused
frame pass with asm kernels.

- **Fused row-band builder — custom kernel #3.** Process the padded source in row
  bands: horizontal 6-tap into an i16 row buffer (reused), vertical 6-tap from 6
  source rows, centre from the vertical intermediates — H, V, C written in one sweep,
  each source pixel read once. Rust AVX2 first (the 6-tap is a
  shuffle/madd pattern auto-vec won't fuse across three outputs); NASM if the
  intrinsics leave a named gap. **Oracle:** the existing per-block builder stays as
  the reference — assert byte-equal planes over real frames + ragged edges. Bit-exact
  is guaranteed by construction only if the fused math reproduces the exact clamp
  and rounding of the tile path — the oracle test is the gate, not the argument.
- Build cost is per-reference-frame, so this also shrinks the fast/balanced gating
  pressure (R3's preset gate exists because the build was expensive; a 6× cheaper
  build may let `balanced` keep the planes unconditionally — re-measure that gate).

Prize: 3.49 → ~0.7 ms.

## 4. Challenge 4 — INTER MC (recon/skip): 1.52 ms vs 0.37 ms (4.1×)

F-2 already routes recon/skip through the plane cache; the 16×16 const-width copy
fast path landed. Remaining bricks:

1. **Const-specialize the remaining widths.** 8-wide luma and chroma MC row loops
   with runtime `bw` are the same memcpy trap — add `bw==8`/`bw==4` (chroma) const
   lanes. Byte-identical, gated per the skip-MC precedent.
2. **Chroma through a plane-cache sibling?** Measure first: chroma MC is ⅛-pel
   bilinear (cheap per call) — price the census before building anything (the
   "measure the op's actual cost before hoisting" law).
3. Accept parity-ish: at 1.5 ms the absolute prize is ~1.1 ms; rank accordingly.

## 5. Challenge 5 — PRED-BUF COPY 3.23 ms + SOURCE COPY 0.49 ms (x264: 0)

x264 has **no such stages** because prediction/recon live in FDEC tiles and the frame
is written once. Ours are pure overhead — and the first fact about them is a warning
from our own ledger: **the `PredBuf` scope name lies** — it wraps MC + MV-commit glue,
not just a copy (2026-07-17 learning).

1. **Decompose + rename the stage** until every ms is named (copy vs MC vs commit).
2. **Commit prediction directly into recon.** If the pipeline is
   `predict → pred_buf → (residual add) → recon plane`, fuse: predict into the recon
   plane (or a stack tile that the recon add consumes in-register) and delete the
   round trip. Byte-identical; A/B it — data-movement bricks have measured ~0 here
   three times, so this ships only with a non-overlapping stage median.
3. **SOURCE COPY: borrow, don't copy, when aligned.** foreman CIF is 352×288 — both
   MB-multiples — yet the stage is nonzero. If `clamp_plane` copies unconditionally,
   make the aligned case a borrow (`Cow`-style: pad only when padding is needed).
   R4 already made the ragged path 2.71× faster; the aligned path should be ~0.
4. Only if a real memory-bound copy core remains: the FENC-tile question goes
   through the **frame-size sweep gate first** (`codec-cache-tiles`) — the decoder
   sweep said not-cache-bound; the encoder sweep has never been run.

Prize: 3.7 → ~0.5 ms.

## 6. Challenge 6 — DEBLOCK (+strength): 1.71 ms vs 0.97 ms (1.8×)

Two checks, then stop (the campaign declared this stage at its floor — verify, don't
grind):

1. **Attribution:** x264 derives bS inside mb-encode; our tap includes strength
   derivation. Subtract like-for-like — the true filter-vs-filter ratio may be ~1.2×.
2. **Kernel wiring audit:** the decoder dispatches ssse3 deblock asm — confirm the
   ENCODER's in-loop deblock reaches the same `rusty_h264-accel` kernels (the
   sibling-path parity audit; one side's optimization missing on the other has
   happened four times). If it's scalar, wiring it is a free, byte-identical ~1.5×.

Prize: ≤ 0.7 ms. Rank last among the code campaigns.

## 7. Challenge 7 — OTHER / residue: 14.20 ms vs 4.46 ms (3.2×)

The second-largest absolute gap (9.7 ms) and completely unnamed — which by the
analyzer's golden rule means "stopped scoping too early," with two specific suspects:

1. **Instrument identity first.** Our inflation is 2.63× vs x264's 2.37× and we run
   far more taps. Compute `Σ(scope calls) × per-scope cost` for the residue's span;
   if residue ≈ tax, the gap is partly the instrument (the 61 ms ghost precedent).
   The scaled-from-inflation methodology also deserves one depth-6 check: verify a
   profile-OFF wall of both encoders reproduces the 98.3/26.0 headline.
2. **Entropy lives here.** The x264 table has NO entropy tap — its CABAC is inside
   mb-encode/OTHER; ours (CABAC default since R11) is inside residue. Add named taps:
   `cabac-emit`, `mb-header/context`, `rc`, `dpb/finalize`, `slice-glue`. The CABAC
   engine is pure Rust against x264's asm-assisted one — once it's named and sized,
   it's an eliminate-redundancy campaign (branchless renorm, byte-at-a-time output,
   table-driven ctx updates) with an asm tail only if a named reason survives.
3. Decompose until every line is named, then rank by absolute ms and route each to
   its skill. No brick ships from inside an unnamed blob.

Prize: unknowable until named; treat 14.2 → ~7 ms as the working target.

## 8. Non-challenges (leave them alone)

- **MODE DECISION 0.72 vs 2.02 — we win 0.4×.** Don't touch it; but note it as
  attribution budget when reconciling Challenge 2/7 (x264 books analysis work here
  that we book in ME).
- **LOOKAHEAD 0.00 vs 2.69.** x264 *spends* here to earn quality (mb-tree, frame
  types). Ours is opt-in (`--mbtree`). A speed win for us today; becomes a quality
  lever later — out of scope for this plan.
- **INTRA COST** — folded into our ME/mode numbers; covered by the Challenge 1/2
  attribution passes.

---

## 9. Sequencing and the prize arithmetic

> **STATUS LEDGER 2026-07-29 (end of the Track-B arc, @49e66e0):**
> **DONE:** Ch.1 Track A (A2/A3/A6); B2 SAD-fp **dispatched default-on** (mgain +
> dcfrac flash veto, corpus mean −0.26%, worst +0.09 noise-class); FC argmin
> diamond both domains via `sad/satd_16x16_x4` (own gate monotone: bus −1.93);
> Ch.3 fused hpel builder (byte-identical). **AUDITED-CLOSED:** Ch.2's zero-block
> outs already existed. **OPEN, ranked:** ① re-profile + matched-tap re-run (the
> table above is STALE — every stage moved); ② Ch.7 residue naming; ③ sub-pel
> batching/restructure (now the dominant named stage); ④ Ch.2 T/Q attribution;
> ⑤ Ch.5 pred-buf decompose; ⑥ Ch.6 deblock audit. B3 cap stays opt-in.
> Full-restore anchor: `RFF_ME_SADFP=0 RFF_ME_FC=0`.

Order: **1.0 anatomy → A1–A6 → 7.1–7.2 (name the residue) → 2 (T/Q) → 3 (hpel
build) → B1 → B2 → 5 (pred-buf) → 4 → 6.** The Track-B restructure lands only after
Track A, because a cheaper eval changes B's pricing (and B3 explicitly re-prices D's
rung table).

| challenge | now | target | mechanism class |
|---|---:|---:|---|
| ME (Track A) | 67.2 | ~35 | Rust restructure + custom kernels (A3, A4) |
| ME (Track B) | ~35 | **~15** | search restructure + `sad/satd_x4` kernels, BD-gated |
| T/Q + recon | 8.77 | ~3.5 | zero-block outs + batch + const lanes |
| hpel build | 3.49 | ~0.7 | fused frame-pass kernel |
| inter MC | 1.52 | ~1.0 | const-width lanes |
| pred/source copy | 3.72 | ~0.5 | fuse commit-into-recon; borrow aligned source |
| deblock | 1.71 | ~1.2 | attribution + wiring audit only |
| other/residue | 14.20 | ~7 | name it, then entropy campaign |
| **TOTAL** | **98.3** | **~30–38** | vs x264's 26.0 |

End state: **~1.2–1.5× of x264 veryfast** at matched settings, from today's 3.8× —
while the BD-rate columns stay where R11/Descent-A left them (quality −9.9% vs
superfast, +4.6% vs veryfast on foreman) because Track A and campaigns 2–7 are
byte-identical and Track B is monotone-non-regression gated. Every remaining ×
after that is structural (FDEC-tile pipeline fusion, entropy asm, threads) and gets
its own plan when the profiler — not this document — says it's next.

**Standing knobs added by this plan:** every Track-B lever ships with an `RFF_*`
escape hatch that reproduces the prior bytes exactly, same as `RFF_DIA_LADDER` /
`RFF_HPEL_PAD` — the escape hatch is the bisection anchor and the oracle.
