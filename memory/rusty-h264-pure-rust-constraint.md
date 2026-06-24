---
name: rusty-h264-pure-rust-constraint
description: Hard constraints for the rusty_h264 project — pure-Rust rebuild, no C/C++ in the build ever
metadata:
  type: feedback
---

rusty_h264 is a ground-up **pure-Rust reimplementation** of Cisco openh264 (a
"Remade With Rust" / Mata Network project), NOT FFI bindings like
ralfbiedert/openh264-rs.

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
- Remaining/optional: P_8x8 deeper sub-partitions (8×4/4×8/4×4), trellis quant
  (attempted, reverted — fights intra-prediction feedback). Ceiling is
  Constrained Baseline (no B-frames, no CABAC). See docs/ for benchmarks + plans.
