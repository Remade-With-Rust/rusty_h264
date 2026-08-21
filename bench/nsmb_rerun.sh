#!/usr/bin/env bash
# ns/MB RERUN — ours vs ffmpeg on the x264-DEFAULT streams, per clip.
#
# Reproduces the `vs_ffmpeg_default` sheet of docs/big-oppy-decoder-truthtable.xlsx,
# whose own method note says its timings were taken on a LOADED box and are
# "ORDINAL use only; re-baseline on sustained-quiet". This is that re-baseline.
#
# Protocol (unchanged from the sheet where it matters):
#   * x264-default streams, concatenated until OUR arm exceeds TARGET_MS so that
#     per-invocation overhead is <1% of both arms (a fixed overhead inflates the
#     SHORTER arm more, which is how a harness manufactures a ratio).
#   * FRAME-COUNT PARITY checked per clip before timing; a mismatch VOIDS the clip
#     (the count-what-the-clock-charges law).
#   * Pinned CPU time, ABBA-alternated pairs via bench/pinvs.ps1, median per arm.
#   * ns/MB = median CPU ms * 1e6 / total macroblocks decoded, where total MBs
#     comes from decode_bench's own px counter (px/256) times the concat factor.
set -uo pipefail
cd "$(dirname "$0")/../../../../../../coding/rs_h264" 2>/dev/null || cd /c/Users/talmo/coding/rs_h264

OURS=${OURS:-$1}
PAIRS=${PAIRS:-7}
TARGET_MS=${TARGET_MS:-1500}
FF=${FFMPEG_BIN:-ffmpeg}
TT=_xbench/tt
OUT=${OUT:-_nsmb}
mkdir -p "$OUT"
W=$(pwd -W 2>/dev/null || pwd)
# `$OURS` may already be absolute (a scratchpad snapshot); only prefix a relative path.
case "$OURS" in
  /*|?:*|*:\*) OURS_ABS="$OURS" ;;
  *) OURS_ABS="$W/$OURS" ;;
esac

# class:clip  — exactly the rows of the vs_ffmpeg_default sheet.
ROWS="static:FourPeople_1280x720_60
static:akiyo_cif
medium:foreman_cif
medium:in_to_tree_420_720p50
detail:720p50_shields_ter
detail:mobile_cif
pan:720p5994_stockholm_ter
pan:bus_cif
complex:crew_4cif
complex:tempete_cif
fastmotion:crowd_run_1080p50
fastmotion:football_cif
smooth:blue_sky_1080p25
grain:grain_akiyo
grain:grain_flat
screen:screen_text
screen:screen_ui"

printf "%-11s %-26s %10s %10s %8s %8s\n" class clip ours_ns_MB ff_ns_MB gap frames
echo "------------------------------------------------------------------------------------"

for row in $ROWS; do
  cls=${row%%:*}; clip=${row#*:}
  src="$TT/${clip}__default.264"
  if [ ! -f "$src" ]; then echo "MISSING $src" >&2; continue; fi

  # --- probe one pass: frames, pixels, ms ---
  probe=$("$OURS" "$src" 3 2>/dev/null | tail -1)
  frames=$(echo "$probe" | grep -o "frames=[0-9]*" | cut -d= -f2)
  px=$(echo "$probe" | grep -o "px=[0-9]*" | cut -d= -f2)
  ms=$(echo "$probe" | grep -o "best=[0-9.]*ms" | tr -dc '0-9.')
  if [ -z "$frames" ] || [ -z "$px" ] || [ -z "$ms" ]; then echo "PROBE FAIL $clip" >&2; continue; fi

  # --- concat until our arm clears TARGET_MS ---
  n=$(python -c "import math,sys; print(max(1, math.ceil($TARGET_MS/float($ms))))")
  long="$OUT/${clip}__default_x$n.264"
  if [ ! -f "$long" ]; then
    : > "$long"
    for _ in $(seq "$n"); do cat "$src" >> "$long"; done
  fi

  # --- WORK PARITY: both decoders must process the same frame count ---
  of=$("$OURS" "$long" 1 2>/dev/null | grep -o "frames=[0-9]*" | cut -d= -f2)
  ff=$("$FF" -hide_banner -loglevel info -threads 1 -i "$long" -f null - 2>&1 |
       grep -o "frame= *[0-9]*" | tail -1 | tr -dc 0-9)
  if [ "$of" != "$ff" ]; then
    printf "%-11s %-26s   WORK MISMATCH ours=%s ff=%s -- VOID\n" "$cls" "$clip" "$of" "$ff"
    continue
  fi

  # --- pinned alternating pairs; take each arm's median CPU ms ---
  line=$(powershell -NoProfile -ExecutionPolicy Bypass -Command \
    "& '$W/bench/pinvs.ps1' -AExe '$OURS_ABS' -AArgs @('$W/$long','1') -ALabel rusty \
      -BExe '$FF' -BArgs @('-hide_banner','-loglevel','error','-threads','1','-i','$W/$long','-f','null','-') \
      -BLabel ffmpeg -Pairs $PAIRS" | grep "median CPU")
  ocpu=$(echo "$line" | sed -n 's/.*rusty median CPU \([0-9,]*\) ms.*/\1/p' | tr -d ,)
  fcpu=$(echo "$line" | sed -n 's/.*ffmpeg median CPU \([0-9,]*\) ms.*/\1/p' | tr -d ,)
  if [ -z "$ocpu" ] || [ -z "$fcpu" ]; then
    printf "%-11s %-26s   PARSE FAIL: %s\n" "$cls" "$clip" "$line"
    continue
  fi

  python - "$cls" "$clip" "$ocpu" "$fcpu" "$px" "$n" "$of" <<'PY'
import sys
cls, clip, ocpu, fcpu, px, n, frames = sys.argv[1:8]
mbs = (int(px) / 256.0) * int(n)
o = float(ocpu) * 1e6 / mbs
f = float(fcpu) * 1e6 / mbs
print("%-11s %-26s %10.0f %10.0f %7.2fx %8s" % (cls, clip, o, f, o / f, frames))
PY
done
