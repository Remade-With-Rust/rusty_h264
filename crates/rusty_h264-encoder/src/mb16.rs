//! I_16x16 macroblock encoding (DC prediction) — the compressing intra path.
//!
//! Index-based loops below drive pixel/block-position arithmetic and read
//! clearer than iterator adapters for this raster math.
#![allow(clippy::needless_range_loop)]
//!
//! Each macroblock is DC-predicted from already-reconstructed neighbors, the
//! residual is transformed/quantized (luma DC via the secondary Hadamard), the
//! coefficients are CAVLC-coded, and the macroblock is reconstructed so the next
//! one can predict from it. `nnz` grids feed the CAVLC `nC` context exactly as a
//! conforming decoder derives it.

use crate::config::EncoderConfig;
use rusty_h264_common::cavlc::{encode_residual_block, write_cbp_inter, write_cbp_intra, ZIGZAG_4X4};
use rusty_h264_common::inter::{
    inter_partitions, mc_chroma, mc_luma, predict_mv, predict_partition_mv, MvNeighbor,
};
use rusty_h264_common::predict::{
    chroma8x8_pred, chroma_mode_available, chroma_qp, intra4x4_pred, luma16x16_pred,
    nc_from_neighbors, reconstruct_4x4, I16Mode, CHROMA_4X4_SCAN_XY, LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{
    dequantize, forward_core, forward_quant_chroma_dc, forward_quant_luma_dc, hadamard_4x4,
    inverse_quant_chroma_dc, inverse_quant_luma_dc, quantize,
};
use rusty_h264_common::{BitWriter, YuvFrame};

/// Per-frame intra encoder state: reconstructed planes (coded size) and the
/// per-4×4-block non-zero-coefficient counts used for CAVLC context.
pub struct FrameEncoder {
    mb_w: usize,
    mb_h: usize,
    qp: u8,
    qpc: u8,
    cw: usize, // coded luma width
    ccw: usize, // coded chroma width
    rec_y: Vec<u8>,
    rec_u: Vec<u8>,
    rec_v: Vec<u8>,
    nnz_y: Vec<u8>,    // (mb_w*4) x (mb_h*4)
    nnz_c: [Vec<u8>; 2], // each (mb_w*2) x (mb_h*2)
    modes_y: Vec<u8>,  // intra4x4 mode per 4×4 block (2=DC for I_16x16 blocks)
    coded_y: Vec<bool>, // whether each 4×4 block is reconstructed (top-right avail)
    mv_y: Vec<(i32, i32)>, // motion vector per 4×4 block (quarter-pel)
    inter_y: Vec<bool>, // whether each 4×4 block is inter-coded
    ref_idx_y: Vec<i32>, // reference index per 4×4 block (-1 = intra/uncoded)
}

/// A chosen inter coding for a macroblock: `mb_type` and, per partition, the
/// reference index and motion vector.
type InterChoice = (u8, Vec<(i32, (i32, i32))>);

/// Edge-clamped, coded-size source planes (luma, Cb, Cr).
fn coded_source(cfg: &EncoderConfig, frame: &YuvFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let cw = cfg.mb_width() * 16;
    let ch = cfg.mb_height() * 16;
    let clamp = |plane: &[u8], w: usize, h: usize, ow: usize, oh: usize| {
        let mut out = vec![0u8; ow * oh];
        for y in 0..oh {
            for x in 0..ow {
                let sx = x.min(w - 1);
                let sy = y.min(h - 1);
                out[y * ow + x] = plane[sy * w + sx];
            }
        }
        out
    };
    let y = clamp(&frame.y, frame.width, frame.height, cw, ch);
    let u = clamp(&frame.u, frame.chroma_width(), frame.chroma_height(), cw / 2, ch / 2);
    let v = clamp(&frame.v, frame.chroma_width(), frame.chroma_height(), cw / 2, ch / 2);
    (y, u, v)
}

impl FrameEncoder {
    fn new(cfg: &EncoderConfig) -> Self {
        let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
        let (cw, ch) = (mb_w * 16, mb_h * 16);
        let (ccw, cch) = (cw / 2, ch / 2);
        Self {
            mb_w,
            mb_h,
            qp: cfg.qp,
            qpc: chroma_qp(cfg.qp),
            cw,
            ccw,
            rec_y: vec![0; cw * ch],
            rec_u: vec![0; ccw * cch],
            rec_v: vec![0; ccw * cch],
            nnz_y: vec![0; (mb_w * 4) * (mb_h * 4)],
            nnz_c: [vec![0; (mb_w * 2) * (mb_h * 2)], vec![0; (mb_w * 2) * (mb_h * 2)]],
            modes_y: vec![2; (mb_w * 4) * (mb_h * 4)],
            coded_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            mv_y: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            inter_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            ref_idx_y: vec![-1; (mb_w * 4) * (mb_h * 4)],
        }
    }

    /// MV-predictor neighbors (left, above, above-right) for the 16×16 partition
    /// of macroblock `(mb_x, mb_y)`, read from the per-4×4-block grids.
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
        let a = get(mb_x > 0, bx - 1, by);
        let b = get(mb_y > 0, bx, by - 1);
        // C = above-right; if unavailable, fall back to D = above-left.
        let c = if mb_y > 0 && mb_x + 1 < self.mb_w {
            get(true, bx + 4, by - 1)
        } else {
            get(mb_x > 0 && mb_y > 0, bx - 1, by - 1)
        };
        [a, b, c]
    }

    /// The `P_Skip` motion vector (spec §8.4.1.1). P_Skip always references
    /// index 0 (the most recent picture).
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

    /// Records a macroblock's per-4×4-block motion state (`ref` = reference index
    /// for inter, ignored for intra where `inter` is false).
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

    /// Block-level MV-predictor neighbors for a partition whose top-left 4×4
    /// block is `(pbx, pby)` and which is `pwb` blocks wide. Availability uses
    /// the decoded-block grid, so in-macroblock partitions see earlier ones.
    fn mv_neighbors_block(&self, pbx: isize, pby: isize, pwb: isize) -> [MvNeighbor; 3] {
        let (w4, h4) = ((self.mb_w * 4) as isize, (self.mb_h * 4) as isize);
        let get = |bx: isize, by: isize| -> MvNeighbor {
            if bx < 0 || by < 0 || bx >= w4 || by >= h4 || !self.coded_y[(by * w4 + bx) as usize] {
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
            c = get(pbx - 1, pby - 1); // D fallback
        }
        [a, b, c]
    }

    /// SATD of a motion-compensated `rw`×`rh` luma region (at macroblock-relative
    /// offset `(rx, ry)`) against the source.
    #[allow(clippy::too_many_arguments)]
    fn mc_satd(
        &self,
        reference: &crate::RefFrame,
        sy: &[u8],
        lx: usize,
        ly: usize,
        rw: usize,
        rh: usize,
        mv: (i32, i32),
    ) -> i64 {
        let ch = self.mb_h * 16;
        let mut pred = [0u8; 256];
        mc_luma(&reference.y, self.cw, ch, lx, ly, rw, rh, mv.0, mv.1, &mut pred);
        let mut s = 0i64;
        for by in 0..rh / 4 {
            for bx in 0..rw / 4 {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        res[dy * 4 + dx] = sy[(ly + by * 4 + dy) * self.cw + (lx + bx * 4 + dx)] as i32
                            - pred[(by * 4 + dy) * rw + (bx * 4 + dx)] as i32;
                    }
                }
                s += hadamard_4x4(&res).iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
            }
        }
        s
    }

    /// Rate-aware motion search for a luma region: full-pel diamond + half/
    /// quarter-pel refinement minimizing `J = SATD + λ·bits(mvd)`, where the
    /// motion cost is measured against `predictors[0]` (the MV predictor the
    /// `mvd` will actually be coded against). The search is seeded from every
    /// entry in `predictors` plus `(0,0)`. Returns the best MV and its `J`.
    ///
    /// The rate term is only a *search heuristic* — whatever MV it picks is still
    /// coded as a correct `mvd`, so this never affects decodability.
    #[allow(clippy::too_many_arguments)]
    fn motion_search(
        &self,
        reference: &crate::RefFrame,
        sy: &[u8],
        lx: usize,
        ly: usize,
        rw: usize,
        rh: usize,
        predictors: &[(i32, i32)],
        lambda_me: f64,
    ) -> ((i32, i32), i64) {
        // Bit length of `se(d)` (Exp-Golomb), i.e. what an `mvd` component costs.
        fn mvbits(d: i32) -> u32 {
            let codenum = if d > 0 { (2 * d - 1) as u32 } else { (-2 * d) as u32 };
            let mut n = codenum + 1;
            let mut len = 1u32;
            while n > 1 {
                n >>= 1;
                len += 2;
            }
            len
        }
        let center = predictors[0];
        let cost = |mv: (i32, i32)| -> i64 {
            let rate = mvbits(mv.0 - center.0) + mvbits(mv.1 - center.1);
            self.mc_satd(reference, sy, lx, ly, rw, rh, mv) + (lambda_me * rate as f64) as i64
        };
        // Seed from (0,0) and each predictor; keep the cheapest.
        let mut best = (0, 0);
        let mut best_c = cost(best);
        for &p in predictors {
            let pc = cost(p);
            if pc < best_c {
                best_c = pc;
                best = p;
            }
        }
        // Coarse-to-fine full-pel search: a 4-point diamond walked at each step
        // size from 16 px down to 1 px (steps in quarter-pel units: 64,32,…,4).
        // The larger initial steps reach fast motion the predictor missed; the
        // diamond stays orthogonal (no diagonals) — diagonal probes were found to
        // chase equally-good far matches on ambiguous motion, wrecking MV-field
        // coherence and the neighbor predictors.
        for step in [64, 32, 16, 8, 4] {
            loop {
                let mut improved = false;
                for &(dx, dy) in &[(step, 0), (-step, 0), (0, step), (0, -step)] {
                    let c = (best.0 + dx, best.1 + dy);
                    let cc = cost(c);
                    if cc < best_c {
                        best_c = cc;
                        best = c;
                        improved = true;
                    }
                }
                if !improved {
                    break;
                }
            }
        }
        for step in [2, 1] {
            for &(dx, dy) in &[
                (step, 0), (-step, 0), (0, step), (0, -step),
                (step, step), (-step, -step), (step, -step), (-step, step),
            ] {
                let c = (best.0 + dx, best.1 + dy);
                let cc = cost(c);
                if cc < best_c {
                    best_c = cc;
                    best = c;
                }
            }
        }
        (best, best_c)
    }

    /// Lowest SATD over the available I_16x16 modes (a cheap intra-cost estimate
    /// for the inter-vs-intra decision).
    fn best_i16_satd(&self, sy: &[u8], mb_x: usize, mb_y: usize) -> i64 {
        let (avail_top, avail_left) = (mb_y > 0, mb_x > 0);
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
        let mut best = i64::MAX;
        for mode in [I16Mode::Dc, I16Mode::Vertical, I16Mode::Horizontal, I16Mode::Plane] {
            if mode.available(avail_top, avail_left) {
                let pred = luma16x16_pred(mode, avail_top, avail_left, &top, &left, corner);
                best = best.min(satd_16x16(sy, self.cw, lx, ly, &pred));
            }
        }
        best
    }

    /// Encodes macroblock `(mb_x, mb_y)` as an inter macroblock of the given
    /// `mode` (0 = P_L0_16x16, 1 = P_16x8, 2 = P_8x16) with one motion vector
    /// per partition: motion-compensate each partition, code the macroblock
    /// residual, and reconstruct.
    #[allow(clippy::too_many_arguments)]
    fn encode_inter_mb(
        &mut self,
        w: &mut BitWriter,
        refs: &[crate::RefFrame],
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        parts: &[(i32, (i32, i32))],
    ) {
        let (qp, qpc) = (self.qp, self.qpc);
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        // ---- per-partition motion compensation + MV prediction ----
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        let mut mvds = Vec::with_capacity(parts.len());
        for (part, &(rx, ry, rw, rh)) in inter_partitions(mode).iter().enumerate() {
            let (refi, mv) = parts[part];
            let reference = &refs[refi as usize];
            let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
            let [a, b, c] = self.mv_neighbors_block(pbx, pby, (rw / 4) as isize);
            let pmv = predict_partition_mv(mode, part, a, b, c, refi);
            mvds.push((mv.0 - pmv.0, mv.1 - pmv.1));
            // Commit this partition's motion so later partitions can predict from it.
            for by in ry / 4..ry / 4 + rh / 4 {
                for bx in rx / 4..rx / 4 + rw / 4 {
                    let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                    self.mv_y[idx] = mv;
                    self.inter_y[idx] = true;
                    self.ref_idx_y[idx] = refi;
                    self.coded_y[idx] = true;
                }
            }
            // Luma MC into the partition's sub-region.
            let mut tmp = [0u8; 256];
            mc_luma(&reference.y, self.cw, ch, mb_x * 16 + rx, mb_y * 16 + ry, rw, rh, mv.0, mv.1, &mut tmp);
            for dy in 0..rh {
                for dx in 0..rw {
                    pred_y[(ry + dy) * 16 + (rx + dx)] = tmp[dy * rw + dx];
                }
            }
            // Chroma MC (half-resolution region).
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

        // ---- luma residual ----
        let mut q_blocks = [[0i32; 16]; 16]; // raster
        let mut cbp_luma = 0u32;
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let mut res = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    let sx = mb_x * 16 + lbx * 4 + dx;
                    let syy = mb_y * 16 + lby * 4 + dy;
                    res[dy * 4 + dx] = sy[syy * self.cw + sx] as i32
                        - pred_y[(lby * 4 + dy) * 16 + (lbx * 4 + dx)] as i32;
                }
            }
            let q = quantize(&forward_core(&res), qp, false);
            if q.iter().any(|&v| v != 0) {
                cbp_luma |= 1 << (blk / 4);
            }
            q_blocks[lby * 4 + lbx] = q;
        }

        // ---- chroma residual (prediction already built per partition) ----
        let mut c_dc_levels = [[0i32; 4]; 2];
        let mut c_recon_dc = [[0i32; 4]; 2];
        let mut c_q = [[[0i32; 16]; 4]; 2];
        let (mut any_ac, mut any_dc) = (false, false);
        for c in 0..2 {
            let src = if c == 0 { su } else { sv };
            let mut dc2x2 = [0i32; 4];
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        let sx = mb_x * 8 + bx * 4 + dx;
                        let syy = mb_y * 8 + by * 4 + dy;
                        res[dy * 4 + dx] = src[syy * self.ccw + sx] as i32
                            - c_pred[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    }
                }
                let coeffs = forward_core(&res);
                dc2x2[by * 2 + bx] = coeffs[0];
                let mut q = quantize(&coeffs, qpc, false);
                q[0] = 0;
                if q[1..].iter().any(|&v| v != 0) {
                    any_ac = true;
                }
                c_q[c][by * 2 + bx] = q;
            }
            let dl = forward_quant_chroma_dc(&dc2x2, qpc, false);
            if dl.iter().any(|&v| v != 0) {
                any_dc = true;
            }
            c_recon_dc[c] = inverse_quant_chroma_dc(&dl, qpc);
            c_dc_levels[c] = dl;
        }
        let cbp_chroma: u32 = if any_ac { 2 } else if any_dc { 1 } else { 0 };
        let cbp = cbp_luma | (cbp_chroma << 4);

        // ---- emit ----
        // mb_pred order (spec 7.3.5.1): mb_type, then all ref_idx_l0, then all
        // mvd_l0. ref_idx is coded only when more than one reference is active.
        w.write_ue(mode as u32); // inter mb_type
        let num_refs = refs.len();
        if num_refs > 1 {
            for &(refi, _) in parts {
                write_ref_idx(w, refi, num_refs);
            }
        }
        for &(mvdx, mvdy) in &mvds {
            w.write_se(mvdx);
            w.write_se(mvdy);
        }
        write_cbp_inter(w, cbp);
        if cbp != 0 {
            w.write_se(0); // mb_qp_delta
        }
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = nc_from_neighbors(
                    self.luma_nnz(bx as isize - 1, by as isize),
                    self.luma_nnz(bx as isize, by as isize - 1),
                );
                let mut scan16 = [0i32; 16];
                for i in 0..16 {
                    scan16[i] = q_blocks[lby * 4 + lbx][ZIGZAG_4X4[i]];
                }
                let total = scan16.iter().filter(|&&v| v != 0).count() as u8;
                encode_residual_block(w, &scan16, 16, nc);
                self.nnz_y[by * w4 + bx] = total;
            } else {
                self.nnz_y[by * w4 + bx] = 0;
            }
        }
        if cbp_chroma != 0 {
            for c in 0..2 {
                encode_residual_block(w, &c_dc_levels[c], 4, -1);
            }
        }
        if cbp_chroma == 2 {
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let abx = mb_x as isize * 2 + bx as isize;
                    let aby = mb_y as isize * 2 + by as isize;
                    let nc = nc_from_neighbors(
                        self.chroma_nnz(c, abx - 1, aby),
                        self.chroma_nnz(c, abx, aby - 1),
                    );
                    let mut ac = [0i32; 15];
                    for i in 0..15 {
                        ac[i] = c_q[c][by * 2 + bx][ZIGZAG_4X4[i + 1]];
                    }
                    let total = ac.iter().filter(|&&v| v != 0).count() as u8;
                    encode_residual_block(w, &ac, 15, nc);
                    self.nnz_c[c][aby as usize * (self.mb_w * 2) + abx as usize] = total;
                }
            }
        }

        // ---- reconstruction ----
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
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
    }

    /// Tries to code macroblock `(mb_x, mb_y)` as `P_Skip`: motion-compensate
    /// from the reference at the skip MV and accept only if the residual
    /// quantizes to nothing (a free, strictly-beneficial skip). On success it
    /// reconstructs the macroblock and updates the motion grids, returning true.
    fn try_skip(
        &mut self,
        refs: &[crate::RefFrame],
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
    ) -> bool {
        let reference = &refs[0]; // P_Skip always references index 0
        let (qp, qpc) = (self.qp, self.qpc);
        let ch = self.mb_h * 16;
        let cch = self.mb_h * 8;
        let mv = self.skip_mv(mb_x, mb_y);

        // Luma prediction + residual-quantizes-to-zero test.
        let mut pred_y = [0u8; 256];
        mc_luma(&reference.y, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred_y);
        for by in 0..4 {
            for bx in 0..4 {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        let sx = mb_x * 16 + bx * 4 + dx;
                        let syy = mb_y * 16 + by * 4 + dy;
                        res[dy * 4 + dx] = sy[syy * self.cw + sx] as i32
                            - pred_y[(by * 4 + dy) * 16 + (bx * 4 + dx)] as i32;
                    }
                }
                if quantize(&forward_core(&res), qp, false).iter().any(|&v| v != 0) {
                    return false;
                }
            }
        }

        // Chroma prediction + residual test (DC + AC) for both components.
        let mut pred_c = [[0u8; 64]; 2];
        for c in 0..2 {
            let rc = if c == 0 { &reference.u } else { &reference.v };
            mc_chroma(rc, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut pred_c[c]);
            let src = if c == 0 { su } else { sv };
            let mut dc2x2 = [0i32; 4];
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        let sx = mb_x * 8 + bx * 4 + dx;
                        let syy = mb_y * 8 + by * 4 + dy;
                        res[dy * 4 + dx] = src[syy * self.ccw + sx] as i32
                            - pred_c[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    }
                }
                let coeffs = forward_core(&res);
                dc2x2[by * 2 + bx] = coeffs[0];
                let q = quantize(&coeffs, qpc, false);
                if q[1..].iter().any(|&v| v != 0) {
                    return false;
                }
            }
            if forward_quant_chroma_dc(&dc2x2, qpc, false).iter().any(|&v| v != 0) {
                return false;
            }
        }

        // Free skip: reconstruction is exactly the prediction.
        for by in 0..4 {
            for bx in 0..4 {
                let mut s = [0u8; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        s[dy * 4 + dx] = pred_y[(by * 4 + dy) * 16 + (bx * 4 + dx)];
                    }
                }
                store(&mut self.rec_y, self.cw, mb_x * 16 + bx * 4, mb_y * 16 + by * 4, &s);
            }
        }
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut s = [0u8; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        s[dy * 4 + dx] = pred_c[c][(by * 4 + dy) * 8 + (bx * 4 + dx)];
                    }
                }
                store(plane, self.ccw, mb_x * 8 + bx * 4, mb_y * 8 + by * 4, &s);
            }
        }
        self.set_mb_mv(mb_x, mb_y, mv, true, 0);
        // For neighbor intra-mode prediction, inter blocks count as "not I_4x4".
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
            self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
        }
        true
    }

    /// nnz of the luma 4×4 block at absolute block coords, or `None` if outside.
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
}

