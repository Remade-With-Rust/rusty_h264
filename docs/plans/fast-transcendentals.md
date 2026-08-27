# Fast transcendentals — 20 deployment sites, ranked and priced

Applying `rusty-fast-transcendentals` to this codec. Census taken 2026-08-22 over
`crates/*/src` (shipping code) plus `examples/`+`tests/` (offline tooling).
Re-censused 2026-08-26 against the working tree (Round 1+2 edits included):
table line numbers refreshed, and the missed components are in the
**Census addendum** at the bottom — one real one (`trellis_quant`), one Part-2
correction, one missing gate spec, and three completeness holes.

## The two findings that shape everything below

**1. THE DECODER HAS ZERO TRANSCENDENTALS.** `rusty_h264-decoder/src` and
`rusty_h264-common/src` contain not one `exp`/`ln`/`log2`/`powf`/`tanh` — H.264
decode is integer arithmetic end to end. Every site in this document is
ENCODER-side. If the goal is decode speed, this skill has nothing to offer and
the entropy tree in `big-oppy-decoder.md` is the live worklist instead.

**2. THE TOP EIGHT SITES WANT A LOOKUP TABLE, NOT A POLYNOMIAL.** Every hot
`powf` here is `2f64.powf((qp as f64 - 12.0) / 3.0)` where **`qp` is a `u8`**
(`fn rdoq(coeffs: &[i32; 16], qp: u8, ...)`). An integer 0..=51 argument makes a
**52-entry `const` LUT EXACT** — bit-identical, gate-able by the existing
byte-identity harness, no accuracy sweep, no tolerance argument. That is
strictly better than the skill's polynomial route, and the skill says so itself:
*"a strength reduction that turns a no-SIMD op into a baseline-SIMD op is
finished."* A LUT is finished before it starts.

The polynomial recipe applies only where the argument is genuinely continuous —
sites 9-11 below.

## What NOT to touch

`crates/` holds **368** call sites of `sqrt` / `abs` / `min` / `max` /
`mul_add`. These are SSE2/NEON baseline, already vectorised, and rewriting them
is the documented way to waste a week (the mp3 `xrpow` hand-AVX2 that measured
0.97x). None of them appear below.

`signals.rs:431` uses `.powi(2)` — that is `x * x`, not a transcendental. No win.

## The 20 sites

Frequency at 720p (3600 MB/frame). ★★★ = do it, ★ = marginal, ✗ = do not.

| #  | site | call | frequency | fix | verdict |
| -- | ---- | ---- | --------- | --- | ------- |
|  1 | `mb16.rs:6687` `rdoq` | `powf` | **per 4x4 BLOCK** ~86k/frame | 52-entry LUT | ★★★ |
|  2 | `mb16.rs:6756` `plan_mb` | `powf` | per MB, 3600/frame | LUT | ★★★ |
|  3 | `mb16.rs:4098` `plan_inter_mb` | `powf` | per MB (t8 pick) | LUT | ★★★ |
|  4 | `mb16.rs:8808` `encode_slice_data_cabac_p` | `powf` | per MB (in-loop, `tune_rd_lambda_mb`) | LUT | ★★★ |
|  5 | `signals.rs:428` `log_vars` | `log2` | per MB map, per frame | poly `log2` | ★★ **WIRED R10** |
|  6 | `mbtree.rs:514` `gop_qp_offsets` | `log2` | per MB per frame per GOP | poly `log2` | ★★ **WIRED R10** |
|  7 | `mb16.rs:1132` `aq_qp_map` | `round` | per MB map | magic number** | ★ **WIRED R10** |
|  8 | `mbtree.rs:589` `gop_qp_offsets` | `round` | per MB map | magic number** | ★ **WIRED R10** |
|  9 | `rc.rs:20` `qstep` | `powf` | per frame | LUT (qp integral — sole shipping caller is `update` at `rc.rs:120`, passes `qp as u8 as f64`) | ☆ |
| 10 | `mb16.rs:5324` `encode_slice_data` | `powf` | per slice | LUT (consistency) | ☆ |
| 11 | `mb16.rs:5891` `encode_slice_data_b` | `powf` | per slice | LUT | ☆ |
| 12 | `mb16.rs:8668` `encode_slice_data_cabac_p` | `powf` | per slice | LUT | ☆ |
| 13 | `mb16.rs:9772` `encode_slice_data_cabac_b` | `powf` | per slice | LUT | ☆ |
| 14 | `rc.rs:94` `pick_qp` | `powf(QCOMP)` | per frame | — | ✗ cold |
| 15 | `rc.rs:106` `pick_qp` | `log2` | per frame | — | ✗ cold |
| 16 | `mb16.rs:1151` `aq_qp_map` | `log2`+`round` | once per frame | — | ✗ cold |
| 17 | `lib.rs:1036` `gop_iqp_offset` | `round` | per GOP | — | ✗ cold |
| 18 | `lib.rs:1050` `gop_bframe_qp_offset` | `round` | per GOP | — | ✗ cold |
| 19 | `telemetry.rs:63` `p_zero_q8` | `round` | per frame | — | ✗ cold |
| 20 | encoder `examples/` (`x264_bdrate`, `rd_skip_ab`, `me_ablation`, `me_signals`, `mbtree_bench`), `tests/` (`transform8x8_roundtrip`, `rd_skip_conformance`, `rusty_h264/tests/roundtrip`), plus `bench/` (Rust + Python) and `video-tests/analyzer/` | `log10`/`powf`/`sin`/`cos`/`exp`/`ln` (~60 sites) | offline analysis | — | ✗ tooling |
| 21 | `rc.rs:114` `pick_qp` | `round` | per frame | — | ✗ cold (census hole, found 2026-08-26) |

** `round` is the skill's borderline case and it is NOT a free swap here. Rust's
`f32/f64::round` is ties-**away-from-zero**, which no x86 instruction
implements; the magic-number trick gives ties-to-**even**. On sites 7 and 8 the
argument is a QP offset that lands on `.5` often enough to matter, so this
changes output and must go through a BD-rate gate, not a byte-identity one.
Sites 1-4 and 9-13 have no such problem — the LUT is exact.

## Already solved — do not "fix" these

Three sites look like hot transcendentals and are not, because the codebase
already did the right thing. Worth naming so nobody re-finds them:

* `mb16.rs:511` `build_mv_cost` — `2*log2(a+1)+0.718`, the classic mvd bit cost.
  Wrapped in a `OnceLock` building a **4096-entry table**. Runs 4096 times per
  PROCESS, not per candidate. This is the single hottest-looking site in the
  census and it is already a LUT.
* `mb16.rs:545` `build_true_biased` — same, `OnceLock`.
* `mb16.rs:1145` `aq_qp_map` — `2f64.powf(...)` inside `std::array::from_fn`,
  a small table built per call rather than per MB.

## Recommended first brick

