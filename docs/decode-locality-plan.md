# Decoder locality plan — closing the 2.0× gap to `h264dec` in safe Rust

Status: **plan** (2026-06-27). Technique: [`cache-tiles`](../../.claude/skills) skill.
Discipline: [`optimize-codec`] — profile first, byte-identical gate, measure, **revert
if flat**, one brick per commit.

## Why this plan exists (the evidence)

- The decoder is **2.0× behind `h264dec`** (openh264's own decoder).
- **The asm kernels are already at C parity** — we vendor openh264's *exact* asm, and
  `h264dec` runs the same asm. So the gap is **not** the math.
- **Deployment profile (asm on):** `mgmt/other` **56%**, entropy 13%, kernels
  (mc 4.7% / deblock 9.5% / reconstruct 4.6% / dequant 4.3%) ~24%. The **56% per-MB
  "glue"** is where we lose to C.
- **The bounds-check tax is ~0** (measured: `get_unchecked` over the hot grids/planes,
  interleaved A/B, **flat** 96.7→94.0 Mpx/s). So the gap is **NOT the safe-Rust tax** —
  it's **data layout / cache locality**, which is closeable **in safe Rust**.
- **Prior structural attempts measured ~0:** buffer-hop elimination, padded-MC (exists
  unwired, commit `a00da6b`), AVX2 transform, per-MB alloc. They restructured *movement*
  but **not the core strided-frame *access***. The untried lever is **MB-local tiles**.

⚠️ **This is a major refactor of a working, bit-exact decoder.** The discipline protects
correctness (revert if flat / not byte-identical), but the effort is real and the payoff
is **plausible, not proven**. Phase 0 is the decisive gate.

---

## Phase 0 — Prove it's cache-bound (cheap, decisive, do FIRST)

The profiler shows the glue is 56% but not *why* (cache misses vs index math vs
branches). The prior padded-MC ~0 is a warning the working set may already be L1.

- **Proxy probe:** decode a frame small enough that all planes fit L1/L2 vs a large
  frame; compare per-pixel throughput. Large ≫ slower per-pixel ⇒ cache-bound ⇒ tiles
  will help. Roughly equal ⇒ NOT cache-bound ⇒ the gap is index-math / branch /
  dispatch and tiles **won't** help — pivot.
- **Optional, stronger:** a sampling profiler (VTune / WPA on Windows) for L1/LLC miss
  rate attributed to the per-MB plane/grid accesses.
- **Finer scopes:** split `mgmt/other` further — wrap the strided-frame plane reads/
  writes and the neighbour grid reads in their own `prof` stages.
- **DECISION GATE:** only proceed to Phase 2 if Phase 0 shows the per-MB access is
  genuinely cache-missing the strided frame.

## Phase 1 — Expand-picture (cheap; code already exists)

- **Gap:** `mc_luma` builds a clamped tile per call (`luma_tile`, ~49% of MC pre-asm).
  openh264 pads each ref once (`ExpandPicture`, `PADDING_LENGTH` 32 luma / 16 chroma) →
  MC reads clamp-free + contiguous.
- **Prior work:** `expand_plane` + `mc_luma_padded` + `mc_chroma_padded` exist in
  `inter.rs`, **bit-exact + tested**, committed **unwired** (`a00da6b`). Prior re-wire
  measured **~0** (the 21-wide tile is already L1-resident; the full padded frame, stride
  ~1984, has *worse* locality than the tiny hot tile).
- **Tasks:** re-wire pad-at-`as_reference` + `mc_*_padded`; re-measure in the current
  state. **Expectation: likely still ~0 in isolation** — its real value is *in
  combination* with Phase 2 (MC a padded ref straight into the MB tile = clamp-free AND
  contiguous). Re-confirm cheaply (code exists), document, and **defer the verdict to
  Phase 2**.

## Phase 2 — MB-local cache tiles (the biggest lever; the untried one)

- **Gap:** every per-MB op indexes the **strided full frame**
  (`rec_y[(mb_y*16+dy)*cw + mb_x*16+dx]`) — a cache miss + index-mul per access across
  the whole MB. `h264dec` works in a contiguous `FDEC`-style tile that stays in L1.
- **Design (x264 `FENC`/`FDEC`):** a per-MB tile — 16-wide luma + 8-wide chroma,
  contiguous, plus the top/left neighbour row/col needed by intra pred + in-MB deblock.
  Load neighbours at MB start (carry a 1-MB-row top buffer + per-MB left col), do
  reconstruction (MC-pred + IDCT-add) and intra prediction **in the tile**, copy the
  finished MB back to the frame **once** (a contiguous 16×16 store).
- **Sub-bricks (each byte-identical + measured; STOP if flat):**
  1. **Tile + recon-into-tile + copy-out (luma).** The simplest slice — reconstruct
     into the contiguous tile instead of the strided plane. *If this is flat, the recon
     access wasn't the cache cost → stop and reassess.*
  2. **Intra prediction from the tile's neighbour row/col** (instead of strided frame
     reads). Intra is lookup-heavy → most likely to show.
  3. **Chroma tile.**
  4. **Deblock** — likely **out of scope**: it crosses MB boundaries and is asm'd at
     frame level; restructuring it risks bit-exactness for little gain.
- **Risk: HIGH** — touches `reconstruct` / `predict` / the recon scatter; bit-exactness
  must hold (35/35 corpus + round-trip after every sub-brick). **Big effort.**

## Phase 3 — scan8 neighbour cache (MV/ref/modes) + grid layout

- **Gap:** `mv_neighbors_block`/`mv_neighbors_list` do bounds-checked `Vec` reads with
  `if bx<0||by<0||bx>=w4...` guards per neighbour. `nnz` already uses a scan8 padded
  sentinel cache; **mv / ref / modes do not**.
- **Tasks:** per-MB padded sentinel cache for mv/ref/modes (mirror the nnz one) →
  branchless contiguous neighbour reads, no `Option`, no guard. Measure on **both**
  intra-heavy and inter content (the encoder's scan8 was "real on ALL-INTRA, ~0 on
  INTER").
- **Risk: medium**, localized to the neighbour lookups; modest expected gain.

---

## Order & honest expectations

1. **Phase 0** (cheap, decisive) → gates everything.
2. **Phase 1** (cheap, code exists) → re-confirm; expect ~0 in isolation.
3. **Phase 2** (expensive, gated on Phase 0) → the real bet; sub-brick + measure, stop
   on the first flat sub-brick.
4. **Phase 3** (medium) → independent of Phase 2; do if it measures.

**Realistic ceiling:** if the glue *is* cache-bound, 2.0× → **~1.3–1.5×** is plausible
(compounding ~5–15% per structural brick). **If Phase 0 shows the working set is already
L1-resident** (as padded-MC and the buffer-hop bricks hinted), then the residual gap is
index-math / branch / `cfg`-dispatch-vs-function-pointer / instruction count — a
**~1.3–1.5× safe-Rust + structural floor we may have to accept**, and the right call is
to **stop and document** rather than refactor a correct decoder for nothing.

**The decoder is shipped, bit-exact, and memory-safe. Every brick here is gated to keep
it that way — and deleted if the benchmark doesn't move.**
