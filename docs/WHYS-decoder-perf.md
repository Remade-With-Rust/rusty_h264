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

---

# PART 3 — the descent redone from D6 (2026-08-01)

Unknown, restated by the user: *"our decoder is garbage compared to ffmpeg — 5x
slower — so we obviously got it wrong in the past."*

Run as a fresh descent. The prior parts' **method** was retained (CPU time not
wall, ABBA interleaving, x264-stream corpus, frame-count parity) because each
rule was itself paid for by a measured instrument defect. The prior parts'
**verdicts** were treated as unproven and re-derived.

That distinction was the right one, and it had a specific target. Part 1's
headline verdict — *"the gap is uniform, no stage dominates"* — was read off a
scope profiler whose own tax Part 1 measured at 1.32-1.43x with a 33-41% unnamed
residue, and Part 1 itself wrote that the honest next instrument was a SAMPLING
profiler. That instrument was never run. A verdict resting on an instrument its
own author declared exhausted is exactly what D6 exists to overturn.

## D6 (first, again) — two more instrument defects, and the first is worth ~2x

### D6f — OUR ARM DECODED EVERY STREAM TWICE

`examples/decode_bench.rs` ran a second full decode pass to feed the profile
dump, **unconditionally** — including in `--features asm` builds, where
`prof::dump()` is a no-op but the decode still happens.

Every ffmpeg comparison measures whole-PROCESS CPU time (`pinvs.ps1`,
`TotalProcessorTime`). So our arm was charged **3600 frames against ffmpeg's
1800**.

Measured on `long_cavlc.264` (1800 frames, 720p):

| | CPU |
|---|---|
| `decode_bench` PROCESS | 24,344 ms |
| its own internal timer, one decode | 14,690 ms |
| ffmpeg PROCESS, same stream | 5,391 ms |

The frame-count parity check could not see it. `frames=` is printed from the
TIMED pass alone, so it truthfully reported 1800 while the process decoded 3600.
**The §4 work-parity check failed inside itself** — the count was right and the
work was not. A count only closes §4 if it counts the work the CLOCK is charged
for; ours counted a subset by construction.

Fixed: the profile pass is now `#[cfg(feature = "profile")]`.

### D6g — the harness invocation was silently dropping most pairs

`powershell -NoProfile -File bench/pinvs.ps1 ... -BArgs ...,"-f","null","-"`:
in `-File` mode PowerShell parses the remaining tokens as a command line, so
ffmpeg's trailing `-` binds as a PARAMETER NAME. The call either dies with
*"the value of argument name is not valid"*, or launches a mangled arm that
exits in milliseconds and is recorded as a **dropped pair**.

The first re-run showed it in the open — `4 of 5 pairs were DROPPED` and median
ratios of exactly **0.333 / 0.667 / 0.500**, i.e. 1/3, 2/3 and 1/2 of a
15.6 ms scheduler tick. `pinvs.ps1`'s own guards (too-short workload, dropped-pair
count) fired correctly and are why this was caught in one run rather than
believed. Both `bench/decode_x264_speedtest.sh` and `bench/decode_speedtest.sh`
carried the defect; both now use `-Command` with an explicit `@(...)` array.

Two lessons, both already in `codec-measurement` and both re-earned here:
an impossible number is the instrument asking for help (§7), and a harness must
ENFORCE the discipline rather than be told it (§13) — the guard that refuses to
report below timer resolution is what made this a five-minute diagnosis.

### D6h — the null arm, so the floor is known rather than assumed

`pinvs.ps1` run with the SAME binary in both arms, 5 pairs, long_cavlc:
ratios 0.999 / 1.003 / 0.998 / 1.003 / 1.031, **median 1.003x, z=0.45**.

The harness resolution floor is ~0.3% median and 3.1% at the worst pair — far
tighter than the +0.2% to +10.8% a previous null arm on this box produced, which
is what pinning plus CPU time (rather than wall) buys. Every verdict in Part 3
clears it by a wide margin: the 2.62x gap and the 28.3% deblock share are orders
of magnitude outside it.

The one figure that does NOT clear it cleanly is `RFF_ABL_DBKERNEL` at 7.9%
with a 1.3-15.5% spread. That spread is ~5x the null floor, so it is not harness
noise — the two arms genuinely differ in cache and branch behaviour. The effect
is real (7/7 directional, z=2.65) but its SIZE is not well resolved, and the
derivation figure derived from it inherits that.

## D1 (redone) — the gap is 2.6-2.9x, not 6x

1800 frames of real 720p (shields / in_to_tree / stockholm), x264 at three tool
tiers, `--qp 26`. Pinned to core 2 at High priority, CPU time, arms ABBA
alternated, ONE decode per arm, frame counts verified equal (1800/1800), all
nine streams verified byte-identical to ffmpeg before timing.

| x264 config | recorded (Part 2) | **re-measured** | paired |
|---|---|---|---|
| baseline `--no-cabac --preset veryfast` | 6.12x -> ~4.7x | **2.62x** | 5/5, z=2.24 |
| main `--preset medium` | 6.03x -> 5.67x | **2.88x** | 5/5, z=2.24 |
| high `--preset slower` | 6.05x -> 5.65x | **2.85x** | 5/5, z=2.24 |

Median CPU, cavlc: ours 12,688 ms vs ffmpeg 5,219 ms. `cpu/wall = 0.82 < 1`
confirms our decoder is single-threaded, so the comparison against `-threads 1`
remains like-for-like.

**The decoder is roughly 2.6-2.9x behind ffmpeg, not the 4.7-6.1x on record.**
Nothing about the decoder changed between those two tables; the arm stopped
doing twice the work.

## What the correction invalidates, and what SURVIVES it

This matters more than the headline, because it decides which of the prior
findings have to be re-earned:

- **SURVIVES: every share measured by same-binary ablation or A/B.** The double
  decode inflated both arms of any same-binary comparison equally, so it cancels
  in the ratio. The deblock share, the `RFF_ABL_DBKERNEL` split, the
  scalar-vs-asm 2.51x (which also still proves the asm path is genuinely active),
  and the three MC bricks' relative gains are all unaffected.
- **INVALIDATED: every cross-implementation ABSOLUTE comparison**, because ours
  was doubled and ffmpeg's was not. Specifically, Part 2's *"our bS derivation
  alone (6641 ms) costs 3.5x ffmpeg's ENTIRE deblock stage (1891 ms)"* was built
  on our inflated absolute. Halving ours puts it near ~1.7x — still a gap, but
  not the dramatic one that made bS derivation look like the campaign's obvious
  target.
- **INVALIDATED: the standing 6.05x**, and with it the framing that the decoder
  is 5-6x off. It is not, and it never was during Part 2.

## D2/D4 (redone) — the gap is UNIFORM at ~2.4-2.5x; deblock is NOT the outlier

All ablation on the UNINSTRUMENTED binary via the new `bench/pinabl.ps1` (same
binary, one env knob, pinned, CPU time, ABBA, paired z). Frame count verified
1800 under every knob — a knob must not change the WORK COUNT.

| arm (long_cavlc, 1800f 720p) | full | ablated | share | paired |
|---|---|---|---|---|
| ours, `RFF_ABL_DEBLOCK` (whole stage) | 13,594 ms | 9,828 ms | **28.3%** (24.6-28.9) | 7/7, z=2.65 |
| ours, `RFF_ABL_DBKERNEL` (SIMD kernels only) | 14,047 ms | 13,297 ms | **7.9%** (1.3-15.5) | 7/7, z=2.65 |
| **ffmpeg, `-skip_loop_filter all`** | 6,047 ms | 4,375 ms | **27.7%** | 7/7, z=2.65 |

`-skip_loop_filter all` was verified to still decode **1800 frames** before use —
Part 1 was burned by a misspelled variant that errored out instantly and yielded a
98.9% share.

Normalising each decoder's own measured share onto its D1 paired median CPU
(shares are drift-robust; absolutes from different runs are not):

| | ours | ffmpeg | ratio |
|---|---|---|---|
| deblock | 3,591 ms | 1,443 ms | **2.49x** |
| everything else | 9,097 ms | 3,776 ms | **2.41x** |
| total | 12,688 ms | 5,219 ms | 2.62x |

**The gap is uniform.** Deblock costs us the same FRACTION it costs ffmpeg
(28.3% vs 27.7%) and the same MULTIPLE as everything else (2.49x vs 2.41x).
There is no dominant stage to attack.

And the specific target Part 2 named collapses. bS derivation + glue is
28.3% - 7.9% = **20.4%** of decode = ~2,588 ms, against ffmpeg's ENTIRE deblock
stage at 1,443 ms — **1.79x, not the 3.5x on record.** The 3.5x was our doubled
absolute measured against ffmpeg's honest one. Stated as a prize: perfectly
eliminating bS derivation moves 2.62x to ~2.09x, and BS_PRECOMP does not
eliminate it, it RELOCATES it into the macroblock loop (where the one prior
measurement of that move, on the encoder, found it cost MORE). Prune on the
arithmetic before building: a 2x-faster derivation buys ~10% of decode.

Honest uncertainty: the `RFF_ABL_DBKERNEL` share is poorly resolved (1.3-15.5%
across seven pairs, median 7.9%) because it is small relative to this box's
noise. The 20.4% derivation figure inherits that spread. The 28.3% whole-stage
share and the 27.7% ffmpeg share are tight and can be relied on.

## Where this leaves the campaign

Part 1's verdict — *"a uniform gap, structural per-macroblock cost, no single
kernel"* — was RIGHT, and it survives re-derivation with a corrected instrument.
Part 2's verdict — *"bS derivation is the largest single named target"* — was an
artifact of comparing our doubled absolute against ffmpeg's honest one, and does
not survive.

So the ranked next step is still the one Part 1 wrote down and nobody ran: a
SAMPLING profiler. Scope instrumentation has hit its own noise floor (tax
1.32-1.43x), and ablation can only price stages someone has already thought to
put a knob on — it cannot discover an unranked cost. `samply` is installed but
needs `xperf` from the Windows Performance Toolkit (ADK), which is not present;
that install is the cheapest remaining move and it is a prerequisite, not a
nice-to-have.

Second, cheaper probe worth running first: the vendored kernels are openh264's
**SSE2/SSSE3** era (`WelsSampleSatd*_sse2`, `Deblock*_ssse3`, `PixelAvg*_mmx`),
while ffmpeg's h264 kernels reach **AVX2**. A uniform ~2.4x across pixel work is
consistent with a 128-bit-vs-256-bit ISA width gap — but it is NOT consistent
with the gap being equally uniform across the scalar bookkeeping (bS derivation)
that uses no SIMD at all. That tension is the next thing worth resolving, and it
is a `codec-asm-kernel` sse2-vs-avx2 audit, not a guess.

## Deliverables from Part 3

- `examples/decode_bench.rs` — the profile pass is now `#[cfg(feature = "profile")]`.
  This is the ~2x correction. Do not un-gate it.
- `bench/decode_x264_speedtest.sh`, `bench/decode_speedtest.sh` — `powershell -File`
  replaced with `-Command` + explicit `@(...)`, with the reason recorded inline.
- `bench/pinabl.ps1` — NEW. Paired, pinned, ABBA, CPU-time ablation A/B of one
  binary under one env knob, with the too-short-workload and dropped-pair guards.
- Standing numbers to quote from now on: **2.62x / 2.88x / 2.85x** (x264
  baseline-CAVLC / main / high, 1800f 720p, 5 pairs each, z=2.24).

---

# PART 4 — the rebuild: attacking bS derivation (2026-08-01)

## D5 (redone) — the derivation is 25x off x264, and two instruments agree

| instrument | bS derivation cost |
|---|---|
| in-context ablation (20.4% of decode ÷ 6.48M MB) | ~399 ns/MB |
| isolated `deblock_anatomy`, `inter-bs0` arm, 720p | 359-412 ns/MB |
| **x264 `deblock_strength_avx2`, recorded in that bench's own header** | **~15 ns/MB** |

The in-context and isolated numbers agreeing is the §11 cross-check passing — and
it makes the ~25x gap on this one function the most specific finding of the whole
campaign. It is also the counter-example to "we are behind because their asm is
wider": our deblock KERNELS are only 7.9% of decode, so the vendored SSE2 is not
what is costing us. The scalar bookkeeping around it is.

## The MB-kind census — count before building

`derive_mb_kind` (kind-aware: 0 block loads for Intra, 9 for Skip, 16 for
InterUniform, 24 for Inter) already exists and is exercised by the ENCODER. The
DECODER calls the blind `derive_mb`, which gathers 24 blocks for every macroblock.
Whether that matters depends entirely on the kind mix, so it was counted, not
guessed (`--features profile`, deterministic counter, zero cost in the
benchmarked binary):

| corpus | Intra | Skip | InterUniform | Inter (needs full gather) | gather removable |
|---|---|---|---|---|---|
| baseline/CAVLC | 6.9% | 36.4% | 47.3% | **9.4%** | 45.5% |
| main/CABAC | 3.0% | 65.0% | 20.8% | **11.2%** | 50.6% |
| high | 3.0% | 57.8% | 24.6% | **14.6%** | 47.3% |

6.48M macroblocks per corpus. **Only 9-15% of macroblocks need the work we do on
all of them.** The gather runs first and unconditionally, so on 85-91% of
macroblocks we pay 24 block loads across 5-7 frame-wide arrays to discover we did
not need them.

## Brick 1 — fuse the two predicate walks

Found while reading the path, not predicted: `filter_frame` walked the
macroblock's own 16 blocks TWICE per macroblock.

- pass A (`filter_frame`): `flat_inter` = `b0.inter && ∀(inter && !nz && same_motion)`
- pass B (`derive_mb_bs`): `uniform_motion` = `!cur_intra && ∀(inter && same_motion)`

They are the same walk — `flat_inter == uniform_motion && ∀(!nz)` — and pass B ran
BEFORE `derive_mb_bs`'s `if flat_inter { return }` early-out, so on every Skip
macroblock the second walk's result was computed and immediately discarded. The
census puts Skip at **36.4% / 65.0% / 57.8%** of the corpus, so that dead walk ran
on most macroblocks, each one 16 blocks × a 6-field `same_motion` compare.

`scan_uniform_flat` now returns both predicates from one walk. Equivalence is
structural: when the walk breaks early both predicates are false, exactly as both
original `.all()` chains would be.

Gates:
- `scan_uniform_flat == scan_two_pass` asserted per macroblock over the
  pseudo-random grid in `derive_matches_per_edge` — the original is KEPT as the
  oracle, not deleted.
- 73/73 unit tests pass.
- **Byte-identical decode vs ffmpeg on all 9 x264 streams, on BOTH arms** of the
  `RS_H264_BS_TWOPASS` switch.
- Both arms live in one binary so the A/B alternates under one thermal state.

### Brick 1 VERDICT — kept on a COUNTER, because no clock on this box could see it

Three timing attempts, in order, and the instrument failed each time:

| instrument | result |
|---|---|
| whole-decode paired A/B, 7 pairs | median -1.8%, **z = -1.13** — not a verdict |
| same, 15 pairs, after fixing the rig | pairs swung **+15.9% to -16.8%**; sign disagreed with run 1 |
| `deblock_anatomy` best-of-30 ns/MB | **within-arm** spread reached 45% (`skip`: 154.6 then 224.6 for IDENTICAL code) |

A rig defect was found and fixed along the way: `scan_predicates` resolved the arm
with an atomic load + branch PER MACROBLOCK — 6.48M times, in both arms. That is
measurement overhead added in order to take a 2% measurement. Hoisted to once per
frame. It did not rescue the measurement.

The null arm, re-run at the end, explains all three: **median 1.006 (z = -0.38) but
a per-pair range of 0.949-1.070** — ±7% on identical binaries, where the same test
read 1.003 / worst-pair 3.1% earlier in the same session. **The noise floor is not
stationary; re-run it per session, not per machine.** The median estimator survives
(±0.6%), so whole-decode A/B still works — but it needs N>=20 for an effect this
size, which is more machine time than the effect is worth.

So the verdict came from the instrument that drift cannot touch — a deterministic
counter of the work actually removed:

| arm | predicate-walk block visits | per MB |
|---|---|---|
| TWOPASS (old) | 143,745,760 | 22.18 |
| **FUSED (new)** | **90,385,830** | **13.95** |

**37.1% of the predicate walk removed, byte-identical output.** Sizing it:
53.4M visits x ~6 `same_motion` compares ~= **0.4-0.7% of decode** — genuinely
below what any clock here can resolve, and correctly so.

