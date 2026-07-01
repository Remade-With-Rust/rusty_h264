#!/usr/bin/env bash
#
# Brick 0.1 (docs/cabac-decode-plan.md) — deterministic CABAC test corpus.
# Our encoder emits CAVLC only, so CABAC streams come from libx264 (default = High +
# CABAC). Each stream ships with its ffmpeg-decoded reference YUV (the final pixel gate;
# symbol-level localisation still needs the instrumented oracle, Brick 0.2).
#
# Usage: bash bench/make_cabac_corpus.sh   (writes to tests/cabac_data/, gitignored)
set -euo pipefail
cd "$(dirname "$0")/.."
FF="$(command -v ffmpeg)"; [ -n "$FF" ] || { echo "ffmpeg not on PATH"; exit 1; }
OUT=tests/cabac_data; mkdir -p "$OUT"

# Deterministic textured clip at a given size/frames -> raw I420.
gen() { # w h n path
  python -c "
w,h,n=$1,$2,$3
bg=[((i*3+j*2)^((i*7)&(j*5))^(i*j>>5))&0xff for j in range(h) for i in range(w)]
buf=bytearray()
for t in range(n):
  y=bytearray(bg[j*w+((i+t*3)%w)] for j in range(h) for i in range(w))
  for k,(sx,sy,sp) in enumerate([(4,3,3),(11,7,5)]):
    bx=(sx+t*sp)%(max(1,w-12)); by=(sy+t*sp)%(max(1,h-12))
    for dy in range(12):
      for dx in range(12): y[(by+dy)*w+bx+dx]=((dx*7+dy*5+t*11+k*40)^(dx*dy))&0xff
  buf+=y+bytearray(128 for _ in range((w//2)*(h//2)))*2
open('$4','wb').write(bytes(buf))"
}

# name  w h n gop profile  bframes(optional, default 0)
mk() { # name w h n gop profile [bframes]
  local name=$1 w=$2 h=$3 n=$4 gop=$5 prof=$6 bf=${7:-0}
  # B streams: pin the GOP so slice types are deterministic (no scenecut / pyramid /
  # weighted pred — those add coding tools the bring-up gates separately).
  local bparams=""
  [ "$bf" != 0 ] && bparams=":b-pyramid=none:weightp=0:weightb=0:scenecut=0"
  gen "$w" "$h" "$n" "$OUT/_src_$name.yuv"
  "$FF" -hide_banner -loglevel error -y -f rawvideo -pix_fmt yuv420p -s ${w}x${h} \
    -i "$OUT/_src_$name.yuv" -c:v libx264 -profile:v "$prof" -g "$gop" -qp 22 \
    -x264-params "cabac=1:ref=1:bframes=${bf}:8x8dct=$([ "$prof" = high ] && echo 1 || echo 0):threads=1${bparams}" \
    -f h264 "$OUT/$name.264"
  # ffmpeg reference decode (single-thread, planar YUV) — the pixel gate.
  "$FF" -hide_banner -loglevel error -y -threads 1 -i "$OUT/$name.264" \
    -f rawvideo -pix_fmt yuv420p "$OUT/${name}_ref.yuv"
  rm -f "$OUT/_src_$name.yuv"
  echo "  $name.264  ($(stat -c%s "$OUT/$name.264" 2>/dev/null || wc -c <"$OUT/$name.264") B, ${w}x${h} x$n gop$gop $prof/CABAC)  + ${name}_ref.yuv"
}

echo "CABAC corpus -> $OUT/"
mk cabac_i_tiny   48  48  1  1  main    # 3x3 MBs, single I frame — the corner-block bring-up
mk cabac_i_qcif  176 144  1  1  main    # I-only, more MBs
mk cabac_ip_qcif 176 144  6  3  main    # I+P (P-slice CABAC: skip/mvd/ref)
mk cabac_ib_qcif 176 144  9  8  main 2  # I+P+B (B-slice CABAC: direct/L0/L1/Bi/8x8, 2 B-frames)
mk cabac_i_high   64  64  1  1  high    # High profile (8x8 transform + 8x8 CABAC residual)
echo "done. (High needs 8x8dct; Main is 4x4-only — bring up Main first.)"
