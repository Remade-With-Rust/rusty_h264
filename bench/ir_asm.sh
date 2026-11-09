#!/usr/bin/env bash
# Instruction-level census of ONE function: callgrind with --dump-instr=yes on the
# line-tables build ($HOME/rh264-target-dbg, built by ir_lines.sh), joined with
# objdump so every instruction of the function prints with its execution count.
# Answers "what does this function actually execute" when the line census
# attributes inlined code to library frames.
#   bash bench/ir_asm.sh <tag> <fn-substring> [stream] [min-count]
set -uo pipefail
cd "$(dirname "$0")/.."
TAG=${1:?usage: ir_asm.sh <tag> <fn-substring> [stream] [min-count]}
FN=${2:?fn substring}
S=${3:-720p50_shields_ter__cavlc}
MIN=${4:-100000}
BENCH=$HOME/rh264-target-dbg/release/examples/decode_bench
OUT=_ir/asm/$TAG; mkdir -p "$OUT"
if [ ! -s "$OUT/$S.out" ]; then
  valgrind --tool=callgrind --dump-instr=yes --collect-jumps=no \
    --callgrind-out-file="$OUT/$S.out" "$BENCH" "_ir/$S.264" 1 > /dev/null 2>&1
fi
python3 bench/ir_asm.py "$OUT/$S.out" "$BENCH" "$FN" "$MIN"