KEPT, and labelled honestly: kept on the counter plus the byte-identical gate, with
the timing effect recorded as BELOW INSTRUMENT RESOLUTION. Not "a small win" — an
unmeasurable one whose work-removal is proven. `RS_H264_BS_TWOPASS=1` restores the
old walk; both arms stay in the binary.

**The transferable rule:** for a sub-1% brick, the counter is the PRIMARY evidence
and the clock is confirmatory. Reverse that order and you will either discard real
work-removal because a noisy box hid it, or bank a regression because a noisy box
flattered it. Batch such bricks behind one switch and let the BATCH carry the
timing verdict, where 5-15% is resolvable.

## Brick 2 — the decoder supplies the derivation class

The census said 85-91% of macroblocks do not need the 24-block gather. `derive_mb_kind`
(Intra: 0 block loads, Skip: 9, InterUniform: 16 nnz bytes, Inter: 24) already existed
and was exercised by the ENCODER; the decoder called the blind `derive_mb` for every
macroblock despite knowing the class for free the moment it parsed `mb_type`.

### The design decision that makes this safe

`BlockInfo` gained `kind: &[u8]`, and **UNSET falls back to the blind path**. A missed
producer site therefore costs SPEED, never correctness. This deliberately inverts the
risk in the earlier note ("`MbBs::UNSET` exists precisely because missing ONE silently
disables deblocking"): there is no silent-wrongness mode left, only a silent-slowness
one, which the `BLK_LOADS` counter makes visible.

The gather is skipped BY CONSTRUCTION rather than by an early-out inside `gather_tile`,
because building the 5x5 `Tile` at all is an 800-byte default-initialisation before 24
of its 25 entries are overwritten.

### The producer is CONSERVATIVE, and that is not optional

Only classes uniform BY SYNTAX are written:

| syntax | class | why |
|---|---|---|
| `P_Skip` | `Skip` | no coefficients, one (ref, mv) for all 16 blocks |
| `P_L0_16x16` (`mb_type 0`) | `InterUniform` | ONE partition, so no internal edge can reach strength 1 |
| everything else | UNSET -> blind | |

**`B_Skip` and `B_Direct_16x16` are deliberately NOT `Skip`.** Their motion is
direct-derived and may differ per 4x4 sub-block, so their internal edges can legally
reach strength 1. Classifying them by syntax name would have changed strengths silently.
`P_16x8`/`P_8x16`/`P_8x8` are two-or-more partitions with independent motion: also UNSET.
Intra is left UNSET too — the intra short-circuit was REFUTED by measurement in D5 (2-5%
of macroblocks, measured nothing), and that refutation still holds.

`decode_p_skip` is the single producer site covering BOTH entropy coders, which is why
the biggest class needed one line rather than eight.

### The oracle caught a real defect on its first run

`RS_H264_VERIFY_MBKIND=1` derives every classified macroblock BOTH ways and asserts they
agree — on the strengths AND on `flat_inter`. First run, all three CAVLC streams:

```
MB (1,0) kind InterUniform: flat_inter false but blind derived true
```

A single-partition inter macroblock with NO coefficients is flat as well as uniform —
`flat_inter` reduces to "no block has nnz" once motion is uniform. Hardcoding `false`
was PIXEL-IDENTICAL (the internal strengths are 0 either way, so filtering does nothing)
and would have passed the byte-identical corpus gate — while making the consuming loops
walk the internal edge groups instead of skipping them, throwing away part of the win.
**A byte-identical gate cannot see a lost optimisation; only the oracle could.** Fixed by
deriving flat from the 16 nnz bytes `derive_mb_kind` already reads for that class.

### Gates

- `RS_H264_VERIFY_MBKIND=1` over all 9 x264 streams — every classified macroblock agrees
  with the blind derivation, strengths and `flat_inter`.
- Byte-identical vs ffmpeg on all 9 streams, on BOTH arms of `RS_H264_NO_MBKIND`.
- Full workspace test suite green (encoder shares `BlockInfo`).

### Counter — the primary evidence (codec-measurement §15)

| | BLIND | KIND | removed |
|---|---|---|---|
| `Blk::load` gather loads | 154,620,000 | **98,374,800** | **-36.4%** (56.2M) |
| per macroblock | 23.86 | **15.18** | |
| predicate-walk visits | 90,385,830 | **4,998,950** | **-94.5%** (85.4M) |

Against the pre-Brick-1 baseline the predicate walk is down **143.7M -> 5.0M, -96.5%**.

The 36.4% gather reduction sits below the census's 45.5% theoretical ceiling for this
corpus, and the gap is exactly the conservatism: B_Skip/B_Direct and the multi-partition
P modes stay blind. That is the price of not shipping a silent strength change, and it
is the right trade.

Sizing it: 56.2M block loads x ~5 array reads each, plus 85.4M predicate visits x ~6
compares, lands at roughly **2-2.5% of decode** — more than Brick 1's ~0.5%, still under
what a single timing run resolves on this box today.

### Brick 2 v1 was a REGRESSION — and the counter had said "win"

First timing run, kind-aware vs blind, 4 pairs before it was stopped:

```
+12.4%   +7.4%   +17.0%   +1.2%      (positive = kind path SLOWER)
```

Four pairs, one sign, magnitudes far outside the oscillating pattern the noise had been
producing. That is a regression, not drift — and it happened while the counter reported
**36.4% fewer block loads and 94.5% fewer predicate visits.**

Both causes were additions I had made on the supposedly-cheaper path:

1. **An 800-byte `Tile` zero-init per classified macroblock.**
   `let tile = if blind_tile { gather_tile(..) } else { Default::default() };`
   On the fast path that materialises the full 5x5 `Blk` tile and zeroes it — so the
   brick removed a gather and added **~4 GB of memset** across the run. Fixed by making
   it `Option<Tile>`, so the fast path never constructs one.
2. **A packed-to-unpacked expansion**: routing through `derive_mb_kind`'s `MbBs` return
   and expanding it added 32 `u8`->`i32` stores per classified macroblock that the blind
   path never pays. ~0.5% by arithmetic — real, but NOT the dominant cause, and left as a
   documented seam rather than pre-emptively "fixed".

**This is the counterpart to §15 and it deserves equal weight: a counter proves work was
REMOVED; it does not prove time was SAVED.** The removed work here (24 sequential,
prefetch-friendly block loads) was cheaper per unit than the work I added to avoid it —
the same shape as this workspace's recorded case where an 88.9%-redundant operation
regressed 6-7% when eliminated. The discipline that catches it is unchanged: counter for
"did the work go away", clock for "did that help", and NEITHER substitutes for the other.

It also justifies the arm switch existing at all. `RS_H264_NO_MBKIND=1` made the
regression measurable in one process against its own baseline under one thermal state;
comparing two builds would have buried a 10% effect in this box's 20% build-to-build
drift.

### Brick 2 v2 — direction reversed, verdict still short of the bar

After removing the per-macroblock `Tile` zero-init, 9 pairs on `long_cavlc`:

```
-1.8  +10.7  +11.0  -12.3  -8.2  -3.1  0.0  -0.7  -5.1     (negative = kind path FASTER)
median -1.8%   range -12.3%..+11.0%   kind faster in 7/9   z = -1.67
```

Two readings, both worth recording:

- **The v1 regression is gone.** v1 was 4/4 SLOWER at +1.2..+17.0%; v2 is 7/9 FASTER.
  The 800-byte per-macroblock `Tile` memset was the whole of it.
- **z = -1.67 does NOT clear |z| > 2, so this is still NOT PROVEN.** Note the shape
  though: pairs 2-3 carry both positive outliers and every pair from 4 on is <= 0,
  which is what a machine settling mid-run looks like rather than a null effect. That
  is a reason to re-run at N >= 20, not a reason to promote a 9-pair result.

Status: byte-identical and oracle-verified; work-removal proven by counter (-36.4%
gather loads, -94.5% predicate visits); timing LEANS WIN but unproven. Kept behind
`RS_H264_NO_MBKIND` with the verdict pending a quieter box or N >= 20.

### Brick 2 FINAL VERDICT (N=21) — NOT PROVEN, and the extra samples argued AGAINST the lean

```
median -1.3%   range -29.5%..+21.7%   kind faster in 13/21   z = -1.09
```

Compare against the 9-pair run:

| N | median | win rate | z |
|---|---|---|---|
| 9 | -1.8% | 7/9 = 78% | **-1.67** |
| **21** | **-1.3%** | **13/21 = 62%** | **-1.09** |

**z got WEAKER as N grew.** For a real effect at a fixed win rate, z scales with sqrt(N)
— it should have RISEN past 2. Falling from 1.67 to 1.09 while the win rate regressed
toward 50% is the signature of a NULL effect, and it retrospectively convicts the 7/9
lean as noise. Refusing to promote the 9-pair result was correct; promoting it would have
banked a win that does not exist.

Final status of Brick 2:
- correctness PROVEN three ways (both-ways oracle, byte-identical vs ffmpeg on 9 streams
  x 2 arms, full workspace suite)
- work removal PROVEN by counter (-36.4% gather loads, -94.5% predicate visits)
- **time saved: NOT PROVEN at N=21.** Not a regression either (v1 was, unambiguously, at
  4/4); this is a null result.

Kept in tree behind `RS_H264_NO_MBKIND` and labelled UNPROVEN. It is byte-identical, it
demonstrably does less work, and there is no evidence it costs anything — but "does less
work" is not "is faster", which is the whole lesson of v1.

**Stopping rule reached.** Three bricks' worth of 20-minute runs have now failed to
resolve ~2% effects on this box, and the null arm degraded from 3.1% to 7.0% worst-pair
within the session. Continuing to spend machine time at this effect size is not
disciplined measurement, it is hoping. The ranked target — bS derivation at ~400 ns/MB
against x264's ~15 ns/MB — is a 25x effect that even this machine could resolve, and
NEITHER brick has touched it: both nibbled at the GATHER around the derivation, not the
derivation itself. That is where the next work belongs, along with extending the
decomposition to the still-unranked ~72% of decode.

### DECISION (owner's call, 2026-08-01): both bricks DEFAULT ON

`RS_H264_BS_TWOPASS` and `RS_H264_NO_MBKIND` are opt-OUTs, so the fused walk and the
kind-aware dispatch are already what ships. Confirmed rather than changed.

Rationale, stated plainly so a later reader does not mistake this for a measured win:
**less work is less work.** Both bricks are byte-identical, oracle-verified, and remove
provable work (-37.1% predicate visits; -36.4% gather loads, -94.5% predicate visits).
Neither is a measured speedup — Brick 2 finished at z=-1.09, a NULL — but neither shows
any evidence of costing time, and the medians lean favourable across every run.

The standing caveat, which the owner has: v1 proved less work CAN be slower. The
protection is that both arms remain in the binary, so if a future quiet-box measurement
convicts either brick it is one env var to revert, not a rewrite.

## Ranking the unmeasured ~72% — the stage table, read under the tax rule

Instrument already in the tree and not looked at this session. `--features profile`,
x264 CAVLC corpus, 1800 frames. Profiled total 24,871 ms vs ~15,300 ms unprofiled =
**tax 1.63x**, so per-MB stage shares are INFLATED and only low-call-count rows can be
read directly (`codec-measurement` §6).

| stage | profiled share | calls | trustworthy? |
|---|---|---|---|
| deblock | 19.4% | 1,800 | YES — and ablation on the clean binary says 28.3% |
| entropy/cavlc | 12.4% | 39,806,410 | no — inflated |
| inter-mc | 11.7% | 21,193,620 | no — inflated |
| dequant | 4.5% | 48,503,610 | no — nearly all tax |
| mv+grid | 3.3% | 4,037,930 | no |
| reconstruct | 2.4% | 16,743,360 | no |
| dpb-clone | 2.0% | 1,800 | YES |
| pred-buf copy | 1.9% | 13,680,660 | no |
| scatter(store) | 1.7% | 16,743,360 | no |
| syntax-parse | 1.4% | 13,347,020 | no |
| neighbors | 0.8% | 5,243,500 | no |
| skip-recon | 0.7% | 3,642,080 | no |
| **intra-pred** | **0.5%** | 2,679,030 | no — but too small to matter either way |
| finalize | 0.1% | 1,800 | YES |
| residue | ~37% | — | mostly the instrument's own rdtsc pairs |

Conclusions that survive the tax:

- **`intra-pred` is 0.5%.** This independently re-confirms the D5 refutation of the intra
  short-circuit, and is why Brick 2 deliberately left Intra UNCLASSIFIED rather than
  "while we're here". A refutation that keeps being re-confirmed by a second instrument
  is one to stop revisiting.
- **The two unranked stages in deblock's league are entropy/CAVLC and inter-MC.** Nothing
  else named comes close. `dequant`'s 4.5% across 48.5M calls is the textbook shape of a
  stage whose share IS its own instrumentation.
- ffmpeg's own IDCT share, measured against itself for reference, is only ~4.0%
  (median ratio 1.042, but z=1.00 — weakly resolved, quote with that caveat).

### Why this reorders the plan

Deblock is now the best-understood stage in the decoder and has yielded two byte-identical
bricks, NEITHER of which measured. Meanwhile entropy/CAVLC and inter-MC are each
plausibly deblock-sized and have had NO ablation knob built. Continuing to work deblock
because it is the stage we happen to have instruments for is the streetlight error.

Next, in order:
1. `RFF_ABL_MC` — price inter-MC by ablation on the clean binary (frame-count preserving:
   prediction becomes garbage, the parse and residual path are untouched). MC is the one
   of the two that CAN be ablated; entropy cannot be skipped without breaking the parse.
2. The derivation itself — ~400 ns/MB vs x264's ~15 ns/MB. Both bricks so far attacked
   the GATHER around it; the 25x is in `derive_mb_bs`/`bs_inter`, which is scalar per-edge
   work where x264 runs `deblock_strength_avx2` over packed per-MB data. That is a
   `codec-vectorize-kernel` / `codec-asm-kernel` job, and its effect size is large enough
   that even this box could resolve it — unlike the ~2% bricks that just failed to.

## D6 (again) — the degraded noise floor had a NAMED CAUSE, not thermal drift

`RFF_ABL_MC` was built (one early-return inside each of the two padded MC primitives,
so all ~13 decoder call sites are covered and every caller, call COUNT and surrounding
glue is untouched; frame parity 1800/1800 verified). Its first measurement:

```
median 3.9%   range -13.6% .. +24.0%   6/9   z = 1.00
```

Two pairs are NEGATIVE — skipping motion compensation cannot make decode slower. §7:
an impossible number is the instrument asking for help. **DISCARDED, not quoted.**

The determination, and it is not the timer:

| PID | CPU consumed | started | job |
|---|---|---|---|
| 40600 | 3,453 s | **09:46** | `FFai/.tools-bench/omni_split.py mobiledet-crnn layout` |
| 44816 | 3,476 s | **10:05** | `FFai/corpora/refs/unlimited_ocr_ref.py --batch` |
| 1268 | 3,797 s | **10:05** | `FFai/corpora/refs/unlimited_ocr_ref.py --batch` |

System load 75%. These are a DIFFERENT PROJECT's benchmarks, and the timeline matches
this session's measurements exactly:

- before 09:46 — null arm median 1.003 / worst-pair **3.1%**; deblock ablation **7/7, z=2.65**
- after 10:05 — null arm worst-pair **7.0%**; bricks swinging ±12-17%; MC impossible

**Correction to what Part 4 recorded earlier.** "The noise floor is not stationary" was
the right operational lesson but the wrong mechanism: this was not drift, it was three
CPU-heavy foreign processes arriving mid-session. The sharper rule is that when the floor
moves, GO FIND THE PROCESS — `Get-CimInstance Win32_Process` with CPU and CreationDate
named the cause in one command and dated it against the exact measurement that went bad.

**Why CPU time did not save us** (§2): affinity RESTRICTS us, it does not RESERVE the
core, and CPU time removes the PREEMPTION term but not the CONTENTION term. Three
CPU-heavy neighbours sharing L3 and SMT siblings make our code genuinely execute slower,
and `TotalProcessorTime` reports that truthfully. The harness is sound; the machine is
not. Nothing to fix in the instrument.

Standing: MC is UNPRICED. The knob exists and is verified; the measurement needs a clean
window. Do not quote the 3.9%.

## Step 0 gate for vectorizing the derivation — PASSES formally, but the named reason REROUTES it

`codec-vectorize-kernel` Step 0 requires three things. All measured, no timer needed —
which is why this was the right work to do while the box was contended.

**(1) codec-eliminate-redundancy ran first.** Bricks 1 and 2 were both redundancy moves on
this exact code, and the derivation is still the ranked target.

**(2) The scalar code does NOT auto-vectorize** — from `--emit asm`, counting packed
integer ops. The crate as a whole contains 2,875 SIMD register uses, so LLVM is willing
here; it simply refuses on these functions:

| function | asm lines | SIMD regs | packed-int ops |
|---|---|---|---|
| `gather_tile` | 760 | 83 | **0** (moves/spills only) |
| `derive_mb_bs` | 616 | 9 | **0** |
| `bs1_tile` | 279 | 3 | **0** |
| `scan_uniform_flat` | 105 | **0** | **0** |

Note the trap this avoids: `gather_tile` shows 83 xmm references. Counting SIMD REGISTERS
would have called it "vectorized"; counting packed OPS shows all 83 are 16-byte moves and
spills. Count the ops, not the registers.

**(3) The reason is NAMED — and it is a GATHER, which is exactly what vetoes the brick.**

- `gather_tile` reads `inter[]`, `nnz[]`, `mv[]`, `ref_id[]` (+`mv1`/`ref_id1` on B) at
  `(by0+r)*w4 + bx0+c` — 24 blocks x 4-6 SEPARATE frame-wide arrays. Non-contiguous,
  multi-provenance: a textbook gather.
- `Blk` is AoS (~30 bytes of mixed bool/i32); comparing 16 of them is strided access
  inside that layout.
- `bs1_tile` and `scan_uniform_flat` carry data-dependent early-outs — control flow, not
  data flow.

### Why this REROUTES rather than proceeds

The law recorded in this very skill (rav1e, 2026-07-16): **"a gather in the data path
vetoes the 'it will vectorize' justification — check the loads first."** Its rs_h264
entries (2026-07-17, 2026-07-18) record THREE consecutive flat results for wider SIMD on
loads-gated kernels in this codebase.

And the 2026-07-18 entry already diagnosed this exact confusion for motion estimation:
*"x264's advantage is NOT AVX2 width on single-block SAD — it's `sad_x4`'s STRUCTURE...
an algorithmic amortization, not a vectorization."*

**The identical distinction applies to `deblock_strength_avx2`.** x264 can vectorize bS
derivation because of its LAYOUT — nnz as a packed per-MB bitmask, motion vectors in
per-MB contiguous arrays — so the kernel is a few vector loads plus compares. Ours must
first gather 24 blocks across 4-6 frame-wide arrays. An AVX2 twin of the CURRENT structure
leaves ~120 scalar gather loads in place and vectorizes only the compare arithmetic
downstream of them. **The 25x is mostly LAYOUT; the SIMD is downstream of it.**

**Nothing to bind, either.** openh264's vendored asm ships only deblock FILTERING kernels
(`DeblockLumaLt4V_ssse3`, `DeblockChroma*`, `DeblockLumaTranspose*`) — grep found **no
boundary-strength kernel at all**, because openh264 derives bS in C. Unlike the 2026-07-17
win, where AVX2 twins were already compiled and merely unbound, here a kernel would be
written from scratch.

### The correct order, and the honest caveat

1. **Layout first**: make each macroblock's bS inputs contiguous — a packed per-MB nnz
   bitmask plus per-MB contiguous ref/mv — produced AT DECODE TIME, where the decoder
   already holds that state (the same insight Brick 2 used for `MbKind`).
2. **Then** the SIMD twin becomes a few vector loads + compares, i.e. actually x264-shaped.

Caveat stated up front: the frame-size sweep says the derivation is NOT cache-bound
(QCIF 338 -> CIF 306 -> 720p 359 ns/MB — only +17% as the working set crosses L2, against
the 2-3x a cache-bound signature would show). So the layout change must be justified as
ENABLING VECTORIZATION, not as fixing cache misses — a different argument for the same
remedy, and one to gate on its own measurement rather than assume.

**Step 0 verdict: do NOT write the AVX2 twin yet.** It would be the fourth flat
loads-gated SIMD brick in this codebase. The prerequisite is the layout.

## D5 (mechanism) — THE UNIFORM CAUSE: the safe core ships with ZERO AVX2

A uniform ~2.4x gap across every stage cannot be explained by any one kernel — a uniform
effect needs a uniform cause. Found one, and it needs no timer to establish.

There is no `.cargo/config.toml` and no `RUSTFLAGS`, so the whole safe core compiles for
**baseline x86-64 = SSE2**. Counting emitted instructions, same source, same crate:

| metric | baseline (what ships) | `-C target-cpu=native` |
|---|---|---|
| **ymm (AVX2, 256-bit)** | **0** | **1,463** |
| xmm (SSE, 128-bit) | 4,145 | 2,235 |
| VEX-encoded instructions | **0** | 2,284 |
| total asm lines | 33,696 | 32,847 |

**Zero AVX2 anywhere in the safe core.** ~1,463 sites where LLVM would use 256-bit
registers run 128-bit today, and 2,284 instructions would take the cheaper three-operand
VEX encoding (which also removes register-copy pressure, independent of width).

This is the AAC lesson from `codec-vectorize-kernel`, applying at whole-codec scale:
*"a PORTABLE binary compiles for baseline x86-64 (SSE2), and LLVM won't emit AVX2 without
`-C target-feature=+avx2`."* Every auto-vectorized loop we have — in entropy, MC, recon,
deblock glue, everywhere — is half-width against ffmpeg's hand-written AVX2. That is
precisely the signature of a gap that is the same size in every stage.

It also reframes the whole campaign: Bricks 1 and 2 chased ~2% each inside ONE stage,
while a systemic ISA factor sits under ALL of them.

Correctness: the `target-cpu=native` build decodes **byte-identical to ffmpeg on all 9
x264 streams** — expected for integer autovectorization, but verified, not assumed.
Built into a separate `target-native/` dir so the manifest and the normal build cache stay
untouched (`codec-measurement`: prefer env overrides that leave the manifest alone).

### The unresolved part: PORTABILITY vs the project's core promise

`-C target-cpu=native` is NOT shippable as a default — the binary SIGILLs on any CPU
without the host's ISA, and this crate is published. The portable route is
`#[target_feature(enable="avx2")]` + runtime `is_x86_feature_detected!` + scalar
fallback — but that requires `unsafe`, and the codec core is `#![forbid(unsafe_code)]`,
which is the project's headline guarantee.

So the options are a real design decision, not a brick:
1. an opt-in cargo feature / documented `RUSTFLAGS` for users building for their own
   hardware (keeps `forbid(unsafe)`, keeps portability by default, captures nothing for
   users who never set it);
2. move the widened work into `rusty_h264-accel` (already the one `unsafe` crate, already
   has runtime dispatch) — but that means hand-writing kernels rather than letting LLVM
   autovectorize, which is the expensive path Step 0 just argued against;
3. function multiversioning so the hot functions get AVX2 clones with runtime dispatch.

Measurement of the prize is pending a clean box; the ISA COUNT above is the part that is
already certain.

### AVX2 prize: UNMEASURED (not disproven) — the box defeated a third measurement

```
median 1.022x   5/9   z = 0.33   per-pair range 0.772 .. 1.352
```

Read the SHAPE, not the median: pairs 1-5 leaned AVX2 (to 1.352), pairs 6-9 leaned SSE2,
and `sse2_base`'s OWN absolute time fell 15,438 -> 11,219 ms across the run. The reference
arm moved 27% while we were measuring — §12's "never headline a ratio whose denominator
drifts more than your improvement" applies exactly. System load was 96% with 8 foreign
python processes.

**State this precisely, because the two claims are different:**
- **PROVEN, deterministically:** the safe core emits **zero AVX2** (0 ymm vs 1,463; 0 VEX
  vs 2,284). That is an instruction count, not a timing, and no amount of machine noise
  touches it.
- **UNMEASURED:** what closing that gap is WORTH. z=0.33 is not evidence of a win and not
  evidence against one — the instrument could not resolve it. Do not quote 1.022x, and do
  not quote the 1.352 pair either.

Temper the expectation while it is unmeasured: a large fraction of decode is inherently
SERIAL (CAVLC/CABAC bit-by-bit parsing was 12.4% profiled with 39.8M calls) and cannot
use wider vectors at all. The 1,463 ymm sites are spread across the whole common crate,
hot and cold. The honest prior is "somewhere between a few percent and the low tens of
percent", and only a clean box can narrow it.

**Third measurement defeated today. The stopping rule stands** — no more timing runs
until the box is quiet, regardless of how promising the lead looks. The correct next
action is a clean window, not another 20-minute run.

## Scalar-over-packed stage — BUILT, gated, opt-in (`RS_H264_BS_PACKED=1`)

The SIMD substrate, landed as scalar first so the layout is proven before any
intrinsics exist.

- **`MbPack`** — packed per-macroblock bS inputs, raster order (`k = row*4 + col`):
  `nnz_mask: u16` (the derivation only ever asks "is this block coded"), a per-MB
  `inter` flag, `mv: [(i16,i16); 16]`, `ref_id: [i32; 16]`. `ref_id` stays i32 on
  purpose — the decoder supplies a POC, which must remain comparable across slices
  whose reference lists differ; a `ref_idx` would compare equal across slices that
  mean different pictures.
- **`pack_frame`** — one streaming pass over the frame arrays instead of 3600
  scattered per-macroblock gathers. Returns `None` on B frames (List-1 is not modelled),
  which then keep the blind path.
- **`derive_mb_packed`** — the byte-identical twin of `derive_mb_bs`, producing
  `flat_inter` AND the 32 strengths in ONE traversal. `flat_inter` becomes
  `uniform && nnz_mask == 0`, i.e. a single register test replacing a 16-block walk.

### Gates
- `packed_matches_tile` unit oracle (74/74) — strengths and `flat_inter`, over a grid
  seeded to include uniform, flat, and fully-varied macroblocks.
- `RS_H264_VERIFY_PACKED=1` — UNMASKED runtime oracle on real bitstreams, comparing
  all 32 strengths per macroblock against the tile derivation. Clean.
- **Byte-identical vs ffmpeg on all 9 x264 streams** with the path enabled.
- Coverage measured: **1,143,320 macroblocks** take the packed path on `long_cavlc`.

### Two process failures worth recording, both mine

**1. A silent no-op replacement produced a FALSE PASS.** The first attempt inserted the
`verify_packed` function but its *call site* replacement silently matched nothing (the
anchor text had drifted to `tile.as_ref().unwrap()`). The oracle therefore never ran and
reported success by not existing, while the corpus diverged. **A verification that
silently fails to run is worse than no verification** — it converts "untested" into
"tested and fine". Every scripted edit now asserts its anchor count; the two that were
asserted succeeded, the one that was not is the one that broke.

**2. A MASKED oracle passed while the corpus diverged.** The unit oracle compared only
the edges the consuming loops read (skipping `flat` and t8-skipped groups) — reasonable
in isolation, and it hid the real fault. The runtime oracle now compares **all 32
strengths unmasked**. Mask the comparison and you are testing your model of the
consumer, not the derivation.

The actual bug both failures concealed was mundane: with the packed branch missing,
enabling the flag only made `blind_tile` false, so `bs_v`/`bs_h` stayed ZERO for every
unclassified macroblock — no deblocking at all, diverging from frame 0 byte 3.

### Standing

DEFAULT OFF. This is the enabler, not the win: the ceiling probe already showed the
layout saves only ~19 ns/MB of a ~400 ns/MB derivation. The value is that the arithmetic
is now reachable by SIMD — `nnz` as shift-and-or on one 16-bit mask, motion as i16
subtract/abs/compare across 16 lanes — over records that are 2 cache lines instead of
16-20. That kernel is the next brick, and it belongs in `rusty_h264-accel`.

## THE KERNEL — `bs_motion_masks_avx2`, landed and gated

The motion half of the derivation, vectorised. Interface chosen so the kernel is a
pure data-parallel function with a trivially testable signature:

```text
(mvx[16], mvy[16], ref_id[16], NO_REF) -> (left_mask: u16, up_mask: u16)
  left bit k = block k differs in motion from block k-1
  up   bit k = block k differs in motion from block k-4
  differs(a,b) = ref[a]!=ref[b] || (ref[a]!=NO_REF && (|dmvx|>=4 || |dmvy|>=4))
```

The 24 internal edges stop being 24 branchy `bs_inter` calls and become bit tests
against these masks OR'd with `nnz_mask` — the form x264's kernel works in.

**Layout change that made it cheap:** `MbPack` now stores `mvx[16]` / `mvy[16]` as
SEPARATE planes rather than interleaved `(x,y)` pairs. Interleaved would force a
pairwise lane reduction to combine the x and y results; split planes let the two
compares live in separate registers and OR whole-register. Each plane is exactly one
256-bit load.

**The lane-boundary trick, which is load-bearing.** The `left` comparison uses a
WITHIN-128-bit-lane byte shift (`bslli_epi128`), which corrupts the lane at each
128-bit boundary — k=0,8 for i16 and k=0,4,8,12 for i32. Every corrupted position has
`k % 4 == 0`, and those are exactly the macroblock-edge blocks derived separately
against the neighbouring record. The cheap shift is therefore correct where it matters
and garbage only where the result is discarded. Only `up` genuinely crosses lanes and
pays for `permute2x128` + `alignr`.

Both masks clear their don't-care bits (`& 0xEEEE`, `& 0xFFF0`) so the twins are
compared on the FULL u16 — a lane-boundary mistake cannot hide behind "that bit is
unused anyway".

### The gate caught a real bug, and it was an idiom trap

First run: `case 0: SIMD (left=0xa2ca up=0x8a20) != scalar (left=0xa2ee up=0xdb60)`.

Cause: I used the `|a-b| = or(subs(a,b), subs(b,a))` idiom recorded in
`codec-vectorize-kernel`'s ledger — but that idiom requires **UNSIGNED** saturating
subtract, where the wrong direction clamps to zero. Motion vectors are **SIGNED**, so
`subs_epi16` leaves the wrong direction negative and the OR yields garbage. Replaced
with `abs_epi16(sub_epi16(..))`, which also matches the scalar twin's overflow
behaviour exactly (both wrap in i16; both map `i16::MIN` to itself).

**A recorded idiom is not a portable one — check its signedness/range preconditions
against YOUR data before reusing it.** The `*_matches_scalar` test existing BEFORE the
kernel was trusted is the only reason this cost minutes instead of a corpus bisect.

### Gates
- `bs_motion_masks_simd_matches_scalar` — 4000 cases, full-u16 equality, deliberately
  covering NO_REF blocks, identical refs, motion exactly at the `|d| == 4` threshold,
  and large opposite-signed vectors where a saturating subtract misbehaves.
- `packed_matches_tile` unit oracle, and the UNMASKED runtime oracle on real streams.
- **Byte-identical vs ffmpeg on all 9 x264 streams with the kernel live.**
- Scalar twin remains the default on non-AVX2 CPUs and the permanent oracle.

Standing: opt-in (`RS_H264_BS_PACKED=1`), correctness proven, **speed unmeasured** —
the box is still contended. This is the first brick in the campaign whose predicted
effect is large enough that even a loaded box should resolve it, so it is the first
thing to measure when the machine is quiet.

## COVERAGE WIDENING — and the campaign's first |z| > 2

The first packed+kernel A/B came back z=0.30 (null). The diagnosis was not "noisy box"
but **underpowered by construction**: the packed path covered 17.6% of macroblocks, so
even a perfect kernel could only move ~3% of decode against a ~3% floor.

`pack_frame` bailed on any frame carrying List-1, so EVERY macroblock of EVERY B frame
took the blind path — and x264's presets use B-frames heavily. Fixed by extending
`MbPack` with List-1 planes (`ref1`, `mvx1`, `mvy1`, plus an `l1_used` mask) and
implementing `pk_differs` as the EXACT twin of `bs1_tile`, including the two-slot
set-matching rule (which is order-independent: a pair matching after a SWAP is not
"different motion" — a single-list grid cannot test that at all, so the unit oracle was
extended to run every macroblock through both a single-list and a two-list frame).

Coverage, measured:

| corpus | before | after |
|---|---|---|
| cavlc (`--profile baseline`, no B-frames) | 17.6% | 17.6% (unchanged) |
| **main (CABAC, B-heavy)** | low | **95.5%** (6,190,820 / 6,480,000) |

cavlc did not move because baseline profile FORBIDS B-frames — its cap is Brick 2's
kind path taking the cheap macroblocks, not the B gap. Diagnosing that first would have
saved measuring the wrong corpus.

### The verdict, main corpus, null arm run in the same session

```
null arm (identical binary)  median +0.7%   0.954..1.021   4/5, z = 1.34
packed + AVX2 kernel         median +1.7%  -3.5%..+4.2%    8/9, z = 2.33
```

**First result in the campaign to clear |z| > 2.** Read it with two caveats, both
material:

1. **Subtract the null arm.** The harness showed a 0.7% median bias and 4/5
   directionality on IDENTICAL binaries, so the honest net effect is nearer **~1%**,
   not 1.7%. A null arm that is itself 4/5 directional is a caution, not a rubber stamp.
2. **z=2.33 at N=9 clears the bar without much margin.** Confirm at N >= 20 on a quiet
   box before making it default-on.

Note also the per-pair decay (3.5 -> 4.2 -> 2.7 -> 1.7 -> 0.8 -> 0.1) as the machine
warmed: the SIGN was unanimous 6/6 through that stretch while the MAGNITUDE collapsed,
which is why the win-rate carries the verdict and the median does not.

### Still open on the kernel itself

The AVX2 twin implements the SINGLE-LIST rule only, so it runs where `l1_used == 0`.
Bi-predicted B macroblocks get the packed LAYOUT and the scalar two-list rule but not
the kernel. So the 95.5% is packed-path coverage, NOT kernel coverage — the measured
+1.7% is mostly the layout and the branch-free mask formulation, with the kernel firing
on a subset. Extending the kernel to two lists is the next increment, and its prize is
whatever fraction of that 95.5% is bi-predicted.

## COUNTING THE PRIZE KILLED ONE BRICK AND FOUND A BETTER ONE

Before building the two-list kernel, its population was counted (`--features profile`,
deterministic, main corpus):

| | count | share |
|---|---|---|
| packed macroblocks | 6,190,820 | — |
| **reach the mask derivation at all** | **726,540** | **11.7% of packed** |
| ...served by the AVX2 single-list kernel | 523,450 | 72% of those |
| ...would need a TWO-LIST kernel | 203,090 | **3.1% of ALL macroblocks** |

3.1% of macroblocks x the motion-mask share of a derivation that is itself ~20% of
decode = **~0.3% of decode**, an order of magnitude below the floor — for the hardest
SIMD in the area (order-independent two-slot set matching). **PRUNED, unbuilt.**

The denominator was the real find: **only 11.7% of packed macroblocks reach the mask
derivation at all**; the rest are intra or uniform-motion. So the mask kernel — single-
or two-list — can never touch more than ~12% of macroblocks, which explains why the
earlier +1.7% came mostly from the layout rather than the kernel.

Re-counting in elementary compares redirected the work entirely:

| | compares |
|---|---|
| **uniform check (EVERY packed macroblock)** | **557,173,800** |
| mask derivation (11.7% of MBs) | 104,621,760 |
| ...two-list portion — the pruned brick | 58,489,920 |

**The uniform check is 9.5x the pruned brick's work** — and worse for the scalar, because
`.all()` SHORT-CIRCUITS: non-uniform macroblocks bail after a block or two, while the
~5.3M UNIFORM ones (Skip plus single-partition inter) walk all 15 comparisons. The
population paying full scalar price is exactly the one a broadcast-compare eliminates.

`mb_uniform_avx2` broadcasts lane 0 of each of six planes and compares all 16 lanes at
once: 2 i16 registers + 2 narrowed i32 predicate pairs, AND-ed, one `movemask == -1`.

### Gates
- `mb_uniform_simd_matches_scalar` — 6000 cases, built to straddle the decision:
  genuinely uniform macroblocks, ones differing in exactly ONE lane of ONE plane (what a
  broadcast-compare gets wrong if a plane is dropped), and ones differing only on List-1.
- 76/76 unit, 19/19 workspace suites, **byte-identical vs ffmpeg on all 9 streams**.
- Scalar twin remains the oracle and the non-AVX2 path.

### Result — two independent runs, main corpus, null arm alongside

| run | median | paired |
|---|---|---|
| null arm, clean box (identical binary) | **1.000** | 2/7, z = -1.13 |
| packed layout only (earlier) | +1.7% | 8/9, **z = 2.33** |
| **packed + BOTH kernels (clean box)** | **+6.7%** | 11/15, z = 1.81 |

Read honestly: the second run's **z = 1.81 does NOT clear |z| > 2** on its own — one
pair came in at -52.9% (full 13,078 vs ablated 20,000 ms), plainly a machine artifact
rather than the code. But the median is robust to that outlier, the null arm has ZERO
median bias, and **two independent runs agree in direction** with the second showing a
much larger median (+6.7% vs +1.7%) — consistent with the uniform kernel stacking on top
of the packed layout.

Best estimate: **a mid-single-digit percent on B-heavy (main/high) content**, still
opt-in behind `RS_H264_BS_PACKED=1`. One more confirming run on a quiet box should
settle whether it goes default-on.

**The transferable point is the ordering.** Counting the prize before building cost about
fifteen minutes; it killed a brick worth ~0.3% that would have taken a day of the most
error-prone SIMD in this area, and redirected the effort to one worth 9.5x as much and a
fraction of the risk. The count did not merely size the brick — it changed which brick.

## DEFAULTED (2026-08-02) — the packed bS path is now the shipped path

The deciding run cleared the bar:

| run (main corpus, 1800f 720p, pinned, CPU time, ABBA) | median | paired |
|---|---|---|
| packed layout alone | +1.7% | 8/9, **z = 2.33** |
| + both AVX2 kernels | +6.7% | 11/15, z = 1.81 |
| **+ both AVX2 kernels (deciding)** | **+3.3%** | **12/15, z = 2.32** |
| null arm, same sessions | 1.000 / 1.039 | z = -1.13 / 0.38 |

The two full-stack runs pool to **23/30, z = 2.92**.

Read the shape honestly: single-run medians ranged **1.7-6.7%** because this box drifts,
so the WIN RATE carries the verdict and the median is the effect-SIZE estimate, not the
proof. Best statement: **a few percent on B-heavy content**, not "6.7%".

`RS_H264_BS_PACKED` was inverted to an explicit opt-OUT rather than left to flag
polarity: an absent variable now means the fast path, and only the literal `"0"` restores
the blind gather. That is a decision recorded in code, not an accident of defaults.

Gates re-run on the SHIPPED configuration (no env vars set at all):
- 19/19 workspace test suites
- **byte-identical vs ffmpeg on all 9 x264 streams**
- scalar twins retained as oracles and as the non-AVX2 path

### Note on what is default and what that cost

Bricks 1 and 2 were defaulted ON earlier on a "less work is less work" judgement while
measuring NOTHING (Brick 1 unmeasurable at ~0.5%; Brick 2 a null at z=-1.09). The packed
path — the only change in this campaign with a cleared statistical verdict — was the one
left opt-in until now. That ordering was backwards, and it is worth remembering: the
brick with evidence should be the one that ships first, not last.

## STANDING BENCHMARK (2026-08-02) — side-by-side vs ffmpeg, shipped default

Everything landed and default-on. 1800 frames of real 720p (shields / in_to_tree /
stockholm), x264-encoded, pinned to one core at High priority, CPU time, ABBA-alternated,
9 pairs per tier, frame counts equal, all streams byte-identical to ffmpeg before timing.

| x264 tool tier | rusty_h264 | ffmpeg native `h264` | gap | paired | at session start |
|---|---|---|---|---|---|
| baseline / CAVLC `veryfast` | 11,063 ms — **150 Mpx/s** | 5,281 ms — 314 Mpx/s | **2.34x** | 9/9, z=3.00 | 2.62x |
| main / CABAC `medium` | 15,469 ms — **107 Mpx/s** | 5,750 ms — 289 Mpx/s | **2.70x** | 9/9, z=3.00 | 2.88x |
| high `slower` | 19,625 ms — **85 Mpx/s** | 6,938 ms — 239 Mpx/s | **2.49x** | 9/9, z=3.00 | 2.85x |

Caveat on the last column: those start-of-session figures were taken hours earlier under a
different machine load. A cross-session delta is weaker evidence than the same-session
paired ratio — the ratio itself is the trustworthy number, the "improvement" is
directional. Every tier moved the right way (-11% / -6% / -12%) and the shipped bricks
were each gated independently, so the direction is not in doubt; the magnitude is soft.

### READMEs corrected

The published decode figure was **"145 Mpx/s vs ffmpeg ~590 · 0.25x"** in three places
(root, `rusty_h264` facade, `rusty_h264-decoder`). It came from the differential harness
Part 1 refuted — five runs of identical work gave 202 / 391 / 176 / **negative** / 330
Mpx/s. All three now carry the paired table above plus an explicit note saying what the
old number was and why it was wrong, rather than quietly swapping it.

`remade_ffmpeg_rs`'s codec table row for h264 was one line ("with SIMD asm, default") and
now carries the conformance status and the measured gap, matching the depth of its vp9 and
jpeg rows.

---

# PART 5 — closing the open threads, and what the ranking rules out (2026-08-05)

## THREAD 1 — the AVX2 build flag: ≈ +3%, suggestive, NOT a verdict

Baseline (no `target-cpu`) vs `x86-64-v3`, same source, on the quietest window of the
whole campaign (null arm median 0.979, decode 10.5 s — the fastest absolute times yet):

```
pairs 1-6 (clean):  1.024  1.029  0.992  1.049  1.031  1.028   -> 5/6, ~+3%, z ~= 1.63
pairs 7-10:         0.903  1.119  0.881  0.718                 -> box degraded, discarded
```

**z ≈ 1.63 is under the bar**, and only 6 clean pairs. Call it *≈3%, suggestive*. That
matches the tempered prior given when the instruction counts landed: 1,463 ymm sites
looked dramatic, but a large share of decode is inherently SERIAL (bit-by-bit entropy
parsing) and cannot use wider vectors at all.

**Self-inflicted:** pairs 7-10 were corrupted because the ADK installer was running
CONCURRENTLY with the measurement it was meant to enable. Exactly the contention failure
this document has been recording since Part 3, walked into anyway. Sequence installs
before or after a measurement window, never during.

## THREAD 2 — inter-MC: RESOLVED, and it is NOT a target

```
RFF_ABL_MC = -2.1% of decode   range -5.2%..+8.9%   ablated faster in 4/13, z = -1.39
```

**Skipping motion compensation entirely does not make decode faster** — in 9 of 13 pairs
the ablated arm was SLOWER. Two readings, both useful: the already-bound
`McHorVer20/02_avx2` kernels are doing their job, and the ablation's `out.fill(128)`
costs about what the interpolation it replaces costs (MC's output write dominates its
arithmetic). Either way the conclusion holds: **there is no SIMD win available in MC.**
That prunes a whole planned direction.

## THREAD 3 — ranking the remainder WITHOUT a sampling profiler

`xperf`/WPT could not be installed: `Error 0x80070642: Failed to elevate` — the ADK
bootstrapper needs a UAC prompt and this shell is non-interactive, so the prompt is
auto-cancelled. Three attempts (winget x2, adksetup direct) all failed identically. To
unblock, run from an ELEVATED terminal:

```
%LOCALAPPDATA%\Temp\WinGet\Microsoft.WindowsADK.10.1.28000.1\adksetup.exe ^
  /quiet /norestart /features OptionId.WindowsPerformanceToolkit
```

Substitute: two new ablation knobs, `RFF_ABL_INTRA` and `RFF_ABL_RECON` (both gate the
scalar AND the accel path; frame count verified 1800 under every knob on a fresh binary).

| stage | share | evidence |
|---|---|---|
| deblock (whole stage) | **28.3%** | 7/7, z = 2.65 (Part 3) |
| ...SIMD kernels | 7.9% | already AVX2 |
| ...bS derivation | 20.4% | packed layout + 2 AVX2 kernels landed (Part 4) |
| entropy / CAVLC | ~12.4% | profiled, 39.8M calls (tax-inflated) |
| **inter-MC** | **~0%** | **measured: 4/13, z = -1.39** |
| intra-pred | **0.8%** | measured 7/11, z = 0.90 — confirms the 0.5% profiled |
| recon / IDCT | *unresolved* | box degraded mid-block (10.7 -> 14.7 s, range ±45%) |

## THE ARITHMETIC ON 1.3x — it is not reachable by SIMD

```
standing gap (main)   2.70x
target                1.30x
=> must remove        51.9% of ALL decode time

deblock + entropy = 40.7% of decode.
Make BOTH entirely FREE and the result is 1.60x — still short of 1.30x.
```

MC is ~0. Intra is 0.8%. The two largest stages together cannot get there even if
reduced to nothing, and deblock's 28.3% is already the most-worked code in the decoder
(three bricks, two AVX2 kernels). **The remaining gap is not concentrated anywhere a
kernel can reach it** — it is the diffuse per-macroblock orchestration that Part 1 named
and every measurement since has re-confirmed. Reaching 1.3x would require a structural
rewrite of the macroblock loop (x264's scan8 neighbour cache + fused decode/recon
pipeline), not a vectorization campaign.

Stating this now, with the arithmetic, is cheaper than discovering it after building
five more kernels.

## Brick landed: `peek_bits` single-load window

`peek_bits` is called on essentially every parsed symbol and did FOUR separate
bounds-checked `get().unwrap_or(&0)` byte loads plus three shifts and three ORs. Replaced
with ONE range check + `u32::from_be_bytes`, keeping the exact zero-fill-past-the-end
contract in a tail arm (VLC matching at stream end depends on it).

**No `unsafe` required** — `get(range)` + `from_be_bytes` compiles to what an unchecked
load would. The safe restructure came first, per the discipline; there was no need to
reach for `get_unchecked`.

Deterministic evidence (the clock could not resolve ~1% at 90% box load):
**`movbe` instructions 0 -> 8** — a single load-and-byte-swap where the old form did
4 loads + 3 shifts + 3 ORs.

Gates: `peek_bits_matches_zero_fill_reference` (every bit position x every width x 24
buffer lengths, including the boundary region where the two arms diverge) · 77/77 common
tests · 19/19 workspace suites · **byte-identical vs ffmpeg on all 9 x264 streams**.

## SIMD BRICK — AVX2 inverse quantization (`dequant_4x4_avx2`)

**Step 0 answered empirically, not assumed.** The crate emitted **zero
`vpmulld`/`pmulld` and 502 scalar `imul`** in the decoder: LLVM's cost model declines to
vectorize 32-bit integer multiply (`vpmulld` is 2 uops on this microarchitecture). That
is the NAMED reason the vectorize-kernel gate requires — auto-vec demonstrably does not
reach this loop, and it is not a hunch.

It still wins on uop COUNT: 16 scalar `imul` become 2 `vpmulld`, and the
shift/rounding-add likewise collapses 16 scalar ops into 2 vector ops.

The shift amount is a RUNTIME value (derived from QP), so the kernel uses
`_mm256_sll/sra_epi32` (variable count in an xmm) rather than the immediate-only
`_mm256_slli/srai_epi32` — a detail that silently miscompiles if you reach for the
wrong intrinsic.

### Deterministic evidence (immune to box load)

| | scalar | AVX2 |
|---|---|---|
| dequant calls (profiled, long_main) | 48,503,610 | 48,503,610 |
| multiply instructions | **776,057,760** `imul` | **97,007,220** `vpmulld` |
| | | **8x fewer** |

asm confirmation: `vpmulld` **0 -> 2** in the accel crate (exactly the two packed
multiplies covering 16 lanes), `vpsrad` 6, `vpsll` 10.

### VERDICT: NULL, leaning negative — flipped to OPT-IN

```
RS_H264_DEQUANT_AVX2   13 pairs, pinned, CPU time, ABBA, null arm 0.989 same session
scalar arm faster in 8/13   median 0.6% toward scalar   z = 0.83 (not significant)
```

**8x fewer multiply instructions bought NOTHING.** Why: dequant loads 16 levels + 16
scale factors and stores 16 outputs — **192 bytes per call**. It is MEMORY-bound, so the
scalar `imul`s were already hidden under the loads; widening the arithmetic cannot help a
loop whose clock is set by traffic.

This is the campaign's own recorded law firing for the fourth time — *a counter proves
work was REMOVED, never that time was SAVED* — and it is the same shape as the three
memory-bound SIMD reverts already in `codec-vectorize-kernel`'s ledger (SAD, skip-MC,
AVX2 16x16 SAD). **The Step-0 gate I ran checked whether auto-vec REACHED the loop; it
did not check whether the loop was COMPUTE-bound. That second question is the one that
mattered, and asking it first would have pruned this brick before it was written.**

