#!/usr/bin/env bash
# EXACT instruction census of one encoder preset under callgrind (Linux / WSL).
#
#   bench/ir_census.sh [preset] [clip-basename] [frames] [qp]
#   defaults:            fast     foreman_cif      30       26
#
# Runs the SAME binary twice so the exactness ladder (same binary, re-run) is
# MEASURED, not assumed, and prints the work-parity anchors (output bytes + md5)
# so a moved count can never be mistaken for a code change. Then prints the
# self-cost census. Caller/callee edges: bench/ir_census.py.
#
# Needs: valgrind, python3, a release CLI built with
#   CARGO_TARGET_DIR=$HOME/h264-target cargo build --release -p rusty_h264-cli
# Override the binary with IR_BIN, the work dir with IR_DIR.
# Ir counts instructions only: blind to cache, branches, bandwidth and threads.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PRESET=${1:-fast}
CLIP=${2:-foreman_cif}
FRAMES=${3:-30}
QP=${4:-26}
BIN=${IR_BIN:-$HOME/h264-target/release/rusty_h264}
DIR=${IR_DIR:-$HOME/h264-ir}
mkdir -p "$DIR"
[ -x "$BIN" ] || { echo "no binary at $BIN (build the CLI first)"; exit 1; }

Y4M="$ROOT/video-tests/clips/$CLIP.y4m"
YUV="$DIR/${CLIP}_$FRAMES.yuv"
if [ ! -f "$YUV" ]; then
  [ -f "$Y4M" ] || { echo "no clip at $Y4M"; exit 1; }
  # Strip the Y4M container to raw I420 (stdlib only; 4:2:0).
  python3 -I - "$Y4M" "$YUV" "$FRAMES" <<'PY'
import sys
src, dst, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
with open(src, 'rb') as f, open(dst, 'wb') as o:
    hdr = f.readline()
    assert hdr.startswith(b'YUV4MPEG2'), hdr[:20]
    w = h = None
    for tok in hdr.split():
        if tok[:1] == b'W': w = int(tok[1:])
        elif tok[:1] == b'H': h = int(tok[1:])
    fsz = w * h * 3 // 2
    got = 0
    while got < n:
        line = f.readline()
        if not line: break
        assert line.startswith(b'FRAME'), line[:10]
        buf = f.read(fsz)
        if len(buf) < fsz: break
        o.write(buf); got += 1
print(f"{dst}: {w}x{h} frames={got}")
PY
fi
# Dimensions from the Y4M header (the raw file carries none).
read -r W H < <(python3 -I - "$Y4M" <<'PY'
import sys
hdr = open(sys.argv[1], 'rb').readline().split()
w = [t for t in hdr if t[:1] == b'W'][0][1:].decode()
h = [t for t in hdr if t[:1] == b'H'][0][1:].decode()
print(w, h)
PY
)

TAG="$PRESET.$CLIP"
export RUSTY_THREADS=1
cd "$DIR"
for run in 1 2; do
  valgrind --tool=callgrind --callgrind-out-file="cg.$TAG.$run.out" \
    "$BIN" encode --width "$W" --height "$H" --qp "$QP" --preset "$PRESET" \
    --in "$YUV" --out "out.$TAG.$run.264" > "enc.$TAG.$run.log" 2> "vg.$TAG.$run.log"
  echo "run $run exit=$?  $(grep -E 'Collected' "vg.$TAG.$run.log" | head -1)"
  echo "  anchors: bytes=$(stat -c %s "out.$TAG.$run.264")  md5=$(md5sum < "out.$TAG.$run.264" | cut -c1-32)"
done
echo "=== ladder (same binary, re-run; must agree to within tens of Ir) ==="
for run in 1 2; do grep -E '^summary:' "cg.$TAG.$run.out"; done
echo "=== self-cost census (threshold 97%) ==="
callgrind_annotate --threshold=97 "cg.$TAG.1.out" 2>/dev/null \
  | sed -E 's#\[[^]]*\]##; s#\?\?\?:##' | sed -n '/PROGRAM TOTALS/,$p' | head -70
echo "raw profile: $DIR/cg.$TAG.1.out  (edges: python3 -I $ROOT/bench/ir_census.py $DIR/cg.$TAG.1.out <fn-substring>...)"
