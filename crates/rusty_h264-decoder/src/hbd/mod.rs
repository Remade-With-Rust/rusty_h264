//! High-bit-depth H.264 decoding: High 10 (and High, at 8 bits) progressive 4:2:0 streams,
//! reconstructed with 16-bit samples.
//!
//! This is a separate, straightforward pipeline next to the optimized 8-bit decoder rather
//! than a generalisation of it: the 8-bit path keeps its byte-identity gates untouched, and
//! this one can be read against the specification. It follows FFmpeg's `h264` decoder
//! closely (its neighbour caches, motion vector prediction, direct modes, reference list and
//! POC bookkeeping), so its output is bit-exact with FFmpeg's; the pixel arithmetic itself
//! (prediction, transforms, deblocking) is fully specified by ITU-T H.264, and is written
//! from the specification's equations.
//!
//! Scope: frame pictures (`frame_mbs_only_flag`), 4:2:0, bit depths 8 to 14, CAVLC and
//! CABAC, every I/P/B macroblock type, 8x8 transform, scaling matrices, explicit and
//! implicit weighted prediction, spatial and temporal direct, long-term references and
//! MMCO. Not supported: interlaced coding (PAFF/MBAFF), 4:0:0/4:2:2/4:4:4, lossless
//! (`qpprime_y_zero_transform_bypass`), SP/SI slices, slice groups.

mod cabac_mb;
mod cavlc_mb;
mod deblock;
mod mb;
mod recon;
mod tables;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use crate::params::{Pps, Sps};
use crate::DecodeError;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::BitReader;

pub(crate) use mb::Ctx;

/// A decoded picture with 16-bit samples, cropped, 4:2:0. Samples are in `0..1 << bit_depth`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvFrame16 {
    pub width: usize,
    pub height: usize,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    /// Picture order count (display order within a coded video sequence).
    pub poc: i32,
    /// `width * height` samples.
    pub y: Vec<u16>,
    /// `ceil(width / 2) * ceil(height / 2)` samples each.
    pub u: Vec<u16>,
    pub v: Vec<u16>,
}

/// A decoded picture kept for reference: planes at the coded size, and the motion data
/// B-pictures read from their co-located picture.
pub(crate) struct Picture {
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    pub motion_val: [Vec<[i16; 2]>; 2],
    pub ref_index: [Vec<i8>; 2],
    /// Per macroblock, in the context's padded `mb_type` layout.
    pub mb_type: Vec<u32>,
    pub poc: i32,
    pub field_poc: [i32; 2],
    pub frame_num: u32,
    /// `4 * frame_num + 3` of each reference the (last) slice of this picture used, as
    /// FFmpeg records it for temporal direct's co-located reference mapping.
    pub ref_count: [usize; 2],
    pub ref_poc: [[i32; 32]; 2],
    /// Identity for the deblocking filter's "same reference picture" comparisons.
    pub id: u32,
}

/// A reference list entry: the picture and whether it was a long-term reference when the
/// list was built.
#[derive(Clone)]
pub(crate) struct RefEntry {
    pub pic: Rc<Picture>,
    pub long: bool,
}

struct DpbEntry {
    pic: Rc<Picture>,
    frame_num: u32,
    /// `LongTermFrameIdx` when marked long-term.
    long_idx: Option<u32>,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct PredWeight {
    /// 0 none, 1 explicit, 2 implicit.
    pub use_weight: u8,
    pub use_weight_chroma: bool,
    pub luma_log2_denom: u32,
    pub chroma_log2_denom: u32,
    /// `[ref][list] -> (weight, offset)`.
    pub luma: [[(i32, i32); 2]; 32],
    pub chroma: [[[(i32, i32); 2]; 2]; 32],
    /// `[ref0][ref1]`: implicit bi-prediction weight of list 0.
    pub implicit: [[i32; 32]; 32],
}

#[derive(Default)]
pub(crate) struct PredWeightBox(pub alloc::boxed::Box<PredWeight>);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SliceType {
    P,
    B,
    I,
}

pub(crate) struct SliceHeader {
    pub first_mb: usize,
    pub slice_type: SliceType,
    pub frame_num: u32,
    pub idr: bool,
    pub nal_ref_idc: u8,
    pub poc_lsb: u32,
    pub delta_poc_bottom: i32,
    pub delta_poc: [i32; 2],
    pub redundant_pic_cnt: u32,
    pub direct_spatial: bool,
    pub ref_count: [usize; 2],
    pub modifications: [Vec<(u32, u32)>; 2],
    pub pwt: PredWeightBox,
    pub long_term_reference: bool,
    pub adaptive_marking: bool,
    pub mmco: Vec<(u32, u32, u32)>,
    pub cabac_init_idc: u32,
    pub qp: i32,
    /// `disable_deblocking_filter_idc`: 0 on, 1 off, 2 not across slice edges.
    pub deblock: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    /// Bit position of slice_data in the RBSP.
    pub data_bit: usize,
}

/// The picture being decoded.
struct CurPic {
    sps: Rc<Sps>,
    pps: Rc<Pps>,
    frame_num: u32,
    nal_ref_idc: u8,
    idr: bool,
    poc: i32,
    field_poc: [i32; 2],
    poc_lsb: i32,
    long_term_reference: bool,
    adaptive_marking: bool,
    mmco: Vec<(u32, u32, u32)>,
    slices: u16,
    ref_count: [usize; 2],
    ref_poc: [[i32; 32]; 2],
}

#[derive(Default)]
struct PocState {
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num_offset: i64,
    prev_frame_num: i64,
    poc_msb: i32,
    frame_num_offset: i64,
}

/// Decoder for H.264 streams with 16-bit reconstruction (see the module docs for scope).
/// Feed it one access unit at a time, like [`crate::Decoder::decode`]; pictures come back in
/// decoding order.
pub struct HbdDecoder {
    sps: BTreeMap<u32, Rc<Sps>>,
    pps: BTreeMap<u32, Rc<Pps>>,
    dpb: Vec<DpbEntry>,
    poc: PocState,
    next_id: u32,
    ctx: Option<Ctx>,
    cur: Option<CurPic>,
}

impl Default for HbdDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl HbdDecoder {
    pub fn new() -> Self {
        HbdDecoder {
            sps: BTreeMap::new(),
            pps: BTreeMap::new(),
            dpb: Vec::new(),
            poc: PocState {
                prev_frame_num: -1,
                ..PocState::default()
            },
            next_id: 1,
            ctx: None,
            cur: None,
        }
    }

