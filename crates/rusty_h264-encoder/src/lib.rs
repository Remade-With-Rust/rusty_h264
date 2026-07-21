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

mod cabac;
mod config;
mod lookahead;
mod mb16;
mod mbtree;
mod params;
mod rc;
mod slice;

pub use config::{EncoderConfig, LookaheadMode, Preset};
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
    /// Per-MB QP offset for the NEXT `encode()` (mb-tree temporal AQ). Set by the
    /// batch path before each frame; consumed (and cleared) by `try_encode`. Empty /
    /// `None` → no offset (byte-identical).
    pending_qpo: Option<Vec<i32>>,
}

/// A reference picture: deblocked reconstruction at coded (MB-grid) resolution.
/// Stored now (4a); read by motion compensation in 4b.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    // 16-byte aligned (moved from the encoder's aligned rec planes) so the openh264
    // MC asm can load aligned reference row chunks.
    pub y: rusty_h264_common::aligned::AlignedBytes,
    pub u: rusty_h264_common::aligned::AlignedBytes,
    pub v: rusty_h264_common::aligned::AlignedBytes,
    /// Picture Order Count — the DISPLAY position. B ref-lists order L0/L1 by POC
    /// relative to the current picture; P ignores it.
    pub poc: i32,
    /// The picture's `frame_num` (reference frames only advance it).
    pub frame_num: u32,
    /// Per-4×4-block List-0 motion (raster, `mb_w*4` wide). Populated for anchors;
    /// read as the co-located picture (`RefPicList1[0]`) when deriving a B-frame's
    /// spatial-direct `colZeroFlag`. `ref_idx == -1` marks intra/uncoded blocks.
    pub mv: Vec<(i32, i32)>,
    pub ref_idx: Vec<i32>,
    /// Blocks-wide (`mb_w*4`), so the co-located index is `by*w4 + bx`.
    pub w4: usize,
}

