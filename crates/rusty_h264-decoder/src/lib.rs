//! Pure-Rust H.264 (Constrained Baseline) decoder.
//!
//! Parses SPS/PPS/IDR-slice headers and reconstructs I_16x16 (DC-predicted)
//! macroblocks: CAVLC residual decode, inverse transform (incl. luma/chroma DC
//! Hadamard), intra DC prediction. The reconstruction path is shared with the
//! encoder so the two agree bit-for-bit. Inter prediction and deblocking land in
//! later generations behind this same API.

mod mb16;
mod params;

pub use params::{Pps, Sps};

use mb16::FrameDecoder;
use rusty_h264_common::bit_reader::OutOfData;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::{BitReader, NalUnitType, YuvFrame};

/// Decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Bitstream ended unexpectedly.
    Truncated,
    /// A required parameter set was missing before a slice.
    MissingParameterSet,
    /// A coding tool outside the implemented subset appeared in the stream.
    Unsupported(&'static str),
}

impl From<OutOfData> for DecodeError {
    fn from(_: OutOfData) -> Self {
        DecodeError::Truncated
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("bitstream truncated"),
            DecodeError::MissingParameterSet => f.write_str("slice before SPS/PPS"),
            DecodeError::Unsupported(s) => write!(f, "unsupported coding tool: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A reference picture: deblocked reconstruction at coded resolution.
/// Stored now (4a); read by motion compensation in 4b.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub cw: usize,
    pub ch: usize,
    /// `frame_num` of the picture, for PicNum-based reference-list reordering.
    pub frame_num: u32,
    /// `PicOrderCnt` of the picture, for B-slice reference-list ordering.
    pub poc: i32,
    /// Long-term reference state. Long-term refs sit after short-term ones in
    /// `RefPicList0` (ordered by `long_term_idx` ascending) and survive the
    /// sliding window until explicitly unmarked (spec §8.2.4).
    pub long_term: bool,
    pub long_term_idx: u32,
}

/// A memory-management control operation (`dec_ref_pic_marking`, spec §7.4.3.3).
#[derive(Clone, Copy)]
enum Mmco {
    /// 1: mark a short-term reference (by PicNum) as unused.
    Unref(u32),
    /// 2: mark a long-term reference (by LongTermPicNum) as unused.
    UnrefLong(u32),
    /// 3: assign a short-term reference (by PicNum) a LongTermFrameIdx.
    AssignLong(u32, u32),
    /// 4: drop long-term references with idx ≥ max_long_term_frame_idx_plus1.
    MaxLong(u32),
    /// 5: empty the DPB (and reset the current picture's frame_num to 0).
    Reset,
    /// 6: mark the current picture long-term with this LongTermFrameIdx.
    CurrentLong(u32),
}

/// A picture being assembled from one or more slices (spec allows a picture to
/// be split into multiple slices). Finalized — deblocked, output, and entered
/// into the DPB — once all its macroblocks are decoded.
struct PendingPic {
    fd: mb16::FrameDecoder,
    frame_num: u32,
    poc: i32,
    next_mb: usize,
    total_mb: usize,
    slice_count: u16,
    deblock: bool,
    filter_offset_a: i32,
    filter_offset_b: i32,
    crop_r: usize,
    crop_b: usize,
    max_refs: usize,
    log2_max_frame_num: u32,
    /// `false` for a non-reference picture (nal_ref_idc == 0): output it but do
    /// not enter it into the DPB.
    is_reference: bool,
    idr_long_term: bool,
    mmco_ops: Vec<Mmco>,
}

/// A Constrained Baseline H.264 decoder. Holds the most recent parameter sets
/// and the previous decoded picture (the inter reference) across calls.
#[derive(Default)]
pub struct Decoder {
    /// Active parameter sets, keyed by id — a stream may carry several and switch
    /// between them per slice (spec §7.3.2.1/.2).
    sps: std::collections::HashMap<u32, Sps>,
    pps: std::collections::HashMap<u32, Pps>,
    /// Decoded-picture buffer (most-recent first); `ref_idx` indexes into this.
    refs: Vec<RefFrame>,
    /// The picture currently being assembled from its slices, if any.
    cur: Option<PendingPic>,
    /// Picture-order-count state (spec §8.2.1). Tracks the previous reference
    /// picture's MSB/LSB (type 0) and frame-num offset (types 1/2) so display
    /// order can be recovered — needed once B-pictures (out-of-order) land.
    poc: PocState,
    /// `PicOrderCnt` of the most recently returned picture (display-order key).
    last_poc: i32,
    /// `frame_num` of the previous short-term reference picture, for detecting
    /// gaps in `frame_num` (spec §8.2.5.2).
    prev_ref_frame_num: u32,
}

/// Running picture-order-count derivation state.
#[derive(Default)]
struct PocState {
    prev_msb: i32,
    prev_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: i64,
}

impl Decoder {
    /// Creates a decoder with no parameter sets yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes a complete Annex-B access unit, returning the reconstructed,
    /// cropped frame if the access unit contained a coded picture.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<YuvFrame>, DecodeError> {
        let mut frame = None;
        for nal in split_annex_b(annex_b) {
            if nal.is_empty() {
                continue;
            }
            let nal_type = NalUnitType::from_id(nal[0]);
            let rbsp = emulation_unprevent(&nal[1..]);
            match nal_type {
                NalUnitType::Sps => {
                    let s = Sps::parse(&rbsp)?;
                    self.sps.insert(s.seq_parameter_set_id, s);
                }
                NalUnitType::Pps => {
                    let p = Pps::parse(&rbsp)?;
                    self.pps.insert(p.pic_parameter_set_id, p);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    let nal_ref_idc = (nal[0] >> 5) & 3;
                    let is_idr = nal_type == NalUnitType::IdrSlice;
                    if let Some(f) = self.decode_slice(&rbsp, is_idr, nal_ref_idc)? {
                        frame = Some(f);
                    }
                }
                _ => {} // SEI, AUD, etc. ignored
            }
        }
        Ok(frame)
    }

    /// Decodes a complete Annex-B byte stream and returns every picture in
    /// **display order** (`PicOrderCnt` within each GOP; an IDR ends a GOP).
    ///
    /// This is the convenient whole-stream entry point — it handles access-unit
    /// splitting, multi-slice picture assembly, and B-picture reordering — versus
    /// the lower-level per-access-unit [`Decoder::decode`], which returns pictures
    /// in decode order.
    pub fn decode_stream(&mut self, annex_b: &[u8]) -> Result<Vec<YuvFrame>, DecodeError> {
        let mut out = Vec::new();
        let mut gop: Vec<(i32, YuvFrame)> = Vec::new();
        for au in split_access_units(annex_b) {
            if au_is_idr(au) {
                flush_gop(&mut gop, &mut out); // emit the prior GOP before the IDR
            }
            if let Some(frame) = self.decode(au)? {
                gop.push((self.last_poc, frame));
            }
        }
        flush_gop(&mut gop, &mut out);
        Ok(out)
    }

    fn decode_slice(
        &mut self,
        rbsp: &[u8],
        is_idr: bool,
        nal_ref_idc: u8,
    ) -> Result<Option<YuvFrame>, DecodeError> {
        let mut r = BitReader::new(rbsp);
        // --- slice_header ---
        let first_mb_in_slice = r.read_ue()? as usize;
        let slice_type = r.read_ue()?;
        let is_p = matches!(slice_type, 0 | 5);
        let is_b = matches!(slice_type, 1 | 6);
        if !is_p && !is_b && !matches!(slice_type, 2 | 7) {
            return Err(DecodeError::Unsupported("SP/SI slices"));
        }
        // Resolve the parameter sets this slice references (by id).
        let pic_parameter_set_id = r.read_ue()?;
        let pps = self.pps.get(&pic_parameter_set_id).cloned().ok_or(DecodeError::MissingParameterSet)?;
        let sps = self.sps.get(&pps.seq_parameter_set_id).cloned().ok_or(DecodeError::MissingParameterSet)?;
        let sps = &sps;
        let pps = &pps;
        if pps.entropy_coding_mode_flag {
            return Err(DecodeError::Unsupported("CABAC"));
        }
        let frame_num = r.read_bits(sps.log2_max_frame_num)?;
        if is_idr {
            let _idr_pic_id = r.read_ue()?;
        }
        // pic_order_cnt fields (spec §7.3.3). `field_pic_flag` is always 0
        // (frame_mbs_only). Captured to derive PicOrderCnt for display ordering.
        let mut poc_lsb = 0u32;
        let mut delta_poc_bottom = 0i32;
        if sps.pic_order_cnt_type == 0 {
            poc_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb)?;
            if pps.bottom_field_pic_order_present {
                delta_poc_bottom = r.read_se()?;
            }
        } else if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero {
            let _delta_pic_order_cnt_0 = r.read_se()?;
            if pps.bottom_field_pic_order_present {
                let _delta_pic_order_cnt_1 = r.read_se()?;
            }
        }
        // PicOrderCnt is determined by the first slice of the picture; later
        // slices share it (and must not re-advance the POC state).
        let pic_poc = if first_mb_in_slice == 0 {
            self.compute_poc(sps, is_idr, nal_ref_idc, frame_num, poc_lsb, delta_poc_bottom)
        } else {
            self.cur.as_ref().map_or(0, |p| p.poc)
        };
        // redundant_pic_cnt: a non-zero value marks a *redundant* coded picture
        // (an alternative representation of the primary picture). A primary
        // decoder discards it (spec §7.4.3, §8.2.5 note). Must be read here or the
        // rest of the slice header desyncs.
        if pps.redundant_pic_cnt_present_flag {
            let redundant_pic_cnt = r.read_ue()?;
            if redundant_pic_cnt != 0 {
                return Ok(None);
            }
        }
        // B slices choose direct-mode derivation here (spec §7.3.3).
        let direct_spatial = if is_b { r.read_bit()? } else { true };
        let mut num_ref_idx_l0 = pps.num_ref_idx_l0_default as usize;
        let mut num_ref_idx_l1 = pps.num_ref_idx_l1_default as usize;
        let mut reorder_l0: Vec<(u32, u32)> = Vec::new();
        let mut reorder_l1: Vec<(u32, u32)> = Vec::new();
        if is_p || is_b {
            // num_ref_idx_active_override_flag
            if r.read_bit()? {
                num_ref_idx_l0 = (r.read_ue()? + 1) as usize;
                if is_b {
                    num_ref_idx_l1 = (r.read_ue()? + 1) as usize;
                }
            }
            // ref_pic_list_modification_flag_l0
            if r.read_bit()? {
                parse_ref_pic_list_modification(&mut r, &mut reorder_l0)?;
            }
            if is_b && r.read_bit()? {
                // ref_pic_list_modification_flag_l1
                parse_ref_pic_list_modification(&mut r, &mut reorder_l1)?;
            }
        }
        // Explicit weighted prediction carries a pred_weight_table() here; we
        // don't support it (and reading past it would desync). Implicit weighted
        // bipred (idc 2) carries no table and is handled in the MC averaging.
        if (is_p && pps.weighted_pred) || (is_b && pps.weighted_bipred_idc == 1) {
            return Err(DecodeError::Unsupported("explicit weighted prediction"));
        }
        // dec_ref_pic_marking (spec §7.3.3.3) — present only for reference
        // pictures (nal_ref_idc != 0). Reading it for a non-reference slice would
        // desync the rest of the header.
        let mut idr_long_term = false;
        let mut mmco_ops: Vec<Mmco> = Vec::new();
        if nal_ref_idc == 0 {
            // non-reference picture: no marking syntax
        } else if is_idr {
            let _no_output_of_prior_pics = r.read_bit()?;
            idr_long_term = r.read_bit()?; // long_term_reference_flag
        } else if r.read_bit()? {
            // adaptive_ref_pic_marking_mode_flag
            loop {
                let op = r.read_ue()?;
                match op {
                    0 => break,
                    1 => mmco_ops.push(Mmco::Unref(r.read_ue()?)),
                    2 => mmco_ops.push(Mmco::UnrefLong(r.read_ue()?)),
                    3 => {
                        let diff = r.read_ue()?;
                        let idx = r.read_ue()?;
                        mmco_ops.push(Mmco::AssignLong(diff, idx));
                    }
                    4 => mmco_ops.push(Mmco::MaxLong(r.read_ue()?)),
                    5 => mmco_ops.push(Mmco::Reset),
                    6 => mmco_ops.push(Mmco::CurrentLong(r.read_ue()?)),
                    _ => return Err(DecodeError::Unsupported("invalid MMCO")),
                }
                if mmco_ops.len() > 128 {
                    return Err(DecodeError::Truncated);
                }
            }
        }
        let slice_qp_delta = r.read_se()?;
        // When deblocking_filter_control_present_flag is 0 the slice carries no
        // disable_deblocking_filter_idc and it is inferred 0 — i.e. the in-loop
        // filter is ON by default (spec §7.4.3). (Our own encoder always signals
        // the control explicitly, so this default was previously untested.)
        let mut deblock = true;
        let (mut filter_offset_a, mut filter_offset_b) = (0i32, 0i32);
        if pps.deblocking_filter_control_present_flag {
            let disable_deblocking_filter_idc = r.read_ue()?;
            // idc 1 = filter off; idc 0 = on; idc 2 = on but not across slice
            // boundaries (equivalent to on for single-slice pictures).
            deblock = disable_deblocking_filter_idc != 1;
            if disable_deblocking_filter_idc != 1 {
                // FilterOffset = slice_*_offset_div2 × 2 (spec §7.4.3).
                filter_offset_a = r.read_se()? * 2;
                filter_offset_b = r.read_se()? * 2;
            }
        }
        let slice_qp = (pps.pic_init_qp + slice_qp_delta).clamp(0, 51) as u8;

        // Synthesize placeholder short-term references for any gap in frame_num
        // (spec §8.2.5.2) so the DPB / PicNum mapping stays correct.
        if first_mb_in_slice == 0 && !is_idr && sps.gaps_in_frame_num_allowed {
            self.insert_frame_num_gaps(
                frame_num,
                1u32 << sps.log2_max_frame_num,
                sps.max_num_ref_frames.max(1) as usize,
                sps.pic_width_in_mbs * 16,
                sps.pic_height_in_mbs * 16,
            );
        }

        // Build the reference list(s) for this slice. P uses RefPicList0 only;
        // B uses RefPicList0 and RefPicList1 (POC-ordered).
        let max_fn = 1u32 << sps.log2_max_frame_num;
        let (ref_list0, ref_list1) = if is_b {
            build_ref_list_b(
                &self.refs, pic_poc, frame_num, max_fn,
                num_ref_idx_l0, num_ref_idx_l1, &reorder_l0, &reorder_l1,
            )?
        } else if is_p {
            (build_ref_list_p(&self.refs, frame_num, max_fn, num_ref_idx_l0, &reorder_l0)?, Vec::new())
        } else {
            (Vec::new(), Vec::new())
        };
        // B-slice header + reference lists (List0/List1) are parsed and built
        // here; the B macroblock decoder (bi-prediction, direct modes, B_8x8) is
        // the next layer. Reject gracefully at the MB layer for now.
        if is_b {
            let _ = (&ref_list0, &ref_list1, direct_spatial);
            return Err(DecodeError::Unsupported("B macroblock decoding"));
        }

        // --- picture assembly ---
        // first_mb_in_slice == 0 starts a new picture; otherwise this slice
        // continues the one in flight. An IDR clears the DPB at its first slice.
        if first_mb_in_slice == 0 {
            if is_idr {
                self.refs.clear();
            }
            let fd = FrameDecoder::new(
                sps.pic_width_in_mbs,
                sps.pic_height_in_mbs,
                slice_qp,
                pps.chroma_qp_index_offset,
                ref_list0,
                num_ref_idx_l0,
                pps.constrained_intra_pred_flag,
            );
            self.cur = Some(PendingPic {
                fd,
                frame_num,
                poc: pic_poc,
                next_mb: 0,
                total_mb: sps.pic_width_in_mbs * sps.pic_height_in_mbs,
                slice_count: 0,
                deblock,
                filter_offset_a,
                filter_offset_b,
                crop_r: sps.frame_crop_right as usize,
                crop_b: sps.frame_crop_bottom as usize,
                max_refs: sps.max_num_ref_frames.max(1) as usize,
                log2_max_frame_num: sps.log2_max_frame_num,
                is_reference: nal_ref_idc != 0,
                idr_long_term,
                mmco_ops,
            });
        } else {
            // Continuation slice: reset the per-slice QP + reference list.
            let Some(pic) = self.cur.as_mut() else {
                return Err(DecodeError::Unsupported("slice continues a missing picture"));
            };
            pic.fd.begin_slice(slice_qp, ref_list0, num_ref_idx_l0);
            // Latest slice's marking/deblock parameters win at finalization.
            pic.deblock = deblock;
            pic.filter_offset_a = filter_offset_a;
            pic.filter_offset_b = filter_offset_b;
            pic.idr_long_term |= idr_long_term;
            pic.mmco_ops.extend(mmco_ops);
        }

        let pic = self.cur.as_mut().expect("pending picture set above");
        let first = first_mb_in_slice.min(pic.total_mb);
        let next = pic
            .fd
            .decode_slice_data(&mut r, is_p, first)
            .map_err(|e| match e {
                mb16::MbError::Truncated => DecodeError::Truncated,
                mb16::MbError::Unsupported(s) => DecodeError::Unsupported(s),
            })?;
        pic.next_mb = next;
        pic.slice_count += 1;

        if pic.next_mb < pic.total_mb {
            return Ok(None); // picture not yet complete
        }

        // --- finalize the completed picture ---
        let pic = self.cur.take().expect("pending picture");
        let PendingPic {
            mut fd,
            frame_num,
            poc,
            deblock,
            filter_offset_a,
            filter_offset_b,
            crop_r,
            crop_b,
            max_refs,
            log2_max_frame_num,
            is_reference,
            idr_long_term,
            mmco_ops,
            ..
        } = pic;
        self.last_poc = poc;
        if deblock {
            fd.deblock(filter_offset_a, filter_offset_b);
        }
        // A non-reference picture is output but never enters the DPB.
        if is_reference {
            let mut reference = fd.as_reference();
            reference.frame_num = frame_num;
            reference.poc = poc;
            if idr_long_term {
                reference.long_term = true;
                reference.long_term_idx = 0;
            }
            self.apply_ref_marking(&mut reference, &mmco_ops, frame_num, log2_max_frame_num, max_refs);
            // Track the reference frame_num for gap detection (0 after MMCO 5).
            self.prev_ref_frame_num = reference.frame_num;
        }
        Ok(Some(fd.into_frame(crop_r, crop_b)))
    }

