# The decoder's copy inventory — ranked, priced, and dispositioned

**Measured 2026-09-07.** Regenerate with `bench/copy_census.sh` (static, from the
emitted assembly) and `RS_H264_EDC_STATS=1 decode_bench <stream>` (runtime bytes,
`profile` feature).

This is the list, in the order that matters — by bytes actually moved on a real
stream, not by line number. A source grep for `copy_from_slice` finds 101 sites in
`mb16.rs` and ranks none of them: `b_skip_slow`'s 384-byte hop and the nnz grid's
4-byte one look identical in a grep, and one of them is on the rare path.

---

## 0. The instruments, and why there are two

| instrument | what it sees | what it cannot see |
|---|---|---|
| `bench/copy_census.sh` | every real `call memcpy/memset/memmove/alloc_zeroed` in the emitted assembly of **all three decode-path crates**, per symbol — including ones the optimiser INTRODUCED, excluding ones LLVM inlined away | how often each executes |
| `cpystat` (`feature = "profile"`) | bytes moved at runtime, bucketed by caller line via `#[track_caller]` | copies below the instrumented sites |

Neither alone is enough. The static census ranks `decode_stream_threaded_sink`
first with 80 copies; the runtime counters show it executes **zero** times in a
single-threaded decode.

> **The census's own first blind spot was its SCOPE.** Version 1 emitted only
> `rusty_h264_decoder` — so `common` (MC, deblock, transforms) and `accel` (every
> kernel) were invisible while the instrument looked rigorous. Widening it to all
> three crates took the totals from 623 calls / 103 symbols to **726 / 128**, and
> the result is a clean rule-out: none of common's or accel's 103 calls reaches
> the top 28. Those kernels write in place.
>
> **Its second blind spot was SCALE.** `refill` prices the per-picture GRID
> re-arm and nothing was pricing what happens per REFERENCE picture — a
> different, larger unit that the pool-clear counter cannot see by construction.
> That is where the second win came from (§2b).

---

## 1. What was ruled OUT — most of the value of the pass

- **No O(n² ) front-drain.** All five `drain()` sites are full `drain(..)`, not
  front-drains. This is the first thing the playbook says to grep for and it is
  simply not present.
- **The channel plumbing is not hot.** The static census's top three symbols —
  80 copies in the frame-MT sink, 21 in `Sender<JobOut>`, 17 in
  `Sender<PixelCtx>` — execute **zero** times in a single-threaded decode
  (`jobs=0 rows=0 needctx=0`). `EdcMsg` is 80 bytes and `EdcJob` 40; the 2.7 KB
  `PInterJob`/`BJob` are already pooled behind a `Box`, so they move as pointers.
- **The MC staging temporary is not memset.** `recon_p_inter_parts` stages
  through `let mut t = [0u8; 256]`, which looks like the classic per-call stack
  memset. Checked the `.s` for an actual `callq memset`: **none**. LLVM elided
  it. The standing "2 KB stack scratch" lever does not apply here.
- **The band copies are irreducible.** `bz_recon_band_copy` and
  `recon_p_skip_band` (18 `memcpy` each) copy reference pixels straight to the
  output plane with no intermediate. The runtime length makes each a real call,
  and the ladder that would inline it is already a recorded refutation
  (+54% code size, reverted).

---

## 2. The ranked list — per-picture grid re-arm

`refill` is the single funnel for the pool's clear, so one counter prices the
whole thing. Pooling made the ALLOCATION free; what is left is the clear.

**Before this pass: 733,764,000 B over a 60-frame 720p decode — 12.2 MB per
picture, ~7.8% of decode at memset bandwidth.**

| # | grid | element | B/picture | status |
|---|---|---|---:|---|
| 1 | `mv_y` | `(i32,i32)` | 460,800 | **priced, deferred** — see §3 |
| 2 | `mv1` | `(i32,i32)` | 460,800 | **priced, deferred** — see §3 |
| 3 | `ref_idx_y` | `i32` → **`i8`** | 230,400 → 57,600 | ✅ **DONE** |
| 4 | `ref_idx1` | `i32` → **`i8`** | 230,400 → 57,600 | ✅ **DONE** |
| 5 | `mb_mvd` | `[[i16;2];16]` | 230,400 | open — see §4 |
| 6 | `mb_mvd1` (B only) | `[[i16;2];16]` | 230,400 | open, already `is_b`-gated |
| 7 | `bs_frame` | `MbBs` (32 B) | 115,200 | open — see §4 |
| 8 | `mb_nzc` | `[u8;24]` | 86,400 | irreducible (already `u8`) |
| 9–14 | `nnz_y`, `modes_y`, `coded_y`, `inter_y`, `nnz_dbr`, … | `u8`/`bool` | 57,600 each | irreducible at 1 byte |

