//! I_16x16 macroblock decoding — the mirror of the encoder's `mb16`.
//!
//! Parses each macroblock's residuals and reconstructs it with the exact same
//! prediction + inverse-transform helpers the encoder uses, so decoder output
//! matches encoder reconstruction bit-for-bit.
#![allow(clippy::needless_range_loop)]

use rusty_h264_common::bit_reader::OutOfData;
use rusty_h264_common::cavlc::{decode_residual_block, read_cbp_inter, read_cbp_intra, ZIGZAG_4X4};
use rusty_h264_common::inter::{
    inter_partitions, mc_chroma, mc_luma, predict_mv, predict_partition_mv, MvNeighbor,
};
use rusty_h264_common::predict::{
    chroma8x8_pred, chroma_qp, intra4x4_pred, luma16x16_pred, nc_from_neighbors, reconstruct_4x4,
    I16Mode, CHROMA_4X4_SCAN_XY, LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{dequantize, inverse_quant_chroma_dc, inverse_quant_luma_dc};
use rusty_h264_common::{BitReader, YuvFrame};

/// Reconstructed coded-size planes plus CAVLC `nnz` context grids.
pub struct FrameDecoder {
    mb_w: usize,
    mb_h: usize,
    qp: u8,
    cw: usize,
    ch: usize,
    ccw: usize,
    cch: usize,
    rec_y: Vec<u8>,
    rec_u: Vec<u8>,
    rec_v: Vec<u8>,
    nnz_y: Vec<u8>,
    nnz_c: [Vec<u8>; 2],
    modes_y: Vec<u8>,
    coded_y: Vec<bool>,
    mv_y: Vec<(i32, i32)>,
    inter_y: Vec<bool>,
    /// The previous deblocked picture, for inter prediction (`None` in I-slices).
    reference: Option<crate::RefFrame>,
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
    pub fn new(mb_w: usize, mb_h: usize, qp: u8, reference: Option<crate::RefFrame>) -> Self {
        let (cw, ch) = (mb_w * 16, mb_h * 16);
        let (ccw, cch) = (cw / 2, ch / 2);
        Self {
            mb_w,
            mb_h,
            qp,
            cw,
            ch,
            ccw,
            cch,
            rec_y: vec![0; cw * ch],
            rec_u: vec![0; ccw * cch],
            rec_v: vec![0; ccw * cch],
            nnz_y: vec![0; (mb_w * 4) * (mb_h * 4)],
            nnz_c: [vec![0; (mb_w * 2) * (mb_h * 2)], vec![0; (mb_w * 2) * (mb_h * 2)]],
            modes_y: vec![2; (mb_w * 4) * (mb_h * 4)],
            coded_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            mv_y: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            inter_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            reference,
        }
    }

    fn mv_neighbors(&self, mb_x: usize, mb_y: usize) -> [MvNeighbor; 3] {
        let w4 = self.mb_w * 4;
        let get = |avail: bool, bx: isize, by: isize| {
            if avail {
                let idx = by as usize * w4 + bx as usize;
                MvNeighbor {
                    available: true,
                    mv: self.mv_y[idx],
                    inter_ref0: self.inter_y[idx],
                }
            } else {
                MvNeighbor::NONE
            }
        };
        let (bx, by) = (mb_x as isize * 4, mb_y as isize * 4);
        let a = get(mb_x > 0, bx - 1, by);
        let b = get(mb_y > 0, bx, by - 1);
        let c = if mb_y > 0 && mb_x + 1 < self.mb_w {
            get(true, bx + 4, by - 1)
        } else {
            get(mb_x > 0 && mb_y > 0, bx - 1, by - 1)
        };
        [a, b, c]
    }

    fn mv_neighbors_block(&self, pbx: isize, pby: isize, pwb: isize) -> [MvNeighbor; 3] {
        let (w4, h4) = ((self.mb_w * 4) as isize, (self.mb_h * 4) as isize);
        let get = |bx: isize, by: isize| -> MvNeighbor {
            if bx < 0 || by < 0 || bx >= w4 || by >= h4 || !self.coded_y[(by * w4 + bx) as usize] {
                MvNeighbor::NONE
            } else {
                let idx = (by * w4 + bx) as usize;
                MvNeighbor { available: true, mv: self.mv_y[idx], inter_ref0: self.inter_y[idx] }
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
            || (a.inter_ref0 && a.mv == (0, 0))
            || (b.inter_ref0 && b.mv == (0, 0))
        {
            (0, 0)
        } else {
            predict_mv(a, b, c)
        }
    }

    fn set_mb_mv(&mut self, mb_x: usize, mb_y: usize, mv: (i32, i32), inter: bool) {
        let w4 = self.mb_w * 4;
        for dy in 0..4 {
            for dx in 0..4 {
                let idx = (mb_y * 4 + dy) * w4 + (mb_x * 4 + dx);
                self.mv_y[idx] = mv;
                self.inter_y[idx] = inter;
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
        }
    }

    fn luma_nnz(&self, bx: isize, by: isize) -> Option<u8> {
        if bx < 0 || by < 0 || bx as usize >= self.mb_w * 4 || by as usize >= self.mb_h * 4 {
            None
        } else {
            Some(self.nnz_y[by as usize * (self.mb_w * 4) + bx as usize])
        }
    }

    fn chroma_nnz(&self, c: usize, bx: isize, by: isize) -> Option<u8> {
        if bx < 0 || by < 0 || bx as usize >= self.mb_w * 2 || by as usize >= self.mb_h * 2 {
            None
        } else {
            Some(self.nnz_c[c][by as usize * (self.mb_w * 2) + bx as usize])
        }
    }

    /// Decodes all macroblocks of the slice (raster order). In a P-slice each
    /// macroblock is preceded by `mb_skip_run` (a run of skipped macroblocks).
    pub fn decode_slice_data(&mut self, r: &mut BitReader, is_p: bool) -> Result<(), MbError> {
        let total = self.mb_w * self.mb_h;
        let mut addr = 0;
        while addr < total {
            if is_p {
                let skip_run = r.read_ue()? as usize;
                for _ in 0..skip_run {
                    if addr >= total {
                        break;
                    }
                    self.decode_p_skip(addr % self.mb_w, addr / self.mb_w)?;
                    addr += 1;
                }
                if addr >= total {
                    break;
                }
            }
            self.decode_mb(r, addr % self.mb_w, addr / self.mb_w, is_p)?;
            addr += 1;
        }
        Ok(())
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
            // In P-slices, mb_type 0/1/2 are inter (16×16, 16×8, 8×16), 5+ intra.
            if mb_type <= 2 {
                return self.decode_inter(r, mb_x, mb_y, mb_type as u8);
            }
            if mb_type < 5 {
                return Err(MbError::Unsupported("P_8x8 inter"));
            }
            mb_type -= 5;
        }
        if mb_type == 0 {
            self.decode_i4x4(r, mb_x, mb_y)?;
        } else if (1..=24).contains(&mb_type) {
            self.decode_i16(r, mb_x, mb_y, mb_type - 1)?;
        } else {
            return Err(MbError::Unsupported("only I_4x4 / I_16x16 macroblocks"));
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
        let reference = self
            .reference
            .clone()
            .ok_or(MbError::Unsupported("inter without reference"))?;
        let (qp, qpc) = (self.qp, chroma_qp(self.qp));
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        // ---- per-partition motion vectors + motion compensation ----
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        for (part, &(rx, ry, rw, rh)) in inter_partitions(mode).iter().enumerate() {
            let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
            let [a, b, c] = self.mv_neighbors_block(pbx, pby, (rw / 4) as isize);
            let pmv = predict_partition_mv(mode, part, a, b, c);
            let mvd_x = r.read_se()?;
            let mvd_y = r.read_se()?;
            let mv = (pmv.0 + mvd_x, pmv.1 + mvd_y);
            for by in ry / 4..ry / 4 + rh / 4 {
                for bx in rx / 4..rx / 4 + rw / 4 {
                    let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                    self.mv_y[idx] = mv;
                    self.inter_y[idx] = true;
                    self.coded_y[idx] = true;
                }
            }
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

        let cbp = read_cbp_inter(r)?;
        let cbp_luma = cbp & 15;
        let cbp_chroma = cbp >> 4;
        if cbp != 0 {
            let _mb_qp_delta = r.read_se()?;
        }

        // ---- luma residual ----
        let mut q_blocks = [[0i32; 16]; 16];
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = nc_from_neighbors(
                    self.luma_nnz(bx as isize - 1, by as isize),
                    self.luma_nnz(bx as isize, by as isize - 1),
                );
                let scan16 = decode_residual_block(r, 16, nc)?;
                for i in 0..16 {
                    q_blocks[lby * 4 + lbx][ZIGZAG_4X4[i]] = scan16[i];
                }
                self.nnz_y[by * w4 + bx] = scan16.iter().filter(|&&v| v != 0).count() as u8;
            } else {
                self.nnz_y[by * w4 + bx] = 0;
            }
        }

        // ---- chroma residual ----
        let mut c_recon_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for slot in c_recon_dc.iter_mut() {
                let dc = decode_residual_block(r, 4, -1)?;
                *slot = inverse_quant_chroma_dc(&[dc[0], dc[1], dc[2], dc[3]], qpc);
            }
        }
        let mut c_q = [[[0i32; 16]; 4]; 2];
        if cbp_chroma == 2 {
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let abx = mb_x as isize * 2 + bx as isize;
                    let aby = mb_y as isize * 2 + by as isize;
                    let nc = nc_from_neighbors(
                        self.chroma_nnz(c, abx - 1, aby),
                        self.chroma_nnz(c, abx, aby - 1),
                    );
                    let ac = decode_residual_block(r, 15, nc)?;
                    self.nnz_c[c][aby as usize * (self.mb_w * 2) + abx as usize] =
                        ac.iter().filter(|&&v| v != 0).count() as u8;
                    for i in 0..15 {
                        c_q[c][by * 2 + bx][ZIGZAG_4X4[i + 1]] = ac[i];
                    }
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
            let deq = dequantize(&q_blocks[lby * 4 + lbx], qp);
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
                let mut deq = dequantize(&c_q[c][by * 2 + bx], qpc);
                deq[0] = c_recon_dc[c][by * 2 + bx];
                let s = reconstruct_4x4(&deq, &predb);
                store(plane, self.ccw, mb_x * 8 + bx * 4, mb_y * 8 + by * 4, &s);
            }
        }

        // MV grid + coded flags were set per partition; mark modes as DC.
        let w4b = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4b + (mb_x * 4 + lbx)] = 2;
        }
        Ok(())
    }

    /// Reconstructs a `P_Skip` macroblock: motion-compensate from the reference
    /// at the skip MV, with no residual.
    fn decode_p_skip(&mut self, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let reference = self.reference.clone().ok_or(MbError::Unsupported("P_Skip without reference"))?;
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
        self.set_mb_mv(mb_x, mb_y, mv, true);
        // Mark blocks coded; inter blocks count as DC (not I_4x4) for mode pred.
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
        Ok(())
    }

    /// Predicted `Intra_4x4` mode for the block at absolute coords `(bx, by)`.
    fn predict_i4_mode(&self, bx: usize, by: usize) -> u8 {
        if bx == 0 || by == 0 {
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
            let tr_avail = bx + 1 < w4 && self.coded_y[(by - 1) * w4 + (bx + 1)];
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
        if avail_top && avail_left {
            corner = self.rec_y[(py - 1) * cw + px - 1];
        }
        (top, left, corner)
    }

    fn decode_i4x4(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let qp = self.qp;
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
            let _mb_qp_delta = r.read_se()?;
        }

        // luma residuals + serial reconstruction
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let (px, py) = (bx * 4, by * 4);
            let avail_top = by > 0;
            let avail_left = bx > 0;
            let mut qb = [0i32; 16];
            if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = nc_from_neighbors(
                    self.luma_nnz(bx as isize - 1, by as isize),
                    self.luma_nnz(bx as isize, by as isize - 1),
                );
                let scan16 = decode_residual_block(r, 16, nc)?;
                for i in 0..16 {
                    qb[ZIGZAG_4X4[i]] = scan16[i];
                }
                self.nnz_y[by * w4 + bx] = scan16.iter().filter(|&&v| v != 0).count() as u8;
            } else {
                self.nnz_y[by * w4 + bx] = 0;
            }
            let (top, left, corner) = self.gather_i4(px, py, avail_top, avail_left, bx, by);
            let pred = intra4x4_pred(modes[lby * 4 + lbx], avail_top, avail_left, &top, &left, corner);
            let mut predb = [0i32; 16];
            for i in 0..16 {
                predb[i] = pred[i] as i32;
            }
            let s = reconstruct_4x4(&dequantize(&qb, qp), &predb);
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
        let _mb_qp_delta = r.read_se()?;
        let qp = self.qp;
        let w4 = self.mb_w * 4;

        // luma DC
        let nc_dc = nc_from_neighbors(
            self.luma_nnz(mb_x as isize * 4 - 1, mb_y as isize * 4),
            self.luma_nnz(mb_x as isize * 4, mb_y as isize * 4 - 1),
        );
        let dc_scan = decode_residual_block(r, 16, nc_dc)?;
        let mut dc_levels = [0i32; 16];
        for i in 0..16 {
            dc_levels[ZIGZAG_4X4[i]] = dc_scan[i];
        }
        let recon_dc = inverse_quant_luma_dc(&dc_levels, qp);

        // luma AC
        let mut q_blocks = [[0i32; 16]; 16];
        if cbp_luma_15 {
            for &(bx, by) in &LUMA_4X4_SCAN_XY {
                let abx = mb_x as isize * 4 + bx as isize;
                let aby = mb_y as isize * 4 + by as isize;
                let nc = nc_from_neighbors(self.luma_nnz(abx - 1, aby), self.luma_nnz(abx, aby - 1));
                let ac = decode_residual_block(r, 15, nc)?;
                self.nnz_y[aby as usize * w4 + abx as usize] =
                    ac.iter().filter(|&&v| v != 0).count() as u8;
                let q = &mut q_blocks[by * 4 + bx];
                for i in 0..15 {
                    q[ZIGZAG_4X4[i + 1]] = ac[i];
                }
            }
        }

        // prediction + reconstruction
        let avail_top = mb_y > 0;
        let avail_left = mb_x > 0;
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
                let mut deq = dequantize(&q_blocks[by * 4 + bx], qp);
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
        let qpc = chroma_qp(self.qp);
        let (cx, cy) = (mb_x * 8, mb_y * 8);
        let avail_top = mb_y > 0;
        let avail_left = mb_x > 0;

        let mut c_recon_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for slot in c_recon_dc.iter_mut() {
                let dc = decode_residual_block(r, 4, -1)?;
                *slot = inverse_quant_chroma_dc(&[dc[0], dc[1], dc[2], dc[3]], qpc);
            }
        }
        let mut c_q_blocks = [[[0i32; 16]; 4]; 2];
        if cbp_chroma == 2 {
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let abx = mb_x as isize * 2 + bx as isize;
                    let aby = mb_y as isize * 2 + by as isize;
                    let nc = nc_from_neighbors(
                        self.chroma_nnz(c, abx - 1, aby),
                        self.chroma_nnz(c, abx, aby - 1),
                    );
                    let ac = decode_residual_block(r, 15, nc)?;
                    self.nnz_c[c][aby as usize * (self.mb_w * 2) + abx as usize] =
                        ac.iter().filter(|&&v| v != 0).count() as u8;
                    let q = &mut c_q_blocks[c][by * 2 + bx];
                    for i in 0..15 {
                        q[ZIGZAG_4X4[i + 1]] = ac[i];
                    }
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
                let mut deq = dequantize(&c_q_blocks[c][by * 2 + bx], qpc);
                deq[0] = c_recon_dc[c][by * 2 + bx];
                let s = reconstruct_4x4(&deq, &predb);
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                store(plane, self.ccw, cx + bx * 4, cy + by * 4, &s);
            }
        }
        Ok(())
    }

    /// Applies the in-loop deblocking filter to the reconstructed frame.
    pub fn deblock(&mut self) {
        let intra: Vec<bool> = self.inter_y.iter().map(|&i| !i).collect();
        let info = rusty_h264_common::deblock::BlockInfo {
            intra: &intra,
            nnz: &self.nnz_y,
            mv: &self.mv_y,
            w4: self.mb_w * 4,
        };
        rusty_h264_common::deblock::filter_frame(
            &mut self.rec_y,
            &mut self.rec_u,
            &mut self.rec_v,
            self.mb_w,
            self.mb_h,
            self.qp,
            chroma_qp(self.qp),
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

fn store(plane: &mut [u8], stride: usize, x0: usize, y0: usize, s: &[u8; 16]) {
    for dy in 0..4 {
        for dx in 0..4 {
            plane[(y0 + dy) * stride + (x0 + dx)] = s[dy * 4 + dx];
        }
    }
}