    /// Inserts "non-existing" short-term reference frames for each `frame_num`
    /// skipped since the previous reference picture (spec §8.2.5.2). Their samples
    /// are unspecified (a conformant stream never references them); we use mid-grey
    /// so any accidental reference is benign. They occupy DPB slots and advance the
    /// sliding window, keeping PicNum/ref-list derivation correct.
    fn insert_frame_num_gaps(&mut self, frame_num: u32, max_fn: u32, max_refs: usize, w: usize, h: usize) {
        let mut expected = (self.prev_ref_frame_num + 1) % max_fn;
        let mut guard = 0;
        while expected != frame_num && guard < max_fn {
            let (cw, ch) = (w, h);
            self.refs.insert(
                0,
                RefFrame {
                    y: vec![128; cw * ch],
                    u: vec![128; (cw / 2) * (ch / 2)],
                    v: vec![128; (cw / 2) * (ch / 2)],
                    cw,
                    ch,
                    frame_num: expected,
                    poc: 0,
                    long_term: false,
                    long_term_idx: 0,
                },
            );
            self.refs.truncate(max_refs.max(1));
            self.prev_ref_frame_num = expected;
            expected = (expected + 1) % max_fn;
            guard += 1;
        }
    }

    /// The `PicOrderCnt` of the most recently returned picture. Pictures are
    /// returned in decode order; sorting them by this value yields display order
    /// (the only difference is reordered B-pictures).
    pub fn last_poc(&self) -> i32 {
        self.last_poc
    }

