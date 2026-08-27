# Exact-wins dig: the five x264-parity function areas (2026-08-27)

Goal: 20 deterministic instruction-reducing wins inside the five parity knobs'
function areas (bframes, keyint, weightp, trellis, b-pyramid), each gated
byte-identical. Method: release-asm per-function counters (`idiv`/`fdiv`/
`panic_bounds_check`/instrs) before and after; gates = the 12-hash battery
(`hash_r14_exact`, lineage `hash_before → r11 → r13 → r14`), the bframes-AUTO
stream capture (`auto_r14`, 3 clips × 4 QPs × auto/fixed-3), the encoder test
suite, and the full workspace suite.

## The counter table (release asm, per function)

| function | before | after |
| --- | --- | --- |
| `mb16::rdoq` | fdiv=8, 256 instrs | **fdiv=0**, 213 instrs |
| `encode_all_bframes` | idiv=3, bounds=9 | **idiv=0**, bounds=8 |
| `estimate_luma_weights` | fdiv=3, bounds=4 | fdiv=3¹, **bounds=1** |
| `mbtree::downsample2x` (was inlined) | per-pixel bounds | **bounds=0** |
| `gop_bi_residual` closure | 877 instrs | 849 instrs |

¹ the three remaining `divsd` are the estimator's DEFINING divides (means +
weight fit); the win there is pass-count, not divide-count — see W1/W2.

## The 20 wins

**Trellis (rdoq):**
1. **T1** — the eight `65536/QUANT_MF_OH[qp][k]` reciprocals (hoisted to call
   scope in an earlier round) became a process-lifetime `OnceLock` table
   (`rdoq_qstep`): both inputs are const tables, so `fdiv` per trellis call
   (once per 4×4 residual block) went **8 → 0**. IEEE division is
   deterministic — same operands, same bits.
2. **T2** — `rdoq_rate`'s closed form (two branches + `min` + int→f64 convert)
   became a 16-entry const LUT with a clamped index; same 16 values.

**Weightp (`estimate_luma_weights`):**
3. **W1** — the CURRENT frame's subsample grid was re-walked once per ref for
   means and again per kept ref for the SAD test (up to 6 passes at refs=3);
   one pass now collects the sum AND the samples, shared by every reference.
4. **W2** — each reference's samples are cached on its means pass, so the keep
   test re-reads two compact buffers instead of re-striding two planes; each
   reference plane is walked once instead of twice.
5. **W3** — both sampling loops read through row slices; the multiplied
   per-sample indexing (and its bounds re-proof) is gone (bounds 4 → 1).

**Keyint / detector:**
6. **K1** — `lookahead::coded_luma` rewritten to the row-copy + edge-fill shape
   its `mbtree` twin already had (panic-campaign form): two `min`s + a mul + a
   bounds check per PIXEL become a `memcpy`/`memset` per row.
7. **K2** — `downsample2x` re-indexed via row-pair slices + `chunks_exact(2)`:
   five multiplied indexings per output pixel lose their bounds checks and the
   row bases hoist (asm: bounds=0).
8. **K3** — `frame_pair_ratio` split into `pair_prep` (per FRAME: coded +
   half-res planes) and `pair_ratio_prepped` (per PAIR). The batch scorer
   prepped every interior frame TWICE (as `cur`, then as the next pair's
   `prev`); `segment_gops` now rolls the previous pair's prep across — the
   dominant detector cost halves.
9. **K4** — `segment_gops` scores pairs LAZILY through a forward cursor:
   decisions read `scores[i-1..i-3]` only at `i - last >= minki`, so the first
   `minki − 3` pairs after every segment start are unreadable and are no
   longer scored (22 pair evaluations saved per segment at the default
   min-keyint 25). Skipped entries stay NaN — a read would poison the
   comparison to `false`, never silently pass.
10. **K5** — the streaming twin (`try_encode`) caches `last_src`'s prep, so
    each causal pair preps only the NEW frame (was: both, every call).
11. **K6** — streaming skip-window: the pair ratio is not computed while
    `counter + 2 < min_keyint` (no decision can consult it — at the earliest
    decision both spike-baseline slots come from counters `>= minki − 2`).
    Placeholder history entries are provably unread.
12. **K7** — the pub calibration probe `scene_cut_ratios` routes through the
    same rolling scorer (`all_pair_ratios`) — same values, half the preps.

**Bframes (`encode_all_bframes` + gate):**
13. **B1** — flash-veto frame means MEMOIZED across pairs and gaps (interior
    means were computed twice per gap, anchor means once per adjoining gap);
    the in-loop sample counter became `ceil(len/64)`. Short-circuit `any`
    preserved — the memo computes no mean the veto never reads.
14. **B2** — the anchor-cadence `off % step` (runtime `step` → an `idiv` per
    frame) became a rolling phase counter (`phase == 0 ⇔ off % step == 0`).
