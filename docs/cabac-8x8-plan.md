# CABAC 8×8 (High profile) — implementation plan

**Status: NOT IMPLEMENTED.** The decoder fails fast and accurately on
`CABAC + transform_8x8_mode_flag` (lib.rs, H-49). This file is the scope needed
to remove that guard.

## Why the old failure was misleading

`transform_size_8x8_flag` is read in exactly two places (mb16.rs ~1743, ~1879),
both `r.read_bit()` on the CAVLC `BitReader`; `decode_i8x8` also takes a
`BitReader`. The CABAC macroblock loop never reads the flag at all, so a PPS
with `transform_8x8_mode_flag` set desynced the arithmetic decoder within a few
MBs and the mb_type parse landed on 25 out of garbage — reported as
`Unsupported("CABAC I_PCM")`. **I_PCM was a symptom.** Evidence: CABAC *main*
all-intra decodes at qp 1/3/6/27; CABAC *high* fails always; main and high
differ here only by the 8×8 transform.

## What exists already

- I_8x8 prediction, 8×8 dequant/IDCT, and the 8×8 scan — used by the CAVLC
  `decode_i8x8`. The transform side is done and conformant (see
  `transform-8x8-state`); this is a **CABAC reader** gap only.
- `RES_MAXPOS[6] = 63` — the luma-8×8 slot exists in the residual dispatch
  table, but `RES_CBF[6]`, `RES_MAP[6]`, `RES_ONE[6]` are all 0 placeholders and
  there is no `RP_LUMA_8X8` constant. The category was stubbed, never written.

## Work items

1. **`transform_size_8x8_flag` (CABAC).** ctxIdxOffset 399;
   `ctxIdx = 399 + condTermFlagA + condTermFlagB`, where each condTermFlag is 1
   when that neighbour MB has the flag set. `mb_t8x8` already tracks it per MB
   (deblocking uses it), so the neighbour reads exist.
   Two syntax positions: for I_NxN immediately after mb_type and BEFORE the
   intra pred modes; for inter after CBP when `CodedBlockPatternLuma > 0` and
   `noSubMbPartSizeLessThan8x8Flag`.
2. **Luma 8×8 residual, ctxBlockCat 5.** Note it has **no coded_block_flag** —
   presence is inferred from CBP, unlike every category currently implemented.
   Needs: `significant_coeff_flag` ctxIdxOffset 402 with the 63-entry
   position→ctxIdx map (spec Table 9-43), `last_significant_coeff_flag` offset
   417 with its own 63-entry map, `coeff_abs_level_minus1` offset 426. Fill
   `RES_CBF/RES_MAP/RES_ONE[6]` and add `RP_LUMA_8X8`.
   The two 63-entry maps are the only genuinely new spec tables.
3. **Route I_NxN → I_8x8 under CABAC.** `decode_i8x8` is `BitReader`-bound;
   split its prediction/reconstruct half from its parsing half so the CABAC
   path can share it (the CAVLC path must stay byte-identical — gate on it).
4. **Inter 8×8 transform** for P/B macroblocks carrying the flag.
5. **Validation.** Decode x264 `--profile high` output and `cmp` the YUV against
   ffmpeg byte-for-byte, across ≥4 QPs and both all-intra and IPB. Then re-run
   the x264 BD harness WITHOUT the `--profile main` re-anchor from a76d9a4.

## Why this matters beyond conformance

High + CABAC + 8×8 is **x264's default output**. Until this lands, every decoder
speed figure in `WHYS-speed-gap.md` — including "2.52× of ffmpeg" — is measured
on Main-profile content and does not generalise to what x264 actually produces
by default.
