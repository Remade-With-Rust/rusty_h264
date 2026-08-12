# WHYS — decoder performance continuation (2026-08-10)

Unknown: *what is the honest gap vs ffmpeg today, and what owns it — assuming every
prior instrument and every prior number is guilty until re-proven?*

This continues `docs/WHYS-decoder-perf.md`. That file banked real wins and also
shipped conclusions that later parts of itself withdrew (EDC pixel-share 10–31%,
item-5 closed then reopened, standing ratios that moved with harness defects).
The continuation starts at **depth 6 again**, on the harness that produces the
standing number, before any new brick.

Run under `codec-measurement` + `codec-six-whys-unknowns`. Counts before times.

---

## D6 — the race harness (2026-08-10)

### D6-H1 — the committed `bench/ffmpeg_race.ps1` did not match its own comments

Audited against the file on disk (not the 2026-08-09 baseline prose):

| claim in comments / baseline notes | code on disk |
|---|---|
| "OUTPUT GOES TO NUL, both sides" | timed ours via **CLI** `decode --out $TEMP\race_us.yuv`; ffmpeg `--out $TEMP\race_ff.yuv` |
| ffmpeg `-threads 1` | **absent** — default multi-thread ffmpeg |
| pin + High priority | **absent** — plain `Process::Start` |
| correctness gate before clock | **absent** |
| frame-count work parity | **absent** |
| timed arm = `decode_bench` (drop pictures) | timed arm = **CLI** `decode_stream` → `Vec<YuvFrame>` → write |

`cmd_decode` still accumulates every frame then writes. That is the Part-1 D6b
defect (38% output path) reintroduced into the script that was supposed to bury it.

**COUNTED:** one read of the script. No timing required to refute the standing
claim's provenance.

**ANSWER:** the 2026-08-09 baseline file's "four defects fixed today" either
describes a script that was never committed, or a script that was later reverted.
Either way the committed race was not measurement-clean. **Any ratio quoted from
it is void until re-run under the rewrite.**

**STATUS:** closed — harness rewritten (same path `bench/ffmpeg_race.ps1`).

### D6-H2 — desired state (now enforced by the rewrite)

1. Correctness: SHA256 of first `ProbeFrames` display pictures, ours (`decode_bench
   maxf= out=`) vs ffmpeg (`-frames:v -f rawvideo`), before any timed pair.
2. Work parity: full-stream `frames=` from `decode_bench` equals ffmpeg `frame=`.
3. Timed ours = `decode_bench` reps=1 (AU decode, drop pictures).
4. Timed ffmpeg = `-threads 1 -f null -`.
5. Pin both to one core (`RACE_CORE`, default affinity 4), High priority.
6. CPU time; print **cores-busy = cpu/wall** every pair / median.
7. ABBA; paired wins + z; NULL arm (`decode_bench` vs self) before the race.
8. Refuse / flag median arm < 15 s; flag cores-busy far from 1.0.
9. ASCII-only script (PS 5.1 ANSI parse footgun).

`decode_bench` gained `maxf=N` and `out=path` for the gate only — not used on the
clock.

### D6-H3 — null arm + standing re-baseline (short shields, 1260f)

Clean race 2026-08-10 20:27, Pairs=5, null arm on, gate OK, work parity 1260=1260,
cores-busy ~0.98–0.99 both sides:

| tier | rusty ms | ffmpeg ms | ratio | wins | z |
|---|---:|---:|---:|---:|---:|
| cavlc | 6000 | 2500 | **2.388×** | 5/5 | 2.24 |
| main | 7344 | 3219 | **2.286×** | 5/5 | 2.24 |
| high | 7922 | 3344 | **2.363×** | 5/5 | 2.24 |

NULL median = **1.0108**. All three tiers flagged `!!SHORT` (ffmpeg median < 15 s).
Ratios are directionally trustworthy (null floor 1%, cores~1, ABBA) but not yet a
crates.io standing claim until the long corpus clears 15 s.

Void 2026-08-09 script claimed 1.840 / 1.800 / 1.805 — **flattered by ~0.5×** vs
this clean instrument (CLI YUV write + multi-thread ffmpeg).

