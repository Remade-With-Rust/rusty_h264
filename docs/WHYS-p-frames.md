# WHYS — why do our inter frames cost ~1.8x x264's?

> **RESOLVED TO B FRAMES, 2026-08-09.** The title says "P frames" because that is what
> the first (unmatched) measurement said. Matched, it is B frames, on every content
> class measured. See D2b and D5c below.

Descent per `codec-six-whys-unknowns`, measurement rules per `codec-measurement`.
Started 2026-08-09. **Counts before times throughout — this is a RATE question, so the
instruments are bit totals and macroblock counts, not the clock.**

Corpus: `720p50_shields_ter` looped to 300 frames, 720p, qp 26. High-motion content.
One clip — see the OPEN section.

---

## D6 — is the comparison sound? (run FIRST)

- ASKED: are we comparing like with like against x264 veryfast?
- COUNTED: 300 frames decoded from both streams; frame-type mix per stream.
- FOUND: **three configuration asymmetries, all mine**, and fixing them changed the
  answer to every level below.

| asymmetry | ours | x264 veryfast | effect |
|---|---|---|---|
| B-frame count | `--bframes 2` | default 3 | mix 199B/100P vs 224B/75P |
| frame-type QP cascade | `--iqp-offset -3 --bqp-offset 2` | `--ipratio 1.4 --pbratio 1.3` | both sides coding B coarser, by different amounts |
| AQ | on (1.0) | `--tune ssim` | per-MB QP differs |

Matching `--bframes 3` alone reproduced x264's mix EXACTLY (224 B / 1 I / 75 P) and cut
our bytes 8.9%. Neutralising all three (`--ipratio 1.0 --pbratio 1.0 --aq-mode 0
--ref 3` vs `--aq 0 --iqp-offset 0 --bqp-offset 0 --refs 3`):

| | x264 | ours | ratio |
|---|---:|---:|---|
| I | 1,978 B | **1,203 B** | **0.61x - WE WIN INTRA BY 39%** |
| P | 36,370 B/f | 64,865 B/f | 1.78x |
| B | 16,955 B/f | 31,672 B/f | 1.87x |
| total | 6,527,801 | 11,960,762 | 1.83x |
| SSIM | 0.935627 | 0.936710 | ours +0.001 |

- ANSWER: the gap is real (1.83x for the same quality) but **the earlier decomposition
  was an artifact**. Un-matched, P frames looked like 86% of the excess; matched, the
  excess is **B 61% / P 39% / I -0.01%**. x264's `--pbratio` had been making its B
  frames cheap, and our `--bqp-offset` the same for us, and the two did not cancel.
- CONFIDENCE: high. Frame mix identical, frame counts equal (300/300), SSIM within
  0.001 so the quality sign is unambiguous.
- STATUS: **closed**, and it re-scoped the campaign.

### D6a — the quality SIGN (per `codec-rate-allocation-vs-efficiency`)

1.83x the bits for +0.001 SSIM = **CODING INEFFICIENCY**, not rate misallocation. Do
not go looking for a QP-offset fix; the coding path owns this.

### D6b — an instrument that was present, documented and unreadable

`RFF_BITACCT=1` switches on the bit accountant... except `bitacct::init_from_env()` was
never CALLED from the CLI, and `dump()` never called either. So the env var armed
nothing and the buckets read zero. Both now wired in `rusty_h264-cli`. Recorded because
"the instrument exists" and "the instrument works" are different claims.

---

## D2 — which stage owns the excess, in ABSOLUTE bits?

- COUNTED: per-frame-type byte totals, matched configuration (above).
- ANSWER: **B frames 3,296,571 bits of excess (61%)**, P frames 2,137,165 (39%), I
  frames -775 (we are ahead). Intra is not a target; inter is, and B more than P.
- CONFIDENCE: high — deterministic byte totals from ffprobe, one run.
- STATUS: closed.

---

## D3 — which syntax element inside inter coding?

- COUNTED: exact CABAC bit deltas per syntax element, whole encode, matched config.
  Reconciles 93,293,837 accounted bits against 11,960,762 actual bytes = **97.5%**;
  the 2.5% residue is slice headers + NAL + flush, which the accountant does not tap.

