//! Per-picture state and the macroblock layer shared by CABAC and CAVLC: FFmpeg's neighbour
//! caches (`fill_decode_neighbors`/`fill_decode_caches`), motion vector prediction, the
//! direct modes, and writing a macroblock's results back to the picture tables.

use alloc::vec;
use alloc::vec::Vec;

use super::tables::*;
use super::{Picture, PredWeight, RefEntry, SliceHeader, SliceType, YuvFrame16};
use crate::params::{Pps, Sps};
use crate::DecodeError;

/// Deblocking parameters of one slice (filtering reads them per macroblock).
#[derive(Clone, Copy, Default)]
pub(crate) struct SliceParams {
    /// 0 on, 1 off, 2 not across slice edges.
    pub deblock: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    pub chroma_qp_offset: [i32; 2],
    pub cabac: bool,
}

pub(crate) struct Ctx {
    // Geometry.
    pub mb_w: usize,
    pub mb_h: usize,
    /// First macroblock not yet decoded. A picture is complete at `mb_w * mb_h`.
    pub next_mb: usize,
    pub mb_stride: usize,
    pub b_stride: usize,
    pub bit_depth: u32,
    pub pixel_max: i32,
    pub qp_bd_offset: i32,
    pub max_qp: i32,
    // The picture being decoded, at the coded size.
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    pub ly: usize,
    pub lc: usize,
    // Per-macroblock tables, indexed through `t(mb_xy)` (padded so the row above and the
    // column left of the picture exist).
    pub mb_type: Vec<u32>,
    pub slice_table: Vec<u16>,
    pub qscale_table: Vec<u8>,
    pub cbp_table: Vec<u16>,
    pub chroma_pred_mode_table: Vec<u8>,
    pub list_counts: Vec<u8>,
    pub non_zero_count: Vec<[u8; 48]>,
    pub intra4x4_pred_mode: Vec<[i8; 8]>,
    pub mvd_table: [Vec<[[u8; 2]; 8]>; 2],
    pub direct_table: Vec<[u8; 4]>,
    /// Identity of each 8x8 block's reference picture, per list (-1: none), for deblocking.
    pub ref_ids: [Vec<[i32; 4]>; 2],
    // Motion of the picture: 4x4 blocks (`b_stride` wide) and 8x8 reference indices
    // (4 per macroblock at `4 * mb_xy`).
    pub motion_val: [Vec<[i16; 2]>; 2],
    pub ref_index: [Vec<i8>; 2],
    pub slice_params: Vec<SliceParams>,

    // Current slice.
    pub slice_num: u16,
    pub slice_type: SliceType,
    pub list_count: usize,
    pub ref_count: [usize; 2],
    pub ref_list: [Vec<RefEntry>; 2],
    pub qscale: i32,
    pub chroma_qp: [i32; 2],
    pub last_qscale_diff: i32,
    pub direct_spatial: bool,
    pub pwt: PredWeight,
    pub dist_scale_factor: [i32; 32],
    pub map_col_to_list0: [[i32; 32]; 2],
    pub chroma_qp_table: [Vec<u8>; 2],
    pub dequant4: [Vec<[i32; 16]>; 6],
    pub dequant8: [Vec<[i32; 64]>; 2],
    pub transform_8x8_mode: bool,
    pub constrained_intra_pred: bool,
    pub cur_poc: i32,
    pub direct_8x8_inference: bool,
    pub cabac_mode: bool,

    // Current macroblock and its neighbourhood.
    pub mb_x: usize,
    pub mb_y: usize,
    pub mb_xy: isize,
    pub top_mb_xy: isize,
    pub topleft_mb_xy: isize,
    pub topright_mb_xy: isize,
    pub left_mb_xy: isize,
    pub top_type: u32,
    pub topleft_type: u32,
    pub topright_type: u32,
    pub left_type: u32,
    pub topleft_samples_available: u32,
    pub top_samples_available: u32,
    pub topright_samples_available: u32,
    pub left_samples_available: u32,
    pub intra4x4_pred_mode_cache: [i8; 40],
    pub non_zero_count_cache: [u8; 120],
    pub mv_cache: [[[i16; 2]; 40]; 2],
    pub ref_cache: [[i8; 40]; 2],
    pub mvd_cache: [[[u8; 2]; 40]; 2],
    pub direct_cache: [u8; 40],
    pub sub_mb_type: [u32; 4],
    pub neighbor_transform_size: usize,
    pub top_cbp: u32,
    pub left_cbp: u32,
    pub cbp: u32,
    pub intra16x16_pred_mode: i32,
    pub chroma_pred_mode: i32,
    pub prev_mb_skipped: bool,
    /// Coefficients: 4x4 block `n` at `16 * n` (raster), an 8x8 block at `64 * i8x8`.
    pub mb: [i32; 16 * 48],
    pub mb_luma_dc: [i32; 16],
    pub chroma_dc: [[i32; 4]; 2],
    /// An I_PCM macroblock's samples (luma 256, then Cb 64, Cr 64).
    pub pcm: [u16; 384],
    /// Motion compensation scratch: each list's luma and chroma prediction of a partition.
    pub mc_luma: [[i32; 256]; 2],
    pub mc_chroma: [[[i32; 64]; 2]; 2],
}

impl Ctx {
    pub fn new(sps: &Sps) -> Result<Ctx, DecodeError> {
        let mb_w = sps.pic_width_in_mbs;
        let mb_h = sps.pic_height_in_mbs;
        let mb_stride = mb_w + 1;
        let b_stride = mb_w * 4;
        let bit_depth = u32::from(sps.bit_depth_luma);
        let tables = mb_stride * (mb_h + 2) + 2;
        let qp_bd_offset = 6 * (bit_depth as i32 - 8);
        Ok(Ctx {
            mb_w,
            mb_h,
            next_mb: 0,
            mb_stride,
            b_stride,
            bit_depth,
            pixel_max: (1 << bit_depth) - 1,
            qp_bd_offset,
            max_qp: 51 + qp_bd_offset,
            y: vec![0; mb_w * 16 * mb_h * 16],
            cb: vec![0; mb_w * 8 * mb_h * 8],
            cr: vec![0; mb_w * 8 * mb_h * 8],
            ly: mb_w * 16,
            lc: mb_w * 8,
            mb_type: vec![0; tables],
            slice_table: vec![0xFFFF; tables],
            qscale_table: vec![0; tables],
            cbp_table: vec![0; tables],
            chroma_pred_mode_table: vec![0; tables],
            list_counts: vec![0; tables],
            non_zero_count: vec![[0; 48]; tables],
            intra4x4_pred_mode: vec![[0; 8]; tables],
            mvd_table: [vec![[[0; 2]; 8]; tables], vec![[[0; 2]; 8]; tables]],
            direct_table: vec![[0; 4]; tables],
            ref_ids: [vec![[-1; 4]; tables], vec![[-1; 4]; tables]],
            motion_val: [
                vec![[0; 2]; b_stride * mb_h * 4],
                vec![[0; 2]; b_stride * mb_h * 4],
            ],
            ref_index: [
                vec![-1; 4 * mb_stride * mb_h],
                vec![-1; 4 * mb_stride * mb_h],
            ],
            slice_params: Vec::new(),
            slice_num: 0,
            slice_type: SliceType::I,
            list_count: 0,
            ref_count: [0; 2],
            ref_list: [Vec::new(), Vec::new()],
            qscale: 0,
            chroma_qp: [0; 2],
            last_qscale_diff: 0,
            direct_spatial: false,
            pwt: PredWeight::default(),
            dist_scale_factor: [0; 32],
            map_col_to_list0: [[0; 32]; 2],
            chroma_qp_table: [Vec::new(), Vec::new()],
            dequant4: Default::default(),
            dequant8: Default::default(),
            transform_8x8_mode: false,
            constrained_intra_pred: false,
            cur_poc: 0,
            direct_8x8_inference: sps.direct_8x8_inference,
            cabac_mode: false,
            mb_x: 0,
            mb_y: 0,
            mb_xy: 0,
            top_mb_xy: 0,
            topleft_mb_xy: 0,
            topright_mb_xy: 0,
            left_mb_xy: 0,
            top_type: 0,
            topleft_type: 0,
            topright_type: 0,
            left_type: 0,
            topleft_samples_available: 0,
            top_samples_available: 0,
            topright_samples_available: 0,
            left_samples_available: 0,
            intra4x4_pred_mode_cache: [0; 40],
            non_zero_count_cache: [0; 120],
            mv_cache: [[[0; 2]; 40]; 2],
            // Right of the macroblock below its first row: never available (FFmpeg's
            // ff_h264_slice_context_init), for the top-right lookups that land there.
            ref_cache: {
                let mut c = [[0; 40]; 2];
                for list in &mut c {
                    list[16] = PART_NOT_AVAILABLE;
                    list[24] = PART_NOT_AVAILABLE;
                    list[32] = PART_NOT_AVAILABLE;
                }
                c
            },
            mvd_cache: [[[0; 2]; 40]; 2],
            direct_cache: [0; 40],
            sub_mb_type: [0; 4],
            neighbor_transform_size: 0,
            top_cbp: 0,
            left_cbp: 0,
            cbp: 0,
            intra16x16_pred_mode: 0,
            chroma_pred_mode: 0,
            prev_mb_skipped: false,
            mb: [0; 16 * 48],
            mb_luma_dc: [0; 16],
            chroma_dc: [[0; 4]; 2],
            pcm: [0; 384],
            mc_luma: [[0; 256]; 2],
            mc_chroma: [[[0; 64]; 2]; 2],
        })
    }

