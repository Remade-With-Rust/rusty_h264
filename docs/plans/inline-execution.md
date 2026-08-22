# inline-execution — converting inline scalar work to SIMD/NEON/AVX2 (rusty_h264)

**Opened 2026-08-22.** Sister document to `rusty_zstd`'s `docs/plans/inline-execution.md`,
run with the same method: census the whole codec mechanically, read the emitted
assembly rather than guessing, audit *reachability* as hard as we audit loops, and
record refutations instead of deleting them.

**50 opportunities, encoder and decoder listed separately** (§3, §4), plus the
cross-cutting findings (§5) and the standing refutations that constrain all of them
(§7).

---

## 0. The headline

**The kernels are in good shape. The code *around* them is not — and the encoder is
the weak half by a wide margin.**

Assembly census, release + `--features asm` (the shipped path), per crate. These are
each crate's **own emitted body**; the accel kernels live in the accel object and are
called, not inlined, so these numbers measure the *glue and inline work*, which is
exactly what this document is about.

| crate | ymm | xmm | ymm share | reading |
| --- | ---: | ---: | ---: | --- |
| `rusty_h264-common` | 955 | 1,158 | **45%** | healthy — the shared kernels landed |
| `rusty_h264-accel` | 1,877 | 3,624 | **34%** | the kernel crate, as expected |
| `rusty_h264-decoder` | 2,743 | 5,057 | **35%** | **the problem** |
| **`rusty_h264-encoder`** | **1,555** | **8,285** | **16%** | **the problem** |

For scale, `rusty_zstd` reads 491 ymm against 27,862 xmm (1.7%). **This codec is in a
completely different and much better place** — the SIMD campaign that ripped the
NASM and replaced it with portable Rust intrinsics did real work. Nobody should read
this document as saying otherwise.

But the encoder's own 10,020-line `mb16.rs` body is 16% wide, and it holds **146
fixed-extent pixel loops** — the single largest concentration of un-vectorised
per-pixel work in the tree. The worst single symbol in the codec is
`encoder::mb16::encode_slice_data_cabac_p` at **1,730 xmm : 57 ymm (3%)**.

**Loop census: 1,411 non-test loops**, of which **346 are fixed-extent pixel loops**
(`for _ in 0..4 / 0..8 / 0..16`) in the hot files:

| file | pixel loops | | file | pixel loops |
| --- | ---: | --- | --- | ---: |
| `encoder/mb16.rs` | **146** | | `common/predict.rs` | 41 |
| `decoder/mb16.rs` | 78 | | `common/deblock.rs` | 36 |
| `common/transform.rs` | 43 | | `common/inter.rs` | 2 |

**This is the "inline problem" stated precisely:** the extracted kernels are
vectorised; the per-macroblock work that calls them is not.

---

## 1. Method, and one correction I had to make mid-scan

Five lenses, the same set that found the `rusty_zstd` critical bug:

1. **Emitted-assembly census** — per crate and per symbol, `ymm`/`xmm` attributed to
   the containing function. Not a guess about what LLVM does; a reading of what it did.
2. **Exhaustive loop census** — all 1,411 non-test loops, attributed to their
   enclosing function, then filtered to fixed-extent pixel shapes.
3. **Reachability audit** — which exported kernels does the codec actually call?
4. **Portability audit** — which kernels exist on x86-64 only?
5. **Allocation/clone census** — 158 sites in the hot files.

**The correction, recorded because it is the trap this project's own notes warn
about.** My first census was taken on a plain `cargo rustc -p <crate> --release`.
That is the **scalar** path: `asm` is *not* in `default` for `rusty_h264-common`,
`-encoder` or `-decoder` — only the facade (`rusty_h264`) and the CLI enable it. So
the first numbers measured an arm the product does not ship. Re-taken with
`--features asm`; every figure in §0 is from the corrected build.

That mistake is itself finding **X2** below, because the same asymmetry means
`cargo test -p rusty_h264-encoder` exercises the scalar path by default.

---

## 2. Laws inherited from the previous campaigns

These were paid for with real measurements recorded in `memory/` and
`docs/fin/add_SIMD_rip_ASM.md`. **Every item in this document is subordinate to
them.** Violating one is not a bold move; it is a repeat.

**Law 1 — a faster kernel routinely does not move the clock.** The project's own
record: quant, i16 intra-pred, and MC half-pel were each made byte-identical and
each measured *within run-noise*, because post-deblock the encode is
control-flow/mode-decision bound. `decode_benchmark.sh` is the honest wall, not a
microbenchmark.