| element | bits | share | elements |
|---|---:|---:|---:|
| **residual luma** | 78,947,151 | **84.6%** | 705,225 |
| residual chroma | 4,425,512 | 4.7% | 705,225 |
| intra MB body (in P/B) | 5,629,492 | 6.0% | 36,520 |
| cbp | 2,655,611 | 2.8% | 713,521 |
| mb_skip_flag | 663,808 | 0.7% | 1,076,400 |
| **mvd (MOTION)** | 473,853 | **0.5%** | 230,849 |
| mb_qp_delta | 297,771 | 0.3% | 705,225 |
| ref_idx | 128,369 | 0.1% | 230,849 |
| mb_type/sub_type | 38,445 | 0.0% | 234,449 |

- ANSWER: **TEXTURE 89.4%, non-residual syntax 10.6%, of which motion is 0.6%.**
- CLASSIFICATION: the answer is BITS, and it is residual. **Motion SYNTAX is not the
  problem and cannot be — it is 0.5% of the stream, so a perfect motion coder saves
  at most 0.5%.** That prunes an entire class of lever on arithmetic, before building.
- CONFIDENCE: high — exact bit deltas, 97.5% reconciliation.
- STATUS: closed.

**The trap this avoids:** "our ME is weak" is a true statement about SPEED (recorded
elsewhere: ME is 81% of the speed gap, 10.3x per call) and it is tempting to carry it
over to rate. The accountant says motion BITS are 0.6%. Spending nothing on motion
while drowning in texture does not mean motion is fine — it means a search that finds
poor matches emits SMALL vectors and LARGE residuals. The cost lands in texture.

---

## D4 — what does the reference do differently? (reference MB distribution)

- COUNTED: x264's own macroblock census, same matched configuration:

```
mb P  I16..4: 10.0%  0.0%  6.8%   P16..4: 66.8% 5.8% 3.3% 0.0% 0.0%   skip:  7.4%
mb B  I16..4:  0.3%  0.0%  0.1%   B16..8: 19.1% 4.5% 1.1%  direct:42.7%  skip: 32.3%
```

- **75.0% of x264's B macroblocks are direct or skip** and carry little or no residual.
  That is the mechanism behind its 16,955 B/frame against our 31,672.
- Our side, from the accountant: 705,225 of 1,076,400 inter macroblocks code luma
  residual = **65.5% code residual, 34.5% do not**. x264's weighted no-residual share
  (P skip 7.4% x 75 frames, B skip 32.3% x 224) is ~26.0%.
- ANSWER: **we are NOT skipping less often than x264 — we skip MORE (34.5% vs ~26%).**
  So the excess is not skip rate. When we DO code residual we emit far more of it:
  78,947,151 / 705,225 = **112.0 bits of luma residual per residual-coding macroblock.**
- CONFIDENCE: medium-high. The x264 percentages are its own report; our shares come
  from accountant element counts. The two are not identically defined (x264's "direct"
  can still carry residual), so treat ~26% as a lower bound on its no-residual share.
- STATUS: closed enough to direct D5.

---

## D5 — why is the residual so large? OPEN

Two candidates, and they take opposite fixes:

- **D5a — prediction quality.** Our inter prediction leaves more energy than x264's, so
  there is genuinely more to code. Predicts: residual ENERGY (SSD of source minus
  prediction) higher than x264's at matched QP.
- **D5b — residual coding efficiency.** Same energy, more bits: quantisation, scan,
  or CABAC context modelling. Predicts: comparable residual energy, worse bits-per-
  nonzero-coefficient.

**Neither is measured yet, and the descent must not skip to a fix.** The discriminator
is an instrument we do not have: per-macroblock residual energy after prediction,
comparable across encoders. Cheapest route is to add an SSD-of-residual counter to our
own encoder and compare bits-per-unit-residual-energy against the same quantity derived
from x264 via `--dump-yuv` reconstruction differencing.

Prior recorded evidence that bears on it, NOT re-measured here:
- `fast` does not run sub-8x8 at all (Quality-gated) - x264 veryfast uses P8x8 on only
  3.3% of P MBs, so this is probably small on THIS content.
- 16.8% of x264's P macroblocks fall back to INTRA (I16 10.0% + I4 6.8%). Ours is 3.4%
  of all inter-slice MBs. On high-motion content, refusing to go intra when prediction
  fails is a direct route to a large residual. **This is the strongest D5a lead** and
  it is a COUNT we already have.

---

## D2b — is the B-vs-P split content-dependent? NO. (`bench/inter_split.py`)

- ASKED: D2 was closed on one high-motion 720p clip and flagged as probably
  content-dependent. Is it?
- COUNTED: per-frame-type bytes, matched configuration, 7 content classes.

