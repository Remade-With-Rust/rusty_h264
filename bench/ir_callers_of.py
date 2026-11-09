#!/usr/bin/env python3
"""Callers of a function (substring match on the callee), with call counts and
inclusive Ir bought through each edge.  ir_callers_of.py <callgrind.out> <callee> [depth]

depth 2 also prints each caller's own callers (one level further up)."""
import collections, re, sys

path, want = sys.argv[1], sys.argv[2]
depth = int(sys.argv[3]) if len(sys.argv) > 3 else 1
HASH = re.compile(r'::h[0-9a-f]{16}')
names = {}
calls = collections.Counter()
incl = collections.Counter()
cur = callee = None
pending = None
for raw in open(path, encoding='utf-8', errors='replace'):
    line = raw.rstrip('\n')
    if line.startswith('fn='):
        m = re.match(r'\((\d+)\)\s*(.*)', line[3:])
        if m and m.group(2):
            names[m.group(1)] = HASH.sub('', m.group(2))
        cur = names.get(m.group(1)) if m else line[3:]
    elif line.startswith('cfn='):
        m = re.match(r'\((\d+)\)\s*(.*)', line[4:])
        if m and m.group(2):
            names[m.group(1)] = HASH.sub('', m.group(2))
        callee = names.get(m.group(1)) if m else line[4:]
    elif line.startswith('calls='):
        pending = int(line.split()[0][6:])
    elif pending is not None and line and line[0].isdigit():
        parts = line.split()
        calls[(cur, callee)] += pending
        incl[(cur, callee)] += int(parts[1])
        pending = None


def show(target, level):
    rows = [(k, v) for k, v in calls.items() if target in k[1]]
    rows.sort(key=lambda kv: -incl[kv[0]])
    for (caller, ce), n in rows[:12]:
        print(f"{'  ' * level}{n:10,} calls {incl[(caller, ce)]:14,} Ir  {caller[:90]} -> {ce[:40]}")
        if level + 1 < depth:
            show(caller, level + 1)


show(want, 0)
