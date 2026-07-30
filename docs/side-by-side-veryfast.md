# Function-level side-by-side: rs_h264 vs x264 veryfast (encoder) / ffmpeg (decoder)

*2026-07-29 · foreman_cif 120f · qp27 · keyint/gop 30 · threads 1 · both sides
instrumented with the same rdtsc scope profiler (ours: `--features profile`;
x264: `x264-prof.exe`, the `-DX264_PROF` twin in `_ref_x264`).*

## Method & caveats

- **Shares are the truth, walls are anchors.** Profiler builds inflate walls
  (ours ~1.5×, x264's prof twin ~2.4× on this run), and the box was throttling
  long runs today (our stock quality read 684 ms vs the calm-day 435 ms, while
  x264's short stock runs sat stable at ~176 ms). So each column reports **% of
  that binary's own TOTAL**, and the "ms @ stock" columns scale the share onto
  the stock wall (ours calm-day 435 ms; x264 176 ms measured today, matching
  the calm-day 170 ms).
- Per-call ns come from the prof builds — both sides carry the same class of
  rdtsc tax, so **cross-side per-call ratios are meaningful, absolute ns are
  slightly high**.
- Settings matched where the encoders allow: same source, QP, GOP, 1 thread.
  Feature sets are each side's deployment default: ours = quality preset
  (CAVLC, AQ on, no lookahead); x264 = veryfast `--profile main` (CABAC,
  AQ + mb-tree lookahead, sub-8×8 partitions). The BD context for this speed
  table: PSNR-matched we code −1.6% vs veryfast (H-29).

## ENCODER — mapped categories

| category | OURS ms@stock (share) | x264 ms@stock (share) | ratio | per-call OURS | per-call x264 |
|---|---:|---:|---:|---:|---:|
| **Motion estimation (total)** | **257 (59.1%)** | **68 (38.6%)** | **3.8×** | 2029 ns × 192k searches | 455 ns × 355k searches |
| — sub-pel refinement | 167 (38.5%) | (inside ME) | — | 1321 ns/search | — |
| — full-pel diamond | 42 (9.8%) | (inside ME) | — | 335 ns/search | — |
| — hpel plane build+read | 22 (5.0%) | 2.7 (1.5%) | **8.1×** | 236 µs/frame | 5.7 µs/call ×1152 |
| — rescue grid | 2.9 (0.7%) | — | — | 23 ns/search | — |
| Intra cost (in decision) | 4.7 (1.1%) | 7.9 (4.5%) | **0.6× WE WIN** | 172 ns | 1793 cyc |
| Analyse/decision glue | ~9 (2.1%) | 8.4 (4.8%) | ~1× | — | — |
| MB code: T/Q + recon + MC | 43 (9.9%) | 13.8 (7.8%) | 3.1× | 1205 ns/MB (inter) | 870 cyc/MB |
| Entropy emit | 47.6 (10.9%) CAVLC | 15.1 (8.5%) CABAC | **3.2×** | 1750 ns/MB | 1455 ns/call |
| Deblock | 12.3 (2.8%) | 4.5 (2.6%) | 2.7× | 155 µs/frame | 20.6k cyc ×1152 |
| Lookahead (slicetype+mbtree) | 3.4 (0.8%, AQ map only) | 40.3 (22.9%) | **0.08× WE WIN** | — | 6.4M cyc ×36 |
| MB-loop glue (unnamed per-MB) | ~39 (9.0%) | — | — | — | — |
| Frame-level rest + residue | ~17 | 15.3 (8.7%) | ~1× | — | — |
| **TOTAL (stock walls)** | **435 ms** | **176 ms** | **2.5×** | | |

### Encoder verdicts (ranked by absolute gap)

1. **Sub-pel refinement is THE function: 167 ms — 2.5× x264's ENTIRE motion
   estimation.** Everything else combined matters less. Per-search we're at
   ~4.5× their cost (down from 10.5× at campaign start); the count is fine
   (0.54× theirs).
2. **Hpel plane build: 8× (22 vs 2.7 ms).** x264 filters hpel once per frame
   in `hpel-filter` (asm, 5.7 µs/call); our per-reference build costs 236
   µs/frame. The fused-builder base sits reverted behind `RFF_HPEL_FUSED=1`
   awaiting a real AVX2 kernel — this table says that kernel is worth ~5% of
   encode.
3. **Entropy emit: 3.2× (47.6 vs 15.1 ms) — and they're doing CABAC, we're
   doing CAVLC.** Per-MB we're 1750 ns vs their 1455 ns on a *more expensive
   codec*. The emit path (EncEmit) has never had a dedicated descent.
4. **MB code (T/Q+recon+MC): 3.1× (43 vs 13.8 ms).** Includes our pred-buf
   re-stride copies (25.8 prof-ms) — the streamed-data-movement class.
5. **Deblock: 2.7× (12.3 vs 4.5 ms).** Small absolute; asm exists on both
   sides — ours is per-MB-loop structured, theirs frame-sliced.
6. **Where we WIN: no lookahead (they pay 23% for slicetype+mbtree — that's
   where their BD edge is manufactured) and intra cost (0.6×, our SATD
   dispatch work paid off).** The lookahead line is a policy asymmetry, not a
   deficiency: our mbtree is opt-in and CQP-CAVLC-gated; enabling it buys BD
   and costs this same ~20%.

## DECODER — our stage profile on two real streams + ffmpeg reference

Stock walls (best-of-9, asm build): **ours.264 (our CAVLC output) 99.2 ms =
122.6 Mpx/s; vf.264 (x264 veryfast CABAC output) 264.2 ms = 46.1 Mpx/s.**
ffmpeg single-thread decodes either in ≤65 ms wall including process startup
(utime 31–62 ms) → we are **~1.5–2× behind on our own streams, ~4–5× behind on
x264 streams**.

| stage | ours.264 ms@stock (share) | vf.264 ms@stock (share) | note |
|---|---:|---:|---|
| inter-MC | 20.0 (20.2%) | **112.6 (42.6%)** | **231k → 2.41M calls (10.4×)**: sub-8×8 partitions + qpel everywhere; per-call fine (118 ns) |
| entropy parse | 9.7 (9.8%) CAVLC | **0 tapped — in residue** | CABAC parse has NO profiler stage |
| deblock | 11.9 (12.0%) | 11.8 (4.5%) | same absolute both streams |
| pred-buf copy | 8.1 (8.2%) | 0 (path untapped) | re-stride glue, CAVLC path |
| dpb-clone | 4.8 (4.8%) | 1.8 | reference plane clone per frame |
| reconstruct+scatter | 2.0 | 10.1 | 590k blocks on vf (more partitions) |
| mv+grid / syntax / neighbors | 4.7 | 0.2 (untapped paths) | |
| finalize | 2.0 | 3.5 | |
| intra-pred | 0.4 | 0.5 | |
| skip-recon | 0.15 | 0.11 | |
| **residue (unattributed)** | **~35 (36%)** | **~124 (47%)** | part profiler self-cost (~1.3M / 3.1M scopes ≈ 45/110 ms prof-build); the REAL remainder on vf is the **CABAC parse + sub-8×8 MB glue** |

### Decoder verdicts (ranked)

1. **The biggest decoder function has no name: CABAC parse.** On real-world
   (x264-produced) streams ~half the decode is residue, and the entropy bucket
   only taps CAVLC. First brick of any decoder campaign: scope the CABAC
   read path (and the sub-8×8 parse/MC glue) so the residue is attributed.
2. **MC call count, not call cost: 2.41M calls at a healthy 118 ns.** x264's
   veryfast streams carry sub-8×8 partitions and qpel; per-call dispatch
   overhead × 10× the calls = 43% of decode. The openh264-style lever is
   structural: batched/fn-pointer MC dispatch, decode_p8x8-side block merging,
   and serving 4×4/8×8 from wider fetches.
3. **Copy glue (pred-buf + dpb-clone + finalize ≈ 15 ms on our stream)** — the
   ExpandPicture/plane-sharing class of fix; measured small per item but it is
   the same streamed-data-movement family the encoder campaign kept meeting.
4. **Content-of-stream matters more than any single kernel**: same decoder,
   same box — 122.6 vs 46.1 Mpx/s purely from what the encoder put in the
   stream. Decoder benchmarks must use x264-produced streams to be honest.

## Where to explore next (both sides, ranked by prize)

| prize | side | lever |
|---:|---|---|
| ~90 ms/120f | enc | sub-pel per-search cost (still 4.5× theirs after memoized ring + FC) |
| ~120 ms/120f | dec | name + attack the CABAC parse residue on real streams |
| ~90 ms/120f | dec | MC dispatch overhead × 2.4M calls on sub-8×8 streams |
| ~32 ms/120f | enc | entropy emit descent (never had one; 3.2× at CAVLC-vs-CABAC) |
| ~19 ms/120f | enc | AVX2 fused hpel builder (base exists, reverted, 8× gap measured) |
| ~29 ms/120f | enc | MB-code T/Q+recon+pred-buf copies |
| policy | enc | lookahead: they spend 23% to buy their BD edge; ours is opt-in — a speed edge we could either keep or trade |

---

# SHIFT — re-measured after the H-31 hammer (same day, later session)

*Box caveat: the machine degraded ~2× between the morning baseline and this
re-measure (x264 itself dropped 685 → ~320 fps). Every claim below is either a
paired same-minutes A/B or an explicitly implied calm-box number.*

## Decoder — the table that moved

| metric | before | after | how measured |
|---|---:|---:|---|
| decode x264 veryfast stream | 264 ms (calm) | **~142 ms implied calm** | paired pre/post binaries, same minutes: **1.86–1.95×, 7/7 rounds**, byte-identical YUV |
| MC kernel entries (vf, 120f) | 2,413,851 | **263,331 (9.2×)** | deterministic census |
| inter-MC share of decode | 46% | **14.6%** | prof build, same stream |
| decode own CAVLC stream | 99–190 ms | unchanged | untouched path (paired ~1.0×) |
| vs ffmpeg on real-world streams | ~4.1× behind | **~2.2× behind (implied calm)** | ffmpeg ≤65 ms stable across sessions |
| run-to-run stability (vf) | 546–801 ms swings | **379–388 ms** | the coalesced path is far less thermally sensitive |

New decoder share ranking (post, prof build): **per-MB glue residue ~57%**
(incl. profiler self-cost; the un-scoped CABAC MB-loop bookkeeping, nzc/
neighbor caches, dequant/un-scan, B-direct derivation) > inter-MC 14.6% >
deblock 7.5% > CABAC residual parse 6.3% > reconstruct 5.2%. The residue is
the next decoder target — and per H-31's law it gets NAMED before attacked.

## Encoder — verdict shifts, small wall shift

| item | disposition |
|---|---|
| hpel build | **landed**: bucket 52.9 → 31.8 ms (1.66×), byte-identical; remainder = plane-alloc zeroing + pad copy |
| entropy emit | **re-attributed**: per-bit throughput at parity (~6 MB/s both); the "3.2×" was skip-count + CAVLC-bytes, i.e. compression, not speed |
| sub-pel | **closed as policy**: per-eval at kernel floor; the count is the BD edge, capped via shipped rungs |
| lookahead | **measured**: mbtree BD wins everywhere (−1.1..−4.9) but cost unbounded on busy content (bus +251%; full-res vs x264's half-res lookahead) — keep the speed edge; lowres/gated lookahead is the enabling brick |
| encoder wall | unchanged within today's noise (hpel brick ≈ 1.5% of encode) |

## Updated exploration table (replaces the one above)

| prize | side | lever |
|---:|---|---|
| ~165 ms/120f class | dec | NAME the per-MB glue residue (57% share) — scope the CABAC MB loop's bookkeeping, then attack what the taps reveal |
| ~15 ms/120f | dec | plane-copy family: dpb-clone + finalize + pred-buf (ExpandPicture-style plane sharing) |
| ~10 ms/120f | enc | hpel build remainder: reuse plane buffers (skip 700 KB/build zeroing), fold pad copy into the fused pass |
| BD, bounded cost | enc | LOWRES lookahead — unlocks the measured −1..−5 BD of mbtree at x264-class (~20%) cost instead of unbounded |
| wall ÷ ~3.5 | both | threading already verified byte-identical (H-30) — the multiplier stacks on all of the above |

---

# WHERE WE LIVE — full re-benchmark after H-31..H-34 (same-session, boosted box)

*foreman_cif 48f · 4-QP BD · single-thread unless noted · x264 at full boost
(143 Mpx/s superfast), so every ratio is honest.*

## Encoder (quality preset)

| vs x264 | BD (SSIM-tuned default) | BD (PSNR-matched, XB_AQ=0) | our speed |
|---|---:|---:|---:|
| superfast | **−10.3%** | **−14.7%** | 0.17–0.21× |
| veryfast | +5.5% | **+0.3% — a tie** | 0.32× |
| fast/medium | +12.2% | +6.7% | 0.63–0.73× |
| **slow** | +10.3% | **+4.8%** | **1.38× — WE are faster** |

Threading (byte-identical, GOP-parallel): quality 679 → 153 ms/120f (**4.4×**)
= 30.6 ms/24f-equivalent — **threaded, we encode at x264 veryfast's
single-thread speed (28 ms) while matching its PSNR compression.**

## Decoder (same minutes, 1 thread)

| stream | ours | ffmpeg | gap |
|---|---:|---:|---:|
| our CAVLC output | 105 ms = 115.6 Mpx/s | ~45–50 ms | **~2.2×** |
| x264 veryfast output | 160 ms = 76.1 Mpx/s (calm readings ~100–105) | ~50 ms | **~2–3.2×** (was 4.1× this morning) |

Decoder day ledger (all byte-identical): MC coalescing 1.86× → Arc DPB ~1.3×
→ B-tile deblock + zero-block recon ~1.05× → pad-once MC ~1.08× → windowed
CABAC ~1.05×-class = **264 → ~100 ms cumulative (~2.6×)** on real streams.
