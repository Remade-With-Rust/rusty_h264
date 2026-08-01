# WHYS — decoder performance vs ffmpeg (2026-07-31)

Unknown: *how fast is our H.264 decoder against ffmpeg's native software decoder,
and what owns the difference?*

Run under the six-whys discipline with **depth 6 first**. That was the right call:
D6 found **five** separate instrument defects, three of which would each have
sent the campaign at a phantom target.

## D1 — is the gap real, at matched settings?

The recorded answer was `bench/decode_speedtest.sh`: 202.7 vs 622.9 Mpx/s, "we are
33% of ffmpeg". Re-measured with a sound instrument (below), the honest table is:

| stream (1200 frames, 720p, single core, CPU time) | ours | ffmpeg | gap |
|---|---|---|---|
| synthetic, CAVLC | 162.7 Mpx/s | 599.8 Mpx/s | **3.69×** |
| synthetic, CABAC | 125.9 | 207.6 | **1.65×** |
| real (FourPeople), CAVLC | 199.4 | 667.7 | **3.35×** |
| real (FourPeople), CABAC | 163.8 | 404.5 | **2.47×** |

So: real, and between 1.65× and 3.69× depending on content and entropy coder —
consistently *worse on CAVLC*, which is the opposite of where attention would
naturally go (CABAC is what ships in the real world).

## D6 — the instrument (run FIRST, and it kept paying)

### D6a — the reference's number was noise, and the differential was invalid

`decode_speedtest.sh` times N2 frames and N1 frames and divides the pixel delta by
the time delta, to cancel fixed per-invocation cost. On this box ffmpeg's process
startup is ~0.8–1.0 s and its marginal decode of 180 frames is ~0.6 s, so the
differential subtracts two numbers of the same size. Measured across five rounds it
produced 202, 391, 176, **negative**, and 330 Mpx/s for the same work. The recorded
622.9 Mpx/s was a draw from that distribution, not a measurement.

Fix: make the stream long enough that startup does not need cancelling (1200
frames), and read **CPU time**, not wall.

### D6b — the two arms were not doing the same work

The script times our **CLI**, and `cmd_decode` accumulates every frame into a
`Vec<YuvFrame>`, concatenates all of them into one buffer, and writes it — ~331 MB
of allocate/copy/write at 720p×240. ffmpeg's `-f null` streams and discards. The
differential does *not* cancel this, because it scales with the frame count the
differential is taken over. Measured: CLI 3877 ms vs in-process decode 2408 ms —
**38% of the "decode" time was the output path.**

Fix: `crates/rusty_h264-decoder/examples/decode_bench.rs` decodes access unit by
access unit and drops each picture, which is what the reference does.

### D6c — the noise floor is enormous, and wall clock is unusable

Best-of-5 read 2408 ms; best-of-15 read 907 ms; spread across reps reached **285%**.
The box sat at 100% CPU from unrelated desktop processes for the whole session, and
absolute throughput for one fixed stream drifted **3.5×** between sessions.

Consequences, both load-bearing:
- **CPU time (`TotalProcessorTime`) instead of wall.** Contention inflates wall and
  largely does not inflate CPU: three consecutive readings came back 2328/2328/2328 ms
  where wall swung 2.4–3.7 s.
- **A/B arms must be INTERLEAVED, alternating which runs first.** Running all of
  arm A then all of arm B put machine drift between the blocks and produced
  3.9% / 34.1% / 49.4% for one quantity. Interleaved, the same quantity read
  16.0–20.2% across six rounds.

`cpu/wall = 0.72` also settled a side question: the decoder is genuinely
single-threaded, so a CPU-time comparison against `-threads 1` is like-for-like.

### D6d — three stages were declared but never scoped, and one read an impossible zero

- **`inter-mc` read 0.0 ms / 0 calls on every P-frame stream.** The `InterMc` scope
  sits on `mc_luma`/`mc_chroma`, but the decoder calls `mc_luma_padded`/
  `mc_chroma_padded`. Motion compensation — 6–8% of decode — was hiding inside the
  residue, and the profile as printed said *MC is free*.
