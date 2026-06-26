# Decoder gap analysis — toward a general Constrained Baseline decoder

The decoder was built to decode **our own encoder's output** as a conformance
oracle. To become a real, embeddable, memory-safe Baseline *decoder* (the
security-critical half — it eats untrusted input) it must decode arbitrary
conformant streams and **never panic** on malformed ones.

This document is the punch-list. It combines a static read of the decoder with
an empirical run over a real-encoder corpus.

## Corpus & method

Oracle corpus: the **50 `.264` streams Cisco ships in `openh264/res/`** — the
JVT conformance suite (`BA*`, `SVA_*`, `CI*`, `MR*`, `NL*`), Cisco clips, and
deliberately-corrupted error-resilience streams (`*_IDR_LOST`, `*_P_LOST`,
`Error_I_P`). Classifier: [`examples/corpus.rs`](../crates/rusty_h264-decoder/examples/corpus.rs)
runs each file through `Decoder::decode` under `catch_unwind` and reports
**decoded / gracefully-rejected / PANICKED**.

```
cargo run -p rusty_h264-decoder --example corpus -- /path/to/openh264/res
```

### Result

| | before | after this pass |
|---|---|---|
| decoded / parsed | 3 | 3 |
| gracefully rejected | 36 | **47** |
| **PANICKED** | **11** | **0** |

A panic on untrusted input is a bug, full stop. The 11 panics are now graceful
errors; the rejects are the feature gaps (many are valid Baseline tools we don't
decode *yet* — see GENERALIZE below).

### Status of this pass

**Done (hardening — verified):**
- Profile-gate the SPS; reject High/Main/4:2:2 prefixes before they misparse.
- Every `debug_assert!`/"only what we emit" → a real `DecodeError` (interlace,
  slice-groups/FMO, `pic_order_cnt_type`, oversized/over-cropped frames).
- Bound input-driven arithmetic: `read_bits`/`read_ue` panic-free, level-prefix
  capped, residual coefficients clamped to the legal 16-bit range (the
  transform-overflow root cause), DPB-dimension allocation bounded.
- `tests/fuzz_no_panic.rs`: a deterministic mutation fuzzer (encoder seeds +
  random buffers) — **0 panics over ~50 k inputs**; found and fixed 5 sites the
  corpus never reached (`read_bits` assert, two Exp-Golomb shift/again overflows,
  two transform multiply overflows).

**Done (generalize — verified by round-trip + unit test):**
- `mb_qp_delta` accumulation (`QPy = (QPy_prev + delta + 52) % 52`) — was silently
  ignored, mis-dequantizing every adaptive-quant (x264 AQ) stream.
- `chroma_qp_index_offset` applied in the luma→chroma QP map — was read and dropped.
- Both stay byte-identical on our constant-QP streams (so the existing bit-exact
  round-trips still pass); the accumulation/clamp logic is covered by a direct
  unit test since the round-trip can't exercise a non-zero delta.

## Panics (HARDENING — must become graceful errors)

| Count | Location | Trigger | Class |
|---|---|---|---|
| 9 | `params.rs:60` `debug_assert!(frame_mbs_only_flag)` | High/Main-profile SPS **misparsed** (we skip the high-profile prefix: `chroma_format_idc`, bit depths, scaling matrices), so every later field shifts and the interlace bit reads garbage. In **release** the assert vanishes → silent corruption / OOB downstream. | parser robustness |
| 2 | `cavlc.rs:645` `zeros_left -= val` | A (mis-parsed) `run_before` exceeds the zeros remaining → unsigned underflow. Reachable from any desynced/garbage residual. | arithmetic safety |

Root causes: (a) **no profile gate** in SPS parsing, and (b) **input-driven
arithmetic without bounds**. Both are squarely "where Rust earns its keep" — they
must return `DecodeError`, not unwind.

## Feature gaps (GENERALIZE — valid Baseline tools we reject)

