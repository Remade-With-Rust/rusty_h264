//! I_16x16 macroblock decoding — the mirror of the encoder's `mb16`.
//!
//! Parses each macroblock's residuals and reconstructs it with the exact same
//! prediction + inverse-transform helpers the encoder uses, so decoder output
//! matches encoder reconstruction bit-for-bit.
#![allow(clippy::needless_range_loop)]

use rusty_h264_common::bit_reader::OutOfData;
use rusty_h264_common::cavlc::{
    decode_residual_block, read_cbp_inter, read_cbp_intra, un_scan_4x4_ac_into, un_scan_4x4_dcac,
};
use rusty_h264_common::inter::{
    inter_partitions, mc_chroma, mc_luma, predict_mv, predict_partition_mv, MvNeighbor,
};
use rusty_h264_common::predict::{
    chroma8x8_pred, chroma_qp, intra4x4_pred, luma16x16_pred, reconstruct_4x4,
    I16Mode, CHROMA_4X4_SCAN_XY, LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{
    dequantize, dequantize_weighted, inverse_quant_chroma_dc, inverse_quant_chroma_dc_weighted,
    inverse_quant_luma_dc, inverse_quant_luma_dc_weighted,
};
use rusty_h264_common::{BitReader, YuvFrame};

/// Reconstructed coded-size planes plus CAVLC `nnz` context grids.
pub struct FrameDecoder {
    mb_w: usize,
    mb_h: usize,
    /// Slice QP (`SliceQPy`) — the deblock filter's frame-level QP.
    qp: u8,
    /// Running luma QP (`QPy`), carried across macroblocks and stepped by each
    /// `mb_qp_delta` (spec §7.4.5). Equals `qp` on constant-QP streams.
    cur_qp: u8,
    /// `chroma_qp_index_offset` from the active PPS (§8.5.8).
    chroma_qp_offset: i32,
    cw: usize,
    ch: usize,
    ccw: usize,
    cch: usize,
    rec_y: Vec<u8>,
    rec_u: Vec<u8>,
    rec_v: Vec<u8>,
    /// Per-macroblock luma QP (`QPy`), for per-edge deblock strength.
    mb_qp: Vec<u8>,
    /// First macroblock address of the slice currently being decoded. Neighbors
    /// with a lower address belong to an earlier slice and are "not available"
    /// for prediction (spec §8.3/§8.4). Slices are contiguous raster ranges (we
    /// reject FMO/slice-groups), so address ≥ this ⇔ same slice.
    slice_first_mb: usize,
    nnz_y: Vec<u8>,
    nnz_c: [Vec<u8>; 2],
    modes_y: Vec<u8>,
    coded_y: Vec<bool>,
    /// Per-4×4-block List-0 motion (mv + ref index, `-1` = no L0). For P slices
    /// this is the only motion; B slices add the List-1 grids below.
    mv_y: Vec<(i32, i32)>,
    inter_y: Vec<bool>,
    ref_idx_y: Vec<i32>,
    /// Per-4×4-block List-1 motion for B slices (`ref_idx1 = -1` = no L1).
    mv1: Vec<(i32, i32)>,
    ref_idx1: Vec<i32>,
    /// `RefPicList1` and B-slice flags (unused outside B slices).
    refs1: Vec<crate::RefFrame>,
    num_ref_active1: usize,
    is_b: bool,
    direct_spatial: bool,
    nnz_l_cache: [u8; 25],
    nnz_c_cache: [[u8; 9]; 2],
    /// Decoded-picture buffer (most-recent first); empty in I-slices. `ref_idx`
    /// indexes into this list.
    refs: Vec<crate::RefFrame>,
    /// `num_ref_idx_l0_active` for the current slice — drives whether `ref_idx`
    /// is coded (active > 1) and its te(v)/ue(v) form, independently of how many
    /// reference pictures actually exist (spec §7.4.5.1, §9.1).
    num_ref_active: usize,
    /// `constrained_intra_pred_flag`: when set, intra prediction may only use
    /// samples from intra-coded neighbors (inter neighbors are "not available").
    constrained_intra: bool,
    /// High-profile 4×4 scaling matrices in **raster** order, indexed by
    /// `[Y-intra, Cb-intra, Cr-intra, Y-inter, Cb-inter, Cr-inter]`. `None` = flat.
    scaling: Option<[[i32; 16]; 6]>,
}

/// Why a macroblock could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MbError {
    Truncated,
    Unsupported(&'static str),
}

impl From<OutOfData> for MbError {
    fn from(_: OutOfData) -> Self {
        MbError::Truncated
    }
}

impl FrameDecoder {
    pub fn new(
        mb_w: usize,
        mb_h: usize,
        qp: u8,
        chroma_qp_offset: i32,
        refs: Vec<crate::RefFrame>,
        num_ref_active: usize,
        constrained_intra: bool,
    ) -> Self {
        let (cw, ch) = (mb_w * 16, mb_h * 16);
        let (ccw, cch) = (cw / 2, ch / 2);
        Self {
            mb_w,
            mb_h,
            qp,
            cur_qp: qp,
            chroma_qp_offset,
            cw,
            ch,
            ccw,
            cch,
            rec_y: vec![0; cw * ch],
            rec_u: vec![0; ccw * cch],
            rec_v: vec![0; ccw * cch],
            mb_qp: vec![qp; mb_w * mb_h],
            slice_first_mb: 0,
            nnz_y: vec![0; (mb_w * 4) * (mb_h * 4)],
            nnz_c: [vec![0; (mb_w * 2) * (mb_h * 2)], vec![0; (mb_w * 2) * (mb_h * 2)]],
            modes_y: vec![2; (mb_w * 4) * (mb_h * 4)],
            coded_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            mv_y: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            inter_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            ref_idx_y: vec![-1; (mb_w * 4) * (mb_h * 4)],
            mv1: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            ref_idx1: vec![-1; (mb_w * 4) * (mb_h * 4)],
            refs1: Vec::new(),
            num_ref_active1: 0,
            is_b: false,
            direct_spatial: true,
            nnz_l_cache: [0x80; 25],
            nnz_c_cache: [[0x80; 9]; 2],
            refs,
            num_ref_active,
            constrained_intra,
            scaling: None,
        }
    }

    /// Sets the High-profile 4×4 scaling matrices (raster order, six lists). The
    /// caller un-zig-zags the SPS lists. `None`-equivalent (flat) is the default.
    pub fn set_scaling(&mut self, scaling: [[i32; 16]; 6]) {
        self.scaling = Some(scaling);
    }

    /// Dequantizes a 4×4 AC block with scaling list `list` (flat if none active).
    fn dequant(&self, levels: &[i32; 16], qp: u8, list: usize) -> [i32; 16] {
        match &self.scaling {
            Some(s) => dequantize_weighted(levels, qp, &s[list]),
            None => dequantize(levels, qp),
        }
    }

    /// Inverse-quantizes the I_16x16 luma DC with scaling list `list`'s DC weight.
    fn dequant_luma_dc(&self, levels: &[i32; 16], qp: u8, list: usize) -> [i32; 16] {
        match &self.scaling {
            Some(s) => inverse_quant_luma_dc_weighted(levels, qp, s[list][0]),
            None => inverse_quant_luma_dc(levels, qp),
        }
    }

    /// Inverse-quantizes a chroma DC block with scaling list `list`'s DC weight.
    fn dequant_chroma_dc(&self, levels: &[i32; 4], qp: u8, list: usize) -> [i32; 4] {
        match &self.scaling {
            Some(s) => inverse_quant_chroma_dc_weighted(levels, qp, s[list][0]),
            None => inverse_quant_chroma_dc(levels, qp),
        }
    }

    /// Sets the B-slice context for the slice about to be decoded: `RefPicList1`,
    /// its active count, and the direct-mode flag.
    pub fn set_b_context(
        &mut self,
        refs1: Vec<crate::RefFrame>,
        num_ref_active1: usize,
        direct_spatial: bool,
    ) {
        self.is_b = true;
        self.refs1 = refs1;
        self.num_ref_active1 = num_ref_active1;
        self.direct_spatial = direct_spatial;
    }

    /// Steps the running luma QP by a `mb_qp_delta` (spec §7.4.5, 8-bit depth):
    /// `QPy = (QPy_prev + delta + 52) % 52`.
    fn step_qp(&mut self, delta: i32) {
        self.cur_qp = (self.cur_qp as i32 + delta + 52).rem_euclid(52) as u8;
    }

    /// Maps a luma QP to its chroma QP, applying `chroma_qp_index_offset`
    /// (spec §8.5.8): `QPc = qpc_table(Clip3(0, 51, QPy + offset))`.
    fn chroma_qp_for(&self, qp_y: u8) -> u8 {
        let qpi = (qp_y as i32 + self.chroma_qp_offset).clamp(0, 51) as u8;
        chroma_qp(qpi)
    }

    /// Resets per-slice state before decoding a continuation slice of the same
    /// picture: the running QP (each slice carries its own `slice_qp`) and the
    /// reference list (each slice may reorder it).
    pub fn begin_slice(&mut self, slice_qp: u8, refs: Vec<crate::RefFrame>, num_ref_active: usize) {
        self.cur_qp = slice_qp;
        self.qp = slice_qp;
        self.refs = refs;
        self.num_ref_active = num_ref_active;
    }

    /// Whether the neighbor macroblock at `(nbx, nby)` is in the slice currently
    /// being decoded (address ≥ the slice's first MB). For single-slice pictures
    /// `slice_first_mb == 0`, so this is always true and prediction is unchanged.
    #[inline]
    fn nbr_in_slice(&self, nbx: usize, nby: usize) -> bool {
        nby * self.mb_w + nbx >= self.slice_first_mb
    }

    /// Whether the neighbor 4×4 block at `(nbx, nby)` may contribute to intra
    /// prediction. With `constrained_intra_pred`, an inter-coded neighbor is
    /// treated as unavailable (spec §8.3.1.2.{1,2}); otherwise always usable.
    #[inline]
    fn intra_nbr_ok(&self, nbx: usize, nby: usize) -> bool {
        !self.constrained_intra || !self.inter_y[nby * (self.mb_w * 4) + nbx]
    }

    fn mv_neighbors(&self, mb_x: usize, mb_y: usize) -> [MvNeighbor; 3] {
        let w4 = self.mb_w * 4;
        let get = |avail: bool, bx: isize, by: isize| {
            if avail {
                let idx = by as usize * w4 + bx as usize;
                MvNeighbor {
                    available: true,
                    mv: self.mv_y[idx],
                    ref_idx: self.ref_idx_y[idx],
                }
            } else {
                MvNeighbor::NONE
            }
        };
        let (bx, by) = (mb_x as isize * 4, mb_y as isize * 4);
        let a = get(mb_x > 0 && self.nbr_in_slice(mb_x - 1, mb_y), bx - 1, by);
        let b = get(mb_y > 0 && self.nbr_in_slice(mb_x, mb_y - 1), bx, by - 1);
        let c = if mb_y > 0 && mb_x + 1 < self.mb_w && self.nbr_in_slice(mb_x + 1, mb_y - 1) {
            get(true, bx + 4, by - 1)
        } else {
            get(mb_x > 0 && mb_y > 0 && self.nbr_in_slice(mb_x - 1, mb_y - 1), bx - 1, by - 1)
        };
        [a, b, c]
    }

    fn mv_neighbors_block(&self, pbx: isize, pby: isize, pwb: isize) -> [MvNeighbor; 3] {
        let (w4, h4) = ((self.mb_w * 4) as isize, (self.mb_h * 4) as isize);
        let get = |bx: isize, by: isize| -> MvNeighbor {
            // Available iff inside the frame, decoded, and in the current slice.
            if bx < 0
                || by < 0
                || bx >= w4
                || by >= h4
                || !self.coded_y[(by * w4 + bx) as usize]
                || !self.nbr_in_slice(bx as usize / 4, by as usize / 4)
            {
                MvNeighbor::NONE
            } else {
                let idx = (by * w4 + bx) as usize;
                MvNeighbor { available: true, mv: self.mv_y[idx], ref_idx: self.ref_idx_y[idx] }
            }
        };
        let a = get(pbx - 1, pby);
        let b = get(pbx, pby - 1);
        let mut c = get(pbx + pwb, pby - 1);
        if !c.available {
            c = get(pbx - 1, pby - 1);
        }
        [a, b, c]
    }

    fn skip_mv(&self, mb_x: usize, mb_y: usize) -> (i32, i32) {
        let [a, b, c] = self.mv_neighbors(mb_x, mb_y);
        if !a.available
            || !b.available
            || (a.ref_idx == 0 && a.mv == (0, 0))
            || (b.ref_idx == 0 && b.mv == (0, 0))
        {
            (0, 0)
        } else {
            predict_mv(a, b, c, 0)
        }
    }

    fn set_mb_mv(&mut self, mb_x: usize, mb_y: usize, mv: (i32, i32), inter: bool, refi: i32) {
        let w4 = self.mb_w * 4;
        for dy in 0..4 {
            for dx in 0..4 {
                let idx = (mb_y * 4 + dy) * w4 + (mb_x * 4 + dx);
                self.mv_y[idx] = mv;
                self.inter_y[idx] = inter;
                self.ref_idx_y[idx] = if inter { refi } else { -1 };
            }
        }
    }

    /// Snapshots the (deblocked) reconstruction as a reference picture.
    pub fn as_reference(&self) -> crate::RefFrame {
        crate::RefFrame {
            y: self.rec_y.clone(),
            u: self.rec_u.clone(),
            v: self.rec_v.clone(),
            cw: self.cw,
            ch: self.ch,
            frame_num: 0, // set by the caller (decode_slice knows frame_num)
            poc: 0,       // set by the caller
            mv: self.mv_y.clone(),
            ref_idx: self.ref_idx_y.clone(),
            w4: self.mb_w * 4,
            long_term: false,
            long_term_idx: 0,
        }
    }

    fn nnz_cache_load(&mut self, mb_x: usize, mb_y: usize) {
        let w4 = self.mb_w * 4;
        let top_unavail = mb_y == 0 || !self.nbr_in_slice(mb_x, mb_y - 1);
        let left_unavail = mb_x == 0 || !self.nbr_in_slice(mb_x - 1, mb_y);
        for lbx in 0..4 {
            self.nnz_l_cache[1 + lbx] =
                if top_unavail { 0x80 } else { self.nnz_y[(mb_y * 4 - 1) * w4 + (mb_x * 4 + lbx)] };
        }
        for lby in 0..4 {
            self.nnz_l_cache[(lby + 1) * 5] =
                if left_unavail { 0x80 } else { self.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 - 1)] };
        }
    }
    #[inline]
    fn nc_pred(&self, lbx: usize, lby: usize) -> i32 {
        let left = self.nnz_l_cache[(lby + 1) * 5 + lbx] as i32;
        let top = self.nnz_l_cache[lby * 5 + (lbx + 1)] as i32;
        let r = left + top;
        if r < 0x80 { (r + 1) >> 1 } else { r & 0x7f }
    }
    #[inline]
    fn nnz_cache_set(&mut self, lbx: usize, lby: usize, total: u8) {
        self.nnz_l_cache[(lby + 1) * 5 + (lbx + 1)] = total;
    }
    fn chroma_cache_load(&mut self, mb_x: usize, mb_y: usize) {
        let w2 = self.mb_w * 2;
        let top_unavail = mb_y == 0 || !self.nbr_in_slice(mb_x, mb_y - 1);
        let left_unavail = mb_x == 0 || !self.nbr_in_slice(mb_x - 1, mb_y);
        for c in 0..2 {
            for bx in 0..2 {
                self.nnz_c_cache[c][1 + bx] =
                    if top_unavail { 0x80 } else { self.nnz_c[c][(mb_y * 2 - 1) * w2 + (mb_x * 2 + bx)] };
            }
            for by in 0..2 {
                self.nnz_c_cache[c][(by + 1) * 3] =
                    if left_unavail { 0x80 } else { self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 - 1)] };
            }
        }
    }
    #[inline]
    fn chroma_nc_pred(&self, c: usize, bx: usize, by: usize) -> i32 {
        let left = self.nnz_c_cache[c][(by + 1) * 3 + bx] as i32;
        let top = self.nnz_c_cache[c][by * 3 + (bx + 1)] as i32;
        let r = left + top;
        if r < 0x80 { (r + 1) >> 1 } else { r & 0x7f }
    }
    #[inline]
    fn chroma_nnz_cache_set(&mut self, c: usize, bx: usize, by: usize, total: u8) {
        self.nnz_c_cache[c][(by + 1) * 3 + (bx + 1)] = total;
    }

    /// Decodes one slice's macroblocks (raster order) starting at `first_mb`,
    /// until `more_rbsp_data()` is exhausted or the picture is full. Returns the
    /// next macroblock address (= total when the picture is complete). In a
    /// P-slice each macroblock is preceded by `mb_skip_run`.
    pub fn decode_slice_data(
        &mut self,
        r: &mut BitReader,
        is_p: bool,
        first_mb: usize,
    ) -> Result<usize, MbError> {
        let total = self.mb_w * self.mb_h;
        self.slice_first_mb = first_mb;
        let mut addr = first_mb;
        while addr < total {
            if is_p || self.is_b {
                let skip_run = r.read_ue()? as usize;
                for _ in 0..skip_run {
                    if addr >= total {
                        break;
                    }
                    if self.is_b {
                        self.decode_b_skip(addr % self.mb_w, addr / self.mb_w)?;
                    } else {
                        self.decode_p_skip(addr % self.mb_w, addr / self.mb_w)?;
                    }
                    self.mb_qp[addr] = self.cur_qp; // skip inherits QPy
                    addr += 1;
                }
                if addr >= total {
                    break;
                }
                // A trailing skip run with no following macroblock ends the slice.
                if skip_run > 0 && !r.more_rbsp_data() {
                    break;
                }
            }
            if self.is_b {
                self.decode_b_mb(r, addr % self.mb_w, addr / self.mb_w)?;
            } else {
                self.decode_mb(r, addr % self.mb_w, addr / self.mb_w, is_p)?;
            }
            self.mb_qp[addr] = self.cur_qp;
            addr += 1;
            // CAVLC slice end: no more data after this macroblock.
            if !r.more_rbsp_data() {
                break;
            }
        }
        Ok(addr)
    }

    fn decode_mb(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        is_p: bool,
    ) -> Result<(), MbError> {
        let mut mb_type = r.read_ue()?;
        if is_p {
            // In P-slices, mb_type 0/1/2 are inter (16×16, 16×8, 8×16),
            // 3 = P_8x8, 4 = P_8x8ref0 (ref_idx forced 0), 5+ intra.
            if mb_type <= 2 {
                return self.decode_inter(r, mb_x, mb_y, mb_type as u8);
            }
            if mb_type == 3 || mb_type == 4 {
                return self.decode_p8x8(r, mb_x, mb_y, mb_type == 4);
            }
            mb_type -= 5;
        }
        self.decode_intra_mb(r, mb_x, mb_y, mb_type)
    }

    /// Decodes an intra macroblock given its intra `mb_type` (0 = I_4x4,
    /// 1..=24 = I_16x16, 25 = I_PCM) — shared by I-, P- and B-slice paths.
    fn decode_intra_mb(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        mb_type: u32,
    ) -> Result<(), MbError> {
        if mb_type == 0 {
            self.decode_i4x4(r, mb_x, mb_y)?;
        } else if (1..=24).contains(&mb_type) {
            self.decode_i16(r, mb_x, mb_y, mb_type - 1)?;
        } else if mb_type == 25 {
            self.decode_ipcm(r, mb_x, mb_y)?;
        } else {
            return Err(MbError::Unsupported("only I_4x4 / I_16x16 / I_PCM macroblocks"));
        }
        // Mark all luma blocks coded for the next macroblock's top-right.
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
        }
        Ok(())
    }

    /// Reconstructs an inter macroblock (`mode` 0 = P_L0_16x16, 1 = P_16x8,
    /// 2 = P_8x16): parse the per-partition motion vectors and residual,
    /// motion-compensate each partition, and add the residual.
    fn decode_inter(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        mode: u8,
    ) -> Result<(), MbError> {
        if self.refs.is_empty() {
            return Err(MbError::Unsupported("inter without reference"));
        }
        // QP (qp/qpc) is bound after mb_qp_delta is read below.
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let num_refs = self.refs.len();
        let layout = inter_partitions(mode);

        // mb_pred order (spec 7.3.5.1): all ref_idx_l0 first (only when more than
        // one reference is active), then all mvd_l0.
        let mut ref_idxs = vec![0i32; layout.len()];
        if self.num_ref_active > 1 {
            for ri in ref_idxs.iter_mut() {
                *ri = read_ref_idx(r, self.num_ref_active)?;
                if *ri as usize >= num_refs {
                    return Err(MbError::Truncated); // references a non-existent picture
                }
            }
        }

        // Phase 1: per partition, ref-aware MV prediction + mvd, committing the
        // motion grid so a later partition predicts from an earlier one.
        let mut part_mv = vec![(0i32, (0i32, 0i32)); layout.len()];
        for (part, &(rx, ry, rw, rh)) in layout.iter().enumerate() {
            let refi = ref_idxs[part];
            let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
            let [a, b, c] = self.mv_neighbors_block(pbx, pby, (rw / 4) as isize);
            let pmv = predict_partition_mv(mode, part, a, b, c, refi);
            let mvd_x = r.read_se()?;
            let mvd_y = r.read_se()?;
            let mv = (pmv.0 + mvd_x, pmv.1 + mvd_y);
            part_mv[part] = (refi, mv);
            for by in ry / 4..ry / 4 + rh / 4 {
                for bx in rx / 4..rx / 4 + rw / 4 {
                    let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                    self.mv_y[idx] = mv;
                    self.inter_y[idx] = true;
                    self.ref_idx_y[idx] = refi;
                    self.coded_y[idx] = true;
                }
            }
        }

        // Phase 2: motion-compensate each partition from its reference.
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        for (part, &(rx, ry, rw, rh)) in layout.iter().enumerate() {
            let (refi, mv) = part_mv[part];
            let reference = &self.refs[refi as usize];
            let mut tmp = [0u8; 256];
            mc_luma(&reference.y, self.cw, ch, mb_x * 16 + rx, mb_y * 16 + ry, rw, rh, mv.0, mv.1, &mut tmp);
            for dy in 0..rh {
                for dx in 0..rw {
                    pred_y[(ry + dy) * 16 + (rx + dx)] = tmp[dy * rw + dx];
                }
            }
            let (crx, cry, crw, crh) = (rx / 2, ry / 2, rw / 2, rh / 2);
            for cc in 0..2 {
                let rc = if cc == 0 { &reference.u } else { &reference.v };
                let mut tc = [0u8; 64];
                mc_chroma(rc, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv.0, mv.1, &mut tc);
                for dy in 0..crh {
                    for dx in 0..crw {
                        c_pred[cc][(cry + dy) * 8 + (crx + dx)] = tc[dy * crw + dx];
                    }
                }
            }
        }

        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred)
    }

    /// Shared inter tail: parse `coded_block_pattern` + `mb_qp_delta`, decode the
    /// luma/chroma residual, and add it to the already-built motion-compensated
    /// prediction. Used by both the 16×16/16×8/8×16 path and `P_8x8`.
    fn inter_finish(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        pred_y: &[u8; 256],
        c_pred: &[[u8; 64]; 2],
    ) -> Result<(), MbError> {
        let w4 = self.mb_w * 4;
        let cbp = read_cbp_inter(r)?;
        let cbp_luma = cbp & 15;
        let cbp_chroma = cbp >> 4;
        if cbp != 0 {
            self.step_qp(r.read_se()?);
        }
        let (qp, qpc) = (self.cur_qp, self.chroma_qp_for(self.cur_qp));

        // ---- luma residual ----
        let mut q_blocks = [[0i32; 16]; 16];
        self.nnz_cache_load(mb_x, mb_y);
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = self.nc_pred(lbx, lby);
                let scan16 = decode_residual_block(r, 16, nc)?;
                q_blocks[lby * 4 + lbx] = un_scan_4x4_dcac(&scan16);
                scan16.iter().filter(|&&v| v != 0).count() as u8
            } else {
                0
            };
            self.nnz_cache_set(lbx, lby, total);
            self.nnz_y[by * w4 + bx] = total;
        }

        // ---- chroma residual ----
        let mut c_recon_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for (c, slot) in c_recon_dc.iter_mut().enumerate() {
                let dc = decode_residual_block(r, 4, -1)?;
                *slot = self.dequant_chroma_dc(&[dc[0], dc[1], dc[2], dc[3]], qpc, 4 + c);
            }
        }
        let mut c_q = [[[0i32; 16]; 4]; 2];
        if cbp_chroma == 2 {
            self.chroma_cache_load(mb_x, mb_y);
            let w2 = self.mb_w * 2;
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let nc = self.chroma_nc_pred(c, bx, by);
                    let ac = decode_residual_block(r, 15, nc)?;
                    let total = ac.iter().filter(|&&v| v != 0).count() as u8;
                    self.chroma_nnz_cache_set(c, bx, by, total);
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
                    un_scan_4x4_ac_into(&ac, &mut c_q[c][by * 2 + bx]);
                }
            }
        }

        // ---- reconstruction (prediction already built per partition) ----
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let mut predb = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    predb[dy * 4 + dx] = pred_y[(lby * 4 + dy) * 16 + (lbx * 4 + dx)] as i32;
                }
            }
            let deq = self.dequant(&q_blocks[lby * 4 + lbx], qp, 3);
            let s = reconstruct_4x4(&deq, &predb);
            store(&mut self.rec_y, self.cw, mb_x * 16 + lbx * 4, mb_y * 16 + lby * 4, &s);
        }
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut predb = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        predb[dy * 4 + dx] = c_pred[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    }
                }
                let mut deq = match &self.scaling {
                    Some(s) => dequantize_weighted(&c_q[c][by * 2 + bx], qpc, &s[4 + c]),
                    None => dequantize(&c_q[c][by * 2 + bx], qpc),
                };
                deq[0] = c_recon_dc[c][by * 2 + bx];
                let s = reconstruct_4x4(&deq, &predb);
                store(plane, self.ccw, mb_x * 8 + bx * 4, mb_y * 8 + by * 4, &s);
            }
        }

        // MV grid + coded flags were set per partition; mark modes as DC.
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // B-slice macroblock decoding
    // ---------------------------------------------------------------------

    /// Per-list (`list` 0 or 1) MV-prediction neighbors for the block region at
    /// `(pbx, pby)` of width `pwb` blocks — the L0/L1 analogue of
    /// `mv_neighbors_block`.
    fn mv_neighbors_list(&self, pbx: isize, pby: isize, pwb: isize, list: usize) -> [MvNeighbor; 3] {
        let (w4, h4) = ((self.mb_w * 4) as isize, (self.mb_h * 4) as isize);
        let (mvg, refg) = if list == 0 {
            (&self.mv_y, &self.ref_idx_y)
        } else {
            (&self.mv1, &self.ref_idx1)
        };
        let get = |bx: isize, by: isize| -> MvNeighbor {
            if bx < 0
                || by < 0
                || bx >= w4
                || by >= h4
                || !self.coded_y[(by * w4 + bx) as usize]
                || !self.nbr_in_slice(bx as usize / 4, by as usize / 4)
            {
                MvNeighbor::NONE
            } else {
                let idx = (by * w4 + bx) as usize;
                MvNeighbor { available: true, mv: mvg[idx], ref_idx: refg[idx] }
            }
        };
        let a = get(pbx - 1, pby);
        let b = get(pbx, pby - 1);
        let mut c = get(pbx + pwb, pby - 1);
        if !c.available {
            c = get(pbx - 1, pby - 1);
        }
        [a, b, c]
    }

    /// `colZeroFlag` for the 4×4 block at absolute block coords `(bx, by)`: true
    /// when `RefPicList1[0]` is a short-term picture whose co-located block uses
    /// reference 0 with a near-zero motion vector (spec §8.4.1.2.2).
    fn col_zero(&self, bx: usize, by: usize) -> bool {
        let Some(col) = self.refs1.first() else { return false };
        if col.long_term || col.w4 == 0 {
            return false;
        }
        let idx = by * col.w4 + bx;
        if idx >= col.ref_idx.len() {
            return false;
        }
        col.ref_idx[idx] == 0 && col.mv[idx].0.abs() <= 1 && col.mv[idx].1.abs() <= 1
    }

    /// Motion-compensates a region with the given per-list refs/MVs (bi-prediction
    /// = the simple `(a+b+1)>>1` average), writing into `pred_y`/`c_pred`.
    #[allow(clippy::too_many_arguments)]
    fn b_mc(
        &self,
        mb_x: usize,
        mb_y: usize,
        px: usize,
        py: usize,
        rw: usize,
        rh: usize,
        refi0: i32,
        mv0: (i32, i32),
        refi1: i32,
        mv1: (i32, i32),
        pred_y: &mut [u8; 256],
        c_pred: &mut [[u8; 64]; 2],
    ) {
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let (mut a, mut b) = ([0u8; 256], [0u8; 256]);
        if refi0 >= 0 {
            mc_luma(&self.refs[refi0 as usize].y, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut a);
        }
        if refi1 >= 0 {
            mc_luma(&self.refs1[refi1 as usize].y, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut b);
        }
        for dy in 0..rh {
            for dx in 0..rw {
                let (p, q) = (a[dy * rw + dx] as u16, b[dy * rw + dx] as u16);
                pred_y[(py + dy) * 16 + (px + dx)] = if refi0 >= 0 && refi1 >= 0 {
                    ((p + q + 1) >> 1) as u8
                } else if refi0 >= 0 {
                    p as u8
                } else {
                    q as u8
                };
            }
        }
        let (crx, cry, crw, crh) = (px / 2, py / 2, rw / 2, rh / 2);
        for c in 0..2 {
            let (mut ca, mut cb) = ([0u8; 64], [0u8; 64]);
            if refi0 >= 0 {
                let rf = &self.refs[refi0 as usize];
                let pl = if c == 0 { &rf.u } else { &rf.v };
                mc_chroma(pl, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut ca);
            }
            if refi1 >= 0 {
                let rf = &self.refs1[refi1 as usize];
                let pl = if c == 0 { &rf.u } else { &rf.v };
                mc_chroma(pl, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut cb);
            }
            for dy in 0..crh {
                for dx in 0..crw {
                    let (p, q) = (ca[dy * crw + dx] as u16, cb[dy * crw + dx] as u16);
                    c_pred[c][(cry + dy) * 8 + (crx + dx)] = if refi0 >= 0 && refi1 >= 0 {
                        ((p + q + 1) >> 1) as u8
                    } else if refi0 >= 0 {
                        p as u8
                    } else {
                        q as u8
                    };
                }
            }
        }
    }

    /// Commits a region's per-list motion to the 4×4 grids (and marks coded).
    #[allow(clippy::too_many_arguments)]
    fn b_set_motion(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, refi0: i32, mv0: (i32, i32), refi1: i32, mv1: (i32, i32)) {
        let w4 = self.mb_w * 4;
        for by in py / 4..(py + rh) / 4 {
            for bx in px / 4..(px + rw) / 4 {
                let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                self.ref_idx_y[idx] = refi0;
                self.mv_y[idx] = if refi0 >= 0 { mv0 } else { (0, 0) };
                self.ref_idx1[idx] = refi1;
                self.mv1[idx] = if refi1 >= 0 { mv1 } else { (0, 0) };
                self.inter_y[idx] = true;
                self.coded_y[idx] = true;
                self.modes_y[idx] = 2;
            }
        }
    }

    /// Spatial direct prediction for a region (whole MB or an 8×8): derives the
    /// per-list reference indices and base MVs, then motion-compensates each 4×4
    /// sub-block (applying `colZeroFlag`) and commits the motion (spec §8.4.1.2.2).
    fn decode_b_direct(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, pred_y: &mut [u8; 256], c_pred: &mut [[u8; 64]; 2]) {
        // MB-level neighbors drive the direct reference indices and base MVs.
        let (nbx, nby) = ((mb_x * 4) as isize, (mb_y * 4) as isize);
        let n0 = self.mv_neighbors_list(nbx, nby, 4, 0);
        let n1 = self.mv_neighbors_list(nbx, nby, 4, 1);
        let min_pos = |a: i32, b: i32| if a < 0 { b } else if b < 0 { a } else { a.min(b) };
        let rid = |n: &[MvNeighbor; 3]| min_pos(min_pos(n[0].ref_idx, n[1].ref_idx), n[2].ref_idx);
        let (mut refi0, mut refi1) = (rid(&n0), rid(&n1));
        let direct_zero = refi0 < 0 && refi1 < 0;
        if direct_zero {
            refi0 = 0;
            refi1 = 0;
        }
        let mv0 = if refi0 >= 0 && !direct_zero { predict_mv(n0[0], n0[1], n0[2], refi0) } else { (0, 0) };
        let mv1 = if refi1 >= 0 && !direct_zero { predict_mv(n1[0], n1[1], n1[2], refi1) } else { (0, 0) };
        // Per 4×4 sub-block: colZeroFlag zeroes the ref-0 motion vector.
        for sby in py / 4..(py + rh) / 4 {
            for sbx in px / 4..(px + rw) / 4 {
                let cz = !direct_zero && self.col_zero(mb_x * 4 + sbx, mb_y * 4 + sby);
                let m0 = if refi0 == 0 && cz { (0, 0) } else { mv0 };
                let m1 = if refi1 == 0 && cz { (0, 0) } else { mv1 };
                self.b_mc(mb_x, mb_y, sbx * 4, sby * 4, 4, 4, refi0, m0, refi1, m1, pred_y, c_pred);
                self.b_set_motion(mb_x, mb_y, sbx * 4, sby * 4, 4, 4, refi0, m0, refi1, m1);
            }
        }
    }

    /// Reads `ref_idx_lX` for a B partition (te(v)/ue(v) by the list's active
    /// count), bounds-checked against the available reference count.
    fn read_b_ref(&self, r: &mut BitReader, list: usize) -> Result<i32, MbError> {
        let (active, avail) = if list == 0 {
            (self.num_ref_active, self.refs.len())
        } else {
            (self.num_ref_active1, self.refs1.len())
        };
        let v = if active > 1 { read_ref_idx(r, active)? } else { 0 };
        if v as usize >= avail {
            return Err(MbError::Truncated);
        }
        Ok(v)
    }

    /// Reconstructs a `B_Skip` macroblock: spatial-direct prediction, no residual.
    fn decode_b_skip(&mut self, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        if self.refs.is_empty() || self.refs1.is_empty() {
            return Err(MbError::Unsupported("B without references"));
        }
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        self.decode_b_direct(mb_x, mb_y, 0, 0, 16, 16, &mut pred_y, &mut c_pred);
        // Reconstruct with a zero residual.
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let mut predb = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    predb[dy * 4 + dx] = pred_y[(lby * 4 + dy) * 16 + (lbx * 4 + dx)] as i32;
                }
            }
            let s = reconstruct_4x4(&[0; 16], &predb);
            store(&mut self.rec_y, self.cw, mb_x * 16 + lbx * 4, mb_y * 16 + lby * 4, &s);
        }
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                for dy in 0..4 {
                    for dx in 0..4 {
                        let v = c_pred[c][(by * 4 + dy) * 8 + (bx * 4 + dx)];
                        plane[(mb_y * 8 + by * 4 + dy) * self.ccw + (mb_x * 8 + bx * 4 + dx)] = v;
                    }
                }
            }
        }
        // nnz stays 0 (no residual) — clear the grids for neighbor context.
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
        }
        Ok(())
    }

    /// Reconstructs a B macroblock (spec Table 7-14): direct, L0/L1/Bi partitions,
    /// `B_8x8`, or intra.
    fn decode_b_mb(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let mb_type = r.read_ue()?;
        if mb_type >= 23 {
            return self.decode_intra_mb(r, mb_x, mb_y, mb_type - 23);
        }
        if self.refs.is_empty() || self.refs1.is_empty() {
            return Err(MbError::Unsupported("B without references"));
        }
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];

        if mb_type == 0 {
            // B_Direct_16x16
            self.decode_b_direct(mb_x, mb_y, 0, 0, 16, 16, &mut pred_y, &mut c_pred);
            return self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred);
        }
        if mb_type == 22 {
            return self.decode_b_8x8(r, mb_x, mb_y);
        }

        // 16x16 / 16x8 / 8x16 partitions with per-partition L0/L1/Bi.
        let (layout, mvmode, preds) = b_inter_layout(mb_type);
        // mb_pred order: ref_idx_l0 (all L0 parts), ref_idx_l1, mvd_l0, mvd_l1.
        let mut refi = [[-1i32; 2]; 2]; // [part][list]
        for (p, &(_, _, _, _)) in layout.iter().enumerate() {
            if preds[p].uses(0) {
                refi[p][0] = self.read_b_ref(r, 0)?;
            }
        }
        for (p, _) in layout.iter().enumerate() {
            if preds[p].uses(1) {
                refi[p][1] = self.read_b_ref(r, 1)?;
            }
        }
        let mut mvd = [[(0i32, 0i32); 2]; 2];
        for (p, _) in layout.iter().enumerate() {
            if preds[p].uses(0) {
                mvd[p][0] = (r.read_se()?, r.read_se()?);
            }
        }
        for (p, _) in layout.iter().enumerate() {
            if preds[p].uses(1) {
                mvd[p][1] = (r.read_se()?, r.read_se()?);
            }
        }
        // Per partition: predict + commit each list's MV, then motion-compensate.
        for (p, &(rx, ry, rw, rh)) in layout.iter().enumerate() {
            let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
            let pwb = (rw / 4) as isize;
            let mut mv = [(0i32, 0i32); 2];
            for list in 0..2 {
                if refi[p][list] >= 0 {
                    let n = self.mv_neighbors_list(pbx, pby, pwb, list);
                    let pmv = predict_partition_mv(mvmode, p, n[0], n[1], n[2], refi[p][list]);
                    mv[list] = (pmv.0 + mvd[p][list].0, pmv.1 + mvd[p][list].1);
                }
            }
            self.b_set_motion(mb_x, mb_y, rx, ry, rw, rh, refi[p][0], mv[0], refi[p][1], mv[1]);
            self.b_mc(mb_x, mb_y, rx, ry, rw, rh, refi[p][0], mv[0], refi[p][1], mv[1], &mut pred_y, &mut c_pred);
        }
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred)
    }

    /// Reconstructs a `B_8x8` macroblock: four 8×8 sub-macroblock partitions, each
    /// direct or L0/L1/Bi with its own sub-partitioning (spec Table 7-18).
    fn decode_b_8x8(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let mut sub = [0u32; 4];
        for s in sub.iter_mut() {
            let v = r.read_ue()?;
            if v > 12 {
                return Err(MbError::Unsupported("invalid B sub_mb_type"));
            }
            *s = v;
        }
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        // ref_idx for all 8×8 partitions (L0 batch, then L1 batch), for the
        // non-direct sub-partitions.
        let mut refi = [[-1i32; 2]; 4];
        for (p, &st) in sub.iter().enumerate() {
            if st != 0 && b_sub_uses(st, 0) {
                refi[p][0] = self.read_b_ref(r, 0)?;
            }
        }
        for (p, &st) in sub.iter().enumerate() {
            if st != 0 && b_sub_uses(st, 1) {
                refi[p][1] = self.read_b_ref(r, 1)?;
            }
        }
        // mvd: all mvd_l0 (partition-major, sub-partition order), then all mvd_l1.
        let mut mvd0: Vec<(i32, i32)> = Vec::new();
        let mut mvd1: Vec<(i32, i32)> = Vec::new();
        for &st in &sub {
            if st != 0 && b_sub_uses(st, 0) {
                for _ in b_sub_parts(st) {
                    mvd0.push((r.read_se()?, r.read_se()?));
                }
            }
        }
        for &st in &sub {
            if st != 0 && b_sub_uses(st, 1) {
                for _ in b_sub_parts(st) {
                    mvd1.push((r.read_se()?, r.read_se()?));
                }
            }
        }
        // Decode each 8×8 partition.
        let (mut i0, mut i1) = (0usize, 0usize);
        for (p, &st) in sub.iter().enumerate() {
            let (b8x, b8y) = ((p % 2) * 8, (p / 2) * 8);
            if st == 0 {
                self.decode_b_direct(mb_x, mb_y, b8x, b8y, 8, 8, &mut pred_y, &mut c_pred);
                continue;
            }
            for &(sx, sy, sw, sh) in b_sub_parts(st) {
                let (px, py) = (b8x + sx, b8y + sy);
                let (pbx, pby) = ((mb_x * 4 + px / 4) as isize, (mb_y * 4 + py / 4) as isize);
                let pwb = (sw / 4) as isize;
                let mut mv = [(0i32, 0i32); 2];
                if b_sub_uses(st, 0) {
                    let n = self.mv_neighbors_list(pbx, pby, pwb, 0);
                    let pmv = predict_mv(n[0], n[1], n[2], refi[p][0]);
                    let d = mvd0[i0];
                    i0 += 1;
                    mv[0] = (pmv.0 + d.0, pmv.1 + d.1);
                }
                if b_sub_uses(st, 1) {
                    let n = self.mv_neighbors_list(pbx, pby, pwb, 1);
                    let pmv = predict_mv(n[0], n[1], n[2], refi[p][1]);
                    let d = mvd1[i1];
                    i1 += 1;
                    mv[1] = (pmv.0 + d.0, pmv.1 + d.1);
                }
                self.b_set_motion(mb_x, mb_y, px, py, sw, sh, refi[p][0], mv[0], refi[p][1], mv[1]);
                self.b_mc(mb_x, mb_y, px, py, sw, sh, refi[p][0], mv[0], refi[p][1], mv[1], &mut pred_y, &mut c_pred);
            }
        }
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred)
    }

    /// Reconstructs a `P_8x8` macroblock: four 8×8 sub-macroblock partitions,
    /// each independently split (8×8 / 8×4 / 4×8 / 4×4) with its own motion
    /// vector(s). `ref0` is `P_8x8ref0` (every `ref_idx` forced to 0, not coded).
    fn decode_p8x8(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        ref0: bool,
    ) -> Result<(), MbError> {
        if self.refs.is_empty() {
            return Err(MbError::Unsupported("inter without reference"));
        }
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let num_refs = self.refs.len();

        // mb_pred order (spec §7.3.5.2): all sub_mb_type, then all ref_idx_l0,
        // then all mvd_l0 (partition-major, sub-partition order within each).
        let mut sub_types = [0u32; 4];
        for st in sub_types.iter_mut() {
            let v = r.read_ue()?;
            if v > 3 {
                return Err(MbError::Unsupported("B-slice / invalid sub_mb_type"));
            }
            *st = v;
        }
        let mut ref_idxs = [0i32; 4];
        if self.num_ref_active > 1 && !ref0 {
            for ri in ref_idxs.iter_mut() {
                *ri = read_ref_idx(r, self.num_ref_active)?;
                if *ri as usize >= num_refs {
                    return Err(MbError::Truncated); // references a non-existent picture
                }
            }
        }

        // Per sub-partition (in decoding order): median MV prediction from the
        // committed neighbor grid, mvd, commit, then motion-compensate. Committing
        // before the next prediction is what lets sub-partitions chain correctly.
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        for part in 0..4usize {
            let refi = ref_idxs[part];
            let (b8x, b8y) = ((part % 2) * 8, (part / 2) * 8);
            for &(srx, sry, srw, srh) in sub_mb_partitions(sub_types[part]) {
                let (px, py) = (b8x + srx, b8y + sry);
                let (pbx, pby) = ((mb_x * 4 + px / 4) as isize, (mb_y * 4 + py / 4) as isize);
                let [a, b, c] = self.mv_neighbors_block(pbx, pby, (srw / 4) as isize);
                let pmv = predict_mv(a, b, c, refi);
                let mvd_x = r.read_se()?;
                let mvd_y = r.read_se()?;
                let mv = (pmv.0 + mvd_x, pmv.1 + mvd_y);
                for by in py / 4..py / 4 + srh / 4 {
                    for bx in px / 4..px / 4 + srw / 4 {
                        let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                        self.mv_y[idx] = mv;
                        self.inter_y[idx] = true;
                        self.ref_idx_y[idx] = refi;
                        self.coded_y[idx] = true;
                    }
                }
                let reference = &self.refs[refi as usize];
                let mut tmp = [0u8; 256];
                mc_luma(&reference.y, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, srw, srh, mv.0, mv.1, &mut tmp);
                for dy in 0..srh {
                    for dx in 0..srw {
                        pred_y[(py + dy) * 16 + (px + dx)] = tmp[dy * srw + dx];
                    }
                }
                let (crx, cry, crw, crh) = (px / 2, py / 2, srw / 2, srh / 2);
                for cc in 0..2 {
                    let rc = if cc == 0 { &reference.u } else { &reference.v };
                    let mut tc = [0u8; 64];
                    mc_chroma(rc, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv.0, mv.1, &mut tc);
                    for dy in 0..crh {
                        for dx in 0..crw {
                            c_pred[cc][(cry + dy) * 8 + (crx + dx)] = tc[dy * crw + dx];
                        }
                    }
                }
            }
        }

        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred)
    }

    /// Reconstructs a `P_Skip` macroblock: motion-compensate from the reference
    /// at the skip MV, with no residual.
    fn decode_p_skip(&mut self, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        // P_Skip always references index 0 (the most recent picture).
        let reference = self
            .refs
            .first()
            .cloned()
            .ok_or(MbError::Unsupported("P_Skip without reference"))?;
        let mv = self.skip_mv(mb_x, mb_y);
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        let mut pred = [0u8; 256];
        mc_luma(&reference.y, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred);
        for dy in 0..16 {
            for dx in 0..16 {
                self.rec_y[(mb_y * 16 + dy) * self.cw + (mb_x * 16 + dx)] = pred[dy * 16 + dx];
            }
        }
        for c in 0..2 {
            let mut pc = [0u8; 64];
            let rc = if c == 0 { &reference.u } else { &reference.v };
            mc_chroma(rc, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut pc);
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for dy in 0..8 {
                for dx in 0..8 {
                    plane[(mb_y * 8 + dy) * self.ccw + (mb_x * 8 + dx)] = pc[dy * 8 + dx];
                }
            }
        }
        self.set_mb_mv(mb_x, mb_y, mv, true, 0);
        // Mark blocks coded; inter blocks count as DC (not I_4x4) for mode pred.
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
        Ok(())
    }

    /// Predicted `Intra_4x4` mode for the block at absolute coords `(bx, by)`.
    /// If either the left or top neighbor is outside the frame or in another
    /// slice, the prediction is DC (mode 2) (spec §8.3.1.1).
    fn predict_i4_mode(&self, bx: usize, by: usize) -> u8 {
        if bx == 0 || by == 0 {
            return 2;
        }
        // Left neighbor block (bx-1,by); top neighbor block (bx,by-1). A neighbor
        // in another slice — or, under constrained_intra, an inter neighbor — is
        // unavailable, forcing the predicted mode to DC.
        if !self.nbr_in_slice((bx - 1) / 4, by / 4)
            || !self.nbr_in_slice(bx / 4, (by - 1) / 4)
            || !self.intra_nbr_ok(bx - 1, by)
            || !self.intra_nbr_ok(bx, by - 1)
        {
            return 2;
        }
        let w4 = self.mb_w * 4;
        self.modes_y[by * w4 + (bx - 1)].min(self.modes_y[(by - 1) * w4 + bx])
    }

    /// Gathers 4×4 luma intra neighbors at pixel `(px, py)` from `rec_y`.
    fn gather_i4(
        &self,
        px: usize,
        py: usize,
        avail_top: bool,
        avail_left: bool,
        bx: usize,
        by: usize,
    ) -> ([u8; 8], [u8; 4], u8) {
        let (cw, w4) = (self.cw, self.mb_w * 4);
        let mut top = [0u8; 8];
        let mut left = [0u8; 4];
        let mut corner = 0;
        if avail_top {
            for i in 0..4 {
                top[i] = self.rec_y[(py - 1) * cw + px + i];
            }
            let tr_avail = bx + 1 < w4
                && self.coded_y[(by - 1) * w4 + (bx + 1)]
                && self.nbr_in_slice((bx + 1) / 4, (by - 1) / 4)
                && self.intra_nbr_ok(bx + 1, by - 1);
            for i in 0..4 {
                top[4 + i] = if tr_avail {
                    self.rec_y[(py - 1) * cw + px + 4 + i]
                } else {
                    top[3]
                };
            }
        }
        if avail_left {
            for i in 0..4 {
                left[i] = self.rec_y[(py + i) * cw + px - 1];
            }
        }
        // The above-left corner has its own availability (block D); under
        // constrained_intra it is gone if that block is inter.
        if avail_top && avail_left && self.intra_nbr_ok(bx - 1, by - 1) {
            corner = self.rec_y[(py - 1) * cw + px - 1];
        }
        (top, left, corner)
    }

    /// Reconstructs an `I_PCM` macroblock: byte-aligned raw 8-bit samples, no
    /// prediction/transform/quant (spec §7.3.5, §8.3.5).
    fn decode_ipcm(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        r.align_to_byte()?;
        let (lx, ly) = (mb_x * 16, mb_y * 16);
        for dy in 0..16 {
            for dx in 0..16 {
                self.rec_y[(ly + dy) * self.cw + (lx + dx)] = r.read_bits(8)? as u8;
            }
        }
        let (cx, cy) = (mb_x * 8, mb_y * 8);
        for plane in [&mut self.rec_u, &mut self.rec_v] {
            for dy in 0..8 {
                for dx in 0..8 {
                    plane[(cy + dy) * self.ccw + (cx + dx)] = r.read_bits(8)? as u8;
                }
            }
        }
        // Neighbor context: an I_PCM block contributes TotalCoeff = 16, counts as
        // intra with DC mode for prediction, and has no motion (§9.2.1, §8.3.1.2.2).
        let (w4, w2) = (self.mb_w * 4, self.mb_w * 2);
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let idx = (mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx);
            self.nnz_y[idx] = 16;
            self.modes_y[idx] = 2;
            self.inter_y[idx] = false;
            self.ref_idx_y[idx] = -1;
            self.mv_y[idx] = (0, 0);
        }
        for c in 0..2 {
            for by in 0..2 {
                for bx in 0..2 {
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = 16;
                }
            }
        }
        Ok(())
    }

    fn decode_i4x4(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let w4 = self.mb_w * 4;

        // intra4x4 mode signalling
        let mut modes = [2u8; 16]; // raster [lby*4+lbx]
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let predicted = self.predict_i4_mode(bx, by);
            let actual = if r.read_bit()? {
                predicted
            } else {
                let rem = r.read_bits(3)? as u8;
                if rem < predicted {
                    rem
                } else {
                    rem + 1
                }
            };
            self.modes_y[by * w4 + bx] = actual;
            modes[lby * 4 + lbx] = actual;
        }

        let chroma_mode = r.read_ue()? as u8;
        let cbp = read_cbp_intra(r)?;
        let cbp_luma = cbp & 15;
        let cbp_chroma = cbp >> 4;
        if cbp != 0 {
            self.step_qp(r.read_se()?);
        }
        let qp = self.cur_qp;

        // luma residuals + serial reconstruction. Cross-MB neighbors are only
        // available when the adjacent macroblock is in this slice (and, under
        // constrained_intra_pred, is itself intra-coded).
        let top_mb_avail = mb_y > 0
            && self.nbr_in_slice(mb_x, mb_y - 1)
            && self.intra_nbr_ok(mb_x * 4, mb_y * 4 - 1);
        let left_mb_avail = mb_x > 0
            && self.nbr_in_slice(mb_x - 1, mb_y)
            && self.intra_nbr_ok(mb_x * 4 - 1, mb_y * 4);
        self.nnz_cache_load(mb_x, mb_y);
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let (px, py) = (bx * 4, by * 4);
            let avail_top = lby > 0 || top_mb_avail;
            let avail_left = lbx > 0 || left_mb_avail;
            let mut qb = [0i32; 16];
            let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = self.nc_pred(lbx, lby);
                let scan16 = decode_residual_block(r, 16, nc)?;
                qb = un_scan_4x4_dcac(&scan16);
                scan16.iter().filter(|&&v| v != 0).count() as u8
            } else {
                0
            };
            self.nnz_cache_set(lbx, lby, total);
            self.nnz_y[by * w4 + bx] = total;
            let (top, left, corner) = self.gather_i4(px, py, avail_top, avail_left, bx, by);
            let pred = intra4x4_pred(modes[lby * 4 + lbx], avail_top, avail_left, &top, &left, corner);
            let mut predb = [0i32; 16];
            for i in 0..16 {
                predb[i] = pred[i] as i32;
            }
            let s = reconstruct_4x4(&self.dequant(&qb, qp, 0), &predb);
            store(&mut self.rec_y, self.cw, px, py, &s);
            self.coded_y[by * w4 + bx] = true;
        }

        self.decode_chroma(r, mb_x, mb_y, cbp_chroma, chroma_mode)
    }

    fn decode_i16(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        mt: u32,
    ) -> Result<(), MbError> {
        let pred_mode = I16Mode::from_id(mt % 4);
        let cbp_chroma = (mt % 12) / 4;
        let cbp_luma_15 = mt / 12 == 1;
        let chroma_mode = r.read_ue()? as u8;
        self.step_qp(r.read_se()?);
        let qp = self.cur_qp;
        let w4 = self.mb_w * 4;

        // luma DC
        self.nnz_cache_load(mb_x, mb_y);
        let nc_dc = self.nc_pred(0, 0);
        let dc_scan = decode_residual_block(r, 16, nc_dc)?;
        let dc_levels = un_scan_4x4_dcac(&dc_scan);
        let recon_dc = self.dequant_luma_dc(&dc_levels, qp, 0);

        // luma AC (nnz set for all 16 blocks: 0 when DC-only, matching the encoder)
        let mut q_blocks = [[0i32; 16]; 16];
        for &(bx, by) in &LUMA_4X4_SCAN_XY {
            let total = if cbp_luma_15 {
                let nc = self.nc_pred(bx, by);
                let ac = decode_residual_block(r, 15, nc)?;
                un_scan_4x4_ac_into(&ac, &mut q_blocks[by * 4 + bx]);
                ac.iter().filter(|&&v| v != 0).count() as u8
            } else {
                0
            };
            self.nnz_cache_set(bx, by, total);
            self.nnz_y[(mb_y * 4 + by) * w4 + (mb_x * 4 + bx)] = total;
        }

        // prediction + reconstruction
        let avail_top = mb_y > 0
            && self.nbr_in_slice(mb_x, mb_y - 1)
            && self.intra_nbr_ok(mb_x * 4, mb_y * 4 - 1);
        let avail_left = mb_x > 0
            && self.nbr_in_slice(mb_x - 1, mb_y)
            && self.intra_nbr_ok(mb_x * 4 - 1, mb_y * 4);
        let (lx, ly) = (mb_x * 16, mb_y * 16);
        let mut top = [0u8; 16];
        let mut left = [0u8; 16];
        if avail_top {
            for i in 0..16 {
                top[i] = self.rec_y[(ly - 1) * self.cw + lx + i];
            }
        }
        if avail_left {
            for i in 0..16 {
                left[i] = self.rec_y[(ly + i) * self.cw + lx - 1];
            }
        }
        let corner = if avail_top && avail_left {
            self.rec_y[(ly - 1) * self.cw + lx - 1]
        } else {
            0
        };
        let pred_l = luma16x16_pred(pred_mode, avail_top, avail_left, &top, &left, corner);
        for by in 0..4 {
            for bx in 0..4 {
                let mut deq = self.dequant(&q_blocks[by * 4 + bx], qp, 0);
                deq[0] = recon_dc[by * 4 + bx];
                let mut predb = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        predb[dy * 4 + dx] = pred_l[(by * 4 + dy) * 16 + (bx * 4 + dx)] as i32;
                    }
                }
                let s = reconstruct_4x4(&deq, &predb);
                store(&mut self.rec_y, self.cw, lx + bx * 4, ly + by * 4, &s);
            }
        }
        // I_16x16 blocks are treated as DC for neighbor mode prediction.
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }

        self.decode_chroma(r, mb_x, mb_y, cbp_chroma, chroma_mode)
    }

    /// Reads and reconstructs the chroma residual (shared by both luma types).
    fn decode_chroma(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        cbp_chroma: u32,
        chroma_mode: u8,
    ) -> Result<(), MbError> {
        let qpc = self.chroma_qp_for(self.cur_qp);
        let (cx, cy) = (mb_x * 8, mb_y * 8);
        let avail_top = mb_y > 0
            && self.nbr_in_slice(mb_x, mb_y - 1)
            && self.intra_nbr_ok(mb_x * 4, mb_y * 4 - 1);
        let avail_left = mb_x > 0
            && self.nbr_in_slice(mb_x - 1, mb_y)
            && self.intra_nbr_ok(mb_x * 4 - 1, mb_y * 4);

        let mut c_recon_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for (c, slot) in c_recon_dc.iter_mut().enumerate() {
                let dc = decode_residual_block(r, 4, -1)?;
                *slot = self.dequant_chroma_dc(&[dc[0], dc[1], dc[2], dc[3]], qpc, 1 + c);
            }
        }
        let mut c_q_blocks = [[[0i32; 16]; 4]; 2];
        if cbp_chroma == 2 {
            self.chroma_cache_load(mb_x, mb_y);
            let w2 = self.mb_w * 2;
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let nc = self.chroma_nc_pred(c, bx, by);
                    let ac = decode_residual_block(r, 15, nc)?;
                    let total = ac.iter().filter(|&&v| v != 0).count() as u8;
                    self.chroma_nnz_cache_set(c, bx, by, total);
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
                    un_scan_4x4_ac_into(&ac, &mut c_q_blocks[c][by * 2 + bx]);
                }
            }
        }
        for c in 0..2 {
            let mut ctop = [0u8; 8];
            let mut cleft = [0u8; 8];
            let mut ccorner = 0u8;
            {
                let rec_c = if c == 0 { &self.rec_u } else { &self.rec_v };
                if avail_top {
                    for i in 0..8 {
                        ctop[i] = rec_c[(cy - 1) * self.ccw + cx + i];
                    }
                }
                if avail_left {
                    for i in 0..8 {
                        cleft[i] = rec_c[(cy + i) * self.ccw + cx - 1];
                    }
                }
                if avail_top && avail_left {
                    ccorner = rec_c[(cy - 1) * self.ccw + cx - 1];
                }
            }
            let pred8 = chroma8x8_pred(chroma_mode, avail_top, avail_left, &ctop, &cleft, ccorner);
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut predb = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        predb[dy * 4 + dx] = pred8[(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    }
                }
                let mut deq = self.dequant(&c_q_blocks[c][by * 2 + bx], qpc, 1 + c);
                deq[0] = c_recon_dc[c][by * 2 + bx];
                let s = reconstruct_4x4(&deq, &predb);
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                store(plane, self.ccw, cx + bx * 4, cy + by * 4, &s);
            }
        }
        Ok(())
    }

    /// Applies the in-loop deblocking filter to the reconstructed frame, with
    /// the slice's `FilterOffsetA`/`FilterOffsetB` (each = the coded `*_div2`
    /// value × 2).
    pub fn deblock(&mut self, offset_a: i32, offset_b: i32) {
        let intra: Vec<bool> = self.inter_y.iter().map(|&i| !i).collect();
        // Map per-block reference indices to a stable picture identity (POC) so
        // the boundary-strength comparison recognises the same picture across lists.
        let ref_id: Vec<i32> = self
            .ref_idx_y
            .iter()
            .map(|&r| if r >= 0 { self.refs.get(r as usize).map_or(i32::MIN, |f| f.poc) } else { i32::MIN })
            .collect();
        let ref_id1: Vec<i32> = self
            .ref_idx1
            .iter()
            .map(|&r| if r >= 0 { self.refs1.get(r as usize).map_or(i32::MIN, |f| f.poc) } else { i32::MIN })
            .collect();
        let info = rusty_h264_common::deblock::BlockInfo {
            intra: &intra,
            nnz: &self.nnz_y,
            mv: &self.mv_y,
            ref_id: &ref_id,
            mv1: &self.mv1,
            ref_id1: &ref_id1,
            w4: self.mb_w * 4,
        };
        rusty_h264_common::deblock::filter_frame(
            &mut self.rec_y,
            &mut self.rec_u,
            &mut self.rec_v,
            self.mb_w,
            self.mb_h,
            &self.mb_qp,
            self.chroma_qp_offset,
            offset_a,
            offset_b,
            &info,
        );
    }

    /// Crops the reconstructed coded-size planes to the display window.
    pub fn into_frame(self, crop_r: usize, crop_b: usize) -> YuvFrame {
        let dw = self.cw - 2 * crop_r;
        let dh = self.ch - 2 * crop_b;
        let mut y = vec![0u8; dw * dh];
        for row in 0..dh {
            y[row * dw..row * dw + dw].copy_from_slice(&self.rec_y[row * self.cw..row * self.cw + dw]);
        }
        let (cdw, cdh) = (dw / 2, dh / 2);
        let mut u = vec![0u8; cdw * cdh];
        let mut v = vec![0u8; cdw * cdh];
        for row in 0..cdh {
            u[row * cdw..row * cdw + cdw]
                .copy_from_slice(&self.rec_u[row * self.ccw..row * self.ccw + cdw]);
            v[row * cdw..row * cdw + cdw]
                .copy_from_slice(&self.rec_v[row * self.ccw..row * self.ccw + cdw]);
        }
        let _ = self.cch;
        YuvFrame {
            width: dw,
            height: dh,
            y,
            u,
            v,
        }
    }
}

