---
name: asm-campaign-verdict
description: openh264 asm is ~2x faster than our real vectorized kernels (verified); the wiring campaign + its gating constraints
metadata:
  type: project
---

**Verdict (kernel_bench, aligned 16×16): openh264's SSE2 asm is ~2× faster than our
REAL vectorized kernels** — not 1× (our scalar isn't as good as hand-tuned asm) and not
10–36× (that's only vs *naive* scalar, the misleading trap the test kit exists to avoid,
same lesson as the DCT batching):
- SAD16×16: ours 88 M/s (`psadbw` auto-vec via `u8::abs_diff`), asm 210 M/s → **2.4×**
- SATD16×16: ours 11 M/s (`wide` SIMD `satd_4x4_sum`), asm 22 M/s → **2.0×**

This IS the openh264 gap mechanism (2.1× inter): openh264 *is* these asm kernels. So
wiring them is worth ~2× **per kernel** — but end-to-end is diluted: kernels are ~half the
encode (rest is CAVLC, control flow, memory), and the existing DCT/IDCT asm (already wired
for inter luma) moved the whole encode only +3–6%. **Realistic end-to-end ≈ 1.3–1.5×**
(inter ~50 → ~65 Mpx/s, closing toward openh264's ~102), not a 2× flip.

**Foundation built (committed):** accel/build.rs assembles all 23 openh264 asm files;
FFI + byte-exact tests for SAD (16x16/16x8/8x16), SATD (8x8/16x8/8x16/16x16), DCT, IDCT;
`kernel_bench` example is the per-kernel scalar-vs-asm test kit. Run:
`cargo run --release -p rusty_h264-accel --example kernel_bench`.

**Gating constraint for wiring the spatial kernels:** the SSE2 kernels use aligned
(`movdqa`) loads → 16-byte-aligned input, 16-multiple stride. The encoder stays
`forbid(unsafe)` even with `--features asm`, so the aligned buffer must be a safe
`#[repr(align(16))] struct A([u8;256])` (the SOURCE MB copied once per MB ME; the
reference is `movdqu`-tolerant — TODO verify op2 unaligned). Licensing: x264 asm is
**GPL (off-limits)**; openh264 asm is **BSD-2 (ours to use)** — see [[openh264-baseline-build]].

**How to apply / order:** wire in impact order, re-run kernel_bench + the 3-codec
speedtest after each. SAD/SATD (ME) → quant → deblock (needs common-crate plumbing) →
MC sub-pel → intra-pred. Only keep an asm wire if kernel_bench AND the end-to-end
speedtest both improve (don't trust the naive-scalar ratio).

**WIRED so far (end-to-end, headline = fast preset):**
- **SAD** (ME): +9% inter (50→58), byte-identical, gap 2.1×→1.9×. Per-MB aligned source
  copy; reference stays movdqu.
- **quant** (inter-luma AC): byte-IDENTICAL + RD-neutral but HEADLINE-MARGINAL (58→57,
  noise). Key: adopt openh264's quant *structure* (`((|c|+FF)·MF_oh)>>16`, the pmulhuw
  high-word) but keep OUR deadzone via `FF=round(F/MF)` (`quant_dz_ff`); openh264's own
  FF tables regress intra −1.5 dB. Chain DCT→quant in i16 (no i32 round-trip), 16-aligned
  `AlignedDct`; the `quant_four_4x4` wrapper aligns FF/MF internally. **Lesson: quant is a
  small slice — transform+quant is NOT the bottleneck (re-confirmed).**

**Byte-exactness classes:** spec-defined = byte-exact pure speedups (SAD✅, DCT✅, deblock,
MC, intra_pred); encoder-choice = output-changing (quant — made RD-neutral via our-deadzone
trick; SATD — different cost scale, quality-preset only).

**THE WALL (deblock/MC/intra all hit it):** openh264's spatial asm needs the **plane
16-byte aligned** (it loads aligned row chunks → segfault on our plain-`Vec<u8>`
reconstruction/reference planes). SAD/quant got around it cheaply (SAD: per-MB aligned
source *copy*; quant: the i16 DCT buffer is already an `AlignedDct`), but deblock/MC/intra
operate on the plane **in place**, so a copy doesn't help — they need the actual
reconstruction+reference buffers 16-aligned. In safe Rust a `Vec<u8>` is only align-1; the
fix is an aligned plane type (e.g. `Vec<u128>` backing + `bytemuck::cast_slice_mut`, or an
aligned-alloc wrapper in the not-forbid-unsafe accel crate) threaded through `FrameEncoder`
+ `RefFrame` + the decoder. **This aligned-plane infra is the single unblocker for the
last 3 spatial kernels.** deblock plumbing + FFI are committed and ready; the wiring is
reverted to scalar (byte-identical) pending the infra. SATD is the only remaining kernel
wireable without it (SAD-style aligned source copy) but it's quality-preset-only +
output-changing (different cost scale).

**Bottom line:** the headline win (SAD +9%) is banked; quant is byte-identical/RD-neutral
but marginal; the rest hinge on the aligned-plane infra (a focused but real change).

**UNBLOCKED + DONE: aligned-plane infra + horizontal-luma deblock.**
- `AlignedBytes` (common/src/aligned.rs): `Vec<u128>` backing + `bytemuck` cast → 16-aligned
  `[u8]`, Derefs to `[u8]`, no unsafe. `FrameEncoder` rec_y/u/v + `RefFrame` now use it.
  Transparent (60 tests green, byte-identical). **This unblocks MC + intra too.**
- **Deblock convention gotcha:** openh264's `DeblockLumaLt4V`/`Eq4V` "V" = the filter
  *direction* is VERTICAL (`p0 = pPix[-stride]`) → it filters a **HORIZONTAL** edge, 16
  columns, `tc[i]` per 4-col segment, `pPix = p3 + 4·stride`. (Wiring it to *vertical*
  edges read row −1 → segfault — the first failure.) Vertical luma edges need the
  transpose path (`DeblockLumaTransposeH2V` → V filter → `V2H`).
- **DEBLOCK COMPLETE on asm (all byte-identical, cmp-clean):** H-luma (`DeblockLumaLt4V/Eq4V`),
  chroma (`DeblockChroma{Lt4,Eq4}{V,H}`, Cb+Cr together, tc = spec `tc0+1`), and V-luma via
  the transpose path (replicated openh264's `DeblockLumaLt4H` C wrapper:
  `TransposeH2V → Lt4V → TransposeV2H` over a 16-aligned 128-byte buf). **Inter gap
  openh264 2.1×→~1.7–1.8× (≈63–65 Mpx/s).** Key convention gotcha: openh264's `*V` filters
  are *vertically-directed* → for HORIZONTAL edges; `*H` (chroma direct, luma via transpose)
  for vertical edges. tc slice offset is `p3+4·stride` (H-luma), `p1+2`/`p1+2·stride` (chroma).

**Remaining 3 kernels (now all unblocked by the infra, but NOT fast-headline movers):**
- **MC** sub-pel (`McHorVer*`): quality preset only (fast = full-pel copy); 16 sub-position
  funcs. Byte-exact.
- **intra_pred** (`WelsI16x16/I4x4/IChromaPred*`): helps intra frames (3.6× gap); many modes.
  Byte-exact. Likely marginal end-to-end (prediction is cheap vs transform/cavlc).
- **SATD** (`WelsSampleSatd16x16`): quality ME + intra mode cost; **output-changing** (our
  `satd_4x4_sum` sums raw `Σ|H·d|`, openh264 sums `(Σ+1)>>1` per block — a cost-scale change
  that shifts the satd-vs-λ balance → needs RD/PSNR gating, like the quant deadzone).
The fast-inter headline levers (SAD, quant, deblock) are all banked; these three move
quality/intra.

**SATD asm REVERTED — net LOSS (the FFI-overhead trap, measured).** Aligned our satd scale
to openh264's `(Σ|H·d|+1)>>1` per block (RD-neutral: byte-identical size+PSNR at QP22/28 on
a 320×240 motion clip — the satd argmins are scale-invariant), wired `WelsSampleSatd*` into
`mc_satd` + `satd_16x16/8x8/4x4`, asm==scalar byte-identical. BUT inter regressed 1.7×→2.0×
(≈63→51 Mpx/s, confirmed over 3 runs). **Cause: the satd calls are too frequent and small —
mc_satd per half-pel candidate, i4×4 satd_4x4 is 144/MB — so the per-call FFI overhead
exceeds the 2× kernel speedup.** This is the dilution trap turned net-negative. `git reset`
back to the chroma-intra HEAD. **Lesson: only wire asm where the call is coarse (whole-MB
spatial kernels like deblock/MC-block), NOT for tiny hot-loop kernels (4×4 satd) where FFI
overhead dominates.**

**i4x4 intra-pred: NO x86 asm in openh264 (C-only) — cannot be wired.** chroma intra: only
V+Plane have sse2 (DC/H are C-only); wired those (byte-identical, marginal). MC center
(`McHorVer22`) DONE byte-exact (the HorFirst asm internally steps up 2 rows). **Final
campaign state: SAD, quant, DCT, full deblock, i16 intra, full MC half-pel, chroma intra
V+Plane — all byte-identical and kept. SATD wire-able + RD-neutral but reverted for the perf
regression. The fast-inter gap stands at ~1.7×, deblock the one real post-SAD lever.**

**i16 intra-pred WIRED (byte-identical) but MARGINAL** (all-intra 24→25, noise): FFI'd
`WelsI16x16LumaPred{V,H,Dc,Plane}_sse2`, `i16_pred()` dispatches asm for interior MBs
(both neighbors avail), scalar for edges (openh264's DC availability variants are C-only).
**Confirms the prediction is diluted by transform+CAVLC** — same lesson as quant. i4x4
(9 modes, 144/MB) is the bulk of intra-pred but byte-exact-but-likely-still-marginal.

**MC investigation (the last fast-headline-relevant kernel — fast preset now does half-pel
ME):** SSE2 coverage: H-half `McHorVer20WidthEq16/8` (direct), V-half `McHorVer02WidthEq8`
(width-16 = 2× width-8, no sse2 02WidthEq16), center `McHorVer22Width8{HorFirst,VerLast}`
(a 2-stage hor-first→tap-buffer→ver-last, the complex one). Our `luma_h/luma_v/luma_centre`
(inter.rs) map to these; `pSrc = &tile[(2+dr)·ts+2+dc]`, movdqu source (tile unaligned OK)
but **pDst likely movdqa → the out buffer needs aligning (cascades into mc_satd's caller)**.
Half-pel ME interpolation is a fraction of the inter encode (modest payoff), and the center
(used by 4 of 8 half-pel candidates) is genuinely complex.

**Campaign status: the high-value, tractable, byte-safe kernels are ALL wired (SAD, quant,
DCT, full deblock, i16 intra, MC half-pel H+V — all byte-identical). The fast-inter gap
(1.7×) is now dominated by control-flow/CAVLC, not asm-able kernels.**

**MC half-pel H+V WIRED (byte-identical) but MEASURED-NEUTRAL** (inter within run-noise
61–66): `McHorVer20WidthEq16/8` + `McHorVer02WidthEq8` via `mc_hor20`/`mc_ver02` (internal
16-aligned scratch), wired into `luma_h`/`luma_v`. Center `McHorVer22` (2-stage, 4 of 8
half-pel candidates) still scalar.

**THE KEY MEASURED FINDING (the whole campaign's verdict): only SAD (+9%) and the full
deblock moved the headline. quant, i16 intra, and MC half-pel are each byte-identical but
within run-noise — because post-deblock the encode is CAVLC / control-flow / mode-decision
bound, NOT kernel-bound.** The remaining asm work (MC center, i4x4/chroma intra, SATD) is
byte-exact-able but will be similarly headline-neutral. **To close the last 1.7× to openh264,
the lever is ALGORITHMIC (CAVLC throughput, the encode/mode-decision loop structure), not
more asm kernels.** That is the honest next direction. Run order proved the kernel_bench
2×-per-kernel ratio is real but end-to-end dilution is severe once the big spatial kernel
(deblock) is done.