**Law 2 — transforms are not the bottleneck, and LLVM already vectorises them.**
Inverse-DCT asm measured ~0 because rustc auto-vectorises the scalar transform.
SIMD DCT *batching* measured ~3% **slower**. Do not re-open the transform as a
vectorisation target without new profile evidence.

**Law 3 — per-call overhead beats kernel speedup when the call is small and hot.**
SATD asm was wired, byte-identical, RD-neutral, and **reverted for a net loss**:
`satd_4x4` runs 144×/MB, so the per-call cost exceeded the 2× kernel win. Wire
coarse (whole-MB) kernels; leave tiny hot-loop kernels inline. *(The FFI is gone now
that the kernels are Rust, so the constant is smaller — but the shape of the trap is
unchanged, and a `#[target_feature]` boundary is still a call boundary.)*

**Law 4 — replace, then rip.** The vendored asm measured **1.94× median** on decode
(34/35 reps above 1.0). Nothing gets deleted before its replacement is bit-identical
and no slower.

**Law 5 — dispatch resolves once, never per call.** An `OnceLock` guard called *per
band* in a hot loop turned a win into a loss. Resolve to a fn pointer at frame or
slice scope.

**Law 6 — the gate is byte-identical against ffmpeg, all 18 streams.**
`bench/decode_benchmark.sh` exits non-zero otherwise. RD-affecting changes (SATD
scale, quant deadzone) need PSNR/BD-rate gating on **varied** content — the project
has twice shipped a regression validated on one clean dev clip.

---

## 3. DECODER — 25 opportunities

Decoder share figures are from `docs/fin/add_SIMD_rip_ASM.md`'s anatomy where it
gives them. Where it does not, the item says so rather than inventing a number.

### 3.1 Reconstruction — the per-MB pixel work (D1–D8)

The decoder's `mb16.rs` holds **78 fixed-extent pixel loops**. They cluster in the
reconstruction handlers, which are the per-MB glue *around* the MC and transform
kernels — not the kernels themselves.

| # | target | pixel loops | shape / named reason auto-vec may fail |
| --- | --- | ---: | --- |
| **D1** | `add_inter_residual` | 8 | residual add + clamp to `u8` — the classic `paddw`+`packuswb` pair; check whether the `i32` intermediate blocks it |
| **D2** | `recon_p_skip` | 4 | whole-MB copy from reference; should be pure wide loads/stores |
| **D3** | `recon_p_inter_nores` | 4 | no-residual inter copy — same shape as D2 |
| **D4** | `recon_b_skip_fp` | 4 | B-skip full-pel; same family |
| **D5** | `recon_p_skip_fullpel` | 3 | same family — **D2–D5 are four spellings of one kernel** |
| **D6** | `recon_i16_luma` | 3 | I16×16 recon |
| **D7** | `inter_finish` | 3 | per-MB finish/store |
| **D8** | `decode_slice_cabac_inner` | 12 | 547 xmm : 264 ymm — the CABAC slice driver's own inline pixel work |

**D2–D5 deserve one brick, not four.** Four handlers doing full-pel block copy with
slightly different bounds is the `codec-eliminate-redundancy` shape: unify onto one
copy primitive, then vectorise once. That also shrinks the I-cache footprint of the
per-MB dispatch, which Law 1 says is where the decoder actually spends.

### 3.2 Intra prediction (D9–D12)

`common/predict.rs`, 41 pixel loops. **Read the constraint before costing these:**
the campaign ledger puts decoder intra-pred at **0.2–0.7% of decode** and explicitly
Tier-3s it — *"porting 2585 lines of ASM for 0.5% is the exact mistake this plan
exists to avoid."*

| # | target | loops | verdict |
| --- | --- | ---: | --- |
| **D9** | `intra4x4_pred` (9 modes) | 11 | **do not build for decode speed.** Listed for completeness and because the encoder calls the same code (see E-side) |
| **D10** | `intra8x8_pred` (High profile) | 10 | same |
| **D11** | `luma16x16_pred` | 4 | same |
| **D12** | `chroma8x8_pred` | 4 | same — an accel kernel already exists (`chroma8x8_pred`, 4 call sites) |

**These four are in the catalogue as CLOSED-BY-ANATOMY.** If a future profile moves
intra above ~3% of decode, reopen; not before.

### 3.3 Inverse transform and residual (D13–D16)

