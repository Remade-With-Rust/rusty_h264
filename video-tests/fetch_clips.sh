#!/usr/bin/env bash
# Fetch the rs_h264 speed-test corpus: real source video, fixed frame counts, so
# every speed run measures the SAME pixels. Idempotent — re-running skips clips
# that already match their expected byte length.
#
# The sources are uncompressed YUV4MPEG2, so the first N frames are a contiguous
# byte prefix: we HTTP-range-fetch exactly `header + N*(6 + w*h*3/2)` bytes
# instead of downloading the full multi-GB file. ~1.7 GB total instead of ~11 GB.
#
#   bash video-tests/fetch_clips.sh          # fetch anything missing
#   bash video-tests/fetch_clips.sh --verify # re-check sizes/frame counts only
set -u

BASE="https://media.xiph.org/video/derf/y4m"
DIR="$(cd "$(dirname "$0")" && pwd)"
OUT="$DIR/clips"
MAN="$DIR/manifest.tsv"
mkdir -p "$OUT"

# name                        frames  class            what it stresses
CLIPS="
akiyo_qcif                    120     static           near-static talking head; skip/entropy dominated
foreman_qcif                  120     medium           handheld pan + face detail
akiyo_cif                     120     static           near-static talking head at CIF
foreman_cif                   120     medium           the classic mid-complexity reference
mobile_cif                    120     detail           dense texture + slow pan; residual/transform heavy
bus_cif                       120     pan              fast horizontal pan; motion-search heavy
tempete_cif                   120     complex          texture + zoom + falling leaves
football_cif                  120     fastmotion       chaotic multi-object motion; worst case for ME
city_4cif                     120     detail           aerial pan over dense city texture
crew_4cif                     120     complex          camera flashes + body motion
harbour_4cif                  120     detail           water + rigging; high-frequency residual
soccer_4cif                   120     fastmotion       fast pan + player motion at SD
720p50_shields_ter             60     detail           pan over fine repeating detail; brutal at 720p
720p5994_stockholm_ter         60     pan              slow aerial pan, fine rooftop detail
in_to_tree_420_720p50          60     medium           slow dolly into foliage
FourPeople_1280x720_60         60     static           video-conference class; skip dominated
ducks_take_off_1080p50         60     detail           water + feathers + motion at 1080p
park_joy_1080p50               60     fastmotion       fast pan + foliage; the classic 1080p worst case
crowd_run_1080p50              60     fastmotion       dense independent motion at 1080p
blue_sky_1080p25               60     smooth           smooth gradient sky + slow rotate; low complexity
"

VERIFY_ONLY=0
[ "${1:-}" = "--verify" ] && VERIFY_ONLY=1

printf 'name\twidth\theight\tfps\tframes\tclass\tbytes\tsha256\tsource\n' > "$MAN.tmp"

total=0
fail=0
while read -r name frames class rest; do
  [ -z "${name:-}" ] && continue
  url="$BASE/$name.y4m"
  dst="$OUT/$name.y4m"

  # --- read the y4m header (first 200 bytes is always enough) ---
  hdr=$(curl -sS --max-time 60 -r 0-199 "$url" | head -c 200 | tr -d '\000')
  line=$(printf '%s' "$hdr" | head -1)
  case "$line" in
    YUV4MPEG2*) ;;
    *) echo "!! $name: not a y4m header ($line)"; fail=$((fail+1)); continue ;;
  esac
  w=$(printf '%s' "$line" | grep -oE ' W[0-9]+'  | tr -dc 0-9)
  h=$(printf '%s' "$line" | grep -oE ' H[0-9]+'  | tr -dc 0-9)
  fps=$(printf '%s' "$line" | grep -oE ' F[0-9]+:[0-9]+' | sed 's/ F//')
  cs=$(printf '%s' "$line" | grep -oE ' C[0-9a-zA-Z]+' | sed 's/ C//')
  case "${cs:-420}" in 420*) ;; *) echo "!! $name: chroma $cs is not 4:2:0 8-bit"; fail=$((fail+1)); continue ;; esac

  hdrlen=$(( ${#line} + 1 ))                 # header line + '\n'
  fsz=$(( w * h * 3 / 2 ))
  want=$(( hdrlen + frames * (6 + fsz) ))    # 6 = "FRAME\n"

  have=0
  [ -f "$dst" ] && have=$(wc -c < "$dst" | tr -d ' ')
  if [ "$have" != "$want" ]; then
    if [ "$VERIFY_ONLY" = 1 ]; then
      echo "!! $name: have $have bytes, want $want (run without --verify)"; fail=$((fail+1))
    else
      printf '  fetching %-26s %4sx%-4s %3s frames  %6s MiB ... ' "$name" "$w" "$h" "$frames" "$((want/1048576))"
      curl -sS --max-time 900 -r "0-$((want-1))" "$url" -o "$dst" || { echo "FAILED"; fail=$((fail+1)); continue; }
      got=$(wc -c < "$dst" | tr -d ' ')
      if [ "$got" != "$want" ]; then echo "SHORT ($got/$want)"; fail=$((fail+1)); continue; fi
      echo "ok"
    fi
  fi

  sum=$(sha256sum "$dst" 2>/dev/null | cut -c1-16)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$w" "$h" "$fps" "$frames" "$class" "$want" "${sum:-?}" "$url" >> "$MAN.tmp"
  total=$((total+1))
done <<EOF
$CLIPS
EOF

mv "$MAN.tmp" "$MAN"
echo
echo "corpus: $total clips, $(du -sh "$OUT" 2>/dev/null | cut -f1) on disk, $fail problem(s)"
[ "$fail" = 0 ] || exit 1