/// Reads `ref_idx_l0` as `te(v)` with range `num_ref_active - 1`: a single flag
/// when exactly two references are active (cMax == 1), else `ue(v)`.
fn read_ref_idx(r: &mut BitReader, num_ref_active: usize) -> Result<i32, OutOfData> {
    if num_ref_active == 2 {
        Ok(if r.read_bit()? { 0 } else { 1 }) // te(v): value = !bit
    } else {
        Ok(r.read_ue()? as i32)
    }
}

/// B-partition prediction direction.
#[derive(Clone, Copy, PartialEq)]
enum BPred {
    L0,
    L1,
    Bi,
}
impl BPred {
    /// Whether this direction uses reference list `list` (0 or 1).
    fn uses(self, list: usize) -> bool {
        matches!(
            (self, list),
            (BPred::L0, 0) | (BPred::L1, 1) | (BPred::Bi, 0) | (BPred::Bi, 1)
        )
    }
}

const B16X16: &[(usize, usize, usize, usize)] = &[(0, 0, 16, 16)];
const B16X8: &[(usize, usize, usize, usize)] = &[(0, 0, 16, 8), (0, 8, 16, 8)];
const B8X16: &[(usize, usize, usize, usize)] = &[(0, 0, 8, 16), (8, 0, 8, 16)];