| # | target | note |
| --- | --- | --- |
| **D13** | `reconstruct_4x4_into` (3 loops) | inverse core + residual add, fused |
| **D14** | `reconstruct_4x4_dc_into` (3) | DC-only path — the common case; worth a dedicated wide store |
| **D15** | `add_residual_4x4` | the add+clamp leaf |
| **D16** | `inverse_core_x4` / `inverse_core_x8` / `inverse_core_8x8` (3+3+4) | **Law 2 applies** — verify from the emitted asm that LLVM already vectorises these before touching them |

**D14 is the interesting one.** A DC-only residual is a single broadcast value added
to the whole block — a `set1` + add + clamp, with no transform at all. If the code
currently runs the general path for it, that is a redundancy win, not a SIMD one.
**Verify the DC-only frequency first** (it is content-dependent and high at low
bitrate).

### 3.4 Deblocking (D17–D21)

`common/deblock.rs`, 36 pixel loops, 3,439 lines — the largest single common file.
Ledger share: **5–10% of decode**, and all eight `deblock_*` kernels already have
accel implementations *with* aarch64 paths (22 aarch64 references in
`deblock_simd.rs`). So the kernels are done; these are the **glue**.

| # | target | loops | note |
| --- | --- | ---: | --- |
| **D17** | `gather_tile` | 4 | gathers the edge tile for the filter — a strided load, the classic transpose problem |
| **D18** | `filter_frame_rows` | 4 | the frame-level driver |
| **D19** | `pack_mb` | 3 | packs MB pixels for the filter — pairs with D17 as gather/scatter twins |
| **D20** | `precompute_bs_frame` | 2 | boundary-strength precompute; there is a plan for this already (`docs/fin/deblock-bs-precompute-plan.md`) — **read it before starting** |
| **D21** | `derive_mb_kind_into` / `derive_mb_general` | 3 | bS derivation — branchy, per-edge |

**D17+D19 are one brick.** A gather and its inverse scatter around a filter that is
already vectorised is the textbook case where the *transpose* costs more than the
filter saves. Measure the pair together or not at all.

### 3.5 Entropy and drivers (D22–D25)

| # | target | note |
| --- | --- | --- |
| **D22** | CAVLC (`common/cavlc.rs`, 16 loops, **0 pixel loops**) | already table-driven (a measured +15% decoder win). **Sequential by nature — not a SIMD target.** Listed so the next census does not re-flag it |
| **D23** | CABAC decode (`decoder/cabac.rs`, 640 lines) | serial arithmetic decode. Not vectorisable. The known lever is *bin throughput*, not lanes |
| **D24** | `FrameDecoder::with_pool` — **1,053 xmm : 15 ymm** | the worst ymm ratio on the decode side. Almost certainly buffer setup/zeroing, not pixel math. **Check for a `vec![0; …]` that is immediately overwritten** |
| **D25** | `Decoder::decode` — 975 xmm : 54 ymm | top-level driver; same suspicion as D24 |

**D24 is the highest-confidence decoder item in this section** and it is not a kernel
at all — a 1,053-op scalar body in a per-frame setup path is either a redundant clear
or a struct copy. `decoder-pivot` records that a single `.cloned()` on a frame-sized
struct once cost **85.8% of total decode time**. Look here first.

---

## 4. ENCODER — 25 opportunities