Default is now SCALAR; `RS_H264_DEQUANT_AVX2=1` opts in. Kept in tree (byte-identical,
bit-exact-gated) because re-testing is one env var if the surrounding loads ever shrink.

### Gates
- `dequant_4x4_matches_scalar` — **bit-identical**, `assert_eq!` not a tolerance, over
  the FULL qp 0..=51 range so both sides of the `qp >= 24` branch are covered
  (left-shift vs rounding-add-then-arithmetic-shift), with negative levels included
  because an arithmetic-vs-logical shift confusion only shows on negatives.
- 19/19 workspace suites · **byte-identical vs ffmpeg on all 9 x264 streams**.
- Scalar twin retained as the oracle AND as the non-AVX2 path;
  `RS_H264_DEQUANT_SCALAR=1` forces it so both arms live in ONE binary.

## SIMD BRICK — `peek_bits` single-load window

Called on essentially every parsed symbol; did FOUR bounds-checked byte loads + 3 shifts
+ 3 ORs. Now one range check + `u32::from_be_bytes`, keeping the exact
zero-fill-past-the-end contract in a tail arm (VLC matching at stream end depends on it).

**No `unsafe` was needed** — `get(range)` + `from_be_bytes` compiles to what an unchecked
load would, so the safe restructure came first and `get_unchecked` was never reached for.
Deterministic evidence: **`movbe` 0 -> 8** (a single load-and-byte-swap instruction).

