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

use crate::cabac::CabacEncoder;
use crate::config::EncoderConfig;
use rusty_h264_common::cavlc::{
    encode_residual_block, scan_4x4_ac, scan_4x4_dcac, write_cbp_inter, write_cbp_intra,
};
use rusty_h264_common::inter::{
    inter_partitions, mc_chroma, mc_luma, predict_mv, predict_partition_mv, MvNeighbor,
};
use rusty_h264_common::predict::{
    add_residual_4x4, add_residual_8x8, chroma8x8_pred, chroma_mode_available, chroma_qp,
    intra4x4_pred, intra8x8_pred, luma16x16_pred, reconstruct_4x4, I16Mode, CHROMA_4X4_SCAN_XY,
    LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{
    dequantize, forward_core, forward_core_8x8, forward_dct_blocks, forward_quant_chroma_dc,
    forward_quant_luma_dc, inverse_dct_blocks, inverse_quant_8x8, inverse_quant_chroma_dc,
    inverse_quant_luma_dc, quantize, quantize_8x8, satd_4x4_sum,
};
use rusty_h264_common::aligned::AlignedBytes;
use rusty_h264_common::{BitWriter, YuvFrame};

/// A 16-byte-aligned 16×16 luma block — the aligned `op1` openh264's SSE2 SAD/SATD
/// kernels require (`movdqa`). Safe to construct (`forbid(unsafe)` holds); the asm
/// FFI that consumes it lives in `rusty_h264-accel`. Only used on the `asm` feature.
#[cfg(accel)]
#[repr(align(16))]
struct AlignedMb([u8; 256]);

/// A B-slice 16×16 inter-coding spec: the prediction direction and the motion it
/// uses. `dir` 1 = `B_L0_16x16`, 2 = `B_L1_16x16`, 3 = `B_Bi_16x16` (spec Table
/// 7-14). List-0 is `refs[0]` (nearest past anchor); `l1` is List-1 (nearest
/// future anchor). `mv0`/`mv1` are the List-0/List-1 motion vectors (quarter-pel).
#[derive(Clone, Copy)]
struct BInter<'a> {
    dir: u8,
    l1: &'a crate::RefFrame,
    mv0: (i32, i32),
    mv1: (i32, i32),
}

/// 16-byte-aligned 256-`i16` DCT/coefficient buffer — the in-place `movdqa` quant
/// kernel (`WelsQuantFour4x4_sse2`) requires aligned coefficients. `asm`-feature only.
#[cfg(accel)]
#[repr(align(16))]
struct AlignedDct([i16; 256]);

/// Luma variance of the 16×16 source MB at (mb_x, mb_y) — the content signal for
/// the adaptive SAD↔SATD cost dispatch (high variance = detail = SAD misprices).
/// `256·variance` scale (the /256 of the mean-square is kept integer); only the
/// RELATIVE ordering matters for the per-frame percentile, so the constant drops.
fn mb_variance(sy: &[u8], cw: usize, mb_x: usize, mb_y: usize) -> i64 {
    let base = mb_y * 16 * cw + mb_x * 16;
    let (mut s, mut ss) = (0i64, 0i64);
    for r in 0..16 {
        let row = &sy[base + r * cw..base + r * cw + 16];
        for &p in row {
            let v = p as i64;
            s += v;
            ss += v * v;
        }
    }
    ss - s * s / 256 // 256·variance (mean removed), integer, monotone in variance
}

/// Adaptive-Quantization per-MB QP map: flat (low-variance) macroblocks get a FINER
/// QP (where blocking/banding is visible), busy ones a COARSER QP (where the eye
/// masks error) — moving bits to where they're seen. The shift is `strength ·
/// (log2 var − frame mean log2 var)`, so it's relative to THIS frame's texture
/// distribution (content-invariant), rounded to an integer QP step and clamped.
/// `strength == 0` → uniform base QP (byte-identical: every `mb_qp_delta` is 0).
fn aq_qp_map(sy: &[u8], cw: usize, mb_w: usize, mb_h: usize, base_qp: u8, strength: f64) -> Vec<u8> {
    const AQ_DQP_MAX: i32 = 4;
    let n = mb_w * mb_h;
    if strength == 0.0 || n == 0 {
        return vec![base_qp; n];
    }
    // Per-MB variance (the bit-cost weight) and its log2 (+1 avoids log2(0) on a flat
    // MB → reads as maximally flat → finest QP).
    let mut var = Vec::with_capacity(n);
    let mut lv = Vec::with_capacity(n);
    for my in 0..mb_h {
        for mx in 0..mb_w {
            let v = (mb_variance(sy, cw, mx, my) + 1) as f64;
            var.push(v);
            lv.push(v.log2());
        }
    }
    let mean_lv = lv.iter().sum::<f64>() / n as f64;
    // CONTENT-ADAPTIVE STRENGTH: back off where the log-variance SPREAD is high. A
    // wide/bimodal spread means synthetic-ish content (flat regions beside detailed
    // patterns) where "busy = maskable" FAILS and the patterns are salient — full AQ
    // there costs PSNR. Natural content's spread is ~1 (keeps full strength); a
    // synthetic pan's is ~6 (heavily reduced). Ramp 1.0→`AQ_SPREAD_MIN` over
    // [`AQ_SPREAD_LO`, `AQ_SPREAD_HI`].
    const AQ_SPREAD_LO: f64 = 1.5;
    const AQ_SPREAD_HI: f64 = 5.0;
    const AQ_SPREAD_MIN: f64 = 0.0; // extreme spread (pathological synthetic) → AQ OFF
    let std_lv = (lv.iter().map(|&l| (l - mean_lv).powi(2)).sum::<f64>() / n as f64).sqrt();
    let factor = (1.0 - (std_lv - AQ_SPREAD_LO) / (AQ_SPREAD_HI - AQ_SPREAD_LO)).clamp(AQ_SPREAD_MIN, 1.0);
    let eff_strength = strength * factor;
    // Per-MB QP shift (clamped): busy (log-var above mean) coarser, flat finer.
    let dqp: Vec<i32> = lv
        .iter()
        .map(|&l| (eff_strength * (l - mean_lv)).round() as i32)
        .map(|d| d.clamp(-AQ_DQP_MAX, AQ_DQP_MAX))
        .collect();
    // RATE COMPENSATION: AQ nets a rate change (coarsening a busy MB saves more bits
    // than fining a flat one adds), so shift the whole frame's QP by `c` to restore
    // the un-AQ rate — keeping `qp` meaningful. Bit model `bits_i ∝ var_i·2^(−qp_i/6)`
    // (variance as the per-MB cost proxy): `c = 6·log2(Σ var·2^(−dqp/6) / Σ var)`.
    let sum_v: f64 = var.iter().sum();
    let sum_vs: f64 = var
        .iter()
        .zip(&dqp)
        .map(|(&v, &d)| v * 2f64.powf(-(d as f64) / 6.0))
        .sum();
    let c = (6.0 * (sum_vs / sum_v).log2()).round() as i32;
    dqp.iter()
        .map(|&d| (base_qp as i32 + c + d).clamp(0, 51) as u8)
        .collect()
}

/// IMPLICIT bi-prediction weights `(w0, w1)` from POC distances (spec §8.4.2.3.2,
/// `weighted_bipred_idc == 2`), IDENTICAL to the decoder's `implicit_weights`. The
/// closer anchor gets more weight; an equidistant B (`bframes == 1`) yields 32:32,
/// i.e. the plain average. `(32, 32)` fallback for the degenerate/out-of-range cases
/// the decoder also averages (no long-term refs here).
fn implicit_bi_weights(cur_poc: i32, l0_poc: i32, l1_poc: i32) -> (i32, i32) {
    let td = (l1_poc - l0_poc).clamp(-128, 127);
    let tb = (cur_poc - l0_poc).clamp(-128, 127);
    if td == 0 {
        return (32, 32);
    }
    let tx = (16384 + td.abs() / 2) / td;
    let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
    let w1 = dsf >> 2;
    if !(-64..=128).contains(&w1) {
        return (32, 32);
    }
    (64 - w1, w1)
}

/// Bi-prediction blend of two motion-compensated samples `p` (List-0) and `q`
/// (List-1) under weights `(w0, w1)` — the decoder's `b_mc` blend. `(32, 32)` is the
/// plain `(p+q+1)>>1` average.
#[inline(always)]
fn bi_blend(p: i32, q: i32, w: (i32, i32)) -> u8 {
    ((p * w.0 + q * w.1 + 32) >> 6).clamp(0, 255) as u8
}

/// Zig-zag scan of a raster i16 4×4 block into scan-order i32 — the fused-path
/// twin of `scan_4x4_dcac(&q_blocks[..])`, reading quantized levels straight from
/// the hot i16 DCT buffer. Byte-identical: the i16→i32 widening of a quant level
/// is exact (levels always fit i16, being the input to the i16 idct kernel).
#[cfg(accel)]
#[inline]
fn scan_4x4_dcac_i16(d: &[i16]) -> [i32; 16] {
    [
        d[0] as i32, d[1] as i32, d[4] as i32, d[8] as i32, d[5] as i32, d[2] as i32,
        d[3] as i32, d[6] as i32, d[9] as i32, d[12] as i32, d[13] as i32, d[10] as i32,
        d[7] as i32, d[11] as i32, d[14] as i32, d[15] as i32,
    ]
}


