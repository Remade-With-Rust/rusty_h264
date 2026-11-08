# Fifteen instruction-count veins in the encoder (2026-11-08)

The encoder now has an EXACT instrument: callgrind under WSL Ubuntu (valgrind 3.26,
same rustc 1.98 as Windows). Same binary, same input, re-run: the two totals differ
by **28 instructions in 1.36 billion** (fast) and **12 in 5.84 billion** (quality),
with byte-identical output both times. Every number below is from that instrument
on one workload, so every vein can be re-priced to the instruction after a change,
on a loaded box, in one run. Reproduce with `bench/ir_census.sh` (section 6).

Workload: `foreman_cif` frames 0-29, QP 26, CLI defaults (CABAC, refs 3, mb-tree
HalfRes, bframes auto, 8x8 transform, AQ 1.0, scenecut 40), `RUSTY_THREADS=1`.

| preset | Ir total | Ir / frame | Ir / MB | out bytes | md5 |
| --- | ---: | ---: | ---: | ---: | --- |
| fast (default) | 1,364,048,917 | 45.5 M | 115 K | 121,324 | `bd510ec4…c875a5d2` |
| quality | 5,835,377,263 | 194.5 M | 491 K | 86,505 | `393a98c6…65d3b8f0` |

Work-parity anchors for any A/B: output byte count and md5 must not move. Ir does
not see cache misses or branch mispredicts (the clock still owns those), but every
vein here is a straight-line instruction cut, which is exactly what Ir prices.

## 1. What the census says the encode is made of

Self cost, fast preset (deployment default), grouped. Percentages of the fast total.

| group | Ir | % | what it is |
| --- | ---: | ---: | --- |
| ME glue | 223 M | 16.3 | `motion_search::{closure#3}` 176 M (666,526 evals, **265 Ir/eval self**), `motion_search` self 30 M, `{closure#4}` 17 M |
| content signals | 223 M | 16.4 | `gop_bi_residual::{closure#1}` 132 M, `gmc_residual` 68 M, `mb_vars` 11 M, `median_var` 7 M, `flat_hist` 5 M |
| B-direct MC storm | 190 M | 13.9 | `b_direct` inclusive: 195,856 `mc_luma` + 391,712 `mc_chroma` calls for 6,332 `b_direct` calls |
| ME kernels | 165 M | 12.1 | satd_16x16 65 M, satd_8x16 24 M, satd_8x8 20 M, satd_px 20 M, satd_16x8 16 M, sad_16x16 16 M, satd_4x4 6 M |
| edge candidates on per-pixel MC | 98 M | 7.2 | 28,205 `mc_luma` calls from the ME cost closure at **3,490 Ir each** |
| CABAC | 113 M | 8.3 | encode_decision 45 M (948,350 bins, 47.6 Ir/bin), put_bit 21 M, cb_residual 39 M, cb_emit_inter_residual 7 M |
| mb-tree lookahead | 75 M | 5.5 | mc_satd 24 M, satd_px via it 24 M, intra_cost 11 M, mc_luma fallback 11 M, inter_cost 5 M |
| libc memcpy + memset | 54 M | 4.0 | **2,292,767 memcpy calls** at 17.7 Ir each; memset 46,160 calls |
| getenv | 21 M | 1.6 | 47,401 `std::env::var` calls, one per `motion_search` |
| plan / T-Q / recon / deblock / rdoq | ~110 M | ~8 | plan_inter_mb 34 M, rdoq 21 M, idct 12 M, plan_mb 12 M, dct 10 M, deblock ~21 M |

Quality preset: ME glue is **1,913 M = 32.8%** (closure#3 1,172 M over 9,006,080
evals = 130 Ir/eval self; motion_search self 469 M = 4,066 Ir/search; closure#4
272 M), ME kernels ≈ 2,050 M = 35%, so the glue around the SATD kernels costs about
as much as the kernels. Scalar `sad_strided` is 297 M (5.1%), the me_wide rescue grid
240 M (4.1%), scalar `satd_4x4` 150 M (2.6%), `save_mb_into` 53 M (0.9%).