Gate: `peek_bits_matches_zero_fill_reference` over every bit position x every width x 24
buffer lengths, including the boundary region where the two arms diverge.

## THE 72% IS RANKED — and it is not SIMD-addressable

The sampling profiler was never installed (UAC blocked). Substituted a **combined
ablation**: turn OFF every pixel stage at once (`RFF_ABL_DEBLOCK` + `RFF_ABL_MC` +
`RFF_ABL_INTRA` + `RFF_ABL_RECON`) and measure what REMAINS. Paired, pinned, CPU time,
ABBA, 7 pairs, frame count 1800 in both arms:

```
pair 1  18.2%   pair 2  14.1%   pair 3  17.5%   pair 4  17.4%
pair 5  18.6%   pair 6  22.6%   pair 7  17.7%
MEDIAN pixel pipeline = 17.7%   =>   entropy + orchestration = 82.3%
```

**The entire pixel pipeline — deblocking, motion compensation, intra prediction,
inverse transform and reconstruction, i.e. everything a SIMD kernel can touch — is
17.7% of decode. The other 82.3% is entropy decode and per-macroblock orchestration:
serial bit-parsing, neighbour derivation, motion-vector prediction, grid bookkeeping.**

That single number retires the campaign's central question. It also explains every
individual result: MC measured ~0 because MC is a small slice of a small slice; the AVX2
dequant removed 8x the multiplies and bought nothing; intra is 0.8%; and the "uniform
~2.4x gap" Part 1 found was uniform precisely because it is not in the kernels at all.