/// A partition region `(x, y, w, h)` in samples.
type Region = (usize, usize, usize, usize);

/// B `mb_type` 1..=21 → (partition layout, MV-prediction mode 0/1/2 for 16×16/
/// 16×8/8×16, per-partition prediction direction) (spec Table 7-14).
fn b_inter_layout(mb_type: u32) -> (&'static [Region], u8, [BPred; 2]) {
    use BPred::*;
    match mb_type {
        1 => (B16X16, 0, [L0, L0]),
        2 => (B16X16, 0, [L1, L1]),
        3 => (B16X16, 0, [Bi, Bi]),
        4 => (B16X8, 1, [L0, L0]),
        5 => (B8X16, 2, [L0, L0]),
        6 => (B16X8, 1, [L1, L1]),
        7 => (B8X16, 2, [L1, L1]),
        8 => (B16X8, 1, [L0, L1]),
        9 => (B8X16, 2, [L0, L1]),
        10 => (B16X8, 1, [L1, L0]),
        11 => (B8X16, 2, [L1, L0]),
        12 => (B16X8, 1, [L0, Bi]),
        13 => (B8X16, 2, [L0, Bi]),
        14 => (B16X8, 1, [L1, Bi]),
        15 => (B8X16, 2, [L1, Bi]),
        16 => (B16X8, 1, [Bi, L0]),
        17 => (B8X16, 2, [Bi, L0]),
        18 => (B16X8, 1, [Bi, L1]),
        19 => (B8X16, 2, [Bi, L1]),
        20 => (B16X8, 1, [Bi, Bi]),
        _ => (B8X16, 2, [Bi, Bi]), // 21
    }
}