impl Encoder {
    /// Creates an encoder, validating that the configuration is within the
    /// implemented subset.
    pub fn new(cfg: EncoderConfig) -> Result<Self, EncodeError> {
        if !matches!(
            cfg.profile,
            Profile::ConstrainedBaseline | Profile::Baseline | Profile::Main | Profile::High
        ) {
            return Err(EncodeError::Unsupported("unsupported profile"));
        }
        // The 8×8 transform is a High-profile CAVLC feature (our decoder has no CABAC 8×8).
        if cfg.transform_8x8 && (!matches!(cfg.profile, Profile::High) || cfg.cabac) {
            return Err(EncodeError::Unsupported("8x8 transform requires High profile + CAVLC"));
        }
        // B-frames are illegal in Baseline (the decoder enforces this too): Main only.
        if cfg.bframes > 0 && !matches!(cfg.profile, Profile::Main) {
            return Err(EncodeError::Unsupported("B-frames require Main profile"));
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
            pending_qpo: None,
        })
    }

    /// Sets the per-MB QP offset applied to the NEXT [`encode`](Self::encode) call
    /// (mb-tree temporal AQ). One entry per macroblock (raster). Consumed once.
    pub(crate) fn set_pending_qpo(&mut self, qpo: Vec<i32>) {
        self.pending_qpo = Some(qpo);
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
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Total);
        if frame.width != self.cfg.width || frame.height != self.cfg.height || !frame.is_valid() {
            return Err(EncodeError::FrameMismatch);
        }

        // B-frames need lookahead (a future anchor coded before the B), which the
        // one-frame-in streaming API can't provide — use `encode_all` for B.
        if self.cfg.bframes > 0 {
            return Err(EncodeError::Unsupported("B-frames need encode_all (lookahead)"));
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
        // mb-tree per-MB QP offset for this frame (empty = none / byte-identical).
        let qpo = self.pending_qpo.take().unwrap_or_default();

        // Rate control (if enabled) chooses this frame's QP from a cheap
        // look-ahead complexity estimate; otherwise the QP is fixed.
        let complexity = if self.rc.is_some() {
            lookahead::complexity(&self.cfg, frame, if is_idr { None } else { self.refs.first() })
        } else {
            0.0
        };
        let qp = match &self.rc {
            Some(rc) => rc.pick_qp(is_idr, complexity),
            // Constant-QP: apply the per-GOP I-frame cascade offset (0 by default →
            // byte-identical). Keeps the P-only path consistent with `code_picture`.
            None if is_idr => (self.cfg.qp as i32 + self.cfg.i_qp_offset).clamp(0, 51) as u8,
            None => self.cfg.qp,
        };

        let mut out = Vec::new();
        // Pre-size the slice writer to a generous fraction of the raw frame so the
        // CAVLC hot loop never reallocs mid-frame (byte-identical; just capacity).
        let mut w = BitWriter::with_capacity(self.cfg.width * self.cfg.height / 2 + 4096);
        let (nal_type, mut reference) = if is_idr {
            // SPS/PPS precede every IDR so the stream is independently decodable.
            self.sps.to_nal().write_annex_b(&mut out);
            self.pps.to_nal().write_annex_b(&mut out);
            slice::write_idr_slice_header(&mut w, &self.cfg, qp);
            let r = if self.cfg.cabac {
                mb16::encode_slice_data_cabac_intra(&mut w, &self.cfg, frame, qp, &qpo)
            } else {
                mb16::encode_slice_data(&mut w, &self.cfg, frame, qp, false, &[], &qpo)
            };
            (NalUnitType::IdrSlice, r)
        } else {
            slice::write_p_slice_header(&mut w, &self.cfg, qp, frame_num, poc_lsb, self.refs.len());
            let r = if self.cfg.cabac {
                mb16::encode_slice_data_cabac_p(&mut w, &self.cfg, frame, qp, &self.refs, &qpo)
            } else {
                mb16::encode_slice_data(&mut w, &self.cfg, frame, qp, true, &self.refs, &qpo)
            };
            (NalUnitType::NonIdrSlice, r)
        };
        // POC/frame_num carried on the reference so B-frame ref-lists (when enabled)
        // can order L0/L1 by display position. Unused on the P-only path.
        reference.poc = 2 * self.gop_index as i32;
        reference.frame_num = frame_num;
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
        // B-frames need a reorder pipeline (code the future anchor before the B's
        // that reference it) — a separate sequential path.
        if self.cfg.bframes > 0 {
            // Content-adaptive dispatch, PER GOP (codec-content-adaptive-dispatch):
            // code B-frames only in GOPs whose motion is predictable enough to pay,
            // so a mixed clip gets B on its smooth segments and P on its busy ones.
            let gop = self.cfg.gop_size.max(1) as usize;
            let n_gops = frames.len().div_ceil(gop);
            let (w, h) = (self.cfg.width, self.cfg.height);
            // One cheap per-GOP signal drives BOTH content-adaptive knobs: the B/P
            // structure dispatch AND the I-frame QP-cascade depth.
            let gop_sig: Vec<f64> = (0..n_gops)
                .map(|g| gop_bi_residual(&frames[g * gop..((g + 1) * gop).min(frames.len())], w, h, 1))
                .collect();
            let gop_fav: Vec<bool> = if self.cfg.bframes_adaptive {
                gop_sig.iter().map(|&s| bframes_favorable(s)).collect()
            } else {
                vec![true; n_gops]
            };
            let gop_iqp: Vec<i32> = gop_sig.iter().map(|&s| gop_iqp_offset(s, self.cfg.i_qp_offset)).collect();
            let gop_bqp: Vec<i32> = gop_sig.iter().map(|&s| gop_bframe_qp_offset(s, self.cfg.bframe_qp_offset)).collect();
            // Adaptive B-COUNT: how many B's per anchor gap. Fixed `bframes` unless
            // `auto`, where the 2-gap/1-gap bi-residual RATIO picks it — content that
            // survives wider anchor spacing (low ratio) carries more cheap B's; simple
            // translation (high ratio) wants a single equidistant B.
            let bcount = if self.cfg.bframes_adaptive {
                adaptive_bcount(frames, w, h, self.cfg.bframes as usize)
            } else {
                self.cfg.bframes as usize
            };
            if gop_fav.iter().any(|&f| f) {
                return Ok(self.encode_all_bframes(frames, bcount, &gop_fav, &gop_iqp, &gop_bqp));
            }
            // No GOP is B-favorable → pure P-only (byte-identical to bframes=0).
            let mut pcfg = self.cfg.clone();
            pcfg.bframes = 0;
            return Encoder::new(pcfg)?.encode_all(frames);
        }
        // Rate control threads state across frames → it must stay sequential. mb-tree
        // runs in RC mode too: per-GOP lookahead → per-MB offsets (per-GOP centered, so
        // rate-neutral per GOP), and the controller supplies each frame's base QP.
        // (MEASURED: routing the cross-frame allocation through the RC's complexity
        // instead of centering was worse — the centered offsets carry it correctly.)
        if self.cfg.bitrate > 0 {
            let mut enc = Encoder::new(self.cfg.clone())?;
            let offs: Vec<Vec<i32>> = if self.cfg.mbtree {
                let gop = self.cfg.gop_size.max(1) as usize;
                frames
                    .chunks(gop)
                    .flat_map(|g| mbtree::gop_qp_offsets(&self.cfg, g, self.cfg.mbtree_strength))
                    .collect()
            } else {
                Vec::new()
            };
            return frames
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    if let Some(qpo) = offs.get(i) {
                        enc.pending_qpo = Some(qpo.clone());
                    }
                    enc.try_encode(f)
                })
                .collect();
        }
        let gop = self.cfg.gop_size.max(1) as usize;
        let gops: Vec<&[YuvFrame]> = frames.chunks(gop).collect();
        if gops.is_empty() {
            return Ok(Vec::new());
        }
        let n = std::env::var("RUSTY_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| std::thread::available_parallelism().map(|n| n.get()).ok())
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
                            // mb-tree temporal AQ: a per-GOP lookahead over the GOP's
                            // source frames yields per-frame per-MB QP offsets (the GOP
                            // is the natural window — the IDR resets references). Off →
                            // empty → byte-identical.
                            let offs = if cfg.mbtree {
                                mbtree::gop_qp_offsets(cfg, gops_ref[i], cfg.mbtree_strength)
                            } else {
                                Vec::new()
                            };
                            let aus: Vec<Vec<u8>> = gops_ref[i]
                                .iter()
                                .enumerate()
                                .map(|(fi, f)| {
                                    if let Some(o) = offs.get(fi) {
                                        enc.set_pending_qpo(o.clone());
                                    }
                                    enc.encode(f)
                                })
                                .collect();
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

    /// B-frame reorder pipeline (sequential). Produces access units in **coding
    /// order** (the decoder reorders to display order by POC). Structure: an IDR
    /// at each `gop_size` boundary, a P anchor every `bframes+1` frames within a
    /// GOP, `bframes` non-reference B-frames between consecutive anchors, and the
    /// last frame forced to an anchor so trailing B's always have a future
    /// reference. Each anchor is coded before the B's that reference it.
    /// `gop_favorable[g]` (content-adaptive): GOP `g` codes B-frames only when
    /// `true`; a `false` GOP is coded all-P (every frame an anchor) so busy segments
    /// of a mixed clip don't regress. Non-adaptive callers pass all-`true`.
    fn encode_all_bframes(&self, frames: &[YuvFrame], bcount: usize, gop_favorable: &[bool], gop_iqp: &[i32], gop_bqp: &[i32]) -> Vec<Vec<u8>> {
        let n = frames.len();
        if n == 0 {
            return Vec::new();
        }
        let step = bcount.max(1) + 1; // B's per anchor gap + 1 (adaptive in `auto`)
        let gop = self.cfg.gop_size.max(1) as usize;
        // A B-capable config: Main profile + ≥2 refs so the DPB holds both anchors.
        let mut cfg = self.cfg.clone();
        cfg.num_ref_frames = cfg.num_ref_frames.max(2);
        let sps = Sps::from_config(&cfg);
        let pps = Pps::from_config(&cfg);

        // Anchor display-indices: IDR at GOP starts, P anchors every `step`, plus
        // the frame right before each IDR boundary and the clip's last frame — a
        // trailing B with no future reference IN ITS OWN GOP would otherwise be
        // coded after the next GOP's IDR (which clears the DPB), losing its anchors.
        let mut is_anchor = vec![false; n];
        for (d, a) in is_anchor.iter_mut().enumerate() {
            // A non-favorable GOP is coded all-P (every frame an anchor); a favorable
            // one uses the B structure.
            *a = if gop_favorable.get(d / gop).copied().unwrap_or(true) {
                d % gop == 0 || (d % gop) % step == 0 || (d + 1) % gop == 0
            } else {
                true
            };
        }
        is_anchor[n - 1] = true;

        // mb-tree temporal AQ over the ANCHOR reference chain: B-frames are
        // non-reference leaves (mb-tree offsets them at ~0 anyway), so the lookahead
        // runs over each GOP's anchor sub-sequence — the frames that actually form the
        // reference chain — and only anchors receive an offset. `mbtree_off[d]` is that
        // anchor's per-MB offset (empty for B's / when off → byte-identical).
        let mbtree_off: Vec<Vec<i32>> = if cfg.mbtree {
            let mut off = vec![Vec::new(); n];
            let mut g = 0;
            while g < n {
                let gop_end = (g + gop).min(n);
                let anchors: Vec<usize> = (g..gop_end).filter(|&d| is_anchor[d]).collect();
                let aframes: Vec<YuvFrame> = anchors.iter().map(|&d| frames[d].clone()).collect();
                let offs = mbtree::gop_qp_offsets(&cfg, &aframes, cfg.mbtree_strength);
                for (i, &d) in anchors.iter().enumerate() {
                    off[d] = offs[i].clone();
                }
                g = gop_end;
            }
            off
        } else {
            Vec::new()
        };

        // Coding order: each anchor (display order), then the B's before it.
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut prev: Option<usize> = None;
        for d in 0..n {
            if !is_anchor[d] {
                continue;
            }
            order.push(d);
            if let Some(p) = prev {
                order.extend((p + 1)..d);
            }
            prev = Some(d);
        }

        let mut dpb: Vec<RefFrame> = Vec::new();
        let mut aus: Vec<Vec<u8>> = Vec::with_capacity(n);
        let mut frame_num: u32 = 0;
        for &d in &order {
            let is_idr = d % gop == 0;
            if is_idr {
                dpb.clear();
                frame_num = 0;
            }
            let is_b = !is_anchor[d];
            let gop_start = (d / gop) * gop;
            let poc = ((d - gop_start) as i32) * 2; // POC = display position within the GOP
            let iqp = gop_iqp.get(d / gop).copied().unwrap_or(cfg.i_qp_offset);
            let bqp = gop_bqp.get(d / gop).copied().unwrap_or(cfg.bframe_qp_offset);
            let qpo: &[i32] = mbtree_off.get(d).map(|v| v.as_slice()).unwrap_or(&[]);
            let (au, recon) =
                code_picture(&cfg, &sps, &pps, &frames[d], is_idr, is_b, poc, frame_num, &dpb, iqp, bqp, qpo);
            aus.push(au);
            if !is_b {
                if let Some(r) = recon {
                    dpb.insert(0, r);
                    dpb.truncate(cfg.num_ref_frames as usize);
                }
                frame_num = (frame_num + 1) % 16;
            }
        }
        aus
    }
}

