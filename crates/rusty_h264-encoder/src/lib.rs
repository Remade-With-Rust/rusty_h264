//! Pure-Rust H.264 (Constrained Baseline) encoder.
//!
//! Status: all-intra, `I_16x16` DC-predicted macroblocks with the full
//! transform → quantization → CAVLC pipeline. The Annex-B output is bit-exactly
//! decodable by reference decoders (verified against ffmpeg). Richer intra modes
//! (I_4x4), inter prediction, and the in-loop deblocking filter (currently
//! signalled disabled) are layered in by later generations behind this API.
//!
//! ```
//! use rusty_h264_encoder::{Encoder, EncoderConfig};
//! use rusty_h264_common::YuvFrame;
//!
//! let cfg = EncoderConfig::new(16, 16);
//! let mut enc = Encoder::new(cfg).unwrap();
//! let frame = YuvFrame::black(16, 16);
//! let bitstream = enc.encode(&frame); // Annex-B bytes for one access unit
//! assert!(!bitstream.is_empty());
//! ```

mod config;
mod lookahead;
mod mb16;
mod params;
mod rc;
mod slice;

pub use config::{EncoderConfig, Preset};
pub use params::{Pps, Sps};
pub use rc::RateControl;

use rusty_h264_common::{BitWriter, ChromaFormat, NalUnit, NalUnitType, Profile, YuvFrame};

