#!/usr/bin/env python3
"""Self Ir per source line inside a line range of one file, from a callgrind
file of the line-tables build (bench/ir_lines.sh).

  ir_range.py <callgrind.out> <file-substring> <first> <last> [min_ir]
"""
import collections, re, sys

path, want, lo, hi = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
min_ir = int(sys.argv[5]) if len(sys.argv) > 5 else 50_000
names = {}
cost = collections.Counter()
fl = None
sub = None          # current inlined-file context (fi=/fe=)
pending_call = False
last = 0            # callgrind writes positions RELATIVE (+n / -n / *) to the last cost line
for raw in open(path, encoding='utf-8', errors='replace'):
    line = raw.rstrip('\n')
    if not line or line[0] == '#':
        continue
    m = re.match(r'(fl|fi|fe|cfl|cfi)=\((\d+)\)\s*(.*)', line)
    if m:
        key, fid, name = m.groups()
        if name:
            names[fid] = name
        if key == 'fl':
            fl = names.get(fid); sub = None
        elif key in ('fi', 'fe'):
            sub = names.get(fid)
        continue
    if line.startswith('calls='):
        pending_call = True
        continue
    if line[0].isdigit() or line[0] in '+-*':
        parts = line.split()
        if len(parts) < 2:
            continue
        t = parts[0]
        last = last if t == '*' else (last + int(t) if t[0] in '+-' else int(t))
        if pending_call:
            pending_call = False
            continue
        f = sub or fl
        if f and want in f:
            ln = last
            if lo <= ln <= hi:
                cost[ln] += int(parts[1])
tot = sum(cost.values())
print(f"{want} lines {lo}-{hi}: {tot:,} Ir self")
for ln in sorted(cost):
    if cost[ln] >= min_ir:
        print(f"  {ln:6d} {cost[ln]:14,}")
