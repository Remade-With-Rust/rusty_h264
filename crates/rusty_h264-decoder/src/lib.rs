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
        if !is_p && !matches!(slice_type, 2 | 7) {
            return Err(DecodeError::Unsupported("only I and P slices"));
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
        let mut num_ref_idx_l0 = pps.num_ref_idx_l0_default as usize;
        let mut reorder_mods: Vec<(u32, u32)> = Vec::new();
        if is_p {
            // num_ref_idx_active_override_flag
            if r.read_bit()? {
                num_ref_idx_l0 = (r.read_ue()? + 1) as usize;
            }
            // ref_pic_list_modification_flag_l0
            if r.read_bit()? {
                loop {
                    let idc = r.read_ue()?;
                    if idc == 3 {
                        break;
                    }
                    if idc > 3 {
                        return Err(DecodeError::Unsupported("invalid ref_pic_list_modification"));
                    }
                    let val = r.read_ue()?; // abs_diff_pic_num_minus1 / long_term_pic_num
                    reorder_mods.push((idc, val));
                    if reorder_mods.len() > 64 {
                        return Err(DecodeError::Truncated); // runaway / corrupt
                    }
                }
            }
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

        // Build RefPicList0 for this slice (initial PicNum ordering + any
        // ref_pic_list_modification). Indexed by the macroblocks' ref_idx.
        let ref_list = build_ref_list_p(
            &self.refs,
            frame_num,
            1u32 << sps.log2_max_frame_num,
            num_ref_idx_l0,
            &reorder_mods,
        )?;

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
                ref_list,
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
            pic.fd.begin_slice(slice_qp, ref_list, num_ref_idx_l0);
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
            if idr_long_term {
                reference.long_term = true;
                reference.long_term_idx = 0;
            }
            self.apply_ref_marking(&mut reference, &mmco_ops, frame_num, log2_max_frame_num, max_refs);
        }
        Ok(Some(fd.into_frame(crop_r, crop_b)))
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

/// Builds the P-slice `RefPicList0`: short-term references ordered by descending
/// `FrameNumWrap` (spec §8.2.4.2.1), then any `ref_pic_list_modification`
/// reordering (§8.2.4.3.1, short-term only). `frame_num`-wrap is honoured so the
/// list is correct across the `MaxFrameNum` boundary.
fn build_ref_list_p(
    dpb: &[RefFrame],
    curr_frame_num: u32,
    max_frame_num: u32,
    num_active: usize,
    mods: &[(u32, u32)],
) -> Result<Vec<RefFrame>, DecodeError> {
    let curr = curr_frame_num as i64;
    let max = max_frame_num as i64;
    // FrameNumWrap = PicNum for a short-term frame reference.
    let pic_num = |fnum: u32| -> i64 {
        let f = fnum as i64;
        if f > curr {
            f - max
        } else {
            f
        }
    };
    // Initial list: short-term references by descending FrameNumWrap, then
    // long-term references by ascending LongTermFrameIdx (spec §8.2.4.2.1).
    let mut init: Vec<RefFrame> = dpb.iter().filter(|r| !r.long_term).cloned().collect();
    init.sort_by_key(|rf| core::cmp::Reverse(pic_num(rf.frame_num)));
    let mut long: Vec<RefFrame> = dpb.iter().filter(|r| r.long_term).cloned().collect();
    long.sort_by_key(|rf| rf.long_term_idx);
    init.extend(long);

    if mods.is_empty() {
        init.truncate(num_active.max(1));
        return Ok(init);
    }

    // Reordering: walk the commands, each placing the referenced picture at the
    // next list position and sliding the rest down (spec §8.2.4.3). idc 0/1
    // reference short-term pictures by PicNum; idc 2 references long-term ones by
    // LongTermFrameIdx.
    let mut list = init.clone();
    let mut pic_num_pred = curr;
    let mut refidx = 0usize;
    for &(idc, val) in mods {
        // `matches` identifies the target picture in both `init` and `list`.
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
        // Remove the later duplicate of the same picture (it slid down).
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
