---
name: rusty-h264-pure-rust-constraint
description: Hard constraints for the rusty_h264 project — pure-Rust rebuild, no C/C++ in the build ever
metadata:
  type: feedback
---

rusty_h264 is a ground-up **pure-Rust reimplementation** of Cisco openh264 (a
"Remade With Rust" / Mata Network project), NOT FFI bindings like
ralfbiedert/openh264-rs.

**Renamed** from `rs_h264` → `rusty_h264` (crates, identifiers, env var
`RUSTY_H264_BENCH_FFMPEG`, docs, memory). The only thing still named `rs_h264`
is the top-level checkout directory `coding/rs_h264` — Windows wouldn't rename it
while the IDE held it open; close the IDE and `mv rs_h264 rusty_h264` to finish.

**Why:** the whole value proposition is memory safety + permissive BSD-2 + no
copyleft + embeddable, which bindings-over-C cannot give.

**How to apply:**
- Never add a C/C++-compiling dependency (no `openh264` crate, no `cc`/FFI).
  Every codec crate is `#![forbid(unsafe_code)]`.
- The benchmark compares against Cisco only as an **external, separately-installed
  process** (ffmpeg+libopenh264) spawned at runtime — never linked or built.
- Build order is **encoder-first** (user's call); the decoder grows alongside as
  the in-tree conformance oracle.
- Now a real video codec, all **bit-exact vs ffmpeg**: full intra (I_16x16 4
  modes, I_4x4 9 modes, chroma 4 modes, CAVLC), in-loop deblocking (intra+inter
  boundary strengths), RD mode decision + early-termination, and **inter
  prediction** (P-slices, P_Skip, P_L0_16x16, P_16x8/P_8x16 sub-partitions with
  directional MV prediction, quarter-pel motion compensation with 6-tap/bilinear
  luma + eighth-pel chroma, motion estimation, multi-frame DPB). Compression:
  rate-distortion competitive with x264 on intra; ~4.8× smaller than all-intra on
  moving content, ~5.7× on split-motion content via P-frames. Phase 4 complete.
- **Rate control** done: average-bitrate controller (complexity model +
  leaky-bucket buffer) varies per-frame QP via slice_qp_delta, bit-exact vs
  ffmpeg across QP 0–51. CLI `--bitrate <bps> --fps <f>` (0 = constant QP);
  `--gop N` enables P-frames.
- Two pre-existing low-QP bugs were found+fixed while building rate control
  (latent because nothing coded below QP 14 before): (1) **inverse 4×4 transform
  was columns-first; spec/ffmpeg is rows-first** — the >>1 flooring makes the
  integer transform non-separable, so order matters by ±1 on asymmetric blocks
  (test `inverse_core_is_row_first`); (2) CAVLC level escape capped at
  level_prefix 15 — large low-QP levels need the extended escape (prefix ≥ 16).
  Debugging method that worked: minimize to a single MB, clean-room re-decode in
  Python to isolate CAVLC vs reconstruction.
- **Optimization roadmap** in `docs/optimization-roadmap.md`: 5 tiers to close the
  ~15–20% intra / ~1.4× inter equal-quality RD gap vs x264. **Tier 1 (motion
  estimation) COMPLETE**: (1) rate-aware ME `J=SATD+λ·mvbits` (inter gap 1.37→1.19×
  at QP36); (2) coarse-to-fine 4-point full-pel search (−20% on fast motion;
  8-point diagonals reverted — wreck MV coherence on ambiguous motion); (3)
  multiple reference frames (`--refs N`, ref_idx-aware MV pred §8.4.1.3.1,
  sliding-window DPB both sides, bit-exact 27/27, −27% on occlusion w/ 3 refs).
  Single-ref stays byte-identical. **Tier 2 (quantization) partly done**:
  all-intra dead-zone tuning (`quantize` divisor 3→2, gated to gop<=1) is a clean
  win — QP26 −2.4% size +1.4 dB PSNR, PSNR gap to x264 ~halved, inter
  byte-identical. Counter-intuitive: rounding *up* more helps intra (better blocks
  → better neighbor prediction → smaller residuals). **Trellis reverted** — inter
  has the same feedback problem via the *reference chain* (degrading a frame
  inflates later frames); needs mb-tree. Adaptive QP deferred (perceptual/SSIM,
  not measurable on PSNR bench). **Tier 3 started**: RD-optimized skip (P_Skip
  when J_skip<=J_inter, not only when residual is zero) via a trial-encode that
  snapshots/restores per-MB state and measures real CAVLC bits + SSD
  (`save_mb`/`load_mb`/`trial_inter` in encoder mb16). Modest (−3.7% at QP36).
  **Fixed a pre-existing multi-ref deblock bug**: bS ignored ref_idx, but spec
  §8.7.2.1 gives bS 1 when two inter blocks reference different pictures (only at
  refs>=2). BlockInfo now carries ref_idx. The original multi-ref tests missed it
  (need noisy chroma + differing refs across a low-residual edge). Lesson: test
  multi-ref with noisy/varied content, not just clean occlusion clips. Next: full
  inter RDO; Tier 4 look-ahead RC; Tier 5 SIMD/threading. **Invariant: re-verify
  bit-exact vs ffmpeg after every change** (test refs 1/2/3 + varied content).
- Remaining hard ceilings (Constrained Baseline): no B-frames, no CABAC. P_8x8
  deeper sub-partitions (8×4/4×8/4×4) still optional. See docs/ for everything.
