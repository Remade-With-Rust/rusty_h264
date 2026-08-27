# inline-execution — converting inline scalar work to SIMD/NEON/AVX2 (rusty_h264)

**Opened 2026-08-22.** Sister document to `rusty_zstd`'s `docs/plans/inline-execution.md`,
run with the same method: census the whole codec mechanically, read the emitted
assembly rather than guessing, audit *reachability* as hard as we audit loops, and
record refutations instead of deleting them.

**50 opportunities, encoder and decoder listed separately** (§3, §4), plus the
cross-cutting findings (§5) and the standing refutations that constrain all of them
(§7).

**Validated 2026-08-26 — read §11 before acting on anything here.** Every claim was
re-verified against the live tree: three §10 findings were FALSE (H3, H6, H7 — one
census-regex blind spot), one was already done when written (H2/D14), D2–D5 is now
contra-indicated, E23 is closed, and three shipping files the census missed are
catalogued (E26/E27/D26). §11.5 carries the corrected execution order.

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

**RE-PRICED 2026-08-26 (§11): the unification is now contra-indicated.** The family
is no longer four spellings — it is **8+ deliberately specialised arms** (add
`recon_p_skip_band`, `recon_b_skip_zero_bi`, `bz_recon_band_bi/copy/fp_bi`,
`b_skip_slow`, plus the CABAC-loop twins), each landed by the skip-run campaign
with banked, gated wins — banded span recon alone was **screen +32% z=5.57**, the
campaign's biggest. Merging them back onto one primitive would revert those wins.
Per the campaign law: *re-price fast paths when the slow path changes under them* —
any residual dedup here must re-run the skip-run gates, not the generic ones.

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

**D14 is DONE — and it was already done when this document was written (§11).**
`reconstruct_4x4_dc_into` landed at 0.8.0 (2026-08-05) and the skip-run campaign's
DC-only collapse (`c866b5e`, 2026-08-20) wired it across I16/chroma/I8×8: **8 call
sites, 2.46M blocks collapsed, ALL-INTRA −8.7% z=4.85, banked.** The §10 H2 item
mistook the superseded `reconstruct_4x4_dc` leftover for the feature. Remaining
work: delete the leftover.

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

**E23 LOCATED (2026-08-26, §11).** The sorts are the percentile machinery in
`encoder/signals.rs` — four sites (`:264`, `:296`, `:336`, `:415`), each a
**per-frame** sort of a per-MB signal map feeding the content-dispatch/AQ
thresholds. Per-frame over ~3,600 entries at 720p: by design, not a defect.
Optional micro: percentiles need `select_nth_unstable` (O(n)), not a full sort —
but at this frequency it is unmeasurable. **CLOSED as located-and-benign.**

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

**CORRECTED 2026-08-26 (§11): the fast path was already wired — under a different
name.** `reconstruct_4x4_dc_into` (same file, `:559`) has **8 call sites** in the
decoder and has been live since 0.8.0; the DC-only collapse was banked by the
skip-run campaign (ALL-INTRA −8.7%). This dive found the superseded twin and read
it as the feature. Remaining action: **delete `reconstruct_4x4_dc`**, nothing more.

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

**FALSE FINDING — RETRACTED 2026-08-26 (§11).** `avg_rows` is called at
`inter.rs:1156-1158` as `avg_rows::<16>/::<8>/::<4>(…)` — **turbofish calls, which
the census regex (`name(` / `name<`) cannot see** — and has been since `61f038b`
(2026-07-27, before this document existed). Not dark, never was.

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

**FALSE FINDING — RETRACTED 2026-08-26 (§11).** Both are called at
`mb16.rs:2467/:2471` as `MV_COST_TAB.get_or_init(build_mv_cost)` — **fn-as-value
references, the second shape the census regex cannot see.** The sister document
`fast-transcendentals.md` had them right the same day: OnceLock-built 4096-entry
tables, runs-once-per-process, already the correct design.

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

**CORRECTED 2026-08-26 (§11): four of the five ARE reachable in-repo** —
`set_subme` / `set_defer_subpel` / `set_subpel_dispatch` from
`encoder/examples/me_ablation.rs`, and `set_precomputed_bs` from
`video-tests/analyzer` (it is also `#[doc(hidden)]` per `docs/great-gate.md`, i.e.
deliberately non-API). **Only `set_turbo` has zero callers anywhere in the tree**,
despite carrying a measured identity (−0.9% BD at ~0.30× superfast wall, per
`WHYS-speed-gap.md`). The product question stands, but it is sharpest for
`set_turbo`: a benchmarked speed rung nothing can invoke.

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

**REFINED 2026-08-26 (§11).** "Test-only" was imprecise: the three scalars are also
the `#[allow(unreachable_code)]` *fallback tail* of `dct_four_t4` /
`idct_four_t4_rec` / `quant_four_4x4` (`transform_quant.rs:285-334`) — the arm a
target that is neither x86-64 nor aarch64 would run. On the two shipping
architectures the arch-gated early `return` makes them **compile-time unreachable**,
so the substance stands: no runtime knob selects the oracle on any machine we ship.

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
  site. Clear. **STATUS CHANGED 2026-08-26 (§11):** all three have since lost their
  shipping-path callers. `parse_mb_type_b` and `b_inter_shape` are now *documented
  test-only aliases* (`#[doc(hidden)]`, encoder round-trip gates — correct shape);
  `to_full` is `#[allow(dead_code)]` "kept for A/B / debug replay" beside
  `recon_p_inter_nores`. All three are annotated, so none is a hidden defect — but
  the next census will flag them, and this line is why it should not re-chase them.

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

---

## 11. Validation pass — 2026-08-26

Every claim in this document was re-verified against the tree at `53ed871` (+ the
uncommitted fast-transcendentals round-2 work). Method: the reachability census was
**re-run with a corrected reference counter** (bare-identifier tokens over
comment-stripped, `use`-stripped source — 1,156 distinct `fn` names, 177 raw
zero-reference candidates, hand-classified), the loop census was re-run over every
crate file, the allocation census re-counted, and each named function, cfg, and
feature default checked in place.

### 11.1 The law this pass adds — how the first census lied

The §10 pass counted references as `name(` / `name<`. That regex is blind to **two
legal Rust call shapes**, and each blindness produced a false finding:

1. **fn-as-value**: `MV_COST_TAB.get_or_init(build_mv_cost)` — no paren after the
   name. This alone manufactured **H6 and H7**.
2. **turbofish**: `avg_rows::<16>(…)` — `::<` between name and paren. This
   manufactured **H3**.