> **DONE 2026-08-26 (Round 6): all nine sites wired to per-process `OnceLock`
> tables in the new `fastmath` module (A4's home) — not a transcribed `const`,
> the SAME expression cached, so bit-identity needs no argument. 12/12 encode
> hashes unchanged.**

Sites 1-4 and 9-13, as ONE change: a `const LAMBDA_QP: [f64; 52]` in the encoder,
`lambda = scale * LAMBDA_QP[qp as usize]`. Nine call sites, one table, EXACT.

Gate it with `bash bench/ident_gate.sh` plus an encoder byte-identity run — if
the encoded bitstream is not byte-identical, the LUT is wrong, and that is a
complete test. No tolerance sweep, no BD-rate run, no accuracy argument.

Then measure. Per `codec-measurement`: the encoder's own pinned A/B, not a
single wall-clock run. **Do not assume sites 1-4 are worth it until the clock
says so** — `rdoq_strength` defaults to `0.0` and is set above zero only by the
CABAC slice coders, so on a CAVLC encode site 1 never executes at all, and the
whole first tier is dead. Measure on a CABAC encode or the number is fiction.

---

# Part 2 — division, int and float

Division is NOT a transcendental, but the SAME targeting question decides it,
and getting that question wrong is how a division campaign burns a week on
no-ops. Full census of `/` and `%` across shipping `src`, classified by DIVISOR
KIND (the only thing that matters):

| divisor kind | count | what the compiler already does | win? |
| ------------ | ----: | ------------------------------ | ---- |
| power-of-2 literal (`/2 /16 /256 /4.0`) | **487** | emits a SHIFT (int) or `*0.25` (float, exact) | **none** |
| other int literal (`/3 /6 /255`)        | **71**  | emits multiply-high + shift            | **none** |
| float literal, non-pow2 (`/3.0 /1000.0`)| ~6      | KEEPS the divide (Rust will not reciprocate - not IEEE-exact) | small |
| **VARIABLE divisor** (`/w4 /mf /td`)    | **143** | emits a real `div`: 20-40 cycles, does not vectorise | **the target** |

So 558 of the 707 sites are already free, and saying otherwise would be the
`sqrt`/`min`/`max` mistake from Part 1 in a different spelling. Of the 143
variable-divisor sites, the large majority are in `dump`, `report`, `config`,
`encode_all_bframes` and other per-run or per-frame code. **Thirteen are in hot
decoder functions and nine in hot encoder functions**, and those are below.

## Already solved - do not "find" this again

The classic H.264 per-macroblock `addr / mb_w` + `addr % mb_w` pair is GONE from
the slice loops: `decode_slice_cabac_inner` and `decode_slice_cavlc_inner`
compute it ONCE at slice entry to seed `(mbx, mby)` and then carry the
coordinates with a compare-and-wrap. That was the biggest division in the
decoder and it has been fixed.

## The list — every division worth touching

EXACT = bit-identical, gate with `bench/ident_gate.sh` alone.

| #  | site | divisor | frequency | fix | exact? |
| -- | ---- | ------- | --------- | --- | ------ |
| D1 | `enc/mb16.rs:6674` `rdoq::dist` | `65536.0 / mf[POS[p]]` | **per COEFFICIENT per candidate level** | hoist 8 reciprocals before the loop | **EXACT** |
| D2 | `dec/mb16.rs:1338,1345` `row_hook` | `addr / self.mb_w` | **per MACROBLOCK** (its own comment says so) | caller already carries `mby` - pass it | **EXACT** |
| D3 | `dec/mb16.rs:5253` `decode_b_direct_temporal` | `(16384 + td.abs()/2) / td` | per B-direct partition | `td` is `.clamp(-128,127)` -> 256-entry LUT | **EXACT** |
| D4 | `enc/mb16.rs:4627,4630` `skip_luma_is_free` | `(65536 + mf-1) / mf` | per skip test | `ceil(65536/mf)`, mf from `QUANT_MF_OH[qp]` -> 52x8 table | **EXACT** |
| D5 | `enc/mb16.rs:2765` `motion_search` | `(ss - s*s/n) / n`, twice | per wide-search variance test | `n = rw*rh` is ALWAYS a power of 2 (256/128/64/32/16); shift by `n.trailing_zeros()` | **EXACT** (assert it) |
| D6 | `dec/mb16.rs:6952,6972` `recon_p_skip_fullpel` | `plane.len() / stride` | per P_Skip MB | it is the reference frame's ROW COUNT - a constant property, recomputed per call. Store on `RefFrame`. | **EXACT** |
| D7 | `dec/mb16.rs:5488,5496` `bz_fp_valid` | `guard.len() / lst`, `/ cst` | per B-skip validity test | same as D6, same field | **EXACT** |
| D8 | `common/transform.rs:148` `quant_dz_ff` | `(1i64 << qbits) / dz_div` | per block | `dz_div` is 3 or 6; `qbits` from qp -> small table | **EXACT** |
| D9 | `common/transform.rs:616,619` `quantize_8x8` | `/ dz_div`, `/ weight` | per 8x8 block | same shape as D8 | **EXACT** |
| D10| `enc/mb16.rs:1151` `aq_qp_map` | `sum_vs / sum_v` | once per frame | leave it | - |

## Read the ranking like this

**D1 is the one to do first, and it is not a strength reduction at all — it is a
redundant recompute.** `mf` is `QUANT_MF_OH[qp]`, EIGHT entries; `POS` maps the
16 coefficient positions onto those eight. So `65536.0 / mf[POS[p]]` has eight
distinct values for the whole trellis, and it is being divided out per
coefficient per candidate level. Hoisting `let inv: [f64; 8] = ...` before the
loop is the SAME division computed eight times instead of thousands — bit-identical
by construction, no tolerance argument, no accuracy sweep.

**D2 is free information the caller is throwing away.** `row_hook` divides
`addr / self.mb_w` to recover the row, per macroblock, while the slice loop that
calls it already carries `mby` — the very value the division reconstructs. The
loop was optimised and the hook was not.

**D3 and D4 are the Part 1 pattern again**: a division whose argument is a small
clamped integer, so an exact table replaces it. D3's `td` is literally
`.clamp(-128, 127)` two lines above the divide.

**D5 needs a `debug_assert!(n.is_power_of_two())`.** It is true for every H.264
partition (16x16 down to 4x4 gives 256..16, all powers of two), but it is true
by CONSTRUCTION, not by the type — and a shift on a non-power-of-2 is silently
wrong, which is the same failure mode as the `& 15` mask that corrupted 8x8
decode in the bounds-check campaign.

## What NOT to do

- **Do not touch the 487 power-of-2 sites.** They are already shifts.
- **Do not "optimise" `/3` or `/255` on integers.** The compiler emits
  multiply-high; hand-writing it is slower and wrong more often.
- **Do not blanket-replace float `/k` with `* (1.0/k)`.** For non-power-of-2 `k`
  that is NOT bit-identical, so it forfeits the byte-identity gate and needs a
  BD-rate run. Only worth it where the profile says so; none of the sites above
  need it, because every one of them is exact by another route.
- **Do not assume any of this is hot until measured.** D1 sits behind
  `rdoq_strength > 0.0`, which is set only by the CABAC slice coders — on a
  CAVLC encode the whole of D1 never executes, exactly as in Part 1.

## Variable Divisors DONE

**17 variable-divisor sites removed, every one EXACT.** Verified: full workspace
test suite green, decoder **68/68 byte-identical**, and encoder output
byte-identical across six configurations (CABAC default, CAVLC, B-frames,
all-intra, AQ off, SATD) on 720p shields.

Not one of these is an approximation. Every fix is either a table computed from
the same arithmetic, a value the caller already held, or an identity — so the
byte-identity gates ARE the correctness proof and no tolerance argument exists
to get wrong.

| # | site | was | now | why exact |
| - | ---- | --- | --- | --------- |
| 1,2 | `dec/mb16.rs` `row_hook` | `addr / self.mb_w`, twice | `mby` parameter | both callers carry `(mbx,mby)` with compare-and-wrap; `debug_assert_eq!` pins it |
| 3 | `dec/mb16.rs:5253` `decode_b_direct_temporal` | `(16384+\|td\|/2)/td` | `TX_FOR_TD[td+128]` | `td` is `.clamp(-128,127)` two lines above: 256 inputs |
| 4 | `dec/mb16.rs:4656` `implicit_weights` | same expression | same table | **a twin** — found only by grepping the expression, not the function |
| 5,6 | `dec/mb16.rs` `recon_p_skip_fullpel` | `plane.len()/lstride`, `/cstride` | `rf.lrows()`, `rf.crows()` | plane is `(cw+2LPAD)*(ch+2LPAD)`, stride is the first factor |
| 7,8,9 | `dec/mb16.rs` `bz_fp_valid` | same two, luma **twice** | same accessors | another twin |
| 10 | `enc/mb16.rs:6674` `rdoq::dist` | `65536.0/mf[POS[p]]` per coefficient per candidate level | 8 hoisted reciprocals | `mf` has EIGHT entries; same division, 8x instead of thousands |
| 11,12 | `enc/mb16.rs` `skip_luma_is_free` | `ceil(65536/mf)` x8 in a loop, **plus `t_dc` re-dividing the `p==0` case** | `CEIL_65536_MF[qp][p]` | pure function of `QUANT_MF_OH`, 52x8, const-evaluated |
| 13,14 | `enc/mb16.rs:2765` `motion_search` | `(ss - s*s/n)/n` | `>> n.trailing_zeros()` | `n = rw*rh` is 256..16, always a power of two |
| 15 | `common/transform.rs:148` `quant_dz_ff` | `(1<<qbits)/dz_div` | `match` on 3 / 6 | naming the constant turns a `div` into a multiply-high — **REFUTED IN ASM, superseded by Round 3**: once Round 2 added the `d.max(1)` fallback arm, LLVM merged all three arms back into ONE select-fed `divq`. Byte-identity held; the instruction claim was never true in the shipped binary. |
| 16 | `common/transform.rs` `quantize_8x8` | `(1<<qbits)/dz_div` | same `match` | same — same refutation, same supersession |
| 17 | `common/transform.rs` `quantize_8x8` | `mf*16/weight[idx]`, **64x per 8x8 block** | flat-matrix branch | default matrix is `[16;64]`, so `*16/16` is the identity |

### The two findings worth carrying forward

**TWO OF THE SEVENTEEN ARE TWINS, and the function-level census missed both.**
Site 4 is the same `(16384+|td|/2)/td` expression in `implicit_weights`, and
site 9 is a second `luma_guard().len()/lst` in `bz_fp_valid`. The
census grouped by ENCLOSING FUNCTION, so a duplicated expression in a function
already counted read as one site. **Grep the EXPRESSION, not the function** —
the same lesson the bounds-check campaign learned three times.

**THE CENSUS OVER-COUNTED "VARIABLE" BY TREATING ANY IDENTIFIER AS ONE.**
`signals.rs:261` looked like a prime target — `(sq - sum*sum/n)/n` in a per-MB
loop — until the line above turned out to be `let n = 64i64;`. A literal bound to
a name is still a literal, and the compiler already shifts it. Re-classified: of
the original 143 "variable" sites, 3 are literal-bound and 51 are in
`dump`/`report`/`fmt`.

### Why it stops at 17 and not 20

After these, **79 variable-divisor sites remain and not one is per-macroblock or
per-block.** They are in `encode_all_bframes` (8), `gop_qp_offsets` (7),
`insert_frame_num_gaps` (5), `pick_qp` (5), `decode_slice` (4), `prof::ticks` (3)
— per-frame, per-GOP or per-run code where a 20-cycle divide is unmeasurable.

Three that look warm and were deliberately LEFT:

* `dec/mb16.rs:1829` and `:3923` — `(addr % mbw, addr / mbw)` seeding `(mbx,mby)`
  at slice entry. Genuinely necessary and once per slice; this is the surviving
  half of the optimisation that removed the per-MB pair.
* `dec/lib.rs:1261-1262` — three float divides by `mbs` feeding the route gate,
  once per PICTURE. A reciprocal-multiply here is NOT bit-identical, so it would
  forfeit the byte-identity gate to save three divides per frame.

Padding the list to 20 with cold sites would mean shipping churn that no
measurement supports, which is the same error as "optimising" the 487
power-of-2 divides. **17 is the number of variable divisors in this codec that
are both hot and exactly removable.**

### Not yet measured

All 17 are gated CORRECT. None are gated FAST. The instruction-count and clock
work is the next step, and per `codec-measurement` it needs the encoder's pinned
A/B — with the caveat from Part 1 repeated: sites 10-12 sit behind
`rdoq_strength > 0.0` and CABAC, so a CAVLC encode will measure nothing.

---

## Round 2 DONE - 21 panic-path removals

Round 1 mined the hot VARIABLE DIVISORS and stopped honestly at 17, recording
that no 18th hot-and-exactly-removable one existed. Round 2 started by trying to
extend that vein and **failed three times in a row** - which is what pointed at
the right one.

**Every result below is gated: decoder 68/68 byte-identical, encoder
byte-identical across 18 preset/config rows (3 presets x {b0/q26, b0/q32,
b2/q26}), full workspace test suite green.**

### What the assembly said

| class | before | after |
| ----- | -----: | ----: |
| `panic_bounds_check`, encoder | 857 | **597** |
| `panic_const_div/rem`, common | 4 | **0** |
| `panic_const_rem`, decoder slice entry | 2 | **0** |
| `unwrap_failed`, decoder | 263 | **258** |

### The 21

| # | site | shape | effect |
| - | ---- | ----- | ------ |
| 1,2 | `dec` `recon_b_skip_fp` | `&self.refs1[r1.unwrap()]` -> `let Some(rf1) = r1.and_then(..) else { return false }` | the safe idiom was **on the adjacent line** for `rf0` |
| 3 | `dec` `iw00` | `is_none()` + `unwrap()` -> one `match` | one discriminant read, not two |
| 4,5 | `dec` collocated-ref reads | `col.live.is_some()` + `as_ref().unwrap()` -> `if let Some(live)` | x2 |
| 6 | `dec` `b_mc_or_record` | `edc_regions.as_mut().unwrap().push` -> `if let Some` | refusal, not panic |
| 7,8 | `common` `quant_dz_ff`, `quantize_8x8` | `/ d` -> `/ d.max(1)` | `.max(1)` gives LLVM `[1, i64::MAX]`, folding BOTH the zero and the `MIN/-1` overflow check |
| 9 | `common` `quantize_8x8` | `/ weight[idx]` -> `.max(1)` | a 0 scaling-list entry was a panic per coefficient |
| 10,11 | `dec` `decode_slice_*_inner`, `decode_slice_data` | `addr % mb_w` -> `mb_w.max(1)` | **divide-by-zero reachable from a malformed SPS, at slice entry, on the untrusted-input path** |
| 12 | `enc` `load_mb` | per-element stores -> `copy_from_slice` per row | **176 checks -> 0; 1823 -> 1174 instrs (-35.6%)** |
| 13,14 | `enc` `mb_ssd` luma+chroma | zip of two row slices | 40 -> 0; 644 -> 534 instrs |
| 15,16 | `enc` `pred_ssd` luma+chroma | same | -> 0 |
| 17,18,19 | `enc` `residual`, `store`, `pred_block` | row slice per row | -> 0 |
| 20 | `enc` `skip_luma_is_free` | row slice per row | 23 -> 2; 772 -> 739 instrs |
| 21 | `enc` `plan_inter8_luma` (residual + SSD passes) | row slice per row | folds into its inlined caller |

### The three dry veins, recorded so they are not re-dug

1. **`predict.rs` per-pixel stores -> `chunks_exact_mut`.** Measured **NET ZERO**
   across five predictors, with `chroma8x8_pred` **+4.5% WORSE**. Reverted;
   refutation written above `luma16x16_pred`. LLVM already recognises
   `out[y*W+x] = top[x]` as a row copy.
2. **Whole-function spill density.** The scan works once the regex is AT&T
   (`-8(%rbp)`, not `[rbp-8]`), but the top hits were `resolve_scaling`,
   `edc_stats_report`, `new_progress_slot` - all COLD. This is the banked law
   again: a whole-function statistic cannot answer a hot-loop question.
3. **`memcpy`/`memset` call sites.** 517 of them, and the dense ones
   (`pz_flush_slow`, `recon_p_skip_band`) are legitimate bulk `.fill()`/row
   copies - the shape you WANT, not a defect.

### Two refutations that cost nothing because they were checked first

* **`save_mb_into` is NOT the mirror of `load_mb`.** The identical row-slice
  rewrite retired all 15 checks but grew the function **1030 -> 1771 instructions
  (+72%)**, because `Vec::extend_from_slice` carries its own capacity check and
  `memcpy` call. **A write into a pre-sized destination and an append into a
  growable one are different shapes; only the former is a clean win.** Reverted,
  refutation recorded at the site.
* **`commit_direct_motion` was already optimal.** It looks like the same shape,
  but its six parallel arrays would need 6 x 4 = 24 slice checks where LLVM
  currently hoists to 16. Slicing it would have been a REGRESSION. Checked
  before editing, per the untouched-signal law.

### One candidate deliberately refused

`clamp_plane_per_pixel` is the densest remaining per-pixel loop and a textbook
match for the shape - and it is `#[cfg(test)]`, the **correctness ORACLE** for
`clamp_plane`, kept per the scalar-twin discipline. Its naive form is the entire
point of it: "optimising" it would delete the independent check that makes the
fast twin trustworthy. Skipped, and the next candidate taken instead.

The distinction that matters: refusing a SITE is not a reason to stop the COUNT.
The encoder still carried 618 checks at that moment; skipping one oracle and
banking two more real sites was the correct move, and the first pass got it
wrong by treating the refusal as a stopping point.

### The finding that outlives this round

**The bounds-check campaign that took the decoder 765 -> 1 never touched the
encoder, which still carries 618.** `load_mb` alone was 176 of them - the densest
single site in the codec, at 9.7% of its own instructions - and it fell to one
mechanical rewrite. The remaining 618 sit mostly in `plan_mb` (86),
`plan_inter_mb` (83) and `encode_slice_data_cabac_p` (52), at 1-3% density where
the payoff per edit is much lower. **The dense tail is gone; what is left is a
long flat one.**

### Not measured

All 19 are gated CORRECT and are deterministic wins on instruction and
check COUNT. **None are gated FAST** - no clock was applied, per
`codec-measurement` on a box pinned at 100%. The decoder bounds campaign
historically converted this class into +2.2..10.3%, but that is a prior, not a
measurement of these.

---

# Census addendum — 2026-08-26: the missed components

Full re-census of the working tree, every family in the skill's targeting table
(`powf/powi`, `exp/exp2`, `ln/log/log2/log10`, `sin/cos/tan/atan/tanh/sinh`,
`round/floor/ceil/trunc`, `sqrt/cbrt/hypot/recip`, `rem_euclid/fmod`) over ALL
Rust sources, not just `crates/*/src`. Six findings; one is real work.

## A1. `trellis_quant` — the second trellis quantizer, carrying the D1 disease

> **DONE 2026-08-26 (Round 3 below): hoists landed, 32 float divides/block → 4,
> gated by a per-coefficient-formula oracle test + full suite + encode hashes.**

**`common/transform.rs:201 trellis_quant` was missed by BOTH censuses.** Part 1
missed it legitimately (it has no transcendental — its `.powi(2)` at `:224` is a
multiply, same class as `signals.rs:431`). Part 2's division census missed it
wrongly: it computes, **per coefficient, per 4x4 block**:

* `let ideal = num as f64 / scale;` — `:220`
* `let lambda_q = lambda * (mf * mf) as f64 / (scale * scale) * 64.0;` — `:219`

Two float divides x16 coefficients = **32 variable-divisor float `div`s per
call**, where `scale = (1u64 << qbits) as f64` is a runtime power of two the
compiler cannot prove. This is exactly the D1 shape that was hoisted out of
`rdoq` — the codec holds TWO trellis quantizers, the wired one was cured, and
the dormant twin still has the disease. The plan's own twins lesson ("grep the
EXPRESSION, not the function") applied one level up: grep the ALGORITHM.

**Why it is not urgent:** its only caller is its own test (`transform.rs:1412`);
the doc comment says it is deliberately unwired pending feedback-aware
integration. Cold code, verdict ☆ — but it is a landmine, not a no-op, because
the moment it gets wired it is site-1 frequency (per 4x4 block).

**The fix, when wiring it (both routes EXACT, gate = byte-identity):**

1. Hoist `lambda_q` before the 16-coefficient loop: `mf` has only THREE distinct
   values per call (`pos_group`), and `qp`/`scale`/`lambda` are call-constants —
   same division, 3x per call instead of 16x. The literal D1 move.
2. `ideal` cannot hoist (`num` varies), but `num as f64 * (1.0 / scale)` IS
   bit-identical **here specifically**: `scale = 2^qbits`, its reciprocal is
   exactly representable, and multiplying/dividing by an exact power of two is
   pure exponent arithmetic. The "do not blanket-replace `/k` with `*(1/k)`"
   rule from Part 2 is about non-power-of-2 `k`; it does not apply.
3. `off = (1i64 << qbits) / 3` at `:205` is a LITERAL divisor — multiply-high,
   already free, no defect. Do not "fix" it.

**And the consolidation law (skill §7): decide merge-or-delete BEFORE wiring.**
Two trellis implementations (`rdoq` in `enc/mb16.rs`, `trellis_quant` in
`common/transform.rs`) will drift, and the drift will land in exactly the line
that decides the win — it already has, in the shape of the un-hoisted divides.

## A2. Part 2 correction: a per-MB variable float divisor DOES remain

> **DONE 2026-08-26 (Round 4 below): the loop family got its exact wins — the
> zero-propagate shortcut skips the libm `log2` AND the divide for ≥ 1/n of all
> calls, the twin frac divide is computed once, and the pipeline dropped to one
> allocation per array. Gated by a three-mode end-to-end golden.**

Part 2 closed with *"79 variable-divisor sites remain and not one is
per-macroblock or per-block."* **That sentence is wrong.** `mbtree.rs:514`
computes `total / intra` **per MB, per frame, per GOP** — the same frequency as
site 6's `log2` beside it — and the census filed it under "`gop_qp_offsets` (7)"
as per-GOP code.

**The verdict stands anyway, for a reason worth recording:** a float divide is
SIMD-baseline (`divpd` — right-hand column of the targeting table), it
vectorises WITH the loop once site 6's `log2` goes polynomial, and it is
dominated by the `log2` it feeds. Leave it. The correction is to the
completeness claim, not the work list.

## A3. The non-exact tier (sites 5–8) has no gate spec — here it is

> **EXACT TIER DONE 2026-08-26 (Round 5 below): sites 5 and 7's loop family got
> every bit-identical win available — the flat-MB `log2(1)` shortcut, the
> per-process qstep table, three interior-walk div/mod pairs, the O(n) median —
> gated by new bit-exact signal/AQ goldens. The ★★ POLY tier below remains
> deliberately NOT done: it is a BD-gated change and this campaign ships only
> exact rounds.**

The plan gates the LUT tier (byte-identity) and footnotes sites 7/8, but says
nothing about how to gate the ★★ poly-`log2` sites. Per the skill: bit-identity
is not available and demanding it forbids the change. The gate is:

1. **Dense sweep vs libm** over the argument's actual range (site 5:
   `log2(v+1)`, `v` = integer MB variance, so `[0, ~2^16]`; site 6:
   `log2(total/intra) >= 0`), reporting worst error AND its location, with the
   relative-OR-absolute metric from the skill.