    pub fn fits(&self, sps: &Sps) -> bool {
        self.mb_w == sps.pic_width_in_mbs
            && self.mb_h == sps.pic_height_in_mbs
            && self.bit_depth == u32::from(sps.bit_depth_luma)
    }

    /// True when `pic` was stored for this context's geometry, so its planes and
    /// motion tables can be indexed with this picture's strides.
    pub fn reference_matches(&self, pic: &Picture) -> bool {
        let mb_stride = self.mb_w + 1;
        let tables = mb_stride * (self.mb_h + 2) + 2;
        pic.y.len() == self.ly * self.mb_h * 16
            && pic.cb.len() == self.lc * self.mb_h * 8
            && pic.cr.len() == pic.cb.len()
            && pic.mb_type.len() == tables
            && pic.motion_val[0].len() == self.b_stride * self.mb_h * 4
            && pic.motion_val[1].len() == pic.motion_val[0].len()
            && pic.ref_index[0].len() == 4 * mb_stride * self.mb_h
            && pic.ref_index[1].len() == pic.ref_index[0].len()
    }

    /// Index of `mb_xy` in the padded per-macroblock tables.
    #[inline]
    pub fn t(&self, mb_xy: isize) -> usize {
        (mb_xy + self.mb_stride as isize + 1) as usize
    }

    pub fn start_picture(&mut self, sps: &Sps) {
        self.next_mb = 0;
        self.mb_type.fill(0);
        self.slice_table.fill(0xFFFF);
        self.slice_params.clear();
        self.slice_params.push(SliceParams::default());
        self.direct_8x8_inference = sps.direct_8x8_inference;
        for list in 0..2 {
            self.motion_val[list].fill([0; 2]);
            self.ref_index[list].fill(-1);
            self.ref_ids[list].fill([-1; 4]);
        }
    }

    /// Moves the decoded picture out for reference, leaving fresh planes behind.
    pub fn take_picture(
        &mut self,
        poc: i32,
        field_poc: [i32; 2],
        frame_num: u32,
        ref_count: [usize; 2],
        ref_poc: [[i32; 32]; 2],
        id: u32,
    ) -> Picture {
        let (yl, cl) = (self.y.len(), self.cb.len());
        Picture {
            y: core::mem::replace(&mut self.y, vec![0; yl]),
            cb: core::mem::replace(&mut self.cb, vec![0; cl]),
            cr: core::mem::replace(&mut self.cr, vec![0; cl]),
            motion_val: [self.motion_val[0].clone(), self.motion_val[1].clone()],
            ref_index: [self.ref_index[0].clone(), self.ref_index[1].clone()],
            mb_type: self.mb_type.clone(),
            poc,
            field_poc,
            frame_num,
            ref_count,
            ref_poc,
            id,
        }
    }

    pub fn cropped(&self, pic: &Picture, sps: &Sps) -> Result<YuvFrame16, DecodeError> {
        let Some((x0, y0, w, h)) = sps.crop_window() else {
            return Err(DecodeError::Unsupported("crop exceeds frame"));
        };
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let copy = |plane: &[u16],
                    stride: usize,
                    x0: usize,
                    y0: usize,
                    w: usize,
                    h: usize|
         -> Result<Vec<u16>, DecodeError> {
            let mut out = Vec::new();
            for r in 0..h {
                let start = (y0 + r)
                    .checked_mul(stride)
                    .and_then(|s| s.checked_add(x0))
                    .ok_or(DecodeError::Internal("crop outside picture"))?;
                let end = start
                    .checked_add(w)
                    .ok_or(DecodeError::Internal("crop outside picture"))?;
                let row = plane
                    .get(start..end)
                    .ok_or(DecodeError::Internal("crop outside picture"))?;
                out.extend_from_slice(row);
            }
            Ok(out)
        };
        Ok(YuvFrame16 {
            width: w,
            height: h,
            bit_depth_luma: self.bit_depth as u8,
            bit_depth_chroma: self.bit_depth as u8,
            poc: pic.poc,
            y: copy(&pic.y, self.ly, x0, y0, w, h)?,
            u: copy(&pic.cb, self.lc, x0 / 2, y0 / 2, cw, ch)?,
            v: copy(&pic.cr, self.lc, x0 / 2, y0 / 2, cw, ch)?,
        })
    }

    /// Per-PPS tables: chroma QP mapping and dequantisation factors (FFmpeg's
    /// `init_dequant_tables`, in raster order).
    fn init_pps_tables(&mut self, sps: &Sps, pps: &Pps) {
        let max_qp = self.max_qp;
        let off = self.qp_bd_offset;
        for (t, offset) in [
            pps.chroma_qp_index_offset,
            pps.second_chroma_qp_index_offset,
        ]
        .into_iter()
        .enumerate()
        {
            self.chroma_qp_table[t] = (0..=max_qp)
                .map(|q| {
                    let x = (q + offset).clamp(0, max_qp);
                    if x < off {
                        x as u8
                    } else {
                        CHROMA_QP[(x - off) as usize] + off as u8
                    }
                })
                .collect();
        }
        let (scale4, scale8) = crate::resolve_scaling(sps, pps);
        for i in 0..6 {
            self.dequant4[i] = (0..=max_qp as usize)
                .map(|q| {
                    let shift = q / 6 + 2;
                    core::array::from_fn(|pos| (norm_adjust4(q % 6, pos) * scale4[i][pos]) << shift)
                })
                .collect();
        }
        for i in 0..2 {
            self.dequant8[i] = (0..=max_qp as usize)
                .map(|q| {
                    let shift = q / 6;
                    core::array::from_fn(|pos| (norm_adjust8(q % 6, pos) * scale8[i][pos]) << shift)
                })
                .collect();
        }
        self.transform_8x8_mode = pps.transform_8x8_mode_flag;
        self.constrained_intra_pred = pps.constrained_intra_pred_flag;
    }

    pub fn chroma_qp_of(&self, t: usize, qscale: i32) -> i32 {
        i32::from(self.chroma_qp_table[t][qscale as usize])
    }

