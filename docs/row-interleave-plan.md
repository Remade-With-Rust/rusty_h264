# Derive-bS-at-decode + row-interleaved deblocking — campaign plan

*Explored 2026-08-05 (WHYS Parts 15-18 named this the root-cause lever for the
deblock stage's ~10% of non-kernel cost). Status: Stage R1 landed; R2-R3
designed with ordering proofs and site inventory below; not yet built.*

## Why this is the shape of the fix

The decoder runs TWO frame passes: decode writes pixels + syntax grids, then
`fd.deblock()` re-walks everything cold — the grids to derive boundary
strengths, the pixel planes (~1.4 MB/frame) to filter. x264/ffmpeg run ONE
pass: bS derives inside the decode MB loop while the syntax is in registers,
and filtering trails decode by one MB row while the pixels are still in L2.
The fused rolling-window precompute (Stage R1, landed) fixed the GRID half;
the PIXEL half — filtering against warm rows — is what remains.

## Ordering proofs (what makes an interleave byte-identical)

1. **Filter order.** Spec §8.7 filters per macroblock in raster order
   (vertical edges left→right, then horizontal top→bottom). `filter_frame`
   already does exactly this. Filtering MB row `r` immediately after row `r`
   finishes decoding preserves that order EXACTLY — every MB it touches
   (raster-prior) is decoded, and the bottom-adjacent edges belong to row
   `r+1`'s MBs, which filter later. Byte-identical by construction.
2. **Motion compensation** reads REFERENCE pictures only — never the current
   reconstruction. Unconstrained by the interleave.
3. **Intra prediction is the one hazard.** Spec §8.3: intra reads
   reconstructed samples PRIOR to deblocking. Row `r+1`'s intra reads exactly
   ONE pixel row of row `r` (the bottom luma row, + chroma equivalents, + the
   corner sample), while filtering row `r` modifies its bottom THREE rows
   (p0/p1/p2 of row r+1's top edge — which is filtered later, but row r's own
   internal edge 3 writes rows 12-14, and row r+1's top-edge filtering writes
   13-15). Therefore: before filtering row `r`, SAVE its unfiltered bottom row
   (1 luma row = cw bytes + 2 half-width chroma rows ≈ 2.5 KB per MB row) and
   redirect row `r+1`'s intra top-gathers to the backup. Left-neighbor and
   within-row reads need NO redirect: the row filters only after it fully
   decodes, so within-row intra always reads unfiltered pixels.

## Site inventory (the redirect surface)

`rec_y[(py - 1) * cw + ...]`-shaped top-neighbor gathers in
`decoder/src/mb16.rs`: ~10 luma sites (I16 at ~1597/1604, I4 at ~3439-3461,
I8 at ~3708-3729, CAVLC-path twins at ~3783+), plus the chroma `ctop`/corner
gathers (~2049-2079 region and twins). Total ≈ 15-20 read sites, each a
mechanical redirect to `top_backup[row_parity]` guarded by
`row_interleave_on()`. Every one is covered by the byte-identical corpus gate;
the CAVLC and CABAC loops both carry the hook.

## Staged bricks (each independently gated + knobbed)

- **R1 (LANDED, Part 17):** `precompute_bs_frame` — fused pack+derive, 2-row
  rolling record window, feeds the precomputed consumer path.
  `RS_H264_BS_PRE=0` opt-out. Clock-neutral on both content axes; removed the
  1.1 MB frame buffer.
- **R2 — per-row derive at decode.** Move the R1 pass into the decode loop: at
  each ROW completion (both entropy paths; `addr` crossing `mb_w` boundaries,
  slices handled by "derive any not-yet-derived rows ≤ completed row"), build
  the row's records from the just-written (L1-hot) grids into the 2-row window
  and derive its bS. Picture-end fallback derives whatever rows remain (mid-row
  slice ends, error paths). Removes the separate frame pass entirely.
  Gate: byte-identical 9/9 both knob arms; expected ~1-2%.
- **R3 — row-interleaved filtering.** After row `r` decodes (and its bS is
  derived, R2): save the unfiltered bottom rows, then run the filter for MB
  row `r` (a `filter_rows(r..=r)` variant of `filter_frame`); redirect the
  ~15-20 intra top-gathers to the backup. Per-slice deblock-disable and
  alpha/beta offsets must be tracked PER ROW (they are slice properties; the
  current picture-level call assumes one slice's offsets — a latent
  simplification this stage must not inherit). Rows not filtered when the
  picture ends (error paths) fall back to the tail call.
  Gate: byte-identical 9/9 + conformance 160/160 + the multi-slice streams in
  the conf matrix specifically; expected 3-6% (the cold 1.4 MB/frame pixel
  re-walk becomes warm, and the deblock stage's residual edge-setup share
  rides the same locality).
- **R4 (optional depth) — per-MB record build at MB completion** instead of
  row completion: touches every MB-exit point in both entropy loops (~8
  sites); only worth it if R2's row-granularity gather still shows in the
  sampled profile.

## Risk register

- Intra redirect misses one site → wrong pixels: caught by the corpus gate,
  but bisecting is slow; land the redirect FIRST with filtering still at
  picture end (backup rows written + reads redirected + `assert_eq!` between
  backup and live row in a debug gate — proves coverage while output is
  provably unchanged), THEN flip the interleave on.
- Multi-slice pictures: never filter a row a later slice may still write
  (`next_mb` watermark decides).
- The `RFF_ABL_DEBLOCK`/`RFF_ABL_DBKERNEL` ablation knobs must keep meaning
  what they meant (whole-stage / kernels-only) across the restructure, or
  every historical share in the WHYS doc loses its comparator.
