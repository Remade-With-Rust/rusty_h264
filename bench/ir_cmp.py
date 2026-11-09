#!/usr/bin/env python3
"""Compare instruction-count runs from bench/ir_all.sh.

  ir_cmp.py <tag>                 table of one run
  ir_cmp.py <tag> <prev>          per-stream delta of <tag> against <prev>
  ir_cmp.py --fn <tag> <prev> [stream] [top]
                                  per-FUNCTION self-cost and call-count deltas
                                  (attribution: what actually moved)
"""
import collections, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '_ir', 'runs')
SHORT = {
    'foreman_cif__cavlc': 'fm-cavlc', 'foreman_cif__main': 'fm-main',
    'FourPeople_1280x720_60__main': 'FP-main', 'FourPeople_1280x720_60__high': 'FP-high',
    '720p50_shields_ter__cavlc': 'sh-cavlc', '720p50_shields_ter__main': 'sh-main',
    '720p50_shields_ter__high': 'sh-high',
}


def load(tag):
    rows = collections.OrderedDict()
    with open(os.path.join(ROOT, tag, 'summary.tsv')) as f:
        for line in f:
            p = line.rstrip('\n').split('\t')
            rows[p[0]] = int(p[1])
    return rows


def table(tag, prev=None):
    a = load(tag)
    b = load(prev) if prev else None
    print(f"{'stream':10} {'Ir':>15}" + (f" {'prev':>15} {'delta':>13} {'%':>8}" if b else ''))
    for s, ir in a.items():
        name = SHORT.get(s, s)
        if b and s in b:
            d = ir - b[s]
            print(f"{name:10} {ir:15,} {b[s]:15,} {d:+13,} {100 * d / b[s]:+8.3f}")
        else:
            print(f"{name:10} {ir:15,}")


HASH = re.compile(r'::h[0-9a-f]{16}')


def parse(path):
    names, self_cost, calls = {}, collections.Counter(), collections.Counter()
    cur = callee = None
    pending = None
    for raw in open(path, encoding='utf-8', errors='replace'):
        line = raw.rstrip('\n')
        if not line or line[0] == '#':
            continue
        if line.startswith('fn='):
            m = re.match(r'\((\d+)\)\s*(.*)', line[3:])
            if m and m.group(2):
                names[m.group(1)] = HASH.sub('', m.group(2))
            cur = names.get(m.group(1)) if m else line[3:]
            continue
        if line.startswith('cfn='):
            m = re.match(r'\((\d+)\)\s*(.*)', line[4:])
            if m and m.group(2):
                names[m.group(1)] = HASH.sub('', m.group(2))
            callee = names.get(m.group(1)) if m else line[4:]
            continue
        if line.startswith('calls='):
            pending = (callee, int(line.split()[0][6:]))
            continue
        if line[0].isdigit() or line[0] in '+-*':
            parts = line.split()
            if len(parts) < 2 or cur is None:
                continue
            try:
                ir = int(parts[1])
            except ValueError:
                continue
            if pending:
                calls[pending[0]] += pending[1]
                pending = None
            else:
                self_cost[cur] += ir
    return self_cost, calls


def fn_delta(tag, prev, stream, top):
    sa, ca = parse(os.path.join(ROOT, tag, stream + '.out'))
    sb, cb = parse(os.path.join(ROOT, prev, stream + '.out'))
    keys = set(sa) | set(sb)
    rows = sorted(keys, key=lambda k: abs(sa.get(k, 0) - sb.get(k, 0)), reverse=True)[:top]
    print(f"== {stream}: self-cost delta by function ({tag} vs {prev}) ==")
    print(f"{'delta self':>13} {'new self':>13} {'calls new':>11} {'calls old':>11}  function")
    for k in rows:
        d = sa.get(k, 0) - sb.get(k, 0)
        if d == 0:
            continue
        print(f"{d:+13,} {sa.get(k, 0):13,} {ca.get(k, 0):11,} {cb.get(k, 0):11,}  {k[:100]}")


if __name__ == '__main__':
    args = sys.argv[1:]
    if args and args[0] == '--fn':
        tag, prev = args[1], args[2]
        streams = [args[3]] if len(args) > 3 and not args[3].isdigit() else list(SHORT)
        top = int(args[-1]) if args[-1].isdigit() else 25
        for s in streams:
            fn_delta(tag, prev, s, top)
    else:
        table(args[0], args[1] if len(args) > 1 else None)
