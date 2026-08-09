//! Encoder configuration.

use rusty_h264_common::{ChromaFormat, Profile};

/// Speed/quality trade-off, in the spirit of x264's `-preset`. The bitstream is
/// valid (and decodes bit-exactly) either way; only the encoder's effort differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// **Fast** — built to mirror x264's fastest presets: mode decision
    /// by cheap **SAD** estimation (no rate-distortion trial-encoding; SAD
    /// auto-vectorizes to `psadbw`), `P_16x16`-only inter, `I_16x16`-only intra,
    /// and **integer-pel** motion (no sub-pel `mc_luma` interpolation — profiling
    /// showed it was ~55% of the encode). Much faster; larger files, and a little
    /// quality lost on sub-pixel motion (none on integer/screen content).
    Fast,
    /// **Balanced** — [`Fast`](Self::Fast)'s decision path plus **sub-pel motion
    /// refinement**, which `Fast` omits.
    ///
    /// Integer-pel motion cannot track sub-pixel displacement, so on slow pans and
    /// dollies the residual stays large, the intra cost wins, and macroblocks fall
    /// back to intra — which is very expensive. Measured over 4 QPs on four clips,
    /// adding sub-pel to `Fast` is **−42% to −50% BD-rate** (PSNR and SSIM agree)
    /// for ~2.3–3.1× the time. On fine-detail content it beats [`Quality`] on BOTH
    /// size and speed (in_to_tree 26.6 vs 27.3 Mb/s at 7.2× the throughput), because
    /// sub-pel — not the sub-partitions or the RD search — is what that content
    /// needs.
    ///
    /// **This is the default.** Sub-pel costs ~2–3× the time, but a step on
    /// x264's own preset ladder buys ~2–3% BD-rate for ~1.5× — so at −42..−50%
    /// this is dramatically underpriced by comparison. `Fast` remains available
    /// for throughput-critical use.
    #[default]
    Balanced,
    /// **Quality** — full rate-distortion mode decision (every candidate
    /// trial-encoded for real `J = SSD + λ·bits`), `16x8`/`8x16` sub-partitions,
    /// and the full `I_4x4` intra search. Smaller files; much slower.
    Quality,
}

/// Resolution the mb-tree lookahead motion search runs at (speed/quality lever).
/// Measured on CIF (mb-tree BD-rate vs off / encode wall vs FullRes):
/// FullRes mand −0.19% tsrc −1.80% (1.0×) · Hybrid −0.19% / −1.47% (~1.7×) ·
/// HalfRes +0.12% / −1.28% (~4×).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LookaheadMode {
    /// Search AND score intra/inter costs at full resolution. Best quality, slowest
    /// lookahead. The reference against which the others are measured.
    FullRes,
    /// Search the MV on 2×-downsampled planes (cheap), then REFINE + score the final
    /// intra/inter cost at FULL resolution — recovers full-res quality (the half-res
    /// loss was cost accuracy on blurred data, not the MV) at ~1.7× the speed. The
    /// no-regression speed option.
    Hybrid,
    /// **Default** — search AND score at half resolution. Fastest lookahead (~4×), a
    /// small BD-rate cost on fine-detail content (downsampling blurs the cost estimates).
    #[default]
    HalfRes,
}