/// Per-frame intra encoder state: reconstructed planes (coded size) and the
/// per-4×4-block non-zero-coefficient counts used for CAVLC context.
pub struct FrameEncoder {
    mb_w: usize,
    mb_h: usize,
    qp: u8,  // the CURRENT macroblock's target QPy (AQ varies it per MB)
    qpc: u8, // chroma QP for `qp`
    /// Running QPy of the last macroblock that coded an `mb_qp_delta` (spec QPY_PREV).
    /// `mb_qp_delta = qp − cur_qp`; a skip / cbp==0 MB codes no delta and inherits it.
    cur_qp: u8,
    /// Implicit bi-prediction weights `(w0, w1)` for the current B-frame (from its
    /// L0/L1 anchor POC distances). `(32, 32)` = plain average (P/I frames, `bframes
    /// == 1`); unequal for `bframes > 1`.
    bi_w: (i32, i32),
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
    mv_y: Vec<(i32, i32)>, // motion vector per 4×4 block (quarter-pel) — List-0
    inter_y: Vec<bool>, // whether each 4×4 block is inter-coded
    ref_idx_y: Vec<i32>, // reference index per 4×4 block (-1 = intra/uncoded) — List-0
    // B-slice List-1 motion field (empty for P/I). B_L1/B_Bi commit here so a later
    // partition's List-1 median predictor sees it, mirroring the decoder's
    // `mv_neighbors_list(.., 1)` over `mv1`/`ref_idx1`.
    mv1_y: Vec<(i32, i32)>,
    ref_idx1_y: Vec<i32>,
    idz: i64, // intra dead-zone divisor: 2 for all-intra, 3 when frames reference each other
    rdoq_strength: f64, // CABAC trellis (RDOQ) strength; 0 = off (hard quantize, CAVLC path)
    transform_8x8: bool, // High-profile 8x8 transform enabled (transform_8x8_mode_flag)
    fast: bool, // Preset::Fast — SATD mode decision (no RDO), 16×16/I_16x16 only
    skip_accel_check: bool, // A/B knob: whole-MB psadbw gate in the P_Skip free-check
    coded_path_v2: bool,    // A/B knob: route inter coding through encode_inter_mb_v2
    tune_lambda_scale: f64, // tuning knob: scale on the RD λ (1.0 = standard)
    tune_intra_penalty: f64,
    satd_q: f64,               // adaptive: fraction of high-variance MBs routed to SATD cost
    satd_var_thresh: i64,      // per-frame variance threshold for the routing (set in a pre-pass)
    aq_strength: f64,          // adaptive quantization: per-MB QP modulation strength (0 = off)
    mb_use_satd: bool,         // per-MB: this MB uses the SATD cost this decision
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
/// Fast-preset pruned I4x4 mode search ({MPM, DC, V, H} instead of all 9 — the
/// x264-ultrafast-style candidate set). DEFAULT ON for the fast preset (gated:
/// +0.5% size at +0.02 dB on all-intra, +17% all-intra speed); RUSTY_FAST_INTRA=0
/// restores the exhaustive 9-mode search (the pre-flip bitstream).
fn fast_intra_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RUSTY_FAST_INTRA").map_or(true, |v| v != "0"))
}

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
            cur_qp: cfg.qp,
            bi_w: (32, 32),
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
            mv1_y: vec![(0, 0); (mb_w * 4) * (mb_h * 4)],
            ref_idx1_y: vec![-1; (mb_w * 4) * (mb_h * 4)],
            // All-intra (no inter references) tolerates the larger dead-zone; in
            // an I+P stream the IDR is a reference, so keep the standard offset.
            idz: if cfg.gop_size <= 1 { 2 } else { 3 },
            rdoq_strength: 0.0, // set >0 only in the CABAC slice coders
            transform_8x8: cfg.transform_8x8,
            fast: cfg.preset == crate::config::Preset::Fast,
            skip_accel_check: cfg.tune_skip_accel_check,
            coded_path_v2: cfg.coded_path_v2,
            aq_strength: cfg.aq_strength,
            tune_lambda_scale: cfg.tune_lambda_scale,
            tune_intra_penalty: cfg.tune_intra_penalty,
            satd_q: cfg.tune_satd_q,
            satd_var_thresh: i64::MAX,
            mb_use_satd: false,
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

    /// The `mb_qp_delta` for the current macroblock (`qp − cur_qp`) and commits the
    /// running QPy — called ONLY where the syntax actually codes a delta (I_16x16
    /// always; inter / I_4x4 when `cbp != 0`), so a skip / cbp==0 MB leaves `cur_qp`
    /// unchanged and inherits it, exactly as the decoder's `step_qp` does.
    fn qp_delta(&mut self) -> i32 {
        let d = self.qp as i32 - self.cur_qp as i32;
        self.cur_qp = self.qp;
        d
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

    /// List-aware block MV-predictor neighbors (`list` 0 or 1), for the B-slice
    /// per-list `mvd` predictor. Identical geometry to [`Self::mv_neighbors_block`]
    /// but reads the List-1 motion grid when `list == 1`, matching the decoder's
    /// `mv_neighbors_list`. A neighbor not coded in this list reads `ref_idx = -1`
    /// (so `predict_partition_mv` treats it as non-matching, exactly as the decoder).
    fn mv_neighbors_block_list(&self, pbx: isize, pby: isize, pwb: isize, list: usize) -> [MvNeighbor; 3] {
        let (w4, h4) = ((self.mb_w * 4) as isize, (self.mb_h * 4) as isize);
        let (mvg, refg): (&[(i32, i32)], &[i32]) = if list == 0 {
            (&self.mv_y, &self.ref_idx_y)
        } else {
            (&self.mv1_y, &self.ref_idx1_y)
        };
        let get = |bx: isize, by: isize| -> MvNeighbor {
            if bx < 0 || by < 0 || bx >= w4 || by >= h4 || !self.coded_y[(by * w4 + bx) as usize] {
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

    /// Luma distortion of a `B_Bi` 16×16 prediction: motion-compensate `l0`/`l1`,
    /// average `(p+q+1)>>1` (the decoder's `b_mc` blend at `weighted_bipred_idc=0`),
    /// and score vs the source with the SAME metric the per-list searches used —
    /// SAD on the fast path, SATD when this MB is SATD-routed — so `J_bi` compares
    /// directly against `J0`/`J1`.
    fn bi_dist(
        &self,
        l0: &crate::RefFrame,
        l1: &crate::RefFrame,
        sy: &[u8],
        lx: usize,
        ly: usize,
        mv0: (i32, i32),
        mv1: (i32, i32),
    ) -> i64 {
        let ch = self.mb_h * 16;
        let (mut a, mut b) = ([0u8; 256], [0u8; 256]);
        mc_luma(&l0.y, self.cw, ch, lx, ly, 16, 16, mv0.0, mv0.1, &mut a);
        mc_luma(&l1.y, self.cw, ch, lx, ly, 16, 16, mv1.0, mv1.1, &mut b);
        let mut avg = [0u8; 256];
        for i in 0..256 {
            avg[i] = bi_blend(a[i] as i32, b[i] as i32, self.bi_w);
        }
        if self.fast && !self.mb_use_satd {
            let mut sad = 0u32;
            for dy in 0..16 {
                let s = &sy[(ly + dy) * self.cw + lx..][..16];
                let p = &avg[dy * 16..][..16];
                sad += s.iter().zip(p).map(|(&x, &y)| x.abs_diff(y) as u32).sum::<u32>();
            }
            sad as i64
        } else {
            satd_px(&sy[ly * self.cw + lx..], self.cw, &avg, 16, 16, 16)
        }
    }

    /// Distortion of a pre-formed 16×16 luma prediction vs the source (SAD on the
    /// fast path, SATD when SATD-routed) — the mode-decision cost for `B_Direct`,
    /// on the same scale as the per-list search J so they compare directly.
    fn pred_dist(&self, sy: &[u8], lx: usize, ly: usize, pred: &[u8; 256]) -> i64 {
        if self.fast && !self.mb_use_satd {
            let mut sad = 0u32;
            for dy in 0..16 {
                let s = &sy[(ly + dy) * self.cw + lx..][..16];
                let p = &pred[dy * 16..][..16];
                sad += s.iter().zip(p).map(|(&a, &b)| a.abs_diff(b) as u32).sum::<u32>();
            }
            sad as i64
        } else {
            satd_px(&sy[ly * self.cw + lx..], self.cw, pred, 16, 16, 16)
        }
    }

    /// `colZeroFlag` for absolute 4×4 block `(bx, by)` (spec §8.4.1.2.2): true when
    /// the co-located picture `RefPicList1[0]` (`l1`) is short-term (always, here —
    /// we use no long-term refs) and its co-located block uses List-0 reference 0
    /// with a near-zero (|·| ≤ 1) motion vector. Must match the decoder's `col_zero`.
    fn col_zero(&self, l1: &crate::RefFrame, bx: usize, by: usize) -> bool {
        if l1.w4 == 0 {
            return false;
        }
        let idx = by * l1.w4 + bx;
        if idx >= l1.ref_idx.len() {
            return false;
        }
        l1.ref_idx[idx] == 0 && l1.mv[idx].0.abs() <= 1 && l1.mv[idx].1.abs() <= 1
    }

    /// Bi-predictive MC of one small region into `pred_y`/`c_pred` at MB-relative
    /// offset `(dx, dy)` — the per-4×4 primitive the spatial-direct derivation uses.
    /// Mirrors the decoder's `b_mc` (average `(p+q+1)>>1` for bi, copy for uni).
    #[allow(clippy::too_many_arguments)]
    fn b_mc_block(
        &self,
        l0: &crate::RefFrame,
        l1: &crate::RefFrame,
        mb_x: usize,
        mb_y: usize,
        dx: usize,
        dy: usize,
        refi0: i32,
        m0: (i32, i32),
        refi1: i32,
        m1: (i32, i32),
        pred_y: &mut [u8; 256],
        c_pred: &mut [[u8; 64]; 2],
    ) {
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let (px, py) = (mb_x * 16 + dx, mb_y * 16 + dy);
        let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
        if refi0 >= 0 {
            mc_luma(&l0.y, self.cw, ch, px, py, 4, 4, m0.0, m0.1, &mut a);
        }
        if refi1 >= 0 {
            mc_luma(&l1.y, self.cw, ch, px, py, 4, 4, m1.0, m1.1, &mut b);
        }
        for yy in 0..4 {
            for xx in 0..4 {
                let i = yy * 4 + xx;
                let v = match (refi0 >= 0, refi1 >= 0) {
                    (true, true) => bi_blend(a[i] as i32, b[i] as i32, self.bi_w),
                    (true, false) => a[i],
                    _ => b[i],
                };
                pred_y[(dy + yy) * 16 + (dx + xx)] = v;
            }
        }
        // Chroma: the co-located 2×2 block at half resolution.
        let (cpx, cpy) = (mb_x * 8 + dx / 2, mb_y * 8 + dy / 2);
        for c in 0..2 {
            let (r0, r1) = if c == 0 { (&l0.u, &l1.u) } else { (&l0.v, &l1.v) };
            let (mut ca, mut cb) = ([0u8; 4], [0u8; 4]);
            if refi0 >= 0 {
                mc_chroma(r0, self.ccw, cch, cpx, cpy, 2, 2, m0.0, m0.1, &mut ca);
            }
            if refi1 >= 0 {
                mc_chroma(r1, self.ccw, cch, cpx, cpy, 2, 2, m1.0, m1.1, &mut cb);
            }
            for yy in 0..2 {
                for xx in 0..2 {
                    let i = yy * 2 + xx;
                    let v = match (refi0 >= 0, refi1 >= 0) {
                        (true, true) => bi_blend(ca[i] as i32, cb[i] as i32, self.bi_w),
                        (true, false) => ca[i],
                        _ => cb[i],
                    };
                    c_pred[c][(dy / 2 + yy) * 8 + (dx / 2 + xx)] = v;
                }
            }
        }
    }

    /// Spatial-direct (`direct_spatial_mv_pred_flag == 1`) prediction for a 16×16 B
    /// macroblock — the shared basis of `B_Skip` and `B_Direct_16x16`. Returns the
    /// prediction and the per-4×4 `(refIdxL0, mvL0, refIdxL1, mvL1)` motion the
    /// decoder's `decode_b_direct` derives (so the caller commits identical motion).
    fn b_direct(
        &self,
        l0: &crate::RefFrame,
        l1: &crate::RefFrame,
        mb_x: usize,
        mb_y: usize,
    ) -> ([u8; 256], [[u8; 64]; 2], [(i32, (i32, i32), i32, (i32, i32)); 16]) {
        let (nbx, nby) = ((mb_x * 4) as isize, (mb_y * 4) as isize);
        let n0 = self.mv_neighbors_block_list(nbx, nby, 4, 0);
        let n1 = self.mv_neighbors_block_list(nbx, nby, 4, 1);
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
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        let mut motion = [(0i32, (0i32, 0i32), 0i32, (0i32, 0i32)); 16];
        for sby in 0..4 {
            for sbx in 0..4 {
                let cz = !direct_zero && self.col_zero(l1, mb_x * 4 + sbx, mb_y * 4 + sby);
                let m0 = if refi0 == 0 && cz { (0, 0) } else { mv0 };
                let m1 = if refi1 == 0 && cz { (0, 0) } else { mv1 };
                motion[sby * 4 + sbx] = (refi0, m0, refi1, m1);
                self.b_mc_block(l0, l1, mb_x, mb_y, sbx * 4, sby * 4, refi0, m0, refi1, m1, &mut pred_y, &mut c_pred);
            }
        }
        (pred_y, c_pred, motion)
    }

    /// Commits a spatial-direct MB's per-4×4 motion into the List-0/List-1 grids so
    /// later MBs' neighbor predictors see it (mirrors the decoder's `b_set_motion`).
    fn commit_direct_motion(&mut self, mb_x: usize, mb_y: usize, motion: &[(i32, (i32, i32), i32, (i32, i32)); 16]) {
        let w4 = self.mb_w * 4;
        for sby in 0..4 {
            for sbx in 0..4 {
                let (refi0, m0, refi1, m1) = motion[sby * 4 + sbx];
                let idx = (mb_y * 4 + sby) * w4 + (mb_x * 4 + sbx);
                self.inter_y[idx] = true;
                self.coded_y[idx] = true;
                self.mv_y[idx] = m0;
                self.ref_idx_y[idx] = refi0;
                self.mv1_y[idx] = m1;
                self.ref_idx1_y[idx] = refi1;
            }
        }
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
        // Branchless closed form of the old `while n > 1 { n >>= 1; len += 2 }` loop:
        // that loop yields `len = 1 + 2·floor(log2(codenum+1))`, and for x ≥ 1
        // `floor(log2(x)) == 31 - x.leading_zeros()`. Removes a data-dependent branch
        // from the innermost ME cost — bit-identical (verified over the d range).
        #[inline(always)]
        fn mvbits(d: i32) -> u32 {
            let codenum = if d > 0 { (2 * d - 1) as u32 } else { (-2 * d) as u32 };
            1 + 2 * (31 - (codenum + 1).leading_zeros())
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
            let dist = if self.fast && !self.mb_use_satd {
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
    /// Dispatch to the current coded path (`_v1`) or the isolated fused path
    /// (`_v2`), selected by the hidden `coded_path_v2` A/B knob. Both produce
    /// byte-identical bitstreams (gated by the `coded_path_ab` test); the split
    /// exists so the two run side-by-side in one binary for honest timing.
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
        if self.coded_path_v2 {
            self.encode_inter_mb_v2(w, refs, sy, su, sv, mb_x, mb_y, mode, parts);
        } else {
            self.encode_inter_mb_v1(w, refs, sy, su, sv, mb_x, mb_y, mode, parts);
        }
    }

    /// Isolated, coefficient-fused inter coding path (A/B twin of `_v1`). The
    /// quantized luma levels stay in the hot 16-byte-aligned i16 DCT buffer for the
    /// whole MB; the i32 form is materialized on demand only for *coded* blocks
    /// (CAVLC scan + recon dequant), so uncoded quads never pay the conversion and
    /// there is no 256-word i32 `q_blocks` round-trip. Byte-identical to `_v1`
    /// (gated by `coded_path_ab`). Accel-only optimization; the scalar build reuses
    /// `_v1` unchanged.
    #[allow(clippy::too_many_arguments)]
    fn encode_inter_mb_v2(
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
        #[cfg(not(accel))]
        {
            self.encode_inter_mb_v1(w, refs, sy, su, sv, mb_x, mb_y, mode, parts);
        }
        #[cfg(accel)]
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncInterCode);
            let (qp, qpc) = (self.qp, self.qpc);
            let w4 = self.mb_w * 4;
            let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

            // ---- per-partition motion compensation + MV prediction (== v1) ----
            let mut pred_y = [0u8; 256];
            let mut c_pred = [[0u8; 64]; 2];
            let mut mvds = [(0i32, 0i32); 4];
            let mut n_mvd = 0;
            let _g_mc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
            for (part, &(rx, ry, rw, rh)) in inter_partitions(mode).iter().enumerate() {
                let (refi, mv) = parts[part];
                let reference = &refs[refi as usize];
                let (pbx, pby) = ((mb_x * 4 + rx / 4) as isize, (mb_y * 4 + ry / 4) as isize);
                let [a, b, c] = self.mv_neighbors_block(pbx, pby, (rw / 4) as isize);
                let pmv = predict_partition_mv(mode, part, a, b, c, refi);
                mvds[n_mvd] = (mv.0 - pmv.0, mv.1 - pmv.1);
                n_mvd += 1;
                for by in ry / 4..ry / 4 + rh / 4 {
                    for bx in rx / 4..rx / 4 + rw / 4 {
                        let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                        self.mv_y[idx] = mv;
                        self.inter_y[idx] = true;
                        self.ref_idx_y[idx] = refi;
                        self.coded_y[idx] = true;
                    }
                }
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

            // ---- luma residual + quantization: keep levels in the i16 buffer ----
            let mut dctw = AlignedDct([0i16; 256]);
            let dct = &mut dctw.0;
            let mut cbp_luma = 0u32;
            drop(_g_mc);
            let _g_tq = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncTq);
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
            // cbp per quad straight from the i16 levels (no i32 q_blocks copy).
            for blk in 0..16 {
                if dct[blk * 16..blk * 16 + 16].iter().any(|&v| v != 0) {
                    cbp_luma |= 1 << (blk / 4);
                }
            }

            // ---- chroma residual (identical to v1: c_q stays i32) ----
            let mut c_dc_levels = [[0i32; 4]; 2];
            let mut c_recon_dc = [[0i32; 4]; 2];
            let mut c_q = [[[0i32; 16]; 4]; 2];
            let (mut any_ac, mut any_dc) = (false, false);
            for c in 0..2 {
                let src = if c == 0 { su } else { sv };
                let dc2x2 = {
                    #[repr(align(16))]
                    struct A([i16; 64]);
                    let mut cdct = A([0i16; 64]);
                    rusty_h264_accel::dct_four_t4(
                        &mut cdct.0,
                        &src[(mb_y * 8) * self.ccw + mb_x * 8..],
                        self.ccw,
                        &c_pred[c],
                        8,
                    );
                    let dc = [cdct.0[0] as i32, cdct.0[16] as i32, cdct.0[32] as i32, cdct.0[48] as i32];
                    let ffc = rusty_h264_common::transform::quant_dz_ff(qpc, 6);
                    let mfc = &rusty_h264_common::transform::QUANT_MF_OH[qpc as usize];
                    rusty_h264_accel::quant_four_4x4(&mut cdct.0, &ffc, mfc);
                    for i in 0..4 {
                        let q = &mut c_q[c][i];
                        q[0] = 0;
                        for j in 1..16 {
                            let v = cdct.0[i * 16 + j] as i32;
                            q[j] = v;
                            if v != 0 {
                                any_ac = true;
                            }
                        }
                    }
                    dc
                };
                let dl = forward_quant_chroma_dc(&dc2x2, qpc, false);
                if dl.iter().any(|&v| v != 0) {
                    any_dc = true;
                }
                c_recon_dc[c] = inverse_quant_chroma_dc(&dl, qpc);
                c_dc_levels[c] = dl;
            }
            let cbp_chroma: u32 = if any_ac { 2 } else if any_dc { 1 } else { 0 };
            let cbp = cbp_luma | (cbp_chroma << 4);

            // ---- emit syntax (== v1) ----
            drop(_g_tq);
            let _g_syn = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
            w.write_ue(mode as u32);
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
                w.write_se(self.qp_delta()); // mb_qp_delta (AQ per-MB QPy)
            }
            self.nnz_cache_load(mb_x, mb_y);
            drop(_g_syn);

            // ---- CAVLC: scan straight from the i16 levels for coded blocks ----
            let _g_scan = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Scatter);
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                    let nc = self.nc_pred(lbx, lby);
                    let scan16 = scan_4x4_dcac_i16(&dct[blk * 16..blk * 16 + 16]);
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
            drop(_g_scan);

            // ---- reconstruction: dequantize luma straight from the i16 levels ----
            let _g_rec = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::SkipRecon);
            #[repr(align(16))]
            struct Align16([i16; 64]);
            let mut dct_in = Align16([0i16; 64]);
            for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
                let rec_off = base + qy * self.cw + qx;
                if cbp_luma & (1 << qi) == 0 {
                    for r in 0..8 {
                        let (dsti, srci) = (rec_off + r * self.cw, (qy + r) * 16 + qx);
                        self.rec_y[dsti..dsti + 8].copy_from_slice(&pred_y[srci..srci + 8]);
                    }
                    continue;
                }
                for k in 0..4 {
                    let blk = qi * 4 + k;
                    let mut lvl = [0i32; 16];
                    for i in 0..16 {
                        lvl[i] = dct[blk * 16 + i] as i32;
                    }
                    let deq = dequantize(&lvl, qp);
                    for i in 0..16 {
                        dct_in.0[k * 16 + i] = deq[i] as i16;
                    }
                }
                rusty_h264_accel::idct_four_t4_rec(
                    &mut self.rec_y[rec_off..],
                    self.cw,
                    &pred_y[qy * 16 + qx..],
                    16,
                    &dct_in.0,
                );
            }
            // chroma recon (identical to v1)
            for c in 0..2 {
                let base_c = (mb_y * 8) * self.ccw + mb_x * 8;
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                if cbp_chroma == 0 {
                    for r in 0..8 {
                        let dsti = base_c + r * self.ccw;
                        plane[dsti..dsti + 8].copy_from_slice(&c_pred[c][r * 8..r * 8 + 8]);
                    }
                } else {
                    #[repr(align(16))]
                    struct A([i16; 64]);
                    let mut d = A([0i16; 64]);
                    for i in 0..4 {
                        let deq = dequantize(&c_q[c][i], qpc);
                        for j in 0..16 {
                            d.0[i * 16 + j] = deq[j] as i16;
                        }
                        d.0[i * 16] = c_recon_dc[c][i] as i16;
                    }
                    rusty_h264_accel::idct_four_t4_rec(&mut plane[base_c..], self.ccw, &c_pred[c], 8, &d.0);
                }
            }
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_inter_mb_v1(
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
        self.encode_inter_mb_v1_b(w, refs, sy, su, sv, mb_x, mb_y, mode, parts, None);
    }

    /// As [`Self::encode_inter_mb_v1`], but `b_mode` selects B-slice framing: the
    /// macroblock is coded as `B_L0_16x16` (`mb_type == 1`) instead of the P-slice
    /// `mb_type == mode`. Everything else — the single List-0 partition, the median
    /// `mvd_l0` predictor, the residual, and the reconstruction — is byte-identical
    /// to `P_L0_16x16`, so the caller passes `mode == 0`, `refs == &[L0_anchor]`
    /// (length 1 ⇒ no `ref_idx` coded), and `parts == &[(0, mv)]`.
    /// Decide + reconstruct one inter macroblock (motion compensation, residual,
    /// quantize, reconstruct, commit motion grids) — everything except entropy
    /// coding. Returns an [`InterPlan`] coded by either backend, so CAVLC and CABAC
    /// share this whole path bit-for-bit (the P/B analogue of [`plan_mb`]).
    #[allow(clippy::too_many_arguments)]
    fn plan_inter_mb(
        &mut self,
        refs: &[crate::RefFrame],
        sy: &[u8],
        su: &[u8],
        sv: &[u8],
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        parts: &[(i32, (i32, i32))],
        bspec: Option<BInter>,
    ) -> InterPlan {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncInterCode);
        let (qp, qpc) = (self.qp, self.qpc);
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        // ---- per-partition motion compensation + MV prediction ----
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        let mut mvds = [(0i32, 0i32); 4]; // ≤4 partitions; no per-MB Vec alloc
        let mut n_mvd = 0;
        let _g_mc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
        if let Some(b) = bspec.filter(|b| b.dir == 0) {
            // ---- B_Direct_16x16 (mb_type 0): spatial-direct prediction, no mvd ----
            let (dp, dc, motion) = self.b_direct(&refs[0], b.l1, mb_x, mb_y);
            pred_y = dp;
            c_pred = dc;
            self.commit_direct_motion(mb_x, mb_y, &motion);
        } else if let Some(b) = bspec {
            // ---- B 16×16 prediction: List-0 / List-1 / Bi ----
            let use0 = b.dir == 1 || b.dir == 3;
            let use1 = b.dir == 2 || b.dir == 3;
            let (lx, ly) = (mb_x * 16, mb_y * 16);
            let (cx, cy) = (mb_x * 8, mb_y * 8);
            let (pbx, pby) = ((mb_x * 4) as isize, (mb_y * 4) as isize);
            // Per-list `mvd` against the median predictor over that list's neighbors.
            if use0 {
                let [a, c0, c1] = self.mv_neighbors_block_list(pbx, pby, 4, 0);
                let p = predict_partition_mv(0, 0, a, c0, c1, 0);
                mvds[n_mvd] = (b.mv0.0 - p.0, b.mv0.1 - p.1);
                n_mvd += 1;
            }
            if use1 {
                let [a, c0, c1] = self.mv_neighbors_block_list(pbx, pby, 4, 1);
                let p = predict_partition_mv(0, 0, a, c0, c1, 0);
                mvds[n_mvd] = (b.mv1.0 - p.0, b.mv1.1 - p.1);
                n_mvd += 1;
            }
            // Motion compensation. L0/L1 write straight into pred; Bi averages
            // (p+q+1)>>1 — the decoder's `b_mc` blend with weighted_bipred_idc=0.
            let mut a_y = [0u8; 256];
            let mut b_y = [0u8; 256];
            let mut a_c = [[0u8; 64]; 2];
            let mut b_c = [[0u8; 64]; 2];
            if use0 {
                mc_luma(&refs[0].y, self.cw, ch, lx, ly, 16, 16, b.mv0.0, b.mv0.1, &mut a_y);
                mc_chroma(&refs[0].u, self.ccw, cch, cx, cy, 8, 8, b.mv0.0, b.mv0.1, &mut a_c[0]);
                mc_chroma(&refs[0].v, self.ccw, cch, cx, cy, 8, 8, b.mv0.0, b.mv0.1, &mut a_c[1]);
            }
            if use1 {
                mc_luma(&b.l1.y, self.cw, ch, lx, ly, 16, 16, b.mv1.0, b.mv1.1, &mut b_y);
                mc_chroma(&b.l1.u, self.ccw, cch, cx, cy, 8, 8, b.mv1.0, b.mv1.1, &mut b_c[0]);
                mc_chroma(&b.l1.v, self.ccw, cch, cx, cy, 8, 8, b.mv1.0, b.mv1.1, &mut b_c[1]);
            }
            match (use0, use1) {
                (true, true) => {
                    for i in 0..256 {
                        pred_y[i] = bi_blend(a_y[i] as i32, b_y[i] as i32, self.bi_w);
                    }
                    for c in 0..2 {
                        for i in 0..64 {
                            c_pred[c][i] = bi_blend(a_c[c][i] as i32, b_c[c][i] as i32, self.bi_w);
                        }
                    }
                }
                (true, false) => {
                    pred_y = a_y;
                    c_pred = a_c;
                }
                _ => {
                    pred_y = b_y;
                    c_pred = b_c;
                }
            }
            // Commit per-list motion so later MBs' per-list predictors see it.
            for by in 0..4 {
                for bx in 0..4 {
                    let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                    self.inter_y[idx] = true;
                    self.coded_y[idx] = true;
                    self.mv_y[idx] = if use0 { b.mv0 } else { (0, 0) };
                    self.ref_idx_y[idx] = if use0 { 0 } else { -1 };
                    self.mv1_y[idx] = if use1 { b.mv1 } else { (0, 0) };
                    self.ref_idx1_y[idx] = if use1 { 0 } else { -1 };
                }
            }
        } else {
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
        } // end P per-partition formation (else of the B branch)

        // ---- luma residual + quantization ----
        let mut q_blocks = [[0i32; 16]; 16]; // raster, levels
        let mut cbp_luma = 0u32;
        drop(_g_mc);
        let _g_tq = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncTq);
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
                let q = rdoq(&coeffs[lby * 4 + lbx], qp, 6, self.rdoq_strength, 0);
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
            // Fast path: one dct_four_t4 covers the whole 8x8 chroma region (all 4
            // blocks, residual+DCT fused straight from the planes); block b's pre-quant
            // DC is dct[b*16] (quad z-scan == 2x2 raster); quant_four_4x4 with our
            // FF/MF is bit-identical to scalar `quantize`. Same pairing the P_Skip
            // free-check proved byte-identical over the corpus.
            #[cfg(accel)]
            let (mut dc2x2, applied) = {
                #[repr(align(16))]
                struct A([i16; 64]);
                let mut dct = A([0i16; 64]);
                rusty_h264_accel::dct_four_t4(
                    &mut dct.0,
                    &src[(mb_y * 8) * self.ccw + mb_x * 8..],
                    self.ccw,
                    &c_pred[c],
                    8,
                );
                let dc = [
                    dct.0[0] as i32,
                    dct.0[16] as i32,
                    dct.0[32] as i32,
                    dct.0[48] as i32,
                ];
                let ffc = rusty_h264_common::transform::quant_dz_ff(qpc, 6);
                let mfc = &rusty_h264_common::transform::QUANT_MF_OH[qpc as usize];
                rusty_h264_accel::quant_four_4x4(&mut dct.0, &ffc, mfc);
                for i in 0..4 {
                    let q = &mut c_q[c][i];
                    q[0] = 0;
                    for j in 1..16 {
                        let v = dct.0[i * 16 + j] as i32;
                        q[j] = v;
                        if v != 0 {
                            any_ac = true;
                        }
                    }
                }
                (dc, true)
            };
            #[cfg(not(accel))]
            let (mut dc2x2, applied) = ([0i32; 4], false);
            if !applied {
                // Scalar/`wide` twin: gather, batch forward DCT, quantize per block.
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
                for i in 0..4 {
                    dc2x2[i] = coeffs[i][0];
                    let mut q = rdoq(&coeffs[i], qpc, 6, self.rdoq_strength, 1);
                    q[0] = 0;
                    if q[1..].iter().any(|&v| v != 0) {
                        any_ac = true;
                    }
                    c_q[c][i] = q;
                }
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

        drop(_g_tq);
        let _g_rec = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::SkipRecon);
        // ---- reconstruction (luma) ----
        #[cfg(accel)]
        {
            // Dequantize all 16 blocks into the 4-quadrant int16 layout (16-byte
            // aligned — the kernel uses movdqa coeff loads), then inverse-DCT + add
            // prediction + clip per quadrant via openh264. The inverse butterfly +
            // (x+32)>>6 is bit-identical to reconstruct_4x4 (verified in accel).
            // An 8x8 quad whose cbp bit is clear has ZERO residual: reconstruction
            // IS the prediction (the decoder's own uncoded-region fast path) — a row
            // copy replaces dequant + convert + idct for that quad. Byte-identical:
            // idct of an all-zero block adds (0+32)>>6 = 0 to pred, clip is identity.
            #[repr(align(16))]
            struct Align16([i16; 64]);
            let mut dct_in = Align16([0i16; 64]);
            let base = mb_y * 16 * self.cw + mb_x * 16;
            for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
                let rec_off = base + qy * self.cw + qx;
                if cbp_luma & (1 << qi) == 0 {
                    for r in 0..8 {
                        let (dsti, srci) = (rec_off + r * self.cw, (qy + r) * 16 + qx);
                        self.rec_y[dsti..dsti + 8].copy_from_slice(&pred_y[srci..srci + 8]);
                    }
                    continue;
                }
                for k in 0..4 {
                    let blk = qi * 4 + k;
                    let (lbx, lby) = LUMA_4X4_SCAN_XY[blk];
                    let deq = dequantize(&q_blocks[lby * 4 + lbx], qp);
                    for i in 0..16 {
                        dct_in.0[k * 16 + i] = deq[i] as i16;
                    }
                }
                rusty_h264_accel::idct_four_t4_rec(
                    &mut self.rec_y[rec_off..],
                    self.cw,
                    &pred_y[qy * 16 + qx..],
                    16,
                    &dct_in.0,
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
            // Fast path: dequantize into the quad i16 layout (raster == the kernel's
            // z-order for a 2x2) with the Hadamard DC injected, then ONE
            // idct+add-pred+clip kernel writes the 8x8 straight into the plane —
            // bit-identical to the scalar tail below (verified kernel pairing).
            #[cfg(accel)]
            {
                let base = (mb_y * 8) * self.ccw + mb_x * 8;
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                if cbp_chroma == 0 {
                    // No chroma residual at all: recon = prediction (row copies).
                    for r in 0..8 {
                        let dsti = base + r * self.ccw;
                        plane[dsti..dsti + 8].copy_from_slice(&c_pred[c][r * 8..r * 8 + 8]);
                    }
                } else {
                    #[repr(align(16))]
                    struct A([i16; 64]);
                    let mut d = A([0i16; 64]);
                    for i in 0..4 {
                        let deq = dequantize(&c_q[c][i], qpc);
                        for j in 0..16 {
                            d.0[i * 16 + j] = deq[j] as i16;
                        }
                        d.0[i * 16] = c_recon_dc[c][i] as i16;
                    }
                    rusty_h264_accel::idct_four_t4_rec(&mut plane[base..], self.ccw, &c_pred[c], 8, &d.0);
                }
            }
            #[cfg(not(accel))]
            {
                // Dequantize the 4 blocks (raster, DC overridden by the 2×2-Hadamard
                // recon), then batch the inverse DCT and share the add+clip tail.
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
        }
        // MV grid + coded flags were set per partition; mark modes as DC.
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
        }
        InterPlan { mvds, n_mvd, cbp, q_blocks, c_dc_levels, c_q }
    }

    /// Code one planned inter macroblock as CAVLC (the original `encode_inter_mb_v1_b`
    /// tail). `plan_inter_mb` already committed the reconstruction + motion grids.
    #[allow(clippy::too_many_arguments)]
    fn encode_inter_mb_v1_b(
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
        bspec: Option<BInter>,
    ) {
        let plan = self.plan_inter_mb(refs, sy, su, sv, mb_x, mb_y, mode, parts, bspec);
        self.emit_inter_cavlc(w, refs.len(), mb_x, mb_y, mode, parts, bspec, &plan);
    }

    /// CAVLC entropy coding for a planned inter macroblock.
    #[allow(clippy::too_many_arguments)]
    fn emit_inter_cavlc(
        &mut self,
        w: &mut BitWriter,
        num_refs: usize,
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        parts: &[(i32, (i32, i32))],
        bspec: Option<BInter>,
        plan: &InterPlan,
    ) {
        let w4 = self.mb_w * 4;
        let (cbp, cbp_luma, cbp_chroma) = (plan.cbp, plan.cbp & 15, plan.cbp >> 4);
        // mb_pred order (spec 7.3.5.1): mb_type, then all ref_idx_l0, then all mvd_l0.
        // B-slice mb_type = the B direction 1/2/3; P-slice uses `mode`. ref_idx coded
        // only when >1 reference is active.
        w.write_ue(bspec.map_or(mode as u32, |b| b.dir as u32)); // inter mb_type
        if num_refs > 1 {
            for &(refi, _) in parts {
                write_ref_idx(w, refi, num_refs);
            }
        }
        for &(mvdx, mvdy) in &plan.mvds[..plan.n_mvd] {
            w.write_se(mvdx);
            w.write_se(mvdy);
        }
        write_cbp_inter(w, cbp);
        if cbp != 0 {
            w.write_se(self.qp_delta()); // mb_qp_delta (AQ per-MB QPy)
        }
        self.nnz_cache_load(mb_x, mb_y);
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = self.nc_pred(lbx, lby);
                let scan16 = scan_4x4_dcac(&plan.q_blocks[lby * 4 + lbx]);
                encode_residual_block(w, &scan16, 16, nc) as u8
            } else {
                0
            };
            self.nnz_cache_set(lbx, lby, total);
            self.nnz_y[by * w4 + bx] = total;
        }
        if cbp_chroma != 0 {
            for c in 0..2 {
                encode_residual_block(w, &plan.c_dc_levels[c], 4, -1);
            }
        }
        if cbp_chroma == 2 {
            self.chroma_cache_load(mb_x, mb_y);
            let w2 = self.mb_w * 2;
            for c in 0..2 {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    let nc = self.chroma_nc_pred(c, bx, by);
                    let ac = scan_4x4_ac(&plan.c_q[c][by * 2 + bx]);
                    let total = encode_residual_block(w, &ac, 15, nc) as u8;
                    self.chroma_nnz_cache_set(c, bx, by, total);
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
                }
            }
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
            #[repr(align(16))]
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
        // Fast path: one dct_four_t4 covers the whole 8x8 chroma plane region (all 4
        // blocks, residual+DCT fused, no scalar gather). Block order is the quad's
        // z-scan == raster for 2x2, so block b's DC (pre-quant) sits at dct[b*16] —
        // exactly the dc2x2 the Hadamard check needs. quant_four_4x4 with our FF/MF
        // is bit-identical to scalar `quantize`; AC-free = positions 1..16 all zero.
        #[cfg(accel)]
        if self.skip_accel_check {
            #[repr(align(16))]
            struct Align16C([i16; 64]);
            let mut dct = Align16C([0i16; 64]);
            let ff = rusty_h264_common::transform::quant_dz_ff(qpc, 6);
            let mf = &rusty_h264_common::transform::QUANT_MF_OH[qpc as usize];
            for c in 0..2 {
                let src = if c == 0 { su } else { sv };
                rusty_h264_accel::dct_four_t4(
                    &mut dct.0,
                    &src[(mb_y * 8) * self.ccw + mb_x * 8..],
                    self.ccw,
                    &pred_c[c],
                    8,
                );
                let dc2x2 = [
                    dct.0[0] as i32,
                    dct.0[16] as i32,
                    dct.0[32] as i32,
                    dct.0[48] as i32,
                ];
                rusty_h264_accel::quant_four_4x4(&mut dct.0, &ff, mf);
                for b in 0..4 {
                    if dct.0[b * 16 + 1..b * 16 + 16].iter().any(|&v| v != 0) {
                        return false;
                    }
                }
                if forward_quant_chroma_dc(&dc2x2, qpc, false).iter().any(|&v| v != 0) {
                    return false;
                }
            }
            return true;
        }
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
    #[allow(clippy::too_many_arguments)]
    fn commit_skip_probe_marker(&self) {}
    fn commit_skip(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        mv: (i32, i32),
        pred_y: &[u8; 256],
        pred_c: &[[u8; 64]; 2],
    ) {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::MvGrid);
        // Skip recon = the prediction verbatim: straight row copies (byte-identical
        // to the old per-4x4 gather + store scatter, ~5x fewer ops).
        let base = mb_y * 16 * self.cw + mb_x * 16;
        for r in 0..16 {
            let d = base + r * self.cw;
            self.rec_y[d..d + 16].copy_from_slice(&pred_y[r * 16..r * 16 + 16]);
        }
        let cbase = mb_y * 8 * self.ccw + mb_x * 8;
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for r in 0..8 {
                let d = cbase + r * self.ccw;
                plane[d..d + 8].copy_from_slice(&pred_c[c][r * 8..r * 8 + 8]);
            }
        }
        self.set_mb_mv(mb_x, mb_y, mv, true, 0);
        let w4 = self.mb_w * 4;
        for row in 0..4 {
            let st = (mb_y * 4 + row) * w4 + mb_x * 4;
            self.modes_y[st..st + 4].fill(2);
            self.coded_y[st..st + 4].fill(true);
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
    fe.cur_qp = qp;
    if cfg.cabac_dz_div > 0 {
        fe.idz = cfg.cabac_dz_div; // CABAC-specific dead-zone override
    } // QPY_PREV starts at the slice QP so the first mb_qp_delta is 0
    let (sy, su, sv) = coded_source(cfg, frame);
    let lambda = 0.85 * fe.tune_lambda_scale * 2f64.powf((qp as f64 - 12.0) / 3.0);
    let num_refs = refs.len();
    // Content-adaptive cost-function dispatch (codec-content-adaptive-dispatch): the
    // fast preset prices modes by cheap SAD, which is rate-blind on detailed MBs;
    // route the top `satd_q` fraction of highest-VARIANCE MBs to the rate-faithful
    // SATD cost. A per-frame PERCENTILE threshold makes the routed fraction — hence
    // the speed/quality split — content-invariant (same q → same fraction on any
    // clip). `satd_q == 0` leaves the threshold at MAX (pure SAD, byte-identical).
    if is_p && fe.satd_q > 0.0 {
        let mut vars: Vec<i64> = (0..fe.mb_h)
            .flat_map(|my| (0..fe.mb_w).map(move |mx| (mx, my)))
            .map(|(mx, my)| mb_variance(&sy, fe.cw, mx, my))
            .collect();
        vars.sort_unstable();
        let idx = (((1.0 - fe.satd_q) * vars.len() as f64) as usize).min(vars.len() - 1);
        fe.satd_var_thresh = vars[idx];
    }
    // Adaptive Quantization: per-MB target QPy from content (finer on flat MBs,
    // coarser on busy ones). `mb_qpy` records each MB's ACTUAL QPy (a skip / cbp==0
    // MB inherits `cur_qp`), for the deblock filter. `strength 0` → uniform → the
    // mb_qp_delta stays 0, byte-identical.
    let aq_qp = aq_qp_map(&sy, fe.cw, fe.mb_w, fe.mb_h, qp, fe.aq_strength);
    fe.cur_qp = qp;
    let mut mb_qpy = vec![qp; fe.mb_w * fe.mb_h];
    let mut skip_run = 0u32;
    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            let mb_idx = mb_y * fe.mb_w + mb_x;
            fe.qp = aq_qp[mb_idx];
            fe.qpc = chroma_qp(aq_qp[mb_idx]);
            // P_Skip: motion-compensate from the most-recent reference; accept if free.
            // Chosen inter coding: (mb_type, per-partition (ref_idx, mv)).
            let mut inter: Option<InterChoice> = None;
            if is_p {
                if num_refs > 0 {
                    // P_Skip prediction (reference 0). A free skip (zero residual) is
                    // taken immediately; the quality preset also takes a greedy P_Skip
                    // when its SAD is below the neighbour-predicted bound (below).
                    let _g_skip = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncSkip);
                    let _g_smc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Neighbors);
                    let mv_skip = fe.skip_mv(mb_x, mb_y);
                    let skip_y = fe.skip_predict_luma(refs, mb_x, mb_y, mv_skip);
                    drop(_g_smc);
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
                        mb_qpy[mb_idx] = fe.cur_qp; // skip inherits QPy
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
                        // Adaptive dispatch: high-variance MBs price by SATD (both
                        // inter — via `mb_use_satd` in `best_part` — and intra), the
                        // rest by cheap SAD. Set the per-MB flag before best_part.
                        fe.mb_use_satd = fe.satd_q > 0.0
                            && mb_variance(&sy, fe.cw, mb_x, mb_y) >= fe.satd_var_thresh;
                        let (r16, mv16, cost_inter) =
                            fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 16, &[], lme);
                        let cost_intra = if fe.mb_use_satd {
                            fe.best_i16_satd(&sy, mb_x, mb_y)
                        } else {
                            fe.best_i16_sad(&sy, mb_x, mb_y)
                        } + (lme * fe.tune_intra_penalty) as i64;
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
                            mb_qpy[mb_idx] = fe.cur_qp; // skip inherits QPy
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
                            + (lme * fe.tune_intra_penalty) as i64;
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
            mb_qpy[mb_idx] = fe.cur_qp; // ACTUAL QPy (updated iff an mb_qp_delta was coded)
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
    // Per-MB actual QPy (AQ varies it; `mb_qp_delta`-driven). With `aq_strength 0`
    // this is uniform, reproducing the old scalar-QP filtering exactly.
    drop(_g_fin);
    rusty_h264_common::deblock::filter_frame(
        &mut fe.rec_y,
        &mut fe.rec_u,
        &mut fe.rec_v,
        fe.mb_w,
        fe.mb_h,
        &mb_qpy,
        0, // chroma_qp_index_offset — the encoder emits 0
        0, // slice_alpha_c0_offset — the encoder always signals zero offsets
        0, // slice_beta_offset
        &info,
    );
    let w4 = fe.mb_w * 4;
    crate::RefFrame {
        y: fe.rec_y,
        u: fe.rec_u,
        v: fe.rec_v,
        poc: 0,       // set by the caller (it knows the display order)
        frame_num: 0, // set by the caller
        // List-0 motion field, for a later B-frame's spatial-direct colZeroFlag.
        mv: fe.mv_y,
        ref_idx: fe.ref_idx_y,
        w4,
    }
}