**The encoder is where the ratio says the work is** (16% ymm vs the decoder's 35%),
and `encoder/mb16.rs` holds **146 fixed-extent pixel loops** in 10,020 lines.

### 4.1 The mode-decision planners — the concentration (E1–E5)

| # | target | pixel loops | asm (xmm:ymm) |
| --- | --- | ---: | --- |
| **E1** | `plan_inter_mb` | **31** | 185 : **365** — already the *most* wide-vectorised encoder symbol |
| **E2** | `plan_mb` | **29** | — |
| **E3** | `encode_inter_mb_v2` | 11 | — |
| **E4** | `plan_inter8_luma` | 4 | — |
| **E5** | `plan_i8x8` | 4 | — |

**E1 and E2 together hold 60 of the encoder's 146 pixel loops.** Note E1 is *already*
365-ymm — so the planner is partly vectorised and the remaining 31 loops are the
residue. That makes E1 a **surgical** target (find the loops LLVM declined and why),
not a rewrite.

**E2 `plan_mb` has no ymm entry in the top-12 at all**, which given 29 pixel loops is
the strongest single signal in this document. Start here.

### 4.2 The skip/free tests (E6–E9)

Per `x264-speed-architecture`, skip-test is **13.2%** and skip-mc **11.8%** of the
fast-preset encode — together the largest non-`encode` bucket.

| # | target | loops |
| --- | --- | ---: |
| **E6** | `skip_luma_is_free` | 5 |
| **E7** | `skip_chroma_is_free` | 3 |
| **E8** | `commit_skip` | 3 |
| **E9** | `pred_ssd` | 4 |

**E6/E7 are early-out SSD/threshold tests** — the same shape as `count_eq_len` in
`rusty_zstd`: a loop that usually exits early. **Harvest the exit distribution before
vectorising**; if most calls die in the first rows, a wide load is pure overhead.
That is the single most transferable lesson from the sister codec.

### 4.3 RDO trial machinery (E10–E14)

`encode-phase-breakdown` measured the **quality preset at 93% RDO trials**, and a
trial's cost is *reconstruction*, not CAVLC.

| # | target | loops | note |
| --- | --- | ---: | --- |
| **E10** | `mb_ssd` | 4 | the trial's cost function — runs once per candidate |
| **E11** | `save_mb_into` / `load_mb` | 2 + 2 | per-trial state snapshot/restore — **memory traffic, not lanes** |
| **E12** | `gather_i4` | 3 | I4×4 neighbour gather |
| **E13** | `gather_i8_enc` | 3 | I8×8 neighbour gather |
| **E14** | `commit_pred_pixels` | 2 | winner commit |

**E11 is the sleeper.** ~5 trial-encodes/MB × (snapshot + restore) is pure copy
traffic sitting inside the 93%. It belongs to `codec-memory-copies`, and the fix is
structural (copy-on-write, or trial into a scratch and commit once) rather than wide
loads.

### 4.4 Slice drivers — the worst ratios in the codec (E15–E19)

| # | symbol | xmm : ymm | ymm share |
| --- | --- | --- | ---: |
| **E15** | `mb16::encode_slice_data_cabac_p` | **1,730 : 57** | **3%** |
| **E16** | `Encoder::encode_all_bframes` | 1,053 : 92 | 8% |
| **E17** | `mb16::encode_slice_data` | 970 : 47 | 5% |
| **E18** | `Encoder::encode_all` | 628 : 32 | 5% |
| **E19** | `mb16::encode_slice_data_cabac_b` | 324 : 68 | 17% |

**E15 is the single worst symbol in the codec.** 1,730 128-bit ops against 57 wide
ones, in the P-slice CABAC encode driver. Before assuming it is pixel work: a driver
that large is usually **struct copies and buffer moves**, which is a different fix
(and a cheaper one). **Read what the 1,730 ops actually are before proposing a
kernel.**

### 4.5 Setup, rate and search (E20–E25)

| # | target | evidence | note |
| --- | --- | --- | --- |
| **E20** | `FrameEncoder::new` | 358 xmm : 10 ymm | per-frame setup — suspect buffer zeroing, same shape as D24 |
| **E21** | `mbtree::gop_qp_offsets` | 395 xmm : 2 ymm | **0.5% ymm** — a GOP-scale array pass that is almost entirely scalar |
| **E22** | `plan_rate_bits` | 168 xmm : 3 ymm | rate estimation |
| **E23** | `core::slice::sort::unstable::quicksort` | **491 xmm**, 0 ymm | **something sorts inside the encode path.** Find the call site — a per-MB or per-frame sort is a design question before it is a speed one |
| **E24** | `satd_px` | 2 loops | SATD glue — **Law 3 applies**, this is the 144×/MB class |
| **E25** | `forward_core_x4` / `_x8` / `_8x8`, `trellis_quant` | 3+3+4+2 loops | **Law 2 applies** — verify auto-vectorisation from the asm before touching |

**E21 and E23 are the two surprises.** A 395-op scalar body in mb-tree and a
quicksort in the encode path were not on anyone's list, and neither is a kernel
problem.

---

## 5. Cross-cutting findings

**X1 — the NEON gap is large and structural.** `lib.rs` gates `mod x86_asm` on
`target_arch = "x86_64"`, and these modules have **zero aarch64 references**:

| module | lines | aarch64 refs |
| --- | ---: | ---: |
| `x86_asm.rs` | 802 | 0 |
| `satd_avg.rs` | 602 | 0 |
| `intra_pred.rs` | 225 | 0 |
| `hpel.rs` | 203 | 0 |
| `mectx.rs` | — | 0 |

**≈1,832 lines of kernels do not exist on ARM**, including the whole `sad_x4` /
`satd_x4` / `satd_avg` / `satd_x4p` family and `MeCtx` — and ME is ~81% of the
encoder's speed gap. The portable modules (`deblock_simd` 22, `luma_mc` 19,
`transform_quant` 12, `chroma_mc` 9, `satd_sad` 9) *do* have NEON. **This is the
campaign's stated direction** (`add_SIMD_rip_ASM.md` §4, "NEON — a green field, not
a port"); this document quantifies the remaining hole.