/// Encodes a slice's macroblocks then RBSP trailing bits, returning the
/// **deblocked** reconstruction to serve as the next frame's reference.
///
/// `is_p` selects P-slice framing (`mb_skip_run` prefix + intra `mb_type` +5
/// offset). In phase 4a every macroblock is still coded intra; motion-compensated
/// macroblocks arrive in 4b (using `reference`).
pub fn encode_slice_data(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    frame: &YuvFrame,
    qp: u8,
    is_p: bool,
    refs: &[crate::RefFrame],
) -> crate::RefFrame {
    let mut fe = FrameEncoder::new(cfg);
    fe.qp = qp;
    fe.qpc = chroma_qp(qp);
    let (sy, su, sv) = coded_source(cfg, frame);
    let lambda = 0.85 * 2f64.powf((qp as f64 - 12.0) / 3.0);
    let num_refs = refs.len();
    let mut skip_run = 0u32;
    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            // P_Skip: motion-compensate from the most-recent reference; accept if free.
            // Chosen inter coding: (mb_type, per-partition (ref_idx, mv)).
            let mut inter: Option<InterChoice> = None;
            if is_p {
                if num_refs > 0 {
                    if fe.try_skip(refs, &sy, &su, &sv, mb_x, mb_y) {
                        skip_run += 1;
                        continue;
                    }
                    // Rate-aware partition search over all references. For each
                    // region we pick the (ref, MV) minimizing SATD + λ·bits(mvd) +
                    // λ·bits(ref_idx); the median predictor (per ref) is the rate
                    // center, and partition searches also seed from the 16×16 win.
                    // Multi-partition modes pay for their extra MVs via the rate
                    // term, plus a split penalty; intra wins if cheaper.
                    let (lx, ly) = (mb_x * 16, mb_y * 16);
                    let [a, b, c] = fe.mv_neighbors_block(mb_x as isize * 4, mb_y as isize * 4, 4);
                    let lme = lambda.sqrt();
                    let best_for = |rx: usize, ry: usize, rw: usize, rh: usize, extra: &[(i32, i32)]| {
                        let (mut br, mut bmv, mut bc) = (0i32, (0, 0), i64::MAX);
                        for r in 0..num_refs {
                            let rc = predict_mv(a, b, c, r as i32);
                            let mut seeds = vec![rc];
                            seeds.extend_from_slice(extra);
                            let (mv, cost) =
                                fe.motion_search(&refs[r], &sy, rx, ry, rw, rh, &seeds, lme);
                            let cost = cost + (lme * ref_bits(r, num_refs) as f64) as i64;
                            if cost < bc {
                                bc = cost;
                                br = r as i32;
                                bmv = mv;
                            }
                        }
                        (br, bmv, bc)
                    };
                    let (r16, mv16, s16) = best_for(lx, ly, 16, 16, &[]);
                    let (rt, mvt, st) = best_for(lx, ly, 16, 8, &[mv16]);
                    let (rb, mvb, sb) = best_for(lx, ly + 8, 16, 8, &[mv16]);
                    let (rl, mvl, sl) = best_for(lx, ly, 8, 16, &[mv16]);
                    let (rr, mvr, sr) = best_for(lx + 8, ly, 8, 16, &[mv16]);
                    let split_pen = (lambda * 6.0) as i64;
                    let mut best_mode = 0u8;
                    let mut best_cost = s16;
                    let mut best: Vec<(i32, (i32, i32))> = vec![(r16, mv16)];
                    if st + sb + split_pen < best_cost {
                        best_mode = 1;
                        best_cost = st + sb + split_pen;
                        best = vec![(rt, mvt), (rb, mvb)];
                    }
                    if sl + sr + split_pen < best_cost {
                        best_mode = 2;
                        best_cost = sl + sr + split_pen;
                        best = vec![(rl, mvl), (rr, mvr)];
                    }
                    if best_cost < fe.best_i16_satd(&sy, mb_x, mb_y) {
                        inter = Some((best_mode, best));
                    }
                }
                w.write_ue(skip_run); // run of skipped macroblocks before this one
                skip_run = 0;
            }
            match inter {
                Some((mode, parts)) => {
                    fe.encode_inter_mb(w, refs, &sy, &su, &sv, mb_x, mb_y, mode, &parts);
                }
                None => encode_mb(&mut fe, w, mb_x, mb_y, &sy, &su, &sv, is_p),
            }
        }
    }
    if is_p && skip_run > 0 {
        w.write_ue(skip_run); // trailing skipped macroblocks
    }
    w.rbsp_trailing_bits();

    // Deblock the reconstruction; the result is the inter reference.
    let intra: Vec<bool> = fe.inter_y.iter().map(|&i| !i).collect();
    let info = rusty_h264_common::deblock::BlockInfo {
        intra: &intra,
        nnz: &fe.nnz_y,
        mv: &fe.mv_y,
        w4: fe.mb_w * 4,
    };
    rusty_h264_common::deblock::filter_frame(
        &mut fe.rec_y,
        &mut fe.rec_u,
        &mut fe.rec_v,
        fe.mb_w,
        fe.mb_h,
        fe.qp,
        fe.qpc,
        &info,
    );
    crate::RefFrame {
        y: fe.rec_y,
        u: fe.rec_u,
        v: fe.rec_v,
    }
}