    /// Derives `PicOrderCnt` for the current picture (spec §8.2.1) and advances
    /// the POC state. Types 0 and 2 are exact; type 1 is approximated by
    /// frame-num order (no B-stream in scope uses it).
    fn compute_poc(
        &mut self,
        sps: &Sps,
        is_idr: bool,
        nal_ref_idc: u8,
        frame_num: u32,
        poc_lsb: u32,
        delta_bottom: i32,
    ) -> i32 {
        match sps.pic_order_cnt_type {
            0 => {
                let max_lsb = 1i32 << sps.log2_max_pic_order_cnt_lsb;
                let (prev_msb, prev_lsb) =
                    if is_idr { (0, 0) } else { (self.poc.prev_msb, self.poc.prev_lsb) };
                let lsb = poc_lsb as i32;
                let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                    prev_msb + max_lsb
                } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                    prev_msb - max_lsb
                } else {
                    prev_msb
                };
                let top = msb + lsb;
                let poc = top.min(top + delta_bottom);
                if nal_ref_idc != 0 {
                    self.poc.prev_msb = msb;
                    self.poc.prev_lsb = lsb;
                }
                poc
            }
            2 => {
                let max_fn = 1i64 << sps.log2_max_frame_num;
                let offset = if is_idr {
                    0
                } else if self.poc.prev_frame_num > frame_num {
                    self.poc.prev_frame_num_offset + max_fn
                } else {
                    self.poc.prev_frame_num_offset
                };
                let poc = if is_idr {
                    0
                } else {
                    2 * (offset + frame_num as i64) - i64::from(nal_ref_idc == 0)
                };
                self.poc.prev_frame_num_offset = offset;
                self.poc.prev_frame_num = frame_num;
                poc as i32
            }
            _ => {
                self.poc.prev_frame_num = frame_num;
                frame_num as i32 * 2
            }
        }
    }

    /// Inserts the just-decoded picture into the DPB and marks references
    /// (spec §8.2.5). With no MMCO commands this is the sliding window (evict the
    /// oldest short-term reference past capacity); with MMCO it is adaptive
    /// marking, including long-term assignment.
    fn apply_ref_marking(
        &mut self,
        reference: &mut RefFrame,
        ops: &[Mmco],
        frame_num: u32,
        log2_max_frame_num: u32,
        max_refs: usize,
    ) {
        let max = 1i64 << log2_max_frame_num;
        let curr = frame_num as i64;
        let pic_num = |rf: &RefFrame| -> i64 {
            if (rf.frame_num as i64) > curr {
                rf.frame_num as i64 - max
            } else {
                rf.frame_num as i64
            }
        };

        if ops.is_empty() {
            // Sliding window: insert the current (short-term) picture, then evict
            // the oldest short-term reference while over capacity (long-term refs
            // are retained).
            self.refs.insert(0, reference.clone());
            while self.refs.len() > max_refs {
                match self.refs.iter().rposition(|r| !r.long_term) {
                    Some(pos) => {
                        self.refs.remove(pos);
                    }
                    None => break,
                }
            }
            return;
        }

        // Adaptive marking (MMCO), applied in order.
        for &op in ops {
            match op {
                Mmco::Unref(diff) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs.retain(|r| r.long_term || pic_num(r) != target);
                }
                Mmco::UnrefLong(ltpn) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == ltpn));
                }
                Mmco::AssignLong(diff, idx) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == idx));
                    for r in self.refs.iter_mut() {
                        if !r.long_term && pic_num(r) == target {
                            r.long_term = true;
                            r.long_term_idx = idx;
                        }
                    }
                }
                Mmco::MaxLong(max_plus1) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx + 1 > max_plus1));
                }
                Mmco::Reset => {
                    self.refs.clear();
                    reference.frame_num = 0;
                }
                Mmco::CurrentLong(idx) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == idx));
                    reference.long_term = true;
                    reference.long_term_idx = idx;
                }
            }
        }
        self.refs.insert(0, reference.clone());
        // Safety net so a malformed marking stream can't grow the DPB unbounded.
        let cap = max_refs.max(16);
        if self.refs.len() > cap {
            self.refs.truncate(cap);
        }
    }
}

