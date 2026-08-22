# x264 Structural Port — Exact Structure + Build-Identical Plan

Three **pure-structure** changes (no SIMD, no asm, no `unsafe` — they stay inside
`#![forbid(unsafe_code)]`). These attack the single-core profile (encode 45%, ME
27%, `skip_is_free` 18%, deblock 10%) *without* the hand-asm wall that caps the
SIMD width. Read directly from x264 source (`code.videolan.org/videolan/x264`,
cloned at `/tmp/x264_src`).

**Build order (each is a prerequisite for the next):**
1. **scan8 sentinel context cache** — cross-cutting (feeds ME, CAVLC nnz, deblock); the other two read from it.
2. **Decimation** — needs quantized blocks in hand; drops near-empty blocks → less CAVLC + reconstruct + smaller files.
3. **Cached analysis** — reuse the analyse-phase DCT/decision in encode; kills the `skip_is_free` double-transform.

**Invariant for every step:** the 6/6 ffmpeg bit-exact decode test must stay green.
Steps (1) and (3) are pure refactors → output **byte-identical** to today. Step (2)
intentionally changes the bitstream (it's an encode *decision*) → not byte-identical,
but must decode cleanly and be a net rate/speed win.

---

## (1) scan8 sentinel context cache

### Exact x264 structure

**The `scan8` table** (`common/base.h:180`, `x264_scan8[16*3+3]`). Maps each block
to a slot in a **padded grid, stride 8**. Luma 4×4 blocks 0–15 land at rows 1–4,
cols 4–7:
```
block:  0   1   2   3      scan8:  12  13  20  21
        4   5   6   7              14  15  22  23
        8   9  10  11              28  29  36  37
       12  13  14  15              30  31  38  39
```
The load-bearing property: for any block `i`,
- **left neighbour  = `scan8[i] - 1`**  (one column left)
- **top neighbour   = `scan8[i] - 8`**  (one row up)

Row 0 (slots 4–7) holds the **top** neighbours; col 3 (slots 11,19,27,35) holds the
**left** neighbours. Chroma planes occupy the next two 40-slot bands
(`X264_SCAN8_LUMA_SIZE = 5*8 = 40`, `X264_SCAN8_SIZE = 120`).

**The per-MB cache arrays** (`common/common.h:617–630`), all indexed by scan8:
| array | type | sentinel for "unavailable" |
|---|---|---|
| `non_zero_count[120]` | `u8` | **`0x80`** |
| `intra4x4_pred_mode[40]` | `i8` | **`-1`** (`DC=2` if avail non-I4x4) |
| `mv[2][40][2]` | `i16` | `0` |
| `ref[2][40]` | `i8` | `-1` unused, `-2` unavailable |

**`cache_load`** (`common/macroblock.c:855`), once per MB. Fills the top row + left
col of the padded grid from the frame-level store, or writes sentinels at the
picture edge:
```c
// TOP available: copy 4 neighbour nnz at once from the saved row
CP32( &cache.non_zero_count[scan8[0]-8], &nnz[top][12] );   // macroblock.c:886
// TOP unavailable: write the sentinel
M32( &cache.non_zero_count[scan8[0]-8] ) = 0x80808080U;      // macroblock.c:910
// LEFT column, per block (or left as sentinel if unavailable)
cache.non_zero_count[scan8[0]-1] = nnz[lefttop][...];        // macroblock.c:935
```

**The neighbour read** is then branchless (`common/macroblock.h:432`):
```c
int za = cache.non_zero_count[scan8[idx]-1];   // left  (or 0x80)
int zb = cache.non_zero_count[scan8[idx]-8];   // top   (or 0x80)
int i_ret = za + zb;
if( i_ret < 0x80 ) i_ret = (i_ret + 1) >> 1;   // both avail → round-average
return i_ret & 0x7f;                            // 0x80 sentinel + mask = "use the other / 0"
```
The `0x80` + `& 0x7f` trick collapses all four availability cases (both / left-only
/ top-only / none) into 3 branchless ops — *no bounds checks, no `Option`*.

**`cache_save`** (`common/macroblock.c:1680`): after the MB is encoded, writes its
nnz / modes / mv back to the frame-level store for future neighbours.

### Our current code (the gap)
- Frame grids: `nnz_y: Vec<u8>` sized `(mb_w*4)×(mb_h*4)` ([mb16.rs:40](../crates/rusty_h264-encoder/src/mb16.rs#L40)).
- Per-neighbour lookup ([mb16.rs:1030](../crates/rusty_h264-encoder/src/mb16.rs#L1030)):
  ```rust
  fn luma_nnz(&self, bx: isize, by: isize) -> Option<u8> {
      if bx < 0 || by < 0 || bx as usize >= self.mb_w*4 || by as usize >= self.mb_h*4 {
          None
      } else { Some(self.nnz_y[by as usize * (self.mb_w*4) + bx as usize]) }
  }
  ```
  Called **per block** at encode ([:589](../crates/rusty_h264-encoder/src/mb16.rs#L589),
  [:1710](../crates/rusty_h264-encoder/src/mb16.rs#L1710)) and ME — every access is
  a 4-branch bounds check + `Option` combine. Same pattern for `mv_neighbors`,
  intra modes.

### Build-identical Rust plan
1. Add `const SCAN8: [usize; 48]` — port `x264_scan8` verbatim (luma 16 + chroma
   2×16 + 3 DC), values shifted to our padded layout (stride 8).
2. Add a reused-per-MB `MbCtx` (fields on `FrameEncoder`, cleared per MB):
   ```rust
   nnz_cache: [u8; 120],          // 0x80 = unavailable
   i4x4_mode_cache: [i8; 40],     // -1 = unavailable, 2 = DC
   mv_cache: [[i16; 2]; 40],      // per list (P only needs list 0)
   ref_cache: [i8; 40],
   ```
3. `cache_load(mb_x, mb_y)`: fill row 0 + col 3 of each band from `nnz_y`/`mv_y`/…
   or sentinel at the picture edge. (We can use plain 4-element copies — we don't
   need the `CP32`/`M32` asm-aliasing dodge.)
4. Replace neighbour reads with the branchless form:
   `let za = nnz_cache[SCAN8[i]-1]; let zb = nnz_cache[SCAN8[i]-8]; …` — port the
   `0x80` + `&0x7f` predict exactly.
5. `cache_save(mb_x, mb_y)`: write the MB's nnz/modes/mv from the cache back to the
   frame grids (keep `nnz_y` as the cross-MB store).
6. **Bit-exact invariant:** the predicted nnz value MUST equal today's
   `nc_from_neighbors`. Port the `(za+zb+1)>>1` / sentinel logic and assert
   equality against the current path on a dev clip before deleting the old lookups.
   Output stays byte-identical.

**Verify:** `cargo test` + 6/6 ffmpeg decode + `bench/speedtest.sh` (expect a real
single-core gain: this removes per-block branches across ME + CAVLC + deblock).

---

## (2) Decimation

### Exact x264 structure

**`decimate_score`** (`common/quant.c:326`). Scores how "droppable" a quantized 4×4
block is by walking coefficients high-freq → low and weighting runs of zeros:
```c
static ALWAYS_INLINE int decimate_score_internal( dctcoef *dct, int i_max ) {
    const uint8_t *ds_table = (i_max == 64) ? x264_decimate_table8 : x264_decimate_table4;
    int i_score = 0, idx = i_max - 1;
    while( idx >= 0 && dct[idx] == 0 ) idx--;       // skip trailing zeros
    while( idx >= 0 ) {
        if( (unsigned)(dct[idx--] + 1) > 2 )         // any |level| >= 2  → not droppable
            return 9;                                //   (dct in {-1,0,1} ⇒ dct+1 in {0,1,2})
        int i_run = 0;
        while( idx >= 0 && dct[idx] == 0 ) { idx--; i_run++; }
        i_score += ds_table[i_run];                  // cost of an isolated ±1 after a zero-run
    }
    return i_score;
}
// luma AC (skip DC): decimate_score15(dct) = internal(dct+1, 15)   (quant.c:353)
```
`x264_decimate_table4` weights longer zero-runs as cheaper, so a block with a few
isolated ±1s scores low.

**The use** (`encoder/macroblock.c:135–210`, `x264_mb_encode_i16x16` / inter). After
quantizing all 16 luma blocks, accumulate the score, then **zero the whole MB's
luma if it's below threshold**:
```c
int decimate_score = h->mb.b_dct_decimate ? 0 : 9;   // 9 = disabled (never decimate)
...
for( int i8x8 = 0; i8x8 < 4; i8x8++ ) {
    nz = quant_4x4x4( &dct4x4[i8x8*4], ... );
    if( nz ) FOREACH_BIT( idx, i8x8*4, nz ) {
        zigzag/dequant ...
        if( decimate_score < 6 ) decimate_score += decimate_score15( luma4x4[idx] );
        nnz[scan8[idx]] = 1;
    }
}
if( decimate_score < 6 ) {        // macroblock.c:207 — whole-MB luma decimated
    CLEAR_16x16_NNZ( p );         //   zero all 16 nnz
    block_cbp = 0;                //   → coded as cbp=0, no residual emitted
}
```
**Thresholds** (from the comment at `quant.c:320`): **8×8 block < 4 → null; whole MB
luma < 6 → null; chroma < 7 → null.** Saves both the CAVLC bits *and* the
entropy-coding + reconstruction work on the dropped blocks.

### Our current code (the gap)
We code **every** non-zero block — no decimation. After `quantize`, any non-zero cbp
goes straight to CAVLC + reconstruction. (See the inter luma residual loop and the
CBP accumulation in `encode_inter_mb`.)

### Build-identical Rust plan
1. Add `const DECIMATE_TABLE4: [u8; 16]` (and `…8` later) — port `x264_decimate_table4`.
2. Add `fn decimate_score15(levels: &[i32; 16]) -> i32` (skip index 0 / DC) and
   `decimate_score16` — port `decimate_score_internal` exactly.
3. In `encode_inter_mb` (and i16x16), after quantizing the 16 luma blocks: if a
   running `decimate_score` (gated `< 6`, summing `decimate_score15` over non-zero
   blocks) ends `< 6`, **zero the luma cbp + all 16 nnz** for the MB (the block
   becomes P_Skip-eligible or cbp=0). Same for chroma at threshold 7.
4. Gate behind the preset/flag (mirror `b_dct_decimate`): **on for `Preset::Fast`**
   (matches x264 ultrafast, which has `dct_decimate=1`), and optionally quality.
5. **Invariant:** output is *not* byte-identical to today (it drops blocks) — this
   is an encode decision. Verify: 6/6 ffmpeg decode still green; `speedtest.sh` for
   the speed win; check bits ↓ and PSNR ~flat (decimation is a known net RD win).

**Verify:** 6/6 decode + measure Δbits / ΔPSNR / Δspeed.

---

## (3) Cached analysis — kill the `skip_is_free` double-transform

### Exact x264 structure

x264 splits the MB pipeline into **`analyse()`** (decide type/MV/mode, and for intra
*actually compute and stash* the DCT/nnz) and **`encode()`** (reuse the stash). Two
flags carry the reuse:

**`b_skip_mc`** (`encoder/macroblock.c:1125`, set inside `macroblock_probe_skip`).
The skip test does the MC + transform once; if the MB will be coded as P_Skip,
`encode()` does **not** redo motion compensation:
```c
if( h->mb.i_type == P_SKIP )
    if( !h->mb.b_skip_mc )          // macroblock.c:657 — already done during analyse
        h->mc.mc_luma( ... );
```

**`i_skip_intra`** (`encoder/analyse.c:322`, used `encoder/macroblock.c:717–760`).
Analyse encodes the i4x4/i8x8 blocks during mode search and saves the reconstruction
+ nnz + (in RD mode) the DCT into `pic.i4x4_fdec_buf` / `i4x4_nnz_buf` /
`i4x4_dct_buf`. `encode()` then restores them and **only re-encodes the last block**:
```c
if( h->mb.i_skip_intra ) {
    h->mc.copy[PIXEL_16x16]( p_fdec, FDEC_STRIDE, pic.i4x4_fdec_buf, 16, 16 );
    M32( &nnz[scan8[0]] ) = pic.i4x4_nnz_buf[0]; ...        // restore nnz
    h->mb.i_cbp_luma = pic.i4x4_cbp;
    if( h->mb.i_skip_intra == 2 )                            // RD: restore DCT too
        memcpy( h->dct.luma4x4, pic.i4x4_dct_buf, ... );
}
for( int i = (p==0 && h->mb.i_skip_intra) ? 15 : 0; i < 16; i++ )   // skip 15/16!
    ... encode block i ...
```
**The principle:** the transform/quant computed to *decide* the MB is **kept** and
reused to *emit* it — never recomputed.

**The cheap skip test** (`macroblock_probe_skip_internal`, `encoder/macroblock.c:1131`)
also shows x264 testing skip via `sub8x8_dct` + `quant` + **`decimate`** (reusing #2),
returning early as soon as `i_decimate_mb >= 7` — not a full transform-the-whole-MB.

### Our current code (the gap)
`skip_is_free` ([mb16.rs:685](../crates/rusty_h264-encoder/src/mb16.rs#L685))
forward-transforms + quantizes **the entire MB** (16 luma + 8 chroma blocks) purely
to *test* skip:
```rust
if quantize(&forward_core(&res), qp, 6).iter().any(|&v| v != 0) { return false; }
```
Then `encode_inter_mb` transforms the **same residual again** to actually code it —
the double-transform, ~18% of single-core time.

### Build-identical Rust plan
1. **Stash the analyse transform.** When the MB's residual is forward-transformed +
   quantized for the coding decision, keep the coefficients/levels/nnz/cbp in a
   per-MB scratch (mirror `i4x4_dct_buf` + `nnz_buf` + `cbp`).
2. **Reuse in encode.** `encode_inter_mb` consumes the stashed levels instead of
   re-running `forward_core` + `quantize`. Reconstruction reads the stashed
   dequantized coefficients.
3. **Cheap skip test.** Replace `skip_is_free`'s full-MB transform with the
   decimate-based early-out (#2): transform/quant, accumulate `decimate_score`,
   return "skip" as soon as it stays under threshold — and feed the *same* stashed
   coefficients into encode if not skipped (no third pass).
4. **Invariant:** if the stashed levels are exactly what `encode` would have
   recomputed, output is **byte-identical** to today — this is a pure
   compute-elimination. Assert the stash == recompute on a dev clip before removing
   the second transform.

**Verify:** `cargo test` + 6/6 decode (byte-identical) + `speedtest.sh` (expect the
~18% `skip_is_free` slice to largely vanish).

---

## After all three
Re-profile single-core (encode / ME / skip / deblock %) and re-run
`bench/speedtest.sh`. Expected: the per-core gap narrows materially from structure
alone — the branches (#1), dropped work (#2), and eliminated double-transform (#3)
— with the residual gap still being x264's hand-asm SIMD width (the `forbid(unsafe)`
floor, documented in `memory/x264-speed-architecture.md`).
