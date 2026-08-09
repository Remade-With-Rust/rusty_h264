# WHYS — why do our inter frames cost ~1.8x x264's?

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

## Refuted / pruned

- **"Optimise P frames"** as originally framed — P frames own 39% of the excess, not
  86%. The 86% figure came from an unmatched configuration. B frames are the larger
  target.
- **Motion-vector coding** — pruned on arithmetic at D3. 0.5% of the stream; a perfect
  mvd coder cannot pay for itself.
- **Skip rate** — pruned at D4. We skip MORE than x264 (34.5% vs ~26%), so "we are not
  skipping enough" is false on this content.

## Open / not established

- ONE CLIP, high-motion 720p. `codec-measurement` 9 says content decides stage shares;
  this whole descent needs re-running on a smooth clip and a detail clip before any
  fix is sized. The B-vs-P split especially is likely content-dependent.
- D5 undecided (above). No fix should be built until D5a/D5b is settled - that is
  exactly the "profile names a stage, someone infers a cause, builds the obvious fix,
  and it returns nothing" failure this skill exists to prevent.
- The `quality` preset was NOT descended. It needs ~15% FEWER bits than x264 veryfast
  at equal SSIM, i.e. it does not have this problem. Diffing `fast` against `quality`
  on the same accountant buckets is probably the cheapest possible route to D5.