/// Reads a 4×4 residual block (source minus a raster prediction block).
/// Writes `ref_idx_l0` (spec: `te(v)` when two references are active — a single
/// flag — else `ue(v)`). Only called when more than one reference is active.
fn write_ref_idx(w: &mut BitWriter, refi: i32, num_refs: usize) {
    if num_refs == 2 {
        w.write_bit(refi == 0); // te(v): value = !bit
    } else {
        w.write_ue(refi as u32);
    }
}

/// Approximate bit cost of coding `ref_idx = r` with `num_refs` active, for the
/// motion-estimation rate term. Zero with a single reference (no `ref_idx` coded).
fn ref_bits(r: usize, num_refs: usize) -> u32 {
    if num_refs <= 1 {
        0
    } else if num_refs == 2 {
        1
    } else {
        let mut n = r as u32 + 1;
        let mut len = 1;
        while n > 1 {
            n >>= 1;
            len += 2;
        }
        len
    }
}

fn residual(src: &[u8], stride: usize, x0: usize, y0: usize, pred: &[i32; 16]) -> [i32; 16] {
    let mut r = [0i32; 16];
    for dy in 0..4 {
        for dx in 0..4 {
            r[dy * 4 + dx] = src[(y0 + dy) * stride + (x0 + dx)] as i32 - pred[dy * 4 + dx];
        }
    }
    r
}

