#!/usr/bin/env python3
"""Which functions carry self cost on a given source line (inlined code included),
from a callgrind file of the line-tables build (bench/ir_lines.sh).

  ir_who_line.py <callgrind.out> <file-substring> <line> [<line> ...]
"""
import collections, re, sys

path, want = sys.argv[1], sys.argv[2]
lines = {int(x) for x in sys.argv[3:]}
names, fnames = {}, {}
cost = collections.Counter()
fl = sub = fn = None
pending_call = False
last = 0
for raw in open(path, encoding='utf-8', errors='replace'):
    line = raw.rstrip('\n')
    if not line or line[0] == '#':
        continue
    m = re.match(r'(c?)(fl|fi|fe|fn)=\((\d+)\)\s*(.*)', line)
    if m:
        c, key, fid, name = m.groups()
        table = fnames if key == 'fn' else names
        if name:
            table[fid] = name
        if c:
            continue
        if key == 'fl':
            fl = names.get(fid); sub = None
        elif key in ('fi', 'fe'):
            sub = names.get(fid)
        else:
            fn = fnames.get(fid); sub = None
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
        f = sub or fl or ''
        if want in f and last in lines:
            cost[(fn, last)] += int(parts[1])
for (f, ln), c in cost.most_common(20):
    print(f"{c:14,}  line {ln}  {f}")
