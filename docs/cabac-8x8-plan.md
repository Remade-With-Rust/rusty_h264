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

## Debug state (localised — resume here)

Probe: `x264 --keyint 1 --qp 27 --frames 1 --profile high` (foreman_cif), 396 MBs.

- `transform_size_8x8_flag` **decodes correctly**: 19 I_NxN MBs parse before the
  stream dies, **8 of them read t8=true**. A wrong ctx or a phantom bin would
  give all-false or immediate death, so ctxIdx 399+A+B is right.
- Failure therefore lands in the **ctxBlockCat 5 residual**, shortly after the
  first true 8×8 blocks. The parse desyncs (frame never completes).

Ruled out by direct inspection, do not re-check:
- SIG8X8 / LAST8X8 match spec Table 9-43 entry-by-entry.
- CTX_INIT is fully populated at 399..435 with real spec values (not zeros).
- cat 5 correctly has NO coded_block_flag (spec parses it only when
  `maxNumCoeff != 64 || ChromaArrayType == 3`).
- Level bases: 227+199=426 and 232+199=431 are the spec cat-5 bases; maxc2 = 4.
- `RES_MAXPOS[6]=63` is maxNumCoeff-1, and the sig loop runs `0..63` then infers
  position 63 — matching the spec's `i < maxNumCoeff-1`.
- nzc is broadcast to all four 4×4 cells of the 8×8 (matches ffmpeg's
  `fill_rectangle` of nnz), so neighbour contexts in later MBs are fed.

**Next step: bin-level diff.** Run our decoder with `RH_CABAC_TRACE` against an
instrumented reference (the openh264 oracle used for the original CABAC bring-up)
on the FIRST t8=true macroblock, and find the first bin index where the two
disagree. Inspection has been exhausted; this needs the trace.

## Shared 8×8 machinery PROVEN GOOD (2026-07-30)

`x264 --profile high --no-cabac --keyint 1 --qp 27 --frames 3` (foreman_cif) —
i.e. CAVLC **with** the 8×8 transform — decodes **byte-identical to ffmpeg**.

That eliminates half the search space for good. Proven correct on real
High-profile content, do not investigate:
`un_scan_8x8`, `inv_quant8`, `intra8x8_pred`, `gather_i8`, `add_residual_8x8`,
the 8×8 scan, and the I_8x8 prediction/reconstruct path.

**The defect is entirely in the CABAC ctxBlockCat 5 BIN PARSE.** Combined with
the earlier finding that the failure is an early `end_of_slice_flag` (silent
frame drop, no error), the fault is a wrong NUMBER of bins consumed — not wrong
coefficient values, not reconstruction.

Also confirmed not at fault: the nzc broadcast. Cat 5 has no coded_block_flag, so
this MB's own bins cannot depend on it, and `coeff_num` is always ≥ 1 for a coded
8×8 (the inferred last coefficient guarantees it), so a following 4×4 MB's
coded_block_flag ctxIdxInc — which only tests non-zero-ness — is fed correctly.

## Prerequisite fix for the next attempt

A slice that terminates before `total_mb` currently drops the picture SILENTLY
(`decode_prof` reports 0 frames, no error). Raise `Truncated` there first: this
bug presented as "wrong pixels" for hours when it was actually an early slice
end, and any future desync of this class should announce itself.
