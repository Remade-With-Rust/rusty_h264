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
