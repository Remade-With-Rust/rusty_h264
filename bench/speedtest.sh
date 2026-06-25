#!/usr/bin/env bash
#
# THE canonical speed test for rusty_h264 — the ONLY speed comparison we report.
#
# Why this methodology (and not the others we burned ourselves on):
#   * DIFFERENTIAL timing. We time 120 frames and 480 frames and divide the
#     pixel delta by the time delta. Any fixed per-invocation cost — ffmpeg
#     process spawn + DLL load, file open, allocation — appears in BOTH and
#     cancels exactly. What's left is pure steady-state encode throughput.
#   * BEST-of-3 (min), so a scheduler hiccup can't inflate a time.
#   * ffmpeg WARMED first (one throwaway run) so the libx264 DLLs are resident.
#   * SINGLE CORE, both sides (rusty RUSTY_THREADS=1, x264 -threads 1). The 24-core
#     numbers were bandwidth-bound and masked per-core work; this isolates the
#     real question — how fast is each encoder's actual code on one core.
#   * MATCHED: same clip, QP 26, baseline profile, 1 ref, gop, end-to-end (each
#     reads the same YUV from disk).
#
# It reports two workloads: ALL-INTRA (gop1) and INTER (gop15).
#
# Usage:  bash bench/speedtest.sh
#   Requires: cargo build --release   (produces target/release/rusty_h264)
#   ffmpeg:   set RUSTY_H264_BENCH_FFMPEG, or edit the default below.
set -euo pipefail
cd "$(dirname "$0")/.."

FF="${RUSTY_H264_BENCH_FFMPEG:-/c/Users/talmo/AppData/Local/Microsoft/WinGet/Packages/Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe/ffmpeg-8.1.1-full_build/bin/ffmpeg.exe}"
BIN=target/release/rusty_h264.exe
[ -x "$BIN" ] || BIN=target/release/rusty_h264   # non-Windows
W=352; H=288; FS=$((W*H*3/2))

# Deterministic 480-frame CIF clip: textured pan + four moving textured boxes.
python -c "
import random; random.seed(7); w,h=$W,$H
bg=[((i*3+j*2)^((i*7)&(j*5))^(i*j>>5))&0xff for j in range(h) for i in range(w)]
buf=bytearray()
for t in range(480):
  y=bytearray(bg[j*w+((i+t*3)%w)] for j in range(h) for i in range(w))
  for k,(sx,sy,sp) in enumerate([(40,30,5),(150,90,7),(250,180,4),(80,200,6)]):
    bx=(sx+t*sp)%(w-40); by=(sy+t*(sp-2))%(h-40)
    for dy in range(36):
      for dx in range(36): y[(by+dy)*w+bx+dx]=((dx*7+dy*5+t*11+k*40)^(dx*dy))&0xff
  buf+=y+bytearray(128 for _ in range((w//2)*(h//2)))*2
open('_long.yuv','wb').write(bytes(buf))"
head -c $((FS*120)) _long.yuv > _s120.yuv

tt() { best=99; for r in 1 2 3; do t0=$(date +%s.%N); "$@" >/dev/null 2>&1; t1=$(date +%s.%N); d=$(python -c "print($t1-$t0)"); best=$(python -c "print(min($best,$d))"); done; echo "$best"; }

# Warm ffmpeg (load DLLs) so the first timed run isn't a cold-start outlier.
"$FF" -y -loglevel error -f rawvideo -pix_fmt yuv420p -s ${W}x${H} -i _s120.yuv -c:v libx264 -preset ultrafast -g 1 -f h264 _o.264 >/dev/null 2>&1

echo "rusty_h264 speedtest — differential (480f − 120f), best-of-3, SINGLE CORE"
for lbl in "ALL-INTRA gop1" "INTER gop15"; do
  g=$([ "$lbl" = "ALL-INTRA gop1" ] && echo 1 || echo 15)
  x1=$(tt "$FF" -y -loglevel error -f rawvideo -pix_fmt yuv420p -s ${W}x${H} -i _s120.yuv -c:v libx264 -preset ultrafast -profile:v baseline -qp 26 -g "$g" -refs 1 -threads 1 -f h264 _o.264)
  x4=$(tt "$FF" -y -loglevel error -f rawvideo -pix_fmt yuv420p -s ${W}x${H} -i _long.yuv -c:v libx264 -preset ultrafast -profile:v baseline -qp 26 -g "$g" -refs 1 -threads 1 -f h264 _o.264)
  o1=$(RUSTY_THREADS=1 tt $BIN encode --width $W --height $H --gop "$g" --qp 26 --preset fast --in _s120.yuv --out _o.264)
  o4=$(RUSTY_THREADS=1 tt $BIN encode --width $W --height $H --gop "$g" --qp 26 --preset fast --in _long.yuv --out _o.264)
  python -c "px=360*$W*$H; xr=px/(($x4)-($x1))/1e6; rr=px/(($o4)-($o1))/1e6; print(f'  $lbl: x264-ultrafast-1c {xr:>5.0f} Mpx/s   |   rusty fast-1c {rr:>4.0f} Mpx/s   gap {xr/rr:.1f}x')"
done
rm -f _long.yuv _s120.yuv _o.264
