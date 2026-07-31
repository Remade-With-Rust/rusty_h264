#!/usr/bin/env bash
#
# ENCODER conformance MATRIX — every coding-tool lever, on real clips, vs ffmpeg.
#
# `conf_ffmpeg` gates the encoder's DEFAULT config only (preset x qp). Every opt-in
# lever we have landed — CABAC, 8x8 transform, B-frames, sub-8x8, mb-tree, AQ,
# multi-ref, wide ME — ships OUTSIDE that gate, so a tool that emits an illegal or
# mis-predicted stream only shows up when someone turns it on. This is that gate:
# each lever encoded on real content at four QPs, decoded by ffmpeg AND by us, and
# the two reconstructions required to be BYTE-IDENTICAL.
#
# Outcomes are classified, because they mean very different things:
#   SKIP  — the encoder refused with an explicit `unsupported:` message. That is a
#           DOCUMENTED capability gap behaving correctly (refusing beats emitting a
#           broken stream); counted and reported, but not a failure.
#   ENC   — the encoder failed some OTHER way: a crash, an assert, a panic.
#   FFREJ — ffmpeg refused the stream: it is ILLEGAL. Encoder defect.
#   DIFF  — both decoded, reconstructions differ: encoder mis-predicts, or OUR
#           decoder is wrong for that tool. Cross-check against conf_x264_decode.sh.
#
#   bash bench/conf_matrix.sh [frames]
#
# Exits non-zero on any failure. Env: FFMPEG_BIN.
set -uo pipefail
cd "$(dirname "$0")/.."

FRAMES=${1:-16}
FF=${FFMPEG_BIN:-ffmpeg}
BIN=target/release/rusty_h264.exe
[ -x "$BIN" ] || BIN=target/release/rusty_h264
[ -x "$BIN" ] || { echo "build first: cargo build --release --features asm"; exit 1; }
command -v "$FF" >/dev/null || { echo "ffmpeg not on PATH"; exit 1; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

# Content axis: smooth/static, natural/medium, busy/high-detail, and one HD size so
# the tools are exercised at a stride the CIF clips never reach.
CLIPS="akiyo_cif foreman_cif mobile_cif FourPeople_1280x720_60"

# name:extra-args. `base` is the control — if it fails, nothing below is diagnostic.
CONFIGS=(
  "base:"
  "cabac:--cabac 1"
  "t8x8:--transform-8x8 1"
  # 8x8 CROSSED with B-frames. Added 2026-07-31: `t8x8` carries no B-frames and
  # `bframes` carries no 8x8, so the pair was never exercised — and the pair is
  # broken (the B emit path has no 8x8 residual; ffmpeg rejects the slice). It was
  # also UNREACHABLE until the B-frame profile guard was corrected, so a wrong rule
  # was masking a real defect. Expect SKIP while the guard stands; when the encoder
  # learns 8x8-in-B this arm must flip to PASS, not silently stay skipped.
  "t8x8+bframes:--transform-8x8 1 --bframes 2"
  "cabac+t8x8:--cabac 1 --transform-8x8 1"
  "bframes:--bframes 2"
  "bframes+cabac:--bframes 2 --cabac 1"
  "bframes-auto:--bframes auto"
  "sub8x8:--sub8x8 1"
  "mewide:--me-wide 1"
  "refs3:--refs 3"
  # Multi-ref x CABAC is the exact axis on which our DECODER fails x264's streams
  # (deblock bS with >1 reference). Encoding it ourselves cross-checks whether the
  # defect is reachable from our own encoder or only from x264's reference usage.
  "cabac+refs3:--cabac 1 --refs 3"
  "cabac+bframes+refs3:--cabac 1 --bframes 2 --refs 3"
  "mbtree:--mbtree 1"
  "aq-off:--aq 0"
  "satd-full:--satd-q 1"
  "quality:--preset quality"
  "allintra:--gop 1"
  "kitchen-sink:--cabac 1 --transform-8x8 1 --bframes 2 --sub8x8 1 --refs 3 --preset quality"
)

pass=0; fail=0; skip=0; failed=(); skipped=()

for clip in $CLIPS; do
  y4m="video-tests/clips/$clip.y4m"
  [ -f "$y4m" ] || { echo "missing clip: $y4m"; continue; }
  # y4m -> raw I420, once per clip. The CLI takes raw planar input only.
  read -r W H < <(head -c 200 "$y4m" | tr ' ' '\n' | awk '/^W/{w=substr($0,2)} /^H/{h=substr($0,2)} END{print w, h}')
  raw="$TMP/$clip.yuv"
  "$FF" -v error -i "$y4m" -frames:v "$FRAMES" -f rawvideo -pix_fmt yuv420p -y "$raw" || continue

  for cfg in "${CONFIGS[@]}"; do
    name=${cfg%%:*}; extra=${cfg#*:}
    for qp in 22 27 32 37; do
      s="$TMP/s.264"; ours="$TMP/ours.yuv"; ffo="$TMP/ff.yuv"
      rm -f "$s" "$ours" "$ffo"   # never score a stale artifact
      # shellcheck disable=SC2086
      if ! err=$("$BIN" encode --width "$W" --height "$H" --qp "$qp" $extra \
            --in "$raw" --out "$s" 2>&1 >/dev/null); then
        if [[ $err == *"unsupported:"* ]]; then
          skip=$((skip+1)); skipped+=("$clip/$name/qp$qp: ${err#error: unsupported: }")
        else
          fail=$((fail+1)); failed+=("ENC   $clip/$name/qp$qp: $err")
        fi
        continue
      fi
      if ! "$FF" -v error -i "$s" -f rawvideo -pix_fmt yuv420p -y "$ffo" >/dev/null 2>&1; then
        fail=$((fail+1)); failed+=("FFREJ $clip/$name/qp$qp"); continue
      fi
      "$BIN" decode --in "$s" --out "$ours" >/dev/null 2>&1
      if cmp -s "$ours" "$ffo"; then
        pass=$((pass+1))
      else
        fail=$((fail+1)); failed+=("DIFF  $clip/$name/qp$qp")
      fi
    done
  done
  echo "  $clip done   (running: pass=$pass fail=$fail skip=$skip)"
done

echo
echo "encoder conformance matrix vs ffmpeg (${FRAMES}f, ${#CONFIGS[@]} configs x 4 QPs x $(echo $CLIPS | wc -w) clips)"
echo "  PASS $pass   FAIL $fail   SKIP $skip (unsupported combinations, refused cleanly)"
if [ $skip -gt 0 ]; then
  printf '  skip: %s\n' "${skipped[@]}" | sort -u -t: -k2 | head -8
fi
if [ $fail -gt 0 ]; then
  printf '  %s\n' "${failed[@]}"
  exit 1
fi
