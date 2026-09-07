#!/usr/bin/env bash
# Assert the kind-derived uniformity hint against the mb_uniform kernel on every
# corpus stream. Two-sided: the arm is proven able to FAIL at the end.
set -uo pipefail
cd /f/coding/rs_h264
B=/f/coding/rs_h264/target-kn/release/examples/decode_bench.exe
ok=0; bad=0
for f in _xbench/tt/*.264 bench/_map/*.264; do
  out=$(RS_H264_VERIFY_UNIFORM_HINT=1 "$B" "$f" reps=1 maxf=25 2>&1)
  if echo "$out" | grep -qiE 'panic|disagree'; then
    bad=$((bad+1)); echo "DISAGREE: $f"
  else ok=$((ok+1)); fi
done
echo "verify_uniform_hint: streams_ok=$ok disagreements=$bad"