### The ceiling on SIMD, computed rather than asserted

```
standing gap (main)                              2.70x
pixel pipeline                                   17.7% of decode
make ALL of it INSTANTANEOUS  ->  2.70 x 0.823 = 2.22x
target                                           1.30x
```

**Even a perfect, free, infinitely-wide SIMD implementation of every pixel kernel lands
at 2.22x — short of 1.30x by a factor of 1.7.** The 1.3x target is not reachable by
vectorization, and no amount of ASM changes that, because the mass is in code that has
no data parallelism to exploit: CABAC/CAVLC decode a bit at a time by construction, and
the macroblock loop is pointer-chasing neighbour state.

Where the remaining gap actually lives, and what would move it:
- **entropy decode** — ffmpeg's CABAC uses a cached bit window with branchless
  renormalisation; ours re-derives from the slice on every peek (the `peek_bits` brick
  took one bite of this).
- **per-MB orchestration** — ffmpeg's `scan8` keeps neighbour state in a small padded
  per-MB cache so availability/prediction is an array index, never a re-derivation. Ours
  re-gathers neighbours from frame-wide grids. This is the same class of defect the
  packed-bS brick fixed for deblocking, applied to the whole macroblock loop.

Both are STRUCTURAL rewrites of the decode loop, not kernels. That is a larger and
different campaign than "deploy SIMD", and this measurement is what says so.

### Correction recorded

An earlier note in this session claimed horizontal luma deblocking's
transpose -> filter -> transpose-back was "a real SIMD gap vs ffmpeg". **That was wrong
and is retracted.** Filtering a VERTICAL edge puts p3..q3 as adjacent bytes within each
row; getting p0 of 16 rows into one register requires a transpose BY CONSTRUCTION, and
ffmpeg transposes for the same reason. Checking the data layout before calling something
a gap would have caught it — the same discipline that priced every other lever here.

---

## Part 6 — A profiler that finally works, and the first orchestration brick

### The instrument defect that had blinded the whole campaign

Every per-macroblock share in Parts 1–5 was measured with an rdtsc scope guard, and
that guard's own tax was measured at 1.32–1.43× of whole decode. The campaign's
response had been to abandon the profiler for *ablation*, which is tax-free — but
ablation can only price a stage someone has already built an `RFF_ABL_*` knob for.
That is why ~72% of decode stayed unranked for five parts.

The fix was not a better timer. It was to **stop timing every call**. `prof::scope`
now times 1 call in N (`RS_H264_PROF_SAMPLE`, default 1 = old behaviour) and scales
the sample back up. A stage is entered in proportion to how much time it takes, so
timing 1-in-N estimates the same share with 1/N the tax.

Two defects in the first cut, both caught by the numbers rather than by review:

1. **A power-of-two stride aliases.** Decode work is intensely periodic (blocks per
   macroblock, macroblocks per row). A `c % 64` stride can lock onto the same phase
   of that pattern and sample only the cheap — or only the expensive — calls. Fixed
   by selecting on a *hash* of the call index (multiply by the 64-bit golden ratio,
   test high bits), which decorrelates selection from any workload period at ~4
   cycles.
2. **Low-count stages must not be sampled at all.** `Total` is entered once per
   FRAME — 60 calls on a 60-frame clip. At N=64 it estimated the entire denominator
   from ONE sample, and since every share is a ratio to `Total`, that single bad
   estimate skewed the whole table. The symptom was unmistakable in hindsight: every
   leaf share rose *together*. Fixed with `EXACT_PREFIX = 8192` — the first 8192
   calls of any stage are timed exactly and carry weight 1. Sampling a rare stage
   buys no speed anyway, because the tax is proportional to call count.

**Validation against an independent instrument.** Self-consistency is not proof, so
the sampled share was checked against ablation, which is tax-free by construction:

| instrument | deblock share |
|---|---|
| ablation (`RFF_ABL_DEBLOCK`, 6/7 pairs, z=1.89) | **14.0%**  (range 11.8–17.3%) |
| sampled profiler, N=64 | **11.5%**  — inside the ablation range |
| fully-timed profiler, N=1 | **8.9%**  — *below the entire range* |

Sampling moved the estimate from outside the ground-truth range to inside it. The
profiler is trustworthy for the first time in this campaign.

A second, independent confirmation fell out of the same run: the *nested* per-MB
stages moved the opposite way to the leaves (`dec-mb-B` 41.2% → 36.9%) — exactly what
must happen when the tax is removed, since an outer scope contains every child's
rdtsc cost and deflates when the children stop paying it.

### What it found: ~11% of decode is per-frame bulk memory, not codec work

The first look through the working instrument found the largest unattributed cost
outside the macroblock loop entirely:

- `dec-setup` **6.7%** — `FrameDecoder::new`, allocating ~1.65 MB of frame-wide grids
  plus ~1.38 MB of reconstruction planes for **every coded picture**.
- `dpb-clone` **4.1%** — `pad_plane` copying three planes per reference picture.

Neither is entropy decode nor pixel math. Both were invisible for five parts because
they are per-*frame*, so they never showed up in a per-macroblock hunt.

### Brick: pooled per-picture grids — LANDED, default on

`GridPool` hands the finished picture's grids to the next one; `refill()` is
`clear() + resize()`, which keeps the initialising fill and drops only the
allocation. The fill is deliberately *not* skipped: `modes_y` must read 2/DC and
`ref_idx_y` must read −1 as neighbour context before the block that writes them, so a
stale value from the previous picture is a correctness bug, not a performance trade.

The allocation is the bigger of the two costs. A ~460 KB `Vec` goes straight to the
OS, so every page is a fresh zero page and the decoder takes a soft page fault on
FIRST TOUCH of each 4 KB — a cost charged to whatever per-MB stage happens to touch
it first, never to the allocation itself. That is the second reason this hid so well.

Reconstruction planes are deliberately not pooled: `into_frame` MOVES them out as the
caller's output frame, so there is nothing to hand back.

| gate | result |
|---|---|
| x264 corpus, byte-identical vs ffmpeg | **9/9** |
| conformance matrix | **160/160** |
| workspace suites | **19/19 green** |
| paired ABBA, pinned, CPU time (`RS_H264_NO_POOL`) | **8/9 pairs, z=−2.33, ≈5.3% faster** |

Clears the |z|>2 bar. Default on; `RS_H264_NO_POOL=1` restores per-picture allocation
for A/B.

### Negative result worth recording: the CABAC bit engine is not the problem

The entropy stage is the single largest at 18.4%, so the arithmetic decoder was the
obvious suspect. It is already ffmpeg-class: a 64-bit window refilled by ONE 8-byte
big-endian load, branchless renormalization (`leading_zeros`, no loop), branchless
MPS/LPS selection by mask arithmetic, and table indices proven in range so the bounds
checks are gone. There is no structural win available in `take`/`refill`/
`decode_decision`. Entropy time is in context selection and the residual loop
structure around the engine, not in the engine — that is where the next descent goes.

---

## Part 7 — CABAC/CAVLC full diagnosis (goal session, 2026-08-05)

Cross-checked against `../remade_ffmpeg_rs/docs/plans/unsafe-opportunities.md`
(the owner's post-forbid catalog). Its pre-refuted table held up on inspection:
the CABAC arithmetic engine (H-34/H-35 window/renorm/branchless-decision) and the
CAVLC `peek_bits` load are at structural parity with ffmpeg and were not re-attacked.

### Phase 0 profile, sampled N=64, per arm (same content, stockholm 720p)

| stage | CAVLC arm | CABAC arm (high) |
|---|---|---|
| entropy | 15.4% | 18.8% |
| syntax-parse | 1.6% | 4.6% |
| inter-mc | 14.1% | 16.1% |
| deblock | 28.1% | 11.8% |
| scatter(store) | 1.4% | 5.0% (7.4% on main) |
| decode ms | 460 | 866 |

The CABAC/B arm decodes **1.9× slower** than the CAVLC/baseline arm of the same
content. (Confounded: the arms differ in B-frames as well as entropy coder — the
corpus has no CABAC-baseline pair.)

### Entropy-stage decomposition (new `ent:*`/`cav:*` sub-scopes, sampled)

Shares inside the stage are inflated by nested-guard tax; the RANKING is the
finding (§15: counter primary, clock confirmation):

- **CABAC**: significance map ≈ 41% of the stage, level loop ≈ 34%, cbf ≈ 7%.
  The two loops are the entropy stage; everything else is noise.
- **CAVLC**: coeff_token ≈ 26%, runs/zeros ≈ 17%, levels ≈ 15%. And a counter
  finding: **1,558,534 residual calls, only 990,323 with any coefficient** —
  36% of calls pay one VLC read to learn "0 coefficients". That read is how the
  count is coded; ffmpeg pays the same. No lever.
- CAVLC VLC reads are single-peek flat-LUT (O(1) per codeword) — already the
  ffmpeg shape. The bypass helpers (`cabac_exp_bypass`, UEG suffixes) mirror
  openh264 bin-for-bin.

### Brick: sparse CABAC level decode — byte-identical, timing pending

The dense tail did three things per residual block that ffmpeg does not: zero a
256-byte `sig` array, RE-SCAN all 16-64 positions in the level loop testing
`sig[i] != 0` (a data-dependent mispredicting branch, when typically 2-4
positions are significant), and copy the dense array into `out`. Rewritten to
record significant POSITIONS during the sig-map pass and decode levels over just
that list — bin order provably unchanged (levels were decoded at descending
significant positions, which is the position list reversed). Contract: all 10
call sites pass freshly-zeroed `out` (verified by reading each site).

Gates: **byte-identical 9/9** vs ffmpeg, conformance matrix 160/160, workspace
144/144. Timing verdict (longer-arm run, 10 reps/arm, 11 pairs, pinned CPU
time): **7/11 sparse faster, z=0.90, median +6.2%** — the three losses were all
under 1% (ties at this box's resolution) plus one -16% contention burst. Under
the |z|>2 bar this does NOT bank as a measured % win. §15 applied as written:
the COUNTER is primary — per block the rewrite deletes a 256-byte memset, a
16-64-slot mispredicting rescan, and a dense copy, adds only one position write
per significant coefficient, and is byte-identical — so it is KEPT as certain
work-removal with the clock weakly confirming. Recorded expectation: ~0.5-1% of
CABAC-arm decode, below this box's noise floor by construction.

### The real CABAC-arm mass is layout, not bins

Named glue, in descending order of expected value:

1. **Motion-state SoA layout.** Motion lives in four frame-wide strided arrays —
   `mv_y: Vec<(i32,i32)>` (8 B/block), `ref_idx_y: Vec<i32>` (4 B for values
   0..15), `inter_y`/`coded_y: Vec<bool>` — written per-partition by
   `commit_inter_grid` (4 separate arrays touched per 4×4 block), read back by
   `mv_neighbors_block` with strided loads + `nbr_in_slice` per neighbour, and
   **cloned wholesale per reference frame** in `as_reference` (the `dpb-clone`
   stage). This is the same layout gap the bS campaign proved and fixed for
   deblocking (MbPack). The x264 shape is a per-MB packed record + 30-entry
   cache. `scatter(store)` at 5-7.4% and part of dpb-clone's 4.1% price it.
2. **Per-MB stack zeroing**: ~2.5 KB of residual buffers (`luma_scan` 1 KB +
   `luma8` 1 KB + `cac`/`cdc` 0.5 KB) zeroed per inter MB even when cbp==0
   parses nothing. NOTE: with the sparse level decode, these zero fills are now
   LOAD-BEARING (the parser writes only significant entries) — any future
   attack on them must re-introduce per-block clearing.
3. `syntax-parse` 4.6% is the mvd/ref/cbp bins — openh264-shaped, no structural
   lever found.

---

## Part 8 — Six whys on the orchestration around the bins

The Part 7 diagnosis ended at "the mass is orchestration, not bins." This part
descends it to a mechanism, with a measurement at every level.

**Why 1 — symptom.** Decode is 2.4-2.7× slower than ffmpeg on x264 streams
(pinned CPU time, work-count-verified).

**Why 2 — stage.** Not pixel kernels: the combined ablation caps the entire
SIMD-touchable pipeline at 17.7% of decode. Not the entropy engines: Part 7
established structural parity bin-for-bin. What remains is the per-MB
orchestration — `mgmt/other` measured 30-34% on the CABAC arm, ~1.2 µs per
macroblock of unattributed time.

**Why 3 — op (two hypotheses refuted, three named).** The standing hypothesis —
the motion-state SoA layout taxes every MB — was WRONG at per-MB granularity,
and the sampled profiler proved it: the neighbour/state cache shuffling
(`state-cache`) measured **0.2%**, and the grid commit + read-back (`mv+grid`)
measured **0.2%**. The frame-wide arrays are cache-hot when touched MB-locally;
the layout tax is real only at frame scale (`dpb-clone` 3.2%, and the bS pack).
What the accounting actually cornered: `resid-add` **14.6%** inclusive,
`mc-stage` **11.8%** inclusive, plus `b-mc`/`b-direct` internal glue (~5%
combined) — after subtracting the leaf stages nested inside them, roughly
**15% of decode is glue between named steps**.

**Why 4 — primitive.** Reading those bodies: each 4×4 block's data crosses ~7
materialized intermediate arrays — parse → `luma_scan[16]` → nnz RE-COUNT (a
16-element scan for a count the parser just computed) → `un_scan` permute →
`dequant` array → `from_fn` pred gather (u8→i32) → `reconstruct` array →
`store` scatter. The P-path MC staged every rect through a zeroed `t[256]`
then row-copied it into `pred_y` — even for full-width rects whose rows are
already contiguous in the destination.

**Why 5 — mechanism.** **Stage-boundary materialization.** The decoder is
written as pure stages handing each other owned fixed arrays — clean, testable,
and the reason the oracle gates are cheap — but every boundary costs a
materialization plus, in two places, a re-derivation of information the
upstream stage already held. ffmpeg fuses these boundaries: the parse tracks
nnz as it goes, the IDCT adds into the frame buffer in place, MC writes its
destination directly.

**Why 6 — instrument.** Sampled scopes (inclusive-minus-leaves accounting) for
the shares; call counters for the re-derivation volume (~400 redundant loads
per MB × 141k inter MBs on the test clip); byte-identical corpus decode as the
correctness oracle for every fusion.

### Bricks landed from the descent (both safe Rust, no unsafe spent)

1. **MC direct-to-pred**: full-width (16-wide luma / 8-wide chroma) rects MC
   straight into `pred_y`/`c_pred` — the staging buffer + copy exist now only
   for narrow rects whose rows are genuinely strided. Kills 256 B zero + 256 B
   copy on the dominant 16×16/16×8 shapes.
2. **Parsed-nnz threading**: `parse_residual_cabac`'s return value (discarded
   at all 6 luma/chroma call sites) now flows into `add_inter_residual`, which
   stops re-scanning 16-64 coefficients per block to re-learn the count.

Gates: **byte-identical 9/9**, conformance **160/160**, workspace **144/144**.
Timing verdict (11 pairs, 10 reps/arm, pinned CPU time, ABBA): **5/11, z=-0.30 —
NULL; the box's within-arm spread was ±37% (baseline arm ranged 4.8-9.0 s for
identical work), which no pairing can see a ~2% effect through.** The best-of
statistic — robust when contention is purely additive — favours the bricks
(min 4,672 vs 4,781 ms, -2.3%), consistent with the predicted size. §15 verdict:
kept as certain work-removal (the counter: ~400 redundant loads/MB deleted, 512 B
staging traffic/full-width rect deleted, byte-identical), no % claim banked.

