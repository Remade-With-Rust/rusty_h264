# Decoder locality plan — closing the 2.0× gap to `h264dec` in safe Rust

Status: **Phase 0 EXECUTED 2026-06-27 → NOT cache-bound → Phases 1 & 2 RULED OUT.**
Technique: [`cache-tiles`](../../.claude/skills) skill. Discipline: [`optimize-codec`].

> ## ⛔ Phase 0 result — the decoder is NOT cache-bound (do not build the tiles)
> The `cache_probe` test (`tests/profile_decode.rs`) decodes the same content density at
> frame sizes from 256² to 1536²/1080p — working set **<1 MiB → 10 MiB, 5× past this
> machine's 2 MiB L2**. If cache-bound, per-pixel throughput would **drop** sharply.
> Instead it **rose** (≈34 → 42 Mpx/s, two runs, monotonic). A cache-bound workload
> crossing L2 shows a 1.5–3× slowdown; we see none. The active per-MB-row band fits
> cache **regardless of frame size**, so **MB-local tiles (Phase 2) and expand-picture
> (Phase 1) would not help** — confirming why the prior padded-MC / buffer-hop bricks
> were ~0. **The 2.0× gap is NOT cache and NOT bounds-checks (both ruled out by
> measurement) — it is the per-MB scalar instruction-count / codegen / control-flow**
> (C's tighter codegen + function-pointer dispatch on the same per-MB work). That is a
> safe-Rust + structure floor, hard to close without turning glue into a kernel.
>
> **Phase 3 EXECUTED (2026-06-27) → ruled out.** Decomposed the residue with
> Neighbors/Finalize prof stages: **`neighbors` = 0.3%** → scan8's ceiling is 0.3%,
> not worth it. BUT the same decomposition surfaced **`Finalize` = 13%** — the per-frame
> `as_reference` clones + `into_frame` output build, hidden in the "ghost". First brick:
> `into_frame` **moves** the planes when crop==0 (vs alloc+copy) = **byte-identical +5%**
> (`a6d7012`) — the first real decode win. **The "56% ghost" was NOT an irreducible floor
> — it was under-decomposed.** Next lever: `as_reference`'s eager per-block `ref_poc`
> collect + mv/ref_idx clones are only used by B temporal-direct → waste on P/intra.
> See the **`analyzer`** skill for the decomposition toolkit that found this.
>
> **DONE (`as_reference` lever):** gated the B-only motion on `b_possible = profile_idc
> != 66`; skipped on Baseline → **+12%, byte-identical** (B-capable streams take the
> unchanged path). Finalize 13% → 7.2% (remaining = the necessary DPB plane clone).
> **Total from decomposing Finalize: ~+17% on Baseline decode** — the "ghost" was
> under-decomposed, not an irreducible floor.
>
> **DONE (deblock-prep lever, same method):** the per-frame deblock PREP (in the ghost —
> the `Deblock` scope is inside `filter_frame`) had the same unused-feature waste:
> List-1 `ref_id1` (B-only → **+5.7%**), the inverted `intra` mask (→ pass `inter`, no
> alloc, **+3.3%**, decoder+encoder), the `nnz_db` clone (no-8×8). Four bricks total —
> `into_frame`, `as_reference`, `ref_id1`, `intra→inter` — took **decode ~94 → ~110
> Mpx/s**, all byte-identical. Remaining `mgmt/other` is now mostly *necessary* per-MB
> scalar work (syntax parse, dispatch, grid writes) + profiler timer overhead — the
> redundancy vein (per-frame/per-MB work for unused stream features) is mined out.
>
> **DONE (profiler-overhead lever, 2026-07-01):** the residual "34% ghost" was the
> profiler measuring itself (~1.01M scope entries × 2 `Instant::now()` ≈ 61 ms). Swapped
> the per-scope timer to `rdtsc` (~15 ns vs QPC ~30 ns): profile-ON `Total` 145 → 118 ms,
> overhead **61 → 34 ms (−45%)**, and the high-call stages (reconstruct/dequant/scatter)
> snapped to their true, smaller size. Shipped build stays `forbid(unsafe)` (the timer is
> gated on `feature="profile"`). Dropping the bucket atomics to non-atomic `+=` was FLAT
> → reverted; 2× `rdtsc`/scope is the instrumentation floor. See the **`analyzer`** skill.
>
> **DONE (DPB double-clone, 2026-07-01):** meticulous median-of-31 profiling on the
> asm-ON deployment path (entropy 14ms / deblock 12ms / finalize 10ms are the pure-Rust
> levers) + a new `DpbClone` sub-stage exposed a **second** full-plane clone hiding in
> `apply_ref_marking`: it took `&mut RefFrame` and `insert(0, reference.clone())`d a
> caller local that's dropped right after. Take it by value + **move** → **finalize 9.6 →
> 6.1 ms**, byte-identical. Same eliminate-redundancy vein (`&mut`-that-should-be-move +
> `.clone()` at a container insert). Remaining finalize (3.3 ms) + the necessary
> `as_reference` clone (2.8 ms) are real work; a DPB buffer pool is the only further lever
> and is structural + hard to measure under thermal noise — deferred.

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