- **`DecSetup`** existed in the `Stage` enum and was scoped nowhere. Once wired, the
  per-picture grid allocation (14 frame-sized vectors per frame) measured **1.7–1.9%**
  — the "allocation must be huge" hypothesis, refuted by measurement.
- **`Dequant`** likewise: wired, measured **2.2–3.8%**. Refuted.
- `dump()` prints only stages `0..Total`, so `DecMbP`/`DecMbI`/`DecSetup` and the
  whole `b_mc` decomposition — instrumentation that already existed — never reached
  the screen. `decode_bench` now prints the full table.

### D6e — the profiler's own tax was most of the "residue"

`mgmt/other` read 33–41%, which reads like a large pile of unnamed work. It is
mostly the instrument: the scope guard is an rdtsc pair, and this workload runs
~19 M entropy + ~22 M dequant + ~12 M MC scopes. Measured profiled-vs-unprofiled on
the same binary and stream: **tax 1.32× (CAVLC) / 1.43× (CABAC)**, which accounts for
roughly three-quarters of the residue.

The rule that falls out: **a stage's share is only trustworthy when its call count is
small.** `deblock`, `dpb-clone`, `dec-setup` and `finalize` have 1200 calls each
(one per frame) and carry no meaningful self-tax; every per-MB stage's share is
inflated and must be priced by ablation on the uninstrumented binary instead.

## D2/D3 — which stage owns the gap? None of them.

Deblocking is the largest trustworthy named stage, and it was confirmed by two
independent instruments that do not share a failure mode: the scope profiler
(1200 calls, tax-free) said 16.1–19.1%, and `RFF_ABL_DEBLOCK=1` ablation on the
**uninstrumented** binary said 18.5% median (range 16.0–20.2 over six interleaved
rounds).

Then the decisive move — **benchmark the reference against itself**. ffmpeg's own
`-skip_loop_filter all` gives its deblock share on the identical stream:

| stream | our deblock share | ffmpeg deblock share |
|---|---|---|
| synthetic CAVLC | 18.5% | 20.7% |
| real CAVLC | ~19–24% | 32.8% |

**ffmpeg spends a LARGER fraction of its time deblocking than we do.** Normalising to
absolute CPU on the clean synthetic measurements (ours 6797 ms, ffmpeg 1844 ms):

- deblock: 1257 ms vs 382 ms → **3.29×**
- everything else: 5540 ms vs 1462 ms → **3.79×**

The gap is **uniform**. There is no dominant stage to attack: our decoder is roughly
3.5× slower nearly everywhere on CAVLC, and deblocking — already on openh264's asm
kernels with a tiled bS derivation — is if anything our *relatively* strongest area.

(One caveat stated rather than hidden: the first flag I tried, `-skip_loopfilter all`,
does not exist in this ffmpeg build. It errored out instantly and the arm "decoded"
1200 frames in 31 ms, yielding a 98.9% deblock share. The frame-count check caught it.
An impossible number is the instrument asking for help — and comparing a *count*,
not a time, is what makes it answer.)

## What this rules in and out

- **Out:** deblocking as the lever (we are proportionally ahead of ffmpeg there);
  per-frame allocation (1.9%); dequantization (2.2–3.8%); motion compensation as a
  hidden giant (6–8%, now visible and unremarkable).
- **In:** a uniform ~3.5× says the deficit is *structural per-macroblock cost*, not
  any one kernel — consistent with CABAC's much smaller gap (1.65×), where the
  serial entropy decode dominates and per-MB structure matters proportionally less.
  The next honest step is a **sampling** profiler, not more scopes: scope-based
  instrumentation has hit its own noise floor here (D6e), and the residue it reports
  is mostly itself.

## Deliverables from this descent

- `examples/decode_bench.rs` — output-free, in-process, prints the frame COUNT (the
  work-parity check), the rep spread (the noise floor), and the full stage table
  including INFO stages.
- `Decoder::split_access_units` made public — `decode()` takes one access unit, so a
  caller that wants to drop pictures as they arrive needs it.
