#!/usr/bin/env bash
#
# BYTE-IDENTITY GATE — the 68-stream tt corpus, ours vs ffmpeg, plus a
# skip-fast-path A/B arm.
#
# WHY THIS SCRIPT EXISTS IN bench/ AND NOT IN A SCRATCHPAD. The scratchpad copy
# it replaces took a binary path as $1 AND IGNORED IT, hardcoding
# `target/release/rusty_h264.exe` instead. Every caller believed it was gating
# the binary it had just built; it was gating whatever the CLI happened to be.
# On 2026-08-21 that CLI was 14 hours stale, so four consecutive "68/68 ALL
# BYTE-IDENTICAL" results were about code nobody had changed — and a genuinely
# broken decoder (mvd-cache hoist -> MbError::Truncated) sailed through twice.
#
# THE RULE THIS ENCODES: a gate must BUILD what it is about to test. An argument
# a script accepts and ignores is worse than no argument, because it manufactures
# the belief that something was checked.
#
# The decoder under test is the CLI's `decode` (it writes YUV; decode_bench
# discards frames and cannot be compared pixel-wise). The CLI is rebuilt here,
# every run, with `--features asm` — the shipped kernel configuration. A scalar
# build decodes byte-identically and would pass this gate while being ~2x slower,
# so the gate cannot be used to certify a benchmark binary's configuration.
#
#   bash bench/ident_gate.sh            # rebuild + gate all 68
#   NO_BUILD=1 bash bench/ident_gate.sh # gate the CLI as it stands (rare)
set -uo pipefail
cd "$(dirname "$0")/.."

CLI=target/release/rusty_h264.exe
if [ "${NO_BUILD:-0}" != "1" ]; then
  echo "building the CLI under test (--features asm)…"
  cargo build --release -p rusty_h264-cli --features asm 2>&1 | tail -1
fi
[ -x "$CLI" ] || { echo "FATAL: $CLI missing"; exit 1; }

# Refuse to gate a CLI older than the decoder sources it is supposed to embody:
# that is precisely the failure this script was written to make impossible.
newest_src=$(find crates -name '*.rs' -newer "$CLI" -print -quit 2>/dev/null)
if [ -n "$newest_src" ]; then
  echo "FATAL: $CLI is OLDER than $newest_src — it does not contain the code you"
  echo "       are testing. Re-run without NO_BUILD=1."
  exit 1
fi

# PER-INVOCATION scratch. These were fixed /tmp names, so two gates running at
# once (two agent sessions, or a gate beside a bench) overwrote each other's
# YUVs and reported diffs that were pure collision — it manufactured failures
# on tempete and foreman that had nothing to do with either tree.
IDENT_TMP=$(mktemp -d 2>/dev/null || echo "/tmp/ident.$$")
mkdir -p "$IDENT_TMP"
trap 'rm -rf "$IDENT_TMP"' EXIT
fail=0; n=0
for f in _xbench/tt/*.264; do
  id=$(basename "$f" .264)
  wh=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0 "$f")
  w=${wh%%,*}; h=${wh##*,}
  ffmpeg -v error -y -i "$f" -f rawvideo "$IDENT_TMP/ff.yuv" 2>/dev/null
  # Arm A pins the skip fast path OFF, arm B is the default: both must agree with
  # each other AND with ffmpeg, so a fast path that is wrong shows as ARM-DIFF
  # rather than hiding behind a matching-but-wrong pair.
  RS_H264_NO_BSKIPFAST=1 "$CLI" decode --width "$w" --height "$h" --in "$f" --out "$IDENT_TMP/a.yuv" >/dev/null 2>&1 \
    || { echo "DECODE-FAIL(A) $id"; fail=1; n=$((n+1)); continue; }
  "$CLI" decode --width "$w" --height "$h" --in "$f" --out "$IDENT_TMP/b.yuv" >/dev/null 2>&1 \
    || { echo "DECODE-FAIL(B) $id"; fail=1; n=$((n+1)); continue; }
  ha=$(md5sum "$IDENT_TMP/a.yuv" | cut -d' ' -f1)
  hb=$(md5sum "$IDENT_TMP/b.yuv" | cut -d' ' -f1)
  hf=$(md5sum "$IDENT_TMP/ff.yuv" | cut -d' ' -f1)
  n=$((n+1))
  if [ "$ha" != "$hb" ]; then echo "ARM-DIFF  $id"; fail=1
  elif [ "$hb" != "$hf" ]; then echo "FF-DIFF   $id"; fail=1; fi
done
echo "checked=$n fail=$fail"
[ "$fail" = 0 ] && echo "ALL BYTE-IDENTICAL"
exit $fail
