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
use rusty_h264_common::cavlc::{
    encode_residual_block, scan_4x4_ac, scan_4x4_dcac, write_cbp_inter, write_cbp_intra,
};
use rusty_h264_common::inter::{
    inter_partitions, mc_chroma, mc_luma, predict_mv, predict_partition_mv, MvNeighbor,
};
use rusty_h264_common::predict::{
    add_residual_4x4, chroma8x8_pred, chroma_mode_available, chroma_qp, intra4x4_pred,
    luma16x16_pred, reconstruct_4x4, I16Mode, CHROMA_4X4_SCAN_XY, LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{
    dequantize, forward_core, forward_dct_blocks, forward_quant_chroma_dc, forward_quant_luma_dc,
    inverse_dct_blocks, inverse_quant_chroma_dc, inverse_quant_luma_dc, quantize, satd_4x4_sum,
};
use rusty_h264_common::aligned::AlignedBytes;
use rusty_h264_common::{BitWriter, YuvFrame};

/// A 16-byte-aligned 16×16 luma block — the aligned `op1` openh264's SSE2 SAD/SATD
/// kernels require (`movdqa`). Safe to construct (`forbid(unsafe)` holds); the asm
/// FFI that consumes it lives in `rusty_h264-accel`. Only used on the `asm` feature.
#[cfg(accel)]
#[repr(align(16))]
struct AlignedMb([u8; 256]);

/// 16-byte-aligned 256-`i16` DCT/coefficient buffer — the in-place `movdqa` quant
/// kernel (`WelsQuantFour4x4_sse2`) requires aligned coefficients. `asm`-feature only.
#[cfg(accel)]
#[repr(align(16))]
struct AlignedDct([i16; 256]);


/// Per-frame intra encoder state: reconstructed planes (coded size) and the
/// per-4×4-block non-zero-coefficient counts used for CAVLC context.
pub struct FrameEncoder {
    mb_w: usize,
    mb_h: usize,
    qp: u8,
    qpc: u8,
    cw: usize, // coded luma width
    ccw: usize, // coded chroma width
    // 16-byte aligned (the openh264 deblock/MC/intra asm load aligned row chunks).
    rec_y: AlignedBytes,
    rec_u: AlignedBytes,
    rec_v: AlignedBytes,
    nnz_y: Vec<u8>,    // (mb_w*4) x (mb_h*4)
    nnz_c: [Vec<u8>; 2], // each (mb_w*2) x (mb_h*2)
    modes_y: Vec<u8>,  // intra4x4 mode per 4×4 block (2=DC for I_16x16 blocks)
    coded_y: Vec<bool>, // whether each 4×4 block is reconstructed (top-right avail)
    mv_y: Vec<(i32, i32)>, // motion vector per 4×4 block (quarter-pel)
    inter_y: Vec<bool>, // whether each 4×4 block is inter-coded
    ref_idx_y: Vec<i32>, // reference index per 4×4 block (-1 = intra/uncoded)
    idz: i64, // intra dead-zone divisor: 2 for all-intra, 3 when frames reference each other
    fast: bool, // Preset::Fast — SATD mode decision (no RDO), 16×16/I_16x16 only
    skip_accel_check: bool, // A/B knob: whole-MB psadbw gate in the P_Skip free-check
    // Per-MB luma nnz prediction cache (openh264 scan8 style): a padded 5×5 grid,
    // block (lbx,lby) at (lby+1)*5+(lbx+1); row 0 = top neighbours, col 0 = left.
    // Unavailable edges hold the sentinel 0x80, so the nnz predict is branchless.
    nnz_l_cache: [u8; 25],
    // Same, per chroma plane: a padded 3×3 grid for the 2×2 chroma blocks.
    nnz_c_cache: [[u8; 9]; 2],
    // openh264 predicted-SAD skip apparatus (per MB, mb_w×mb_h): the P_Skip
    // prediction's luma SAD, and whether the MB was actually skipped. The greedy
    // skip threshold for an MB is the median of its skip *neighbours'* skip SADs
    // (`PredictSadSkip`) — so skip propagates only from already-skip regions
    // (seeded by free skips) and self-limits, instead of a fixed bound that drifts.
    mb_skip_sad: Vec<u32>,
    mb_was_skip: Vec<bool>,
}

/// A chosen inter coding for a macroblock: `mb_type` and, per partition, the
/// reference index and motion vector.
type InterChoice = (u8, Vec<(i32, (i32, i32))>);

/// Approximate marginal rate (bits) of one `P_Skip` — it only lengthens the
/// surrounding `mb_skip_run` Exp-Golomb code slightly.
const SKIP_RATE_BITS: f64 = 1.0;

/// RDO early-termination gate. Sub-partitions (16×8 / 8×16) only help at motion
/// boundaries, which show up as a heavy 16×16 residual; below this many coded bits
/// the 16×16 already fits, so skip their motion search and trials. (Intra is *not*
/// gated — it can win even against a cheap inter prediction, so gating it on inter
/// cost regresses compression badly on textured content.)
const SPLIT_GATE_BITS: f64 = 60.0;

/// Fast preset: signalling-cost penalty (in bits, SATD-weighted by √λ) charged to
/// the intra candidate so it only wins a P-macroblock when its prediction is
/// clearly better than inter — intra's `mb_type` + modes cost more to signal.
const FAST_INTRA_PENALTY_BITS: f64 = 24.0;

/// A snapshot of one macroblock's per-block grids and reconstruction region,
/// used to roll back a trial encode during RD mode decision.
struct MbState {
    rec_y: Vec<u8>,
    rec_u: Vec<u8>,
    rec_v: Vec<u8>,
    nnz_y: Vec<u8>,
    nnz_c: [Vec<u8>; 2],
    mv_y: Vec<(i32, i32)>,
    inter_y: Vec<bool>,
    ref_idx_y: Vec<i32>,
    coded_y: Vec<bool>,
    modes_y: Vec<u8>,
}

/// Edge-clamped, coded-size source planes (luma, Cb, Cr).
fn coded_source(cfg: &EncoderConfig, frame: &YuvFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncSource);
    let cw = cfg.mb_width() * 16;
    let ch = cfg.mb_height() * 16;
    // MB-aligned frame: the clamp is the identity — a plane memcpy (clone) replaces
    // the per-pixel clamp loop (bit-exact: same bytes).
    if frame.width == cw && frame.height == ch {
        return (frame.y.clone(), frame.u.clone(), frame.v.clone());
    }
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
            rec_y: AlignedBytes::zeroed(cw * ch),
            rec_u: AlignedBytes::zeroed(ccw * cch),
            rec_v: AlignedBytes::zeroed(ccw * cch),
            nnz_y: vec![0; (mb_w * 4) * (mb_h * 4)],
            nnz_c: [vec![0; (mb_w * 2) * (mb_h * 2)], vec![0; (mb_w * 2) * (mb_h * 2)]],
            modes_y: vec![2; (mb_w * 4) * (mb_h * 4)],
            coded_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            mv_y: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            inter_y: vec![false; (mb_w * 4) * (mb_h * 4)],
            ref_idx_y: vec![-1; (mb_w * 4) * (mb_h * 4)],
            // All-intra (no inter references) tolerates the larger dead-zone; in
            // an I+P stream the IDR is a reference, so keep the standard offset.
            idz: if cfg.gop_size <= 1 { 2 } else { 3 },
            fast: cfg.preset == crate::config::Preset::Fast,
            skip_accel_check: cfg.tune_skip_accel_check,
            nnz_l_cache: [0x80; 25],
            nnz_c_cache: [[0x80; 9]; 2],
            mb_skip_sad: vec![0; mb_w * mb_h],
            mb_was_skip: vec![false; mb_w * mb_h],
        }
    }

    /// openh264 `PredictSadSkip`: the greedy P_Skip threshold = the median of the
    /// skip SADs of the *skip* neighbours (left A, top B, top-right C, top-left
    /// fallback for C). Non-skip neighbours contribute 0, so with no skip neighbour
    /// the threshold is 0 (no greedy skip). This makes the skip self-calibrating —
    /// it only spreads where a neighbour already skipped at a comparable SAD.
    fn pred_skip_sad(&self, mb_x: usize, mb_y: usize) -> u32 {
        let mbw = self.mb_w;
        let at = |x: isize, y: isize| -> Option<(bool, u32)> {
            if x < 0 || y < 0 || x >= mbw as isize {
                return None;
            }
            let i = y as usize * mbw + x as usize;
            Some((self.mb_was_skip[i], self.mb_skip_sad[i]))
        };
        let a = at(mb_x as isize - 1, mb_y as isize); // left
        let b = at(mb_x as isize, mb_y as isize - 1); // top
        let c = at(mb_x as isize + 1, mb_y as isize - 1) // top-right
            .or_else(|| at(mb_x as isize - 1, mb_y as isize - 1)); // top-left fallback
        let sad = |n: Option<(bool, u32)>| n.filter(|&(s, _)| s).map_or(0, |(_, v)| v);
        let (sa, sb, sc) = (sad(a), sad(b), sad(c));
        // B and C unavailable but A available → A only.
        if b.is_none() && c.is_none() && a.is_some() {
            return sa;
        }
        match (
            a.is_some_and(|(s, _)| s),
            b.is_some_and(|(s, _)| s),
            c.is_some_and(|(s, _)| s),
        ) {
            (true, false, false) => sa,
            (false, true, false) => sb,
            (false, false, true) => sc,
            _ => sb.max(sa.min(sc)).min(sa.max(sc)), // median(sa, sb, sc)
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
        let cw = self.cw;

        // The coarse-to-fine diamond walks only whole samples, so most candidates
        // are full-pel; when the region also lies inside the frame, the prediction
        // is just a copy of the reference. SATD it straight against the reference,
        // skipping mc_luma's per-pixel sampling (bit-identical — same samples).
        let (ix0, iy0) = (lx as isize + (mv.0 >> 2) as isize, ly as isize + (mv.1 >> 2) as isize);
        let interior_fullpel = mv.0 & 3 == 0
            && mv.1 & 3 == 0
            && ix0 >= 0
            && iy0 >= 0
            && ix0 + rw as isize <= cw as isize
            && iy0 + rh as isize <= ch as isize;

        let src = &sy[ly * cw + lx..];
        if interior_fullpel {
            let (rx0, ry0) = (ix0 as usize, iy0 as usize);
            satd_px(src, cw, &reference.y[ry0 * cw + rx0..], cw, rw, rh)
        } else {
            let mut pred = [0u8; 256];
            mc_luma(&reference.y, cw, ch, lx, ly, rw, rh, mv.0, mv.1, &mut pred);
            satd_px(src, cw, &pred, rw, rw, rh)
        }
    }

    /// SAD (sum of absolute differences) of a motion-compensated `rw`×`rh` luma
    /// region against the source — the **fast** preset's motion-search cost.
    ///
    /// SAD is far cheaper than SATD (no Hadamard transform), and the inner loop is
    /// written as `Σ a.abs_diff(b)` over `u8` slices, the exact pattern LLVM
    /// auto-vectorizes to the `psadbw` SAD instruction — the same instruction
    /// x264's hand-written assembly uses, but reached without any `unsafe`. (x264's
    /// fast presets use SAD for the full-pel search for precisely this reason.)
    #[allow(clippy::too_many_arguments)]
    fn mc_sad(
        &self,
        reference: &crate::RefFrame,
        sy: &[u8],
        lx: usize,
        ly: usize,
        rw: usize,
        rh: usize,
        mv: (i32, i32),
        // 16-aligned source MB (built once per search) for the asm SAD; `None`
        // (and unused) on the scalar build.
        _asrc: Option<&[u8; 256]>,
    ) -> i64 {
        let ch = self.mb_h * 16;
        let cw = self.cw;
        let (ix0, iy0) = (lx as isize + (mv.0 >> 2) as isize, ly as isize + (mv.1 >> 2) as isize);
        let interior_fullpel = mv.0 & 3 == 0
            && mv.1 & 3 == 0
            && ix0 >= 0
            && iy0 >= 0
            && ix0 + rw as isize <= cw as isize
            && iy0 + rh as isize <= ch as isize;
        // Full-pel interior 16×16: openh264's `psadbw` SAD of the aligned source vs
        // the (movdqu) reference block. SAD is exact, so this is byte-identical to the
        // scalar path — a pure ME speedup (~2.4× the kernel).
        #[cfg(accel)]
        if interior_fullpel && rw == 16 && rh == 16 {
            if let Some(src) = _asrc {
                let (rx0, ry0) = (ix0 as usize, iy0 as usize);
                return rusty_h264_accel::sad_16x16(src, 16, &reference.y[ry0 * cw + rx0..], cw)
                    as i64;
            }
        }
        let mut sad = 0u32;
        if interior_fullpel {
            // Direct from the reference (a copy at full-pel) — no interpolation.
            let (rx0, ry0) = (ix0 as usize, iy0 as usize);
            let refy = &reference.y;
            for dy in 0..rh {
                let s = &sy[(ly + dy) * cw + lx..][..rw];
                let r = &refy[(ry0 + dy) * cw + rx0..][..rw];
                sad += s.iter().zip(r).map(|(&a, &b)| a.abs_diff(b) as u32).sum::<u32>();
            }
        } else {
            let mut pred = [0u8; 256];
            mc_luma(&reference.y, cw, ch, lx, ly, rw, rh, mv.0, mv.1, &mut pred);
            for dy in 0..rh {
                let s = &sy[(ly + dy) * cw + lx..][..rw];
                let p = &pred[dy * rw..][..rw];
                sad += s.iter().zip(p).map(|(&a, &b)| a.abs_diff(b) as u32).sum::<u32>();
            }
        }
        sad as i64
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
        // Build the 16-aligned source MB ONCE per search for the asm SAD path (fast
        // preset, full 16×16). Amortized over every candidate's SAD; the reference
        // block stays unaligned (movdqu). Scalar build does no copy.
        #[cfg(accel)]
        let asrc_buf = if self.fast && rw == 16 && rh == 16 {
            let mut a = AlignedMb([0u8; 256]);
            for dy in 0..16 {
                a.0[dy * 16..dy * 16 + 16].copy_from_slice(&sy[(ly + dy) * self.cw + lx..][..16]);
            }
            Some(a)
        } else {
            None
        };
        #[cfg(accel)]
        let asrc: Option<&[u8; 256]> = asrc_buf.as_ref().map(|a| &a.0);
        #[cfg(not(accel))]
        let asrc: Option<&[u8; 256]> = None;
        let cost = |mv: (i32, i32)| -> i64 {
            let rate = mvbits(mv.0 - center.0) + mvbits(mv.1 - center.1);
            // Fast preset: SAD (psadbw — asm kernel on `--features asm`, else auto-vec)
            // — far cheaper than SATD, the single biggest reason x264 fast out-runs us.
            let dist = if self.fast {
                self.mc_sad(reference, sy, lx, ly, rw, rh, mv, asrc)
            } else {
                self.mc_satd(reference, sy, lx, ly, rw, rh, mv)
            };
            dist + (lambda_me * rate as f64) as i64
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
        // The fast preset trusts the neighbour MV predictor and refines locally
        // (one coarse reach + fine), like x264's `me=dia`; quality sweeps the full
        // coarse-to-fine range. Each step's diamond still walks until no
        // improvement, so even fast reaches far motion — just in smaller hops.
        let steps: &[i32] = if self.fast { &[16, 4] } else { &[64, 32, 16, 8, 4] };
        for &step in steps {
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
        // Sub-pel refinement uses the 6-tap/bilinear interpolation — the expensive
        // per-pixel `mc_luma` path that profiling pinned at ~55% of the entire
        // encode. The fast preset skips it (integer-pel only, like x264's fastest
        // presets `subme=0`): ~3× faster, trading a little quality on sub-pixel
        // motion. The quality preset does the full half-pel + quarter-pel rings.
        let subpel: &[i32] = if self.fast { &[] } else { &[2, 1] };
        for &step in subpel {
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
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncInterCode);
        let (qp, qpc) = (self.qp, self.qpc);
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        // ---- per-partition motion compensation + MV prediction ----
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        let mut mvds = [(0i32, 0i32); 4]; // ≤4 partitions; no per-MB Vec alloc
        let mut n_mvd = 0;
        for (part, &(rx, ry, rw, rh)) in inter_partitions(mode).iter().enumerate() {
            let (refi, mv) = parts[part];
            let reference = &refs[refi as usize];
            let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
            let [a, b, c] = self.mv_neighbors_block(pbx, pby, (rw / 4) as isize);
            let pmv = predict_partition_mv(mode, part, a, b, c, refi);
            mvds[n_mvd] = (mv.0 - pmv.0, mv.1 - pmv.1);
            n_mvd += 1;
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
            // Luma MC into the partition's sub-region. A full-MB (16×16) partition is
            // the whole `pred_y`, so MC straight into it — no scratch + repack copy.
            if rw == 16 && rh == 16 {
                mc_luma(&reference.y, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred_y);
            } else {
                let mut tmp = [0u8; 256];
                mc_luma(&reference.y, self.cw, ch, mb_x * 16 + rx, mb_y * 16 + ry, rw, rh, mv.0, mv.1, &mut tmp);
                for dy in 0..rh {
                    for dx in 0..rw {
                        pred_y[(ry + dy) * 16 + (rx + dx)] = tmp[dy * rw + dx];
                    }
                }
            }
            // Chroma MC (half-resolution region); 8×8 = the whole plane prediction.
            let (crx, cry, crw, crh) = (rx / 2, ry / 2, rw / 2, rh / 2);
            for cc in 0..2 {
                let rc = if cc == 0 { &reference.u } else { &reference.v };
                if crw == 8 && crh == 8 {
                    mc_chroma(rc, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut c_pred[cc]);
                } else {
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

        // ---- luma residual + quantization ----
        let mut q_blocks = [[0i32; 16]; 16]; // raster, levels
        let mut cbp_luma = 0u32;
        #[cfg(accel)]
        {
            // openh264 `WelsDctFourT4_sse2` (fused residual+DCT) → i16, then
            // `WelsQuantFour4x4_sse2` in place — the whole DCT→quant chain stays in i16,
            // no i32 round-trip. Quant is openh264's structure carrying OUR deadzone
            // (`quant_dz_ff` + `QUANT_MF_OH`), so levels are bit-identical to `quantize`.
            let mut dctw = AlignedDct([0i16; 256]);
            let dct = &mut dctw.0;
            let base = mb_y * 16 * self.cw + mb_x * 16;
            for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
                rusty_h264_accel::dct_four_t4(
                    &mut dct[qi * 64..qi * 64 + 64],
                    &sy[base + qy * self.cw + qx..],
                    self.cw,
                    &pred_y[qy * 16 + qx..],
                    16,
                );
            }
            let ff = rusty_h264_common::transform::quant_dz_ff(qp, 6);
            let mf = &rusty_h264_common::transform::QUANT_MF_OH[qp as usize];
            for qi in 0..4 {
                rusty_h264_accel::quant_four_4x4(&mut dct[qi * 64..qi * 64 + 64], &ff, mf);
            }
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let mut nz = false;
                for i in 0..16 {
                    let v = dct[blk * 16 + i] as i32;
                    q_blocks[lby * 4 + lbx][i] = v;
                    nz |= v != 0;
                }
                if nz {
                    cbp_luma |= 1 << (blk / 4);
                }
            }
        }
        #[cfg(not(accel))]
        {
            // Scalar/`wide`: gather all 16 residual blocks, batched forward-DCT, quantize.
            let mut res_blocks = [[0i32; 16]; 16]; // raster
            for lby in 0..4 {
                for lbx in 0..4 {
                    let b = &mut res_blocks[lby * 4 + lbx];
                    for dy in 0..4 {
                        for dx in 0..4 {
                            let sx = mb_x * 16 + lbx * 4 + dx;
                            let syy = mb_y * 16 + lby * 4 + dy;
                            b[dy * 4 + dx] = sy[syy * self.cw + sx] as i32
                                - pred_y[(lby * 4 + dy) * 16 + (lbx * 4 + dx)] as i32;
                        }
                    }
                }
            }
            let mut coeffs = [[0i32; 16]; 16];
            forward_dct_blocks(&res_blocks, &mut coeffs);
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let q = quantize(&coeffs[lby * 4 + lbx], qp, 6);
                if q.iter().any(|&v| v != 0) {
                    cbp_luma |= 1 << (blk / 4);
                }
                q_blocks[lby * 4 + lbx] = q;
            }
        }

        // ---- chroma residual (prediction already built per partition) ----
        let mut c_dc_levels = [[0i32; 4]; 2];
        let mut c_recon_dc = [[0i32; 4]; 2];
        let mut c_q = [[[0i32; 16]; 4]; 2];
        let (mut any_ac, mut any_dc) = (false, false);
        for c in 0..2 {
            let src = if c == 0 { su } else { sv };
            // Gather the 4 residual blocks (raster `by*2+bx`), then batch the forward
            // DCT (`forward_dct_blocks` → SIMD `wide`), as the luma scalar path does —
            // bit-identical to `forward_core` per block.
            let mut res_blocks = [[0i32; 16]; 4];
            for by in 0..2 {
                for bx in 0..2 {
                    let b = &mut res_blocks[by * 2 + bx];
                    for dy in 0..4 {
                        for dx in 0..4 {
                            let sx = mb_x * 8 + bx * 4 + dx;
                            let syy = mb_y * 8 + by * 4 + dy;
                            b[dy * 4 + dx] = src[syy * self.ccw + sx] as i32
                                - c_pred[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                        }
                    }
                }
            }
            let mut coeffs = [[0i32; 16]; 4];
            forward_dct_blocks(&res_blocks, &mut coeffs);
            let mut dc2x2 = [0i32; 4];
            for i in 0..4 {
                dc2x2[i] = coeffs[i][0];
                let mut q = quantize(&coeffs[i], qpc, 6);
                q[0] = 0;
                if q[1..].iter().any(|&v| v != 0) {
                    any_ac = true;
                }
                c_q[c][i] = q;
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
        for &(mvdx, mvdy) in &mvds[..n_mvd] {
            w.write_se(mvdx);
            w.write_se(mvdy);
        }
        write_cbp_inter(w, cbp);
        if cbp != 0 {
            w.write_se(0); // mb_qp_delta
        }
        self.nnz_cache_load(mb_x, mb_y);
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = self.nc_pred(lbx, lby);
                let scan16 = scan_4x4_dcac(&q_blocks[lby * 4 + lbx]);
                encode_residual_block(w, &scan16, 16, nc) as u8
            } else {
                0
            };
            self.nnz_cache_set(lbx, lby, total);
            self.nnz_y[by * w4 + bx] = total;
        }
        if cbp_chroma != 0 {
            for c in 0..2 {
                encode_residual_block(w, &c_dc_levels[c], 4, -1);
            }
        }
        if cbp_chroma == 2 {
            self.chroma_cache_load(mb_x, mb_y);
            let w2 = self.mb_w * 2;
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let nc = self.chroma_nc_pred(c, bx, by);
                    let ac = scan_4x4_ac(&c_q[c][by * 2 + bx]);
                    let total = encode_residual_block(w, &ac, 15, nc) as u8;
                    self.chroma_nnz_cache_set(c, bx, by, total);
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
                }
            }
        }

        // ---- reconstruction (luma) ----
        #[cfg(accel)]
        {
            // Dequantize all 16 blocks into the 4-quadrant int16 layout (16-byte
            // aligned — the kernel uses movdqa coeff loads), then inverse-DCT + add
            // prediction + clip per quadrant via openh264. The inverse butterfly +
            // (x+32)>>6 is bit-identical to reconstruct_4x4 (verified in accel).
            #[repr(align(16))]
            struct Align16([i16; 256]);
            let mut dct_in = Align16([0i16; 256]);
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let deq = dequantize(&q_blocks[lby * 4 + lbx], qp);
                for i in 0..16 {
                    dct_in.0[blk * 16 + i] = deq[i] as i16;
                }
            }
            let base = mb_y * 16 * self.cw + mb_x * 16;
            for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
                rusty_h264_accel::idct_four_t4_rec(
                    &mut self.rec_y[base + qy * self.cw + qx..],
                    self.cw,
                    &pred_y[qy * 16 + qx..],
                    16,
                    &dct_in.0[qi * 64..qi * 64 + 64],
                );
            }
        }
        #[cfg(not(accel))]
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
            // Dequantize the 4 blocks (raster, DC overridden by the 2×2-Hadamard recon),
            // then batch the inverse DCT (`inverse_dct_blocks` → SIMD) and share the
            // add-prediction+clip tail (`add_residual_4x4`) — bit-identical to the
            // per-block `reconstruct_4x4`.
            let mut deq_blocks = [[0i32; 16]; 4];
            for i in 0..4 {
                deq_blocks[i] = dequantize(&c_q[c][i], qpc);
                deq_blocks[i][0] = c_recon_dc[c][i];
            }
            let mut res = [[0i32; 16]; 4];
            inverse_dct_blocks(&deq_blocks, &mut res);
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for by in 0..2 {
                for bx in 0..2 {
                    let mut predb = [0i32; 16];
                    for dy in 0..4 {
                        for dx in 0..4 {
                            predb[dy * 4 + dx] = c_pred[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                        }
                    }
                    let s = add_residual_4x4(&res[by * 2 + bx], &predb);
                    store(plane, self.ccw, mb_x * 8 + bx * 4, mb_y * 8 + by * 4, &s);
                }
            }
        }
        // MV grid + coded flags were set per partition; mark modes as DC.
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
    }

    /// Motion-compensates the `P_Skip` prediction (luma + both chroma) from
    /// reference 0 at the skip MV.
    /// Luma half of the P_Skip prediction. Split out so the fast path can test the
    /// luma residual first and only motion-compensate chroma when luma is free —
    /// for the majority of (non-free) macroblocks the chroma MC is never needed.
    fn skip_predict_luma(
        &self,
        refs: &[crate::RefFrame],
        mb_x: usize,
        mb_y: usize,
        mv: (i32, i32),
    ) -> [u8; 256] {
        let reference = &refs[0]; // P_Skip always references index 0
        let ch = self.mb_h * 16;
        let mut pred_y = [0u8; 256];
        mc_luma(&reference.y, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred_y);
        pred_y
    }

    /// Chroma half of the P_Skip prediction (see [`Self::skip_predict_luma`]).
    fn skip_predict_chroma(
        &self,
        refs: &[crate::RefFrame],
        mb_x: usize,
        mb_y: usize,
        mv: (i32, i32),
    ) -> [[u8; 64]; 2] {
        let reference = &refs[0];
        let cch = self.mb_h * 8;
        let mut pred_c = [[0u8; 64]; 2];
        for c in 0..2 {
            let rc = if c == 0 { &reference.u } else { &reference.v };
            mc_chroma(rc, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut pred_c[c]);
        }
        pred_c
    }

    /// Whether the luma half of the P_Skip prediction has an all-zero quantized
    /// residual. Tested first and independently so the caller can defer the chroma
    /// MC + test for the common case where luma already disqualifies the skip (a
    /// "free", exact P_Skip costs no bits and is strictly beneficial).
    fn skip_luma_is_free(&self, sy: &[u8], mb_x: usize, mb_y: usize, pred_y: &[u8; 256]) -> bool {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncFree);
        let qp = self.qp;
        // Fast path (deployment): the SAME asm kernels the coding path uses —
        // `dct_four_t4` computes the 4x4 DCTs of (src - pred) for an 8x8 quad
        // STRAIGHT FROM THE PLANES (no scalar gather), `quant_four_4x4` quantizes
        // with the identical FF/MF math as scalar `quantize` (bit-identical), and
        // "free" = all 64 levels zero, which is order-independent. Per-quad early
        // exit. The knob interleaves this against the scalar twin for A/B.
        #[cfg(accel)]
        if self.skip_accel_check {
            struct Align16([i16; 64]);
            let mut dct = Align16([0i16; 64]);
            let ff = rusty_h264_common::transform::quant_dz_ff(qp, 6);
            let mf = &rusty_h264_common::transform::QUANT_MF_OH[qp as usize];
            for &(qx, qy) in &[(0usize, 0usize), (8, 0), (0, 8), (8, 8)] {
                rusty_h264_accel::dct_four_t4(
                    &mut dct.0,
                    &sy[(mb_y * 16 + qy) * self.cw + mb_x * 16 + qx..],
                    self.cw,
                    &pred_y[qy * 16 + qx..],
                    16,
                );
                rusty_h264_accel::quant_four_4x4(&mut dct.0, &ff, mf);
                if dct.0.iter().any(|&v| v != 0) {
                    return false;
                }
            }
            return true;
        }
        // Exact quantize-to-zero bounds (mirrors `quantize`: level != 0 iff
        // (|c| + ff[p])·mf_oh[p] >= 2^16). With |C_ij| <= 4·SAD (max |H| entry = 2)
        // and C_DC = Σres, most blocks are decided by one SAD/sum pass — the full
        // scalar DCT+quant proof only runs for the rare undecided middle band.
        // BIT-EXACT: both shortcuts are sufficient conditions of the exact check.
        let mf = &rusty_h264_common::transform::QUANT_MF_OH[qp as usize];
        let ff = rusty_h264_common::transform::quant_dz_ff(qp, 6);
        let mut t_min = i32::MAX;
        for p in 0..8 {
            let t = (65536 + mf[p] as i32 - 1) / mf[p] as i32 - ff[p] as i32;
            t_min = t_min.min(t);
        }
        let t_dc = (65536 + mf[0] as i32 - 1) / mf[0] as i32 - ff[0] as i32;
        // Whole-MB gate: SAD(any 4x4) <= SAD(MB), so 4*SAD_MB < T_min proves all 16
        // blocks quantize to zero from ONE (psadbw) SAD. On skip-heavy content most
        // free MBs are exact/near-exact copies (SAD_MB ~ 0) - they skip the whole
        // per-block walk. Not-free MBs pay one extra SAD (~2% of their check).
        for by in 0..4 {
            for bx in 0..4 {
                let mut res = [0i32; 16];
                let (mut sad, mut dc) = (0i32, 0i32);
                for dy in 0..4 {
                    for dx in 0..4 {
                        let sx = mb_x * 16 + bx * 4 + dx;
                        let syy = mb_y * 16 + by * 4 + dy;
                        let d = sy[syy * self.cw + sx] as i32
                            - pred_y[(by * 4 + dy) * 16 + (bx * 4 + dx)] as i32;
                        res[dy * 4 + dx] = d;
                        sad += d.abs();
                        dc += d;
                    }
                }
                if 4 * sad < t_min {
                    continue; // every |C| <= 4·SAD < T_min → all levels zero
                }
                if dc.abs() >= t_dc {
                    return false; // DC level provably nonzero
                }
                if quantize(&forward_core(&res), qp, 6).iter().any(|&v| v != 0) {
                    return false;
                }
            }
        }
        true
    }

    /// Chroma half of [`Self::skip_is_free`].
    fn skip_chroma_is_free(
        &self,
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        pred_c: &[[u8; 64]; 2],
    ) -> bool {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncFree);
        let qpc = self.qpc;
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
                            - pred_c[c][(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                    }
                }
                let coeffs = forward_core(&res);
                dc2x2[by * 2 + bx] = coeffs[0];
                if quantize(&coeffs, qpc, 6)[1..].iter().any(|&v| v != 0) {
                    return false;
                }
            }
            if forward_quant_chroma_dc(&dc2x2, qpc, false).iter().any(|&v| v != 0) {
                return false;
            }
        }
        true
    }

    /// SSD between the source and a macroblock prediction (luma + chroma).
    #[allow(clippy::too_many_arguments)]
    fn pred_ssd(
        &self,
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        pred_y: &[u8; 256],
        pred_c: &[[u8; 64]; 2],
    ) -> i64 {
        let mut ssd = 0i64;
        for dy in 0..16 {
            for dx in 0..16 {
                let d = sy[(mb_y * 16 + dy) * self.cw + mb_x * 16 + dx] as i64
                    - pred_y[dy * 16 + dx] as i64;
                ssd += d * d;
            }
        }
        for c in 0..2 {
            let src = if c == 0 { su } else { sv };
            for dy in 0..8 {
                for dx in 0..8 {
                    let d = src[(mb_y * 8 + dy) * self.ccw + mb_x * 8 + dx] as i64
                        - pred_c[c][dy * 8 + dx] as i64;
                    ssd += d * d;
                }
            }
        }
        ssd
    }

    /// SSD between the *reconstructed* macroblock and the source.
    fn mb_ssd(&self, sy: &[u8], su: &[u8], sv: &[u8], mb_x: usize, mb_y: usize) -> i64 {
        let mut ssd = 0i64;
        for dy in 0..16 {
            for dx in 0..16 {
                let i = (mb_y * 16 + dy) * self.cw + mb_x * 16 + dx;
                let d = sy[i] as i64 - self.rec_y[i] as i64;
                ssd += d * d;
            }
        }
        for c in 0..2 {
            let (src, rec) = if c == 0 { (su, &self.rec_u) } else { (sv, &self.rec_v) };
            for dy in 0..8 {
                for dx in 0..8 {
                    let i = (mb_y * 8 + dy) * self.ccw + mb_x * 8 + dx;
                    let d = src[i] as i64 - rec[i] as i64;
                    ssd += d * d;
                }
            }
        }
        ssd
    }

    /// Reconstructs a `P_Skip` macroblock (reconstruction *is* the prediction —
    /// no residual coded) and records its motion state.
    fn commit_skip(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        mv: (i32, i32),
        pred_y: &[u8; 256],
        pred_c: &[[u8; 64]; 2],
    ) {
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
        let w4 = self.mb_w * 4;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
            self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
        }
    }

    /// Trial-encodes an inter macroblock to measure its rate-distortion cost
    /// `(SSD, bits)` without committing: snapshot the macroblock's grid + recon
    /// region, run the real `encode_inter_mb` into a scratch writer, read the
    /// bit count and reconstruction SSD, then restore. Neighbor CAVLC context is
    /// read (not mutated), so the bit count is accurate.
    #[allow(clippy::too_many_arguments)]
    fn trial_inter(
        &mut self,
        refs: &[crate::RefFrame],
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        parts: &[(i32, (i32, i32))],
    ) -> (i64, usize) {
        let snap = self.save_mb(mb_x, mb_y);
        let mut scratch = BitWriter::new();
        self.encode_inter_mb(&mut scratch, refs, sy, su, sv, mb_x, mb_y, mode, parts);
        let bits = scratch.bit_len();
        let ssd = self.mb_ssd(sy, su, sv, mb_x, mb_y);
        self.load_mb(mb_x, mb_y, &snap);
        (ssd, bits)
    }

    /// Trial-encodes the macroblock as **intra** (`encode_mb` runs its own
    /// I_16x16-vs-I_4x4 decision), measuring `(SSD, bits)` without committing —
    /// the intra candidate for the RD mode decision.
    fn trial_intra(
        &mut self,
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        is_p: bool,
    ) -> (i64, usize) {
        let snap = self.save_mb(mb_x, mb_y);
        let mut scratch = BitWriter::new();
        encode_mb(self, &mut scratch, mb_x, mb_y, sy, su, sv, is_p);
        let bits = scratch.bit_len();
        let ssd = self.mb_ssd(sy, su, sv, mb_x, mb_y);
        self.load_mb(mb_x, mb_y, &snap);
        (ssd, bits)
    }

    /// Best `(ref_idx, mv, cost)` for one partition by `SATD + λ·bits`, searched
    /// across every reference (`cost` is that SATD-domain rate-distortion cost).
    /// `extra` seeds the search with already-found MVs (e.g. the 16×16 result when
    /// refining a sub-partition).
    #[allow(clippy::too_many_arguments)]
    fn best_part(
        &self,
        refs: &[crate::RefFrame],
        sy: &[u8],
        nb: &[MvNeighbor; 3],
        num_refs: usize,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
        extra: &[(i32, i32)],
        lme: f64,
    ) -> (i32, (i32, i32), i64) {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncMe);
        let [a, b, c] = *nb;
        let (mut br, mut bmv, mut bc) = (0i32, (0, 0), i64::MAX);
        for r in 0..num_refs {
            let mut seeds = vec![predict_mv(a, b, c, r as i32)];
            seeds.extend_from_slice(extra);
            let (mv, cost) = self.motion_search(&refs[r], sy, rx, ry, rw, rh, &seeds, lme);
            let cost = cost + (lme * ref_bits(r, num_refs) as f64) as i64;
            if cost < bc {
                bc = cost;
                br = r as i32;
                bmv = mv;
            }
        }
        (br, bmv, bc)
    }

    /// Cheapest `I_16x16` prediction's SAD over the four whole-block modes, using
    /// the already-reconstructed top/left neighbours — the intra candidate's cost
    /// in the fast (SAD) mode decision, without the full `I_4x4` search.
    fn best_i16_sad(&self, sy: &[u8], mb_x: usize, mb_y: usize) -> i64 {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncIntraCost);
        let (lx, ly) = (mb_x * 16, mb_y * 16);
        let (avail_top, avail_left) = (mb_y > 0, mb_x > 0);
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
            if !mode.available(avail_top, avail_left) {
                continue;
            }
            let pred = i16_pred(self, mode, avail_top, avail_left, &top, &left, corner, lx, ly);
            best = best.min(sad_16x16(sy, self.cw, lx, ly, &pred));
        }
        best
    }

    /// SATD sibling of [`Self::best_i16_sad`] — the intra candidate's cost in the
    /// quality preset's SATD mode decision (openh264's `WelsMdI16x16`).
    fn best_i16_satd(&self, sy: &[u8], mb_x: usize, mb_y: usize) -> i64 {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncIntraCost);
        let (lx, ly) = (mb_x * 16, mb_y * 16);
        let (avail_top, avail_left) = (mb_y > 0, mb_x > 0);
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
            if !mode.available(avail_top, avail_left) {
                continue;
            }
            let pred = i16_pred(self, mode, avail_top, avail_left, &top, &left, corner, lx, ly);
            best = best.min(satd_16x16(sy, self.cw, lx, ly, &pred));
        }
        best
    }

    /// Snapshots the per-block grids and reconstruction for one macroblock, so a
    /// trial encode can be rolled back.
    fn save_mb(&self, mb_x: usize, mb_y: usize) -> MbState {
        let w4 = self.mb_w * 4;
        let w2 = self.mb_w * 2;
        macro_rules! reg4 {
            ($v:expr) => {{
                let mut o = Vec::with_capacity(16);
                for dy in 0..4 {
                    for dx in 0..4 {
                        o.push($v[(mb_y * 4 + dy) * w4 + mb_x * 4 + dx]);
                    }
                }
                o
            }};
        }
        macro_rules! regn {
            ($v:expr, $n:expr, $ox:expr, $oy:expr, $stride:expr) => {{
                let mut o = Vec::with_capacity($n * $n);
                for dy in 0..$n {
                    for dx in 0..$n {
                        o.push($v[($oy + dy) * $stride + $ox + dx]);
                    }
                }
                o
            }};
        }
        MbState {
            rec_y: regn!(self.rec_y, 16, mb_x * 16, mb_y * 16, self.cw),
            rec_u: regn!(self.rec_u, 8, mb_x * 8, mb_y * 8, self.ccw),
            rec_v: regn!(self.rec_v, 8, mb_x * 8, mb_y * 8, self.ccw),
            nnz_y: reg4!(self.nnz_y),
            nnz_c: [
                regn!(self.nnz_c[0], 2, mb_x * 2, mb_y * 2, w2),
                regn!(self.nnz_c[1], 2, mb_x * 2, mb_y * 2, w2),
            ],
            mv_y: reg4!(self.mv_y),
            inter_y: reg4!(self.inter_y),
            ref_idx_y: reg4!(self.ref_idx_y),
            coded_y: reg4!(self.coded_y),
            modes_y: reg4!(self.modes_y),
        }
    }

    /// Restores a macroblock's grids + reconstruction from a [`save_mb`] snapshot.
    fn load_mb(&mut self, mb_x: usize, mb_y: usize, s: &MbState) {
        let w4 = self.mb_w * 4;
        let w2 = self.mb_w * 2;
        macro_rules! put4 {
            ($v:expr, $src:expr) => {
                for dy in 0..4 {
                    for dx in 0..4 {
                        $v[(mb_y * 4 + dy) * w4 + mb_x * 4 + dx] = $src[dy * 4 + dx];
                    }
                }
            };
        }
        macro_rules! putn {
            ($v:expr, $src:expr, $n:expr, $ox:expr, $oy:expr, $stride:expr) => {
                for dy in 0..$n {
                    for dx in 0..$n {
                        $v[($oy + dy) * $stride + $ox + dx] = $src[dy * $n + dx];
                    }
                }
            };
        }
        putn!(self.rec_y, s.rec_y, 16, mb_x * 16, mb_y * 16, self.cw);
        putn!(self.rec_u, s.rec_u, 8, mb_x * 8, mb_y * 8, self.ccw);
        putn!(self.rec_v, s.rec_v, 8, mb_x * 8, mb_y * 8, self.ccw);
        put4!(self.nnz_y, s.nnz_y);
        putn!(self.nnz_c[0], s.nnz_c[0], 2, mb_x * 2, mb_y * 2, w2);
        putn!(self.nnz_c[1], s.nnz_c[1], 2, mb_x * 2, mb_y * 2, w2);
        put4!(self.mv_y, s.mv_y);
        put4!(self.inter_y, s.inter_y);
        put4!(self.ref_idx_y, s.ref_idx_y);
        put4!(self.coded_y, s.coded_y);
        put4!(self.modes_y, s.modes_y);
    }

    /// Loads the per-MB luma nnz prediction cache (openh264 `scan8` style): the top
    /// row from the macroblock above and the left column from the macroblock to the
    /// left (both already in `nnz_y`), with `0x80` at the picture edges. After this,
    /// neighbour nnz reads are branchless cache indexing — no bounds-checked `Option`.
    fn nnz_cache_load(&mut self, mb_x: usize, mb_y: usize) {
        let w4 = self.mb_w * 4;
        for lbx in 0..4 {
            self.nnz_l_cache[1 + lbx] = if mb_y == 0 {
                0x80
            } else {
                self.nnz_y[(mb_y * 4 - 1) * w4 + (mb_x * 4 + lbx)]
            };
        }
        for lby in 0..4 {
            self.nnz_l_cache[(lby + 1) * 5] = if mb_x == 0 {
                0x80
            } else {
                self.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 - 1)]
            };
        }
    }

    /// Branchless nnz prediction (`nC`) for luma block `(lbx,lby)` from the cache —
    /// the `0x80` sentinel + `& 0x7f` mask collapse the four availability cases
    /// (matches the scalar nnz predict). Call after the block's left/top are cached.
    #[inline]
    fn nc_pred(&self, lbx: usize, lby: usize) -> i32 {
        let left = self.nnz_l_cache[(lby + 1) * 5 + lbx] as i32; // (lbx-1)+1
        let top = self.nnz_l_cache[lby * 5 + (lbx + 1)] as i32; // (lby-1)+1
        let r = left + top;
        if r < 0x80 {
            (r + 1) >> 1
        } else {
            r & 0x7f
        }
    }

    /// Records a luma block's nnz into the per-MB cache (for later neighbour reads).
    #[inline]
    fn nnz_cache_set(&mut self, lbx: usize, lby: usize, total: u8) {
        self.nnz_l_cache[(lby + 1) * 5 + (lbx + 1)] = total;
    }

    /// Loads the per-MB chroma nnz prediction cache (both planes) from the chroma
    /// blocks above/left, `0x80` at the picture edges — the chroma analogue of
    /// [`Self::nnz_cache_load`] (2×2 blocks → padded 3×3 grid).
    fn chroma_cache_load(&mut self, mb_x: usize, mb_y: usize) {
        let w2 = self.mb_w * 2;
        for c in 0..2 {
            for bx in 0..2 {
                self.nnz_c_cache[c][1 + bx] = if mb_y == 0 {
                    0x80
                } else {
                    self.nnz_c[c][(mb_y * 2 - 1) * w2 + (mb_x * 2 + bx)]
                };
            }
            for by in 0..2 {
                self.nnz_c_cache[c][(by + 1) * 3] = if mb_x == 0 {
                    0x80
                } else {
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 - 1)]
                };
            }
        }
    }

    /// Branchless chroma nnz prediction (`nC`) for plane `c`, block `(bx,by)`.
    #[inline]
    fn chroma_nc_pred(&self, c: usize, bx: usize, by: usize) -> i32 {
        let left = self.nnz_c_cache[c][(by + 1) * 3 + bx] as i32;
        let top = self.nnz_c_cache[c][by * 3 + (bx + 1)] as i32;
        let r = left + top;
        if r < 0x80 {
            (r + 1) >> 1
        } else {
            r & 0x7f
        }
    }

    /// Records a chroma block's nnz into the per-MB cache.
    #[inline]
    fn chroma_nnz_cache_set(&mut self, c: usize, bx: usize, by: usize, total: u8) {
        self.nnz_c_cache[c][(by + 1) * 3 + (bx + 1)] = total;
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
                    // P_Skip prediction (reference 0). A free skip (zero residual) is
                    // taken immediately; the quality preset also takes a greedy P_Skip
                    // when its SAD is below the neighbour-predicted bound (below).
                    let _g_skip = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncSkip);
                    let mv_skip = fe.skip_mv(mb_x, mb_y);
                    let skip_y = fe.skip_predict_luma(refs, mb_x, mb_y, mv_skip);
                    let luma_free = fe.skip_luma_is_free(&sy, mb_x, mb_y, &skip_y);
                    // Chroma MC only when it can matter: luma already free (so the
                    // skip might be taken) or the quality path needs it below.
                    let skip_c = if luma_free || !fe.fast {
                        fe.skip_predict_chroma(refs, mb_x, mb_y, mv_skip)
                    } else {
                        [[0u8; 64]; 2]
                    };
                    let is_free =
                        luma_free && fe.skip_chroma_is_free(&su, &sv, mb_x, mb_y, &skip_c);
                    let mb_idx = mb_y * fe.mb_w + mb_x;
                    // Skip-prediction luma SAD (the quality preset's predicted-SAD apparatus).
                    let skip_sad = if fe.fast {
                        0
                    } else {
                        let (lx, ly) = (mb_x * 16, mb_y * 16);
                        let mut s = 0u32;
                        for dy in 0..16 {
                            let src = &sy[(ly + dy) * fe.cw + lx..][..16];
                            let p = &skip_y[dy * 16..][..16];
                            s += src.iter().zip(p).map(|(&a, &b)| a.abs_diff(b) as u32).sum::<u32>();
                        }
                        s
                    };
                    if is_free {
                        fe.commit_skip(mb_x, mb_y, mv_skip, &skip_y, &skip_c);
                        if !fe.fast {
                            fe.mb_was_skip[mb_idx] = true;
                            fe.mb_skip_sad[mb_idx] = skip_sad;
                        }
                        skip_run += 1;
                        continue;
                    }
                    drop(_g_skip);
                    let (lx, ly) = (mb_x * 16, mb_y * 16);
                    let nb = fe.mv_neighbors_block(mb_x as isize * 4, mb_y as isize * 4, 4);
                    let lme = lambda.sqrt();

                    if fe.fast {
                        // Fast preset: pick the cheapest *prediction* by SATD (no
                        // trial-encoding), then always code its residual — P_16x16 vs
                        // I_16x16 only, no sub-partitions. Crucially it does NOT make a
                        // SATD skip-vs-code decision: P_Skip is taken only for a truly
                        // free (zero-residual) macroblock, handled above. Pricing skip
                        // by SATD would drop residual the QP wants coded and tank PSNR;
                        // like x264's fast presets, fast trades *efficiency* (more bits)
                        // for speed, not quality. The faster ME is what makes it fast.
                        let (r16, mv16, cost_inter) =
                            fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 16, &[], lme);
                        let cost_intra = fe.best_i16_sad(&sy, mb_x, mb_y)
                            + (lme * FAST_INTRA_PENALTY_BITS) as i64;
                        inter = if cost_intra < cost_inter {
                            None // intra wins → encode_mb below
                        } else {
                            Some((0, vec![(r16, mv16)]))
                        };
                    } else {
                        // Quality preset: openh264's mode-decision model — SATD + λ·mvbits
                        // cost ESTIMATE (no per-candidate trial-encode); modes are ranked
                        // by that cost and only the winner is encoded (once) below. This
                        // removes ~the 93%-of-quality re-encode cost.

                        // Greedy P_Skip (openh264 `PredictSadSkip`): take the skip when its
                        // luma SAD is below the neighbour-predicted skip SAD. The threshold
                        // is what skip neighbours achieved, so the skip propagates from the
                        // free skips and self-limits — no fixed bound, no inter-chain drift.
                        if skip_sad < fe.pred_skip_sad(mb_x, mb_y) {
                            fe.commit_skip(mb_x, mb_y, mv_skip, &skip_y, &skip_c);
                            fe.mb_was_skip[mb_idx] = true;
                            fe.mb_skip_sad[mb_idx] = skip_sad;
                            skip_run += 1;
                            continue;
                        }

                        // 16×16 baseline (SATD + λ·bits, with sub-pel refinement).
                        let (r16, mv16, c16) =
                            fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 16, &[], lme);
                        let mut best_c = c16;
                        let mut pick: Option<InterChoice> = Some((0, vec![(r16, mv16)]));

                        // Sub-partitions, ranked by SATD, gated on a heavy 16×16 (a likely
                        // motion boundary — the 4 sub-pel searches are the expensive part).
                        const QSTEP16: [i64; 6] = [10, 11, 13, 14, 16, 18];
                        let qstep16 = QSTEP16[(fe.qp % 6) as usize] << (fe.qp / 6);
                        let split_gate = ((30 * (qstep16 + 160)) >> 3) * 2;
                        if c16 > split_gate {
                            let (rt, mvt, ct) = fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 8, &[mv16], lme);
                            let (rb, mvb, cb) = fe.best_part(refs, &sy, &nb, num_refs, lx, ly + 8, 16, 8, &[mv16], lme);
                            let (rl, mvl, cl) = fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 8, 16, &[mv16], lme);
                            let (rr, mvr, cr) = fe.best_part(refs, &sy, &nb, num_refs, lx + 8, ly, 8, 16, &[mv16], lme);
                            if ct + cb < best_c {
                                best_c = ct + cb;
                                pick = Some((1u8, vec![(rt, mvt), (rb, mvb)]));
                            }
                            if cl + cr < best_c {
                                best_c = cl + cr;
                                pick = Some((2u8, vec![(rl, mvl), (rr, mvr)]));
                            }
                        }

                        // Intra is ALWAYS a candidate (textured / occluded content):
                        // I_16x16 SATD + λ·mode bits.
                        let c_intra = fe.best_i16_satd(&sy, mb_x, mb_y)
                            + (lme * FAST_INTRA_PENALTY_BITS) as i64;
                        inter = if c_intra < best_c { None } else { pick };
                        fe.mb_was_skip[mb_idx] = false;
                        fe.mb_skip_sad[mb_idx] = skip_sad;
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

    // Deblock the reconstruction; the result is the inter reference. Baseline: the
    // intra mask is `!inter_y` (passed directly, no alloc); no B (List-1 empty); no
    // 8×8 transform (t8x8 empty). ref_id is each block's List-0 ref index.
    let _g_fin = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncFinal);
    let ref_id: Vec<i32> = fe.ref_idx_y.iter().map(|&r| if r >= 0 { r } else { i32::MIN }).collect();
    let info = rusty_h264_common::deblock::BlockInfo {
        inter: &fe.inter_y,
        nnz: &fe.nnz_y,
        mv: &fe.mv_y,
        ref_id: &ref_id,
        mv1: &[],
        ref_id1: &[],
        w4: fe.mb_w * 4,
        t8x8: &[],
    };
    // The encoder uses a single QP per frame and zero chroma_qp_index_offset, so
    // a uniform per-MB QP grid reproduces the old scalar-QP filtering exactly.
    let mb_qp = vec![fe.qp; fe.mb_w * fe.mb_h];
    drop(_g_fin);
    rusty_h264_common::deblock::filter_frame(
        &mut fe.rec_y,
        &mut fe.rec_u,
        &mut fe.rec_v,
        fe.mb_w,
        fe.mb_h,
        &mb_qp,
        0, // chroma_qp_index_offset — the encoder emits 0
        0, // slice_alpha_c0_offset — the encoder always signals zero offsets
        0, // slice_beta_offset
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
/// SATD of a `w`×`h` luma block: `src` (stride `ss`) vs `pred` (stride `ps`).
///
/// With `--features asm` and a supported size this is `2 · WelsSampleSatd_sse2`, which
/// is **byte-identical** to the scalar `Σ|H·d|` Hadamard: the openh264 kernel returns
/// `(Σ+1)>>1`, and `Σ` is always even (every 4×4 Hadamard coefficient shares the block
/// sum's parity, so 16 of them sum even), so `×2` recovers `Σ` exactly — proven over
/// 20 k random blocks at 4×4/8×8/16×16 in `tests/satd_asm_compare.rs`. Without asm (or
/// for an unsupported size) it falls back to the scalar Hadamard — the original path.
#[inline]
fn satd_px(src: &[u8], ss: usize, pred: &[u8], ps: usize, w: usize, h: usize) -> i64 {
    #[cfg(accel)]
    {
        let asm = match (w, h) {
            (16, 16) => Some(rusty_h264_accel::satd_16x16(src, ss, pred, ps)),
            (16, 8) => Some(rusty_h264_accel::satd_16x8(src, ss, pred, ps)),
            (8, 16) => Some(rusty_h264_accel::satd_8x16(src, ss, pred, ps)),
            (8, 8) => Some(rusty_h264_accel::satd_8x8(src, ss, pred, ps)),
            (4, 4) => Some(rusty_h264_accel::satd_4x4(src, ss, pred, ps)),
            _ => None,
        };
        if let Some(v) = asm {
            return 2 * v as i64;
        }
    }
    // Scalar Hadamard (also the no-asm path): Σ over the 4×4 sub-blocks.
    let (nbx, nby) = (w / 4, h / 4);
    let mut blocks = [[0i32; 16]; 16];
    let mut bi = 0;
    for by in 0..nby {
        for bx in 0..nbx {
            let blk = &mut blocks[bi];
            for dy in 0..4 {
                for dx in 0..4 {
                    blk[dy * 4 + dx] =
                        src[(by * 4 + dy) * ss + bx * 4 + dx] as i32 - pred[(by * 4 + dy) * ps + bx * 4 + dx] as i32;
                }
            }
            bi += 1;
        }
    }
    satd_4x4_sum(&blocks[..nbx * nby])
}

fn satd_16x16(src: &[u8], stride: usize, lx: usize, ly: usize, pred: &[u8; 256]) -> i64 {
    satd_px(&src[ly * stride + lx..], stride, pred, 16, 16, 16)
}

/// SAD over a 16×16 luma macroblock against a prediction — the fast preset's
/// intra cost, kept in the same (SAD) domain as its inter cost. `Σ a.abs_diff(b)`
/// over `u8` slices auto-vectorizes to `psadbw`.
fn sad_16x16(src: &[u8], stride: usize, lx: usize, ly: usize, pred: &[u8; 256]) -> i64 {
    let mut sad = 0u32;
    for dy in 0..16 {
        let s = &src[(ly + dy) * stride + lx..][..16];
        let p = &pred[dy * 16..][..16];
        sad += s.iter().zip(p).map(|(&a, &b)| a.abs_diff(b) as u32).sum::<u32>();
    }
    sad as i64
}

/// SATD over an 8×8 chroma block (four 4×4 sub-blocks) against a prediction.
fn satd_8x8(src: &[u8], stride: usize, x0: usize, y0: usize, pred: &[u8; 64]) -> i64 {
    satd_px(&src[y0 * stride + x0..], stride, pred, 8, 8, 8)
}

/// SATD of one 4×4 luma block against a prediction.
fn satd_4x4(src: &[u8], stride: usize, px: usize, py: usize, pred: &[u8; 16]) -> i64 {
    satd_px(&src[py * stride + px..], stride, pred, 4, 4, 4)
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
        let qb = quantize(&forward_core(&res), qp, fe.idz); // full 16 incl DC
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

/// 16×16 luma intra prediction. For interior MBs (both neighbors available) this
/// dispatches to openh264's `WelsI16x16LumaPred*_sse2` (bit-identical to the spec
/// predictor); edge MBs (partial availability → C-only DC variants) use the scalar
/// path. The scalar `top`/`left`/`corner` are gathered by the caller regardless.
#[inline]
fn i16_pred(
    fe: &FrameEncoder,
    mode: I16Mode,
    avail_top: bool,
    avail_left: bool,
    top: &[u8; 16],
    left: &[u8; 16],
    corner: u8,
    lx: usize,
    ly: usize,
) -> [u8; 256] {
    #[cfg(accel)]
    if avail_top && avail_left {
        let mode_n = match mode {
            I16Mode::Vertical => 0,
            I16Mode::Horizontal => 1,
            I16Mode::Dc => 2,
            I16Mode::Plane => 3,
        };
        let mut p = AlignedMb([0; 256]);
        rusty_h264_accel::i16x16_luma_pred(mode_n, &mut p.0, &fe.rec_y[..], ly * fe.cw + lx, fe.cw);
        return p.0;
    }
    let _ = (fe, lx, ly);
    luma16x16_pred(mode, avail_top, avail_left, top, left, corner)
}

/// 8×8 chroma intra prediction. Interior MBs use openh264's `WelsIChromaPred{V,Plane}_sse2`
/// for the V/Plane modes (bit-identical); DC/Horizontal (C-only in openh264) and edge MBs
/// use the scalar path.
#[inline]
#[allow(clippy::too_many_arguments)]
fn chroma_pred(
    fe: &FrameEncoder,
    mode: u8,
    avail_top: bool,
    avail_left: bool,
    c: usize,
    top: &[u8; 8],
    left: &[u8; 8],
    corner: u8,
    cx: usize,
    cy: usize,
) -> [u8; 64] {
    #[cfg(accel)]
    if avail_top && avail_left && (mode == 2 || mode == 3) {
        let plane = if c == 0 { &fe.rec_u } else { &fe.rec_v };
        let mut p = AlignedMb([0; 256]);
        rusty_h264_accel::chroma8x8_pred(mode, &mut p.0[..64], &plane[..], cy * fe.ccw + cx, fe.ccw);
        let mut out = [0u8; 64];
        out.copy_from_slice(&p.0[..64]);
        return out;
    }
    let _ = (fe, c, cx, cy);
    chroma8x8_pred(mode, avail_top, avail_left, top, left, corner)
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
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncIntraCode);
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
    let mut best_pred = i16_pred(fe, I16Mode::Dc, avail_top, avail_left, &top, &left, corner, lx, ly);
    let mut best_cost = satd_16x16(sy, fe.cw, lx, ly, &best_pred);
    for mode in [I16Mode::Vertical, I16Mode::Horizontal, I16Mode::Plane] {
        if !mode.available(avail_top, avail_left) {
            continue;
        }
        let pred = i16_pred(fe, mode, avail_top, avail_left, &top, &left, corner, lx, ly);
        let cost = satd_16x16(sy, fe.cw, lx, ly, &pred);
        if cost < best_cost {
            best_cost = cost;
            i16_mode = mode;
            best_pred = pred;
        }
    }
    // I_16x16 blocks are independent (one fixed whole-MB prediction), so batch the
    // forward DCT (`forward_dct_blocks` → SIMD), bit-identical to `forward_core`.
    let mut dc4x4 = [0i32; 16];
    let mut i16_q = [[0i32; 16]; 16];
    let mut res_blocks = [[0i32; 16]; 16];
    for by in 0..4 {
        for bx in 0..4 {
            let predb = pred_block(&best_pred, bx, by);
            res_blocks[by * 4 + bx] = residual(sy, fe.cw, lx + bx * 4, ly + by * 4, &predb);
        }
    }
    let mut coeffs = [[0i32; 16]; 16];
    forward_dct_blocks(&res_blocks, &mut coeffs);
    for i in 0..16 {
        dc4x4[i] = coeffs[i][0];
        let mut q = quantize(&coeffs[i], qp, fe.idz);
        q[0] = 0;
        i16_q[i] = q;
    }
    let i16_dc_levels = forward_quant_luma_dc(&dc4x4, qp, true);
    let i16_recon_dc = inverse_quant_luma_dc(&i16_dc_levels, qp);
    let i16_cbp15 = i16_q.iter().any(|b| b[1..].iter().any(|&c| c != 0));
    let mut recon16 = [0u8; 256];
    let mut deq_blocks = [[0i32; 16]; 16];
    for i in 0..16 {
        deq_blocks[i] = dequantize(&i16_q[i], qp);
        deq_blocks[i][0] = i16_recon_dc[i];
    }
    let mut idct = [[0i32; 16]; 16];
    inverse_dct_blocks(&deq_blocks, &mut idct);
    for by in 0..4 {
        for bx in 0..4 {
            let s = add_residual_4x4(&idct[by * 4 + bx], &pred_block(&best_pred, bx, by));
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
            let pred8 = chroma_pred(fe, m, avail_top, avail_left, c, &ntop[c], &nleft[c], ncorner[c], cx, cy);
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
            chroma_pred(fe, chroma_mode, avail_top, avail_left, c, &ntop[c], &nleft[c], ncorner[c], cx, cy);
        let pblk = |bx: usize, by: usize| -> [i32; 16] {
            let mut predb = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    predb[dy * 4 + dx] = pred8[(by * 4 + dy) * 8 + (bx * 4 + dx)] as i32;
                }
            }
            predb
        };
        // Independent 4×4 blocks → batch the forward DCT (bit-identical to forward_core).
        let mut res_blocks = [[0i32; 16]; 4];
        for by in 0..2 {
            for bx in 0..2 {
                res_blocks[by * 2 + bx] =
                    residual(src, fe.ccw, cx + bx * 4, cy + by * 4, &pblk(bx, by));
            }
        }
        let mut coeffs = [[0i32; 16]; 4];
        forward_dct_blocks(&res_blocks, &mut coeffs);
        let mut dc2x2 = [0i32; 4];
        let mut qbs = [[0i32; 16]; 4];
        for i in 0..4 {
            dc2x2[i] = coeffs[i][0];
            let mut q = quantize(&coeffs[i], qpc, fe.idz);
            q[0] = 0;
            qbs[i] = q;
            if q[1..].iter().any(|&v| v != 0) {
                any_chroma_ac = true;
            }
        }
        let dl = forward_quant_chroma_dc(&dc2x2, qpc, true);
        if dl.iter().any(|&v| v != 0) {
            any_chroma_dc = true;
        }
        let recon_dc = inverse_quant_chroma_dc(&dl, qpc);
        // commit chroma reconstruction (batched inverse DCT + shared add+clip tail)
        let mut deq_blocks = [[0i32; 16]; 4];
        for i in 0..4 {
            deq_blocks[i] = dequantize(&qbs[i], qpc);
            deq_blocks[i][0] = recon_dc[i];
        }
        let mut idct = [[0i32; 16]; 4];
        inverse_dct_blocks(&deq_blocks, &mut idct);
        let plane = if c == 0 { &mut fe.rec_u } else { &mut fe.rec_v };
        for by in 0..2 {
            for bx in 0..2 {
                let s = add_residual_4x4(&idct[by * 2 + bx], &pblk(bx, by));
                store(plane, fe.ccw, cx + bx * 4, cy + by * 4, &s);
            }
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
        fe.nnz_cache_load(mb_x, mb_y);
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let total = if i4.cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = fe.nc_pred(lbx, lby);
                let scan16 = scan_4x4_dcac(&i4.q[lby * 4 + lbx]);
                encode_residual_block(w, &scan16, 16, nc) as u8
            } else {
                0
            };
            fe.nnz_cache_set(lbx, lby, total);
            fe.nnz_y[by * w4 + bx] = total;
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
        fe.nnz_cache_load(mb_x, mb_y);
        let nc_dc = fe.nc_pred(0, 0);
        let dc_scan = scan_4x4_dcac(&i16_dc_levels);
        encode_residual_block(w, &dc_scan, 16, nc_dc);
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.nnz_cache_set(lbx, lby, 0);
            fe.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
        }
        if i16_cbp15 {
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let nc = fe.nc_pred(lbx, lby);
                let ac = scan_4x4_ac(&i16_q[lby * 4 + lbx]);
                let total = encode_residual_block(w, &ac, 15, nc) as u8;
                fe.nnz_cache_set(lbx, lby, total);
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
        fe.chroma_cache_load(mb_x, mb_y);
        let w2 = fe.mb_w * 2;
        for c in 0..2 {
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let nc = fe.chroma_nc_pred(c, bx, by);
                let ac = scan_4x4_ac(&c_q_blocks[c][by * 2 + bx]);
                let total = encode_residual_block(w, &ac, 15, nc) as u8;
                fe.chroma_nnz_cache_set(c, bx, by, total);
                fe.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
            }
        }
    }

    // Mark all luma blocks coded for the next macroblock's top-right availability.
    for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
        fe.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
    }
}
