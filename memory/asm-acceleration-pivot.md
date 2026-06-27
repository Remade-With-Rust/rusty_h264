---
name: asm-acceleration-pivot
description: Decision + proven path to embed openh264's BSD-2 x86 asm for speed (full pivot, forbid lifted in accel crate)
metadata:
  type: project
---

**Architecture decision (2026-06-25, user chose "Full pivot" via AskUserQuestion):**
embed openh264's hand-written x86 assembly for the hot kernels; Rust orchestrates
everything else. This REVERSES the earlier `#![forbid(unsafe_code)]` / pure-Rust
constraint — accepted deliberately to close the ~14×/core gap (proven five ways to
be the assembly: per-thread decomp 13.9×, chunking +11%, AVX2 codegen +0%, i32x8
+0%, structural micro-opts <noise). openh264 is **BSD-2** so vendoring its asm is
legal (x264's GPL would NOT be); openh264 itself ships ~27.5k lines of x86 asm —
the same strategy. Clone at `C:/Users/talmo/coding/openh264` (129MB, shallow).

**Toolchain PROVEN end-to-end (commit 5024b0b), crate `rusty_h264-accel` (NOT
forbid-unsafe):** nasm assembles openh264 `.asm` for win64 (`-f win64 -DWIN64
-DHAVE_AVX2 -I codec/common/x86/`), `cc` links the `.obj`, called via `extern "C"`.
First kernel `WelsSampleSatd4x4_sse2` is **bit-exact vs openh264's
`WelsSampleSatd4x4_c`** (256 cases). nasm at `C:/Users/talmo/nasm-portable/
nasm-2.16.03/nasm.exe` (winget install didn't register; portable copy works);
overridable via env `NASM` + `OPENH264_DIR`.

**Kernel inventory** (openh264 entry points): `satd_sad.asm` (WelsSampleSatd*/Sad*/
SadFour* — ME), `quant.asm` (WelsQuant4x4/QuantFour4x4/Dequant* — encode+skip-test),
`dct.asm` enc (WelsHadamardT4Dc; main fwd DCT named elsewhere — FIND IT), common
`dct.asm`/`mc_luma.asm`/`mc_chroma.asm`/`deblock.asm`/`mb_copy.asm`. All use WIN64
convention (arg1=rcx,arg2=rdx,arg3=r8,arg4=r9) → maps to Rust `extern "C"` directly.

**WIRED + MEASURED (2026-06-27, asm now DEFAULT for cli+facade; scalar via
`--no-default-features`).** Build needs nasm + OPENH264_DIR (build.rs has this
machine's defaults). All kernels bit-exact (per-kernel `*_matches_scalar` tests +
35/35 corpus MATCH unchanged):
- **MC luma half-pel (width 8/16)** — pre-existing, activated by turning asm on:
  decoder 1080p **28.4 → 41.2 Mpx/s (+45%)**.
- **Chroma MC (McChromaWidthEq8_sse2 → accel::mc_chroma_w8)** — NEW; wired the
  8-wide path in `inter::mc_chroma` over the same clamped tile (ABCD =
  [(8-fx)(8-fy),fx(8-fy),(8-fx)fy,fxfy] u8; width 2/4 stay scalar). **41 → 45.8**.
- **Inverse DCT (idct_four_t4_rec)** wired into inter luma recon (per 8×8 region,
  i32→i16 deq) — bit-exact but **~0 speed** (LLVM auto-vectorizes the scalar
  inverse; same as the encoder forward-DCT +3%). Kept behind cfg. Lesson
  reconfirmed: transform asm ≈0, MC asm is the win.
- **Encoder INTER 49 → 64 Mpx/s (+31%), gap vs openh264 2.1× → 1.6×** (shared MC).
  ALL-INTRA unchanged (~21, 3.7×) — no MC in intra; intra-pred/DCT asm ≈0.
- **Net: decoder 1080p gap vs h264dec 3.7× → ~2.35×; MC kernels (luma+chroma) are
  the lever, transforms are not.**

**WIDTH-4 LUMA = DEAD END (investigated 2026-06-27, don't retry):** openh264 has
NO clean SSE2 width-4 luma — `McLuma_sse2` dispatches every quarter-pel position
for a 4-wide block to **MMX** kernels (`McHorVer20WidthEq4_mmx`,
`PixelAvgWidthEq4_mmx`, mc.cpp:458/492/519…). MMX shares the x87 reg file (needs
`emms`, FP-state risk across FFI) and is ~scalar-speed on modern CPUs → not worth
wiring. The `Width5_sse2` kernels are only the centre's wider intermediate, not a
width-4 path. Tried the structural alternative instead — `decode_b_direct_temporal`
now MCs the whole 8×8 in one call under `direct_8x8_inference` (commit 68f1fdc,
bit-exact, 35/35) so those blocks hit the width-8 asm — but **~0 on the benchmark**:
the 0.281s sub-width MC is mostly genuinely-sub-8×8 partitions (P_8x8/B_8x8
4×8/8×4/4×4) that can't be merged. So sub-width MC is largely irreducible. Remaining
non-MC levers: chroma width 2/4 (no SSE2 kernel — w2 has none, w4 is MMX), CAVLC
(~14%, sequential — scalar table-driven VLC rewrite, NOT asm).

**INTEGRATION CHALLENGE (the real remaining work):** openh264 kernels expect
openh264's data layout (FENC/FDEC strides, their coefficient/zigzag order). Wiring
into our pipeline needs layout glue + per-kernel bit-exact verification against our
scalar path (else reconstruction drifts). Fast-preset hot targets by profile:
encode 54% (MC+DCT+quant+CAVLC+reconstruct), skip-mc 12%, skip-test 13%, me 10%,
deblock 11%. Highest asm value where `wide` provably fails: DCT+quant (gather/
scatter wall). Verify EACH kernel bit-exact + ffmpeg-decode + no GOP drift.
Productionise: vendor the `.asm` into the crate so no external clone is needed.