/// Writes reconstructed samples back into a plane.
fn store(plane: &mut [u8], stride: usize, x0: usize, y0: usize, s: &[u8; 16]) {
    for dy in 0..4 {
        for dx in 0..4 {
            plane[(y0 + dy) * stride + (x0 + dx)] = s[dy * 4 + dx];
        }
    }
}

/// Extracts the 4×4 raster prediction block at `(bx, by)` from a 16×16 (256-sample)
/// luma prediction.
fn pred_block(pred: &[u8; 256], bx: usize, by: usize) -> [i32; 16] {
    let mut p = [0i32; 16];
    for dy in 0..4 {
        for dx in 0..4 {
            p[dy * 4 + dx] = pred[(by * 4 + dy) * 16 + (bx * 4 + dx)] as i32;
        }
    }
    p
}

/// Sum of absolute transformed differences over a 16×16 luma macroblock — the
/// mode-decision cost (correlates with coded bits better than plain SAD).
fn satd_16x16(src: &[u8], stride: usize, lx: usize, ly: usize, pred: &[u8; 256]) -> i64 {
    let mut cost = 0i64;
    for by in 0..4 {
        for bx in 0..4 {
            let predb = pred_block(pred, bx, by);
            let res = residual(src, stride, lx + bx * 4, ly + by * 4, &predb);
            let h = hadamard_4x4(&res);
            cost += h.iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
        }
    }
    cost
}