### D6-H4 — open instrument risks still to prove

- [x] Display-order probe (D6-H5): was decode-order vs display-order; fixed.
- [x] Gate pixel match on main/high after D6-H5 fix.
- [ ] Affinity under job objects / Windows Terminal — cores-busy column catches it.
- [ ] Binary freshness: rebuild `decode_bench` before every claim session.
- [ ] Long corpus (>=15 s ffmpeg) standing number (D6-H6 unblocked).

### D6-H5 — gate false-positive: decode order vs display order

The first smoke run: **cavlc OK**, **main PROBE PIXEL MISMATCH**. Digging:

- Frames 0–2 matched ffmpeg byte-for-byte.
- Frame 3 differed in ~1.2M of 1.38M bytes — not a 1-pixel bug.
- `ours[5]` hash == `ffmpeg[3]` hash; pattern is classic B-reorder.

**COUNTED:** `Decoder::decode` returns **decode order**; `decode_stream` sorts by
POC into **display order**. ffmpeg emits display order. Gate wrote decode order.

**FIX:** `out=` path emits display-order (GOP POC sort, early-stop). Timed path
still uses `decode` + drop.

**STATUS:** closed.

### D6-H6 — WORK-phase hang: redirected-pipe deadlock on ffmpeg stderr

The 8x long race sat on `WORK: frame counts...` for >67 minutes. ffmpeg PID
had **frozen CPU at ~19 s** — not slow, deadlocked.

**COUNTED:** `Get-FfFrames` used `-Capture` + `-loglevel info`, then sequential
`stdout.ReadToEnd()` before `stderr.ReadToEnd()`. Per-frame progress on stderr
filled the OS pipe; ffmpeg blocked on write; harness blocked on stdout.

**FIX:** concurrent `ReadToEndAsync` when Capture is set; `Get-FfFrames` uses
`-progress <file> -nostats -loglevel error` with **no redirected pipes**.
Smoke: long8 cavlc → frames=10080 in 19.7 s wall, exit 0.

**STATUS:** closed in harness; long standing race re-run pending.

---

## D1 — is the gap real? (re-ask after D6)

**Yes.** Long8 standing race 2026-08-11 (`bench/baselines/ffmpeg_race_2026-08-11.txt`):

| tier | ratio | wins | cores |
|---|---:|---:|---:|
| cavlc | **2.257×** | 5/5 | 0.99/0.99 |
| main | **2.107×** | 5/5 | 0.99/0.99 |
| high | **2.219×** | 5/5 | 0.99/0.99 |

10080 frames, both arms ≥19 s, work parity OK. Void 08-09 ~1.80× is dead. Honest
gap is **~2.1–2.3×**, uniform across tiers — same structural signature as WHYS
Part 18/27, not a single-kernel story.

Campaign bricks may proceed under this number.

---

## Campaign target (after D6 closes)

Prioritized by the Part 5 arithmetic (pixel pipeline ceiling ~18% ⇒ SIMD cannot
reach 1.3× alone):

1. **Frame / slice threading** — parallel unit includes parse; EDC seam prize is
   bounded (~1.09× ceiling at p≈0.16).
2. **Per-MB orchestration / neighbour locality** (scan8-class) — the diffuse
   residue that ablation could not name as a kernel.
3. **Entropy residual loops** — not the CABAC engine (near parity on ns/bin).
4. Content-gated EDC_MT only with a runtime proxy (D11 open, modest prize).

No brick until D6-H3 has numbers and the null arm is printed.

---

### D6-H5 — gate false-positive: decode order vs display order

The first smoke run: **cavlc OK**, **main PROBE PIXEL MISMATCH**. Digging:

- Frames 0–2 matched ffmpeg byte-for-byte.
- Frame 3 differed in ~1.2M of 1.38M bytes — not a 1-pixel bug.
- `ours[5]` hash == `ffmpeg[3]` hash; multisets of the first 6 were **not** equal
  under a pure swap, but the pattern is classic B-reorder.

