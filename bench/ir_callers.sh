#!/usr/bin/env bash
# Section (e) of the census for a run: who calls memset/memcpy, per stream.
#   bash bench/ir_callers.sh <tag> [top] [stream-substring ...]
set -uo pipefail
cd "$(dirname "$0")/.."
TAG=$1; TOP=${2:-18}; shift; shift || true
pats=${*:-shields_ter__high FourPeople_1280x720_60__main shields_ter__cavlc}
for p in $pats; do
  for f in _ir/runs/$TAG/*$p*.out; do
    echo "=== $(basename "$f" .out)"
    python3 bench/callgrind_census.py "$f" "$TOP" | grep -A "$TOP" 'WHO CALLS' \
      | grep -v -E '_dl_|strdup|env::|getenv|btree|FilterMap|args_os|0x0000|grisu|realloc_live|SMALL OUT'
  done
done
