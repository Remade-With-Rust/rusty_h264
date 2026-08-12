#!/usr/bin/env bash
#
# PGO BUILD — profile-guided optimization for the decoder binaries.
#
# BANKED 2026-08-12: PGO vs same-source baseline, pinvs (pinned CPU time, ABBA,
# 15 pairs): high 0.969x (12/15, z=-2.32), cavlc 0.947x (13/15, z=-2.84) —
# a 3-5% whole-decoder win from branch/code layout alone, no code changes.
# Decode is branch-prediction-bound (entropy bins, mode dispatch), which is
# precisely what ffmpeg's hand-asm gets right and the compiler guesses at;
# PGO is the pure-Rust way to buy the same layout.
#
# The training workload is the standing x264 3-tier corpus (bench/
# decode_x264_speedtest.sh builds it into _xbench/), so the profile matches the
# content class the standing benchmark measures. Training on the benchmark
# corpus slightly flatters the benchmark — acceptable while both use the same
# real-720p clips; retrain if the deployment content class differs.
#
# Usage:  bash bench/pgo.sh            # builds target-pgouse/release/...
# Needs:  rustup component add llvm-tools   (one-time)
#         _xbench/long_{cavlc,main,high}.264 (run decode_x264_speedtest.sh once)
set -euo pipefail
cd "$(dirname "$0")/.."

for t in cavlc main high; do
  [ -f "_xbench/long_$t.264" ] || {
    echo "missing _xbench/long_$t.264 — run bench/decode_x264_speedtest.sh once to build the corpus"
    exit 1
  }
done

PD=$(ls "$HOME"/.rustup/toolchains/*/lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-profdata* 2>/dev/null | head -1)
[ -n "$PD" ] || { echo "llvm-profdata not found — rustup component add llvm-tools"; exit 1; }

PROF_DIR=$(mktemp -d)
trap 'rm -rf "$PROF_DIR"' EXIT

echo "== 1/3 instrumented build (target-pgo) =="
RUSTFLAGS="-Cprofile-generate=$PROF_DIR" CARGO_TARGET_DIR=target-pgo \
  cargo build --release --features asm -p rusty_h264-cli
RUSTFLAGS="-Cprofile-generate=$PROF_DIR" CARGO_TARGET_DIR=target-pgo \
  cargo build --release --features asm -p rusty_h264-decoder --example decode_bench

echo "== 2/3 training on the 3-tier x264 corpus =="
for t in cavlc main high; do
  target-pgo/release/examples/decode_bench.exe "_xbench/long_$t.264" 1 | head -1
done
# Exercise the CLI decode path too (stream splitting + output assembly).
target-pgo/release/rusty_h264.exe decode --width 1280 --height 720 \
  --in _xbench/long_high.264 --out "$PROF_DIR/train.yuv" >/dev/null 2>&1 || true
rm -f "$PROF_DIR/train.yuv"

"$PD" merge -o "$PROF_DIR/merged.profdata" "$PROF_DIR"/*.profraw

echo "== 3/3 optimized build (target-pgouse) =="
RUSTFLAGS="-Cprofile-use=$PROF_DIR/merged.profdata" CARGO_TARGET_DIR=target-pgouse \
  cargo build --release --features asm -p rusty_h264-cli
RUSTFLAGS="-Cprofile-use=$PROF_DIR/merged.profdata" CARGO_TARGET_DIR=target-pgouse \
  cargo build --release --features asm -p rusty_h264-decoder --example decode_bench

echo
echo "PGO binaries:"
echo "  target-pgouse/release/rusty_h264.exe"
echo "  target-pgouse/release/examples/decode_bench.exe"