**COUNTED:** `Decoder::decode` returns pictures in **decode order**;
`Decoder::decode_stream` sorts each GOP by POC into **display order** (documented
in `lib.rs`). ffmpeg `-f rawvideo` emits display order. The gate wrote `decode()`
order via `out=` and SHA'd against ffmpeg — instrument defect, not a decoder
regression.

**FIX:** `out=` path now goes through `decode_stream` (display order). Timed path
still uses `decode` + drop (order irrelevant when discarding).

**STATUS:** closed in code; re-smoke pending.

---

### D6-H6 — WORK-phase hang: redirected-pipe deadlock on ffmpeg stderr

The 8x long race sat on `WORK: frame counts...` for >67 minutes. ffmpeg PID
had **frozen CPU at ~19 s** — not slow, deadlocked.

**COUNTED:** `Get-FfFrames` called `Invoke-Pinned -Capture` with
`-loglevel info`, then `stdout.ReadToEnd()` *before* `stderr.ReadToEnd()`.
ffmpeg emits a progress line per frame on stderr; ~10k frames fill the OS pipe;
ffmpeg blocks on write; harness blocks on stdout. Classic.

**FIX:**
1. Concurrent `ReadToEndAsync` on both pipes when Capture is set.
2. `Get-FfFrames` no longer captures pipes at all: `-loglevel error -nostats
   -progress <file>`, parse `frame=` from the progress file.

**STATUS:** fixed in `bench/ffmpeg_race.ps1`; long race re-run pending.

---

## D9b/D9c/D13 — ship-today bricks (2026-08-11)

After D6 standing numbers, preferred order is frame/slice MT → orchestration →
entropy residuals. Same-day clock work started with unfinished D9 consumer half.

### Landed (byte-identical vs ffmpeg probe, cavlc/main/high)

1. **D9b** — `recon_p_inter_nores`: InterNoRes consumer no longer `to_full()`
   (memset ~2.5 KB zeros) + `add_inter_residual` zero walk. Shared
   `coalesce_p_inter_mc` + plane copy (B_Skip pattern).
2. **D9c** — CABAC `cbp==0` early-out before coeff-array stack init when
   `NORES` on; ships `InterNoRes` / calls nores recon directly.
3. **CABAC `MB_KIND_INTER_UNIFORM`** for `mbt==0` (sibling of CAVLC P_16x16).
4. **D13** — skip B-only CABAC neighbour grids (`mb_ref1`/`mb_mvd1`/`mb_direct`,
   ~292 KB @720p) on P/I slices. `RS_H264_FAT_SLICE=1` for A/B.

### Clock (pinned ABBA CPU, shields 1260f)

| brick | median | wins | null | verdict |
|---|---|---|---|---|
| D9 package (`nores=1` vs `0`) | ~1.00–1.01 | noise | ~1.01 | under floor — D9 was already channel-neutral on single-core CPU |
| D13 P-gate (`fatslice=0` vs `1`) | 1.0089 | 8/11 z=1.51 | 1.0078 | **matches null** — not bankable |
| D13b TLS slice pool (tried, **reverted**) | 0.9989 | 4/11 | — | refill still zeros; no page-fault prize once warm |

**Counter keep:** D9b/D9c remove ~2.5 KB materialization on the 13–37% nores
share; D13 removes ~292 KB alloc on every P/I CABAC slice. Default ON.

---

## D0-FMT — Phase 0 counts (campaign #1, 2026-08-11)

Instrument: `examples/fmt_census.rs` (header parse + unit-work discrete-event
sim of a **full-ref barrier**: a picture starts when the last `max_num_ref_frames`
prior reference pictures have finished).

| stream | pics | slices/pic | multi-slice | I/P/B | ceiling | max_inflight |
|---|---:|---:|---:|---|---:|---:|
| shields main | 1260 | **1.00** | 0% | 42/336/882 | **1.875×** | 3 |
| shields high | 1260 | **1.00** | 0% | 2/378/880 | **1.813×** | 3 |
| shields cavlc | 1260 | **1.00** | 0% | 42/1218/0 | **1.000×** | 1 |