/// SATD over an 8×8 chroma block (four 4×4 sub-blocks) against a prediction.
fn satd_8x8(src: &[u8], stride: usize, x0: usize, y0: usize, pred: &[u8; 64]) -> i64 {
    let mut cost = 0i64;
    for by in 0..2 {
        for bx in 0..2 {
            let mut res = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    let p = pred[(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    res[dy * 4 + dx] =
                        src[(y0 + by * 4 + dy) * stride + (x0 + bx * 4 + dx)] as i32 - p;
                }
            }
            cost += hadamard_4x4(&res).iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
        }
    }
    cost
}

/// SATD of one 4×4 luma block against a prediction.
fn satd_4x4(src: &[u8], stride: usize, px: usize, py: usize, pred: &[u8; 16]) -> i64 {
    let mut res = [0i32; 16];
    for dy in 0..4 {
        for dx in 0..4 {
            res[dy * 4 + dx] = src[(py + dy) * stride + (px + dx)] as i32 - pred[dy * 4 + dx] as i32;
        }
    }
    hadamard_4x4(&res).iter().map(|&v| v.unsigned_abs() as i64).sum()
}

/// Whether an `Intra_4x4` mode is usable given top/left neighbor availability.
fn i4_mode_available(mode: u8, top: bool, left: bool) -> bool {
    match mode {
        0 | 3 | 7 => top,        // vertical, diag-down-left, vertical-left
        1 | 8 => left,           // horizontal, horizontal-up
        2 => true,               // DC
        _ => top && left,        // diag-down-right, vertical-right, horizontal-down
    }
}