Counts are how many corpus files each gap blocks (a file can hit several; the
*first* one is what the classifier reports).

| Gap | Files (first-hit) | Spec | Notes |
|---|---|---|---|
| **`P_8x8` sub-partitions** (8×8/8×4/4×8/4×4, `sub_mb_type`) | 9 | §7.3.5.2 | The single biggest feature gap. x264/openh264 use it constantly. Decode side only — we never *emit* it. |
| **`ref_pic_list_modification`** (reference reordering) | 5 | §7.3.3.1 | Currently `Unsupported`. Common in multi-ref streams. |
| **Non-zero deblocking offsets** (`slice_alpha_c0_offset`/`beta`) | 4 | §7.3.3 / §8.7 | We only accept 0/0 (what we emit). JVT `MR*`/`NLMQ*` set them; the deblock filter must honor `tc`/threshold offsets. |
| **`I_PCM`** macroblocks (mb_type 25 / 5+25) | (PCM streams) | §7.3.5 | Raw uncompressed samples; byte-aligned. Trivially in Baseline. |
| **Intra-only mb_types beyond I_4x4/I_16x16** in some streams | 2 (`CVPCMNL1`, `MPS_MW`) | — | Includes I_PCM and edge mb_type handling. |
| **Multiple slices per picture** (`first_mb_in_slice != 0`) | (`jm_1080p_allslice`, others, currently "truncated") | §7.3.3 | We discard `first_mb_in_slice` and decode the whole frame as one slice from MB 0. Real streams split a picture into many slices; neighbor availability must stop at slice boundaries. Structural. |
| **High/Main profile** (CABAC, B-slices, 8×8 transform, scaling lists) | many | — | *Correctly* out of scope for Constrained Baseline — but must be **rejected cleanly**, not misparsed into a panic. |

## "Decode-our-own-output" assumptions found by static read

Each is a place the parser hard-codes what our encoder emits instead of reading
the syntax. (✓ = also causes a panic above.)

**SPS** ([params.rs](../crates/rusty_h264-decoder/src/params.rs)):
- ✓ `profile_idc` read but never gated; high-profile prefix not parsed.
- ✓ `frame_mbs_only_flag` enforced with `debug_assert!` (panics in debug, ignored in release).
- `pic_order_cnt_type == 1` not handled (reads none of its fields → misparse).
- No sanity bound on `pic_width/height_in_mbs` → a hostile SPS can request a huge allocation / size overflow.
- Only one SPS stored (`Option`), not keyed by `seq_parameter_set_id`.

**PPS** ([params.rs](../crates/rusty_h264-decoder/src/params.rs)):
- `num_slice_groups_minus1` enforced with `debug_assert!` (FMO misparsed in release).
- `chroma_qp_index_offset` read but **ignored** in `chroma_qp()` → wrong chroma for streams that set it.
- `constrained_intra_pred_flag` not read/applied → wrong intra availability next to inter MBs.
- `redundant_pic_cnt_present_flag` not read → slice header misparse if set.
- Only one PPS stored.

**Slice header** ([lib.rs](../crates/rusty_h264-decoder/src/lib.rs)):
- `first_mb_in_slice` discarded (see multi-slice gap).
- `ref_pic_list_modification_flag_l0` set → `Unsupported`.
- `adaptive_ref_pic_marking` (MMCO) → `Unsupported`.
- Non-zero deblock offsets → `Unsupported`; `disable_deblocking_filter_idc == 2` (filter-except-slice-edges) not handled.
- `num_ref_idx` override read but not used to bound `ref_idx`.

**Macroblock layer** ([mb16.rs](../crates/rusty_h264-decoder/src/mb16.rs)):
- `mb_qp_delta` read but **ignored** (`_mb_qp_delta`) → every per-MB-QP (adaptive-quant) stream dequantizes at the wrong QP. The spec accumulates `QP_Y = (QP_prev + mb_qp_delta + 52) % 52`.
- `I_PCM` not handled.
- `P_8x8` not handled.