| clip | class | total | I | P | **B** | B per-frame |
|---|---|---:|---:|---:|---:|---:|
| akiyo_cif | smooth/static | 1.78x | +2.0% | +25.5% | **+72.6%** | **2.57x** |
| FourPeople | smooth 720p | 1.63x | +7.9% | +35.3% | **+56.8%** | 1.88x |
| foreman_cif | medium motion | 1.73x | +2.1% | +34.4% | **+63.5%** | 1.90x |
| mobile_cif | detail + pan | 1.56x | +0.5% | +33.1% | **+66.4%** | 1.62x |
| harbour_4cif | detail + motion | 1.33x | +0.7% | +27.0% | **+72.4%** | 1.41x |
| grain_akiyo | grain | 1.04x | +1.5% | +49.6% | +48.9% | **1.02x** |
| screen_text | screen | **2.98x** | **-2.2%** | +17.0% | **+85.2%** | **9.93x** |

- ANSWER: **B frames own the excess on 7 of 7 classes (48.9-85.2%).** NOT
  content-dependent, so this is ONE DEFECT, not a dispatch problem -- which matters,
  because `codec-content-adaptive-dispatch` would have been the wrong route.
- The SHAPE is the diagnostic: the B ratio is WORST on the easiest content (static
  akiyo 2.57x, screen 9.93x) and at PARITY on the hardest (grain 1.02x). x264 gets
  screen B frames to 51 bytes; we spend 509. A gap that grows as the content gets
  easier is a FLOOR COST, not a compression gap.
- Our I frame is smaller than x264's on screen_text (0.92x) and within 7% on 5 of 7 --
  intra is not a target anywhere.
- CONFIDENCE: high. Deterministic byte totals, frame counts verified equal per clip,
  SSIM within 0.002 on 6 of 7 (grain -0.013, the one clip where we are behind on
  quality too).
- STATUS: closed.

## D5c — is the B floor cost a SKIP-rate failure? NO. (`RFF_BSTATS`)

- ASKED: x264 puts 75% of its B macroblocks in direct/skip. Are we simply not skipping?
- COUNTED: B-slice mode census, ours vs x264's own `mb B` line, matched config.

| clip | our B_Skip | x264 skip | bits per CODED B-MB (ours / x264) | coded-MB ratio |
|---|---:|---:|---:|---:|
| screen_text | 92.5% | 94.3% | **137 / 18.5 = 7.4x** | 1.34x |
| akiyo_cif | 76.0% | 88.4% | 54 / 43.6 = 1.24x | **2.07x** |
| harbour_4cif | 13.8% | **6.9%** | 128 / 84 = 1.53x | 0.93x |
| grain_akiyo | 0.0% | 0.0% | 448 / 437 = **1.02x** | 1.00x |

- ANSWER: **REFUTED.** Our skip rate tracks x264's (92.5 vs 94.3 on screen) and on
  harbour we skip TWICE as often (13.8% vs 6.9%). The excess is not in how often we
  skip; it is in **what a coded B macroblock costs**, and that ratio is worst on the
  EASIEST content (7.4x screen) and at parity on the hardest (1.02x grain).
- MECHANISM, still a hypothesis: on easy content there is almost nothing to code, x264
  emits `cbp = 0`, and we emit coefficients anyway. That is a RESIDUAL DECISION, and it
  is consistent with D3 (texture 84.6% of all bits). A macroblock that x264 codes in
  18.5 bits and we code in 137 is not carrying 7x the information.
- Two sub-effects that do NOT align and should not be merged:
  - **akiyo: 2.07x more coded MBs**, each only 1.24x dearer -> a MODE decision issue.
  - **screen: 1.34x more coded MBs**, each 7.4x dearer -> a RESIDUAL issue.
  Both may be present with different weights per content; do not fit one story to both.
- CONFIDENCE: medium-high on the refutation (four clips, direct count comparison);
  LOW on the mechanism, which is not yet measured.
- STATUS: refutation closed; mechanism OPEN.

## Refuted / pruned

- **"Optimise P frames"** as originally framed — P frames own 39% of the excess, not
  86%. The 86% figure came from an unmatched configuration. B frames are the larger
  target.
- **Motion-vector coding** — pruned on arithmetic at D3. 0.5% of the stream; a perfect
  mvd coder cannot pay for itself.
- **Skip rate** — pruned at D4 and again, harder, at D5c across four content classes.
  Our B_Skip tracks x264's within a few points and EXCEEDS it on harbour. "We are not
  skipping enough" is false.
