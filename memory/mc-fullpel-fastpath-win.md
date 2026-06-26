---
name: mc-fullpel-fastpath-win
description: +15% inter win — mc_luma/mc_chroma full-pel copy fast path; found by profiling first, not SIMD-guessing
metadata:
  type: project
---

**+15% inter encode** (352×288 gop12 qp26 fast: 140.8→122.3 ms, 43.2→49.7 Mpx/s,
byte-identical) from one change: a **full-pel interior fast path in `mc_luma`/`mc_chroma`**
(commit 69c4d4b). They were sampling every pixel through `luma_sample`/`at()` with
per-pixel bounds-clamping even for integer MVs; the fast preset searches full-pel only, so
the P_Skip prediction + every inter MB's final prediction paid 6-tap/bilinear dispatch cost
for a verbatim copy. `mc_satd`/`mc_sad` already had this `interior_fullpel` path; the actual
MC kernels didn't. Fix: `if fx==fy==0 && fully-inside-frame { row-copy; return }`.
Bit-identical (interior `at()` is unclamped `reference[..]`; chroma `(64a+32)>>6==a`).

**The meta-lesson (vs [[transform-batching-regresses]]):** I found this by **adding temp
phase timers and profiling first** — NOT by assuming. The profile (ENCODE 50%, SKIP 24%,
ME-search 15%, DEBLOCK 9%) showed the motion *search* was only 15% and already
`psadbw`-auto-vectorized, while the real cost was MC interpolation doing per-pixel work for
copies. The win was a **copy-vs-per-pixel** structural fix, not hand-SIMD — the same class
of win as CAVLC (allocations/passes), and the opposite of the DCT batching that regressed.
**Profile → find the copy/redundant-pass/alloc win → measure → keep only if it wins.**

**How to apply (temp profiler recipe):** static `AtomicU64` ns counters per phase in
`mb16.rs`, `Instant::now()` around each phase in the per-MB loop of `encode_slice_data`,
print on `RUSTY_PROFILE` env, run `RUSTY_THREADS=1 ... --runs 1`. Remove after measuring
(`git checkout HEAD -- mb16.rs`). The bench's gradient clip is the right workload
(compressible) — NOT a synthetic noise clip (which is CAVLC-bound and misleads).

**Remaining ME/MC (assessed, not yet done):** `SadFour` batching is low-probability (source
is L1-hot, like the failed DCT batching — measure first); a cheaper SAD-threshold skip test
is openh264's approach but NOT byte-identical (changes the skip decision — a rate/quality
tradeoff for the user to weigh). See docs/openh264-me-mc.md.