**ANSWER:** Slice-MT is **refuted** as primary lever on this corpus (1 slice/pic).
Full-ref barrier Phase A has ~1.8× unit-work ceiling on B-heavy main/high and
**zero** overlap on CAVLC (every pic is a ref). Phase A is a scaffold; Phase B
row-progress is required for CAVLC and for closing on ffmpeg's ~1.9×/core.

Measurement gate for this campaign: [`bench/pinmt.ps1`](../bench/pinmt.ps1)
(WALL + CPU, multi-core mask) — **not** the 1T `ffmpeg_race` CPU ratio.

---

## D1-FMT — Phase A frame-MT (full-ref barrier, 2026-08-11)

**Knob:** `decode_bench fthreads=N` → `RS_H264_FRAME_THREADS` /
`Decoder::decode_stream_threaded`. `fthreads=0/1` = serial (byte-identity oracle).

**Correctness:** `fthreads=0` vs `fthreads=2` SHA-identical on shields main
(maxf=15 YUV file + maxf=120 in-memory hash).

**pinmt** (mask=20, two P-cores, ABBA, N=11) on `_dprof/shields__main.264`:

| arm | wall median | cpu median |
|---|---:|---:|
| fthreads=2 | 6846 ms | 8031 ms |
| fthreads=1 | 7852 ms | 8109 ms |

- WALL ratio ft2/ft1 = **0.881×** (ft2 faster **11/11**, **z=3.32**)
- CPU ratio ≈ **1.00×** — not spin-shaped
- Null arm ft2 vs ft2: WALL ≈0.98× (instrument OK)

**BANK:** modest Phase A wall win on main (~1.13×). Matches “scaffold with real
overlap on B-heavy GOP”; still well below ffmpeg’s ~1.9×/core and the ~1.8×
unit-work ceiling (Phase B early-start is the remaining gap).

**vs ffmpeg `-threads 2` (single-shot wall, same mask, not pinmt):** ours 6805 ms /
ffmpeg 2263 ms ≈ **3.0×** — expected given the standing 1T gap; Phase B early-start
is what targets ffmpeg’s per-core scaling, not Phase A alone.

**Phase B status:** `ready_rows` + `luma_guard`/`chroma_guard` MC waits + row-hook
publish are wired. Early-start (submit while a ref is in-flight) is **not**
enabled yet — progress-Arc metadata (`frame_num`/`mv`) must be interior-mutable
before `RS_H264_ROW_PROGRESS=1` loosens the barrier (attempt hung / Truncated on
shields main). Scheduler stays full-ref barrier.

---

## D2-FMT — Phase B early-start attempt (2026-08-11)

**Shipped (opt-in):** freeze finished refs off `live` RwLock, Condvar park on
`ready_rows`, MB-row MC watermark (`set_mc_row_need`), coarse strip publish,
early-start lead gate, `RS_H264_ROW_PUB=1` for incremental planes (default off).

**Correctness:** serial / ft2 / rowprog identity OK (YUV + hash).

**pinmt** shields main (early-start, strip pub off):

| compare | WALL | CPU | note |
|---|---:|---:|
| PB vs Phase A (`rowprog=0`) | **1.062×** (0/11) | 1.18× | **not banked** — regresses PA |
| PB vs ft1 | **0.941×** (11/11) | 1.18× | beats 1T, worse than PA’s 0.881× |
| ft4 vs ft2 (mask=85, 4 P-cores) | **0.771×** (7/7) | ~1.0× | **scales** — bank as thread count |
| vs ffmpeg `-threads 2` (smoke) | ~3.2× | — | 1T gap unchanged |

**Default:** `RS_H264_ROW_PROGRESS` **OFF** (Phase A barrier). Opt in with
`rowprog=1`. Slice-MT still refuted (D0-FMT slices/pic=1.0).

---



---

## D3-1T — single-core signal / content-gate campaign (2026-08-11)

**Pivot:** Frame-MT Phase B paused (not beating Phase A on 2T wall). Focus returns to
the **~2.1–2.3×** 1T gap vs ffmpeg. Gate = **byte-identical** (decoder quality =
pixels). Cheap paths: same ops, or content-dispatch between two identical-output arms.

