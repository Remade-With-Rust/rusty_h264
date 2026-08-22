# SATD-asm experiment ledger — cost/benefit, for comparing future asm/RDO bets

Executed 2026-07-01 from `docs/satd-asm-plan.md`. Format so future experiments are
comparable: **benefit** = measured speedup on the *affected path*; **cost** = risk +
RD impact + code/complexity + scope (how much of the workload it touches); **verdict**.
The single most useful column is **benefit ÷ cost** — a byte-identical swap has ~zero
cost, so even a modest speedup is a strong keeper; an RD-risky change must clear a much
higher bar.

## Baseline anchors (this machine, thermally noisy ~±10%, take max-of-N)

- Encode throughput (`profile_encode`, 832×480, QP26, asm ON):
  | preset | INTER | ALL-INTRA |
  |---|---|---|
  | Fast (default) | 44→61 Mpx/s (1.39× asm) | 18.5→21.4 (1.16× asm) |
  | Quality | **5.9→11.8 (2.0× asm)** | 17.4→21.7 (1.25× asm) |
- RD baseline (quality INTER, CIF 352×288 ×24, gop12): QP20 0.216bpp/50.62dB ·
  QP26 0.137/48.03 · QP32 0.100/43.67 · QP38 0.068/37.56.

## The bets

| # | change | path affected | **benefit** | cost | benefit÷cost | verdict |
|---|---|---|---|---|---|---|
| **3a** | `mc_satd` → `2·WelsSampleSatd` | quality-preset **inter ME** (called per diamond-search candidate — very hot) | **1.7×** (11.3→~19 Mpx/s) | **byte-identical** (RD=0), +1 helper fn, no unsafe (accel wrapper), quality-preset+asm only | **very high** | ✅ keep |
| **3b** | intra `satd_16x16/8x8/4x4` → `2·WelsSampleSatd` | quality-preset **intra mode decision** | **~1.1×** (21.6→24.0 Mpx/s, max-of-4) | **byte-identical**, *removes* the hand-Hadamard loops (simpler) | high (free win) | ✅ keep |
| P0-fast | (considered) asm SAD for fast preset | fast preset (default) | — | — | — | ❌ **no lever**: fast-preset SAD is `Σ a.abs_diff(b)` which **auto-vectorizes to `psadbw`** — already asm-equivalent. Confirmed: fast ALL-INTRA only 1.16× from asm. Don't pursue. |

## Why the numbers (the transferable findings)

1. **The asm-SATD lever is preset-specific.** The default **Fast** preset does mode
   decision by **SAD** (auto-vec `psadbw`) and has **no SATD lever**. Only the **Quality**
   preset uses SATD (Rust `satd_4x4_sum`). Measuring the wrong preset would have shown a
   fake ceiling. Phase 0 (measure both presets) was the decisive step.
2. **The `(Σ+1)>>1` vs `Σ` mismatch turned out BYTE-IDENTICAL, not RD-risky.** `Σ|H·d|`
   is *always even* (every 4×4 Hadamard coefficient shares the block-sum parity; 16 of
   them sum even), so openh264's `(Σ+1)>>1` = `Σ/2` exactly → `2·asm` = `Σ` exactly.
   Proven over 20 k random blocks (`tests/satd_asm_compare.rs`). **Characterize the metric
   before assuming a cost-function swap changes the bitstream** — this one didn't, so the
   whole thing gated on the cheap byte-exact oracle instead of the corpus BD-rate.
3. **Benefit tracks how hot the SATD call is.** 3a (inter ME, SATD per search candidate,
   many calls) → 1.7×. 3b (intra, few candidates per MB) → 1.1×. Same one-line swap, very
   different payoff — the call *frequency* on the hot path is what matters.
4. **`mc_satd` win compounds with the already-asm MC.** Quality inter was already 2× from
   asm (its sub-pel `mc_luma` is asm'd); with MC asm'd, the Rust SATD in the same
   refinement loop became the next bottleneck — so wiring it paid disproportionately.

## Follow-ups (children of this node)

- **AVX2 SATD** — we wired `_sse2`; openh264 ships `WelsSampleSatd*_avx2`. For 16×16 the
  data fills 256-bit lanes, so unlike the 4×4 transform (which was FLAT on AVX2) this
  *might* pay on quality inter. Cheap to try behind `has_avx2()`; gate on `profile_encode`.
- **Lookahead SATD** (`lookahead.rs:satd4`) — a separate Rust SATD in the GOP/complexity
  estimator; wire the same way if the lookahead shows up hot (doesn't affect the bitstream).
- **Fast-preset ceiling is structural** — its cost is SAD(psadbw, optimal) + entropy(CAVLC,
  serial) + glue; no asm lever remains. Future fast-preset speed is a pure-Rust game.
