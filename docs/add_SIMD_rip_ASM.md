# Rip the ASM, deploy a portable SIMD/NEON kernel set

**Status:** plan, 2026-08-07. Nothing here is committed work yet.
**Goal:** delete the 18,984 lines of vendored openh264 NASM, replace the kernels that
earn their place with portable Rust SIMD (x86-64 intrinsics + aarch64 NEON), and get
aarch64 off the scalar path it sits on today.

---

## 0. Read this before picking a kernel

The Great Gate campaign's central lesson was paid for twice: **a local speedup that
does not reach the top-level clock is not a win, and "we made X cheaper" nets to zero
if X is simultaneously switched on.** This plan is therefore ordered by the *measured*
decoder anatomy, not by which kernels look most vectorizable.

From `bash bench/decode_benchmark.sh` (2026-08-07, 18 streams, 6 content classes ×
cavlc/main/high, all byte-identical vs ffmpeg, call counts exact 538/538):

| bucket | share of decode | SIMD-addressable? |
|---|---|---|
| entropy (CAVLC/CABAC) | **31–40%** | **No.** Serial bit-at-a-time arithmetic decode. |
| per-MB loop glue (`row_hook`, header parse) | **~26%** | **No.** Structural, not arithmetic. |
| inter-mc | 10–17% | **Yes** — already NASM. |
| deblock (filter kernels) | 5–10% | **Yes** — already NASM. |
| syntax-parse | 5–8% | No. |
| dpb-clone | 2.9–3.7% | Partly (it is a memcpy). |
| dec-setup | 2.2–4.8% | No — allocation. |
| reconstruct + dequant | 4–7% | **Yes** — partly NASM, partly our Rust AVX2. |
| intra-pred | 0.2–0.7% | Yes, but the share says don't bother. |

**The honest ceiling: SIMD-addressable work is roughly 20–30% of decode.** The two
largest buckets — entropy and per-MB glue, together ~60% — are not SIMD problems. A
perfect kernel set, infinitely fast, leaves the decoder ~70–80% of its current cost.

That is not an argument against this project. It is an argument for doing it for the
*right* reasons — portability, build simplicity, supply chain, and NEON — and for not
promising a speedup the anatomy says is not there.

---

## 1. What is actually in the tree

| | |
|---|---|
| vendored NASM | **18,984 LOC**, 21 `.asm` files under `crates/rusty_h264-accel/vendor/` |
| `extern "C"` symbols | **44** |
| safe wrappers exported by `accel` | **27** |
| already pure Rust | **4** — `bs_motion_masks`, `bs_motion_masks_two_list`, `dequant_4x4`, `mb_uniform` |
| our own Rust AVX2 files | `hpel.rs`, `satd_avg.rs`, `mectx.rs`, parts of `lib.rs` |
| architectures | **x86-64 only.** On aarch64 `build.rs` returns early, the crate compiles to an empty lib, and the codec runs **scalar**. |
| build dependency | **`nasm` on PATH**, or the whole accel crate silently degrades to scalar |

Largest `.asm` files by LOC — this is the demolition list:

```
4490  common/x86/mc_luma.asm            2734  common/x86/satd_sad.asm
1829  encoder/core/x86/sample_sc.asm    1456  decoder/core/x86/intra_pred.asm
1129  encoder/core/x86/intra_pred.asm   1036  common/x86/dct.asm
 848  common/x86/deblock.asm             743  common/x86/asm_inc.asm
 728  common/x86/expand_picture.asm      695  encoder/core/x86/coeff.asm
 615  common/x86/mb_copy.asm             507  encoder/core/x86/quant.asm
```

### Why rip it, stated plainly

1. **aarch64 gets nothing today.** Apple Silicon, Graviton, and every ARM phone run the
   scalar path. This is the single biggest argument, and it is a *capability* argument,
   not a speed one.
2. **`nasm` is a build-time dependency that fails open.** No nasm → no kernels → silent
   scalar fallback. A performance cliff that does not announce itself is exactly the
   class of defect this codebase keeps finding in its own harnesses.
