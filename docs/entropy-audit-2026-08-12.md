# Entropy syntax layer audit — 2026-08-12

Three-agent sweep (decoder CABAC syntax, CAVLC layer, encoder↔decoder CABAC
symmetry) plus a header-layer pass, hunting mis-wired functions, inactive
functions, silent zeroing, and wrong-function substitutions.

## Fixed this session (all gated)

| id | defect | gate |
|---|---|---|
| S2 | Multi-slice CABAC: `decode_slice_cabac_inner` never set `slice_first_mb`, and ctx neighbour `left`/`top` ignored slice membership (per-slice array defaults are not the "unavailable" value: `cat` 255 reads as available-I16, `mb_cbp` 0 as zero-cbp, `mb_t8x8` is a frame grid). Every non-first slice desynced. | x264 `--slices 4` CABAC + CAVLC streams byte-identical vs ffmpeg; single-slice regression clean |
| M1 | `qpprime_y_zero_transform_bypass_flag` parsed and DISCARDED — lossless-bypass MBs silently mis-decoded (observed: deterministic garbage on the non-PCM half of an x264 qp0 stream). Now a targeted refusal in `step_qp` (fires only for residual-coded MBs at QP'Y==0 with the flag set), so all-PCM lossless streams still decode. | lossless-mixed refuses with named error; all-PCM vector + normal smoke still byte-identical |
| C1 | `encode_inter_mb_v2` omitted `sub_mb_type` (P_8x8) and `transform_size_8x8_flag` — desync the moment `coded_path_v2=true` on an accel build; its only gate was `#[ignore]`d. | `coded_path_ab`: BYTE-IDENTICAL, v2 −12.7% vs v1 |
| C3 | `read_cbp_intra/inter` mapped out-of-table `me(v)` to cbp 0 (silent garbage continuation) — now errors like every other unmatched VLC. | suites |
| C4 | `mb_skip_run` past picture end silently clamped — now `Truncated` (run *to* the end stays legal). | suites |
| C5 | CAVLC `mvd` accepted ±2^31−1 from `se(v)` and fed unchecked `pmv + mvd` (debug overflow panic / release wrap) — bounded at ±2^17 quarter-pel via `read_mvd`. | suites + fuzz |
| C7 | `decode_residual_block` scatter guard used the 16-wide array bound, not `max_coeff` (4 for chroma DC, 15 for AC). | suites |
| S4 | `qp_delta()` had no `[-26, 25]` legality guard — `debug_assert` added (external `qpo` maps could exceed; decoder reconstructs via §7.4.5 modulo either way). | suites |
| S5 | `NZC_CACHE`/`RES_*`/`CACHE30`/`G_SCAN4` were byte-copies in both mb16.rs files — now one copy in `cabac_tables.rs`. (The reported `CB_RES_ONE[6]` 0-stub drift was already healed; 199 both sides.) | suites |
| S6 | Bit accountant: skip paths counted `B::Terminate` twice (nested duplicated block), the RD-B_Skip arm zero times — reconciliation was impossible on skip-heavy slices. | build |
| S7 | Zero pairwise coverage for `cb_ref_idx`/`cb_cbp`/`cb_mb_qp_delta` ↔ their parsers, and no multi-ref test anywhere (`num_ref_frames` default 1). Four new tests: full-range round-trips + `num_ref_frames=3` e2e. | new tests green |

## Refuted (do not re-litigate)

- **B_Direct neighbour refIdx ctx forcing 0**: matches ffmpeg's `DIRECT2`
  exclusion and the correct reading of §9.3.3.1.1.6; empirically byte-identical
  on x264 `--ref 3 --bframes 3 --preset slower` (60 frames) and the 1800-frame
  slower corpus.
- **`RES_ONE[6]=199` vs `CAT5_LEVEL_BASE=426` divergence**: consistent by
  construction (227+199=426, 232+199=431); no special case needed.
- **All CABAC ctxIdxOffset/ctxIdxInc derivations**: verified against Tables
  9-34..9-40 and §9.3.3.1.1.x — no mis-wiring found. `CTX_INIT` models 1..3
  spot-verified.

## Known gaps — deliberate, documented, NOT silent

- **`direct_8x8_inference_flag = 0` is formally non-conformant at level ≥ 3.0**
  (spec requires 1 there; every 720p+ encode carries it). Flipping it changes
  direct-motion derivation = bitstream change → needs the encoder's direct
  derivation updated in lockstep + BD gate. The encoder hard-codes the
  flag's consequence (`allow8` for B_Direct) with no cross-file assertion.
- **Encoder cannot emit**: intra MBs in CABAC B slices (no `m4==13` escape in
  `cb_mb_type_b` — BD-rate cost on scene cuts, not conformance), B_8x8
  (`cb_sub_mb_type_b` does not exist), B multi-ref (`write_b_slice_header`
  hard-codes 1,1 — wiring multi-ref without the ref_idx writer is an instant
  desync), CABAC I_PCM (no writer; decoder side is done + gated).
- **Encoder nC neighbour availability ignores slice boundaries**
  (`nnz_cache_load`/`chroma_cache_load` use `mb_x/mb_y == 0` only) — safe
  solely because the encoder always writes `first_mb_in_slice = 0`. First
  multi-slice encode feature must add the slice gate or desync at slice 2.
- **`disable_deblocking_filter_idc == 2`** (filter within slice only) is
  collapsed to "on" — now reachable since multi-slice decode works; x264
  defaults don't emit it.
- **4:2:2 landmine** commented at the sig/last ctx site: `(i,i)` is right only
  while chroma-DC `NumC8x8 == 1`.
- **`ref_idx` clamp on corrupt input** (`.clamp(0,15)` + MC from wrong ref
  instead of erroring): intentional fuzz-armor, documented at the parser.
- Dead-but-kept: `refc[0]/refc[5]` seeding (C/D positions unused by mvd/ref
  ctx — removal is a perf micro-brick), openh264's CAVLC-only `RES_*` rows
  4/5/0.
