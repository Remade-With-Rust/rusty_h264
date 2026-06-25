---
name: x264-speed-architecture
description: How x264 achieves its speed (esp. -preset ultrafast) — read from source, mapped to rusty_h264's gaps
metadata:
  type: reference
---

Read from x264 source (cloned `code.videolan.org/videolan/x264`) to understand
why x264-ultrafast is ~19–64× faster than rusty_h264 single-thread (see
[[rusty-h264-pure-rust-constraint]] competitive-positioning note). Three stacked
layers, in order of impact:

**1. It does FAR less algorithmic work (the `-preset` system).** ultrafast
(`common/base.c` ~L496) sets: `i_subpel_refine=0` (subme), `me=DIA`,
`analyse.inter=0` (16×16 only), no trellis/AQ/mb-tree/lookahead, 1 ref, no
deblock, CAVLC, no B-frames. The load-bearing line is `encoder/analyse.c:301`:
`a->i_mbrd = (subme>=6) + (subme>=8) + (subme>=10)`. **At subme=0, i_mbrd=0 → RDO
never runs.** Mode decision is by cheap SATD, NOT trial-encoding. RDO only turns
on at subme≥6 (medium+). rusty_h264 does **full RDO always** (~5 trial-encodes/MB
— the single biggest cost). x264 has ONE encoder with knobs; rusty has ONE
operating point = max effort (≈ x264 veryslow). We compared our veryslow-equiv to
their ultrafast → not "matched speed failing," apples-to-oranges on EFFORT.

**2. Hand-written SIMD assembly: 29,527 lines of x86 asm** (~43% of the codebase;
`common/x86/*.asm` — sad/pixel/dct/mc/quant/deblock/cabac all have asm), typically
4–16× over scalar C. The ME even batches 4 diamond candidates into ONE asm SAD
call (`fpelcmp_x4`, `encoder/me.c:97`, macro `COST_MV_X4_DIR`). rusty has ONE SIMD
kernel (SATD via `wide`) + autovec; the rest is scalar.

**3. Multithreading** (sliced/frame threads, `i_threads`). rusty is single-thread.

**ME details worth copying** (`encoder/me.c`): starts from MV **predictors**
(neighbor/temporal `mvc` set) rounded to fullpel — not (0,0); diamond radius-1
with **immediate early-out when the center wins** (`if(!(bcost&15)) break`);
fullpel search uses **SAD** (`fpelcmp`), SATD only at subpel; cost+direction
bit-packed for branchless min. rusty's coarse-to-fine 64→1 + subpel does many
more evals.

**Path for rusty to "out-compete x264 at top speed" (all fully safe, no unsafe):**
(a) **Add a fast preset** — SATD mode decision (skip RDO trial-encodes), diamond
ME, 16×16-only. Pure algorithm, biggest lever, ~5–20× for a fast mode. (b)
**Multithreading** (GOP/frame-parallel) — N× on cores. (c) **More `wide` SIMD** —
MC/DCT/deblock + batch ME candidates like fpelcmp_x4. The residual asm-vs-`wide`
+ decades-of-tuning gap is what we likely can't fully close; target is
"practical/competitive," not beating ultrafast's raw asm+threaded throughput.