3. **It is the only `unsafe` crate** and the only vendored third-party source.
4. **We already do this successfully.** `satd_avg.rs` and `hpel.rs` are our own Rust
   AVX2 with `#[target_feature]`, and the campaign's "compose, don't write kernels"
   result (8x4/8x8 SATD composition, byte-identical, 4.61× → 3.05×) shows the Rust
   intrinsic path reaches the same performance class.

---

## 0a. MEASURED: what the ASM is worth, and why the order is replace-then-rip

Before deleting anything, the obvious question got measured: what does the vendored ASM
actually buy? Paired within-rep ratios (scalar / asm), drift-cancelling, N=5:

| stream | scalar / asm |
|---|---|
| shields main | **2.31x** |
| in_to_tree main | 2.26x |
| stockholm high | 2.06x |
| shields cavlc | 1.94x |
| crowd_run 1080p main | 1.84x |
| bus cif main | 1.78x |
| mobile cif main | 1.77x |
| **median** | **1.94x** |

**34 of 35 individual reps landed above 1.0.** The magnitude is noisy on this box; the
direction is not in question.

So **ripping the ASM with no replacement makes decode ~2x slower.** The decoder is
already ~2x behind ffmpeg, so a rip-then-replace order would ship a ~4x gap and then
climb back. Every kernel must be **replaced before its ASM is deleted**, gated
byte-identical, with the anatomy harness green at each step.

The one exception is dead code, which is free — see Phase 0, already done.

---

## 1a. Who actually consumes the ASM (call-site census, verified)

This was measured, and it corrected an earlier draft of this plan.

**The decoder makes ZERO direct `accel::` calls.** Every kernel it uses arrives through
`rusty_h264-common`. The split:

| consumer | call sites | kernels |
|---|---|---|
| **encoder only** | 18 | `satd_*` / `sad_*` family, `dct_four_t4`, `quant_four_4x4`, **`idct_four_t4_rec`**, `i16x16_luma_pred`, `chroma8x8_pred` |
| **shared, via `common`** | 19 | all 8 `deblock_*`, `mc_hor20`, `mc_ver02`, `mc_centre`, `mc_chroma_w4/w8`, `pixel_avg`, `hpel_fused`, `dequant_4x4`, `bs_motion_masks*` |
| **decoder direct** | **0** | — reaches SIMD only via `common::deblock` (4 refs), `common::inter` (25 refs), `common::transform` |

Two consequences that change the work:

1. **The decoder's entire ASM exposure is `deblock` + `inter-mc`.** Nothing else in the
   18,984 LOC of NASM is on a decode path. That is the whole decoder-facing port.
2. **The decoder's IDCT and intra-pred are ALREADY scalar Rust** — `idct_four_t4_rec`
   and the pred kernels are encoder-only. So for the decoder these are not ASM *ports*
   at all; they are un-vectorised code. They are also 2–3% and 0.2–0.7% of decode
   respectively, so the anatomy says leave them alone. Do not mistake "there is no SIMD
   here" for "there is a win here."

So: **SIMD/NEON is emphatically not encoder-only — but the decoder's share of it is two
kernel families, not the whole vendored set.**

---

## 2. Ranked target list

Ranked by **measured decoder share × vectorizability**, not by kernel glamour.
Encoder shares are separate and noted where they dominate.

### Tier 1 — worth doing, measurable payoff

| target | ASM source | decoder share | notes |
|---|---|---|---|
| **inter-mc luma** (`McHorVer*`, `PixelAvg*`) | `mc_luma.asm` (4490 LOC) | 10–17% | The single biggest SIMD-addressable decoder bucket. `hpel.rs` already covers part of this in Rust — extend rather than start over. |
| **deblock filters** (`DeblockLuma*`, `DeblockChroma*`) | `deblock.asm` (848) | 5–10% | Self-contained, fixed 4/8-wide edges. The transposes (`DeblockLumaTransposeH2V`) are the fiddly part. |
| **inter-mc chroma** (`McChromaWidthEq4/8`) | `mc_chroma.asm` (313) | inside inter-mc | Small file, bilinear — easiest real kernel in the set. |
| ~~idct/dct 4x4~~ | `dct.asm` (1036) | — | **MOVED to Tier 2.** The call-site census shows `idct_four_t4_rec` is *encoder-only*; the decoder's inverse transform is already scalar Rust and is 2–3% of decode. Not a decoder port. |

