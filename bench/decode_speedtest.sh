#!/usr/bin/env bash
#
# Decode speed: rusty_h264 (pure Rust, asm kernels) vs ffmpeg's native `h264`
# SOFTWARE decoder. Same stream, single core.
#
# METHODOLOGY — rewritten 2026-07-31. The previous version reported a phantom
# figure (ffmpeg "622.9 Mpx/s"); see docs/WHYS-decoder-perf.md. Three defects, all
# of which this version fixes:
#
#   1. It timed a DIFFERENTIAL (N2 frames minus N1 frames) to cancel per-invocation
#      cost. ffmpeg's startup on Windows is ~0.8-1.0 s while its marginal decode of
#      180 frames is ~0.6 s, so the differential subtracted two numbers of the same
#      size: five repeats of the same work gave 202, 391, 176, NEGATIVE and 330
#      Mpx/s. Fixed by using a stream long enough (1200 frames) that startup needs
#      no cancelling.
#   2. It timed our CLI, whose `decode` accumulates every frame into a Vec, then
#      concatenates ~331 MB and writes it, while ffmpeg's `-f null` streams and
#      discards. That is 38% of the measured time, and the differential does NOT
#      cancel it because it scales with the frame count. Fixed by timing the
#      `decode_bench` example, which drops each picture as ffmpeg does.
#   3. It timed WALL clock on a box whose spread reached 285% and whose absolute
#      throughput drifted 3.5x between sessions. Fixed by reading CPU time, which
#      contention does not inflate (three consecutive readings agreed to the ms
#      while wall swung 1.5x).
#
# Both arms print their FRAME COUNT and the script refuses to report a ratio unless
# they match -- a time comparison between arms doing different work is void, and
# that check is what caught a mistyped ffmpeg flag silently decoding nothing.
#
# Usage:  cargo build --release --features asm
#         cargo build --release -p rusty_h264-decoder --features asm --example decode_bench
#         bash bench/decode_speedtest.sh [W H FRAMES COPIES]
set -euo pipefail
cd "$(dirname "$0")/.."

command -v ffmpeg >/dev/null || { echo "ffmpeg not on PATH"; exit 1; }
# Bare name, NOT `command -v`: the timing helper is a Windows `python`, which
# cannot execute the /c/... path Git Bash reports. PATH resolution works for both.
FF=ffmpeg
ENC=target/release/rusty_h264.exe
[ -x "$ENC" ] || ENC=target/release/rusty_h264
BENCH=target/release/examples/decode_bench.exe
[ -x "$BENCH" ] || BENCH=target/release/examples/decode_bench
[ -x "$ENC" ] || { echo "build first: cargo build --release --features asm"; exit 1; }
[ -x "$BENCH" ] || { echo "build first: cargo build --release -p rusty_h264-decoder --features asm --example decode_bench"; exit 1; }

W=${1:-1280}; H=${2:-720}; N=${3:-240}; COPIES=${4:-5}

# Repo-local, NOT mktemp: this script hands paths to a Windows `python`, which
# cannot resolve Git Bash's /tmp/... mount.
TMP=./_dspeed_tmp; rm -rf "$TMP"; mkdir -p "$TMP"; trap 'rm -rf "$TMP"' EXIT

# Deterministic clip: textured pan + four moving textured boxes (real intra detail,
# inter motion, residual).
python -c "
w,h=$W,$H
bg=[((i*3+j*2)^((i*7)&(j*5))^(i*j>>5))&0xff for j in range(h) for i in range(w)]
buf=bytearray()
for t in range($N):
  y=bytearray(bg[j*w+((i+t*3)%w)] for j in range(h) for i in range(w))
  for k,(sx,sy,sp) in enumerate([(40,30,5),(150,90,7),(250,180,4),(80,200,6)]):
    bx=(sx+t*sp)%(w-40); by=(sy+t*(sp-2))%(h-40)
    for dy in range(36):
      for dx in range(36): y[(by+dy)*w+bx+dx]=((dx*7+dy*5+t*11+k*40)^(dx*dy))&0xff
  buf+=y+bytearray(128 for _ in range((w//2)*(h//2)))*2
open('$TMP/c.yuv','wb').write(bytes(buf))"

# Both entropy coders: the gap differs sharply between them (CAVLC is the WORSE
# case for us -- 3.7x vs 1.7x), so reporting only one hides half the picture.
for E in 0 1; do
  name=$([ "$E" = 0 ] && echo cavlc || echo cabac)
  RUSTY_THREADS=1 "$ENC" encode --width "$W" --height "$H" --gop 12 --qp 26 --preset fast \
      --cabac "$E" --in "$TMP/c.yuv" --out "$TMP/one_$name.264" >/dev/null 2>&1
  : > "$TMP/long_$name.264"
  for _ in $(seq "$COPIES"); do cat "$TMP/one_$name.264" >> "$TMP/long_$name.264"; done
done

TOTAL=$((N * COPIES))
PX=$((TOTAL * W * H))

# CPU time of one process, best of 3. Wall clock is unusable on a contended box.
cpu_ms() {
  python - "$@" <<'PY'
import os, subprocess, sys, ctypes, ctypes.wintypes as wt
k32 = ctypes.windll.kernel32
# Windows CreateProcess rejects a RELATIVE path written with forward slashes, so
# `target/release/...` fails while a bare `ffmpeg` (resolved via PATH) succeeds.
# Absolutize anything that names a real file; leave PATH-resolved names alone.
argv = list(sys.argv[1:])
if os.path.exists(argv[0]):
    argv[0] = os.path.abspath(argv[0])
best = None
for _ in range(3):
    p = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    p.wait()
    h = k32.OpenProcess(0x1000, False, p.pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    c, e, kt, ut = wt.FILETIME(), wt.FILETIME(), wt.FILETIME(), wt.FILETIME()
    ok = k32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(kt), ctypes.byref(ut))
    k32.CloseHandle(h)
    if not ok:
        print(-1); sys.exit()
    tot = lambda f: (f.dwHighDateTime << 32 | f.dwLowDateTime) / 1e4  # 100ns -> ms
    ms = tot(kt) + tot(ut)
    best = ms if best is None else min(best, ms)
print(f"{best:.1f}")
PY
}

echo
echo "rusty_h264 DECODE speedtest — ${W}x${H}, ${TOTAL} frames, SINGLE CORE, CPU time (best of 3)"
for name in cavlc cabac; do
  s="$TMP/long_$name.264"
  ours_frames=$("$BENCH" "$s" 1 | grep -o "frames=[0-9]*" | cut -d= -f2)
  ff_frames=$("$FF" -hide_banner -loglevel info -threads 1 -i "$s" -f null - 2>&1 |
              grep -o "frame= *[0-9]*" | tail -1 | tr -dc 0-9)
  if [ "$ours_frames" != "$ff_frames" ]; then
    echo "  $name: WORK MISMATCH — ours decoded $ours_frames frames, ffmpeg $ff_frames. Comparison VOID."
    continue
  fi
  r=$(cpu_ms "$BENCH" "$s" 1)
  f=$(cpu_ms "$FF" -hide_banner -loglevel error -threads 1 -i "$s" -f null -)
  python -c "
px=$PX
rr=px/($r/1000)/1e6; fr=px/($f/1000)/1e6
print('  %-6s %d frames both arms   rusty %6.1f Mpx/s   ffmpeg %6.1f Mpx/s   gap %.2fx'
      % ('$name', $ours_frames, rr, fr, $r/$f))"
done
