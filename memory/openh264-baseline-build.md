---
name: openh264-baseline-build
description: How to build Cisco openh264's h264enc here + the measured 3-way speed positioning vs x264 and rusty
metadata:
  type: reference
---

**Measured 3-way speed (single core, CQP 26, baseline/CAVLC, differential 480f−120f, CIF
352×288, best-of-3) — the canonical comparison:**

| workload | x264-ultrafast | openh264 (low-complexity) | rusty (fast) |
|---|---|---|---|
| ALL-INTRA | 183 Mpx/s | 85 Mpx/s | 22 Mpx/s |
| INTER | 536 Mpx/s | 109 Mpx/s | 52 Mpx/s |

So rusty is **8–10× behind x264-ultrafast but only ~2–4× behind openh264** (3.8× intra,
2.1× inter). openh264 — the codec we're reimplementing — is itself ~2–5× slower than
x264-ultrafast, so x264-ultrafast is the speed *outlier*; openh264 is the apt yardstick
(same baseline/CAVLC profile, it's the C reference). Most of the remaining 2–4× is its
SIMD asm, which our codec crates forbid (`forbid(unsafe)` → auto-vectorized scalar).

**Building openh264's h264enc.exe here (the hard-won recipe):**
- ffmpeg on this box has NO libopenh264; no prebuilt Cisco DLL. Must build from source at
  `C:/Users/talmo/coding/openh264`.
- Toolchain: clang 22 (`C:\Program Files\LLVM`), nasm at
  `C:/Users/talmo/nasm-portable/nasm-2.16.03`, meson+ninja via `pip install meson ninja`.
- **vcvars HANGS in this sandboxed shell — do NOT use it.** clang auto-detects the MSVC
  SDK without vcvars (proven: `clang++ x.cpp` links fine).
- **clang's path has a space** (`C:/Program Files/LLVM`) → ninja splits the arg → fails.
  Use the 8.3 short path: CC=`C:/PROGRA~1/LLVM/bin/clang.exe`,
  CXX=`C:/PROGRA~1/LLVM/bin/CLANG_~1.EXE` (get via PowerShell FSO `.ShortPath`).
- `meson setup builddir_rs --buildtype=release` then `ninja -C builddir_rs
  codec/console/enc/h264enc.exe` (~90 targets, builds the asm too). Output:
  `builddir_rs/codec/console/enc/h264enc.exe`.

**welsenc CLI gotchas (constant-QP, single-layer, all frames):**
`-rc -1 -lqp 0 26` (CQP) ; `-cabac 0 -dprofile 0 66` (baseline/CAVLC) ;
`-complexity 0 -threadIdc 1` (fastest, 1 core) ; `-numl 1 -dw 0 W -dh 0 H` (layer OUTPUT
size — `-sw/-sh` is only the SOURCE) ; **`-frin 30 -frout 0 30`** (MUST match, else the
default SVC temporal layers subsample to ~7.5fps and encode every 4th–8th frame) ;
`-iper 1` all-intra / `-iper 16` inter (must be power-of-2 or −1). h264enc reads YUV via
`-org`, writes `-bf`. Paths must be Windows-style (git-bash `/tmp` ≠ the exe's `/tmp`).
Reproduce via `bash bench/speedtest.sh` (now runs openh264 if OPENH264_ENC is set).