2. **Landmarks:** exact at powers of two (a bit-pattern range reduction gives
   this for free), monotone across the range.
3. **The caller's own oracle:** 4-QP BD-rate per clip, distribution not mean —
   the same gate as sites 7/8's footnote, because all four sites perturb only
   per-MB QP offsets. Conformance (decode = recon) must still hold; only the
   DECISIONS may move.

**Pairing:** each of the four loops carries exactly ONE libm barrier (site 5's
`log2` map loop and site 7's `round` map loop are separate loops in the AQ
pipeline; likewise sites 6 and 8 in mb-tree — verified against the working
tree). So each fix vectorises its own loop and stands alone — but ship and gate
each PIPELINE (5+7 = AQ, 6+8 = mb-tree) as one BD-rate change, or the campaign
pays for two conformance runs per pipeline.

## A4. One fastmath module, not two

> **DONE 2026-08-26 (Round 6 below): `enc/src/fastmath.rs` exists and is the
> one home — the wired lambda/qstep tables plus the RESIDENT, oracle-tested,
> deliberately unwired poly-`log2` and magic-round kernels for the BD round.**

Sites 5 and 6 live in different files (`signals.rs`, `mbtree.rs`); the naive
patch writes two poly-`log2`s, which is the skill's three-copies drift trap
verbatim. Grep confirmed 2026-08-26: this repo has **no existing** `fast_exp` /
`fast_log` / magic-number kernel anywhere — the poly `log2` will be the FIRST.
Put it in ONE place with the scalar-oracle sweep test beside it, and both sites
call it. The magic-number `round` helper for sites 7/8 belongs in the same
module for the same reason.

