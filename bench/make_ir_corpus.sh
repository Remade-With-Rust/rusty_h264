#!/usr/bin/env bash
# Build the INSTRUCTION-COUNT corpus: the standing three tool tiers (the same
# x264 recipe as decode_x264_speedtest.sh), encoded through ffmpeg's libx264 so
# a box without a standalone x264 can still produce them, into _ir/.
#
# Every stream gets its ffmpeg reference SHA256 (display-order I420) written
# beside it. callgrind_ir.sh refuses to count a stream whose decode does not
# reproduce that hash: a count over wrong pixels is not a count.
#
#   bash bench/make_ir_corpus.sh            # every clip in video-tests/clips
#   bash bench/make_ir_corpus.sh foreman_cif FourPeople_1280x720_60
#
# Frame caps follow video-tests/manifest.tsv (CIF 120, 720p 60) so the corpus
# is the one the manifest describes, not whatever length the y4m happens to be.
set -euo pipefail
cd "$(dirname "$0")/.."
FF=${FF:-ffmpeg}
OUT=_ir; mkdir -p "$OUT"

if [ $# -gt 0 ]; then CLIPS="$*"; else
  CLIPS=$(ls video-tests/clips/*.y4m 2>/dev/null | xargs -n1 basename | sed 's/\.y4m$//')
fi
[ -n "$CLIPS" ] || { echo "no clips in video-tests/clips (run video-tests/fetch_clips.sh)"; exit 1; }

frames_for() {
  awk -F'\t' -v n="$1" '$1==n {print $5}' video-tests/manifest.tsv
}

# tier : ffmpeg profile : preset : extra x264 params
CFGS=("cavlc:baseline:veryfast:cabac=0"
      "main:main:medium:"
      "high:high:slower:")

for clip in $CLIPS; do
  src="video-tests/clips/$clip.y4m"
  [ -f "$src" ] || { echo "missing $src"; continue; }
  n=$(frames_for "$clip"); n=${n:-60}
  for cfg in "${CFGS[@]}"; do
    IFS=: read -r tier prof preset extra <<<"$cfg"
    dst="$OUT/${clip}__${tier}.264"
    if [ ! -f "$dst" ]; then
      echo "encode $dst ($n frames)"
      # shellcheck disable=SC2086
      "$FF" -v error -y -i "$src" -frames:v "$n" -an -c:v libx264 -preset "$preset" \
        -profile:v "$prof" -qp 26 ${extra:+-x264-params "$extra"} -f h264 "$dst"
    fi
    if [ ! -f "$dst.ffsha" ]; then
      "$FF" -v error -i "$dst" -f rawvideo -pix_fmt yuv420p - | sha256sum | cut -d' ' -f1 > "$dst.ffsha"
      ffn=$("$FF" -hide_banner -loglevel info -threads 1 -i "$dst" -f null - 2>&1 | grep -o "frame= *[0-9]*" | tail -1 | tr -dc 0-9)
      echo "$ffn" > "$dst.fframes"
      echo "  ffmpeg: frames=$ffn sha=$(cut -c1-16 "$dst.ffsha")"
    fi
  done
done
ls -la "$OUT"/*.264
