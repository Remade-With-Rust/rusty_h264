//! Shared codec types: profiles, chroma format, and the raw YUV frame container.

/// H.264 profile. The encoder targets Constrained Baseline; the rest are named
/// for parsing/identification only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Constrained Baseline (`profile_idc = 66` with `constraint_set1_flag`).
    ConstrainedBaseline,
    /// Baseline (`profile_idc = 66`).
    Baseline,
    /// Main (`profile_idc = 77`).
    Main,
    /// High (`profile_idc = 100`).
    High,
    /// Any other `profile_idc`.
    Other(u8),
}

impl Profile {
    /// The `profile_idc` byte written into the SPS.
    pub fn profile_idc(self) -> u8 {
        match self {
            Profile::ConstrainedBaseline | Profile::Baseline => 66,
            Profile::Main => 77,
            Profile::High => 100,
            Profile::Other(v) => v,
        }
    }
}

/// Chroma subsampling. The encoder supports 4:2:0 only for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaFormat {
    /// Monochrome (`chroma_format_idc = 0`).
    Monochrome,
    /// 4:2:0 (`chroma_format_idc = 1`).
    Yuv420,
}

impl ChromaFormat {
    /// `chroma_format_idc`.
    pub fn idc(self) -> u8 {
        match self {
            ChromaFormat::Monochrome => 0,
            ChromaFormat::Yuv420 => 1,
        }
    }
}

/// A raw planar YUV 4:2:0 frame (8-bit). Plane strides equal their widths;
/// chroma planes are half-resolution in each dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvFrame {
    /// Luma width in pixels.
    pub width: usize,
    /// Luma height in pixels.
    pub height: usize,
    /// Y plane, `width * height` bytes.
    pub y: Vec<u8>,
    /// Cb plane, `(width/2) * (height/2)` bytes.
    pub u: Vec<u8>,
    /// Cr plane, `(width/2) * (height/2)` bytes.
    pub v: Vec<u8>,
}

impl YuvFrame {
    /// Allocates a black (Y=0, U=V=128) frame. Dimensions must be even.
    pub fn black(width: usize, height: usize) -> Self {
        assert!(width % 2 == 0 && height % 2 == 0, "dimensions must be even");
        let cw = width / 2;
        let ch = height / 2;
        Self {
            width,
            height,
            y: vec![0; width * height],
            u: vec![128; cw * ch],
            v: vec![128; cw * ch],
        }
    }

    /// Chroma plane width.
    pub fn chroma_width(&self) -> usize {
        self.width / 2
    }

    /// Chroma plane height.
    pub fn chroma_height(&self) -> usize {
        self.height / 2
    }

    /// Validates plane sizes against the dimensions.
    pub fn is_valid(&self) -> bool {
        self.y.len() == self.width * self.height
            && self.u.len() == self.chroma_width() * self.chroma_height()
            && self.v.len() == self.chroma_width() * self.chroma_height()
    }
}