## A5. The `lme = lambda.sqrt()` companions — named so nobody re-finds them

> **VERIFIED + area mined 2026-08-26 (Round 7 below): the "LICM hoists them"
> claim was an assumption; the asm now confirms it (every sqrt sits in the
> function preheader). Measuring the claim surfaced the drivers' real glue
> defects — fixed, −2 bounds per driver + a codec-wide panic-path
> retirement.**

Four sites sit next to the lambda `powf`s: `mb16.rs:5495` (CAVLC P, in-loop),
`:5892` (B, per slice), `:8864` (CABAC P, in the coded-MB path), `:9776`
(CABAC B, per slice). All four take the SLICE lambda, so the in-loop ones are
loop-invariant and LICM hoists them; `sqrt` is SSE2/NEON baseline (the
do-not-touch column). **Zero work — and the `LAMBDA_QP` first brick changes
nothing here**, since `LAMBDA_QP[qp].sqrt()` is the identical arithmetic. The
one that IS real is site 4's per-MB `lam_mb` powf at `:8808`, already in the
table.

## A6. Scope completion — the trees the census never claimed

> **CONSOLIDATED 2026-08-26 (Round 8 below): the A6 trees' verdict was
> "✗ tooling, leave" — and then an AST-level audit found the BD-rate gating
> arithmetic already FORKED across them: four `bd` variants, three `polyfit3`s,
> two `ssim_db` dB-caps and two `bits()` clamp policies, two pairs under the
> same name. All consolidated into one Rust home (`bench::metrics`) and one
> Python home (`bench/bdmath.py`), variants named, divergences pinned by
> test.**

