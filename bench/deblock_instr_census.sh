#!/usr/bin/env bash
# Deterministic instruction census of the DEBLOCK symbols, x86-64.
# Same toolchain + same source = same number on any machine under any load.
set -uo pipefail
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-accel --lib -- --emit asm >/dev/null 2>&1 || { echo "accel asm build FAILED"; exit 1; }
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-common --lib --features asm -- --emit asm >/dev/null 2>&1 || { echo "common asm build FAILED"; exit 1; }
cd /f/coding/rs_h264/target-db/release/deps
python3 - "$@" <<'PY'
import re,io,glob,sys
INSTR=re.compile(r'^\s+[a-z][a-z0-9.]*\s')
LBL=re.compile(r'^([a-zA-Z_$][a-zA-Z0-9_$.@]*):\s*$')
PACK=re.compile(r'^\s+(v?p(add|sub|mull|mulh|madd|maddubs|and|andn|or|xor|shuf|unpck|cmpe|cmpg|ack|avg|abs|max|min|sll|srl|sra|blend|movmsk|extr|insr|sad|mulhrsw|mulld|cmpeq|cmpgt)|v?(add|sub|mul|max|min|and|or|xor|cmp)(ps|pd)\b)',re.I)
PANIC=re.compile(r'panic|bounds_check',re.I)
d={}
for f in glob.glob('*.s'):
    sym=None
    for line in io.open(f,encoding='utf-8',errors='replace'):
        m=LBL.match(line)
        if m:
            sym=m.group(1); sym=None if sym.startswith(('$','.')) else sym
            if sym and sym not in d: d[sym]=[0,0,0]
            continue
        if sym is None: continue
        if INSTR.match(line):
            d[sym][0]+=1
            if PACK.match(line): d[sym][1]+=1
            if PANIC.search(line): d[sym][2]+=1
KEYS=['deblock_simd','7deblock']
rows=[]
for s,v in d.items():
    if not any(k in s for k in KEYS): continue
    ids=re.findall(r'[0-9]+([a-zA-Z_][a-zA-Z0-9_]{2,})', s)
    nm='/'.join([x for x in ids if x not in ('rusty','h264','accel','common','core','alloc','std')][-2:])
    if v[0]>=12: rows.append((nm,v[0],v[1],v[2]))
rows.sort(key=lambda r:-r[1])
tot=sum(r[1] for r in rows)
print(f"{'symbol':<44} {'instrs':>7} {'packed':>7} {'panic':>6}")
print('-'*70)
for nm,i,p,pa in rows[:34]: print(f"{nm:<44} {i:>7} {p:>7} {pa:>6}")
print('-'*70)
print(f"{'TOTAL (deblock symbols)':<44} {tot:>7}")
PY
