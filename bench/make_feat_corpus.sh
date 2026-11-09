#!/usr/bin/env bash
# FEATURE-COVERAGE identity corpus for the instruction-count campaign.
#
# The seven `_ir/*.264` streams are what we COUNT on; they are three x264
# presets on three clips, which leaves whole syntax paths unexercised: multiple
# slices, temporal direct, constrained intra, scaling matrices, eight
# references, CAVLC B-frames, cropping, deblock offsets and deblock off, all-intra,
# and the two QP extremes. Each of those is one small stream here (CIF, 30
# frames), decoded by ffmpeg once to record the reference SHA256, and decoded
# by us on EVERY run of bench/ir_all.sh. A brick that breaks any of them fails
# the run before a single instruction is counted.
#
#   bash bench/make_feat_corpus.sh
set -euo pipefail
cd "$(dirname "$0")/.."
FF=${FF:-ffmpeg}
OUT=_ir/feat; mkdir -p "$OUT"
SRC=video-tests/clips/foreman_cif.y4m
HD=video-tests/clips/FourPeople_1280x720_60.y4m
[ -f "$SRC" ] || { echo "missing $SRC"; exit 1; }

# name | source | frames | ffmpeg video args
CFGS=(
  "mslice_cabac|$SRC|30|-profile:v main -preset medium -qp 26 -x264-params slices=4"
  "mslice_cavlc|$SRC|30|-profile:v baseline -preset veryfast -qp 26 -x264-params slices=4:cabac=0"
  "mslice_hd|$HD|20|-profile:v high -preset medium -qp 28 -x264-params slices=6"
  "tdirect|$SRC|30|-profile:v high -preset medium -qp 26 -x264-params direct=temporal:bframes=3"
  "cintra|$SRC|30|-profile:v main -preset medium -qp 26 -x264-params constrained-intra=1"
  "noweight|$SRC|30|-profile:v high -preset medium -qp 26 -x264-params weightp=0:8x8dct=0:b-pyramid=none:bframes=2"
  "cqm|$SRC|30|-profile:v high -preset medium -qp 26 -x264-params cqm=jvt"
  "refs8|$SRC|30|-profile:v high -preset slower -qp 26 -x264-params ref=8:b-pyramid=strict:bframes=5"
  "cavlc_b|$SRC|30|-profile:v main -preset medium -qp 26 -x264-params cabac=0:bframes=3"
  "veryslow|$SRC|30|-profile:v high -preset veryslow -qp 26"
  "intra|$SRC|12|-profile:v high -preset medium -qp 26 -x264-params keyint=1"
  "qp10|$SRC|12|-profile:v high -preset medium -qp 10"
  "qp45|$SRC|30|-profile:v high -preset medium -qp 45"
  "deblock_off|$SRC|30|-profile:v high -preset medium -qp 26 -x264-params deblock=-3,2"
  "nodeblock|$SRC|30|-profile:v main -preset medium -qp 26 -x264-params no-deblock=1"
  "crop|$SRC|30|-profile:v main -preset medium -qp 26 -vf scale=344:282"
  "cavlc_wp|$SRC|30|-profile:v baseline -preset slow -qp 30 -x264-params cabac=0:ref=4"
)

for c in "${CFGS[@]}"; do
  IFS='|' read -r name src n args <<<"$c"
  [ -f "$src" ] || { echo "skip $name: missing $src"; continue; }
  dst="$OUT/$name.264"
  if [ ! -f "$dst" ]; then
    # shellcheck disable=SC2086
    "$FF" -v error -y -i "$src" -frames:v "$n" -an -c:v libx264 $args -f h264 "$dst"
  fi
  if [ ! -f "$dst.ffsha" ]; then
    "$FF" -v error -i "$dst" -f rawvideo -pix_fmt yuv420p - | sha256sum | cut -d' ' -f1 > "$dst.ffsha"
    "$FF" -hide_banner -loglevel info -threads 1 -i "$dst" -f null - 2>&1 \
      | grep -o "frame= *[0-9]*" | tail -1 | tr -dc 0-9 > "$dst.fframes"
  fi
  printf '%-14s %8s B  frames=%s  sha=%s\n' "$name" "$(stat -c %s "$dst")" "$(cat "$dst.fframes")" "$(cut -c1-16 "$dst.ffsha")"
done
