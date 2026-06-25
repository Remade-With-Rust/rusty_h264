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
kernel (SATD via `wide`) + autovec; the rest is scalar. **The 10–15× is the
PRODUCT of asm across every stage, not one kernel** — measured: switching our fast
ME from SATD to SAD (below) gave only +18%, because the ME is ~30% of inter time;
the other ~70% (6-tap interpolation, forward/inverse DCT, quant, CAVLC, deblock)
is all still scalar in our build and all hand-asm in x264.

**2a. The ME cost function — SAD vs SATD (measured, actionable).** x264's full-pel
search uses **SAD** (`h->pixf.sad`, `encoder/me.c:659/691`) computed by the
`psadbw` instruction — ONE instruction = SAD of 16 bytes (SSE2) / 32 (AVX2) / 64
(AVX512). We used **SATD** (Hadamard, ~5–10× more ops than SAD) for the ME cost.
Fix landed: the fast preset's ME + intra cost now use SAD written as
`Σ a.abs_diff(b)` over `u8` slices — **LLVM auto-vectorizes that to `psadbw`, so we
get x264's exact instruction with NO unsafe** (`mc_sad`/`sad_16x16` in encoder
mb16). Result: inter fast 77→91 Mpx/s (+18%), +17% bits (SAD picks slightly worse
MVs than SATD — fine for a fast preset), bit-exact. SATD stays in the quality
preset (better RD).

**3. Multithreading** (sliced/frame threads, `i_threads`). rusty is single-thread.

**ME details worth copying** (`encoder/me.c`): starts from MV **predictors**
(neighbor/temporal `mvc` set) rounded to fullpel — not (0,0); diamond radius-1
with **immediate early-out when the center wins** (`if(!(bcost&15)) break`);
fullpel search uses **SAD** (`fpelcmp`), SATD only at subpel; cost+direction
bit-packed for branchless min. rusty's coarse-to-fine 64→1 + subpel does many
more evals.

**MEASURED verdict (canonical test `bench/speedtest.sh`: differential 480f−120f,
best-of-3, all 24 cores, both MT, startup-cancelled): x264-ultrafast vs rusty
fast+parallel — INTER 1352 vs 91 Mpx/s (~15×), ALL-INTRA 1127 vs 118 (~10×).** We
do NOT exceed x264-ultrafast and realistically cannot: the gap IS the assembly,
and `#![forbid(unsafe_code)]` forbids the `std::arch` intrinsics it's built from.
Done this session (all safe): fast preset (SAD/psadbw ME, no-RDO mode decision) +
GOP-parallel multithreading took the encoder 2 → ~90–118 Mpx/s (~40–50×) — from
impractical to genuinely fast. Remaining lever: `wide` SIMD on MC/DCT/quant/
deblock (#3) narrows ~10–15× to maybe ~5–8×, but a safe wrapper won't match hand
AVX. The win is safety + BSD-2 + bit-exact + competitive compression at a usable
speed, NOT out-dragging x264's asm. Beware apples-to-oranges (MT-vs-1-thread,
in-memory-vs-disk) — ALWAYS use speedtest.sh.