    /// Decodes one Annex-B access unit and returns its picture, if it held one.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<YuvFrame16>, DecodeError> {
        let mut frame = None;
        for nal in split_annex_b(annex_b) {
            let Some(&header) = nal.first() else {
                continue;
            };
            let nal_ref_idc = (header >> 5) & 3;
            let nal_type = header & 0x1f;
            match nal_type {
                7 => {
                    let rbsp = emulation_unprevent(&nal[1..]);
                    let s = Sps::parse(&rbsp)?;
                    self.sps.insert(s.seq_parameter_set_id, Rc::new(s));
                }
                8 => {
                    let rbsp = emulation_unprevent(&nal[1..]);
                    let p = Pps::parse(&rbsp)?;
                    self.pps.insert(p.pic_parameter_set_id, Rc::new(p));
                }
                1 | 5 => {
                    let rbsp = emulation_unprevent(&nal[1..]);
                    match self.decode_slice(&rbsp, nal_type == 5, nal_ref_idc) {
                        Ok(Some(f)) => frame = Some(f),
                        Ok(None) => {}
                        Err(e) => {
                            // A damaged picture is dropped, never used as a reference.
                            self.cur = None;
                            return Err(e);
                        }
                    }
                }
                _ => {}
            }
        }
        // A multi-slice picture stays open until its last macroblock, so one slice
        // per call (what `split_access_units` yields) returns the picture from the
        // call that completes it. Finishing early would deblock a partial frame.
        if self.picture_complete() {
            frame = Some(self.finish_picture()?);
        }
        Ok(frame)
    }

    /// Every macroblock of the picture in flight has been decoded.
    fn picture_complete(&self) -> bool {
        match (&self.cur, &self.ctx) {
            (Some(_), Some(ctx)) if ctx.mb_w > 0 => ctx.next_mb >= ctx.mb_w * ctx.mb_h,
            _ => false,
        }
    }