Two laws confirmed again: a mature vectorised stage is mostly not its vector loop,
and the profiler's written plan (ME 81%, sub-pel 44%) was overturned by the exact
census (sub-pel is not even visible as a line; the B-direct MC storm and the signal
pre-passes, both unnamed in the speed map, are 30% of the default encode).

## 2. The veins, ranked by removable instructions on the default preset

Each vein: measured size, the mechanism (which redundancy shape), the fix, the gate,
and effort. "Exact" means byte-identical output by construction. Sizes are the
measured cost of the work; the removable fraction is my estimate and is written as
one, so run the ceiling probe (stub the work, read the delta) before building.

### V1. The B-direct per-4x4 MC storm — ~10% of the default encode, exact

`b_direct` (mb16.rs:3126) calls `b_mc_block` sixteen times per macroblock, and each
call does a 4x4 luma MC per list plus a 2x2 chroma MC per list per plane. Measured:
6,332 `b_direct` calls made 195,856 `mc_luma` (219 Ir each: the 6-tap tile path at
4x4) and 391,712 `mc_chroma` calls (231 Ir each: a zeroed 81-byte tile, 3-byte row
copies through `call memcpy`, scalar bilinear). Inclusive 190 M Ir = 13.9% of fast,
236 M = 4.0% of quality.

