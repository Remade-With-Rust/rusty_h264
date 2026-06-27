---
name: decoder-pivot
description: Project refocus onto a general, hardened pure-Rust Baseline DECODER (untrusted-input = where Rust pays off); encoder kept, not discarded
metadata:
  type: project
---

**Direction (2026-06-26):** user judged the encoder effort had hit diminishing
returns and pivoted emphasis to the **decoder** — the security-critical half (it
eats untrusted input, where memory-safety prevents the CVE class C decoders rack
up). The pure-Rust encoder is **kept** as the conformant counterpart, NOT thrown
away. Explicitly decided **against** FFI-linking openh264's encoder (would put C
back in the build = lose the whole differentiator); instead use openh264/ffmpeg
only as an *external* source of test bitstreams.

**Corpus oracle:** Cisco ships **50 `.264` streams in `openh264/res/`** (JVT
conformance + Cisco clips + deliberately-corrupted `*_LOST`/`Error_*`). The
classifier `cargo run -p rusty_h264-decoder --example corpus -- <dir>` buckets
each as decoded / gracefully-rejected / PANICKED. Gap analysis written to
[[decoder-gap-analysis]] → `docs/decoder-gap-analysis.md`.

**Done this session (all verified):**
- HARDENING: corpus panics **11 → 0**. Profile-gate SPS (reject High/Main prefix
  before it misparses), every `debug_assert!` → `DecodeError`, panic-free
  `read_bits`/`read_ue`, level-prefix capped, **residual coeffs clamped to i16**
  (the transform-multiply-overflow root cause), frame-size/crop bounds. New
  `crates/rusty_h264-decoder/tests/fuzz_no_panic.rs` mutation fuzzer (encoder
  seeds + random) — 0 panics / ~50k inputs; it found 5 sites the corpus missed.
- GENERALIZE: `mb_qp_delta` accumulation (`QPy=(prev+delta+52)%52`, was IGNORED)
  + `chroma_qp_index_offset` (was read & dropped) now applied. Byte-identical on
  our constant-QP streams (round-trip still bit-exact); logic unit-tested.

**Baseline feature build (2026-06-26): oracle MATCH 2 → 20.** Implemented +
oracle-validated (all in decoder, round-trip + fuzz stay green, 0 panics):
non-zero deblock offsets; **I_PCM**; POC type-1/bottom-field slice-header parse
(desync fix); **P_8x8** sub-partitions; **ref_pic_list_modification** (short-term);
**MMCO** short-term (op1 unref, op5 reset); **multiple slices per picture**
(picture assembly across slice NALs + slice-boundary neighbor availability via
`slice_first_mb`; single-slice unchanged by construction — `jm_1080p_allslice`,
`BA1_FT_C` 45MB match); **per-MB-QP deblocking** (filter averages each edge's two
MB QPs — `deblock::filter_frame` now takes a per-MB qp grid + chroma offset;
encoder passes a uniform grid = identity). Tooling: `examples/oracle.rs` is THE
correctness gate.

**COMPLETE: oracle 2 → 31 MATCH = every clean in-scope CBP stream is bit-exact.**
Full primitive set. Beyond the 22-mark, the wins that closed it:
- **constrained_intra_pred** (inter neighbors unavailable for intra pred/mode/corner;
  helper `intra_nbr_ok`) → fixed BOTH `CI*` streams (DIFF = recon values, not bits).
- **nal_ref_idc honored** — `dec_ref_pic_marking` is present ONLY when nal_ref_idc!=0;
  non-reference pics output but NOT added to DPB. THE big one: fixed 6 mid-frame
  desyncs (`Adobe_PDF`, `NRF_MW_E`, `Static`, `Zhling`, `test_vd_1d/rc`) — they were
  reading a marking that wasn't there. (Lesson: mid-frame P-slice desync on a
  many-MB-clean stream ⇒ suspect a slice-header field gated on a flag we ignored.)
- **Multiple parameter sets** — SPS/PPS keyed by id (HashMap), slice resolves PPS by
  pic_parameter_set_id → SPS by its seq_parameter_set_id. Fixed `MPS_MW_A`.
- long-term refs, redundant_pic_cnt, ref_idx-by-num_ref_idx_l0_active (earlier).

