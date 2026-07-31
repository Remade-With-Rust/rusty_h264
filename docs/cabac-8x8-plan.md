# CABAC 8×8 (High profile) — implementation plan

**Status: INTRA 8x8 DONE (bit-exact). Inter 8x8 remains.** The decoder fails fast and accurately on
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


## RESOLVED — the bug was LAST8X8 (2026-07-30)

`git clone --depth 1 cisco/openh264` produced the table in one command:
`codec/decoder/core/inc/wels_common_basis.h:121`,
`g_kuiIdx2CtxLastSignificantCoeffFlag8x8[64]` — "Table 9-43, Page 289".

Correct mapping: `0; 1x15; 2x16; 3x8; 4x8; 5x4; 6x4; 7x4; 8x4` — values 0..8,
exactly filling the 9 contexts at 417..425. Ours had `0; 1x31; 2x16; 3x8; 4x7`
(max 4), which is wrong.

The 9-context argument that motivated the earlier `i >> 3` attempt was therefore
CORRECT reasoning with the wrong function. Lesson: when a structural argument
says a table is wrong, that is a reason to GO GET the table, not to guess a
plausible formula — the guess cost a cycle and measured worse.

Result: x264 High-profile all-intra (8x8 + CABAC, x264's default tools) decodes
**byte-identical to ffmpeg** at qp 18/22/27/32/37/44 on foreman and at qp 22/32
on mobile — 8/8.

INTER 8x8 is still unimplemented: the flag is also read after CBP when
CodedBlockPatternLuma > 0 and noSubMbPartSizeLessThan8x8Flag (see `allow_8x8`,
mb16.rs ~2818, for the existing CAVLC condition). Both CABAC inter paths now
raise `Unsupported("CABAC inter transform_8x8")` rather than desyncing, so those
streams fail loudly. Remaining work: read the flag there, route cat-5 for inter
luma, and add the 8x8 inverse transform to `add_inter_residual`.

## Inter 8×8 implemented — and a PRE-EXISTING CABAC inter bug found (2026-07-30)

All three remaining items are implemented: `transform_size_8x8_flag` in both CABAC
inter paths (after CBP, gated on `CodedBlockPatternLuma > 0` and a real
`noSubMbPartSizeLessThan8x8Flag` — `allow8`, derived from P sub_mb_types, B
sub_mb_types, and `direct_8x8_inference_flag` for direct MBs), cat-5 routing for
inter luma, and an 8×8 branch in `add_inter_residual` reusing `un_scan_8x8` /
`inv_quant8` (inter list 1) / `add_residual_8x8`, setting `nnz_y`, `coded_y` and
`mb_t8x8`.

It cannot be validated to byte-exactness, because a SEPARATE PRE-EXISTING BUG
sits underneath it. Control experiment on 8-frame P-only foreman @ qp27:

| stream | vs ffmpeg |
|---|---|
| Baseline (CAVLC) P | **byte-exact** |
| Main (CABAC) P | differs at byte 304185 |
| High (CABAC) P, `--no-8x8dct` | differs at byte 304185 — SAME byte |
| High (CABAC) P, with 8×8 | differs at byte 304185 — SAME byte |

High-without-8×8 fails identically to High-with-8×8, and Main fails the same way,
while Baseline is exact. **The fault is in CABAC INTER decoding generally, not in
the 8×8 path.** Main P differs from Baseline P only by CABAC.

`long.264` — the stream behind every "2.52× of ffmpeg" figure — is ALSO not
bit-exact: frames 1 and 9 of the first 12 differ (max delta ~51) while 2–8 are
clean. Localised, non-propagating errors on non-reference frames: the signature of
a B-macroblock defect. Frame 0 (intra) is perfect.

**This supersedes the crate doc's "gated pixel-exact vs ffmpeg" claim for CABAC
inter.** Whatever gate produced that claim did not cover multi-frame CABAC P/B
against ffmpeg. Highest-priority next work, ahead of any optimisation:
1. Add a `cmp`-vs-ffmpeg conformance gate over CABAC P and B streams at several
   QPs (this is the missing test, and it would have caught the above).
2. Bisect the P divergence (Baseline-exact vs Main-differs isolates it to CABAC
   inter residual/MV parse), then the B one.

## Decoder conformance gate + P-divergence bisect (2026-07-30)

`bench/conf_x264_decode.sh` — the missing gate. x264 encodes, we decode, ffmpeg
decodes, byte-compare. Sweeps entropy coder × GOP structure × QP, because the
defect is invisible on CAVLC and on intra-only content.

First run, foreman_cif 8f: **PASS 24 / FAIL 24**, and the split is perfectly clean:

| axis | result |
|---|---|
| Baseline (CAVLC), all GOPs, all QPs | **12/12 pass** |
| CABAC, intra-only (main/high/high-no8x8) | **12/12 pass** |
| CABAC, P-only and IPB (main/high/high-no8x8) | **0/24 pass** |

### Bisect: it is CABAC × MULTI-REF

| configuration | result |
|---|---|
| x264 defaults (3 refs) | FAIL |
| `--ref 1` | **PASS** |
| `--ref 1 --no-mixed-refs --partitions p16x16` | **PASS** |
| `--ref 1 --partitions p16x16,p8x8` | **PASS** |
| `--ref 3 --partitions p16x16` | FAIL |

Single-reference CABAC inter is exact even with sub-8×8 partitions; plain P_L0_16x16
fails as soon as there are 3 references. And CAVLC passes at x264's default 3 refs,
so **reference-list construction, DPB sliding window and MC are all fine** — the
fault is CABAC-specific handling of multiple references.

Ruled out by inspection: `parse_ref_idx_cabac` matches spec §9.3.3.1.1.6/Table 9-34
(ctxIdxOffset 54; bin 0 → ctxIdxInc 0..3, bin 1 → 4, bins ≥2 → 5), and the
ctxIdxInc `(refc[left] > 0) + 2*(refc[top] > 0)` matches condTermFlagA + 2·condTermFlagB
with intra/unavailable neighbours reading -1.

Next candidates, in order:
1. The `refc` neighbour cache: it is seeded from `mb_ref` for left/top, but the
   within-MB writes in `refidx!` may not cover every block a later partition reads,
   so partition 2+ of a 16×8/8×16 could see a stale neighbour ref.
2. `predict_partition_mv`'s ref-matching — with one reference every neighbour
   matches trivially, which is exactly why `--ref 1` hides the bug.

### Refined: it is NOT ref_idx parsing — chroma is exact

`--ref 2 --partitions p16x16 --profile main`, per-plane:

| frame | Y diff | U diff | max delta |
|---|---|---|---|
| 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 |
| 2 | 20981 | **0** | 4 |
| 3 | 30551 | **0** | 6 |

Fails from `--ref 2` (not just 3), and `--no-weightp` changes nothing.

**Chroma being exact is the whole finding.** Motion compensation reads the same
reference picture and the same MV for luma and chroma — a wrong `ref_idx` or a
wrong MV would corrupt BOTH planes. It corrupts neither. Together with no desync
(every frame completes, correct size), that clears:
- `parse_ref_idx_cabac` and its ctxIdxInc (already spec-verified),
- reference-list construction and DPB order,
- `predict_partition_mv` ref matching,
- motion compensation.

What is left is a LUMA-ONLY post-reconstruction stage: **deblocking boundary
strength**. `deblock()` maps each block's `ref_idx` through
`self.refs[r].poc` into `ref_id` so bS can compare picture identity across lists.
With ONE reference every block maps to the same POC and that comparison can never
fire — which is exactly why `--ref 1` passes and `--ref 2` does not, and why this
survived every previous test.

Note the deltas (≤6) and the pattern are consistent with wrong bS on the internal
luma edges at x/y = 4 and 12, which have no chroma counterpart in 4:2:0 (chroma
edges map only to luma 0 and 8) — that is why chroma stays clean even though the
filter runs.

**Next step: compare our bS derivation against the spec §8.7.2.1 mixedModeEdgeFlag
/ different-reference rule for the multi-reference case, starting with the
`ref_id` POC mapping in `deblock()` and `gather_tile`'s per-block ref handling.**

---

## 2026-07-30 — full-corpus validation run: the 8×8 work is CLEAN; three decoder defects separated

Ran the whole gate battery on real clips to answer "does the recent encoding work
cause any issue". Two results matter more than the pass counts.

### The recent CABAC 8×8 work is exonerated

Every encoder gate is green, including on the real corpus:

| gate | scope | result |
|---|---|---|
| `cargo test --release --workspace --features asm` | 19 binaries | 136 pass / 0 fail |
| `conf_ffmpeg` | default config, **all 20 clips** × 2 presets × 2 QPs | **80 / 0**, pixel-exact |
| `bench/conf_matrix.sh` (NEW) | 18 tool configs × 4 QPs × 4 clips | **256 / 0**, 32 refused cleanly |

`conf_matrix.sh` is new and is the gate that was missing on the ENCODER side:
`conf_ffmpeg` only ever exercised the default config, so every opt-in lever we have
landed — CABAC, 8×8, B-frames, sub-8×8, multi-ref, wide ME, mb-tree, AQ — shipped
outside any external conformance check. Zero FFREJ and zero DIFF across all of them.

The 32 refusals are all one documented combination, `--cabac 1 --transform-8x8 1`,
rejected with an explicit `unsupported:` message. That guard is CORRECT but its
stated reason was STALE — it blamed the decoder, which gained CABAC 8×8 in c1375d1 /
d137218. The real blocker is the ENCODER: `emit_mb_cabac_*` has no
`transform_size_8x8_flag` and no ctxBlockCat-5 residual. Comment corrected.

### The decoder failures are on axes ORTHOGONAL to the 8×8 work

Bisect over profile × bframes × ref × b-pyramid, foreman_cif, 24f, qp27, decoded
against ffmpeg. **The `profile` column is identical for main and high in every single
row** — i.e. turning the 8×8 transform on changes no outcome, which independently
clears the newly-landed cat-5 / `transform_size_8x8_flag` decode path of all three
failures below.

| bframes | ref | b-pyramid | result |
|---|---|---|---|
| 0 | 1 | — | exact |
| 0 | 3 | — | differs |
| 2 | 1 | normal / none | **exact** |
| 2 | 3 | none | differs |
| 2 | 3 | normal | **DECODE FAIL — "bitstream truncated"** |
| 3 | 1 | normal / none | differs |
| 3 | 3 | none | differs |
| 3 | 3 | normal | **DECODE FAIL — "bitstream truncated"** |

Three independent defects, not one:

1. **multi-reference** (`--ref > 1`, any GOP) — the deblock bS `ref_id` bug already
   localized above. Fires with B-frames entirely absent.
2. **B-depth ≥ 3 on a long GOP** (`--bframes 3`, even at `--ref 1`, either pyramid
   setting) — a SEPARATE defect. `--bframes 2 --ref 1` is byte-exact, so this is not
   "B-frames are broken"; it is specific to the deeper B structure.
   **It is GOP-LENGTH-DEPENDENT**, which nearly cost the finding: measured on
   foreman_cif, `--bframes 3 --ref 1 --b-pyramid none` is byte-exact at `--keyint 12`
   and diverges at `--keyint 30` and `--keyint 60`, at both 24 and 48 frames. The
   first bisect used keyint 60 and saw it; the gate arm was written with keyint 12
   and passed. Both measurements were correct about their own configuration — a
   short GOP re-anchors on an I-frame before the defect can express. The `ipb3` gate
   arm is therefore pinned to `--keyint 30`.
3. **B-as-reference + multi-ref** (`--b-pyramid normal --ref 3`) — a PARSE failure,
   not a divergence. Distinct class: nothing is reconstructed at all.

Defects 2 and 3 were invisible to the gate as written, because its single `ipb` arm
used `--bframes 2` at x264's default `--ref`. The gate now sweeps `ipb` / `ipb3` /
`pyr` separately, and reports PARSE failures apart from DIFF ones.

### A measurement-validity trap in the BD-rate harness (D6)

`x264_bdrate` scores x264's streams through OUR decoder, so any decoder defect on
x264's output is charged to x264 as distortion and **flatters our BD-rate**.

- The DEFAULT arm is safe: it pins x264 to `--ref 1 --bframes 0 --profile main`, and
  that combination was re-verified byte-exact vs ffmpeg at qp27 and qp37 on both
  foreman_cif and mobile_cif. The BD tables from this run are sound.
- `XB_ALLTOOLS` is NOT safe: it moves x264 to `--ref 3 --bframes 3`, which trips all
  three defects above (including the parse failure). The harness now REFUSES to run
  with `XB_ALLTOOLS` rather than print a biased number that reads authoritative.

Re-enable `XB_ALLTOOLS` only once `bench/conf_x264_decode.sh` is green on the
multi-ref and B axes.

---

## 2026-07-30 (later) — ROOT CAUSE: weighted prediction missing from the CABAC inter path

`weight_partition` is called by the CAVLC inter path (mb16.rs ~2025) and by P_Skip.
It is **never called in the CABAC inter path**. The MC-call-coalescing rewrite that
fused per-block MC into the rect ladder dropped it, and nothing caught the loss
because the effect is invisible unless a stream carries non-default weights.

x264's `weightp` **duplicates a reference picture** and distinguishes the copy ONLY
by its weights. Our dump confirms it:

```
RefPicList0: [0] poc=2 fn=1  [1] poc=2 fn=1  [2] poc=0 fn=0
```

So every macroblock selecting the weighted index was reconstructed unweighted.

| observation | explained by |
|---|---|
| CAVLC exact, CABAC wrong | only the CABAC path lost the call |
| `--ref 1` exact, `--ref >= 2` wrong | with one reference there is no weighted duplicate |
| **100% of `ref_idx==1` MBs wrong** (81/81) | index 1 IS the weighted duplicate |
| chroma exact at every QP incl. qp12 | x264 weights LUMA ONLY; chroma stays default |
| starts at delta 1, accumulates to 10 | weights are near-neutral; drift compounds via refs |
| survives `--no-deblock`, qp51, every partition/mixed-ref ablation | none touch weighting |

### CORRECTION to the previous two entries

- **The deblock-bS conclusion was wrong.** Refuted by the first probe of this
  session: `--no-deblock --ref 3` still diverges. Deblocking was never involved.
  The chroma-exact reasoning that produced it was structurally sound but rested on
  an untested premise; the premise was finally checked (frame0 vs frame1 chroma
  differs in 26.9% of samples, so chroma COULD see a reference mix-up) and that is
  what kept the "wrong picture at index 1" line of enquiry alive.
- **"Three independent defects" was wrong — there were two.** Defects 1
  (multi-reference) and 2 (B-depth >= 3 on a long GOP) had the SAME root cause and
  BOTH cleared with this single fix. B-depth only looked separate because deeper B
  structures make x264 use more references, hence more weighted duplicates.

Gate on foreman_cif, 24f: **PASS 44 -> 68, FAIL 36 -> 12.** All twelve combinations
of {CAVLC, CABAC} x {1,2,3 refs} x {deblock on, off} are byte-exact.

### Still open

`pyr` (`--b-pyramid normal --ref 3`) remains a PARSE failure, "bitstream truncated",
on all three CABAC profiles x 4 QPs. Unrelated to weighting: nothing decodes at all.
B-as-reference means ref-list modification / MMCO handling, which is where to look.

### Instrument

`RH264_DUMP_MB=1` (decoder, stderr) prints per frame: a per-MB map of List-0
reference index (`i` = intra), a reference histogram with an out-of-range count, and
`RefPicList0` with each entry's POC / frame_num plus a `SYNTH-GREY` marker for
frame_num-gap frames. The duplicate-entry line above is what cracked this; the
remaining defect will need the same instrument.

### The transferable lesson

Two of the wrong turns came from treating a plane-level observation as a
localization. "Chroma is exact" was read as "prediction inputs are correct", but its
real content was "whatever is wrong does not affect chroma" — and a tool that
weights luma only satisfies that while corrupting prediction. **Before inferring
from an untouched signal, verify the signal COULD have moved.** One measurement
(27% chroma difference between the candidate references) converted a dead end back
into the live hypothesis that found the bug.

---

## 2026-07-30 (later still) — `pyr` ROOT CAUSE: CABAC B slices never parsed ref_idx

The `pyr` failure was NOT b-pyramid, MMCO, or reference-list modification. Those
were all downstream.

`parse_ref_idx_cabac` had exactly ONE call site — the P path. The CABAC **B** path
never parsed `ref_idx_l0` / `ref_idx_l1` at all; mb16.rs said so in its own comment,
"ref not coded on this 1-ref stream". Any B slice with more than one active
reference in either list therefore desynced the arithmetic decoder at the first
partition coding a ref_idx.

### The causal chain (why it presented as a parse failure far away)

1. B slice desyncs → hits a phantom `end_of_slice_flag` → ends early
   (measured: 64/396, 303/396, 28/396 macroblocks).
2. A picture is only finalized at `next_mb >= total_mb`, so the incomplete picture
   was **silently dropped** and never entered the DPB.
3. A later slice's `ref_pic_list_modification` asked for that missing picture.
4. `apply_list_modification` returned `Truncated` → **"bitstream truncated"**,
   hundreds of macroblocks after the actual fault.

`--b-pyramid normal` was only the REVEALER: it makes the dropped picture a
*reference*. With `--b-pyramid none` the very same desync dropped only
non-reference B frames, so it read as "differs" instead of a parse failure. One
bug, two faces — and the two faces were logged as two separate defects.

### Fix

`ref_idx_l0`/`ref_idx_l1` parsed in spec order — 7.3.5.1 (all L0 for every
partition, then all L1, then the mvds) and 7.3.5.2 (ONE ref per 8x8, never per
sub-partition; `B_Direct_8x8` codes none) — with ctxIdxInc from the existing
`refc0`/`refc1` neighbour caches. The parsed reference is threaded through
`predict_mv` / `predict_partition_mv` (`cur_ref`), `parse_mvd_partition`,
`b_set_motion` and `b_mc`, all of which had `0` hardcoded.

Two hardening changes came with it:

- **`b_mc` clamps its reference indices.** Previously refi was 0 by construction,
  so `self.refs[refi0]` could not be out of range; once B slices really parse a
  ref_idx, a mutated stream can overrun either list. The fuzz gate
  (`decoder_never_panics_on_mutated_streams`) caught this — it would otherwise
  have shipped.
- **An incomplete picture no longer vanishes.** A pending picture displaced before
  reaching `total_mb` now raises `Truncated`. Silently dropping it is exactly what
  hid this defect, and the docs had flagged that hazard earlier without acting.

### Result

Decoder conformance gate: **PASS 80 / FAIL 0** — fully green for the first time.
`--bframes 2` is byte-exact at every {1,2,3 refs} x {none, normal, strict}.

### STILL OPEN — do not read "80/0" as "all B is correct"

`--bframes 3` with `--b-pyramid normal` or `strict` still DIFFERS (no longer a
parse failure; `--bframes 3 --b-pyramid none` is exact). Slices now complete
(0 incomplete), and BOTH luma and chroma diverge on frames 1, 5 and 9 with the
rest clean — a non-propagating recon/prediction fault on specific pictures, NOT
the luma-only signature of the weighting bug. The gate does not currently cover
this combination; that is the next arm to add, and it will be red when added.

---

## 2026-07-30 (final) — two more B defects; decoder gate 120/120

### 1. Gate first (it went red, as intended)

Two arms added, both chosen to expose a KNOWN-live defect rather than to pass:

- **`maincavlc`** (`--profile main --no-cabac`). Baseline FORBIDS B-frames, so the
  `baseline` arm can never carry one and every CAVLC B stream was untested. Three
  defects have now hidden in that blind spot.
- **`pyr3`** (b-pyramid at B-depth 3). `pyr` (bframes 2) and `ipb3` (bframes 3, no
  pyramid) BOTH passed while this combination diverged — the axes had to be
  crossed, not swept independently.

Result on adding them: **PASS 92 / FAIL 28** — `maincavlc` red on all four B arms,
`pyr3` red on all three CABAC profiles. Exactly the two invisible defects.

### 2. CAVLC B: a deliberate openh264 bug-compat that had gone stale

Ablation: `--partitions p16x16` EXACT, `--partitions b8x8` DIFFERS. In x264 the
`b8x8` flag gates B_16x8/B_8x16 as well as B_8x8 (`X264_ANALYSE_BSUB16x16`), so
this implicates all three. Correlating decoded mb_type against the per-MB diff:

| mb_type | meaning | wrong |
|---|---|---|
| 1..3 | B_L0/L1/Bi_16x16 | 8-25% (collateral) |
| 4..11 | 16x8/8x16, no Bi partition | partial (collateral) |
| **12..21** | **16x8/8x16 with >=1 Bi partition** | **100%, every MB** |

`decode_b_mb` deliberately replicated an openh264 bug for a Bi 16x8/8x16 partition
(partition 0 came out List-1-only, partition 1 List-0-only). That was correct when
openh264's `h264dec` WAS the conformance oracle — but the gate is ffmpeg now, and
the CABAC path had already gone spec-correct and even documented the divergence.
The CAVLC path was simply left behind. Removed; all 136 workspace tests still pass,
so nothing was asserting the openh264 behaviour.

### 3. bframes 3 + pyramid: co-located motion needs the List-1 fallback

After (2), the SAME configuration failed under BOTH entropy coders — which
immediately reframed it as shared reconstruction, not parsing. Ablation:
`--direct temporal` and `--direct none` EXACT, `--direct spatial` differs;
`--partitions p16x16` EXACT.

Spec 8.4.1.2.1: the co-located motion is List-0's when the co-located block has a
List-0 prediction, and **List-1's otherwise** (`predFlagL0Col == 0`). `col_zero`
read List-0 unconditionally, so an L1-only co-located block looked intra
(`ref_idx == -1`) and colZeroFlag was silently suppressed. `RefFrame` did not even
store List-1 motion.

An L1-only co-located block can only exist when the co-located picture is itself a
B picture — i.e. **only under b-pyramid**. That is why it survived every
non-pyramid B stream, and why the bug needed depth 3 to show: it needs a B
reference whose own blocks are L1-only.

Fix: `RefFrame` gains `mv1`/`ref_idx1`; `col_zero` falls back to List-1.

### Result

**{CAVLC, CABAC} x {bframes 2,3,4} x {ref 1,3} x {none, normal, strict} = 36/36
byte-exact.**

Decoder conformance gate: **PASS 120 / FAIL 0** (5 profiles x 6 GOP arms x 4 QPs),
green including both newly added arms.

### The pattern across all four defects this campaign

Every one was invisible because a gate arm did not exist, and every one became
obvious within minutes once it did. In order: weighted prediction (needed a
multi-ref arm), B ref_idx (needed a B-with-multi-ref arm), CAVLC B bi-prediction
(needed a CAVLC arm that could carry B-frames at all), co-located List-1 (needed
b-pyramid CROSSED with B-depth). Sweeping axes independently found none of them;
crossing them found all four.
