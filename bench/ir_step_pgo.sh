#!/usr/bin/env bash
# PGO-built decode_bench, priced on the exact instrument (Ir vein V14).
#
#   wsl -e bash -lc 'cd /mnt/e/coding/rusty_h264 && bash bench/ir_step_pgo.sh <tag> [prev-tag]'
#
# 1. instrumented build (-Cprofile-generate) into $HOME/rh264-target-pgogen;
# 2. TRAINING on the 19 feature streams (_ir/feat/*.264) -- never on the seven
#    counted streams, and the feature corpus holds no shields content at all, so
#    three of the seven measured streams are content the profile never saw;
# 3. llvm-profdata merge (the toolchain's own, so the profile format matches);
# 4. profile-use build into $HOME/rh264-target-pgouse;
# 5. bench/ir_all.sh with BENCH pointing at that binary: the same 26-stream
#    identity gate and the same callgrind count as every other brick.
#
# RUSTFLAGS REPLACES the config's target rustflags rather than adding to them,
# so the flags a normal build gets from .cargo/config.toml are restated below
# (checked against `cargo build -v`). bench/pgo.sh set RUSTFLAGS to the profile
# flag alone and so built without target-cpu=x86-64-v3, frame pointers or the
# hardening link flags: its banked 3-5% compared two different builds.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
TAG=${1:?usage: ir_step_pgo.sh <tag> [prev-tag]}
PREV=${2:-}
BASE="-C target-cpu=x86-64-v3 -C force-frame-pointers=yes -C relocation-model=pic -C link-arg=-Wl,-z,relro,-z,now -C link-arg=-Wl,-z,noexecstack"
PD="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata"
[ -x "$PD" ] || { echo "no llvm-profdata at $PD (rustup component add llvm-tools)"; exit 1; }
PROF=$HOME/rh264-pgo-prof
rm -rf "$PROF"; mkdir -p "$PROF" _ir/runs
t0=$(date +%s)
RUSTFLAGS="$BASE -Cprofile-generate=$PROF" CARGO_TARGET_DIR=$HOME/rh264-target-pgogen \
  cargo build --release -p rusty_h264-decoder --example decode_bench > "_ir/runs/$TAG.gen.build" 2>&1 \
  || { tail -30 "_ir/runs/$TAG.gen.build"; echo "GEN BUILD FAILED"; exit 1; }
for f in _ir/feat/*.264; do
  "$HOME/rh264-target-pgogen/release/examples/decode_bench" "$f" 1 > /dev/null 2>&1 || { echo "training decode failed: $f"; exit 1; }
done
"$PD" merge -o "$PROF/merged.profdata" "$PROF"/*.profraw || { echo "MERGE FAILED"; exit 1; }
RUSTFLAGS="$BASE -Cprofile-use=$PROF/merged.profdata" CARGO_TARGET_DIR=$HOME/rh264-target-pgouse \
  cargo build --release -p rusty_h264-decoder --example decode_bench > "_ir/runs/$TAG.log.build" 2>&1 \
  || { tail -30 "_ir/runs/$TAG.log.build"; echo "USE BUILD FAILED"; exit 1; }
echo "pgo build+train: $(( $(date +%s) - t0 ))s"
BENCH=$HOME/rh264-target-pgouse/release/examples/decode_bench bash bench/ir_all.sh "$TAG" $PREV > "_ir/runs/$TAG.log" 2>&1
rc=$?
tail -12 "_ir/runs/$TAG.log"
exit $rc
