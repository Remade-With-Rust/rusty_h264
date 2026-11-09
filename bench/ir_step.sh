#!/usr/bin/env bash
# Build the shipping decode_bench in WSL, then run bench/ir_all.sh.
#   wsl -e bash -lc 'cd /mnt/e/coding/rusty_h264 && bash bench/ir_step.sh <tag> [prev-tag]'
# Writes the full log to _ir/runs/<tag>.log and prints it.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/rh264-target}
TAG=${1:?usage: ir_step.sh <tag> [prev-tag]}
mkdir -p _ir/runs
LOG=_ir/runs/$TAG.log
start=$(date +%s)
if ! cargo build --release -p rusty_h264-decoder --example decode_bench > "$LOG.build" 2>&1; then
  grep -E '^(error|warning)' -A 12 "$LOG.build" | head -60
  echo "BUILD FAILED"; exit 1
fi
grep -E '^warning' -A 6 "$LOG.build" | head -30
echo "build    : $(( $(date +%s) - start ))s"
bash bench/ir_all.sh "$@" > "$LOG" 2>&1
rc=$?
cat "$LOG"
echo "total    : $(( $(date +%s) - start ))s  rc=$rc"
exit $rc