## Priorities

1. ✅ **Harden to zero panics** (the whole point of a Rust decoder) — done; see above.
2. **Cheap correctness wins**: ✅ `mb_qp_delta`, ✅ `chroma_qp_index_offset`; *remaining*
   — non-zero deblock offsets, `I_PCM`, multiple SPS/PPS by id, POC-type-1 fields.
3. **Big features (next)**: `I_PCM`, `P_8x8` sub-partitions, `ref_pic_list_modification`,
   multiple slices per picture, MMCO.

### The YUV oracle (built — `examples/oracle.rs`)

Correctness (not just safety) is validated by decoding each stream with **both**
our decoder and Cisco's `h264dec` reference and diffing the YUV byte-for-byte.

- Reference: built `openh264/builddir_rs/codec/console/dec/h264dec.exe` (ninja;
  recipe in [openh264-baseline-build](../memory/openh264-baseline-build.md), same
  toolchain as the encoder).
- Harness: [`examples/oracle.rs`](../crates/rusty_h264-decoder/examples/oracle.rs):
  ```
  H264DEC=.../h264dec.exe cargo run -p rusty_h264-decoder --example oracle -- /path/to/openh264/res
  ```

**Result: every Baseline stream we fully support is bit-exact to the reference.**

| | initial | after Baseline feature build |
|---|---|---|
| MATCH (bit-exact vs `h264dec`) | 2 | **31** |
| DIFF | 1 | 1 |
| ours-rejected | 47 | 18 |
| ours-panicked | **0** | **0** |

**Every clean in-scope Constrained Baseline stream in the corpus now decodes
bit-exactly against the reference decoder.** The 18 rejects + 1 DIFF are all
genuinely *outside* Constrained Baseline and correctly refused (never misparsed):
**CABAC** (5), **B-slices** (2), **High/4:2:2 profiles** (8 — incl. a High-profile
all-`I_PCM` clip), **SVC** (1, the lone DIFF — a type-20 scalable slice with no
base-layer picture), and **deliberately-corrupted error-resilience clips** (3,
`*_LOST`/`Error_*`, which need error concealment — a non-codec feature).

The 20 bit-exact streams span `I_4x4`/`I_16x16`/`I_PCM`, `P_16x16`/`16x8`/`8x16`/
`P_8x8`, multiple references with list reordering, per-MB QP (multi-QP), non-zero
deblock offsets, and **multi-slice pictures** (`jm_1080p_allslice`, `BA1_FT_C` at
45 MB). The 3 DIFFs: `CI1_FT_B` (P-slice multi-slice edge case), `CI_MW_D`
(P_8x8 multi-ref divergence from frame ~32), `sps_subsetsps` (SVC subset-SPS, we
emit no frame). The 27 rejects are out-of-scope-for-Constrained-Baseline (CABAC,
B-slices, High/4:2:2 profiles) or the remaining in-scope gap, **long-term
references** (the `MR2_*`, `Zhling`, `test_vd_*` streams).

### Baseline features implemented this pass (each oracle-validated)

- **Non-zero deblock offsets** (`slice_alpha_c0`/`beta`) threaded into the filter.
- **`I_PCM`** macroblocks; **POC type-1 / bottom-field** slice-header parsing (a
  desync fix that unblocked the multi-QP streams).
- **`P_8x8`** sub-partitions (8×8 / 8×4 / 4×8 / 4×4, per-sub-partition MV).
- **`ref_pic_list_modification`** (short-term `RefPicList0` reorder by PicNum).
- **Adaptive ref-pic marking (MMCO)** short-term ops (1 = unref, 5 = reset).
- **Multiple slices per picture** — picture assembly across slice NALs with
  slice-boundary-aware neighbor availability (intra pred, MV pred, CAVLC nC,
  deblock); single-slice pictures are unchanged by construction.
