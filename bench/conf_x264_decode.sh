#!/usr/bin/env bash
#
# DECODER conformance gate: x264 encodes, WE decode, FFMPEG decodes, the two
# reconstructions must be BYTE-IDENTICAL.
#
# `conf_ffmpeg` is the mirror of this and gates the ENCODER (our stream -> ffmpeg's
# decoder). Nothing gated the DECODER against real third-party CABAC streams, which
# is why a CABAC inter defect survived: `long.264`'s md5 was being tracked for
# stability and never once compared to a reference.
#
# Sweeps entropy coder x GOP structure x QP, because the defect is invisible on
# CAVLC and on intra-only content — the axes that matter are exactly the ones a
# single-stream check collapses.
#
#   bash bench/conf_x264_decode.sh [clip.y4m] [frames]
#
# Exits non-zero if any configuration mismatches. Env: X264_BIN, FFMPEG_BIN.
set -uo pipefail
cd "$(dirname "$0")/.."

CLIP=${1:-video-tests/clips/foreman_cif.y4m}
FRAMES=${2:-8}
X264=${X264_BIN:-../_ref_x264/x264.exe}
FF=${FFMPEG_BIN:-ffmpeg}
BIN=target/release/rusty_h264.exe
[ -x "$BIN" ] || BIN=target/release/rusty_h264
[ -x "$BIN" ] || { echo "build first: cargo build --release -p rusty_h264-cli --features asm"; exit 1; }
command -v "$FF" >/dev/null || { echo "ffmpeg not on PATH"; exit 1; }
[ -x "$X264" ] || { echo "x264 not found at $X264 (set X264_BIN)"; exit 1; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
pass=0; fail=0; failed=()

# profile:extra-args — baseline is the CAVLC control that MUST stay exact, so a
# regression in the shared reconstruct path is distinguishable from a CABAC one.
for prof in "baseline:--no-cabac" "main:" "high:" "high8:--no-8x8dct"; do
  name=${prof%%:*}; extra=${prof#*:}
  pname=$name; [ "$name" = "high8" ] && pname=high
  # intra-only / P-only / IPB: the GOP axis that a single stream collapses.
  for gop in "intra:--keyint 1" "p:--keyint 12 --bframes 0" "ipb:--keyint 12 --bframes 2"; do
    gname=${gop%%:*}; gargs=${gop#*:}
    for qp in 22 27 32 37; do
      s="$TMP/s.264"
      # shellcheck disable=SC2086
      "$X264" $gargs $extra --qp "$qp" --frames "$FRAMES" --profile "$pname" \
        -o "$s" "$CLIP" >/dev/null 2>&1 || { echo "  x264 failed: $name/$gname/qp$qp"; continue; }
      "$BIN" decode --in "$s" --out "$TMP/ours.yuv" >/dev/null 2>&1
      "$FF" -v error -i "$s" -f rawvideo -pix_fmt yuv420p -y "$TMP/ff.yuv" >/dev/null 2>&1
      if cmp -s "$TMP/ours.yuv" "$TMP/ff.yuv"; then
        pass=$((pass+1))
      else
        fail=$((fail+1)); failed+=("$name/$gname/qp$qp")
      fi
    done
  done
done

echo "decoder conformance vs ffmpeg on x264 streams ($CLIP, ${FRAMES}f)"
echo "  PASS $pass   FAIL $fail"
if [ $fail -gt 0 ]; then
  printf '  mismatch: %s\n' "${failed[@]}"
  exit 1
fi