`bench/` (Rust harness examples + Python reports) and `video-tests/analyzer/`
carry ~40 further `log10`/`log2`/`powf`/`exp`/`ln` sites — all BD-rate/SSIM-dB
analysis math, all ✗ tooling, same verdict as site 20 (row updated).
`_greatgate/` has none. The two shipping `rem_euclid` sites
(`dec/params.rs:49` on 256, `dec/mb16.rs:963` on 52) are literal divisors —
already multiply-high, free.

**Finding 1 re-verified on the current tree: still zero `exp`/`ln` anywhere in
shipping `crates/*/src`, and still zero transcendentals of any kind in the
decoder.**

## A7. The deblock tail — the last counted defect in `common` (added 2026-08-26)

Rounds 3 and 7 both measured it and deferred it: after the quant family went
to zero, EVERY remaining bounds check in `rusty_h264-common` sat in `deblock`
(`filter_frame_rows` 14, `filter_luma_line` 8, one closure) — and deblock is
DECODER-HOT, per-edge per-macroblock, the class the bounds campaign
historically converted into +2.2..10.3%. The scalar line filters are the
DEFAULT build's production path (`--features asm` is opt-in), and their
per-sample strided indexing (`plane[(base + i·step)]` x8 reads + up to 6
writes) is the dense spot: for VERTICAL edges `step == 1`, so the eight
samples are CONTIGUOUS and a fixed-extent window can fold every per-sample
check.

> **DONE 2026-08-26 (Round 9 below).**

## Revised first brick (supersedes nothing, extends the original)

Unchanged: `const LAMBDA_QP: [f64; 52]`, nine call sites, EXACT, byte-identity
gate, measure on a CABAC encode. Added by this addendum:

* ~~When `trellis_quant` is ever wired: apply A1 first~~ — **A1 DONE, Round 3.**
  The two-trellis consolidation question still stands before wiring.
* The ★★ tier now has its gate (A3) and its home (A4).
* Site 21 and the A6 trees are census-complete and stay untouched.

---

# Round 3 DONE — A1 + five wins in the quant family (2026-08-26)

All in `common/transform.rs`. Every change bit-identical by construction, each
with the ORIGINAL arithmetic preserved as a test oracle. Gates: **encoder
byte-identity 12/12 hashes** (encode_hash on foreman_cif across default/qp38/
all-intra-dz2/bframes2, x3 presets, sequential==parallel), **full workspace
suite green** (189+ tests), **3 new oracle tests**, and the asm counter below.

## First, the instrument bug and the refutation it uncovered

The div counter initially reported **zero integer divides in the whole crate**
— the regex matched `div ` but this toolchain emits AT&T with size suffixes
(`divq`, `idivl`). This is the SAME lesson Round 2 recorded for the spill scan
("the scan works once the regex is AT&T"), re-learned because the lesson was
written next to a different scanner. Fixed pattern: `\t i?div[bwlq]\t`.

The corrected counter then showed **Round 1's items 15/16 had never worked**:
`quant_dz_ff` still executed a real `divq` per call, because Round 2's
`d => .max(1)` fallback arm made LLVM canonicalize the three match arms into
one select-fed divide — un-doing the literal-arm strength reduction while
every byte-identity gate stayed green (correct, but not reduced). And the
eight per-group `(f + mf/2) / mf` divides under it were never in any census.
**Law: a source-level strength reduction is only real in the ASM, and a later
edit to the same expression can silently undo it. Re-count after every edit
that touches the expression, not just the one that claimed the win.**

## The scoreboard (static counts, release asm; dynamic per 4x4 block in parens)

| function | before | after |
| -------- | -----: | ----: |
| `quantize` | idiv 8 (9 divides/block) | **0** |
| `forward_quant` | idiv 12 (9/block) | **0** |
| `quant_dz_ff` hot path | idiv 8 (9/call, 9 direct encoder call sites) | **0** (fallback outlined `#[cold]`, 9→4 dynamic) |
| `quantize_8x8` | idiv 4 | **2** (custom-scaling-matrix arm only — rare, opt-in path) |
| `trellis_quant` | fdiv 8 (32 float divides/block) | **4** (= 1 reciprocal + 3 group values, 4/block) |

## The six changes

1. **A1 — `trellis_quant`**: `lambda_q` hoisted per position group (3 values,
   the D1 move), `/ scale` → `* (1.0/scale)` (exact: `scale = 2^qbits`), loops
   flattened over `POS_GROUP_FLAT`. Oracle:
   `trellis_matches_the_per_coefficient_formula`.
2. **`DZ_FF` const table** — `quant_dz_ff` rows for dz ∈ {2,3,6} × qp 0..52,
   const-evaluated from the same arithmetic (`dz_ff_row`). The hot path is a
   row load; kills the divides in `quantize`, `forward_quant`, and all nine
   direct encoder call sites.
3. **`DZ_F_8X8` const table** — the 8x8 dead-zone `f`, same disease, same fix.
4. **`quant_dz_ff_slow`** — the computed fallback (`cabac_dz_div` override,
   out-of-range qp), outlined `#[cold]` so its divides cannot re-merge into the
   table path, with the 8 per-group divides hoisted to 3.