    pub fn set_qscale(&mut self, q: i32) {
        self.qscale = q;
        self.chroma_qp = [self.chroma_qp_of(0, q), self.chroma_qp_of(1, q)];
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decode_slice(
        &mut self,
        rbsp: &[u8],
        sps: &Sps,
        pps: &Pps,
        h: SliceHeader,
        lists: [Vec<RefEntry>; 2],
        slice_num: u16,
        poc: i32,
        field_poc: [i32; 2],
    ) -> Result<(), DecodeError> {
        let _ = field_poc;
        self.init_pps_tables(sps, pps);
        self.slice_num = slice_num;
        self.slice_type = h.slice_type;
        self.list_count = match h.slice_type {
            SliceType::I => 0,
            SliceType::P => 1,
            SliceType::B => 2,
        };
        self.ref_count = [lists[0].len(), lists[1].len()];
        self.ref_list = lists;
        self.cur_poc = poc;
        self.direct_spatial = h.direct_spatial;
        self.cabac_mode = pps.entropy_coding_mode_flag;
        self.pwt = *h.pwt.0;
        self.last_qscale_diff = 0;
        self.set_qscale(h.qp + self.qp_bd_offset);
        self.prev_mb_skipped = false;
        if self.slice_params.len() != usize::from(slice_num) {
            self.slice_params
                .resize(usize::from(slice_num), SliceParams::default());
        }
        self.slice_params.push(SliceParams {
            deblock: h.deblock,
            alpha_offset: h.alpha_offset,
            beta_offset: h.beta_offset,
            chroma_qp_offset: [
                pps.chroma_qp_index_offset,
                pps.second_chroma_qp_index_offset,
            ],
            cabac: pps.entropy_coding_mode_flag,
        });
        if h.slice_type == SliceType::B {
            if self.pwt.use_weight == 2 {
                self.implicit_weight_table();
            }
            self.direct_ref_list_init();
        }

        if h.first_mb >= self.mb_w * self.mb_h {
            return Err(DecodeError::Unsupported("invalid first_mb_in_slice"));
        }
        if pps.entropy_coding_mode_flag {
            self.decode_slice_cabac(rbsp, &h)
        } else {
            self.decode_slice_cavlc(rbsp, &h)
        }
    }

    /// FFmpeg's `implicit_weight_table` for frames.
    fn implicit_weight_table(&mut self) {
        let cur_poc = i64::from(self.cur_poc);
        if self.ref_count == [1, 1]
            && i64::from(self.ref_list[0][0].pic.poc) + i64::from(self.ref_list[1][0].pic.poc)
                == 2 * cur_poc
        {
            self.pwt.use_weight = 0;
            self.pwt.use_weight_chroma = false;
            return;
        }
        self.pwt.use_weight = 2;
        self.pwt.use_weight_chroma = true;
        self.pwt.luma_log2_denom = 5;
        self.pwt.chroma_log2_denom = 5;
        for r0 in 0..self.ref_count[0] {
            let e0 = &self.ref_list[0][r0];
            let poc0 = i64::from(e0.pic.poc);
            for r1 in 0..self.ref_count[1] {
                let e1 = &self.ref_list[1][r1];
                let mut w = 32;
                if !e0.long && !e1.long {
                    let poc1 = i64::from(e1.pic.poc);
                    let td = (poc1 - poc0).clamp(-128, 127) as i32;
                    if td != 0 {
                        let tb = (cur_poc - poc0).clamp(-128, 127) as i32;
                        let tx = (16384 + (td.abs() >> 1)) / td;
                        let dsf = (tb * tx + 32) >> 8;
                        if (-64..=128).contains(&dsf) {
                            w = 64 - dsf;
                        }
                    }
                }
                self.pwt.implicit[r0][r1] = w;
            }
        }
    }

    /// `ff_h264_direct_ref_list_init` + `ff_h264_direct_dist_scale_factor` for frames.
    fn direct_ref_list_init(&mut self) {
        if self.ref_count[1] == 0 {
            return;
        }
        let col = self.ref_list[1][0].pic.clone();
        let poc1 = i64::from(col.poc);
        for i in 0..self.ref_count[0] {
            let e = &self.ref_list[0][i];
            let poc0 = i64::from(e.pic.poc);
            let td = (poc1 - poc0).clamp(-128, 127) as i32;
            self.dist_scale_factor[i] = if td == 0 || e.long {
                256
            } else {
                let tb = (i64::from(self.cur_poc) - poc0).clamp(-128, 127) as i32;
                let tx = (16384 + (td.abs() >> 1)) / td;
                ((tb * tx + 32) >> 6).clamp(-1024, 1023)
            };
        }
        if self.direct_spatial {
            return;
        }
        for list in 0..2 {
            self.map_col_to_list0[list] = [0; 32];
            for old_ref in 0..col.ref_count[list].min(32) {
                let poc = col.ref_poc[list][old_ref] | 3;
                for j in 0..self.ref_count[0] {
                    if 4 * self.ref_list[0][j].pic.frame_num as i32 + 3 == poc {
                        self.map_col_to_list0[list][old_ref] = j as i32;
                        break;
                    }
                }
            }
        }
    }

    // --- Neighbours and caches ---------------------------------------------------------

    pub fn fill_decode_neighbors(&mut self) {
        let mb_xy = self.mb_xy;
        let top_xy = mb_xy - self.mb_stride as isize;
        self.topleft_mb_xy = top_xy - 1;
        self.top_mb_xy = top_xy;
        self.topright_mb_xy = top_xy + 1;
        self.left_mb_xy = mb_xy - 1;
        let ty = |c: &Ctx, xy: isize| c.mb_type[c.t(xy)];
        self.topleft_type = ty(self, self.topleft_mb_xy);
        self.top_type = ty(self, top_xy);
        self.topright_type = ty(self, self.topright_mb_xy);
        self.left_type = ty(self, self.left_mb_xy);
        let st = |c: &Ctx, xy: isize| c.slice_table[c.t(xy)];
        let n = self.slice_num;
        if st(self, self.topleft_mb_xy) != n {
            self.topleft_type = 0;
            if st(self, top_xy) != n {
                self.top_type = 0;
            }
            if st(self, self.left_mb_xy) != n {
                self.left_type = 0;
            }
        }
        if st(self, self.topright_mb_xy) != n {
            self.topright_type = 0;
        }
    }

    pub fn fill_decode_caches(&mut self, mb_type: u32) {
        let cabac = self.cabac_mode;
        let (top_xy, left_xy) = (self.top_mb_xy, self.left_mb_xy);
        let (top_type, left_type, topleft_type, topright_type) = (
            self.top_type,
            self.left_type,
            self.topleft_type,
            self.topright_type,
        );

        if !is_skip(mb_type) {
            if is_intra(mb_type) {
                let type_mask = |t: u32| {
                    if self.constrained_intra_pred {
                        is_intra(t)
                    } else {
                        t != 0
                    }
                };
                self.topleft_samples_available = 0xFFFF;
                self.top_samples_available = 0xFFFF;
                self.left_samples_available = 0xFFFF;
                self.topright_samples_available = 0xEEEA;
                if !type_mask(top_type) {
                    self.topleft_samples_available = 0xB3FF;
                    self.top_samples_available = 0x33FF;
                    self.topright_samples_available = 0x26EA;
                }
                if !type_mask(left_type) {
                    self.topleft_samples_available &= 0xDF5F;
                    self.left_samples_available &= 0x5F5F;
                }
                if !type_mask(topleft_type) {
                    self.topleft_samples_available &= 0x7FFF;
                }
                if !type_mask(topright_type) {
                    self.topright_samples_available &= 0xFBFF;
                }
                if is_intra4x4(mb_type) {
                    if is_intra4x4(top_type) {
                        let m = self.intra4x4_pred_mode[self.t(top_xy)];
                        self.intra4x4_pred_mode_cache[4..8].copy_from_slice(&m[0..4]);
                    } else {
                        let v = if type_mask(top_type) { 2 } else { -1 };
                        self.intra4x4_pred_mode_cache[4..8].fill(v);
                    }
                    for i in 0..2 {
                        if is_intra4x4(left_type) {
                            let m = self.intra4x4_pred_mode[self.t(left_xy)];
                            self.intra4x4_pred_mode_cache[3 + 8 + 16 * i] =
                                m[6 - LEFT_BLOCK[2 * i]];
                            self.intra4x4_pred_mode_cache[3 + 16 + 16 * i] =
                                m[6 - LEFT_BLOCK[1 + 2 * i]];
                        } else {
                            let v = if type_mask(left_type) { 2 } else { -1 };
                            self.intra4x4_pred_mode_cache[3 + 8 + 16 * i] = v;
                            self.intra4x4_pred_mode_cache[3 + 16 + 16 * i] = v;
                        }
                    }
                }
            }

            // Non-zero counts of the neighbours (64: not available).
            let nnz_cache = &mut self.non_zero_count_cache;
            if top_type != 0 {
                let nnz = &self.non_zero_count[(top_xy + self.mb_stride as isize + 1) as usize];
                nnz_cache[4..8].copy_from_slice(&nnz[12..16]);
                nnz_cache[4 + 8 * 5..8 + 8 * 5].copy_from_slice(&nnz[20..24]);
                nnz_cache[4 + 8 * 10..8 + 8 * 10].copy_from_slice(&nnz[36..40]);
            } else {
                let v = if cabac && !is_intra(mb_type) { 0 } else { 64 };
                nnz_cache[4..8].fill(v);
                nnz_cache[4 + 8 * 5..8 + 8 * 5].fill(v);
                nnz_cache[4 + 8 * 10..8 + 8 * 10].fill(v);
            }
            for i in 0..2 {
                if left_type != 0 {
                    let nnz =
                        &self.non_zero_count[(left_xy + self.mb_stride as isize + 1) as usize];
                    nnz_cache[3 + 8 + 16 * i] = nnz[LEFT_BLOCK[8 + 2 * i]];
                    nnz_cache[3 + 16 + 16 * i] = nnz[LEFT_BLOCK[8 + 1 + 2 * i]];
                    nnz_cache[3 + 8 * 6 + 8 * i] = nnz[LEFT_BLOCK[8 + 4 + 2 * i]];
                    nnz_cache[3 + 8 * 11 + 8 * i] = nnz[LEFT_BLOCK[8 + 5 + 2 * i]];
                } else {
                    let v = if cabac && !is_intra(mb_type) { 0 } else { 64 };
                    for k in [
                        3 + 8 + 16 * i,
                        3 + 16 + 16 * i,
                        3 + 8 * 6 + 16 * i,
                        3 + 8 * 7 + 16 * i,
                        3 + 8 * 11 + 16 * i,
                        3 + 8 * 12 + 16 * i,
                    ] {
                        nnz_cache[k] = v;
                    }
                }
            }

            if cabac {
                self.top_cbp = if top_type != 0 {
                    u32::from(self.cbp_table[self.t(top_xy)])
                } else if is_intra(mb_type) {
                    0x7CF
                } else {
                    0x00F
                };
                self.left_cbp = if left_type != 0 {
                    let c = u32::from(self.cbp_table[self.t(left_xy)]);
                    (c & 0x7F0)
                        | ((c >> (LEFT_BLOCK[0] & !1)) & 2)
                        | (((c >> (LEFT_BLOCK[2] & !1)) & 2) << 2)
                } else if is_intra(mb_type) {
                    0x7CF
                } else {
                    0x00F
                };
            }
        }

        if is_inter(mb_type) || (is_direct(mb_type) && self.direct_spatial) {
            let b_stride = self.b_stride as isize;
            let s0 = SCAN8[0];
            for list in 0..self.list_count {
                if !uses_list(mb_type, list) {
                    continue;
                }
                // Top row.
                if uses_list(top_type, list) {
                    let b_xy = self.mb2b(top_xy) + 3 * b_stride;
                    for k in 0..4 {
                        self.mv_cache[list][s0 - 8 + k] =
                            self.motion_val[list][(b_xy + k as isize) as usize];
                    }
                    let r = self.ref_idx_at(list, top_xy);
                    self.ref_cache[list][s0 - 8] = r[2];
                    self.ref_cache[list][s0 - 7] = r[2];
                    self.ref_cache[list][s0 - 6] = r[3];
                    self.ref_cache[list][s0 - 5] = r[3];
                } else {
                    for k in 0..4 {
                        self.mv_cache[list][s0 - 8 + k] = [0; 2];
                    }
                    let v = if top_type != 0 {
                        LIST_NOT_USED
                    } else {
                        PART_NOT_AVAILABLE
                    };
                    self.ref_cache[list][s0 - 8..s0 - 4].fill(v);
                }
                // Left column.
                if mb_type & (MB_TYPE_16X8 | MB_TYPE_8X8) != 0 {
                    for i in 0..2 {
                        let cache_idx = s0 - 1 + 16 * i;
                        if uses_list(left_type, list) {
                            let b_xy = self.mb2b(left_xy) + 3;
                            let r = self.ref_idx_at(list, left_xy);
                            self.mv_cache[list][cache_idx] = self.motion_val[list]
                                [(b_xy + b_stride * LEFT_BLOCK[2 * i] as isize) as usize];
                            self.mv_cache[list][cache_idx + 8] = self.motion_val[list]
                                [(b_xy + b_stride * LEFT_BLOCK[1 + 2 * i] as isize) as usize];
                            self.ref_cache[list][cache_idx] = r[1 + (LEFT_BLOCK[2 * i] & !1)];
                            self.ref_cache[list][cache_idx + 8] =
                                r[1 + (LEFT_BLOCK[1 + 2 * i] & !1)];
                        } else {
                            self.mv_cache[list][cache_idx] = [0; 2];
                            self.mv_cache[list][cache_idx + 8] = [0; 2];
                            let v = if left_type != 0 {
                                LIST_NOT_USED
                            } else {
                                PART_NOT_AVAILABLE
                            };
                            self.ref_cache[list][cache_idx] = v;
                            self.ref_cache[list][cache_idx + 8] = v;
                        }
                    }
                } else if uses_list(left_type, list) {
                    let b_xy = self.mb2b(left_xy) + 3;
                    let r = self.ref_idx_at(list, left_xy);
                    self.mv_cache[list][s0 - 1] =
                        self.motion_val[list][(b_xy + b_stride * LEFT_BLOCK[0] as isize) as usize];
                    self.ref_cache[list][s0 - 1] = r[1 + (LEFT_BLOCK[0] & !1)];
                } else {
                    self.mv_cache[list][s0 - 1] = [0; 2];
                    self.ref_cache[list][s0 - 1] = if left_type != 0 {
                        LIST_NOT_USED
                    } else {
                        PART_NOT_AVAILABLE
                    };
                }
                // Top right, and top left when top right is missing.
                if uses_list(topright_type, list) {
                    let b_xy = self.mb2b(self.topright_mb_xy) + 3 * b_stride;
                    self.mv_cache[list][s0 + 4 - 8] = self.motion_val[list][b_xy as usize];
                    self.ref_cache[list][s0 + 4 - 8] =
                        self.ref_idx_at(list, self.topright_mb_xy)[2];
                } else {
                    self.mv_cache[list][s0 + 4 - 8] = [0; 2];
                    self.ref_cache[list][s0 + 4 - 8] = if topright_type != 0 {
                        LIST_NOT_USED
                    } else {
                        PART_NOT_AVAILABLE
                    };
                }
                if self.ref_cache[list][s0 + 2 - 8] < 0 || self.ref_cache[list][s0 + 4 - 8] < 0 {
                    if uses_list(topleft_type, list) {
                        // Bottom-right block of the top-left macroblock.
                        let b_xy = self.mb2b(self.topleft_mb_xy) + 3 + 3 * b_stride;
                        self.mv_cache[list][s0 - 1 - 8] = self.motion_val[list][b_xy as usize];
                        self.ref_cache[list][s0 - 1 - 8] =
                            self.ref_idx_at(list, self.topleft_mb_xy)[3];
                    } else {
                        self.mv_cache[list][s0 - 1 - 8] = [0; 2];
                        self.ref_cache[list][s0 - 1 - 8] = if topleft_type != 0 {
                            LIST_NOT_USED
                        } else {
                            PART_NOT_AVAILABLE
                        };
                    }
                }

                if mb_type & (MB_TYPE_SKIP | MB_TYPE_DIRECT2) != 0 {
                    continue;
                }
                self.ref_cache[list][s0 + 2] = PART_NOT_AVAILABLE;
                self.ref_cache[list][s0 + 2 + 16] = PART_NOT_AVAILABLE;
                self.mv_cache[list][s0 + 2] = [0; 2];
                self.mv_cache[list][s0 + 2 + 16] = [0; 2];

                if cabac {
                    if uses_list(top_type, list) {
                        let m = self.mvd_table[list][self.t(top_xy)];
                        self.mvd_cache[list][s0 - 8..s0 - 4].copy_from_slice(&m[0..4]);
                    } else {
                        self.mvd_cache[list][s0 - 8..s0 - 4].fill([0; 2]);
                    }
                    if uses_list(left_type, list) {
                        let m = self.mvd_table[list][self.t(left_xy)];
                        for k in 0..4 {
                            self.mvd_cache[list][s0 - 1 + 8 * k] = m[6 - LEFT_BLOCK[k]];
                        }
                    } else {
                        for k in 0..4 {
                            self.mvd_cache[list][s0 - 1 + 8 * k] = [0; 2];
                        }
                    }
                    self.mvd_cache[list][s0 + 2] = [0; 2];
                    self.mvd_cache[list][s0 + 2 + 16] = [0; 2];
                    if self.slice_type == SliceType::B {
                        for y in 0..4 {
                            self.direct_cache[s0 + 8 * y..s0 + 8 * y + 4]
                                .fill((MB_TYPE_16X16 >> 1) as u8);
                        }
                        let dt = |c: &Ctx, xy: isize| c.direct_table[c.t(xy)];
                        if is_direct(top_type) {
                            self.direct_cache[s0 - 8..s0 - 4].fill((MB_TYPE_DIRECT2 >> 1) as u8);
                        } else if top_type & MB_TYPE_8X8 != 0 {
                            let d = dt(self, top_xy);
                            self.direct_cache[s0 - 8] = d[2];
                            self.direct_cache[s0 - 6] = d[3];
                        } else {
                            self.direct_cache[s0 - 8..s0 - 4].fill((MB_TYPE_16X16 >> 1) as u8);
                        }
                        let left_direct = if is_direct(left_type) {
                            [(MB_TYPE_DIRECT2 >> 1) as u8; 2]
                        } else if left_type & MB_TYPE_8X8 != 0 {
                            let d = dt(self, left_xy);
                            [d[1 + (LEFT_BLOCK[0] & !1)], d[1 + (LEFT_BLOCK[2] & !1)]]
                        } else {
                            [(MB_TYPE_16X16 >> 1) as u8; 2]
                        };
                        self.direct_cache[s0 - 1] = left_direct[0];
                        self.direct_cache[s0 - 1 + 16] = left_direct[1];
                    }
                }
            }
        }
        self.neighbor_transform_size =
            usize::from(is_8x8dct(top_type)) + usize::from(is_8x8dct(left_type));
    }

    /// Index of a macroblock's top-left 4x4 block in `motion_val`.
    #[inline]
    pub fn mb2b(&self, mb_xy: isize) -> isize {
        let x = mb_xy.rem_euclid(self.mb_stride as isize);
        let y = mb_xy.div_euclid(self.mb_stride as isize);
        4 * x + 4 * y * self.b_stride as isize
    }

    /// A macroblock's four 8x8 reference indices for `list`.
    #[inline]
    pub fn ref_idx_at(&self, list: usize, mb_xy: isize) -> [i8; 4] {
        let i = 4 * mb_xy as usize;
        let r = &self.ref_index[list];
        [r[i], r[i + 1], r[i + 2], r[i + 3]]
    }

    // --- Motion vector prediction (h264_mvpred.h) --------------------------------------

    fn fetch_diagonal_mv(&self, i: usize, list: usize, part_width: usize) -> ([i16; 2], i8) {
        let topright_ref = self.ref_cache[list][i - 8 + part_width];
        if topright_ref != PART_NOT_AVAILABLE {
            (self.mv_cache[list][i - 8 + part_width], topright_ref)
        } else {
            (
                self.mv_cache[list][i - 8 - 1],
                self.ref_cache[list][i - 8 - 1],
            )
        }
    }

    pub fn pred_motion(&self, n: usize, part_width: usize, list: usize, r: i8) -> (i32, i32) {
        let index8 = SCAN8[n];
        let top_ref = self.ref_cache[list][index8 - 8];
        let left_ref = self.ref_cache[list][index8 - 1];
        let a = self.mv_cache[list][index8 - 1];
        let b = self.mv_cache[list][index8 - 8];
        let (c, diagonal_ref) = self.fetch_diagonal_mv(index8, list, part_width);
        let match_count =
            u32::from(diagonal_ref == r) + u32::from(top_ref == r) + u32::from(left_ref == r);
        let mid = |a: [i16; 2], b: [i16; 2], c: [i16; 2]| {
            (
                mid_pred(i32::from(a[0]), i32::from(b[0]), i32::from(c[0])),
                mid_pred(i32::from(a[1]), i32::from(b[1]), i32::from(c[1])),
            )
        };
        let v = |m: [i16; 2]| (i32::from(m[0]), i32::from(m[1]));
        if match_count > 1 {
            mid(a, b, c)
        } else if match_count == 1 {
            if left_ref == r {
                v(a)
            } else if top_ref == r {
                v(b)
            } else {
                v(c)
            }
        } else if top_ref == PART_NOT_AVAILABLE
            && diagonal_ref == PART_NOT_AVAILABLE
            && left_ref != PART_NOT_AVAILABLE
        {
            v(a)
        } else {
            mid(a, b, c)
        }
    }

    pub fn pred_16x8_motion(&self, n: usize, list: usize, r: i8) -> (i32, i32) {
        if n == 0 {
            let top_ref = self.ref_cache[list][SCAN8[0] - 8];
            if top_ref == r {
                let b = self.mv_cache[list][SCAN8[0] - 8];
                return (i32::from(b[0]), i32::from(b[1]));
            }
        } else {
            let left_ref = self.ref_cache[list][SCAN8[8] - 1];
            if left_ref == r {
                let a = self.mv_cache[list][SCAN8[8] - 1];
                return (i32::from(a[0]), i32::from(a[1]));
            }
        }
        self.pred_motion(n, 4, list, r)
    }

    pub fn pred_8x16_motion(&self, n: usize, list: usize, r: i8) -> (i32, i32) {
        if n == 0 {
            let left_ref = self.ref_cache[list][SCAN8[0] - 1];
            if left_ref == r {
                let a = self.mv_cache[list][SCAN8[0] - 1];
                return (i32::from(a[0]), i32::from(a[1]));
            }
        } else {
            let (c, diagonal_ref) = self.fetch_diagonal_mv(SCAN8[4], list, 2);
            if diagonal_ref == r {
                return (i32::from(c[0]), i32::from(c[1]));
            }
        }
        self.pred_motion(n, 2, list, r)
    }

    /// P_Skip motion (`pred_pskip_motion`).
    pub fn pred_pskip_motion(&mut self) {
        let s0 = SCAN8[0];
        for y in 0..4 {
            self.ref_cache[0][s0 + 8 * y..s0 + 8 * y + 4].fill(0);
        }
        let b_stride = self.b_stride as isize;
        let zero = [0i16; 2];
        let set_all = |c: &mut Ctx, mv: [i16; 2]| {
            for y in 0..4 {
                c.mv_cache[0][s0 + 8 * y..s0 + 8 * y + 4].fill(mv);
            }
        };
        let (left_ref, a) = if uses_list(self.left_type, 0) {
            let r = self.ref_idx_at(0, self.left_mb_xy)[1 + (LEFT_BLOCK[0] & !1)];
            let mv = self.motion_val[0]
                [(self.mb2b(self.left_mb_xy) + 3 + b_stride * LEFT_BLOCK[0] as isize) as usize];
            if r == 0 && mv == zero {
                return set_all(self, zero);
            }
            (r, mv)
        } else if self.left_type != 0 {
            (LIST_NOT_USED, zero)
        } else {
            return set_all(self, zero);
        };
        let (top_ref, b) = if uses_list(self.top_type, 0) {
            let r = self.ref_idx_at(0, self.top_mb_xy)[2];
            let mv = self.motion_val[0][(self.mb2b(self.top_mb_xy) + 3 * b_stride) as usize];
            if r == 0 && mv == zero {
                return set_all(self, zero);
            }
            (r, mv)
        } else if self.top_type != 0 {
            (LIST_NOT_USED, zero)
        } else {
            return set_all(self, zero);
        };
        let (diagonal_ref, c) = if uses_list(self.topright_type, 0) {
            let r = self.ref_idx_at(0, self.topright_mb_xy)[2];
            let mv = self.motion_val[0][(self.mb2b(self.topright_mb_xy) + 3 * b_stride) as usize];
            (r, mv)
        } else if self.topright_type != 0 {
            (LIST_NOT_USED, zero)
        } else if uses_list(self.topleft_type, 0) {
            let r = self.ref_idx_at(0, self.topleft_mb_xy)[3];
            let mv =
                self.motion_val[0][(self.mb2b(self.topleft_mb_xy) + 3 + 3 * b_stride) as usize];
            (r, mv)
        } else if self.topleft_type != 0 {
            (LIST_NOT_USED, zero)
        } else {
            (PART_NOT_AVAILABLE, zero)
        };
        let match_count =
            u32::from(diagonal_ref == 0) + u32::from(top_ref == 0) + u32::from(left_ref == 0);
        let mv = if match_count > 1 {
            [
                mid_pred(i32::from(a[0]), i32::from(b[0]), i32::from(c[0])) as i16,
                mid_pred(i32::from(a[1]), i32::from(b[1]), i32::from(c[1])) as i16,
            ]
        } else if match_count == 1 {
            if left_ref == 0 {
                a
            } else if top_ref == 0 {
                b
            } else {
                c
            }
        } else {
            [
                mid_pred(i32::from(a[0]), i32::from(b[0]), i32::from(c[0])) as i16,
                mid_pred(i32::from(a[1]), i32::from(b[1]), i32::from(c[1])) as i16,
            ]
        };
        set_all(self, mv);
    }

    pub fn pred_intra_mode(&self, n: usize) -> i8 {
        let index8 = SCAN8[n];
        let left = self.intra4x4_pred_mode_cache[index8 - 1];
        let top = self.intra4x4_pred_mode_cache[index8 - 8];
        let min = left.min(top);
        if min < 0 {
            2 // DC_PRED
        } else {
            min
        }
    }

    pub fn get_dct8x8_allowed(&self) -> bool {
        let mask = if self.direct_8x8_inference {
            MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_8X8
        } else {
            MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_8X8 | MB_TYPE_DIRECT2
        };
        self.sub_mb_type.iter().all(|&t| t & mask == 0)
    }

    // --- Direct prediction (h264_direct.c, frames only) --------------------------------

    pub fn pred_direct_motion(&mut self, mb_type: &mut u32) -> Result<(), DecodeError> {
        if self.ref_count[1] == 0 || self.ref_count[0] == 0 {
            return Err(DecodeError::Unsupported("B direct without references"));
        }
        let col = self.ref_list[1].first().map(|e| e.pic.clone());
        let Some(col) = col else {
            return Err(DecodeError::Unsupported("B direct without references"));
        };
        if !self.reference_matches(&col) {
            return Err(DecodeError::Unsupported("reference picture size mismatch"));
        }
        if self.direct_spatial {
            self.pred_spatial_direct_motion(mb_type);
        } else {
            self.pred_temp_direct_motion(mb_type);
        }
        Ok(())
    }

    fn fill_cache_rect<T: Copy>(cache: &mut [T; 40], start: usize, w: usize, h: usize, v: T) {
        for y in 0..h {
            cache[start + 8 * y..start + 8 * y + w].fill(v);
        }
    }

    fn pred_spatial_direct_motion(&mut self, mb_type: &mut u32) {
        let is_b8x8 = *mb_type & MB_TYPE_8X8 != 0;
        let mut sub_mb_type = MB_TYPE_L0L1;
        let s0 = SCAN8[0];
        let mut r = [0i32; 2];
        let mut mv = [[0i16; 2]; 2];
        for list in 0..2 {
            let left_ref = self.ref_cache[list][s0 - 1];
            let top_ref = self.ref_cache[list][s0 - 8];
            let mut refc = self.ref_cache[list][s0 - 8 + 4];
            let mut c = self.mv_cache[list][s0 - 8 + 4];
            if refc == PART_NOT_AVAILABLE {
                refc = self.ref_cache[list][s0 - 8 - 1];
                c = self.mv_cache[list][s0 - 8 - 1];
            }
            let m = (left_ref as u32).min(top_ref as u32).min(refc as u32) as i32;
            r[list] = m;
            if m >= 0 {
                let a = self.mv_cache[list][s0 - 1];
                let b = self.mv_cache[list][s0 - 8];
                let rl = m as i8;
                let match_count =
                    u32::from(left_ref == rl) + u32::from(top_ref == rl) + u32::from(refc == rl);
                mv[list] = if match_count > 1 {
                    [
                        mid_pred(i32::from(a[0]), i32::from(b[0]), i32::from(c[0])) as i16,
                        mid_pred(i32::from(a[1]), i32::from(b[1]), i32::from(c[1])) as i16,
                    ]
                } else if left_ref == rl {
                    a
                } else if top_ref == rl {
                    b
                } else {
                    c
                };
            } else {
                let mask = !(MB_TYPE_L0 << (2 * list));
                mv[list] = [0; 2];
                r[list] = -1;
                if !is_b8x8 {
                    *mb_type &= mask;
                }
                sub_mb_type &= mask;
            }
        }
        if r[0] < 0 && r[1] < 0 {
            r = [0, 0];
            if !is_b8x8 {
                *mb_type |= MB_TYPE_L0L1;
            }
            sub_mb_type |= MB_TYPE_L0L1;
        }
        let (r0, r1) = (r[0] as i8, r[1] as i8);
        if !is_b8x8 && mv[0] == [0; 2] && mv[1] == [0; 2] {
            Self::fill_cache_rect(&mut self.ref_cache[0], s0, 4, 4, r0);
            Self::fill_cache_rect(&mut self.ref_cache[1], s0, 4, 4, r1);
            Self::fill_cache_rect(&mut self.mv_cache[0], s0, 4, 4, [0; 2]);
            Self::fill_cache_rect(&mut self.mv_cache[1], s0, 4, 4, [0; 2]);
            *mb_type = (*mb_type
                & !(MB_TYPE_8X8 | MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_P1L0 | MB_TYPE_P1L1))
                | MB_TYPE_16X16
                | MB_TYPE_DIRECT2;
            return;
        }

        let col = self.ref_list[1][0].pic.clone();
        let col_long = self.ref_list[1][0].long;
        let mb_type_col = col.mb_type[self.t(self.mb_xy)];
        sub_mb_type |= MB_TYPE_16X16 | MB_TYPE_DIRECT2;
        if !is_b8x8
            && mb_type_col
                & (MB_TYPE_16X16 | MB_TYPE_INTRA4X4 | MB_TYPE_INTRA16X16 | MB_TYPE_INTRA_PCM)
                != 0
        {
            *mb_type |= MB_TYPE_16X16 | MB_TYPE_DIRECT2;
        } else if !is_b8x8 && mb_type_col & (MB_TYPE_16X8 | MB_TYPE_8X16) != 0 {
            *mb_type |= MB_TYPE_DIRECT2 | (mb_type_col & (MB_TYPE_16X8 | MB_TYPE_8X16));
        } else {
            if !self.direct_8x8_inference {
                sub_mb_type = (sub_mb_type & !MB_TYPE_16X16) | MB_TYPE_8X8;
            }
            *mb_type |= MB_TYPE_8X8;
        }

        let b4_stride = self.b_stride;
        let b_base = self.mb2b(self.mb_xy) as usize;
        let l1mv = |list: usize, idx: usize| col.motion_val[list][b_base + idx];
        let rbase = 4 * self.mb_xy as usize;
        let l1ref0 = |i: usize| col.ref_index[0][rbase + i];
        let l1ref1 = |i: usize| col.ref_index[1][rbase + i];
        let small = |m: [i16; 2]| m[0].abs() <= 1 && m[1].abs() <= 1;

        if *mb_type & MB_TYPE_16X16 != 0 {
            Self::fill_cache_rect(&mut self.ref_cache[0], s0, 4, 4, r0);
            Self::fill_cache_rect(&mut self.ref_cache[1], s0, 4, 4, r1);
            let (a, b) = if !is_intra(mb_type_col)
                && !col_long
                && ((l1ref0(0) == 0 && small(l1mv(0, 0)))
                    || (l1ref0(0) < 0 && l1ref1(0) == 0 && small(l1mv(1, 0))))
            {
                (
                    if r[0] > 0 { mv[0] } else { [0; 2] },
                    if r[1] > 0 { mv[1] } else { [0; 2] },
                )
            } else {
                (mv[0], mv[1])
            };
            Self::fill_cache_rect(&mut self.mv_cache[0], s0, 4, 4, a);
            Self::fill_cache_rect(&mut self.mv_cache[1], s0, 4, 4, b);
        } else {
            let mut n = 0;
            for i8 in 0..4 {
                let (x8, y8) = (i8 & 1, i8 >> 1);
                if is_b8x8 && !is_direct(self.sub_mb_type[i8]) {
                    continue;
                }
                self.sub_mb_type[i8] = sub_mb_type;
                let at = SCAN8[i8 * 4];
                Self::fill_cache_rect(&mut self.mv_cache[0], at, 2, 2, mv[0]);
                Self::fill_cache_rect(&mut self.mv_cache[1], at, 2, 2, mv[1]);
                Self::fill_cache_rect(&mut self.ref_cache[0], at, 2, 2, r0);
                Self::fill_cache_rect(&mut self.ref_cache[1], at, 2, 2, r1);
                if !is_intra(mb_type_col)
                    && !col_long
                    && (l1ref0(i8) == 0 || (l1ref0(i8) < 0 && l1ref1(i8) == 0))
                {
                    let lst = if l1ref0(i8) == 0 { 0 } else { 1 };
                    if sub_mb_type & MB_TYPE_16X16 != 0 {
                        let mv_col = l1mv(lst, x8 * 3 + y8 * 3 * b4_stride);
                        if small(mv_col) {
                            if r[0] == 0 {
                                Self::fill_cache_rect(&mut self.mv_cache[0], at, 2, 2, [0; 2]);
                            }
                            if r[1] == 0 {
                                Self::fill_cache_rect(&mut self.mv_cache[1], at, 2, 2, [0; 2]);
                            }
                            n += 4;
                        }
                    } else {
                        let mut m = 0;
                        for i4 in 0..4 {
                            let mv_col =
                                l1mv(lst, x8 * 2 + (i4 & 1) + (y8 * 2 + (i4 >> 1)) * b4_stride);
                            if small(mv_col) {
                                let c = SCAN8[i8 * 4 + i4];
                                if r[0] == 0 {
                                    self.mv_cache[0][c] = [0; 2];
                                }
                                if r[1] == 0 {
                                    self.mv_cache[1][c] = [0; 2];
                                }
                                m += 1;
                            }
                        }
                        if m & 3 == 0 {
                            self.sub_mb_type[i8] =
                                (self.sub_mb_type[i8] & !MB_TYPE_8X8) | MB_TYPE_16X16;
                        }
                        n += m;
                    }
                }
            }
            if !is_b8x8 && n & 15 == 0 {
                *mb_type = (*mb_type
                    & !(MB_TYPE_8X8 | MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_P1L0 | MB_TYPE_P1L1))
                    | MB_TYPE_16X16
                    | MB_TYPE_DIRECT2;
            }
        }
    }

    fn pred_temp_direct_motion(&mut self, mb_type: &mut u32) {
        let is_b8x8 = *mb_type & MB_TYPE_8X8 != 0;
        let s0 = SCAN8[0];
        let col = self.ref_list[1][0].pic.clone();
        let mb_type_col = col.mb_type[self.t(self.mb_xy)];
        let mut sub_mb_type = MB_TYPE_16X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_DIRECT2;
        if !is_b8x8
            && mb_type_col
                & (MB_TYPE_16X16 | MB_TYPE_INTRA4X4 | MB_TYPE_INTRA16X16 | MB_TYPE_INTRA_PCM)
                != 0
        {
            *mb_type |= MB_TYPE_16X16 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_DIRECT2;
        } else if !is_b8x8 && mb_type_col & (MB_TYPE_16X8 | MB_TYPE_8X16) != 0 {
            *mb_type |=
                MB_TYPE_L0L1 | MB_TYPE_DIRECT2 | (mb_type_col & (MB_TYPE_16X8 | MB_TYPE_8X16));
        } else {
            if !self.direct_8x8_inference {
                sub_mb_type = MB_TYPE_8X8 | MB_TYPE_P0L0 | MB_TYPE_P0L1 | MB_TYPE_DIRECT2;
            }
            *mb_type |= MB_TYPE_8X8 | MB_TYPE_L0L1;
        }

        let b4_stride = self.b_stride;
        let b_base = self.mb2b(self.mb_xy) as usize;
        let l1mv = |list: usize, idx: usize| col.motion_val[list][b_base + idx];
        let rbase = 4 * self.mb_xy as usize;
        let l1ref0 = |i: usize| col.ref_index[0][rbase + i];
        let l1ref1 = |i: usize| col.ref_index[1][rbase + i];
        let map = self.map_col_to_list0;
        let dsf = self.dist_scale_factor;
        let pack = |mx: i32, my: i32| [mx as i16, my as i16];

        if *mb_type & MB_TYPE_16X16 != 0 {
            Self::fill_cache_rect(&mut self.ref_cache[1], s0, 4, 4, 0);
            let (r, mv0, mv1) = if is_intra(mb_type_col) {
                (0i8, [0i16; 2], [0i16; 2])
            } else {
                let (ref0, mv_col) = if l1ref0(0) >= 0 {
                    (map[0][l1ref0(0) as usize], l1mv(0, 0))
                } else {
                    (map[1][l1ref1(0).max(0) as usize], l1mv(1, 0))
                };
                let scale = dsf[ref0 as usize];
                let mx = (scale * i32::from(mv_col[0]) + 128) >> 8;
                let my = (scale * i32::from(mv_col[1]) + 128) >> 8;
                (
                    ref0 as i8,
                    pack(mx, my),
                    pack(mx - i32::from(mv_col[0]), my - i32::from(mv_col[1])),
                )
            };
            Self::fill_cache_rect(&mut self.ref_cache[0], s0, 4, 4, r);
            Self::fill_cache_rect(&mut self.mv_cache[0], s0, 4, 4, mv0);
            Self::fill_cache_rect(&mut self.mv_cache[1], s0, 4, 4, mv1);
        } else {
            for i8 in 0..4 {
                let (x8, y8) = (i8 & 1, i8 >> 1);
                if is_b8x8 && !is_direct(self.sub_mb_type[i8]) {
                    continue;
                }
                self.sub_mb_type[i8] = sub_mb_type;
                let at = SCAN8[i8 * 4];
                Self::fill_cache_rect(&mut self.ref_cache[1], at, 2, 2, 0);
                if is_intra(mb_type_col) {
                    Self::fill_cache_rect(&mut self.ref_cache[0], at, 2, 2, 0);
                    Self::fill_cache_rect(&mut self.mv_cache[0], at, 2, 2, [0; 2]);
                    Self::fill_cache_rect(&mut self.mv_cache[1], at, 2, 2, [0; 2]);
                    continue;
                }
                let (ref0, lst) = if l1ref0(i8) >= 0 {
                    (map[0][l1ref0(i8) as usize], 0)
                } else {
                    (map[1][l1ref1(i8).max(0) as usize], 1)
                };
                let scale = dsf[ref0 as usize];
                Self::fill_cache_rect(&mut self.ref_cache[0], at, 2, 2, ref0 as i8);
                if sub_mb_type & MB_TYPE_16X16 != 0 {
                    let mv_col = l1mv(lst, x8 * 3 + y8 * 3 * b4_stride);
                    let mx = (scale * i32::from(mv_col[0]) + 128) >> 8;
                    let my = (scale * i32::from(mv_col[1]) + 128) >> 8;
                    Self::fill_cache_rect(&mut self.mv_cache[0], at, 2, 2, pack(mx, my));
                    Self::fill_cache_rect(
                        &mut self.mv_cache[1],
                        at,
                        2,
                        2,
                        pack(mx - i32::from(mv_col[0]), my - i32::from(mv_col[1])),
                    );
                } else {
                    for i4 in 0..4 {
                        let mv_col =
                            l1mv(lst, x8 * 2 + (i4 & 1) + (y8 * 2 + (i4 >> 1)) * b4_stride);
                        let c = SCAN8[i8 * 4 + i4];
                        let mx = (scale * i32::from(mv_col[0]) + 128) >> 8;
                        let my = (scale * i32::from(mv_col[1]) + 128) >> 8;
                        self.mv_cache[0][c] = pack(mx, my);
                        self.mv_cache[1][c] =
                            pack(mx - i32::from(mv_col[0]), my - i32::from(mv_col[1]));
                    }
                }
            }
        }
    }

    // --- Writing a macroblock back to the picture tables -------------------------------

    pub fn write_back_intra_pred_mode(&mut self) {
        let t = self.t(self.mb_xy);
        let c = &self.intra4x4_pred_mode_cache;
        let m = &mut self.intra4x4_pred_mode[t];
        m[0..4].copy_from_slice(&c[4 + 8 * 4..8 + 8 * 4]);
        m[4] = c[7 + 8 * 3];
        m[5] = c[7 + 8 * 2];
        m[6] = c[7 + 8];
    }

    pub fn write_back_non_zero_count(&mut self) {
        let t = self.t(self.mb_xy);
        let c = self.non_zero_count_cache;
        let nnz = &mut self.non_zero_count[t];
        for (dst, row) in [
            (0, 1),
            (4, 2),
            (8, 3),
            (12, 4),
            (16, 6),
            (20, 7),
            (32, 11),
            (36, 12),
        ] {
            nnz[dst..dst + 4].copy_from_slice(&c[4 + 8 * row..8 + 8 * row]);
        }
    }

    pub fn write_back_motion(&mut self, mb_type: u32) {
        let b_xy = self.mb2b(self.mb_xy) as usize;
        let b8_xy = 4 * self.mb_xy as usize;
        let s0 = SCAN8[0];
        let t = self.t(self.mb_xy);
        for list in 0..2 {
            if !uses_list(mb_type, list) {
                if list == 0 {
                    self.ref_index[0][b8_xy..b8_xy + 4].fill(LIST_NOT_USED);
                    self.ref_ids[0][t] = [-1; 4];
                }
                continue;
            }
            for y in 0..4 {
                for x in 0..4 {
                    self.motion_val[list][b_xy + y * self.b_stride + x] =
                        self.mv_cache[list][s0 + 8 * y + x];
                }
            }
            if self.cabac_mode {
                let m = &mut self.mvd_table[list][t];
                if is_skip(mb_type) {
                    *m = [[0; 2]; 8];
                } else {
                    let c = &self.mvd_cache[list];
                    m[0..4].copy_from_slice(&c[s0 + 8 * 3..s0 + 8 * 3 + 4]);
                    m[6] = c[s0 + 3];
                    m[5] = c[s0 + 3 + 8];
                    m[4] = c[s0 + 3 + 16];
                }
            }
            let rc = &self.ref_cache[list];
            let refs = [rc[SCAN8[0]], rc[SCAN8[4]], rc[SCAN8[8]], rc[SCAN8[12]]];
            self.ref_index[list][b8_xy..b8_xy + 4].copy_from_slice(&refs);
            let ids = refs.map(|r| {
                if r >= 0 {
                    self.ref_list[list]
                        .get(r as usize)
                        .map_or(-1, |e| e.pic.id as i32)
                } else {
                    -1
                }
            });
            self.ref_ids[list][t] = ids;
        }
        if self.slice_type == SliceType::B && self.cabac_mode && mb_type & MB_TYPE_8X8 != 0 {
            let d = &mut self.direct_table[t];
            d[1] = (self.sub_mb_type[1] >> 1) as u8;
            d[2] = (self.sub_mb_type[2] >> 1) as u8;
            d[3] = (self.sub_mb_type[3] >> 1) as u8;
        }
    }

    /// P_Skip / B_Skip (`decode_mb_skip`).
    pub fn decode_mb_skip(&mut self) -> Result<(), DecodeError> {
        let t = self.t(self.mb_xy);
        self.non_zero_count[t] = [0; 48];
        let mut mb_type;
        if self.slice_type == SliceType::B {
            mb_type = MB_TYPE_L0L1 | MB_TYPE_DIRECT2 | MB_TYPE_SKIP;
            if self.direct_spatial {
                self.fill_decode_neighbors();
                self.fill_decode_caches(mb_type);
            }
            self.pred_direct_motion(&mut mb_type)?;
            mb_type |= MB_TYPE_SKIP;
        } else {
            if self.ref_count[0] == 0 {
                return Err(DecodeError::Unsupported("P_Skip without references"));
            }
            mb_type = MB_TYPE_16X16 | MB_TYPE_P0L0 | MB_TYPE_P1L0 | MB_TYPE_SKIP;
            self.fill_decode_neighbors();
            self.pred_pskip_motion();
        }
        self.write_back_motion(mb_type);
        self.mb_type[t] = mb_type;
        self.qscale_table[t] = self.qscale as u8;
        self.slice_table[t] = self.slice_num;
        self.prev_mb_skipped = true;
        Ok(())
    }

    /// The per-macroblock state every path sets once the macroblock is parsed.
    pub fn finish_mb_tables(&mut self) {
        let t = self.t(self.mb_xy);
        self.list_counts[t] = self.list_count as u8;
    }
}

#[inline]
pub fn mid_pred(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}
