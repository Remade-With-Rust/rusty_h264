#!/usr/bin/env bash
#
# DECODER ANATOMY: per-function ms AND deterministic call counts, across content
# classes and tool tiers. Emits RAW blocks; bench/decode_anatomy_report.py parses.
#
# TWO BINARIES, deliberately. The stage profiler charges an rdtsc pair per scope and
# the fine buckets are entered millions of times, so a profiled build's wall is
# INFLATED and its shares skew toward high-call-count stages. Therefore:
#
#   * HONEST WALL comes from a build WITHOUT `--features profile`. That is the only
#     number ever quoted as decode speed.
#   * STAGE TABLE comes from a build WITH it, used ONLY to rank stages and to read
#     CALL COUNTS.
#
# The call counts are the durable half. A count is deterministic: no pinning, no ABBA,
# no repetitions, identical on a loaded box and an idle one. Where a count and a time
# disagree, the count wins. Printing both makes an absurd ns/call legible as an
# instrument problem rather than a finding.
#
# The profiler TAX is measured per stream (profiled wall / honest wall) so no stage
# share is ever read without knowing how inflated its run was.
#
# Parsing lives in Python because stage names contain spaces and parentheses
# ("pred-buf copy", "scatter(store)"), which whitespace-splitting silently mangles.
#
#   bash bench/decode_anatomy.sh > bench/_map/decode_anatomy.raw
set -uo pipefail
cd "$(dirname "$0")/.."

T=${DPROF_DIR:-_dprof}
HONEST=${HONEST_BIN:-/tmp/decode_prof_honest.exe}
PROF=${PROF_BIN:-/tmp/decode_prof_prof.exe}
REPS=${DP_REPS:-5}

[ -x "$HONEST" ] || { echo "missing $HONEST" >&2; exit 1; }
[ -x "$PROF" ]   || { echo "missing $PROF" >&2; exit 1; }

for f in "$T"/*__*.264; do
  b=$(basename "$f" .264)
  echo "===STREAM $b"
  echo "---HONEST"
  DP_REPS=$REPS "$HONEST" "$f" 2>/dev/null
  echo "---PROFILED"
  DP_REPS=1 RS_H264_PROF_SAMPLE=1 "$PROF" "$f" 2>/dev/null
  # SAMPLED: time 1 call in 64. The scope guard is an rdtsc pair and the per-MB
  # buckets are entered tens of millions of times, so exact timing taxes the run
  # 1.3-1.4x and inflates precisely the high-call-count stages whose share we are
  # trying to read. Sampling scales the survivors back up, so shares stay unbiased
  # while the tax falls ~64x; counts are a relaxed add and stay EXACT either way.
  # Stages entered under 8192 times are always timed exactly (sampling `Total`,
  # entered once per frame, would estimate the whole denominator from one sample).
  # THREE sampled passes. A single instrumented pass inherits this box's drift, and
  # the first run of this harness showed the sampled residue at 4.5% against the exact
  # instrument's 36.8% on the same stream -- a disagreement far too large to publish.
  # Per-stream medians across passes is the cheapest thing that fixes it.
  for i in 1 2 3; do
    echo "---SAMPLED64"
    DP_REPS=1 RS_H264_PROF_SAMPLE=64 "$PROF" "$f" 2>/dev/null
  done
done