### Tier 2 — encoder-dominated, do after Tier 1

| target | ASM source | why |
|---|---|---|
| **SATD/SAD** (`WelsSampleSatd*`, `WelsSampleSad*`) | `satd_sad.asm` (2734) | ME is ~81% of the encoder's speed gap (`me-speed-state`), so this matters — but it is an *encoder* lever and does not touch decode. `satd_avg.rs` already ports the fused shapes. |
| **quant** (`WelsQuantFour4x4`) | `quant.asm` (507) | encoder-only |
| **dct / idct 4x4** (`WelsDctFourT4`, `WelsIDctFourT4Rec`) | `dct.asm` (1036) | encoder-only by census (8 + 6 sites); the decoder does not touch it |
| **intra-pred** (`i16x16_luma_pred`, `chroma8x8_pred`) | `intra_pred.asm` | encoder-only by census; decoder intra is scalar and is 0.2–0.7% of decode |

### Tier 3 — delete, do not port

| target | ASM | why |
|---|---|---|
| intra-pred (both) | 2585 LOC | **0.2–0.7% of decode.** Porting 2585 lines of ASM for 0.5% is the exact mistake this plan exists to avoid. Scalar Rust is fine. |
| `sample_sc.asm` | 1829 | preprocessing/screen-content; not on our path |
| `expand_picture.asm` | 728 | we do not use openh264's padding scheme |
| `memzero`, `matrix_transpose`, `coeff`, `score`, `vaa`, `mb_copy` | ~2300 | either unused or trivially expressed in safe Rust |
| `cpuid.asm` | 263 | `std::arch::is_x86_feature_detected!` replaces it outright |

**Roughly 8,000 of the 18,984 LOC get deleted without a replacement being written.**

---

## 3. Method — per kernel, non-negotiable

The existing `satd_avg.rs`/`hpel.rs` work is the template.

1. **Scalar reference first, and keep it.** Every kernel keeps a scalar twin reachable
   at runtime. That twin is the differential-test oracle forever, not a temporary.
2. **Byte-identical gate.** The kernel is a bit-exact replacement or it does not land.
   For the decoder this is enforceable end-to-end: `bench/decode_benchmark.sh` already
   requires all 18 streams byte-identical vs ffmpeg and exits non-zero otherwise.
3. **Differential fuzz** scalar vs SIMD over random inputs including edge values
   (0, 255, saturating deltas) before any timing is taken.
4. **Runtime dispatch** via `is_x86_feature_detected!` / `is_aarch64_feature_detected!`,
   resolved **once** into a fn pointer — not per call. (The campaign already paid for
   this: an `OnceLock` guard called *per band* in a hot loop made a win into a loss.)
5. **Measure with the anatomy harness, not a microbenchmark.** A kernel win must show up
   in `decode_benchmark.sh`'s honest wall. The project's own record is that a faster
   kernel routinely does not move the top-level clock — padded-MC, buffer-hop
   elimination, and gather/scatter fusion all measured ~0.
6. **Revert what does not move.** Record it in the ledger so it is not re-attempted.

### The measurement trap specific to this work

This box drifts **~25% run to run** and cannot resolve small effects: the row-deblock
ablation above returned paired ratios spanning 0.175–2.420 and settled nothing. So:

- Use **paired within-rep ratios** (arm A / arm B inside the same rep), never medians
  compared across arms — drift leaks straight into the latter.
- Use a **null arm** (same binary, same args, different label) to publish the floor with
  every result. The encoder harness measured 0.2–1.0% this way; anything finer is noise.