/// Configuration for an [`crate::Encoder`].
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Picture width in luma samples. Arbitrary (not restricted to /16).
    pub width: usize,
    /// Picture height in luma samples.
    pub height: usize,
    /// Target profile. Only [`Profile::ConstrainedBaseline`] is implemented.
    pub profile: Profile,
    /// Chroma format. Only [`ChromaFormat::Yuv420`] is implemented.
    pub chroma: ChromaFormat,
    /// `level_idc` (e.g. 30 = level 3.0). Caller is responsible for choosing a
    /// level that admits the resolution/bitrate; not yet validated.
    pub level_idc: u8,
    /// Quantization parameter (0..=51). With rate control off this is the fixed
    /// QP for every frame; with it on, the base/fallback QP and `pic_init_qp`.
    pub qp: u8,
    /// Frames between IDR pictures. `1` = all-intra (every frame an IDR).
    pub gop_size: u32,
    /// Target bitrate in bits per second. `0` disables rate control (constant
    /// QP); any positive value enables average-bitrate control, which varies the
    /// per-frame QP around [`qp`](Self::qp) to converge on this rate.
    pub bitrate: u32,
    /// Frame rate (frames per second), used by rate control to turn the bitrate
    /// target into a per-frame bit budget.
    pub framerate: f32,
    /// Number of reference frames the encoder may use for P-pictures (1..=16).
    /// `1` keeps the single-reference bitstream; higher values let P-macroblocks
    /// pick an older reference (`ref_idx`), helping occlusion/periodic motion.
    pub num_ref_frames: u32,
    /// Speed/quality trade-off. Defaults to [`Preset::Balanced`].
    pub preset: Preset,
    /// EXPERIMENT KNOB (hidden): use the asm (dct_four_t4 + quant_four_4x4) fast
    /// path in the P_Skip free-check instead of the scalar twin. Byte-identical
    /// either way; exists so A/B arms interleave in ONE binary (honest thermals).
    #[doc(hidden)]
    pub tune_skip_accel_check: bool,
    /// EXPERIMENT KNOB (hidden): route inter-MB coding through the isolated,
    /// coefficient-fused `encode_inter_mb_v2` path instead of the current
    /// `encode_inter_mb`. Byte-identical output (gated), selectable at runtime so
    /// the two implementations run side-by-side in ONE binary for honest A/B
    /// timing on the coded path. See the `coded_path_ab` test.
    #[doc(hidden)]
    pub coded_path_v2: bool,
    /// TUNING KNOB (hidden): scale on the Lagrangian λ = 0.85·2^((qp−12)/3) that
    /// prices bits in the RD/mode/ME decisions. `1.0` = the standard H.264 model
    /// (byte-identical default). The BD-rate harness sweeps this to calibrate the
    /// rate weight; a content-adaptive dispatcher can vary it per frame.
    #[doc(hidden)]
    pub tune_lambda_scale: f64,
    /// TUNING KNOB (hidden): the λ·bits penalty (in bits) added to the intra cost
    /// in the fast/quality mode decision, biasing toward inter. `24.0` = default.
    /// Higher → fewer intra MBs. Content-adaptive candidate (textured content).
    #[doc(hidden)]
    pub tune_intra_penalty: f64,
    /// TUNING KNOB (hidden): content-adaptive cost-function dispatch. Fraction of
    /// each frame's highest-VARIANCE MBs whose fast-preset mode decision uses the
    /// rate-faithful SATD cost instead of cheap SAD (SAD is rate-blind on detailed
    /// MBs). `0.0` = pure SAD (byte-identical default); `1.0` = all SATD.
    #[doc(hidden)]
    pub tune_satd_q: f64,
    /// EXPERIMENT KNOB (hidden): force sub-pel motion refinement in the FAST
    /// preset, which is otherwise integer-pel only. Exists to run the force-on
    /// oracle per clip: content whose true displacement is sub-pixel (slow pans,
    /// dollies) cannot be tracked by integer-pel ME, falls back to intra, and
    /// codes very expensively. Bitstream-changing — BD-rate gated, not byte-exact.
    #[doc(hidden)]
    pub tune_subpel: bool,
    /// Rate-distortion P_Skip decision. The default criterion skips only when the
    /// residual quantizes to EXACTLY zero (a proof of freeness); this instead
    /// compares `J = SSD + λ·bits` for the skip against the chosen coded mode, so
    /// macroblocks with a small but non-zero residual can skip too. Measured
    /// against x264 at matched QP, the exact-zero rule leaves 17-23 percentage
    /// points of macroblocks coded that x264 skips (foreman 6.4% vs 23.6%,
    /// in_to_tree 1.0% vs 24.1%) — while matching it exactly at both extremes
    /// (akiyo 72.5 vs 73.6, mobile 1.0 vs 1.4), which is what proves the gap is
    /// the CRITERION and not the machinery. Bitstream-changing; BD-rate gated.
    #[doc(hidden)]
    /// Quality preset's greedy P_Skip (openh264 `PredictSadSkip`): take the skip
    /// when its luma SAD is under the neighbour-predicted threshold, without
    /// pricing the coded alternative. An APPROXIMATE skip decision inside the
    /// P-chain — the same class as [`Self::tune_rd_skip_fast_t`] — so it is
    /// subject to the same propagation multiplier and exists as a knob to audit
    /// that. `true` is the long-standing default behaviour.
    /// Snap the full-pel diamond's centre to integer-pel before searching.
    ///
    /// The diamond walks WHOLE-pel offsets, but its seed is the neighbour MV
    /// predictor, which is fractional — so a sub-pel seed drags every candidate in
    /// the search through the 6-tap interpolation filter. Measured: 84-90% of all
    /// SATD evaluations interpolate. Snapping makes the full-pel phase genuinely
    /// full-pel (direct SATD against the reference), leaving only the sub-pel
    /// refine to interpolate. The un-snapped seed is retained as a candidate, so
    /// the search can never come out worse than its own starting point.
    ///
    /// Same fix already applied to the stall-rescue grid (2.21x -> 1.19x on zoom).
    pub tune_me_snap: bool,
    /// Walk the sub-pel refinement until it stops improving, instead of a single
    /// 8-point pass per step. Independent of [`Self::tune_me_snap`] — measured
    /// separately, because the two were first built coupled and the attribution
    /// was ambiguous.
    pub tune_me_subpel_iter: bool,
    pub tune_greedy_skip: bool,
    /// Minimum online FREE-skip percentage for [`Self::tune_greedy_skip`] to
    /// engage, dispatched on exactly the signal that gates RD skip
    /// ([`Self::tune_rd_skip_min_free`]). The greedy skip wins on temporally
    /// redundant content and LOSES on detailed content (BD-SSIM: akiyo -0.59,
    /// FourPeople -0.32 vs foreman +1.23) — the same sign-flip, separated by the
    /// same signal. `None` resolves to 85 — the calibrated default, at which the
    /// corpus regression on foreman (+1.23% BD-SSIM, previously shipping) becomes
    /// 0.00 and nothing else regresses. `Some(0)` restores the old ungated
    /// behaviour; `Some(101)` disables the greedy skip entirely.
    pub tune_greedy_skip_min_free: Option<u32>,
    /// RD `B_Skip` strength, in units of lambda. **DEFAULT-ON at 48.0**;
    /// `None`/`<=0` restores the previous exactly-free-only rule byte-identically.
    ///
    /// A B macroblock is skipped when direct WON the mode decision and its
    /// prediction distortion is under `T*lambda` — i.e. the residual is not worth
    /// its bits. Our previous rule demanded the residual quantize to EXACTLY zero,
    /// which reaches 93.5% of B macroblocks on akiyo and 34.5% on foreman (at or
    /// ABOVE x264) but collapses to 7.8% on mobile where x264 still finds 27.4%.
    /// The deficit is BUSY-CONTENT-ONLY, so this is DISPATCHED on the online
    /// free-skip rate of the frame ([`Self::tune_bskip_busy_pct`]) rather than
    /// applied as a flat constant — on content where we already out-skip x264 it
    /// stays a byte-identical no-op.
    ///
    /// 4-QP per-clip BD at T=48 (worst clip 0.00): mobile -0.51 PSNR / -0.96 SSIM,
    /// foreman -0.30 / -0.34, bus -0.10 / -0.50, akiyo byte-identical.
    pub tune_bskip_rd: Option<f64>,
    /// Engage [`Self::tune_bskip_rd`] only while the frame's online FREE-skip rate
    /// is below this percentage — the busy-content dispatch. Default 60.
    pub tune_bskip_busy_pct: Option<usize>,
    /// Minimum online DIRECT-WIN rate (percent of not-free B macroblocks where
    /// direct won the mode decision) for [`Self::tune_bskip_rd`] to engage.
    /// Default 10 — calibrated on the one corpus clip that regressed (football,
    /// 7.0%) against the lowest-rate winner (foreman, 14.1%).
    pub tune_bskip_dirwin_pct: Option<usize>,
    /// Search B 16x8 / 8x16 partitions. x264 spends 13.5% of its B macroblocks
    /// there; we had none, which is why the B bucket kept reading as a CODING gap
    /// after every constant in it had been swept flat. DEFAULT ON: the 4-QP
    /// per-clip table wins on all seven clips and both metrics with no sign flip
    /// (BD-SSIM akiyo -0.17%, FourPeople -0.80%, tempete -1.66%, mobile -3.36%,
    /// foreman -3.49%, bus -4.56%, football -7.09%), so there is nothing to
    /// dispatch on -- the win simply concentrates on busy/high-motion content.
    /// CABAC B path only; the CAVLC B path still emits 16x16 modes.
    pub tune_b_split: bool,
    pub tune_rd_skip: bool,
    /// Minimum FREE-skip percentage, measured online over the frame so far, for
    /// [`Self::tune_rd_skip`] to engage on the rest of that frame.
    ///
    /// `None` resolves per preset, because the signal's SCALE is preset-dependent:
    /// sub-pel refinement predicts better, so it lifts the free-skip rate on ALL
    /// content and the same absolute bar starts admitting content that loses.
    /// Fast (no sub-pel) calibrates to 60; the sub-pel presets need 90. Each is
    /// the smallest bar at which no corpus clip regresses on BD-PSNR or BD-SSIM.
    /// `Some(0)` forces RD skip on everywhere (which LOSES badly on detailed
    /// content); `Some(101)` disables it.
    pub tune_rd_skip_min_free: Option<u32>,
    /// Skip-gate on the null arm's cost, in units of lambda: when
    /// `SSD(skip) <= lambda * T` the skip is taken WITHOUT trial-encoding the
    /// coded arm at all.
    ///
    /// The RD skip decision has to encode the coded arm to price it, and 55-80%
    /// of the time it then throws that encode away — the null arm wins. This is
    /// the standard search-skip gate over that: it trades a small number of
    /// decisions (the RD comparison would occasionally have coded) for not
    /// encoding at all. `None`/`<= 0.0` disables it (every candidate is priced
    /// exactly). Unlike the rest of the decision this is NOT byte-identical, so
    /// it is BD-rate gated.
    pub tune_rd_skip_fast_t: Option<f64>,
    /// Number of B-frames between reference (I/P) anchors. `0` = no B-frames
    /// (Constrained Baseline, byte-identical). `>0` requires Main profile (B is
    /// illegal in Baseline) and activates the reorder pipeline: anchors are coded
    /// ahead of the B-frames that reference them (L0 past + L1 future), and B-
    /// frames are non-reference. WORK IN PROGRESS — see the B-frame build plan.
    pub bframes: u32,
    /// QP offset applied to B-frames (added to [`qp`](Self::qp)). B-frames are
    /// non-reference, so their coding error never propagates — quantizing them
    /// harder (a positive offset) spends the saved bits on the reference anchors.
    /// Only used when `bframes > 0`. Default `2`.
    pub bframe_qp_offset: i32,
    /// Adaptive Quantization strength. Modulates the QP per macroblock by content:
    /// flat/low-variance MBs (where blocking & banding are visible) get a FINER QP,
    /// busy/high-variance MBs (where the eye masks error) a COARSER one — moving bits
    /// to where they're seen, a perceptual (SSIM) win at ~neutral PSNR. The QP shift
    /// is relative to the FRAME's mean log-variance (content-invariant), rate-
    /// compensated, and its EFFECTIVE strength backs off automatically where the
    /// log-variance spread is extreme (pathological synthetic content), so it never
    /// regresses. Default **`1.0`** (on); `0.0` = off (uniform QP, byte-identical).
    pub aq_strength: f64,
    /// Content-adaptive B-frame ENABLE. When set (with `bframes > 0`), the encoder
    /// measures the clip's temporal predictability (a cheap global-motion bi-
    /// prediction residual) and codes B-frames ONLY when they'll help — smooth /
    /// predictable motion, where bi-pred + spatial-direct are cheap. On busy content
    /// it falls back to P-only, so B-frames never regress. Default `false`.
    pub bframes_adaptive: bool,
    /// Per-GOP I-frame QP cascade — the BASE offset for the classic `ip_ratio`
    /// (added to [`qp`](Self::qp) on each GOP's I-frame; it's the root reference for
    /// its whole GOP, so coding it finer propagates quality GOP-wide). Default `-3`.
    /// In the B-capable batch path this is CONTENT-ADAPTIVE per GOP: predictable
    /// GOPs — where the I-frame dominates the GOP's bits — deepen it up to 2 further
    /// QP steps (calibrated: busy ≈ base, compressible ≈ base−2). `0` disables the
    /// cascade entirely (byte-identical escape hatch). Constant-QP only.
    pub i_qp_offset: i32,
    /// CABAC entropy coding (PPS `entropy_coding_mode_flag = 1`, Main profile).
    /// Codes ~5–17% smaller than CAVLC at matched quality (I- and P-slices; B-slice
    /// CABAC pending). Default `false` (CAVLC — Constrained Baseline, unchanged).
    pub cabac: bool,
    /// `cabac_init_idc` (0..2) — selects one of 3 context-initialization tables for
    /// P/B slices (I-slices always use the I preset). The best table is
    /// content-dependent; `0` is the default. Signalled in the P/B slice header.
    pub cabac_init_idc: u32,
    /// Multiplier on the mode-decision Lagrangian (√λ) in the CABAC P/B path only.
    /// CABAC codes ~9% fewer bits than the CAVLC-flavoured rate estimate the mode
    /// decision uses, so the rate term is slightly over-weighted; this retunes it.
    /// Default `1.0` (unchanged). CAVLC path is never affected.
    pub cabac_lambda_scale: f64,
    /// ME lambda scale used on NORMAL-texture content, dispatched by the frame's
    /// median source macroblock variance. `None` = no dispatch (always
    /// [`Self::cabac_lambda_scale`]). **DEFAULT None — the dispatch is OFF.**
    ///
    /// The texture gate works for what it was built for: it holds mobile (median MB
    /// variance 1554) at the conservative value, byte-identically, where a flat 1.8
    /// costs +0.42% BD-SSIM. But it does NOT clear the monotone bar, because `bus`
    /// regresses at EVERY high value tried (1.4 +0.35, 1.6 +0.09, 1.8 +0.29 BD-PSNR
    /// vs 1.25) and bus's texture (454) sits BELOW football's (583), which WANTS the
    /// high value — so no threshold on this signal separates them. Kept as an
    /// opt-in knob with the machinery intact; needs a second, motion-flavoured term
    /// before it can be default-on.
    pub tune_lme_hi: Option<f64>,
    /// Median source MB variance at or above which the conservative
    /// [`Self::cabac_lambda_scale`] is used instead of [`Self::tune_lme_hi`].
    /// **Default 650.** Measured medians: akiyo 61, foreman 219, city 300, bus 454,
    /// football 583, **tempete 746**, mobile 1554. 650 sits between football (583,
    /// which WANTS the high lambda) and tempete (746, which regresses on BD-SSIM at
    /// it) — the third loser, found only after the first two were gated. An earlier
    /// 800 let tempete through by 54 points.
    pub tune_lme_tex_thresh: Option<i64>,
    /// Global-MC residual at or above which the conservative
    /// [`Self::cabac_lambda_scale`] is used. Default 26.0 — measured residuals are
    /// akiyo 1.5, foreman 9.6, city 12.4, mobile 19.5, football 24.8, **bus 27.5**;
    /// **Default 20.0.** Measured residuals: akiyo 1.5, foreman 9.6, city 12.4,
    /// mobile 19.5, football 24.8, bus 27.5. At 20 bus is clean (0.00 BD-PSNR /
    /// -0.03 BD-SSIM); a looser 24-26 leaves bus slightly positive because the
    /// per-frame residual straddles it. The cost of 20 is football's win (-0.57 ->
    /// -0.01): its per-frame residual also crosses 20, so it is held conservative.
    /// Deliberate — protecting a regressor outranks capturing a win.
    pub tune_lme_motion_thresh: Option<f64>,
    /// OPT-IN, BD-gate pending (Great Gate P1 machinery — docs/great-gate.md §6 P2):
    /// the population-shaped PER-MB form of the [`Self::tune_lme_tex_thresh`] texture
    /// veto. `Some(q)` routes each P frame's top-`q` fraction of highest-variance MBs
    /// to the conservative [`Self::cabac_lambda_scale`] individually (per-frame
    /// percentile — the routed fraction is content-invariant by construction, where
    /// the absolute median test cannot separate bus 454 from football 583, which want
    /// opposite values). The motion veto stays frame-level; B slices keep the
    /// frame-median form. `None` (default) = the frame-level veto exactly,
    /// byte-identical. Env override for sweep arms: `RFF_LME_Q`.
    pub tune_lme_q: Option<f64>,
    /// Quantizer dead-zone divisor override for the CABAC path (`F = 2^qbits/dz`).
    /// A smaller divisor (bigger F) keeps more near-threshold coefficients — cheaper
    /// under CABAC's context-coded residual than under CAVLC. `0` = use the standard
    /// content-derived dead-zone (default, unchanged).
    pub cabac_dz_div: i64,
    /// CABAC trellis-quantization (RDOQ) strength. Each 4×4 residual coefficient is
    /// RD-optimized (level vs level−1 minimizing `SSD + λ·R_cabac`, `λ` scaled by
    /// this; ~8 calibrated). `0.0` = off. DEFAULT-ON for CABAC I-slices (frame-type
    /// adaptive — P/B off, sparse residual gains ~0); CAVLC path always off.
    pub cabac_rdoq: f64,
    /// OPT-IN, BD-gate pending (Great Gate P2): trellis (RDOQ) strength for
    /// CABAC **P slices**. The encoder-bringup law says RDOQ's win is
    /// reference-structure-adaptive — a P frame is a REFERENCE inside its GOP,
    /// so trading its distortion for rate propagates without a weighting term;
    /// expect a wash-or-loss until propagation-weighted. `0.0` = off
    /// (byte-identical default).
    pub cabac_rdoq_p: f64,
    /// OPT-IN, BD-gate pending (Great Gate P2): trellis (RDOQ) strength for
    /// CABAC **B slices** — non-reference, so the structure-adaptive law says
    /// the trade is clean there (nothing depends on a B's reconstruction).
    /// `0.0` = off (byte-identical default).
    pub cabac_rdoq_b: f64,
    /// OPT-IN, BD-gate pending (Great Gate P3.3): search 8x4/4x8/4x4
    /// sub-partitions inside P_8x8 (CABAC quality path, single-ref). The
    /// decoder has parsed these since bring-up; the encoder emits them only
    /// when this is set. `false` (default) = byte-identical. Env sweep arm:
    /// `RFF_SUB8X8_SPLIT=1`. Default-on decision DEFERRED until the 4-wide
    /// MC/cost kernels land (census #8 -- the scalar fall-through would
    /// double-charge the feature's speed cost).
    pub tune_sub8x8_split: bool,
    /// PROBE (Great Gate P3.3 gate): price the sub-8x8 SPLIT-vs-8x8 decision in
    /// the RD currency (`SSD_recon + lambda*bits`, both arms fully planned)
    /// instead of the SATD proxy. Only meaningful with
    /// [`Self::tune_sub8x8_split`]. Costs two extra macroblock plans per split
    /// candidate — a probe, not a shipping speed point.
    pub tune_sub8_rd: bool,
    /// PROBE (Great Gate P3 RD-pricing #2): price the INTRA-vs-INTER decision
    /// by `SSD_recon + lambda*bits` (both candidates planned for real) instead
    /// of the SATD proxy plus the fitted [`Self::tune_intra_penalty`]. That
    /// penalty is itself a correction for this proxy's bias, so if the probe
    /// wins the penalty should be re-swept (probably toward 0 — the P2 lambda
    /// campaign already found 0 better on all three clips). Costs one extra
    /// trial-encode plus one extra MB plan per coded macroblock.
    pub tune_intra_rd: bool,
    /// PROBE (Great Gate P3 RD-pricing #3): price the PARTITION SHAPE decision
    /// (16x16 / 16x8 / 8x16 / P_8x8) by `SSD_recon + lambda*bits` instead of
    /// the SATD proxy. The third SATD-priced default-on site. Costs one full
    /// macroblock plan per candidate shape.
    pub tune_shape_rd: bool,
    /// Price the inter RD trials with the macroblock's ACTUAL quantizer rather
    /// than the slice's frame-level one. AQ (`aq_strength`, default 1.0) and
    /// mb-tree rewrite QP per macroblock; a frame-level lambda misprices rate by
    /// `2^((qp_frame-qp_mb)/3)` at every RD site, worst on the high-variance
    /// macroblocks AQ moves furthest. `false` restores the frame-lambda form for
    /// A/B (the arm must PIN the value, never rely on an absent override).
    pub tune_rd_lambda_mb: bool,
    /// High-profile 8×8 transform (`transform_8x8_mode_flag`). When set, an intra
    /// macroblock may use one 8×8 integer DCT per 8×8 block (I_8x8) instead of four
    /// 4×4s — a per-MB RD choice that wins on smooth / large-structure content.
    /// Requires High profile (profile_idc 100). CAVLC only (our decoder has no CABAC
    /// 8×8). Default `false`.
    pub transform_8x8: bool,
    /// P_8x8 sub-partition motion: allow a P macroblock to split into four 8×8
    /// partitions, each with its own motion vector (finer motion granularity on
    /// complex / boundary motion). A per-MB RD choice vs 16×16/16×8/8×16, gated on the
    /// heavy-16×16 motion-boundary signal. A NET WIN on real content (12-clip Derf
    /// corpus: −0.23% mean BD, big wins on bus/mobile/flower; a rigorous 6-channel
    /// discovery harvest proved no cheap gate beats default-on, oracle headroom only
    /// 0.18%), so it is DEFAULT-ON for the Quality preset. Quality-only. `None` =
    /// follow the preset (ON for Quality); `Some(b)` forces it either way. (8×4/4×8/4×4
    /// sub-shapes within an 8×8 are a further split, not yet built.)
    pub sub_8x8: Option<bool>,
    /// Adaptive WIDE motion search: on flat source blocks (where the gradient-descent
    /// diamond stalls at a plateau and misses the true MV) cover the ±16 neighbourhood
    /// with a grid search instead; busy blocks keep the fast diamond. A big win on
    /// smooth/low-motion content (the diamond's flat-surface failure), free on busy
    /// content — content-adaptive (a per-frame coherence gate keeps it from regressing
    /// even on pure pans), so it is DEFAULT-ON for the Quality preset. Quality-only.
    /// `None` = follow the preset (ON for Quality); `Some(b)` forces it either way.
    pub me_wide: Option<bool>,
    /// Macroblock-tree lookahead adaptive QP (TEMPORAL AQ). A cheap forward pass over
    /// each GOP's source frames propagates future-reference importance backward along
    /// motion vectors and lowers the QP of heavily-referenced macroblocks — investing
    /// bits where they pay off across many later frames. The complement to the spatial
    /// [`aq_strength`](Self::aq_strength). Per-GOP-centered (rate-preserving). Applies
    /// only in the batch (`encode_all`) constant-QP path, where the GOP's future frames
    /// are available (a `bframes > 0` encode uses the reorder pipeline and ignores it).
    ///
    /// **Default `true` since 0.5.0** (H-37) — the gate cleared and the architectural
    /// blocker is gone: the streaming path now carries a one-GOP lookahead queue, so
    /// `encode()` + [`flush`](crate::Encoder::flush) is byte-identical to
    /// `encode_all()`. Set `false` for zero added latency (one AU per `encode` call)
    /// or for the pre-0.5.0 bytes. Evidence:
    /// * BD: the 4-QP per-clip gate CLEARS with room to spare (akiyo −4.82%,
    ///   foreman −3.13%, football −0.53%, bus −0.29%, mobile −0.24%,
    ///   city_4cif +0.01% neutral) — the monotone non-regression bar, not a mean.
    /// * Cost: content-INDEPENDENT at 16-21 candidate evaluations per macroblock per
    ///   frame across that corpus (1.3× spread) ≈ 1-2% of a busy-clip encode. The
    ///   per-clip "blowups" (+251%, +34%) were wall-clock artifacts of a drifting box.
    ///
    /// COST OF THE DEFAULT: `encode()` now returns a whole GOP's access units at once
    /// (empty while the GOP fills), so end-to-end latency is up to `gop_size` frames
    /// and **`flush()` is required at end of stream**. Batch callers
    /// (`encode_all`) are unaffected — they already had the whole GOP.
    pub mbtree: bool,
    /// Minimum dispersion of mb-tree's own propagation offsets for it to apply
    /// at all (the DIFFERENTIATION LATCH — see `mbtree.rs`). Below this the
    /// offsets carry no information and are zeroed, which is byte-identical to
    /// mb-tree off. `0.0` disables the latch (ungated, pre-gate behaviour).
    pub mbtree_spread_min: f64,
    /// mb-tree QP-offset strength: `qp_offset = -strength · log2((intra+propagate)/intra)`.
    /// Larger = more aggressive bit redistribution toward referenced MBs. Default `0.9`.
    pub mbtree_strength: f64,
    /// Resolution the mb-tree lookahead motion search runs at (see [`LookaheadMode`]).
    /// Default [`HalfRes`](LookaheadMode::HalfRes) — fastest (~4× the lookahead), a small
    /// BD-rate cost on fine detail; use [`Hybrid`](LookaheadMode::Hybrid) to recover
    /// full-res quality at ~1.7×. Only relevant when [`mbtree`](Self::mbtree) is on.
    pub mbtree_lookahead: LookaheadMode,
}