/// Codes ONE picture (IDR / P anchor / B) with explicit POC + frame_num + DPB.
/// Returns the access unit and, for reference pictures, the reconstruction to add
/// to the DPB (B-frames are non-reference → `None`). `dpb` is most-recent-first.
#[allow(clippy::too_many_arguments)]
fn code_picture(
    cfg: &EncoderConfig,
    sps: &Sps,
    pps: &Pps,
    frame: &YuvFrame,
    is_idr: bool,
    is_b: bool,
    poc: i32,
    frame_num: u32,
    dpb: &[RefFrame],
    i_qp_offset: i32,
    b_qp_offset: i32,
    qpo: &[i32],
) -> (Vec<u8>, Option<RefFrame>) {
    let mut out = Vec::new();
    let mut w = BitWriter::with_capacity(cfg.width * cfg.height / 2 + 4096);
    let poc_lsb = (poc as u32) & 0xF; // log2_max_pic_order_cnt_lsb = 4
    // Per-GOP QP cascade, both offsets content-adaptive: B-frames are non-reference →
    // quantize HARDER (`b_qp_offset`, deeper on very predictable GOPs); the GOP's
    // I-frame is the root reference → quantize FINER (`i_qp_offset`, deeper on
    // predictable GOPs where the I dominates the bits).
    let qp = if is_b {
        (cfg.qp as i32 + b_qp_offset).clamp(0, 51) as u8
    } else if is_idr {
        (cfg.qp as i32 + i_qp_offset).clamp(0, 51) as u8
    } else {
        cfg.qp
    };
    let (nal_type, nal_ref_idc, recon) = if is_idr {
        sps.to_nal().write_annex_b(&mut out);
        pps.to_nal().write_annex_b(&mut out);
        slice::write_idr_slice_header(&mut w, cfg, qp);
        let mut r = if cfg.cabac {
            mb16::encode_slice_data_cabac_intra(&mut w, cfg, frame, qp, qpo)
        } else {
            mb16::encode_slice_data(&mut w, cfg, frame, qp, false, &[], qpo)
        };
        r.poc = poc;
        r.frame_num = frame_num;
        (NalUnitType::IdrSlice, 3u8, Some(r))
    } else if is_b {
        // B is non-reference. We signal one active reference per list: L0[0] =
        // nearest PAST anchor (highest poc < current), L1[0] = nearest FUTURE anchor
        // (lowest poc > current) — the heads of the decoder's POC-ordered B lists.
        let l0 = dpb.iter().filter(|r| r.poc < poc).max_by_key(|r| r.poc);
        let l1 = dpb.iter().filter(|r| r.poc > poc).min_by_key(|r| r.poc);
        slice::write_b_slice_header(&mut w, cfg, qp, frame_num, poc_lsb, 1, 1);
        match (l0, l1) {
            // B-frames are non-reference leaves — mb-tree offsets them at 0 anyway, so
            // `qpo` is `&[]` here (the anchor reference chain carries the temporal AQ).
            (Some(l0), Some(l1)) if cfg.cabac => {
                mb16::encode_slice_data_cabac_b(&mut w, cfg, frame, qp, poc, l0, l1, &[])
            }
            (Some(l0), Some(l1)) => mb16::encode_slice_data_b(&mut w, cfg, frame, qp, poc, l0, l1, &[]),
            // A B with no bracketing anchor pair can't be List-0/1 coded; fall back
            // to an all-B_Skip slice (spatial-direct) so the stream stays legal.
            _ => {
                let n = cfg.mb_width() * cfg.mb_height();
                if cfg.cabac {
                    mb16::encode_all_skip_b_cabac(&mut w, cfg, qp, n);
                } else {
                    w.write_ue(n as u32);
                    w.rbsp_trailing_bits();
                }
            }
        }
        (NalUnitType::NonIdrSlice, 0u8, None)
    } else {
        // P anchor: L0 = the DPB (past anchors), ordered most-recent-first. Our CABAC
        // decode + encode are single-reference (ref_idx not coded), so a CABAC P uses
        // only the most-recent anchor (the B path forces num_ref_frames>=2 for the L0/
        // L1 lists, but each P ref list stays length 1). CAVLC P uses the full DPB.
        let p_dpb: &[RefFrame] = if cfg.cabac { &dpb[..dpb.len().min(1)] } else { dpb };
        slice::write_p_slice_header(&mut w, cfg, qp, frame_num, poc_lsb, p_dpb.len());
        let mut r = if cfg.cabac {
            mb16::encode_slice_data_cabac_p(&mut w, cfg, frame, qp, p_dpb, qpo)
        } else {
            mb16::encode_slice_data(&mut w, cfg, frame, qp, true, dpb, qpo)
        };
        r.poc = poc;
        r.frame_num = frame_num;
        (NalUnitType::NonIdrSlice, 3u8, Some(r))
    };
    let slice_bytes = w.into_bytes();
    NalUnit::new(nal_ref_idc, nal_type, slice_bytes).write_annex_b(&mut out);
    (out, recon)
}