- `InterMc` scoped on the padded MC path; `DecSetup` and `Dequant` scoped at all.
- `RFF_ABL_DEBLOCK=1` — ablation knob for tax-free pricing.
- `bench/decode_speedtest.sh` rewritten around CPU time, long streams and frame-count
  parity, so it stops emitting the phantom figure.

---

# PART 2 — the x264-stream benchmark (2026-07-31)

## D1 (redone) — we were measuring the wrong bitstreams

Every number in Part 1 was taken on **our own encoder's** output. That is a narrow,
self-selected slice of H.264: mostly 16x16 partitions, one reference, I16x16-heavy
intra. Real content is x264's, and x264 at medium/slower emits sub-8x8 partitions,
multi-ref, B-pyramid, the 8x8 transform, weighted prediction and i4x4/i8x8 intra.

Re-measured on x264 streams — 1800 frames of real 720p (shields / in_to_tree /
stockholm), pinned to one core at High priority, CPU time, arms alternated, frame
counts verified equal, streams long enough that per-invocation overhead is <1% of
both arms:

| x264 config | ours | ffmpeg | gap | paired |
|---|---|---|---|---|
| baseline `--no-cabac --preset veryfast` | 30.3 s | 4.9 s | **6.12x** | 9/9, z=3.00 |
| main `--preset medium` | 49.5 s | 8.2 s | **6.03x** | 9/9, z=3.00 |
| high `--preset slower` | 61.9 s | 10.3 s | **6.05x** | 9/9, z=3.00 |

**The gap on real bitstreams is 6.05x, not the 1.65-3.69x Part 1 reported.** The
instrument in Part 1 was sound; the *corpus* was not. Bitstream provenance is a
content axis, and `codec-analyzer`'s "profile on REAL content" law applies to it
exactly as it applies to synthetic pixels.

Note how flat it is: 6.12 / 6.03 / 6.05 across both entropy coders and three very
different tool sets. That uniformity is the same signature Part 1 found (deblock
3.29x, everything-else 3.79x) — a structural per-macroblock cost, not one kernel.

## A conformance defect the benchmark found

Before any of the above could be timed, 2 of 3 clips at `--preset slower` decoded
DIFFERENTLY from ffmpeg. Root cause: spatial direct read the co-located block at
its own 4x4 coords, ignoring `direct_8x8_inference_flag`, while the temporal path
mapped the 8x8 corner correctly. Invisible on every stream the conformance gate
had ever produced, because x264's default partition set has an 8x8 minimum, so all
four 4x4s of a co-located 8x8 carry identical motion. `--partitions all` makes them
differ. Fixed; gate gained crossed `sub8`/`sub8pyr` arms (120 -> 160 configs), both
verified to go red without the fix.

**Benchmarking a decoder on a corpus it has never seen is a conformance test.**

## D3 (x264 corpus) — the ranking, and what the bricks moved