/// Codes a B-slice's macroblock layer. B-frames are **non-reference**, so the
/// reconstruction is computed (the CAVLC nnz predictor needs it) but discarded.
///
/// This brick: every MB is coded `B_L0_16x16` (`mb_type == 1`) — a real
/// motion-compensated prediction from `l0` (the nearest PAST anchor, List-0 index
/// 0) plus a coded residual. Because every MB is List-0-only with `ref_idx`
/// inferred 0, the per-4×4 List-0 motion field and its median `mvd` predictor are
/// byte-identical to the P-slice `P_L0_16x16` path — so this reuses
/// [`FrameEncoder::encode_inter_mb_v1_b`] verbatim, differing from P only in the
/// `mb_type` value. `l1` (nearest future anchor) is unused until `B_Bi` lands.
#[allow(clippy::too_many_arguments)]
pub fn encode_slice_data_b(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    frame: &YuvFrame,
    qp: u8,
    poc: i32,
    l0: &crate::RefFrame,
    l1: &crate::RefFrame,
) {
    let mut fe = FrameEncoder::new(cfg);
    fe.qp = qp;
    fe.qpc = chroma_qp(qp);
    fe.cur_qp = qp;
    if cfg.cabac_dz_div > 0 {
        fe.idz = cfg.cabac_dz_div; // CABAC-specific dead-zone override
    } // QPY_PREV starts at the slice QP so the first mb_qp_delta is 0
    // Implicit bi-prediction weights from the anchor POC distances (matches the
    // decoder). Equidistant B (bframes==1) → 32:32 (plain average); unequal → weighted.
    fe.bi_w = implicit_bi_weights(poc, l0.poc, l1.poc);
    let (sy, su, sv) = coded_source(cfg, frame);
    let lambda = 0.85 * fe.tune_lambda_scale * 2f64.powf((qp as f64 - 12.0) / 3.0);
    let lme = lambda.sqrt();
    let refs = std::slice::from_ref(l0); // List-0 = [nearest past anchor]
    // Same content-adaptive SAD→SATD dispatch as the P path (codec-content-adaptive-
    // dispatch): the top `satd_q` fraction of highest-variance MBs price by SATD.
    if fe.satd_q > 0.0 {
        let mut vars: Vec<i64> = (0..fe.mb_h)
            .flat_map(|my| (0..fe.mb_w).map(move |mx| (mx, my)))
            .map(|(mx, my)| mb_variance(&sy, fe.cw, mx, my))
            .collect();
        vars.sort_unstable();
        let idx = (((1.0 - fe.satd_q) * vars.len() as f64) as usize).min(vars.len() - 1);
        fe.satd_var_thresh = vars[idx];
    }
    let mut skip_run = 0u32; // run of consecutive B_Skip MBs pending a coded MB
    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            let (lx, ly) = (mb_x * 16, mb_y * 16);
            let (pbx, pby) = (mb_x as isize * 4, mb_y as isize * 4);
            fe.mb_use_satd =
                fe.satd_q > 0.0 && mb_variance(&sy, fe.cw, mb_x, mb_y) >= fe.satd_var_thresh;
            // Per-list median MV predictors — the search-rate center AND the actual
            // `mvd` predictor (identical to the decoder's `predict_partition_mv`).
            let n0 = fe.mv_neighbors_block_list(pbx, pby, 4, 0);
            let n1 = fe.mv_neighbors_block_list(pbx, pby, 4, 1);
            let pmv0 = predict_partition_mv(0, 0, n0[0], n0[1], n0[2], 0);
            let pmv1 = predict_partition_mv(0, 0, n1[0], n1[1], n1[2], 0);
            // Independent List-0 / List-1 motion searches (their J already includes
            // the mvd rate against the matching predictor, so J0/J1 compare directly).
            // Spatial-direct prediction (basis of B_Skip and B_Direct_16x16).
            let (dp, dc, dmotion) = fe.b_direct(l0, l1, mb_x, mb_y);
            // B_Skip: take the direct prediction with NO coded residual (~1 bit in
            // the mb_skip_run) only when it is truly FREE — its residual quantizes to
            // zero at the B QP, so skipping loses nothing. (A looser SATD-threshold
            // skip was measured strictly WORSE: on B's derived prediction the SATD
            // proxy over-values the skip, dropping residual the quantizer wanted —
            // the same proxy-vs-quantization gap seen on sub-pel. So skip only when
            // provably free; the rest goes through the L0/L1/Bi/Direct RD decision.)
            if fe.skip_luma_is_free(&sy, mb_x, mb_y, &dp)
                && fe.skip_chroma_is_free(&su, &sv, mb_x, mb_y, &dc)
            {
                fe.commit_direct_motion(mb_x, mb_y, &dmotion);
                skip_run += 1;
                continue;
            }
            let d_direct = fe.pred_dist(&sy, lx, ly, &dp);
            let (mv0, j0) = fe.motion_search(l0, &sy, lx, ly, 16, 16, &[pmv0], lme);
            let (mv1, j1) = fe.motion_search(l1, &sy, lx, ly, 16, 16, &[pmv1], lme);
            // Bi: average the two winners' predictions; rate = both mvds.
            let d_bi = fe.bi_dist(l0, l1, &sy, lx, ly, mv0, mv1);
            let r_bi = mvd_bits(mv0.0 - pmv0.0) + mvd_bits(mv0.1 - pmv0.1)
                + mvd_bits(mv1.0 - pmv1.0) + mvd_bits(mv1.1 - pmv1.1);
            let j_bi = d_bi + (lme * r_bi as f64) as i64;
            // B_Direct (mb_type 0): spatial-direct prediction, NO coded MV — so its
            // J (d_direct, computed above) carries zero mvd rate and it wins wherever
            // the derived motion predicts as well as an explicit vector.
            // Pick the cheapest of {0=Direct, 1=L0, 2=L1, 3=Bi}; Direct wins ties.
            let (mut dir, mut best) = (0u8, d_direct);
            if j0 < best { dir = 1; best = j0; }
            if j1 < best { dir = 2; best = j1; }
            if j_bi < best { dir = 3; best = j_bi; }
            let _ = best;
            w.write_ue(skip_run); // run of B_Skips preceding this coded MB
            skip_run = 0;
            let bspec = BInter { dir, l1, mv0, mv1 };
            fe.encode_inter_mb_v1_b(w, refs, &sy, &su, &sv, mb_x, mb_y, 0, &[], Some(bspec));
        }
    }
    if skip_run > 0 {
        w.write_ue(skip_run); // trailing B_Skip run
    }
    w.rbsp_trailing_bits();
}

