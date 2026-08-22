# CABAC decode — bring-up plan + brick list

**Goal:** decode `entropy_coding_mode_flag == 1` (CABAC) streams — Main/High profile,
the format of Blu-ray, most streaming, and virtually every encoder's default. Today the
decoder **rejects** CABAC (`lib.rs:236 → Unsupported("CABAC")`); it handles CAVLC only.

**Governing skill:** `bringup-decoder` (correctness, not speed). The gate is **bit-exact
arithmetic-coder state after every coded symbol** vs an instrumented reference oracle —
NOT a pixel diff. For H.264 the state is the two engine registers **`codIRange` (9-bit)**
and **`codIOffset`**; verify BOTH after every `DecodeDecision`/`DecodeBypass`/`Terminate`
(range alone lies — bypass/sign bits move offset, not range). One symbol, gate, next.

## What already exists (don't rebuild)
- **Engine:** `cabac.rs` — `decode_decision(ctxIdx)`, `decode_bypass()`,
  `decode_bypass_bits(n)`, `decode_terminate()`, `new(qp, init_idc, is_i)` (slice init).
  Round-trip self-consistent, but **never verified against a real CABAC stream**.
- **Context init:** `cabac_tables.rs` — `CTX_INIT[460][4]` (per cabac_init_idc),
  `RANGE_LPS[64][4]`, `STATE_TRANS[64][2]`. Sourced from openh264 → should match it exactly.

## What's missing = the bricks
Every syntax-element parser, plus the **binarization** and **ctxIdxInc** tables each needs
(none of those tables exist yet), plus residual significance-map/level coding. The CAVLC
path in `mb16.rs` gives the exact **syntax order** to mirror (mb_type → intra modes /
ref_idx+mvd → cbp → transform_8x8 → mb_qp_delta → residual); CABAC parses the same elements
through the arithmetic engine with context modeling.

---

