# Plan — wire asm SATD into encoder mode decision (P1 from `asm-targets.md`)

**Goal:** close the intra-encode asm gap (measured **1.14×** vs 1.44× inter) by routing
the mode-decision SATD search through openh264's `WelsSampleSatd*_sse2` kernels (already
vendored + exported in `rusty_h264-accel`, currently **uncalled**), without regressing
compression.

**The obstacle (why this isn't a normal bit-exact asm swap):**
our Rust SATD is `Σ|H·d|` (`common/src/transform.rs:556`); openh264's asm is
`(Σ|H·d| + 1) >> 1` (`accel/src/lib.rs:460`) — **~half**. SATD is a *heuristic cost*
that steers mode decisions, not bitstream output, so we have latitude the MC/IDCT/deblock
kernels don't. But halving it shifts the `SATD vs λ·rate` balance at the RD-combination
sites → **different (still valid) mode decisions → different output bytes**. So the gate
is the **RD/PSNR corpus (`bench/`), not the byte-exact oracle**.

Discipline: one site per commit; the "keep it" test is *encode faster **and** RD neutral-
or-better*; revert otherwise. Same measure-first rigor as the decode work.

---

## Phase 0 — Size the prize + freeze the RD baseline (do first, cheap)

- **Sub-profile the SATD share of intra encode.** Add a `prof` stage around the SATD
  search (or reuse `profile_encode`) so we know SATD's fraction of ALL-INTRA. Ceiling
  math: if SATD ≈ ⅓ of intra encode and asm is ~3× on it, the intra-encode win is ~1.2×.
  If SATD is only ~15%, the prize is small → reconsider before investing.
- **Freeze the RD baseline.** Run the `bench/` RD sweep (bpp + Y-PSNR at QP 22/26/30/36,
  intra + inter, matched 1-ref) on today's encoder. These numbers are the bar the swap
  must not regress. Save them in this doc.
- **Decision gate:** proceed only if SATD is a real fraction of encode time.

## Phase 1 — Characterize the metric mismatch EXACTLY (unit test, no perf)

- New test: for many random `(src, pred)` pairs at 4×4 / 8×8 / 16×16, compare
  `rusty_h264_accel::satd_NxN` against the encoder's Rust `satd_NxN`. Establish the exact
  relation at each size — is it uniformly `asm = (rust + 1) >> 1`? Does `2·asm` land within
  `±1` of `rust` **always**? (Determines whether "×2-scale" is effectively lossless.)
  ⚠️ openh264 may apply the `+1>>1` **once per block** vs **once per MB** — do NOT assume;
  the test decides it.
- **Map the call sites into two buckets:**
  - *Pure-argmin* (SATD compared only to other SATDs): monotonic → asm is safe there even
    unscaled. e.g. i16 mode pick among 4, I4×4 mode among 9.
  - *RD-combination* (SATD `+ λ·bits`, or intra-vs-inter-vs-skip): the risk sites. e.g.
    `best_i16_satd + …` at `mb16.rs:1488`, `mc_satd` in ME.
  Sites: `best_i16_satd` (1158/1900), `mc_satd` (290/1748), I4×4/I8×8 search, chroma
  (2001) — enumerate precisely before touching any.

## Phase 2 — Choose the alignment strategy (measure the cheap one first)

- **Strategy A — scale `×2` at the call site.** Use asm, multiply by 2 → within `±1` of
  the Rust magnitude (Phase 1 confirms). Mode decisions flip only on exact ties (expect
  ~0.0x% of MBs). Cheapest; likely RD-neutral. **Try this first.**
- **Strategy B — adopt openh264's metric everywhere.** Change the Rust `satd_4x4_sum`
  reference to `(Σ+1)>>1` too, and halve `λ` (or the rate term) to rebalance. Then Rust
  and asm are **bit-identical** → asm becomes a permanent bit-exact drop-in, and we're
  aligned with openh264's battle-tested cost. More work (λ retune + full RD sweep), but
  the clean long-term end state.
- **Recommendation:** A first (a one-liner per site + an RD check). Only fall to B if A
  moves RD measurably. Record which won here.

## Phase 3 — Wire it, one site per commit (bricks; gate each)

Order by isolation (smallest blast radius first):

1. **`mc_satd` (inter ME refinement).** Swap → asm `satd_16x16`/`8x8`. Gate: inter encode
   Mpx/s up (`profile_encode`) **and** inter RD sweep neutral.
2. **`best_i16_satd` (intra 16×16 mode).** Pure-argmin-ish → lowest risk. Gate as above.
3. **I4×4 / I8×8 candidate search** (`satd_4x4`/`satd_8x8`) — the hottest, most candidates.
4. **Chroma SATD.**

Each brick: swap one site → `cargo test` (still decodes/round-trips) → RD sweep (no
regression) → `profile_encode` (faster) → keep or revert. Bit-exactness is **not** the
gate here; RD + validity are.

## Phase 4 — Validate + document

- Full `bench/` RD sweep + PSNR corpus vs the Phase-0 baseline: **confirm no regression**
  (retune `λ` if Strategy B). Confirm every stream still decodes under ffmpeg.
- Measure final ALL-INTRA / INTER encode speedup; update `docs/asm-targets.md`, the README
  encode rows, and `docs/benchmarks.md`.
- If Strategy B: note that asm SATD is now a bit-exact drop-in and the Rust path mirrors
  openh264 exactly (useful for future SATD-adjacent kernels).

---

## Honest expectations

The prize is bounded by Amdahl (Phase 0 quantifies it): intra encode ~1.14× → maybe
~1.2–1.3× if SATD is ~⅓ of it. Worth doing because it's the **one** untapped asm lever on
the encoder, and P2/P3 (asm intra-predictors, sub-block SAD) compound on top. It is a
heuristic change gated on RD, so the risk is *compression drift*, not decode failure —
which is exactly why it must be measured on a corpus, never a single clip.
