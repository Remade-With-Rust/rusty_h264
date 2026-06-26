---
name: encode-phase-breakdown
description: measured per-phase cost of the encode (fast vs quality); CAVLC is 4.6% (a red herring), reconstruction + RDO trials are the real costs
metadata:
  type: project
---

**Measured the encode phase breakdown (352×288, qp26, gop12) by stubbing + Instant timers.**
Settles where the time actually goes — and kills the long-standing "the gap is CAVLC" hunch.

**CAVLC bit-writing = 4.6% of the fast encode** (stubbed `encode_residual_block`'s writes,
kept the param-calc + nnz feedback). Not the bottleneck. The CAVLC primitives are already
openh264-aligned (single-pass `CavlcParamCal`, scan8 nnz cache, u32-flush bit-writer, packed
writes, inverted-CBP lookup) — and we're `forbid(unsafe)`, so the bit-writer is near the safe
ceiling. **Optimizing CAVLC throughput (tables/scan/bit-writer) caps at ~5% — not worth it.**

**Phase breakdown (PROFILE=1 env probe, since reverted):**
- **FAST (the speedtest headline):** encode(recon+cavlc) **58%**, deblock 5%, modedec+ME+overhead **37%**.
  → reconstruction ≈ 53% (58% − 4.6% cavlc). **The headline lever is reconstruction + ME, NOT cavlc.**
- **QUALITY:** 9× slower than fast; **93% is the RDO trials**, final encode only 6%, deblock 1%.
  → A trial computes SSD, which needs MC+DCT+quant+**IDCT+reconstruct** — so the trial cost is
  *reconstruction*, not cavlc. The reconstruction primitives are SHARED with the fast encode.

**Reconstruction primitive state (the 53% fast / 93% quality lever):** inter-luma DCT+quant+IDCT
are asm ✅; **dequant is scalar (before the asm IDCT), chroma transform is fully scalar (8
blocks/MB), the MC→pred_y copy is redundant for 16×16.** These are spread thin — no single big
win — and per the [[asm-campaign-verdict]] SATD lesson, asm-wiring small per-4×4 kernels REGRESSES
(FFI overhead). So recon wins must be **algorithmic** (batch the chroma transform like the luma
scalar `forward_dct_blocks`, fuse, drop the MC copy), each ~1-3%.

**Quality RDO:** `trial_inter`/`trial_intra` do a full re-encode per candidate (`save_mb` →
encode → `mb_ssd` → `load_mb`); winner encoded twice. `trial_intra` (always run, full i16+i4×4
RDO) is the heaviest. **Done: SATD-prune the split trials** (SSD-trial only the SATD-cheaper of
16×8/8×16) → ~3% quality, RD-neutral (committed). Bigger cuts (cheap-estimate intra, eliminate
winner double-encode) are RD-affecting or a real restructure.

**Bottom line: after the asm campaign the encode is well-optimized; remaining wins are modest
and spread (recon nibbles + quality-RDO trims), not one big lever. CAVLC is a dead end.**
