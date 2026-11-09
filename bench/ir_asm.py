#!/usr/bin/env python3
"""ir_asm.py <callgrind.out (--dump-instr=yes)> <binary> <fn-substring> [min-count]

Per-instruction execution counts of one function, printed against its objdump
disassembly with source lines. Positions in the file are `instr line` pairs,
each possibly relative (+n / -n / *) to the previous cost line."""
import collections, re, subprocess, sys

path, binary, want = sys.argv[1], sys.argv[2], sys.argv[3]
min_n = int(sys.argv[4]) if len(sys.argv) > 4 else 100_000
fnames = {}
cnt = collections.Counter()
fn = None
pending_call = False
last_a = last_l = 0
for raw in open(path, encoding='utf-8', errors='replace'):
    line = raw.rstrip('\n')
    if not line or line[0] == '#':
        continue
    m = re.match(r'(c?fn)=\((\d+)\)\s*(.*)', line)
    if m:
        key, fid, name = m.groups()
        if name:
            fnames[fid] = name
        if key == 'fn':
            fn = fnames.get(fid)
        continue
    if re.match(r'(c?(fl|fi|fe|ob)|jump|jcnd)=', line):
        continue
    if line.startswith('calls='):
        pending_call = True
        continue
    if line[0] in '0123456789+-*':
        parts = line.split()
        if len(parts) < 3:
            continue
        ta, tl = parts[0], parts[1]
        def rel(t, last):
            if t == '*':
                return last
            if t[0] in '+-':
                return last + int(t, 16) if t.startswith(('+0x', '-0x')) else last + int(t, 0)
            return int(t, 0)
        last_a = rel(ta, last_a)
        last_l = rel(tl, last_l)
        if pending_call:
            pending_call = False
            continue
        if fn and want in fn:
            cnt[last_a] += int(parts[2])
if not cnt:
    sys.exit('no cost found for ' + want)
lo, hi = min(cnt), max(cnt) + 16
tot = sum(cnt.values())
dis = subprocess.run(['objdump', '-d', '-l', '--no-show-raw-insn', '-C',
                      f'--start-address={lo:#x}', f'--stop-address={hi:#x}', binary],
                     capture_output=True, text=True).stdout
print(f"fn~{want!r}: {tot:,} Ir over {len(cnt)} instructions; showing count >= {min_n:,}")
src = ''
for l in dis.split('\n'):
    if re.match(r'^/.*:\d+', l):
        src = l.split('/')[-1]
        continue
    m = re.match(r'\s*([0-9a-f]+):\s+(.*)', l)
    if not m:
        continue
    a = int(m.group(1), 16)
    n = cnt.get(a, 0)
    if n >= min_n:
        print(f"{n:12,}  {a:8x}  {m.group(2)[:60]:60s} {src}")