/// Whether a B `sub_mb_type` (1..=12) uses reference list `list`.
fn b_sub_uses(st: u32, list: usize) -> bool {
    let pred = match st {
        1 | 4 | 5 | 10 => 0,  // L0
        2 | 6 | 7 | 11 => 1,  // L1
        _ => 2,               // Bi (3, 8, 9, 12)
    };
    (list == 0 && pred != 1) || (list == 1 && pred != 0)
}

/// Sub-partition shapes within an 8×8 for a B `sub_mb_type` (1..=12).
fn b_sub_parts(st: u32) -> &'static [(usize, usize, usize, usize)] {
    match st {
        1..=3 => &[(0, 0, 8, 8)],
        4 | 6 | 8 => &[(0, 0, 8, 4), (0, 4, 8, 4)],
        5 | 7 | 9 => &[(0, 0, 4, 8), (4, 0, 4, 8)],
        _ => &[(0, 0, 4, 4), (4, 0, 4, 4), (0, 4, 4, 4), (4, 4, 4, 4)], // 10/11/12
    }
}

/// Sub-macroblock partition layout `(x, y, w, h)` in samples within an 8×8, for
/// a P-slice `sub_mb_type` (0 = 8×8, 1 = 8×4, 2 = 4×8, 3 = 4×4).
fn sub_mb_partitions(sub_type: u32) -> &'static [(usize, usize, usize, usize)] {
    match sub_type {
        0 => &[(0, 0, 8, 8)],
        1 => &[(0, 0, 8, 4), (0, 4, 8, 4)],
        2 => &[(0, 0, 4, 8), (4, 0, 4, 8)],
        _ => &[(0, 0, 4, 4), (4, 0, 4, 4), (0, 4, 4, 4), (4, 4, 4, 4)],
    }
}

