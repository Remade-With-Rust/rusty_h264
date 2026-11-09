#!/usr/bin/env python3
"""The four callgrind censuses from one raw callgrind.out (no annotate needed).

  (a) self cost by function      -- where are the instructions?
  (b) call counts per function   -- called a lot, or expensive per call?
  (c) caller->callee EDGES       -- which caller is buying it?
  (d) line census                -- only if the binary carried line tables

Usage: callgrind_census.py <callgrind.out> [top]

Reads the compressed name format (`fn=(id) name` defines, `fn=(id)` refers).
Self cost = sum of the function's own cost lines; inclusive-per-edge = the cost
line following each `calls=` record. Symbol hashes (::h0123abcd) are stripped.
"""
import re, sys, collections

path = sys.argv[1]
top = int(sys.argv[2]) if len(sys.argv) > 2 else 40

names = {}            # id -> name  (per name space: fn / fl / cfn / cfl)
HASH = re.compile(r'::h[0-9a-f]{16}')

def resolve(space, tok):
    m = re.match(r'\((\d+)\)\s*(.*)', tok)
    if not m:
        return tok
    k = (space, m.group(1))
    if m.group(2):
        names[k] = m.group(2)
    return names.get(k, tok)

self_cost = collections.Counter()
calls_to = collections.Counter()           # callee -> calls
edge_calls = collections.Counter()         # (caller, callee) -> calls
edge_incl = collections.Counter()          # (caller, callee) -> inclusive Ir
line_cost = collections.Counter()          # (file, line) -> self Ir
cur_fn = None
cur_fl = None
pending_call = None                        # (callee, ncalls) awaiting its cost line
total = None
last_ln = 0                                # positions are RELATIVE (+n / -n / *) to the last cost line

with open(path, encoding='utf-8', errors='replace') as f:
    for raw in f:
        line = raw.rstrip('\n')
        if not line or line[0] == '#':
            continue
        if line.startswith(('summary:', 'totals:')):
            total = int(line.split()[1]); continue
        if line.startswith('fl=') or line.startswith('fi=') or line.startswith('fe='):
            cur_fl = resolve('fl', line[3:]); continue
        if line.startswith('fn='):
            cur_fn = HASH.sub('', resolve('fn', line[3:])); continue
        if line.startswith('cfl=') or line.startswith('cfi='):
            resolve('fl', line[4:]); continue
        if line.startswith('cfn='):
            pending_callee = HASH.sub('', resolve('fn', line[4:])); continue
        if line.startswith('calls='):
            n = int(line.split()[0][6:]); pending_call = (pending_callee, n); continue
        if line.startswith(('ob=', 'cob=', 'positions:', 'events:', 'version:', 'creator:', 'pid:', 'cmd:', 'part:', 'desc:')):
            continue
        # cost line: "<line-or-+/-delta> <Ir>"
        parts = line.split()
        if len(parts) < 2 or cur_fn is None:
            continue
        try:
            ir = int(parts[1])
        except ValueError:
            continue
        # Resolve the position BEFORE the call/self split: a call's cost line moves
        # the reference point too. (Until 2026-11-08 only absolute positions were
        # counted, so the line census under-read every line written as a delta.)
        t = parts[0]
        try:
            last_ln = last_ln if t == '*' else (last_ln + int(t) if t[0] in '+-' else int(t))
        except ValueError:
            pass
        if pending_call is not None:
            callee, n = pending_call
            calls_to[callee] += n
            edge_calls[(cur_fn, callee)] += n
            edge_incl[(cur_fn, callee)] += ir
            pending_call = None
        else:
            self_cost[cur_fn] += ir
            if cur_fl and not cur_fl.startswith('???'):
                line_cost[(cur_fl, last_ln)] += ir

tot = total or sum(self_cost.values())
print(f"\n== (a) SELF COST by function  (total Ir {tot:,}) ==")
print(f"{'self Ir':>16} {'%':>6} {'calls':>12} {'Ir/call':>10}  function")
for fn, ir in self_cost.most_common(top):
    c = calls_to.get(fn, 0)
    per = f"{ir / c:10.1f}" if c else f"{'inl':>10}"
    print(f"{ir:16,} {100 * ir / tot:6.2f} {c:12,} {per}  {fn[:110]}")

print(f"\n== (b) CALL COUNTS  (most-called functions; 'inl' above = inlined, never called) ==")
print(f"{'calls':>12} {'incl Ir':>16} {'Ir/call':>10}  callee")
incl_by_callee = collections.Counter()
for (caller, callee), ir in edge_incl.items():
    incl_by_callee[callee] += ir
for fn, c in calls_to.most_common(top):
    inc = incl_by_callee[fn]
    print(f"{c:12,} {inc:16,} {inc / c:10.1f}  {fn[:110]}")

print(f"\n== (c) EDGES  (caller -> callee, by inclusive Ir bought through that edge) ==")
print(f"{'calls':>12} {'incl Ir':>16} {'Ir/call':>10}  caller -> callee")
for (caller, callee), ir in edge_incl.most_common(top):
    c = edge_calls[(caller, callee)]
    print(f"{c:12,} {ir:16,} {ir / max(c, 1):10.1f}  {caller[:60]} -> {callee[:60]}")

print(f"\n== (e) WHO CALLS THE LIBC KERNELS  (memset / memcpy / memmove callers, by calls) ==")
print(f"{'calls':>12} {'incl Ir':>16} {'Ir/call':>10}  caller -> kernel")
libc = [(k, v) for k, v in edge_calls.items() if re.search(r'mem(set|cpy|move)', k[1])]
for (caller, callee), c in sorted(libc, key=lambda kv: -kv[1])[:top]:
    ir = edge_incl[(caller, callee)]
    print(f"{c:12,} {ir:16,} {ir / max(c, 1):10.1f}  {caller[:70]} -> {callee[:28]}")

print(f"\n== (f) SMALL OUT-OF-LINE FUNCTIONS  (>= 10k calls and < 120 Ir/call: a frame per call) ==")
print(f"{'calls':>12} {'incl Ir':>16} {'Ir/call':>10}  callee")
small = [(fn, c) for fn, c in calls_to.items() if c >= 10_000 and incl_by_callee[fn] / c < 120]
for fn, c in sorted(small, key=lambda kv: -kv[1])[:top]:
    inc = incl_by_callee[fn]
    print(f"{c:12,} {inc:16,} {inc / c:10.1f}  {fn[:110]}")

if line_cost:
    print(f"\n== (d) LINE CENSUS  (self Ir attributed to source lines) ==")
    for (fl, ln), ir in line_cost.most_common(top):
        print(f"{ir:16,} {100 * ir / tot:6.2f}  {fl}:{ln}")
else:
    print("\n== (d) LINE CENSUS: binary has no line tables (build with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only) ==")