/// `se(d)` Exp-Golomb bit length — the `mvd`-component rate for the B mode
/// decision. Same closed form as `motion_search`'s private `mvbits` (kept separate
/// so the P search's heuristic — and thus P output — is untouched).
#[inline(always)]
fn mvd_bits(d: i32) -> u32 {
    let codenum = if d > 0 { (2 * d - 1) as u32 } else { (-2 * d) as u32 };
    1 + 2 * (31 - (codenum + 1).leading_zeros())
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

/// A fully-decided intra macroblock: the mode decision, the quantized coefficients,
/// and the committed reconstruction. Produced by [`plan_mb`] (which reuses the
/// entire mode-decision + transform + reconstruct path), then consumed by an
/// entropy backend — `emit_mb_cavlc` or `emit_mb_cabac` — so the two coders share
/// every non-entropy decision bit-for-bit (the bringup-encoder reuse guarantee).
struct MbPlan {
    use_i4: bool,
    // I_16x16 (when !use_i4): prediction mode, whether any AC is coded (cbp_luma=15),
    // luma DC levels (block order), per-4×4 quantized AC (raster).
    i16_mode: I16Mode,
    i16_cbp15: bool,
    i16_dc_levels: [i32; 16],
    i16_q: [[i32; 16]; 16],
    // I_4x4 (when use_i4 && i8 is None): the sub-plan, already reconstructed.
    i4: Option<I4Plan>,
    // I_8x8 (High profile; when use_i4 && i8 is Some): the sub-plan, already
    // reconstructed. use_i4 means "I_NxN"; i8 present disambiguates 8x8 from 4x4.
    i8: Option<I8Plan>,
    // Chroma (shared by both luma types).
    chroma_mode: u8,
    cbp_chroma: u32,
    c_dc_levels: [[i32; 4]; 2],
    c_q_blocks: [[[i32; 16]; 4]; 2],
}

/// A fully-decided inter macroblock: the per-partition motion residuals, coded
/// block pattern, and quantized residual, with the reconstruction + motion grids
/// already committed. Produced by [`FrameEncoder::plan_inter_mb`] (which reuses the
/// whole MC + residual + reconstruct path), then coded by `emit_inter_cavlc` or
/// `emit_inter_cabac` — so the two entropy backends share every non-entropy
/// decision bit-for-bit (the P/B analogue of [`MbPlan`]).
struct InterPlan {
    mvds: [(i32, i32); 4], // per-partition mvd (P: mvd_l0; B: mvd_l0 then mvd_l1)
    n_mvd: usize,
    cbp: u32,
    q_blocks: [[i32; 16]; 16], // luma quantized levels (raster)
    c_dc_levels: [[i32; 4]; 2],
    c_q: [[[i32; 16]; 4]; 2],
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
/// Neighbour 4x4 block intra mode for the MPM candidate: in-MB blocks read the
/// in-progress local `modes`; blocks in earlier MBs read `fe.modes_y`. (bx, by)
/// are the current block's absolute 4x4 grid coords.
#[inline]
fn modes_at(fe: &FrameEncoder, modes: &[u8; 16], lbx: usize, lby: usize, dx: isize, dy: isize, bx: usize, by: usize) -> u8 {
    let (nx, ny) = (lbx as isize + dx, lby as isize + dy);
    if (0..4).contains(&nx) && (0..4).contains(&ny) {
        modes[ny as usize * 4 + nx as usize]
    } else {
        let w4 = fe.mb_w * 4;
        let gx = (bx as isize + dx) as usize;
        let gy = (by as isize + dy) as usize;
        fe.modes_y[gy * w4 + gx]
    }
}

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

        // Pick the lowest-SATD available mode. RUSTY_FAST_INTRA prunes the
        // candidate set to {MPM, DC, V, H} (x264-ultrafast-style); the H.264
        // predicted mode (min of left/top block modes, DC on the edge) keeps the
        // 1-bit prev_intra4x4_pred_mode signalling cheap for the common winner.
        let mut best_m = 2u8;
        let mut best_cost = i64::MAX;
        if fe.fast && fast_intra_enabled() {
            let lm = if bx > 0 { modes_at(fe, &modes, lbx, lby, -1, 0, bx, by) } else { 2 };
            let tm = if by > 0 { modes_at(fe, &modes, lbx, lby, 0, -1, bx, by) } else { 2 };
            let mpm = lm.min(tm);
            let mut cands = [mpm, 2u8, 0, 1];
            for i in 1..4 {
                for j in 0..i {
                    if cands[i] == cands[j] {
                        cands[i] = 255;
                    }
                }
            }
            for &m in cands.iter() {
                if m == 255 || !i4_mode_available(m, avail_top, avail_left) {
                    continue;
                }
                let pred = intra4x4_pred(m, avail_top, avail_left, &top, &left, corner);
                let cost = satd_4x4(sy, fe.cw, px, py, &pred);
                if cost < best_cost {
                    best_cost = cost;
                    best_m = m;
                }
            }
        } else {
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
        }

        // Quantize + reconstruct with the chosen mode.
        let pred = intra4x4_pred(best_m, avail_top, avail_left, &top, &left, corner);
        let mut predb = [0i32; 16];
        for i in 0..16 {
            predb[i] = pred[i] as i32;
        }
        let res = residual(sy, fe.cw, px, py, &predb);
        let qb = rdoq(&forward_core(&res), qp, fe.idz, fe.rdoq_strength, 0); // full 16 incl DC
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

/// A planned I_8x8 macroblock (High profile): one intra8x8 mode + one 8x8 DCT per
/// 8x8 block. Reconstructed serially into `rec_y` (each block predicts from the
/// previous), and `modes_y` written per block so later blocks' MPM sees earlier.
struct I8Plan {
    modes: [u8; 4],     // per-8x8-block intra8x8 mode (raster b8 0..3)
    q: [[i32; 64]; 4],  // per-8x8-block quantized levels (raster)
    cbp_luma: u32,      // 4-bit coded-block-pattern (one bit per 8x8 block)
    nonzero: i64,       // rate proxy
}

/// Forward zig-zag scan of a raster 8x8 block: `scan[i] = raster[ZIGZAG_8X8[i]]`
/// (the inverse of the decoder's `un_scan_8x8`).
const ZIGZAG_8X8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[inline]
fn scan_8x8_fwd(raster: &[i32; 64]) -> [i32; 64] {
    std::array::from_fn(|i| raster[ZIGZAG_8X8[i]])
}

/// Gather the 8x8 intra reference samples (top[16] incl top-right, left[8], corner)
/// from `rec_y` — the encoder counterpart of the decoder's `gather_i8`.
fn gather_i8_enc(
    fe: &FrameEncoder,
    px: usize,
    py: usize,
    avail_top: bool,
    avail_left: bool,
    bx: usize,
    by: usize,
) -> ([u8; 16], [u8; 8], u8, bool) {
    let (cw, w4) = (fe.cw, fe.mb_w * 4);
    let mut top = [0u8; 16];
    let mut left = [0u8; 8];
    let mut corner = 0;
    if avail_top {
        for i in 0..8 {
            top[i] = fe.rec_y[(py - 1) * cw + px + i];
        }
        let tr_avail = bx + 2 < w4 && fe.coded_y[(by - 1) * w4 + (bx + 2)];
        for i in 0..8 {
            top[8 + i] = if tr_avail {
                fe.rec_y[(py - 1) * cw + px + 8 + i]
            } else {
                top[7]
            };
        }
    }
    if avail_left {
        for i in 0..8 {
            left[i] = fe.rec_y[(py + i) * cw + px - 1];
        }
    }
    let avail_corner = avail_top && avail_left;
    if avail_corner {
        corner = fe.rec_y[(py - 1) * cw + px - 1];
    }
    (top, left, corner, avail_corner)
}

/// Plans an I_8x8 macroblock: per 8x8 block, picks the lowest-SATD intra8x8 mode,
/// 8x8-forward-transforms + quantizes, and reconstructs serially into `rec_y`.
fn plan_i8x8(fe: &mut FrameEncoder, sy: &[u8], mb_x: usize, mb_y: usize, qp: u8) -> I8Plan {
    let w4 = fe.mb_w * 4;
    let mut modes = [2u8; 4];
    let mut q = [[0i32; 64]; 4];
    let mut cbp_luma = 0u32;
    let mut nonzero = 0i64;
    let weight = [16i32; 64];

    for b8 in 0..4usize {
        let (b8x, b8y) = (b8 % 2, b8 / 2);
        let (px, py) = (mb_x * 16 + b8x * 8, mb_y * 16 + b8y * 8);
        let (bx, by) = (mb_x * 4 + b8x * 2, mb_y * 4 + b8y * 2); // top-left 4x4 cell
        let avail_top = b8y > 0 || mb_y > 0;
        let avail_left = b8x > 0 || mb_x > 0;
        let (top, left, corner, avail_corner) =
            gather_i8_enc(fe, px, py, avail_top, avail_left, bx, by);

        // Mode decision: lowest-SATD available intra8x8 mode (same 9 modes / avail
        // rules as intra4x4). The MPM (predict_i4_mode on the top-left 4x4) keeps the
        // 1-bit prev-mode signalling cheap; a small penalty biases toward it.
        let predicted = predict_i4_mode(fe, bx, by);
        let mut best_m = 2u8;
        let mut best_cost = i64::MAX;
        for m in 0..9u8 {
            if !i4_mode_available(m, avail_top, avail_left) {
                continue;
            }
            let pred = intra8x8_pred(m, avail_top, avail_left, avail_corner, &top, &left, corner);
            let mut cost = satd_8x8(sy, fe.cw, px, py, &pred);
            if m != predicted {
                cost += 4 * fe.qp as i64; // ~mode-signal penalty (rem vs prev flag)
            }
            if cost < best_cost {
                best_cost = cost;
                best_m = m;
            }
        }
        modes[b8] = best_m;

        // Forward 8x8 transform + quantize + reconstruct (shared decoder primitives).
        let pred = intra8x8_pred(best_m, avail_top, avail_left, avail_corner, &top, &left, corner);
        let mut res = [0i32; 64];
        for dy in 0..8 {
            for dx in 0..8 {
                res[dy * 8 + dx] =
                    sy[(py + dy) * fe.cw + (px + dx)] as i32 - pred[dy * 8 + dx] as i32;
            }
        }
        let levels = quantize_8x8(&forward_core_8x8(&res), qp, &weight, fe.idz);
        let nz = levels.iter().filter(|&&v| v != 0).count();
        if nz > 0 {
            cbp_luma |= 1 << b8;
        }
        nonzero += nz as i64;
        q[b8] = levels;

        let res_r = inverse_quant_8x8(&levels, qp, &weight);
        let predb: [i32; 64] = std::array::from_fn(|i| pred[i] as i32);
        let recon = add_residual_8x8(&res_r, &predb);
        for dy in 0..8 {
            for dx in 0..8 {
                fe.rec_y[(py + dy) * fe.cw + (px + dx)] = recon[dy * 8 + dx];
            }
        }
        // Publish the mode into all four 4x4 cells + mark coded — so the next 8x8
        // block's MPM (and later MBs' neighbours) see it, exactly as the decoder does.
        for sry in 0..2 {
            for srx in 0..2 {
                fe.modes_y[(by + sry) * w4 + (bx + srx)] = best_m;
                fe.coded_y[(by + sry) * w4 + (bx + srx)] = true;
            }
        }
    }
    I8Plan {
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
/// Zig-zag scan: block (raster 4×4) index at scan position i.
const RDOQ_ZZ: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Approximate CABAC bit cost of coding one residual coefficient at magnitude
/// `level`: significant_coeff_flag (~1) + coeff_abs_level_minus1 bins (gt1 + UEG0)
/// + sign (~1); `level == 0` is significant_coeff_flag = 0 (~1). A coarse model —
/// the transform-norm/bin-to-bit scaling is absorbed into the calibrated strength.
#[inline]
fn rdoq_rate(level: i64) -> f64 {
    if level == 0 {
        1.0
    } else if level == 1 {
        3.0 // sig(1) + gt1=0 (1) + sign(1)
    } else {
        // sig(1) + gt1=1 (1) + UEG0(level-2) prefix (~level-1, capped) + sign(1)
        3.0 + (level - 1).min(13) as f64
    }
}

/// Rate-distortion optimized quantization (CABAC trellis, RDOQ) for one 4×4 residual
/// block. Refines the hard-decision levels toward min over {|q|, |q|-1} of
/// `SSD_coef + λ·R_cabac` per coefficient (coefficient-domain distortion
/// `(|coeff| - level·deq_step)²`; `λ = strength·2^((qp-12)/3)`). `strength == 0`
/// returns the hard quantization unchanged (the CAVLC path). `first` = 1 skips the
/// DC (AC-only categories: I_16x16 AC, chroma AC), else 0.
fn rdoq(coeffs: &[i32; 16], qp: u8, dz_div: i64, strength: f64, first: usize) -> [i32; 16] {
    let mut q = quantize(coeffs, qp, dz_div);
    if strength <= 0.0 {
        return q;
    }
    let lambda = strength * 2f64.powf((qp as f64 - 12.0) / 3.0);
    // Distortion is measured in the QUANTIZER-INPUT (forward-transform) domain, where
    // level L reconstructs to L·qstep, qstep = 2^16 / MF (the inverse of the forward
    // quant scale). The transform norm (forward↔pixel) folds into `strength`.
    let mf = &rusty_h264_common::transform::QUANT_MF_OH[qp as usize];
    const POS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5, 6, 7];
    let dist = |p: usize, level: i64| -> f64 {
        let e = coeffs[p].unsigned_abs() as f64 - level as f64 * (65536.0 / mf[POS[p]] as f64);
        e * e
    };
    // Pass 1: per-coefficient level lowering (|q| → |q|-1) minimizing D + λ·R.
    for i in first..16 {
        let p = RDOQ_ZZ[i];
        let m = q[p].unsigned_abs() as i64;
        if m == 0 {
            continue;
        }
        let j_keep = dist(p, m) + lambda * rdoq_rate(m);
        let j_down = dist(p, m - 1) + lambda * rdoq_rate(m - 1);
        if j_down < j_keep {
            let nl = (m - 1) as i32;
            q[p] = if q[p] < 0 { -nl } else { nl };
        }
    }
    // Pass 2: last-significant-position trimming. Zeroing the trailing significant
    // coefficient frees its own bits AND the last_significant flag + every sig=0 flag
    // between it and the previous significant coefficient (positions past the new last
    // aren't coded at all) — the dominant RDOQ gain on sparse (inter) residuals.
    loop {
        let Some(li) = (first..16).rev().find(|&i| q[RDOQ_ZZ[i]] != 0) else {
            break;
        };
        let p = RDOQ_ZZ[li];
        let m = q[p].unsigned_abs() as i64;
        let prev = (first..li).rev().find(|&i| q[RDOQ_ZZ[i]] != 0);
        let base = prev.map_or(first, |j| j + 1);
        let bits = rdoq_rate(m) + 1.0 + (li - base) as f64; // coeff + last-flag + freed sig=0
        let d_add = dist(p, 0) - dist(p, m);
        if d_add < lambda * bits {
            q[p] = 0;
        } else {
            break;
        }
    }
    q
}

/// Decide one intra macroblock (I_16x16 vs I_4x4, prediction modes, chroma),
/// forward-transform + quantize, and commit the reconstruction + neighbour mode
/// state — everything except entropy coding. The returned [`MbPlan`] is coded by
/// either entropy backend, so CAVLC and CABAC share this whole path bit-for-bit.
fn plan_mb(
    fe: &mut FrameEncoder,
    mb_x: usize,
    mb_y: usize,
    sy: &[u8],
    su: &[u8],
    sv: &[u8],
) -> MbPlan {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncIntraCode);
    let qp = fe.qp;
    let qpc = fe.qpc;
    // Lagrangian λ for rate-distortion decisions (standard H.264 form).
    let lambda = 0.85 * fe.tune_lambda_scale * 2f64.powf((qp as f64 - 12.0) / 3.0);

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
    // Fast path: forward DCT of (src - pred) straight from the planes per 8x8 quad,
    // quantize with the identical FF/MF math (deadzone = fe.idz), recon via the
    // bit-identical idct+add+clip kernel — the same pairing encode_inter_mb and the
    // P_Skip free-check already use, byte-identical to the scalar twin below.
    #[cfg(accel)]
    let (i16_dc_levels, _i16_recon_dc, recon16) = {
        #[repr(align(16))]
        struct A([i16; 256]);
        let mut dct = A([0i16; 256]);
        let base = ly * fe.cw + lx;
        for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
            rusty_h264_accel::dct_four_t4(
                &mut dct.0[qi * 64..qi * 64 + 64],
                &sy[base + qy * fe.cw + qx..],
                fe.cw,
                &best_pred[qy * 16 + qx..],
                16,
            );
        }
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            dc4x4[lby * 4 + lbx] = dct.0[blk * 16] as i32;
        }
        if fe.rdoq_strength > 0.0 {
            // Trellis (all-intra only): scalar RDOQ from the asm DCT output instead of
            // the asm hard quantizer. dct.0 keeps the raw DCT here; the recon loop below
            // overwrites it with the dequantized RDOQ levels.
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let coeffs: [i32; 16] = std::array::from_fn(|i| dct.0[blk * 16 + i] as i32);
                let mut q = rdoq(&coeffs, qp, fe.idz, fe.rdoq_strength, 1);
                q[0] = 0;
                i16_q[lby * 4 + lbx] = q;
            }
        } else {
            let ff = rusty_h264_common::transform::quant_dz_ff(qp, fe.idz);
            let mf = &rusty_h264_common::transform::QUANT_MF_OH[qp as usize];
            for qi in 0..4 {
                rusty_h264_accel::quant_four_4x4(&mut dct.0[qi * 64..qi * 64 + 64], &ff, mf);
            }
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let q = &mut i16_q[lby * 4 + lbx];
                q[0] = 0;
                for i in 1..16 {
                    q[i] = dct.0[blk * 16 + i] as i32;
                }
            }
        }
        let i16_dc_levels = forward_quant_luma_dc(&dc4x4, qp, true);
        let i16_recon_dc = inverse_quant_luma_dc(&i16_dc_levels, qp);
        // Recon: dequantize (DC injected from the Hadamard) back into quad layout,
        // then idct+add-pred+clip into the trial buffer.
        let mut recon16 = [0u8; 256];
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let mut deq = dequantize(&i16_q[lby * 4 + lbx], qp);
            deq[0] = i16_recon_dc[lby * 4 + lbx];
            for i in 0..16 {
                dct.0[blk * 16 + i] = deq[i] as i16;
            }
        }
        for (qi, &(qx, qy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
            rusty_h264_accel::idct_four_t4_rec(
                &mut recon16[qy * 16 + qx..],
                16,
                &best_pred[qy * 16 + qx..],
                16,
                &dct.0[qi * 64..qi * 64 + 64],
            );
        }
        (i16_dc_levels, i16_recon_dc, recon16)
    };
    #[cfg(not(accel))]
    let (i16_dc_levels, _i16_recon_dc, recon16) = {
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
            let mut q = rdoq(&coeffs[i], qp, fe.idz, fe.rdoq_strength, 1);
            q[0] = 0;
            i16_q[i] = q;
        }
        let i16_dc_levels = forward_quant_luma_dc(&dc4x4, qp, true);
        let i16_recon_dc = inverse_quant_luma_dc(&i16_dc_levels, qp);
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
        (i16_dc_levels, i16_recon_dc, recon16)
    };
    let i16_cbp15 = i16_q.iter().any(|b| b[1..].iter().any(|&c| c != 0));
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
        // Fast path: forward DCT of (src - pred8) straight from the planes, quantize
        // with identical FF/MF (idz deadzone), recon via one idct+add+clip kernel —
        // bit-identical to the scalar twin below (proven kernel pairings).
        let mut dc2x2 = [0i32; 4];
        let mut qbs = [[0i32; 16]; 4];
        #[cfg(accel)]
        let recon_dc = {
            #[repr(align(16))]
            struct A([i16; 64]);
            let mut d = A([0i16; 64]);
            rusty_h264_accel::dct_four_t4(&mut d.0, &src[cy * fe.ccw + cx..], fe.ccw, &pred8, 8);
            for i in 0..4 {
                dc2x2[i] = d.0[i * 16] as i32;
            }
            if fe.rdoq_strength > 0.0 {
                // Trellis (all-intra only): scalar RDOQ from the asm chroma DCT.
                for i in 0..4 {
                    let coeffs: [i32; 16] = std::array::from_fn(|j| d.0[i * 16 + j] as i32);
                    let mut q = rdoq(&coeffs, qpc, fe.idz, fe.rdoq_strength, 1);
                    q[0] = 0;
                    if q[1..].iter().any(|&v| v != 0) {
                        any_chroma_ac = true;
                    }
                    qbs[i] = q;
                }
            } else {
                let ff = rusty_h264_common::transform::quant_dz_ff(qpc, fe.idz);
                let mf = &rusty_h264_common::transform::QUANT_MF_OH[qpc as usize];
                rusty_h264_accel::quant_four_4x4(&mut d.0, &ff, mf);
                for i in 0..4 {
                    let q = &mut qbs[i];
                    q[0] = 0;
                    for j in 1..16 {
                        let v = d.0[i * 16 + j] as i32;
                        q[j] = v;
                        if v != 0 {
                            any_chroma_ac = true;
                        }
                    }
                }
            }
            let dl = forward_quant_chroma_dc(&dc2x2, qpc, true);
            if dl.iter().any(|&v| v != 0) {
                any_chroma_dc = true;
            }
            let recon_dc = inverse_quant_chroma_dc(&dl, qpc);
            for i in 0..4 {
                let deq = dequantize(&qbs[i], qpc);
                for j in 0..16 {
                    d.0[i * 16 + j] = deq[j] as i16;
                }
                d.0[i * 16] = recon_dc[i] as i16;
            }
            let plane = if c == 0 { &mut fe.rec_u } else { &mut fe.rec_v };
            rusty_h264_accel::idct_four_t4_rec(&mut plane[cy * fe.ccw + cx..], fe.ccw, &pred8, 8, &d.0);
            c_dc_levels[c] = dl;
            recon_dc
        };
        #[cfg(not(accel))]
        let recon_dc = {
            let mut res_blocks = [[0i32; 16]; 4];
            for by in 0..2 {
                for bx in 0..2 {
                    res_blocks[by * 2 + bx] =
                        residual(src, fe.ccw, cx + bx * 4, cy + by * 4, &pblk(bx, by));
                }
            }
            let mut coeffs = [[0i32; 16]; 4];
            forward_dct_blocks(&res_blocks, &mut coeffs);
            for i in 0..4 {
                dc2x2[i] = coeffs[i][0];
                let mut q = rdoq(&coeffs[i], qpc, fe.idz, fe.rdoq_strength, 1);
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
            recon_dc
        };
        let _ = recon_dc;
        c_q_blocks[c] = qbs;
    }
    let cbp_chroma: u32 = if any_chroma_ac {
        2
    } else if any_chroma_dc {
        1
    } else {
        0
    };

    // ============ I_NxN plan + RD: I_16x16 vs I_4x4 vs (High profile) I_8x8 ============
    // I_4x4 and I_8x8 both reconstruct serially into rec_y, but each block predicts
    // only from NEIGHBOURS + earlier blocks it fills itself — never the stale MB
    // content — so running I_8x8 after I_4x4 needs no restore. J = SSD + λ·R picks the
    // per-MB transform (the content-adaptive win: 8x8 on smooth, 4x4 on detail).
    let base = ly * fe.cw + lx;
    let i4 = if i16_rate > 2 {
        Some(plan_i4x4(fe, sy, mb_x, mb_y, qp))
    } else {
        None
    };
    let (j4, i4_recon) = match &i4 {
        Some(p) => {
            let mut ssd = 0i64;
            let mut rec = [0u8; 256];
            for i in 0..256 {
                let v = fe.rec_y[base + (i / 16) * fe.cw + i % 16];
                rec[i] = v;
                let d = v as i64 - sy[base + (i / 16) * fe.cw + i % 16] as i64;
                ssd += d * d;
            }
            (ssd as f64 + lambda * (p.nonzero + 16) as f64, Some(rec))
        }
        None => (f64::INFINITY, None),
    };
    let i8 = if fe.transform_8x8 {
        Some(plan_i8x8(fe, sy, mb_x, mb_y, qp))
    } else {
        None
    };
    let j8 = match &i8 {
        Some(p) => {
            let mut ssd = 0i64;
            for i in 0..256 {
                let d = fe.rec_y[base + (i / 16) * fe.cw + i % 16] as i64
                    - sy[base + (i / 16) * fe.cw + i % 16] as i64;
                ssd += d * d;
            }
            ssd as f64 + lambda * (p.nonzero + 16) as f64
        }
        None => f64::INFINITY,
    };
    let j16 = ssd16 as f64 + lambda * i16_rate as f64;

    // ============ commit the RD winner's reconstruction + neighbour modes ============
    let (use_i4, i4, i8) = if i8.is_some() && j8 <= j4 && j8 <= j16 {
        // I_8x8: plan_i8x8 already committed rec_y AND modes_y (per 8x8 block).
        (true, None, i8)
    } else if i4.is_some() && j4 < j16 {
        // I_4x4: restore its reconstruction (I_8x8 may have overwritten rec_y), publish modes.
        let rec = i4_recon.unwrap();
        for i in 0..256 {
            fe.rec_y[base + (i / 16) * fe.cw + i % 16] = rec[i];
        }
        let modes = i4.as_ref().unwrap().modes;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = modes[lby * 4 + lbx];
        }
        (true, i4, None)
    } else {
        // I_16x16: commit its reconstruction, mark modes DC.
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
        (false, None, None)
    };
    // Mark all luma blocks coded for the next macroblock's top-right availability.
    for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
        fe.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
    }

    MbPlan {
        use_i4,
        i16_mode,
        i16_cbp15,
        i16_dc_levels,
        i16_q,
        i4,
        i8,
        chroma_mode,
        cbp_chroma,
        c_dc_levels,
        c_q_blocks,
    }
}

