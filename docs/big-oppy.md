# big-oppy — the big-opportunity map for rusty_h264

**Status:** living plan, started 2026-08-19.
**Premise:** "good on some content, bad on other" is never averaged away and
never abandoned — it is DISPATCHED ([adaptive-is-the-default]). This document
is the map of every content and context axis that flows through the encoder
and the decoder, so that opportunities are hunted per-class instead of on the
mean. Sign-flips in the tables below are not problems; they are triggers.

---

## 1. The benchmark (top of the document, by design)

### 1a. Standing numbers of record

**Decoder vs ffmpeg 8.1.2, 1T** (x264-encoded 720p corpus, 1800 frames/tier,
pinned CPU time, ABBA ×9 pairs, frame counts matched, all streams
byte-identical before timing — `bench/decode_x264_speedtest.sh`):

| tier                       | ratio (rusty/ffmpeg) | provenance               |
| -------------------------- | -------------------- | ------------------------ |
| CAVLC (baseline, veryfast) | **2.24×**            | 2026-08-12 morning-clean |
| Main (medium)              | **2.04×**            | 2026-08-12 morning-clean |
| High (slower)              | **2.04×**            | 2026-08-12 morning-clean |

Cross-session band: cavlc 1.98–2.49, main 2.04–2.35, high 2.04–2.25 — the
cross-binary ratio drifts with box state far more than any single change
(§16 law), so **the morning-clean session is the number of record** and every
loaded-box reading since has been bracketed by named neighbour processes
(`ocr_batch` 08-12, `faucet` 08-19). Same-lineage paired A/Bs (the valid
instrument under load) pinned every change since at null: entropy audit
1.002×, pure-Rust rip 1.004×, all |z| < 0.3.

**Decoder throughput at record:** ~213 / 146 / 125 Mpx/s (cavlc/main/high)
vs ffmpeg ~412 / 294 / 255.

> **Read the tiers correctly:** cavlc/main/high are TOOL tiers (x264 encoder
> configurations exercising different syntax), not content types. Each tier
> runs over the same 3 content clips — shields (`detail`), in_to_tree
> (`medium`), stockholm (`pan`). The decoder's real content/context space is
> §3; this table is a 3-tool × 3-content sample of it.