- **Content-adaptive dispatch for the B gap** — pruned at D2b. B owns the excess on
  7 of 7 classes, so there is no content axis to dispatch on. One defect.

## Open / not established

- ~~ONE CLIP~~ CLOSED by D2b: seven content classes, same answer.
- Original note kept for the record: ONE CLIP, high-motion 720p. `codec-measurement` 9 says content decides stage shares;
  this whole descent needs re-running on a smooth clip and a detail clip before any
  fix is sized. The B-vs-P split especially is likely content-dependent.
- D5 undecided (above). No fix should be built until D5a/D5b is settled - that is
  exactly the "profile names a stage, someone infers a cause, builds the obvious fix,
  and it returns nothing" failure this skill exists to prevent.
- The `quality` preset was NOT descended. It needs ~15% FEWER bits than x264 veryfast
  at equal SSIM, i.e. it does not have this problem. Diffing `fast` against `quality`
  on the same accountant buckets is probably the cheapest possible route to D5.

---

## D5d — locating what `fast` is missing. NARROWED BY ELIMINATION, not found.

- ASKED: the accountant diff (D5e below) says `quality` beats `fast` by 29% on the same
  clip. Which preset-gated knob carries that?
- COUNTED, liveness FIRST (the discipline four dead arms taught today): byte totals on
  foreman_cif, `--preset fast --bframes 3`, each knob toggled alone.