---

## Part 9 — Hammering the Part 8 targets: three fusion bricks

All three attack the named mechanism (stage-boundary materialization), all in
safe Rust, all gated byte-identical 9/9 + conformance 160/160 + workspace
144/144 before any timing.

### Brick A — DC-only residual fast path (the ffmpeg `idct_dc_add` split)

When a block's only significant coefficient is the DC (`nnz == 1 &&
scan[0] != 0` — knowable for free now that parsed nnz is threaded), the entire
dequant (16 multiplies) + two IDCT butterfly passes provably collapse to
`(dequant_dc(level) + 32) >> 6` added flat: `inv_1d(f,0,0,0) = (f,f,f,f)` takes
no `>> 1` flooring path, so the collapse is bit-exact, not approximate.
`dequantize_dc4` is the single-coefficient twin of both flat and
scaling-list dequant; `reconstruct_4x4_dc` honors the same ablation knob and
profiling stage as the dense path so measurement arms stay comparable.
Chroma is even cheaper: its DC arrives already dequantized, so every
`cbp_chroma==1` block and every AC-empty block of `cbp_chroma==2` skips
dequant AND un-scan AND IDCT entirely — the un-scan now runs only on blocks
with coded AC.

### Brick B — b_mc staging fusion (196,844 calls on the test clip)

`b_mc` zeroed 512 B of luma + 256 B of chroma staging EVERY call, then
row-copied even uni-pred output. Now: full-width regions (px==0, rw==16 — every
16×16/16×8 partition and most direct regions) MC DIRECTLY into
`pred_y`/`c_pred`; uni-pred needs no staging at all; bi-pred stages only the
second list and blends in place; staging arrays exist only on the branches that
read them. Mirror of the P path's mc_rect fusion.

### Brick C — DPB plane pool (the `dpb-clone` 3-4%)

`as_reference` allocated ~1.9 MB of fresh padded planes per reference picture.
Evicted DPB frames now park in `Decoder::retired` and their planes are
reclaimed into a bounded pool once the evicting picture's `FrameDecoder` is
consumed (earlier the `Arc`s aren't unique and `try_unwrap` would fail — that
ordering subtlety is why reclamation happens at the picture boundary, not at
eviction). `pad_plane_into` writes EVERY byte of the padded plane, so a
right-sized recycled buffer needs no clearing and its pages are already warm.
This respects the standing refutation of pad_plane memset-elimination: on a
FRESH alloc the zero pages are free and the memset skip bought nothing; the win
only exists when the ALLOCATION is recycled, which is what the pool adds.

### Part 9 timing verdict — BANKED

Paired ABBA vs the pre-hammer binary (11 pairs, 10 reps/arm, pinned CPU time,
stockholm 720p high/CABAC): **9/11 pairs faster, z=2.11, median +5.1%,
best-of +4.2%** — clears the |z|>2 bar; the effect size and the best-of agree,
so this is stated as **~4-5% of CABAC-arm decode removed** by the three fusion
bricks together. Combined with the grid pool banked earlier today (~5.3%,
z=-2.33), the session total on this arm is ~9-10%.

Root-cause narrative confirmed end to end: the profiler named
stage-boundary materialization, the bricks removed exactly those boundaries,
and the clock moved by about what the inclusive-scope arithmetic predicted.

---

## Part 10 — Resid-add glue fusion (the last named boundary)

Part 9's Brick A removed the DC-only blocks from the dense pipeline; this part
removes the pipeline itself for the blocks that remain (coded-AC blocks).

**The op** (why 4 of the standing descent): for every coded-AC 4×4 block,
`add_inter_residual` still ran dense un-scan (16 loads + 16 stores) → dense
dequant (16 multiplies, even with 3 significant coefficients) → a 16-element
u8→i32 prediction gather → a result array → a `store` call.

**The fusion**:
- `dequant_scatter_4x4` — un-scan + dequant in ONE pass over only the
  significant coefficients, exiting after the `nnz`-th one (the parse's count
  is exact; CABAC levels are never zero). Bit-exact because a zero level
  dequantizes to zero under both qp branches (for qp<24 the rounding term
  `(1<<(3-shift)) >> (4-shift)` is 0 for every shift 0..=3 — checked per
  shift, not assumed). Dense 16-mul dequant becomes `nnz` multiplies scattered
  straight to raster positions via the ZIG4 table; the `ac_shift` parameter
  serves chroma AC blocks whose scan index i is overall position i+1.
- `reconstruct_4x4_into` / `reconstruct_4x4_dc_into` — IDCT + add + clip +
  store in one pass, prediction read strided in place, output written straight
  into the frame plane. Kills the predb gather, the result array, and the
  store call. Both honor the `RFF_ABL_RECON` knob so ablation arms stay
  comparable. The zero-residual path likewise copies pred rows directly into
  the plane.
- Applied to the luma 4×4 and both chroma paths of `add_inter_residual`
  (the CABAC inter P/B hot path). The 8×8-transform branch is untouched
  (smaller population; separate brick if the profile ever names it).

**Process incident, recorded because the catalog predicted it**: the helper
script was written but never executed, the build errors scrolled past a
truncated grep, and the first "9/9 byte-identical" gate ran the PREVIOUS
binary — a textbook stale-binary false pass ("verify the exe mtime before
trusting any run", unsafe-opportunities.md Phase 0). Caught one step later by
the workspace test build failing on the missing imports; re-ran, re-built with
a zero-error assertion, re-gated on a verified-fresh binary: byte-identical
9/9, conformance 160/160, workspace 144/144.

### Part 10 timing verdict — hybrid kept, and the null taught something

Two timing rounds against the Part 9 binary (11 pairs each, 10 reps/arm,
pinned CPU time, box under heavy foreign load both times):

| arm | win rate | median | best-of |
|---|---|---|---|
| pure scatter (all coded blocks) | 4/11, z=-0.90 | **-0.9%** | +6.8% |
| hybrid (scatter iff nnz ≤ 6, dense above) | 7/11, z=0.90 | **+8.1%** | +3.8% |

The pure-scatter null's NEGATIVE median was not dismissed as noise — it flagged
a real mechanism: the scatter walks scan positions with a data-dependent branch
per slot, which beats the branchless dense 16-multiply loop only while the
block is sparse, and the DC/zero fast paths had already removed the sparsest
blocks from this population. The nnz ≤ 6 hybrid flipped every statistic
positive. Under the |z|>2 bar neither round BANKS a % claim; the hybrid is
kept on the counter (arrays, copies and store calls certainly deleted; dense
dequant retained exactly where it wins) with the clock directionally
confirming (median +8.1%).

Two stale-binary incidents this part — a helper script that never executed and
a borrow-check failure whose errors scrolled past a truncated grep — both
caught before any wrong number was recorded, the second by the exe-mtime check
added after the first. Every gate now prints the binary's mtime.

---

## Part 11 — b-direct: the descent found my own regression

Target: `b-direct` at 16.7% inclusive. The descent's first step — READ the
body before building anything — found that the derivation itself is already
lean (coalesced colZero rects, `b-deriv` measured 175 ns/call = 1.4%), and the
mass is the `b_mc` bi-prediction it triggers. And inside THAT, the biggest
defect was one this campaign introduced two parts ago:

**Part 9's b_mc refactor made the chroma blend a `&dyn Fn` — an INDIRECT CALL
PER BLENDED PIXEL** (~25M virtual calls per clip on bi-pred chroma). The luma
closure stayed monomorphic, but it still carried the `weights` match INSIDE
the per-pixel body, hiding a loop-invariant behind a capture and blocking
autovectorization of the standard `(p+q+1)>>1` bi-pred average.

Fix: `b_mc_chroma` now takes `weights: Option<(i32,i32)>` directly, and every
blend site (luma full-width, luma narrow, chroma full-width, chroma narrow)
matches on `weights` ONCE and runs a branch-free pixel loop — the unweighted
average is now a plain u16 add/shift loop the compiler can vectorize.
Bit-exact: u16 arithmetic covers the 511 max, weighted arm formula unchanged.

Gates (exe mtime verified): byte-identical 9/9, conformance 160/160,
workspace 144/144.

Lesson recorded: a fusion brick that threads a callback through a new function
boundary must pass DATA (the weights), not CODE (`&dyn Fn`) — the borrow
checker pushed toward `&dyn` and the byte-identical gate cannot see a
performance regression, so nothing objected until the next descent read the
code with per-call counts in hand.

### Part 11 timing verdict

Two rounds vs the Part 10 binary: 8/11 (median +10.7%) then 5/9 — pooled
**13/20, z=1.34**, under the |z|>2 bar. Kept on the counter: ~25M indirect
calls per clip certainly removed and the bi-pred average loop made
vectorizable; the clock is directionally positive but the effect is a slice of
`b-mc`, not the whole 16.7% of `b-direct` — the derivation itself was measured
LEAN (175 ns/call, 1.4%), so the stage's remaining mass is genuine
bi-prediction work (two MC passes per region by construction), which is a
kernel-efficiency question, not an orchestration one.

---

## Part 12 — b-direct kernels: refuted by disasm, upgraded for free

The Part 11 verdict left b-direct's mass as "genuine two-pass MC arithmetic —
a kernel-efficiency question (SIMD pavgb-class blend, wider MC)". Phase 2
discipline (cheap refutations first) settled both halves without writing a
kernel:

**REFUTED: a hand AVX2 blend kernel.** Isolation disasm at `x86-64-v3` shows
rustc ALREADY lowers `(p+q+1)>>1` over u8 to `vpavgb`, and the weighted
`(p·w0+q·w1+32)>>6` arm to unrolled `vpmull`/`vpmadd` — even in the indexed
loop form. A hand kernel cannot beat the instruction the compiler already
emits (same law as the psadbw SAD refutation). DO NOT build one.

**Found in the same disasm: a free upgrade.** The indexed form keeps a
per-iteration bounds check and a loop; the SLICE-then-zip form compiles the
whole 256-byte average to **8 straight-line vpavgb ops — no loop, no checks**.
All four blend sites converted to sliced/zip.

**CONFIRMED already-served: MC width dispatch.** `mc_luma_padded` has
const-width full-pel row paths (16/8/4) and `mc_luma_subpel` reaches the
width-parameterized accel kernels; 16-wide direct regions hit them today.
Nothing to build.

Gates (fresh mtime): byte-identical 9/9, conformance 160/160, workspace
144/144. Timing vs Part 11: 6/11, z=0.30, median +1.0%, best-of 0.0% — an
honest null, as physics predicts (the de-virtualization last part was the
first-order effect; slicing removes only loop overhead). Kept on the counter:
strictly fewer instructions, no downside. b-direct is now CLOSED as a target —
its remaining cost is two MC passes per bi-pred region, which is the codec's
own arithmetic, and the pixel-pipeline ceiling (17.7%) prices what any further
kernel work there can buy.

---

## Part 13 — The two MC passes: census-guided descent into quarter-pel

"Two MC passes per bi-pred region" was attacked by instrument, not intuition.
The MC census (size × phase, CYCLE-weighted) says where those passes spend:
**quarter-pel is 67.8% of decoder MC cycles** (8x8-q 29.0%, 16x16-q 28.3%,
16x8-q 9.9%), half-HV 22.3%, full-pel only 5.3%.

Two named findings:

1. **The scalar `pixel_avg` leak.** The one-filter quarter positions
   ((1,0)/(3,0)/(0,1)/(0,3)) end in `avg_full`, which got the pavgb kernel
   long ago — its own comment records that the scalar average "was handing the
   kernel's win back". The EIGHT two-filter positions ((1,1)-class and the
   centre-adjacent four) end in `pixel_avg`, which was still the scalar
   runtime-width loop. Same leak, other door. Now dispatched to the same asm
   kernel; the scalar loop stays as the non-accel path and oracle.

2. **A segfault the corpus gate caught before anything shipped.** The first
   cut reconstructed the kernel geometry from `n` alone, forcing width 16 —
   giving the row-unrolled SSE2 kernel h=1..2 on sub-8x8 blocks, which
   over-runs the block. Every `__high` stream (sub-8x8 partitions) crashed;
   every `__main` stream (min 8x8, h ≥ 4) passed — a clean demonstration of
   why the corpus must span partition shapes ([[cross-axes-dont-sweep]]).
   Fixed by passing the TRUE (bw, bh): the kernels are already proven at every
   real shape by `avg_full`. Lesson: NEVER hand asm a synthetic geometry that
   element-wise reasoning says is equivalent — the kernel's loop structure,
   not the arithmetic, defines the contract.

Also confirmed from the census: the 8x8-quarter per-pixel cost (3.1 cyc/px vs
16x16's 1.1) prices a ~128-cycle fixed per-call overhead — scratch borrow,
range checks, dispatch — that bi-pred pays twice per region. That overhead is
the remaining MC target, but it is profile-build-inflated (the census guards
themselves sit in it); price it by ablation before attacking.

Gates (fresh mtime): byte-identical 9/9 (including the previously-crashing
streams), conformance 160/160, workspace 144/144. Timing: 6/11, z=0.30,
median +4.5% under ~60%-inflated foreign load — kept on the counter (≥15
scalar ops per 16 pixels replaced by one pavgb across 375k+ quarter calls per
clip), clock directional only.

---

## Part 14 — The MC per-call overhead: fixed and BANKED

The Part 13 census priced a fixed per-call overhead on sub-pel MC (the 8x8
quarter row: 3.1 cyc/px vs 16x16's 1.1). Honest sizing first: the 128-cycle
figure was PROFILE-INFLATED (the census guards sit inside it); the shipping
build's true per-call cost is the thread-local scratch lookup + RefCell borrow
+ range checks + dispatch — ~30-60 cycles, paid on ~1M sub-pel calls per clip
and TWICE per bi-pred region.

Fix: `with_mc_scratch` + `mc_luma_padded_pre`. The scratch borrow (TLS lookup
+ RefCell) is hoisted out of the per-call path: `b_mc` borrows ONCE per region
— both bi-pred passes and all narrow-arm staging included — and the P-path
rect ladder borrows around its calls. The public `mc_luma_padded` keeps the
old signature as a thin wrapper, so every other caller (skip paths, encoder,
tests) is untouched.

One bug caught BEFORE build, by reading the generated diff: wrapping the
region body in a closure turned the bi-full arm's `return self.b_mc_chroma()`
into a return from the CLOSURE — chroma would have run twice. Restructured to
a `chroma_done` flag yielded by the closure. (A `return` inside a new closure
boundary is the control-flow twin of the `&dyn` data-flow lesson from Part 11:
every mechanical wrap changes semantics somewhere.)

Gates (fresh mtime): byte-identical 9/9, conformance 160/160, workspace
144/144.

**Timing verdict: 10/11 pairs, z=2.71, median +5.2% — CLEARS the bar.**
Every pair non-negative (worst 0.0%), the monotone signature of an always-on
win. Third banked brick of the campaign (grid pool z=-2.33, fusion round
z=2.11, scratch hoist z=2.71).

---

## Part 15 — Deblock cracked open: the filters are free, the plumbing is not

Diagnosis session on the last big named stage (11.9% CABAC arm / ~28% CAVLC arm).

**The filter math is already solved.** Every luma and chroma edge dispatches to
the vendored SSSE3 kernels (`DeblockLumaLt4/Eq4` H+V, chroma pairs); the scalar
line filters fire only in non-accel builds. The `RFF_ABL_DBKERNEL` ablation
prices ALL deblock kernels at **~1.3% of decode** (median, 6/9 — noisy but
bounded). Since the stage is ~11.9%, **derivation + orchestration ≈ 10% and
the kernels ≈ 1-2%**. An AVX2 rewrite of the SSSE3 filters is ceiling-capped
at well under 1% — refuted before being built.

**Where the ~10% lives (functions, ranked):**

1. `pack_frame` — allocates a fresh `Vec<MbPack>` (~320 B × 3600 MBs ≈ 1.1 MB)
   EVERY frame, then fully writes it. Same mechanism the banked GridPool brick
   removed (fresh large allocs = first-touch page faults charged elsewhere).
2. `FrameDecoder::deblock` — builds `ref_id`/`ref_id1` POC-map `Vec<i32>`s
   (230 KB each, ~460 KB on B frames, 57,600 mapped elements) as a pure
   calling-convention shim: the actual map is a ≤16-entry ref→POC table, and
   `pack_frame` immediately re-reads the big Vecs into MbPack records. Passing
   the small table + raw `ref_idx` grids through `BlockInfo` removes both the
   allocation AND the per-element mapping pass.
3. Same function — clones `nnz_y` (57 KB) per frame on any t8x8-bearing frame
   to build the transform-block coded mask.
4. The per-MB derivation loop itself (packed AVX2 pipeline, uniform fast
   paths, kind gates) — already heavily optimized by the packed-bS campaign;
   no obvious structural fat found on read.

All three top items are the SESSION'S proven mechanism (per-frame
materialization) applied to deblock's input side; together they plausibly
cover several points of the ~10%.

### Part 15 bricks 1+2 — LANDED

1. **`pack_frame_into` + thread-local recycled buffer.** The ~1.1 MB
   `Vec<MbPack>` is now built into a `Cell`-held scratch (take/set around
   `filter_frame`'s MB loop — no borrow held across it). Records are built in
   locals and pushed, so a reused allocation is written exactly once per byte:
   no defaults pass, no page faults.
2. **`poc0`/`poc1` maps in `BlockInfo`.** The decoder passes its raw
   `ref_idx` grids plus two ≤16-entry ref→POC tables; `rid()`/`rid1()` map at
   the read sites (`Blk::load`, `pack_frame_into`). The two frame-wide
   pre-mapped `Vec<i32>` shims (230-460 KB + 57,600 mapped elements per frame)
   are GONE. Empty maps preserve the old contract, so the encoder and all
   tests pass `&[]` unchanged.

Combined counter: ~1.6 MB/frame of allocation and a 57,600-element mapping
pass removed. Gates (fresh mtime): byte-identical 9/9, conformance 160/160,
workspace 144/144 — the 9/9 also empirically confirms the intra-frame identity
subtlety (-1 vs NO_REF is invisible to bs because intra blocks never reach the
inter rules). Timing: 6/11, z=0.30, median +5.9%, best-of +4.1% under 25-60%
foreign-load inflation — the two contention-robust statistics agree positive;
kept on the counter, no % banked.

---

## Part 16 — Per-MB derivation arithmetic: five whys, one refuted deployment, the root named

Treated as if the packed-bS campaign had missed routes. Instrumented first
(`deb:pack` 2.5%, `deb:derive` 3.7%, edge-setup ~4.4%, kernels 1.3%).

**The chain:**
1. Deblock is ~10% software around ~1.3% of filter math.
2. Pack costs 2.5% because it re-materializes 320 B/MB from strided grids —
   data the decode loop held in registers when it finished that macroblock.
3. Derive costs 3.7%; reading it fresh: every inter MB pays TWO vector
   dispatches (`mb_uniform`, then `bs_motion_masks`), and uniformity is
   derivable from the masks (`(left & 0xEEEE) == 0 && (up & 0xFFF0) == 0`).
4. The uniform bs arm duplicates the general arm at masks==0 — dead branch.
5. **Root cause: the two-pass architecture.** Decode writes grids; deblock
   re-packs, re-derives, then filters. x264 derives bS inside the decode MB
   loop while the data is hot and filters a row behind decode. The full fix is
   row-interleaved deblocking — a campaign, not a brick.

**Deployment of iterations 3-4 — REFUTED at z=-2.11, reverted.** The
masks-derived-uniform fusion regressed 2/11, median -1.9%. Mechanism (found by
re-reading the dispatchers): the two calls are ASYMMETRIC. `mb_uniform` is a
cheap AVX2 compare-to-block-0 that handles two-list B data in VECTOR code;
`bs_motion_masks` falls to the scalar §8.7.2.1 set-matching walk whenever
`l1_used != 0` — most B inter MBs. The population reaching `derive_mb_packed`
on B frames is uniform-heavy (Skip/Direct), so the "fusion" replaced their one
cheap vector call with an expensive scalar walk. The refutation and its
mechanism are recorded IN THE CODE at the revert site.

Collateral finding worth having: the runtime unmasked oracle FIRED during this
work (flat-flag divergence, by design of the widened rule) — the
oracle-contract discussion is preserved in history; the strict contract is
restored with the revert and passes on three streams.

**The actual lever this names:** an AVX2 TWO-LIST `bs_motion_masks` kernel
(vectorized set-matching). It removes the scalar walk from every non-uniform B
MB — the real cost iteration 3 found — AND retro-validates the fusion, whose
only flaw was making cheap MBs pay the scalar path. One prerequisite kernel,
two wins. That plus derive-at-decode-time (root, campaign-scale) are the
remaining routes; everything else in the derivation read as already-taken.

---

## Part 17 — Both Part 16 levers built

### Lever 1 — AVX2 two-list `bs_motion_masks` kernel: LANDED

The §8.7.2.1 set-matching rule, vectorized branchless:
`differs = !((e0 & e1 & !farStraight) | (c0 & c1 & !farCross))` — proven
case-equal to the scalar `pk_differs` decision tree INCLUDING its
slot-compaction cases, because unused-slot motion is neutralized to zero
INSIDE the kernel (`mv &= (ref != NO_REF)`), making the formula's invariant
hold by construction rather than by caller contract. Gated by
`two_list_masks_match_scalar`: 50,000 randomized two-list records with
deliberate GARBAGE on unused slots (proving the neutralization), bit-exact.
Dispatched for `l1_used != 0` — B macroblocks with List-1 slots no longer fall
to the scalar per-edge walk. This also retro-validates the Part 16 fusion's
logic for a future retry: its only flaw was routing cheap MBs to the scalar
path that no longer exists.

### Lever 2 — Fused pack+derive with a rolling window: LANDED, null-neutral

`precompute_bs_frame`: each MbPack record is derived the moment it is built,
with only a TWO-ROW window (~50 KB) of records ever existing — the ~1.1 MB
frame buffer is gone — and the output (32 B/MB of strengths) feeds
`filter_frame`'s existing precomputed path, collapsing the per-MB gate ladder
in the hot loop. `RS_H264_BS_PRE=0` restores the old pipeline for A/B.

Timing, BOTH content axes crossed (per the population-shaping law this
session added to the dispatch skill): B-heavy high arm 6/11 z=0.30 NULL;
skip-heavy CAVLC arm 6/11 z=-0.30 NULL — and the feared failure mode (Skip
MBs paying record-derivation they previously dodged via kind gates) did NOT
materialize on the arm where it would live. Verdict: kept DEFAULT ON — clock
neutral on both axes, working-set counter certain (-1.05 MB/frame), and the
precomputed-consumer shape is the stepping stone the true derive-at-decode
campaign (the Part 16 root) will feed from a per-MB decode hook.

Gates (fresh mtime): byte-identical 9/9 (both knob arms), conformance
160/160, workspace 145/145 (the kernel oracle added one).

---

## Part 18 — Full head-to-head vs ffmpeg: the gap after the campaign

Same instrument as the 2026-08-01 baseline (pinned core, High priority, CPU
time, ABBA pairs, ffmpeg `-threads 1`, 1800-frame work parity verified both
sides), 9 pairs per stream, every pair ffmpeg-faster (z=3.00 — no arm
ambiguity):

| x264 stream | 2026-08-01 baseline | 2026-08-05 NOW | runtime removed |
|---|---|---|---|
| long_cavlc (Baseline/CAVLC) | 2.62× | **1.983×** | **-24.3%** |
| long_main (Main/CABAC+B) | 2.88× | **2.194×** | **-23.8%** |
| long_high (High/8x8+sub8) | 2.85× | **2.213×** | **-22.3%** |

The CAVLC arm is UNDER 2× for the first time. The session's per-brick ledger
claimed ~10% in |z|>2-banked wins plus a family of counter-kept bricks the box
could not individually resolve; the compounded end-to-end number is ~23% on
every arm — the counter-kept bricks were real, exactly as §15's
counter-primary rule predicted. All of it byte-identical to ffmpeg on all nine
conformance streams throughout, all in safe Rust outside the pre-existing
accel boundary.

Remaining gap shape (~2.0-2.2×): entropy bins + the per-MB orchestration that
remains after fusion, the two-pass deblock architecture (Part 16 root), and
ffmpeg's fully-tuned AVX2 kernel set against our SSSE3-era vendored kernels
(capped at ~1-2% by the ablation — not the story). The next big structural
lever on record is derive-bS-at-decode + row-interleaved filtering.

---

## Part 19 — Entropy bins: the prep measurement and the named brick

First-ever bin-level census (profile-gated counters in the engine itself),
stockholm high, 60 frames:

- **30,756,716 bins**: 26.4M context-coded decisions, 3.9M bypasses, 0.44M
  terminates (one per MB, as the spec requires).
- Entropy stage + syntax stage ≈ 190 ms (profile build) over those bins →
  **~5-6 ns/bin, roughly 2× ffmpeg's asm engine** — the same multiple as the
  whole-decoder gap. Per-bin ARITHMETIC is at parity (Part 7); per-bin COST is
  not.

**The named brick: state residency.** `decode_decision` reads and writes
`range`/`offset`/`window`/`wbits` through `&mut self` — up to four
memory-resident fields round-tripped per bin, 26.4M times — while ffmpeg's
engine pins that state in registers across an entire residual block. The Rust
shape for the same effect: a by-value `CabacFast` view (fields in locals),
constructed at residual-block entry, `#[inline(always)]` bin methods, one
write-back at block exit. Mechanical, safe, byte-identical by construction;
sizing: even 1-2 ns/bin recovered ≈ 26-50 ms/run ≈ **3-7% of decode** — a
banked-brick-sized prize. Secondary: batch the EG/suffix bypass runs through
`decode_bypass_bits`-style windowed reads (3.9M bypasses, smaller).