> **✅ Phase 0 EXECUTED 2026-07-01.** Corpus built (`bench/make_cabac_corpus.sh`);
> **openh264 `h264dec` oracle built + instrumented + decoding our streams**; our-side
> symbol trace wired. Reproduce the oracle: `pip install meson` then
> `git clone --depth 1 https://github.com/cisco/openh264 && cd openh264 &&
> CC=clang CXX=clang++ meson setup build && ninja -C build codec/console/dec/h264dec.exe`,
> apply `tests/cabac_data/openh264_cabac_trace.patch`, rebuild, run with
> `RH_CABAC_TRACE=1`. Both sides print `"<n> <D|B|T> r=<codIRange> o=<codIOffset>"`
> (openh264's wide engine → `codIOffset = uiOffset >> iBitsLeft`). Reference trace for
> the fixture `cabac_i_tiny.264` (12 619 symbols) saved as
> `tests/cabac_data/cabac_i_tiny.oracle_trace.gz`. **The reference *source* is
> `parse_mb_syn_cabac.cpp`** — port the syntax bricks from it, don't reverse-engineer.
> Next: Brick 4.1/1.1 — plumb the slice-data byte offset into `Cabac::new`, gate the
> init `(r=510, o=456)` vs the oracle's symbol 0.

## Phase 0 — Oracle + harness (the foundation; blocks everything)

- **0.1 CABAC test corpus.** Our encoder emits CAVLC only, so make CABAC streams
  externally: `ffmpeg -c:v libx264` (default = High+CABAC) at a few QPs — an **I-only**
  clip (`-g 1 -x264-params keyint=1`), an **I+P** clip, and a Main-profile CABAC clip.
  Also pull the `*cabac*` streams from openh264's `res/` conformance set.
- **0.2 Instrumented reference oracle (CRITICAL PATH).** Build **openh264's `h264dec`**
  (BSD-2 — the source of our `CTX_INIT`, so contexts match to the bit; recipe in
  `memory/openh264-baseline-build.md`) in debug; add ungated `fprintf(stderr,…)` printing
  `(codIRange, codIOffset)` + the decoded value at every `Decode{Bin,Bypass,Terminate}` and
  at each syntax element. Force single-thread → deterministic trace. *(Fallback if the build
  fights us: patch ffmpeg's `h264_cabac`/`cabac.h` `get_cabac*` to trace — bigger codebase,
  same idea. Pixel-diff-only is NOT acceptable per the skill — it tells you *that*, never
  *where*.)*
- **0.3 Our-side symbol trace.** A debug feature in our `cabac.rs` printing
  `(codIRange, codIOffset)` per symbol, so the diff vs 0.2 is line-for-line, brick by brick.

## Phase 1 — Engine + init, verified against a REAL stream (not round-trip)

- **1.1 Slice CABAC init** — byte-align after the slice header, `codIRange=510`,
  `codIOffset` = 9 read bits; gate the entering state vs oracle at the first MB.
- **1.2 Context init** — the 460 contexts from `CTX_INIT` given `SliceQPy` + `cabac_init_idc`
  (clip, the `preCtxState`/`pStateIdx`/`valMPS` derivation). Dump initial state vs oracle.
- **1.3 Engine transitions** — `decode_decision`/`bypass`/`terminate` produce bit-exact
  `(codIRange,codIOffset)` on the first real symbols. First proof the engine is right on
  real data (renorm, LPS/MPS, the terminate special-case).

## Phase 2 — I-slice macroblock (the "corner block" — intra-only, simplest first)

The first MB has cleared neighbour contexts (hides neighbour bugs); get it fully exact, then
the 2nd MB (§Phase 3 lesson) exposes the neighbour-context work.

- **2.1 `end_of_slice_flag`** (`decode_terminate`) + the I-slice MB loop skeleton.
- **2.2 `mb_type`** — I-slice binarization (I_NxN vs I_16x16 prefix/suffix vs I_PCM) +
  contexts (ctxIdxOffset 3, ctxIdxInc from neighbour mb_type). **New table:** the mb_type
  bin strings + ctxIdxInc.
- **2.3 `transform_size_8x8_flag`** (High profile; neighbour ctxIdxInc).
- **2.4 Intra prediction modes** — `prev_intra4x4_pred_mode_flag` + `rem_intra4x4_pred_mode`
  (and the 8×8 variants), `intra_chroma_pred_mode` (TU + neighbour ctx).
- **2.5 `coded_block_pattern`** — luma prefix (4 bins, neighbour-cbp ctxIdxInc) + chroma
  suffix (TU, its own contexts).
- **2.6 `mb_qp_delta`** — ctxIdxInc keyed on "prev mb_qp_delta ≠ 0".
- **2.7 Residual (the biggest brick — sub-bricks):**
  - **2.7a `coded_block_flag`** — per-block, ctxIdxInc from left+top cbf (the neighbour cbf
    arrays are new state to splat).
  - **2.7b significance map** — `significant_coeff_flag` + `last_significant_coeff_flag`,
    per scan position, frame-coded ctxIdxInc maps (4×4 and 8×8 distinct). **New tables:** the
    per-position ctxIdxInc for 4×4 and 8×8.
  - **2.7c `coeff_abs_level_minus1`** — UEGk (TU prefix, ctxIdxInc from
    `numDecodAbsLevelGt1/Eq1`; EG0 suffix in **bypass**).
  - **2.7d `coeff_sign_flag`** — bypass.
  - **2.7e ctxBlockCat dispatch** — 14 block categories (luma DC/AC/4×4/8×8, Cb/Cr DC/AC,
    Intra16 DC/AC…), each with its own context offsets; a table + a dispatch.
- **2.8 Whole I-slice MB, bit-exact** — wire 2.1–2.7 and verify block-end
  `(codIRange,codIOffset)` for every block of an all-I CABAC frame vs the oracle.

## Phase 3 — P/B-slice macroblock (the neighbour-context-heavy part)

Per the bring-up rule, the **2nd real MB** (with a left neighbour) is where most context
bricks actually appear — re-verify each context once a non-degenerate neighbour exists.

- **3.1 `mb_skip_flag`** — P then B; ctxIdxInc from left+top skip (new skip-neighbour state).
- **3.2 `mb_type`** — P then B binarization + contexts (different tables than I).
- **3.3 `sub_mb_type`** — P/B 8×8 partitions.
- **3.4 `ref_idx_l0/l1`** — ctxIdxInc from neighbour ref usage.
- **3.5 `mvd_l0/l1`** — TU prefix, ctxIdxInc from the **left+top |mvd| sum** thresholds
  (new mvd-neighbour state), UEGk suffix + sign in bypass. (The MV *predictor* is recon, not
  parse — bring up the parse first, per the skill.)
- **3.6 B-slice specifics** — B_Skip/B_Direct contexts, direct/co-located mode; only after
  P is solid.

## Phase 4 — Integration + conformance

- **4.1 Dispatch** — replace `lib.rs:236` reject; branch the MB loop on
  `entropy_coding_mode_flag` (CABAC parse alongside the CAVLC one, sharing recon).
- **4.2 8×8 residual** — High-profile CABAC 8×8 significance/level (if 2.7 scoped 4×4 only).
- **4.3 Conformance gate** — decode the openh264 `*cabac*` streams bit-exact via the existing
  `examples/corpus.rs` harness (the 35/35 CAVLC oracle, extended).
- **4.4 Robustness** — panic-free on malformed CABAC (mirror the CAVLC fuzz hardening).

---

## Risks / notes (from the bring-up playbook)
- **Oracle is the critical path.** No instrumented reference ⇒ you can find *that* you
  diverged (pixel diff vs ffmpeg) but not *where*. Build 0.2 first.
- **Residual (2.7) is ~half the work** — significance-map contexts + UEGk levels + 14 block
  categories. Expect the most bricks and tables here.
- **Probe contamination** — gate every oracle/our-trace probe by (mb_addr, blockCat, scan
  pos); a shared residual fn read by many blocks will capture the wrong symbol.
- **Corner MB proves little** — all neighbour ctxIdxInc are 0/degenerate; the 2nd MB is the
  real test of skip/cbf/mb_type/ref/mvd contexts.
- **Verify `codIOffset`, not just `codIRange`** — bypass/sign bits hide a divergence behind a
  matching range until the next context-coded bin.
- Effort: large (multi-session). Sequencing: 0 → 1 → 2 (get one I-frame bit-exact) is the
  decisive milestone; if the engine+init+one-residual-block gate, the rest is grinding the
  syntax table by table.