**Benchmark coverage matrix** (✓ = timed today, anatomy = profiled in
`decode_benchmark.sh`'s 6-clip set, — = never measured):

| content class    | cavlc   | main    | high    |
| ---------------- | ------- | ------- | ------- |
| `detail`         | ✓       | ✓       | ✓       |
| `medium`         | ✓       | ✓       | ✓       |
| `pan`            | ✓       | ✓       | ✓       |
| `fastmotion`     | anatomy | anatomy | anatomy |
| `static`         | —       | —       | —       |
| `smooth`         | —       | —       | —       |
| `complex`        | —       | —       | —       |
| `grain`          | —       | —       | —       |
| `screen content` | —       | —       | —       |

Five of nine classes have never been timed — including `static` (skip-run
fast paths) and `smooth` (DC-only fast path), the two whose profiles should
differ MOST from the current 3-clip mix. Closing this matrix is §4 item 1's
real content, and per §9 of the measurement law a class the decoder has
never seen is also a conformance probe.

**Multi-thread:** ours/ffmpeg-2T = 2.75–2.86× wall (equal 2 physical cores);
our frame-MT extracts cores-busy 1.13–1.20 on dep-dense high streams vs
ffmpeg ~1.5 — **2T is currently a wall regression vs our own 1T** there.
Refuted as a config flip; requires a dependency-scheduling campaign.

**Encoder vs x264** (memory of record, `inter-coding-gap` /
`me-speed-state`): ~30% BD behind x264 defaults on natural content, **~2%
behind at matched features**; all-intra we WIN on tsrc-class content. Speed:
ME is 81% of the encoder speed gap, and it is per-call (10.3×), not
call-count.

### 1b. Re-baseline protocol (do not skip)

1. Box must be **sustained-quiet ≥ 10 minutes** (single gap samples get
   ambushed by batch jobs — measured twice). Name any >30 CPU-min process
   before trusting anything.
2. `bash bench/decode_x264_speedtest.sh 9` — correctness precondition is
   byte-identity vs ffmpeg on every stream; frame counts must match or the
   comparison is void.
3. Fresh binaries: build the **bin** crates, check mtimes, never after a
   `--features profile` session without a clean rebuild (two stale-binary
   incidents on record, one produced an impossible 0.28×).
4. Cross-binary ratio = standing only. Keep/revert decisions use same-binary
   paired A/B with |z| > 2, N ≥ 15 (N ≥ 31 for cross-implementation claims).
5. PGO build (`bash bench/pgo.sh`) is worth −3.1% high / −5.3% cavlc — state
   whether the arm under test is PGO or plain.

### 1c. Current pipeline anatomy (sampled profiler, 2026-08-12, trustworthy)

| bucket                      | high (CABAC)         | cavlc         | SIMD-able?                      |
| --------------------------- | -------------------- | ------------- | ------------------------------- |
| entropy                     | 22.3% (+8.3% syntax) | 31.2%         | no — serial                     |
| inter-MC (+b-mc glue)       | 19.5% (+22% INFO)    | 14.7%         | done (SSE2/AVX2/NEON)           |
| per-MB loop glue / row-hook | ~26% class           | ~similar      | no — structural                 |
| deblock                     | 6.8%                 | **14.2%**     | done                            |
| dequant+reconstruct         | ~5%                  | ~14% combined | partly; dense-residual inherent |
| dpb-clone                   | 2.3%                 | 1.9%          | memcpy-bound                    |
| intra-pred                  | 0.3%                 | 0.9%          | not worth it (measured)         |

The honest ceiling from here in pure Rust: ~1.4–1.5× (entropy asm gap is the
floor); the wall-clock lever is the frame-MT scheduling campaign.

---

## 2. Content/context types — ENCODER

The encoder sees two independent input spaces: the **video content** and the
**configuration context**. Opportunities live at their intersections.

### 2a. Content classes (what the pixels are)

Corpus classes already curated in `video-tests/manifest.tsv` + synthetic
additions in `video-tests/clips/`:

| class                        | exemplar clips                        | what stresses                                                                  |
| ---------------------------- | ------------------------------------- | ------------------------------------------------------------------------------ |
| `static` (talking head)      | akiyo, FourPeople                     | skip machinery, AQ flat-region behavior, B-frames win big                      |
| `medium` (mixed motion)      | foreman, in_to_tree                   | the default path; mode-decision balance                                        |
| `detail` (texture-heavy)     | mobile, city, harbour, shields, ducks | intra 4x4/8x8, RDOQ, AQ *loses* on synthetic-like texture                      |
| `pan` (global motion)        | bus, stockholm                        | ME predictor quality, mbtree propagation (tsrc −1.8% class)                    |
| `complex` (motion+detail)    | tempete, crew                         | RD pressure everywhere; crew's flash frames broke B2                           |
| `fastmotion` (chaotic)       | football, soccer, park_joy, crowd_run | sub-8x8 partitions, B-frames *lose* (+3.6% busy), mbtree backs off             |
| `smooth` (gradients+slow)    | blue_sky                              | banding/AQ, B-frames win (−19.6%), DC-heavy residual                           |
| `grain` (synthetic)          | grain_akiyo, grain_flat               | grain floor signal (harvest-only today); PCM/lossless edge                     |
| `screen content` (synthetic) | screen_text, screen_ui                | sharp edges, flat runs, palette-like content — **no dedicated tooling at all** |

Within-frame and within-GOP sub-axes that cut across the classes: scene cuts
(B-intra escape only just landed), fades/lighting change (explicit weighted-P
exists, **no automatic detection**), flash frames (crew — broke B2 until the
translational-gain signal), duplicate/near-static frames, letterbox borders,
noise floor vs true detail (the AQ self-limiter axis).

### 2b. Configuration context (what the settings are)

From `EncoderConfig` (55 public axes), grouped by what they change:

- **Stream contract:** width/height, profile, level, chroma (4:2:0 only),
  `cabac` + `cabac_init_idc`, `transform_8x8` (default ON), `num_ref_frames`
  (default 1; multi-ref P wired + tested, multi-ref B writer MISSING),
  gop_size, bframes (+`bframes_adaptive`).
- **Rate control:** CQP `qp` vs `bitrate`+`framerate`; `i_qp_offset`
  (per-GOP cascade), `bframe_qp_offset`, `aq_strength` (default on),
  `mbtree` (+spread_min/strength/lookahead; opt-in, CQP CAVLC).
- **Search effort:** preset fast/quality, `tune_satd_q` (SAD→SATD routed
  fraction), `tune_subpel`/`me_snap`/`me_subpel_iter`, `me_wide`,
  `sub_8x8` (+`tune_sub8x8_split`, `sub8_rd`), `intra_rd`, `shape_rd`,
  `rd_lambda_mb`, `tune_b_split`.
- **Skip family:** `greedy_skip`(+min_free), `rd_skip`(+min_free, fast_t),
  `bskip_rd`(+busy_pct, dirwin_pct) — note RD-skip and sub-pel are measured
  SUBSTITUTES, not independent wins.
- **Entropy tuning:** `cabac_lambda_scale`, `cabac_dz_div`,
  `cabac_rdoq`/`_p`/`_b` (trellis default-on all-intra only).

### 2c. Existing per-content dispatchers (the machinery to extend, not rebuild)

Signals live in one memoized per-frame vector (`signals.rs`, Great Gate P1):
MB variance/lme clip table, **B2 translational-gain** (SAD@0 vs full-pel grid
— separates every B2 loss on the 16-clip truth table), me_wide coherence,
global-MC residual, plus two harvest-only axes with no consumer yet:
**synthetic-vs-natural** and **grain floor**.

Deployed dispatches: `--bframes auto` (content-adaptive, never regresses),
AQ strength self-limiter, mbtree predictability back-off, satd-q fraction
routing, per-GOP I/B QP cascades driven by one predictability signal,
best_part shape dispatches (5.59×→~4×), EDC seam auto (`bits/MB > 38.4 &&
cabac && ≤5000 MBs` — the cabac clause is load-bearing).

### 2d. Encoder opportunity seeds (per-class, to be ledgered one by one)

1. **Screen content: greenfield.** No signal, no tool, no corpus class in the
   BD gates. Cheapest first probes: flat-run/edge-histogram signal; then
   dispatch existing levers (stronger skip, 4x4-intra bias, dz tuning).
2. **Scene-cut/fade context:** B-intra escape just landed — the mode decision
   that *uses* it well (cut detection → intra-biased B, auto weighted-P on
   fades) is unowned.
3. **The two harvest-only signals** (synthetic, grain-floor) each need a P2
   fit to earn a gate — grain likely gates AQ/deadzone, synthetic gates AQ
   off (its known loss class).
4. **Multi-ref-B / B_8x8 writers** are missing syntax (documented in
   entropy-audit) — they cap the fastmotion/complex classes' BD ceiling.
5. **ME per-call cost** (Track B fixed-centre + x4 batch, BD-gated) remains
   the single largest encoder-speed lever, orthogonal to content.

---

## 3. Content/context types — DECODER

The decoder's "content" is the **bitstream**: who wrote it, with which tools,
carrying which pixel statistics. All three vary independently.

### 3a. Provenance axis (the corpus law)

Own-encoder streams are a narrow dialect — historically **100.0% full-pel MC**
on the fast preset, which hid HALF the decode gap (3× vs 6×) until x264
streams were made the standing corpus. Every decoder claim must state its
provenance: {own-encoder, x264 veryfast/medium/slower, other-encoder,
adversarial/fuzzed}. Foreign-encoder features we decode but never emit
ourselves (multi-slice, I_PCM, multi-ref B, weighted variants, lossless-PCM)
are exactly where this month's conformance bugs lived.

### 3b. Syntax/tool context axes (what the stream uses)

| axis            | values seen                         | decode-cost/behavior consequence                                                               |
| --------------- | ----------------------------------- | ---------------------------------------------------------------------------------------------- |
| entropy coder   | CAVLC / CABAC                       | CABAC arm ~1.9× slower per content; different bucket mix (deblock 14.2% on cavlc tier vs 6.8%) |
| slice structure | 1 / N slices per picture            | fixed 08-12; idc==2 bS suppression now gated                                                   |
| frame types     | I-only / IP / IPB (+pyramid)        | B-heavy = b-mc glue 22%; dep-density kills frame-MT overlap                                    |
| refs            | 1 / N, long-term, MMCO, gaps        | placeholder-ref path; reorder/marking                                                          |
| partitions      | 16x16-heavy … sub-8x8-rich          | per-call MC glue count; the coalescing ladder's win profile                                    |
| MC precision    | full-pel … qpel-rich                | interpolation share swings the whole profile (corpus law)                                      |
| transform       | 4x4 / 8x8 mix                       | cat-5 residual path; scaling matrices                                                          |
| special MBs     | skip-run-heavy / PCM / lossless-PCM | skip density flips which fast paths matter; PCM = byte-copy                                    |
| weighted pred   | none / explicit-P / implicit-B      | explicit-B refused (unimplemented, loud)                                                       |
| bits/MB density | sparse … dense residual             | drives EDC dispatch today; the strongest single cost signal                                    |

### 3c. Pixel-content axes as the decoder feels them

Same classes as §2a but felt through the stream: `static`→skip-run dominated
(CAVLC fast paths), `detail`→dense residual + intra mix, `fastmotion`→
partition-rich + qpel-rich (MC-bound), `smooth`→DC-only residual fast path
territory, B-heavy smooth → b-direct/b-mc glue.

### 3d. Existing decoder dispatches + known sign-flips

EDC seam auto-on (bits/MB × entropy-coder × frame-size); row-interleaved
deblock default; frame-MT opt-in (Phase A/B) — **sign-flips on dep-density**
(the 2T refutation); DC-only residual fast path; nnz≤6 scatter hybrid (the
dense/sparse crossover is itself a content dispatch).

### 3e. Decoder opportunity seeds

1. **Frame-MT dependency scheduling** — the only named lever that changes the
   wall-clock story; must be gated per dep-density class (it will sign-flip).
2. **§15 batch** of the counter-kept bricks for one resolvable timing verdict.
3. **Per-provenance profile refresh** whenever the corpus gains a class
   (screen-content and grain streams from §2 land here too — nobody has ever
   profiled our decoder on them).
4. Entropy syntax layer micro-shapes: engine is CLOSED (Part 24); only
   population-level restructuring (skip-run batching on static-class CAVLC)
   remains admissible.

---

## 4. Next actions

1. Re-baseline §1a on the next sustained-quiet window (protocol §1b) — add
   the grain/screen clips to a fourth benchmark tier while at it.
2. Build the §2a class × §2b lever **BD matrix** for the encoder (4-QP,
   per-clip, distribution-not-mean) — the sign-flips it exposes are the
   dispatch backlog, pre-filtered by the refuted-ledger.
3. Wire the two harvest-only signals to their first gates (grain → deadzone,
   synthetic → AQ) once their truth tables hold both sides of the threshold.
4. Screen-content: corpus first (x264 + our encoder over screen_text/ui at
   4 QPs), then signal, then dispatch — never a fixed compromise.