**After item 3+4: 630,084,000 B — −103,680,000 (−14.1%)**, matching the
prediction exactly.

---

## 2b. Per-REFERENCE-picture — the unit the pool counter cannot see

`as_reference_pooled` runs once per reference picture, outside the per-picture
re-arm. Measured at **202,752,000 B over the same decode — 1,267,200 B per
reference picture.**

| item | B/ref picture | status |
|---|---:|---|
| `mv_y.clone()` | 460,800 | open (same deferral as §3) |
| `mv1.clone()` | 460,800 | open (same deferral as §3) |
| `ref_poc` expansion | 230,400 | ✅ **DONE** — replaced by a 32-entry LUT |
| `ref_idx_y` / `ref_idx1` clones | 115,200 | already narrowed by §2 item 3–4 |

**`ref_poc` was a 230 KB expansion of a 128-byte array.** Every element was
`poc_lut[ref_idx[i]]`, the table was built three lines above it, and `ref_idx`
travels in the same struct. Its only consumer (temporal direct's
`MapColToList0`) reads a single entry, so it does the lookup itself now.
**202,752,000 → 165,888,000 B (−18.2%)**, plus the `RefFrame::clone` copies this
counter does not include. It also retires a hazard the code documented against
itself — `mv` and `ref_poc` were parallel `Vec`s whose lengths nothing tied
together; a fixed-size array cannot desynchronise.

> A counter caveat worth keeping: the first version of this measurement used `*4`
> for `(i32,i32)` entries that are 8 bytes and read 129 MB. Corrected before
> acting on it. A wrong number is worse than no number.

---

## 2c. Ruled out by measurement, not by argument

- **`new_progress_slot` allocates the four motion grids TWICE** (once in
  `LiveMeta`, once on the `RefFrame`), ~2.07 MB zeroed, and
  `as_reference_pooled` then replaces them. It looks like an obvious win.
  Instrumented it: **zero calls.** It is the frame-MT progress slot and never
  fires on the single-threaded path. Optimising it would have been work on a
  function that does not run.
- **The output path.** `into_frame` already MOVES the reconstruction planes when
  there is no cropping — the planes ARE the output. No copy to remove.
- **The padded reference planes.** `row_publish_on()` is a build-time knob, so
  `(py, pu, pv)` are `Vec::new()` in the shipped build.
- **The NAL path, by DENOMINATOR rather than by measurement.** `Decoder::decode`
  carries 24 `memcpy` and `emulation_unprevent` allocates per NAL — but it walks
  the COMPRESSED stream: 954,785 B for 60 frames against 82,944,000 B of output
  pixels. Copying every compressed byte three times is 2.9 MB against 630 MB of
  pool clear. Ruled out with arithmetic; no instrument needed.
- **The span fills** in `bz_flush_slow`/`pz_flush_slow` (24 and 21 `memset`).
  A short runtime-length `fill` is a call and can cost more than the stores it
  saves — but these run 53.2 macroblocks per span, i.e. ~213 blocks per fill,
  well above the ~64-element break-even. Correct as written.

---

## 3. `mv_y` / `mv1` -- the deferral, and what reversed it

The largest item in the inventory, deferred twice. The width was never the
question: a motion vector is quarter-pel and level-bounded (+/-2048 qpel at 5.1,
16x headroom in `i16`), and **`pack_mb` already narrowed them unconditionally**
for the deblock path. The objection was about ARITHMETIC:

> `ref_idx` is COMPARED. A motion vector is COMPUTED WITH -- median prediction,
> mvd addition, scaling for temporal direct. Narrowing the grid pulls `i16` into
> arithmetic that was `i32`, and since this pass also turned `overflow-checks`
> OFF in the release profile, an intermediate that overflows now wraps silently.
> The 68-stream gate may not contain a stream with extreme enough vectors to
> catch it.

**What reversed it: Rust has no implicit numeric coercion.** If the grid is
`(i16, i16)`, every read must widen explicitly or it does not compile -- so the
COMPILER is the proof that "no arithmetic happens at `i16`", which is exactly the
proof the deferral said it needed and could not get from a passing gate.

And it was not a formality. The compiler rejected `dsf * mvc.0` at mb16.rs:6777,
the temporal-direct MV scaling -- the single site the deferral named. Storage is
narrow, all arithmetic still runs at `i32`, only the final store narrows.

