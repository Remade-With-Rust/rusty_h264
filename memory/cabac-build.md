---
name: cabac-build
description: CABAC decoder build state — engine+tables DONE; per-syntax parsing roadmap with openh264 references
metadata:
  type: project
---

**CABAC = the largest remaining decoder piece** (unlocks ~8 rejected streams:
`test_*cabac*`, `test_qcif_cabac`, the `*cabac*` VIDs, `Cisco_Men_whisper_CABAC`).
Target order: `test_cif_I_CABAC_slice` (I-only) → `test_cif_P_CABAC_slice` →
B/High. Validate via the oracle (`examples/oracle.rs`).

**DONE + committed (d0123c9):** the foundation — the hardest-to-validate part.
- `crates/rusty_h264-decoder/src/cabac.rs`: literal-spec engine (§9.3.3.2)
  `Cabac::new(data, start_byte, qp, init_idc, is_i)`, `decode_decision(ctx)`,
  `decode_bypass`, `decode_bypass_bits(n)`, `decode_terminate`, `byte_pos()`.
  Init = §9.3.1.1 (`Clip3(1,126,(m·QP>>4)+n)` → state/mps).
- `cabac_tables.rs`: `RANGE_LPS[64][4]`, `STATE_TRANS[64][2]` (=transIdxLPS,MPS),
  `CTX_INIT[460][4]` (m,n; model 0=I, 1..3=init_idc 0..2). Generated from
  openh264 `common_tables.cpp` (`CTX_NA`→0). Regen: `/tmp/gencabac.py` style.
- `BitReader::data()`, `::bit_pos()` added for the engine handoff.

**NEXT — per-syntax parsing (port from openh264 `parse_mb_syn_cabac.cpp`).**
Context bases (openh264 `decoder_context.h` `NEW_CTX_OFFSET_*`): MB_TYPE_I=3,
SKIP=11, SUBMB_TYPE=21, B mb_type base=27, B_SUBMB=36, MVD=40, REF_NO=54,
DELTA_QP=60, CIPR(chroma pred)=64, IPR(intra4x4 pred)=68, CBP=73, CBF=85,
MAP(sig_coeff)=105, LAST(last_sig)=166, ONE=227, ABS=232, TS_8x8_FLAG=399,
MAP_8x8=402, LAST_8x8=417, ONE_8x8=426, ABS_8x8=431.
- `ParseMBTypeISliceCabac` (line 243): ctxInc=leftIntra16/PCM + topIntra16/PCM;
  bin@(3+ctxInc): 0→I_4x4(0). 1→ terminate→I_PCM(25), else @+3=I16 pred hi
  (val=1+bit*12), cbp bins @+4,+5 (+4,+8), 2 pred bins @+6,+7. Maps to our
  mb_type 0/1-24/25.
- transform_size_8x8_flag (line 391): ctxInc = leftT8 + topT8, base 399.
- intra4x4/8x8 pred: prev_flag @ IPR+0; if !prev, rem = 3 bypass bits. chroma
  pred: ctxInc=leftChromaPredNon0+topChromaPredNon0 @ CIPR; then bins.
- cbp (ParseCbpInfoCabac): 4 luma bits + 2 chroma, ctx from neighbour cbp @ CBP.
- mb_qp_delta (ParseDeltaQpCabac): ctxInc based on prev delta!=0, base DELTA_QP;
  binarization = unary mapped to signed.
- **RESIDUAL (hardest, Parse*Residual*Cabac):** per block: coded_block_flag
  (ctxInc from left/top cbf, base CBF) → significance map: for each scan pos,
  significant_coeff_flag @ MAP+pos, if 1 then last_significant_coeff_flag @
  LAST+pos → coeff levels: coeff_abs_level_minus1 (ctx from numEq1/numGt1 @ ONE
  for first bin, ABS for the rest as unary+EGk bypass tail) + sign (bypass).
  ctxBlockCat (0..13) selects the MAP/LAST/CBF context sub-range. The 8x8 sig
  map uses a position→ctx remap table (spec Table 9-43).

**INTEGRATION:** `lib.rs` decode_slice rejects CABAC at the
`pps.entropy_coding_mode_flag` check (~line 235). Replace: after the slice header
+ `r.align_to_byte()`, build `Cabac::new(r.data(), r.bit_pos()/8, slice_qp,
cabac_init_idc, is_i)` and run a CABAC MB loop (mirror `decode_slice_data` but
read syntax via the engine; `end_of_slice_flag` = `decode_terminate`). The recon
(transforms/intra-pred/MC/deblock) is SHARED with CAVLC — only the entropy layer
differs. Need `cabac_init_idc` from the slice header (read after slice_qp_delta
for P/B when !I).