Terminate cost is real but fixed by the spec (one per MB); the sig-map loop's
serial bin dependency is information-theoretic and not recoverable.

Row-interleave exploration: ordering proofs, the unfiltered-top-row backup
requirement, the ~15-20-site intra redirect inventory, and the R1-R4 staged
plan now live in `docs/row-interleave-plan.md` (R1 landed in Part 17).

---

## Part 20 — R1-R4 hammered: the decoder now has x264's single-pass shape

All four stages of `docs/row-interleave-plan.md` resolved:

- **R1** (Part 17): fused rolling-window precompute — superseded by R2.
- **R2 LANDED**: bS derives INSIDE the decode loop. A `row_hook` at both
  entropy loops' heads derives each row the moment it completes, from
  just-written (hot) grids, through the same `pack_mb`/`derive_mb_records`
  core, into a per-frame strength store. Mid-row slice ends and error paths
  fall back to a picture-start remainder loop in `deblock()`.
- **R3 LANDED**: row-interleaved FILTERING. Each completed row filters
  immediately (spec raster per-MB order preserved — proven in the plan doc,
  so byte-identity is by construction), with the one intra hazard handled as
  designed: one unfiltered backup row per plane saved before filtering, and
  all 14 inventoried intra top-neighbour reads routed through
  `top_y_px/top_y_row/top_c_px/top_c_row` helpers whose `flt_rows` gate makes
  them compile to the plain read when the interleave is off. Per-slice
  enable latching handles mixed-flag pictures by falling back to the tail.
  The full corpus gate passed FIRST RUN — 9/9 byte-identical with the
  interleave ON (a single missed redirect would corrupt intra prediction),
  conformance 160/160, workspace 145/145, `RS_H264_ROWDB=0` fallback intact.
- **R4 DECLINED by evidence**: per-MB record building would touch every
  MB-exit point in both entropy loops for reads that are already ≤1 row old;
  the profile shows the relocated derivation invisible against the residue.
  Risk asymmetry says stop.

Profile shape after: the deblock stage is **6.4%** (was 11.9%) and now
contains ONLY filtering; derivation lives in the decode loop where its inputs
are hot. Timing on both content axes: row-interleave faster in **7/11 on
each** (z=-0.90 each, pooled 14/22), medians mixed under heavy load — under
the bar, kept as the architectural default: this is x264's single-pass shape,
the second full-frame pixel re-walk is gone (certain locality counter), and
both axes lean positive with no regression evidence.

---

## Part 21 — CabacFast refuted by the symbol table; the gap holds at ~2.1-2.35×

**CabacFast (state residency): REFUTED, the cheap way.** `#[inline(always)]`
on the engine ops A/B'd null (6/11, contradictory robust statistics) — and the
symbol table explains why: `decode_decision` has ZERO outlined copies in the
un-attributed binary. LLVM was already fully inlining the engine; the state
was already promotable; the attribute was a no-op. The Part 19 sizing is also
corrected: the ~5-6 ns/bin figure carried the bin census's own atomic
`fetch_add` per bin (~2 ns of instrument tax — the profiler-tax law at bin
granularity). **True engine cost ≈ 4 ns/bin.** The residual factor vs
ffmpeg's ~2 ns/bin is per-bin WORK — our u64-window renorm does more
bookkeeping per bin (window shift + wbits + refill test) than ffmpeg's
16-bit-low lazy-refill shape — not call overhead. That reshape (a different
refill contract with the same bit output, gated by the zero-fill oracle) is
the honest entropy-bins prep target, and it is engine surgery, not plumbing.

**Side-by-side vs ffmpeg** (same harness as Parts 18: pinned CPU time,
`-threads 1`, 1800-frame parity, 9 pairs each, all z=3.00), under a heavier
box load than the Part 18 run:

| stream | Part 18 | now | note |
|---|---|---|---|
| long_cavlc | 1.983× | 2.107× | both arms ~10% load-inflated |
| long_main | 2.194× | 2.348× | |
| long_high | 2.213× | 2.153× | |

Run-to-run band on this box is ±0.15; the two measurements agree on a
**~2.0-2.35× gap**, and today's structural bricks (row-interleave, kernels)
individually measured null-to-positive — consistent. The campaign total from
the 2026-08-01 baseline (2.62/2.88/2.85×) stands at roughly **-20-25% of
runtime**, all byte-identical throughout.

---

## Part 22 — Prometheus deployed on the CABAC table; refuted at the domain level

The sibling refinery (`remade_ffmpeg_rs/Prometheus`) — built to replace
lookup tables with discovered, proven closed forms — now has two `rs_h264`
targets wired end-to-end (`prom distill --target cabac-lps|deblock-alpha`),
its first cross-repo deployment. Motivation: Part 21 located the engine's
per-bin critical path on the rangeTabLPS L1 load (~26M loads/clip).

