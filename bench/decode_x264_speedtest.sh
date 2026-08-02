#!/usr/bin/env bash
#
# DECODE SPEED on x264-ENCODED streams — the benchmark that matters.
#
# `decode_speedtest.sh` measures decode of OUR OWN encoder's bitstreams. Those are
# a narrow, self-selected slice of H.264: mostly 16x16 partitions, one reference,
# I16x16-heavy intra. Real content is x264's, and x264 at medium/slower emits
# sub-8x8 partitions, multi-ref, B-pyramid, the 8x8 transform, weighted prediction
# and i4x4/i8x8 intra. Measured 2026-07-31, the difference is not cosmetic:
#
#     our own streams   1.65 - 3.69x behind ffmpeg
#     x264 streams      6.03 - 6.12x behind ffmpeg
#
# So this is the honest standing number, and the one to drive down.
#
# METHOD (see docs/WHYS-decoder-perf.md, and codec-six-whys-unknowns H-41/H-46):
#   * CPU time, not wall — this box runs pinned at 100% from unrelated processes,
#     and elapsed wall counts time spent descheduled (5x looser under load).
#   * Pinned to one core at High priority, arms ALTERNATED (ABBA) so drift and
#     warm-up bias cancel; reported as a paired win-rate with a z-score.
#   * Streams long enough (1800 frames) that per-invocation overhead is <1% of
#     BOTH arms — an overhead paid per run inflates the SHORTER arm by a larger
#     fraction, which is how a harness manufactures a ratio.
#   * FRAME COUNTS compared between arms; a mismatch VOIDS the comparison.
#
# Correctness is a precondition, not a separate concern: every stream is decoded
# by us and by ffmpeg and required byte-identical before it is ever timed. The
# spatial-direct/direct_8x8_inference defect (fixed 2026-07-31) was found exactly
# here — `--preset slower` streams that no conformance arm had ever produced.
#
# Usage:  cargo build --release --features asm
#         cargo build --release -p rusty_h264-decoder --features asm --example decode_bench
#         bash bench/decode_x264_speedtest.sh [pairs]
# Env: X264_BIN (default ../_ref_x264/x264.exe), FFMPEG_BIN.
set -uo pipefail
cd "$(dirname "$0")/.."

PAIRS=${1:-9}
X264=${X264_BIN:-../_ref_x264/x264.exe}
FF=${FFMPEG_BIN:-ffmpeg}
BENCH=target/release/examples/decode_bench.exe
[ -x "$BENCH" ] || BENCH=target/release/examples/decode_bench
CLI=target/release/rusty_h264.exe
[ -x "$CLI" ] || CLI=target/release/rusty_h264
[ -x "$X264" ] || { echo "x264 not found at $X264 (set X264_BIN)"; exit 1; }
[ -x "$BENCH" ] || { echo "build: cargo build --release -p rusty_h264-decoder --features asm --example decode_bench"; exit 1; }
command -v "$FF" >/dev/null || { echo "ffmpeg not on PATH"; exit 1; }

TMP=./_xbench; mkdir -p "$TMP"

# Real 720p content, three clips of different character (detail / foliage / pan).
CLIPS="720p50_shields_ter in_to_tree_420_720p50 720p5994_stockholm_ter"

# Three tool tiers. `high --preset slower` is the one that exercises sub-8x8
# partitions (`--partitions all` via the preset), multi-ref and B-pyramid.
CFGS=("cavlc:baseline:--no-cabac --preset veryfast"
      "main:main:--preset medium"
      "high:high:--preset slower")

echo "building x264 corpus…"
for clip in $CLIPS; do
  for cfg in "${CFGS[@]}"; do
    n=${cfg%%:*}; r=${cfg#*:}; prof=${r%%:*}; extra=${r#*:}
    # shellcheck disable=SC2086
    "$X264" $extra --qp 26 --profile "$prof" -o "$TMP/${clip}__${n}.264" \
        "video-tests/clips/$clip.y4m" >/dev/null 2>&1
  done
done

# CORRECTNESS FIRST — never time a stream we decode wrong.
fail=0
for f in "$TMP"/*__*.264; do
  "$FF" -v error -i "$f" -f rawvideo -pix_fmt yuv420p -y "$TMP/ff.yuv" 2>/dev/null || { echo "  FFREJ $(basename "$f")"; fail=1; continue; }
  "$CLI" decode --in "$f" --out "$TMP/ours.yuv" >/dev/null 2>&1
  cmp -s "$TMP/ours.yuv" "$TMP/ff.yuv" || { echo "  ***DIFF*** $(basename "$f") — decode is WRONG, timing it would be meaningless"; fail=1; }
done
rm -f "$TMP/ff.yuv" "$TMP/ours.yuv"
[ $fail -eq 0 ] || { echo "correctness failed; fix before benchmarking"; exit 1; }
echo "correctness: all streams byte-identical to ffmpeg"

for n in cavlc main high; do
  rm -f "$TMP/long_$n.264"
  for _ in $(seq 10); do for c in $CLIPS; do cat "$TMP/${c}__${n}.264" >> "$TMP/long_$n.264"; done; done
  ourf=$("$BENCH" "$TMP/long_$n.264" 1 2>/dev/null | grep -o "frames=[0-9]*" | cut -d= -f2)
  fff=$("$FF" -hide_banner -loglevel info -threads 1 -i "$TMP/long_$n.264" -f null - 2>&1 |
        grep -o "frame= *[0-9]*" | tail -1 | tr -dc 0-9)
  if [ "$ourf" != "$fff" ]; then
    echo "  $n: WORK MISMATCH ours=$ourf ff=$fff — comparison VOID"; continue
  fi
  echo
  echo "=== x264 $n — $ourf frames, 720p, pinned CPU time, $PAIRS pairs ==="
  # NOTE: `powershell -File` is FORBIDDEN here. In -File mode PowerShell parses the
  # remaining tokens as a command line, so ffmpeg's trailing `-` (from `-f null -`)
  # binds as a PARAMETER NAME and the whole call dies with "the value of argument
  # name is not valid" — or, worse, the arm launches with mangled arguments, exits
  # in milliseconds, and pinvs records a 0 that it reports as a DROPPED PAIR. That
  # is how this script silently produced ratios built from one or two samples.
  # `-Command` with an explicit @(...) array passes the arguments verbatim.
  W=$(pwd -W 2>/dev/null || pwd)
  powershell -NoProfile -ExecutionPolicy Bypass -Command \
    "& '$W/bench/pinvs.ps1' -AExe '$W/$BENCH' -AArgs @('$W/$TMP/long_$n.264','1') -ALabel rusty \
      -BExe '$FF' -BArgs @('-hide_banner','-loglevel','error','-threads','1','-i','$W/$TMP/long_$n.264','-f','null','-') \
      -BLabel ffmpeg -Pairs $PAIRS" | tail -4
done