/// The B-favorability threshold on the per-GOP signal (`gop_bi_residual`): below it,
/// motion is predictable enough that B-frames pay AND the I-frame dominates the GOP's
/// bits (so it wants a deeper QP cascade); above it the GOP is busy.
const BI_THRESH: f64 = 4.0;

/// Whether a GOP's temporal residual makes B-frames pay (predictable motion).
fn bframes_favorable(residual: f64) -> bool {
    residual < BI_THRESH
}

/// Content-adaptive per-GOP I-frame QP offset (the ip_ratio cascade, DISPATCHED by
/// content). `base` is the busy-GOP offset (`cfg.i_qp_offset`, default −3); a
/// predictable GOP — where the I-frame is a large fraction of the GOP's bits, so
/// investing in it pays outsized — gets up to 2 QP steps FINER, ramping from `base`
/// at the threshold to `base−2` at residual 0. Calibrated: busy ≈ −3, compressible
/// ≈ −5 (−11.6% vs −7.3% at −3). `base == 0` (the opt-out) disables it entirely so
/// the byte-identical escape hatch survives.
fn gop_iqp_offset(residual: f64, base: i32) -> i32 {
    if base == 0 {
        return 0;
    }
    let bonus = (2.0 * ((BI_THRESH - residual) / BI_THRESH).clamp(0.0, 1.0)).round() as i32;
    base - bonus
}

