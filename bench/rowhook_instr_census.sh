#!/usr/bin/env bash
# Deterministic instruction census of the ROW-HOOK path: the row deblock, the
# boundary-strength derivation it calls, and the EDC flush. Same instrument as
# bench/deblock_instr_census.sh -- emitted asm, no clock, valid on a loaded box.
set -uo pipefail
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-decoder --lib --features asm -- --emit asm >/dev/null 2>&1 || { echo "decoder asm FAILED"; exit 1; }
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo rustc --release -p rusty_h264-common  --lib --features asm -- --emit asm >/dev/null 2>&1 || { echo "common asm FAILED";  exit 1; }
cd /f/coding/rs_h264/target-db/release/deps
python3 - "$@" <<'PY'
import re,io,glob
INSTR=re.compile(r'^\s+[a-z][a-z0-9.]*\s')
LBL=re.compile(r'^([a-zA-Z_$][a-zA-Z0-9_$.@]*):\s*$')
PACK=re.compile(r'^\s+(v?p(add|sub|mull|mulh|madd|and|andn|or|xor|shuf|unpck|cmpe|cmpg|ack|avg|abs|max|min|sll|srl|sra|blend|movmsk|sad)|v?(add|sub|mul|max|min|and|or|xor|cmp)(ps|pd)\b)',re.I)
BND=re.compile(r'panic_bounds_check')
# SPILL STORES: a register written to a stack slot. Added because it is the
# instrument that told a real win from a mirage. Outlining the derivation's
# per-lane arms shrank its hot symbol 58% AND the binary -- genuine
# deduplication. Outlining the packer's gather arm shrank its hot symbol 16%
# too, but the binary grew 105 and SPILLS went 198 -> 211: the shrink was code
# relocated, not work removed. When a size drop and a total disagree, this
# column breaks the tie.
SPILL=re.compile(r'^\s+v?mov[a-z]*\s+%[a-z0-9]+,\s*-?[0-9]*\((%rsp|%rbp)\)')
KEYS=['row_hook','filter_frame','derive_bs_row','edc_flush','edc_commit','edc_send','edc_give',
      'deblock','derive_mb','gather_tile','pack_mb','pack_frame','bs1_tile','pk_differs',
      'precompute_bs','scan_uniform','mb_uniform','bs_motion','publish_filtered','edc_take','edc_restore']
d={}
for f in glob.glob('rusty_h264_decoder-*.s') + glob.glob('rusty_h264_common-*.s'):
    sym=None
    for line in io.open(f,encoding='utf-8',errors='replace'):
        m=LBL.match(line)
        if m:
            sym=m.group(1); sym=None if sym.startswith(('$','.')) else sym
            if sym and sym not in d: d[sym]=[0,0,0,0]
            continue
        if sym is None: continue
        if INSTR.match(line):
            d[sym][0]+=1
            if PACK.match(line): d[sym][1]+=1
            if BND.search(line): d[sym][2]+=1
            if SPILL.match(line): d[sym][3]+=1
rows=[]
for s,v in d.items():
    low=s.lower()
    if not any(k in low for k in KEYS): continue
    ids=re.findall(r'[0-9]+([a-zA-Z_][a-zA-Z0-9_]{2,})', s)
    nm='/'.join([x for x in ids if x not in ('rusty','h264','decoder','common','accel','core','alloc','std')][-2:]) or s[:44]
    if v[0]>=20: rows.append((nm,v[0],v[1],v[2],v[3]))
rows.sort(key=lambda r:-r[1])
print("%-44s %7s %7s %6s %7s" % ('symbol','instrs','packed','bnds','spills'))
print('-'*68)
for nm,i,p,b,sp in rows[:24]: print("%-44s %7d %7d %6d %7d" % (nm,i,p,b,sp))
print('-'*68)
print("%-44s %7d" % ('TOTAL (row-hook symbols)', sum(r[1] for r in rows)))
PY