/// Emits a GOP's buffered pictures in display order (sorted by `PicOrderCnt`).
fn flush_gop(gop: &mut Vec<(i32, YuvFrame)>, out: &mut Vec<YuvFrame>) {
    gop.sort_by_key(|(poc, _)| *poc);
    out.extend(gop.drain(..).map(|(_, f)| f));
}

/// Whether an access unit contains an IDR coded-slice NAL.
fn au_is_idr(au: &[u8]) -> bool {
    split_annex_b(au)
        .iter()
        .any(|n| !n.is_empty() && NalUnitType::from_id(n[0]) == NalUnitType::IdrSlice)
}

/// Splits an Annex-B byte stream into access units, each ending after a VCL
/// (coded-slice) NAL with any preceding parameter-set/SEI NALs attached. Start
/// codes are preserved so each unit can be passed straight to [`Decoder::decode`].
fn split_access_units(stream: &[u8]) -> Vec<&[u8]> {
    // (offset of the start code, whether the NAL it begins is a VCL slice).
    let mut codes: Vec<(usize, bool)> = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let nal_type = NalUnitType::from_id(stream.get(i + 3).copied().unwrap_or(0));
            let is_vcl = matches!(nal_type, NalUnitType::IdrSlice | NalUnitType::NonIdrSlice);
            // Include a leading zero (4-byte start code) in the unit boundary.
            let sc = if i > 0 && stream[i - 1] == 0 { i - 1 } else { i };
            codes.push((sc, is_vcl));
            i += 3;
        } else {
            i += 1;
        }
    }
    if codes.is_empty() {
        return vec![stream];
    }
    let mut aus = Vec::new();
    let mut start = codes[0].0;
    for k in 0..codes.len() {
        if codes[k].1 {
            let end = codes.get(k + 1).map_or(stream.len(), |c| c.0);
            aus.push(&stream[start..end]);
            start = end;
        }
    }
    aus
}