/// Errors that can arise constructing or driving the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// A feature outside the implemented Constrained Baseline subset was asked for.
    Unsupported(&'static str),
    /// The supplied frame's dimensions or plane sizes don't match the config.
    FrameMismatch,
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EncodeError::Unsupported(s) => write!(f, "unsupported: {s}"),
            EncodeError::FrameMismatch => write!(f, "frame dimensions do not match encoder config"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// A Constrained Baseline H.264 encoder.
#[derive(Debug)]
pub struct Encoder {
    cfg: EncoderConfig,
    sps: Sps,
    pps: Pps,
    /// Count of frames fed so far; drives IDR placement via `gop_size`.
    frame_index: u32,
    /// `frame_num` of the next picture (resets to 0 at each IDR).
    next_frame_num: u32,
    /// Index of the current picture within its GOP (0 at IDR), for POC.
    gop_index: u32,
    /// Decoded-picture buffer: recent **deblocked** reconstructions (coded size),
    /// most-recent first, used as inter references (`ref_idx` 0 = front).
    refs: Vec<RefFrame>,
    /// Average-bitrate controller; `None` for constant-QP encoding.
    rc: Option<RateControl>,
}

/// A reference picture: deblocked reconstruction at coded (MB-grid) resolution.
/// Stored now (4a); read by motion compensation in 4b.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Encoder {
    /// Creates an encoder, validating that the configuration is within the
    /// implemented subset.
    pub fn new(cfg: EncoderConfig) -> Result<Self, EncodeError> {
        if !matches!(cfg.profile, Profile::ConstrainedBaseline | Profile::Baseline) {
            return Err(EncodeError::Unsupported("only Constrained Baseline profile"));
        }
        if cfg.chroma != ChromaFormat::Yuv420 {
            return Err(EncodeError::Unsupported("only 4:2:0 chroma"));
        }
        if cfg.width == 0 || cfg.height == 0 || cfg.width % 2 != 0 || cfg.height % 2 != 0 {
            return Err(EncodeError::Unsupported("dimensions must be positive and even"));
        }
        let sps = Sps::from_config(&cfg);
        let pps = Pps::from_config(&cfg);
        let rc = (cfg.bitrate > 0).then(|| RateControl::new(cfg.bitrate, cfg.framerate, cfg.qp));
        Ok(Self {
            cfg,
            sps,
            pps,
            frame_index: 0,
            next_frame_num: 0,
            gop_index: 0,
            refs: Vec::new(),
            rc,
        })
    }

    /// The active configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Encodes one frame, returning the Annex-B access unit. Every `gop_size`
    /// frames (and always the first) is coded as an IDR, prefixed with SPS/PPS.
    ///
    /// Generation 1 codes *every* picture as an IDR (all-intra); inter frames
    /// arrive with motion compensation later.
    pub fn encode(&mut self, frame: &YuvFrame) -> Vec<u8> {
        self.try_encode(frame).expect("frame matched config")
    }

    /// Fallible [`encode`](Self::encode): validates the frame against the config.
    pub fn try_encode(&mut self, frame: &YuvFrame) -> Result<Vec<u8>, EncodeError> {
        if frame.width != self.cfg.width || frame.height != self.cfg.height || !frame.is_valid() {
            return Err(EncodeError::FrameMismatch);
        }

        // GOP placement: an IDR at each `gop_size` boundary, P-frames between.
        let is_idr = self.cfg.gop_size <= 1 || self.frame_index % self.cfg.gop_size == 0;
        if is_idr {
            self.gop_index = 0;
            self.next_frame_num = 0;
            self.refs.clear();
        }
        let frame_num = self.next_frame_num;
        let poc_lsb = (2 * self.gop_index) % 16;

        // Rate control (if enabled) chooses this frame's QP from a cheap
        // look-ahead complexity estimate; otherwise the QP is fixed.
        let complexity = if self.rc.is_some() {
            lookahead::complexity(&self.cfg, frame, if is_idr { None } else { self.refs.first() })
        } else {
            0.0
        };
        let qp = match &self.rc {
            Some(rc) => rc.pick_qp(is_idr, complexity),
            None => self.cfg.qp,
        };

        let mut out = Vec::new();
        let mut w = BitWriter::new();
        let (nal_type, reference) = if is_idr {
            // SPS/PPS precede every IDR so the stream is independently decodable.
            self.sps.to_nal().write_annex_b(&mut out);
            self.pps.to_nal().write_annex_b(&mut out);
            slice::write_idr_slice_header(&mut w, &self.cfg, qp);
            let r = mb16::encode_slice_data(&mut w, &self.cfg, frame, qp, false, &[]);
            (NalUnitType::IdrSlice, r)
        } else {
            slice::write_p_slice_header(&mut w, &self.cfg, qp, frame_num, poc_lsb, self.refs.len());
            let r = mb16::encode_slice_data(&mut w, &self.cfg, frame, qp, true, &self.refs);
            (NalUnitType::NonIdrSlice, r)
        };
        let slice_bytes = w.into_bytes();
        // Feed the coded slice size (the picture's own bits) back to the controller.
        if let Some(rc) = &mut self.rc {
            rc.update(is_idr, slice_bytes.len() * 8, qp, complexity);
        }
        NalUnit::new(3, nal_type, slice_bytes).write_annex_b(&mut out);

        // The deblocked reconstruction enters the DPB (most-recent first), which
        // is kept to `max_num_ref_frames` by a sliding window.
        self.refs.insert(0, reference);
        self.refs.truncate(self.cfg.num_ref_frames.max(1) as usize);
        self.frame_index += 1;
        self.gop_index += 1;
        self.next_frame_num = (self.next_frame_num + 1) % 16;
        Ok(out)
    }

    /// Batch-encodes every frame, returning one Annex-B access unit per frame.
    ///
    /// At constant QP the GOPs are independent — each begins with an IDR that
    /// resets the DPB, `frame_num` and POC, and SPS/PPS precede every IDR — so they
    /// are encoded **in parallel across CPU cores** and the result is
    /// **byte-identical** to calling [`encode`](Self::encode) frame-by-frame. With
    /// rate control enabled the per-frame QP depends on history, so this falls back
    /// to sequential encoding. Within a GOP, P-frames are inherently sequential
    /// (each predicts from the previous reconstruction); the parallelism is across
    /// GOPs, so it scales with the number of GOPs in the clip.
    pub fn encode_all(&self, frames: &[YuvFrame]) -> Result<Vec<Vec<u8>>, EncodeError> {
        for f in frames {
            if f.width != self.cfg.width || f.height != self.cfg.height || !f.is_valid() {
                return Err(EncodeError::FrameMismatch);
            }
        }
        // Rate control threads state across frames → it must stay sequential.
        if self.cfg.bitrate > 0 {
            let mut enc = Encoder::new(self.cfg.clone())?;
            return frames.iter().map(|f| enc.try_encode(f)).collect();
        }
        let gop = self.cfg.gop_size.max(1) as usize;
        let gops: Vec<&[YuvFrame]> = frames.chunks(gop).collect();
        if gops.is_empty() {
            return Ok(Vec::new());
        }
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(gops.len());
        // Each GOP is encoded with a fresh encoder (an IDR resets all state), so
        // GOPs distribute across `n` worker threads with no shared mutable state.
        let mut out: Vec<Option<Vec<Vec<u8>>>> = (0..gops.len()).map(|_| None).collect();
        let cfg = &self.cfg;
        let gops_ref = &gops;
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..n)
                .map(|t| {
                    s.spawn(move || {
                        let mut local = Vec::new();
                        let mut i = t;
                        while i < gops_ref.len() {
                            let mut enc = Encoder::new(cfg.clone()).expect("config");
                            let aus: Vec<Vec<u8>> = gops_ref[i].iter().map(|f| enc.encode(f)).collect();
                            local.push((i, aus));
                            i += n;
                        }
                        local
                    })
                })
                .collect();
            for h in handles {
                for (i, aus) in h.join().expect("encode worker panicked") {
                    out[i] = Some(aus);
                }
            }
        });
        Ok(out.into_iter().flatten().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_profile() {
        let mut cfg = EncoderConfig::new(16, 16);
        cfg.profile = Profile::High;
        assert!(matches!(Encoder::new(cfg), Err(EncodeError::Unsupported(_))));
    }

    #[test]
    fn encodes_access_unit_with_sps_pps_idr() {
        use rusty_h264_common::nal::split_annex_b;
        let cfg = EncoderConfig::new(32, 32);
        let mut enc = Encoder::new(cfg).unwrap();
        let frame = YuvFrame::black(32, 32);
        let au = enc.encode(&frame);

        let nals = split_annex_b(&au);
        assert_eq!(nals.len(), 3);
        assert_eq!(NalUnitType::from_id(nals[0][0]), NalUnitType::Sps);
        assert_eq!(NalUnitType::from_id(nals[1][0]), NalUnitType::Pps);
        assert_eq!(NalUnitType::from_id(nals[2][0]), NalUnitType::IdrSlice);
    }

    #[test]
    fn encode_all_matches_sequential_cqp() {
        // GOP-parallel batch encoding must be byte-identical to frame-by-frame
        // sequential encoding at constant QP (GOPs are independent).
        let (w, h) = (48usize, 32usize);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 4; // 10 frames → 3 GOPs (4,4,2)
        let frames: Vec<YuvFrame> = (0..10u8)
            .map(|t| YuvFrame {
                width: w,
                height: h,
                y: (0..w * h).map(|i| (i as u8).wrapping_add(t.wrapping_mul(7))).collect(),
                u: vec![128u8.wrapping_add(t); (w / 2) * (h / 2)],
                v: vec![128u8.wrapping_sub(t); (w / 2) * (h / 2)],
            })
            .collect();
        let mut seq_enc = Encoder::new(cfg.clone()).unwrap();
        let seq: Vec<Vec<u8>> = frames.iter().map(|f| seq_enc.encode(f)).collect();
        let par = Encoder::new(cfg).unwrap().encode_all(&frames).unwrap();
        assert_eq!(seq, par, "GOP-parallel must equal sequential at CQP");
    }

    #[test]
    fn rejects_mismatched_frame() {
        let cfg = EncoderConfig::new(16, 16);
        let mut enc = Encoder::new(cfg).unwrap();
        let frame = YuvFrame::black(32, 16);
        assert_eq!(enc.try_encode(&frame), Err(EncodeError::FrameMismatch));
    }
}