fn store(plane: &mut [u8], stride: usize, x0: usize, y0: usize, s: &[u8; 16]) {
    for dy in 0..4 {
        for dx in 0..4 {
            plane[(y0 + dy) * stride + (x0 + dx)] = s[dy * 4 + dx];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(qp: u8, offset: i32) -> FrameDecoder {
        FrameDecoder::new(1, 1, qp, offset, Vec::new(), 1, false)
    }

    #[test]
    fn mb_qp_delta_accumulates_mod_52() {
        let mut d = fd(26, 0);
        assert_eq!(d.cur_qp, 26, "QPy starts at the slice QP");
        d.step_qp(4);
        assert_eq!(d.cur_qp, 30); // 26 + 4
        d.step_qp(-10);
        assert_eq!(d.cur_qp, 20); // carries from the previous MB, not the slice
        // Wrap-around: (20 + 40 + 52) % 52 = 112 % 52 = 8.
        d.step_qp(40);
        assert_eq!(d.cur_qp, 8);
        // Negative wrap: (8 - 20 + 52) % 52 = 40.
        d.step_qp(-20);
        assert_eq!(d.cur_qp, 40);
    }

    #[test]
    fn chroma_qp_index_offset_applied_and_clamped() {
        // Offset 0 reproduces the bare luma->chroma table (QP30 -> 29).
        assert_eq!(fd(0, 0).chroma_qp_for(30), 29);
        // Positive offset shifts the table lookup (QP30 + 2 -> table[2] = 31).
        assert_eq!(fd(0, 2).chroma_qp_for(30), 31);
        // The qPi index is clamped into 0..=51 before the lookup.
        assert_eq!(fd(0, -12).chroma_qp_for(5), chroma_qp(0));
        assert_eq!(fd(0, 99).chroma_qp_for(40), chroma_qp(51));
    }
}
