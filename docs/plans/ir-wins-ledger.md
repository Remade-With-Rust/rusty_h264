# Decoder instruction-count wins: the running ledger (2026-11-08)

Three byte-identical wins per vein of `docs/plans/ir-veins-2026-11-08.md`, measured with
callgrind (exact executed instructions) on the seven corpus streams, each run gated on
26 streams decoding byte-identical to ffmpeg 8.1.2. Every delta is against the run
immediately before it, so the column sums telescope to final minus baseline.

Instrument: `bash bench/ir_step.sh <tag> <prev>` in WSL (build, 26-stream SHA gate,
parallel callgrind, delta table). Same binary + input = same count to the instruction;
rebuild spread 6-10 Ir.

Baseline **B0** (shipping build plus the `finish_pending` correctness fix):

| fm-cavlc | fm-main | FP-main | FP-high | sh-cavlc | sh-main | sh-high | total |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 507,675,537 | 615,171,964 | 1,160,754,292 | 1,681,082,533 | 2,318,037,093 | 2,632,096,219 | 2,969,606,617 | 11,884,424,255 |

Streams: fm = foreman CIF 120 frames, FP = FourPeople 720p 60 frames (static), sh =
shields 720p 60 frames (detail); cavlc = Baseline veryfast, main = Main medium, high =
High slower (x264 recipes of `bench/decode_x264_speedtest.sh`, qp 26).

## Result