**Scope expansion underway (beyond Constrained Baseline).** Committed this phase:
POC derivation + display-order output; `decode_stream` public API (productization);
`gaps_in_frame_num` + conformance notes; **B-slices** — dual POC-ordered ref lists,
all B mb_types (incl. `B_8x8`), bi-prediction, spatial direct w/ colZeroFlag (uses
`RefFrame`'s stored L0 motion field), per-list MV prediction, and B-aware deblock
boundary strength by reference-picture identity (POC sets, not list indices).
**`Cisco_Adobe_PDF_CAVLC_Bframe` decodes BIT-EXACT (33 MATCH range)**; the whole B
path is proven. `Men_whisper` is ~99.9% (a narrow B `8x16` L1 MV-prediction edge
case — needs a per-MB MV trace vs h264dec). Remaining scope-expansion: that edge
case, **High profile** (8×8 transform/CAVLC/intra + scaling lists + High SPS
prefix), and **CABAC** (the arithmetic engine) — each a large dedicated build.
B streams targeted: `Cisco_*_CAVLC_Bframe` (Main 77, CAVLC, spatial direct,
bipred_idc 0 → simple average, 1 ref each way — no temporal direct needed).

**High profile started: SPS prefix (4:2:0/8-bit) + scaling-list weighted dequant.**
`test_scalinglist_jm` decodes BIT-EXACT (**33 MATCH**). Scaling lists parsed with
fall-back rule A (default Intra/Inter matrices, prev-list inheritance), un-zig-zag
to raster, weighted dequant in transform.rs (`dequantize_weighted` +
`inverse_quant_*_dc_weighted`; flat lists keep the fast path so Baseline is
byte-identical). Per-block list select by component×intra/inter (Y/Cb/Cr).
**8×8 transform DONE + VID IDR bit-exact.** Built the High 8×8 path: inverse 8×8
transform + dequant (`transform.rs`, 6-group `normAdjust8x8`, unit-tested),
`intra8x8_pred` (filtered refs + 9 modes, `predict.rs`), `decode_i8x8` (4 luma
8×8 blocks, 8×8 CAVLC = 4 interleaved 4×4 sub-blocks `4k+s`, un-zig-zag Table
8-12), PPS High ext (`transform_8x8_mode_flag`, `pic_scaling_matrix` rule B,
`second_chroma_qp_index_offset`). **Bug found by isolated validation: deblocking
must SKIP the internal 4×4 luma edges (be=1,3) of 8×8-transform MBs** — they're
not transform boundaries (spec §8.7); we were filtering them (antisymmetric ±1-2,
center-clustered). Fixed via `BlockInfo.t8x8` per-MB flag. **VID IDR now BIT-EXACT.**

**VALIDATION METHOD for partial-decode streams:** truncate the Annex-B stream to
the first N access units (split on 00 00 01, stop at first VCL nut∈1..4 after the
IDR — see `scratchpad/trunc.py`), decode both with h264dec.exe and our CLI
(`decode --width --height`), `cmp` the YUV. Diff-stat helper `scratchpad/.../yd.py`
(use **Windows paths** with `python`, not git-bash `/c/` paths). This let me
validate the 8×8 IDR without a full-stream decode (VID's later frames need more).

**Remaining for VID full-decode (CAVLC clips):** explicit weighted pred (P-slices,
`weighted_pred=1`), implicit B weighting (idc=2, verify vs simple-avg), temporal
direct (direct_spatial=0), 8×8 inter (`transform_size_8x8` after CBP). Then the
`*cabac*` VIDs + `QCIF` need **CABAC**. No remaining stream is a single-piece win.

**The Constrained-Baseline 18 REJECT + 1 DIFF were all genuinely OUT of CBP,
correctly refused, never misparsed:** CABAC (5), B-slices (2), High/4:2:2 profile
(8, incl. a High all-I_PCM clip — profile_idc 100), SVC (1 DIFF — type-20 scalable
slice, no base picture), deliberately-corrupted error-resilience clips (3 `*_LOST`/
`Error_*`, need error concealment). None are codec primitives.

**YUV ORACLE BUILT + WIRED (2026-06-26):** built `h264dec.exe` via
`ninja -C builddir_rs codec/console/dec/h264dec.exe` (ninja.exe lives in the pip
`ninja` package's `Scripts/` dir — `python -c "import ninja,os;print(os.path.join(ninja.BIN_DIR,'ninja.exe'))"`;
builddir_rs was already configured from the encoder build). `h264dec in.264 out.yuv`
writes concatenated I420. Harness `examples/oracle.rs` decodes each corpus file
with BOTH our decoder and h264dec and diffs YUV: run with
`H264DEC=.../h264dec.exe cargo run -p rusty_h264-decoder --example oracle -- <res-dir>`.

**Result: 2 MATCH (SVA_BA1_B, SVA_NL1_B bit-exact vs reference), 1 DIFF
(sps_subsetsps = SVC subset-SPS, we emit no frame), 47 rejected, 0 panic.** The
oracle immediately caught a real bug: **deblock default was OFF; spec infers
disable_deblocking_filter_idc=0 (filter ON) when deblocking_filter_control_present_flag=0**
— our encoder always signals it so the round-trip never caught it. Fixed
(`deblock` defaults true in decode_slice). This oracle is now the regression gate
for the big features (P_8x8, multi-slice, ref-list-mod, MMCO, I_PCM): implement →
require MATCH on the relevant res/ streams.
