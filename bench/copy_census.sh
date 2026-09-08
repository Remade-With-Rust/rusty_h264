#!/usr/bin/env bash
# EMITTED-ASSEMBLY COPY CENSUS for the decoder.
#
# WHY NOT A SOURCE GREP: a grep for `copy_from_slice` finds only the copies
# somebody WROTE. The assembly finds three more classes that matter as much:
#
#   * `call memcpy` / `call memset` the OPTIMISER introduced -- a large struct
#     moved by value, a `Vec` grown, an array returned;
#   * stack temporaries that get a memset because the compiler cannot prove they
#     are fully written before use;
#   * short runtime-length `copy_from_slice`/`fill` that did NOT inline and
#     became an opaque call, which clobbers volatiles and forces spills around it.
#
# A constant-length copy inlines and never appears here, which is the point: the
# calls this prints are exactly the ones with call overhead attached.
#
# Deterministic: same toolchain + same source = same number under any load.
set -uo pipefail
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-decoder \
  --lib --features asm -- --emit asm >/dev/null 2>&1 || { echo "asm build FAILED"; exit 1; }
S=$(ls -t /f/coding/rs_h264/target-db/release/deps/rusty_h264_decoder-*.s | head -1)
python3 - "$S" <<'PY'
import re, io, sys, collections
LBL = re.compile(r'^([A-Za-z_$][\w$.@]*):\s*$')
CALL = re.compile(r'^\s+callq?\s+\*?([\w$.@]+)')
cur = None
hits = collections.defaultdict(lambda: collections.Counter())
instrs = collections.Counter()
for line in io.open(sys.argv[1], encoding='utf-8', errors='replace'):
    m = LBL.match(line)
    if m:
        s = m.group(1)
        cur = None if s.startswith(('$', '.')) else s
        continue
    if cur is None:
        continue
    if re.match(r'^\s+[a-z]', line):
        instrs[cur] += 1
    c = CALL.match(line)
    if c:
        f = c.group(1)
        for k in ('memcpy', 'memset', 'memmove'):
            if k in f:
                hits[cur][k] += 1
def pretty(sym):
    ids = re.findall(r'[0-9]+([A-Za-z_][A-Za-z0-9_]*)', sym)
    ids = [i for i in ids if i not in ('rusty','h264','decoder','common','accel','core','alloc','std')]
    return '/'.join(ids[-2:]) or sym[:46]
rows = []
for sym, c in hits.items():
    rows.append((pretty(sym), c['memcpy'], c['memset'], c['memmove'],
                 c['memcpy'] + c['memset'] + c['memmove'], instrs[sym]))
rows.sort(key=lambda r: -r[4])
print("%-46s %6s %6s %7s %6s %8s" % ('symbol', 'memcpy', 'memset', 'memmove', 'total', 'instrs'))
print('-' * 84)
for r in rows[:28]:
    print("%-46s %6d %6d %7d %6d %8d" % r)
print('-' * 84)
print("%-46s %6d %6d %7d %6d" % ('TOTAL (decoder rlib)',
      sum(r[1] for r in rows), sum(r[2] for r in rows),
      sum(r[3] for r in rows), sum(r[4] for r in rows)))
print("symbols carrying at least one: %d" % len(rows))
PY
