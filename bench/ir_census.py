#!/usr/bin/env python3
"""Call-count + caller->callee EDGE census from a raw callgrind.out file.

  python3 -I bench/ir_census.py cg.out [callee-substring ...]

Prints (a) the top functions by SELF Ir with their call counts and Ir/call (the
"called a lot, or expensive per call?" census), and (b) for each requested callee
substring every caller edge with its call count and inclusive Ir (the "which caller
is buying it?" census). Parses the raw format (fn=/cfn=/calls= records), so it does
not depend on callgrind_annotate's tree formatting. Companion of bench/ir_census.sh.
"""
import collections
import re
import sys

path = sys.argv[1]
wants = sys.argv[2:]

ID = re.compile(r'^\((\d+)\)(?: (.*))?$')
self_ir = collections.Counter()
calls_in = collections.Counter()
edges = collections.defaultdict(lambda: [0, 0])  # (caller, callee) -> [calls, inclusive Ir]


def resolve(tok, table):
    m = ID.match(tok)
    if m:
        i, nm = m.group(1), m.group(2)
        if nm is not None:
            table[i] = nm
        return table[i]
    return tok


names = {}
cur = None
callee = None
pending = None  # (callee, calls) awaiting its cost line
with open(path, encoding='utf-8', errors='replace') as f:
    for line in f:
        line = line.rstrip('\n')
        if line.startswith('fn='):
            cur = resolve(line[3:], names)
            pending = None
        elif line.startswith('cfn='):
            callee = resolve(line[4:], names)
        elif line.startswith('calls='):
            pending = (callee, int(line.split('=')[1].split()[0]))
        elif line and (line[0].isdigit() or line[0] in '+-*'):
            parts = line.split()
            if len(parts) < 2:
                continue
            try:
                cost = int(parts[1])
            except ValueError:
                continue
            if pending is not None:
                ce, n = pending
                e = edges[(cur, ce)]
                e[0] += n
                e[1] += cost
                calls_in[ce] += n
                pending = None
            elif cur is not None:
                self_ir[cur] += cost


def short(n):
    for a, b in (('rusty_h264_encoder::', 'enc::'), ('rusty_h264_common::', 'com::'),
                 ('rusty_h264_accel::', 'acc::'), ('<enc::mb16::FrameEncoder>', 'FE')):
        n = n.replace(a, b)
    return n[:96]


total = sum(self_ir.values())
print(f"TOTAL Ir {total:,}")
print(f"\n{'self Ir':>15} {'%':>6} {'calls':>10} {'Ir/call':>9}  function")
for fn, ir in self_ir.most_common(45):
    c = calls_in.get(fn, 0)
    per = f"{ir / c:9.1f}" if c else "        -"
    print(f"{ir:15,} {100 * ir / total:6.2f} {c:10,} {per}  {short(fn)}")

for w in wants:
    print(f"\n=== callers of *{w}* ===")
    rows = sorted(((k, v) for k, v in edges.items() if w in k[1]), key=lambda kv: -kv[1][1])
    for (caller, ce), (n, ir) in rows[:14]:
        print(f"{ir:15,} ({100 * ir / total:5.2f}%) {n:10,}x  {short(caller)}  ->  {short(ce)}")
