#!/usr/bin/env bash
# Line-attributed census of the CURRENT tree: builds decode_bench with line
# tables (codegen unchanged) into $HOME/rh264-target-dbg, runs callgrind on the
# named streams, and prints the decoder-owned hot lines plus the self-cost table.
#   bash bench/ir_lines.sh <tag> [stream ...]
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
TAG=${1:?usage: ir_lines.sh <tag> [stream ...]}; shift
STREAMS=${*:-FourPeople_1280x720_60__main 720p50_shields_ter__cavlc 720p50_shields_ter__high}
BENCH=$HOME/rh264-target-dbg/release/examples/decode_bench
CARGO_TARGET_DIR=$HOME/rh264-target-dbg CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
  cargo build --release -p rusty_h264-decoder --example decode_bench > /tmp/irlines.build 2>&1 \
  || { tail -30 /tmp/irlines.build; echo BUILD FAILED; exit 1; }
OUT=_ir/lines/$TAG; mkdir -p "$OUT"
for s in $STREAMS; do
  ( valgrind --tool=callgrind --callgrind-out-file="$OUT/$s.out" --dump-instr=no --collect-jumps=no \
      "$BENCH" "_ir/$s.264" 1 > /dev/null 2>&1 ) &
done
wait
for s in $STREAMS; do
  python3 bench/callgrind_census.py "$OUT/$s.out" 90 > "$OUT/$s.txt"
  echo "=== $s"
  grep -A 90 'LINE CENSUS' "$OUT/$s.txt" | grep rusty_h264 | sed 's#/mnt/e/coding/rusty_h264/crates/##' | head -${TOP:-30}
done