**X2 — `asm` is not a default feature on the codec crates.** Only the facade and CLI
turn it on. Consequences: (a) `cargo test -p rusty_h264-encoder` exercises the
**scalar** path, so the SIMD kernels are not covered by per-crate test runs; (b) any
benchmark or example built against a codec crate directly measures scalar unless it
opts in. **This caught me mid-scan** (§1) and it will catch the next person.
**Fix:** either make `asm` default on the codec crates with the facade opting out, or
add a CI job that runs the per-crate suites with `--features asm`.

**X3 — `mc_ver02_avg` is exported and never called.** Zero call sites in
`common`/`encoder`/`decoder`; the only mention is a *comment* in `inter.rs:609`.
Either wire it or delete it — an unwired kernel is indistinguishable from a bug, and
this is the exact defect class that cost the sister codec its checksum win.

**X4 — 158 allocation/clone sites in the hot files**: `encoder/mb16.rs` 60,
`decoder/mb16.rs` 46, `common/inter.rs` 26, `common/deblock.rs` 24. Given
`decoder-pivot`'s record — one `.cloned()` was **85.8% of decode time** — this vein
deserves a deliberate census, not incidental discovery. Tooling exists
(`RUSTY_PROFILE`, the anatomy harnesses).

**X5 — dispatch must resolve once (Law 5), and the codebase has a cached
`has_avx2()` `OnceLock` in `x86_asm.rs`.** Any new kernel must reuse it rather than
adding a second detector, and must not call it per band.

---

## 6. Execution order

Ordered by *evidence strength*, not kernel glamour. Items whose first step is
**measure** are marked — those come first because they are cheap and they decide
whether the brick exists at all.

| # | brick | side | first step |
| --- | --- | --- | --- |
| 1 | **E15** — read what 1,730 xmm ops in `encode_slice_data_cabac_p` actually are | enc | **measure/read asm** |
| 2 | **D24 + E20** — per-frame setup scalar bodies (1,053 / 358 xmm) | both | **read for redundant clears/copies** |
| 3 | **E23** — find the quicksort in the encode path | enc | **locate the call site** |
| 4 | **E2** — `plan_mb`, 29 pixel loops and no ymm in the top-12 | enc | **measure/read asm** |
| 5 | **X3** — wire or delete `mc_ver02_avg` | both | decide |
| 6 | **X2** — make the SIMD path testable by default | both | Cargo/CI change |
| 7 | **D2–D5** — unify the four full-pel recon copies, then vectorise once | dec | refactor |
| 8 | **E6/E7** — harvest skip-test early-exit distribution | enc | **harvest first** |
| 9 | **D14** — DC-only residual fast path | dec | **harvest DC frequency first** |
| 10 | **E11** — RDO snapshot/restore copy traffic | enc | structural |
| 11 | **D1** — `add_inter_residual` add+clamp | dec | kernel |
| 12 | **E21** — `mbtree::gop_qp_offsets` | enc | kernel |
| 13 | **D17+D19** — deblock gather/pack pair | dec | measure together |
| 14 | **X1** — NEON for `satd_avg`/`hpel`/`intra_pred`/`mectx` | both | green-field port |
| 15 | **X4** — allocation census in the hot files | both | **harvest first** |

**Deliberately NOT scheduled:** D9–D12 (intra-pred, closed by anatomy at 0.2–0.7%),
D22/D23 (entropy, sequential), D16/E25 (transforms — Law 2), E24 (SATD glue —
Law 3). These stay in the catalogue so the next census does not re-discover them.

**Note the shape of the top of this list.** Six of the first ten bricks start with
*measure* or *read*, and four are not SIMD at all. That is the same conclusion the
sister codec reached twice, and this codec's own memories reached three times
(quant, i16 intra, MC half-pel — all byte-identical, all within noise). **The wins
here have historically been algorithmic and structural; the changes that looked like
levers measured ~0.**

---

## 7. CLOSED — do not retry

Recorded from `memory/` and `docs/fin/`. Each cost real time.

1. **AVX2 MC ≈ 0.** Wired, bit-exact, kept as best-available — **~0 on 1080p**. Once
   block-MC is SSE2-wide, the bottleneck is outside the kernel.
2. **Inverse-DCT / forward-DCT vectorisation ≈ 0.** rustc already auto-vectorises the
   scalar transform. **SIMD DCT batching measured ~3% SLOWER.**
