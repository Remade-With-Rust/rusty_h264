---
name: transform-batching-regresses
description: SIMD DCT batching regresses in portable Rust; only the quant flatten helped; transform isn't the bottleneck
metadata:
  type: project
---

Tried converting the transform+quant primitives to mirror openh264 (like CAVLC). Measured
on the bench's transform-favorable gradient clip (352×288, gop12, qp26, median-of-9 encode-ms):
pre-transform **133.7ms** → +quant/dequant flatten **132.6ms** → +forward/inverse DCT batching
**136.5ms**.

**Only the flatten won** (per-position MF/LevelScale tables, replacing the per-coefficient
`pos_group(i,j)` match — kept, commit 9edca4c). **The DCT batching is a ~3% REGRESSION** and was
reverted from the hot path (commit 669bc03).

**Why:** rustc already auto-vectorizes the scalar `forward_core`/`inverse_core`, so the explicit
per-block gather/scatter into `i32x4`/`i32x8` lanes (`forward_dct_blocks`/`inverse_dct_blocks`) is
pure overhead on the portable SSE2 build. `target-cpu=native` (AVX2) barely closed it. openh264's
`WelsDctMb`/`WelsIDctFourT4Rec` batching only pays off with its **hand-written AVX2** kernels —
mirroring the *structure* in portable Rust does not.

**Why it matters:** this is the [[x264-speed-architecture]] / CAVLC lesson again — explicit SIMD
micro-batching measures ≈0 or negative; the wins were allocations + redundant passes + flattened
lookups, NOT hand-SIMD. **Transform+quant is NOT the current bottleneck** on compressible clips
(the scalar path is near-optimal); the earlier "39%" figure doesn't translate to a speedup lever.

**How to apply:** before more transform work, RE-PROFILE to find the real hot path (likely ME/MC,
~29%). Kept `inverse_dct_blocks` + `add_residual_4x4` + the `batched_inverse_dct_matches_scalar`
test as byte-identical library primitives, ready to wire behind a future *runtime AVX2 dispatch*
(like openh264's CPU dispatch) where the SIMD width actually pays off. Don't re-add batching to
the default scalar path.