    fn decode_slice(
        &mut self,
        rbsp: &[u8],
        idr: bool,
        nal_ref_idc: u8,
    ) -> Result<Option<YuvFrame16>, DecodeError> {
        let mut r = BitReader::new(rbsp);
        let first_mb = r.read_ue()? as usize;
        let slice_type = match r.read_ue()? % 5 {
            0 => SliceType::P,
            1 => SliceType::B,
            2 => SliceType::I,
            _ => return Err(DecodeError::Unsupported("SP/SI slices")),
        };
        let pps_id = r.read_ue()?;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(DecodeError::MissingParameterSet)?;
        let sps = self
            .sps
            .get(&pps.seq_parameter_set_id)
            .cloned()
            .ok_or(DecodeError::MissingParameterSet)?;
        if sps.transform_bypass {
            return Err(DecodeError::Unsupported("lossless (transform bypass)"));
        }
        let header =
            parse_slice_header(&mut r, &sps, &pps, first_mb, slice_type, idr, nal_ref_idc)?;
        if header.redundant_pic_cnt > 0 {
            return Ok(None); // FFmpeg drops redundant slices
        }

        // A slice at macroblock 0 begins a picture; any other continues the one in
        // flight. An earlier picture still open here was left short of its last
        // macroblock, which is a truncated access unit, not a picture to display.
        let mut finished = None;
        if first_mb == 0 && self.cur.is_some() {
            if !self.picture_complete() {
                self.cur = None;
                return Err(DecodeError::Truncated);
            }
            finished = Some(self.finish_picture()?);
        }
        if self.cur.is_none() {
            if first_mb != 0 {
                return Err(DecodeError::Unsupported(
                    "slice continues a missing picture",
                ));
            }
            self.start_picture(&sps, &pps, &header)?;
        }
        let Some(cur) = self.cur.as_mut() else {
            return Err(DecodeError::Internal("no current picture"));
        };
        cur.slices += 1;
        let slice_num = cur.slices;

        // Reference lists for this slice.
        let lists = if header.slice_type == SliceType::I {
            [Vec::new(), Vec::new()]
        } else {
            self.build_ref_lists(&header)?
        };
        let cur = self
            .cur
            .as_mut()
            .ok_or(DecodeError::Internal("no current picture"))?;
        for list in 0..2 {
            cur.ref_count[list] = lists[list].len();
            for (j, e) in lists[list].iter().enumerate().take(32) {
                cur.ref_poc[list][j] = 4 * e.pic.frame_num as i32 + 3;
            }
        }
        let (poc, field_poc) = (cur.poc, cur.field_poc);
        let (sps, pps) = (cur.sps.clone(), cur.pps.clone());
        // Slices of one picture are contiguous. A first_mb that skips or rewinds
        // would finish the picture with macroblocks still untouched and put that
        // picture in the DPB.
        let next_mb = self
            .ctx
            .as_ref()
            .ok_or(DecodeError::Internal("no context"))?
            .next_mb;
        if header.first_mb != next_mb {
            self.cur = None;
            return Err(DecodeError::Truncated);
        }
        let ctx = self
            .ctx
            .as_mut()
            .ok_or(DecodeError::Internal("no context"))?;
        ctx.decode_slice(rbsp, &sps, &pps, header, lists, slice_num, poc, field_poc)?;
        Ok(finished)
    }

    fn start_picture(
        &mut self,
        sps: &Rc<Sps>,
        pps: &Rc<Pps>,
        h: &SliceHeader,
    ) -> Result<(), DecodeError> {
        let bit_depth = sps.bit_depth_luma;
        if !(8..=14).contains(&bit_depth) || sps.bit_depth_chroma != bit_depth {
            return Err(DecodeError::Unsupported(
                "bit depth outside 8..=14 or luma != chroma",
            ));
        }
        if sps.max_num_ref_frames > 16 {
            // FFmpeg rejects the SPS; the limit also bounds the pictures kept alive.
            return Err(DecodeError::Unsupported("more than 16 reference frames"));
        }
        if h.idr {
            self.dpb.clear();
            self.poc = PocState {
                prev_frame_num: 0,
                ..PocState::default()
            };
        }
        // A new geometry or bit depth does not keep the old references. Motion
        // compensation indexes them with this picture's stride; a non-IDR size
        // change is non-conformant and predicts as a missing reference instead.
        if self.ctx.as_ref().is_some_and(|c| !c.fits(sps))
            || self
                .dpb
                .iter()
                .any(|e| !Self::picture_matches_sps(&e.pic, sps))
        {
            self.dpb.clear();
        }
        let max_frame_num = 1i64 << sps.log2_max_frame_num;

        // Missing frame_num values: FFmpeg fills the gap (shortened to the reference count)
        // with copies of the previous reference picture.
        if !h.idr
            && self.poc.prev_frame_num >= 0
            && i64::from(h.frame_num) != self.poc.prev_frame_num
        {
            let mut unwrap_prev = self.poc.prev_frame_num;
            if unwrap_prev > i64::from(h.frame_num) {
                unwrap_prev -= max_frame_num;
            }
            if i64::from(h.frame_num) - unwrap_prev > i64::from(sps.max_num_ref_frames) {
                unwrap_prev = i64::from(h.frame_num) - i64::from(sps.max_num_ref_frames) - 1;
                if unwrap_prev < 0 {
                    unwrap_prev += max_frame_num;
                }
                self.poc.prev_frame_num = unwrap_prev;
            }
            while i64::from(h.frame_num) != self.poc.prev_frame_num
                && i64::from(h.frame_num) != (self.poc.prev_frame_num + 1) % max_frame_num
            {
                let fnum = ((self.poc.prev_frame_num + 1) % max_frame_num) as u32;
                self.insert_gap_frame(sps, fnum)?;
                self.poc.prev_frame_num_offset = self.poc.frame_num_offset;
                self.poc.prev_frame_num = i64::from(fnum);
            }
        }

        // Picture order count (FFmpeg's ff_h264_init_poc).
        let frame_num = i64::from(h.frame_num);
        self.poc.frame_num_offset = self.poc.prev_frame_num_offset;
        if frame_num < self.poc.prev_frame_num {
            self.poc.frame_num_offset += max_frame_num;
        }
        let field_poc: [i64; 2] = match sps.pic_order_cnt_type {
            0 => {
                let max_lsb = 1i32 << sps.log2_max_pic_order_cnt_lsb;
                let lsb = h.poc_lsb as i32;
                let prev_lsb = self.poc.prev_poc_lsb;
                let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                    self.poc.prev_poc_msb + max_lsb
                } else if lsb > prev_lsb && prev_lsb - lsb < -max_lsb / 2 {
                    self.poc.prev_poc_msb - max_lsb
                } else {
                    self.poc.prev_poc_msb
                };
                self.poc.poc_msb = msb;
                let top = i64::from(msb) + i64::from(lsb);
                [top, top + i64::from(h.delta_poc_bottom)]
            }
            1 => {
                let cycle = &sps.offset_for_ref_frame;
                let mut abs_frame_num = if cycle.is_empty() {
                    0
                } else {
                    self.poc.frame_num_offset + frame_num
                };
                if h.nal_ref_idc == 0 && abs_frame_num > 0 {
                    abs_frame_num -= 1;
                }
                let per_cycle: i64 = cycle.iter().map(|&o| i64::from(o)).sum();
                let mut expected = 0i64;
                if abs_frame_num > 0 {
                    let n = cycle.len() as i64;
                    let cycle_cnt = (abs_frame_num - 1) / n;
                    let in_cycle = (abs_frame_num - 1) % n;
                    expected = cycle_cnt * per_cycle;
                    for &o in &cycle[..=in_cycle as usize] {
                        expected += i64::from(o);
                    }
                }
                if h.nal_ref_idc == 0 {
                    expected += i64::from(sps.offset_for_non_ref_pic);
                }
                let top = expected + i64::from(h.delta_poc[0]);
                [
                    top,
                    top + i64::from(sps.offset_for_top_to_bottom_field) + i64::from(h.delta_poc[1]),
                ]
            }
            _ => {
                let mut poc = 2 * (self.poc.frame_num_offset + frame_num);
                if h.nal_ref_idc == 0 {
                    poc -= 1;
                }
                [poc, poc]
            }
        };
        let field_poc = [
            i32::try_from(field_poc[0]).map_err(|_| DecodeError::Unsupported("invalid POC"))?,
            i32::try_from(field_poc[1]).map_err(|_| DecodeError::Unsupported("invalid POC"))?,
        ];
        let poc = field_poc[0].min(field_poc[1]);