3. **SATD as a wired coarse kernel — net LOSS.** Byte-identical and RD-neutral, but
   `satd_4x4` runs 144×/MB and per-call overhead exceeded the 2× kernel win. Reverted.
4. **quant, i16 intra-pred, MC half-pel** — each wired and byte-identical, each
   **within run-noise**. Post-deblock the encode is control-flow bound.
5. **scan8 neighbour cache, recon restructure, full padded-frame MC** — all measured
   ~0 and not shipped (padded MC kept unwired at `a00db62`).
6. **CAVLC throughput optimisation caps at ~5%** — bit-writing is 4.6% of the fast
   encode. The "the gap is CAVLC" hunch is dead; the primitives are already
   openh264-aligned and near the `forbid(unsafe)` ceiling.
7. **Width-4 luma MC** — openh264 had no clean SSE2 path (MMX only); the structural
   merge to 8×8 measured ~0 because the residue is genuinely sub-8×8 partitions.
8. **i4×4 intra-pred asm** — never existed in openh264 (C-only). Nothing to port.

---

## 8. Gates

Non-negotiable, in order, per brick:

1. **Byte-identical vs ffmpeg, all 18 streams** — `bench/decode_benchmark.sh` exits
   non-zero otherwise. This is the decoder's whole correctness story.
2. **Differential fuzz** scalar vs SIMD over random input including edge values
   (0, 255, saturating deltas) *before* any timing.
3. **Scalar twin kept reachable at runtime, forever** — it is the differential oracle,
   not a temporary.
4. **The anatomy harness, not a microbenchmark** (Law 1).
5. **RD-affecting changes** (anything touching SATD scale, quant deadzone, mode cost)
   need PSNR/BD-rate on **varied** content across ≥2 QPs. The project has twice
   shipped a regression validated on a single clean dev clip — the multi-ref deblock
   bug and the intra-gate `+40%` size blowup.
6. **A brick that measures FLAT gets reverted**, byte-identical or not.

---

## 9. What this document does not claim

- **No item here has been built or measured.** It is static analysis plus four asm
  builds. Every figure is a count or a ratio, not a timing.
- **The ymm/xmm ratios are a proxy, not a verdict.** A high xmm count can be struct
  copies (cheap, correct) rather than un-vectorised math. That is precisely why the
  top of §6 says *read* before *build* — E15, D24 and E20 are all flagged as
  "find out what these ops are," not "write a kernel."
- **The 50 are not 50 wins.** By the sister codec's record and this project's own,
  the realistic yield is a handful of real movers, a tail of ~0s, and several items
  that close as CLOSED-BY-ANATOMY once someone puts a number on them. The catalogue's
  value is that the ~0s get *recorded* instead of rediscovered.

---

## 10. The hiding functions — a reachability dive

**Added after the first pass.** §5 (X3) found one unwired kernel by accident. This
section ran that lens deliberately: **what is built, correct, compiled — and never
called?**

Method: extract all **880 non-test `fn` definitions** across `crates/` and `bench/`,
count every `name(` / `name<` reference in one pass, subtract the definitions, and
keep the functions whose call count is zero. Then hand-classify — trait impls
(`deref`, `fmt`), entry points (`main`), and instrumentation APIs called only from
`examples/` are **not** findings and are excluded below.

**46 def-only functions survived the automated pass; 10 are real.** They are listed
in descending order of consequence.

### H1 — the `accel` cfg still requires x86-64, so the NEON kernels are unreachable. **CRITICAL**

