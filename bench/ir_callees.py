#!/usr/bin/env python3
"""Callees of one function (calls and inclusive Ir bought through each edge),
side by side for two callgrind files: what changed in a function's call graph.

  ir_callees.py <new.out> <old.out> <fn-substring>
"""
import collections, re, sys

def callees(path, want):
    names = {}
    out = collections.defaultdict(lambda: [0, 0])
    fn = cfn = None
    pending = None
    for raw in open(path, encoding='utf-8', errors='replace'):
        line = raw.rstrip('\n')
        m = re.match(r'(c?fn)=\((\d+)\)\s*(.*)', line)
        if m:
            key, fid, name = m.groups()
            if name:
                names[fid] = name
            if key == 'fn':
                fn = names.get(fid)
            else:
                cfn = names.get(fid)
            continue
        if line.startswith('calls='):
            pending = int(line.split()[0][6:])
            continue
        if pending is not None and line and (line[0].isdigit() or line[0] in '+-*'):
            parts = line.split()
            if fn and want in fn and cfn:
                out[cfn][0] += pending
                out[cfn][1] += int(parts[-1])
            pending = None
    return out

new, old, want = sys.argv[1], sys.argv[2], sys.argv[3]
a, b = callees(new, want), callees(old, want)
keys = sorted(set(a) | set(b), key=lambda k: -max(a.get(k, [0, 0])[1], b.get(k, [0, 0])[1]))
print(f"{'calls new':>10} {'calls old':>10} {'incl new':>14} {'incl old':>14}  callee of ~{want!r}")
for k in keys[:30]:
    an, bo = a.get(k, [0, 0]), b.get(k, [0, 0])
    print(f"{an[0]:10,} {bo[0]:10,} {an[1]:14,} {bo[1]:14,}  {k[:90]}")
