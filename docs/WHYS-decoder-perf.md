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
