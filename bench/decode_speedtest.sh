#!/usr/bin/env bash
#
# Decode speed: rusty_h264 (pure Rust, asm kernels) vs ffmpeg's native `h264`
# SOFTWARE decoder — the most-optimized C/asm H.264 decoder commonly available
# (openh264's standalone h264dec is the historical yardstick but is not installed;
# ffmpeg's native decoder is a *tougher* bar). Same stream, single core.
#
# Methodology mirrors bench/speedtest.sh (the canonical encoder test):
#   * DIFFERENTIAL: time N2 frames and N1 frames, divide the pixel delta by the
#     time delta. Fixed per-invocation cost (process spawn, DLL load, file open,
#     stream parse warm-up) is in BOTH and cancels — leaving steady-state decode.
#   * BEST-of-3 (min) so a scheduler hiccup can't inflate a time.
#   * ffmpeg WARMED first (one throwaway run) so its DLLs are resident.
#   * SINGLE CORE both sides (ffmpeg -threads 1; our decoder is single-threaded).
#   * decode-to-NULL both sides (ffmpeg -f null; ours --out NUL) to isolate DECODE
#     from output I/O. Same .264 (our encoder, Constrained Baseline / CAVLC),
#     which ffmpeg also accepts (bit-exact-validated elsewhere).
#
# Usage:  cargo build --release -p rusty_h264-cli --features asm
#         bash bench/decode_speedtest.sh
set -euo pipefail
cd "$(dirname "$0")/.."

FF="$(command -v ffmpeg || true)"
[ -n "$FF" ] || { echo "ffmpeg not on PATH"; exit 1; }
BIN=target/release/rusty_h264.exe
[ -x "$BIN" ] || BIN=target/release/rusty_h264
[ -x "$BIN" ] || { echo "build first: cargo build --release -p rusty_h264-cli --features asm"; exit 1; }

# Resolution / frame counts overridable as positional args: W H N1 N2
W=${1:-1280}; H=${2:-720}; N1=${3:-60}; N2=${4:-240}; FS=$((W*H*3/2))

# Deterministic 720p clip: textured pan + four moving textured boxes (matches the
# profile_decode / speedtest clips — real intra detail, inter motion, residual).
python -c "
w,h=$W,$H
bg=[((i*3+j*2)^((i*7)&(j*5))^(i*j>>5))&0xff for j in range(h) for i in range(w)]
buf=bytearray()
for t in range($N2):
  y=bytearray(bg[j*w+((i+t*3)%w)] for j in range(h) for i in range(w))
  for k,(sx,sy,sp) in enumerate([(40,30,5),(150,90,7),(250,180,4),(80,200,6)]):
    bx=(sx+t*sp)%(w-40); by=(sy+t*(sp-2))%(h-40)
    for dy in range(36):
      for dx in range(36): y[(by+dy)*w+bx+dx]=((dx*7+dy*5+t*11+k*40)^(dx*dy))&0xff
  buf+=y+bytearray(128 for _ in range((w//2)*(h//2)))*2
open('_dd.yuv','wb').write(bytes(buf))"
head -c $((FS*N1)) _dd.yuv > _dd1.yuv

# Encode both lengths with OUR encoder (baseline, QP26, gop12) → streams both decode.
RUSTY_THREADS=1 "$BIN" encode --width $W --height $H --gop 12 --qp 26 --preset fast --in _dd1.yuv --out _dd1.264 >/dev/null 2>&1
RUSTY_THREADS=1 "$BIN" encode --width $W --height $H --gop 12 --qp 26 --preset fast --in _dd.yuv  --out _dd2.264 >/dev/null 2>&1

tt() { best=99; for r in 1 2 3; do t0=$(date +%s.%N); "$@" >/dev/null 2>&1; t1=$(date +%s.%N); d=$(python -c "print($t1-$t0)"); best=$(python -c "print(min($best,$d))"); done; echo "$best"; }

# Warm ffmpeg (load DLLs) so the first timed run isn't a cold-start outlier.
"$FF" -hide_banner -loglevel error -threads 1 -i _dd1.264 -f null - >/dev/null 2>&1 || true

r1=$(tt "$BIN" decode --width $W --height $H --in _dd1.264 --out NUL)
r2=$(tt "$BIN" decode --width $W --height $H --in _dd2.264 --out NUL)
f1=$(tt "$FF" -hide_banner -loglevel error -threads 1 -i _dd1.264 -f null -)
f2=$(tt "$FF" -hide_banner -loglevel error -threads 1 -i _dd2.264 -f null -)

python -c "
px=($N2-$N1)*$W*$H
rr=px/(($r2)-($r1))/1e6; fr=px/(($f2)-($f1))/1e6
print('')
print('rusty_h264 DECODE speedtest - differential (%df-%df), best-of-3, SINGLE CORE, decode-to-null' % ($N2,$N1))
print('  clip: %dx%d, Constrained Baseline / CAVLC, QP26, gop12' % ($W,$H))
print('    rusty_h264   %6.1f Mpx/s   (pure Rust + vendored openh264 asm kernels)' % rr)
print('    ffmpeg h264  %6.1f Mpx/s   (native C/asm software decoder)' % fr)
print('    ratio: rusty is %.2fx ffmpeg  (%.0f%% of ffmpeg throughput)' % (rr/fr, 100*rr/fr))"

rm -f _dd.yuv _dd1.yuv _dd1.264 _dd2.264