/// Result of planning an I_4x4 macroblock (luma). Reconstruction has already
/// been written into the frame's `rec_y` and `coded_y` by [`plan_i4x4`].
struct I4Plan {
    modes: [u8; 16],       // per-block intra4x4 mode, raster [lby*4+lbx]
    q: [[i32; 16]; 16],    // per-block quantized coefficients (full, raster)
    cbp_luma: u32,         // 4-bit coded-block-pattern (one bit per 8×8 region)
    nonzero: i64,          // total non-zero coefficients (rate proxy)
}

/// Gathers the 4×4 luma intra neighbors at pixel `(px, py)` from `rec_y`.
fn gather_i4(
    fe: &FrameEncoder,
    px: usize,
    py: usize,
    avail_top: bool,
    avail_left: bool,
    bx: usize,
    by: usize,
) -> ([u8; 8], [u8; 4], u8) {
    let (cw, w4) = (fe.cw, fe.mb_w * 4);
    let mut top = [0u8; 8];
    let mut left = [0u8; 4];
    let mut corner = 0;
    if avail_top {
        for i in 0..4 {
            top[i] = fe.rec_y[(py - 1) * cw + px + i];
        }
        let tr_avail = bx + 1 < w4 && fe.coded_y[(by - 1) * w4 + (bx + 1)];
        for i in 0..4 {
            top[4 + i] = if tr_avail {
                fe.rec_y[(py - 1) * cw + px + 4 + i]
            } else {
                top[3]
            };
        }
    }
    if avail_left {
        for i in 0..4 {
            left[i] = fe.rec_y[(py + i) * cw + px - 1];
        }
    }
    if avail_top && avail_left {
        corner = fe.rec_y[(py - 1) * cw + px - 1];
    }
    (top, left, corner)
}

/// Plans an I_4x4 macroblock: picks a mode per 4×4 block (lowest-SATD available
/// mode), quantizes, and reconstructs serially into `rec_y` so each block can
/// predict from the previous one.
fn plan_i4x4(fe: &mut FrameEncoder, sy: &[u8], mb_x: usize, mb_y: usize, qp: u8) -> I4Plan {
    let w4 = fe.mb_w * 4;
    let mut modes = [2u8; 16];
    let mut q = [[0i32; 16]; 16];
    let mut cbp_luma = 0u32;
    let mut nonzero = 0i64;

    for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
        let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
        let (px, py) = (bx * 4, by * 4);
        let avail_top = by > 0;
        let avail_left = bx > 0;
        let (top, left, corner) = gather_i4(fe, px, py, avail_top, avail_left, bx, by);

        // Pick the lowest-SATD available mode.
        let mut best_m = 2u8;
        let mut best_cost = i64::MAX;
        for m in 0..9u8 {
            if !i4_mode_available(m, avail_top, avail_left) {
                continue;
            }
            let pred = intra4x4_pred(m, avail_top, avail_left, &top, &left, corner);
            let cost = satd_4x4(sy, fe.cw, px, py, &pred);
            if cost < best_cost {
                best_cost = cost;
                best_m = m;
            }
        }

        // Quantize + reconstruct with the chosen mode.
        let pred = intra4x4_pred(best_m, avail_top, avail_left, &top, &left, corner);
        let mut predb = [0i32; 16];
        for i in 0..16 {
            predb[i] = pred[i] as i32;
        }
        let res = residual(sy, fe.cw, px, py, &predb);
        let qb = quantize(&forward_core(&res), qp, true); // full 16 incl DC
        let s = reconstruct_4x4(&dequantize(&qb, qp), &predb);
        store(&mut fe.rec_y, fe.cw, px, py, &s);
        fe.coded_y[by * w4 + bx] = true;

        let nz = qb.iter().filter(|&&v| v != 0).count();
        if nz > 0 {
            cbp_luma |= 1 << ((lby / 2) * 2 + (lbx / 2));
        }
        nonzero += nz as i64;
        modes[lby * 4 + lbx] = best_m;
        q[lby * 4 + lbx] = qb;
    }
    I4Plan {
        modes,
        q,
        cbp_luma,
        nonzero,
    }
}

/// Predicted `Intra_4x4` mode for the block at absolute coords `(bx, by)` —
/// `min` of the left/top neighbor modes, or DC if either is unavailable.
fn predict_i4_mode(fe: &FrameEncoder, bx: usize, by: usize) -> u8 {
    if bx == 0 || by == 0 {
        return 2;
    }
    let w4 = fe.mb_w * 4;
    fe.modes_y[by * w4 + (bx - 1)].min(fe.modes_y[(by - 1) * w4 + bx])
}

