#!/usr/bin/env bash
#
# THE DECODER BENCHMARK -- one entrypoint, reproducible end to end.
#
#   bash bench/decode_benchmark.sh            # full run: build, corpus, gate, measure
#   bash bench/decode_benchmark.sh --report   # re-report from the saved raw, no re-run
#
# Produces:
#   bench/_map/decode_anatomy.raw     raw instrument output (the archive)
#   bench/_map/decode_anatomy.txt     the rendered report
#
# ---------------------------------------------------------------------------
# WHY IT IS SHAPED THIS WAY. Each rule below was paid for.
#
# 1. TWO BINARIES. The stage profiler is an rdtsc pair per scope entered tens of
#    millions of times; measured tax here was 1.3-2.5x typical and up to 19x on one
#    stream. So the profiled build NEVER supplies a speed number. Honest wall comes
#    from a build with `profile` compiled OUT; the profiled build only ranks stages
#    and reports call counts.
#
# 2. COUNTS ARE THE DURABLE HALF. Counting is a relaxed add, not a timer, so counts
#    are unaffected by sampling period, machine load or drift. Verified: 538/538
#    stage-count checks identical across 4 instrumented passes per stream. Where a
#    count and a time disagree, the count wins.
#
# 3. SAMPLED + EXACT, BOTH. `RS_H264_PROF_SAMPLE=64` times 1 call in 64 and scales
#    survivors back up, cutting the tax ~64x while leaving SHARES unbiased. Running
#    exact AND sampled gives two independent instruments; the report flags any stage
#    where they disagree by >3 points. Two instruments agreeing is the standard of
#    evidence, one instrument is a hypothesis.
#
# 4. THREE SAMPLED PASSES, MEDIANED. A single instrumented pass inherits this box's
#    drift; the first version of this harness reported the residue at 4.5% (sampled)
#    against 36.8% (exact) on the same stream. Per-stream medians fixed it.
#
# 5. CORRECTNESS BEFORE TIMING. Every stream is decoded by us and by ffmpeg and
#    required byte-identical. Timing a stream we decode wrong is meaningless, and this
#    gate is how the spatial-direct defect was originally found.
#
# 6. LOOPED TO 300 FRAMES. The corpus tops out at 60-120 frames. decode_prof times
#    decode_stream() only (process start is outside the measured region), but longer
#    streams still stabilise the number. Looping is legitimate for SPEED and is stated.
#
# 7. ABSOLUTE Mpx/s IS NOT STABLE RUN-TO-RUN. Two identical runs of this harness gave
#    180.4 and 143.2 Mpx/s for shields/cavlc -- this box drifts ~25% between runs.
#    Compare COUNTS and WITHIN-RUN ratios across time; do not track absolute Mpx/s.
# ---------------------------------------------------------------------------
set -uo pipefail
cd "$(dirname "$0")/.."

T=${DPROF_DIR:-_dprof}
N=${FRAMES:-300}
X=${X264_BIN:-../_ref_x264/x264.exe}
FF=${FFMPEG_BIN:-ffmpeg}
CLI=target/release/rusty_h264.exe
RAW=bench/_map/decode_anatomy.raw
OUT=bench/_map/decode_anatomy.txt

CLIPS="720p50_shields_ter in_to_tree_420_720p50 720p5994_stockholm_ter mobile_cif bus_cif crowd_run_1080p50"
# Three tool tiers so the anatomy is read against the entropy coder and the partition
# set, not one arbitrary encode. `high --preset slower` is the tier that exercises
# sub-8x8, multi-ref and B-pyramid.
CFGS=("cavlc:baseline:--no-cabac --preset veryfast"
      "main:main:--preset medium"
      "high:high:--preset slower")

if [ "${1:-}" = "--report" ]; then
  python bench/decode_anatomy_report.py "$RAW" | tee "$OUT"
  exit 0
fi

echo "== build: honest (profiler OUT) and profiled (profiler IN) ==" >&2
cargo build --release --features asm 2>&1 | tail -1 >&2
cargo build --release -p rusty_h264-decoder --features asm --example decode_prof 2>&1 | tail -1 >&2
cp target/release/examples/decode_prof.exe /tmp/decode_prof_honest.exe
cargo build --release -p rusty_h264-decoder --features asm,profile --example decode_prof 2>&1 | tail -1 >&2
cp target/release/examples/decode_prof.exe /tmp/decode_prof_prof.exe

echo "== corpus: $N-frame looped sources + 18 x264 streams ==" >&2
mkdir -p "$T"
for clip in $CLIPS; do
  [ -f "$T/${clip}_$N.y4m" ] || "$FF" -v error -stream_loop 20 -i "video-tests/clips/$clip.y4m" \
      -frames:v "$N" -f yuv4mpegpipe -y "$T/${clip}_$N.y4m"
  for cfg in "${CFGS[@]}"; do
    n=${cfg%%:*}; r=${cfg#*:}; prof=${r%%:*}; extra=${r#*:}
    # shellcheck disable=SC2086
    [ -f "$T/${clip}__${n}.264" ] || "$X" $extra --qp 26 --profile "$prof" --keyint "$N" \
        -o "$T/${clip}__${n}.264" "$T/${clip}_$N.y4m" >/dev/null 2>&1
  done
done

echo "== correctness gate: every stream byte-identical vs ffmpeg ==" >&2
fail=0; ok=0
for f in "$T"/*__*.264; do
  "$FF" -v error -i "$f" -f rawvideo -pix_fmt yuv420p -y "$T/ff.yuv" 2>/dev/null \
    || { echo "  FFREJ $(basename "$f")" >&2; fail=1; continue; }
  "$CLI" decode --in "$f" --out "$T/ours.yuv" >/dev/null 2>&1
  if cmp -s "$T/ours.yuv" "$T/ff.yuv"; then ok=$((ok+1)); else
    echo "  ***DIFF*** $(basename "$f") -- decode is WRONG, timing it would be meaningless" >&2; fail=1
  fi
done
rm -f "$T/ff.yuv" "$T/ours.yuv"
echo "  byte-identical: $ok/$(ls "$T"/*__*.264 | wc -l)" >&2
[ "$fail" -eq 0 ] || { echo "CORRECTNESS FAILED -- refusing to report timings" >&2; exit 1; }

echo "== measure ==" >&2
bash bench/decode_anatomy.sh > "$RAW"
python bench/decode_anatomy_report.py "$RAW" | tee "$OUT"
