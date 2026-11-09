#!/usr/bin/env bash
# EXACT instruction count (callgrind Ir) of one decode. Run INSIDE WSL.
#
#   wsl -e bash -lc 'bash /mnt/e/coding/rusty_h264/bench/callgrind_ir.sh _ir/foreman_cif__main.264 [label]'
#
# Prints, in order:
#   1. the correctness anchor: our decode's SHA256 must equal the ffmpeg hash
#      make_ir_corpus.sh stored beside the stream (byte-identity first);
#   2. the work-parity anchors: frames= and px= from decode_bench, and
#      ffmpeg's frame count (a moved anchor voids the comparison);
#   3. TOTAL Ir -- same binary + same input = same number, to the instruction;
#   4. the four censuses (instruction-counting skill, section 5): self-cost by
#      function, call counts, caller->callee edges, and (if the binary carries
#      line tables) a line census.
#
# The binary is built OUTSIDE this script so a stale build cannot hide behind a
# grep: pass BENCH=... or let it default to the WSL-side target dir. The script
# refuses a binary older than any decoder source file.
set -uo pipefail
cd "$(dirname "$0")/.."
STREAM=${1:?usage: callgrind_ir.sh <stream.264> [label]}
LABEL=${2:-$(basename "$STREAM" .264)}
BENCH=${BENCH:-$HOME/rh264-target/release/examples/decode_bench}
OUTDIR=${OUTDIR:-_ir/cg}; mkdir -p "$OUTDIR"
CG="$OUTDIR/$LABEL.out"

[ -x "$BENCH" ] || { echo "no binary at $BENCH"; exit 1; }
newest_src=$(find crates -name '*.rs' -newer "$BENCH" | head -1)
[ -z "$newest_src" ] || { echo "STALE BINARY: $newest_src is newer than $BENCH -- rebuild"; exit 1; }

# 1. correctness anchor (display order, same path ffmpeg's rawvideo takes)
if [ -f "$STREAM.ffsha" ]; then
  tmp=$(mktemp); "$BENCH" "$STREAM" 1 out="$tmp" >/dev/null 2>&1
  ours=$(sha256sum "$tmp" | cut -d' ' -f1); rm -f "$tmp"
  want=$(cat "$STREAM.ffsha")
  if [ "$ours" != "$want" ]; then echo "***DIFF*** $STREAM: ours $ours != ffmpeg $want -- NOT COUNTED"; exit 2; fi
  echo "identity : ok ($(echo "$ours" | cut -c1-16))"
else
  echo "identity : (no $STREAM.ffsha beside the stream -- unverified)"
fi

# 2+3. the count
valgrind --tool=callgrind --callgrind-out-file="$CG" --dump-instr=no --collect-jumps=no \
  "$BENCH" "$STREAM" 1 > "$OUTDIR/$LABEL.stdout" 2> "$OUTDIR/$LABEL.stderr"
ir=$(grep -E '^(summary|totals):' "$CG" | head -1 | awk '{print $2}')
anchors=$(grep -o 'frames=[0-9]* px=[0-9]*' "$OUTDIR/$LABEL.stdout" | head -1)
ff=$( [ -f "$STREAM.fframes" ] && cat "$STREAM.fframes" || echo '?')
arm=$(grep '^arm' "$OUTDIR/$LABEL.stdout" | head -1)
printf '%-28s Ir %16s   %s ffmpeg-frames=%s   %s\n' "$LABEL" "$(printf "%'d" "$ir")" "$anchors" "$ff" "$arm"

# 4. censuses
python3 "$(dirname "$0")/callgrind_census.py" "$CG" "${TOP:-40}"