15. **B3** — the per-frame `is_anchor` derivation restructured segment-OUTER:
    favorability, segment start and the boundary test hoist to once per
    segment (each was a bounds-checked per-frame lookup).
16. **B4** — the mb-tree anchor windows called `gop_qp_offsets` on DEEP-CLONED
    frames (full Y+U+V per anchor per window, ~150 KB CIF / ~3 MB 1080p);
    new `gop_qp_offsets_refs(&[&YuvFrame])` carries references, zero copies
    (the owned wrapper keeps contiguous callers unchanged).
17. **B5** — the per-anchor offset rows were CLONED out of an owned Vec;
    `into_iter()` moves them.
18. **B6** — `global_me`'s refine loop re-evaluated its own centre (the coarse
    best, cost already in `bc`; `<` is strict and `bc` only decreases — the
    revisit can never win): one full subsampled-SAD call per `global_me`
    skipped. The wandering inner range (`best.0` re-read per row) is
    deliberately preserved — hoisting it would CHANGE the search.

**B-pyramid (ref-B tail):**
19. **P1** — the ref-B deblock tail allocated a fresh List-1 index Vec per
    reference-B slice; recycled `enc_scratch` slot (`REFID1`), like List-0.
20. **P2** — the CABAC-B coder allocated `mb_qpy` fresh (`vec![qp; n]`) while
    all three sibling coders pool it; now pooled (`clear` + `resize(n, qp)`),
    returned on BOTH exits (leaf and ref).

Plus (same area, counted with T-batch): **implicit_bi_weights**' `tx` divide —
the one variable `idiv` left in the whole B coding path — tabulated over the
clamped td ∈ [−128, 127] domain at compile time (td = 0 guarded; slot unused).

## Refusals (recorded, not shipped)

* `gop_bi_residual`'s `bi as f64 / n_samp` per sampled frame: hoisting to a
  reciprocal-multiply or a summed single divide is NOT bit-identical (IEEE
  rounding differs). Kept.
* Segment-level `gop_bi_residual` reuse across `adaptive_bcount`'s whole-clip
  probes: the subsample pair sets differ (cross-boundary pairs) — not exact.
* `frame_pair_ratio`'s `c.inter.min(c.intra)`: the cap is a `frame_costs`
  invariant, but dropping the `min` rides on a non-local invariant for one
  instruction per MB. Kept.
* B5 first draft hoisted the refine window to the entry centre — REJECTED
  before landing: the inner range re-reads the mutating `best.0` (a wandering
  window), and pinning it changes which points are searched.

## Surfaced defects (the dig's real haul)

1. **P0, FIXED — scalar builds decoded packed-bS streams with chroma deblock
   silently OFF** (shipped in 0.11.0). The scalar chroma loops in
   `filter_frame_rows` lacked the `pre_bs` branch the luma loops and the accel
   arm both have, and read the never-populated `bs_v`/`bs_h` zero-init
   instead. Invisible because the CLI defaults to `asm` (accel); triggered by
   any stream the decoder routes through precomputed strengths. **This also
   closes the filed "Main-profile chroma-deblock divergence"** — the encoder
   was never at fault, and the original "decoder exonerated" arm was an accel
   build that never executed the broken closure (gate-must-build-what-it-tests).
   After the one-closure fix: ffmpeg **full-pixel** exact on default (High),
   Main, and B-frame/pyramid streams, all three presets — the pyramid gates'
   luma-only concession is obsolete.
2. **FIXED — stale single-shot docs/tests**: the parity campaign's keyint-250
   default made the documented mb-tree buffering live for default configs, so
   one `encode()` with no `flush()` emits nothing. The facade round-trip
   tests, the facade docs example, and `Encoder::encode`'s doc now carry the
   flush contract. (The buffering itself is deliberate and documented in
   `try_encode`.)
3. **Instrument corrections**: (a) the r13 hash baseline's quality-default row
   was stale — re-captured as `hash_r14_exact` (one row differs, the late
   1080p grain-planes fix); (b) the session's first AUTO-arm baseline was
   captured through a stale `parity_ab.exe` — the stale-binary law's third
   strike, now re-captured fresh (`auto_r14`). Every win above is gated
   against the fresh captures.

## Gates standing at close

12-hash battery byte-identical (r14) after every batch; AUTO-arm streams
byte-identical (auto_r14); encoder suite + full workspace suite green;
`streaming_equals_batch_with_a_firing_cut` added (two-sided: asserts the cut
FIRES and streaming == batch bytes — the skip-window/lazy-cursor identity is
now gated on content where the detector routes, per
gate-must-prove-the-tool-ran); ffmpeg full-pixel conformance on default, Main,
and B2 streams, all presets. Not clocked, as ever (box pinned).