The decoder already fixed this shape (H-31, commit a54195a: "the CABAC residue was an
MC call storm", MC entries 2.41 M → 263 K, 1.86x). With `direct_8x8_inference_flag`
every 4x4 in an 8x8 quadrant shares one motion (the corner co-located block), so the
quadrant is ONE 8x8 luma MC and ONE 4x4 chroma MC per list, and when colZero does
not fire in any quadrant the whole macroblock is one 16x16 MC per list. The MC
filters are per-output-pixel, so the coalesced prediction is bit-identical (the
decoder's YUV cmp proved it). 8x8 and 16x16 widths also reach the `hpel_block`
plane cache and the `mc_chroma_w4/w8` kernels that 4x4/2x2 cannot.

Gate: byte-identical bitstream on the B-frame corpus (the 12-hash battery) plus
decoder round-trip. Effort: one session. Expected: 10-12% of fast Ir.

### V2. Two global-motion probes with no early-out — ~8% of the default encode, exact

`gop_bi_residual::global_me` (lib.rs:2088) and `signals::global_mc_residual`
(signals.rs:239) each run a ±12 step-4 grid (49 SADs) plus a ±3 refine (48 SADs) of a
stride-8 subsampled frame SAD. Measured 132 M + 68 M = 200 M Ir = 14.7% of fast
(3.4% of quality); the same absolute cost on every preset because it is a per-frame
pre-pass. The B-decision probe runs on SOURCE frames and the lme/me_wide probe on the
RECON reference, so they are twins, not duplicates.

Both consumers want only the argmin and its minimum. SAD is a sum of non-negative
terms, so a shift whose partial sum reaches the incumbent `bc` is already proven not
to win the strict `<`: abandoning it there changes neither `best` nor `bc` (the
rusty_dds "monotone accumulator early-out" law). Add the comparison per sampled row
inside the `sad` closure and pass `bc` in. Expected to remove half or more of the
SAD work on content with small global motion. Second brick, also exact: memoize
`global_me(cur, past)` per (frame index, gap) — `gop_bi_residual` is called from
seven sites with overlapping frame windows and recomputes the same pair searches.

Gate: `bframes_gate_signals`, the signal golden test (`signal_probes_golden`), and
the hash battery (the signals route defaults, so a changed value flips bytes).
Effort: half a day. Also fold in: `mb_vars` (signals.rs:466) is a `FlatMap`
iterator chain costing 11 M Ir; a plain row loop over 16-byte rows auto-vectorises.

### V3. Collapse the ME glue chain — 16% fast / 33% quality is glue, exact bricks

The cost closure's SELF cost is 265 Ir per evaluation on fast and 130 on quality,
plus 4,066 Ir of per-search setup in `motion_search` itself; the kernels it calls are
100-300 Ir. In H-15 the MeCtx brick collected ~23 ns of dispatch; the census says the
remaining chain is still as large as the kernel. Bricks, exact by construction:

1. **λ·rate as a per-search table.** `(lam_r * rate as f64) as i64` runs on every
   evaluation in `cost`, `cost_fp` and all four fixed-centre arms: two int↔f64
   conversions and a multiply, on the dependency chain of the compare. Build
   `ratecost[0..128]` once per search from the identical expression and index it;
   `rate` is bounded by two `mvbits` (≤ 2·25). Same f64 expression, same operands,
   same bits: exact.
2. **The sub-pel memo init is dead on fast and half-dead everywhere.** `memo_mv`
   (64 tags) and `memo_c` (64 values, 1 KiB together) are initialised per search;
   `memo_c` is only ever read after a tag hit so its zeroing is dead, and on the fast
   preset `subpel` is empty so both are. Initialise lazily behind `!subpel.is_empty()`.
3. **One `MePlan` struct per frame.** The knob census counts ~24 OnceLock/atomic
   reads at the top of every `motion_search` (`me_oracle_on`, `sp_dispatch_cfg`,
   `sp_maxit`, `sp_fc_enabled`, `me_fc_enabled`, `dia_mask`, `hpel_ref_enabled`,
   `satd_avg_enabled`, `mectx_enabled`, `subpel_harvest::enabled`, …), each a load,
   test and branch inside a 4 K-Ir function. Resolve them once per frame into a
   plain struct on `FrameEncoder` and read fields (the rusty_h265 "plan struct"
   law). Same for the `is_x86_feature_detected!` inside `satd_fn!`
   (satd_sad.rs:316), paid on every `satd_16x16`/`16x8`/`8x16`/`8x8` call.
4. **Harvest bookkeeping compiled out.** `hv_evals`, `hv_to_best`, `hv_ring1` are
   maintained per evaluation for a harvest that is off unless an env var names a
   file; gate them on a `const` feature.

Instrument: the emitted-asm instruction count of `motion_search::{closure#3}` and
the Ir/eval column from `bench/ir_census.py`. Expect 20-40 Ir per evaluation off a
130-265 Ir glue budget: 3-6% fast, 6-10% quality. Effort: two sessions.

### V4. Fast-preset edge candidates fall to the per-pixel clamped MC — 7.2% fast, exact

The fast preset never builds half-pel planes, so `mc_satd_hp`/`mc_sad` have no
`f` plane and every full-pel candidate that overhangs the frame edge falls to
`mc_luma`, whose edge arm (inter.rs:1483) does `at()` with two clamps per pixel:
measured 28,205 calls at 3,490 Ir each = 98.5 M Ir, 7.2% of the default encode.
The E-3 fix ("serve edge full-pel from the padded `f` plane") only reached presets
that build the planes.

Two exact fixes, either suffices: (a) make the edge full-pel arm row-wise — clamp the
row index once, then each row is left-fill + interior `copy_from_slice` + right-fill,
the `pad_plane_into` shape (13.6 Ir/pixel → ~1); (b) keep one padded full-pel plane
per reference for the fast preset too (`pad_plane_into`, ~0.2 M Ir per CIF frame)
and read candidates in place. (a) also helps every other edge caller (lookahead,
B-direct at the frame border). Gate: `mc_luma_clamps_at_edges` + hash battery.
Effort: hours.

### V5. The byte-wise CABAC engine core — 4.9% fast, exact

`encode_decision` 47.6 Ir per bin plus `put_bit` 44.8 Ir per output bit, 66 M Ir on
fast. H-16 sized this and re-scoped it (H-18): a 32-bit `low` with headroom, renorm
as ONE shift by `lzcnt(range)`, carry deferred through a byte buffer (the 0xFF chain),
bypass bins batchable because bypass leaves `range` constant. x264's engine codes
the same spec arithmetic at roughly a third of the instructions. Spec-exact output,
so the gate is the existing `engine_roundtrip_many` sweep plus the hash battery.
Effort: one focused session, ~200 lines; the delicate parts are first-byte
suppression, the carry chain and flush alignment.

### V6. An 8x8 SAD kernel arm — 5.1% of the quality encode, exact, one line

`sad_strided` (mb16.rs:7486) routes only 16x16, 16x8 and 8x16 to the SSE2 kernel;
8x8 and the sub-8x8 shapes take the scalar `abs_diff` rows. The B2 SAD full-pel
phase evaluates 8x8 partitions through it: 1,012,089 calls at 293 Ir = 297 M Ir.
The accel kernel `x86::sad(w, h)` already handles width 8 through `_mm_loadl_epi64`
(satd_sad.rs:167); adding `(8, 8) => rusty_h264_accel::sad_8x8` (and the 8x4/4x8
pairs by composition, as `satd_px` does) is a wrapper line. Expect 293 → ~60 Ir per
call, about 4% of quality. SAD is exact. Effort: 30 minutes.

### V7. The me_wide rescue grid through the x4 kernels — 4.1% quality → ~2%, exact

`motion_search` calls `satd_16x16` 790,000 times from the batched rescue grid
(25x25 points per firing, 240 M Ir, 4.1% of quality on foreman). Four horizontally
adjacent grid points share the source band; `satd_x4` already exists for the
fixed-centre diamond and costs 661 Ir for four (165 per point against 304). Walk the
grid in groups of four, same values, same `<` comparisons in the same order. Also
removes four `is_x86_feature_detected!` + two asserts per point. Effort: hours.

### V8. Runtime-length row copies that are `call memcpy` — 3.0% fast / 0.9% quality, exact

2,292,767 libc memcpy calls on fast (54 M Ir including memset); 1.08 M from
`mc_chroma` (3- and 5-byte tile rows for 2x2/4x4 chroma), 1.09 M from `mc_luma`
(4- and 8-byte rows for sub-16 full-pel and the 9/13-byte tile rows), 177 K from
`hpel_block`, 512 K from `luma_tile_into` on quality. H-17 found four of these; the
census finds the remaining populations. Fix: const-width arms (`match bw { 4 => u32
load/store, 8 => u64, 16 => two u64 }`) at each site; a runtime-length copy of 2-16
bytes must never leave the function. Price it AFTER V1, which removes most of the
2x2/4x4 population. Effort: half a day.

### V9. CABAC residual emitter: one pass, not four — 2.9% fast, exact

`cb_residual` (mb16.rs:9003) costs 385 Ir per coded block (102,161 blocks): a count
pass, a reverse find for `lastnz`, the significance walk, the level walk, after a
by-value `scan_4x4_dcac`. The decoder's twin was rewritten around a significance
bitmask (commits e105409, a922f60: `tzcnt`/`blsr` level walk). Port the shape:
build `nz_mask` in the scan, `coeff_num = popcnt`, `lastnz = 15 - lzcnt`, walk set
bits. Same bins in the same order, exact. Expect ~385 → ~250 Ir. Effort: hours.

### V10. Scalar 4x4 SATD — 2.6% quality, exact

`satd_4x4` is the scalar `satd4x4_scalar`: 630,901 calls at 237 Ir = 150 M Ir;
462,907 from `plan_mb` (the Intra_4x4 search scores all nine modes for all sixteen
blocks, no early exit) and 167,994 from the 8x4/4x8 composition in `satd_px`. A
single-block SSE2 SATD (four rows in one xmm, the x264 `pixel_satd_4x4` shape) is
~50 instructions. This is NOT the refuted brick: that one gathered nine modes into
lanes; this is one block in one register. Exact integer. Effort: hours.

### V11. B-direct predicted twice per direct macroblock — 1.7% fast, exact, trivial

The B driver computes `(dp, dc, dmotion) = fe.b_direct(..)` (mb16.rs:11911) for
the skip test, and when direct wins, `plan_inter_mb` recomputes `self.b_direct(..)`
(mb16.rs:4787): 788 calls at 29.9 K Ir = 23.5 M on fast. "Compute to decide, then
recompute to use." Carry the triple in `BInter`. Independent of V1 and worth doing
first because it also halves V1's remaining population.

### V12. An environment variable read on every motion search — 1.6% fast / 0.9% quality, trivial

`subpel_pattern_override` (mb16.rs:1252) caches `RFF_SUBPEL_PAT` only when it is
SET; unset (the default) it calls `std::env::var` on every `motion_search`:
47,401 getenv calls, 21 M Ir inclusive on fast, 115,643 and 52 M on quality. Cache
the `None` too (a second sentinel, or `OnceLock<Option<u32>>`). Same bug class per
frame: `RFF_ME_COH_DBG` (mb16.rs:10445), the four `RFF_BSKIP_*` reads per B slice
(mb16.rs:11871-11888), `polytier_on` per frame, `RFF_MBTREE_LA/DBG` per GOP.
Fifteen minutes.

### V13. Per-frame allocation, zeroing and cloning — ~2-3% fast, exact

`zalloc_generic` memset 9.6 M Ir (353 allocations per 30 frames), `FrameEncoder::new`
allocates and zeroes rec planes and nine grids per slice (`mv1_y`/`ref_idx1_y` filled
for P and I slices that never read them); evicted `RefFrame` planes and
`Arc<HpelPlanes>` are freed, not recycled; `try_encode_planes` makes TWO full YUV
clones per frame (`to_frame` = black-fill + copy) for scenecut and the lookahead
queue; `coded_luma` + `downsample2x` run twice per frame (`pair_prep`, then
`gop_qp_offsets`). The decoder's GridPool was never ported to the encoder. Fix:
pool grids and planes on the `Encoder`, `copy_into` instead of `to_frame`, share
`pair_prep`'s planes with mb-tree. Count allocations per frame as the gate (the
decoder campaign's counting-allocator instrument). Effort: one session.

### V14. The RDO snapshot as fixed arrays — 0.9% quality, exact

`save_mb_into` costs 9,196 Ir per snapshot (5,773 on quality): 536 bytes of payload
pushed one element at a time into eleven `Vec`s, each push a capacity check. The
recorded refutation was `extend_from_slice` into a GROWABLE Vec; the shape that works
(`load_mb`, "write into a pre-sized destination") wants `MbState` to hold `[u8; 256]`,
`[u8; 64]`, … so both directions are const-length `copy_from_slice`. ~9 K → ~0.6 K
Ir per snapshot. Effort: an hour.

### V15. Hand-rolled twins of kernels that already exist — ~1% quality / 0.5% fast, exact

`me_wide_headroom`'s `sad16` closure (70,616 calls at 364 Ir) is `accel::sad_16x16`
at 92; mb-tree's `intra_cost` runs the scalar `hadamard_4x4` sixteen times per MB
(11 M Ir); the lookahead `mc_satd` fallback (8,580 calls at 1,258 Ir) materialises
through `mc_luma` then runs scalar `satd4` per 4x4 where `satd_px` on a padded plane
would do; `pred_ssd`/`mb_ssd` are scalar SSD loops (15 M Ir on quality) with no
kernel. Move #6 of codec-eliminate-redundancy: grep for the kernel before the loop.

## 3. Where the freed instructions buy quality

- Every vein above is byte-identical, so BD-rate cannot move. The quality effect is
  indirect and large: `Balanced` (sub-pel) is −42…−50% BD-rate for 2.3-3.1x the time
  of `Fast`. V1 + V2 + V4 + V5 + V12 together are ~30% of the fast encode's
  instructions and none of them is search; removing them moves the whole speed
  ladder, and the quality preset's 33% glue (V3) is the price of every exact-RD trial.
- **RD currency defect, surfaced by the census (quality-gated, not exact).** In a
  CABAC encode the quality preset prices inter shapes with the `plan_rate_bits` model
  but prices the intra candidate with REAL CAVLC bits: `encode_residual_block` ran
  63,672 times from `encode_mb` inside `trial_intra`. One J comparison, two rate
  currencies, neither of them the CABAC the stream is written in. x264 prices with
  CABAC bit-cost tables derived from the live context state. Fixing this is a
  `codec-tune-quality` brick (4-QP per-clip BD gate), and it is also cheaper than
  running the CAVLC writer.
- V7 makes `me_wide` cheaper, which lets its head-room threshold (16, calibrated on
  a narrow boundary pair) come down on the clips where it earns +0.6…+4.7%.

## 4. What the census refutes or demotes

- Sub-pel refinement is not a line in either profile: its cost is the glue (V3),
  not the ring. The "U1 — 44% of encode" speed map was a profiler-ON number with
  per-eval scope tax; the exact instrument puts the kernel+glue of ALL evaluations
  at ~50% of quality, with glue the larger half.
- Transform/quant/recon: idct 12 M + dct 10 M + quant 4 M + dequant 4 M + rdoq 21 M
  on fast = 3.7%. Still not the lever (fourth confirmation).
- CAVLC is 0 on the default path (CABAC) and 0.45% on quality (only the trial writer).
- Deblock 1.5%, mb-tree 5.5% (HalfRes default is doing its job).

## 5. Order of work

1. V12, V11, V6 (an afternoon together; ~3% fast, ~5% quality, three trivial exact bricks).
2. V1 then V8 (the B-direct storm, then the memcpy population it leaves behind).
3. V2 (signals early-out), V4 (edge candidates): the two biggest remaining fast levers.
4. V3 (glue chain), bricked one at a time against the closure's instruction count.
5. V5, V9 (CABAC engine and residual emitter), V7, V10, V14, V13, V15.

Re-run the census after every brick: the ranking moved every time in every previous
campaign and the instrument costs one run.

## 6. The instrument

```sh
# in WSL Ubuntu (valgrind + the same rustc as Windows); first build:
cd /mnt/e/coding/rusty_h264 && CARGO_TARGET_DIR=$HOME/h264-target cargo build --release -p rusty_h264-cli
# then, for each preset (runs twice, prints the ladder, the anchors and the self-cost census):
bench/ir_census.sh fast
bench/ir_census.sh quality
# caller/callee edges + call counts for named functions:
python3 -I bench/ir_census.py $HOME/h264-ir/cg.fast.1.out mc_luma mc_chroma closure#3 getenv memcpy
```

Exactness ladder measured this session: same binary re-run 28 and 8 Ir (fast), 12 Ir
(quality); the same binary from a second shell invocation sat 43 Ir higher, with every
function's self count identical to the instruction. A rebuild from identical source has
not been measured yet; expect the 6-10 of the instruction-counting skill, so compare
only same-session pairs and treat anything under ~50 Ir as pedestal. The decoder has
its own harness from the same day (`bench/callgrind_ir.sh`, `bench/callgrind_census.py`,
`bench/make_ir_corpus.sh`); this one exists because the encoder takes raw YUV and a
preset, not a bitstream. Ir prices every
instruction at one: it over-prices `rep movsb` memsets and cannot see the branch
mispredicts in `b_direct`'s per-4x4 blend or the cache behaviour of the global-ME
probes, so the clock (bench/campaign_speed.ps1, pinned, ABBA) still owns the final
word on any brick above ~5%.