/// Emit one planned intra macroblock as CAVLC (the original `encode_mb` tail). Reads
/// only the decided values from `plan`; `plan_mb` already committed recon + modes.
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
    let plan = plan_mb(fe, mb_x, mb_y, sy, su, sv);
    // In a P-slice, intra macroblock types are offset by 5 (0..4 are inter).
    let mb_type_offset = if is_p { 5 } else { 0 };
    let w4 = fe.mb_w * 4;
    let cbp_chroma = plan.cbp_chroma;

    // ============ emit luma ============
    if let Some(i8) = plan.i8.as_ref().filter(|_| plan.use_i4) {
        // ---- I_8x8 (High profile): mb_type = I_NxN, transform_size_8x8_flag = 1, then
        // one intra8x8 mode per 8x8 block, cbp, mb_qp_delta, and the 8x8 residual as
        // four interleaved 4x4 CAVLC sub-blocks (coeff k of sub s -> 8x8 scan 4k+s). ----
        let cbp = i8.cbp_luma | (cbp_chroma << 4);
        w.write_ue(mb_type_offset); // mb_type = I_NxN
        w.write_bit(true); // transform_size_8x8_flag = 1
        for b8 in 0..4usize {
            let (bx, by) = (mb_x * 4 + (b8 % 2) * 2, mb_y * 4 + (b8 / 2) * 2);
            let predicted = predict_i4_mode(fe, bx, by);
            let actual = i8.modes[b8];
            if actual == predicted {
                w.write_bit(true);
            } else {
                w.write_bit(false);
                let rem = if actual < predicted { actual } else { actual - 1 };
                w.write_bits(rem as u32, 3);
            }
        }
        w.write_ue(plan.chroma_mode as u32); // intra_chroma_pred_mode
        write_cbp_intra(w, cbp);
        if cbp != 0 {
            w.write_se(fe.qp_delta());
        }
        fe.nnz_cache_load(mb_x, mb_y);
        for b8 in 0..4usize {
            let (b8x, b8y) = (b8 % 2, b8 / 2);
            let scan8 = scan_8x8_fwd(&i8.q[b8]);
            for sub in 0..4usize {
                let (cx, cy) = (b8x * 2 + sub % 2, b8y * 2 + sub / 2);
                let (bx, by) = (mb_x * 4 + cx, mb_y * 4 + cy);
                let total = if i8.cbp_luma & (1 << b8) != 0 {
                    let nc = fe.nc_pred(cx, cy);
                    let blk: [i32; 16] = std::array::from_fn(|k| scan8[4 * k + sub]);
                    encode_residual_block(w, &blk, 16, nc) as u8
                } else {
                    0
                };
                fe.nnz_cache_set(cx, cy, total);
                fe.nnz_y[by * w4 + bx] = total;
            }
        }
    } else if plan.use_i4 {
        let i4 = plan.i4.as_ref().unwrap();
        let cbp = i4.cbp_luma | (cbp_chroma << 4);
        w.write_ue(mb_type_offset); // mb_type = I_4x4 (+5 in P-slices)
        if fe.transform_8x8 {
            w.write_bit(false); // transform_size_8x8_flag = 0 (this I_NxN is 4x4)
        }
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
        }
        w.write_ue(plan.chroma_mode as u32); // intra_chroma_pred_mode
        write_cbp_intra(w, cbp);
        if cbp != 0 {
            w.write_se(fe.qp_delta()); // mb_qp_delta (AQ per-MB QPy)
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
        let mb_type = 1 + plan.i16_mode as u32 + 4 * cbp_chroma + if plan.i16_cbp15 { 12 } else { 0 };
        w.write_ue(mb_type + mb_type_offset);
        w.write_ue(plan.chroma_mode as u32); // intra_chroma_pred_mode
        w.write_se(fe.qp_delta()); // mb_qp_delta (I_16x16 always codes it; AQ per-MB QPy)
        fe.nnz_cache_load(mb_x, mb_y);
        let nc_dc = fe.nc_pred(0, 0);
        let dc_scan = scan_4x4_dcac(&plan.i16_dc_levels);
        encode_residual_block(w, &dc_scan, 16, nc_dc);
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.nnz_cache_set(lbx, lby, 0);
            fe.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
        }
        if plan.i16_cbp15 {
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let nc = fe.nc_pred(lbx, lby);
                let ac = scan_4x4_ac(&plan.i16_q[lby * 4 + lbx]);
                let total = encode_residual_block(w, &ac, 15, nc) as u8;
                fe.nnz_cache_set(lbx, lby, total);
                fe.nnz_y[by * w4 + bx] = total;
            }
        }
    }

    // ============ emit chroma residual (shared) ============
    if cbp_chroma != 0 {
        for c in 0..2 {
            encode_residual_block(w, &plan.c_dc_levels[c], 4, -1);
        }
    }
    if cbp_chroma == 2 {
        fe.chroma_cache_load(mb_x, mb_y);
        let w2 = fe.mb_w * 2;
        for c in 0..2 {
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let nc = fe.chroma_nc_pred(c, bx, by);
                let ac = scan_4x4_ac(&plan.c_q_blocks[c][by * 2 + bx]);
                let total = encode_residual_block(w, &ac, 15, nc) as u8;
                fe.chroma_nnz_cache_set(c, bx, by, total);
                fe.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total;
            }
        }
    }
}

// ============================================================================
// CABAC I-slice entropy coding — the exact forward inverse of the decoder's
// `decode_slice_data_cabac` I-slice path (rusty_h264-decoder mb16.rs). Every
// binarization + context-selection here mirrors a `parse_*_cabac` there; the
// neighbour state (nzc cache, cbf_dc, cat, cmode, mb_cbp, last_delta_qp) is
// reconstructed identically so the contexts evolve bit-for-bit. Reuses `plan_mb`
// for the entire mode-decision/transform/recon (shared with CAVLC).
// ============================================================================

// --- res-property tables (must match the decoder's mb16.rs g_kBlockCat2CtxOffset*) ---
const CB_NZC_CACHE: [usize; 24] = [
    9, 10, 17, 18, 11, 12, 19, 20, 25, 26, 33, 34, 27, 28, 35, 36, // luma
    14, 15, 22, 23, // Cb
    38, 39, 46, 47, // Cr
];
const CB_RES_MAXPOS: [i32; 11] = [0, 15, 14, 15, 3, 14, 63, 3, 3, 14, 14];
const CB_RES_MAXC2: [i32; 11] = [0, 4, 4, 4, 3, 4, 4, 3, 3, 4, 4];
const CB_RES_CBF: [usize; 11] = [0, 0, 4, 8, 12, 16, 0, 12, 12, 16, 16];
const CB_RES_MAP: [usize; 11] = [0, 0, 15, 29, 44, 47, 0, 44, 44, 47, 47];
const CB_RES_ONE: [usize; 11] = [0, 0, 10, 20, 30, 39, 0, 30, 30, 39, 39];
const CB_RP_I16_DC: usize = 1;
const CB_RP_I16_AC: usize = 2;
const CB_RP_LUMA_4X4: usize = 3;
const CB_RP_CHROMA_DC: usize = 7;
const CB_RP_CHROMA_AC: usize = 9;

/// Inverse of `cabac_unary(ctx, off)`: bin0 at `ctx`; for value >= 1, `value-1` ones
/// then a terminating 0, all at `ctx+off`.
fn cb_unary(cab: &mut CabacEncoder, ctx: usize, off: usize, value: u32) {
    if value == 0 {
        cab.encode_decision(ctx, 0);
        return;
    }
    cab.encode_decision(ctx, 1);
    for _ in 0..value - 1 {
        cab.encode_decision(ctx + off, 1);
    }
    cab.encode_decision(ctx + off, 0);
}

/// Exp-Golomb order-`k` in bypass — inverse of `cabac_exp_bypass(k)`.
fn cb_exp_bypass(cab: &mut CabacEncoder, mut k: i32, mut n: u32) {
    while n >= (1 << k) {
        cab.encode_bypass(1);
        n -= 1 << k;
        k += 1;
    }
    cab.encode_bypass(0);
    while k > 0 {
        k -= 1;
        cab.encode_bypass((n >> k) & 1);
    }
}

