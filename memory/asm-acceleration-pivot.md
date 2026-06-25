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

**INTEGRATION CHALLENGE (the real remaining work):** openh264 kernels expect
openh264's data layout (FENC/FDEC strides, their coefficient/zigzag order). Wiring
into our pipeline needs layout glue + per-kernel bit-exact verification against our
scalar path (else reconstruction drifts). Fast-preset hot targets by profile:
encode 54% (MC+DCT+quant+CAVLC+reconstruct), skip-mc 12%, skip-test 13%, me 10%,
deblock 11%. Highest asm value where `wide` provably fails: DCT+quant (gather/
scatter wall). Verify EACH kernel bit-exact + ffmpeg-decode + no GOP drift.
Productionise: vendor the `.asm` into the crate so no external clone is needed.