/// Content-adaptive per-GOP B-frame QP offset. B-frames are non-reference, so on a
/// VERY predictable GOP (bi-pred + spatial-direct nail them → tiny residual) they can
/// be quantized much HARDER for near-free bits. But the optimum is KNIFE-EDGE in the
/// signal — measured ~+8 at residual 0.10 yet ~+2 by residual 0.29 (and a heavy LOSS
/// at +12 there) — so unlike the I-cascade this ramp is STEEP and confined to the
/// near-perfect-motion regime: `base` (default +2) everywhere, boosted up to +4 only
/// as residual → 0 (decaying to `base` by ~0.3/px). Deliberately conservative — it
/// helps near-static / clean-pan content and must never touch the common range.
fn gop_bframe_qp_offset(residual: f64, base: i32) -> i32 {
    const RAMP: f64 = 0.3; // residual above this gets no boost (steep — see calibration)
    let boost = (4.0 * ((RAMP - residual) / RAMP).clamp(0.0, 1.0)).round() as i32;
    base + boost
}

/// Adaptive B-COUNT (B-frames per anchor gap) for `auto` mode. The RATIO of the
/// 2-gap to 1-gap bi-prediction residual measures how fast bi-pred degrades as the
/// anchor spacing widens: LOW ratio (content survives wider gaps) carries MORE cheap
/// non-reference B's; HIGH ratio (simple translation — degrades fast, so wider anchors
/// cost more than the extra B's save) wants a single equidistant B. Calibrated on
/// pans/zoom: ratio ≥ 1.8 → 1, ≥ 1.4 → 2, else 3. Capped at `max_b` (the `auto` cap).
fn adaptive_bcount(frames: &[YuvFrame], w: usize, h: usize, max_b: usize) -> usize {
    let cap = max_b.clamp(1, 3);
    let g1 = gop_bi_residual(frames, w, h, 1);
    let g2 = gop_bi_residual(frames, w, h, 2);
    if !g1.is_finite() || !g2.is_finite() {
        return 1;
    }
    let ratio = g2 / g1.max(1e-3);
    // Calibrated on this encoder's (subsampled global-ME) ratios: a simple
    // translation degrades to ~1.5 (→ 1 B), predictable-under-wide-gaps content sits
    // ~1.3 or below (→ 3 B).
    let c = if ratio >= 1.4 { 1 } else if ratio >= 1.3 { 2 } else { 3 };
    c.clamp(1, cap)
}