/// UEG0 coeff-level suffix — inverse of `cabac_ueg_level(ctx)` (TU prefix <=13 at
/// `ctx`, then an EG0 bypass suffix).
fn cb_ueg_level(cab: &mut CabacEncoder, ctx: usize, value: u32) {
    if value == 0 {
        cab.encode_decision(ctx, 0);
        return;
    }
    let ones = value.min(13);
    for _ in 0..ones {
        cab.encode_decision(ctx, 1);
    }
    if value < 13 {
        cab.encode_decision(ctx, 0);
    } else {
        cb_exp_bypass(cab, 0, value - 13);
    }
}

/// `mb_qp_delta` — inverse of `parse_mb_qp_delta_cabac` (ctxIdxOffset 60).
fn cb_mb_qp_delta(cab: &mut CabacEncoder, last_delta_qp: &mut i32, delta: i32) {
    const O: usize = 60;
    let ctx_inc = (*last_delta_qp != 0) as usize;
    if delta == 0 {
        cab.encode_decision(O + ctx_inc, 0);
    } else {
        cab.encode_decision(O + ctx_inc, 1);
        // code = 2|d| - (d>0); the decode's cabac_unary sees code-1.
        let code = 2 * delta.unsigned_abs() - (delta > 0) as u32;
        cb_unary(cab, O + 2, 1, code - 1);
    }
    *last_delta_qp = delta;
}

/// `intra_chroma_pred_mode` (TU cMax=3) — inverse of `parse_intra_chroma_pred_mode_cabac`.
fn cb_chroma_pred_mode(cab: &mut CabacEncoder, ctx_inc: usize, mode: u8) {
    const C: usize = 64;
    if mode == 0 {
        cab.encode_decision(C + ctx_inc, 0);
        return;
    }
    cab.encode_decision(C + ctx_inc, 1);
    if mode == 1 {
        cab.encode_decision(C + 3, 0);
    } else if mode == 2 {
        cab.encode_decision(C + 3, 1);
        cab.encode_decision(C + 3, 0);
    } else {
        cab.encode_decision(C + 3, 1);
        cab.encode_decision(C + 3, 1);
    }
}

/// I-slice `mb_type` — inverse of `parse_mb_type_i_cabac` (ctxIdxOffset 3).
fn cb_mb_type_i(
    cab: &mut CabacEncoder,
    ctx_inc: usize,
    use_i4: bool,
    i16_mode: u32,
    cbp_chroma: u32,
    cbp_luma15: bool,
) {
    const O: usize = 3;
    if use_i4 {
        cab.encode_decision(O + ctx_inc, 0); // I_NxN
        return;
    }
    cab.encode_decision(O + ctx_inc, 1);
    cab.encode_terminate(false); // not I_PCM
    cab.encode_decision(O + 3, cbp_luma15 as u32);
    if cbp_chroma != 0 {
        cab.encode_decision(O + 4, 1);
        cab.encode_decision(O + 5, (cbp_chroma == 2) as u32);
    } else {
        cab.encode_decision(O + 4, 0);
    }
    cab.encode_decision(O + 6, (i16_mode >> 1) & 1);
    cab.encode_decision(O + 7, i16_mode & 1);
}

/// One `Intra_4x4` pred-mode — inverse of `parse_intra4x4_pred_mode_cabac` (ctx 68).
fn cb_intra4x4_pred_mode(cab: &mut CabacEncoder, predicted: u8, actual: u8) {
    const IPR: usize = 68;
    if actual == predicted {
        cab.encode_decision(IPR, 1);
    } else {
        cab.encode_decision(IPR, 0);
        let rem = if actual < predicted { actual } else { actual - 1 } as u32;
        cab.encode_decision(IPR + 1, rem & 1);
        cab.encode_decision(IPR + 1, (rem >> 1) & 1);
        cab.encode_decision(IPR + 1, (rem >> 2) & 1);
    }
}

/// `coded_block_pattern` — inverse of `parse_cbp_cabac` (ctxIdxOffset 73).
fn cb_cbp(cab: &mut CabacEncoder, top: Option<u8>, left: Option<u8>, cbp: u32) {
    const CBP: usize = 73;
    let t = |m: u32| top.map_or(0u32, |c| ((c as u32 & m) == 0) as u32);
    let l = |m: u32| left.map_or(0u32, |c| ((c as u32 & m) == 0) as u32);
    let nb = |x: u32| (x == 0) as u32;
    let b0 = cbp & 1;
    let b1 = (cbp >> 1) & 1;
    let b2 = (cbp >> 2) & 1;
    let b3 = (cbp >> 3) & 1;
    cab.encode_decision(CBP + (l(1 << 1) + (t(1 << 2) << 1)) as usize, b0);
    cab.encode_decision(CBP + (nb(b0) + (t(1 << 3) << 1)) as usize, b1);
    cab.encode_decision(CBP + (l(1 << 3) + (nb(b0) << 1)) as usize, b2);
    cab.encode_decision(CBP + (nb(b2) + (nb(b1) << 1)) as usize, b3);
    let cbp_chroma = cbp >> 4;
    let ct = top.map_or(0u32, |c| ((c >> 4) != 0) as u32);
    let cl = left.map_or(0u32, |c| ((c >> 4) != 0) as u32);
    cab.encode_decision(CBP + 4 + (cl + (ct << 1)) as usize, (cbp_chroma != 0) as u32);
    if cbp_chroma != 0 {
        let ct2 = top.map_or(0u32, |c| ((c >> 4) == 2) as u32);
        let cl2 = left.map_or(0u32, |c| ((c >> 4) == 2) as u32);
        cab.encode_decision(CBP + 8 + (cl2 + (ct2 << 1)) as usize, (cbp_chroma == 2) as u32);
    }
}

/// One residual block — inverse of `parse_residual_cabac`. `coeffs` is scan-order
/// (len >= maxPos+1). Returns totalCoeffNum (for the nzc cache + deblock nnz).
#[allow(clippy::too_many_arguments)]
fn cb_residual(
    cab: &mut CabacEncoder,
    nzc: &mut [u8; 48],
    cbf_dc: &mut u16,
    iz: usize,
    rp: usize,
    is_intra: bool,
    ndc: (Option<u16>, Option<u16>),
    coeffs: &[i32],
) -> u32 {
    let is_dc = rp == CB_RP_I16_DC || rp == CB_RP_CHROMA_DC || rp == CB_RP_CHROMA_DC + 1;
    let (mut na, mut nb) = (is_intra as u8, is_intra as u8);
    let scan = CB_NZC_CACHE[iz.min(23)];
    if is_dc {
        if let Some(t) = ndc.0 {
            nb = ((t >> rp) & 1) as u8;
        }
        if let Some(l) = ndc.1 {
            na = ((l >> rp) & 1) as u8;
        }
    } else {
        if nzc[scan - 8] != 0xff {
            nb = (nzc[scan - 8] != 0) as u8;
        }
        if nzc[scan - 1] != 0xff {
            na = (nzc[scan - 1] != 0) as u8;
        }
    }
    let maxpos = CB_RES_MAXPOS[rp] as usize;
    let coeff_num = coeffs[..=maxpos].iter().filter(|&&c| c != 0).count() as u32;
    let cbf = coeff_num != 0;
    cab.encode_decision(85 + CB_RES_CBF[rp] + (na + (nb << 1)) as usize, cbf as u32);
    if !cbf {
        if !is_dc {
            nzc[scan] = 0;
        }
        return 0;
    }
    if is_dc {
        *cbf_dc |= 1 << rp;
    }
    // significance map
    let map = 105 + CB_RES_MAP[rp];
    let last = 166 + CB_RES_MAP[rp];
    let lastnz = (0..=maxpos).rev().find(|&i| coeffs[i] != 0).unwrap();
    for i in 0..maxpos {
        let s = coeffs[i] != 0;
        cab.encode_decision(map + i, s as u32);
        if s {
            let is_last = i == lastnz;
            cab.encode_decision(last + i, is_last as u32);
            if is_last {
                break;
            }
        }
    }
    // levels (reverse scan)
    let one = 227 + CB_RES_ONE[rp];
    let abs = 232 + CB_RES_ONE[rp];
    let maxc2 = CB_RES_MAXC2[rp];
    let (mut c1, mut c2) = (1i32, 0i32);
    for i in (0..=maxpos).rev() {
        if coeffs[i] != 0 {
            let av = coeffs[i].unsigned_abs();
            let gt1 = av > 1;
            cab.encode_decision(one + c1 as usize, gt1 as u32);
            if gt1 {
                cb_ueg_level(cab, abs + c2 as usize, av - 2);
                c2 = (c2 + 1).min(maxc2);
                c1 = 0;
            } else if c1 != 0 {
                c1 = (c1 + 1).min(4);
            }
            cab.encode_bypass((coeffs[i] < 0) as u32);
        }
    }
    if !is_dc {
        nzc[scan] = coeff_num as u8;
    }
    coeff_num
}

/// Build the 48-entry padded nzc cache from the top/left neighbour MB exports
/// (openh264 `WelsFillCacheNonZeroCount`) — identical to the decoder.
fn cb_build_nzc(mb_nzc: &[[u8; 24]], top: Option<usize>, left: Option<usize>) -> [u8; 48] {
    let mut nzc = [0xffu8; 48];
    if let Some(t) = top {
        let tn = mb_nzc[t];
        nzc[1..5].copy_from_slice(&tn[12..16]);
        (nzc[0], nzc[5], nzc[29]) = (0, 0, 0);
        (nzc[6], nzc[7]) = (tn[20], tn[21]);
        (nzc[30], nzc[31]) = (tn[22], tn[23]);
    }
    if let Some(l) = left {
        let ln = mb_nzc[l];
        (nzc[8], nzc[16], nzc[24], nzc[32]) = (ln[3], ln[7], ln[11], ln[15]);
        (nzc[13], nzc[21], nzc[37], nzc[45]) = (ln[17], ln[21], ln[19], ln[23]);
    }
    nzc
}

/// Extract the 24-entry per-MB nzc (raster luma + chroma) for future neighbours.
fn cb_export_nzc(nzc: &[u8; 48]) -> [u8; 24] {
    let mut mn = [0u8; 24];
    for k in 0..4 {
        mn[k] = nzc[9 + k];
        mn[4 + k] = nzc[17 + k];
        mn[8 + k] = nzc[25 + k];
        mn[12 + k] = nzc[33 + k];
    }
    (mn[16], mn[17], mn[20], mn[21]) = (nzc[14], nzc[15], nzc[22], nzc[23]);
    (mn[18], mn[19], mn[22], mn[23]) = (nzc[38], nzc[39], nzc[46], nzc[47]);
    for v in mn.iter_mut() {
        if *v == 0xff {
            *v = 0;
        }
    }
    mn
}

/// Per-frame CABAC neighbour state (I-slice): one entry per macroblock, mirroring
/// the arrays the decoder's `decode_slice_data_cabac` maintains.
struct CabacState {
    cat: Vec<u8>,          // 2 = I_16x16, 0 = I_NxN, 100 = inter (mb_type / skip ctxInc)
    cmode: Vec<i32>,       // per-MB chroma mode (chroma-pred ctxInc)
    mb_cbp: Vec<u8>,       // per-MB cbp byte (cbp ctxInc)
    cbf_dc: Vec<u16>,      // per-MB DC coded_block_flag mask (residual DC ctxInc)
    mb_nzc: Vec<[u8; 24]>, // per-MB nzc export (residual AC ctxInc)
    // Inter (P/B) neighbour state — mirrors the decoder's WelsFillCacheInterCabac.
    mb_mvd: Vec<[[i16; 2]; 16]>,  // per-MB per-4x4 List-0 mvd (raster), for the mvd ctxInc cache
    mb_ref: Vec<[i8; 16]>,        // per-MB per-4x4 List-0 ref idx (raster); -1 = unavailable
    mb_mvd1: Vec<[[i16; 2]; 16]>, // B: per-MB per-4x4 List-1 mvd
    mb_ref1: Vec<[i8; 16]>,       // B: per-MB per-4x4 List-1 ref idx
    mb_skip: Vec<bool>,           // per-MB mb_skip_flag (skip ctxInc)
    mb_direct: Vec<bool>,         // B: per-MB B_Direct/B_Skip (B mb_type ctxInc)
    last_delta_qp: i32,
}

impl CabacState {
    fn new(n: usize) -> Self {
        CabacState {
            cat: vec![0; n],
            cmode: vec![0; n],
            mb_cbp: vec![0; n],
            cbf_dc: vec![0; n],
            mb_nzc: vec![[0u8; 24]; n],
            mb_mvd: vec![[[0i16; 2]; 16]; n],
            mb_ref: vec![[-1i8; 16]; n],
            mb_mvd1: vec![[[0i16; 2]; 16]; n],
            mb_ref1: vec![[-1i8; 16]; n],
            mb_skip: vec![false; n],
            mb_direct: vec![false; n],
            last_delta_qp: 0,
        }
    }
}

/// Emit one planned intra macroblock as CABAC (I-slice). Mirrors the decoder's
/// I-slice MB body exactly: `mb_type`, then per luma-type the intra modes / cbp /
/// `mb_qp_delta` / residual in spec order, maintaining `cs` and `fe.nnz_y`.
fn emit_mb_cabac_i(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    plan: &MbPlan,
    mb_x: usize,
    mb_y: usize,
) {
    let mb_w = fe.mb_w;
    let addr = mb_y * mb_w + mb_x;
    let top = if mb_y > 0 { Some(addr - mb_w) } else { None };
    let left = if mb_x > 0 { Some(addr - 1) } else { None };

    // ---- mb_type (I-slice prefix; carries I_16x16 pred-mode/cbp) ----
    let li = left.map_or(0, |a| (cs.cat[a] >= 2) as usize);
    let ti = top.map_or(0, |a| (cs.cat[a] >= 2) as usize);
    if plan.use_i4 {
        cb_mb_type_i(cab, li + ti, true, 0, 0, false);
    } else {
        cb_mb_type_i(cab, li + ti, false, plan.i16_mode as u32, plan.cbp_chroma, plan.i16_cbp15);
    }
    emit_intra_body_cabac(fe, cab, cs, plan, mb_x, mb_y, addr, top, left);
}

/// The intra macroblock body (chroma pred mode, intra modes, cbp, mb_qp_delta,
/// residual) shared by I-slice intra and P/B-slice intra — everything AFTER the
/// slice-specific `mb_type` prefix (which already carries the I_16x16 pred-mode/cbp).
#[allow(clippy::too_many_arguments)]
fn emit_intra_body_cabac(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    plan: &MbPlan,
    mb_x: usize,
    mb_y: usize,
    addr: usize,
    top: Option<usize>,
    left: Option<usize>,
) {
    let w4 = fe.mb_w * 4;
    let cbp_chroma = plan.cbp_chroma;
    // chroma-pred-mode ctxInc from neighbour chroma modes (1..=3).
    let cci = left.map_or(0, |a| (1..=3).contains(&cs.cmode[a]) as usize)
        + top.map_or(0, |a| (1..=3).contains(&cs.cmode[a]) as usize);

    let mut nzc;
    let mut cbfdc = 0u16;
    let ndc = (top.map(|a| cs.cbf_dc[a]), left.map(|a| cs.cbf_dc[a]));

    if !plan.use_i4 {
        // ---- I_16x16 ----
        cb_chroma_pred_mode(cab, cci, plan.chroma_mode);
        cs.cmode[addr] = plan.chroma_mode as i32;
        cs.cat[addr] = 2;
        cs.mb_cbp[addr] = ((cbp_chroma as u8) << 4) | if plan.i16_cbp15 { 15 } else { 0 };
        nzc = cb_build_nzc(&cs.mb_nzc, top, left);

        let delta = fe.qp_delta();
        cb_mb_qp_delta(cab, &mut cs.last_delta_qp, delta);

        // luma DC
        let dc_scan = scan_4x4_dcac(&plan.i16_dc_levels);
        cb_residual(cab, &mut nzc, &mut cbfdc, 0, CB_RP_I16_DC, true, ndc, &dc_scan);
        // luma AC
        for (iz, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let total = if plan.i16_cbp15 {
                let ac = scan_4x4_ac(&plan.i16_q[lby * 4 + lbx]);
                cb_residual(cab, &mut nzc, &mut cbfdc, iz, CB_RP_I16_AC, true, ndc, &ac)
            } else {
                nzc[CB_NZC_CACHE[iz]] = 0;
                0
            };
            fe.nnz_y[by * w4 + bx] = total as u8;
        }
        cb_emit_chroma_residual(cab, fe, &mut nzc, &mut cbfdc, ndc, true, plan.cbp_chroma, &plan.c_dc_levels, &plan.c_q_blocks, mb_x, mb_y);
    } else {
        // ---- I_NxN (I_4x4) ----
        let i4 = plan.i4.as_ref().unwrap();
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
            let predicted = predict_i4_mode(fe, bx, by);
            cb_intra4x4_pred_mode(cab, predicted, i4.modes[lby * 4 + lbx]);
        }
        cb_chroma_pred_mode(cab, cci, plan.chroma_mode);
        cs.cmode[addr] = plan.chroma_mode as i32;
        cs.cat[addr] = 0;
        let cbp = i4.cbp_luma | (cbp_chroma << 4);
        cb_cbp(cab, top.map(|a| cs.mb_cbp[a]), left.map(|a| cs.mb_cbp[a]), cbp);
        cs.mb_cbp[addr] = cbp as u8;
        nzc = cb_build_nzc(&cs.mb_nzc, top, left);

        if cbp == 0 {
            cs.last_delta_qp = 0;
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                fe.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
            }
        } else {
            let delta = fe.qp_delta();
            cb_mb_qp_delta(cab, &mut cs.last_delta_qp, delta);
            for id8 in 0..4usize {
                for id4 in 0..4usize {
                    let iz = id8 * 4 + id4;
                    let (lbx, lby) = LUMA_4X4_SCAN_XY[iz];
                    let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                    let total = if i4.cbp_luma & (1 << id8) != 0 {
                        let sc = scan_4x4_dcac(&i4.q[lby * 4 + lbx]);
                        cb_residual(cab, &mut nzc, &mut cbfdc, iz, CB_RP_LUMA_4X4, true, ndc, &sc)
                    } else {
                        nzc[CB_NZC_CACHE[iz]] = 0;
                        0
                    };
                    fe.nnz_y[by * w4 + bx] = total as u8;
                }
            }
            cb_emit_chroma_residual(cab, fe, &mut nzc, &mut cbfdc, ndc, true, plan.cbp_chroma, &plan.c_dc_levels, &plan.c_q_blocks, mb_x, mb_y);
        }
    }

    cs.cbf_dc[addr] = cbfdc;
    cs.mb_nzc[addr] = cb_export_nzc(&nzc);
}