**Instrument:** \decode_bench\ + \sm,profile\ on \_dprof/shields__main.264\.

### Stage picture (deployment signal)

| lever | share | notes |
|---|---:|---|
| **mgmt/other** | **~46%** | top-level residue — name next |
| entropy | ~24% | CABAC (56% renorm in census) |
| **dec-row-hook** (nested) | **~20%** | derive_bs + filter_row + EdcMsg::Row |
| **dec-mb-B** (nested) | **~39%** | B dominates main |
| b-mc / b-direct | ~17% / ~12% | bi-pred + direct |
| inter-mc | ~11% | 16×16 quarter ~55% of MC cycles |
| deblock | ~6% | + deb:derive |
| resid-add / mc-stage | ~8% each | |

Synthetic CAVLC \profile_decode\ mis-ranks (I-heavy) — do not steer from it.

### Defaults already banked (opt out with \=0\)

EDC, ROWDB, NORES, BATCH, content-gated EDC_MT, `global-alloc`, `asm`.
`ROW_PROGRESS` stays OFF (Phase B).

### Bricks executed (2026-08-11)

| # | brick | result |
|---|---|---|
| 1 | **dec-row-hook** early-out | Mid-row no-op (scope only on row crossings). Identity OK. **pinvs early/eager = 1.002× (z=0.30)** — clock noise; keep as profile hygiene (calls 55440 ≈ rows, was per-MB self-tax). `rowhook=eager` / `RS_H264_ROWHOOK_EAGER=1` = oracle. |
| 2 | **B-path glue** | **D9c-B:** coded B `cbp==0` → plane-copy / `EdcJob::BSkip` (no 2.5 KB coeff init, no fat `BJob`). + `col_zero` 8×8 hoist under `direct_8x8_inference`; `b_set_motion` row `fill`. Identity cavlc/main/high vs `nores=0`. **pinvs nores/fat = 0.977× (z=−2.71, 10/11)** — bank under existing `NORES` default (P D9c alone was previously noise). |
| 3 | **Entropy residual loops** | **Exhausted** as loop algorithm (sparse sigmap + level-over-`pos[]` already in). Remaining ROI was the B zero-init (landed in #2). UEG bypass batch = secondary, not chased. |
| 4 | **Name 46% OTHER** | Top-level OTHER ~45% is not a mystery kernel. Leaves subtract entropy~25% / inter-mc~11% / deblock~7% / syntax~5%. INFO names the in-OTHER orchestration: **row-hook ~20%**, **loop-glue ~26%** (mostly hook), B nested (b-mc~17%, b-direct~12%), resid-add/mc-stage ~9% each. Dump now prints `OTHER named` reconciliation. Remainder ≈ timer tax + tiny parse glue. |
| 5 | **16×16-quarter MC** | **Fused one-filter + HV two-filter qpel** (`mc_hor_qpel`/`mc_ver_qpel`/`mc_ver02_avg`): kill scratch store(s). Identity OK (`qpel=compose` oracle). **pinvs fused/compose = 0.995× (z=−1.51)** — BELOW-RESOLUTION, keep as default (not slower; matches openh264 McHorVer* shape). Centre-adjacent two-filter still compose. Leaf still soft vs ffmpeg fused put; orchestration owns the 2.1× gap. |
| 6 | **scan8 / neighbour rewalk** | **Full scan8 MV cache REFUTED** (pinvs scan8/grid **1.13×**, 11/11, z=3.32) — grids already L1; halo load/fill is pure tax. H-39 / decode-locality-plan Phase 3 still hold (`neighbors` stage 0.3%). **Whole-MB direct memo REFUTED** (7/7 slower) — B_Skip 16×16 pays store tax for no saved walk. **Kept:** B_8x8 spatial-direct A/B/C hoist (`b_direct_nbrs` once if any sub is direct). Identity cavlc/main/high vs `dmemo=0`. **pinvs hoist/rewalk = 0.998× (z=−0.90, 4/11)** — below resolution; keep (strictly ≤ walks, zero tax on 16×16). `dmemo=0` / `RS_H264_DIRECT_MEMO=0` = oracle. CABAC `CACHE30` already covers mvd/ref ctxInc. |

**STATUS:** D3-1T bricks 1–6 executed; #2 banked; #5–#6 kept below-resolution. Scan8-as-locality closed.

---

## D4-FMT — the parallel unit is the picture (2026-08-11)

ffmpeg `-threads N` is **not** “split parse from recon.” It is N copies of
the whole picture function. Ours nested the wrong function inside that.

### Function-to-function

| ffmpeg (one frame worker) | ours (wrong) | ours (now) |
|---|---|---|
| `ff_h264_execute_decode_slices` / frame thread | `frame_mt::decode_pic_detached` | same |
| `decode_slice_header` | `Decoder::decode` → slice header | same |
| `ff_h264_decode_mb_cabac` / `decode_mb_cavlc` | `decode_slice_*_inner` (parse) | same thread |
| `hl_decode_mb` + `h264dsp.*` | **`edc_worker` / `PixelCtx::recon_*`** (other thread) | **`decode_slice_*_inner` inline recon** |
| `ff_thread_report_progress` (row) | `RefFrame::publish_ready_rows` | unchanged (Phase B) |
| `ff_thread_await_progress` (MC) | `luma_guard` / `wait_ready_rows` | unchanged |

The nested `edc_worker` was `hl_decode_mb` ripped onto a second thread
while the picture thread only parsed. That is a pipeline, not frame-MT.
Amdahl on the pixel half (~16%) caps it at ~1.09×; on a 1T pin it is two
threads thrashing one core (D6/D7, 1.25–1.45× CPU).

**COUNTED:** `edc_dispatch` fires on shields main (CABAC ∧ 720p ∧ bits/MB>38.4).
The 1T ffmpeg race therefore spawned `edc_worker` under a 1-core pin.

### Resolve

- `edc_spawn_worker`: **never** if `FRAME_THREADS>1` (picture thread owns recon).
- Default (unset) = **inline** — ffmpeg `-threads 1` shape.
- `RS_H264_EDC_MT=1` = old nested worker (oracle), still forbidden under frame-MT.
- `=auto` = the old content gate (`edc_dispatch`), 1T only.

E1 same-thread job queue (`edc_on`, flush before intra/row) is unchanged:
that is loop fission, not a thread.

### Gates (2026-08-11)

**Identity** cavlc/main/high: inline == `mt=1` == `fthreads=2`
(`69ede9f8…` / `9c518166…` / `6bf29ef0…`).

**1T pinvs** (pinned 1 core, N=11) inline / `mt=1` = **1.008×** (z=1.51, 8/11
edcmt). Below bar — loop fission still slightly cheaper CPU when the worker
is forced onto the same core. **Preventing thrash is a race-correctness
benefit, not a banked 1T CPU win:** CPU is blind to two threads serialised
onto one core; the tell is cores-busy / wall (now printed every ABBA pair).
Default stays inline: like-for-like with `ffmpeg -threads 1`. `mt=1` remains
the oracle.

**pinmt ft2/ft1:** not quoted. Box was ~4× the D1-FMT floor (wall ~33 s vs
D1 6.8–7.9 s); 3 pairs of noise, aborted. Re-run on a quiet box; D1-FMT
0.881× is still the standing Phase A number, now without a nested
`edc_worker` stealing the second core.


## D5-GLUE — compute errors in the call graph (2026-08-11)

The 2× ffmpeg gap is still function deployment, not Rust. These were **wrong
calls / unused producers / per-row allocs**, not new kernels.

| glue | what was wrong | fix |
|---|---|---|
| `mb_kind` | Parse wrote SKIP / INTER_UNIFORM; `derive_bs_row` passed `kind: &[]` and always `pack_mb`+`derive_mb_records` | Kind-fast for Intra/Skip/InterUniform; packed path for Inter/UNSET |
| Intra class | `MB_KIND_INTRA` existed; **no producer**. ~14% of MBs (H-48: 66k/475k) paid the 24-block gather for a constant 4/3 pattern | Write at `decode_intra_mb` (CAVLC) and CABAC inlined I path |
| POC maps | `refs.iter().map(pic_poc).collect()` **every MB row** (≤16 i32, same for the slice) | `ref_poc0`/`ref_poc1` filled in `begin_slice` / `set_b_context` |
| High nnz clone | `deblock()` cloned+rewrote whole-frame `nnz_y` even when rowdb already stored bS | Skip clone/grids when `rowdb` — filter reads `bs`+`t8`+qp |
| 1T MC wait | `set_mc_row_need` TLS + `luma_guard` `ready_rows` on refs with `live=None` | No-op TLS unless `row_progress_on`; guards skip wait if `live.is_none()` |
| `RH264_DUMP_MB` | `var_os` every picture | `OnceLock` |

**Identity** cavlc/main/high = `69ede9f8514942e7` / `9c51816659e8d68f` /
`6bf29ef0376b7a6b` (1260f). Not yet pinvs — box loaded; bank only if 1T
CPU wins.

Still UNSET (correct): `B_Skip`/`B_Direct`, `P_16x8`/`P_8x16`/`P_8x8`.
`derive_mb_kind_into` still expands via `MbBs` (stores ~0.5%; seam only).

Next glue to hammer if this is below the null floor: leftover
`set_mc_row_need` call sites (now no-ops on 1T); getenv `RS_H264_BS_PRE`
on the rowdb-off path; duplicate `b_mc` on `FrameDecoder` vs `PixelCtx`.

---

## Log

| date | event |
|---|---|
| 2026-08-10 | Opened continuation. Refuted committed race script by reading it. Rewrote harness + `decode_bench` gate helpers. |
| 2026-08-10 | Smoke gate: main "mismatch" = decode-order dump vs display-order ffmpeg (D6-H5). Fixed `out=` to display-order emit. |
| 2026-08-10 | Clean short race: cavlc/main/high = 2.39/2.29/2.36x, null=1.011, cores~1.0, !!SHORT. Old ~1.80x void. |
| 2026-08-10 | Long8 race hung 67m on WORK (D6-H6 pipe deadlock). Killed. Fixed progress-file frame count. |
| 2026-08-11 | D9b/D9c + CABAC INTER_UNIFORM + D13 P-gate. Gates green. Clock under null floor. TLS slice pool tried+reverted. Frame-MT next. |
| 2026-08-11 | D0-FMT census: main/high ceiling ~1.8x full-ref; cavlc 1.0x; slices/pic=1.0. Slice-MT weak. |
| 2026-08-11 | D1-FMT Phase A: fthreads=2 vs 1 pinmt WALL 0.881x (11/11, z=3.32), CPU~1.0. Banked. Phase B early-start deferred. |
| 2026-08-11 | D2-FMT Phase B: identity OK; PB vs PA 1.062x (not banked); ft4/ft2 0.771x on 4 cores; rowprog default OFF. |
| 2026-08-11 | D3-1T opened: 1T content-gate campaign; shields main profile ranks residue/row-hook/B-path. |
| 2026-08-11 | D3-1T: row-hook early-out (hygiene); D9c-B banked 0.977x; OTHER named; entropy loops exhausted; fused qpel kept below-res (0.995x). |
| 2026-08-11 | Scan8 MV cache 1.13x slower — reverted. Whole-MB direct memo 7/7 slower — reverted. B_8x8 neighbour hoist kept (0.998x, identity OK). |
| 2026-08-11 | D4-FMT: edc_worker default OFF; forbidden under frame-MT. Picture thread owns parse+recon (ffmpeg shape). |
| 2026-08-11 | D5-GLUE: kind into derive_bs_row + Intra producer + POC hoist + 1T MC skip + rowdb nnz skip. Identity OK. Not banked. |
| 2026-08-11 | D5b-GLUE: wait_refs once/MB; strip FrameDecoder set_mc_row_need; kind→MbBs direct; kind_into i32 direct; BS_PRE OnceLock. |