- **Counts before clocks.** A kernel that is not called cannot be fast — verify the
  dispatch actually took the SIMD path before interpreting any timing.

---

## 4. NEON — a green field, not a port

There is no NEON today; aarch64 is scalar. Two options:

**(a) Two intrinsic implementations per kernel** (`x86_64` + `aarch64` modules behind one
safe wrapper). Explicit, maximum control, ~2× the code.

**(b) A thin portable vector layer** over the shapes this codec actually uses — 8×u8
load, 16×u8 load, widening add, horizontal sum, saturating pack, absolute difference.
The kernel set is small and the shapes repeat, so one layer covers most of Tier 1.
`std::simd` is still unstable; a hand-rolled trait over `__m128i`/`uint8x16_t` is the
pragmatic form.

**Recommendation: (b) for Tier 1, with (a) as the escape hatch** where a target has an
instruction with no counterpart (e.g. x86 `psadbw` vs NEON `vabdl`+`vaddlv`). Prove the
layer on `mc_chroma` — the smallest real kernel — before committing to it.

---

## 5. Sequencing

Each phase ends with the decoder benchmark green (18/18 byte-identical) and its result
recorded, win or loss.

- **Phase 0 — delete the dead. DONE 2026-08-07.** 10 `.asm` files with zero live extern
  symbols removed: `cpuid`, `expand_picture`, `vaa`, `decoder/intra_pred`, `coeff`,
  `encoder/dct`, `matrix_transpose`, `memzero`, `sample_sc`, `score` — **6,380 LOC**.
  `asm_inc.asm` is macros-only (`%include`d by all 20 others) and stays. Verified:
  clean link, decoder **18/18 byte-identical vs ffmpeg**, and encoder output bit-for-bit
  unchanged (1952709 / 1484719 bytes, matching the pre-deletion measurements exactly).
  Remaining: 11 files, 12,604 LOC.
- **Phase 1 — the portable layer, proven on `mc_chroma`.** Smallest real kernel; both
  ISAs; byte-identical; keeps the scalar twin.
- **Phase 2 — inter-mc luma.** The biggest addressable decoder bucket, and where
  `hpel.rs` already has a foothold.
- **Phase 3 — deblock.** Then re-measure the deblock stage against the anatomy baseline.
- **Phase 4 — drop `nasm` from the DECODER build entirely.** After Phases 2–3 the
  decoder's only ASM (deblock + inter-mc) is gone, so this is reachable without
  touching a single encoder kernel. Confirm aarch64 runs the same kernels, byte-identical.
- **Phase 5 — encoder side: SATD/SAD, dct/idct, quant, intra-pred.** Judge against the
  encoder harness (`bench/x264_speed.ps1` + `bench/x264_quality.ps1`), not this one. ME
  is ~81% of the encoder's speed gap, so SATD/SAD is the lever that matters here.

**Exit criterion:** `crates/rusty_h264-accel/vendor/` is gone, `build.rs` no longer
shells out to nasm, and `bench/decode_benchmark.sh` is byte-identical with decode speed
no worse than the 2026-08-07 baseline in `bench/baselines/` on x86-64 — with aarch64
running SIMD for the first time.

---

## 6. What this plan explicitly does not promise

- **It is not a decoder speedup plan.** The anatomy says SIMD-addressable work is
  20–30% of decode; entropy and per-MB glue own ~60% and are untouched by any kernel.
  If the goal is decode speed, the higher-value targets are `row_hook` running per-MB
  for per-row work, and the CABAC header-parse path — both structural.
- **It will not close the gap to ffmpeg on its own.** The standing decoder gap is ~2×,
  and ffmpeg's advantage is not concentrated in the kernels we would be porting.
- **Tier 3 deletions may reveal live call sites.** The wrapper→symbol mapping used here
  was proximity-based and is approximate; Phase 0 must verify each deletion against the
  linker and the byte-identity gate rather than against this document.
