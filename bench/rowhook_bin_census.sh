#!/usr/bin/env bash
# Deterministic instruction census of the boundary-strength derivation AS LINKED
# INTO THE FINAL BINARY.
#
# WHY a second script: bench/rowhook_instr_census.sh emits the RLIB, and an rlib
# carries every `pub fn` whether or not anything calls it. Dead-code elimination
# happens at LINK time, so an entry point that no shipping path reaches still
# shows a full instruction count there. This one disassembles the linked
# `decode_bench` example, so a symbol that appears here is a symbol that survived
# DCE -- i.e. one the decoder can actually reach.
set -uo pipefail
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR=/f/coding/rs_h264/target-db cargo build --release -p rusty_h264-decoder \
  --example decode_bench --features asm,std >/dev/null 2>&1 || { echo "build FAILED"; exit 1; }
BIN=$(ls -t /f/coding/rs_h264/target-db/release/examples/decode_bench*.exe 2>/dev/null | head -1)
[ -n "$BIN" ] || BIN=$(ls -t /f/coding/rs_h264/target-db/release/examples/decode_bench 2>/dev/null | head -1)
[ -n "$BIN" ] || { echo "no decode_bench binary"; exit 1; }
objdump -d --demangle=rust "$BIN" 2>/dev/null > /f/coding/rs_h264/target-db/decode_bench.dis || { echo "objdump FAILED"; exit 1; }
python3 - "$@" <<'PY'
import re,io
HDR=re.compile(r'^[0-9a-f]+ <(.+)>:\s*$')
INSTR=re.compile(r'^\s+[0-9a-f]+:\s')
KEYS=['derive_mb','gather_tile','pack_mb','bs1_tile','pk_differs','intra_mb_bs',
      'bs_motion','mb_uniform','scan_uniform','derive_bs_row','filter_frame','precompute_bs']
d={}; sym=None
for line in io.open('/f/coding/rs_h264/target-db/decode_bench.dis',encoding='utf-8',errors='replace'):
    m=HDR.match(line)
    if m: sym=m.group(1); d.setdefault(sym,0); continue
    if sym and INSTR.match(line): d[sym]+=1
rows=[(s,n) for s,n in d.items() if any(k in s for k in KEYS) and n>=8]
rows.sort(key=lambda r:-r[1])
print("%-58s %7s" % ('symbol IN THE LINKED BINARY','instrs'))
print('-'*68)
for s,n in rows: print("%-58s %7d" % (s[-58:],n))
print('-'*68)
print("%-58s %7d" % ('TOTAL (derivation symbols in binary)', sum(n for _,n in rows)))
PY
