#!/usr/bin/env python3
"""Self Ir of ONE function broken down by (source file, line), inlined code
included, from a callgrind file of the line-tables build (bench/ir_lines.sh).
Answers "what is this function's self cost actually made of" when most of it
is code inlined from other files.

  ir_fn_lines.py <callgrind.out> <fn-substring> [min_ir] [top]
"""
import collections, re, sys

path, want = sys.argv[1], sys.argv[2]
min_ir = int(sys.argv[3]) if len(sys.argv) > 3 else 100_000
top = int(sys.argv[4]) if len(sys.argv) > 4 else 60
names, fnames = {}, {}
cost = collections.Counter()
by_file = collections.Counter()
fl = sub = fn = None
pending_call = False
last = 0            # callgrind writes positions RELATIVE (+n / -n / *) to the last cost line
for raw in open(path, encoding='utf-8', errors='replace'):
    line = raw.rstrip('\n')
    if not line or line[0] == '#':
        continue
    m = re.match(r'(fl|fi|fe|fn)=\((\d+)\)\s*(.*)', line)
    if m:
        key, fid, name = m.groups()
        table = fnames if key == 'fn' else names
        if name:
            table[fid] = name
        if key == 'fl':
            fl = names.get(fid); sub = None
        elif key in ('fi', 'fe'):
            sub = names.get(fid)
        else:
            fn = fnames.get(fid); sub = None
        continue
    if re.match(r'c(fl|fi|fn|ob)=', line):
        m2 = re.match(r'c(fl|fi|fn)=\((\d+)\)\s*(.*)', line)
        if m2 and m2.group(3):
            (fnames if m2.group(1) == 'fn' else names)[m2.group(2)] = m2.group(3)
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
        if fn and want in fn:
            f = (sub or fl or '?').split('/crates/')[-1]
            c = int(parts[1])
            cost[(f, last)] += c
            by_file[f] += c
tot = sum(cost.values())
print(f"fn~{want!r}: {tot:,} Ir self")
for f, c in by_file.most_common():
    print(f"  {c:14,}  {f}")
print("-- lines")
for (f, ln), c in cost.most_common(top):
    if c < min_ir:
        break
    print(f"  {c:14,}  {f}:{ln}")
