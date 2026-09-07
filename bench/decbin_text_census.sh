#!/usr/bin/env bash
# DCE-AWARE instruction census of the linked decode binary.
#
# WHY this exists alongside bench/rowhook_instr_census.sh: that script emits the
# RLIB, and an rlib carries every `pub fn` and every monomorphization whether or
# not anything reaches it. Dead-code elimination happens at LINK time. So the
# rlib census can only ATTRIBUTE a change to a symbol -- it cannot tell you
# whether the change made the shipped decoder bigger or smaller, and it reads a
# generic split (one body -> two monomorphizations) as pure growth even when the
# binary drops the arm nobody calls.
#
# This one disassembles the linked example and counts instructions in .text.
# MSVC PE release builds carry no per-function symbols, so there is no
# attribution here -- only the total. That total is the verdict; the rlib census
# says which function moved. Use both.
#
# IS IT DECODER-ONLY? YES, AND THAT WAS PROVEN, NOT ASSUMED. rusty_h264-encoder
# is a dev-dependency of the decoder crate, so it is COMPILED for every example
# and the build log mentions it -- which looks exactly like it is being linked.
# Ablation settles it: wiring 64 extra instructions into `filter_frame` (the
# blind filter entry, reachable only from the encoder, a test and an example)
# moved this count by ZERO. So the encoder half is not in the binary, the number
# below is the decoder, and a question about dead-code elimination in the
# decoder CAN be answered here.
#
# It also means the large blind-path symbols the rlib census reports
# (filter_frame_rows, derive_mb_kind, gather_tile, derive_mb_general,
# derive_mb_bs, derive_mb_packed, the i32 monomorphizations) are ALREADY absent
# from the shipped decoder -- there is no DCE win left in them.
#
# Deterministic: same toolchain + same source = same number under any load.
set -uo pipefail
cd "$(dirname "$0")/.."
OBJDUMP="/c/Program Files/LLVM/bin/llvm-objdump"
[ -x "$OBJDUMP" ] || { echo "llvm-objdump not found"; exit 1; }
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo build --release \
  -p rusty_h264-decoder --example decode_bench --features asm,std >/dev/null 2>&1 \
  || { echo "build FAILED"; exit 1; }
BIN=/f/coding/rs_h264/target-db/release/examples/decode_bench.exe
"$OBJDUMP" -d "$BIN" 2>/dev/null | awk '
  /^[0-9a-f]+: /  { n++ }
  END { printf "decode_bench .text instructions %10d\n", n }'
stat -c '%n bytes %s' "$BIN" 2>/dev/null | sed 's#.*/##'