        let fits = self.ctx.as_ref().is_some_and(|c| c.fits(sps));
        if !fits {
            self.ctx = Some(Ctx::new(sps)?);
        }
        let Some(ctx) = self.ctx.as_mut() else {
            return Err(DecodeError::Internal("no context"));
        };
        ctx.start_picture(sps);
        self.cur = Some(CurPic {
            sps: sps.clone(),
            pps: pps.clone(),
            frame_num: h.frame_num,
            nal_ref_idc: h.nal_ref_idc,
            idr: h.idr,
            poc,
            field_poc,
            poc_lsb: h.poc_lsb as i32,
            long_term_reference: h.long_term_reference,
            adaptive_marking: h.adaptive_marking,
            mmco: h.mmco.clone(),
            slices: 0,
            ref_count: [0; 2],
            ref_poc: [[0; 32]; 2],
        });
        Ok(())
    }

    /// Planes and motion tables of `pic` belong to `sps`'s macroblock grid.
    fn picture_matches_sps(pic: &Picture, sps: &Sps) -> bool {
        let mb_w = sps.pic_width_in_mbs;
        let mb_h = sps.pic_height_in_mbs;
        let mb_stride = mb_w + 1;
        let tables = mb_stride * (mb_h + 2) + 2;
        let b_stride = mb_w * 4;
        pic.y.len() == mb_w * 16 * mb_h * 16
            && pic.cb.len() == mb_w * 8 * mb_h * 8
            && pic.cr.len() == pic.cb.len()
            && pic.mb_type.len() == tables
            && pic.motion_val[0].len() == b_stride * mb_h * 4
            && pic.motion_val[1].len() == pic.motion_val[0].len()
            && pic.ref_index[0].len() == 4 * mb_stride * mb_h
            && pic.ref_index[1].len() == pic.ref_index[0].len()
    }

    /// A placeholder reference for a missing `frame_num`, as FFmpeg makes one: a copy of the
    /// latest short-term reference, marked by the sliding window.
    fn insert_gap_frame(&mut self, sps: &Rc<Sps>, frame_num: u32) -> Result<(), DecodeError> {
        let prev = self
            .dpb
            .iter()
            .filter(|e| e.long_idx.is_none())
            .max_by_key(|e| e.pic.id)
            .map(|e| e.pic.clone());
        let Some(prev) = prev else {
            return Err(DecodeError::Unsupported(
                "frame_num gap without a reference to copy",
            ));
        };
        let id = self.next_id;
        self.next_id += 1;
        let pic = Picture {
            y: prev.y.clone(),
            cb: prev.cb.clone(),
            cr: prev.cr.clone(),
            motion_val: [
                vec![[0; 2]; prev.motion_val[0].len()],
                vec![[0; 2]; prev.motion_val[1].len()],
            ],
            ref_index: [
                vec![-1; prev.ref_index[0].len()],
                vec![-1; prev.ref_index[1].len()],
            ],
            mb_type: vec![tables::MB_TYPE_INTRA4X4; prev.mb_type.len()],
            poc: prev.poc.wrapping_add(2),
            field_poc: [
                prev.field_poc[0].wrapping_add(2),
                prev.field_poc[1].wrapping_add(2),
            ],
            frame_num,
            ref_count: [0; 2],
            ref_poc: [[0; 32]; 2],
            id,
        };
        self.sliding_window(sps, frame_num);
        self.dpb.push(DpbEntry {
            pic: Rc::new(pic),
            frame_num,
            long_idx: None,
        });
        Ok(())
    }

    fn finish_picture(&mut self) -> Result<YuvFrame16, DecodeError> {
        let cur = self
            .cur
            .take()
            .ok_or(DecodeError::Internal("no current picture"))?;
        let ctx = self
            .ctx
            .as_mut()
            .ok_or(DecodeError::Internal("no context"))?;
        ctx.deblock_picture();
        let id = self.next_id;
        self.next_id += 1;
        // memory_management_control_operation 5 makes the picture frame_num 0 (FFmpeg keeps
        // its POC as decoded).
        let reset =
            cur.nal_ref_idc != 0 && cur.adaptive_marking && cur.mmco.iter().any(|m| m.0 == 5);
        let frame_num = if reset { 0 } else { cur.frame_num };
        let pic = Rc::new(ctx.take_picture(
            cur.poc,
            cur.field_poc,
            frame_num,
            cur.ref_count,
            cur.ref_poc,
            id,
        ));
        let frame = ctx.cropped(&pic, &cur.sps)?;
        let max_frame_num = 1i64 << cur.sps.log2_max_frame_num;

        if cur.nal_ref_idc != 0 {
            // Reference marking (FFmpeg's ff_h264_execute_ref_pic_marking).
            let mut current_long: Option<u32> = None;
            if cur.idr {
                self.dpb.clear();
                if cur.long_term_reference {
                    current_long = Some(0);
                }
            } else if cur.adaptive_marking {
                for &(op, a, b) in &cur.mmco {
                    match op {
                        1 | 3 => {
                            let pic_num = ((i64::from(cur.frame_num) - i64::from(a) - 1)
                                & (max_frame_num - 1))
                                as u32;
                            let pos = self
                                .dpb
                                .iter()
                                .position(|e| e.long_idx.is_none() && e.frame_num == pic_num);
                            let Some(pos) = pos else { continue };
                            if op == 1 {
                                self.dpb.remove(pos);
                            } else {
                                self.dpb.retain(|e| e.long_idx != Some(b));
                                if let Some(e) = self
                                    .dpb
                                    .iter_mut()
                                    .find(|e| e.long_idx.is_none() && e.frame_num == pic_num)
                                {
                                    e.long_idx = Some(b);
                                }
                            }
                        }
                        2 => self.dpb.retain(|e| e.long_idx != Some(a)),
                        4 => self.dpb.retain(|e| e.long_idx.is_none_or(|i| i < a)),
                        5 => self.dpb.clear(),
                        6 => {
                            self.dpb.retain(|e| e.long_idx != Some(a));
                            current_long = Some(a);
                        }
                        _ => {}
                    }
                }
            } else {
                self.sliding_window(&cur.sps, cur.frame_num);
            }
            // The current picture joins as short-term unless made long-term.
            self.dpb.retain(|e| {
                !(e.long_idx.is_none() && e.frame_num == frame_num && current_long.is_none())
            });
            self.dpb.push(DpbEntry {
                pic: pic.clone(),
                frame_num,
                long_idx: current_long,
            });
            let max_refs = cur.sps.max_num_ref_frames.max(1) as usize;
            if self.dpb.len() > max_refs {
                // Corrupt stream: FFmpeg drops the oldest short-term (or first long-term).
                let short = self
                    .dpb
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.long_idx.is_none() && !Rc::ptr_eq(&e.pic, &pic))
                    .min_by_key(|(_, e)| e.pic.id)
                    .map(|(i, _)| i);
                let victim = short.or_else(|| {
                    self.dpb
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.long_idx.is_some())
                        .min_by_key(|(_, e)| e.long_idx)
                        .map(|(i, _)| i)
                });
                if let Some(v) = victim {
                    self.dpb.remove(v);
                }
            }
            self.poc.prev_poc_msb = self.poc.poc_msb;
            self.poc.prev_poc_lsb = cur.poc_lsb;
        }
        self.poc.prev_frame_num_offset = self.poc.frame_num_offset;
        self.poc.prev_frame_num = i64::from(frame_num);
        Ok(frame)
    }

    /// Sliding window marking (8.2.5.3), before the current picture is added.
    fn sliding_window(&mut self, sps: &Sps, current_frame_num: u32) {
        let has_short = self.dpb.iter().any(|e| e.long_idx.is_none());
        if has_short && self.dpb.len() >= sps.max_num_ref_frames as usize {
            let max_frame_num = 1i64 << sps.log2_max_frame_num;
            let wrap = |f: u32| {
                let f = i64::from(f);
                if f > i64::from(current_frame_num) {
                    f - max_frame_num
                } else {
                    f
                }
            };
            if let Some(pos) = self
                .dpb
                .iter()
                .enumerate()
                .filter(|(_, e)| e.long_idx.is_none())
                .min_by_key(|(_, e)| wrap(e.frame_num))
                .map(|(i, _)| i)
            {
                self.dpb.remove(pos);
            }
        }
    }

    /// Initial reference lists (8.2.4.2) with the slice's modifications (8.2.4.3), as FFmpeg
    /// builds them (including its fallbacks for missing references).
    fn build_ref_lists(&self, h: &SliceHeader) -> Result<[Vec<RefEntry>; 2], DecodeError> {
        let cur = self
            .cur
            .as_ref()
            .ok_or(DecodeError::Internal("no current picture"))?;
        let max_frame_num = 1i64 << cur.sps.log2_max_frame_num;
        let cur_fn = i64::from(cur.frame_num);
        let wrap = |f: u32| {
            let f = i64::from(f);
            if f > cur_fn {
                f - max_frame_num
            } else {
                f
            }
        };
        let mut shorts: Vec<&DpbEntry> = self.dpb.iter().filter(|e| e.long_idx.is_none()).collect();
        let mut longs: Vec<&DpbEntry> = self.dpb.iter().filter(|e| e.long_idx.is_some()).collect();
        longs.sort_by_key(|e| e.long_idx);
        let entry = |e: &DpbEntry| RefEntry {
            pic: e.pic.clone(),
            long: e.long_idx.is_some(),
        };
        let list_count = if h.slice_type == SliceType::B { 2 } else { 1 };
        let mut init: [Vec<RefEntry>; 2] = [Vec::new(), Vec::new()];
        if h.slice_type == SliceType::P {
            shorts.sort_by_key(|e| core::cmp::Reverse(wrap(e.frame_num)));
            init[0] = shorts
                .iter()
                .map(|e| entry(e))
                .chain(longs.iter().map(|e| entry(e)))
                .collect();
        } else {
            let cur_poc = cur.poc;
            for (list, dst) in init.iter_mut().enumerate() {
                let mut before: Vec<&DpbEntry> = shorts
                    .iter()
                    .copied()
                    .filter(|e| e.pic.poc < cur_poc)
                    .collect();
                let mut after: Vec<&DpbEntry> = shorts
                    .iter()
                    .copied()
                    .filter(|e| e.pic.poc > cur_poc)
                    .collect();
                before.sort_by_key(|e| core::cmp::Reverse(e.pic.poc));
                after.sort_by_key(|e| e.pic.poc);
                let (first, second) = if list == 0 {
                    (before, after)
                } else {
                    (after, before)
                };
                *dst = first
                    .iter()
                    .chain(second.iter())
                    .map(|e| entry(e))
                    .chain(longs.iter().map(|e| entry(e)))
                    .collect();
            }
            if init[0].len() == init[1].len()
                && init[1].len() > 1
                && init[0]
                    .iter()
                    .zip(&init[1])
                    .all(|(a, b)| Rc::ptr_eq(&a.pic, &b.pic))
            {
                init[1].swap(0, 1);
            }
        }

        let mut lists: [Vec<Option<RefEntry>>; 2] = [Vec::new(), Vec::new()];
        for list in 0..list_count {
            let n = h.ref_count[list];
            let mut l: Vec<Option<RefEntry>> =
                init[list].iter().take(n).cloned().map(Some).collect();
            l.resize(n, None);
            let default_ref = init[list].first().cloned();

            let max_pic_num = max_frame_num;
            let mut pred = cur_fn;
            for (index, &(op, val)) in h.modifications[list].iter().enumerate() {
                if index >= n {
                    break;
                }
                let found: Option<(RefEntry, i64)> = match op {
                    0 | 1 => {
                        let abs_diff = i64::from(val) + 1;
                        if abs_diff > max_pic_num {
                            return Err(DecodeError::Unsupported("invalid abs_diff_pic_num"));
                        }
                        pred = if op == 0 {
                            pred - abs_diff
                        } else {
                            pred + abs_diff
                        };
                        pred &= max_pic_num - 1;
                        self.dpb
                            .iter()
                            .rev()
                            .find(|e| e.long_idx.is_none() && i64::from(e.frame_num) == pred)
                            .map(|e| (entry(e), pred))
                    }
                    2 => self
                        .dpb
                        .iter()
                        .find(|e| e.long_idx == Some(val))
                        .map(|e| (entry(e), i64::from(val))),
                    _ => {
                        return Err(DecodeError::Unsupported(
                            "invalid modification_of_pic_nums_idc",
                        ));
                    }
                };
                match found {
                    None => l[index] = None,
                    Some((e, pic_id)) => {
                        let id_of = |x: &RefEntry| {
                            if x.long {
                                self.dpb
                                    .iter()
                                    .find(|d| Rc::ptr_eq(&d.pic, &x.pic))
                                    .and_then(|d| d.long_idx)
                                    .map_or(-1, i64::from)
                            } else {
                                i64::from(x.pic.frame_num)
                            }
                        };
                        let mut i = index;
                        while i + 1 < n {
                            if l[i]
                                .as_ref()
                                .is_some_and(|x| x.long == e.long && id_of(x) == pic_id)
                            {
                                break;
                            }
                            i += 1;
                        }
                        while i > index {
                            l[i] = l[i - 1].clone();
                            i -= 1;
                        }
                        l[index] = Some(e);
                    }
                }
            }
            for slot in l.iter_mut() {
                if slot.is_none() {
                    *slot = default_ref.clone();
                }
            }
            lists[list] = l;
        }
        let mut out: [Vec<RefEntry>; 2] = [Vec::new(), Vec::new()];
        for list in 0..list_count {
            let mut v = Vec::with_capacity(lists[list].len());
            for e in lists[list].drain(..) {
                v.push(e.ok_or(DecodeError::Unsupported("missing reference picture"))?);
            }
            out[list] = v;
        }
        Ok(out)
    }
}