/// Parses a `ref_pic_list_modification` command list (spec §7.3.3.1) into
/// `(modification_of_pic_nums_idc, value)` pairs, stopping at idc 3.
fn parse_ref_pic_list_modification(
    r: &mut BitReader,
    out: &mut Vec<(u32, u32)>,
) -> Result<(), DecodeError> {
    loop {
        let idc = r.read_ue()?;
        if idc == 3 {
            break;
        }
        if idc > 3 {
            return Err(DecodeError::Unsupported("invalid ref_pic_list_modification"));
        }
        let val = r.read_ue()?; // abs_diff_pic_num_minus1 / long_term_pic_num
        out.push((idc, val));
        if out.len() > 64 {
            return Err(DecodeError::Truncated); // runaway / corrupt
        }
    }
    Ok(())
}

/// Builds the P-slice `RefPicList0`: short-term references ordered by descending
/// `FrameNumWrap`, then long-term by ascending idx (spec §8.2.4.2.1), with any
/// `ref_pic_list_modification` applied.
fn build_ref_list_p(
    dpb: &[RefFrame],
    curr_frame_num: u32,
    max_frame_num: u32,
    num_active: usize,
    mods: &[(u32, u32)],
) -> Result<Vec<RefFrame>, DecodeError> {
    let curr = curr_frame_num as i64;
    let max = max_frame_num as i64;
    let pic_num = |fnum: u32| -> i64 {
        let f = fnum as i64;
        if f > curr { f - max } else { f }
    };
    let mut init: Vec<RefFrame> = dpb.iter().filter(|r| !r.long_term).cloned().collect();
    init.sort_by_key(|rf| core::cmp::Reverse(pic_num(rf.frame_num)));
    let mut long: Vec<RefFrame> = dpb.iter().filter(|r| r.long_term).cloned().collect();
    long.sort_by_key(|rf| rf.long_term_idx);
    init.extend(long);
    apply_list_modification(init, curr_frame_num, max_frame_num, num_active, mods)
}