Profile of the SAME decoder on the SAME three clips, our-encoder streams vs x264
streams, was the measurement that unlocked this. The MC census (a facility that
existed but was wired only into the ENCODER's `mc_luma`) settled it in one table:

| corpus | MC cycle distribution |
|---|---|
| our own streams | **100.0% 16x16 FULL-PEL** |
| x264 streams | 16x16 quarter 69.2%, 16x8/8x16 quarter 9.6%, 8x8 quarter 5.8% |

Our `fast` preset is integer-pel only, so our own bitstreams contain **no sub-pel
motion at all** — every decode benchmark ever run here skipped the entire
interpolation path. That is the whole 3x-vs-6x discrepancy.

Three byte-identical bricks followed, all SIBLING-PATH PARITY gaps (the encoder had
the fix, the decoder did not):

1. wire the vendored `PixelAvgWidthEq16/8/4` asm into the quarter-pel average
   (it was in the objects, never declared) — 16x16 quarter 1432 -> 762 cyc/call
2. const-width re-stride of MC output, 4 sites — pred-buf 319 -> 50 ms (-84%)
3. const-width full-pel MC copy — MC cycles 689M -> 492M (-29%)

CAVLC 6.12x -> ~4.7x; CABAC 6.03x -> 5.67x; profiled TOTAL 4086 -> 2226 ms.

### D4 — deblocking is 31% of decode and only 5.6% of it is filtering

`RFF_ABL_DBKERNEL=1` no-ops every deblock KERNEL while leaving the boundary-strength
derivation and per-edge glue running; paired with `RFF_ABL_DEBLOCK` (skips the whole
stage) it splits the cost with no profiler scope in either — at per-edge granularity
the scope tax would exceed the thing being measured.

| | ms | % of decode |
|---|---|---|
| deblock stage total | 8094 | **31.1%** |
| SIMD kernels | 1453 | 5.6% |
| **bS derivation + glue** | **6641** | **25.5%** |

ffmpeg's ENTIRE deblock stage is 1891 ms on the identical stream — so **our
derivation alone costs 3.5x ffmpeg's whole deblock**, while the two stages take
almost exactly the same SHARE of each decoder (ours 29.9%, ffmpeg 28.9%). Equal
shares hid a 4.3x absolute gap; only the ablation split showed it is all in the
derivation.

This is the largest single named target found: 25.5% of decode, in code that is
pure per-edge bookkeeping. Prior art says where it belongs — x264 and ffmpeg derive
boundary strength DURING macroblock decode, from state already in registers, rather
than re-deriving it in a separate pass that re-gathers every neighbour.

Also verified along the way (the "prove the kernel ran" law): the asm really is
active in the benchmarked binary — scalar 69.4 s vs asm 27.0 s, 2.51x, 5/5 pairs.
A uniform gap across asm-backed stages is exactly what a silently-scalar build
would look like, so it had to be ruled out.

### D5 — inside the derivation: what is and is not the cost

The derivation loop is already well tuned in the places that are easy to suspect:
the per-MB TILE gather is the shipped default (`RS_H264_DEBLOCK_BRANCHY=1` restores
the old per-edge arm), and alpha/beta/tc0 are computed AFTER the all-zero early-out,
so edges that filter nothing cost no thresholds.

**REFUTED (measured, reverted): short-circuiting INTRA macroblocks.** An intra
macroblock's strengths are constants (4 on its own MB edges, 3 internal) and need no
block reads, yet the loop still gathered a 36-block tile for them. Wiring
`derive_mb_kind(Intra)` in and skipping the gather was byte-identical and gated clean
(139/0, 160/0) — and measured 4.906x against 4.782x, i.e. nothing. Cause: on this
corpus intra macroblocks are ~2-5% of the total (x264's default keyint means 3 IDRs
per 180 frames, plus scattered intra in P), so the prize was never more than ~1%.
Reverted rather than left as a hot-path branch that buys nothing. It would pay on
intra-heavy content; it does not pay here, and the corpus is the one that counts.

**Still open, and still the ranked target: the per-MB tile gather for INTER
macroblocks.** That is where the 25.5% lives. The path is `BS_PRECOMP` — derive each
macroblock's strengths at DECODE time, when the kind is known for free and the state
is already hot, and hand them to `filter_frame` via `BlockInfo.bs` (a consumption
path the encoder already exercises and gates).

The recorded refutation of that idea does NOT transfer, and the reason it does not is
written into the refutation itself: it was measured on the ENCODER, at CIF, and its
stated cause was "the block grids were never cold (~90 KB at CIF, L2-resident)" plus
"the encode loop's contended working set makes it cost MORE there". At 720p the grids
are ~9x larger and the decoder's macroblock loop is far leaner than the encoder's
(no ME, no RD trials). Both premises fail here. A refutation expires when its
baseline moves — but it must be RE-MEASURED, not assumed inverted.

Cost of doing it properly: the decoder has ~12 macroblock exit points (skip, intra
16x16/4x4/8x8, I_PCM, P 16x16/16x8/8x16, P_8x8, B direct/16x16/8x8, x2 for CAVLC and
CABAC) and `MbBs::UNSET` exists precisely because missing ONE silently disables
deblocking for that macroblock. It also wants a per-block reference-POC grid so the
per-frame `ref_id` Vec (57,600 entries, rebuilt every frame) disappears with it.
That is a bounded but real refactor, not a micro-brick.
