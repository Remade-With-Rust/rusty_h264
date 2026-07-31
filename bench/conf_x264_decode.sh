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

# name:x264-profile:extra-args — `baseline` is the CAVLC control that MUST stay
# exact, so a regression in the shared reconstruct path is distinguishable from a
# CABAC one.
#
# `maincavlc` added 2026-07-30. It exists for ONE reason: Baseline FORBIDS
# B-frames, so the `baseline` arm can never carry one, and every CAVLC B-slice
# stream was therefore untested. Three separate defects have now hidden in that
# blind spot. CAVLC x B needs its own profile arm — `--profile main --no-cabac`
# is the only way to reach it.
for prof in "baseline:baseline:--no-cabac" \
            "maincavlc:main:--no-cabac" \
            "main:main:" \
            "high:high:" \
            "high8:high:--no-8x8dct"; do
  name=${prof%%:*}; rest=${prof#*:}; pname=${rest%%:*}; extra=${rest#*:}
  # intra-only / P-only / IPB: the GOP axis that a single stream collapses.
  # `ipb3` and `pyr` were added 2026-07-30 after a bisect found two defects the
  # original three arms missed entirely: bframes>=3 diverges even at --ref 1 (the
  # bframes-2 arm is exact there), and b-pyramid + multi-ref fails to PARSE
  # ("bitstream truncated") rather than merely diverging. Both stay invisible unless
  # the B-DEPTH and the B-as-reference axes are swept separately from plain "ipb".
  #
  # `ipb3` MUST keep --keyint 30. Measured on foreman_cif: bframes 3 / ref 1 is
  # byte-EXACT at keyint 12 and diverges at keyint 30 and 60, at both 24 and 48
  # frames. A short GOP re-anchors on an I-frame before the defect can express, so
  # the arm silently passes at the keyint the other arms use. Do not "tidy" this
  # back to 12 — that would make the gate green while the defect is still live.
  #
  # `pyr3` added 2026-07-30: b-pyramid at B-DEPTH 3. `pyr` (bframes 2) and `ipb3`
  # (bframes 3, no pyramid) both pass while this combination still diverges, so
  # neither existing arm covers it — the two axes have to be crossed, not swept
  # independently.
  for gop in "intra:--keyint 1" "p:--keyint 12 --bframes 0" \
             "ipb:--keyint 12 --bframes 2 --b-pyramid none --ref 1" \
             "ipb3:--keyint 30 --bframes 3 --b-pyramid none --ref 1" \
             "pyr:--keyint 12 --bframes 2 --b-pyramid normal --ref 3" \
             "pyr3:--keyint 30 --bframes 3 --b-pyramid normal --ref 3"; do
    gname=${gop%%:*}; gargs=${gop#*:}
    for qp in 22 27 32 37; do
      s="$TMP/s.264"
      # shellcheck disable=SC2086
      "$X264" $gargs $extra --qp "$qp" --frames "$FRAMES" --profile "$pname" \
        -o "$s" "$CLIP" >/dev/null 2>&1 || { echo "  x264 failed: $name/$gname/qp$qp"; continue; }
      rm -f "$TMP/ours.yuv" "$TMP/ff.yuv"   # never score a stale artifact
      # A hard PARSE failure and a wrong RECONSTRUCTION are different bugs and want
      # different first moves, so never let the byte-compare collapse them: without
      # this split, a decoder that emits nothing reads as "differs" with a byte count
      # equal to the whole file, which looks like catastrophic drift.
      if ! derr=$("$BIN" decode --in "$s" --out "$TMP/ours.yuv" 2>&1 >/dev/null); then
        fail=$((fail+1)); failed+=("PARSE $name/$gname/qp$qp: ${derr#error: }"); continue
      fi
      "$FF" -v error -i "$s" -f rawvideo -pix_fmt yuv420p -y "$TMP/ff.yuv" >/dev/null 2>&1
      if cmp -s "$TMP/ours.yuv" "$TMP/ff.yuv"; then
        pass=$((pass+1))
      else
        fail=$((fail+1)); failed+=("DIFF  $name/$gname/qp$qp")
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
