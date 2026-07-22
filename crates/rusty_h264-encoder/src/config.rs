//! Encoder configuration.

use rusty_h264_common::{ChromaFormat, Profile};

/// Speed/quality trade-off, in the spirit of x264's `-preset`. The bitstream is
/// valid (and decodes bit-exactly) either way; only the encoder's effort differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// **Fast** (default) — built to mirror x264's fastest presets: mode decision
    /// by cheap **SAD** estimation (no rate-distortion trial-encoding; SAD
    /// auto-vectorizes to `psadbw`), `P_16x16`-only inter, `I_16x16`-only intra,
    /// and **integer-pel** motion (no sub-pel `mc_luma` interpolation — profiling
    /// showed it was ~55% of the encode). Much faster; larger files, and a little
    /// quality lost on sub-pixel motion (none on integer/screen content).
    #[default]
    Fast,
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
    /// Speed/quality trade-off. Defaults to [`Preset::Fast`].
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
    /// High-profile 8×8 transform (`transform_8x8_mode_flag`). When set, an intra
    /// macroblock may use one 8×8 integer DCT per 8×8 block (I_8x8) instead of four
    /// 4×4s — a per-MB RD choice that wins on smooth / large-structure content.
    /// Requires High profile (profile_idc 100). CAVLC only (our decoder has no CABAC
    /// 8×8). Default `false`.
    pub transform_8x8: bool,
    /// P_8x8 sub-partition motion: allow a P macroblock to split into four 8×8
    /// partitions, each with its own motion vector (finer motion granularity on
    /// complex / boundary motion). A per-MB RD choice vs 16×16/16×8/8×16, gated on the
    /// heavy-16×16 motion-boundary signal — content-adaptive, never regresses. Default
    /// `false`. (8×4/4×8/4×4 sub-shapes within an 8×8 are a further split, not yet built.)
    pub sub_8x8: bool,
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
    /// are available. Default `false`; `true` uses [`mbtree_strength`](Self::mbtree_strength).
    pub mbtree: bool,
    /// mb-tree QP-offset strength: `qp_offset = -strength · log2((intra+propagate)/intra)`.
    /// Larger = more aggressive bit redistribution toward referenced MBs. Default `0.9`.
    pub mbtree_strength: f64,
    /// Resolution the mb-tree lookahead motion search runs at (see [`LookaheadMode`]).
    /// Default [`HalfRes`](LookaheadMode::HalfRes) — fastest (~4× the lookahead), a small
    /// BD-rate cost on fine detail; use [`Hybrid`](LookaheadMode::Hybrid) to recover
    /// full-res quality at ~1.7×. Only relevant when [`mbtree`](Self::mbtree) is on.
    pub mbtree_lookahead: LookaheadMode,
}

impl EncoderConfig {
    /// A minimal all-intra Constrained Baseline configuration at the given size.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            profile: Profile::ConstrainedBaseline,
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
            aq_strength: 1.0,
            bframes: 0,
            bframe_qp_offset: 2,
            bframes_adaptive: false,
            // Calibrated per-GOP I-frame cascade (~x264 ip_ratio 1.4): a robust
            // BD-rate win across content (clip240 P −0.6%, dpan B −7.3%, mixed
            // −1.7%). Trades a few I-frame bits for GOP-wide propagated quality.
            i_qp_offset: -3,
            cabac: false,
            cabac_init_idc: 0,
            cabac_lambda_scale: 1.0,
            cabac_dz_div: 0,
            cabac_rdoq: 8.0,
            transform_8x8: false,
            sub_8x8: false,
            me_wide: None,
            mbtree: false,
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
