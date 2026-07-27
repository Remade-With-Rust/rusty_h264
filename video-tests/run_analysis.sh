#!/usr/bin/env bash
# Drive the full function-level analysis: rusty_h264 vs x264, every preset,
# encoder + decoder, on the fixed video-tests corpus.
#
#   bash video-tests/run_analysis.sh                 # whole corpus
#   CLIPS=akiyo_cif,foreman_cif bash .../run_analysis.sh   # a subset
#
# Three passes, because throughput and per-function breakdown cannot come from
# the same binary — the profiler's rdtsc scopes inflate wall time:
#   1. speed   (profiler OFF)  -> results/speed.tsv
#   2. stages  (profiler ON)   -> results/stages.tsv
#   3. report  (merge)         -> results/REPORT.md
set -eu
cd "$(dirname "$0")/analyzer"

X264_DIR="${X264_DIR:-../../../_ref_x264}"
if [ ! -x "$X264_DIR/x264.exe" ] || [ ! -x "$X264_DIR/x264-prof.exe" ]; then
  echo "!! x264 reference not built. Run:"
  echo "     cd $X264_DIR && bash build.sh && bash build.sh prof"
  exit 1
fi
if [ ! -d ../clips ]; then echo "!! no corpus — run video-tests/fetch_clips.sh"; exit 1; fi

echo "### 1/3  throughput (profiler OFF)"
cargo build --release -q
./target/release/analyzer speed

echo
echo "### 2/3  per-function breakdown (profiler ON)"
cargo build --release -q --features profile
./target/release/analyzer stages

echo
echo "### 3/3  report"
cargo build --release -q
./target/release/analyzer report

echo
echo "results in video-tests/results/ — REPORT.md, speed.tsv, stages.tsv"