Measured on `_dprof/shields__main.264` (1260 frames, 720p Main), before and after
built from the same source in separate worktrees, with **identical call counts on
both sides** (183,330 / 3,360) so it is a pure width change and not changed work:

| counter | before | after | delta |
|---|---:|---:|---:|
| per-picture pool clear | 11,567,052,000 | 8,664,012,000 | **-25.1%** |
| per-reference motion clone | 3,483,648,000 | 1,935,360,000 | **-44.4%** |
| combined | 15,050,700,000 | 10,599,372,000 | **-29.6%** |

Every per-line bucket cross-checks against the frame geometry, which is what
makes the reading trustworthy rather than merely plausible: 57,600 4x4 blocks x
4 B = 225.0 KB/call for `mv_y`; 3,600 MBs x 32 B = 112.5 KB/call for `bs_frame`
and `mb_mvd`; 3,600 x 24 B = 84.4 KB for `mb_nzc`.

> **The instrument had to be fixed before it could report this.** The `REF_BUILD`
> counter hardcoded `len() * size_of::<(i32, i32)>()` and would have reported
> DOUBLE the moment the grids narrowed -- a counter describing the code as it was
> written, not as it is. It now reads `size_of_val` on the slice, so the compiler
> supplies the width and the instrument cannot drift from what it prices. This is
> the second width bug in this counter (the first read `*4` for 8-byte entries);
> both were caught before being acted on, and the fix is structural rather than
> another correction.

---

## 4. What is left, with the test each one has to pass

The remaining items are all **clear-elimination**, and the rule from the h265
decoder applies to each: *a recycled buffer's clear is removable only if you can
NAME who covers each byte and prove the uncovered set is zeroed before anything
reads it.*

- **`bs_frame` (115 KB/picture).** `derive_bs_row` assigns `bs_row[mb_x]` on
  every arm, so coverage looks total — but the `else { continue }` on
  `pk_cur.get(mb_x)` is an uncovered path, and a picture with deblocking
  disabled writes nothing. Needs the coverage flag set AFTER the producing call,
  not before.
- **`mb_mvd` (230 KB/picture).** Only the CABAC context needs it, and only as
  `|mvd|` thresholded at 3 and 33 — so a `u8` cap would be 16 B/MB instead of 64.
  That changes what is stored, so it needs its own gate.
- **The `u8`/`bool` grids** are already one byte per 4×4 block. `coded_y` and
  `inter_y` are one BIT each in principle, but a bitset costs the hot neighbour
  reads more than the clear costs.

A note on the shape of the ceiling: `v.clear(); v.resize(n, val)` always writes
`n` elements, and the crate is `#![forbid(unsafe_code)]`, so there is no
`set_len` escape. **For a `Vec`, narrowing the element is the only lever** —
which is why §2 is a table of element widths rather than of call sites.

---

## 5. Refuted, and why the refutation is the useful part

- **`refill`'s same-length fast path.** The pool hands back the grid it was
  given, so `v.len() == n` on every call after the first picture; `clear()` +
  `resize()` cannot see that and re-enters `extend_with`, while `slice::fill` on
  the same memory is a straight splat. Whole-binary static count moved the right
  way (-22 instructions, -1024 bytes). **Reverted anyway.** The per-symbol census
  showed why: `with_pool` already carried 19 `call memset` before the change, so
  `resize` was ALREADY lowering to a memset -- both arms use the same mechanism.
  What the branch actually bought was 38 memset call sites instead of 19 and
  **+88 instructions in `decode_slice_cabac_inner`**, the hottest function in the
  census. Same mechanism, bigger hot loop, no dynamic case to make.
  *Law: a whole-binary instruction delta is not evidence about a hot path. Read
  the per-symbol census before believing a shape change helped.*

- **The bS row publish (`bs_frame[..].to_vec()` per MB row) and the bS store
  clone.** 2,560 B per row plus an allocation, 45 rows a picture, and a full
  115,200 B `clone` -- a textbook per-row `to_vec` in a loop. Both sit on the
  frame-MT `EdcMsg::Row` path, and EDCSTAT reports `rows=0 jobs=0 needctx=0`:
  **zero calls single-threaded.** Ruled out by the counter, not by argument.

- **`build_ref_list_b`'s three `.clone()`s.** Real (5 allocations where 2 would
  do), but `Ref` is `Arc<RefFrame>` -- 8 bytes -- over a DPB of at most 16, and
  pre-sizing trades allocations for MORE atomic refcount traffic, not less. An
  allocation-count change with no byte case; not taken.

