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
| `bench/copy_census.sh` | every real `call memcpy/memset/memmove` in the emitted assembly, per symbol — including ones the optimiser INTRODUCED, and excluding the ones LLVM inlined away | how often each executes |
| `cpystat` (`feature = "profile"`) | bytes moved at runtime, bucketed by caller line via `#[track_caller]` | copies below the instrumented sites |

Neither alone is enough. The static census ranks `decode_stream_threaded_sink`
first with 80 copies; the runtime counters show it executes **zero** times in a
single-threaded decode.

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

## 3. `mv_y` / `mv1` — priced at 276 MB, and DEFERRED on purpose

The largest remaining item, and the width is provably sufficient: a motion vector
is quarter-pel and level-bounded (±2048 qpel at 5.1, so `i16` has 16× headroom),
and **`pack_mb` already narrows them to `i16` unconditionally** for the deblock
path — which is the proof the storage width was never needed.

It was implemented and then **reverted**, for a reason that does not apply to the
reference grids:

> `ref_idx` is COMPARED. A motion vector is COMPUTED WITH — median prediction,
> mvd addition, scaling for temporal direct. Narrowing the grid pulls `i16` into
> arithmetic that was `i32`, and since this pass also turned `overflow-checks`
> OFF in the release profile, an intermediate that overflows now wraps silently.
> The 68-stream gate may not contain a stream with extreme enough vectors to
> catch it.

The safe form is to keep the grid narrow and widen at **every** read so all
arithmetic stays `i32`, narrowing only at the final store. That is a provable
change, but "no arithmetic happens at `i16`" is a claim that needs more than a
successful compile across the ~42 sites it touches. Worth doing deliberately;
not worth doing quickly.

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