/// Builds the B-slice `RefPicList0` and `RefPicList1` (spec §8.2.4.2.3), ordered
/// by `PicOrderCnt` relative to the current picture: List0 leads with nearer
/// past pictures, List1 with nearer future pictures. Long-term references follow.
/// Per-list `ref_pic_list_modification` is then applied.
#[allow(clippy::too_many_arguments)]
fn build_ref_list_b(
    dpb: &[RefFrame],
    curr_poc: i32,
    curr_frame_num: u32,
    max_frame_num: u32,
    num0: usize,
    num1: usize,
    mods0: &[(u32, u32)],
    mods1: &[(u32, u32)],
) -> Result<(Vec<RefFrame>, Vec<RefFrame>), DecodeError> {
    let mut less: Vec<RefFrame> =
        dpb.iter().filter(|r| !r.long_term && r.poc < curr_poc).cloned().collect();
    let mut greater: Vec<RefFrame> =
        dpb.iter().filter(|r| !r.long_term && r.poc > curr_poc).cloned().collect();
    let mut long: Vec<RefFrame> = dpb.iter().filter(|r| r.long_term).cloned().collect();
    less.sort_by_key(|r| core::cmp::Reverse(r.poc)); // nearest past first
    greater.sort_by_key(|r| r.poc); // nearest future first
    long.sort_by_key(|r| r.long_term_idx);

    let mut init0 = less.clone();
    init0.extend(greater.clone());
    init0.extend(long.clone());
    let mut init1 = greater;
    init1.extend(less);
    init1.extend(long);

    // When List1 (truncated to its active length) equals List0 and has more than
    // one entry, swap its first two entries (spec §8.2.4.2.3).
    let eq_len = num0.min(num1).min(init0.len()).min(init1.len());
    if num1 > 1
        && init1.len() > 1
        && (0..eq_len).all(|i| same_picture(&init0[i], &init1[i]))
        && eq_len == num1.min(init1.len())
        && eq_len == num0.min(init0.len())
    {
        init1.swap(0, 1);
    }

    let list0 = apply_list_modification(init0, curr_frame_num, max_frame_num, num0, mods0)?;
    let list1 = apply_list_modification(init1, curr_frame_num, max_frame_num, num1, mods1)?;
    Ok((list0, list1))
}