- **Per-MB-QP deblocking** — the filter now averages each edge's two MB QPs
  (completing multi-QP support; fixed 4 deblock-on DIFFs). `mb_qp_delta` dequant
  + `chroma_qp_index_offset` from the prior pass.
- **Long-term references** — `LongTermFrameIdx` tracking, MMCO ops 2/3/4/6, IDR
  `long_term_reference_flag`, `ref_pic_list_modification` idc 2; `RefPicList0` is
  short-term (by PicNum desc) then long-term (by idx asc). (`MR2_MW_A`,
  `MR2_TANDBERG_E` MATCH.)
- **`redundant_pic_cnt`** parsed and redundant coded pictures discarded.
- **`ref_idx` coding driven by `num_ref_idx_l0_active`** (not the DPB size) — its
  te(v)/ue(v) form and presence follow the slice's active count, correct even
  when more references are active than pictures yet exist.

- **`constrained_intra_pred_flag`** — under it, intra prediction treats inter
  neighbors as unavailable (samples, mode prediction, and the above-left corner).
  Fixed both `CI*` streams (the divergence was reconstruction values, not bits).
- **`nal_ref_idc`** honored — `dec_ref_pic_marking` is present only for reference
  pictures; non-reference pictures are output but never enter the DPB. This one
  fix resolved six mid-frame desyncs (`Adobe_PDF`, `NRF_MW_E`, `Static`, `Zhling`,
  `test_vd_1d/rc`) — they desynced reading a marking that wasn't there.
- **Multiple parameter sets** — SPS/PPS stored keyed by id; a slice resolves its
  PPS (and thence SPS) by `pic_parameter_set_id`. Fixed `MPS_MW_A`.

With these, the decoder implements the full Constrained Baseline primitive set:
intra (`I_4x4`/`I_16x16`/`I_PCM` + all chroma modes, constrained or not), inter
(`P_16x16`/`16x8`/`8x16`/`P_8x8` all sub-shapes/`P_Skip`), multi-reference with
short- and long-term list reordering, MMCO, `nal_ref_idc`/non-reference pictures,
multiple slices per picture, multiple parameter sets, per-MB QP, and the in-loop
deblocking filter with offsets — all bit-exact vs the reference.

**Bug the oracle caught + fixed:** `SVA_BA1_B` differed in 43 % of bytes from
frame 0. Root cause: when `deblocking_filter_control_present_flag == 0`, the slice
carries no `disable_deblocking_filter_idc` and it is **inferred 0 (filter ON)**
(spec §7.4.3) — but the decoder defaulted the filter OFF. Our own encoder always
signals the control explicitly, so the round-trip never exercised the default;
only a third-party stream did. Fixed (`deblock` defaults `true`); round-trip and
fuzzer stay green, and `SVA_BA1_B` now MATCHES.

This oracle is now the regression mechanism for the big features below: implement,
then require MATCH on the relevant corpus streams (decode → diff vs `h264dec`).

### Conformance & `frame_num` gaps

The `openh264/res/` corpus *is* a JVT conformance-vector subset — `SVA_*`, `BA*`,
`CI*`, `MR*`, `NL*`, `CVPCMNL1_SVA_C` are named after official JVT bitstreams — so
the oracle already exercises a slice of the ITU suite (all in-scope pass). Running
the *complete* JVT/ITU suite is a matter of pointing the oracle at that external
bitstream set (`H264DEC=… cargo run … --example oracle -- <dir>`).

`gaps_in_frame_num` placeholder insertion (spec §8.2.5.2) is implemented:
`Decoder::insert_frame_num_gaps` synthesizes "non-existing" short-term references
(grey-filled, with the skipped `frame_num`s) so the DPB sliding window and
PicNum/ref-list derivation stay correct when `frame_num` skips values. No clean
corpus stream has an actual gap (the `*_LOST` clips do, but those also need error
concealment to match), so it is unit-tested rather than oracle-validated.

The invariant for every change mirrors the encoder's: a decoder change must be
validated against real streams, and **must never panic on any input**.