And one shape hides real findings instead of inventing them: a **doc-comment or
comment mention** (`mc_ver02_avg` survived only because its one mention has no
paren). The corrected counter strips comments and `use` lines and counts bare
tokens; anything it flags was then hand-read. **Rule for every future reachability
pass, both codecs: count bare identifiers on comment-stripped source, never a
call-shaped regex.** (Same family as the fast-transcendentals lesson "grep the
EXPRESSION, not the function.")

### 11.2 Item-by-item verdicts

| item | verdict 2026-08-26 |
| --- | --- |
| **H1** accel cfg x86-only | **CONFIRMED, STILL OPEN.** All three `build.rs` files unchanged, stale justification comment intact, 67 `#[cfg(accel)]` sites still compile out on aarch64. Still rank 1. |
| **H2** `reconstruct_4x4_dc` dark | **STALE AT WRITING.** The feature shipped as `reconstruct_4x4_dc_into` in 0.8.0 (2026-08-05), 8 call sites, banked by the DC-collapse (`c866b5e`). Only the leftover deletion remains. D14 is DONE. |
| **H3** `avg_rows` dark | **FALSE** — turbofish blind spot; called since 2026-07-27. Retracted. |
| **H4/X3** `mc_ver02_avg` dark | **CONFIRMED** by the corrected census. The only genuinely dark accel kernel. Wire or delete. |
| **H5** `recon_b_skip_zero_uni` dark | **CONFIRMED.** Sharpened: its sibling `recon_b_skip_zero_bi` *was* wired by the skip-run campaign — the uni twin is the half that stayed dark, exactly the H2+H5 pattern §10 named. |
| **H6/H7** `build_mv_cost` / `build_true_biased` dark | **FALSE** — fn-as-value blind spot. Retracted. |
| **H8** five setters unreachable | **PARTLY WRONG.** Four are example/tool-reachable; **only `set_turbo` is fully dark** — a measured speed rung (−0.9% BD @ ~0.30× superfast) nothing can invoke. Product question stands, now focused. |
| **H9** `encode_bypass_bits` | **CONFIRMED** — still the annotated CABAC-4 placeholder. Correct as-is. |
| **H10** scalar oracles test-only | **CONFIRMED IN SUBSTANCE**, phrasing refined: they are the other-arch fallback tail; compile-time unreachable on x86-64/aarch64. No runtime oracle arm exists on shipping targets. |
| **X1** NEON gap | **CONFIRMED EXACTLY**: x86_asm 802 / satd_avg 602 / intra_pred 225 / hpel 203 / mectx 167 lines, all 0 aarch64 refs; the five portable modules unchanged. And H1 still sits upstream of all of it. |
| **X2** `asm` not default on codec crates | **CONFIRMED** — `default = ["global-alloc"]` on common/encoder/decoder; only facade + CLI carry `asm`. Still open. |
| **X4** allocation census | **RECONFIRMED to the digit**: 60 / 46 / 26 / 24. Still unharvested. |
| **X5** `has_avx2` OnceLock | **CONFIRMED** at `x86_asm.rs:27`. |
| **E23** encode-path quicksort | **LOCATED & CLOSED** — per-frame percentile sorts in `signals.rs` (4 sites). Benign by design. |
| **D2–D5** unify four recon copies | **RE-PRICED: contra-indicated** — the family is now 8+ specialised arms with banked skip-run wins (§3.1 note). |
| **D14** DC-only fast path | **DONE** (see H2). |
| D/E function names | **All 34 named functions still exist**; per-function loop attribution re-confirmed E1=31, E2=29, E3=11, D8=12, D1=8 (was "8", now measures 8), all others within ±1. |

Net: **of the ten §10 findings, three were false and one was stale on the day it was
written — all four from the same regex.** The six that survive (H1, H4, H5, H8-as-
`set_turbo`, H9, H10) were each re-confirmed by the corrected counter.

### 11.3 Instances the first pass missed (the "miss nothing" sweep)

The full-tree loop census (504 fixed-extent pixel loops, all crates) found three
shipping files absent from §0's table:

| file | pixel loops | reading |
| --- | ---: | --- |
| `encoder/lookahead.rs` | 8 | `intra_activity` (4) + `inter_activity` (4) — the mb-tree lookahead's per-MB SATD/activity passes. Cold unless `--mbtree`; **hot inside the opt-in that exists to be cheap**. Catalogue as **E26**. |
| `encoder/signals.rs` | 5 | `b2_mgain` (2) + singles — the per-frame content-signal builders (and E23's sorts). Per-frame per-MB; borderline. Catalogue as **E27**. |
| `encoder/mbtree.rs` | 4 | adjacent to E21, same verdict as E21. |

Decoder-side, per-function attribution adds two names the D-list skipped:
`decode_i8x8` (4 loops — High-profile 8×8 parse+recon glue, catalogue as **D26**)
and `decode_ipcm` (3 — I_PCM, rare by construction, CLOSED-BY-ANATOMY). The
`accel` crate's own 90-odd loops (transform_quant 31, x86_asm 28, …) are the
scalar twins and kernel interiors — **expected, not findings**; recorded so the
next census does not flag the oracle for being scalar.

New reachability classes to EXCLUDE from future passes (all verified deliberate):
public-API accessors with no in-repo consumer (`content_route`,
`display_width`, `display_height`) and the now-annotated test-only aliases
(`parse_mb_type_b`, `b_inter_shape`, `to_full` — see §10.1 status change).

### 11.4 What the concurrent campaign already moved (working tree, uncommitted)

The fast-transcendentals round 2 (sister plan, same dates) landed bounds/divide
work **inside five of this document's targets** — the baselines for these items
have changed under them:

- **E6** `skip_luma_is_free`: row-sliced + the 9-divide → `CEIL_65536_MF` table.
  The *harvest-the-early-exit-distribution* step is still open, but the loop is
  cheaper now — re-price before vectorising.
- **E9/E10** `pred_ssd` / `mb_ssd`: rewritten as per-row zip (2 checks/row from
  2/pixel). The SSD loops LLVM now sees are clean slices — re-read the asm before
  assuming they still decline to vectorise.
- **E11** `save_mb_into`/`load_mb`: **half done, half refuted.** `load_mb`
  row-sliced: −35.6% instructions, banked. `save_mb_into` same rewrite: **+72%
  instructions, REVERTED** — append-into-growable ≠ write-into-presized. The
  structural half (trial-into-scratch, commit once) remains open and is still the
  sleeper.
- **D9–D12 corollary**: the `chunks_exact_mut` rewrite of the predictors measured
  **NET ZERO** (chroma8x8 +4.5% worse) — LLVM already emits memcpy/memset for
  those loop shapes. This *strengthens* CLOSED-BY-ANATOMY: the 41 predict.rs
  loops are not just cold, they are already compiled wide.

### 11.5 Revised execution order

§10.2's revision stands with these strikes and adds; items unlisted are unchanged:

| rank | brick | change |
| --- | --- | --- |
| 1 | **H1** — split the accel cfg | re-confirmed open; still first |
| 2 | ~~H2~~ | done long ago — replace with **delete the three leftovers** (`reconstruct_4x4_dc`, and wire-or-delete `mc_ver02_avg` (H4), `recon_b_skip_zero_uni` (H5)) — one small cleanup brick |
| 3 | **E15** — read the 1,730 xmm ops | unchanged |
| 4 | **D24 + E20** — per-frame setup bodies | unchanged |
| 5 | ~~E23~~ | CLOSED (located, benign) — replaced by **E2** `plan_mb` |
| 6 | **X2** — make SIMD path testable per-crate | re-confirmed open |
| 7 | ~~D2–D5 unify~~ | contra-indicated (§3.1) — dropped |
| 8 | **E6/E7** — harvest skip-test exits | still open; re-price on the new, cheaper loop |
| 9 | ~~D14~~ | DONE — dropped |
| 10 | **E11** — the structural half only | `load_mb` banked, `save_mb_into` refuted |
| — | **E26** (lookahead activity), **D26** (`decode_i8x8`) | new catalogue entries, measure-first, low priority |
| — | **H8-turbo** — expose or delete `set_turbo` | product decision, now sharply scoped |

### 11.6 H1 EXECUTED — 2026-08-26, same day

**The accel cfg is split and the NEON kernels are reachable on ARM.** The change is
exactly the shape §10 predicted: build scripts plus call-site retags, no kernel
written.

- **`build.rs` ×3** now emit two cfgs: `accel` = `asm && (x86_64 || aarch64)`
  (the portable modules: deblock_simd, luma_mc, chroma_mc, transform_quant,
  satd_sad, intra_pred) and `accel_x86` = `asm && x86_64` (the `x86_asm` module:
  `MeCtx`, `sad_x4`/`satd_x4`/`satd_x4p`, `satd_avg`/`satd_avg_x4`, `hpel_fused`).
- **15 call-site retags** to `accel_x86`: 12 in `encoder/mb16.rs` (the satd_avg
  arm, the MeCtx evaluator + its `not()` twin, fixed-centre `fc`, `sp_fc` rings,
  3 lint attrs) and 3 in `common/inter.rs` (`hpel_fused` + its knob + its test).
  Every other site (60+) guards portable kernels and keeps plain `accel`.
- **A discovery that shrank the job:** the x86-only kernels called from
  `deblock.rs` (`mb_uniform`, `bs_motion_masks`, `bs_motion_masks_two_list`) and
  `transform.rs` (`dequant_4x4`) were **already correctly gated** with
  `#[cfg(all(target_arch = "x86_64", feature = "asm"))]` — a second idiom the
  §5 X1 analysis did not distinguish. Only the `accel`-cfg'd sites needed work.

**Gates run:**

| gate | result |
| --- | --- |
| cfg matrix proven from `-v` rustc invocations | aarch64: `accel` SET, `accel_x86` unset — all 3 crates. x86-64: BOTH set → **x86-64 compiles the identical token set, byte-identical by construction** |
| `cargo check` aarch64-unknown-linux-gnu, asm on | **clean** — zero new warnings (4 pre-existing from the concurrent campaign's tree) |
| `cargo check` x86-64 asm + x86-64 scalar (`--no-default-features`) | clean |
| `cargo test --release` accel (37), common+asm (80+12), decoder+asm, encoder+asm | **all green, 0 failures** |

**Deferred, stated honestly:** the 18-stream byte-identical *execution* gate on real
ARM hardware — same standing as the NEON campaign (`385b11b`): compile-verified,
cfg-proven, execution gate = the first ARM build runs the arch-agnostic
differential tests. On x86-64 no execution gate is needed beyond the suites,
because the compiled code is provably unchanged.

**Three small wins identified in the same seam (next bricks, in order):**

1. **Delete `reconstruct_4x4_dc`** (`predict.rs:593`) — the superseded twin of the
   wired `_into` DC fast path (§11.2 H2). Ten lines, zero risk, kills the census
   false lead permanently.
2. **Wire-or-delete `mc_ver02_avg`** (`accel/luma_mc.rs:1071`) — the one genuinely
   dark accel kernel (H4). Its intended consumer is named by the comment at
   `inter.rs:609` (the B-average staging read-back); either fuse it there under
   the census-A/B discipline or delete the 40 lines. Sibling
   `recon_b_skip_zero_uni` (H5) is the same decision one file over — the compiler
   now flags it `never used` in every build.
3. **X2 as a CI line** — the exact commands this gate just ran
   (`cargo test -p <crate> --features asm` ×3) are the missing per-crate SIMD
   coverage. Three lines of CI, closes X2 without touching the feature graph.

### 11.6a H2 EXECUTED — 2026-08-26

**`reconstruct_4x4_dc` is deleted** (`predict.rs:593`, 12 lines). Pre-deletion
verification per the cfg-scoped dead-code law: zero references across crates,
bench, video-tests, tests and examples in every feature combination — the only
mentions left are prose in `docs/fin/WHYS-decoder-perf.md` (historical record,
kept). The live DC fast path (`reconstruct_4x4_dc_into`, 8 call sites) is
untouched. Gates: `--all-features` check clean, facade check clean, common suite
**83/83 green with asm** (81 scalar — the two accel differential tests gate out,
as designed).

*Session note:* the common suite grew 80 → 83 **between two runs in this
session** — the concurrent fast-transcendentals session added 3 transform.rs
tests to the shared checkout mid-flight. Explained, not an anomaly; the
two-sessions-one-checkout law is in force on this tree today.

**Three small wins identified in the same function area (the reconstruct leaf
family, `predict.rs` + its encoder callers):**

1. **Transplant the `_into` shape to the encoder's I4×4 leaf** (`mb16.rs:6356`,
   the DEFAULT path inside the RDO trial machinery, E10–E14's bucket): it still
   does the 16-store `predb` i32 gather → by-value `reconstruct_4x4` → separate
   `store()` — the exact round-trip `reconstruct_4x4_into` was built to kill
   decoder-side (WHYS Part 8/9 priced it). Mechanical, byte-identical-gateable,
   and it deletes the predb gather and `store` call for free.
2. **Same transplant on the two scalar-arm sites** (`mb16.rs:4315` — gather +
   by-value + `store` in the `not(accel)` inter-luma recon; `mb16.rs:4110` — the
   T8-decision SSD variant, where the fusion target is recon-row + SSD in one
   pass rather than a store). Scalar-build path only, so lower priority, but the
   same shape and the same oracle.
3. **Hoist `abl_recon()` out of the per-block leaves** (Law 5 shape): every
   `reconstruct_4x4*` call re-reads the ablation atomic and branches, millions of
   times per frame, to answer a question that is constant for the process. Resolve
   once per frame/slice (or compile the knob out like `prof`). Rider-sized; only
   worth landing with win 1 and only if the anatomy harness moves.

*Law 2 guardrail recorded:* do NOT hand-vectorise `add_residual_4x4`/D15 on the
way past — the fixed-extent i32→u8 clamp loop is the classic auto-vec shape, and
this same file now carries the campaign's NET-ZERO refutation for hand-rewriting
predictor loops LLVM already compiles wide.

### 11.6b H3 CLOSED, H4 EXECUTED — 2026-08-26

**H3 has no action** — it was retracted in §11.2 (turbofish blind spot; `avg_rows`
has been live since 2026-07-27). The execution slot went to the next open item in
the same function area (MC averaging): **H4/X3, and the verdict was DELETE.**

**`mc_ver02_avg` is ripped: ~130 lines** — the public wrapper, its SSE2 arm
(`x86::ver02_avg`), AVX2 arm (`x86_avx2::ver02_avg_w16`), NEON arm
(`arm::ver02_avg`), and the `lib.rs` export; the one comment naming it reworded
so no ghost reference survives for the next census. The evidence made the
wire-vs-delete call unambiguous: the comment at its intended call site
(`qpel_hv`, inter.rs) records that the two-step form (`luma_h` into staging,
then `mc_ver02_avg` reading it back) was **superseded by the fully-fused
`mc_hv_qpel`** in the qpel-fusion campaign (`9fbfb78`, MC −8.3%, banked). The
campaign replaced but never ripped — this is Law 4's second half, four kernels
of it compiled into every build for nothing.

**Gates:** accel differential suite 37/37 green; facade check x86-64 clean;
aarch64 check clean (the NEON arm deletion re-proven on the target it served);
zero `ver02_avg` references remain repo-wide.

**Five small wins identified in the same function area (MC averaging / qpel /
skip-MC):**

1. **H5 `recon_b_skip_zero_uni`** — the dark uni twin of the *wired*
   `recon_b_skip_zero_bi` in the B-skip MC recon family; the compiler now flags
   it `never used` in every build. Same wire-or-delete shape as H4 and the
   evidence leans the same way (superseded by banded span recon `f4f9060`) —
   but read `recon_b_skip_zero_bi`'s router first to confirm the uni case lands
   in a fast path and not `b_skip_slow`.
2. **The Descent-C harvest is pre-instrumented** (`inter.rs:~1140`): single-plane
   half-pel ME candidates are copied into a 256-byte temp then SATD'd, though
   the comment itself notes they are contiguous at plane stride and could be
   SATD'd in place like the interior full-pel path — and `hpelphase::bump`
   already counts the single-vs-qpel split behind `profile`. The harvest the
   comment asks for is ONE profiled run; the win exists only if single-plane
   dominates.
3. **`avg_rows` Law-2 read** (the retracted H3's actual site): the
   `(a+b+1)>>1` loop is the textbook `pavgb`/`urhadd` shape — read the emitted
   asm; if LLVM declined, route the three widths through the existing
   `pixel_avg` kernel (compose-don't-write). Byte-identical either way.
4. **Per-call feature detection in the `luma_mc` dispatchers**: every `mc_*`
   call runs `is_x86_feature_detected!("avx2")` (atomic load + branch) though
   `has_avx2()` (X5's OnceLock) exists one file over — Law 5, measure-first on
   x86. Sharper on ARM: `is_aarch64_feature_detected!("neon")` guards a
   **baseline** feature, contradicting the NEON campaign's own documented
   convention (cfg-only dispatch on aarch64) — those branches can simply go.
5. **`qpel_compose()` re-read per sub-pel call** (3 dispatch sites): the oracle
   knob is process-constant but read per call on the default path — same Law-5
   rider class as `abl_recon()` (§11.6a win 3); hoist to slice scope, gate with
   the anatomy harness, land only if it moves.

### 11.6c H3 EXECUTED AT ITS OWN SITE — 2026-08-26

§11.6b closed H3 as "retracted, nothing to execute." That was too fast: H3's
directive was **"decide and act"** on `avg_rows`, and its still-open half was the
Law-2 read this section now performs. The read produced a real refutation and a
shipped fix.

**The asm read (deterministic, counts not timings):** release + asm,
`rusty_h264-common` emitted assembly —

| symbol | pavgb | xmm | ymm | instr |
| --- | ---: | ---: | ---: | ---: |
| `hpel_block` (before) | 0 | 0 | 0 | 289 |
| `avg_rows::<16>` / `::<8>` / `::<4>` | 0 | 0 | 0 | 191 / 143 / 120 |
| **`hpel_block` (after)** | — | — | — | **282 + 1 `call pixel_avg`** |

**LLVM compiles the `(a+b+1)>>1` quarter-pel average fully scalar at all three
widths** — the "textbook pavgb shape auto-vectorises" assumption from §11.6b
win 3 is REFUTED for this loop — while `pixel_avg` (identical arithmetic, SSE2
`pavgb` / NEON `vrhadd`, `pixel_avg_matches_scalar`-gated) shipped one crate
over with 5 call sites. The fix is the compose-don't-write one-liner:
`#[cfg(accel)]` route the three widths through `pixel_avg` (house pattern from
the sibling site at `inter.rs:507`), `avg_rows` kept as the reachable scalar
twin for non-accel builds. Byte-identical by formula identity plus the kernel's
differential test.

**Gates:** common 83/83 + encoder suites green with asm; facade + aarch64 checks
clean; the post-change asm shows the kernel call. Not clocked (per
`codec-measurement` on this box) — this is an ME-materialization path win whose
size the Descent-C harvest (below) will price.

**The five open small wins in this function area** (ME sub-pel materialization /
averaging; §11.6b's list refreshed now its item 3 is executed):

1. **H5 `recon_b_skip_zero_uni`** — wire-or-delete, evidence leans delete; check
   the `zero_bi` router first (unchanged from §11.6b).
2. **Descent-C harvest** (`hpel_block`'s other arm, pre-instrumented via
   `hpelphase::bump`): single-plane half-pel candidates are copied to a temp
   then SATD'd though contiguous at plane stride — one profiled run decides.
3. **Per-call feature detection in `luma_mc` dispatchers** — Law 5 measure-first
   on x86; on aarch64 the per-call NEON detect guards a baseline feature and
   contradicts the campaign's cfg-only-dispatch convention — deletable outright.
4. **`qpel_compose()` re-read per sub-pel call** — Law-5 rider, anatomy-gated.
5. **The hpel staging planes** (`h`/`v`/`c` in the fused-hpel builder,
   `inter.rs:~830`): allocation frequency unverified — if they are `vec!`-fresh
   per frame per reference, this is the X4/`codec-memory-copies` shape (pool or
   reuse); harvest the allocation count first.

### 11.6d H5 EXECUTED — 2026-08-26 (H4 was already done in §11.6b)

**`recon_b_skip_zero_uni` is deleted (30 lines).** The router read settled
wire-vs-delete beyond argument: the zero-uni B_Skip arm pushes
`BzKind::ZeroUni(list, ri)` into the **banded span machinery** and returns —
the per-MB spelling was never consulted and cannot be reached as a fallback.
The banded span recon (`f4f9060`, the campaign's biggest win) rebuilt this fast
path under a different architecture and left the old name dark — the H2 pattern,
third instance. Sibling `recon_b_skip_zero_bi` keeps its one live caller (the
nonzero-ref pair case) and is untouched.

**Gates (deterministic):** the compiler's own `never used` warning for the
function is GONE from the facade build (was standing in every build); decoder
suite green with asm including the conformance round-trip; aarch64 check clean;
zero references remain.

*Tree note:* a new `unused variable: mf` warning appeared in
`skip_luma_is_free` mid-goal — that is the concurrent session's CEIL_65536_MF
edit leaving its old `mf` binding behind. Theirs to sweep; flagged, not touched.

**Five small wins identified in the same function area (decoder B-skip / job
structs / `mb16.rs` standing warnings):**

1. **The `t8` dead-field pair + `to_full`, one coupled brick**: `BJob.t8`
   (`:9074`) is read nowhere; the second flagged `t8` (`:9544`) is read ONLY by
   `to_full` — which is itself `#[allow(dead_code)]` "kept for A/B replay."
   Delete `to_full` and both fields (+ initializers at `:4438`/`:4442`): three
   compiler warnings gone and the per-MB job payload shrinks. The third `t8`
   (`PInterJob`, read at `:6724`) is live — leave it.
2. **Doubled `#[inline]`** at `:8905-8906` — the "unused attribute" warning;
   one-line deletion.
3. **`edcstat::bump` reads its off-by-default knob per call** — the module's own
   comment says so ("read on EVERY bump — up to four times per B_Skip"). Cache
   it on `FrameDecoder` the way `edc_active` already caches `edc_on()`. Law-5
   rider, pre-annotated by the code itself.
4. **`edc_regions = Some(Vec::with_capacity(4))` per B_Skip MB** on the slow arm
   (`:5862`) — a fresh allocation per macroblock when EDC is active; the
   `codec-memory-copies` shape (take-and-reuse a pooled Vec). Harvest frequency
   under EDC runs first.
5. **Extend `BzKind::ZeroBi` to carry ref indices** — the last per-MB zero-MV
   B_Skip case (`recon_b_skip_zero_bi`, nonzero-ref pairs at implicit 32:32)
   could ride the span machinery like every other zero case; the edcstat
   counters already split the arms, so the frequency harvest is one profiled
   run. Only worth it if nonzero-ref pairs are common on B-heavy content.

### 11.6e H4 CLOSED AS A CLASS — 2026-08-26

§11.6b executed H4's instance (the `mc_ver02_avg` rip). This pass executed H4's
**claim** — *"an unwired kernel is indistinguishable from a bug"* — to its
completion state: a consumer-reference census over the accel crate's ENTIRE
public surface.

**Result (deterministic, a count): 43 exports, 0 dark, 0 dead-code warnings in
the crate.** Every kernel the accel crate ships is reached by at least one
shipping call site in common/encoder/decoder. Before the H4 rip the same census
read 44/1. The unwired-kernel class this codec inherited from the sister
codec's checksum defect is now EMPTY, and this census (qualified-path count per
export) is the standing instrument to keep it that way — cheap enough to re-run
after any kernel landing.

**Five small wins identified in the same function area (the accel dispatch
surface):**

1. **Per-call `is_x86_feature_detected!("avx2")` in the w16 dispatchers**
   (`mc_ver_qpel`, `mc_hor_qpel`, `mc_hv_qpel`, `mc_centre_hq/vq`, `pixel_avg`) —
   an atomic load + branch per kernel call to answer a boot-constant question.
   Law-5 measure-first: one resolved bool/fn-ptr at slice scope; remember the
   OnceLock-per-band loss before celebrating.
2. **The aarch64 NEON detect branches are compile-time-folded but
   convention-breaking**: `is_aarch64_feature_detected!("neon")` expands to
   `true` on the baseline target (zero runtime cost), yet it *reads* as runtime
   dispatch and deviates from the campaign's documented cfg-only-dispatch
   convention (`transform_quant` calls its NEON arms directly). Align for
   clarity — zero codegen risk, stops the next audit from flagging phantom
   dispatch.
3. **H10, still open, lives here**: the `transform_quant` scalar oracles have no
   runtime A/B arm on shipping targets — the `RFF_*` knob pattern the method
   requires and this crate's siblings already use.
4. **Two AVX2 caches coexist in one crate**: `x86_asm::has_avx2()` (OnceLock)
   and std's detect cache (atomic) — X5 says every kernel must reuse ONE.
   Unify; this is how the per-band trap gets reintroduced otherwise.
5. **Re-price CLOSED §7-item 7 (width-4 luma MC)** — its recorded premise
   ("openh264 had no clean SSE2 path, MMX only") **died with the asm rip**: the
   kernels are pure Rust now and a w4 arm is 4-lane `movd` work, not an MMX
   port. The closure predates the architecture it closed against. Harvest w4
   call frequency (mcstats exists) before building anything.

### 11.6f H5 CLOSED AS A CLASS — 2026-08-26

§11.6d executed H5's instance. This pass executed H5's **pattern** — *"specialised
paths, written, dark, beside wired siblings"* — by clearing every remaining
compiler-flagged member in the decoder: **`to_full` deleted** (with the `qp`
field carried only for it), **`BJob.t8` and `PInterJob.t8` deleted** with their
constructor inits, **the doubled `#[inline]` fixed**. The stale doc claim that
`RS_H264_NORES=0` needs `to_full` was refuted in code: the knob routes through
the constructor, never the rebuild.

**Deterministic observable: the decoder now compiles with ZERO warnings and zero
`#[allow(dead_code)]` in `mb16.rs`** (was 3 warnings + 2 allowances). Decoder
suite green with asm including the conformance round-trip; aarch64 clean.

**A near-miss, recorded as a law:** the first cut deleted the WRONG `t8` —
`PInterNoResJob`'s, which is live (the `coded_y` deblock fixup reads it) — because
the warning was mapped to its struct by grep proximity instead of by the
warning's own line number. The aarch64 compile gate caught it before anything
shipped, and the corrected read explained the real deadness: **`PInterJob.t8`
died because `luma8.is_some()` subsumed it.** Map a never-read warning to its
exact definition line before deleting; a field that LOOKS parallel across three
job structs can be live in one and dead in two.

**Five small wins identified in the same function area (decoder EDC jobs /
B-skip machinery):**

1. **`RefFrame`'s struct-level `#[allow(dead_code)]`** (`lib.rs:106`) — a blanket
   mute on the codec's central reference-frame struct, the last allowance in the
   decoder. Remove it and let the compiler name any dead fields — the exact
   instrument that produced this brick.
2. **`edcstat::bump` re-reads its off-by-default knob per call** (carried from
   §11.6d) — the module's own comment complains; cache on `FrameDecoder` like
   `edc_active`.
3. **`edc_regions = Some(Vec::with_capacity(4))` per B_Skip MB** (carried) —
   pool the Vec; `codec-memory-copies` shape.
4. **Extend `BzKind::ZeroBi` to carry refs** (carried) — retire the last per-MB
   zero-B_Skip recon; the edcstat counters already split the arms for the
   harvest.
5. **The next derivable job field** — this brick's own discovery generalises:
   `PInterJob.t8` fell to `luma8.is_some()`. Audit the job structs for the next
   redundant rider (candidate: `nnzs` rides every MT job though MT commits nnz
   earlier via `edc_commit_nnz`); the D9 record (2,784 B → 176 B) shows
   job-slimming is a banked win pattern here.

### 11.6g H6 EXECUTED AT ITS OWN SITE — 2026-08-26

H6 was retracted in §11.2 (fn-as-value blind spot). As with H3, the retraction
left H6's real questions unanswered at its site, and both are now closed.

**Question half — the "superseded by `mvd_cost_tab.rs`" suspicion is REFUTED by
reading.** There is no duplication: the ME rate model is a deliberate
**three-model dispatch** — mode 0 the branchless Exp-Golomb step (CAVLC-exact),
mode 1 `build_mv_cost` = the analytic x264 smooth curve (H-23), mode 2/3
`build_true_biased` = the *measured* CABAC table `MVD_TRUE_COST4` (H-25) plus a
coherence bias — routed per frame by the `b2_mgain` motion probe (the
content-adaptive law applied to the cost function itself). The two OnceLock
builders are the H-23 curve and the H-25 view; `mvd_cost_tab.rs` is H-25's data.

**Act half — the Law-5 defect at the use site is FIXED.** `mvbits` — the
innermost ME cost — ran `OnceLock::get_or_init` (acquire load + branch) **per
candidate per component** for modes 1/2. The table reference is now resolved
once per search above the closure; the step model binds an empty slice and
initializes nothing. Byte-identical by construction (same table, same lookups).
Gates: encoder suite 53 green with asm (includes encode round-trips), both arch
checks clean.

**Five small wins identified in the same function area (the ME cost model):**

1. **`(lambda_me * rate as f64) as i64` per candidate** in the batched arms — a
   float multiply + cast in the innermost ring loops. A fixed-point λ is integer
   but NOT bit-identical (rounding) — BD-gate on varied content or leave.
2. **Ring rate precompute in the fc/sp_fc arms**: a fixed-centre ring's mvd
   component deltas are a small fixed set per step — precompute the per-component
   `mvbits` row once per ring instead of per candidate. Byte-identical.
3. **Threshold-transfer audit on `MVD_TRUE_COST4`**: the table is harvested from
   the CABAC emitter (its own header says so). Verify modes 2/3 cannot activate
   on a CAVLC encode — mode 0 is CAVLC-*exact*, and the entropy-coder axis is
   precisely where two shipped gates already broke (threshold-transfer law).
4. **Law-2 read on the residual `match mvk` branch** inside the closure — LLVM
   should hoist the loop-invariant after inlining; verify from asm before
   splitting closures.
5. **The `min(4095)` saturation edge**: above |d|=4095 the table models go flat
   while EG keeps growing — a search-only bias for extreme vectors. Harvest
   |mvd|≥4096 frequency first (expected ~zero on real content → close as
   anatomy).

### 11.6h H7 EXECUTED AT ITS OWN SITE — 2026-08-26

H7 was retracted with H6 (same census blind spot); §11.6g's Law-5 fix already
covered the shared use site. What remained distinctly H7's — the
`build_true_biased` / measured-table machinery — held one contradiction and one
unanswered audit, both now closed deterministically.

**1. A doc-vs-code contradiction, arbitrated by its own commit.** The H-26 doc
said `RFF_MVCOST_BIAS` "default 0 = pure truth"; the code ships
`unwrap_or(1.0)`. `git show 12dbfb0` proves both lines were **born together** —
an internal inconsistency from birth, not staleness. The surrounding evidence
(mode 3 is named "the biased-truth table"; H-26's verdict text) says the code
carries the intent. **Doc fixed** to state the shipped default and how to get
the H-25 pure-truth form. Behavior unchanged; encoder suite green.

**2. The threshold-transfer audit, resolved by reachability + provenance.**
Deterministic facts: (a) `mv_cost_kind` takes **no entropy-coder input** — the
dispatch is entropy-blind by construction; (b) modes 2/3 (the CABAC-harvested
table) are reachable ONLY via `RFF_MVCOST=2/3` or the `me_ablation` example —
**no shipping path can select them**, so the harvested-under-CABAC table cannot
silently drive a CAVLC encode; (c) the shipping exposure narrows to **mode 1**,
whose H-23/H-24 calibration era was CABAC (H-23's own title — "mvd's *bypass*
tail" — is CABAC terminology). The open half is BD-shaped, not code-shaped:
T=0.10 has never been validated on a CAVLC encode (win 1 below).

**Five small wins identified in the same function area (the mv-cost dispatch /
probe machinery):**

1. **Run the H-24 BD table once under CAVLC** — the only axis the mode-1
   dispatch corpus never varied (threshold-transfer law). One bench run; either
   it validates T=0.10 there or the dispatch gains a CAVLC guard.
2. **`set_mv_smooth` / `set_mv_smooth_mode` join the H8 bundle** — pub API,
   example-only callers, same product decision (expose, `#[doc(hidden)]`, or
   delete) as the five H8 setters.
3. **The twin probe sites** (`fe.mv_smooth = mg >= mv_smooth_t() && …` at
   `:5418` AND `:8798`) — the duplicated-expression shape the divide campaign's
   twin law exists for; hoist to one helper so the CAVLC and CABAC slice paths
   cannot drift.
4. **Hoist `mv_smooth_mode()` to a per-frame field** — it runs an atomic load +
   match (+ OnceLock on first pass) per SEARCH inside `mv_cost_kind`; same
   Law-5 shape as §11.6g's fix, one level up. Byte-identical.
5. **The `min(4095)` saturation harvest** (carried from §11.6g) — count
   |mvd|≥4096 on real content; expected zero → close as anatomy.

### 11.6i H8 EXECUTED — 2026-08-26

The product question answered itself from the project's own record: deleting
`set_turbo` would contradict `WHYS-speed-gap.md`, which banks it as **"the
honest shape rung"** (1.81× faster than default quality, −0.9% BD vs x264
superfast) and explicitly frames it as composable with the presets. The
execution was therefore **expose**:

- **`--turbo 1` added to the CLI** (composable with any `--preset`, per the
  recorded design; ARGV not env, per the pinvs.ps1 harness rationale), and
  `set_turbo` re-exported through the facade with its measured identity.
- **Two stale-string fixes in the same touch**: the `--preset` error text said
  `(fast|quality)` though `balanced` parses (the exact "documented and never
  parsed" drift its own comment records), and the usage line now carries both.

**Gates (the flag must PROVE it reaches the machinery):** default vs `--turbo`
encodes of a split-heavy synthetic clip **differ** (8,136 → 8,941 bytes — the
documented bus-class signature of the shape rung); the turbo stream round-trips
in our own decoder AND decodes **byte-identical vs ffmpeg** (the dual gate);
the flag-absent path is unchanged by construction. CLI builds clean.

Note: H8's framing quote — *"there is NO speed/quality preset"* — was already
stale (the CLI ships `fast|balanced|quality`); with `--turbo` the missing
x264-superfast-class rung is now reachable too. Remaining H8 members
(`set_defer_subpel`, `set_subpel_dispatch` + H7's `set_mv_smooth*`) are
research knobs with example callers — win 2 below.

**Five small wins identified in the same function area (the product/knob
surface):**

1. **Expose the `--subme` ladder** (`set_subme` 1..=5, me_ablation-only today) —
   the other measured effort rung; the docs explicitly invite composing it with
   turbo.
2. **Research-knob API hygiene**: `#[doc(hidden)]` on `set_defer_subpel`,
   `set_subpel_dispatch`, `set_mv_smooth`, `set_mv_smooth_mode` — the treatment
   `set_precomputed_bs` already received — so the public API states what is
   product and what is experiment scaffolding.
3. **A knob↔env-twin registry** (one doc table: API fn, `RFF_*` twin, measured
   BD/speed, provenance commit) — the census keeps re-finding these one at a
   time; a registry closes the vein and gives harnesses the ARGV story.
4. **A `--preset superfast` composition** — name the turbo(+subme) composition
   the way users expect from x264; the docs note the eventual no-tax answer is
   the per-frame split dispatch (H-11 next-brick b), so the preset is the
   bridge until then.
5. **The two-command flag-proof as a CI smoke** — encode ±flag, assert
   byte-difference + own-decode: the cheapest standing guard against the
   knob-stops-reaching-its-machinery rot class this campaign keeps finding.

### 11.6j H9 EXECUTED — 2026-08-26

§10 called H9 "a placeholder doing its job." The execution found the marker had
quietly gone STALE: `encode_bypass_bits` was "reserved for the P/B inter syntax
(CABAC-4)" — but CABAC-4 shipped long ago and **reimplemented the reserved loop
inline** in `cb_exp_bypass`'s suffix tail, which serves both the mvd UEG3 and
coeff-level UEG0 bypass suffixes. A reserved API whose consumer landed without
it is the compose-don't-write defect in miniature.

**Executed: WIRED.** `cb_exp_bypass`'s tail now calls `encode_bypass_bits(n, k)`
— the same call sequence, byte-identical by construction; the
`#[allow(dead_code)]` and the stale "reserved" doc are gone.

**Gates:** a real CABAC encode (the §11.6i clip, quality preset) is
**byte-identical** to the pre-wiring binary's output; encoder suite 54 green
with asm; the function now has a live caller (the census closes).

**Five small wins identified in the same function area (the CABAC writer):**

1. **E15's read is still the area's headline** — 1,730 xmm : 57 ymm in
   `encode_slice_data_cabac_p` remains unread (§6 rank 3 originally). Everything
   below prices against what that read finds.
2. **Arithmetic bypass batching**: `encode_bypass_bits` still walks the full
   per-bin low/range path; the x264-class form codes n bypass bins in one
   arithmetic step. Now that ALL EG suffixes route through one function, one
   upgrade site covers mvd + levels. Law-1 measure-first — the writer's share
   of encode is unpriced.
3. **`self.bins += 1` per bin in the shipping writer** — the bit-accountant's
   clock ticks unconditionally in release. Deterministic first step: find its
   non-instrument readers; if none, feature-gate it with the accountant.
4. **Twin audit of the 4×4 vs 8×8 residual emitters** — the sign-bypass +
   c1/c2 ladder shapes exist in both; grep the EXPRESSION (the divide
   campaign's twin law) before either is touched again.
5. **The `put_bit`/`outstanding` carry chain** emits bit-by-bit; a byte-wise
   packer (the decoder's window shape) is the known upgrade — priced only
   after win 1 says the writer matters at all.

### 11.6k H10 EXECUTED — 2026-08-26. The H-list is CLOSED.

The method-compliance gap is closed by **adding the runtime arm** the method
demands (not by amending the method): `RFF_TQ_SCALAR=1` pins `dct_four_t4`,
`idct_four_t4_rec` and `quant_four_4x4` to their scalar oracles at runtime —
the house cached-atomic shape, sitting beside `abl_recon` which already rides
the same dispatchers. The scalar twins + knob are now public oracle surface
(census-exempt by the comment at the export).

**Gates:**
- **Liveness proven out-of-process** (`tests/tq_scalar_arm.rs` — an integration
  test is its own process, so the env-before-first-call ordering the knob's
  cache requires is guaranteed; a unit test cannot promise that). The knob
  engages and the forced arm bit-matches the scalar twins through the PUBLIC
  dispatchers.
- **The oracle run end-to-end for the first time**: a real CABAC quality encode
  under `RFF_TQ_SCALAR=1` is **byte-identical** to the kernel-arm encode —
  simultaneously proving the arm works and re-validating the SSE2 kernels
  against their oracles on real data.
- accel suite 37+1 green; aarch64 clean — **ARM now has a rebuild-free
  scalar-vs-NEON bisection arm, the exact tool §11.6's deferred ARM execution
  gate was waiting for.**

**Five small wins identified in the same function area (the oracle/dispatch
discipline in the accel crate):**

1. **The twin ablation knob**: `RFF_ABL_RECON` is cached by TWO independent
   statics (`common/predict.rs` and `accel/transform_quant.rs`) — same env,
   two caches, semantics currently agree by luck of parallel edits. Single-source
   or cross-test them (the twin-expression law applied to knobs).
2. **Crate-wide oracle-arm completeness sweep**: transform_quant now complies;
   audit deblock_simd, chroma_mc, satd_sad, intra_pred against the same method
   line — each either gets the H10 pattern or a recorded reason it is exempt
   (some have partial arms: `qpel_compose`, `RFF_SATD_AVG`).
3. **The idct dispatcher now reads two cached knobs per call**
   (`abl_recon` + `tq_scalar_forced`) — Law-5 rider; fold only if a future
   profile ever shows the dispatcher hot (Law 2 says it will not).
4. **A per-family arm-introspection accessor** ("which arm is live") for bench
   logs — turns every future liveness gate into a one-liner and makes
   gate-must-prove-the-tool-ran cheap by default.
5. **X5 carried**: unify `has_avx2()` (OnceLock) with std's detect cache — one
   crate, one caching mechanism, per X5's own directive.

**With this, every item of §10's reachability dive is executed or closed:**
H1 (cfg split, NEON reachable), H2/D14 (DC path done + leftover ripped),
H3 (asm-read → pixel_avg composed), H4 (kernel ripped + class proven empty),
H5 (dark twin ripped + warnings zeroed), H6 (3-model dispatch verified + Law-5
hoist), H7 (contradiction arbitrated + transfer audit), H8 (--turbo shipped),
H9 (stale reserve wired), H10 (oracle arm + end-to-end differential). Ten
items, ten deterministic gates, zero clocks trusted.

### 11.8 E15 HAMMERED — 2026-08-26. Ten deterministic wins.

**Win 1 — the read, and both §4.4 guesses were wrong.** Mnemonic classification
of the 1,723 "xmm ops": **420 are `.seh_savexmm` — Windows SEH unwind
DIRECTIVES, not instructions (24% census artifact)** — and the rest are ~1,200
**scalar f64** moves/compares/converts (`vmovapd` 888, `vucomisd` 79, `vmulsd`
35…): the RD-decision currency (λ, J values) spilled around 461 calls. Not
pixel work, not struct copies — **register pressure from f64 locals live across
the mode-decision calls. There is nothing to vectorise; E15-as-kernel-target is
CLOSED forever.** Census methodology fix recorded: exclude `.seh_*` directives
before counting.

**Wins 2–10 — executed, all byte-identical across FOUR config baselines**
(CABAC/CAVLC × P-only/B-frames — the cross-axes gate):

| # | win | shape |
| - | --- | ----- |
| 2 | `lambda.sqrt()` per coded MB → slice-hoisted | Law 5 |
| 3 | `shape_rd_on()` computed per MB AND re-called later the same MB → one slice read | Law 5 + dedup |
| 4 | six knob reads per coded MB (`split_t`, `sub8x8_split_on`, `sub8_grain_veto_on`, `sub8_rd_on`, `intra_rd_on`+`intra_rd_grain_gate`) → slice-hoisted | Law 5 |
| 5 | `sig.grain_signature()` per coded MB ×2 → frame-constant hoist | Law 5 |
| 6 | `bitacct::enabled()` ×6 per MB → one slice read | Law 5 |
| 7 | `greedy_min_free == 0` re-tested per MB → hoisted | invariant |
| 8 | the two byte-for-byte terminate+accountant blocks → one `term_acct` helper | twin-drift protection |
| 9 | `mb_variance()` recomputed per MB → `sig.mb_vars()` memoized dedup, **in all FOUR drivers** (the lockstep law; the CAVLC-P, CAVLC-B and CABAC-B twins carried the same defect) — `mb_variance` import retired | dedup |
| 10 | **the gate census ran 2 atomic RMWs + a TLS RefCell push per consultation + a TLS drain per MB, unconditionally in release** — now default-off behind cached `RFF_GATE_CENSUS`; gatecheck/mecost opt themselves in | instrument off the shipping path |

**Deterministic deltas on the symbol** (same instrument as the baseline):

| metric | before | after | Δ |
| --- | ---: | ---: | ---: |
| instructions | 11,377 | **10,012** | **−12.0%** |
| xmm lines | 1,723 | 1,477 | −14.3% |
| bounds checks | 50 | 43 | −7 |
| calls | 461 | 436 | −25 |
| `lock` ops | 2 | **0** | census off the hot path |

**Gates:** four-config byte-identity (P/B × CABAC/CAVLC), encoder + decoder
suites green, aarch64 unaffected (encoder-only edits). Win 10 changes
instrument semantics knowingly: census counters read zero unless
`RFF_GATE_CENSUS=1` — the two harness examples set it themselves. Per Law 1,
no clock was consulted; the wins are counts. The remaining 43 bounds checks
and 436 calls are the residue for a future pass — and the B-CABAC driver still
carries its own copies of wins 2–8's patterns (the lockstep follow-on).

### 11.9 E15 ROUND 2 — ten more, 2026-08-26

Gated on **five config baselines** (P-CABAC, B-CABAC, CAVLC-fast, CAVLC-fast+B,
all-intra — CAVLC's default preset is Fast, so the fast path is covered) plus
suites. Round 2's wins are **dynamic-frequency** wins the static instruction
count barely shows (P symbol ~flat at 10,019): calls that ran per BYTE or per
MACROBLOCK now run once per slice.

| # | win | scope |
| - | --- | ----- |
| 11 | **CABAC payload hand-off: per-BYTE `write_bits` loop → one `write_aligned_bytes`** (new BitWriter API with the invariant-correct pending-byte drain) | all FOUR slice tails (I/P/B/all-skip-B) — ~payload-size calls per frame → 1 `extend_from_slice` |
| 12 | **`cb_term_acct` module helper** — the terminate+accountant block was a byte-for-byte twin at SEVEN sites | P×2, B×4, I×1 deduped |
| 13 | **B-driver `acct` hoist** — `bitacct::enabled()` re-read up to 10×/MB | 1 read/slice |
| 14 | **I-driver `acct` hoist** (the same twin, found by the n=2 assert) | 1 read/slice |
| 15 | **`lme_vars` precompute** — the per-coded-MB `sig.mb_vars()` OnceCell deref folded into the threshold Option | P driver |
| 16 | **`cb_fill_inter_cache` row-binding** — 8 bounds checks per call from repeated outer-index accesses → row refs bound once | every P and B coded MB |
| 17 | **E20 CLOSED BY READ**: `FrameEncoder::new` = 2,891 instrs, **31% of its 358 xmm are SEH directives**, 3 memsets + vec zero-init stores, 0 bounds checks — allocation, not redundant clears. §4.5's suspicion corrected; **D24 predicted to be the same class** (same census, same artifact). | catalogue |
| 18 | **Dead 128-byte zero fill** per fast-preset non-free MB (`skip_c`'s filler arm, never read on that path) → `Option`, never built | fast preset |
| 19 | **Duplicate `fe.cur_qp = qp` pairs in FOUR drivers** (entry + post-harvest; nothing between touches it) — post-harvest copies deleted | CAVLC-P, I, CABAC-P, CABAC-B |
| 20 | **REFUTED + RECORDED**: slice-binding `&aq_qp[..total]` removes ZERO loop bounds checks (43 → 43) — the `mb_y*w + mb_x` index defeats the length hint, the decoder campaign's runtime-extent refutation in encoder form. Reverted, refutation left at the site. | law |

Also checked-clear: `bstats` is already knob-guarded (unlike the gate census W10
caught). The twin-hunting asserts (`count == n`) caught THREE unplanned twins
mid-patch — the greedy commit, the 16-space terminate, the cur_qp pair — each
either correctly scoped or harvested; the all-or-nothing patch style is why
none of them shipped half-applied.

**Cumulative E15 campaign** (rounds 1+2): P symbol 11,377 → 10,019 instrs
(−11.9%), locks 2 → 0, bc 50 → 43, ~20 slice-constant reads and 2 census
subsystems off the per-MB path, per-byte hand-off gone from four drivers —
every change byte-identical across five configs. Remaining residue: 43 bc / 439
calls in P, the B-driver's per-MB knob sweeps (own §11.8-style pass), and the
per-MB `Vec` allocations in the mode-decision (structural, SmallVec-shaped).

### 11.10 D24 + E20 HAMMERED — 2026-08-26. Fifteen wins.

**The reads first, closing both catalogue items:**

- **E20 (`FrameEncoder::new`)**: §11.9's artifact finding held (31% SEH, vec
  zero-init, 0 bc) — but the REAL payload was hiding in plain sight:
  **fourteen raw `std::env::var` reads PER SLICE** (`RFF_SUB8X8`, `RFF_ME_WIDE`,
  `ME_WIDE_VAR`, `ME_RESCUE`, `ME_COH`, `ME_RANGE`, `ME_FASTMO`, `ME_LEARN`,
  `ME_PAYOFF`, `DEFER_SUBPEL`, `INTER8`, `INTER8_PEN`, `SUBPEL`,
  `RDSKIP_MINFREE`) — an OS query + String alloc + parse each, while every
  other knob in the tree caches. The constructor was one-third env-parsing
  machinery.
- **D24 (`FrameDecoder::with_pool`)**: the "1,053 xmm" = **29% SEH directives**
  + 18 memsets that are all GridPool refills (`clear`+`resize` — the design
  working as intended, not redundant clears). §3.5's `vec![0]`-immediately-
  overwritten suspicion: NOT FOUND. Two true residues named: the three
  full-frame recon planes are the ONLY un-pooled allocations (structural —
  their ownership leaves via `into_frame`), and the ref-POC mirrors were
  fresh-allocated per picture.

**Wins 1–14: the `CtorEnv` cache** — one `OnceLock` struct holds all fourteen
parsed knobs; `FrameEncoder::new` reads fields. Each replaced expression is
mirrored exactly (Option where a cfg/preset fallback applies). Process-constant
by the same contract as every cached knob; A/B harnesses set env before
encoding, unchanged.

**Win 15: `ref_poc0`/`ref_poc1` join the GridPool** — the last per-picture Vecs
outside the pool; alloc+collect per picture → clear+extend on recycled capacity.

**Deterministic scoreboard:**

| symbol | before | after | Δ |
| --- | ---: | ---: | ---: |
| `FrameEncoder::new` instructions | 2,891 | **1,752** | **−39.4%** |
| — plus per SLICE at runtime | 14 OS env queries + ~14 String allocs + parses | 0 | eliminated |
| `with_pool` heap allocs per picture | 2 (ref-POC mirrors) | 0 | eliminated |
| `with_pool` static instructions | 6,191 | 6,296 | **+105 — recorded honestly** (the extend path inlines larger than collect; accepted for the dynamic alloc win) |

**Gates:** five-config encode byte-identity, BOTH decode streams byte-identical
vs ffmpeg (the absolute oracle), decoder and encoder suites green.

**Residue for the next pass:** the un-pooled recon planes (needs a frame-return
lifecycle, structural), and **D25** (`Decoder::decode`, 4,904 instrs, 240 SEH,
**29 memcpys** — the decoder-pivot `.cloned()` history says read those next).

### 11.11 D25 + THE SETUP NEIGHBORS — ten more, 2026-08-26

The per-frame/per-slice copy-and-alloc sweep D24's read pointed at. All wins are
**deterministic by construction** (the allocation/copy is provably absent from
the path); the static instruction counts moved little — these are per-slice and
per-NAL frequency wins.

| # | win | what it was |
| - | --- | ----------- |
| 1 | **`coded_source` → `Cow`** | THREE full source-plane clones PER SLICE in the MB-aligned (default) case — the drivers only read them. ~1.4 MB/slice at 720p → 0. |
| 2 | **SPS storage → `Arc`** | struct clone (scaling lists included) per slice → refcount bump |
| 3 | **PPS storage → `Arc`** | same |
| 4 | **`emulation_unprevent` → `Cow`** | fresh Vec + full copy PER NAL; most NALs carry no 0x03 — scan first (same predicate), borrow when clean, original loop verbatim otherwise |
| 5 | **`split_annex_b` single-pass** | a second `starts` Vec + a second walk per access unit → one Vec, one walk, same slices |
| 6 | **`CabacState` per-thread pool** | **13 fresh Vecs (~hundreds of KB) PER CABAC SLICE — D13's decoder finding, never ported to the encoder.** `refill()` resets to `new(n)`'s exact contents on retained capacity |
| 7 | **`mb_qpy` pooled** (3 drivers) | per-slice `vec![qp; mbs]` → recycled |
| 8 | **slice-tail `ref_id` pooled** (2 drivers) | per-slice collect → clear+extend on retained capacity |
| 9 | **`reorder_l0/l1` + `mmco` scratch on `Decoder`** | three per-slice header Vecs → take/put recycling |
| 10 | **CABAC payload buffer recycled** (`CabacEncoder::new_with_out`, 4 drivers) | the output Vec grows to slice size every slice; capacity now survives |

**Gate story — and a two-sessions incident worth its own paragraph.** The
five-config byte-gate FAILED on first run: every config's output had changed.
The cause was NOT this batch: the concurrent session landed encoder behavior
changes mid-flight (mbtree +233 lines, signals +211, rc, config — smaller
output at every config), invalidating the frozen baselines. The batch was then
proven three ways: output DETERMINISTIC, all four stream types **byte-identical
vs ffmpeg** (the absolute oracle), suites green — then baselines re-frozen.

**The bonus find (win 11, unplanned): the fuzz gate caught a LATENT
malformed-stream OOB** in `recon_p_skip_band` (`plane 6400 vs index 6416`) —
surfaced BECAUSE the fuzz corpus is built by encoding with our encoder, whose
bytes the concurrent session changed: new corpus, new mutations, new territory.
On malformed streams a reference can carry geometry that does not match the
open picture; the band fast path now REFUSES via checked slices instead of
panicking (repro banked in scratch; conformant streams provably unaffected —
ffmpeg-identical decode after the fix). Sibling bands (`recon_b_skip_zero_bi`,
`bz_recon_band_*`) share the exposure class — the fuzz gate is the standing
finder; noted for the next pass.

**Final gates:** 0 failing suites across all three crates (fuzzer included),
dual-decoder conformance byte-identical, encode identity vs re-frozen
baselines. Static-count honesty: `Decoder::decode` +371 instrs (the inline Cow
scan), memcpys 29 → 25 — the wins live in per-slice/per-NAL dynamic counts.

### 11.12 E2 HAMMERED — twenty wins, 2026-08-26

**The read first, and §4.1's premise is DEAD: `plan_mb` is no longer
"no-ymm".** Today's symbol reads **322 ymm : 68 xmm** — the concurrent
campaign's rewrites plus compiler drift vectorised it since the 2026-08-22
census. What remained true: 66 bounds checks (the encoder's densest surviving
site) and a body full of per-pixel loops in the proven row-slice shapes.

**The twenty** (every one byte-identical across the five config baselines,
first run):

| # | win | class |
| - | --- | ----- |
| 1 | luma top-row gather → `copy_from_slice` | row slice |
| 2 | chroma `ntop` rows ×2 → `copy_from_slice` | row slice |
| 3 | `ssd16` per-pixel → row zips | row slice |
| 4 | I4 recon gather+SSD 256-loop (`i/16, i%16`) → row copy + zip | row slice |
| 5 | `j8` SSD 256-loop → row zips | row slice |
| 6 | I4 recon restore 256-loop → row copies | row slice |
| 7 | I16 recon commit 4-deep nest → 16 row copies | row slice |
| 8 | I4 modes z-order scatter → raster row copies | row slice |
| 9 | I16 modes scatter → row `fill(2)` | row slice |
| 10 | `coded_y` scatter → row `fill(true)` | row slice |
| 11 | **winning chroma mode was RE-PREDICTED after the search** → cache the pair from the loop (2 `chroma_pred` calls/MB gone) | redundant recompute |
| 12–13 | `gather_i4` top + top-right rows → copy-or-fill | row slice |
| 14–15 | `gather_i8_enc` top + top-right rows → copy-or-fill | row slice |
| 16 | `plan_i4x4` winner **re-predicted** after each block's search → cached (1 `intra4x4_pred` per block ×16) | redundant recompute |
| 17 | `plan_i4x4` recon → `reconstruct_4x4_into` — **§11.6a win 1 executed**: no `[u8;16]` temp, no separate `store` walk | fused kernel transplant |
| 18 | `plan_i8x8` winner re-predicted → cached (1 `intra8x8_pred`/block ×4) | redundant recompute |
| 19 | `plan_i8x8` residual per-pixel → row slices | row slice |
| 20 | `chroma_pred` accel arm zeroed a 256-byte buffer for a 64-byte pred then copied out → right-sized aligned buffer, direct return | dead fill + copy |

**Deterministic scoreboard** (`plan_mb` symbol; the helper wins are inlined into
it): instructions 5,543 → **5,428**, bounds checks 66 → **31 (−53%)**, calls
162 → **149** (the cached winner predictions, statically visible). Encoder
suites 0 failing; decoder untouched; fuzz corpus unchanged (output
byte-identical ⇒ seed streams identical).

### 11.12a E2 ROUND 2 — seven more, and the A/B that beat a moving tree

**Seven further wins** in the E2 family, found by reading the parts round 1
skipped (the `plan_i8x8` tail, `trial_intra`, the `best_i16_*` twins):

| # | win |
| - | --- |
| 21 | `plan_i8x8` recon commit per-pixel nest → row copies |
| 22 | `plan_i8x8` 2×2 mode/coded publish → paired `fill`s |
| 23 | `trial_intra` built a fresh heap `BitWriter` PER RD TRIAL → thread-recycled (`BitWriter::clear` API added) |
| 24–25 | `best_i16_sad` AND `best_i16_satd` carry the identical top/left gather **twins #3 and #4** of plan_mb's — row-sliced |
| 26 | **the column-gather experiment KEPT**: bounded column slices (`col[i*cw]` provably inside `15·cw+1`) across all six left-gather sites — the batch took plan_mb's bc 31 → **24** |

Cumulative E2: `plan_mb` instructions 5,543 → **5,401**, bounds checks 66 →
**24 (−64%)**; calls 149 → 156 (+7 — the pooled-writer take/put pairs,
recorded honestly).

**The gate story is the section's real lesson.** The five-config byte-gate went
red AGAIN mid-round, plus two encoder tests — and this time a minute-later
rebuild hit a compile error in code that isn't ours (`b_is_ref`, lib.rs): the
concurrent session was landing an mbtree/lookahead campaign LIVE (lookahead.rs
+128 new lines between my two builds; their own tests red in their own area).
Frozen baselines cannot gate a shared moving tree. The answer was a **direct
A/B**: AST-invert my whole round-2 patch, rebuild, encode all five configs,
compare against the forward arm — with a tree FINGERPRINT taken around both
arms to prove no interference landed inside the window. First attempt: the
reverse arm FAILED TO BUILD (their transient break) and the comparison
"passed" vacuously — **gate-must-build-what-it-tests caught in the act**; the
error count in the gate script is what exposed it. Second attempt: both arms
clean, fingerprints equal, **all five configs byte-identical with vs without
the patch**. That A/B — not a frozen baseline — is the attribution instrument
for shared-tree work from here on; the inverse-patch generator
(`ast`-swap of the edit tuples) is in the session scratchpad.

### 11.13 X2 LANDED — the CI was stale in exactly the X2 shape. Ten gates.

The read: `.github/workflows/ci.yml` still **installed nasm** (ripped
2026-08-12 — nothing uses it), its asm job's comment described "vendored
openh264 SIMD assembled with nasm" (gone), Windows was excluded from the SIMD
matrix (a nasm-era artifact — the intrinsics build everywhere), and
`cargo test --workspace` ran each codec crate with ITS OWN defaults — the
scalar path. **The X2 blind spot was not just a local habit; it was
institutionalized in CI.**

**The ten deterministic gates now standing** (each executed locally as proof):

| # | gate | what it catches |
| - | ---- | --------------- |
| 1–3 | per-crate `--features asm` test runs (common / encoder / decoder, release) | the X2 core: kernels, dispatch sites, and the H10 oracle arm finally covered per-crate |
| 4 | dead nasm installs + stale premise DELETED | CI honesty; two package installs per run gone |
| 5 | **Windows joins the SIMD matrix** | the intrinsics path on MSVC, previously never built in CI |
| 6 | facade end-to-end suite as a named step | encoder+decoder together under asm |
| 7 | the mutation fuzzer as its OWN job (release) | malformed-input armor, named in the UI — and its corpus is our own encoder's output, so it doubles as an encode smoke |
| 8 | aarch64 cross-check ×2 arms (asm + scalar) | the H1 regression guard: a consumer-side cfg regression becomes a compile error, not silent scalar |
| 9 | `--all-features` workspace check | the cfg-scoped dead-code law as CI |
| 10 | CLI smoke: synthetic YUV → encode → OWN decode, size-exact | CLI plumbing rot, zero external tools |

**Local execution proof:** 9 of 10 green on today's tree; the encoder step is
**correctly red** on the concurrent session's two in-flight test failures
(`pps_roundtrips_through_reader`, `encodes_access_unit_with_sps_pps_idr` —
their active mbtree/params landing). A gate that catches the other session's
mid-flight state on first contact is the gate earning its keep, not a defect.

X2 closes. Of the §5 cross-cutting findings, only X1 (the ME-family NEON
green-field) and X4 (the allocation-census harvest) remain open.

### 11.14 THE DEEP DIG — what is still hiding, by fresh census (2026-08-26)

Four new instruments over TODAY's tree (the 08-22 census is obsolete —
SEH-corrected, post both sessions' campaigns): symbol census by true
instruction count, bounds-check concentration map, the winner-re-predict law
grep, and the CAVLC-driver knob sweep. Encoder bc total now **578** (was 618);
decoder bc total **1** — the 765→0 campaign is CLOSED-CONFIRMED.

**Newly surfaced (never on any list):**

1. **`emit_intra_body_cabac` — 50 bc in 1,963 instrs (2.5% density, the
   encoder's DENSEST site)**. Hidden because the original census ranked by
   top-12 symbol size, and this helper is mid-sized. Per-MB `CabacState`
   neighbor-cache indexed traffic (`cs.cmode[a]`, `cs.cbf_dc[a]`, …) plus
   residual-emission loops. Catalogue as **E28**; first step is the read.
2. **`skip_chroma_is_free` — 23 bc in 772 instrs (3%)**: the transcendentals
   round row-sliced `skip_luma_is_free` (23→2) and NEVER TOUCHED THE CHROMA
   TWIN. The twin law's cleanest remaining instance; the fix is the proven
   luma shape transplanted. (E7's other half.)
3. **`bitacct::enabled()` re-read per SYNTAX ELEMENT inside the emit helpers**
   (~12 sites in pos/add pairs at `:8419–:8893`) — §11.8's hoist stopped at
   driver scope; the helpers re-read the knob several times per MB. Fix:
   thread the slice-hoisted `acct` in as a parameter (or hoist per helper).
4. **The 5,500-instr anonymous symbol** = `frame_mt::decode_stream_threaded_sink`'s
   worker closure — named at last; opt-in path (frame-MT), leave until that
   campaign reopens.

**Re-confirmed / re-ranked for the next hammers:**

5. **E1 `plan_inter_mb`: bc 79 / 6,360 instr — now the encoder's #1 bc
   concentration** and the direct sibling of the E2 pass (same shapes: gathers,
   SSD loops, winner caches). The hammer is loaded.
6. **B-CABAC driver** (6,108 instr, 38 bc): still owed the §11.8 rounds' twin
   sweeps (knobs, sctx, per-MB glue).
7. **CAVLC-P driver** (5,561 instr, 251 calls): knob-CLEAN (4 in-loop reads —
   earlier campaigns hoisted it) but call-heavy; after E1.
8. `encode_inter_mb` (21 bc) — small row-slice pass.
9. `encode_all_bframes` (4,695 instr, E16) — per-GOP frequency, LOW priority
   (honest: big body, cold cadence).
10. **Verification wins**: the winner-re-predict grep comes back CLEAN across
    the encoder (the E2 law is fully swept — its one hit is the decoder's
    parse-driven predict, not a search), and the decoder's single remaining
    bounds check lives in a derive(Clone). Two closed questions that cost a
    grep each.

**Standing residue re-stated with today's numbers:** X4's remaining vein is
the mode-decision per-MB `Vec` allocations (`pick`/`p8`/`shape_cands` —
SmallVec-shaped, structural); X1 (ME-family NEON) unchanged; the un-pooled
decoder recon planes (frame-lifecycle) unchanged.

### 11.15 THE TAIL CLOSED — X1 first blood, harvests banked (2026-08-26)

Ten wins across the catalogue's remaining tail:

1. **X1 FIRST FAMILY PORTED: `hpel_fused` has a NEON twin.** Moved out of
   `x86_asm` into a portable module — AVX2 on x86-64 (unchanged code), **NEON
   on aarch64** (8-lane i16, widening to i32 for the centre plane; the
   round-shift-saturate steps are `vqrshrun_n_s16::<5>` / `vqrshrn_n_s32::<10>`,
   bit-exact per the module's arithmetic notes and the same idioms `luma_mc`'s
   NEON already ships). Consumer sites flipped `accel_x86` → `accel`, so ARM
   adopts it with no further plumbing. aarch64 compile-verified; execution gate
   = the in-tree fused-hpel A/B test on first ARM run (the NEON campaign's
   precedent). A `#[path]`-attribute trap cost one build (the dangling
   `#[path = "hpel.rs"]` latched onto `mod mectx`) — caught by the compiler.
2. **X1 SCOPE CORRECTED:** `intra_pred` was misfiled — it is portable with a
   scalar ARM fallback already (only its NEON arms are missing). The true
   x86-only residue is `satd_avg` (602) + `mectx` (167) ≈ **770 lines, not
   1,800** — and mectx consumes satd_avg, so it is ONE port campaign.
3. **E7's real half: `skip_chroma_is_free` row-sliced** — the luma twin's
   proven shape transplanted (the deep dig's 23-bc find).
4. **D1 CLOSED-BY-READ:** `add_inter_residual` is already AVX2-vectorised
   (50 ymm, 0 bc) — §3.3's `paddw+packuswb` worry: the compiler did it.
5. **E27 CLOSED-AS-ANATOMY:** `b2_mgain` = 216 scalar instructions per FRAME.
6. **E21 read on today's tree:** still scalar (2,229 instr / 421 xmm / 11 bc) —
   but `mbtree.rs` is the concurrent session's active campaign file; the
   finding is handed over here rather than edited under them.
7. **E26 / D26 / D17+D19 CLOSED-AS-INLINED:** no standalone symbols exist —
   their loops live inside parents that carry 0 bc (decoder) / healthy
   vectorisation; D17+D19's pair-measurement stays with the anatomy harness.
8. **X4 CLOSED as the deliberate census it asked to be** — per-fn attribution:
   encoder = `FrameEncoder::new` 25 (the missing encoder GridPool, structural)
   + CABAC-P driver 21 (mode-decision Vecs, SmallVec-shaped); decoder =
   EDC ctx/job machinery (partially pooled by design) + `as_reference_pooled`
   13 (pooled, measured); inter/deblock = almost entirely TEST code. The
   incidental-discovery era of this item is over; the residue has names.
9. **E6/E7's vectorize-question parked with receipts** — the deterministic half
   (the row-slice) is delivered; the wide-load question still needs the
   exit-distribution harvest and is recorded as such.
10. **The moving-tree A/B instrument, second live fire:** this round's
    five-config gate went red on the concurrent session's 15:30 landings;
    the AST-inverse A/B (hpel via its HEAD copy + reps) proved my round
    **byte-neutral on all five configs**, and baselines were re-frozen.
    E11's structural half remains the one untouched listed item — a real
    encoder-architecture brick, not a win to shave.

### 11.16 E28 CUT, B-SWEEP COMPLETED, E1 OPENED, E11 VERDICT — 2026-08-26

**E28 (`emit_intra_body_cabac`): bc 50 → 37.** The emit loops must run in scan
order for the CABAC contexts, so the per-block `fe.nnz_y[by*w4+bx]` scatters
became a raster-local `[u8; 16]` collected in-loop and committed as four row
copies (`nnz_commit_rows`, shared by all three arms — I16 AC, I8, I4, plus the
two cbp==0 zero arms). Instr +90 (the helper inlines thrice) — checks bought
with a little size, recorded honestly.

**The B-sweep's last member — and it serves P too:** the emit helpers re-read
`bitacct::enabled()` per syntax element. Now hoisted once per call in the
multi-read helpers: `cb_mvd` (3 → 1, and it runs PER MVD COMPONENT), `cb_ueg_mv`'s
escape arm, and both skip emitters (2 → 1 each). The single-read helpers were
already minimal.

**E1 opened (`plan_inter_mb`): bc 79 → 58 in the first pass.** Three
motion-commit nests whose values are CONSTANT per rect became row `fill`s
across all six grids (whole-MB B commit, P partition rect, B partition rect),
and the scalar-arm residual gather got the row-slice transplant. The remaining
clusters (the chroma recon region ~4333–4504 and the sub-partition arm) are
read-mapped and listed for the E1 round 2.

**E11's structural half — the honest verdict: PARKED AS ITS OWN CAMPAIGN.**
The trial flow's copy pair (`save_mb` → plan → score → `load_mb`) can only be
removed by planning into a scratch TARGET, and `plan_inter_mb`'s write surface
is the whole MbState set: three recon planes + six motion grids + nnz caches.
Threading a target through every recon/commit callee is a real architecture
brick — not a win to shave off a hammer pass. The cheap halves are already
banked (`load_mb` −35.6%, `save_mb_into` REFUTED +72%).

**Gates:** five-config byte-identity FIRST RUN (the tree held still); the only
red suite remains the concurrent session's two in-flight mbtree tests.

**"NEON everywhere" status, restated:** after §11.15's hpel port, the entire
remaining NEON gap is `satd_avg` + `mectx` (≈770 lines, one campaign — mectx
consumes satd_avg). Everything else the codec ships is portable.

### 11.16a E1 ROUND 2 + E11 STRUCTURAL HALF EXECUTED — 2026-08-26

**E1 completed: bc 79 → 51 (−35%), instr 6,360 → 6,312.** Round 2 cut the
mapped clusters: the t8 recon commit's per-pixel 8×8 nests → row copies (both
arms — identical twins, one n=2 rep), the scalar chroma residual gather →
row slices, and the cold-arm 4×4 recon → the fused `reconstruct_4x4_into`
(plan_i4x4's transplant, third instance). The residue (51) is the MC-pred
interior and per-partition glue — kernel-adjacent, not scatter shapes.

**E11's structural half is EXECUTED, not parked.** The insight that made it
tractable without the scratch-target refactor: **in the multi-trial RD sites,
every candidate is a full `plan_inter_mb` whose write-set completely overwrites
the previous candidate's** — recon planes and all four P-side motion grids are
rewritten on every mode path (the §11.16 row-fill commits are the coverage
proof), while everything else MbState guards (`nnz`, `modes_y`, `cur_qp`)
moves only at EMIT. Therefore the per-candidate restores were pure waste:

- **shape-RD re-rank loop: N restores → 1** (after the loop, before the intra
  trial, which needs the pre-trial state exactly once).
- **sub8-RD pair: the middle restore removed** (plan B fully overwrites plan A).

**The gate that makes this trustworthy:** shape-RD and sub8-RD are knob-gated,
so a plain config gate would never execute the changed lines
(gate-must-prove-the-tool-ran). A **forced-RD baseline**
(`RFF_SHAPE_RD=1 RFF_SUB8X8_SPLIT=1 RFF_SUB8_RD=1`) was frozen on the
pre-change binary — its 7,645 bytes differ from the default config's,
proving the knobs engaged — and the post-change encode is **byte-identical**
on that arm. Five default configs also byte-identical; the only red suite
remains the concurrent session's two mbtree tests.

The FULL scratch-target refactor (planning with no commit at all) remains a
real further campaign — but the copy-pair waste the E11 item named is now
removed from both multi-trial sites.

### 11.17 THE E11 DEEP DIG — what was still hiding in the trial machinery

Three finds the earlier E11 work walked past, all now landed:

1. **`save_mb()` heap-allocated TEN Vecs per RD trial.** It builds
   `MbState::default()` — ten empty Vecs — and `save_mb_into`'s pushes then
   allocate them, every trial, every macroblock. The snapshot buffers are now
   TLS-recycled through TWO slots: **A** for the driver sites (sub8-RD,
   shape-RD, the intra/inter `j_inter` trial — their lifetimes are strictly
   sequential) and **B** for `trial_intra`, which NESTS inside a shape-RD trial
   that still holds A. After the first MB per thread, every snapshot is a pure
   copy into retained capacity.
2. **The CAVLC RD-skip coded-arm writer was a fresh heap Vec per trial** —
   the one trial writer the E2-round pooling missed, because it isn't dropped
   at trial end: it is KEPT (`coded = Some(scratch)`) and spliced into the
   slice on the coded path. Pooled via the same bits slot, recycled at BOTH
   consumers (the splice, and the `won` branch that discards it).
3. **Verified already-smart, recorded so nobody "fixes" it:** the CAVLC arm's
   `rdskip_snap` is per-slice and reused via `save_mb_into` (the shape this dig
   now spreads everywhere), the trial-and-keep splice already avoids the
   double encode, and the `save_mb` at its `debug_assert_eq!` is
   release-compiled-out — three things that LOOK like the defect and are its
   fix.

**Gates:** five default configs + the forced-RD arm all byte-identical; the
RD-skip splice arm (`RFF_RDSKIP_T=1.0`, CAVLC quality) exercised and stable;
the recycling is value-invisible by construction (`take` + `clear` ≡ `new`).
The only red suite remains the concurrent session's two mbtree tests.

With this, E11's ledger reads: `load_mb` row-sliced (−35.6%), `save_mb_into`
append-shape refuted, restore-elision at both multi-trial sites, snapshot and
trial-writer allocations pooled. What remains is only the FULL scratch-target
refactor (plan with zero commit) — a compression-neutral architecture campaign
whose payoff is now bounded by what this section already removed.

### 11.18 THE TREE GOES GREEN — cross-session repairs (2026-08-26)

The two encoder tests the concurrent mbtree/weightp campaign left red are
fixed IN THAT CAMPAIGN'S OWN INTENT (diagnosed from its code and comments, not
guessed):

- `pps_roundtrips_through_reader`: the campaign added `cfg.weightp`
  (x264-parity explicit P weighted prediction, **default ON**) and the PPS now
  signals `weighted_pred_flag` from it. The test pins axes it isn't testing —
  it now pins `weightp = false` too, in its own established comment style.
- `encodes_access_unit_with_sps_pps_idr`: `mbtree` now defaults ON, so the
  streaming path buffers one GOP and a single frame's AU arrives on `flush()`
  — the same encode+flush shape the fuzz seed builder already uses. The test
  follows the real default pipeline now.
- The `mf` leftover binding in `skip_luma_is_free` (their table refactor's
  orphan) and the `w4` orphan in `emit_intra_body_cabac` (MY §11.16 refactor's
  own) are both swept.

**State of the tree: every suite in all three codec crates green, all five
config byte-gates + the forced-RD arm green, ZERO code warnings.** The only
remaining warnings are the pre-existing Cargo.toml `default-features is
ignored for workspace dependencies` nits — a real latent issue (per-crate
`default-features = false` on workspace deps is being IGNORED, which will
someday be a hard error) but a feature-resolution semantics change, flagged
for the owner rather than changed unilaterally.

**This is the commit checkpoint this document has been recommending: both
sessions' full day of work is uncommitted in one tree, and the tree is,
for the first time today, entirely green.**

### 11.7 Honest scope of this pass

The **ymm/xmm assembly census was NOT re-taken** — the tree is mid-campaign with
uncommitted work, so §0's ratios remain the 2026-08-22 snapshot and remain a
proxy (§9 unchanged). Loop counts drifted exactly as the working tree predicts:
`encoder/mb16.rs` 146 → **140** (row-slice rewrites), `decoder/mb16.rs` 78 → **81**
(campaign additions); the other four hot files are unchanged to the digit. No item
in this document has yet been *measured* except where §11.4 says the sister
campaign gated it.

### 11.19 THE 20-SITE REACHABILITY SWEEP — 2026-08-27, all sites dispositioned

Prompted by "we deployed AVX2/SIMD and did not see the win": a fresh audit of
every place the decoder's high-performance tools could fail to be called,
then a fix or an evidence-backed closure for each. Sites grouped by class;
**bold** = code landed today.

**A. Build gating (sites 1–4)**

1. **`asm` is now a DEFAULT feature on all three codec crates** (the X2 root
   fix). A plain `cargo build/test/bench -p rusty_h264-decoder` now measures
   the shipping SIMD arm; the scalar oracle arm is explicit:
   `--no-default-features`. All bench scripts already passed `--features asm`
   (verified) — nothing breaks; the `dectest` scalar probe invocation is
   updated in big-oppy-decoder.md §1a. NOTE: every encoder-crate A/B example
   (`refs_ab`, `parity_ab`, `interplan_bench`, …) now builds the ACCEL arm by
   default — historical numbers recorded from those harnesses were scalar-arm.
2. **§11.18's "default-features is ignored for workspace dependencies" latent
   nit is FIXED, and the flip made it mandatory**: `[workspace.dependencies]`
   now declares the three codec crates `default-features = false`. Without
   that, Cargo ignores members' `default-features = false`, and the new asm
   default would have leaked into every `--no-default-features` build — the
   scalar arm would be dead while the "pure" CI job claimed to test it.
   Verified by `cargo tree -e features`: scalar arm has zero accel edges,
   default arm has them, warning gone.
3. **Runtime arm proof now exists**: `rusty_h264_common::arms::simd_arms()`
   (one line naming the compiled+detected arm) and `active_knobs()` (every
   live `RS_H264_*`/`RFF_*` var with its effect class). Wired into the CLI
   (stderr banner), `bench/src/main.rs` and `decode_bench` (stdout, so pasted
   result tables carry their arm). The knob scan is by PREFIX with a
   classification table — deliberately no second parse of any knob's polarity
   (instrument-fork law).
4. **The decoder's phantom accel edge is removed**: it held a direct
   `rusty_h264-accel` optional dep + a cfg-emitting build.rs with ZERO
   `rusty_h264_accel::` / `cfg(accel)` references anywhere in the crate —
   every kernel the decoder reaches goes through common's wrappers. build.rs
   deleted; `asm = ["rusty_h264-common/asm"]`.

**B. Runtime knobs (sites 5–9)**

5–7, 9. The ~30-knob inventory is surfaced by `active_knobs()` (site 3's
   banner); `RFF_ABL_*` are flagged "OUTPUT IS WRONG while set". The knobs
   themselves are load-bearing oracle arms (H10, QPEL_COMPOSE A/B) and stay.
8. **`RS_H264_DOUBLE_RECON` polarity bug FIXED**: it triggered on `is_some()`,
   so `RS_H264_DOUBLE_RECON=0` DOUBLED the recon work — the one knob whose
   "off" spelling turned it on. Now `== "1"`. (Polarity of the other 29
   checked one by one: all correct.)

**C. Kernels the decoder path never reached (sites 10–13) — all CLOSED BY
EVIDENCE, none wired, because the evidence says wiring loses:**

10. AVX2 dequant stays opt-in (measured null, §"dequantize" — and the
    post-LTO read shows `dequantize` at 28–43 ymm from v3 AUTO-vec anyway:
    the workspace binary already runs vector dequant without the kernel).
11./12. accel `i16x16_luma_pred`/`chroma8x8_pred` stay encoder-only, three
    reasons, each sufficient: (a) they are deliberately PLAIN RUST (the module
    header: "memory-shaped work LLVM vectorises well") — there is no SIMD
    being missed; (b) their contract reads neighbours from the rec PLANE, and
    the decoder must source top rows from `bak_y` (pre-deblock backups) under
    default row-deblock — plane reads would read FILTERED pixels = recon
    drift, the exact 0.11.0 bug class; (c) post-LTO census: the decoder's
    scalar `luma16x16_pred` is 220-ymm / `chroma8x8_pred` 59-ymm — already
    vector code in the shipping binary.
13. accel `idct_four_t4_rec` stays encoder-only: post-LTO, the decoder's
    residual family is already vectorised (`add_inter_residual` 58–116 ymm,
    `add_residual_8x8` 26 ymm); `reconstruct_4x4_into` is scalar-ish (0 ymm)
    but that is §7 closed item 2 (SIMD DCT batching measured ~3% SLOWER) —
    the DC-collapse already removed most of its population.

**D. Shape gates (sites 14–17) — census says the gates are right:**

14./15. The MC size×phase census (decode_bench `--features asm,profile`) on
    the x264 HIGH tier (the sub-8x8-emitting streams): sub-8x8 sizes
    (8x4/4x8 + 4x4, all phases) = **2.0% of MC cycles on shields, 0.9% on
    stockholm**. The `bw ∈ {16, 8}` SIMD gates and the chroma w2 fallback are
    losing ~1–2% of MC ≈ well under 0.5% of decode. §7 closed item 7
    re-confirmed with fresh numbers on the heaviest content; no w4 path.
16. The decoder's ~16 hand-rolled `(a+b+1)>>1` loops **ARE `vpavgb` in the
    shipping binary** — but ONLY post-LTO. ⚠ CENSUS TRAP, recorded so the
    next audit does not re-trip: the per-crate rlib `.s` under `lto = "thin"`
    shows ZERO vpavgb and near-zero vector arithmetic — pre-LTO codegen is
    NOT the shipping code. Read the asm of the FINAL BINARY
    (`cargo rustc --example decode_bench -- --emit asm`): decoder CGU has 32
    vpavgb (12 in `bz_flush_slow` = the span bands, 4+4 in the `b_mc` twins,
    8+4 in `b_mc_chroma`). The in-tree "compiler already emits the ideal
    instruction" comment is validated; no avg kernel needed.
17. `weight_partition` (scalar per-sample weighting) — no kernel exists;
    filed as a BACKLOG candidate gated on a census of weighted-partition
    share on fade content (the identity-skip already removes the x264
    weightp=2 no-op passes; `edcstat::WP_SKIPPED` counts them).

**E. Arch coverage (sites 18–19)**

18. **SSE2 twins landed for `mb_uniform` + `bs_motion_masks`** (x86_asm.rs):
    the dispatchers never return `None` on x86-64 now — before, ANY
    environment without AVX2 (hypervisor masking it, pre-Haswell) silently
    dropped the packed-bS arm (+3–7%) and the kind-routing stacked on it.
    Gate: 50k-round AVX2==SSE2 differentials (incl. i16::MIN abs-wrap edge)
    + the existing common scalar==dispatcher tests ⇒ scalar==SSE2
    transitively. `bs_motion_masks_two_list` stays AVX2-only (3.1% of MBs;
    dispatcher note says when that changes). Dispatchers now use the cached
    `has_avx2()` (X5).
19. **CI now EXECUTES NEON**: new `aarch64-run` job on `ubuntu-24.04-arm`
    runs the accel differential suite + common-with-asm + the decoder
    round-trip on real ARM hardware every push — the execution gate the NEON
    campaign left at "first ARM build".

**F. Expectation (site 20)**

20. With every site above dispositioned, the standing conclusion holds: the
    SIMD deployment IS reachable on the default x86-64 path, and the residual
    1.4–1.5x vs ffmpeg is the entropy layer (§1 of big-oppy-decoder.md), not
    unwired kernels. The instrument that distinguishes "not wired" from
    "wired but not the bottleneck" is now permanent: the arm banner + knob
    audit + the MC census, all in the default drivers.