fn parse_slice_header(
    r: &mut BitReader,
    sps: &Sps,
    pps: &Pps,
    first_mb: usize,
    slice_type: SliceType,
    idr: bool,
    nal_ref_idc: u8,
) -> Result<SliceHeader, DecodeError> {
    let frame_num = r.read_bits(sps.log2_max_frame_num)?;
    // Frame pictures only: field_pic_flag is present when field coding is permitted.
    if !sps.frame_mbs_only && r.read_bit()? {
        return Err(DecodeError::Unsupported(
            "interlace / field coding (field picture)",
        ));
    }
    if idr {
        let _idr_pic_id = r.read_ue()?;
    }
    let mut poc_lsb = 0;
    let mut delta_poc_bottom = 0;
    let mut delta_poc = [0; 2];
    if sps.pic_order_cnt_type == 0 {
        poc_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb)?;
        if pps.bottom_field_pic_order_present {
            delta_poc_bottom = r.read_se()?;
        }
    } else if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero {
        delta_poc[0] = r.read_se()?;
        if pps.bottom_field_pic_order_present {
            delta_poc[1] = r.read_se()?;
        }
    }
    let redundant_pic_cnt = if pps.redundant_pic_cnt_present_flag {
        r.read_ue()?
    } else {
        0
    };
    let direct_spatial = slice_type == SliceType::B && r.read_bit()?;
    let mut ref_count = [0usize; 2];
    if slice_type != SliceType::I {
        ref_count = [
            pps.num_ref_idx_l0_default as usize,
            pps.num_ref_idx_l1_default as usize,
        ];
        if r.read_bit()? {
            ref_count[0] = r.read_ue()? as usize + 1;
            if slice_type == SliceType::B {
                ref_count[1] = r.read_ue()? as usize + 1;
            }
        }
        if slice_type != SliceType::B {
            ref_count[1] = 0;
        }
        if ref_count[0] > 16 || ref_count[1] > 16 {
            return Err(DecodeError::Unsupported("invalid num_ref_idx_active"));
        }
    }
    let mut modifications: [Vec<(u32, u32)>; 2] = [Vec::new(), Vec::new()];
    if slice_type != SliceType::I {
        let lists = if slice_type == SliceType::B { 2 } else { 1 };
        for list in modifications.iter_mut().take(lists) {
            if r.read_bit()? {
                loop {
                    let op = r.read_ue()?;
                    if op == 3 {
                        break;
                    }
                    if op > 2 || list.len() >= 32 {
                        return Err(DecodeError::Unsupported(
                            "invalid ref_pic_list_modification",
                        ));
                    }
                    let val = r.read_ue()?;
                    list.push((op, val));
                }
            }
        }
    }
    let mut pwt = PredWeightBox::default();
    if (pps.weighted_pred && slice_type == SliceType::P)
        || (pps.weighted_bipred_idc == 1 && slice_type == SliceType::B)
    {
        parse_pred_weight_table(r, &mut pwt.0, slice_type, ref_count)?;
    } else if pps.weighted_bipred_idc == 2 && slice_type == SliceType::B {
        pwt.0.use_weight = 2;
    }
    let mut long_term_reference = false;
    let mut adaptive_marking = false;
    let mut mmco = Vec::new();
    if nal_ref_idc != 0 {
        if idr {
            let _no_output_of_prior_pics = r.read_bit()?;
            long_term_reference = r.read_bit()?;
        } else {
            adaptive_marking = r.read_bit()?;
            if adaptive_marking {
                loop {
                    let op = r.read_ue()?;
                    if op == 0 {
                        break;
                    }
                    if op > 6 || mmco.len() >= 66 {
                        return Err(DecodeError::Unsupported("invalid MMCO"));
                    }
                    // (op, difference_of_pic_nums_minus1 or long-term argument, long-term idx)
                    let (a, b) = match op {
                        1 => (r.read_ue()?, 0),
                        3 => (r.read_ue()?, r.read_ue()?),
                        2 | 4 | 6 => (r.read_ue()?, 0),
                        _ => (0, 0),
                    };
                    let long_arg = if op == 3 { b } else { a };
                    let limit = if op == 4 { 16 } else { 15 };
                    if matches!(op, 2 | 3 | 4 | 6) && long_arg > limit {
                        return Err(DecodeError::Unsupported("invalid long-term index in MMCO"));
                    }
                    mmco.push((op, a, b));
                }
            }
        }
    }
    let cabac_init_idc = if pps.entropy_coding_mode_flag && slice_type != SliceType::I {
        let v = r.read_ue()?;
        if v > 2 {
            return Err(DecodeError::Unsupported("invalid cabac_init_idc"));
        }
        v
    } else {
        0
    };
    let qp = pps.pic_init_qp + r.read_se()?;
    let qp_bd_offset = 6 * (i32::from(sps.bit_depth_luma) - 8);
    if qp < -qp_bd_offset || qp > 51 {
        return Err(DecodeError::Unsupported("invalid slice QP"));
    }
    let (mut deblock, mut alpha_offset, mut beta_offset) = (0, 0, 0);
    if pps.deblocking_filter_control_present_flag {
        deblock = r.read_ue()?;
        if deblock > 2 {
            return Err(DecodeError::Unsupported(
                "invalid disable_deblocking_filter_idc",
            ));
        }
        if deblock != 1 {
            alpha_offset = r.read_se()? * 2;
            beta_offset = r.read_se()? * 2;
            if !(-12..=12).contains(&alpha_offset) || !(-12..=12).contains(&beta_offset) {
                return Err(DecodeError::Unsupported("invalid deblocking offsets"));
            }
        }
    }
    Ok(SliceHeader {
        first_mb,
        slice_type,
        frame_num,
        idr,
        nal_ref_idc,
        poc_lsb,
        delta_poc_bottom,
        delta_poc,
        redundant_pic_cnt,
        direct_spatial,
        ref_count,
        modifications,
        pwt,
        long_term_reference,
        adaptive_marking,
        mmco,
        cabac_init_idc,
        qp,
        deblock,
        alpha_offset,
        beta_offset,
        data_bit: r.bit_pos(),
    })
}