/// Chroma DC + AC residual (shared by intra I_16x16/I_NxN and inter) — matches the
/// decoder's chroma residual order. `is_intra` selects the coded_block_flag default
/// (nA=nB default to is_intra). Populates the chroma nnz grid for deblock.
#[allow(clippy::too_many_arguments)]
fn cb_emit_chroma_residual(
    cab: &mut CabacEncoder,
    fe: &mut FrameEncoder,
    nzc: &mut [u8; 48],
    cbfdc: &mut u16,
    ndc: (Option<u16>, Option<u16>),
    is_intra: bool,
    cbp_chroma: u32,
    c_dc_levels: &[[i32; 4]; 2],
    c_q: &[[[i32; 16]; 4]; 2],
    mb_x: usize,
    mb_y: usize,
) {
    let w2 = fe.mb_w * 2;
    if cbp_chroma >= 1 {
        for i in 0..2usize {
            cb_residual(cab, nzc, cbfdc, 16 + i * 4, CB_RP_CHROMA_DC + i, is_intra, ndc, &c_dc_levels[i]);
        }
    }
    if cbp_chroma == 2 {
        for i in 0..2usize {
            for (id4, &(bx, by)) in CHROMA_4X4_SCAN_XY.iter().enumerate() {
                let ac = scan_4x4_ac(&c_q[i][by * 2 + bx]);
                let total = cb_residual(
                    cab, nzc, cbfdc, 16 + i * 4 + id4, CB_RP_CHROMA_AC + i, is_intra, ndc, &ac,
                );
                fe.nnz_c[i][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] = total as u8;
            }
        }
    }
}

/// CABAC all-intra slice-data coder (IDR / I-slice). Mirrors `encode_slice_data`'s
/// setup + deblock + `RefFrame` construction, but codes every MB via `plan_mb` +
/// `emit_mb_cabac_i` into a CABAC bitstream. `w` already holds the byte-aligned
/// slice header; the CABAC bytes are appended after `cabac_alignment_one_bit`.
pub fn encode_slice_data_cabac_intra(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    frame: &YuvFrame,
    qp: u8,
) -> crate::RefFrame {
    let mut fe = FrameEncoder::new(cfg);
    fe.qp = qp;
    fe.qpc = chroma_qp(qp);
    fe.cur_qp = qp;
    if cfg.cabac_dz_div > 0 {
        fe.idz = cfg.cabac_dz_div; // CABAC-specific dead-zone override
    }
    let (sy, su, sv) = coded_source(cfg, frame);
    let aq_qp = aq_qp_map(&sy, fe.cw, fe.mb_w, fe.mb_h, qp, fe.aq_strength);
    fe.cur_qp = qp;
    let mut mb_qpy = vec![qp; fe.mb_w * fe.mb_h];

    // CABAC trellis (RDOQ): structure-adaptive. ON only for ALL-INTRA streams
    // (gop_size<=1), where each IDR is independent so trading a little distortion for
    // rate is a clean −0.5..−1.3% BD-rate win. OFF inside a GOP: there the I-frame is
    // a REFERENCE, and degrading it costs the dependent P-frames more than the I-frame
    // saves (measured ~+0.1% net) — so the safe end is a true no-op (never regresses).
    fe.rdoq_strength = if cfg.gop_size <= 1 { cfg.cabac_rdoq } else { 0.0 };
    // Contexts init from SliceQPY (the slice qp), init_idc unused for I, is_i = true.
    let mut cab = CabacEncoder::new(qp as i32, 0, true);
    let mut cs = CabacState::new(fe.mb_w * fe.mb_h);
    let total = fe.mb_w * fe.mb_h;

    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            let mb_idx = mb_y * fe.mb_w + mb_x;
            fe.qp = aq_qp[mb_idx];
            fe.qpc = chroma_qp(aq_qp[mb_idx]);
            let plan = plan_mb(&mut fe, mb_x, mb_y, &sy, &su, &sv);
            emit_mb_cabac_i(&mut fe, &mut cab, &mut cs, &plan, mb_x, mb_y);
            mb_qpy[mb_idx] = fe.cur_qp;
            // end_of_slice_flag (EncodeTerminate): 1 on the last MB, else 0.
            cab.encode_terminate(mb_idx + 1 == total);
        }
    }

    // Append CABAC slice data after cabac_alignment_one_bit (pad header with 1-bits).
    while !w.is_byte_aligned() {
        w.write_bit(true);
    }
    for b in cab.into_bytes() {
        w.write_bits(b as u32, 8);
    }

    // Deblock the reconstruction (all-intra: BS derives from intra-ness) -> reference.
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
    rusty_h264_common::deblock::filter_frame(
        &mut fe.rec_y, &mut fe.rec_u, &mut fe.rec_v, fe.mb_w, fe.mb_h, &mb_qpy, 0, 0, 0, &info,
    );
    let w4 = fe.mb_w * 4;
    crate::RefFrame {
        y: fe.rec_y,
        u: fe.rec_u,
        v: fe.rec_v,
        poc: 0,
        frame_num: 0,
        mv: fe.mv_y,
        ref_idx: fe.ref_idx_y,
        w4,
    }
}

// ============================================================================
// CABAC P-slice entropy coding — the forward inverse of the decoder's
// decode_slice_data_cabac P-slice path. mb_skip_flag / mb_type_p / mvd (UEG3) /
// inter residual, plus intra-in-P (the shared intra body under a P mb_type prefix).
// Scope: 1 reference (no ref_idx), P_16x16/16x8/8x16 (no P_8x8/sub_mb_type) — the
// modes the encoder's decision produces.
// ============================================================================

// z-order 4x4 block -> 30-entry (6-stride) mvd/ref cache index (openh264 g_kCache30ScanIdx).
const CB_CACHE30: [usize; 16] = [7, 8, 13, 14, 9, 10, 15, 16, 19, 20, 25, 26, 21, 22, 27, 28];
// z-order 4x4 block -> raster index (openh264 g_kuiScan4): the per-MB mvd/ref grid layout.
const CB_G_SCAN4: [usize; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

/// UEG3 mvd suffix — inverse of `decode_ueg_mv(base)` (TU prefix at base+{0,1,2,3,3..},
/// cMax 7, then EG3 bypass). `v` is the value decode_ueg_mv returns.
fn cb_ueg_mv(cab: &mut CabacEncoder, base: usize, v: u32) {
    const P2C: [usize; 8] = [0, 1, 2, 3, 3, 3, 3, 3];
    if v == 0 {
        cab.encode_decision(base, 0);
        return;
    }
    cab.encode_decision(base, 1);
    if v <= 7 {
        // (v-1) ones then a terminating 0, at base+P2C[count] for count = 1..
        let mut count = 1;
        for _ in 0..v - 1 {
            cab.encode_decision(base + P2C[count], 1);
            count += 1;
        }
        cab.encode_decision(base + P2C[count], 0);
    } else {
        // prefix maxes out: 7 ones (count 1..7) then EG3(v-8).
        let mut count = 1;
        for _ in 0..7 {
            cab.encode_decision(base + P2C[count], 1);
            count += 1;
        }
        cb_exp_bypass(cab, 3, v - 8);
    }
}

/// One `mvd` component — inverse of `parse_mvd_cabac(comp, ctx_inc)` (ctxIdxOffset
/// 40 for x, 47 for y).
fn cb_mvd(cab: &mut CabacEncoder, comp: usize, ctx_inc: usize, d: i32) {
    let base = 40 + comp * 7;
    if d == 0 {
        cab.encode_decision(base + ctx_inc, 0);
        return;
    }
    cab.encode_decision(base + ctx_inc, 1);
    cb_ueg_mv(cab, base + 3, d.unsigned_abs() - 1); // decode adds 1 back
    cab.encode_bypass((d < 0) as u32);
}

/// `mb_skip_flag` — inverse of `parse_mb_skip_cabac` (ctx 11 P + neighbour-not-skip).
fn cb_mb_skip(cab: &mut CabacEncoder, ctx_inc: usize, skip: bool) {
    cab.encode_decision(ctx_inc, skip as u32);
}

/// P-slice inter `mb_type` (0/1/2 = P_L0_16x16 / P_16x8 / P_8x16) — inverse of the
/// inter branch of `parse_mb_type_p_cabac` (ctx base 11).
fn cb_mb_type_p_inter(cab: &mut CabacEncoder, mode: u8) {
    const S: usize = 11;
    cab.encode_decision(S + 3, 0); // inter (prefix bit 0)
    match mode {
        0 => {
            cab.encode_decision(S + 4, 0);
            cab.encode_decision(S + 5, 0);
        }
        1 => {
            cab.encode_decision(S + 4, 1);
            cab.encode_decision(S + 6, 1);
        }
        _ => {
            // mode == 2 (P_8x16)
            cab.encode_decision(S + 4, 1);
            cab.encode_decision(S + 6, 0);
        }
    }
}

/// P-slice intra `mb_type` prefix — inverse of the intra branch of
/// `parse_mb_type_p_cabac` (ctx base 11). Carries the I_16x16 pred-mode/cbp exactly
/// like the I-slice mb_type, so the shared intra body re-emits neither.
fn cb_mb_type_p_intra(cab: &mut CabacEncoder, plan: &MbPlan) {
    const S: usize = 11;
    cab.encode_decision(S + 3, 1); // intra (prefix bit 1)
    if plan.use_i4 {
        cab.encode_decision(S + 6, 0); // I_4x4
        return;
    }
    cab.encode_decision(S + 6, 1); // I_16x16
    cab.encode_terminate(false); // not I_PCM
    cab.encode_decision(S + 7, plan.i16_cbp15 as u32);
    if plan.cbp_chroma != 0 {
        cab.encode_decision(S + 8, 1);
        cab.encode_decision(S + 8, (plan.cbp_chroma == 2) as u32);
    } else {
        cab.encode_decision(S + 8, 0);
    }
    cab.encode_decision(S + 9, (plan.i16_mode as u32 >> 1) & 1);
    cab.encode_decision(S + 9, plan.i16_mode as u32 & 1);
}

/// P-slice partition layout: `(part_idx, z-blocks)` per motion partition (matches
/// the decoder's `part!` invocations). part_idx = the partition's top-left z-block
/// (its `CACHE30` slot for the mvd ctxInc); z-blocks = every 4x4 it covers.
fn p_partition_layout(mode: u8) -> &'static [(usize, &'static [usize])] {
    match mode {
        1 => &[(0, &[0, 1, 2, 3, 4, 5, 6, 7]), (8, &[8, 9, 10, 11, 12, 13, 14, 15])],
        2 => &[(0, &[0, 1, 2, 3, 8, 9, 10, 11]), (4, &[4, 5, 6, 7, 12, 13, 14, 15])],
        _ => &[(0, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])],
    }
}

/// Emit one motion partition's `mvd` (x,y) and splat it into the 30-entry cache +
/// per-MB raster mvd/ref grids — inverse of the decoder's `parse_mvd_partition`.
#[allow(clippy::too_many_arguments)]
fn cb_emit_mvd_partition(
    cab: &mut CabacEncoder,
    part_idx: usize,
    zblocks: &[usize],
    mvdc: &mut [[i16; 2]; 30],
    refc: &mut [i8; 30],
    mmvd: &mut [[i16; 2]; 16],
    mref: &mut [i8; 16],
    mvd: (i32, i32),
) {
    let s = CB_CACHE30[part_idx];
    let ctx = |comp: usize| -> usize {
        let mut a = 0i32;
        if refc[s - 6] >= 0 {
            a += mvdc[s - 6][comp].unsigned_abs() as i32;
        }
        if refc[s - 1] >= 0 {
            a += mvdc[s - 1][comp].unsigned_abs() as i32;
        }
        if a >= 3 {
            1 + (a > 32) as usize
        } else {
            0
        }
    };
    cb_mvd(cab, 0, ctx(0), mvd.0);
    cb_mvd(cab, 1, ctx(1), mvd.1);
    let (mx, my) = (mvd.0 as i16, mvd.1 as i16);
    for &zb in zblocks {
        mvdc[CB_CACHE30[zb]] = [mx, my];
        refc[CB_CACHE30[zb]] = 0;
        mmvd[CB_G_SCAN4[zb]] = [mx, my];
        mref[CB_G_SCAN4[zb]] = 0;
    }
}

/// Emit one planned INTER macroblock as CABAC (P-slice, mb_skip_flag already coded
/// as 0). `mode`/`parts` + `plan` from `plan_inter_mb`. 1-ref: no ref_idx.
fn emit_mb_cabac_p_inter(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    mode: u8,
    plan: &InterPlan,
    mb_x: usize,
    mb_y: usize,
) {
    let mb_w = fe.mb_w;
    let addr = mb_y * mb_w + mb_x;
    let top = if mb_y > 0 { Some(addr - mb_w) } else { None };
    let left = if mb_x > 0 { Some(addr - 1) } else { None };

    cb_mb_type_p_inter(cab, mode);

    // ---- mvd (build the 30-entry List-0 mvd/ref neighbour cache, then per partition) ----
    let mut mvdc = [[0i16; 2]; 30];
    let mut refc = [-1i8; 30];
    cb_fill_inter_cache(&cs.mb_ref, &cs.mb_mvd, &mut refc, &mut mvdc, top, left, addr, mb_w);
    let mut mmvd = [[0i16; 2]; 16];
    let mut mref = [0i8; 16];
    for (part, &(part_idx, zblocks)) in p_partition_layout(mode).iter().enumerate() {
        cb_emit_mvd_partition(cab, part_idx, zblocks, &mut mvdc, &mut refc, &mut mmvd, &mut mref, plan.mvds[part]);
    }
    cs.mb_mvd[addr] = mmvd;
    cs.mb_ref[addr] = mref;
    cs.cat[addr] = 100;
    cb_emit_inter_residual(fe, cab, cs, plan, mb_x, mb_y, addr, top, left);
}

/// Inter cbp + residual (is_intra = false) — shared by P and B inter MBs. Maintains
/// cs.mb_cbp/cbf_dc/mb_nzc/last_delta_qp + fe.nnz_y.
#[allow(clippy::too_many_arguments)]
fn cb_emit_inter_residual(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    plan: &InterPlan,
    mb_x: usize,
    mb_y: usize,
    addr: usize,
    top: Option<usize>,
    left: Option<usize>,
) {
    let w4 = fe.mb_w * 4;
    let cbp = plan.cbp;
    let (cbp_luma, cbp_chroma) = (cbp & 15, cbp >> 4);
    cb_cbp(cab, top.map(|a| cs.mb_cbp[a]), left.map(|a| cs.mb_cbp[a]), cbp);
    cs.mb_cbp[addr] = cbp as u8;
    let mut nzc = cb_build_nzc(&cs.mb_nzc, top, left);
    let mut cbfdc = 0u16;
    let ndc = (top.map(|a| cs.cbf_dc[a]), left.map(|a| cs.cbf_dc[a]));

    if cbp == 0 {
        cs.last_delta_qp = 0;
        for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
            fe.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
        }
    } else {
        let delta = fe.qp_delta();
        cb_mb_qp_delta(cab, &mut cs.last_delta_qp, delta);
        for id8 in 0..4usize {
            for id4 in 0..4usize {
                let iz = id8 * 4 + id4;
                let (lbx, lby) = LUMA_4X4_SCAN_XY[iz];
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let total = if cbp_luma & (1 << id8) != 0 {
                    let sc = scan_4x4_dcac(&plan.q_blocks[lby * 4 + lbx]);
                    cb_residual(cab, &mut nzc, &mut cbfdc, iz, CB_RP_LUMA_4X4, false, ndc, &sc)
                } else {
                    nzc[CB_NZC_CACHE[iz]] = 0;
                    0
                };
                fe.nnz_y[by * w4 + bx] = total as u8;
            }
        }
        cb_emit_chroma_residual(cab, fe, &mut nzc, &mut cbfdc, ndc, false, cbp_chroma, &plan.c_dc_levels, &plan.c_q, mb_x, mb_y);
    }
    cs.cbf_dc[addr] = cbfdc;
    cs.mb_nzc[addr] = cb_export_nzc(&nzc);
}

/// Emit one planned INTRA macroblock inside a P-slice: the P mb_type prefix (which
/// carries the I_16x16 pred-mode/cbp) then the shared intra body.
fn emit_mb_cabac_p_intra(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    plan: &MbPlan,
    mb_x: usize,
    mb_y: usize,
) {
    let mb_w = fe.mb_w;
    let addr = mb_y * mb_w + mb_x;
    let top = if mb_y > 0 { Some(addr - mb_w) } else { None };
    let left = if mb_x > 0 { Some(addr - 1) } else { None };
    cb_mb_type_p_intra(cab, plan);
    emit_intra_body_cabac(fe, cab, cs, plan, mb_x, mb_y, addr, top, left);
}

/// Emit a P_Skip macroblock's `mb_skip_flag = 1` and update neighbour state. The
/// motion grid was committed by `commit_skip`; the mvd/ref cache is LEFT at its
/// init (-1 ref) — matching the decoder, which does not touch mb_mvd/mb_ref for a
/// P_Skip (so a skip neighbour contributes nothing to a later mvd ctxInc).
fn emit_p_skip_cabac(cab: &mut CabacEncoder, cs: &mut CabacState, addr: usize, top: Option<usize>, left: Option<usize>) {
    let sctx = 11
        + left.map_or(0, |a| (!cs.mb_skip[a]) as usize)
        + top.map_or(0, |a| (!cs.mb_skip[a]) as usize);
    cb_mb_skip(cab, sctx, true);
    cs.mb_skip[addr] = true;
    cs.cat[addr] = 100;
    cs.last_delta_qp = 0;
}