45 wins in the Wins table, three per vein (vein V14's third is row 45, the PGO build), every one byte-identical
on the 26-stream ffmpeg gate. The scalar (no-`asm`) build is also 26/26, and the accel, common,
decoder and encoder test suites pass in release.

| build | fm-cavlc | fm-main | FP-main | FP-high | sh-cavlc | sh-main | sh-high | total | vs B0 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| B0 | 507,675,537 | 615,171,964 | 1,160,754,292 | 1,681,082,533 | 2,318,037,093 | 2,632,096,219 | 2,969,606,617 | 11,884,424,255 | |
| W59 (`cargo build`) | 392,754,244 | 483,454,526 | 797,670,082 | 1,240,292,195 | 1,805,352,599 | 2,058,427,338 | 2,332,722,568 | 9,110,673,552 | -23.34% |
| P2 (PGO, W59 tree) | 373,981,167 | 452,488,765 | 756,571,622 | 1,221,190,964 | 1,742,876,484 | 1,919,956,124 | 2,175,977,372 | 8,643,042,498 | -27.27% |

The chain's last plain-`cargo build` state is row 44 (W59); row 45 is the same tree built with PGO,
so the cumulative ends at the PGO total. Per vein: V1 -216.7M, V2 -49.1M, V3 -373.2M, V4 -500.7M,
V5 -198.4M, V6 -129.4M, V7 -268.5M, V8 -46.1M, V9 -48.9M, V10 -130.6M, V11 -91.4M, V12 -37.9M,
V13 -203.8M, V14 -772.9M (fat LTO + pins -297.8M, tail-dup -7.5M, PGO -467.6M), V15 -173.7M.

## Wins

Deltas in Ir (negative = fewer instructions).

| # | tag | vein | change | fm-cavlc | fm-main | FP-main | FP-high | sh-cavlc | sh-main | sh-high | total | cumulative |
| --: | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | W01 | V3 | `b_set_motion`: dispatch on block width, constant-length row fills (sh-main memset calls 3.92M to 0.99M) | -2,431 | -26,396,608 | -29,938,413 | -51,882,436 | -2,336 | -101,020,695 | -94,936,009 | -304,178,928 | -304,178,928 |
| 2 | W02 | V3 | `commit_inter_grid`: constant-width partition rows (877k memset calls on sh-high) | -38 | -3,873,303 | -5,438,613 | -6,033,478 | +134 | -18,468,658 | -22,033,629 | -55,847,585 | -360,026,513 |
| 3 | W03 | V3 | `bz_flush_slow` / `pz_flush_slow`: spans of 1-4 MBs fill at a literal width (24 memsets per flush) | -320,544 | -340,936 | -4,986,159 | -3,260,565 | -129,986 | -2,138,899 | -2,015,439 | -13,192,528 | -373,219,041 |
| 4 | W04b | V6 | `copy_rows`: MC staging copies of narrow rects at literal widths 16/8/4/2, both recon twins (704k-873k memcpy calls) | -9,539,419 | -3,813,954 | -3,771,241 | -4,211,440 | -14,441,717 | -14,614,672 | -17,607,844 | -68,000,287 | -441,219,328 |
| 5 | W05 | V6 | `mc_chroma_padded_pair` full-pel rows at literal widths 8/4/2 (475k-502k memcpy calls) | +1,755 | -444,196 | -5,494,522 | -33,825,588 | -3,334,575 | -2,875,796 | -3,455,045 | -49,427,967 | -490,647,295 |
| 6 | W06c | V6 | `coalesce_p_inter_mc` (P no-residual + worker path) narrow-rect staging copies via `copy_rows` | -2,093,574 | -777,399 | -1,299,740 | -1,420,796 | -2,516,984 | -1,828,329 | -2,011,249 | -11,948,071 | -502,595,366 |
| 7 | W07 | V5 | `avg_row_into`: one auto-vectorised `pavgb` loop per row instead of a kernel call per 16 bytes | +4,285 | -798,059 | -63,817,542 | -39,605,485 | +4,449 | -9,553,852 | -11,290,844 | -125,057,048 | -627,652,414 |
| 8 | W08d | V5 | implicit-weighted bi blend in i16 lanes (`wavg`, exact: weights in -64..=128 summing to 64), `b_mc` luma only | -4,456 | -1,830,202 | -1,743,287 | -18,361,578 | -4,541 | -9,841,868 | -13,023,264 | -44,809,196 | -672,461,610 |
| 9 | W10 | V4 | `Decoder::recycle_frame`: a returned frame's planes are reused by the next picture without the zero fill (needs the caller to recycle; `decode_bench` does, and its identity path recycles every picture) | -22,428,939 | -22,398,759 | -89,009,945 | -88,747,456 | -89,010,813 | -89,010,214 | -89,009,411 | -489,615,537 | -1,162,077,147 |
| 10 | W11 | V4 | `bs_frame` and the three deblock backup rows reused without the per-picture fill (every entry written before read) | -71,418 | -71,812 | -197,709 | -197,907 | -197,410 | -197,693 | -197,993 | -1,131,942 | -1,163,209,089 |
| 11 | W12 | V4 | `inter_finish`: scratch boxes returned on the deferred arm too (each deferred cbp==0 CAVLC MB allocated + zeroed three boxes) | -3,676,345 | +67 | +8 | -86 | -6,324,086 | -43 | +43 | -10,000,442 | -1,173,209,531 |
| 12 | W13 | V2 | per-slice implicit bi-weight table (`iw_tab`, built in `set_b_context`) instead of the derivation per bi region (CAVLC +: `decode_slice_cavlc_inner` register re-roll, no call-count change) | +472,793 | -1,447,086 | -1,512,493 | -11,901,554 | +2,036,319 | -6,298,792 | -8,600,298 | -27,251,111 | -1,200,460,642 |
| 13 | W14 | V2 | `b_mc` narrow bi regions: blend the contiguous staging in one vectorised loop, then place rows (was a scalar 8/4-sample loop per row) | -75 | -113,772 | -78,249 | -8,766,393 | +8 | -110,018 | -2,551,787 | -11,620,286 | -1,212,080,928 |
| 14 | W15 | V2 | narrow MC staging buffers sized to their arm (luma 128 B, chroma 32 B; zero-init paid per call) | -875,146 | -523,188 | -468,212 | -2,670,123 | -1,247,029 | -1,731,465 | -2,754,129 | -10,269,292 | -1,222,350,220 |
| 15 | W16 | V12 | `sat_mvd` inlined (call frame + 64-byte by-value argument for ~15 instructions, 209k calls on sh-high) | -2,203 | -598,019 | -776,022 | -783,453 | -2,144 | -1,346,193 | -1,860,392 | -5,368,426 | -1,227,718,646 |
| 16 | W17 | V12 | `predict_partition_mv` `#[inline]` across the crate boundary (literal mode/part at every call site fold the dispatch) | -1,420,635 | -1,234,256 | -1,828,311 | -1,713,543 | -7,127,469 | -4,342,906 | -4,716,575 | -22,383,695 | -1,250,102,341 |
| 17 | W18 | V12 | `col_zero` split body from symbol: frozen-colocated fast path `#[inline]`, live/fallback arms out of line | +104 | -548,862 | -318,255 | -171,083 | +18 | -4,997,791 | -4,086,566 | -10,122,435 | -1,260,224,776 |
| 18 | W19 | V9 | `read_ue` split: 24-bit-window fast path `#[inline]` across the crate boundary, long codewords cold (1.07M out-of-line calls on sh-cavlc); `read_se` inline | -2,571,830 | -24,500 | -11,977 | -13,810 | -10,473,405 | -12,043 | -13,961 | -13,121,526 | -1,273,346,302 |
| 19 | W21 | V9 | CAVLC P partition commit through `commit_inter_grid` (constant-width row fills) instead of four fallible lookups per 4x4 cell | -5,066,700 | +81,716 | +74,881 | +84,949 | -27,765,182 | +243,301 | +266,655 | -32,080,380 | -1,305,426,682 |
| 20 | W22 | V9 | `inter_finish`: luma nnz neighbour cache loaded only when a luma block is coded (`cbp_luma != 0`) | -844,980 | -4,504 | -4,461 | -4,461 | -2,846,371 | -4,461 | -4,547 | -3,713,785 | -1,309,140,467 |
| 21 | W23 | V7 | `add_inter_residual` quadrant-first: counts as 2-cell rows, an all-zero 8x8 quadrant copied as 8 rows of 8 bytes and skipped by the block loop | -8,693,592 | -5,216,183 | -5,884,855 | -2,654,211 | -44,948,298 | -28,297,699 | -16,330,940 | -112,025,778 | -1,421,166,245 |
| 22 | W24b | V7 | `add_inter_residual` (both twins): `cbp_chroma == 0` copies each chroma plane as 8 rows of 8 bytes, skipping the 8-block ladder and its dequant setup | -11,790,892 | -5,747,827 | -5,939,600 | -6,314,432 | -60,366,280 | -23,959,662 | -28,766,039 | -142,884,732 | -1,564,050,977 |
| 23 | W26 | V7 | luma residual block loop nested by quadrant: one `q_zero` test per quadrant, `luma8` test hoisted out of the loop | -944,348 | -542,894 | -551,333 | -364,099 | -5,669,729 | -3,146,916 | -2,359,392 | -13,578,711 | -1,577,629,688 |
| 24 | W27 | V10 | `emulation_unprevent`: zero-free 8-byte words skipped (scan) and appended whole (copy) when no zero run is pending | -4,609,346 | -3,353,420 | -3,748,297 | -3,823,578 | -26,622,871 | -14,936,268 | -17,508,259 | -74,602,039 | -1,652,231,727 |
| 25 | W28 | V10 | `split_annex_b` on `find_start_code` (zero-free 8-byte words cannot hold the start of a start code) | -1,742,638 | -1,263,269 | -1,412,887 | -1,437,221 | -10,129,563 | -5,644,991 | -6,620,253 | -28,250,822 | -1,680,482,549 |
| 26 | W29 | V10 | `split_access_units` on `find_start_code` (the AU split before `decode()`) | -1,713,277 | -1,243,456 | -1,388,892 | -1,412,536 | -9,925,083 | -5,530,918 | -6,485,790 | -27,699,952 | -1,708,182,501 |
| 27 | W30 | V11 | `pk_differs_two_list`: identical motion in both lists answers `false` before the slot arrays are built (exact: every arm of the set rule agrees) | -8 | -965,706 | -23,635,359 | -22,362,445 | -35 | -9,224,904 | -7,901,042 | -64,089,499 | -1,772,272,000 |
| 28 | W31 | V11 | the identical-motion early-out hoisted into the inlined `pk_differs` prefix (two-list pairs only), so ~3/4 of two-list pairs skip the outlined call | -167,676 | +379,549 | -3,085,542 | -2,793,670 | -286,661 | -235,724 | +42,863 | -6,146,861 | -1,778,418,861 |
| 29 | W32 | V11 | `pk_differs_two_list` slow path reads the used slots straight off the record (no zeroed slot arrays, no gathering closure) | +0 | -1,971,268 | -3,276,662 | -3,377,154 | +129 | -6,103,197 | -6,478,596 | -21,206,748 | -1,799,625,609 |
| 30 | W33 | V1 | per-edge `tc` lanes from static per-indexA rows (`TC_LUMA_LANES` / `TC_CHROMA_LANES`, index `bS & 7`) instead of two range tests and a 3-entry index per lane | -8,423,987 | -8,498,184 | -14,569,603 | -13,960,915 | -45,330,498 | -36,049,306 | -39,816,561 | -166,649,054 | -1,966,274,663 |
| 31 | W35 | V1 | precomputed `vnz`/`hnz` edge masks removed: each edge group tests the u32 of strengths it loads (no 8-compare pack and stack store per macroblock) | -2,308,785 | -2,158,916 | -4,175,044 | -3,872,742 | -12,032,819 | -9,370,048 | -9,381,813 | -43,300,167 | -2,009,574,830 |
| 32 | W36 | V1 | internal-edge thresholds (luma and chroma) derived eagerly once per macroblock: the lazy `Option` memo was already hoisted by LLVM, so each internal edge paid its test, reloads and jump for nothing | -488,167 | -296,717 | -240,596 | -250,572 | -2,732,402 | -1,286,199 | -1,452,873 | -6,747,526 | -2,016,322,356 |
| 33 | W37 | V15 | `luma_h_regs` (vertical-edge luma kernels): rows loaded off one half-base pointer at constant stride multiples instead of `(h*8+i)*stride` (a lea + imul per row load) | -2,277,375 | -1,744,375 | -2,571,308 | -2,162,725 | -12,719,736 | -7,903,391 | -7,224,342 | -36,603,252 | -2,052,925,608 |
| 34 | W40 | V15 | luma `tc` lanes for both halves from one 4-byte load (`tc_lanes4_pair`: sign-extend + three unpacks) instead of two byte loads, two broadcasts and a blend per half | -1,768,239 | -1,365,047 | -2,036,684 | -1,708,579 | -9,979,076 | -6,236,960 | -5,695,153 | -28,789,738 | -2,081,715,346 |
| 35 | W41 | V15 | `luma_lt4_v` on AVX2 (the x86-64-v3 baseline): the 16 columns in one 16-lane pass (one load, one filter core, one pack + 16-byte store per row) instead of two 8-column SSE halves | -6,601,355 | -5,125,693 | -7,651,587 | -6,405,839 | -37,478,928 | -23,533,501 | -21,469,740 | -108,266,643 | -2,189,981,989 |
| 36 | W42 | V13 | High-profile 8x8 residual through a fused AVX2 IDCT + add (`idct8x8_add`, in-register 8x8 transposes): replaces scalar `inverse_core_8x8`, the i32 prediction gather, the add-and-clip block and 8 row copies (all three decoder sites) | -388,996 | -246,087 | -261,552 | -5,605,128 | -1,787,451 | -1,062,491 | -24,575,225 | -33,926,930 | -2,223,908,919 |
| 37 | W44 | V13 | explicit weighted prediction coalesced: one `weight_partition` per macroblock (or per uniform 8x8 quadrant) instead of sixteen 4x4 calls with 4-sample rows (`weight_mb_by_ref`, all four call sites) | -81,947 | -8,084,717 | -12,956,365 | -14,771,517 | -343,222 | -47,033,900 | -55,343,989 | -138,615,657 | -2,362,524,576 |
| 38 | W45b | V13 | 8x8 levels stored at their raster position by every producer (CABAC 8x8 category, both CAVLC interleaves via an out-of-line helper), retiring `un_scan_8x8` (zeroed array + 64 scattered moves per coded 8x8 block). Inline-helper first form cost CAVLC +2.6M (`inter_finish` +17.5 Ir/call); out of line it is +5 | +100,068 | -57,623 | -36,799 | -6,090,780 | +428,227 | -166,244 | -25,449,939 | -31,273,090 | -2,393,797,666 |
| 39 | W46 | V5 | zero-bi B_Skip band averaged as ONE strided block per plane (`avg_block_into` over the new AVX2 `avg_2d`: one assert per plane, pointer steps per row) instead of one row call per row (~40 Ir of slice glue each) | -6,751 | -354,878 | -18,123,538 | -10,122,436 | -6,767 | -16,322 | +108,650 | -28,522,042 | -2,422,319,708 |
| 40 | W48 | V14 | `lto = "fat"` with the eight deblock kernel entry points pinned `#[inline(never)]` (fat LTO alone inlined them into `filter_frame_rows_pre`: +341M there vs -268M of kernels on sh-cavlc, net only -19.4M with CAVLC +0.74%) | -13,421,386 | -14,218,179 | -33,237,279 | -51,061,784 | -64,473,807 | -61,283,132 | -60,088,053 | -297,783,620 | -2,720,103,328 |
| 41 | W56 | V14 | `-C llvm-args=-tail-dup-size=4` (LLVM tail-duplication budget, default 2) in the x86-64 rustflags: small blocks copied into predecessors, fewer taken jumps (sweep: 4 = -7.5M all streams; 6 = -4.7M; 10 = -3.8M with CAVLC up) | -225,359 | -404,171 | -618,563 | -848,511 | -1,297,850 | -1,909,813 | -2,200,118 | -7,504,385 | -2,727,607,713 |
| 42 | W57 | V8 | CABAC slice loop, coded-B cbp==0 inline plane copy: strides and plane slices bound to locals and the row offset stepped (a store through a plane may alias `self.cw` for LLVM, so 32 rows each reloaded the field, the plane pointer and length and re-multiplied the offset) | +3 | -2,713,098 | -4,593,356 | -4,005,674 | -144 | -6,884,877 | -6,903,091 | -25,100,237 | -2,752,707,950 |
| 43 | W58 | V8 | CABAC B macroblock: the L0/L1 neighbour caches filled only for the lists the mb_type can read (B_Direct_16x16 none, single-list 16x16/16x8/8x16 one, B_8x8 both); the skipped fills were written and never read | +86 | -1,367,604 | -2,468,118 | -1,897,504 | 0 | -5,701,245 | -5,082,027 | -16,516,412 | -2,769,224,362 |
| 44 | W59 | V8 | CAVLC P inter arms (16x16/16x8/8x16 and P_8x8): the deferred arm returns before building `pred_y`/`c_pred` and passes shared static zero planes (`inter_finish` never reads them when it defers); was a 384-byte zero-fill per P macroblock | -927,530 | -43 | -129 | 0 | -3,598,510 | -86 | -43 | -4,526,341 | -2,773,750,703 |
| 45 | P2 | V14 | PGO build of the W59 tree (`bench/ir_step_pgo.sh`: the config's rustflags restated, trained on the 19 feature streams and never on the seven counted ones, so the three shields streams are unseen content). Realised only by a profile-guided release build, not by plain `cargo build` | -18,773,077 | -30,965,761 | -41,098,460 | -19,101,231 | -62,476,115 | -138,471,214 | -156,745,196 | -467,631,054 | -3,241,381,757 |

## Build pipeline (PGO)

Measured on the same instrument; realised only by a profile-guided build. P2, the pricing on the
final tree, is folded into the Wins chain as row 45 (vein V14's third win); P1 was the first pricing,
on the W48 tree, and is NOT in the cumulative. Both trained on the 19 feature streams (`_ir/feat/`),
never on the seven counted streams; the feature corpus has no shields content, so three of the seven
are unseen content. Script: `bash bench/ir_step_pgo.sh <tag> <prev>`.

| tag | vein | change (vs) | fm-cavlc | fm-main | FP-main | FP-high | sh-cavlc | sh-main | sh-high | total |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| P1 | V14 | PGO build of the W48 tree (vs W48), config rustflags restated | -24,155,220 | -32,821,533 | -43,903,944 | -40,019,028 | -73,507,607 | -140,355,301 | -166,077,675 | -520,840,308 |
| P2 | V14 | PGO build of the FINAL tree (vs W59), same recipe; = Wins row 45 | -18,773,077 | -30,965,761 | -41,098,460 | -19,101,231 | -62,476,115 | -138,471,214 | -156,745,196 | -467,631,054 |

Where it comes from: hot call sites inlined by profile (MC kernels into `mc_luma_padded_pre`, chroma MC,
`weight_block`, `idct8x8_add`, `parse_cbp_cabac`, the mv-neighbour helpers): no single annotation
reproduces it. `bench/pgo.sh` (the August 3-5% clock result) set RUSTFLAGS to the profile flag alone,
which REPLACES the config's target rustflags: its PGO binaries lacked `target-cpu=x86-64-v3`, frame
pointers and the hardening flags. Fixed to restate them.

## Refuted (reverted, with the number)

| tag | vein | change | total delta | why |
| --- | --- | --- | ---: | --- |
| W04 | - | (accidental null arm: an unused helper function compiled in, nothing else changed) | +242 | the rebuild/layout noise floor of this campaign: about 250 Ir on 11.5 G |
| W06 | V6 | `b_mc_chroma` narrow arm: uni-pred copies hoisted out of the per-row loop into `copy_rows` | +42,836,683 (with b_mc) | removed 140k memcpy calls on FP-high but `b_mc_chroma` self rose +41.4M (+206 Ir on every call): whole-function codegen moved |
| W06b | V6 | `b_mc` uni-pred narrow copies via `copy_rows` | +4,639,890 on FP-high | `b_mc` +32 Ir on every one of 201k calls for a 1.4M memcpy saving; same mechanism |
| W08 | V5 | `avg_row_into` literal-width arms for 16/8/32 | +63,546,660 | the arms pushed the function past the inliner: 474k per-row calls came back |
| W08c | V5 | i16 weighted blend also in `b_mc_chroma` | +63,110,177 (with luma) | `b_mc_chroma` +80M on FP-high: on 8- and 4-wide chroma rows the i16 form vectorises worse than i32 |
| W09 | V5 | zero-bi band at literal widths for 1- and 2-MB spans (`avg_band::<W>`) | +23,770,768 | four unrolled instantiations bloated `bz_flush_slow` (+820 Ir per flush) and pushed `chroma_guard` out of line |
| W20 | V9 | `nnz_cache_load` inlined, left column through one window slice | +634,182 | inlined copies cost the CAVLC callers more than the frame they removed |
| W25 | V7 | whole-MB zero-luma arm (one 16x16 copy) ahead of the quadrant path | +240,748 | the 16-byte test is paid by every MB; CAVLC +4M against High -3M |
| W09c/d | V5 | full-pel bi regions averaged straight from the two padded planes (`b_bi_fullpel`) | +32,373,827 / -274,524 | first form did not vectorise (4,251 Ir per call); fixed-array form a wash: fires on <10% of `b_mc` calls and every call pays the test |
| W34 | V1 | the four `tc` lane lookups written as an array literal instead of `bs4.map(..)` | -86 | null arm: LLVM emits the same four loads; the line census charged them to `core::array` because that is where `map` inlines them |
| W38 | V15 | 8x4 / 8x2 stores: interleave the i16 lanes, then one saturating pack | +5,635,152 | LLVM rewrites the pack as max(0) + min(255) + truncating pack in either shape; chroma compiled identically, luma +4 Ir per call |
| W39 | V15 | `_v` kernels store two rows from one `packus` (low + high half) | +1,741 | null arm: the pack is still lowered to max(0) + min(255) + a truncating pack per row |
| W43 | V13 | `weight_rows`: exact i16 path (bounded w, o, denom) ahead of the i32 loop | +319,855,124 | the second loop pushed `weight_block` out of line (914k calls, 129 Ir each) and every call it serves is a 4x4 block |
| W47 | V14 | `lto = "fat"` alone | -19,404,005 | sign flip by content (sh-cavlc +13.9M, fm-cavlc +1.7M; FP-high -23.2M): the deblock kernels inlined into `filter_frame_rows_pre` (+341M vs -268M). Superseded by W48, which pins them out of line |
| W49 | V14 | `-C llvm-args=-inline-threshold=500` (under fat LTO) | +46,681,154 | worse on every stream (FP-high +2.2%) |
| W50 | V14 | `-C llvm-args=-inline-threshold=150` (under fat LTO) | +24,311,633 | worse on every stream: both directions lose, LLVM's default is the optimum here |
| W51 | V14 | `#[inline]` on the accel chroma MC kernels (PGO inlines them) | +100,782 | null: the cost model still keeps them out of line |
| W52 | V14 | `#[inline(always)]` on the chroma MC wrappers | -4,848,416 | sign flip (FP-high +1.27M): the boundary moved to the inner `_sse2` kernels |
| W53 | V14 | `#[inline(always)]` on wrappers and inner kernels | -6,435,461 | sign flip (FP-high +3.47M): `mc_chroma_padded_pair` +22.2M against -18.8M of kernel calls; PGO's per-callsite hotness is what a fixed attribute cannot copy |
| W54 | V14 | `-tail-dup-size=6` | -4,707,109 | superseded by 4 (W56); fm-cavlc +5,340 |
| W55 | V14 | `-tail-dup-size=10` | -3,826,244 | sign flip: both CAVLC streams rise; the budget has an optimum near 4 |