/// `pred_weight_table()` (7.3.3.2), with FFmpeg's defaults for absent entries.
fn parse_pred_weight_table(
    r: &mut BitReader,
    pwt: &mut PredWeight,
    slice_type: SliceType,
    ref_count: [usize; 2],
) -> Result<(), DecodeError> {
    pwt.use_weight = 0;
    pwt.use_weight_chroma = false;
    pwt.luma_log2_denom = r.read_ue()?;
    pwt.chroma_log2_denom = r.read_ue()?;
    if pwt.luma_log2_denom > 7 || pwt.chroma_log2_denom > 7 {
        return Err(DecodeError::Unsupported("invalid weight denominator"));
    }
    let luma_def = 1 << pwt.luma_log2_denom;
    let chroma_def = 1 << pwt.chroma_log2_denom;
    let lists = if slice_type == SliceType::B { 2 } else { 1 };
    for list in 0..lists {
        for i in 0..ref_count[list] {
            if r.read_bit()? {
                let w = r.read_se()?;
                let o = r.read_se()?;
                if !(-128..=127).contains(&w) || !(-128..=127).contains(&o) {
                    return Err(DecodeError::Unsupported("invalid luma weight"));
                }
                pwt.luma[i][list] = (w, o);
                if w != luma_def || o != 0 {
                    pwt.use_weight = 1;
                }
            } else {
                pwt.luma[i][list] = (luma_def, 0);
            }
            if r.read_bit()? {
                for c in 0..2 {
                    let w = r.read_se()?;
                    let o = r.read_se()?;
                    if !(-128..=127).contains(&w) || !(-128..=127).contains(&o) {
                        return Err(DecodeError::Unsupported("invalid chroma weight"));
                    }
                    pwt.chroma[i][list][c] = (w, o);
                    if w != chroma_def || o != 0 {
                        pwt.use_weight_chroma = true;
                        pwt.use_weight = 1;
                    }
                }
            } else {
                pwt.chroma[i][list] = [(chroma_def, 0); 2];
            }
        }
    }
    Ok(())
}
