# `video-tests` — the fixed speed-test corpus and the function-level analyzer

A pinned set of **real** source video plus a harness that measures rusty_h264 and
x264 side by side, function by function, across every preset, for both the
encoder and the decoder. The point is repeatability: the same pixels, the same
settings, every run, so two measurements taken weeks apart are comparable.

> H.264 is video-only, so there is no audio arm here. The corpus spans the axes
> that actually move encoder cost: resolution, motion, and texture detail.

## Layout

```
video-tests/
  fetch_clips.sh        # reproducible corpus fetch (HTTP range, not full downloads)
  manifest.tsv          # name, dims, fps, frame count, class, byte size, hash
  clips/                # the pixels (gitignored — regenerate with fetch_clips.sh)
  analyzer/             # the Rust harness (own workspace, pure Rust, path deps)
  run_analysis.sh       # drives the three passes end to end
  x264_instrument.py    # adds the rdtsc stage profiler to the x264 reference
  primitive_compare.py  # kernel-level: our SIMD primitives vs x264's checkasm
  results/              # speed.tsv, stages.tsv, primitives.tsv, REPORT.md (gitignored)
```

## The corpus

20 clips, ~1.4 GB, five resolution rungs × seven content classes. Sources are
Xiph's Derf collection (the standard codec-research corpus).

| rung | clips | frames |
|---|---|---|
| QCIF 176×144 | akiyo, foreman | 120 |
| CIF 352×288 | akiyo, foreman, mobile, bus, tempete, football | 120 |
| 4CIF 704×576 | city, crew, harbour, soccer | 120 |
| 720p 1280×720 | shields, stockholm, in_to_tree, FourPeople | 60 |
| 1080p 1920×1080 | ducks_take_off, park_joy, crowd_run, blue_sky | 60 |

Classes: `static` (skip/entropy dominated) · `medium` · `pan` (motion-search
heavy) · `detail` (residual/transform heavy) · `complex` · `fastmotion` (worst
case for ME) · `smooth` (low complexity).

`fetch_clips.sh` exploits the fact that y4m is uncompressed: the first N frames
are a contiguous byte prefix, so it HTTP-range-fetches exactly what it needs —
~1.4 GB instead of the ~11 GB of full source files. Re-running skips clips whose
byte length already matches, and `--verify` checks the corpus without fetching.

## The x264 reference

x264 is built from source **outside this repo** (`../_ref_x264`) and driven as an
external process. Nothing here compiles C or C++; the codec crates stay pure,
`forbid(unsafe_code)` Rust.

```sh
cd ../_ref_x264
bash build.sh          # x264.exe + checkasm8.exe   (stock — the throughput arm)
bash build.sh prof     # x264-prof.exe              (rdtsc taps — the breakdown arm)
```

`build.sh` stands in for `make` (not installed on this machine) and mirrors
x264's own Makefile object lists and pattern rules for the configured target.
`checkasm8.exe` passing all tests is the gate that the build's asm is correct.

Two binaries on purpose: measuring throughput on the instrumented build would tax
x264 with overhead our own profiler-off build does not pay.

`x264_instrument.py` adds a stage profiler mirroring
`rusty_h264-common/src/prof.rs` — one RAII scope per stage function, cycles and
call counts into static buckets, dumped at exit — so both encoders' breakdowns
are read the same way. It is idempotent and gated on `-DX264_PROF`.

## Running the analysis

```sh
bash video-tests/fetch_clips.sh        # once
bash video-tests/run_analysis.sh       # the whole corpus
CLIPS=foreman_cif,mobile_cif bash video-tests/run_analysis.sh   # a subset
python video-tests/primitive_compare.py   # kernel-level, on an idle machine
```

Three passes, because throughput and per-function breakdown cannot come from the
same binary — the profiler's rdtsc scopes inflate wall time:

1. `speed` (profiler **off**) → `results/speed.tsv`
2. `stages` (profiler **on**) → `results/stages.tsv`
3. `report` (merge) → `results/REPORT.md`

## What is controlled

* **Single-threaded on both sides** — per-function attribution against our
  single-threaded encoder core. x264's stage counters are non-atomic, so its
  profiled runs require `--threads 1` regardless.
* **Matched QP 26, matched keyint 60.** Rate control off on both sides.
* **x264 timing is its own reported encode-loop time**, not process wall clock.
  Process startup is 10–20 ms, which would swamp a 25 ms QCIF encode.
* **ffmpeg's decode bar is net of process startup**, measured once against a
  do-nothing invocation, for the same reason.
* **Quality is computed in-process**, frame index against frame index, after
  decoding both encoders' output with the same external ffmpeg. ffmpeg's own
  `psnr`/`ssim` filters pair by *timestamp*, and a raw Annex-B stream has no
  container timing — so ffmpeg assumes 25 fps, compares it against a 29.97 fps
  y4m misaligned, and reports a large entirely fictitious quality loss.
* **Two x264 feature arms.** `high` is stock x264 (its real defaults: CABAC,
  B-frames, 8×8 transform, weighted prediction). `baseline` clamps it to the
  toolset we implement by default. The `baseline` arm is the
  implementation-vs-implementation comparison; `high` is the real-world bar.

## Reading the stage numbers honestly

The two profilers partition their own encode loops, and the partitions do not
line up one-to-one (x264 codes entropy as a top-level stage; ours nests it inside
MB coding). So the report gives each side's own top-level partition *and* a
narrower functional comparison covering only work both sides genuinely measure,
normalised to ms per megapixel. Work that exists on one side only — x264's
half-pel prefilter and lookahead, our source copy — is listed, never folded into
a ratio.

Every fine-grained scope costs ~2 rdtsc reads, and our encode path opens ~1M of
them per clip, so the residue (`mgmt/other`) is partly the profiler measuring
itself. The report prints a self-calibrated `profiler-overhead(est)` line next to
it: when the residue matches the overhead, there is no hidden work left.
