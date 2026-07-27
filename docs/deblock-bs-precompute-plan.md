# Deriving boundary strengths during encode (the x264 structure)

**Status:** implemented and measured. Phase 2 kept; Phase 1 built, measured, and
defaulted OFF; Phase 3 pruned by measurement. See §7.
**Motivation:** `video-tests` puts our deblocking at ~2.7× x264's. Decomposed, the
gap is not the filter kernels (1.6×) but the **boundary-strength derivation**:
~105 ns/MB against x264's ~18 ns/MB. And within that, the cost is not the bS
arithmetic (~1.25 ns per decision) — it is **gathering the block state**
(~45 ns/MB) that the encoder already had in hand a moment earlier.

---

## 1. What x264 actually does

Three separate mechanisms, and the middle one is the part usually missed.

### 1.1 It derives bS inside the MB-encode loop, not the deblock pass

`x264_macroblock_deblock_strength( h )` is called from `encoder.c` per macroblock,
right after the macroblock is coded — **not** from `x264_frame_deblock_row`. The
deblock pass only *reads* the result. That is why x264's `deblock` stage looks so
cheap in a naive stage-vs-stage comparison, and why our first measurement read
6.4× when the honest number was ~3.7× (see `video-tests/README.md`).

Because it runs inside the encode loop, its inputs — `h->mb.cache.non_zero_count`,
`h->mb.cache.ref`, `h->mb.cache.mv` — are the **scan8 cache**, already populated
for the macroblock being coded. It performs no gather. The one exception is
`neighbour_changed`, which reloads neighbour state *only* when deblocking across
slice edges with multiple slices.

### 1.2 Whole-macroblock constant fills carry the average

The measured ~18 ns/MB is not one fast SIMD kernel applied uniformly — most
macroblocks never reach the kernel:

```c
if( IS_INTRA( h->mb.i_type ) ) {            /* intra: no derivation at all */
    M32( bs[0][1] ) = 0x03030303;           /* two 32-bit + two 64-bit stores */
    M64( bs[0][2] ) = 0x0303030303030303ULL;
    M32( bs[1][1] ) = 0x03030303;
    M64( bs[1][2] ) = 0x0303030303030303ULL;
    return;
}
/* 8x8 transform with every luma cbp bit set => nnz guarantees strength 2 */
if( h->mb.b_transform_8x8 && !CHROMA444 ) { ... M32/M64 fills of 0x02 ...; return; }
```

Note what this implies: **the kernel only ever produces bS ∈ {0,1,2}.** Intra
internal edges are the constant 3; the bS=4 macroblock-edge case is applied later
by the deblock loop from `intra_cur`. Restricting the kernel to the inter case is
exactly what makes it branch-light and vectorisable.

### 1.3 Storage is a two-row ring, not a frame buffer

`uint8_t bs[2][8][4]` — direction × edge × segment, 64 B/MB — stored at
`h->deblock_strength[mb_y & 1][mb_x]`. Only **two macroblock rows** are retained,
because deblocking lags encoding by a row. At CIF that is 2 × 22 × 64 ≈ 2.8 KB,
comfortably L1-resident.

And the consumer's early-out tests all four segments with a single 32-bit load:

```c
if( !M32(bS) || !alpha || !beta ) return;
```

---

## 2. Mapping onto our code

| x264 | ours | note |
|---|---|---|
| `h->mb.cache.{non_zero_count,ref,mv}` (scan8) | `fe.{nnz_y,ref_idx_y,mv_y,inter_y}` frame-wide grids | ours are frame-wide and strided; x264's are a per-MB cache |
| `x264_macroblock_deblock_strength()` | *(does not exist)* — we derive inside `filter_frame` | the change |
| `h->deblock_strength[mb_y&1][mb_x]` | *(does not exist)* | new storage |
| `deblock_strength_c/_avx2` | `bs_tile()` × 32 per MB | ours also handles intra; x264's does not |
| `deblock_edge()`'s `!M32(bS)` early-out | `bs4.iter().all(|&b| b == 0)` | ours is 4 compares, could be one `u32` |
| `x264_frame_deblock_row()` | `deblock::filter_frame()` | ours is a whole-frame pass |
| per-MB `Tile` gather (24 blocks) | — | **pure overhead x264 never pays** |

