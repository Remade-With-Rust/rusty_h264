# openh264 transform/quant — every encoder primitive, mapped to ours

> **RESULT (measured, bench gradient clip, 352×288 gop12 qp26, median-of-9):**
> pre-transform **133.7ms** → +flatten **132.6ms** → +forward/inverse batching **136.5ms**.
> **Only the flatten helped.** The DCT batching (gather residuals → SIMD `forward_dct_blocks`/
> `inverse_dct_blocks` → scatter) is a **~3% regression** on the portable SSE2 build:
> rustc already auto-vectorizes the scalar `forward_core`/`inverse_core`, so the explicit
> per-block lane gather/scatter is pure overhead (AVX2 `target-cpu=native` barely closed it).
> openh264's `WelsDctMb`/`IDctFourT4` batching only pays off with its hand-written AVX2.
> **Kept** the flatten + the `inverse_dct_blocks` primitive (tested, for a future runtime AVX2
> dispatch); **reverted** the hot-path batching. Transform+quant is NOT the bottleneck on
> compressible clips — the scalar path is already near-optimal. Next lever: re-profile (ME/MC).


Same playbook as CAVLC (`docs/openh264-cavlc.md`): map each openh264 primitive to
ours, find the inefficiency (the CAVLC lesson: **allocations, redundant passes, and
per-element lookups that can be flattened** — not bit-twiddling), go one at a time,
each byte-identical + verified. Transform+quant is now **39% of the encode** (the
biggest slice after CAVLC). Sources: `encode_mb_aux.cpp`, `decode_mb_aux.cpp`,
`svc_encode_mb.cpp` (the `WelsEncRecMb` flow).

## The whole-MB encode→reconstruct pipeline (openh264 `WelsEncRecMb`)

openh264 keeps the macroblock's coefficients in **one contiguous `int16 pRes[256]`**
and runs **batched** kernels over it (4 blocks per call), in place:

```
WelsDctMb (pRes, pEncMb, stride, pBestPred)        # fwd DCT all 16 blocks -> pRes
# --- I16x16 only: ---
pfTransformHadamard4x4Dc (aDctT4Dc, pRes)          # gather the 16 DCs, Hadamard them
pfQuantizationDc4x4 (aDctT4Dc, FF<<1, MF>>1)       # quant the DC block
pfScan4x4 / pfGetNoneZeroCount                     # scan + count the DC
# --- AC (all luma types): ---
pfQuantizationFour4x4 (pRes, pFF, pMF)   x4        # quant 4 blocks/call, IN PLACE
pfScan4x4Ac (pBlock, pRes)               x4        # scan 4 blocks/call
# --- decimation (CalculateSingleCtr4x4) ---
# --- reconstruct: ---
pfDequantizationFour4x4 (pRes, dequant) x4         # dequant 4 blocks/call, IN PLACE
pfIDctFourT4 (pPred, stride, pBestPred, 16, pRes) x4  # inverse DCT + add pred + clip
```

**Ours** (`encode_inter_mb`/`encode_mb`) uses separate `[[i32;16];16]` stack arrays
(`res_blocks` → `coeffs` → `q_blocks`) and **per-block scalar calls**. No heap allocs
(those are stack — unlike CAVLC), so the levers here are *per-element lookups*,
*redundant passes*, and *scalar-vs-batched*.

## Primitive-by-primitive map

| # | openh264 primitive | our equivalent | status / opportunity |
|---|---|---|---|
| 1 | `WelsDctT4` / `WelsDctFourT4` (fwd DCT 1/4 blocks) | `forward_core` / `forward_dct_blocks` | asm-wired behind `feature=asm` (bit-exact); scalar default is `forward_core`/`wide` |
| 2 | `WelsDctMb` (whole-MB fwd DCT, 4 quadrants) | gather `res_blocks` + `forward_dct_blocks` | **gather/scatter pass** — could fuse residual+DCT (the asm path already does) |
| 3 | `WelsQuant4x4`/`WelsQuantFour4x4` (quant) | `quantize` | **per-coeff `pos_group(i,j)` match + `QUANT_MF[m][group]` lookup → flatten to a 16-entry per-`m` MF table** (the cbp-inverse-table trick) |
| 4 | `WelsQuant4x4Dc` (quant the i16 luma DC) | `forward_quant_luma_dc` | same flatten; also the `f`/`mf` recompute |
| 5 | `WelsHadamardT4Dc` (luma DC Hadamard) | `hadamard_4x4` | scalar 1-D butterflies; `forward_quant_luma_dc` does Hadamard **then** quant |
| 6 | `WelsHadamardQuant2x2`/`Skip` (chroma DC) | `forward_quant_chroma_dc` / `hadamard_2x2` | scalar |
| 7 | `WelsScan4x4Ac`/`DcAc`/`Dc` (zigzag) | `scan_4x4_ac` / `scan_4x4_dcac` | ✅ DONE (CAVLC pass — unrolled) |
| 8 | `WelsGetNoneZeroCount` (nnz) | `encode_residual_block` return / `.filter().count()` | ✅ DONE (CAVLC — returns total_coeff) |
| 9 | `WelsCalculateSingleCtr4x4` (decimate score) | reverted (`decimate_score16` exists, unused) | gated; off by default |
| 10 | `WelsDequant4x4`/`WelsDequantFour4x4` | `dequantize` | **per-coeff `pos_group` + branch on `qp>=24` → flatten to a 16-entry per-`qp` LevelScale table** |
| 11 | `WelsDequantIHadamard4x4` (dequant+inv Hadamard DC) | `inverse_quant_luma_dc` | Hadamard + dequant |
| 12 | `WelsIDctT4Rec`/`WelsIDctFourT4Rec` (inv DCT + recon) | `inverse_core` / `reconstruct_4x4` | asm-wired behind `feature=asm`; scalar default |
| 13 | `WelsIDctRecI16x16Dc` (recon with DC) | the i16 reconstruction loop | per-block dequant+inverse |

## Ranked opportunities (highest-leverage first, by the CAVLC lesson)

1. **`quantize` — flatten the per-coeff lookup.** Every coefficient recomputes
   `pos_group(i,j)` (a `match`) then indexes `QUANT_MF[m][group]`. Precompute a flat
   `[i32;16]` MF table per `m` (`qp%6`) → one array read per coeff, no `match`. This
   is the most-called transform primitive (16 luma + 8 chroma blocks/MB). Byte-identical.
2. **`dequantize` — same flatten** (`pos_group` + the `qp>=24` branch → a 16-entry
   per-`qp` LevelScale table; the rounding handled per-table). Per-block, on the
   reconstruct path. Byte-identical.
3. **`skip_is_free` luma transform-test** — still does a full `forward_core`+`quantize`
   per block to test skip (the chroma half was deferred already). Mirror openh264's
   cheaper probe, or fuse.
4. **Whole-MB flow** — our `res_blocks`→`coeffs`→`q_blocks` separate arrays vs
   openh264's one in-place `pRes`. Possible copy reduction (fuse gather+DCT+quant).
5. **i16/chroma DC Hadamard** (`forward_quant_luma_dc`, `forward_quant_chroma_dc`) —
   scalar butterflies; lower frequency (1 DC block/MB).

## Order
Start with **#1 `quantize` flatten** (most-called, clearest, byte-identical), then
**#2 `dequantize` flatten**, then re-profile to see if transform+quant shrank before
the harder ones (skip-test, whole-MB fusion). Each: byte-identical, `cargo test` +
`cmp` vs HEAD + re-bench.
