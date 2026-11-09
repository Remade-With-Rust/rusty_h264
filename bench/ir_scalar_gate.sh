#!/usr/bin/env bash
# The identity gate for the SCALAR build (no `asm` feature: `cfg(accel)` off),
# which the counted runs never execute. Builds decode_bench without the SIMD
# kernels into $HOME/rh264-target-scalar and checks every stream in _ir/ and
# _ir/feat/ against ffmpeg's SHA256, exactly like ir_all.sh's identity step.
#   wsl -e bash -lc 'cd /mnt/e/coding/rusty_h264 && bash bench/ir_scalar_gate.sh'
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
CARGO_TARGET_DIR=$HOME/rh264-target-scalar cargo build --release -p rusty_h264-decoder \
  --no-default-features --features std,global-alloc --example decode_bench > /tmp/scalar.build 2>&1 \
  || { tail -30 /tmp/scalar.build; echo BUILD FAILED; exit 1; }
BENCH=$HOME/rh264-target-scalar/release/examples/decode_bench
check() {
  f=$1; t=$(mktemp)
  if ! "$BENCH" "$f" 1 out="$t" >/dev/null 2>&1; then echo "DECODE-FAIL $f"; rm -f "$t"; return; fi
  ours=$(sha256sum "$t" | cut -d' ' -f1); rm -f "$t"
  [ "$ours" = "$(cat "$f.ffsha")" ] || echo "DIFF $f"
}
export -f check; export BENCH
n=$(ls _ir/*.264 _ir/feat/*.264 | wc -l)
bad=$(ls _ir/*.264 _ir/feat/*.264 | xargs -P 12 -I{} bash -c 'check {}')
if [ -n "$bad" ]; then echo "$bad"; echo "scalar identity: FAILED"; exit 1; fi
echo "scalar identity: $n/$n streams byte-identical to ffmpeg"