Our commit points — the places that already hold the state bS needs — are:

* `commit_skip()` — skip macroblock: all-inter, nnz 0, one (ref, mv). Internal
  edges are all bS 0 by construction (this is exactly today's `flat_inter` gate).
* `plan_inter_mb()` — inter macroblock: cbp/nnz, per-partition mv and ref_idx.
* `plan_mb()` / `encode_mb()` — intra macroblock: constant fill, per §1.2.

---

## 3. Proposed phases

### Phase 1 — storage + encoder-side derivation (encoder-only win)

Add a per-frame bS grid and fill it at the three commit points.

```rust
/// Boundary strengths for one macroblock: [direction][edge][segment].
/// direction 0 = vertical edges, 1 = horizontal. 32 B/MB.
/// Chroma edge groups reuse edges 0 and 2 (pinned by `chroma_bs_matches_luma`).
pub struct MbBs([[u8; 4]; 8]);
```

`filter_frame` gains an optional precomputed grid; when present it skips the
`Tile` gather and all derivation, when absent it behaves exactly as today (so the
**decoder path is untouched** — the same "leave the feature's own path unchanged"
discipline that made the earlier `as_reference` work safe).

Copy x264's two early-outs verbatim in spirit:
* intra macroblock → constant fill 3 on internal edges;
* skip macroblock → internal edges all 0, only the two macroblock edges derived.

### Phase 2 — narrow the kernel to the inter case

Once intra is a constant fill, the remaining derivation only produces 0/1/2, so
`bs_tile` loses its intra branch. That is the precondition for vectorising four
segments at once; attempting SIMD *before* this is fighting a branch that does not
need to be there.

### Phase 3 (optional) — two-row ring + row-interleaved deblocking

Only worth doing if Phase 1–2 leave deblocking hot. Restructuring `filter_frame`
into a per-MB-row call inside the encode loop buys pixel locality on top of the
bS win, but it is a real restructure of when deblocking happens and it changes
the encoder/decoder shared-path story.

---

## 4. Expected payoff — and the honest caveat

The gather does **not** vanish entirely. At commit time we hold the current
macroblock's 16 blocks, but the left column and top row still have to be read:

* left column — written moments ago, hot;
* top row — one macroblock row back, likely still in L2.

So the gather goes from 24 blocks (cold-ish, strided, four separate frame-wide
arrays) to 8 (hot). Estimate:

| | now | after Phase 1–2 |
|---|---:|---:|
| gather | ~45 ns/MB | ~15 ns/MB |
| derivation | ~85 ns/MB (32 edges) | ~15 ns/MB (intra/skip fills + inter-only kernel) |
| **bS total** | **~105 ns/MB** | **~30 ns/MB** |

Deblock overall ≈ 300 → ~230 ns/MB (−23%), which at deblock's ~11% share is
**≈ 2.5% of encode**. Real, but firmly a second-order lever — worth doing after
anything larger, and worth knowing the size of before starting.

## 5. Gates

* **Equivalence oracle (mandatory).** The precomputed grid must equal what
  `filter_frame` derives. A test encoding corpus clips with both paths and
  asserting the grids match, macroblock by macroblock — the same shape as
  `bs_arms_agree` and `tile_matches_frame_indexing`.
* **Byte-identical bitstream.** Deblocking feeds the inter prediction reference,
  so any behavioural change moves the bits: `analyzer hash` against the baseline.
* **Interleaved A/B**, not separate builds — this box drifts ~20% run to run, more
  than the effect being measured.

## 6. Risks

* bS logic would exist in two places (encoder commit + `filter_frame` fallback).
  The equivalence test is what keeps that honest; without it this is a latent
  drift bug that only shows up as a wrong reconstruction much later.
* Memory: 32 B/MB frame-wide is 261 KB at 1080p. Fine as a streaming write/read,
  but it is why x264 uses a two-row ring — adopt that if it ever matters.
* B-frames use the two-reference-list derivation; Phase 1 must either handle it or
  fall back to the existing path for B (the safer first cut).
* Encoder-only. The decoder keeps deriving, so decode is unaffected.


---

## 7. Results

### Phase 2 — kept (the win)

Per-macroblock constant fills + an inter-only kernel, plus two redundancies found
on the way:

* **chroma reuse** — a chroma edge group is co-located with luma edges 0 and 2 and
  derives identical strengths, so 16 of 48 per-macroblock derivations were
  recomputes (`chroma_bs_matches_luma`);
* **uniform-motion fast path** — a coded inter macroblock whose 16 blocks share one
  (ref, mv), i.e. every single-partition `P_L0_16x16`, cannot reach internal
  strength 1, so its internal edges depend on coefficients alone and every motion
  comparison is skipped.

Derivation-only cost fell **105 → 83 ns/MB**, and the tile-vs-per-edge advantage
rose (CIF `mixed` 1.55 → 1.74×, `inter-bs0` 1.75 → 2.13×). Byte-identical.

### Phase 1 — built, measured, defaulted OFF

It does exactly what §1 said: the deblocking stage sheds the gather and the
derivation and gets **1.4–1.7× faster**. It does not make the encoder faster.

Interleaved A/B, one process, alternating passes (foreman CIF, fast):

| | derive-in-filter | precompute-in-loop | Δ |
|---|---:|---:|---:|
| deblock | 21.4 ms | 14.5 ms | **−6.9** |
| mb-loop | 135.1 ms | 145.2 ms | **+10.1** |
| TOTAL | 170.9 ms | 173.5 ms | +2.7 |

**Why — the premise was wrong.** Scoping the in-loop derivation directly
(`enc-bs-derive`) priced it at 138–174 ns/MB against ~116–154 ns/MB for the same
work in the deblocking pass. The block grids were never cold: ~90 KB at CIF is
L2-resident, so a streaming pass reads them just as cheaply. Worse, the encode
loop's working set (source, reference and reconstruction planes, DCT buffers,
CAVLC state) is contended, so the loop grew by roughly **twice** the derivation's
own cost — adding work there also slows the surrounding code.

