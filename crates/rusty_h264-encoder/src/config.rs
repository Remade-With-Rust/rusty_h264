//! Encoder configuration.

use rusty_h264_common::{ChromaFormat, Profile};

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