/// Escape hatch restoring the pre-U6 defaults (Constrained Baseline + CAVLC), so the
/// previous bitstream is reproducible byte-for-byte for bisection and for callers that
/// must remain Baseline-compatible.
fn legacy_cavlc() -> bool {
    use std::sync::OnceLock;
    static L: OnceLock<bool> = OnceLock::new();
    *L.get_or_init(|| std::env::var_os("RUSTY_H264_LEGACY_CAVLC").is_some())
}

impl EncoderConfig {
    /// A minimal all-intra Constrained Baseline configuration at the given size.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            // DEFAULT-ON as of the U6 measurement: CABAC is -9.00%/-8.83% BD-rate for
            // 1.10-1.22x time on the 4-QP corpus — better value than any preset step in
            // either encoder — so shipping CAVLC by default was leaving a large win on
            // the table. CABAC requires Main profile, hence the profile default moves
            // with it. `RUSTY_H264_LEGACY_CAVLC=1` restores the exact prior defaults
            // (Constrained Baseline + CAVLC) as the escape hatch and bisection anchor.
            // HIGH by default, matching x264. High is required to signal
            // transform_8x8_mode_flag at all, and the 8x8 transform is now default-on
            // (below). Legacy CAVLC keeps Constrained Baseline, which cannot carry it.
            profile: if legacy_cavlc() { Profile::ConstrainedBaseline } else { Profile::High },
            chroma: ChromaFormat::Yuv420,
            level_idc: 30,
            qp: 26,
            gop_size: 1,
            bitrate: 0,
            framerate: 30.0,
            num_ref_frames: 1,
            preset: Preset::Fast,
            tune_skip_accel_check: true,
            coded_path_v2: false,
            tune_lambda_scale: 1.0,
            tune_intra_penalty: 24.0,
            tune_satd_q: 0.5,
            tune_subpel: false,
            tune_me_snap: true,
            tune_me_subpel_iter: true,
            tune_greedy_skip: true,
            tune_greedy_skip_min_free: None,
            tune_bskip_rd: Some(48.0),
            tune_bskip_busy_pct: None,
            tune_bskip_dirwin_pct: None,
            tune_b_split: true,
            tune_rd_skip: false,
            tune_rd_skip_min_free: None,
            tune_rd_skip_fast_t: None,
            aq_strength: 1.0,
            bframes: 0,
            bframe_qp_offset: 3,
            bframes_adaptive: false,
            // Calibrated per-GOP I-frame cascade (~x264 ip_ratio 1.4): a robust
            // BD-rate win across content (clip240 P −0.6%, dpan B −7.3%, mixed
            // −1.7%). Trades a few I-frame bits for GOP-wide propagated quality.
            i_qp_offset: -3,
            cabac: !legacy_cavlc(),
            cabac_init_idc: 0,
            cabac_lambda_scale: 1.25,
            tune_lme_hi: Some(1.6),
            tune_lme_tex_thresh: None,
            tune_lme_motion_thresh: Some(20.0),
            tune_lme_q: None,
            cabac_dz_div: 0,
            cabac_rdoq: 8.0,
            cabac_rdoq_p: 0.0,
            cabac_rdoq_b: 0.0,
            // DEFAULT-ON 2026-08-06 (docs/gate-ledger.md sub8x8-split): the split
            // search PLUS its RD pricing — the two are a package, since the
            // SATD-priced split search alone is a net LOSER (7W/13L/3N) and only
            // becomes 10W/9N/0L once the decision is priced in the RD currency.
            // Cost is MEASURED and accepted, not unknown: 5.59x CPU on the
            // quality preset (prototype-grade — exhaustive unpruned search, no
            // 4-wide kernels, one duplicate plan per split MB; the best_part
            // campaign is the follow-up). `tune_sub8x8_split=false` restores the
            // pre-P3.3 bytes exactly.
            tune_sub8x8_split: true,
            tune_sub8_rd: true,
            // DEFAULT-ON 2026-08-06 (docs/gate-ledger.md intra-rd-grain): grain-
            // gated, so byte-identical (1.00x) off grain and 1.327x ON grain for
            // -4.73 PSNR / -5.22 SSIM — a better trade than a preset step.
            tune_intra_rd: true,
            tune_shape_rd: true,
            tune_rd_lambda_mb: false,
            // DEFAULT-ON since 2026-08-08, matching x264. Measured per clip across
            // all-intra / I+P / I+P+B (bench/t8_default.py) with inter-8x8 off:
            // wins up to -1.90% BD-SSIM (akiyo) and -0.77% (FourPeople), worst cell
            // +0.34%. Baseline/Constrained Baseline cannot signal it.
            transform_8x8: !legacy_cavlc(),
            sub_8x8: None,
            me_wide: None,
            mbtree: true,
            // Expressed in the STRENGTH-INVARIANT unit (see mbtree.rs): the
            // fitted value was 1.0 against the raw RMS at the default strength 0.9.
            // LATCH DISABLED 2026-08-08 (was 1.0/0.9). The spread latch turns mb-tree
            // off for a GOP whose offsets look undifferentiated. Audited on the
            // content it actually fires on and it was COSTING us: harbour_4cif
            // -0.88%, foreman_cif -1.20% BD-SSIM to disable it, mobile_cif +0.01%
            // (neutral), and byte-identical on every clip it does not fire on.
            // Sweeping it the other way is catastrophic — at 2.0 it suppresses
            // mb-tree on akiyo (+9.16%) and FourPeople (+6.32%), which is the
            // measure of how much mb-tree is worth where it works. A latch whose
            // every measured firing is a loss is not a guard.
            mbtree_spread_min: 0.0,
            mbtree_strength: 0.9,
            mbtree_lookahead: LookaheadMode::HalfRes,
        }
    }

    /// Picture width rounded up to whole macroblocks.
    pub fn mb_width(&self) -> usize {
        self.width.div_ceil(16)
    }

    /// Picture height rounded up to whole macroblocks.
    pub fn mb_height(&self) -> usize {
        self.height.div_ceil(16)
    }
}
