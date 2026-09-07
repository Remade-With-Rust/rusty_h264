#!/usr/bin/env bash
# Deterministic instruction census of the dec-mb-B PATH symbols.
#
# Same instrument as bench/deblock_instr_census.sh: emitted asm, per-symbol
# instruction / packed-op counts. No clock, so it is valid on a loaded box and
# a 2-instruction change is a verdict rather than noise.
set -uo pipefail
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-decoder --lib --features asm -- --emit asm >/dev/null 2>&1 \
  || { echo "decoder asm build FAILED"; exit 1; }
cd /f/coding/rs_h264/target-db/release/deps
python3 - "$@" <<'PY'
import re,io,glob,sys
INSTR=re.compile(r'^\s+[a-z][a-z0-9.]*\s')
LBL=re.compile(r'^([a-zA-Z_$][a-zA-Z0-9_$.@]*):\s*$')
PACK=re.compile(r'^\s+(v?p(add|sub|mull|mulh|madd|maddubs|and|andn|or|xor|shuf|unpck|cmpe|cmpg|ack|avg|abs|max|min|sll|srl|sra|blend|movmsk|extr|insr|sad)|v?(add|sub|mul|max|min|and|or|xor|cmp)(ps|pd)\b)',re.I)
BOUND=re.compile(r'panic_bounds_check')
# the B arm: slice loop, B macroblock bodies, B motion comp, B recon, span flush
KEYS=['decode_slice_cabac','decode_b_skip','b_mc','recon_b','bz_flush','bz_recon','b_direct',
      'b_set_motion','inter_finish','decode_b_mb','pz_flush','predict_mv','decode_mb_b']
d={}
for f in glob.glob('rusty_h264_decoder-*.s'):
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
            if BOUND.search(line): d[sym][2]+=1
rows=[]
for s,v in d.items():
    ids=re.findall(r'[0-9]+([a-zA-Z_][a-zA-Z0-9_]{2,})', s)
    low=s.lower()
    if not any(k in low for k in KEYS): continue
    nm='/'.join([x for x in ids if x not in ('rusty','h264','decoder','common','core','alloc','std')][-2:])
    if v[0]>=20: rows.append((nm,v[0],v[1],v[2]))
rows.sort(key=lambda r:-r[1])
tot=sum(r[1] for r in rows)
print("%-52s %7s %7s %6s" % ('symbol','instrs','packed','bnds'))
print('-'*76)
for nm,i,p,b in rows[:26]: print("%-52s %7d %7d %6d" % (nm,i,p,b))
print('-'*76)
print("%-52s %7d" % ('TOTAL (B-path symbols)', tot))
PY