- **`mvdc` / `refc` / `mmvd`, the per-inter-MB neighbour caches.** 150 B and 48 B
  of stack init per inter macroblock, ~540 KB a picture and invisible to the
  runtime counter -- the classic fixed-size stack scratch. Not removable: the
  30-entry cache is written only at the neighbour slots that exist, and the parse
  reads the untouched slots as "unavailable neighbour" context, so the init is
  load-bearing. They are also constant-size, so they lower to inline stores, not
  the counted `call memset`.

- **`MbBs::UNSET` is dead.** It is defined, carries a paragraph of doc explaining
  the bring-up bug it caught, and is **never compared against anywhere** -- the
  refill uses `Default::default()`. Noted rather than deleted: it costs nothing,
  and the doc comment is the record of a real defect.

## 6. The floor I called too early

The section that stood here said the decoder was "mined out at the narrowing
level" and that the only lever left was clear elimination. That was wrong, and
the way it was wrong is the most useful thing in this document.

**It was true of the thing being counted.** Every per-picture grid really is at
one byte per 4x4 block or is a per-MB struct that cannot shrink. What the claim
missed is that the census only ever priced COPIES, and the decoder's largest
remaining waste was not a copy. It was allocation -- and an allocation that is
immediately overwritten is invisible to a copy counter by construction, because
no byte is copied.

Four more wins came out of the ground the "floor" had declared empty:

| # | what | measured |
|---|---|---|
| 7 | the padded-plane pool was never fed | misses 10,080 -> 1,275 |
| 8 | the DPB motion clone allocated instead of recycling | 6,720 big allocs, 1.55 GB |
| 9 | the IDR `refs.clear()` freed planes instead of parking them | misses 1,275 -> 75 |
| 10 | 2 of 5 nores-job sites bypassed their own pool | allocations -87.2% |

## 7. The allocation census, and the class of defect it catches

`rusty_h264-common` carries a counting global allocator under `profile`, with a
size histogram and a recursion-guarded backtrace sampler
(`RS_H264_ALLOC_SITE=<bytes>`). It exists because of a defect class the copy
census cannot see:

> **A recycle pool that stops being fed does not fail. It degrades silently to
> "always allocate".** No correctness gate can catch it -- where a buffer came
> from cannot change decoded output, so byte-identity is green either way -- and
> the code reads correctly at both ends: the pool is defined, filled at one site,
> and drained at another. Only the RATIO of hits to misses shows it, and nothing
> was counting that.

Four pools were checked by hand before the counter existed; three were fine and
one (`plane_pool`) sat at a 100% miss rate. The counter then found two more in
one run, without an audit. **Audit does not scale; a counter does.**

How the last one was localised, because the method transfers:

1. The histogram put 693,653 of 795,183 allocations in one 97-128 B band. A band
   that narrow is one call site, not a pattern.
2. The backtrace sampler bottomed out at `Decoder::decode` -- LTO had inlined the
   entire decode path into it. **A backtrace in a release build names the
   inlining root, not the code.** Re-probed with `CARGO_PROFILE_RELEASE_DEBUG=2
   CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_OPT_LEVEL=1` and got
   mb16.rs:2757.
3. It was `Box::new(pj)` at two of the five sites that construct that job; the
   other three call the pooled `take_nores_job`, and the pool was already being
   refilled at the drain.
4. **The confirmation that made it a diagnosis rather than a guess:** an existing
   work counter, `EDCMIX nores_sent`, read 693,630 against the band's 693,653.
   The counter that names the WORK and the counter that names the COST agreeing
   to 23 is what closes a finding.

## 8. Where the floor actually is

After the four: 19,056 large allocations and 8.78 GB remain, and **18,900 of them
are the reconstruction planes** -- `vec![0; cw * ch]`, moved into the `YuvFrame`
handed to the caller. That is the product, not waste; recycling it needs the
caller to give the frame back, which is an API question rather than an
optimisation.

And its zeroing must stay, for a reason worth recording because it runs opposite
to every instinct this campaign trained:

> **`vec![0; n]` for a buffer the decoder fully overwrites looks like textbook
> waste. It is a security property.** On a malformed or truncated stream the
> decoder may not cover every macroblock, and the zero fill is what makes an
> uncovered region come out black instead of disclosing whatever the allocator
> last held. Removing it converts a decode error into heap disclosure.

The same argument now covers §4's clear elimination, and states it more sharply
than "error-path determinism" did: skipping a per-picture grid clear is not a
correctness risk, it is an information-disclosure one.