5. **`POS_GROUP_FLAT` derived** from `const fn pos_group` at compile time —
   the hand-transcribed table deleted, surviving as the oracle in
   `derived_tables_match_documented_layout` (derive, don't transcribe).
6. **`GROUP8` derived** as the first half of `POS_GROUP_FLAT` (was a second
   hand copy of the same layout inside `quant_dz_ff`), same oracle.

## What was checked and deliberately NOT done

* **Bounds checks: zero in the whole quant family** (the counter proved it —
  LLVM's known-bits on const-table loads folds them). The planned
  table-padding edits were dead on arrival and were not made. The crate's
  remaining 23 bounds checks all sit in `deblock` (`filter_frame_rows` 14,
  `filter_luma_line` 8) — a different area, left for its own campaign.
* `quantize_8x8`'s remaining 2 idivs are the custom-scaling-matrix arm —
  per-coefficient `mf·16/weight[idx]`, unreachable on default streams (the
  Round-1 flat-matrix branch), not worth a per-call table.
* Not clocked, per `codec-measurement` on a box pinned at 100% — these are
  count wins with the usual prior (decoder bounds class converted to
  +2.2..10.3%), not measured speedups.

---

# Round 4 DONE — A2 + five wins in the mb-tree pipeline (2026-08-26)

All in `enc/mbtree.rs` (`gop_qp_offsets` and its helpers). Every change
bit-identical; the gate is a NEW three-mode end-to-end golden
(`gop_qp_offsets_golden`: exact `Vec<Vec<i32>>` output hashed for
HalfRes/Hybrid/FullRes on a deterministic synthetic GOP, vetoes pinned OPEN via
their documented env anchors so the golden pins the ARITHMETIC, with a
proves-the-tool-ran nonzero assert) captured BEFORE the edits and unchanged
after, plus a `coded_luma` per-pixel oracle test and the full workspace suite.

## The scoreboard (static asm counts for `gop_qp_offsets`, whole inlined pipeline)

| metric | before | after |
| ------ | -----: | ----: |
| float `div` sites | 8 | **7** (the residual pass's twin frac divide — (n−1)·mbs dynamic — is gone) |
| bounds-check calls | 23 | **19** |
| int `div` sites | 0 | **0** (see the chunks_exact refusal below) |
| `log2` call sites | 1 | 1 static — but the A2 shortcut skips it (AND the divide) for every zero-propagate MB: **≥ 1/n of all calls by construction** (backward propagation never credits the GOP's last frame), more on content with unreferenced regions |
| heap allocations in the pipeline | 2n+1 (`propagate` n+1, `offs` n) | **3** (two flat arrays + `frac_buf`) |

## The six changes

1. **A2 — zero-propagate shortcut** in the offs loop: `propagate == 0` ⇒
   `total == intra` ⇒ ratio EXACTLY 1.0 (`intra >= 1`, and `x/x` is exact) ⇒
   `log2(1.0)` exactly +0.0 — the surviving `-eff_strength * 0.0` is the
   ORIGINAL expression at its exact value (keeps the −0.0). Skips the libm call
   and the A2 divide, bit-identically.
2. **`propagate` flattened** to one `Vec<f64>` (same `split_at_mut` cur/prev
   discipline, same cells): n+1 allocations → 1.
3. **`offs` flattened** to one `Vec<f64>`: n allocations → 1; centering, sd and
   rounding iterate the same frame-major order, so every f64 sum sees the same
   summands in the same order.
4. **`frac` computed once** — the propagation pass and the residual-fraction
   pass computed the IDENTICAL `(intra−inter)/intra` per MB; the propagation
   pass now stores it (`frac_buf`, index-addressed so its reverse frame order
   doesn't matter) and the residual pass reads it in its original forward
   order — `fsum` bit-identical, (n−1)·mbs divides gone, at the honest cost of
   one (n−1)·mbs f64 buffer.
5. **`fc` closed-form** — it was `+= 1.0` per MB per frame; every increment is
   exact, so `((n−1)·mbs) as f64` is the same number and (n−1)·mbs float adds
   disappear.
6. **`coded_luma` row-slice rewrite** — the per-pixel double-`min` gather
   became a `copy_from_slice` + right-edge `fill` per row (the `load_mb`
   pre-sized-destination shape that works, NOT the append shape that
   regressed). Oracle test covers non-MB-multiple sizes where the padding is
   live.

## One refusal, caught by the recount

The first version of the flat-`offs` rounding used `chunks_exact(mbs)` — and
the recount showed **idiv 0 → 2**: `chunks_exact` computes `len / mbs`, a
runtime-divisor `div`. Per call and harmless, but a division campaign does not
get to ADD divides; replaced with indexed slicing (`offs[f*mbs..][..mbs]`),
idiv back to 0. This is the `strength-reduction-lives-in-asm` law paying for
itself one round after it was written: recount after EVERY edit, including the
cosmetic ones.

## Reassociation refusals (why two "obvious" fusions were not done)

* Fusing the residual-fraction sum INTO the propagation loop would accumulate
  `fsum` in reverse frame order — f64 addition is not associative, so that is
  NOT bit-identical. Refused; the frac buffer keeps the original order.
* Per-frame partial sums (`fsum_f[f]`, then summing those) reassociate across
  frame boundaries — also not bit-identical. Refused for the same reason.

## Honest caveats

* mb-tree is **opt-in** (`--mbtree`); nothing here moves a default encode.
* Not clocked, same as Rounds 2-3 — counts, not speedups. The A2 skip count is
  by-construction (≥ 1/n), not instrumented; the lookahead's own cost census
  (`SATD_CALLS`) still prices the search, which dominates this pipeline.
* The area's remaining static counts (7 fdiv, 19 bounds, 1 log2) are the
  mean/sd divides (per GOP, cold), the live per-MB frac/ratio divides (SIMD-
  baseline, vectorize with their loops), and `propagate_to`'s clamped-index
  writes (LLVM cannot prove `cy·mb_w+cx < len` from clamped factors). The log2
  is site 6 of Part 1 — the ★★ poly tier, a BD-gated change, deliberately NOT
  part of an exact round.

---

# Round 5 DONE — A3's exact tier + five wins in the signal pipeline (2026-08-26)

The A3 sites still standing after Round 4 are the AQ pair — site 5
(`signals.rs log_vars`) and site 7's function (`mb16.rs aq_qp_map`) — plus the
frame-signal probes that feed the same calibrated gates. This round took every
BIT-IDENTICAL win in that family; the poly/magic-round replacements stay
BD-gated future work, exactly as A3 specifies.

**Gates (all captured BEFORE the edits, all unchanged after):** a bit-exact
signal-vector golden (`signal_probes_golden`: f64-bit hashes of `log_vars`,
`headroom`, `mgain_dc`, `grain_floor`, `median_var`, `gmc_residual`,
`flat_run`/`hist_top16` at THREE frame sizes chosen to cover interior-walk
stride 1, stride > 1 and stride > row-width — these values feed CALIBRATED
tables, so one moved bit is a broken gate); an AQ-map golden
(`aq_qp_map_golden`, two strengths, with a proves-the-tool-ran assert that
caught its own first synthetic frame latching AQ off); encoder byte-identity
12/12 hashes vs the Round-3 baseline; full workspace suite green.

## A3 headline — site 5's flat-MB shortcut

`log_vars` computes `log2(v+1)` per MB; a FLAT MB (`v == 0`) computes
`log2(1.0)`, which **C11 Annex F requires to be exactly +0.0** — so the branch
skips the libm call bit-identically. The identity itself is asserted in the
golden (`1f64.log2().to_bits() == 0`), so a nonconforming libm would fail the
suite rather than drift. Content-scaled, not by-construction: flat MBs
dominate screen/synthetic/letterboxed content and vanish on noisy natural
video — stated honestly, and the golden proves the arm executes.

## The five wins

1. **`aq_qp_map`'s qstep table → `OnceLock`** — the 9-entry `2^(-d/6)` table
   (a pure function of constants) was rebuilt with 9 `powf` + 9 divides EVERY
   FRAME; now once per process, the `build_mv_cost` pattern, same expression.
2. **`b2_mgain` interior walk** — `i % (mbw-4)` + `i / (mbw-4)` per sampled
   block (the classic addr pair) → carried `(rx, ry)` with compare-and-wrap
   (`while`, because stride can exceed the row width).
3. **`me_wide_headroom`** — same pair, same fix.
4. **`grain_floor`** — same pair, same fix. (Three sites, one shape — the
   twins lesson again: the expression was grepped, not the function.)
5. **`frame_median_mb_var`** — full `sort_unstable` for ONE median →
   `select_nth_unstable` (the element at `mid` is its sorted-position element
   by contract: the same median exactly, O(n) not O(n log n)).

Plus a sixth that came free: `aq_qp_map`'s materialized `var: Vec<f64>` (one
n-sized allocation per frame) is gone — `(v+1) as f64` converts on the fly at
both consumers in the original read order, so both sums are bit-identical.

## The asm scoreboard (signal builders, whole inlined bodies)

| metric | before | after |
| ------ | -----: | ----: |
| int `div` sites across the probe builders | 6 (three builders x the div+mod pair) | **0** |
| `powf` in the per-frame AQ path | 9 calls + 9 divides per frame | **0** (once per process) |
| `log2` in `log_vars` | every MB | skipped on every flat MB (Annex-F-exact) |
| median cost | O(n log n) sort | O(n) select, +2 static bounds sites in select's internals (recorded honestly; per frame, off the per-MB path) |

## What was deliberately left

* **The ★★ poly-log2 / magic-round tier (sites 5-8 proper)** — non-exact,
  needs the A3 BD gate. The exact rounds are now MINED OUT in this family;
  the next win in these loops requires paying the BD-rate toll.
* `flat_hist`'s 256-bin sort for a top-16 sum could be a `select_nth` too —
  256 fixed elements, once per frame; below the churn line. Noted, not done.
* `var_percentile_thresh`'s memoized full sort SERVES multiple percentile
  queries per frame; `select_nth` per query would break the memoization.
  Correct as is.

---

# Round 6 DONE — A4's module + the first brick's nine sites (2026-08-26)

`enc/src/fastmath.rs` now exists as the encoder's ONE fastmath home, and the
plan's "recommended first brick" — untouched since the original census — is
landed through it. **Gates: encoder byte-identity 12/12 hashes vs the Round-3
baseline** (the all-intra config exercises `rdoq`'s trellis, the default
configs the CABAC coders — so site 1's per-block path is proven live, per the
census caveat that a CAVLC-only measurement would gate nothing), **full
workspace suite green, and three new in-module oracle tests.**

## What the asm actually said, first

The census said `powf`; the assembly says **LLVM had already rewritten every
constant-base `2f64.powf(x)` into an `exp2(x)` call** — a real libm call
still, but a different symbol, and a counter looking for `pow` alone reads
"already fixed". (Third instrument lesson of this campaign: count the op the
COMPILER emits, not the one the source spells.) Baseline: **10 static `exp2`
sites** across `rdoq` (per 4x4 block), `plan_mb` / `plan_inter_mb` (per MB),
`encode_slice_data_cabac_p` (2: per-MB `lam_mb` + per-slice), the CAVLC/B/
CABAC-B slice coders, `RateControl::update` (`qstep`), plus the B-slice copy
inlined into both `Encoder` drivers.

## A4 headline — the module

* **Wired, EXACT:** `lambda_qp(qp)` and `qstep_qp(qp)` — 256-entry
  per-process `OnceLock` tables built by the SAME expression the sites
  inlined (the `build_mv_cost` pattern; 256 entries so every `u8` input is
  table-exact and no range fallback exists to get wrong). Not a transcribed
  `const`: deriving at first use is what makes bit-identity need no argument.
* **Resident, oracle-tested, NOT wired:** `log2_poly` (sqrt-2-split atanh
  series, coefficients derived not transcribed, exact at powers of two BY
  CONSTRUCTION, sweep-gated `< 1e-11` worst error over both sites' domains,
  monotone) and `round_ties_even_fast` (f64 magic number `1.5·2^52`, pinned
  against its derivation, value-identical to `f64::round_ties_even` over a
  200k-point sweep INCLUDING exact .5 ties, sign-of-zero deviation pinned,
  and the skill's anti-aliasing assert: `fast(2.5)==2.0 != (2.5).round()`).
  Wiring either pays the A3 BD toll — that is written on the kernels, not
  just here.

## The five wins (the nine sites, wired)

1. Site 1 — `rdoq` (**per 4x4 block**, the census's hottest tier).
2. Sites 2+3 — `plan_mb` / `plan_inter_mb` (per MB).
3. Site 4 — `cabac_p`'s per-MB `lam_mb` (the AQ/mb-tree lambda-repricing path).
4. Sites 10-13 — the four per-slice lambdas (consistency: one expression, one
   home).
5. Site 9 — `rc::qstep`, signature tightened `f64 → u8` (every caller was
   integral; the spec landmark `Qstep(28)/Qstep(22) == 2` moved into the
   module's oracle).

## Scoreboard

| metric | before | after |
| ------ | -----: | ----: |
| `exp2`/`powf` static sites in shipping encoder functions | 10 | **0** (2 remain inside the two `OnceLock` initializers — once per process) |
| `pick_qp`'s `pow(QCOMP)` (site 14, ✗ cold) | 1 | 1 — untouched, as the table says |
| homes for transcendental-derived tables | scattered (mb16 x2 OnceLock, aq static, rc inline) | one module + the pre-existing mv-cost pair |

Dynamic accounting for site 1 alone: one libm call per 4x4 block (~86k/frame
at 720p on the all-intra trellis path) → one table load. Not clocked, as
every round: counts, with the standing prior.

---

# Round 7 DONE — A5 verified by instrument + five wins in the slice drivers (2026-08-26)

A5 claimed the four `lambda.sqrt()` companions were zero work because "LICM
hoists them" — an ASSUMPTION, never checked, which is exactly the class the
untouched-signal law exists for. **The instrument now says: confirmed.** Each
driver carries exactly ONE static `vsqrtsd`, positioned at instruction
720/7457 (`encode_slice_data`), 853/12416 (`cabac_p`) and 315/6193
(`cabac_b`) — function-preheader territory, not loop body. The claim is a
measured fact; the sites stay untouched (hand-hoisting what LLVM already
hoists is the predict.rs refutation).

Putting the counter on A5's functions is what surfaced the round's real work
— the drivers' per-MB glue:

## The five wins

1-4. **`aq_qp[mb_idx]` double-indexed in ALL FOUR drivers** (CAVLC-P,
   CABAC-intra, CABAC-P, CABAC-B): `fe.qp = aq_qp[mb_idx];
   fe.qpc = chroma_qp(aq_qp[mb_idx]);` loaded — and bounds-checked — the same
   element twice per macroblock. One local now feeds both. Same shape, four
   sites: the twins lesson, again, in the fourth spelling.
5. **`chroma_qp`'s panic path retired codec-wide** (`common/predict.rs`): QP
   is 0..=51 by construction on BOTH sides (encoder clamps, decoder wraps
   `rem_euclid(52)`), but the type admits 255, so `QPC[(qp-30) as usize]` — a
   per-macroblock helper for encoder AND decoder — carried a bounds check and
   panic path. `.min(21)` is inert for every reachable input and folds both
   (the quantizer round's `.max(1)` move).

Bonus, same loop: `sig.mb_vars()[mb_y * fe.mb_w + mb_x]` on `cabac_p`'s
per-coded-MB lme-veto path (and one harvest row) recomputed an index that IS
`mb_idx`, already in scope. Two sites.

## Scoreboard

| driver | bounds before | after |
| ------ | ------------: | ----: |
| `encode_slice_data` (CAVLC P) | 30 | **28** |
| `encode_slice_data_cabac_intra` | 12 | **10** |
| `encode_slice_data_cabac_p` | 52 | **50** |
| `encode_slice_data_cabac_b` | 34 | **32** |
| `chroma_qp` (common, standalone symbol) | 1 + panic path | **0** |

Gates: encoder byte-identity **12/12 hashes** vs the standing baseline, full
workspace suite green. The decoder inlines the same `chroma_qp`, so its
copies lose the panic path by the same mechanism.

## Left on the table, deliberately

`cabac_b` carries one static `idiv` from an inlined callee (once per slice —
source shows no variable divisor in its own body) and the drivers' remaining
~120 bounds sites are the flat 1-3%-density tail Round 2 already priced as
low-payoff-per-edit. Neither is worth blind edits in 6-12k-instruction
functions; they wait for a profiler-guided round.

---

# Round 8 DONE — A6's trees: the instruments get ONE arithmetic (2026-08-26)

A6 classified `bench/` and `video-tests/analyzer/` as "✗ tooling — leave".
Correct for SPEED, wrong for the thing that actually matters there: these
scripts GATE campaigns, and an AST-level audit (hash the parsed function
bodies, not the text) showed the Bjøntegaard arithmetic already forked:

* `ssim_db`: 7 Python copies + 1 Rust with a 90 dB cap, 1 Rust
  (`mbtree_gop_harvest`) with a **60 dB cap** — different clamp, same name.
* `bits()`: 3 Rust copies under ONE name with TWO clamp policies
  (`casc_a0`/`a1` unclamped vs `casc_ceiling` clamped).
* `polyfit3`: three AST-distinct Python variants; `bd`/`bd_rate`: four —
  all still semantically identical (truthiness vs `!= 0.0` on floats,
  len-check placement, lambda-vs-def), i.e. drift caught at the last moment
  it was still cosmetic. Two tools "measuring BD-rate" with different
  arithmetic gate campaigns against different rulers.

## The A6 headline — one home per language

* **Rust:** `bench/` gains a lib target (`rusty_h264_bench`), and
  `metrics.rs` becomes the home: `ssim_db`, `ssim_db_capped60`, `bin_bits`,
  `bin_bits_clamped`, `polyfit3`, `bd_rate` — the bdrate example's bodies
  moved VERBATIM. Where two POLICIES genuinely exist, both are named and the
  divergence is PINNED by test (the anti-aliasing pattern), so nobody
  "unifies" them and silently changes a harvest or a ceiling.
* **Python:** `bench/bdmath.py`, canonical bodies = the majority variant
  verbatim, with a frozen copy of the original as the selftest oracle (2000
  random point-set sweeps, EXACT float equality, plus the −50%-at-halved-rate
  landmark).

## The five wins

1. `bdrate.rs` — local `ssim_db`/`polyfit3`/`bd_rate` (67 lines) → the
   metrics home, verbatim move.
2. `casc_a0` + `casc_a1` — local unclamped `bits` → `metrics::bin_bits`.
3. `casc_ceiling` — local CLAMPED `bits` → `metrics::bin_bits_clamped`; the
   same-name/different-guard trap is now a visible, named choice.
4. `mbtree_gop_harvest` — its 60 dB-capped `ssim_db` → `ssim_db_capped60`,
   PRESERVED rather than "fixed": its banked CSVs were produced under this
   cap, and silently canonicalizing it would make new harvests incomparable
   on near-lossless GOPs. The variant is now impossible to miss.
5. The EIGHT Python reports (`campaign_delta`, `gate_refit`, `x264_standing`,
   `intra_vs_x264`, `t8_default`, `gate_audit`, `x264_h2h_report`,
   `x264_quality_report`) — 24 function definitions deleted, one import each.

## Gates

`cargo test` in bench (3 oracle tests: expression pins, variant-divergence
pins, BD landmarks), every bench example compiles, `python bdmath.py`
selftest exact-equality green, `py_compile` clean on all eight scripts. The
workspace is untouched this round (bench is deliberately outside it), so the
codec gates stand as of Round 7.

## Left, with reasons

* `video-tests/analyzer`'s PSNR copy duplicates `metrics.rs`'s — a THIRD
  package; a cross-package dependency for one shared line is below the churn
  line. Named here so it stays a known copy, not a discovered one.
* The mb-tree harvest's two-point BD approximation (ln-domain slope
  interpolation) is a deliberately different estimator for a different
  question (per-GOP counterfactual, 2 QPs), not a drifted copy of `bd_rate` —
  audited, left.

---

# Round 9 DONE — A7: the deblock vertical-edge window split (2026-08-26)

The addendum's last entry (A7, above) named the deblock tail as the one
remaining counted defect in `common`. The dense spot was the scalar line
filters' per-sample strided indexing; the exact win is that HALF of all
filtered lines never needed strided access at all.

## A7 headline — `filter_luma_line_contig`

For a vertical edge (`step == 1`) the eight samples `p3..p0|q0..q3` are the
consecutive bytes `plane[base-4 .. base+4]`. The new variant takes one
fixed-extent window (`&mut plane[base-4..][..8]` — the Round-2 shape whose
length LLVM keeps LITERAL, with constant indices 0..8 into it), so the
per-sample checks fold by construction: **at most 2 checks per vertical line
(the window slice itself) versus 8-14 checked strided accesses** — and the
underflow guard is free, because the walker already skips the left-border MB
edge (`be == 0 && mb_x == 0`). Identical loads, stores, and arithmetic — the
strided original stays as the HORIZONTAL path's production code, which makes
it a live oracle, not a frozen copy.

## The five wins

1. `filter_chroma_line_contig` — the 4-byte twin for vertical chroma edges
   (guarded the same way: `cxe == 0 && mb_x == 0` is skipped).
2. The luma vertical call site: `Line` construction gone, base-only call.
3. The chroma vertical call site: same.
4. `contig_line_filters_match_strided` — 4000 randomized planes x parameters,
   both `bs` arms plus the early-return path, strided-vs-contig planes
   asserted byte-equal. The oracle is the shipping horizontal code.
5. The A7 addendum entry itself — the tail is now a named, closed item
   instead of a twice-deferred footnote.

## Scoreboard, stated honestly

Static totals in `common`'s deblock: **23 → 19** bounds-check sites — but the
per-function split moved, because both line filters INLINED into
`filter_frame_rows` once the split gave each call site a single shape
(`filter_luma_line` as a standalone symbol is gone; the walker grew 3306 →
4231 instrs absorbing them). The deterministic claim is therefore the
structural one above (≤2 checks per vertical line, by the literal-extent
shape), not a per-function static delta — the count-what-the-clock-charges
law applied to one's own counter.

Gates: the oracle test, full workspace suite green, and **encoder
byte-identity 12/12** — which GATES DEBLOCK, because the encoder's
reconstruction loop runs the in-loop filter on its references, so a deblock
bit-change moves the bitstream.

## Refused

* Precomputing the strided filter's eight sample offsets (reads and writes
  recompute `base + i·step`): after inlining, GVN already merges the
  identical address expressions — hand-hoisting what the compiler does is the
  predict.rs refutation.
* `filtstat::report`'s 3 float divides: a cold, env-gated report.
* The walker's remaining ~18 sites: the flat tail again; profiler-guided or
  not at all.

---

# Round 10 DONE — the BD round: the poly tier ships, and the toll was zero (2026-08-26)

The plan's last open tier. Sites 5–8 (poly `log2` in the AQ and mb-tree per-MB
loops, magic-number round in their dQP maps) are non-exact IN PRINCIPLE — so
the round's design question was whether the encoder's DECISIONS ever move. The
answer, measured: **no. On a 12-clip corpus spanning every content axis, at 4
QPs, in both pipelines, the poly arm produced bit-identical bitstreams to libm
in all 96 comparisons. The A3 BD toll is 0.000% BY IDENTITY, and the tier
ships DEFAULT-ON.** The mechanism: the downstream decisions are integers with
fat margins, a < 1e-11 log2 perturbation cannot cross them off a knife edge,
and an exact-.5 round tie on transcendental-derived values is measure-zero —
argued before, now ASSERTED by hash.

## The twenty wins

**Wiring (1–6):**
1. `polytier_on()` — env switch (`RFF_POLYTIER=0` = the libm bisection
   anchor), read per call and deliberately uncached so one process can run
   both arms; plus a thread-local test pin, because tests run threaded and
   `set_var` would race.
2. Site 5 wired: poly `log2` in `log_vars` (the flat-MB shortcut precedes the
   kernel choice, so it stays exact).
3. Site 6 wired: poly `log2` in the mb-tree offs loop (the zero-propagate
   shortcut likewise).
4. Site 7 wired: magic round in the AQ dQP map.
5. Site 8 wired: magic round in the mb-tree rounding.
6. The A4 kernels promoted from oracle-tested residents to SHIPPING code —
   `#[allow(dead_code)]` gone; the module doc now states the measured truth.

**Gates built (7–12):**
7. Signal golden extended: libm arm pinned (it hashes f64 bits), poly arm
   swept against it (≤ 1e-11 everywhere, flat MBs exactly +0.0).
8. AQ golden extended: BOTH arms must produce the IDENTICAL u8 map, at two
   strengths — decision-identity asserted, not argued.
9. mb-tree golden extended: both arms, identical i32 offsets, all three
   lookahead modes.
10. `polytier_gate` harness: per-clip, 4 QPs x 2 arms x 2 pipelines,
    bitstream FNV verdicts with a (bytes, PSNR) fallback for `bdmath.py` had
    anything differed.
11. Anchor-arm proof: `RFF_POLYTIER=0` reproduces the standing 12-hash
    baseline exactly — the switch machinery itself adds zero drift.
12. Poly-arm default vs the same baseline: **12/12 identical** (AQ is
    default-on, so these are live default-path configs).

**Corpus verdicts (13–18):** decision-identical on every clip —
13. natural CIF x6 (foreman, akiyo, bus, football, mobile, tempete);
14. 4CIF x2 (city, crew);
15. 720p x2 (shields, FourPeople);
16. screen content (screen_text — the flat-MB-heavy class, AQ fully active);
17. grain (grain_akiyo — gates the VETO path's identity: both pipelines
    collapse to the same stream when the grain latch fires);
18. the AQ+mb-tree pipeline on all twelve (sites 6/8 live).

**Instrument findings (19–20):**
19. `.round()` on this toolchain compiles to an inline `roundsd` sequence
    (SSE4.1 in the target), NOT a libm call — so sites 7/8 were never
    call-bound HERE; the magic number trims the sequence and matters as a
    call only on portable-baseline builds. Counted, not assumed (the
    `strength-reduction-lives-in-asm` law's fourth application).
20. The poly `log2` loop did NOT auto-vectorize (bit-pattern ops + selects);
    the banked win is CALL REMOVAL — a real libm `log2` call per coded MB
    (x2 pipelines) becomes ~20 inline flops — not SIMD width. Stated so
    nobody cites the skill's 4.71x table for a loop that never went packed.

## What this round is and is not

It closes the plan: every site in the census is now wired, refused-with-
reasons, or table-backed, and the last "pays a BD toll" caveat is retired by
measurement. It does NOT close the x264 gap — these loops are per-frame setup
in opt-in-or-vetoed features, and the compression distance lives in the inter
tier (`inter-coding-gap`: P-16x16 motion inefficiency, reference count,
lookahead scope). That is the next campaign, and it starts from a codebase
whose transcendental floor is now zero-cost and fully gated.
