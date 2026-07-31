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

## Failure localised to a DATA-DEPENDENT edge case (2026-07-30)

Per-MB trace on `one8.264` (1 frame, High, qp27). Slice dies at **mb17, which is
t8=true**. Sequence before it includes mb10–mb14 all t8=true (five consecutive
8×8 macroblocks parsed fine) and mb15/mb16 t8=false immediately following a
t8=true MB — also fine.

Two conclusions:
1. **The nzc / coded_block_flag interaction is EXONERATED.** A 4×4 macroblock
   directly after an 8×8 one parses correctly, which is exactly the case that
   would break if the nzc broadcast were wrong.
2. **cat-5 is correct for most 8×8 blocks and fails on a specific one** — a
   data-dependent edge case, not a systematic error.

Additionally ruled out by trace: `cabac_ueg_level`. binIdx 0 is decoded by the
caller and binIdx 1..13 inside the function, giving exactly the 14-bin uCoff
prefix; the all-ones case yields 14 + EG0 and a 13-ones-then-zero prefix yields
13. Both correct. (This was the prime suspect because the 8×8 transform's larger
dynamic range makes the escape fire far more often than at 4×4.)

Remaining candidate edge cases, in order of suspicion:
- a coefficient count near the maximum (the inferred-last path at position 63);
- the `last_hit` break exactly at i = 62;
- a rarely-reached SIG8X8 / LAST8X8 index (the high-index entries 48..62 are only
  touched by blocks with coefficients deep in the scan).

Next step is unchanged and now much cheaper: trace the bins of **mb17** only.

## CORRECTION: the failure is SYSTEMATIC, not an edge case (2026-07-30)

Two experiments overturn the "data-dependent edge case at mb17" reading above.
Treat that section as superseded.

1. **QP sweep.** High+8×8 at qp37 also decodes 0 frames. If the fault were the
   UEG0 escape (the natural suspect, since the 8×8 transform's larger dynamic
   range makes it fire far more often), high QP would shrink levels below the
   escape and the stream would survive. It does not.
2. **A large level DID parse.** mb14 b8=2 decoded coeff_num=20, maxlevel=65,
   lastpos=41 — well past the 14-coefficient escape — without dying.

Conclusion: cat-5 is very likely wrong from the FIRST 8×8 block, and mb0–mb16
were consuming plausible-looking garbage until `end_of_slice_flag` happened to
trip at mb17. "It survived N macroblocks" is NOT evidence the parse is mostly
right when the only failure signal is an early terminate — the arithmetic
decoder produces well-formed-looking symbols from a desynced state.

The all-intra probe also confirms mb0..mb17 are ALL mb_type=0 (I_NxN), so no
I_16x16 macroblock is involved and the nzc / coded_block_flag interaction stays
exonerated.

**Revised next step.** Do not hunt an edge case. Verify the cat-5 fundamentals
against a reference on the FIRST 8×8 block of the FIRST t8=true macroblock:
the ctx-model init values at 402..416 / 417..421 / 426..435 (CTX_INIT is
populated, but its cat-5 rows have never been exercised by any working code
path), then the first ~20 bins of that block.

## Reference cross-check + a REFUTED fix attempt (2026-07-30)

Cross-checked against ffmpeg's `h264_cabac.c` (WebFetch). **Confirmed correct:**
- `SIG8X8` matches `significant_coeff_flag_offset_8x8[0]` byte-for-byte (all 63).
- The cat-5 bases: sig **402**, last **417**, coeff_abs **426** — exactly as used
  (`significant_coeff_flag_offset[0][5]`, `last_coeff_flag_offset[0][5]`,
  `coeff_abs_level_m1_offset[5]`).

`LAST8X8` could NOT be verified — ffmpeg indexes
`ff_h264_last_coeff_flag_offset_8x8[]` from `decode_significance_8x8`, and the
fetcher truncates the file before that table. h264data.c does not carry it.

**REFUTED: `LAST8X8[i] = i >> 3`.** Motivated by a context-count argument (417..425
is 9 contexts, while our table only emits 0..4, leaving 4 unused). Tested: it
desyncs EARLIER than the current table — High+8×8 regresses from 18 macroblocks
to an immediate `I_PCM` symptom. Reverted.

So the existing `LAST8X8` (0; 1×31; 2×16; 3×8; 4×7) is very likely RIGHT: its
non-uniform grouping — finer contexts near the end of the scan, where "last" is
more probable — is a sensible design, and using 5 of 9 allocated contexts is
unremarkable. **Do not "fix" this table on the context-count argument; that has
been tried and measured worse.**

Net: every constant, base and table in the cat-5 path is now either
reference-confirmed or empirically defended. The remaining fault is in the
CONTROL FLOW of the cat-5 parse or its call site, not in its data.

## openh264 cross-check — our dispatch tables are VERBATIM correct (2026-07-30)

Fetched `codec/decoder/core/src/parse_mb_syn_cabac.cpp` (openh264, the source our
CABAC bring-up mirrored). Every dispatch table matches ours exactly, index 6
(luma 8×8) included:

| openh264 | ours | value |
|---|---|---|
| `g_kMaxPos` | `RES_MAXPOS` | `{_,15,14,15,3,14,63,3,3,14,14}` |
| `g_kMaxC2` | `RES_MAXC2` | `{_,4,4,4,3,4,4,3,3,4,4}` |
| `g_kBlockCat2CtxOffsetCBF` | `RES_CBF` | `{_,0,4,8,12,16,0,12,12,16,16}` |
| `g_kBlockCat2CtxOffsetMap` | `RES_MAP` | `{_,0,15,29,44,47,0,44,44,47,47}` |
| `g_kBlockCat2CtxOffsetLast` | (reuses `RES_MAP`) | identical to Map — so reusing it is CORRECT |
| `g_kBlockCat2CtxOffsetOne`/`Abs` | `RES_ONE` | `{_,0,10,20,30,39,0,30,30,39,39}` |

Note openh264 keeps offset 0 at index 6 and switches to a separate BASE for 8×8;
we fold that into `RES_ONE[6] = 199` instead (227+199 = 426, 232+199 = 431).
Equivalent, and both reach the spec's cat-5 bases.

openh264 indexes the two 8×8 maps exactly as we do:
`iCtx = (iResProperty == LUMA_DC_AC_8 ? g_kuiIdx2CtxSignificantCoeffFlag8x8[i] : i);`
— i.e. table lookup for cat 5, raw position otherwise. That is our `is8` branch.

**`g_kuiIdx2CtxLastSignificantCoeffFlag8x8` is still not obtained.** It is defined
in neither `parse_mb_syn_cabac.cpp` (referenced only) nor
`inc/parse_mb_syn_cabac.h`, and ffmpeg's equivalent sits past the fetcher's
truncation point. **This is the ONE unverified datum in the entire cat-5 path.**

Get it by cloning openh264 or ffmpeg locally and grepping, or from H.264 spec
Table 9-43. If it differs from our `LAST8X8`, the fix is a one-line table swap.
(Remember: `i >> 3` was tried and measured WORSE — see the refutation above.)