| arm | refs 3 | refs 1 |
|---|---|---|
| `RFF_INTRA_RD_ALL=1` (item #5) | **byte-identical** | **byte-identical** |
| `RFF_ME_WIDE=1` | **byte-identical** | **byte-identical** |
| `--sub8x8 1` | **byte-identical** | **byte-identical** |
| CONTROL `--preset quality` | 204,028 -> 144,518 | 197,825 -> **139,484** |

- ANSWER: **all three preset-gated knobs are INERT in the CABAC B-frame configuration**,
  at both reference counts, while the preset itself moves 29%. So the `fast`/`quality`
  delta is NOT carried by intra-RD-everywhere, me_wide, or sub-8x8 -- it lives
  somewhere else in the path, or those three are unreachable from the CABAC
  `encode_slice_data_cabac_p` route that `--bframes 3` selects.
- **ITEM #5 IS THEREFORE NOT ANSWERED.** `RFF_INTRA_RD_ALL=1` changes nothing here, so
  the x264 comparison (16.8% intra-in-P vs our 3.4%) has no lever attached to it yet.
  The gate at mb16.rs:9167 reads the right variable; something upstream suppresses it.
  Do NOT record this as "intra-RD does not help" -- it was never exercised.
- CONFIDENCE: high on the elimination (byte-identity is exact, 4 arms x 2 ref counts),
  ZERO on where the delta actually lives.
- STATUS: **open**, and now cheap to continue: bisect `fast` -> `quality` by flipping
  the four preset-derived fields in `FrameEncoder::new` (mb16.rs ~1660/1685/1706/1736)
  one at a time. `fast: cfg.preset != Preset::Quality` at 1736 is the untested one and
  is the ME switch.

## D5e — the accountant diff, `fast` vs `quality` (item #3's evidence)

foreman_cif, 60 frames, gop 60, bframes 3, refs 3, same binary:

| | fast | quality |
|---|---:|---:|
| total | 204,028 B | **144,518 B (-29%)** |
| mvd (MOTION) | 30,682 bits (2.1%) | **76,510 (7.9%) -- 2.5x MORE** |
| residual luma | 1,076,641 (72.2%) | **576,401 (59.2%) -- 46% LESS** |
| residual-coding MBs | 14,003 | 11,689 |

- ANSWER: **`quality` spends 2.5x more on motion and 46% less on texture, and lands 29%
  smaller.** This CONFIRMS D3's caution by existence proof: emitting tiny motion vectors
  is not evidence that motion is fine, it is evidence the search is not finding matches,
  and the cost reappears in texture multiplied.
- The fix for `fast` is therefore in MOTION SEARCH, not entropy coding -- but see D5d:
  the knob that carries it has not been located, and it is a speed/quality trade that
  needs the ME-speed work before it is affordable.
- CONFIDENCE: high. Same binary, same clip, exact bit deltas.
- STATUS: cause identified, lever not located.

---

## D5f — item #5 ANSWERED: intra-RD shares #3's root cause

- ASKED: why is `RFF_INTRA_RD_ALL=1` byte-identical on `fast`, when x264 falls back to
  intra on 16.8% of P macroblocks and we manage 3.4%?
- COUNTED: the gate's own census (`RFF_CENSUS_CSV`), `intra_rd_flip` as fired/seen:

| preset | intra_rd_flip |
|---|---|
| fast | **0 / 0 -- never consulted** |
| balanced | **313 / 5,897** |
| quality | 274 / 5,876 |

- ANSWER: **seen = 0.** The gate is not mis-thresholded and the env var is not broken --
  the intra-vs-inter RD trial is NEVER REACHED on `fast`, because it sits behind the
  SAME `fe.fast` flag that gates sub-pel. One flag, two tools.
- So #5's lever IS #3's lever, and shipping `balanced` already released it: balanced now
  overturns the cheap SATD intra/inter pick on 5.3% of macroblocks, slightly MORE often
  than Quality does.
- **CORRECTS D5e's attribution:** Balanced's -30..-49.5% BD was credited entirely to
  sub-pel. It is sub-pel AND intra-RD, released together. The two cannot be separated by
  any knob that exists today.
- CONFIDENCE: high -- `seen = 0` is unambiguous, and the census is the gate's own tap.
- STATUS: closed.

**Why the earlier arm looked dead.** `RFF_INTRA_RD_ALL=1` correctly flips
`intra_rd_grain_gate()`, and `use_rd` would have become true -- but the enclosing block
never executes on `fast`, so the env var had nothing to switch. That is the fifth
dead arm of the campaign, and the FIRST one caught by a `seen` counter rather than by
byte-identity. A fired/seen pair distinguishes "the gate said no" from "the gate was
never asked"; byte-identity alone cannot.

---

## STANDING vs x264 veryfast after the campaign (`bench/x264_standing.py`)

BD-rate (SSIM), defaults both sides. Negative = we win. **We do not win.**

| clip | ours fast | ours balanced |
|---|---:|---:|
| harbour_4cif | +104.9% | **+8.6%** |
| akiyo_cif | +99.1% | +18.1% |
| foreman_cif | +147.9% | +19.0% |
| FourPeople 720p | +85.0% | +29.4% |
| mobile_cif | +234.4% | +32.8% |
| screen_text | +96.0% | +49.7% |
| grain_akiyo | +225.5% | +114.9% |

Still behind on 7 of 7. The gap fell from +85..+234% to +8.6..+33% on natural content
-- a 3-4x reduction -- but the standing is BEHIND, and grain/screen remain the worst
classes by a wide margin. Recorded here so the relative improvement is never quoted as
a win over x264.

---

## D5g — the sub-pel grain veto: TWO implementations, neither lands. UNRESOLVED.

- ASKED: Balanced regresses +13.61% BD-SSIM on grain because sub-pel interpolates
  noise. Gate it off on grain.
- ATTEMPT 1 (per-frame): `if !fe.fast && sig.grain_signature() { fe.fast = true; }` at
  all three slice coders that build their own `FrameSignals`. Result: grain output NOT
  byte-identical to Fast. Removed rather than shipped half-applied.
- ATTEMPT 2 (sequence-scoped): decide once in `encode_all` from frames[0..2], carry it
  in an RAII `SeqFastPath` static that `FrameEncoder::new` ORs into the `fast` field.
  Instrumented and CONFIRMED reaching the decision:

      GRAIN_SEQ preset=Balanced nframes=60 grain_seq=true

  ...and the output was still 635,423 bytes -- unchanged from sub-pel-on. Verified the
  field reads the flag (mb16.rs:1762) and that `fast:` has exactly ONE assignment. So
  the DETECTION is right, the WIRE is right, and the behaviour still does not change.
- ANSWER: **unresolved.** Something between `FrameEncoder::new`'s `fast` field and the
  motion search consumes the decision earlier than either hook. Candidates not yet
  checked: a cached hpel plane built once per reference rather than per frame
  (`reference.hpel(...)` at mb16.rs:2537), or an ME context that snapshots `fast` at
  construction.
- Both attempts REVERTED. Shipping a veto whose comment claims it works, when it does
  not, is worse than shipping no veto: the next person reads the comment.
- CONFIDENCE: high that detection works (printed), high that the naive wires do not
  (byte totals), ZERO on the mechanism.
- CONSEQUENCE FOR THE PRESET DECISION: `balanced` must stay OPT-IN. Defaulting it would
  ship an unmitigated +13.61% grain regression, and "wins on 5 of 6 classes" is exactly
  the fixed compromise `adaptive-is-the-default` forbids. Revisit the default once this
  veto lands.