/// Cheap content signal for the content-adaptive dispatch: the mean per-pixel
/// residual of a coarse GLOBAL-motion BI-prediction, over a subsample of interior
/// frames. Low = temporally predictable (bi-pred + spatial-direct cheap → B-frames
/// WIN, and the I-frame dominates → deeper QP cascade); high = busy motion.
/// `f64::INFINITY` when the GOP is too short to measure (treated as busy).
///
/// Global (not block) ME keeps it O(pixels)-cheap and biases toward "coherent
/// motion", which is what spatial-direct/skip exploit. Thresholds calibrated on
/// extremes (pan ~0.03/px, high-motion ~12.3/px); refine on a corpus.
fn gop_bi_residual(frames: &[YuvFrame], w: usize, h: usize, gap: usize) -> f64 {
    let n = frames.len();
    if n < 2 * gap + 1 || w < 48 || h < 48 {
        return f64::INFINITY;
    }
    // Subsampled SAD of `cur` vs `rf` shifted by (dx,dy): interior pixels only
    // (|shift| ≤ 15 stays in-bounds, no clamping), every 4th pixel for speed.
    let sad = |cur: &[u8], rf: &[u8], dx: isize, dy: isize| -> u64 {
        let mut s = 0u64;
        let mut y = 16;
        while y < h - 16 {
            let cbase = (y * w) as isize;
            let rbase = ((y as isize + dy) * w as isize) + dx;
            let mut x = 16isize;
            while x < (w - 16) as isize {
                let c = cur[(cbase + x) as usize] as i32;
                let r = rf[(rbase + x) as usize] as i32;
                s += (c - r).unsigned_abs() as u64;
                x += 8;
            }
            y += 8;
        }
        s
    };
    // Coarse global ME: ±12 step 4, then refine ±3 step 1.
    let global_me = |cur: &[u8], rf: &[u8]| -> (isize, isize) {
        let (mut best, mut bc) = ((0isize, 0isize), u64::MAX);
        let mut dy = -12;
        while dy <= 12 {
            let mut dx = -12;
            while dx <= 12 {
                let c = sad(cur, rf, dx, dy);
                if c < bc {
                    bc = c;
                    best = (dx, dy);
                }
                dx += 4;
            }
            dy += 4;
        }
        for dy in best.1 - 3..=best.1 + 3 {
            for dx in best.0 - 3..=best.0 + 3 {
                let c = sad(cur, rf, dx, dy);
                if c < bc {
                    bc = c;
                    best = (dx, dy);
                }
            }
        }
        best
    };
    let mut n_samp = 0usize;
    {
        let mut y = 16;
        while y < h - 16 {
            let mut x = 16;
            while x < w - 16 {
                n_samp += 1;
                x += 8;
            }
            y += 8;
        }
    }
    let step = (n / 5).max(1);
    let (mut total, mut cnt) = (0f64, 0usize);
    // `gap` frames each side (1 = adjacent, for the B/P dispatch; 2 probes how well
    // bi-prediction survives WIDER anchor spacing, for the adaptive B-count).
    let mut d = gap;
    while d < n - gap {
        let (cur, past, fut) = (&frames[d].y, &frames[d - gap].y, &frames[d + gap].y);
        let (mpx, mpy) = global_me(cur, past);
        let (mfx, mfy) = global_me(cur, fut);
        let mut bi = 0u64;
        let mut y = 16;
        while y < h - 16 {
            let mut x = 16isize;
            while x < (w - 16) as isize {
                let c = cur[y * w + x as usize] as i32;
                let p = past[((y as isize + mpy) * w as isize + x + mpx) as usize] as i32;
                let f = fut[((y as isize + mfy) * w as isize + x + mfx) as usize] as i32;
                bi += (c - ((p + f + 1) >> 1)).unsigned_abs() as u64;
                x += 8;
            }
            y += 8;
        }
        total += bi as f64 / n_samp as f64;
        cnt += 1;
        d += step;
    }
    if cnt > 0 {
        total / cnt as f64
    } else {
        f64::INFINITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_config() {
        // High profile is supported (8x8 transform); a High-profile 8x8 stream must be
        // CAVLC (our decoder has no CABAC 8x8) — that combination is rejected.
        let mut cfg = EncoderConfig::new(16, 16);
        cfg.profile = Profile::High;
        cfg.transform_8x8 = true;
        cfg.cabac = true;
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