Making the in-loop version cheaper did not rescue it. `MbKind` (intra → constant
fill, skip → internal zeros, single-partition inter → nnz only) cut the derivation
to 66–138 ns/MB and helped the skip-heavy clips (akiyo +3.9 → +1.5 ms, mobile
+23.4 → +9.1) but not foreman, which is only 5.9% skip macroblocks. Six of seven
fast-preset measurements still showed TOTAL neutral-to-worse.

Kept behind `set_precomputed_bs` (default off) with its tests, because the
machinery is what a genuine commit-time derivation would build on — one that never
re-reads the grids, taking values still live in registers when the macroblock is
committed. That, not relocation, is the only version that can win.

### Phase 3 — pruned by measurement

Row-interleaved deblocking with a two-row ring is a **locality** fix, so the
cache-boundedness sweep gates it. Per-macroblock deblocking cost is flat as the
frame crosses L2 — QCIF 217 → CIF 265 → 720p 244 ns/MB — so deblocking is **not
cache-bound** and row-interleaving cannot pay. (It would also need unfiltered
neighbour rows preserved for intra prediction, which is why our filter is a
post-pass at all.) Pruned before writing any of it, per the analyzer rule: run the
sweep BEFORE a locality refactor.

### Transferable lesson

*Moving work to where the data is "hot" only pays if the data was actually cold.*
Measure the destination's cost, not just the source's saving — a stage-level win
that reappears one stage over is not a win, and a hot loop can be the **worst**
place to add work.