/// Two DPB entries refer to the same picture (used for the List1 swap rule).
fn same_picture(a: &RefFrame, b: &RefFrame) -> bool {
    a.long_term == b.long_term
        && if a.long_term { a.long_term_idx == b.long_term_idx } else { a.poc == b.poc }
}

/// Applies `ref_pic_list_modification` to an initialized reference list and
/// truncates it to `num_active` (spec §8.2.4.3). `init` is the full ordered list;
/// the result is `num_active` entries, possibly reordered. idc 0/1 reference
/// short-term pictures by PicNum, idc 2 long-term ones by LongTermFrameIdx.
fn apply_list_modification(
    init: Vec<RefFrame>,
    curr_frame_num: u32,
    max_frame_num: u32,
    num_active: usize,
    mods: &[(u32, u32)],
) -> Result<Vec<RefFrame>, DecodeError> {
    if mods.is_empty() {
        let mut init = init;
        init.truncate(num_active.max(1));
        return Ok(init);
    }
    let curr = curr_frame_num as i64;
    let max = max_frame_num as i64;
    let mut list = init.clone();
    let mut pic_num_pred = curr;
    let mut refidx = 0usize;
    for &(idc, val) in mods {
        let matches: Box<dyn Fn(&RefFrame) -> bool> = if idc == 2 {
            Box::new(move |r: &RefFrame| r.long_term && r.long_term_idx == val)
        } else {
            let abs_diff = (val as i64) + 1;
            let no_wrap = if idc == 0 {
                let x = pic_num_pred - abs_diff;
                if x < 0 { x + max } else { x }
            } else {
                let x = pic_num_pred + abs_diff;
                if x >= max { x - max } else { x }
            };
            pic_num_pred = no_wrap;
            let target = if no_wrap > curr { no_wrap - max } else { no_wrap };
            Box::new(move |r: &RefFrame| {
                let pn = if r.frame_num as i64 > curr {
                    r.frame_num as i64 - max
                } else {
                    r.frame_num as i64
                };
                !r.long_term && pn == target
            })
        };
        let found = init.iter().find(|r| matches(r)).cloned();
        let Some(found) = found else {
            return Err(DecodeError::Truncated); // references a picture not in the DPB
        };
        if refidx > list.len() {
            break;
        }
        list.insert(refidx, found);
        if let Some(dup) = list.iter().enumerate().skip(refidx + 1).find(|(_, r)| matches(r)).map(|(i, _)| i) {
            list.remove(dup);
        }
        refidx += 1;
        if refidx >= num_active {
            break;
        }
    }
    list.truncate(num_active.max(1));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_at(poc: i32, fnum: u32) -> RefFrame {
        RefFrame {
            y: vec![],
            u: vec![],
            v: vec![],
            cw: 0,
            ch: 0,
            frame_num: fnum,
            poc,
            long_term: false,
            long_term_idx: 0,
        }
    }

    #[test]
    fn b_ref_lists_ordered_by_poc() {
        // Current POC 4; DPB has past (0,2) and future (6,8) references.
        let dpb = vec![ref_at(8, 4), ref_at(6, 3), ref_at(2, 1), ref_at(0, 0)];
        let (l0, l1) = build_ref_list_b(&dpb, 4, 5, 16, 4, 4, &[], &[]).unwrap();
        // List0: nearer past first (desc), then nearer future (asc).
        assert_eq!(l0.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![2, 0, 6, 8]);
        // List1: nearer future first (asc), then nearer past (desc).
        assert_eq!(l1.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![6, 8, 2, 0]);
    }

    #[test]
    fn b_ref_list1_swap_when_equal() {
        // Only past references -> List0 and List1 initialize identically, so
        // List1's first two entries are swapped (spec §8.2.4.2.3).
        let dpb = vec![ref_at(4, 2), ref_at(2, 1), ref_at(0, 0)];
        let (l0, l1) = build_ref_list_b(&dpb, 6, 3, 16, 3, 3, &[], &[]).unwrap();
        assert_eq!(l0.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![4, 2, 0]);
        assert_eq!(l1.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![2, 4, 0]);
    }

    #[test]
    fn frame_num_gaps_insert_placeholders() {
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 2;
        // frame_num jumps 2 -> 5: placeholders for the skipped 3 and 4.
        d.insert_frame_num_gaps(5, 16, 8, 16, 16);
        let fns: Vec<u32> = d.refs.iter().map(|r| r.frame_num).collect();
        assert_eq!(fns, vec![4, 3], "most-recent placeholder at the front");
        assert_eq!(d.prev_ref_frame_num, 4);
        assert!(d.refs.iter().all(|r| r.y.iter().all(|&p| p == 128)), "grey fill");
    }

    #[test]
    fn frame_num_gaps_wrap_and_noop() {
        // Wrap across MaxFrameNum: prev 14, frame_num 1 (max 16) -> fill 15, 0.
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 14;
        d.insert_frame_num_gaps(1, 16, 8, 16, 16);
        assert_eq!(d.refs.iter().map(|r| r.frame_num).collect::<Vec<_>>(), vec![0, 15]);
        // No gap (consecutive) inserts nothing.
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 3;
        d.insert_frame_num_gaps(4, 16, 8, 16, 16);
        assert!(d.refs.is_empty());
    }

    #[test]
    fn missing_param_sets_errors() {
        let mut d = Decoder::new();
        // A lone (fake) IDR slice header: first_mb_in_slice=0, slice_type=7 (I),
        // pic_parameter_set_id=0 — then the PPS lookup fails (none stored).
        let nal = rusty_h264_common::NalUnit::new(3, NalUnitType::IdrSlice, vec![0x88, 0x80]);
        let err = d.decode(&nal.to_annex_b()).unwrap_err();
        assert_eq!(err, DecodeError::MissingParameterSet);
    }
}
