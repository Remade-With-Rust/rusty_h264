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