`crates/rusty_h264-common/build.rs` and `crates/rusty_h264-decoder/build.rs` (and the
encoder's) each define the internal `accel` cfg as:

```rust
let asm    = std::env::var_os("CARGO_FEATURE_ASM").is_some();
let x86_64 = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
if asm && x86_64 { println!("cargo:rustc-cfg=accel"); }
```

with the stated justification:

> *"The vendored openh264 SIMD kernels in `rusty_h264-accel` are x86-64 only, so the
> `accel` code paths must not be compiled on other architectures…"*

**That justification is stale.** The vendored NASM was fully ripped on 2026-08-12 and
`rusty_h264-accel/src/lib.rs` now says the opposite:

> *"The whole crate used to be `#![cfg(target_arch = "x86_64")]` — compiled to nothing
> on ARM, which is why aarch64 ran fully scalar. **That gate now sits on the `x86_asm`
> module alone, so portable kernels reach ARM as they land.**"*

They do not reach ARM. The crate-level gate moved; **the consumer-side gate did not.**
On aarch64 `accel` is never set, so all **67 `#[cfg(accel)]` sites** (common 27,
encoder 40, decoder 0 — it reaches SIMD via common) compile out, and these kernels are
built and never called:

| accel module | aarch64 refs | reachable on ARM? |
| --- | ---: | --- |
| `deblock_simd.rs` | 22 | **no** |
| `luma_mc.rs` | 19 | **no** |
| `transform_quant.rs` | 12 | **no** |
| `chroma_mc.rs` | 9 | **no** |
| `satd_sad.rs` | 9 | **no** |

**Fix:** split the cfg. `accel` should mean "the `asm` feature is on **and** this
target has a kernel" — which is now `x86_64 OR aarch64` for the portable modules, and
`x86_64` only for whatever still lives in `x86_asm`. The cleanest shape is two cfgs
(`accel_portable`, `accel_x86`) rather than one overloaded flag.

**Gate:** ARM is currently *fully scalar and byte-identical by construction*; turning
the kernels on makes ARM a real arm of the differential test for the first time. Run
the 18-stream byte-identical gate on aarch64 before and after — that is the whole
point of having kept the scalar twins.

**Why this outranks everything else in this document:** it is not a kernel to write.
The kernels exist, have NEON paths, are bit-exact, and are already tested. This is a
three-line build-script change standing between them and every ARM target.

### H2 — `reconstruct_4x4_dc` is built, documented, and never called

`common/predict.rs:585`:

```rust
/// Reconstructs a block whose residual is FLAT (every position carries the same
/// value `r`) — the tail of the DC-only fast path.
pub fn reconstruct_4x4_dc(r: i32, pred: &[i32; 16]) -> [u8; 16]
```

**Zero call sites.** It even honours the same ablation knob and profiling stage as
`reconstruct_4x4` *"so measurement arms stay comparable whichever path a block
takes"* — a comparison nothing can make, because no block takes it.

**This supersedes D14.** That item proposed *building* a DC-only residual fast path.
It exists. The work is to **wire it and harvest the DC-only frequency**, which is
content-dependent and high at low bitrate.

### H3 — `avg_rows<const BW>` — an MC averaging kernel with no callers

`common/inter.rs:1165`:

```rust
fn avg_rows<const BW: usize>(pa: &[u8], oa: usize, pb: &[u8], ob: usize,
                             cw: usize, bh: usize, out: &mut [u8])
```

A const-generic two-source row averager — the shape bi-prediction and quarter-pel
averaging both need. Never called. Either it was superseded by `pixel_avg` (the accel
kernel, 5 call sites) and should be deleted, or it is the width-parameterised path
that was meant to replace a scalar loop and never got wired. **Decide and act;
do not leave it.**

### H4 — `mc_ver02_avg` — accel kernel, never called *(confirmed repo-wide)*

Already listed as X3. The repo-wide pass confirms it: one definition, zero calls, and
the only mention anywhere is a **comment** in `inter.rs:609`.

### H5 — `recon_b_skip_zero_uni` — a B-skip reconstruction path with no callers

`decoder/mb16.rs:6910`. A specialised B-skip recon (zero-MV, uni-directional) sitting
beside `recon_b_skip_fp` (D4) which *is* called. This is a fast path for the most
common B-skip case, dark.

**Note the pattern H2 + H5 make together:** two specialised fast paths, both written,
both dark. Whatever process wired their general siblings did not come back for them.

### H6 / H7 — `build_mv_cost()` and `build_true_biased()` — uncalled table builders

`encoder/mb16.rs:508` and `:539`, both returning `Vec<u16>`. Cost/bias tables for
motion estimation that nothing constructs. Given `mvd_cost_tab.rs` (264 lines) exists
as a separate module, these are most likely **superseded** — but they are compiled
into every build. Confirm and delete, or wire.

### H8 — five encoder tuning knobs are unreachable from the CLI

All `pub fn`, all with zero in-tree callers including `rusty_h264-cli`:

| setter | location |
| --- | --- |
| `set_subme(level: u32)` | `encoder/lib.rs:329` |
| `set_turbo(on: bool)` | `encoder/lib.rs:351` |
| `set_defer_subpel(on: bool)` | `encoder/lib.rs:368` |
| `set_subpel_dispatch(on: bool)` | `encoder/lib.rs:376` |
| `set_precomputed_bs(on: bool)` | `common/deblock.rs:1773` |

They are `pub`, so an external API consumer *can* reach them — but **nothing in this
repository does**, which means: they are not exercised by the test suite, not
settable from the shipped CLI, and not covered by any benchmark arm. `set_subme` and
`set_turbo` in particular sound like the speed/quality preset controls the project
says it lacks (*"there is NO speed/quality preset in rusty — it is always at full-RDO
effort"*).

**This is a product question before it is a code question.** Either they are the
preset mechanism and the CLI should expose them, or they are dead experiment
scaffolding and should go.

### H9 — `encode_bypass_bits` — dead CABAC bypass path *(roadmap marker, not a bug)*

`encoder/cabac.rs:169`, `#[allow(dead_code)]`, with the comment *"Reserved for the P/B
inter syntax (mvd / ref_idx bypass suffixes) — see CABAC-4."* Correctly marked and
honestly documented. **Listed so the next census does not re-flag it** — it is a
placeholder for unfinished work, and the `#[allow(dead_code)]` is doing its job.

### H10 — the accel scalar oracles are test-only, which deviates from the campaign's own method

`dct_four_t4_scalar`, `idct_four_t4_rec_scalar`, `quant_four_4x4_scalar` are `pub` in
the accel crate with **no callers outside it** — they are reached only from `#[cfg(test)]`.

`docs/fin/add_SIMD_rip_ASM.md` §3, method step 1, is explicit:

> *"**Scalar reference first, and keep it.** Every kernel keeps a scalar twin
> **reachable at runtime**. That twin is the differential-test oracle forever, not a
> temporary."*

A test-only twin is not runtime-reachable. There is no arm that selects it, so there
is no way to A/B a kernel against its oracle in a running encode or to bisect a
suspected kernel bug in the field without a rebuild. **Add a runtime arm** (the
sister codec's `set_*_arm` pattern; this codebase already has `RFF_MECTX`,
`RFF_SATD_AVG`, `RFF_HPEL_AVX2` doing exactly this for other kernels) — or amend the
method to say test-only is sufficient. Either is defensible; the current state
silently does not match the written rule.

### 10.1 Checked and NOT findings — recorded so they are not re-chased

The automated pass surfaced these; each was hand-checked and cleared:

- **`mectx_enabled()`, `satd_avg_enabled()`** — `unwrap_or(true)`. These default **ON**;
  `RFF_MECTX=0` / `RFF_SATD_AVG=0` are documented *escape hatches*, not gates. Not hiding.
- **`hpel_fused_enabled()`** — defaults off, but the accel `hpel_fused` kernel runs by
  default at `inter.rs:832`; the env knob selects the *scalar fused* builder as an A/B
  oracle. The comment states the measurement that decided it (950 vs 617 µs/frame).
  Working as designed.
- **`qpel_compose()`** — defaults off and the fast path is `!qpel_compose()`. It is the
  compose **oracle**, not a disabled optimisation.
- **`abl_deblock()`, `no_skipband()`, `no_runmv()`, `fat_slice_on()`, `batch_on()`,
  `nores_on()`** — ablation knobs; the feature is the default and the knob turns it
  *off*. Correct shape.
- **`site_snapshot`, `snapshot_cycles`, `gate_census*`, `spstats_*`, `satdpath_*`,
  `diastats_*`, `mbtree_satd_*`** — instrumentation APIs consumed by `examples/`,
  which the reference pass deliberately excluded. Not dead.
- **`parse_mb_type_b`, `b_inter_shape`, `to_full`** — each has exactly one real call
  site. Clear.

### 10.2 What this dive changes

| item | was | now |
| --- | --- | --- |
| **D14** (DC-only residual fast path) | "build it" | **"wire H2"** — it exists |
| **X1** (NEON gap) | "≈1,832 lines have no ARM path" | **still true, but H1 is upstream of it** — even the modules that *do* have NEON are unreachable on ARM |
| **§6 rank 14** (NEON port) | rank 14 | **H1 jumps to rank 1** — a build-script fix, not a port |

**The revised top of §6 is H1, then H2, then §6's existing rank 1 (E15).** H1 is three
lines of build script standing in front of five kernel families on every ARM target;
H2 is a documented fast path with a `pub fn` and no callers. Neither requires writing
a kernel, and both are byte-identical-gateable today.

**And the meta-finding holds for the third time in this session.** Across two codecs
and four passes, the reachability lens has now produced: an unreachable checksum
kernel, three unwired FSE table builders, a per-block table rebuilt for a constant,
and now an entire architecture's SIMD compiled-but-uncalled. **Auditing what the code
*calls* keeps out-yielding auditing what the code *does*.**
