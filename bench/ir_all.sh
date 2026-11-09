#!/usr/bin/env bash
# ONE run of the instruction-count campaign. Run INSIDE WSL, invoked the SAME
# way every time (the exactness ladder: a different shell invocation moves the
# count by a fixed ~950 Ir pedestal).
#
#   wsl -e bash -lc 'cd /mnt/e/coding/rusty_h264 && bash bench/ir_all.sh <tag> [prev-tag]'
#
# 1. refuses a binary older than any crate source (a stale binary turns every
#    verdict into a statement about old code);
# 2. IDENTITY: decodes every stream in _ir/ and _ir/feat/ to display-order I420
#    and requires the SHA256 ffmpeg produced; any mismatch or decode failure
#    aborts the run before anything is counted;
# 3. COUNT: callgrind on the seven corpus streams, in parallel (Ir does not
#    care how loaded the box is);
# 4. prints Ir per stream with the work anchors, the total, and the delta
#    against [prev-tag] if given.
#
# Raw callgrind files stay in _ir/runs/<tag>/ for bench/ir_cmp.py --fn.
set -uo pipefail
cd "$(dirname "$0")/.."
TAG=${1:?usage: ir_all.sh <tag> [prev-tag]}
PREV=${2:-}
BENCH=${BENCH:-$HOME/rh264-target/release/examples/decode_bench}
OUT=_ir/runs/$TAG
STREAMS="foreman_cif__cavlc foreman_cif__main FourPeople_1280x720_60__main FourPeople_1280x720_60__high 720p50_shields_ter__cavlc 720p50_shields_ter__main 720p50_shields_ter__high"

[ -x "$BENCH" ] || { echo "no binary at $BENCH"; exit 1; }
stale=$(find crates -name '*.rs' -newer "$BENCH" -print -quit)
[ -z "$stale" ] || { echo "STALE BINARY: $stale is newer than $BENCH"; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT"

# ---- identity (parallel, native speed) ----
check() {
  f=$1; t=$(mktemp)
  if ! "$BENCH" "$f" 1 out="$t" >/dev/null 2>&1; then echo "DECODE-FAIL $f"; rm -f "$t"; return; fi
  ours=$(sha256sum "$t" | cut -d' ' -f1); rm -f "$t"
  [ "$ours" = "$(cat "$f.ffsha")" ] || echo "DIFF $f"
}
export -f check; export BENCH
bad=$(ls _ir/*.264 _ir/feat/*.264 | xargs -P 12 -I{} bash -c 'check {}')
nid=$(ls _ir/*.264 _ir/feat/*.264 | wc -l)
if [ -n "$bad" ]; then echo "$bad"; echo "IDENTITY FAILED -- nothing counted"; exit 2; fi
echo "identity : $nid/$nid streams byte-identical to ffmpeg"

# ---- count (parallel) ----
for s in $STREAMS; do
  ( valgrind --tool=callgrind --callgrind-out-file="$OUT/$s.out" --dump-instr=no --collect-jumps=no \
      "$BENCH" "_ir/$s.264" 1 > "$OUT/$s.stdout" 2> "$OUT/$s.stderr" ) &
done
wait

tot=0
: > "$OUT/summary.tsv"
for s in $STREAMS; do
  ir=$(grep -E '^(summary|totals):' "$OUT/$s.out" | head -1 | awk '{print $2}')
  fr=$(grep -o 'frames=[0-9]*' "$OUT/$s.stdout" | head -1 | cut -d= -f2)
  px=$(grep -o 'px=[0-9]*' "$OUT/$s.stdout" | head -1 | cut -d= -f2)
  want=$(cat "_ir/$s.264.fframes")
  [ "$fr" = "$want" ] || { echo "WORK ANCHOR MOVED on $s: frames=$fr want=$want"; exit 3; }
  printf '%s\t%s\t%s\t%s\n' "$s" "$ir" "$fr" "$px" >> "$OUT/summary.tsv"
  tot=$((tot + ir))
done
printf 'TOTAL\t%s\n' "$tot" >> "$OUT/summary.tsv"
python3 bench/ir_cmp.py "$TAG" $PREV