/// CABAC P-slice data coder. Mirrors `encode_slice_data`'s decision (P_Skip check +
/// fast/quality inter-vs-intra RD) exactly — only the emit differs (per-MB
/// mb_skip_flag + CABAC syntax + per-MB end_of_slice terminate).
pub fn encode_slice_data_cabac_p(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    frame: &YuvFrame,
    qp: u8,
    refs: &[crate::RefFrame],
) -> crate::RefFrame {
    let mut fe = FrameEncoder::new(cfg);
    fe.qp = qp;
    fe.qpc = chroma_qp(qp);
    fe.cur_qp = qp;
    if cfg.cabac_dz_div > 0 {
        fe.idz = cfg.cabac_dz_div; // CABAC-specific dead-zone override
    }
    let (sy, su, sv) = coded_source(cfg, frame);
    let lambda = 0.85 * fe.tune_lambda_scale * 2f64.powf((qp as f64 - 12.0) / 3.0);
    let num_refs = refs.len();
    if fe.satd_q > 0.0 {
        let mut vars: Vec<i64> = (0..fe.mb_h)
            .flat_map(|my| (0..fe.mb_w).map(move |mx| (mx, my)))
            .map(|(mx, my)| mb_variance(&sy, fe.cw, mx, my))
            .collect();
        vars.sort_unstable();
        let idx = (((1.0 - fe.satd_q) * vars.len() as f64) as usize).min(vars.len() - 1);
        fe.satd_var_thresh = vars[idx];
    }
    let aq_qp = aq_qp_map(&sy, fe.cw, fe.mb_w, fe.mb_h, qp, fe.aq_strength);
    fe.cur_qp = qp;
    let mut mb_qpy = vec![qp; fe.mb_w * fe.mb_h];

    let mut cab = CabacEncoder::new(qp as i32, cfg.cabac_init_idc, false); // P-slice
    let mut cs = CabacState::new(fe.mb_w * fe.mb_h);
    let total = fe.mb_w * fe.mb_h;

    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            let mb_idx = mb_y * fe.mb_w + mb_x;
            let addr = mb_idx;
            let top = if mb_y > 0 { Some(addr - fe.mb_w) } else { None };
            let left = if mb_x > 0 { Some(addr - 1) } else { None };
            fe.qp = aq_qp[mb_idx];
            fe.qpc = chroma_qp(aq_qp[mb_idx]);

            // ---- P_Skip check (identical logic to encode_slice_data) ----
            let mut inter: Option<InterChoice> = None;
            let mut did_skip = false;
            if num_refs > 0 {
                let mv_skip = fe.skip_mv(mb_x, mb_y);
                let skip_y = fe.skip_predict_luma(refs, mb_x, mb_y, mv_skip);
                let luma_free = fe.skip_luma_is_free(&sy, mb_x, mb_y, &skip_y);
                let skip_c = if luma_free || !fe.fast {
                    fe.skip_predict_chroma(refs, mb_x, mb_y, mv_skip)
                } else {
                    [[0u8; 64]; 2]
                };
                let is_free = luma_free && fe.skip_chroma_is_free(&su, &sv, mb_x, mb_y, &skip_c);
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
                    did_skip = true;
                } else {
                    let (lx, ly) = (mb_x * 16, mb_y * 16);
                    let nb = fe.mv_neighbors_block(mb_x as isize * 4, mb_y as isize * 4, 4);
                    let lme = lambda.sqrt() * cfg.cabac_lambda_scale;
                    if fe.fast {
                        fe.mb_use_satd = fe.satd_q > 0.0
                            && mb_variance(&sy, fe.cw, mb_x, mb_y) >= fe.satd_var_thresh;
                        let (r16, mv16, cost_inter) =
                            fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 16, &[], lme);
                        let cost_intra = if fe.mb_use_satd {
                            fe.best_i16_satd(&sy, mb_x, mb_y)
                        } else {
                            fe.best_i16_sad(&sy, mb_x, mb_y)
                        } + (lme * fe.tune_intra_penalty) as i64;
                        inter = if cost_intra < cost_inter {
                            None
                        } else {
                            Some((0, vec![(r16, mv16)]))
                        };
                    } else {
                        // Quality preset: greedy P_Skip, then 16x16 baseline + sub-partitions + intra.
                        if skip_sad < fe.pred_skip_sad(mb_x, mb_y) {
                            fe.commit_skip(mb_x, mb_y, mv_skip, &skip_y, &skip_c);
                            fe.mb_was_skip[mb_idx] = true;
                            fe.mb_skip_sad[mb_idx] = skip_sad;
                            did_skip = true;
                        } else {
                            let (r16, mv16, c16) =
                                fe.best_part(refs, &sy, &nb, num_refs, lx, ly, 16, 16, &[], lme);
                            let mut best_c = c16;
                            let mut pick: Option<InterChoice> = Some((0, vec![(r16, mv16)]));
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
                            let c_intra = fe.best_i16_satd(&sy, mb_x, mb_y)
                                + (lme * fe.tune_intra_penalty) as i64;
                            inter = if c_intra < best_c { None } else { pick };
                            fe.mb_was_skip[mb_idx] = false;
                            fe.mb_skip_sad[mb_idx] = skip_sad;
                        }
                    }
                }
            }

            // ---- emit ----
            if did_skip {
                emit_p_skip_cabac(&mut cab, &mut cs, addr, top, left);
                mb_qpy[mb_idx] = fe.cur_qp;
                cab.encode_terminate(mb_idx + 1 == total);
                continue;
            }
            // mb_skip_flag = 0
            let sctx = 11
                + left.map_or(0, |a| (!cs.mb_skip[a]) as usize)
                + top.map_or(0, |a| (!cs.mb_skip[a]) as usize);
            cb_mb_skip(&mut cab, sctx, false);
            cs.mb_skip[addr] = false;
            match inter {
                Some((mode, parts)) => {
                    let plan = fe.plan_inter_mb(refs, &sy, &su, &sv, mb_x, mb_y, mode, &parts, None);
                    emit_mb_cabac_p_inter(&mut fe, &mut cab, &mut cs, mode, &plan, mb_x, mb_y);
                }
                None => {
                    let plan = plan_mb(&mut fe, mb_x, mb_y, &sy, &su, &sv);
                    emit_mb_cabac_p_intra(&mut fe, &mut cab, &mut cs, &plan, mb_x, mb_y);
                }
            }
            mb_qpy[mb_idx] = fe.cur_qp;
            cab.encode_terminate(mb_idx + 1 == total);
        }
    }

    while !w.is_byte_aligned() {
        w.write_bit(true);
    }
    for b in cab.into_bytes() {
        w.write_bits(b as u32, 8);
    }

    // Deblock -> inter reference (same as encode_slice_data).
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
    rusty_h264_common::deblock::filter_frame(
        &mut fe.rec_y, &mut fe.rec_u, &mut fe.rec_v, fe.mb_w, fe.mb_h, &mb_qpy, 0, 0, 0, &info,
    );
    let w4 = fe.mb_w * 4;
    crate::RefFrame {
        y: fe.rec_y,
        u: fe.rec_u,
        v: fe.rec_v,
        poc: 0,
        frame_num: 0,
        mv: fe.mv_y,
        ref_idx: fe.ref_idx_y,
        w4,
    }
}

// ============================================================================
// CABAC B-slice entropy coding — inverse of the decoder's decode_slice_data_cabac
// B-slice path. Scope: the modes the encoder's B decision produces — B_Skip,
// B_Direct_16x16 (0), B_L0/L1/Bi_16x16 (1/2/3) — no sub_mb_type, no intra-in-B.
// The new piece vs P is the dual-list (L0 + L1) mvd/ref neighbour cache.
// ============================================================================

/// Fill one list's 30-entry mvd/ref neighbour cache from the per-MB export grids
/// (openh264 WelsFillCacheInterCabac). Shared by P (List-0) and B (both lists).
fn cb_fill_inter_cache(
    mb_ref: &[[i8; 16]],
    mb_mvd: &[[[i16; 2]; 16]],
    refc: &mut [i8; 30],
    mvdc: &mut [[i16; 2]; 30],
    top: Option<usize>,
    left: Option<usize>,
    addr: usize,
    mb_w: usize,
) {
    if let Some(l) = left {
        for (ci, bi) in [(6usize, 3usize), (12, 7), (18, 11), (24, 15)] {
            refc[ci] = mb_ref[l][bi];
            mvdc[ci] = mb_mvd[l][bi];
        }
    }
    if let Some(t) = top {
        for (ci, bi) in [(1usize, 12usize), (2, 13), (3, 14), (4, 15)] {
            refc[ci] = mb_ref[t][bi];
            mvdc[ci] = mb_mvd[t][bi];
        }
    }
    let mb_x = addr % mb_w;
    let mb_y = addr / mb_w;
    if mb_x > 0 && mb_y > 0 {
        let a = addr - mb_w - 1;
        (refc[0], mvdc[0]) = (mb_ref[a][15], mb_mvd[a][15]);
    }
    if mb_y > 0 && mb_x + 1 < mb_w {
        let a = addr - mb_w + 1;
        (refc[5], mvdc[5]) = (mb_ref[a][12], mb_mvd[a][12]);
    }
}

/// B-slice `mb_type` for the encoder's B modes (0 = B_Direct_16x16, 1 = B_L0_16x16,
/// 2 = B_L1_16x16, 3 = B_Bi_16x16) — inverse of `parse_mb_type_b_cabac` (ctx 27).
fn cb_mb_type_b(cab: &mut CabacEncoder, ctx_inc: usize, dir: u8) {
    const B: usize = 27;
    match dir {
        0 => cab.encode_decision(B + ctx_inc, 0), // B_Direct_16x16
        1 => {
            cab.encode_decision(B + ctx_inc, 1);
            cab.encode_decision(B + 3, 0);
            cab.encode_decision(B + 5, 0); // L0
        }
        2 => {
            cab.encode_decision(B + ctx_inc, 1);
            cab.encode_decision(B + 3, 0);
            cab.encode_decision(B + 5, 1); // L1
        }
        _ => {
            // dir == 3 (B_Bi_16x16): m = 0 → return m+3 = 3
            cab.encode_decision(B + ctx_inc, 1);
            cab.encode_decision(B + 3, 1);
            cab.encode_decision(B + 4, 0);
            cab.encode_decision(B + 5, 0);
            cab.encode_decision(B + 5, 0);
            cab.encode_decision(B + 5, 0);
        }
    }
}

const CB_ALL16: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Emit one planned INTER B macroblock (mb_skip_flag already coded 0). `dir` is the
/// B direction 0/1/2/3; `plan.mvds` holds mvd_l0 then mvd_l1 (per used list).
fn emit_mb_cabac_b(
    fe: &mut FrameEncoder,
    cab: &mut CabacEncoder,
    cs: &mut CabacState,
    dir: u8,
    plan: &InterPlan,
    mb_x: usize,
    mb_y: usize,
) {
    let mb_w = fe.mb_w;
    let addr = mb_y * mb_w + mb_x;
    let top = if mb_y > 0 { Some(addr - mb_w) } else { None };
    let left = if mb_x > 0 { Some(addr - 1) } else { None };

    let bci = left.map_or(0, |a| (!cs.mb_direct[a]) as usize)
        + top.map_or(0, |a| (!cs.mb_direct[a]) as usize);
    cb_mb_type_b(cab, bci, dir);

    // Dual-list mvd/ref caches (L0 = mb_ref/mb_mvd, L1 = mb_ref1/mb_mvd1).
    let mut mvdc0 = [[0i16; 2]; 30];
    let mut refc0 = [-1i8; 30];
    let mut mvdc1 = [[0i16; 2]; 30];
    let mut refc1 = [-1i8; 30];
    cb_fill_inter_cache(&cs.mb_ref, &cs.mb_mvd, &mut refc0, &mut mvdc0, top, left, addr, mb_w);
    cb_fill_inter_cache(&cs.mb_ref1, &cs.mb_mvd1, &mut refc1, &mut mvdc1, top, left, addr, mb_w);
    let mut mmvd0 = [[0i16; 2]; 16];
    let mut mref0 = [-1i8; 16];
    let mut mmvd1 = [[0i16; 2]; 16];
    let mut mref1 = [-1i8; 16];
    let (use0, use1) = (dir == 1 || dir == 3, dir == 2 || dir == 3);
    if dir == 0 {
        // B_Direct_16x16: no coded motion; ref 0 in both lists (mvd stays 0) so a
        // later MB's mvd ctxInc sums |0|.
        mref0 = [0i8; 16];
        mref1 = [0i8; 16];
    } else {
        // mvd parse order: list-major (L0 then L1); a single 16x16 partition (idx 0).
        let mut k = 0;
        if use0 {
            cb_emit_mvd_partition(cab, 0, &CB_ALL16, &mut mvdc0, &mut refc0, &mut mmvd0, &mut mref0, plan.mvds[k]);
            k += 1;
        }
        if use1 {
            cb_emit_mvd_partition(cab, 0, &CB_ALL16, &mut mvdc1, &mut refc1, &mut mmvd1, &mut mref1, plan.mvds[k]);
        }
    }
    cs.mb_mvd[addr] = mmvd0;
    cs.mb_ref[addr] = mref0;
    cs.mb_mvd1[addr] = mmvd1;
    cs.mb_ref1[addr] = mref1;
    cs.mb_direct[addr] = dir == 0;
    cs.cat[addr] = 100;
    cb_emit_inter_residual(fe, cab, cs, plan, mb_x, mb_y, addr, top, left);
}

/// Emit a B_Skip macroblock's mb_skip_flag = 1 (ctx 24 base) + neighbour state. The
/// direct motion was committed by `commit_direct_motion`; ref 0 in both lists, mvd 0
/// (matching the decoder's decode_b_skip handling).
fn emit_b_skip_cabac(cab: &mut CabacEncoder, cs: &mut CabacState, addr: usize, top: Option<usize>, left: Option<usize>) {
    let sctx = 24
        + left.map_or(0, |a| (!cs.mb_skip[a]) as usize)
        + top.map_or(0, |a| (!cs.mb_skip[a]) as usize);
    cb_mb_skip(cab, sctx, true);
    cs.mb_skip[addr] = true;
    cs.cat[addr] = 100;
    cs.mb_direct[addr] = true;
    cs.mb_ref[addr] = [0i8; 16];
    cs.mb_ref1[addr] = [0i8; 16];
    cs.last_delta_qp = 0;
}

/// CABAC B-slice data coder. Mirrors `encode_slice_data_b`'s B_Skip-free check +
/// L0/L1/Bi/Direct RD decision verbatim; only the emit differs (per-MB
/// mb_skip_flag + CABAC + per-MB terminate). B is non-reference → no deblock/return.
#[allow(clippy::too_many_arguments)]
pub fn encode_slice_data_cabac_b(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    frame: &YuvFrame,
    qp: u8,
    poc: i32,
    l0: &crate::RefFrame,
    l1: &crate::RefFrame,
) {
    let mut fe = FrameEncoder::new(cfg);
    fe.qp = qp;
    fe.qpc = chroma_qp(qp);
    fe.cur_qp = qp;
    if cfg.cabac_dz_div > 0 {
        fe.idz = cfg.cabac_dz_div; // CABAC-specific dead-zone override
    }
    fe.bi_w = implicit_bi_weights(poc, l0.poc, l1.poc);
    let (sy, su, sv) = coded_source(cfg, frame);
    let lambda = 0.85 * fe.tune_lambda_scale * 2f64.powf((qp as f64 - 12.0) / 3.0);
    let lme = lambda.sqrt() * cfg.cabac_lambda_scale;
    let refs = std::slice::from_ref(l0);
    if fe.satd_q > 0.0 {
        let mut vars: Vec<i64> = (0..fe.mb_h)
            .flat_map(|my| (0..fe.mb_w).map(move |mx| (mx, my)))
            .map(|(mx, my)| mb_variance(&sy, fe.cw, mx, my))
            .collect();
        vars.sort_unstable();
        let idx = (((1.0 - fe.satd_q) * vars.len() as f64) as usize).min(vars.len() - 1);
        fe.satd_var_thresh = vars[idx];
    }
    let aq_qp = aq_qp_map(&sy, fe.cw, fe.mb_w, fe.mb_h, qp, fe.aq_strength);
    fe.cur_qp = qp;

    let mut cab = CabacEncoder::new(qp as i32, cfg.cabac_init_idc, false);
    let mut cs = CabacState::new(fe.mb_w * fe.mb_h);
    let total = fe.mb_w * fe.mb_h;

    for mb_y in 0..fe.mb_h {
        for mb_x in 0..fe.mb_w {
            let mb_idx = mb_y * fe.mb_w + mb_x;
            let addr = mb_idx;
            let top = if mb_y > 0 { Some(addr - fe.mb_w) } else { None };
            let left = if mb_x > 0 { Some(addr - 1) } else { None };
            fe.qp = aq_qp[mb_idx];
            fe.qpc = chroma_qp(aq_qp[mb_idx]);
            let (lx, ly) = (mb_x * 16, mb_y * 16);
            let (pbx, pby) = (mb_x as isize * 4, mb_y as isize * 4);
            fe.mb_use_satd =
                fe.satd_q > 0.0 && mb_variance(&sy, fe.cw, mb_x, mb_y) >= fe.satd_var_thresh;
            let n0 = fe.mv_neighbors_block_list(pbx, pby, 4, 0);
            let n1 = fe.mv_neighbors_block_list(pbx, pby, 4, 1);
            let pmv0 = predict_partition_mv(0, 0, n0[0], n0[1], n0[2], 0);
            let pmv1 = predict_partition_mv(0, 0, n1[0], n1[1], n1[2], 0);
            let (dp, dc, dmotion) = fe.b_direct(l0, l1, mb_x, mb_y);
            // B_Skip: free direct prediction → mb_skip_flag = 1.
            if fe.skip_luma_is_free(&sy, mb_x, mb_y, &dp)
                && fe.skip_chroma_is_free(&su, &sv, mb_x, mb_y, &dc)
            {
                fe.commit_direct_motion(mb_x, mb_y, &dmotion);
                emit_b_skip_cabac(&mut cab, &mut cs, addr, top, left);
                cab.encode_terminate(mb_idx + 1 == total);
                continue;
            }
            let d_direct = fe.pred_dist(&sy, lx, ly, &dp);
            let (mv0, j0) = fe.motion_search(l0, &sy, lx, ly, 16, 16, &[pmv0], lme);
            let (mv1, j1) = fe.motion_search(l1, &sy, lx, ly, 16, 16, &[pmv1], lme);
            let d_bi = fe.bi_dist(l0, l1, &sy, lx, ly, mv0, mv1);
            let r_bi = mvd_bits(mv0.0 - pmv0.0) + mvd_bits(mv0.1 - pmv0.1)
                + mvd_bits(mv1.0 - pmv1.0) + mvd_bits(mv1.1 - pmv1.1);
            let j_bi = d_bi + (lme * r_bi as f64) as i64;
            let (mut dir, mut best) = (0u8, d_direct);
            if j0 < best { dir = 1; best = j0; }
            if j1 < best { dir = 2; best = j1; }
            if j_bi < best { dir = 3; best = j_bi; }
            let _ = best;
            // mb_skip_flag = 0, then the coded B MB.
            let sctx = 24
                + left.map_or(0, |a| (!cs.mb_skip[a]) as usize)
                + top.map_or(0, |a| (!cs.mb_skip[a]) as usize);
            cb_mb_skip(&mut cab, sctx, false);
            cs.mb_skip[addr] = false;
            let bspec = BInter { dir, l1, mv0, mv1 };
            let plan = fe.plan_inter_mb(refs, &sy, &su, &sv, mb_x, mb_y, 0, &[], Some(bspec));
            emit_mb_cabac_b(&mut fe, &mut cab, &mut cs, dir, &plan, mb_x, mb_y);
            cab.encode_terminate(mb_idx + 1 == total);
        }
    }

    while !w.is_byte_aligned() {
        w.write_bit(true);
    }
    for b in cab.into_bytes() {
        w.write_bits(b as u32, 8);
    }
    // B is non-reference: no deblock, no RefFrame (the decoder deblocks for display).
}

/// Minimal all-B_Skip CABAC B-slice (the rare no-bracketing-anchor fallback in
/// `code_picture`): every MB is mb_skip_flag = 1. B is non-reference so the recon
/// is irrelevant; this only needs to be a legal CABAC slice.
pub fn encode_all_skip_b_cabac(w: &mut BitWriter, cfg: &EncoderConfig, qp: u8, n: usize) {
    let mut cab = CabacEncoder::new(qp as i32, cfg.cabac_init_idc, false);
    for i in 0..n {
        // ctxInc = 24 + (left avail & not-skip) + (top avail & not-skip). Every
        // neighbour is either a skip (contributes 0) or unavailable (0) → always 24.
        cab.encode_decision(24, 1);
        cab.encode_terminate(i + 1 == n);
    }
    while !w.is_byte_aligned() {
        w.write_bit(true);
    }
    for b in cab.into_bytes() {
        w.write_bits(b as u32, 8);
    }
}