#[allow(clippy::too_many_arguments)]
fn encode_mb(
    fe: &mut FrameEncoder,
    w: &mut BitWriter,
    mb_x: usize,
    mb_y: usize,
    sy: &[u8],
    su: &[u8],
    sv: &[u8],
    is_p: bool,
) {
    let qp = fe.qp;
    let qpc = fe.qpc;
    // In a P-slice, intra macroblock types are offset by 5 (0..4 are inter).
    let mb_type_offset = if is_p { 5 } else { 0 };
    // Lagrangian λ for rate-distortion decisions (standard H.264 form).
    let lambda = 0.85 * 2f64.powf((qp as f64 - 12.0) / 3.0);

    // ---------------- luma ----------------
    let (lx, ly) = (mb_x * 16, mb_y * 16);
    let avail_top = mb_y > 0;
    let avail_left = mb_x > 0;
    let mut top = [0u8; 16];
    let mut left = [0u8; 16];
    if avail_top {
        for i in 0..16 {
            top[i] = fe.rec_y[(ly - 1) * fe.cw + lx + i];
        }
    }
    if avail_left {
        for i in 0..16 {
            left[i] = fe.rec_y[(ly + i) * fe.cw + lx - 1];
        }
    }
    let corner = if avail_top && avail_left {
        fe.rec_y[(ly - 1) * fe.cw + lx - 1]
    } else {
        0
    };

    let w4 = fe.mb_w * 4;

    // ============ I_16x16 plan (reconstruct into a local buffer) ============
    let mut i16_mode = I16Mode::Dc;
    let mut best_pred = luma16x16_pred(I16Mode::Dc, avail_top, avail_left, &top, &left, corner);
    let mut best_cost = satd_16x16(sy, fe.cw, lx, ly, &best_pred);
    for mode in [I16Mode::Vertical, I16Mode::Horizontal, I16Mode::Plane] {
        if !mode.available(avail_top, avail_left) {
            continue;
        }
        let pred = luma16x16_pred(mode, avail_top, avail_left, &top, &left, corner);
        let cost = satd_16x16(sy, fe.cw, lx, ly, &pred);
        if cost < best_cost {
            best_cost = cost;
            i16_mode = mode;
            best_pred = pred;
        }
    }
    let mut dc4x4 = [0i32; 16];
    let mut i16_q = [[0i32; 16]; 16];
    for by in 0..4 {
        for bx in 0..4 {
            let predb = pred_block(&best_pred, bx, by);
            let coeffs = forward_core(&residual(sy, fe.cw, lx + bx * 4, ly + by * 4, &predb));
            dc4x4[by * 4 + bx] = coeffs[0];
            let mut q = quantize(&coeffs, qp, true);
            q[0] = 0;
            i16_q[by * 4 + bx] = q;
        }
    }
    let i16_dc_levels = forward_quant_luma_dc(&dc4x4, qp, true);
    let i16_recon_dc = inverse_quant_luma_dc(&i16_dc_levels, qp);
    let i16_cbp15 = i16_q.iter().any(|b| b[1..].iter().any(|&c| c != 0));
    let mut recon16 = [0u8; 256];
    for by in 0..4 {
        for bx in 0..4 {
            let mut deq = dequantize(&i16_q[by * 4 + bx], qp);
            deq[0] = i16_recon_dc[by * 4 + bx];
            let s = reconstruct_4x4(&deq, &pred_block(&best_pred, bx, by));
            for dy in 0..4 {
                for dx in 0..4 {
                    recon16[(by * 4 + dy) * 16 + (bx * 4 + dx)] = s[dy * 4 + dx];
                }
            }
        }
    }
    let i16_dc_nz = i16_dc_levels.iter().filter(|&&v| v != 0).count() as i64;
    let i16_ac_nz: i64 = i16_q
        .iter()
        .map(|b| b[1..].iter().filter(|&&v| v != 0).count() as i64)
        .sum();
    // I_16x16 AC is all-or-nothing: any AC ⇒ all 16 blocks pay a coeff_token.
    let i16_rate = i16_dc_nz + i16_ac_nz + if i16_cbp15 { 16 } else { 0 };
    // Reconstruction distortion (SSD) for the rate-distortion decision.
    let mut ssd16 = 0i64;
    for dy in 0..16 {
        for dx in 0..16 {
            let d = recon16[dy * 16 + dx] as i64 - sy[(ly + dy) * fe.cw + (lx + dx)] as i64;
            ssd16 += d * d;
        }
    }

    // ============ chroma (shared by both luma types; commit immediately) ============
    let (cx, cy) = (mb_x * 8, mb_y * 8);
    // Gather both components' neighbors, then pick a chroma mode by combined SATD.
    let mut ntop = [[0u8; 8]; 2];
    let mut nleft = [[0u8; 8]; 2];
    let mut ncorner = [0u8; 2];
    for c in 0..2 {
        let rec_c = if c == 0 { &fe.rec_u } else { &fe.rec_v };
        if avail_top {
            for i in 0..8 {
                ntop[c][i] = rec_c[(cy - 1) * fe.ccw + cx + i];
            }
        }
        if avail_left {
            for i in 0..8 {
                nleft[c][i] = rec_c[(cy + i) * fe.ccw + cx - 1];
            }
        }
        if avail_top && avail_left {
            ncorner[c] = rec_c[(cy - 1) * fe.ccw + cx - 1];
        }
    }
    let mut chroma_mode = 0u8;
    let mut best_c_cost = i64::MAX;
    for m in 0..4u8 {
        if !chroma_mode_available(m, avail_top, avail_left) {
            continue;
        }
        let mut cost = 0i64;
        for c in 0..2 {
            let src = if c == 0 { su } else { sv };
            let pred8 = chroma8x8_pred(m, avail_top, avail_left, &ntop[c], &nleft[c], ncorner[c]);
            cost += satd_8x8(src, fe.ccw, cx, cy, &pred8);
        }
        if cost < best_c_cost {
            best_c_cost = cost;
            chroma_mode = m;
        }
    }

    let mut c_dc_levels = [[0i32; 4]; 2];
    let mut c_q_blocks = [[[0i32; 16]; 4]; 2];
    let mut any_chroma_ac = false;
    let mut any_chroma_dc = false;
    for c in 0..2 {
        let src = if c == 0 { su } else { sv };
        let pred8 =
            chroma8x8_pred(chroma_mode, avail_top, avail_left, &ntop[c], &nleft[c], ncorner[c]);
        let mut dc2x2 = [0i32; 4];
        let mut qbs = [[0i32; 16]; 4];
        for &(bx, by) in &CHROMA_4X4_SCAN_XY {
            let mut predb = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    predb[dy * 4 + dx] = pred8[(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                }
            }
            let coeffs = forward_core(&residual(src, fe.ccw, cx + bx * 4, cy + by * 4, &predb));
            dc2x2[by * 2 + bx] = coeffs[0];
            let mut q = quantize(&coeffs, qpc, true);
            q[0] = 0;
            qbs[by * 2 + bx] = q;
            if q[1..].iter().any(|&v| v != 0) {
                any_chroma_ac = true;
            }
        }
        let dl = forward_quant_chroma_dc(&dc2x2, qpc, true);
        if dl.iter().any(|&v| v != 0) {
            any_chroma_dc = true;
        }
        let recon_dc = inverse_quant_chroma_dc(&dl, qpc);
        // commit chroma reconstruction
        for &(bx, by) in &CHROMA_4X4_SCAN_XY {
            let mut predb = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    predb[dy * 4 + dx] = pred8[(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                }
            }
            let mut deq = dequantize(&qbs[by * 2 + bx], qpc);
            deq[0] = recon_dc[by * 2 + bx];
            let s = reconstruct_4x4(&deq, &predb);
            let plane = if c == 0 { &mut fe.rec_u } else { &mut fe.rec_v };
            store(plane, fe.ccw, cx + bx * 4, cy + by * 4, &s);
        }
        c_dc_levels[c] = dl;
        c_q_blocks[c] = qbs;
    }
    let cbp_chroma: u32 = if any_chroma_ac {
        2
    } else if any_chroma_dc {
        1
    } else {
        0
    };

    // ============ I_4x4 plan + rate-distortion decision ============
    // Early-termination: when I_16x16 already predicts the macroblock almost
    // perfectly, I_4x4 (with its per-block mode overhead) cannot win — skip its
    // expensive 9-mode-per-block search entirely.
    let i4 = if i16_rate > 2 {
        Some(plan_i4x4(fe, sy, mb_x, mb_y, qp))
    } else {
        None
    };
    let use_i4 = match &i4 {
        Some(p) => {
            let mut ssd4 = 0i64;
            for dy in 0..16 {
                for dx in 0..16 {
                    let d = fe.rec_y[(ly + dy) * fe.cw + (lx + dx)] as i64
                        - sy[(ly + dy) * fe.cw + (lx + dx)] as i64;
                    ssd4 += d * d;
                }
            }
            // J = SSD + λ·R; I_4x4 pays ~16 bits of mode/CBP signalling overhead.
            let j16 = ssd16 as f64 + lambda * i16_rate as f64;
            let j4 = ssd4 as f64 + lambda * (p.nonzero + 16) as f64;
            j4 < j16
        }
        None => false,
    };

    // ============ emit luma ============
    if use_i4 {
        let i4 = i4.as_ref().unwrap();
        // rec_y already holds the I_4x4 reconstruction from plan_i4x4.
        let cbp = i4.cbp_luma | (cbp_chroma << 4);
        w.write_ue(mb_type_offset); // mb_type = I_4x4 (+5 in P-slices)
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let predicted = predict_i4_mode(fe, bx, by);
            let actual = i4.modes[lby * 4 + lbx];
            if actual == predicted {
                w.write_bit(true);
            } else {
                w.write_bit(false);
                let rem = if actual < predicted { actual } else { actual - 1 };
                w.write_bits(rem as u32, 3);
            }
            fe.modes_y[by * w4 + bx] = actual;
        }
        w.write_ue(chroma_mode as u32); // intra_chroma_pred_mode
        write_cbp_intra(w, cbp);
        if cbp != 0 {
            w.write_se(0); // mb_qp_delta
        }
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            if i4.cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = nc_from_neighbors(
                    fe.luma_nnz(bx as isize - 1, by as isize),
                    fe.luma_nnz(bx as isize, by as isize - 1),
                );
                let mut scan16 = [0i32; 16];
                for i in 0..16 {
                    scan16[i] = i4.q[lby * 4 + lbx][ZIGZAG_4X4[i]];
                }
                let total = scan16.iter().filter(|&&v| v != 0).count() as u8;
                encode_residual_block(w, &scan16, 16, nc);
                fe.nnz_y[by * w4 + bx] = total;
            } else {
                fe.nnz_y[by * w4 + bx] = 0;
            }
        }
    } else {
        // commit the I_16x16 reconstruction and mark modes as DC.
        for by in 0..4 {
            for bx in 0..4 {
                for dy in 0..4 {
                    for dx in 0..4 {
                        fe.rec_y[(ly + by * 4 + dy) * fe.cw + (lx + bx * 4 + dx)] =
                            recon16[(by * 4 + dy) * 16 + (bx * 4 + dx)];
                    }
                }
            }
        }
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
        let mb_type = 1 + i16_mode as u32 + 4 * cbp_chroma + if i16_cbp15 { 12 } else { 0 };
        w.write_ue(mb_type + mb_type_offset);
        w.write_ue(chroma_mode as u32); // intra_chroma_pred_mode
        w.write_se(0); // mb_qp_delta
        let nc_dc = nc_from_neighbors(
            fe.luma_nnz(mb_x as isize * 4 - 1, mb_y as isize * 4),
            fe.luma_nnz(mb_x as isize * 4, mb_y as isize * 4 - 1),
        );
        let mut dc_scan = [0i32; 16];
        for i in 0..16 {
            dc_scan[i] = i16_dc_levels[ZIGZAG_4X4[i]];
        }
        encode_residual_block(w, &dc_scan, 16, nc_dc);
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
        }
        if i16_cbp15 {
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let nc = nc_from_neighbors(
                    fe.luma_nnz(bx as isize - 1, by as isize),
                    fe.luma_nnz(bx as isize, by as isize - 1),
                );
                let q = &i16_q[lby * 4 + lbx];
                let mut ac = [0i32; 15];
                for i in 0..15 {
                    ac[i] = q[ZIGZAG_4X4[i + 1]];
                }
                let total = ac.iter().filter(|&&v| v != 0).count() as u8;
                encode_residual_block(w, &ac, 15, nc);
                fe.nnz_y[by * w4 + bx] = total;
            }
        }
    }

    // ============ emit chroma residual (shared) ============
    if cbp_chroma != 0 {
        for c in 0..2 {
            encode_residual_block(w, &c_dc_levels[c], 4, -1);
        }
    }
    if cbp_chroma == 2 {
        for c in 0..2 {
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let abx = mb_x as isize * 2 + bx as isize;
                let aby = mb_y as isize * 2 + by as isize;
                let nc = nc_from_neighbors(
                    fe.chroma_nnz(c, abx - 1, aby),
                    fe.chroma_nnz(c, abx, aby - 1),
                );
                let q = &c_q_blocks[c][by * 2 + bx];
                let mut ac = [0i32; 15];
                for i in 0..15 {
                    ac[i] = q[ZIGZAG_4X4[i + 1]];
                }
                let total = ac.iter().filter(|&&v| v != 0).count() as u8;
                encode_residual_block(w, &ac, 15, nc);
                fe.nnz_c[c][aby as usize * (fe.mb_w * 2) + abx as usize] = total;
            }
        }
    }

    // Mark all luma blocks coded for the next macroblock's top-right availability.
    for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
        fe.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
    }
}