Verdict, and it is the strong kind: **Table 9-44 admits no simple exact
closed form.** Symbolic regression found only trivia, and the decisive probe
was direct — the spec's own generative law `round(K_q · α^σ)`,
α = (0.01875/0.5)^(1/63), mismatches **86 of 256 entries**, because the
published table was hand-adjusted (q=0 head rows clamped at 128, terminal row
special-cased). A conformant decoder needs exactness, so the load cannot be
formula'd away at any price below the load itself. The LPS table access is
now REFUTED as an optimization surface (ledgered in Prometheus so it stays
refuted); the engine's remaining headroom is confined to the renorm/refill
shape (Part 21's lazy-refill candidate).

---

## Part 23 — The renorm/refill reshape: fused-low engine LANDED

The Part 21/22 conclusion — the engine's remaining headroom is the
renorm/refill SHAPE — is now built. The offset register and the bit window
are ONE u64: `low = codIOffset · 2^41 + buffered bits`, stream bits
left-aligned directly below the offset field.

What that deletes from the per-bin serial chain: the old renorm did
`offset = (offset << n) | take(n)` — a window shift, a `wbits` check and
update, and a merge, every bin. Now renorm is `low <<= n` (the next bits
enter the offset field by construction) plus a rarely-taken refill check
(4 bytes, lasts ~30 bins). The bin decision's compare/subtract runs against
`range << 41` — a constant shift — with the same branchless sign-mask trick
widened to 64 bits.

Exactness is by proven invariant, not hope: `offset ≥ range ⟺
low ≥ range·2^41` (buffered bits can never flip the comparison, `buf < 2^41`);
the masked LPS subtract cannot borrow across bit 41; `cnt ≤ 38 < 41` keeps
the buffer clear of the offset field; zero-fill past the stream end is
preserved (the fuzzer bound depends on it). The openh264-format trace and
`dbg_state` read the offset back as `low >> 41`, so the oracle surface is
unchanged.

Gates: CABAC encoder→decoder roundtrip suite green, byte-identical 9/9,
conformance 160/160, workspace 145/145 (fresh mtime).
Timing vs the pre-reshape binary: 6/11 under ~60%-inflated load — but median
(+3.6%) and best-of (+3.1%) AGREE, and both sit inside the predicted band
(3-4 serial ops removed from ~15 per bin × 30.7M bins ≈ 3-5% of decode).
Kept on the counter with the clock's two robust statistics concurring; the
first engine-shape change of the campaign, and with the LPS table refuted
(Part 22) and the arithmetic serial by information theory, likely the last.

---

## Part 24 — Inside the serial dependency: one refutation by counter, one brick by late-load removal

Asked to find wins INSIDE the arithmetic coder's serial chain, the survey
produced exactly the campaign's texture — a doomed idea killed for free and a
real op removed:

1. **Renorm-skip branch — REFUTED BY COUNTER before building.** The candidate:
   the branchless renorm pays lzcnt + two shifts + cnt update + refill check
   on EVERY bin, including bins that need no renormalization — so branch
   around it. The new census counter answered first: renorm fires on **46.2%
   (high) / 55.6% (main) of decision bins** — a coin-flip branch, the worst
   possible prediction case, and precisely why H-35 went branchless
   originally. No arm was ever built or timed; the counter refuted it for the
   cost of one profile run.

2. **Fused entry table — LANDED.** The chain's LAST op was a LATE load: the
   transition table's address needs the LPS/MPS mask, which exists only after
   the compare, so the context write-back — and every same-context successor
   bin, i.e. all unary and level-prefix loops — waited on a ~5-cycle load
   issued at the chain's end. `FUSED[q*128+s] = lps | trans_mps<<8 |
   trans_lps<<16` makes both transitions arrive WITH the early lps load; the
   post-compare step is now a 1-cycle shift-select
   (`(e >> (8 + (mask & 8))) & 0xFF`). 2 KB, same L1 class as the two tables
   it replaces on this path. Gates: roundtrip suite, byte-identical 9/9,
   conformance 160/160, workspace 145/145. Clock: 5/11 in ±24% chaos, median
   +2.2% / best-of -4.2% (disagreeing) — null; kept on the counter (one load
   per decision bin certainly removed, no structural downside).

3. **Bypass-run division — declined by population.** Multi-bypass decode via
   one division beats serial compare-subtract only for runs ≥ ~6 bits; the
   3.9M bypasses are dominated by single sign bins and short EG suffixes on
   this corpus. Not built.

With the LPS table refuted at domain level (Part 22), the refill shape taken
(Part 23), call overhead refuted (Part 21), the renorm branch refuted by
counter, and the late load now removed, the per-bin serial chain is down to
its irreducible core: ctx load → lps select → compare → masked update. The
engine survey is CLOSED.

---

## Part 25 — Entropy decoupling E1: the seam is in, gated, and (surprisingly) not even a cost

`docs/entropy-decouple-plan.md` written (the enabling facts, each verified:
parsing needs no pixels; B temporal direct needs the co-located frame's
PARSE product, not its pixels; intra is the ONE pixel coupling at ~3.6% of
MBs). Stage E1 landed: the CABAC P path's pixel work — `recon_p_inter` (the
whole MC-staging + residual-add block) and `recon_p_skip` — is extracted
behind a defer-and-flush job queue (`EdcJob`, ~2.6 KB per inter MB). Flush
points: before any intra MB (pixel reads), before row filtering, at B-branch
entry, at slice end, and a `deblock()` backstop. Grid commits stay at parse
time (later MBs' MV prediction reads them); the job re-gathers its own
committed block MVs at replay (stable after commit — E2 will carry copies).

Byte-identity is by ordering argument — replay order equals inline order at
every pixel-observable point — and the gates agree: **9/9 byte-identical on
BOTH knob arms, conformance 160/160 on both, workspace 145/145.**
`RS_H264_EDC=1` opts in.

**The seam measured FREE-to-positive**: the seam-ON arm was faster in 6/9,
median +6.2% (z=1.00, under bar, ±19% range). The expected job-copy cost did
not appear; the plausible mechanism is LOOP FISSION — batching all parse then
all recon per row keeps each giant code path's I-cache and branch state hot,
instead of alternating them per MB. Not banked; noted as a tailwind for E2,
whose thread now starts from a seam that costs nothing.

Next: E2 — the flush boundary becomes a channel to a scoped worker owning
the pixel side; parse of row r+1 overlaps reconstruction of row r.

---

## Part 26 — E2's win came early: the E1 seam BANKS at z=2.84, default ON

The E2 hammer stopped at its own gate: threading demands re-homing ~400 lines
of reconstruction onto an ownable pixel context, and doing that on top of the
session's giant uncommitted tree is how wins get lost. The deciding
measurement came first instead — and the seam did not need the thread:

**BANKED: 13/15 pairs, z=2.84, median +4.0% (pooled with the first run:
19/24, z=2.86).** The E1 defer-and-flush seam — expected to be cost-neutral
scaffolding — is a win on its own. Mechanism: LOOP FISSION. Batching a row's
parsing and then a row's reconstruction keeps each large code path's I-cache
and branch-predictor state hot, instead of alternating two giant bodies every
macroblock. `RS_H264_EDC` default is now ON (`=0` opts out).

**The default flip earned its gate immediately.** The encoder's delta-QP
CABAC roundtrip stream failed where the 9/9 corpus passed: `recon_p_inter`
carried `j.qp` locally but `add_inter_residual` reads `self.cur_qp` — which
at flush time belongs to a LATER macroblock. The x264 corpus's near-constant
QP masked the bug completely; the adaptive-QP roundtrip caught it. Fixed with
save/set/restore around the replay; all 145 tests + 9/9 both arms + 160/160
re-green on the fixed binary. LESSON (recorded next to the
cross-axes law): a deferral seam's gate matrix must include a stream that
VARIES every piece of state the jobs snapshot — qp was snapshotted in the job
but not restored for the code under the replay.

Fourth banked brick of the campaign (grid pool z=-2.33, fusion round z=2.11,
MC scratch hoist z=2.71, EDC seam z=2.84). E2 (the thread — up to ~25% more)
proceeds next on a committed baseline, exactly as planned.

---

## Part 27 — The campaign's closing benchmark, and the READMEs now say it

Full head-to-head on the cleanest box conditions of the session (both arms at
their best CPU times; same harness, 1800-frame parity, `-threads 1`, 9 pairs,
all z=3.00) — the FIRST full run carrying the banked EDC seam:

| stream | ratio | ours | ffmpeg |
|---|---|---|---|
| long_cavlc | **1.981×** | 213 Mpx/s | 412 Mpx/s |
| long_main  | **2.160×** | 146 Mpx/s | 294 Mpx/s |
| long_high  | **2.057×** | 125 Mpx/s | 255 Mpx/s |

Against the 2026-08-01 published figures (2.34× / 2.70× / 2.49×): **-15% /
-20% / -17% of the remaining gap ratio**, and against the same-day baseline
before this campaign began, ~25-30% of decode runtime removed. Cross-run band
over the three full measurements: cavlc 1.98-2.11, main 2.16-2.35, high
2.06-2.21 — today's run sits at the favorable edge (calm box), the published
table uses it with the method + provenance notes attached.

READMEs updated: root `README.md` (restored from the pre-existing 185-line
truncation first), `crates/rusty_h264/README.md`,
`crates/rusty_h264-decoder/README.md`, and the codec-table row in
`../remade_ffmpeg_rs/readme.md`. Each carries a provenance note: same
harness, same streams as the old figures — the change is decoder speed.

---

## Part 28 — E2 BANKED: the decoder's first thread, +23.4% wall time on P content

The E2 worker is built exactly as the plan specified, entirely in safe Rust:

- **Ownership, not sharing.** `PixelCtx` OWNS the pixel side — planes, backup
  rows, DPB Arcs, scaling/weights clones, and its own qp/t8/bs grids fed by
  per-row messages. The parse thread keeps every syntax grid. Nothing is
  shared but three mpsc channels; no unsafe anywhere.
- **The recon ports** (recon_p_inter / recon_p_skip / add_inter_residual /
  save_bak / filter_row) moved onto `PixelCtx` with two principled edits: the
  job now CARRIES the committed block motion (the worker reads no grids), and
  the nnz/coded grid writes moved to a parse-side twin (`edc_commit_nnz`) —
  they are parse state (deblock derivation, CAVLC nC) and their equality with
  the recon-side recount is the Part 8 nnz-threading invariant itself.
- **Intra ping-pong.** An intra-in-P macroblock sends `NeedCtx`; the worker
  drains (FIFO) and ships the whole context over; parse installs the planes,
  runs the unchanged inline intra path, and gives the context back LAZILY at
  the next job/row/slice-end — consecutive intra MBs pay one round-trip.
- **Scope-per-slice.** `std::thread::scope` around each threaded P slice;
  error paths drop the sender, the worker drains and returns the context, the
  planes always come home. I and B slices take the inline path untouched.

Gates, all with the worker LIVE (`RS_H264_EDC_MT=1`): corpus **9/9
byte-identical**, conformance **160/160**, encoder delta-QP roundtrips
**39/39** (the suite that caught E1's bug, now exercising the ping-pong),
default arm 145/145, plus the purpose-built P-only CABAC stream byte-exact on
both arms.

**The measurement — wall time on TWO cores** (the harness change the plan
demanded: overlap is invisible to single-core CPU time), ABBA, 11 pairs,
P-only 720p main stream: **MT faster in 9/11, z=2.11, median +23.4%**, with
the eight steady-box pairs at 22-28% — the predicted ~25% overlap prize,
delivered. The CPU-time column shows the overhead story: worker arm ~14.5 s
CPU vs ~15.1 s single-threaded — the thread costs nothing in work; it only
reclaims wall time.

`RS_H264_EDC_MT=1` stays OPT-IN for now: threading policy belongs to the
embedder, and on B-heavy content the worker idles until E3 (B jobs — the same
machinery, region lists instead of a gmv block) extends coverage. Fifth
banked brick of the campaign, and the first that changes the decoder's SHAPE
rather than its arithmetic.

---

## Part 29 — E2 defaulted, E3 built: the worker now covers B slices

`RS_H264_EDC_MT` DEFAULT ON (the Part 28 bank). E3 extends the same machinery
to B slices — the corpus's dominant content:

- `b_mc` + `b_mc_chroma` ported onto `PixelCtx` with ONE semantic edit: the
  implicit bi-prediction weights become REGION DATA. Computing them reads the
  ref lists' POCs (parse-side state); each recorded region carries the
  resolved pair, and the worker's port takes it as a parameter. Same
  function, same values, computed on the side that owns the inputs.
- `b_mc_or_record` wraps all six MC call sites (CABAC B body, spatial +
  temporal direct, and the CAVLC B body, whose inline arm it preserves
  untouched): in threaded mode a `BRegion` is recorded; inline, the original
  `b_mc` runs.
- A `BJob` carries the region list + the residual arrays; the B body's
  residual-add site and `decode_b_skip`'s pixel copy dispatch it; the nnz
  clears and `edc_commit_nnz` stay parse-side, as in E2.
- Intra-in-B reuses the E2 ping-pong unchanged (the flush site is shared).

Gates: **10/10 byte-identical on BOTH knob arms** — and on this B-heavy
corpus that is the hardest available exercise of the port: spatial and
temporal direct, B_Skip, bi-prediction with implicit weights, sub-8×8
partitions, and intra-in-B all ran through the worker. Conformance 160/160.

### Part 29 verdict — E3 BANKED, and three findings the gates earned

**The fuzzer caught two real bugs before any user could:**
1. **A panic became a deadlock.** An unwind through the threaded wrapper
   skipped the cleanup lines; the channel sender lives in `self`, which
   outlives the unwind, so the worker never saw the channel close and the
   scope's join blocked forever — zero CPU, three sleeping threads, exactly
   what the wedged fuzzer showed. Fixed with a `catch_unwind` guard that
   cleans up, joins, RESTORES THE PLANES, then resumes the panic; the hang
   became a diagnosable 9-second failure with the repro printed.
2. **The recorder bypassed the malformed-stream armor.** `b_mc` clamps
   over-range ref indices BEFORE computing implicit weights;
   `b_mc_or_record` called `implicit_weights` with the raw indices — index
   out of bounds on a mutated stream. Fixed by mirroring the clamp sequence
   exactly. LESSON: when a seam re-orders derivation relative to execution,
   every piece of armor between them must move with the derivation.

**The measurement nearly buried a banked win.** The first B-heavy runs showed
MT LOSING at -4% to -41% wall — with the profiler proving the threaded work
identical. The cause was the harness: affinity mask 0xC is logical CPUs 2+3 —
HYPERTHREAD SIBLINGS of one physical core. The "second core" didn't exist.
On two separate physical cores (mask 0x14):

- **B-heavy long_high: MT faster 8/9, z=2.33, median +13.6% wall** — under a
  box so loaded the 1T arms ran 47-62 s, and the paired deltas held anyway.
- P-only (Part 28): +23.4%, z=2.11.

MEASUREMENT LAW (recorded for every future threading number): a multithread
wall measurement must pin to SEPARATE PHYSICAL cores — sibling logical CPUs
measure hyperthreading, not parallelism.

Also landed en route: slim `BSkip` jobs (regions only — the full job carried
2.6 KB of zeroed coefficient arrays for ~60% of B macroblocks). CPU overhead
of the worker under saturation is real (~+30% CPU for -14% wall on this box);
on an unloaded machine the overlap is nearer free (Part 28's P-only run:
worker arm used LESS CPU).

**Sixth banked brick.** `RS_H264_EDC_MT` DEFAULT ON; all gates green with the
worker live: corpus 10/10 both arms, conformance 160/160, workspace 145/145
INCLUDING the fuzzer.

---

## Part 30 — The user-experience comparison: 2 threads vs 2 threads

Same two physical cores, wall time, ABBA, clean binaries (the first attempt
measured a stale PROFILE build of our arm — 43-71 s absurdities — caught by
implausibility and the exe-mtime discipline; rerun clean):

| stream | ffmpeg 1T | ffmpeg 2T (est med) | ours 2T | ours/ffmpeg-2T |
|---|---|---|---|---|
| long_cavlc | 4.7 s | ~3.1 s | ~9.4 s | **3.53×** |
| long_main | 6.3 s | ~5.1 s | ~12.8 s | **2.71×** |
| long_high | 6.3 s | ~5.7 s | ~12.8 s | **2.77×** |

(7/7 ffmpeg-faster per stream; heavy foreign load compressed BOTH sides'
threading gains — ffmpeg's 2T scaling here is ~1.1-1.5× vs its ~1.9×
unloaded.)

**The honest reading.** At one thread each we are ~2.0-2.2× behind. At two
threads each the gap widens to ~2.7-3.5×, because ffmpeg's FRAME threading
converts a second core into ~1.9× while our PIPELINE threading converts it
into 1.15-1.3× — a structural difference, not a tuning one. Two specifics:
- CAVLC is the worst ratio (3.5×) because the E-series seam is CABAC-only —
  the CAVLC slice loop has no flush hooks and runs fully single-threaded.
- Per-thread efficiency (the 1T-each comparison) remains the fair measure of
  the CODEC; the 2T comparison measures the THREADING ARCHITECTURE, and
  ffmpeg's is a generation ahead.

**Roadmap consequence, stated plainly:** the next parallelism tier is
FRAME-level threading — decode N pictures concurrently with per-row
reference-progress signaling — which scales per core like ffmpeg's. The E2/E3
work is its foundation, not its rival: PixelCtx is the ownable per-picture
pixel state such a design hands each frame worker, and the parse/pixel split
is the intra-frame pipeline each of those workers keeps. Smaller item on the
same list: give the CAVLC loop the E-seam.
