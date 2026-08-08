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
    inter_partitions, mc_chroma_padded, mc_luma_padded, predict_mv, predict_partition_mv,
    MvNeighbor,
};
use rusty_h264_common::predict::{
    add_residual_8x8, chroma8x8_pred, chroma_qp, intra4x4_pred, intra8x8_pred, luma16x16_pred,
    reconstruct_4x4, reconstruct_4x4_dc, reconstruct_4x4_dc_into, reconstruct_4x4_into, I16Mode,
    CHROMA_4X4_SCAN_XY, LUMA_4X4_SCAN_XY,
};
use rusty_h264_common::transform::{
    dequant_scatter_4x4, dequantize, dequantize_weighted, inverse_quant_8x8,
    inverse_quant_chroma_dc,
    inverse_quant_chroma_dc_weighted, inverse_quant_luma_dc, inverse_quant_luma_dc_weighted,
};
use rusty_h264_common::{BitReader, YuvFrame};

/// One frame's motion field, in 4x4-block raster (`mb_w*4` wide).
///
/// Captured from any conformant stream this decoder parses — including x264's —
/// so a harness can compare motion fields between encoders without depending on
/// external MV-export tooling.
pub struct MvField {
    pub mb_w: usize,
    pub mb_h: usize,
    pub mv: Vec<(i32, i32)>,
    pub ref_idx: Vec<i32>,
    pub inter: Vec<bool>,
}

/// Frames captured in decode order when `RFF_MV_DUMP=1`. Diagnostic only.
pub static MV_DUMP: std::sync::Mutex<Vec<MvField>> = std::sync::Mutex::new(Vec::new());

pub fn mv_dump_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RFF_MV_DUMP").map_or(false, |v| v != "0"))
}

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
    refs1: Vec<crate::Ref>,
    num_ref_active1: usize,
    is_b: bool,
    /// True if the stream's profile permits B-slices (`profile_idc != 66`). When
    /// false (Baseline / Constrained Baseline), `as_reference` skips the per-block
    /// motion (mv/ref_idx/ref_poc) that only B temporal/spatial direct ever reads.
    b_possible: bool,
    direct_spatial: bool,
    nnz_l_cache: [u8; 25],
    nnz_c_cache: [[u8; 9]; 2],
    /// Decoded-picture buffer (most-recent first); empty in I-slices. `ref_idx`
    /// indexes into this list.
    refs: Vec<crate::Ref>,
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
    /// High-profile 8×8 luma scaling matrices in raster order `[Y-intra, Y-inter]`
    /// (4:2:0 has only these two). `None` = flat.
    scaling8: Option<[[i32; 64]; 2]>,
    /// `transform_8x8_mode_flag` from the PPS: enables `transform_size_8x8_flag`.
    transform_8x8_mode: bool,
    /// Per-macroblock `transform_size_8x8_flag` (for deblocking: internal 4×4
    /// luma edges of 8×8-transform MBs are not filtered).
    mb_t8x8: Vec<bool>,
    // ---- Row-interleaved deblocking state (docs/row-interleave-plan.md) ----
    /// Per-MB boundary strengths, filled row-by-row as decode completes rows.
    bs_frame: Vec<rusty_h264_common::deblock::MbBs>,
    /// Rows whose bS is derived (watermark).
    bs_rows: usize,
    /// Rows already deblock-FILTERED (watermark; R3).
    flt_rows: usize,
    /// Two-row rolling window of packed records (prev = row r-1, cur = row r).
    pk_prev: Vec<rusty_h264_common::deblock::MbPack>,
    pk_cur: Vec<rusty_h264_common::deblock::MbPack>,
    /// Transform-block coded mask (nnz with the 8x8 OR applied), filled per row.
    nnz_dbr: Vec<u8>,
    /// Unfiltered bottom rows of the last-filtered MB row (intra reads these).
    bak_y: Vec<u8>,
    bak_u: Vec<u8>,
    bak_v: Vec<u8>,
    /// Entropy-decouple: deferred pixel jobs + the per-slice activation flag
    /// (CABAC slices only — the CAVLC loop has no flush hooks).
    edc_jobs: Vec<EdcJob>,
    edc_active: bool,
    // ---- E2: the worker-thread plumbing (all None outside a threaded slice) ----
    edc_tx: Option<std::sync::mpsc::SyncSender<EdcMsg>>,
    edc_ctx_rx: Option<std::sync::mpsc::Receiver<PixelCtx>>,
    edc_back_tx: Option<std::sync::mpsc::Sender<PixelCtx>>,
    /// While the parse thread holds the pixel context for an intra macroblock
    /// (planes moved into `self`), the rest of the context parks here.
    edc_parked: Option<PixelCtx>,
    /// E3: while parsing a B macroblock in threaded mode, its MC regions
    /// accumulate here instead of executing (the pixel side is the worker's).
    edc_regions: Option<Vec<BRegion>>,
    /// D10: jobs accumulated for the current row, sent as ONE message.
    edc_batch: Vec<EdcJob>,
    /// D12: bits/MB carried in from previous slices (0 = not yet known).
    bits_per_mb: f64,
    /// Current slice's deblock parameters (set per slice by the caller).
    db_ena: bool,
    db_oa: i32,
    db_ob: i32,
    /// Per-macroblock deblock derivation CLASS (`MB_KIND_*`), so the loop filter
    /// can skip the 24-block neighbourhood gather on macroblocks whose strengths
    /// are determined by syntax alone. Starts UNSET; anything left UNSET simply
    /// takes the blind path, so a missed producer site costs speed, not
    /// correctness. Only classes that are uniform BY SYNTAX are written — notably
    /// NOT `B_Skip`/`B_Direct`, whose direct-derived motion varies per 4×4.
    mb_kind: Vec<u8>,
    /// Explicit weighted-prediction tables, when active for this slice.
    weights: Option<WeightTable>,
    /// Current picture's `PicOrderCnt` (for temporal direct + implicit weighting).
    cur_poc: i32,
    /// `weighted_bipred_idc` (0 = none/average, 1 = explicit, 2 = implicit).
    weighted_bipred_idc: u8,
    /// `direct_8x8_inference_flag` (B direct co-located sub-block selection).
    direct_8x8_inference: bool,
}

/// Explicit weighted-prediction tables (spec §7.4.3.2 / §8.4.2.3.2). Per
/// reference list, per ref index: a luma `(weight, offset)` and two chroma
/// `(weight, offset)` (Cb, Cr). `log2` denominators are shared.
#[derive(Clone, Default)]
pub struct WeightTable {
    pub luma_log2_denom: i32,
    pub chroma_log2_denom: i32,
    /// `[list][ref_idx] = (weight, offset)`.
    pub luma: [Vec<(i32, i32)>; 2],
    /// `[list][ref_idx][cb=0/cr=1] = (weight, offset)`.
    pub chroma: [Vec<[(i32, i32); 2]>; 2],
}

impl WeightTable {
    /// Applies a single-list (uni-prediction) luma weight (spec §8.4.2.3.2).
    fn apply_luma(&self, sample: u8, list: usize, refi: usize) -> u8 {
        let (w, o) = self.luma[list][refi];
        let lwd = self.luma_log2_denom;
        let v = if lwd >= 1 {
            ((sample as i32 * w + (1 << (lwd - 1))) >> lwd) + o
        } else {
            sample as i32 * w + o
        };
        v.clamp(0, 255) as u8
    }

    /// Applies a single-list (uni-prediction) chroma weight for component `cc`.
    fn apply_chroma(&self, sample: u8, list: usize, refi: usize, cc: usize) -> u8 {
        let (w, o) = self.chroma[list][refi][cc];
        let cwd = self.chroma_log2_denom;
        let v = if cwd >= 1 {
            ((sample as i32 * w + (1 << (cwd - 1))) >> cwd) + o
        } else {
            sample as i32 * w + o
        };
        v.clamp(0, 255) as u8
    }
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

/// Recycled per-picture scratch grids.
///
/// `FrameDecoder::new` used to allocate ~1.65 MB of frame-wide grids for EVERY
/// coded picture and drop them when the picture finished. The sampled profiler
/// prices that (stage `dec-setup`) at 6.7% of decode — larger than dequant,
/// reconstruct and intra prediction combined, and none of it is codec work.
///
/// Two costs are being paid, and the allocation is the bigger one. A ~460 KB
/// `Vec` goes straight to the OS, so every page is a fresh zero page and the
/// decoder takes a soft page fault on FIRST TOUCH of each 4 KB — a cost charged
/// to whatever per-macroblock stage happens to touch it first, not to the
/// allocation. Handing the same buffers back keeps the pages mapped and warm.
///
/// The initialising fill is NOT skipped: these grids are read as neighbour
/// context (`modes_y` must read 2/DC, `ref_idx_y` must read -1) before every
/// block that writes them, so a stale value from the previous picture is a
/// correctness bug, not a performance trade. `clear()` + `resize()` keeps the
/// fill and drops only the allocation.
///
/// The reconstruction planes are deliberately NOT pooled: `into_frame` MOVES
/// them out as the caller's output frame, so there is nothing to hand back.
#[derive(Default)]
pub struct GridPool {
    /// D12: running bits-per-macroblock of decoded slices, the E2 dispatch's
    /// density signal. Lives here because `GridPool` is the only state that
    /// survives a picture (`FrameDecoder` is rebuilt per picture).
    bits_per_mb: f64,
    mb_qp: Vec<u8>,
    bs_frame: Vec<rusty_h264_common::deblock::MbBs>,
    pk_prev: Vec<rusty_h264_common::deblock::MbPack>,
    pk_cur: Vec<rusty_h264_common::deblock::MbPack>,
    nnz_dbr: Vec<u8>,
    bak_y: Vec<u8>,
    bak_u: Vec<u8>,
    bak_v: Vec<u8>,
    nnz_y: Vec<u8>,
    nnz_c0: Vec<u8>,
    nnz_c1: Vec<u8>,
    modes_y: Vec<u8>,
    coded_y: Vec<bool>,
    mv_y: Vec<(i32, i32)>,
    inter_y: Vec<bool>,
    ref_idx_y: Vec<i32>,
    mv1: Vec<(i32, i32)>,
    ref_idx1: Vec<i32>,
    mb_t8x8: Vec<bool>,
    mb_kind: Vec<u8>,
}

/// Reuse `v`'s allocation for `n` copies of `val`. Identical OBSERVABLE result to
/// `vec![val; n]`; differs only in that it reuses the existing allocation when the
/// capacity already suffices.
#[inline]
fn refill<T: Clone>(mut v: Vec<T>, n: usize, val: T) -> Vec<T> {
    v.clear();
    v.resize(n, val);
    v
}

impl FrameDecoder {
    pub fn new(
        mb_w: usize,
        mb_h: usize,
        qp: u8,
        chroma_qp_offset: i32,
        refs: Vec<crate::Ref>,
        num_ref_active: usize,
        constrained_intra: bool,
        transform_8x8_mode: bool,
        b_possible: bool,
    ) -> Self {
        Self::with_pool(
            mb_w,
            mb_h,
            qp,
            chroma_qp_offset,
            refs,
            num_ref_active,
            constrained_intra,
            transform_8x8_mode,
            b_possible,
            GridPool::default(),
        )
    }

    /// As `new`, but reusing a previous picture's grid allocations. See `GridPool`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_pool(
        mb_w: usize,
        mb_h: usize,
        qp: u8,
        chroma_qp_offset: i32,
        refs: Vec<crate::Ref>,
        num_ref_active: usize,
        constrained_intra: bool,
        transform_8x8_mode: bool,
        b_possible: bool,
        pool: GridPool,
    ) -> Self {
        let (cw, ch) = (mb_w * 16, mb_h * 16);
        let (ccw, cch) = (cw / 2, ch / 2);
        let bits_per_mb = pool.bits_per_mb;
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
            mb_qp: refill(pool.mb_qp, mb_w * mb_h, qp),
            slice_first_mb: 0,
            nnz_y: refill(pool.nnz_y, (mb_w * 4) * (mb_h * 4), 0),
            nnz_c: [
                refill(pool.nnz_c0, (mb_w * 2) * (mb_h * 2), 0),
                refill(pool.nnz_c1, (mb_w * 2) * (mb_h * 2), 0),
            ],
            modes_y: refill(pool.modes_y, (mb_w * 4) * (mb_h * 4), 2),
            coded_y: refill(pool.coded_y, (mb_w * 4) * (mb_h * 4), false),
            mv_y: refill(pool.mv_y, (mb_w * 4) * (mb_h * 4), (0, 0)),
            inter_y: refill(pool.inter_y, (mb_w * 4) * (mb_h * 4), false),
            ref_idx_y: refill(pool.ref_idx_y, (mb_w * 4) * (mb_h * 4), -1),
            mv1: refill(pool.mv1, (mb_w * 4) * (mb_h * 4), (0, 0)),
            ref_idx1: refill(pool.ref_idx1, (mb_w * 4) * (mb_h * 4), -1),
            refs1: Vec::new(),
            num_ref_active1: 0,
            is_b: false,
            b_possible,
            direct_spatial: true,
            nnz_l_cache: [0x80; 25],
            nnz_c_cache: [[0x80; 9]; 2],
            refs,
            num_ref_active,
            constrained_intra,
            scaling: None,
            scaling8: None,
            transform_8x8_mode,
            mb_t8x8: refill(pool.mb_t8x8, mb_w * mb_h, false),
            bs_frame: refill(pool.bs_frame, mb_w * mb_h, Default::default()),
            bs_rows: 0,
            flt_rows: 0,
            pk_prev: {
                let mut v = pool.pk_prev;
                v.clear();
                v
            },
            pk_cur: {
                let mut v = pool.pk_cur;
                v.clear();
                v
            },
            nnz_dbr: refill(pool.nnz_dbr, (mb_w * 4) * (mb_h * 4), 0),
            bak_y: refill(pool.bak_y, cw, 0),
            bak_u: refill(pool.bak_u, ccw, 0),
            bak_v: refill(pool.bak_v, ccw, 0),
            edc_jobs: Vec::new(),
            edc_active: false,
            edc_tx: None,
            edc_ctx_rx: None,
            edc_back_tx: None,
            edc_parked: None,
            edc_regions: None,
            edc_batch: Vec::new(),
            bits_per_mb,
            db_ena: false,
            db_oa: 0,
            db_ob: 0,
            mb_kind: refill(
                pool.mb_kind,
                mb_w * mb_h,
                rusty_h264_common::deblock::MB_KIND_UNSET,
            ),
            weights: None,
            cur_poc: 0,
            weighted_bipred_idc: 0,
            direct_8x8_inference: false,
        }
    }

    /// Sets the explicit weighted-prediction tables for this slice.
    pub fn set_weights(&mut self, weights: WeightTable) {
        self.weights = Some(weights);
    }

    /// Applies explicit uni-prediction weighting to a motion-compensated partition
    /// (luma `pred_y` region + the two chroma planes), if weighting is active.
    /// `list` is the reference list and `refi` the partition's reference index.
    fn weight_partition(
        &self,
        pred_y: &mut [u8; 256],
        c_pred: &mut [[u8; 64]; 2],
        list: usize,
        refi: usize,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
    ) {
        let Some(wt) = &self.weights else { return };
        for dy in 0..rh {
            for dx in 0..rw {
                let i = (ry + dy) * 16 + (rx + dx);
                pred_y[i] = wt.apply_luma(pred_y[i], list, refi);
            }
        }
        let (crx, cry, crw, crh) = (rx / 2, ry / 2, rw / 2, rh / 2);
        for cc in 0..2 {
            for dy in 0..crh {
                for dx in 0..crw {
                    let i = (cry + dy) * 8 + (crx + dx);
                    c_pred[cc][i] = wt.apply_chroma(c_pred[cc][i], list, refi, cc);
                }
            }
        }
    }

    /// Sets the High-profile scaling matrices (raster order: six 4×4 lists, two
    /// 8×8 luma lists). The caller un-zig-zags the SPS lists. Flat is the default.
    pub fn set_scaling(&mut self, scaling: [[i32; 16]; 6], scaling8: [[i32; 64]; 2]) {
        self.scaling = Some(scaling);
        self.scaling8 = Some(scaling8);
    }

    /// Dequantizes a 4×4 AC block with scaling list `list` (flat if none active).
    fn dequant(&self, levels: &[i32; 16], qp: u8, list: usize) -> [i32; 16] {
        match &self.scaling {
            Some(s) => dequantize_weighted(levels, qp, &s[list]),
            None => dequantize(levels, qp),
        }
    }

    /// Single-coefficient twin of `dequant` for position 0 (DC-only fast path).
    fn dequant_dc4(&self, level: i32, qp: u8, list: usize) -> i32 {
        rusty_h264_common::transform::dequantize_dc4(
            level,
            qp,
            self.scaling.as_ref().map(|s| s[list][0]),
        )
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
    #[allow(clippy::too_many_arguments)]
    pub fn set_b_context(
        &mut self,
        refs1: Vec<crate::Ref>,
        num_ref_active1: usize,
        direct_spatial: bool,
        cur_poc: i32,
        weighted_bipred_idc: u8,
        direct_8x8_inference: bool,
    ) {
        self.is_b = true;
        self.refs1 = refs1;
        self.num_ref_active1 = num_ref_active1;
        self.direct_spatial = direct_spatial;
        self.cur_poc = cur_poc;
        self.weighted_bipred_idc = weighted_bipred_idc;
        self.direct_8x8_inference = direct_8x8_inference;
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
    pub fn begin_slice(&mut self, slice_qp: u8, refs: Vec<crate::Ref>, num_ref_active: usize) {
        self.cur_qp = slice_qp;
        self.qp = slice_qp;
        self.refs = refs;
        self.num_ref_active = num_ref_active;
        self.weights = None; // re-set per slice if a pred_weight_table is present
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
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Neighbors);
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

    /// Commit one inter partition's motion into the 4×4 grid (ref 0, 1-ref P).
    /// `(rx,ry,rw,rh)` are MB-relative luma pixels; committing before the next
    /// partition's prediction is what lets a later partition predict from it.
    fn commit_inter_grid(&mut self, mb_x: usize, mb_y: usize, rx: usize, ry: usize, rw: usize, rh: usize, mv: (i32, i32), refi: i8) {
        let w4 = self.mb_w * 4;
        for by in ry / 4..ry / 4 + rh / 4 {
            for bx in rx / 4..rx / 4 + rw / 4 {
                let idx = (mb_y * 4 + by) * w4 + (mb_x * 4 + bx);
                self.mv_y[idx] = mv;
                self.inter_y[idx] = true;
                self.ref_idx_y[idx] = refi as i32;
                self.coded_y[idx] = true;
            }
        }
    }

    /// Per-slice deblock parameters, needed DURING decode by the row-interleave
    /// path. `ena` is already resolved against `RFF_ABL_DEBLOCK` by the caller.
    pub fn set_deblock_params(&mut self, ena: bool, oa: i32, ob: i32) {
        // Latch: the FIRST disabling slice turns row filtering off for the rest
        // of the picture (see `row_hook`); rows already filtered stay counted
        // in `flt_rows` and the picture-end tail handles the remainder.
        self.db_ena = ena && (self.flt_rows == 0 || self.db_ena);
        self.db_oa = oa;
        self.db_ob = ob;
    }

    /// Derives bS for macroblock row `r` from the just-decoded (hot) grids into
    /// `bs_frame`, maintaining the two-row rolling record window (R2 of
    /// docs/row-interleave-plan.md).
    fn derive_bs_row(&mut self, r: usize) {
        // Same stage label the in-filter derivation used, so profiles keep
        // pricing bS derivation wherever it lives.
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DebDerive);
        use rusty_h264_common::deblock::{derive_mb_records, pack_mb, BlockInfo, MbBs};
        let (mb_w, w4) = (self.mb_w, self.mb_w * 4);
        // Transform-block coded mask for this row: raw nnz, then the 8x8 OR for
        // t8 macroblocks (spec §8.7: the 8x8 transform's coded status is per 8x8).
        for br in r * 4..r * 4 + 4 {
            let a = br * w4;
            self.nnz_dbr[a..a + w4].copy_from_slice(&self.nnz_y[a..a + w4]);
        }
        for mb_x in 0..mb_w {
            if !self.mb_t8x8[r * mb_w + mb_x] {
                continue;
            }
            for b8 in 0..4usize {
                let (bx, by) = (mb_x * 4 + (b8 % 2) * 2, r * 4 + (b8 / 2) * 2);
                let any = (0..2).any(|sy| (0..2).any(|sx| self.nnz_y[(by + sy) * w4 + bx + sx] > 0));
                for sy in 0..2 {
                    for sx in 0..2 {
                        self.nnz_dbr[(by + sy) * w4 + bx + sx] = any as u8;
                    }
                }
            }
        }
        let poc0: Vec<i32> = self.refs.iter().map(|f| f.poc).collect();
        let poc1: Vec<i32> = self.refs1.iter().map(|f| f.poc).collect();
        let info = BlockInfo {
            inter: &self.inter_y,
            nnz: &self.nnz_dbr,
            mv: &self.mv_y,
            ref_id: &self.ref_idx_y,
            mv1: &self.mv1,
            ref_id1: if poc1.is_empty() { &[] } else { &self.ref_idx1 },
            w4,
            t8x8: &self.mb_t8x8,
            bs: &[],
            poc0: &poc0,
            poc1: &poc1,
            kind: &[],
        };
        let has1 = !info.ref_id1.is_empty();
        std::mem::swap(&mut self.pk_prev, &mut self.pk_cur);
        self.pk_cur.clear();
        for mb_x in 0..mb_w {
            self.pk_cur.push(pack_mb(&info, has1, mb_x, r));
            let cur = &self.pk_cur[mb_x];
            let left = if mb_x > 0 { Some(&self.pk_cur[mb_x - 1]) } else { None };
            let top = if r > 0 { Some(&self.pk_prev[mb_x]) } else { None };
            let mb_t8 = self.mb_t8x8[r * mb_w + mb_x];
            let (mut bv, mut bh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
            derive_mb_records(cur, left, top, mb_t8, &mut bv, &mut bh);
            let mut m = MbBs::default();
            for e in 0..4 {
                for sg in 0..4 {
                    m.v[e][sg] = bv[e][sg] as u8;
                    m.h[e][sg] = bh[e][sg] as u8;
                }
            }
            self.bs_frame[r * mb_w + mb_x] = m;
        }
    }

    /// Decode-loop hook: called at each MB-loop head with the NEXT address to be
    /// decoded; derives AND FILTERS (R3) every fully-decoded row. Filtering a
    /// row here preserves the spec's raster per-MB filter order exactly (every
    /// MB the row's edges touch is already decoded; bottom-adjacent edges
    /// belong to the NEXT row's MBs, which filter later).
    #[inline]
    fn row_hook(&mut self, addr: usize) {
        let _rh = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecRowHook);
        edcstat::bump(&edcstat::MBS, 1); // called once per MB-loop head
        if !rowdb_on() {
            // Even without row deblocking, completed rows' deferred pixel jobs
            // must not pile up past a row boundary indefinitely; flush here so
            // the queue stays row-sized.
            self.edc_flush();
            return;
        }
        let done = addr / self.mb_w;
        if self.edc_tx.is_some() {
            // E2: derivation stays here (it reads the syntax grids); filtering
            // is the worker's, fed the row's bs/qp/t8 snapshot. `flt_rows`
            // advances on the worker and comes home with the context.
            if self.bs_rows < done {
                self.edc_giveback();
            }
            while self.bs_rows < done {
                let r = self.bs_rows;
                self.derive_bs_row(r);
                self.bs_rows += 1;
                let base = r * self.mb_w;
                // ORDER: this row's pixel jobs must reach the worker BEFORE the
                // filter message for the same row.
                self.edc_flush_batch();
                edcstat::bump(&edcstat::ROWS, 1);
                edcstat::bump(
                    &edcstat::ROWBYTES,
                    (self.mb_w
                        * (std::mem::size_of::<rusty_h264_common::deblock::MbBs>() + 2))
                        as u64,
                );
                let msg = EdcMsg::Row {
                    r,
                    bs: self.bs_frame[base..base + self.mb_w].to_vec(),
                    qp: self.mb_qp[base..base + self.mb_w].to_vec(),
                    t8: self.mb_t8x8[base..base + self.mb_w].to_vec(),
                };
                self.edc_tx.as_ref().unwrap().send(msg).expect("worker alive");
            }
            return;
        }
        if self.bs_rows < done {
            self.edc_flush();
        }
        while self.bs_rows < done {
            let r = self.bs_rows;
            self.derive_bs_row(r);
            self.bs_rows += 1;
            // Row filtering requires deblock enabled on EVERY slice so far
            // (`db_ena` latches false once any slice disables it): a mixed
            // picture falls back to the picture-end tail so "latest slice
            // wins" semantics are preserved.
            if self.db_ena {
                self.save_bak(r);
                self.filter_row(r);
                self.flt_rows = r + 1;
            }
        }
    }

    /// Saves the UNFILTERED bottom pixel rows of MB row `r` before filtering
    /// modifies them: the next row's intra prediction must read pre-deblock
    /// samples (spec §8.3), and filtering touches the bottom three rows while
    /// intra reads exactly the bottom ONE (+ the corner) — so one backup row
    /// per plane suffices, overwritten per row.
    fn save_bak(&mut self, r: usize) {
        let y0 = (r * 16 + 15) * self.cw;
        self.bak_y.copy_from_slice(&self.rec_y[y0..y0 + self.cw]);
        let c0 = (r * 8 + 7) * self.ccw;
        self.bak_u.copy_from_slice(&self.rec_u[c0..c0 + self.ccw]);
        self.bak_v.copy_from_slice(&self.rec_v[c0..c0 + self.ccw]);
    }

    /// Filters one MB row against the stored strengths, using the CURRENT
    /// slice's alpha/beta offsets (single-offset streams — the whole corpus —
    /// are bit-identical to the picture-end call; the plan's risk register
    /// documents the multi-offset divergence).
    fn filter_row(&mut self, r: usize) {
        let info = rusty_h264_common::deblock::BlockInfo {
            inter: &self.inter_y,
            nnz: &self.nnz_dbr,
            mv: &self.mv_y,
            ref_id: &self.ref_idx_y,
            mv1: &self.mv1,
            ref_id1: &self.ref_idx1,
            w4: self.mb_w * 4,
            t8x8: &self.mb_t8x8,
            bs: &self.bs_frame,
            poc0: &[],
            poc1: &[],
            kind: &self.mb_kind,
        };
        rusty_h264_common::deblock::filter_frame_rows(
            &mut self.rec_y,
            &mut self.rec_u,
            &mut self.rec_v,
            self.mb_w,
            self.mb_h,
            r..r + 1,
            &self.mb_qp,
            self.chroma_qp_offset,
            self.db_oa,
            self.db_ob,
            &info,
        );
    }

    /// Top-neighbour LUMA pixel for intra prediction: reads the unfiltered
    /// backup row when the row above has already been deblock-filtered by the
    /// row-interleave (flt_rows gates it; 0 when the interleave is off, so
    /// this compiles to the plain read on the fallback path).
    #[inline]
    fn top_y_px(&self, py: usize, x: usize) -> u8 {
        if py % 16 == 0 && self.flt_rows * 16 >= py {
            self.bak_y[x]
        } else {
            self.rec_y[(py - 1) * self.cw + x]
        }
    }

    /// Slice form of [`Self::top_y_px`] for the contiguous 16-wide I16 gather.
    #[inline]
    fn top_y_row(&self, py: usize, x: usize, n: usize) -> &[u8] {
        if py % 16 == 0 && self.flt_rows * 16 >= py {
            &self.bak_y[x..x + n]
        } else {
            &self.rec_y[(py - 1) * self.cw + x..][..n]
        }
    }

    /// Top-neighbour CHROMA pixel (plane `c`: 0 = U, 1 = V).
    #[inline]
    fn top_c_px(&self, c: usize, cy: usize, x: usize) -> u8 {
        if cy % 8 == 0 && self.flt_rows * 8 >= cy {
            if c == 0 { self.bak_u[x] } else { self.bak_v[x] }
        } else {
            let rec = if c == 0 { &self.rec_u } else { &self.rec_v };
            rec[(cy - 1) * self.ccw + x]
        }
    }

    /// Slice form of [`Self::top_c_px`] for the 8-wide chroma gather.
    #[inline]
    fn top_c_row(&self, c: usize, cy: usize, x: usize, n: usize) -> &[u8] {
        if cy % 8 == 0 && self.flt_rows * 8 >= cy {
            if c == 0 { &self.bak_u[x..x + n] } else { &self.bak_v[x..x + n] }
        } else {
            let rec = if c == 0 { &self.rec_u } else { &self.rec_v };
            &rec[(cy - 1) * self.ccw + x..][..n]
        }
    }

    /// Snapshots the (deblocked) reconstruction as a reference picture.
    pub fn as_reference(&self) -> crate::RefFrame {
        self.as_reference_pooled(&mut Vec::new())
    }

    /// `as_reference` drawing its padded-plane allocations from `pool` (recycled
    /// planes of evicted DPB frames — see `Decoder::reclaim_retired`). ~1.9 MB of
    /// fresh allocation per reference picture otherwise (`dpb-clone` stage, 3-4%
    /// of decode, mostly first-touch page faults).
    pub fn as_reference_pooled(&self, pool: &mut Vec<Vec<u8>>) -> crate::RefFrame {
        // MV CAPTURE (`RFF_MV_DUMP=1`) — lets a harness read the motion field any
        // conformant H.264 stream carries, including x264's, using this decoder as
        // the parser. Diagnostic only; inert unless the env var is set.
        if mv_dump_on() {
            MV_DUMP.lock().unwrap().push(MvField {
                mb_w: self.mb_w,
                mb_h: self.mb_h,
                mv: self.mv_y.clone(),
                ref_idx: self.ref_idx_y.clone(),
                inter: self.inter_y.clone(),
            });
        }

        // The per-block motion (mv/ref_idx/ref_poc) is read ONLY by B temporal/spatial
        // direct (`col.mv/ref_idx/ref_poc`, guarded on `w4 != 0` + `idx < len`). On
        // Baseline/Constrained-Baseline streams (no B) it's pure waste — skip the two
        // grid clones + the per-block ref_poc resolve/alloc. `w4 = 0` makes the B
        // readers no-op even on malformed input.
        let (mv, ref_idx, mv1, ref_idx1, ref_poc, w4) = if self.b_possible {
            (
                self.mv_y.clone(),
                self.ref_idx_y.clone(),
                self.mv1.clone(),
                self.ref_idx1.clone(),
                // Resolve each block's List-0 ref index to the referenced picture's
                // POC, so temporal direct can map it into the current list.
                self.ref_idx_y
                    .iter()
                    .map(|&r| {
                        if r >= 0 {
                            self.refs.get(r as usize).map_or(i32::MIN, |f| f.poc)
                        } else {
                            i32::MIN
                        }
                    })
                    .collect(),
                self.mb_w * 4,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0)
        };
        // Pop an exact-size recycled buffer per plane; a miss falls back to a
        // fresh allocation inside `pad_plane_into`.
        let mut take = |len: usize| -> Vec<u8> {
            match pool.iter().position(|v| v.len() == len) {
                Some(i) => pool.swap_remove(i),
                None => Vec::new(),
            }
        };
        let (lpw, lph) = (self.cw + 2 * crate::LPAD, self.ch + 2 * crate::LPAD);
        let (cpw, cph) = (self.ccw + 2 * crate::CPAD, self.ch / 2 + 2 * crate::CPAD);
        crate::RefFrame {
            // Pad once here (ExpandPicture) instead of extracting a clamped tile
            // on every MC call — same copy class as the old plane clone.
            py: rusty_h264_common::inter::pad_plane_into(take(lpw * lph), &self.rec_y, self.cw, self.ch, crate::LPAD),
            pu: rusty_h264_common::inter::pad_plane_into(take(cpw * cph), &self.rec_u, self.ccw, self.ch / 2, crate::CPAD),
            pv: rusty_h264_common::inter::pad_plane_into(take(cpw * cph), &self.rec_v, self.ccw, self.ch / 2, crate::CPAD),
            cw: self.cw,
            ch: self.ch,
            frame_num: 0, // set by the caller (decode_slice knows frame_num)
            poc: 0,       // set by the caller
            mv,
            ref_idx,
            mv1,
            ref_idx1,
            ref_poc,
            w4,
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
    /// CABAC slice-data decode (docs/cabac-decode-plan.md), brought up brick by brick
    /// against the instrumented openh264 oracle. Phase 1: verify engine init; the
    /// syntax layer (Phase 2+) is WIP.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_slice_data_cabac(
        &mut self,
        rbsp: &[u8],
        start_byte: usize,
        slice_qp: u8,
        cabac_init_idc: u32,
        is_i: bool,
        is_p: bool,
        first_mb: usize,
    ) -> Result<usize, MbError> {
        // E2: overlap parse (this thread) with pixel reconstruction (a scoped
        // worker owning the planes) for P slices. I slices and B slices keep
        // the inline path (their pixel coupling is per-MB); the ownership
        // ping-pong around intra-in-P macroblocks is `edc_intra_sync`.
        let eligible = edc_on() && rowdb_on() && !is_i && (is_p || self.is_b);
        let threaded = eligible
            && edc_mt()
                .unwrap_or_else(|| edc_dispatch(self.mb_w, self.mb_h, self.bits_per_mb, true));
        edcstat::bump(&edcstat::DISPATCH_ON, threaded as u64);
        edcstat::bump(&edcstat::DISPATCH_SEEN, eligible as u64);
        if !threaded {
            let r = self.decode_slice_cabac_inner(rbsp, start_byte, slice_qp, cabac_init_idc, is_i, is_p, first_mb);
            self.note_slice_density(rbsp.len().saturating_sub(start_byte), first_mb, &r);
            return r;
        }
        let ctx = self.edc_take_ctx();
        // D7 PROBE: is the CPU overhead PAYLOAD (alloc/copy per job) or
        // SYNCHRONISATION (blocking on a full queue, park/unpark)? The bound
        // separates them: raising it removes send-blocking without changing a
        // single byte copied. `RS_H264_EDC_BOUND` sweeps it.
        let (tx, rx) = std::sync::mpsc::sync_channel::<EdcMsg>(edc_bound());
        let (ctx_tx, ctx_rx) = std::sync::mpsc::channel::<PixelCtx>();
        let (back_tx, back_rx) = std::sync::mpsc::channel::<PixelCtx>();
        let (res, ctx, panicked) = std::thread::scope(|sc| {
            let h = sc.spawn(move || edc_worker(ctx, rx, ctx_tx, back_rx));
            self.edc_tx = Some(tx);
            self.edc_ctx_rx = Some(ctx_rx);
            self.edc_back_tx = Some(back_tx);
            // UNWIND SAFETY (found by the fuzzer as a HANG, not a failure): a
            // panic inside the parse loop would skip the cleanup below — but
            // the sender lives in `self`, which outlives the unwind, so the
            // channel would never close, the worker would never exit, and the
            // scope's join would block forever, converting a diagnosable panic
            // into a silent deadlock under `catch_unwind` harnesses. Catch,
            // clean up, join, restore the planes, THEN resume the panic.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.decode_slice_cabac_inner(rbsp, start_byte, slice_qp, cabac_init_idc, is_i, is_p, first_mb)
            }));
            self.edc_flush_batch(); // ORDER: no job may outlive the channel
            self.edc_giveback(); // if an intra macroblock left us holding
            self.edc_tx = None; // closes the channel -> worker drains + returns
            self.edc_ctx_rx = None;
            self.edc_back_tx = None;
            match (r, h.join()) {
                (Ok(res), Ok(ctx)) => (res, Some(ctx), None),
                (Err(p), Ok(ctx)) => (Err(MbError::Truncated), Some(ctx), Some(p)),
                (Ok(_), Err(p)) | (Err(_), Err(p)) => (Err(MbError::Truncated), None, Some(p)),
            }
        });
        if let Some(ctx) = ctx {
            self.edc_restore_ctx(ctx);
        }
        if let Some(p) = panicked {
            std::panic::resume_unwind(p);
        }
        self.note_slice_density(rbsp.len().saturating_sub(start_byte), first_mb, &res);
        res
    }

    /// Feed the D12 dispatch its density signal from a slice just decoded.
    /// Exponentially smoothed so one atypical slice cannot flip the arm, and
    /// only ever read on the NEXT slice — the current one is already committed.
    fn note_slice_density(&mut self, bytes: usize, first_mb: usize, r: &Result<usize, MbError>) {
        let Ok(end) = r else { return };
        let mbs = end.saturating_sub(first_mb);
        if mbs == 0 {
            return;
        }
        let bpm = (bytes * 8) as f64 / mbs as f64;
        self.bits_per_mb = if self.bits_per_mb == 0.0 {
            bpm
        } else {
            0.75 * self.bits_per_mb + 0.25 * bpm
        };
    }

    fn decode_slice_cabac_inner(
        &mut self,
        rbsp: &[u8],
        start_byte: usize,
        slice_qp: u8,
        cabac_init_idc: u32,
        is_i: bool,
        is_p: bool,
        first_mb: usize,
    ) -> Result<usize, MbError> {
        self.edc_active = edc_on();
        let mut cab = crate::cabac::Cabac::new(rbsp, start_byte, slice_qp as i32, cabac_init_idc, is_i);
        let (range, _offset) = cab.dbg_state();
        let trace = std::env::var_os("RH_CABAC_TRACE").is_some();
        debug_assert_eq!(range, 510, "CABAC init range must be 510");

        const I16_CBP: [u32; 6] = [0, 16, 32, 15, 31, 47];
        let mbw = self.mb_w;
        let total = self.mb_w * self.mb_h;
        // Per-MB neighbour state (single-slice assumption: avail == in-bounds).
        // SCOPED: 11 zero-initialised allocations sized by MB count, once per slice.
        let _alloc_g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecSliceAlloc);
        let mut cat = vec![255u8; total]; // 0=I4x4, 2=I16, 255=unavailable
        let mut mb_cbp = vec![0u8; total];
        let mut cmode = vec![-1i32; total]; // chroma pred mode
        let mut mb_nzc = vec![[0u8; 24]; total]; // 16 luma raster + 8 chroma
        let mut cbf_dc = vec![0u16; total];
        let mut mb_skip = vec![false; total];
        let mut mb_ref = vec![[-1i8; 16]; total]; // per-4×4-block List-0 ref (-1 = intra)
        let mut mb_mvd = vec![[[0i16; 2]; 16]; total]; // per-block mvd (for mvd ctxInc)
        let mut mb_ref1 = vec![[-1i8; 16]; total]; // B: per-block List-1 ref (-1 = not in list)
        let mut mb_mvd1 = vec![[[0i16; 2]; 16]; total]; // B: per-block List-1 mvd (ctxInc)
        let mut mb_direct = vec![false; total]; // B: MB is (skip/)direct — for mb_type ctxInc
        drop(_alloc_g);
        let mut last_delta_qp = 0i32;
        let mut addr = first_mb;

        let _mbloop_g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMbLoop);
        loop {
            // BOUND the entropy-coded loop. `decode_terminate` is the only exit, and a
            // mutated stream can simply never produce it — the arithmetic decoder
            // zero-fills past the end of the buffer and keeps yielding symbols. Without
            // this the loop walks `addr` past the picture and indexes out of bounds.
            // (Surfaced by the fuzzer the moment CABAC became the default; the CAVLC
            // slice loop already had its own bound.)
            if addr >= total {
                return Err(MbError::Truncated);
            }
            self.row_hook(addr);
            let (mbx, mby) = (addr % mbw, addr / mbw);
            let left = (mbx > 0).then(|| addr - 1);
            let top = (mby > 0).then(|| addr - mbw);

            // Brick 3.1/3.2: P-slice mb_skip_flag, then mb_type (P mb_type is neighbour-
            // independent; intra sub-types map to the I dispatch below).
            let mb_type;
            if is_p {
                let sctx = 11
                    + left.map_or(0, |a| (!mb_skip[a]) as usize)
                    + top.map_or(0, |a| (!mb_skip[a]) as usize);
                if parse_mb_skip_cabac(&mut cab, sctx) {
                    mb_skip[addr] = true;
                    cat[addr] = 100; // inter (not I16/PCM) for neighbour context
                    last_delta_qp = 0; // skip codes no mb_qp_delta → delta ctxInc resets
                    // P_Skip recon reuses the entropy-free CAVLC primitive verbatim: it
                    // takes no bit-reader (skip has no coded syntax past the flag), just
                    // predicts the skip MV, motion-compensates, and commits the grid.
                    self.decode_p_skip(mbx, mby)?;
                    self.mb_qp[addr] = self.cur_qp; // skip inherits QPy
                    let eos = cab.decode_terminate();
                    addr += 1;
                    if eos || addr >= total {
                        break;
                    }
                    continue;
                }
                let mbt = parse_mb_type_p_cabac(&mut cab);
                if mbt == 30 {
                    return Err(MbError::Unsupported("CABAC I_PCM (WIP)"));
                }
                if mbt <= 3 {
                    let _gb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMbP);
                    // noSubMbPartSizeLessThan8x8Flag (spec 7.3.5): P_8x8 permits the
                    // 8x8 transform only when every sub-partition is itself 8x8.
                    let mut allow8 = true;
                    // Inter MB (Bricks 3.3/3.4/3.5). 1-ref stream → ref_idx not coded (ref=0).
                    // Build the 30-entry mvd/ref neighbour cache (openh264 WelsFillCacheInterCabac).
                    let mut mvdc = [[0i16; 2]; 30];
                    let mut refc = [-1i8; 30];
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
                    if mbx > 0 && mby > 0 {
                        let a = addr - mbw - 1;
                        (refc[0], mvdc[0]) = (mb_ref[a][15], mb_mvd[a][15]);
                    }
                    if mby > 0 && mbx + 1 < mbw {
                        let a = addr - mbw + 1;
                        (refc[5], mvdc[5]) = (mb_ref[a][12], mb_mvd[a][12]);
                    }
                    let mut mmvd = [[0i16; 2]; 16];
                    let mut mref = [0i8; 16];
                    // mb_pred (spec 7.3.5.1): all ref_idx_l0 FIRST (only when >1 active
                    // ref), then all mvd + ref-aware predict + commit. `refidx!` parses one
                    // partition's ref_idx (ctxIdxOffset 54, ctx from neighbour refc) and
                    // seeds refc so a later partition's ref/mvd context sees it — mirror
                    // of the encoder's two-phase emit_mb_cabac_p_inter.
                    macro_rules! refidx {
                        ($pi:expr, $zb:expr) => {{
                            if self.num_ref_active > 1 {
                                let s = CACHE30[$pi];
                                let c0 = (refc[s - 1] > 0) as usize + 2 * (refc[s - 6] > 0) as usize;
                                let r = parse_ref_idx_cabac(&mut cab, c0);
                                for &zb in $zb.iter() {
                                    refc[CACHE30[zb]] = r;
                                }
                                r
                            } else {
                                0i8
                            }
                        }};
                    }
                    macro_rules! part {
                        ($pi:expr, $zb:expr, $pred:expr, $rx:expr, $ry:expr, $rw:expr, $rh:expr, $refi:expr) => {{
                            let (mvx, mvy) = parse_mvd_partition(&mut cab, $pi, $zb, &mut mvdc, &mut refc, &mut mmvd, &mut mref, $refi);
                            let [na, nb, nc] = self.mv_neighbors_block(
                                (mbx * 4 + $rx / 4) as isize,
                                (mby * 4 + $ry / 4) as isize,
                                ($rw / 4) as isize,
                            );
                            let pmv = $pred(na, nb, nc);
                            self.commit_inter_grid(mbx, mby, $rx, $ry, $rw, $rh, (pmv.0 + mvx, pmv.1 + mvy), $refi);
                        }};
                    }
                    match mbt {
                        0 => {
                            let r0 = refidx!(0, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
                            part!(0, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], |a, b, c| predict_partition_mv(0, 0, a, b, c, r0 as i32), 0, 0, 16, 16, r0);
                        }
                        1 => {
                            let r0 = refidx!(0, &[0, 1, 2, 3, 4, 5, 6, 7]);
                            let r1 = refidx!(8, &[8, 9, 10, 11, 12, 13, 14, 15]);
                            part!(0, &[0, 1, 2, 3, 4, 5, 6, 7], |a, b, c| predict_partition_mv(1, 0, a, b, c, r0 as i32), 0, 0, 16, 8, r0);
                            part!(8, &[8, 9, 10, 11, 12, 13, 14, 15], |a, b, c| predict_partition_mv(1, 1, a, b, c, r1 as i32), 0, 8, 16, 8, r1);
                        }
                        2 => {
                            let r0 = refidx!(0, &[0, 1, 2, 3, 8, 9, 10, 11]);
                            let r1 = refidx!(4, &[4, 5, 6, 7, 12, 13, 14, 15]);
                            part!(0, &[0, 1, 2, 3, 8, 9, 10, 11], |a, b, c| predict_partition_mv(2, 0, a, b, c, r0 as i32), 0, 0, 8, 16, r0);
                            part!(4, &[4, 5, 6, 7, 12, 13, 14, 15], |a, b, c| predict_partition_mv(2, 1, a, b, c, r1 as i32), 8, 0, 8, 16, r1);
                        }
                        _ => {
                            // P_8x8: 4 sub_mb_types, then 4 ref_idx (one per 8×8), then mvd.
                            let mut subt = [0u32; 4];
                            for st in &mut subt {
                                *st = parse_sub_mb_type_p_cabac(&mut cab);
                            }
                            allow8 = subt.iter().all(|&t| t == 0);
                            let mut pr = [0i8; 4];
                            for (i, r) in pr.iter_mut().enumerate() {
                                let b = i * 4;
                                *r = refidx!(b, &[b, b + 1, b + 2, b + 3]);
                            }
                            for i in 0..4usize {
                                let b = i * 4;
                                let (ox, oy) = ((i % 2) * 8, (i / 2) * 8); // 8×8 pixel origin in MB
                                let ri = pr[i];
                                match subt[i] {
                                    0 => part!(b, &[b, b + 1, b + 2, b + 3], |a, b, c| predict_mv(a, b, c, ri as i32), ox, oy, 8, 8, ri),
                                    1 => {
                                        part!(b, &[b, b + 1], |a, b, c| predict_mv(a, b, c, ri as i32), ox, oy, 8, 4, ri);
                                        part!(b + 2, &[b + 2, b + 3], |a, b, c| predict_mv(a, b, c, ri as i32), ox, oy + 4, 8, 4, ri);
                                    }
                                    2 => {
                                        part!(b, &[b, b + 2], |a, b, c| predict_mv(a, b, c, ri as i32), ox, oy, 4, 8, ri);
                                        part!(b + 1, &[b + 1, b + 3], |a, b, c| predict_mv(a, b, c, ri as i32), ox + 4, oy, 4, 8, ri);
                                    }
                                    _ => {
                                        for j in 0..4usize {
                                            let (sx, sy) = ((j % 2) * 4, (j / 2) * 4);
                                            part!(b + j, &[b + j], |a, b, c| predict_mv(a, b, c, ri as i32), ox + sx, oy + sy, 4, 4, ri);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    mb_ref[addr] = mref;
                    mb_mvd[addr] = mmvd;
                    cat[addr] = 100;

                    // Inter cbp + residual (is_intra = false → cbf default nA=nB=0).
                    let cbp = parse_cbp_cabac(&mut cab, top.map(|a| mb_cbp[a]), left.map(|a| mb_cbp[a]));
                    mb_cbp[addr] = cbp as u8;
                    // H-49: an INTER macroblock carries transform_size_8x8_flag AFTER cbp
                    // (spec 7.3.5), present only when CodedBlockPatternLuma > 0 and
                    // noSubMbPartSizeLessThan8x8Flag. Same context as the intra read.
                    let t8 = self.transform_8x8_mode && (cbp & 15) != 0 && allow8 && {
                        let a = left.map_or(0, |x| self.mb_t8x8[x] as usize);
                        let b = top.map_or(0, |x| self.mb_t8x8[x] as usize);
                        cab.decode_decision(399 + a + b) != 0
                    };
                    self.mb_t8x8[addr] = t8;
                    let mut luma8 = [[0i32; 64]; 4]; // per 8x8 block, 8x8 scan order (t8)
                    let (cbp_luma, cbp_chroma) = (cbp & 15, cbp >> 4);
                    let mut nzc = [0xffu8; 48];
                    if let Some(t) = top {
                        let tnz = mb_nzc[t];
                        nzc[1..5].copy_from_slice(&tnz[12..16]);
                        (nzc[0], nzc[5], nzc[29]) = (0, 0, 0);
                        (nzc[6], nzc[7], nzc[30], nzc[31]) = (tnz[20], tnz[21], tnz[22], tnz[23]);
                    }
                    if let Some(l) = left {
                        let lnz = mb_nzc[l];
                        (nzc[8], nzc[16], nzc[24], nzc[32]) = (lnz[3], lnz[7], lnz[11], lnz[15]);
                        (nzc[13], nzc[21], nzc[37], nzc[45]) = (lnz[17], lnz[21], lnz[19], lnz[23]);
                    }
                    let mut cbfdc = 0u16;
                    let mut nnzs = [0u8; 24]; // parsed totalCoeff per block (see add_inter_residual)
                    let mut luma_scan = [[0i32; 16]; 16]; // per z-order 4×4 block (scan order)
                    let mut cdc = [[0i32; 4]; 2]; // chroma DC per plane (scan order)
                    let mut cac = [[[0i32; 16]; 4]; 2]; // chroma AC per plane, per 4×4 block
                    // A cbp==0 MB codes no mb_qp_delta → the next MB's delta ctxInc sees 0.
                    if cbp == 0 {
                        last_delta_qp = 0;
                    }
                    if cbp != 0 {
                        let ndc = (top.map(|a| cbf_dc[a]), left.map(|a| cbf_dc[a]));
                        let qpd = parse_mb_qp_delta_cabac(&mut cab, &mut last_delta_qp);
                        self.step_qp(qpd);
                        for id8 in 0..4usize {
                            if cbp_luma & (1 << id8) != 0 {
                                if t8 {
                                    // All four slots carry the 8x8 total: cat 5 has no per-4x4
                                    // counts, and the recon helper now reads one slot
                                    // per 4x4 cell.
                                    let n8 = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, id8 * 4, RP_LUMA_8X8, false, ndc, &mut luma8[id8]) as u8;
                                    for k in 0..4 {
                                        nnzs[id8 * 4 + k] = n8;
                                    }
                                } else {
                                    for id4 in 0..4usize {
                                        let iz = id8 * 4 + id4;
                                        nnzs[iz] = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, iz, RP_LUMA_4X4, false, ndc, &mut luma_scan[iz]) as u8;
                                    }
                                }
                            } else {
                                for k in 0..4 {
                                    nzc[NZC_CACHE[id8 * 4 + k]] = 0;
                                }
                            }
                        }
                        if cbp_chroma >= 1 {
                            for i in 0..2usize {
                                parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4, RP_CHROMA_DC + i, false, ndc, &mut cdc[i]);
                            }
                        }
                        if cbp_chroma == 2 {
                            for i in 0..2usize {
                                for id4 in 0..4usize {
                                    nnzs[16 + i * 4 + id4] = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4 + id4, RP_CHROMA_AC + i, false, ndc, &mut cac[i][id4]) as u8;
                                }
                            }
                        }
                    }
                    self.mb_qp[addr] = self.cur_qp;
                    cbf_dc[addr] = cbfdc;
                    let _sc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecStateCache);
                    let mut mn = [0u8; 24];
                    for k in 0..4 {
                        mn[k] = nzc[9 + k];
                        mn[4 + k] = nzc[17 + k];
                        mn[8 + k] = nzc[25 + k];
                        mn[12 + k] = nzc[33 + k];
                    }
                    (mn[16], mn[17], mn[20], mn[21]) = (nzc[14], nzc[15], nzc[22], nzc[23]);
                    (mn[18], mn[19], mn[22], mn[23]) = (nzc[38], nzc[39], nzc[46], nzc[47]);
                    // A block whose residual was skipped (cbp bit clear / no chroma AC)
                    // has 0 coeffs, not "unavailable" — export 0 so an intra neighbour's
                    // CBF ctxInc reads 0 (not the 0xff sentinel → is_intra default).
                    for v in mn.iter_mut() {
                        if *v == 0xff {
                            *v = 0;
                        }
                    }
                    mb_nzc[addr] = mn;
                    drop(_sc);

                    if self.refs.is_empty() {
                        return Err(MbError::Unsupported("inter without reference"));
                    }
                    let (mut jgmv, mut jgref) = ([(0i32, 0i32); 16], [0u8; 16]);
                    {
                        let w4r = self.mb_w * 4;
                        for by in 0..4usize {
                            for bx in 0..4usize {
                                let bidx = (mby * 4 + by) * w4r + (mbx * 4 + bx);
                                jgmv[by * 4 + bx] = self.mv_y[bidx];
                                jgref[by * 4 + bx] =
                                    self.ref_idx_y[bidx].clamp(0, 15) as u8;
                            }
                        }
                    }
                    let job = PInterJob {
                        mbx,
                        mby,
                        t8,
                        qp: self.cur_qp,
                        cbp_chroma,
                        gmv: jgmv,
                        gref: jgref,
                        luma_scan,
                        luma8,
                        cdc,
                        cac,
                        nnzs,
                    };
                    // D9: `cbp == 0` means every coefficient array in `job` is
                    // ZERO — 2,592 of its 2,784 bytes. Ship the motion-only form
                    // instead of allocating, filling, channel-passing and freeing
                    // 2.6 KB of nothing (12.8-37.5% of inter macroblocks on the
                    // x264 corpus). The worker rebuilds the zeroed job and calls
                    // the SAME `recon_p_inter`, so no second implementation
                    // exists to drift.
                    let nores = cbp == 0 && nores_on();
                    if self.edc_tx.is_some() {
                        self.edc_giveback();
                        self.edc_commit_nnz(mbx, mby, t8, &nnzs, cbp_chroma);
                        if edcstat::on() {
                            edcstat::bump(&edcstat::J_INTER, 1);
                            let nores = job.cbp_chroma == 0
                                && job.luma_scan.iter().all(|b| b.iter().all(|&c| c == 0))
                                && job.cdc.iter().all(|p| p.iter().all(|&c| c == 0))
                                && job.cac.iter().all(|p| p.iter().all(|b| b.iter().all(|&c| c == 0)));
                            if nores {
                                edcstat::bump(&edcstat::J_INTER_NORES, 1);
                            }
                        }
                        let job_msg = if nores {
                            edcstat::bump(&edcstat::J_NORES_SENT, 1);
                            EdcJob::InterNoRes(Box::new(PInterNoResJob {
                                mbx: job.mbx,
                                mby: job.mby,
                                t8: job.t8,
                                qp: job.qp,
                                gmv: job.gmv,
                                gref: job.gref,
                            }))
                        } else {
                            EdcJob::Inter(Box::new(job))
                        };
                        self.edc_send_job(job_msg);
                    } else if self.edc_active {
                        if nores {
                            edcstat::bump(&edcstat::J_NORES_SENT, 1);
                            self.edc_jobs.push(EdcJob::InterNoRes(Box::new(PInterNoResJob {
                                mbx: job.mbx,
                                mby: job.mby,
                                t8: job.t8,
                                qp: job.qp,
                                gmv: job.gmv,
                                gref: job.gref,
                            })));
                        } else {
                            self.edc_jobs.push(EdcJob::Inter(Box::new(job)));
                        }
                    } else {
                        self.recon_p_inter(&job);
                        if double_recon() {
                            self.recon_p_inter(&job);
                        }
                    }

                    let eos = cab.decode_terminate();
                    addr += 1;
                    if eos || addr >= total {
                        break;
                    }
                    continue;
                }
                mb_type = mbt - 5; // 5→0 (I_4x4), 6..29→1..24 (I_16x16)
            } else if self.is_b {
                self.edc_flush(); // (E1 single-thread mode: B stays inline)
                if self.edc_tx.is_some() {
                    // E3: this B macroblock's MC regions record instead of executing.
                    self.edc_regions = Some(Vec::with_capacity(8));
                }
                let _gb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMbB);
                // noSubMbPartSizeLessThan8x8Flag for B: direct MBs qualify only under
                // direct_8x8_inference_flag; B_8x8 needs every sub-partition 8x8.
                let mut allow8 = true;
                // B-slice: mb_skip_flag (ctx 24 + neighbour-not-skip), then B mb_type.
                let sctx = 24
                    + left.map_or(0, |a| (!mb_skip[a]) as usize)
                    + top.map_or(0, |a| (!mb_skip[a]) as usize);
                if parse_mb_skip_cabac(&mut cab, sctx) {
                    mb_skip[addr] = true;
                    cat[addr] = 100;
                    mb_direct[addr] = true;
                    last_delta_qp = 0; // skip codes no mb_qp_delta → delta ctxInc resets
                    // B_Skip recon reuses the entropy-free CAVLC primitive (spatial/temporal
                    // direct with no residual), which also commits the motion grid.
                    self.decode_b_skip(mbx, mby)?;
                    self.mb_qp[addr] = self.cur_qp;
                    // Skip/direct blocks contribute mvd 0 to a later MB's mvd ctxInc; the
                    // ref stays in-list so |mvd|=0 is summed (same result either way).
                    mb_ref[addr] = [0i8; 16];
                    mb_ref1[addr] = [0i8; 16];
                    let eos = cab.decode_terminate();
                    addr += 1;
                    if eos || addr >= total {
                        break;
                    }
                    continue;
                }
                let bci = left.map_or(0, |a| (!mb_direct[a]) as usize)
                    + top.map_or(0, |a| (!mb_direct[a]) as usize);
                let bmt = parse_mb_type_b_cabac(&mut cab, bci);
                if bmt < 23 {
                    // ---- B inter: parse motion (mvd L0/L1; ref not coded on this 1-ref
                    // stream) + residual. Recon (b_mc/direct) deferred to B.3. ----
                    let mut mvdc0 = [[0i16; 2]; 30];
                    let mut refc0 = [-1i8; 30];
                    let mut mvdc1 = [[0i16; 2]; 30];
                    let mut refc1 = [-1i8; 30];
                    // WelsFillCacheInterCabac, per list (L0 = mb_ref/mb_mvd, L1 = mb_ref1/mb_mvd1).
                    macro_rules! fill {
                        ($mrf:expr, $mmv:expr, $rc:expr, $mc:expr) => {{
                            if let Some(l) = left {
                                for (ci, bi) in [(6usize, 3usize), (12, 7), (18, 11), (24, 15)] {
                                    $rc[ci] = $mrf[l][bi];
                                    $mc[ci] = $mmv[l][bi];
                                }
                            }
                            if let Some(t) = top {
                                for (ci, bi) in [(1usize, 12usize), (2, 13), (3, 14), (4, 15)] {
                                    $rc[ci] = $mrf[t][bi];
                                    $mc[ci] = $mmv[t][bi];
                                }
                            }
                            if mbx > 0 && mby > 0 {
                                let a = addr - mbw - 1;
                                ($rc[0], $mc[0]) = ($mrf[a][15], $mmv[a][15]);
                            }
                            if mby > 0 && mbx + 1 < mbw {
                                let a = addr - mbw + 1;
                                ($rc[5], $mc[5]) = ($mrf[a][12], $mmv[a][12]);
                            }
                        }};
                    }
                    fill!(mb_ref, mb_mvd, refc0, mvdc0);
                    fill!(mb_ref1, mb_mvd1, refc1, mvdc1);
                    let mut mmvd0 = [[0i16; 2]; 16];
                    let mut mref0 = [-1i8; 16];
                    let mut mmvd1 = [[0i16; 2]; 16];
                    let mut mref1 = [-1i8; 16];
                    if self.refs.is_empty() || self.refs1.is_empty() {
                        return Err(MbError::Unsupported("B without references"));
                    }
                    // Recon (mirrors CAVLC decode_b_mb / decode_b_8x8): predict each list's
                    // MV off the committed grid + the CABAC-parsed mvd, commit, MC (bi-pred
                    // blend), then add the residual. Prediction reads mmvd0/mmvd1 (the mvd
                    // per raster block, splatted during the parse above).
                    let mut pred_y = [0u8; 256];
                    let mut c_pred = [[0u8; 64]; 2];

                    if bmt == 0 {
                        // B_Direct_16x16: no coded motion. A direct block contributes mvd 0
                        // to a later MB's mvd ctxInc with its ref in-list (|0| summed).
                        mb_direct[addr] = true;
                        allow8 = self.direct_8x8_inference;
                        (mref0, mref1) = ([0i8; 16], [0i8; 16]);
                        self.decode_b_direct(mbx, mby, 0, 0, 16, 16, &mut pred_y, &mut c_pred);
                    } else if bmt == 22 {
                        // B_8x8: 4 sub_mb_types, (ref not coded on 1-ref), then mvd
                        // list-major → sub-MB → sub-partition (openh264 order).
                        let mut subt = [0u32; 4];
                        for s in &mut subt {
                            *s = parse_sub_mb_type_b_cabac(&mut cab);
                        }
                        allow8 = subt.iter().all(|&t| if t == 0 { self.direct_8x8_inference } else { (1..=3).contains(&t) });
                        // A direct sub-partition contributes mvd 0 / ref in-list to the
                        // ctxInc — both the per-MB export and the within-MB 30-cache that a
                        // later (non-direct) sub in this MB reads.
                        for i in 0..4usize {
                            if subt[i] == 0 {
                                let b = i * 4;
                                for &zb in &[b, b + 1, b + 2, b + 3] {
                                    (mref0[G_SCAN4[zb]], mref1[G_SCAN4[zb]]) = (0, 0);
                                    (refc0[CACHE30[zb]], refc1[CACHE30[zb]]) = (0, 0);
                                }
                            }
                        }
                        // ref_idx_l0 for all four 8x8s, then ref_idx_l1, then the mvds
                        // (spec 7.3.5.2 sub_mb_pred). ONE ref per 8x8 -- never per
                        // sub-partition -- and B_Direct_8x8 codes none.
                        let mut sref = [[0i8; 2]; 4]; // [sub-MB][list]
                        for list in 0..2usize {
                            let active = if list == 0 { self.num_ref_active } else { self.num_ref_active1 };
                            if active <= 1 {
                                continue;
                            }
                            let rc = if list == 0 { &mut refc0 } else { &mut refc1 };
                            for i in 0..4usize {
                                let st = subt[i];
                                if st == 0 || !b_sub_uses(st, list) {
                                    continue;
                                }
                                let b = i * 4;
                                let s = CACHE30[b];
                                let c0 = (rc[s - 1] > 0) as usize + 2 * (rc[s - 6] > 0) as usize;
                                let r = parse_ref_idx_cabac(&mut cab, c0);
                                for &zb in &[b, b + 1, b + 2, b + 3] {
                                    rc[CACHE30[zb]] = r;
                                }
                                sref[i][list] = r;
                            }
                        }
                        for list in 0..2usize {
                            let (mmv, mrf, mc, rc) = if list == 0 {
                                (&mut mmvd0, &mut mref0, &mut mvdc0, &mut refc0)
                            } else {
                                (&mut mmvd1, &mut mref1, &mut mvdc1, &mut refc1)
                            };
                            for i in 0..4usize {
                                let st = subt[i];
                                if st == 0 || !b_sub_uses(st, list) {
                                    continue;
                                }
                                let b = i * 4;
                                for &(sx, sy, sw, sh) in b_sub_parts(st) {
                                    let mut zb = [0usize; 4];
                                    let mut n = 0;
                                    for ly in sy / 4..sy / 4 + sh / 4 {
                                        for lx in sx / 4..sx / 4 + sw / 4 {
                                            zb[n] = b + ly * 2 + lx;
                                            n += 1;
                                        }
                                    }
                                    parse_mvd_partition(&mut cab, zb[0], &zb[..n], mc, rc, mmv, mrf, sref[i][list]);
                                }
                            }
                        }
                        // Recon each 8×8: direct sub → decode_b_direct; else per sub-part
                        // predict (median) + commit + MC.
                        for (p, &st) in subt.iter().enumerate() {
                            let (b8x, b8y) = ((p % 2) * 8, (p / 2) * 8);
                            if st == 0 {
                                self.decode_b_direct(mbx, mby, b8x, b8y, 8, 8, &mut pred_y, &mut c_pred);
                                continue;
                            }
                            for &(sx, sy, sw, sh) in b_sub_parts(st) {
                                let (px, py) = (b8x + sx, b8y + sy);
                                let mut mv = [(0i32, 0i32); 2];
                                for list in 0..2usize {
                                    if b_sub_uses(st, list) {
                                        let d = if list == 0 { mmvd0 } else { mmvd1 }[(py / 4) * 4 + px / 4];
                                        let n = self.mv_neighbors_list((mbx * 4 + px / 4) as isize, (mby * 4 + py / 4) as isize, (sw / 4) as isize, list);
                                        let pmv = predict_mv(n[0], n[1], n[2], sref[p][list] as i32);
                                        mv[list] = (pmv.0 + d[0] as i32, pmv.1 + d[1] as i32);
                                    }
                                }
                                let refi0 = if b_sub_uses(st, 0) { sref[p][0] as i32 } else { -1 };
                                let refi1 = if b_sub_uses(st, 1) { sref[p][1] as i32 } else { -1 };
                                self.b_set_motion(mbx, mby, px, py, sw, sh, refi0, mv[0], refi1, mv[1]);
                                self.b_mc_or_record(mbx, mby, px, py, sw, sh, refi0, mv[0], refi1, mv[1], &mut pred_y, &mut c_pred);
                            }
                        }
                    } else {
                        let (layout, mvmode, preds) = b_inter_layout(bmt);
                        let parts: &[(usize, &[usize])] = match mvmode {
                            0 => &[(0, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])],
                            1 => &[(0, &[0, 1, 2, 3, 4, 5, 6, 7]), (8, &[8, 9, 10, 11, 12, 13, 14, 15])],
                            _ => &[(0, &[0, 1, 2, 3, 8, 9, 10, 11]), (4, &[4, 5, 6, 7, 12, 13, 14, 15])],
                        };
                        // ref_idx_l0 for EVERY partition, then ref_idx_l1, then the mvds
                        // (spec 7.3.5.1 macroblock_prediction). This was missing entirely
                        // -- the B path assumed a single reference -- so any B slice with
                        // more than one active reference in either list desynced the
                        // arithmetic decoder at the first partition that codes a ref_idx,
                        // and the slice ended early at a phantom end_of_slice_flag.
                        let mut pref = [[0i8; 2]; 2]; // [partition][list]
                        for list in 0..2usize {
                            let active = if list == 0 { self.num_ref_active } else { self.num_ref_active1 };
                            if active <= 1 {
                                continue;
                            }
                            let rc = if list == 0 { &mut refc0 } else { &mut refc1 };
                            for (p, &(pidx, zb)) in parts.iter().enumerate() {
                                if !preds[p].uses(list) {
                                    continue;
                                }
                                let s = CACHE30[pidx];
                                let c0 = (rc[s - 1] > 0) as usize + 2 * (rc[s - 6] > 0) as usize;
                                let r = parse_ref_idx_cabac(&mut cab, c0);
                                // Seed the cache so a later partition's ref/mvd ctxInc sees it.
                                for &zbi in zb.iter() {
                                    rc[CACHE30[zbi]] = r;
                                }
                                pref[p][list] = r;
                            }
                        }
                        // mvd parse order: list-major, partition-minor (openh264
                        // ParseInterBMotionInfoCabac); the ctxInc reads the same-list cache.
                        for list in 0..2usize {
                            let (mmv, mrf, mc, rc) = if list == 0 {
                                (&mut mmvd0, &mut mref0, &mut mvdc0, &mut refc0)
                            } else {
                                (&mut mmvd1, &mut mref1, &mut mvdc1, &mut refc1)
                            };
                            for (p, &(pidx, zb)) in parts.iter().enumerate() {
                                if preds[p].uses(list) {
                                    parse_mvd_partition(&mut cab, pidx, zb, mc, rc, mmv, mrf, pref[p][list]);
                                }
                            }
                        }
                        // Per-partition recon: predict each list's MV, commit, MC.
                        for (p, &(rx, ry, rw, rh)) in layout.iter().enumerate() {
                            let mut mv = [(0i32, 0i32); 2];
                            for list in 0..2usize {
                                if preds[p].uses(list) {
                                    let d = if list == 0 { mmvd0 } else { mmvd1 }[(ry / 4) * 4 + rx / 4];
                                    let n = self.mv_neighbors_list((mbx * 4 + rx / 4) as isize, (mby * 4 + ry / 4) as isize, (rw / 4) as isize, list);
                                    let pmv = predict_partition_mv(mvmode, p, n[0], n[1], n[2], pref[p][list] as i32);
                                    mv[list] = (pmv.0 + d[0] as i32, pmv.1 + d[1] as i32);
                                }
                            }
                            let refi0 = if preds[p].uses(0) { pref[p][0] as i32 } else { -1 };
                            let refi1 = if preds[p].uses(1) { pref[p][1] as i32 } else { -1 };
                            self.b_set_motion(mbx, mby, rx, ry, rw, rh, refi0, mv[0], refi1, mv[1]);
                            // Proper spec bi-prediction (average of L0+L1). NOTE: the CAVLC
                            // decode_b_mb replicates an openh264 bug here for a Bi 16×8/8×16
                            // partition; our pixel gate is ffmpeg (spec-correct), so we do NOT.
                            self.b_mc_or_record(mbx, mby, rx, ry, rw, rh, refi0, mv[0], refi1, mv[1], &mut pred_y, &mut c_pred);
                        }
                    }
                    mb_ref[addr] = mref0;
                    mb_mvd[addr] = mmvd0;
                    mb_ref1[addr] = mref1;
                    mb_mvd1[addr] = mmvd1;
                    cat[addr] = 100;

                    // Inter cbp + residual (identical to the P path).
                    let cbp = parse_cbp_cabac(&mut cab, top.map(|a| mb_cbp[a]), left.map(|a| mb_cbp[a]));
                    mb_cbp[addr] = cbp as u8;
                    // H-49: an INTER macroblock carries transform_size_8x8_flag AFTER cbp
                    // (spec 7.3.5), present only when CodedBlockPatternLuma > 0 and
                    // noSubMbPartSizeLessThan8x8Flag. Same context as the intra read.
                    let t8 = self.transform_8x8_mode && (cbp & 15) != 0 && allow8 && {
                        let a = left.map_or(0, |x| self.mb_t8x8[x] as usize);
                        let b = top.map_or(0, |x| self.mb_t8x8[x] as usize);
                        cab.decode_decision(399 + a + b) != 0
                    };
                    self.mb_t8x8[addr] = t8;
                    let mut luma8 = [[0i32; 64]; 4]; // per 8x8 block, 8x8 scan order (t8)
                    let (cbp_luma, cbp_chroma) = (cbp & 15, cbp >> 4);
                    let mut nzc = [0xffu8; 48];
                    if let Some(t) = top {
                        let tnz = mb_nzc[t];
                        nzc[1..5].copy_from_slice(&tnz[12..16]);
                        (nzc[0], nzc[5], nzc[29]) = (0, 0, 0);
                        (nzc[6], nzc[7], nzc[30], nzc[31]) = (tnz[20], tnz[21], tnz[22], tnz[23]);
                    }
                    if let Some(l) = left {
                        let lnz = mb_nzc[l];
                        (nzc[8], nzc[16], nzc[24], nzc[32]) = (lnz[3], lnz[7], lnz[11], lnz[15]);
                        (nzc[13], nzc[21], nzc[37], nzc[45]) = (lnz[17], lnz[21], lnz[19], lnz[23]);
                    }
                    let mut cbfdc = 0u16;
                    let mut nnzs = [0u8; 24]; // parsed totalCoeff per block
                    let mut luma_scan = [[0i32; 16]; 16];
                    let mut cdc = [[0i32; 4]; 2];
                    let mut cac = [[[0i32; 16]; 4]; 2];
                    if cbp == 0 {
                        last_delta_qp = 0;
                    }
                    if cbp != 0 {
                        let ndc = (top.map(|a| cbf_dc[a]), left.map(|a| cbf_dc[a]));
                        let qpd = parse_mb_qp_delta_cabac(&mut cab, &mut last_delta_qp);
                        self.step_qp(qpd);
                        for id8 in 0..4usize {
                            if cbp_luma & (1 << id8) != 0 {
                                if t8 {
                                    // All four slots carry the 8x8 total: cat 5 has no per-4x4
                                    // counts, and the recon helper now reads one slot
                                    // per 4x4 cell.
                                    let n8 = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, id8 * 4, RP_LUMA_8X8, false, ndc, &mut luma8[id8]) as u8;
                                    for k in 0..4 {
                                        nnzs[id8 * 4 + k] = n8;
                                    }
                                } else {
                                    for id4 in 0..4usize {
                                        let iz = id8 * 4 + id4;
                                        nnzs[iz] = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, iz, RP_LUMA_4X4, false, ndc, &mut luma_scan[iz]) as u8;
                                    }
                                }
                            } else {
                                for k in 0..4 {
                                    nzc[NZC_CACHE[id8 * 4 + k]] = 0;
                                }
                            }
                        }
                        if cbp_chroma >= 1 {
                            for i in 0..2usize {
                                parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4, RP_CHROMA_DC + i, false, ndc, &mut cdc[i]);
                            }
                        }
                        if cbp_chroma == 2 {
                            for i in 0..2usize {
                                for id4 in 0..4usize {
                                    nnzs[16 + i * 4 + id4] = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4 + id4, RP_CHROMA_AC + i, false, ndc, &mut cac[i][id4]) as u8;
                                }
                            }
                        }
                    }
                    self.mb_qp[addr] = self.cur_qp;
                    cbf_dc[addr] = cbfdc;
                    let _sc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecStateCache);
                    let mut mn = [0u8; 24];
                    for k in 0..4 {
                        mn[k] = nzc[9 + k];
                        mn[4 + k] = nzc[17 + k];
                        mn[8 + k] = nzc[25 + k];
                        mn[12 + k] = nzc[33 + k];
                    }
                    (mn[16], mn[17], mn[20], mn[21]) = (nzc[14], nzc[15], nzc[22], nzc[23]);
                    (mn[18], mn[19], mn[22], mn[23]) = (nzc[38], nzc[39], nzc[46], nzc[47]);
                    // A block whose residual was skipped (cbp bit clear / no chroma AC)
                    // has 0 coeffs, not "unavailable" — export 0 so an intra neighbour's
                    // CBF ctxInc reads 0 (not the 0xff sentinel → is_intra default).
                    for v in mn.iter_mut() {
                        if *v == 0xff {
                            *v = 0;
                        }
                    }
                    mb_nzc[addr] = mn;
                    drop(_sc);
                    if let Some(regions) = self.edc_regions.take() {
                        self.edc_giveback();
                        self.edc_commit_nnz(mbx, mby, t8, &nnzs, cbp_chroma);
                        let job = BJob {
                            mbx,
                            mby,
                            t8,
                            qp: self.cur_qp,
                            cbp_chroma,
                            skip: false,
                            regions,
                            luma_scan,
                            luma8,
                            cdc,
                            cac,
                            nnzs,
                        };
                        self.edc_send_job(EdcJob::B(Box::new(job)));
                    } else {
                        self.add_inter_residual(mbx, mby, &pred_y, &c_pred, &luma_scan, if t8 { Some(&luma8) } else { None }, &cdc, &cac, cbp_chroma, &nnzs);
                    }

                    let eos = cab.decode_terminate();
                    addr += 1;
                    if eos || addr >= total {
                        break;
                    }
                    continue;
                }
                mb_type = bmt - 23; // 23→0 (I_4x4), 24..=47→1..24 (I_16x16), 48→25 (PCM)
                if mb_type == 25 {
                    return Err(MbError::Unsupported("CABAC I_PCM (WIP)"));
                }
            } else {
                let li = left.map_or(0, |a| (cat[a] >= 2) as usize);
                let ti = top.map_or(0, |a| (cat[a] >= 2) as usize);
                mb_type = parse_mb_type_i_cabac(&mut cab, li + ti);
                if mb_type == 25 {
                    return Err(MbError::Unsupported("CABAC I_PCM (WIP)"));
                }
            }
            // H-48: the CABAC intra path is INLINED in this loop, not routed through
            // `decode_intra_mb` (which only the CAVLC readers call) — wiring the scope
            // there reported ZERO calls against 480,510 intra-pred calls. All three
            // intra entries (I-slice, P-slice mb_type>3, B-slice bmt>=23) converge
            // here, so this is the one point that sees every intra MB.
            self.edc_intra_sync(); // intra reconstruction reads neighbour PIXELS
            let _gi = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMbI);
            // chroma-pred-mode ctxInc from neighbour chroma modes (1..=3).
            let cci = left.map_or(0, |a| (1..=3).contains(&cmode[a]) as usize)
                + top.map_or(0, |a| (1..=3).contains(&cmode[a]) as usize);

            if mb_type != 0 {
                // ---- I_16x16 (mb_type 1..=24): pred mode & cbp DERIVED from mb_type;
                // luma DC always coded. Syntax order: intra_chroma_pred_mode, mb_qp_delta,
                // luma DC (Hadamard), luma AC (if cbp_luma), chroma DC/AC. Mirrors the CAVLC
                // decode_i16, driven by the CABAC residual. ----
                let mt = mb_type - 1;
                let pred_mode = I16Mode::from_id(mt % 4);
                let cbp_chroma = (mt % 12) / 4;
                let cbp_luma_15 = mt / 12 == 1;
                let chroma_mode = parse_intra_chroma_pred_mode_cabac(&mut cab, cci) as u8;
                cmode[addr] = chroma_mode as i32;
                cat[addr] = 2;
                mb_cbp[addr] = ((cbp_chroma as u8) << 4) | if cbp_luma_15 { 15 } else { 0 };
                let w4 = self.mb_w * 4;

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

                let ndc = (top.map(|a| cbf_dc[a]), left.map(|a| cbf_dc[a]));
                let qpd = parse_mb_qp_delta_cabac(&mut cab, &mut last_delta_qp);
                self.step_qp(qpd);
                let qp = self.cur_qp;
                let mut cbfdc = 0u16;

                // Luma DC (iz=0, category I16_LUMA_DC, 16 coeffs) → Hadamard dequant.
                let mut dc_scan = [0i32; 16];
                parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 0, RP_I16_DC, true, ndc, &mut dc_scan);
                let recon_dc = self.dequant_luma_dc(&un_scan_4x4_dcac(&dc_scan), qp, 0);

                // Luma AC (iz 0..15, category I16_LUMA_AC, 15 coeffs) when cbp_luma set.
                let mut q_blocks = [[0i32; 16]; 16];
                for (iz, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                    let total = if cbp_luma_15 {
                        let mut ac = [0i32; 16];
                        let t = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, iz, RP_I16_AC, true, ndc, &mut ac);
                        un_scan_4x4_ac_into(&ac, &mut q_blocks[lby * 4 + lbx]);
                        t as u8
                    } else {
                        nzc[NZC_CACHE[iz]] = 0;
                        0
                    };
                    self.nnz_y[(mby * 4 + lby) * w4 + (mbx * 4 + lbx)] = total;
                }

                let mut cdc = [[0i32; 4]; 2];
                let mut cac = [[[0i32; 16]; 4]; 2];
                if cbp_chroma >= 1 {
                    for i in 0..2usize {
                        parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4, RP_CHROMA_DC + i, true, ndc, &mut cdc[i]);
                    }
                }
                if cbp_chroma == 2 {
                    for i in 0..2usize {
                        for id4 in 0..4usize {
                            parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4 + id4, RP_CHROMA_AC + i, true, ndc, &mut cac[i][id4]);
                        }
                    }
                }

                // Luma recon: 16×16 intra prediction, then per-4×4 (dequant AC + injected DC).
                let top_ok = mby > 0 && self.nbr_in_slice(mbx, mby - 1) && self.intra_nbr_ok(mbx * 4, mby * 4 - 1);
                let left_ok = mbx > 0 && self.nbr_in_slice(mbx - 1, mby) && self.intra_nbr_ok(mbx * 4 - 1, mby * 4);
                let (lx, ly) = (mbx * 16, mby * 16);
                let mut t16 = [0u8; 16];
                let mut l16 = [0u8; 16];
                if top_ok {
                    t16.copy_from_slice(self.top_y_row(ly, lx, 16));
                }
                if left_ok {
                    for i in 0..16 {
                        l16[i] = self.rec_y[(ly + i) * self.cw + lx - 1];
                    }
                }
                let corner = if top_ok && left_ok { self.top_y_px(ly, lx - 1) } else { 0 };
                let pred_l = luma16x16_pred(pred_mode, top_ok, left_ok, &t16, &l16, corner);
                for by in 0..4 {
                    for bx in 0..4 {
                        let mut deq = self.dequant(&q_blocks[by * 4 + bx], qp, 0);
                        deq[0] = recon_dc[by * 4 + bx];
                        let predb: [i32; 16] = std::array::from_fn(|i| pred_l[(by * 4 + i / 4) * 16 + (bx * 4 + i % 4)] as i32);
                        let s = reconstruct_4x4(&deq, &predb);
                        store(&mut self.rec_y, self.cw, lx + bx * 4, ly + by * 4, &s);
                        // I_16x16 blocks predict as DC for neighbour mode-prediction, and
                        // must be marked coded so a later I_4x4 MB's top-right availability
                        // (gather_i4 reads coded_y) sees this block as present.
                        self.modes_y[(mby * 4 + by) * w4 + (mbx * 4 + bx)] = 2;
                        self.coded_y[(mby * 4 + by) * w4 + (mbx * 4 + bx)] = true;
                    }
                }
                self.recon_chroma_cabac(mbx, mby, chroma_mode, &cdc, &cac, cbp_chroma, top_ok, left_ok);

                self.mb_qp[addr] = self.cur_qp;
                cbf_dc[addr] = cbfdc;
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
                mb_nzc[addr] = mn;

                let eos = cab.decode_terminate();
                addr += 1;
                if eos || addr >= total {
                    break;
                }
                continue;
            }
            cat[addr] = 0;
            let w4 = self.mb_w * 4;
            // H-49: transform_size_8x8_flag. For I_NxN it precedes the intra pred
            // modes (spec §7.3.5); ctxIdx = 399 + condTermFlagA + condTermFlagB,
            // each 1 when that neighbour MB carries the flag. Omitting this read is
            // what desynced every High-profile stream.
            let t8 = self.transform_8x8_mode && {
                let a = left.map_or(0, |x| self.mb_t8x8[x] as usize);
                let b = top.map_or(0, |x| self.mb_t8x8[x] as usize);
                cab.decode_decision(399 + a + b) != 0
            };
            self.mb_t8x8[addr] = t8;
            // Brick 2.4 + recon: derive & store each intra mode (prev-flag → the
            // neighbour-predicted mode, else rem), exactly as the CAVLC path.
            let mut modes = [2u8; 16]; // raster [lby*4+lbx]
            let mut modes8 = [2u8; 4]; // one per 8×8 when t8
            if t8 {
                // One mode per 8×8, broadcast to its four 4×4 cells so neighbour
                // mode prediction keeps working unchanged.
                for b8 in 0..4usize {
                    let (b8x, b8y) = (b8 % 2, b8 / 2);
                    let (bx, by) = (mbx * 4 + b8x * 2, mby * 4 + b8y * 2);
                    let predicted = self.predict_i4_mode(bx, by);
                    let rr = parse_intra4x4_pred_mode_cabac(&mut cab);
                    let actual = if rr < 0 {
                        predicted
                    } else {
                        let rem = rr as u8;
                        if rem < predicted { rem } else { rem + 1 }
                    };
                    modes8[b8] = actual;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            self.modes_y[(by + dy) * w4 + (bx + dx)] = actual;
                            modes[(b8y * 2 + dy) * 4 + (b8x * 2 + dx)] = actual;
                        }
                    }
                }
            } else {
                for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                    let (bx, by) = (mbx * 4 + lbx, mby * 4 + lby);
                    let predicted = self.predict_i4_mode(bx, by);
                    let rr = parse_intra4x4_pred_mode_cabac(&mut cab);
                    let actual = if rr < 0 {
                        predicted
                    } else {
                        let rem = rr as u8;
                        if rem < predicted { rem } else { rem + 1 }
                    };
                    self.modes_y[by * w4 + bx] = actual;
                    modes[lby * 4 + lbx] = actual;
                }
            }
            let chroma_mode = parse_intra_chroma_pred_mode_cabac(&mut cab, cci) as u8;
            cmode[addr] = chroma_mode as i32;
            let cbp = parse_cbp_cabac(&mut cab, top.map(|a| mb_cbp[a]), left.map(|a| mb_cbp[a]));
            mb_cbp[addr] = cbp as u8;
            let (cbp_luma, cbp_chroma) = (cbp & 15, cbp >> 4);

            // Build the padded nzc cache from neighbours (openh264 WelsFillCacheNonZeroCount).
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

            // Bricks 2.6 + 2.7: mb_qp_delta + residual (I_4x4 luma 4×4 + chroma DC/AC),
            // storing scan-order coefficients for recon.
            let mut cbfdc = 0u16;
            let mut luma_scan = [[0i32; 16]; 16]; // per z-order 4×4 block
            let mut luma8 = [[0i32; 64]; 4]; // per 8×8 block, 8×8 scan order (t8)
            let mut cdc = [[0i32; 4]; 2]; // chroma DC per plane
            let mut cac = [[[0i32; 16]; 4]; 2]; // chroma AC per plane, per 4×4 block
            if cbp == 0 {
                last_delta_qp = 0;
            }
            if cbp != 0 {
                let ndc = (top.map(|a| cbf_dc[a]), left.map(|a| cbf_dc[a]));
                let qpd = parse_mb_qp_delta_cabac(&mut cab, &mut last_delta_qp);
                self.step_qp(qpd);
                for id8 in 0..4usize {
                    if cbp_luma & (1 << id8) != 0 {
                        if t8 {
                            // ctxBlockCat 5: ONE 64-coefficient block per 8×8, and no
                            // coded_block_flag — presence comes from cbp_luma alone.
                            let n = parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, id8 * 4, RP_LUMA_8X8, true, ndc, &mut luma8[id8]);
                            let (b8x, b8y) = (id8 % 2, id8 / 2);
                            for sy in 0..2 {
                                for sx in 0..2 {
                                    self.nnz_y[(mby * 4 + b8y * 2 + sy) * w4 + (mbx * 4 + b8x * 2 + sx)] = n as u8;
                                }
                            }
                        } else {
                            for id4 in 0..4usize {
                                let iz = id8 * 4 + id4;
                                parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, iz, RP_LUMA_4X4, true, ndc, &mut luma_scan[iz]);
                            }
                        }
                    } else {
                        for k in 0..4 {
                            nzc[NZC_CACHE[id8 * 4 + k]] = 0;
                        }
                        if t8 {
                            let (b8x, b8y) = (id8 % 2, id8 / 2);
                            for sy in 0..2 {
                                for sx in 0..2 {
                                    self.nnz_y[(mby * 4 + b8y * 2 + sy) * w4 + (mbx * 4 + b8x * 2 + sx)] = 0;
                                }
                            }
                        }
                    }
                }
                if cbp_chroma >= 1 {
                    for i in 0..2usize {
                        parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4, RP_CHROMA_DC + i, true, ndc, &mut cdc[i]);
                    }
                }
                if cbp_chroma == 2 {
                    for i in 0..2usize {
                        for id4 in 0..4usize {
                            parse_residual_cabac(&mut cab, &mut nzc, &mut cbfdc, 16 + i * 4 + id4, RP_CHROMA_AC + i, true, ndc, &mut cac[i][id4]);
                        }
                    }
                }
            }
            self.mb_qp[addr] = self.cur_qp;
            cbf_dc[addr] = cbfdc;
            // Extract the MB's nzc (raster luma + chroma) for future neighbours.
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
            mb_nzc[addr] = mn;

            // ---- Brick 4.3a: recon (I_4x4 luma + chroma) via the CAVLC-proven primitives.
            let qp = self.cur_qp;
            let top_ok = mby > 0 && self.nbr_in_slice(mbx, mby - 1) && self.intra_nbr_ok(mbx * 4, mby * 4 - 1);
            let left_ok = mbx > 0 && self.nbr_in_slice(mbx - 1, mby) && self.intra_nbr_ok(mbx * 4 - 1, mby * 4);
            if t8 {
                // I_8x8 recon, reusing the CAVLC-proven primitives verbatim
                // (un_scan_8x8 / inv_quant8 / gather_i8 / intra8x8_pred /
                // add_residual_8x8). Only the ENTROPY half differed.
                for b8 in 0..4usize {
                    let (b8x, b8y) = (b8 % 2, b8 / 2);
                    let (bx, by) = (mbx * 4 + b8x * 2, mby * 4 + b8y * 2);
                    let (px, py) = (bx * 4, by * 4);
                    let res8 = if cbp_luma & (1 << b8) != 0 {
                        let raster = un_scan_8x8(&luma8[b8]);
                        self.inv_quant8(&raster, qp, 0)
                    } else {
                        [0i32; 64]
                    };
                    let avail_top = b8y > 0 || top_ok;
                    let avail_left = b8x > 0 || left_ok;
                    let (t, l, corner, avail_corner) =
                        self.gather_i8(px, py, avail_top, avail_left, bx, by);
                    let pred =
                        intra8x8_pred(modes8[b8], avail_top, avail_left, avail_corner, &t, &l, corner);
                    let mut predb = [0i32; 64];
                    for i in 0..64 {
                        predb[i] = pred[i] as i32;
                    }
                    let recon = add_residual_8x8(&res8, &predb);
                    for dy in 0..8 {
                        for dx in 0..8 {
                            self.rec_y[(py + dy) * self.cw + (px + dx)] = recon[dy * 8 + dx];
                        }
                    }
                    for sy in 0..2 {
                        for sx in 0..2 {
                            self.coded_y[(by + sy) * w4 + (bx + sx)] = true;
                        }
                    }
                }
            }
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                if t8 {
                    break;
                }
                let (bx, by) = (mbx * 4 + lbx, mby * 4 + lby);
                let (px, py) = (bx * 4, by * 4);
                let at = lby > 0 || top_ok;
                let al = lbx > 0 || left_ok;
                let qb = un_scan_4x4_dcac(&luma_scan[blk]);
                self.nnz_y[by * w4 + bx] = luma_scan[blk].iter().filter(|&&v| v != 0).count() as u8;
                let (t, l, corner) = self.gather_i4(px, py, at, al, bx, by);
                let pred = intra4x4_pred(modes[lby * 4 + lbx], at, al, &t, &l, corner);
                let predb = std::array::from_fn(|i| pred[i] as i32);
                let s = reconstruct_4x4(&self.dequant(&qb, qp, 0), &predb);
                store(&mut self.rec_y, self.cw, px, py, &s);
                self.coded_y[by * w4 + bx] = true;
            }
            self.recon_chroma_cabac(mbx, mby, chroma_mode, &cdc, &cac, cbp_chroma, top_ok, left_ok);

            // Brick 2.1: end_of_slice_flag.
            let eos = cab.decode_terminate();
            addr += 1;
            if eos || addr >= total {
                break;
            }
        }
        if trace {
            eprintln!("# CABAC decoded {} MBs (of {total})", addr - first_mb);
        }
        self.edc_flush(); // slice end: no job crosses a slice boundary
        Ok(addr)
    }

    /// CABAC chroma recon (mirrors `decode_chroma`'s reconstruction, driven by the
    /// CABAC-parsed DC/AC coefficients). `cdc[c]` = 2×2 DC (scan order); `cac[c][blk]`
    /// = 15 AC per 4×4 block (scan order).
    #[allow(clippy::too_many_arguments)]
    /// Add a CABAC-parsed inter residual to an already-built motion-comp prediction
    /// (`pred_y`/`c_pred`), writing the reconstruction. Shared by the P and B inter
    /// paths — same `reconstruct_4x4` as intra, MC output as the prediction, inter
    /// scaling lists (luma 3 / chroma 4+c). `luma_scan[z]`/`cdc`/`cac` are the
    /// scan-order coefficients; uncoded blocks are zero so recon == prediction.
    #[allow(clippy::too_many_arguments)]
    fn add_inter_residual(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        pred_y: &[u8; 256],
        c_pred: &[[u8; 64]; 2],
        luma_scan: &[[i32; 16]; 16],
        // `Some` when the macroblock carries transform_size_8x8_flag: four 8x8
        // blocks in 8x8 scan order, replacing the sixteen 4x4 luma blocks.
        luma8: Option<&[[i32; 64]; 4]>,
        cdc: &[[i32; 4]; 2],
        cac: &[[[i32; 16]; 4]; 2],
        cbp_chroma: u32,
        // Parsed totalCoeff per block, indexed exactly as the parse's `iz`:
        // [0..16] luma 4x4 z-order (for t8, the 8x8 count sits at `id8*4`),
        // [16..24] chroma AC as `16 + c*4 + id4`. The parser already counted
        // every significant coefficient; re-deriving the counts here scanned
        // 16-64 array elements per block (~400 loads/MB) for information the
        // caller was holding — the diagnosis's stage-boundary re-derivation tax.
        nnzs: &[u8; 24],
    ) {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecResidAdd);
        let qp = self.cur_qp;
        let qpc = self.chroma_qp_for(qp);
        let (w4r, w2r) = (self.mb_w * 4, self.mb_w * 2);
        if let Some(l8) = luma8 {
            // INTER 8x8 luma: same primitives the I_8x8 and CAVLC paths use.
            for b8 in 0..4usize {
                let (b8x, b8y) = (b8 % 2, b8 / 2);
                // PER-CELL, not one aggregate broadcast over all four cells. CAVLC
                // codes an 8x8 block as four 4x4 sub-blocks and its nC predictor
                // reads these per-4x4 counts from `nnz_y`, so the broadcast
                // corrupted the NEXT macroblock's nC and desynced the parse -- which
                // is why CAVLC 8x8 streams ffmpeg accepts would not decode here. The
                // worker copy of this function never wrote `nnz_y` at all, so the
                // threaded path was unaffected and hid the defect. CABAC has no
                // per-4x4 counts, so its callers put the 8x8 total in all four slots.
                let nnz: u32 = (0..4).map(|k| nnzs[b8 * 4 + k] as u32).sum();
                for sy in 0..2 {
                    for sx in 0..2 {
                        self.nnz_y[(mb_y * 4 + b8y * 2 + sy) * w4r + (mb_x * 4 + b8x * 2 + sx)] =
                            nnzs[b8 * 4 + sy * 2 + sx];
                    }
                }
                let res8 = if nnz == 0 {
                    [0i32; 64]
                } else {
                    let raster = un_scan_8x8(&l8[b8]);
                    // list 1 = INTER 8x8 luma scaling list (0 is the intra one).
                    self.inv_quant8(&raster, qp, 1)
                };
                // The 4x4 inter path marks coded_y per block; the 8x8 branch must too,
                // or a later intra macroblock's neighbour availability is wrong.
                for sy in 0..2 {
                    for sx in 0..2 {
                        self.coded_y[(mb_y * 4 + b8y * 2 + sy) * w4r + (mb_x * 4 + b8x * 2 + sx)] = true;
                    }
                }
                let predb: [i32; 64] =
                    std::array::from_fn(|i| pred_y[(b8y * 8 + i / 8) * 16 + (b8x * 8 + i % 8)] as i32);
                let recon = add_residual_8x8(&res8, &predb);
                let (px, py) = (mb_x * 16 + b8x * 8, mb_y * 16 + b8y * 8);
                for dy in 0..8 {
                    for dx in 0..8 {
                        self.rec_y[(py + dy) * self.cw + (px + dx)] = recon[dy * 8 + dx];
                    }
                }
            }
        }
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            if luma8.is_some() {
                break;
            }
            let nnz = nnzs[blk];
            self.nnz_y[(mb_y * 4 + lby) * w4r + (mb_x * 4 + lbx)] = nnz;
            let cw = self.cw;
            let p_off = (lby * 4) * 16 + lbx * 4;
            let r_off = (mb_y * 4 + lby) * 4 * cw + (mb_x * 4 + lbx) * 4;
            if nnz == 0 {
                // Zero residual → recon == prediction EXACTLY (the integer IDCT is
                // linear so zeros map to zeros, and pred is already 0..=255) — copy
                // the pred rows straight into the plane. On real (sparse-cbp)
                // streams this is MOST of the 4×4 blocks.
                for r in 0..4 {
                    self.rec_y[r_off + r * cw..r_off + r * cw + 4]
                        .copy_from_slice(&pred_y[p_off + r * 16..p_off + r * 16 + 4]);
                }
                continue;
            }
            // DC-ONLY: the sole significant coefficient is scan position 0 (the
            // zig-zag starts at DC, and un_scan keeps it at raster 0), so the
            // whole dequant + IDCT collapses to one multiply and a flat add.
            if nnz == 1 && luma_scan[blk][0] != 0 {
                let f = self.dequant_dc4(luma_scan[blk][0], qp, 3);
                reconstruct_4x4_dc_into((f + 32) >> 6, pred_y, p_off, 16, &mut self.rec_y, r_off, cw);
            } else {
                // Fused un-scan + dequant over ONLY the significant coefficients,
                // then IDCT + add + clip straight into the plane — no `qb`, no
                // `deq`-from-dense, no `predb` gather, no `s`, no `store` call.
                //
                // HYBRID: the scatter walks scan positions with a data-dependent
                // branch per slot, which beats the branchless dense 16-multiply
                // loop only while the block is SPARSE. The DC/zero fast paths
                // already removed the sparsest blocks, so the population here
                // skews denser — above ~6 coefficients the dense loop wins.
                let deq = if nnz <= 6 {
                    dequant_scatter_4x4(&luma_scan[blk], nnz, 0, qp, self.scaling.as_ref().map(|sc| &sc[3]))
                } else {
                    self.dequant(&un_scan_4x4_dcac(&luma_scan[blk]), qp, 3)
                };
                reconstruct_4x4_into(&deq, pred_y, p_off, 16, &mut self.rec_y, r_off, cw);
            }
        }
        let mut c_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for c in 0..2 {
                c_dc[c] = self.dequant_chroma_dc(&cdc[c], qpc, 4 + c);
            }
        }
        let ccw = self.ccw;
        for c in 0..2 {
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut ac_nz = false;
                if cbp_chroma == 2 {
                    let n = nnzs[16 + c * 4 + by * 2 + bx];
                    self.nnz_c[c][(mb_y * 2 + by) * w2r + (mb_x * 2 + bx)] = n;
                    ac_nz = n != 0;
                }
                let dc = c_dc[c][by * 2 + bx];
                let p_off = (by * 4) * 8 + bx * 4;
                let r_off = (mb_y * 2 + by) * 4 * ccw + (mb_x * 2 + bx) * 4;
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                if dc == 0 && !ac_nz {
                    // Zero residual (no AC, zero DC) → recon == prediction exactly.
                    for r in 0..4 {
                        plane[r_off + r * ccw..r_off + r * ccw + 4]
                            .copy_from_slice(&c_pred[c][p_off + r * 8..p_off + r * 8 + 4]);
                    }
                    continue;
                }
                // DC-ONLY (no coded AC — covers every cbp_chroma==1 block and the
                // AC-empty blocks of cbp_chroma==2): the chroma DC arrives ALREADY
                // dequantized, so the residual is `(dc + 32) >> 6` flat.
                if !ac_nz {
                    reconstruct_4x4_dc_into((dc + 32) >> 6, &c_pred[c], p_off, 8, plane, r_off, ccw);
                    continue;
                }
                // AC-only scan: index i is overall scan position i+1 (ac_shift=1).
                // Same sparse/dense hybrid as luma.
                let n = nnzs[16 + c * 4 + by * 2 + bx];
                let mut deq = if n <= 6 {
                    dequant_scatter_4x4(&cac[c][by * 2 + bx], n, 1, qpc, self.scaling.as_ref().map(|sc| &sc[4 + c]))
                } else {
                    let mut ac = [0i32; 16];
                    un_scan_4x4_ac_into(&cac[c][by * 2 + bx], &mut ac);
                    // Free-fn dequant: `self.dequant` borrows all of `self`, which
                    // conflicts with the live `plane` (&mut self.rec_u/v) borrow.
                    match &self.scaling {
                        Some(sc) => dequantize_weighted(&ac, qpc, &sc[4 + c]),
                        None => dequantize(&ac, qpc),
                    }
                };
                deq[0] = dc;
                reconstruct_4x4_into(&deq, &c_pred[c], p_off, 8, plane, r_off, ccw);
            }
        }
    }

    fn recon_chroma_cabac(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        chroma_mode: u8,
        cdc: &[[i32; 4]; 2],
        cac: &[[[i32; 16]; 4]; 2],
        cbp_chroma: u32,
        avail_top: bool,
        avail_left: bool,
    ) {
        let qpc = self.chroma_qp_for(self.cur_qp);
        let (cx, cy) = (mb_x * 8, mb_y * 8);
        let mut c_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for c in 0..2 {
                c_dc[c] = self.dequant_chroma_dc(&cdc[c], qpc, 1 + c);
            }
        }
        let w2 = self.mb_w * 2;
        for c in 0..2 {
            let mut ctop = [0u8; 8];
            let mut cleft = [0u8; 8];
            let mut ccorner = 0u8;
            {
                let rec_c = if c == 0 { &self.rec_u } else { &self.rec_v };
                if avail_top {
                    ctop.copy_from_slice(self.top_c_row(c, cy, cx, 8));
                }
                if avail_left {
                    for i in 0..8 {
                        cleft[i] = rec_c[(cy + i) * self.ccw + cx - 1];
                    }
                }
                if avail_top && avail_left {
                    ccorner = self.top_c_px(c, cy, cx - 1);
                }
            }
            let pred8 = chroma8x8_pred(chroma_mode, avail_top, avail_left, &ctop, &cleft, ccorner);
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut ac = [0i32; 16];
                if cbp_chroma == 2 {
                    un_scan_4x4_ac_into(&cac[c][by * 2 + bx], &mut ac);
                    self.nnz_c[c][(mb_y * 2 + by) * w2 + (mb_x * 2 + bx)] =
                        cac[c][by * 2 + bx].iter().filter(|&&v| v != 0).count() as u8;
                }
                let mut deq = self.dequant(&ac, qpc, 1 + c);
                deq[0] = c_dc[c][by * 2 + bx];
                let predb: [i32; 16] =
                    std::array::from_fn(|i| pred8[(by * 4 + i / 4) * 8 + (bx * 4 + i % 4)] as i32);
                let s = reconstruct_4x4(&deq, &predb);
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                store(plane, self.ccw, cx + bx * 4, cy + by * 4, &s);
            }
        }
    }

    /// D14 — the CAVLC E-seam (P3 item 5). Mirrors `decode_slice_data_cabac`:
    /// overlap parse (this thread) with pixel reconstruction (a scoped worker
    /// owning the planes). Now possible because the CAVLC inter recon was
    /// converged onto `add_inter_residual`, so both entropy coders emit the SAME
    /// `PInterJob` and share one worker recon.
    pub fn decode_slice_data(
        &mut self,
        r: &mut BitReader,
        is_p: bool,
        first_mb: usize,
    ) -> Result<usize, MbError> {
        let eligible = edc_on() && rowdb_on() && (is_p || self.is_b);
        let threaded = eligible
            && edc_mt().unwrap_or_else(|| edc_dispatch(self.mb_w, self.mb_h, self.bits_per_mb, false));
        edcstat::bump(&edcstat::DISPATCH_ON, threaded as u64);
        edcstat::bump(&edcstat::DISPATCH_SEEN, eligible as u64);
        if !threaded {
            return self.decode_slice_cavlc_inner(r, is_p, first_mb);
        }
        let ctx = self.edc_take_ctx();
        let (tx, rx) = std::sync::mpsc::sync_channel::<EdcMsg>(edc_bound());
        let (ctx_tx, ctx_rx) = std::sync::mpsc::channel::<PixelCtx>();
        let (back_tx, back_rx) = std::sync::mpsc::channel::<PixelCtx>();
        let (res, ctx, panicked) = std::thread::scope(|sc| {
            let h = sc.spawn(move || edc_worker(ctx, rx, ctx_tx, back_rx));
            self.edc_tx = Some(tx);
            self.edc_ctx_rx = Some(ctx_rx);
            self.edc_back_tx = Some(back_tx);
            // UNWIND SAFETY (same trap the CABAC wrapper documents): the sender
            // lives in `self`, which outlives an unwind, so a panic in the parse
            // loop would leave the channel open, the worker alive and the scope
            // join blocking forever — turning a diagnosable panic into a silent
            // deadlock. Catch, clean up, join, restore, THEN resume.
            let r2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.decode_slice_cavlc_inner(r, is_p, first_mb)
            }));
            self.edc_flush_batch();
            self.edc_giveback();
            self.edc_tx = None;
            self.edc_ctx_rx = None;
            self.edc_back_tx = None;
            match (r2, h.join()) {
                (Ok(res), Ok(ctx)) => (res, Some(ctx), None),
                (Err(pn), Ok(ctx)) => (Err(MbError::Truncated), Some(ctx), Some(pn)),
                (Ok(_), Err(pn)) | (Err(_), Err(pn)) => (Err(MbError::Truncated), None, Some(pn)),
            }
        });
        if let Some(ctx) = ctx {
            self.edc_restore_ctx(ctx);
        }
        if let Some(pn) = panicked {
            std::panic::resume_unwind(pn);
        }
        res
    }

    fn decode_slice_cavlc_inner(
        &mut self,
        r: &mut BitReader,
        is_p: bool,
        first_mb: usize,
    ) -> Result<usize, MbError> {
        let total = self.mb_w * self.mb_h;
        self.slice_first_mb = first_mb;
        self.edc_active = edc_on();
        let mut addr = first_mb;
        while addr < total {
            self.row_hook(addr);
            if is_p || self.is_b {
                let skip_run = {
                    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
                    r.read_ue()?
                } as usize;
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
                // ORDER: B reconstructs inline (not seam-ready).
                self.edc_intra_sync();
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
        self.edc_flush(); // slice end: no job crosses a slice boundary
        Ok(addr)
    }

    fn decode_mb(
        &mut self,
        r: &mut BitReader,
        mb_x: usize,
        mb_y: usize,
        is_p: bool,
    ) -> Result<(), MbError> {
        let mut mb_type = {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
            r.read_ue()?
        };
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
        // ORDER: intra reconstruction reads neighbour PIXELS, so the worker
        // must have applied every deferred job before this point.
        self.edc_intra_sync();
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
        // H-48: this scope was DECLARED and never wired, which is precisely why the
        // stage table left 19.8% unaccounted — 66,120 of 475,200 macroblocks on the
        // reference stream are I-type and had no scope at all.
        let _gi = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMbI);
        if mb_type == 0 {
            // I_NxN: transform_size_8x8_flag (when enabled) selects I_8x8 vs I_4x4.
            if self.transform_8x8_mode && r.read_bit()? {
                self.decode_i8x8(r, mb_x, mb_y)?;
            } else {
                self.decode_i4x4(r, mb_x, mb_y)?;
            }
        } else if (1..=24).contains(&mb_type) {
            self.decode_i16(r, mb_x, mb_y, mb_type - 1)?;
        } else if mb_type == 25 {
                        // ORDER: I_PCM writes pixels directly.
            self.edc_intra_sync();
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
        // DEBLOCK CLASS: mode 0 is P_L0_16x16 — ONE partition, so all 16 blocks
        // share a reference and motion vector and no internal edge can reach
        // strength 1. Internal strengths then follow from coefficients alone, i.e.
        // 16 nnz bytes instead of a 24-block gather across 5-7 grids. Modes 1/2
        // (P_16x8 / P_8x16) have two partitions with independent motion and stay
        // UNSET (blind path).
        if mode == 0 {
            self.mb_kind[mb_y * self.mb_w + mb_x] =
                rusty_h264_common::deblock::MB_KIND_INTER_UNIFORM;
        }
        // QP (qp/qpc) is bound after mb_qp_delta is read below.
        let w4 = self.mb_w * 4;
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let num_refs = self.refs.len();
        let layout = inter_partitions(mode);

        // mb_pred order (spec 7.3.5.1): all ref_idx_l0 first (only when more than
        // one reference is active), then all mvd_l0.
        let nparts = layout.len();
        let mut ref_idxs = [0i32; 4];
        if self.num_ref_active > 1 {
            for ri in ref_idxs[..nparts].iter_mut() {
                *ri = read_ref_idx(r, self.num_ref_active)?;
                if *ri as usize >= num_refs {
                    return Err(MbError::Truncated); // references a non-existent picture
                }
            }
        }

        // Phase 1: per partition, ref-aware MV prediction + mvd, committing the
        // motion grid so a later partition predicts from an earlier one.
        let mut part_mv = [(0i32, (0i32, 0i32)); 4];
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::MvGrid);
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
        }

        // Phase 2: motion-compensate each partition from its reference.
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        // D8-CAVLC: the double-stage ablation, extended to the CAVLC loop. The
        // CABAC measurement could not run here at all (`edc_active` is false on
        // this path, so nothing replays through `edc_flush` and `doubled` read
        // 0) — yet CAVLC is exactly the population P3 item 5 targets, and its
        // cheaper parse should make the PIXEL share larger. Doubling the whole
        // partition loop is idempotent: pass 2 overwrites `pred_y` with fresh MC
        // BEFORE `weight_partition` runs, so weighting cannot apply twice.
        // This doubles MC only (not the residual add inside `inter_finish`), so
        // it is a LOWER BOUND on the CAVLC pixel share.
        // D14 (CAVLC E-seam): when the seam is live the WORKER motion-compensates
        // from the committed MV grids, so skip MC here rather than computing a
        // prediction that would be discarded.
        let defer = self.edc_tx.is_some() || self.edc_active;
        let mc_passes = if defer { 0 } else if double_recon() { 2 } else { 1 };
        for _pass in 0..mc_passes {
        if _pass > 0 {
            edcstat::bump(&edcstat::DOUBLED, 1);
        }
        for (part, &(rx, ry, rw, rh)) in layout.iter().enumerate() {
            let (refi, mv) = part_mv[part];
            let reference = &self.refs[refi as usize];
            let mut tmp = [0u8; 256];
            mc_luma_padded(&reference.py, reference.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + rx, mb_y * 16 + ry, rw, rh, mv.0, mv.1, &mut tmp);
            {
                let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                restride(&mut pred_y, 16, rx, ry, &tmp, rw, rh);
            }
            let (crx, cry, crw, crh) = (rx / 2, ry / 2, rw / 2, rh / 2);
            for cc in 0..2 {
                let rc = if cc == 0 { &reference.pu } else { &reference.pv };
                let mut tc = [0u8; 64];
                mc_chroma_padded(rc, reference.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv.0, mv.1, &mut tc);
                {
                    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                    restride(&mut c_pred[cc], 8, crx, cry, &tc, crw, crh);
                }
            }
            self.weight_partition(&mut pred_y, &mut c_pred, 0, refi as usize, rx, ry, rw, rh);
        }
        }

        // 16×16/16×8/8×16 partitions are all ≥ 8×8, so the 8×8 transform is allowed.
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred, true, defer)
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
        allow_8x8: bool,
        // D14: emit a worker job instead of reconstructing. Only the CAVLC
        // 16x16/16x8/8x16 path sets this; B and P_8x8 stay inline.
        defer: bool,
    ) -> Result<(), MbError> {
        let w4 = self.mb_w * 4;
        let cbp = {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
            read_cbp_inter(r)?
        };
        let cbp_luma = cbp & 15;
        let cbp_chroma = cbp >> 4;
        // transform_size_8x8_flag follows cbp (before mb_qp_delta) when luma has
        // coefficients, the 8×8 transform is enabled, and every partition ≥ 8×8.
        let t8x8 = cbp_luma > 0 && self.transform_8x8_mode && allow_8x8 && r.read_bit()?;
        if t8x8 {
            self.mb_t8x8[mb_y * self.mb_w + mb_x] = true;
        }
        if cbp != 0 {
            self.step_qp(r.read_se()?);
        }
        let (qp, qpc) = (self.cur_qp, self.chroma_qp_for(self.cur_qp));

        // ---- luma residual ----
        self.nnz_cache_load(mb_x, mb_y);
        let mut luma_scan = [[0i32; 16]; 16];
        let mut nnzs = [0u8; 24];
        let mut luma8 = [[0i32; 64]; 4]; // 8×8-transform residuals (when t8x8)
        if t8x8 {
            for b8 in 0..4 {
                let (b8x, b8y) = (b8 % 2, b8 / 2);
                let (bx, by) = (mb_x * 4 + b8x * 2, mb_y * 4 + b8y * 2);
                if cbp_luma & (1 << b8) != 0 {
                    let mut scan8 = [0i32; 64];
                    for sub in 0..4 {
                        let (sx, sy) = (sub % 2, sub / 2);
                        let (cx, cy) = (b8x * 2 + sx, b8y * 2 + sy);
                        let nc = self.nc_pred(cx, cy);
                        let blk = decode_residual_block(r, 16, nc)?;
                        let total = blk.iter().filter(|&&v| v != 0).count() as u8;
                        self.nnz_cache_set(cx, cy, total);
                        self.nnz_y[(by + sy) * w4 + (bx + sx)] = total;
                        // The PER-SUB-BLOCK count the next macroblock's nC prediction
                        // depends on -- summing these into one slot and letting the
                        // recon helper broadcast it back is what broke CAVLC 8x8.
                        nnzs[b8 * 4 + sub] = total;
                        for k in 0..16 {
                            scan8[4 * k + sub] = blk[k];
                        }
                    }
                    // RAW: `add_inter_residual` applies un_scan_8x8 + inv_quant8
                    // itself, exactly as it does for the CABAC path.
                    luma8[b8] = scan8;
                } else {
                    for sub in 0..4 {
                        let (sx, sy) = (sub % 2, sub / 2);
                        self.nnz_cache_set(b8x * 2 + sx, b8y * 2 + sy, 0);
                        self.nnz_y[(by + sy) * w4 + (bx + sx)] = 0;
                    }
                }
            }
        } else {
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                let (bx, by) = (mb_x * 4 + lbx, mb_y * 4 + lby);
                let total = if cbp_luma & (1 << (blk / 4)) != 0 {
                    let nc = self.nc_pred(lbx, lby);
                    let scan16 = decode_residual_block(r, 16, nc)?;
                    luma_scan[blk] = scan16; // RAW scan order, like CABAC
                    scan16.iter().filter(|&&v| v != 0).count() as u8
                } else {
                    0
                };
                self.nnz_cache_set(lbx, lby, total);
                self.nnz_y[by * w4 + bx] = total;
                nnzs[blk] = total;
            }
        }

        // ---- chroma residual ----
        let mut c_recon_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for (c, slot) in c_recon_dc.iter_mut().enumerate() {
                let dc = decode_residual_block(r, 4, -1)?;
                *slot = [dc[0], dc[1], dc[2], dc[3]]; // RAW; dequantised in the helper
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
                    c_q[c][by * 2 + bx] = ac; // RAW scan order
                    nnzs[16 + c * 4 + by * 2 + bx] = total;
                }
            }
        }

        // ---- reconstruction ----
        //
        // D13: this used to be a 109-line hand-rolled copy of the residual add.
        // It now calls the SAME `add_inter_residual` the CABAC path uses, which
        // is what makes the CAVLC E-seam possible at all: the two paths had
        // different residual representations (CAVLC pre-applied un_scan and
        // inv_quant at PARSE time; CABAC carries raw scan-order coefficients and
        // dequantises inside the helper), so no job could be shared. Carrying the
        // raw forms — which CAVLC already had in hand — converges them, deletes
        // a duplicate implementation, and lets a deferred job reuse the existing
        // worker recon instead of needing a second copy that could drift.
        if defer {
            // The residual representation now matches CABAC exactly (the
            // convergence commit), so the SAME `PInterJob` and the SAME worker
            // `recon_p_inter` serve both entropy coders — no second recon
            // implementation exists that could drift.
            let (mut gmv, mut gref) = ([(0i32, 0i32); 16], [0u8; 16]);
            let w4r = self.mb_w * 4;
            for by in 0..4usize {
                for bx in 0..4usize {
                    let bi = (mb_y * 4 + by) * w4r + (mb_x * 4 + bx);
                    gmv[by * 4 + bx] = self.mv_y[bi];
                    gref[by * 4 + bx] = self.ref_idx_y[bi].clamp(0, 15) as u8;
                }
            }
            // D9 applies here too: `cbp == 0` means all 2,592 coefficient bytes
            // of the 2,784-byte job are ZERO, so ship the 176-byte motion-only
            // form. Discovered on the CABAC path; it transfers for free because
            // the CAVLC arm now emits the SAME job type.
            let ej = if cbp == 0 && nores_on() {
                edcstat::bump(&edcstat::J_NORES_SENT, 1);
                EdcJob::InterNoRes(Box::new(PInterNoResJob {
                    mbx: mb_x, mby: mb_y, t8: t8x8, qp, gmv, gref,
                }))
            } else {
                EdcJob::Inter(Box::new(PInterJob {
                    mbx: mb_x, mby: mb_y, t8: t8x8, qp,
                    cbp_chroma, gmv, gref,
                    luma_scan, luma8, cdc: c_recon_dc, cac: c_q, nnzs,
                }))
            };
            if self.edc_tx.is_some() {
                self.edc_giveback();
                self.edc_send_job(ej);
            } else {
                self.edc_jobs.push(ej);
            }
        } else {
            self.add_inter_residual(
                mb_x, mb_y, pred_y, c_pred, &luma_scan,
                if t8x8 { Some(&luma8) } else { None },
                &c_recon_dc, &c_q, cbp_chroma, &nnzs,
            );
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
    /// Co-located 4x4 block coords for the current block's `(bx4, by4)` within the
    /// macroblock, per spec 8.4.1.2.1. Under `direct_8x8_inference_flag` every 4x4
    /// in an 8x8 takes that 8x8's OUTER CORNER (`luma4x4BlkIdx = 5 * mbPartIdx`,
    /// i.e. (0,0) (3,0) (0,3) (3,3)); otherwise motion is genuinely per-4x4.
    ///
    /// 8.4.1.2.1 is SHARED by both direct modes, so spatial and temporal must map
    /// identically. They did not: temporal mapped the corner and spatial read the
    /// block's own coords, which is invisible while every 4x4 in the co-located 8x8
    /// carries the same motion -- true of every stream until sub-8x8 P partitions
    /// (x264 `--partitions p4x4`) make them differ. Hence one function.
    #[inline]
    fn col_block(&self, bx4: usize, by4: usize) -> (usize, usize) {
        if self.direct_8x8_inference {
            ((bx4 / 2) * 3, (by4 / 2) * 3)
        } else {
            (bx4, by4)
        }
    }

    fn col_zero(&self, bx: usize, by: usize) -> bool {
        let Some(col) = self.refs1.first() else { return false };
        if col.long_term || col.w4 == 0 {
            return false;
        }
        let idx = by * col.w4 + bx;
        if idx >= col.ref_idx.len() {
            return false;
        }
        // Spec 8.4.1.2.1: the co-located motion is List-0's when the co-located
        // block HAS a List-0 prediction, and List-1's otherwise (predFlagL0Col == 0).
        // Reading List-0 unconditionally treats an L1-only block as intra
        // (ref_idx -1), which silently suppresses colZeroFlag. An L1-only
        // co-located block can only exist when the co-located picture is itself a
        // B picture, i.e. only under b-pyramid -- which is why this survived every
        // non-pyramid B stream.
        let (cref, cmv) = if col.ref_idx[idx] >= 0 {
            (col.ref_idx[idx], col.mv[idx])
        } else if idx < col.ref_idx1.len() && col.ref_idx1[idx] >= 0 {
            (col.ref_idx1[idx], col.mv1[idx])
        } else {
            return false;
        };
        cref == 0 && cmv.0.abs() <= 1 && cmv.1.abs() <= 1
    }

    /// Implicit bi-prediction weights `(w0, w1)` from POC distances (spec
    /// §8.4.2.3.2), or `None` for the plain average (idc≠2, uni-pred, or the
    /// equidistant / out-of-range fall-back to 32:32 which equals the average).
    fn implicit_weights(&self, refi0: i32, refi1: i32) -> Option<(i32, i32)> {
        if self.weighted_bipred_idc != 2 || refi0 < 0 || refi1 < 0 {
            return None;
        }
        let r0 = &self.refs[refi0 as usize];
        let r1 = &self.refs1[refi1 as usize];
        let td = (r1.poc - r0.poc).clamp(-128, 127);
        let tb = (self.cur_poc - r0.poc).clamp(-128, 127);
        if td == 0 || r0.long_term || r1.long_term {
            return None; // 32:32 → identical to the average
        }
        let tx = (16384 + td.abs() / 2) / td;
        let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
        let w1 = dsf >> 2;
        if !(-64..=128).contains(&w1) {
            return None; // out of range → 32:32 average
        }
        Some((64 - w1, w1))
    }

    /// Motion-compensates a region with the given per-list refs/MVs. Bi-prediction
    /// is the simple `(a+b+1)>>1` average, or POC-weighted when implicit weighting
    /// (idc 2) is active. Writes into `pred_y`/`c_pred`.
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
        let _gb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBMc);
        // Malformed-stream armor, mirroring the P path: now that B slices actually
        // PARSE ref_idx (they used to be hardcoded to 0), a mutated stream can hand
        // us an index past the end of either list. Clamp rather than panic — the
        // crate is `forbid(unsafe_code)` and fuzz-gated to never panic, and a
        // wrong picture on garbage input carries no conformance duty.
        let refi0 = if refi0 >= 0 { (refi0 as usize).min(self.refs.len().saturating_sub(1)) as i32 } else { -1 };
        let refi1 = if refi1 >= 0 { (refi1 as usize).min(self.refs1.len().saturating_sub(1)) as i32 } else { -1 };
        if (refi0 >= 0 && self.refs.is_empty()) || (refi1 >= 0 && self.refs1.is_empty()) {
            return;
        }
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        let weights = {
            let _gw = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBWeights);
            self.implicit_weights(refi0, refi1)
        };
        // Bi-prediction blend: the weights decision is LOOP-INVARIANT, so every
        // blend site below matches on `weights` ONCE and runs a branch-free
        // pixel loop — the unweighted `(p+q+1)>>1` average then autovectorizes
        // (the per-pixel closure this replaces hid the invariant behind a
        // capture, and its chroma form was a &dyn call PER PIXEL).
        // FULL-WIDTH regions (px == 0, rw == 16 — every 16×16/16×8 partition and
        // most direct regions) occupy contiguous rows of `pred_y`, so MC writes
        // the destination DIRECTLY: uni-pred needs no staging at all, bi-pred
        // stages only the second list and blends in place. The staging arrays
        // (512 B zeroed per call before this) now exist only on the branches
        // that read them. Same fusion as the P path's mc_rect (WHYS Part 8).
        let full = px == 0 && rw == 16;
        let _gl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBLuma);
        // One scratch borrow for the whole region — both bi-pred passes included.
        // The closure yields whether the arm already ran the chroma half (the
        // bi-pred full-width arm does, to keep its staging alive) — a plain
        // `return` inside would exit the CLOSURE only and chroma would run twice.
        let chroma_done = rusty_h264_common::inter::with_mc_scratch(|scr| match (refi0 >= 0, refi1 >= 0, full) {
            (true, false, true) => {
                let rf = &self.refs[refi0 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                false
            }
            (false, true, true) => {
                let rf = &self.refs1[refi1 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                false
            }
            (true, true, true) => {
                let rf = &self.refs[refi0 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                let mut b = [0u8; 256];
                let rf = &self.refs1[refi1 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut b[..rw * rh]);
                drop(_gl);
                let _gbl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBBlend);
                // SLICE-then-zip: proving the bounds ONCE lets rustc emit the whole
                // 256-byte average as 8 straight-line vpavgb ops (verified in
                // isolation, x86-64-v3); the indexed form kept a per-iteration
                // bounds check and a loop. A hand AVX2 kernel is refuted — the
                // compiler already emits the ideal instruction.
                let dst = &mut pred_y[py * 16..py * 16 + rw * rh];
                match weights {
                    None => {
                        for (d, s) in dst.iter_mut().zip(&b[..rw * rh]) {
                            *d = ((*d as u16 + *s as u16 + 1) >> 1) as u8;
                        }
                    }
                    Some((w0, w1)) => {
                        for (d, s) in dst.iter_mut().zip(&b[..rw * rh]) {
                            *d = ((*d as i32 * w0 + *s as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                        }
                    }
                }
                let _gc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBChroma);
                self.b_mc_chroma(mb_x, mb_y, px, py, rw, rh, refi0, mv0, refi1, mv1, c_pred, weights, cch);
                true
            }
            _ => {
                // Narrow region — rows are strided in `pred_y`; stage and copy.
                let (mut a, mut b) = ([0u8; 256], [0u8; 256]);
                if refi0 >= 0 {
                    let rf = &self.refs[refi0 as usize];
                    rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut a[..rw * rh]);
                }
                if refi1 >= 0 {
                    let rf = &self.refs1[refi1 as usize];
                    rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut b[..rw * rh]);
                }
                drop(_gl);
                let _gbl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBBlend);
                match (refi0 >= 0, refi1 >= 0) {
                    (true, true) => {
                        for dy in 0..rh {
                            let (ar, br) = (&a[dy * rw..dy * rw + rw], &b[dy * rw..dy * rw + rw]);
                            let base = (py + dy) * 16 + px;
                            let dst = &mut pred_y[base..base + rw];
                            match weights {
                                None => {
                                    for ((d, p), q) in dst.iter_mut().zip(ar).zip(br) {
                                        *d = ((*p as u16 + *q as u16 + 1) >> 1) as u8;
                                    }
                                }
                                Some((w0, w1)) => {
                                    for ((d, p), q) in dst.iter_mut().zip(ar).zip(br) {
                                        *d = ((*p as i32 * w0 + *q as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                                    }
                                }
                            }
                        }
                    }
                    (true, false) => {
                        for dy in 0..rh {
                            let d = (py + dy) * 16 + px;
                            pred_y[d..d + rw].copy_from_slice(&a[dy * rw..dy * rw + rw]);
                        }
                    }
                    _ => {
                        for dy in 0..rh {
                            let d = (py + dy) * 16 + px;
                            pred_y[d..d + rw].copy_from_slice(&b[dy * rw..dy * rw + rw]);
                        }
                    }
                }
                false
            }
        });
        if chroma_done {
            return;
        }
        let _gc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBChroma);
        self.b_mc_chroma(mb_x, mb_y, px, py, rw, rh, refi0, mv0, refi1, mv1, c_pred, weights, cch);
    }

    /// Chroma half of `b_mc`, with the same full-width direct-write fusion
    /// (crw == 8 rows are contiguous in the 8-wide `c_pred` planes).
    #[allow(clippy::too_many_arguments)]
    fn b_mc_chroma(
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
        c_pred: &mut [[u8; 64]; 2],
        weights: Option<(i32, i32)>,
        cch: usize,
    ) {
        let (crx, cry, crw, crh) = (px / 2, py / 2, rw / 2, rh / 2);
        let full = crx == 0 && crw == 8;
        for c in 0..2 {
            match (refi0 >= 0, refi1 >= 0, full) {
                (true, false, true) => {
                    let rf = &self.refs[refi0 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                }
                (false, true, true) => {
                    let rf = &self.refs1[refi1 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                }
                (true, true, true) => {
                    let rf = &self.refs[refi0 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                    let mut cb = [0u8; 64];
                    let rf = &self.refs1[refi1 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut cb[..crw * crh]);
                    let dst = &mut c_pred[c][cry * 8..cry * 8 + crw * crh];
                    match weights {
                        None => {
                            for (d, s) in dst.iter_mut().zip(&cb[..crw * crh]) {
                                *d = ((*d as u16 + *s as u16 + 1) >> 1) as u8;
                            }
                        }
                        Some((w0, w1)) => {
                            for (d, s) in dst.iter_mut().zip(&cb[..crw * crh]) {
                                *d = ((*d as i32 * w0 + *s as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                            }
                        }
                    }
                }
                _ => {
                    let (mut ca, mut cb) = ([0u8; 64], [0u8; 64]);
                    if refi0 >= 0 {
                        let rf = &self.refs[refi0 as usize];
                        let pl = if c == 0 { &rf.pu } else { &rf.pv };
                        mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut ca[..crw * crh]);
                    }
                    if refi1 >= 0 {
                        let rf = &self.refs1[refi1 as usize];
                        let pl = if c == 0 { &rf.pu } else { &rf.pv };
                        mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut cb[..crw * crh]);
                    }
                    match (refi0 >= 0, refi1 >= 0) {
                        (true, true) => {
                            for dy in 0..crh {
                                let (pr, qr) = (&ca[dy * crw..dy * crw + crw], &cb[dy * crw..dy * crw + crw]);
                                let base = (cry + dy) * 8 + crx;
                                let dst = &mut c_pred[c][base..base + crw];
                                match weights {
                                    None => {
                                        for ((d, p), q) in dst.iter_mut().zip(pr).zip(qr) {
                                            *d = ((*p as u16 + *q as u16 + 1) >> 1) as u8;
                                        }
                                    }
                                    Some((w0, w1)) => {
                                        for ((d, p), q) in dst.iter_mut().zip(pr).zip(qr) {
                                            *d = ((*p as i32 * w0 + *q as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                                        }
                                    }
                                }
                            }
                        }
                        (true, false) => {
                            for dy in 0..crh {
                                let d = (cry + dy) * 8 + crx;
                                c_pred[c][d..d + crw].copy_from_slice(&ca[dy * crw..dy * crw + crw]);
                            }
                        }
                        _ => {
                            for dy in 0..crh {
                                let d = (cry + dy) * 8 + crx;
                                c_pred[c][d..d + crw].copy_from_slice(&cb[dy * crw..dy * crw + crw]);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Commits a region's per-list motion to the 4×4 grids (and marks coded).
    #[allow(clippy::too_many_arguments)]
    fn b_set_motion(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, refi0: i32, mv0: (i32, i32), refi1: i32, mv1: (i32, i32)) {
        let _gs = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBSet);
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
    #[allow(clippy::too_many_arguments)]
    /// Splits a `w`×`h` block region (4×4-block units) into the fewest rectangles
    /// whose contents are `uniform`, preferring partition-shaped cuts (whole →
    /// horizontal halves → vertical halves → quadrants). Emits at most w·h rects
    /// (the all-different worst case degenerates to per-block, i.e. the old loop).
    fn coalesce_region(
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        uniform: &dyn Fn(usize, usize, usize, usize) -> bool,
        emit: &mut dyn FnMut(usize, usize, usize, usize),
    ) {
        if uniform(x, y, w, h) {
            emit(x, y, w, h);
            return;
        }
        if h > 1 && uniform(x, y, w, h / 2) && uniform(x, y + h / 2, w, h / 2) {
            emit(x, y, w, h / 2);
            emit(x, y + h / 2, w, h / 2);
            return;
        }
        if w > 1 && uniform(x, y, w / 2, h) && uniform(x + w / 2, y, w / 2, h) {
            emit(x, y, w / 2, h);
            emit(x + w / 2, y, w / 2, h);
            return;
        }
        match (w > 1, h > 1) {
            (true, true) => {
                for q in 0..4usize {
                    Self::coalesce_region(x + (q % 2) * (w / 2), y + (q / 2) * (h / 2), w / 2, h / 2, uniform, emit);
                }
            }
            (true, false) => {
                Self::coalesce_region(x, y, w / 2, h, uniform, emit);
                Self::coalesce_region(x + w / 2, y, w / 2, h, uniform, emit);
            }
            (false, true) => {
                Self::coalesce_region(x, y, w, h / 2, uniform, emit);
                Self::coalesce_region(x, y + h / 2, w, h / 2, uniform, emit);
            }
            (false, false) => emit(x, y, 1, 1),
        }
    }

    fn decode_b_direct(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, pred_y: &mut [u8; 256], c_pred: &mut [[u8; 64]; 2]) {
        let _gb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBDirect);
        if !self.direct_spatial {
            return self.decode_b_direct_temporal(mb_x, mb_y, px, py, rw, rh, pred_y, c_pred);
        }
        // H-48: DERIVATION-ONLY scope, dropped before the MC loop below. DecBDirect
        // wraps this function whole and therefore INCLUDES the `b_mc` calls it makes,
        // so its 1460 ns/call was never "MV derivation is slow" — that read was wrong.
        // This guard is what separates the two.
        let gd = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBDeriv);
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
        // Per 4×4 sub-block: colZeroFlag zeroes the ref-0 motion vector. cz is the
        // ONLY per-block variable (two possible (m0,m1) values for the region), and
        // the MC filters + bi-blend are per-output-pixel — so sub-blocks with equal
        // cz coalesce into one wider `b_mc`, BIT-IDENTICAL. A 16×16 direct MB paid
        // 16 bi-pred b_mc calls (~96 MC kernel entries) before this; typically 1 now.
        let (bx0, by0, bw, bh) = (px / 4, py / 4, rw / 4, rh / 4);
        let mut czg = [[false; 4]; 4]; // region-local, [dy][dx]
        for dy in 0..bh {
            for dx in 0..bw {
                let (colx, coly) = self.col_block(bx0 + dx, by0 + dy);
                czg[dy][dx] = !direct_zero && self.col_zero(mb_x * 4 + colx, mb_y * 4 + coly);
            }
        }
        let uniform = |x: usize, y: usize, w: usize, h: usize| -> bool {
            let t = czg[y][x];
            (y..y + h).all(|dy| (x..x + w).all(|dx| czg[dy][dx] == t))
        };
        let mut rects: [(usize, usize, usize, usize); 16] = [(0, 0, 0, 0); 16];
        let mut n = 0usize;
        Self::coalesce_region(0, 0, bw, bh, &uniform, &mut |x, y, w, h| {
            rects[n] = (x, y, w, h);
            n += 1;
        });
        drop(gd); // derivation ends; everything below is MC + motion-grid commit
        for &(x, y, w, h) in &rects[..n] {
            let cz = czg[y][x];
            let m0 = if refi0 == 0 && cz { (0, 0) } else { mv0 };
            let m1 = if refi1 == 0 && cz { (0, 0) } else { mv1 };
            let (lx, ly, lw, lh) = ((bx0 + x) * 4, (by0 + y) * 4, w * 4, h * 4);
            self.b_mc_or_record(mb_x, mb_y, lx, ly, lw, lh, refi0, m0, refi1, m1, pred_y, c_pred);
            self.b_set_motion(mb_x, mb_y, lx, ly, lw, lh, refi0, m0, refi1, m1);
        }
    }

    /// Temporal direct prediction for a region (spec §8.4.1.2.3): for each 4×4
    /// (or per-8×8 corner under `direct_8x8_inference`), take the co-located
    /// List-0 motion from `RefPicList1[0]`, map its reference into the current
    /// List-0 by POC, and scale the motion vector by the POC distances.
    #[allow(clippy::too_many_arguments)]
    fn decode_b_direct_temporal(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, pred_y: &mut [u8; 256], c_pred: &mut [[u8; 64]; 2]) {
        let poc1 = self.refs1.first().map_or(0, |f| f.poc);
        let infer = self.direct_8x8_inference;
        // Under direct_8x8_inference every 4×4 in an 8×8 takes the same MB-corner
        // co-located motion, so motion-compensate the whole 8×8 in one call — this
        // hits the width-8 MC asm and pays the per-call tile/blend setup 4× less.
        // Without inference, motion is genuinely per-4×4. Bit-identical either way
        // (MC of an 8×8 with one MV == four 4×4 MCs with that same MV).
        let step = if infer { 8 } else { 4 };
        let mut sy = py;
        while sy < py + rh {
            let mut sx = px;
            while sx < px + rw {
                // Co-located 4×4 (the 8×8's MB-corner under inference) — shared with
                // the spatial path's colZeroFlag, which must map identically.
                let (colx, coly) = self.col_block(sx / 4, sy / 4);
                let (mvcol, refpoc) = {
                    let col = &self.refs1[0];
                    let idx = (mb_y * 4 + coly) * col.w4 + (mb_x * 4 + colx);
                    if col.w4 != 0 && idx < col.mv.len() && col.ref_poc[idx] != i32::MIN {
                        (col.mv[idx], col.ref_poc[idx])
                    } else {
                        ((0, 0), i32::MIN) // intra co-located → zero motion, refIdxL0 = 0
                    }
                };
                // MapColToList0: the current-list index of the co-located reference.
                let (refi0, mvc) = if refpoc == i32::MIN {
                    (0, (0, 0))
                } else {
                    let r = self.refs.iter().position(|f| f.poc == refpoc).unwrap_or(0) as i32;
                    (r, mvcol)
                };
                let poc0 = self.refs[refi0 as usize].poc;
                let td = (poc1 - poc0).clamp(-128, 127);
                let tb = (self.cur_poc - poc0).clamp(-128, 127);
                let (mv0, mv1) = if td == 0 || self.refs[refi0 as usize].long_term {
                    (mvc, (0, 0))
                } else {
                    let tx = (16384 + td.abs() / 2) / td;
                    let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                    let m0 = ((dsf * mvc.0 + 128) >> 8, (dsf * mvc.1 + 128) >> 8);
                    (m0, (m0.0 - mvc.0, m0.1 - mvc.1))
                };
                self.b_mc_or_record(mb_x, mb_y, sx, sy, step, step, refi0, mv0, 0, mv1, pred_y, c_pred);
                self.b_set_motion(mb_x, mb_y, sx, sy, step, step, refi0, mv0, 0, mv1);
                sx += step;
            }
            sy += step;
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
        if self.edc_tx.is_some() {
            self.edc_regions = Some(Vec::with_capacity(4));
        }
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        self.decode_b_direct(mb_x, mb_y, 0, 0, 16, 16, &mut pred_y, &mut c_pred);
        if let Some(regions) = self.edc_regions.take() {
            // nnz clears are PARSE state; the pixel copy is the worker's.
            let w4 = self.mb_w * 4;
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                self.nnz_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 0;
            }
            self.edc_giveback();
            self.edc_send_job(EdcJob::BSkip { mbx: mb_x, mby: mb_y, regions });
            return Ok(());
        }
        // Zero residual: the prediction is the reconstruction — copy it row-wise.
        for dy in 0..16 {
            let d = (mb_y * 16 + dy) * self.cw + mb_x * 16;
            self.rec_y[d..d + 16].copy_from_slice(&pred_y[dy * 16..dy * 16 + 16]);
        }
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for dy in 0..8 {
                let d = (mb_y * 8 + dy) * self.ccw + mb_x * 8;
                plane[d..d + 8].copy_from_slice(&c_pred[c][dy * 8..dy * 8 + 8]);
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
            // B_Direct_16x16 — 8×8 transform allowed only with direct_8x8_inference.
            self.decode_b_direct(mb_x, mb_y, 0, 0, 16, 16, &mut pred_y, &mut c_pred);
            return self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred, self.direct_8x8_inference, false);
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
            // Spec-correct bi-prediction (average of L0 and L1), matching the CABAC
            // path. This used to replicate an openh264 bug for a Bi 16x8/8x16
            // partition -- openh264 mis-handles the destination buffer there, so
            // partition 0 came out List-1-only and partition 1 List-0-only. That was
            // deliberate when openh264's h264dec WAS the conformance oracle, but the
            // gate is ffmpeg now and the CABAC path already went spec-correct; the
            // CAVLC path was simply left behind. Measured: mb_type 12..21 (every B
            // 16x8/8x16 with at least one Bi partition) were 100% wrong vs ffmpeg,
            // while 1..11 (no Bi partition) were only collaterally damaged.
            self.b_mc_or_record(mb_x, mb_y, rx, ry, rw, rh, refi[p][0], mv[0], refi[p][1], mv[1], &mut pred_y, &mut c_pred);
        }
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred, true, false)
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
        //
        // FIXED ARRAYS, NOT `Vec::new()` + push. These are per-MACROBLOCK on every
        // B_8x8, and a growing Vec allocated (and reallocated) twice per MB — on a
        // B-heavy stream that is thousands of allocations per frame for data whose
        // maximum size is a compile-time constant: 4 partitions x at most 4
        // sub-partitions = 16 entries. Indexing a fixed array cannot exceed that, and
        // an out-of-range index would panic rather than misbehave, so the bound is
        // enforced either way and this crate stays forbid(unsafe).
        const MAX_MVD: usize = 16;
        let mut mvd0 = [(0i32, 0i32); MAX_MVD];
        let mut mvd1 = [(0i32, 0i32); MAX_MVD];
        let (mut n0, mut n1) = (0usize, 0usize);
        for &st in &sub {
            if st != 0 && b_sub_uses(st, 0) {
                for _ in b_sub_parts(st) {
                    mvd0[n0] = (r.read_se()?, r.read_se()?);
                    n0 += 1;
                }
            }
        }
        for &st in &sub {
            if st != 0 && b_sub_uses(st, 1) {
                for _ in b_sub_parts(st) {
                    mvd1[n1] = (r.read_se()?, r.read_se()?);
                    n1 += 1;
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
                self.b_mc_or_record(mb_x, mb_y, px, py, sw, sh, refi[p][0], mv[0], refi[p][1], mv[1], &mut pred_y, &mut c_pred);
            }
        }
        // noSubMbPartSizeLessThan8x8: each sub-partition must be ≥ 8×8 (direct
        // counts only with the 8×8 inference flag).
        let allow_8x8 = sub
            .iter()
            .all(|&st| if st == 0 { self.direct_8x8_inference } else { st <= 3 });
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred, allow_8x8, false)
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
        // D14b: P_8x8 defers too. Syncing before it instead cost 9,772 pipeline
        // drains per stream (45 per 1000 MBs, vs CABAC's 3.3) because P_8x8 is
        // common in CAVLC P slices — and the seam measured 1.65-1.97x SLOWER
        // for it. Deferring is byte-identical for sub-partitions: the worker
        // motion-compensates per 4x4 from the committed grids, which is exactly
        // how the CABAC path already handles P_8x8, and a 6-tap filter applied
        // per-4x4 with the same MV gives the same pixels as one 8x8 call.
        let defer = self.edc_tx.is_some() || self.edc_active;

        // ── PHASE 1: PARSE + COMMIT. Must always run to completion. ──────────
        //
        // These two are interleaved BY NECESSITY: each sub-partition's MV
        // prediction reads the grids the previous one committed, so they cannot
        // be separated from each other. But they consume BITSTREAM, so nothing
        // here may ever be skipped conditionally.
        //
        // This phase split is a STRUCTURAL guard, not a tidy-up. When MC lived
        // inside this loop, deferring it was written as a `break` — which also
        // skipped the `read_se` mvd reads, desynced the bitstream, and
        // mis-parsed as a B-slice sub_mb_type on a Baseline stream. It happened
        // to crash; a desync that stayed IN RANGE would have produced plausible
        // garbage instead. With MC in its own pass below, skipping pixel work
        // cannot reach the bitstream at all — the mistake is unavailable.
        let mut regions: [(usize, usize, usize, usize, i32, (i32, i32)); 16] =
            [(0, 0, 0, 0, 0, (0, 0)); 16];
        let mut nreg = 0usize;
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
                regions[nreg] = (px, py, srw, srh, refi, mv);
                nreg += 1;
            }
        }

        // ── PHASE 2: PIXEL WORK ONLY. Reads no bitstream; safe to skip. ──────
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        if !defer {
            for &(px, py, srw, srh, refi, mv) in &regions[..nreg] {
                let reference = &self.refs[refi as usize];
                let mut tmp = [0u8; 256];
                mc_luma_padded(&reference.py, reference.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, srw, srh, mv.0, mv.1, &mut tmp);
                restride(&mut pred_y, 16, px, py, &tmp, srw, srh);
                let (crx, cry, crw, crh) = (px / 2, py / 2, srw / 2, srh / 2);
                for cc in 0..2 {
                    let rc = if cc == 0 { &reference.pu } else { &reference.pv };
                    let mut tc = [0u8; 64];
                    mc_chroma_padded(rc, reference.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv.0, mv.1, &mut tc);
                    restride(&mut c_pred[cc], 8, crx, cry, &tc, crw, crh);
                }
                self.weight_partition(
                    &mut pred_y, &mut c_pred, 0, refi as usize, px, py, srw, srh,
                );
            }
        }

        // P_8x8 allows the 8×8 transform only when every sub-partition is 8×8.
        let allow_8x8 = sub_types.iter().all(|&t| t == 0);
        self.inter_finish(r, mb_x, mb_y, &pred_y, &c_pred, allow_8x8, defer)
    }

    /// Reconstructs a `P_Skip` macroblock: motion-compensate from the reference
    /// at the skip MV, with no residual.
    /// Records a B MC region (threaded mode) or executes it inline — the SAME
    /// arguments as `b_mc`; the recorded arm resolves the implicit weights at
    /// parse time (identical function, parse-side data).
    #[allow(clippy::too_many_arguments)]
    fn b_mc_or_record(&mut self, mb_x: usize, mb_y: usize, px: usize, py: usize, rw: usize, rh: usize, refi0: i32, mv0: (i32, i32), refi1: i32, mv1: (i32, i32), pred_y: &mut [u8; 256], c_pred: &mut [[u8; 64]; 2]) {
        if self.edc_regions.is_some() {
            // Mirror `b_mc`'s malformed-stream armor EXACTLY before touching the
            // ref lists: the inline path clamps the indices and bails on empty
            // lists BEFORE computing weights; calling `implicit_weights` with the
            // raw indices re-introduced the panic the armor exists to prevent
            // (found by the fuzzer, via the unwind-guard that turned the
            // resulting worker deadlock back into a diagnosable failure).
            let cr0 = if refi0 >= 0 { (refi0 as usize).min(self.refs.len().saturating_sub(1)) as i32 } else { -1 };
            let cr1 = if refi1 >= 0 { (refi1 as usize).min(self.refs1.len().saturating_sub(1)) as i32 } else { -1 };
            let w = if (cr0 >= 0 && self.refs.is_empty()) || (cr1 >= 0 && self.refs1.is_empty()) {
                None // the worker's port returns before reading the weights
            } else {
                self.implicit_weights(cr0, cr1)
            };
            self.edc_regions.as_mut().unwrap().push(BRegion { px, py, rw, rh, refi0, refi1, mv0, mv1, w });
            return;
        }
        self.b_mc(mb_x, mb_y, px, py, rw, rh, refi0, mv0, refi1, mv1, pred_y, c_pred);
    }

    /// Builds the worker's owned pixel context from `self` (planes MOVED out,
    /// shared read-only state cloned, filter inputs snapshotted).
    fn edc_take_ctx(&mut self) -> PixelCtx {
        PixelCtx {
            rec_y: std::mem::take(&mut self.rec_y),
            rec_u: std::mem::take(&mut self.rec_u),
            rec_v: std::mem::take(&mut self.rec_v),
            bak_y: std::mem::take(&mut self.bak_y),
            bak_u: std::mem::take(&mut self.bak_u),
            bak_v: std::mem::take(&mut self.bak_v),
            refs: self.refs.clone(),
            refs1: self.refs1.clone(),
            weights: self.weights.clone(),
            scaling: self.scaling,
            scaling8: self.scaling8,
            cw: self.cw,
            ccw: self.ccw,
            mb_w: self.mb_w,
            mb_h: self.mb_h,
            chroma_qp_offset: self.chroma_qp_offset,
            flt_rows: self.flt_rows,
            db_ena: self.db_ena,
            db_oa: self.db_oa,
            db_ob: self.db_ob,
            cur_qp: self.cur_qp,
            qp_grid: self.mb_qp.clone(),
            t8_grid: self.mb_t8x8.clone(),
            bs_store: self.bs_frame.clone(),
        }
    }

    /// Restores the planes (and the filter watermark) from a returned context.
    fn edc_restore_ctx(&mut self, ctx: PixelCtx) {
        self.rec_y = ctx.rec_y;
        self.rec_u = ctx.rec_u;
        self.rec_v = ctx.rec_v;
        self.bak_y = ctx.bak_y;
        self.bak_u = ctx.bak_u;
        self.bak_v = ctx.bak_v;
        self.flt_rows = ctx.flt_rows;
    }

    /// Intra macroblocks read neighbour PIXELS: fetch the context from the
    /// worker (which drains all prior jobs first — the channel is FIFO) and
    /// install the planes so the inline intra path runs unchanged. The context
    /// is given back lazily at the next job/row/slice-end (`edc_giveback`), so
    /// consecutive intra macroblocks pay ONE round-trip.
    /// Queue a pixel job for the worker (D10). Batched per row; see `EdcMsg::Batch`.
    #[inline]
    fn edc_send_job(&mut self, job: EdcJob) {
        edcstat::bump(&edcstat::JOBS, 1);
        if !batch_on() {
            self.edc_tx
                .as_ref()
                .unwrap()
                .send(EdcMsg::Job(job))
                .expect("worker alive");
            return;
        }
        self.edc_batch.push(job);
    }

    /// Ship the accumulated row batch. MUST be called before anything that
    /// depends on those jobs having been applied: the row's `Row` filter
    /// message, a `NeedCtx` handover, and slice end.
    fn edc_flush_batch(&mut self) {
        if self.edc_batch.is_empty() {
            return;
        }
        // Replace with a PRE-RESERVED buffer rather than the empty Vec
        // `mem::take` would leave: otherwise each row reallocs and regrows from
        // zero, trading 208k channel sends for ~7 reallocs x 3k rows.
        let cap = self.edc_batch.capacity().max(self.mb_w);
        let jobs = std::mem::replace(&mut self.edc_batch, Vec::with_capacity(cap));
        edcstat::bump(&edcstat::BATCHES, 1);
        self.edc_tx
            .as_ref()
            .unwrap()
            .send(EdcMsg::Batch(jobs))
            .expect("worker alive");
    }

    fn edc_intra_sync(&mut self) {
        if self.edc_tx.is_none() {
            self.edc_flush();
            return;
        }
        if self.edc_parked.is_some() {
            return; // already holding
        }
        // ORDER: drain the batch before taking the planes, or those jobs would
        // be applied to a context the parse thread is concurrently holding.
        self.edc_flush_batch();
        edcstat::bump(&edcstat::NEEDCTX, 1);
        self.edc_tx.as_ref().unwrap().send(EdcMsg::NeedCtx).expect("worker alive");
        let mut ctx = self.edc_ctx_rx.as_ref().unwrap().recv().expect("worker ctx");
        self.rec_y = std::mem::take(&mut ctx.rec_y);
        self.rec_u = std::mem::take(&mut ctx.rec_u);
        self.rec_v = std::mem::take(&mut ctx.rec_v);
        self.bak_y = std::mem::take(&mut ctx.bak_y);
        self.bak_u = std::mem::take(&mut ctx.bak_u);
        self.bak_v = std::mem::take(&mut ctx.bak_v);
        self.flt_rows = ctx.flt_rows;
        self.edc_parked = Some(ctx);
    }

    /// Returns a held context to the worker (inverse of `edc_intra_sync`).
    /// NOTE (D10): this is called per-macroblock on the job paths, so it must
    /// NOT flush the batch — that would undo the batching. It is safe: the
    /// parked state is only ever entered through `edc_intra_sync`, which
    /// flushes before taking the planes, so nothing can be queued-but-unsent
    /// while the parse thread holds them.
    fn edc_giveback(&mut self) {
        if let Some(mut parked) = self.edc_parked.take() {
            parked.rec_y = std::mem::take(&mut self.rec_y);
            parked.rec_u = std::mem::take(&mut self.rec_u);
            parked.rec_v = std::mem::take(&mut self.rec_v);
            parked.bak_y = std::mem::take(&mut self.bak_y);
            parked.bak_u = std::mem::take(&mut self.bak_u);
            parked.bak_v = std::mem::take(&mut self.bak_v);
            parked.flt_rows = self.flt_rows;
            // Fail-soft: on the unwind path the worker may already be gone;
            // dropping the parked context is acceptable there (the planes are
            // lost, but the panic is being propagated anyway).
            let _ = self.edc_back_tx.as_ref().unwrap().send(parked);
        }
    }

    /// Parse-side twin of the nnz/coded grid writes `add_inter_residual` does
    /// inline — the worker's port omits them (they are PARSE state: deblock
    /// derivation and the CAVLC nC contexts read them), so the threaded path
    /// commits them here, from the same parsed counts (their equality with the
    /// recon-side recount is the Part 8 nnz-threading brick's own invariant).
    fn edc_commit_nnz(&mut self, mbx: usize, mby: usize, t8: bool, nnzs: &[u8; 24], cbp_chroma: u32) {
        let (w4r, w2r) = (self.mb_w * 4, self.mb_w * 2);
        if t8 {
            for b8 in 0..4usize {
                let (b8x, b8y) = (b8 % 2, b8 / 2);
                let n = nnzs[b8 * 4];
                for sy in 0..2 {
                    for sx in 0..2 {
                        self.nnz_y[(mby * 4 + b8y * 2 + sy) * w4r + (mbx * 4 + b8x * 2 + sx)] = n;
                    }
                }
            }
        } else {
            for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
                self.nnz_y[(mby * 4 + lby) * w4r + (mbx * 4 + lbx)] = nnzs[blk];
            }
        }
        if cbp_chroma == 2 {
            for c in 0..2usize {
                for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                    self.nnz_c[c][(mby * 2 + by) * w2r + (mbx * 2 + bx)] = nnzs[16 + c * 4 + by * 2 + bx];
                }
            }
        }
    }

    /// Flush the entropy-decouple job queue: replay every deferred pixel job
    /// in parse order. Called before any intra macroblock (its reconstruction
    /// reads neighbour PIXELS), before row filtering, at B-branch entry, at
    /// slice end, and at `deblock()` as a backstop.
    fn edc_flush(&mut self) {
        if self.edc_jobs.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.edc_jobs);
        for j in &jobs {
            match j {
                EdcJob::Skip { mbx, mby, mv } => {
                    self.recon_p_skip(*mbx, *mby, *mv);
                    if double_recon() {
                        edcstat::bump(&edcstat::DOUBLED, 1);
                        self.recon_p_skip(*mbx, *mby, *mv);
                    }
                }
                EdcJob::Inter(job) => {
                    self.recon_p_inter(job);
                    if double_recon() {
                        edcstat::bump(&edcstat::DOUBLED, 1);
                        self.recon_p_inter(job);
                    }
                }
                EdcJob::InterNoRes(job) => {
                    let full = job.to_full();
                    self.recon_p_inter(&full);
                    if double_recon() {
                        edcstat::bump(&edcstat::DOUBLED, 1);
                        self.recon_p_inter(&full);
                    }
                }
                // B jobs exist only in worker (MT) mode and are never queued
                // here — the single-thread seam keeps B inline. Not reachable
                // from any input (the push sites are gated on `edc_tx`).
                EdcJob::B(_) | EdcJob::BSkip { .. } => unreachable!("B jobs are worker-only"),
            }
        }
        // Hand the (now empty) Vec back so its allocation is reused.
        self.edc_jobs = jobs;
        self.edc_jobs.clear();
    }

    /// Reconstructs one CABAC P inter macroblock from its parse job — the
    /// pixel half of the entropy-decouple seam (docs/entropy-decouple-plan.md
    /// E1). Reads NOTHING from parse state except the frame grids this MB's
    /// parse already committed (its own block MVs/refs, re-gathered below —
    /// stable after commit) and the immutable DPB; called either inline
    /// (seam off / flush disabled) or in-order at a flush point. Byte-
    /// identical to the former inline block by construction: replay order
    /// equals inline order at every pixel-observable point (intra reads, row
    /// filtering) because flushes precede both.
    fn recon_p_inter(&mut self, j: &PInterJob) {
        let mbw = self.mb_w;
        // `add_inter_residual` (and anything under it) reads `self.cur_qp`,
        // which at FLUSH time belongs to a later macroblock — replay must
        // restore this MB's qp. The x264 corpus (near-constant QP) could not
        // see this; the encoder's delta-QP roundtrip stream caught it.
        let saved_qp = self.cur_qp;
        self.cur_qp = j.qp;
                    // ---- Recon: motion-comp (per 4×4 luma / co-located 2×2 chroma using the
                    // committed grid MV — the 6-tap/bilinear filter is per-output-pixel, so
                    // per-block MC is bit-identical to per-partition MC) + residual add via the
                    // SAME reconstruct_4x4 as intra, with the MC output as the prediction.
                    let qp = j.qp;
                    let qpc = self.chroma_qp_for(qp);
                    let (w4r, w2r) = (mbw * 4, mbw * 2);
                    let mut pred_y = [0u8; 256];
                    let mut c_pred = [[0u8; 64]; 2];
                    {
                        // MC-CALL COALESCING (side-by-side descent, dec target #2): the old
                        // loop paid 16 mc_luma(4×4) + 32 mc_chroma(2×2) per MB regardless of
                        // partitioning — 48 calls even for a single-MV 16×16 MB, and the
                        // per-call glue around 2.4M calls was ~40% of decoding real-world
                        // (x264) streams. The 6-tap/bilinear filters are per-output-pixel,
                        // so merging blocks with equal (mv, ref) into one wider MC call is
                        // BIT-IDENTICAL; the rect ladder mirrors the partition shapes.
                        let _ms = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMcStage);
                        let (rh16, cch) = (self.mb_h * 16, self.mb_h * 8);
                        let mut gmv = [(0i32, 0i32); 16];
                        let mut gref = [0usize; 16];
                        let _gg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::MvGrid);
                        for by in 0..4usize {
                            for bx in 0..4usize {
                                let bidx = (j.mby * 4 + by) * w4r + (j.mbx * 4 + bx);
                                gmv[by * 4 + bx] = self.mv_y[bidx];
                                // Per-block reference (multi-ref P): ref_idx_l0 committed to the
                                // grid. Clamp — a corrupt stream can over-range it (never panic).
                                gref[by * 4 + bx] =
                                    (self.ref_idx_y[bidx].max(0) as usize).min(self.refs.len() - 1);
                            }
                        }
                        drop(_gg);
                        // All blocks of the rect (in 4×4-block units) match its top-left?
                        let rect_eq = |x4: usize, y4: usize, w4: usize, h4: usize| -> bool {
                            let t = y4 * 4 + x4;
                            (0..h4).all(|dy| {
                                (0..w4).all(|dx| {
                                    let b = (y4 + dy) * 4 + (x4 + dx);
                                    gmv[b] == gmv[t] && gref[b] == gref[t]
                                })
                            })
                        };
                        let refs = &self.refs;
                        let (cw, ccw) = (self.cw, self.ccw);
                        let mut mc_rect = |x4: usize,
                                           y4: usize,
                                           w4: usize,
                                           h4: usize,
                                           pred_y: &mut [u8; 256],
                                           c_pred: &mut [[u8; 64]; 2]| {
                            let b = y4 * 4 + x4;
                            let (mv, reference) = (gmv[b], &refs[gref[b]]);
                            let (w, h) = (w4 * 4, h4 * 4);
                            // A FULL-WIDTH rect (w == 16, so x4 == 0) occupies contiguous
                            // whole rows of `pred_y` — the MC output layout and the
                            // destination layout coincide, so MC writes the prediction
                            // buffer DIRECTLY. The staging copy exists only for narrow
                            // rects, whose rows really are strided in `pred_y`. This is
                            // the diagnosis's "stage-boundary materialization" tax paid
                            // by the dominant 16×16/16×8 shapes: 256 B of `t` zeroing
                            // plus a 256 B copy per rect, for nothing.
                            if w == 16 {
                                rusty_h264_common::inter::with_mc_scratch(|scr| rusty_h264_common::inter::mc_luma_padded_pre(scr, &reference.py, reference.lstride(), crate::LPAD, cw, rh16, j.mbx * 16, j.mby * 16 + y4 * 4, w, h, mv.0, mv.1, &mut pred_y[y4 * 64..y4 * 64 + w * h]));
                            } else {
                                let mut t = [0u8; 256];
                                rusty_h264_common::inter::with_mc_scratch(|scr| rusty_h264_common::inter::mc_luma_padded_pre(scr, &reference.py, reference.lstride(), crate::LPAD, cw, rh16, j.mbx * 16 + x4 * 4, j.mby * 16 + y4 * 4, w, h, mv.0, mv.1, &mut t[..w * h]));
                                let _pb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                                for dy in 0..h {
                                    pred_y[(y4 * 4 + dy) * 16 + x4 * 4..][..w]
                                        .copy_from_slice(&t[dy * w..dy * w + w]);
                                }
                            }
                            let (cw4, ch4) = (w4 * 2, h4 * 2);
                            for cc in 0..2 {
                                let rc = if cc == 0 { &reference.pu } else { &reference.pv };
                                // Same full-width coincidence for chroma: cw4 == 8 rows
                                // are contiguous in the 8-wide `c_pred` plane.
                                if cw4 == 8 {
                                    mc_chroma_padded(rc, reference.cstride(), crate::CPAD, ccw, cch, j.mbx * 8, j.mby * 8 + y4 * 2, cw4, ch4, mv.0, mv.1, &mut c_pred[cc][y4 * 16..y4 * 16 + cw4 * ch4]);
                                    continue;
                                }
                                let mut tc = [0u8; 64];
                                mc_chroma_padded(rc, reference.cstride(), crate::CPAD, ccw, cch, j.mbx * 8 + x4 * 2, j.mby * 8 + y4 * 2, cw4, ch4, mv.0, mv.1, &mut tc[..cw4 * ch4]);
                                let _pb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                                for dy in 0..ch4 {
                                    c_pred[cc][(y4 * 2 + dy) * 8 + x4 * 2..][..cw4]
                                        .copy_from_slice(&tc[dy * cw4..dy * cw4 + cw4]);
                                }
                            }
                        };
                        if rect_eq(0, 0, 4, 4) {
                            mc_rect(0, 0, 4, 4, &mut pred_y, &mut c_pred);
                        } else if rect_eq(0, 0, 4, 2) && rect_eq(0, 2, 4, 2) {
                            mc_rect(0, 0, 4, 2, &mut pred_y, &mut c_pred);
                            mc_rect(0, 2, 4, 2, &mut pred_y, &mut c_pred);
                        } else if rect_eq(0, 0, 2, 4) && rect_eq(2, 0, 2, 4) {
                            mc_rect(0, 0, 2, 4, &mut pred_y, &mut c_pred);
                            mc_rect(2, 0, 2, 4, &mut pred_y, &mut c_pred);
                        } else {
                            for q in 0..4usize {
                                let (qx, qy) = ((q % 2) * 2, (q / 2) * 2);
                                if rect_eq(qx, qy, 2, 2) {
                                    mc_rect(qx, qy, 2, 2, &mut pred_y, &mut c_pred);
                                } else if rect_eq(qx, qy, 2, 1) && rect_eq(qx, qy + 1, 2, 1) {
                                    mc_rect(qx, qy, 2, 1, &mut pred_y, &mut c_pred);
                                    mc_rect(qx, qy + 1, 2, 1, &mut pred_y, &mut c_pred);
                                } else if rect_eq(qx, qy, 1, 2) && rect_eq(qx + 1, qy, 1, 2) {
                                    mc_rect(qx, qy, 1, 2, &mut pred_y, &mut c_pred);
                                    mc_rect(qx + 1, qy, 1, 2, &mut pred_y, &mut c_pred);
                                } else {
                                    for j in 0..4usize {
                                        mc_rect(qx + (j % 2), qy + (j / 2), 1, 1, &mut pred_y, &mut c_pred);
                                    }
                                }
                            }
                        }
                        // EXPLICIT WEIGHTED PREDICTION (spec 8.4.2.3). The CAVLC inter
                        // path weights each partition after MC; the MC-call-coalescing
                        // rewrite of this CABAC path lost it, and nothing caught that
                        // because the effect is invisible unless a stream actually
                        // carries non-default weights. x264's `weightp` DUPLICATES a
                        // reference and distinguishes the copy ONLY by its weights, so
                        // every macroblock picking the weighted index decoded unweighted
                        // -- a silent, accumulating luma drift.
                        //
                        // Applied per 4x4 block rather than per partition: the weight
                        // depends solely on the block's reference index, so the two are
                        // equivalent, and `gref` already holds it for every block
                        // regardless of which rect ladder rung ran.
                        if self.weights.is_some() {
                            for by in 0..4usize {
                                for bx in 0..4usize {
                                    let refi = gref[by * 4 + bx];
                                    self.weight_partition(
                                        &mut pred_y, &mut c_pred, 0, refi, bx * 4, by * 4, 4, 4,
                                    );
                                }
                            }
                        }
                    }
                    // Residual add — the SAME helper the B path uses (this inline
                    // copy was a duplicate; deduped when the zero-block fast path
                    // landed so both paths share it).
                    self.add_inter_residual(j.mbx, j.mby, &pred_y, &c_pred, &j.luma_scan, if j.t8 { Some(&j.luma8) } else { None }, &j.cdc, &j.cac, j.cbp_chroma, &j.nnzs);
        self.cur_qp = saved_qp;
    }

    fn decode_p_skip(&mut self, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        // DEBLOCK CLASS: a P_Skip macroblock carries no coefficients and one
        // (ref, mv) for all 16 blocks, so every internal boundary strength is 0 by
        // §8.7.2.1 and the loop filter needs 9 block loads instead of 24. This is
        // the single highest-value classification: the MB-kind census measures Skip
        // at 36.4% (CAVLC) / 65.0% (main) / 57.8% (high) of real x264 corpora.
        // Written HERE because both the CAVLC and the CABAC slice loops funnel
        // through this one function.
        //
        // Deliberately NOT done for `B_Skip` — its motion is direct-derived and can
        // differ per 4×4 sub-block, so its internal edges can legally reach
        // strength 1. B_Skip stays UNSET and takes the blind path.
        self.mb_kind[mb_y * self.mb_w + mb_x] = rusty_h264_common::deblock::MB_KIND_SKIP;
        // P_Skip always references index 0 (the most recent picture). Borrow it —
        // a full-frame `.cloned()` here was ~86% of total decode time (one ~3 MB
        // plane copy per skip MB, thousands per frame).
        if self.refs.is_empty() {
            return Err(MbError::Unsupported("P_Skip without reference"));
        }
        let mv = self.skip_mv(mb_x, mb_y);
        // Grid commits are PARSE state (later macroblocks' MV prediction and
        // availability read them) — they run now; the pixel half reads only
        // the DPB + `mv`, so it defers cleanly (E1 seam).
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::SkipRecon);
            self.set_mb_mv(mb_x, mb_y, mv, true, 0);
            let w4 = self.mb_w * 4;
            for &(lbx, lby) in &LUMA_4X4_SCAN_XY {
                self.coded_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = true;
                self.modes_y[(mb_y * 4 + lby) * w4 + (mb_x * 4 + lbx)] = 2;
            }
        }
        if self.edc_tx.is_some() {
            self.edc_giveback();
            self.edc_send_job(EdcJob::Skip { mbx: mb_x, mby: mb_y, mv });
            return Ok(());
        }
        if self.edc_active {
            self.edc_jobs.push(EdcJob::Skip { mbx: mb_x, mby: mb_y, mv });
            return Ok(());
        }
        self.recon_p_skip(mb_x, mb_y, mv);
        if double_recon() {
            self.recon_p_skip(mb_x, mb_y, mv);
        }
        Ok(())
    }

    /// Pixel half of P_Skip (see the E1 seam note on `recon_p_inter`).
    fn recon_p_skip(&mut self, mb_x: usize, mb_y: usize, mv: (i32, i32)) {
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        let mut pred = [0u8; 256];
        let rf0 = &self.refs[0];
        mc_luma_padded(&rf0.py, rf0.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred);
        if let Some(wt) = &self.weights {
            for p in pred.iter_mut() {
                *p = wt.apply_luma(*p, 0, 0);
            }
        }
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::SkipRecon);
            for dy in 0..16 {
                let d = (mb_y * 16 + dy) * self.cw + mb_x * 16;
                self.rec_y[d..d + 16].copy_from_slice(&pred[dy * 16..dy * 16 + 16]);
            }
        }
        for c in 0..2 {
            let mut pc = [0u8; 64];
            let rf0 = &self.refs[0];
            let rc = if c == 0 { &rf0.pu } else { &rf0.pv };
            mc_chroma_padded(rc, rf0.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut pc);
            if let Some(wt) = &self.weights {
                for p in pc.iter_mut() {
                    *p = wt.apply_chroma(*p, 0, 0, c);
                }
            }
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for dy in 0..8 {
                let d = (mb_y * 8 + dy) * self.ccw + mb_x * 8;
                plane[d..d + 8].copy_from_slice(&pc[dy * 8..dy * 8 + 8]);
            }
        }
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
                top[i] = self.top_y_px(py, px + i);
            }
            let tr_avail = bx + 1 < w4
                && self.coded_y[(by - 1) * w4 + (bx + 1)]
                && self.nbr_in_slice((bx + 1) / 4, (by - 1) / 4)
                && self.intra_nbr_ok(bx + 1, by - 1);
            for i in 0..4 {
                top[4 + i] = if tr_avail {
                    self.top_y_px(py, px + 4 + i)
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
            corner = self.top_y_px(py, px - 1);
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

    /// Decodes an `I_8x8` macroblock (High profile): four 8×8 luma blocks, each
    /// with its own intra mode, 8×8 transform residual (CAVLC = four interleaved
    /// 4×4 blocks), and 8×8 intra prediction.
    fn decode_i8x8(&mut self, r: &mut BitReader, mb_x: usize, mb_y: usize) -> Result<(), MbError> {
        let w4 = self.mb_w * 4;
        self.mb_t8x8[mb_y * self.mb_w + mb_x] = true;

        // intra8x8 mode signalling — one mode per 8×8 block (raster 0..3),
        // stored into all four of its 4×4 cells so neighbors can read it.
        let mut modes8 = [2u8; 4];
        for (b8, mode) in modes8.iter_mut().enumerate() {
            let (b8x, b8y) = (b8 % 2, b8 / 2);
            let (bx, by) = (mb_x * 4 + b8x * 2, mb_y * 4 + b8y * 2);
            let predicted = self.predict_i4_mode(bx, by);
            let actual = if r.read_bit()? {
                predicted
            } else {
                let rem = r.read_bits(3)? as u8;
                if rem < predicted { rem } else { rem + 1 }
            };
            *mode = actual;
            for sy in 0..2 {
                for sx in 0..2 {
                    self.modes_y[(by + sy) * w4 + (bx + sx)] = actual;
                }
            }
        }

        let chroma_mode = r.read_ue()? as u8;
        let cbp = read_cbp_intra(r)?;
        let cbp_luma = cbp & 15;
        let cbp_chroma = cbp >> 4;
        if cbp != 0 {
            self.step_qp(r.read_se()?);
        }
        let qp = self.cur_qp;

        let top_mb_avail = mb_y > 0
            && self.nbr_in_slice(mb_x, mb_y - 1)
            && self.intra_nbr_ok(mb_x * 4, mb_y * 4 - 1);
        let left_mb_avail = mb_x > 0
            && self.nbr_in_slice(mb_x - 1, mb_y)
            && self.intra_nbr_ok(mb_x * 4 - 1, mb_y * 4);
        self.nnz_cache_load(mb_x, mb_y);

        for b8 in 0..4 {
            let (b8x, b8y) = (b8 % 2, b8 / 2);
            let (bx, by) = (mb_x * 4 + b8x * 2, mb_y * 4 + b8y * 2);
            let (px, py) = (bx * 4, by * 4);

            // residual: 8×8 CAVLC = four 4×4 sub-blocks, coeff k of sub-block s
            // mapping to 8×8 scan position 4·k + s (spec §7.3.5.3.2).
            let mut res8 = [0i32; 64];
            if cbp_luma & (1 << b8) != 0 {
                let mut scan8 = [0i32; 64];
                for sub in 0..4 {
                    let (sx, sy) = (sub % 2, sub / 2);
                    let (cx, cy) = (b8x * 2 + sx, b8y * 2 + sy);
                    let nc = self.nc_pred(cx, cy);
                    let blk = decode_residual_block(r, 16, nc)?;
                    let total = blk.iter().filter(|&&v| v != 0).count() as u8;
                    self.nnz_cache_set(cx, cy, total);
                    self.nnz_y[(by + sy) * w4 + (bx + sx)] = total;
                    for k in 0..16 {
                        scan8[4 * k + sub] = blk[k];
                    }
                }
                let raster = un_scan_8x8(&scan8);
                res8 = self.inv_quant8(&raster, qp, 0);
            } else {
                for sub in 0..4 {
                    let (sx, sy) = (sub % 2, sub / 2);
                    self.nnz_cache_set(b8x * 2 + sx, b8y * 2 + sy, 0);
                    self.nnz_y[(by + sy) * w4 + (bx + sx)] = 0;
                }
            }

            let avail_top = b8y > 0 || top_mb_avail;
            let avail_left = b8x > 0 || left_mb_avail;
            let (top, left, corner, avail_corner) =
                self.gather_i8(px, py, avail_top, avail_left, bx, by);
            let pred = intra8x8_pred(
                modes8[b8], avail_top, avail_left, avail_corner, &top, &left, corner,
            );
            let mut predb = [0i32; 64];
            for i in 0..64 {
                predb[i] = pred[i] as i32;
            }
            let recon = add_residual_8x8(&res8, &predb);
            for dy in 0..8 {
                for dx in 0..8 {
                    self.rec_y[(py + dy) * self.cw + (px + dx)] = recon[dy * 8 + dx];
                }
            }
            for sy in 0..2 {
                for sx in 0..2 {
                    self.coded_y[(by + sy) * w4 + (bx + sx)] = true;
                }
            }
        }

        self.decode_chroma(r, mb_x, mb_y, cbp_chroma, chroma_mode)
    }

    /// Dequantizes + inverse-transforms an 8×8 luma block, applying the scaling
    /// matrix `list` (0 = intra, 1 = inter) or flat weights.
    fn inv_quant8(&self, raster: &[i32; 64], qp: u8, list: usize) -> [i32; 64] {
        match &self.scaling8 {
            Some(s) => inverse_quant_8x8(raster, qp, &s[list]),
            None => inverse_quant_8x8(raster, qp, &[16i32; 64]),
        }
    }

    /// Gathers the 8×8 luma intra reference samples at pixel `(px, py)`: the 16
    /// top samples (8..15 substituted from the last when no top-right), 8 left
    /// samples, the above-left corner, and whether the corner is available.
    #[allow(clippy::too_many_arguments)]
    fn gather_i8(
        &self,
        px: usize,
        py: usize,
        avail_top: bool,
        avail_left: bool,
        bx: usize,
        by: usize,
    ) -> ([u8; 16], [u8; 8], u8, bool) {
        let (cw, w4) = (self.cw, self.mb_w * 4);
        let mut top = [0u8; 16];
        let mut left = [0u8; 8];
        let mut corner = 0;
        if avail_top {
            for i in 0..8 {
                top[i] = self.top_y_px(py, px + i);
            }
            let tr_avail = bx + 2 < w4
                && self.coded_y[(by - 1) * w4 + (bx + 2)]
                && self.nbr_in_slice((bx + 2) / 4, (by - 1) / 4)
                && self.intra_nbr_ok(bx + 2, by - 1);
            for i in 0..8 {
                top[8 + i] = if tr_avail {
                    self.top_y_px(py, px + 8 + i)
                } else {
                    top[7]
                };
            }
        }
        if avail_left {
            for i in 0..8 {
                left[i] = self.rec_y[(py + i) * cw + px - 1];
            }
        }
        let avail_corner = avail_top && avail_left && self.intra_nbr_ok(bx - 1, by - 1);
        if avail_corner {
            corner = self.top_y_px(py, px - 1);
        }
        (top, left, corner, avail_corner)
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
                top[i] = self.top_y_px(ly, lx + i);
            }
        }
        if avail_left {
            for i in 0..16 {
                left[i] = self.rec_y[(ly + i) * self.cw + lx - 1];
            }
        }
        let corner = if avail_top && avail_left {
            self.top_y_px(ly, lx - 1)
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
                        ctop[i] = self.top_c_px(c, cy, cx + i);
                    }
                }
                if avail_left {
                    for i in 0..8 {
                        cleft[i] = rec_c[(cy + i) * self.ccw + cx - 1];
                    }
                }
                if avail_top && avail_left {
                    ccorner = self.top_c_px(c, cy, cx - 1);
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
    /// Per-frame per-MB dump for conformance bisection, keyed on `RH264_DUMP_MB`.
    /// Prints one char per macroblock: `i` = intra, otherwise the List-0 reference
    /// index of the MB's top-left 4x4 block. Directly comparable with ffmpeg's
    /// `-debug mb_type` map, which is the only per-MB ground truth we can get out
    /// of the reference decoder.
    fn dump_mb_map(&self) {
        if std::env::var_os("RH264_DUMP_MB").is_none() {
            return;
        }
        let w4 = self.mb_w * 4;
        let mut hist = [0usize; 4];
        eprintln!("--- frame poc {} ---", self.cur_poc);
        for mb_y in 0..self.mb_h {
            let mut row = String::new();
            for mb_x in 0..self.mb_w {
                let b = (mb_y * 4) * w4 + mb_x * 4;
                let r = self.ref_idx_y[b];
                if r < 0 {
                    row.push('i');
                } else {
                    if (r as usize) < 4 {
                        hist[r as usize] += 1;
                    }
                    row.push((b'0' + (r as u8).min(9)) as char);
                }
            }
            eprintln!("{row}");
        }
        eprintln!(
            "ref histogram: {hist:?}   num_ref_active={} refs.len()={}   OUT-OF-RANGE={}",
            self.num_ref_active,
            self.refs.len(),
            hist.iter().skip(self.refs.len()).sum::<usize>()
        );
        let list: Vec<String> = self
            .refs
            .iter()
            .enumerate()
            .map(|(i, f)| {
                // A synthesized frame_num-gap frame is uniform grey with w4 == 0;
                // flag it, because it silently displaces real pictures in the list.
                let synth = if f.w4 == 0 { " SYNTH-GREY" } else { "" };
                format!("[{i}] poc={} fn={}{synth}", f.poc, f.frame_num)
            })
            .collect();
        eprintln!("  RefPicList0: {}", list.join("  "));
    }

    pub fn deblock(&mut self, offset_a: i32, offset_b: i32) {
        self.edc_flush(); // backstop: no pixel job may survive to filtering
        self.dump_mb_map();
        // ROW MODE: finish any rows not derived during decode (mid-row slice
        // ends, error paths) FIRST, while `self` is still mutably borrowable.
        if rowdb_on() {
            while self.bs_rows < self.mb_h {
                let r = self.bs_rows;
                self.derive_bs_row(r);
                self.bs_rows += 1;
            }
        }
        // Deblock boundary strength uses the *transform block's* coded status. For
        // an 8×8-transform macroblock the unit is the whole 8×8, so every 4×4 cell
        // shares the 8×8's coefficient presence (OR of its four sub-block counts)
        // — distinct from the per-sub-block `nnz_y` used for the CAVLC nC context.
        // Only differs from `nnz_y` when some MB uses the 8×8 transform (High
        // profile). On Baseline (no 8×8) it's identical — skip the clone + rewrite.
        let nnz_db_storage;
        let nnz_db: &[u8] = if self.mb_t8x8.iter().any(|&t| t) {
            let mut n = self.nnz_y.clone();
            let w4 = self.mb_w * 4;
            for mb_y in 0..self.mb_h {
                for mb_x in 0..self.mb_w {
                    if !self.mb_t8x8[mb_y * self.mb_w + mb_x] {
                        continue;
                    }
                    for b8 in 0..4 {
                        let (bx, by) = (mb_x * 4 + (b8 % 2) * 2, mb_y * 4 + (b8 / 2) * 2);
                        let any = (0..2).any(|sy| (0..2).any(|sx| self.nnz_y[(by + sy) * w4 + (bx + sx)] > 0));
                        for sy in 0..2 {
                            for sx in 0..2 {
                                n[(by + sy) * w4 + (bx + sx)] = u8::from(any);
                            }
                        }
                    }
                }
            }
            nnz_db_storage = n;
            &nnz_db_storage
        } else {
            &self.nnz_y
        };
        // Map per-block reference indices to a stable picture identity (POC) so
        // the boundary-strength comparison recognises the same picture across lists.
        // ≤16-entry ref→POC maps; the frame-wide pre-mapped Vec shims this
        // replaces cost 230-460 KB + 57,600 mapped elements PER FRAME (WHYS
        // Part 15 item 2) — `pack_frame` now maps per block via `poc0`/`poc1`.
        let poc0: Vec<i32> = self.refs.iter().map(|f| f.poc).collect();
        let poc1: Vec<i32> = self.refs1.iter().map(|f| f.poc).collect();
        let mut info = rusty_h264_common::deblock::BlockInfo {
            inter: &self.inter_y,
            nnz: nnz_db,
            mv: &self.mv_y,
            ref_id: &self.ref_idx_y,
            mv1: &self.mv1,
            ref_id1: if poc1.is_empty() { &[] } else { &self.ref_idx1 },
            w4: self.mb_w * 4,
            t8x8: &self.mb_t8x8,
            bs: &[],
            poc0: &poc0,
            poc1: &poc1,
            kind: &self.mb_kind,
        };
        // ROW MODE (R2): rows were derived during decode; the remainder was
        // finished above (before `info` borrowed the grids). Fallback: the
        // Part 16/17 picture-end precompute; `RS_H264_BS_PRE=0` further falls
        // back to the pack-then-derive-in-loop pipeline.
        let bs_store;
        if rowdb_on() {
            bs_store = std::mem::take(&mut self.bs_frame);
            info.bs = &bs_store;
        } else if !std::env::var_os("RS_H264_BS_PRE").is_some_and(|v| v == "0") {
            let mut buf = Vec::new();
            rusty_h264_common::deblock::precompute_bs_frame(&info, self.mb_w, self.mb_h, &mut buf);
            bs_store = buf;
            info.bs = &bs_store;
        } else {
            bs_store = Vec::new();
        }
        let first_row = if rowdb_on() { self.flt_rows } else { 0 };
        rusty_h264_common::deblock::filter_frame_rows(
            &mut self.rec_y,
            &mut self.rec_u,
            &mut self.rec_v,
            self.mb_w,
            self.mb_h,
            first_row..self.mb_h,
            &self.mb_qp,
            self.chroma_qp_offset,
            offset_a,
            offset_b,
            &info,
        );
        drop(info);
        if rowdb_on() {
            self.bs_frame = bs_store;
        }
    }

    /// Crops the reconstructed coded-size planes to the display window.
    /// `into_frame`, additionally handing the per-picture grids back for reuse by
    /// the next picture. See `GridPool` for why this is worth doing.
    pub fn into_frame_recycle(mut self, crop_r: usize, crop_b: usize) -> (YuvFrame, GridPool) {
        let [c0, c1] = std::mem::take(&mut self.nnz_c);
        let pool = GridPool {
            bits_per_mb: self.bits_per_mb,
            mb_qp: std::mem::take(&mut self.mb_qp),
            bs_frame: std::mem::take(&mut self.bs_frame),
            pk_prev: std::mem::take(&mut self.pk_prev),
            pk_cur: std::mem::take(&mut self.pk_cur),
            nnz_dbr: std::mem::take(&mut self.nnz_dbr),
            bak_y: std::mem::take(&mut self.bak_y),
            bak_u: std::mem::take(&mut self.bak_u),
            bak_v: std::mem::take(&mut self.bak_v),
            nnz_y: std::mem::take(&mut self.nnz_y),
            nnz_c0: c0,
            nnz_c1: c1,
            modes_y: std::mem::take(&mut self.modes_y),
            coded_y: std::mem::take(&mut self.coded_y),
            mv_y: std::mem::take(&mut self.mv_y),
            inter_y: std::mem::take(&mut self.inter_y),
            ref_idx_y: std::mem::take(&mut self.ref_idx_y),
            mv1: std::mem::take(&mut self.mv1),
            ref_idx1: std::mem::take(&mut self.ref_idx1),
            mb_t8x8: std::mem::take(&mut self.mb_t8x8),
            mb_kind: std::mem::take(&mut self.mb_kind),
        };
        (self.into_frame(crop_r, crop_b), pool)
    }

    pub fn into_frame(self, crop_r: usize, crop_b: usize) -> YuvFrame {
        // No cropping (the common case): the reconstruction planes ARE the output —
        // move them out instead of allocating + copying three full planes per frame.
        if crop_r == 0 && crop_b == 0 {
            return YuvFrame {
                width: self.cw,
                height: self.ch,
                y: self.rec_y,
                u: self.rec_u,
                v: self.rec_v,
            };
        }
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
// ---- CABAC binarization engine helpers (openh264 cabac_decoder.cpp) ----

/// Unary bin (`DecodeUnaryBinCabac`): bin0 at `ctx`; if 1, count bins at `ctx+off`
/// (including the terminating 0) until a 0.
fn cabac_unary(cab: &mut crate::cabac::Cabac, ctx: usize, off: usize) -> u32 {
    if cab.decode_decision(ctx) == 0 {
        return 0;
    }
    let mut sym = 0;
    loop {
        let bin = cab.decode_decision(ctx + off);
        sym += 1;
        // Cap the unary run: no valid H.264 element coded through this helper
        // (mb_qp_delta) exceeds a few dozen bins, but on malformed / buffer-exhausted
        // input the arithmetic engine keeps yielding 1s (it zero-fills past the end),
        // which would loop forever. 512 is far beyond any legal value.
        if bin == 0 || sym >= 512 {
            break;
        }
    }
    sym
}

/// k-th order Exp-Golomb in bypass (`DecodeExpBypassCabac`).
fn cabac_exp_bypass(cab: &mut crate::cabac::Cabac, mut count: i32) -> u32 {
    let mut sym = 0u32;
    loop {
        let c = cab.decode_bypass();
        if c == 1 {
            sym += 1 << count;
            count += 1;
        }
        if c == 0 || count == 16 {
            break;
        }
    }
    let mut sym2 = 0u32;
    while count > 0 {
        count -= 1;
        if cab.decode_bypass() != 0 {
            sym2 |= 1 << count;
        }
    }
    sym + sym2
}

/// UEG0 coeff-level suffix (`DecodeUEGLevelCabac`): TU prefix at `ctx` (≤13) then an
/// EG0 bypass suffix.
fn cabac_ueg_level(cab: &mut crate::cabac::Cabac, ctx: usize) -> u32 {
    if cab.decode_decision(ctx) == 0 {
        return 0;
    }
    let mut code = 0u32;
    let mut count = 1;
    let mut tmp;
    loop {
        tmp = cab.decode_decision(ctx);
        code += 1;
        count += 1;
        if tmp == 0 || count == 13 {
            break;
        }
    }
    if tmp != 0 {
        code += cabac_exp_bypass(cab, 0) + 1;
    }
    code
}

/// `mb_qp_delta` CABAC (`ParseDeltaQpCabac`): ctxIdxOffset 60, ctxInc = (prev delta ≠ 0).
fn parse_mb_qp_delta_cabac(cab: &mut crate::cabac::Cabac, last_delta_qp: &mut i32) -> i32 {
    const O: usize = 60;
    let ctx_inc = (*last_delta_qp != 0) as usize;
    let mut qp_delta = 0;
    if cab.decode_decision(O + ctx_inc) != 0 {
        let code = cabac_unary(cab, O + 2, 1) + 1;
        qp_delta = ((code + 1) >> 1) as i32;
        if code & 1 == 0 {
            qp_delta = -qp_delta;
        }
    }
    *last_delta_qp = qp_delta;
    qp_delta
}

/// z-order block → padded (8-stride) nzc-cache index (openh264 g_kCacheNzcScanIdx):
/// 16 luma, 4 Cb, 4 Cr. Top neighbour = cache[idx-8], left = cache[idx-1].
const NZC_CACHE: [usize; 24] = [
    9, 10, 17, 18, 11, 12, 19, 20, 25, 26, 33, 34, 27, 28, 35, 36, // luma
    14, 15, 22, 23, // Cb
    38, 39, 46, 47, // Cr
];

// g_kBlockCat2CtxOffset* + maxPos/maxC2, indexed by CABAC res-property (1..10; 0 unused).
const RES_MAXPOS: [i32; 11] = [0, 15, 14, 15, 3, 14, 63, 3, 3, 14, 14];
const RES_MAXC2: [i32; 11] = [0, 4, 4, 4, 3, 4, 4, 3, 3, 4, 4];
const RES_CBF: [usize; 11] = [0, 0, 4, 8, 12, 16, 0, 12, 12, 16, 16];
const RES_MAP: [usize; 11] = [0, 0, 15, 29, 44, 47, 0, 44, 44, 47, 47];
// Index 6 (luma 8×8) = 199 so that 227+199 = 426 and 232+199 = 431 — the spec's
// coeff_abs_level_minus1 base for ctxBlockCat 5 and its >1-bin sub-block.
const RES_ONE: [usize; 11] = [0, 0, 10, 20, 30, 39, 199, 30, 30, 39, 39];
// res-property values (post GetMbResProperty, CABAC): the ctx-table index.
const RP_I16_DC: usize = 1;
const RP_I16_AC: usize = 2;
const RP_LUMA_4X4: usize = 3;
const RP_CHROMA_DC: usize = 7; // U (V=8, same offsets)
const RP_CHROMA_AC: usize = 9; // U (V=10, same offsets)
/// Luma 8×8 (ctxBlockCat 5). Its RES_MAP/RES_CBF entries stay 0: cat 5 does NOT
/// share the `105 + off` / `166 + off` context bases the 4×4 categories use — it
/// has its own absolute bases (402 sig, 417 last) and its own per-position
/// ctxIdxInc maps below. RES_ONE[6] = 199 IS used, because 227 + 199 = 426 and
/// 232 + 199 = 431 reproduce the spec's coeff_abs_level_minus1 base exactly, so
/// the level loop needs no special case at all.
const RP_LUMA_8X8: usize = 6;

// SIG8X8 / LAST8X8 moved to `rusty_h264_common::cabac_tables` (R6-1) so the encoder's
// ctxBlockCat 5 writer shares the exact spec data this reader is validated against.
use rusty_h264_common::cabac_tables::{LAST8X8, SIG8X8};

/// One residual block (openh264 `ParseResidualBlockCabac`), generic over the 5 CABAC
/// block categories. `rp` selects the context offsets. DC categories (I16 luma DC,
/// chroma DC) take the cbf context from the per-MB `cbf_dc` bitmask + neighbour MB DC
/// cbf; AC categories from the padded nzc cache. Returns totalCoeffNum.
#[allow(clippy::too_many_arguments)]
fn parse_residual_cabac(
    cab: &mut crate::cabac::Cabac,
    nzc: &mut [u8; 48],
    cbf_dc: &mut u16,
    iz: usize,
    rp: usize,
    is_intra: bool,
    ndc: (Option<u16>, Option<u16>), // (top MB cbf_dc, left MB cbf_dc); None = unavailable
    out: &mut [i32],                 // scan-order coefficients written here (len ≥ maxPos+1)
) -> u32 {
    // The CABAC residual parse IS the decoder's entropy stage on Main-profile
    // streams — it was invisible (a ~47% residue) until this scope named it.
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Entropy);
    // ---- coded_block_flag ----
    // ctxBlockCat 5 is the ONLY category with no coded_block_flag: its presence is
    // inferred from CodedBlockPatternLuma, so parsing one here would desync.
    let is8 = rp == RP_LUMA_8X8;
    let is_dc = rp == RP_I16_DC || rp == RP_CHROMA_DC || rp == RP_CHROMA_DC + 1;
    let (mut na, mut nb) = (is_intra as u8, is_intra as u8);
    let scan = NZC_CACHE[iz.min(23)];
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
    if !is8 {
        let _sg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EntCbf);
        let cbf = cab.decode_decision(85 + RES_CBF[rp] + (na + (nb << 1)) as usize);
        if cbf == 0 {
            if !is_dc {
                nzc[scan] = 0;
            }
            return 0;
        }
        if is_dc {
            *cbf_dc |= 1 << rp;
        }
    }
    // ---- significance map ----
    let maxpos = RES_MAXPOS[rp] as usize;
    // cat 5 uses its own absolute bases; the 4×4 categories share 105/166 + offset.
    let (map, last) = if is8 { (402, 417) } else { (105 + RES_MAP[rp], 166 + RES_MAP[rp]) };
    // SPARSE significance map: record each significant POSITION in `pos[..n]`
    // instead of marking a dense 64-entry array. Three costs disappear — the
    // 256-byte `sig` zeroing per call, the level loop's data-dependent
    // `sig[i] != 0` re-scan of every position (a branch mispredict per
    // transition on typical 2-4-coeff blocks), and the final dense copy into
    // `out`. Bin ORDER is unchanged: levels were decoded at descending
    // significant positions, which is exactly `pos[..n]` reversed.
    //
    // CONTRACT with the callers (all 10 sites): `out` is freshly zeroed, so
    // writing only the significant entries leaves the same contents the dense
    // copy produced. A reused non-zero `out` would be a correctness bug.
    let mut pos = [0u8; 64];
    let mut n = 0usize;
    let mut last_hit = false;
    let _sg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EntSig);
    for i in 0..maxpos {
        // 4×4: ctxIdxInc IS the scan position. 8×8: it comes from the folded maps.
        let (mi, li) = if is8 { (SIG8X8[i] as usize, LAST8X8[i] as usize) } else { (i, i) };
        if cab.decode_decision(map + mi) != 0 {
            pos[n] = i as u8;
            n += 1;
            if cab.decode_decision(last + li) != 0 {
                last_hit = true;
                break;
            }
        }
    }
    if !last_hit {
        pos[n] = maxpos as u8;
        n += 1;
    }
    let coeff_num = n as u32;
    // ---- levels ----
    let one = 227 + RES_ONE[rp];
    let abs = 232 + RES_ONE[rp];
    let maxc2 = RES_MAXC2[rp];
    let (mut c1, mut c2) = (1i32, 0i32);
    drop(_sg);
    let _lg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EntLvl);
    for k in (0..n).rev() {
        let mut level = 1 + cab.decode_decision(one + c1 as usize) as i32;
        if level == 2 {
            level += cabac_ueg_level(cab, abs + c2 as usize) as i32;
            c2 = (c2 + 1).min(maxc2);
            c1 = 0;
        } else if c1 != 0 {
            c1 = (c1 + 1).min(4);
        }
        if cab.decode_bypass() != 0 {
            level = -level;
        }
        out[pos[k] as usize] = level;
    }
    if is8 {
        // One 8×8 covers four consecutive z-order 4×4 cells. Every later
        // coded_block_flag ctxIdxInc reads this cache, so all four must carry the
        // count — writing only `scan` would corrupt the NEXT macroblock's contexts.
        for k in 0..4 {
            nzc[NZC_CACHE[(iz + k).min(23)]] = coeff_num as u8;
        }
    } else if !is_dc {
        nzc[scan] = coeff_num as u8;
    }
    coeff_num
}


// ============================================================================
// Entropy-decouple E2: the OWNED pixel context that crosses the thread
// boundary (docs/entropy-decouple-plan.md). The worker owns the planes, the
// backup rows, the DPB Arcs and its own qp/t8/bs grids (fed by Row messages);
// the parse thread keeps every syntax grid. The methods below are ports of
// the FrameDecoder pixel halves — grid writes removed (parse commits its own
// grids), motion carried in the job instead of re-gathered.
// ============================================================================

/// Messages from the parse thread to the pixel worker.
enum EdcMsg {
    Job(EdcJob),
    /// A ROW's worth of pixel jobs in one message (D10).
    ///
    /// The seam sent ONE message per macroblock: 208k sends per 60-frame pass
    /// against 2,596 rows. The overhead is per-JOB (channel lock, park/unpark)
    /// while the prize is proportional to pixel WORK, so the per-job send was
    /// dividing the payoff by ~80 for nothing. Batching per row cuts the
    /// synchronisation events by that factor and moves not one byte of pixel
    /// work off the worker.
    ///
    /// ORDER IS THE CORRECTNESS CONDITION: the worker must see a row's jobs
    /// before that row's `Row` filter message, so the batch is flushed at every
    /// row boundary, before `NeedCtx`, and at slice end.
    Batch(Vec<EdcJob>),
    /// A macroblock row finished parsing: install its qp/t8/bs and filter it.
    Row {
        r: usize,
        bs: Vec<rusty_h264_common::deblock::MbBs>,
        qp: Vec<u8>,
        t8: Vec<bool>,
    },
    /// An intra macroblock needs the planes on the parse thread: send the
    /// context over and wait for it to come back.
    NeedCtx,
}

pub(crate) struct PixelCtx {
    rec_y: Vec<u8>,
    rec_u: Vec<u8>,
    rec_v: Vec<u8>,
    bak_y: Vec<u8>,
    bak_u: Vec<u8>,
    bak_v: Vec<u8>,
    refs: Vec<crate::Ref>,
    refs1: Vec<crate::Ref>,
    weights: Option<WeightTable>,
    scaling: Option<[[i32; 16]; 6]>,
    scaling8: Option<[[i32; 64]; 2]>,
    cw: usize,
    ccw: usize,
    mb_w: usize,
    mb_h: usize,
    chroma_qp_offset: i32,
    flt_rows: usize,
    db_ena: bool,
    db_oa: i32,
    db_ob: i32,
    cur_qp: u8,
    qp_grid: Vec<u8>,
    t8_grid: Vec<bool>,
    bs_store: Vec<rusty_h264_common::deblock::MbBs>,
}

impl PixelCtx {
    fn chroma_qp_for(&self, qp: u8) -> u8 {
        rusty_h264_common::predict::chroma_qp(
            ((qp as i32 + self.chroma_qp_offset).clamp(0, 51)) as u8,
        )
    }

    fn dequant(&self, levels: &[i32; 16], qp: u8, list: usize) -> [i32; 16] {
        match &self.scaling {
            Some(sc) => dequantize_weighted(levels, qp, &sc[list]),
            None => dequantize(levels, qp),
        }
    }

    fn dequant_dc4(&self, level: i32, qp: u8, list: usize) -> i32 {
        rusty_h264_common::transform::dequantize_dc4(
            level,
            qp,
            self.scaling.as_ref().map(|sc| sc[list][0]),
        )
    }

    fn inv_quant8(&self, raster: &[i32; 64], qp: u8, list: usize) -> [i32; 64] {
        match &self.scaling8 {
            Some(sc) => inverse_quant_8x8(raster, qp, &sc[list]),
            None => inverse_quant_8x8(raster, qp, &[16i32; 64]),
        }
    }

    fn dequant_chroma_dc(&self, levels: &[i32; 4], qp: u8, list: usize) -> [i32; 4] {
        match &self.scaling {
            Some(sc) => inverse_quant_chroma_dc_weighted(levels, qp, sc[list][0]),
            None => inverse_quant_chroma_dc(levels, qp),
        }
    }

    fn weight_partition(
        &self,
        pred_y: &mut [u8; 256],
        c_pred: &mut [[u8; 64]; 2],
        list: usize,
        refi: usize,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
    ) {
        let Some(wt) = &self.weights else { return };
        for dy in 0..rh {
            for dx in 0..rw {
                let i = (ry + dy) * 16 + (rx + dx);
                pred_y[i] = wt.apply_luma(pred_y[i], list, refi);
            }
        }
        let (crx, cry, crw, crh) = (rx / 2, ry / 2, rw / 2, rh / 2);
        for cc in 0..2 {
            for dy in 0..crh {
                for dx in 0..crw {
                    let i = (cry + dy) * 8 + (crx + dx);
                    c_pred[cc][i] = wt.apply_chroma(c_pred[cc][i], list, refi, cc);
                }
            }
        }
    }

        fn recon_p_inter(&mut self, j: &PInterJob) {
        let mbw = self.mb_w;
        // `add_inter_residual` (and anything under it) reads `self.cur_qp`,
        // which at FLUSH time belongs to a later macroblock — replay must
        // restore this MB's qp. The x264 corpus (near-constant QP) could not
        // see this; the encoder's delta-QP roundtrip stream caught it.
        let saved_qp = self.cur_qp;
        self.cur_qp = j.qp;
                    // ---- Recon: motion-comp (per 4×4 luma / co-located 2×2 chroma using the
                    // committed grid MV — the 6-tap/bilinear filter is per-output-pixel, so
                    // per-block MC is bit-identical to per-partition MC) + residual add via the
                    // SAME reconstruct_4x4 as intra, with the MC output as the prediction.
                    let qp = j.qp;
                    let qpc = self.chroma_qp_for(qp);
                    let (w4r, w2r) = (mbw * 4, mbw * 2);
                    let mut pred_y = [0u8; 256];
                    let mut c_pred = [[0u8; 64]; 2];
                    {
                        // MC-CALL COALESCING (side-by-side descent, dec target #2): the old
                        // loop paid 16 mc_luma(4×4) + 32 mc_chroma(2×2) per MB regardless of
                        // partitioning — 48 calls even for a single-MV 16×16 MB, and the
                        // per-call glue around 2.4M calls was ~40% of decoding real-world
                        // (x264) streams. The 6-tap/bilinear filters are per-output-pixel,
                        // so merging blocks with equal (mv, ref) into one wider MC call is
                        // BIT-IDENTICAL; the rect ladder mirrors the partition shapes.
                        let _ms = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecMcStage);
                        let (rh16, cch) = (self.mb_h * 16, self.mb_h * 8);
                        // E2: the worker owns no syntax grids — the job carries the
                        // committed per-block motion (filled at parse time).
                        let gmv = j.gmv;
                        let mut gref = [0usize; 16];
                        for k in 0..16 {
                            gref[k] = (j.gref[k] as usize).min(self.refs.len() - 1);
                        }
                        // All blocks of the rect (in 4×4-block units) match its top-left?
                        let rect_eq = |x4: usize, y4: usize, w4: usize, h4: usize| -> bool {
                            let t = y4 * 4 + x4;
                            (0..h4).all(|dy| {
                                (0..w4).all(|dx| {
                                    let b = (y4 + dy) * 4 + (x4 + dx);
                                    gmv[b] == gmv[t] && gref[b] == gref[t]
                                })
                            })
                        };
                        let refs = &self.refs;
                        let (cw, ccw) = (self.cw, self.ccw);
                        let mut mc_rect = |x4: usize,
                                           y4: usize,
                                           w4: usize,
                                           h4: usize,
                                           pred_y: &mut [u8; 256],
                                           c_pred: &mut [[u8; 64]; 2]| {
                            let b = y4 * 4 + x4;
                            let (mv, reference) = (gmv[b], &refs[gref[b]]);
                            let (w, h) = (w4 * 4, h4 * 4);
                            // A FULL-WIDTH rect (w == 16, so x4 == 0) occupies contiguous
                            // whole rows of `pred_y` — the MC output layout and the
                            // destination layout coincide, so MC writes the prediction
                            // buffer DIRECTLY. The staging copy exists only for narrow
                            // rects, whose rows really are strided in `pred_y`. This is
                            // the diagnosis's "stage-boundary materialization" tax paid
                            // by the dominant 16×16/16×8 shapes: 256 B of `t` zeroing
                            // plus a 256 B copy per rect, for nothing.
                            if w == 16 {
                                rusty_h264_common::inter::with_mc_scratch(|scr| rusty_h264_common::inter::mc_luma_padded_pre(scr, &reference.py, reference.lstride(), crate::LPAD, cw, rh16, j.mbx * 16, j.mby * 16 + y4 * 4, w, h, mv.0, mv.1, &mut pred_y[y4 * 64..y4 * 64 + w * h]));
                            } else {
                                let mut t = [0u8; 256];
                                rusty_h264_common::inter::with_mc_scratch(|scr| rusty_h264_common::inter::mc_luma_padded_pre(scr, &reference.py, reference.lstride(), crate::LPAD, cw, rh16, j.mbx * 16 + x4 * 4, j.mby * 16 + y4 * 4, w, h, mv.0, mv.1, &mut t[..w * h]));
                                let _pb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                                for dy in 0..h {
                                    pred_y[(y4 * 4 + dy) * 16 + x4 * 4..][..w]
                                        .copy_from_slice(&t[dy * w..dy * w + w]);
                                }
                            }
                            let (cw4, ch4) = (w4 * 2, h4 * 2);
                            for cc in 0..2 {
                                let rc = if cc == 0 { &reference.pu } else { &reference.pv };
                                // Same full-width coincidence for chroma: cw4 == 8 rows
                                // are contiguous in the 8-wide `c_pred` plane.
                                if cw4 == 8 {
                                    mc_chroma_padded(rc, reference.cstride(), crate::CPAD, ccw, cch, j.mbx * 8, j.mby * 8 + y4 * 2, cw4, ch4, mv.0, mv.1, &mut c_pred[cc][y4 * 16..y4 * 16 + cw4 * ch4]);
                                    continue;
                                }
                                let mut tc = [0u8; 64];
                                mc_chroma_padded(rc, reference.cstride(), crate::CPAD, ccw, cch, j.mbx * 8 + x4 * 2, j.mby * 8 + y4 * 2, cw4, ch4, mv.0, mv.1, &mut tc[..cw4 * ch4]);
                                let _pb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::PredBuf);
                                for dy in 0..ch4 {
                                    c_pred[cc][(y4 * 2 + dy) * 8 + x4 * 2..][..cw4]
                                        .copy_from_slice(&tc[dy * cw4..dy * cw4 + cw4]);
                                }
                            }
                        };
                        if rect_eq(0, 0, 4, 4) {
                            mc_rect(0, 0, 4, 4, &mut pred_y, &mut c_pred);
                        } else if rect_eq(0, 0, 4, 2) && rect_eq(0, 2, 4, 2) {
                            mc_rect(0, 0, 4, 2, &mut pred_y, &mut c_pred);
                            mc_rect(0, 2, 4, 2, &mut pred_y, &mut c_pred);
                        } else if rect_eq(0, 0, 2, 4) && rect_eq(2, 0, 2, 4) {
                            mc_rect(0, 0, 2, 4, &mut pred_y, &mut c_pred);
                            mc_rect(2, 0, 2, 4, &mut pred_y, &mut c_pred);
                        } else {
                            for q in 0..4usize {
                                let (qx, qy) = ((q % 2) * 2, (q / 2) * 2);
                                if rect_eq(qx, qy, 2, 2) {
                                    mc_rect(qx, qy, 2, 2, &mut pred_y, &mut c_pred);
                                } else if rect_eq(qx, qy, 2, 1) && rect_eq(qx, qy + 1, 2, 1) {
                                    mc_rect(qx, qy, 2, 1, &mut pred_y, &mut c_pred);
                                    mc_rect(qx, qy + 1, 2, 1, &mut pred_y, &mut c_pred);
                                } else if rect_eq(qx, qy, 1, 2) && rect_eq(qx + 1, qy, 1, 2) {
                                    mc_rect(qx, qy, 1, 2, &mut pred_y, &mut c_pred);
                                    mc_rect(qx + 1, qy, 1, 2, &mut pred_y, &mut c_pred);
                                } else {
                                    for j in 0..4usize {
                                        mc_rect(qx + (j % 2), qy + (j / 2), 1, 1, &mut pred_y, &mut c_pred);
                                    }
                                }
                            }
                        }
                        // EXPLICIT WEIGHTED PREDICTION (spec 8.4.2.3). The CAVLC inter
                        // path weights each partition after MC; the MC-call-coalescing
                        // rewrite of this CABAC path lost it, and nothing caught that
                        // because the effect is invisible unless a stream actually
                        // carries non-default weights. x264's `weightp` DUPLICATES a
                        // reference and distinguishes the copy ONLY by its weights, so
                        // every macroblock picking the weighted index decoded unweighted
                        // -- a silent, accumulating luma drift.
                        //
                        // Applied per 4x4 block rather than per partition: the weight
                        // depends solely on the block's reference index, so the two are
                        // equivalent, and `gref` already holds it for every block
                        // regardless of which rect ladder rung ran.
                        if self.weights.is_some() {
                            for by in 0..4usize {
                                for bx in 0..4usize {
                                    let refi = gref[by * 4 + bx];
                                    self.weight_partition(
                                        &mut pred_y, &mut c_pred, 0, refi, bx * 4, by * 4, 4, 4,
                                    );
                                }
                            }
                        }
                    }
                    // Residual add — the SAME helper the B path uses (this inline
                    // copy was a duplicate; deduped when the zero-block fast path
                    // landed so both paths share it).
                    self.add_inter_residual(j.mbx, j.mby, &pred_y, &c_pred, &j.luma_scan, if j.t8 { Some(&j.luma8) } else { None }, &j.cdc, &j.cac, j.cbp_chroma, &j.nnzs);
    }

    fn recon_p_skip(&mut self, mb_x: usize, mb_y: usize, mv: (i32, i32)) {
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);

        let mut pred = [0u8; 256];
        let rf0 = &self.refs[0];
        mc_luma_padded(&rf0.py, rf0.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16, 16, 16, mv.0, mv.1, &mut pred);
        if let Some(wt) = &self.weights {
            for p in pred.iter_mut() {
                *p = wt.apply_luma(*p, 0, 0);
            }
        }
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::SkipRecon);
            for dy in 0..16 {
                let d = (mb_y * 16 + dy) * self.cw + mb_x * 16;
                self.rec_y[d..d + 16].copy_from_slice(&pred[dy * 16..dy * 16 + 16]);
            }
        }
        for c in 0..2 {
            let mut pc = [0u8; 64];
            let rf0 = &self.refs[0];
            let rc = if c == 0 { &rf0.pu } else { &rf0.pv };
            mc_chroma_padded(rc, rf0.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8, 8, 8, mv.0, mv.1, &mut pc);
            if let Some(wt) = &self.weights {
                for p in pc.iter_mut() {
                    *p = wt.apply_chroma(*p, 0, 0, c);
                }
            }
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for dy in 0..8 {
                let d = (mb_y * 8 + dy) * self.ccw + mb_x * 8;
                plane[d..d + 8].copy_from_slice(&pc[dy * 8..dy * 8 + 8]);
            }
        }
    }

    fn add_inter_residual(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        pred_y: &[u8; 256],
        c_pred: &[[u8; 64]; 2],
        luma_scan: &[[i32; 16]; 16],
        // `Some` when the macroblock carries transform_size_8x8_flag: four 8x8
        // blocks in 8x8 scan order, replacing the sixteen 4x4 luma blocks.
        luma8: Option<&[[i32; 64]; 4]>,
        cdc: &[[i32; 4]; 2],
        cac: &[[[i32; 16]; 4]; 2],
        cbp_chroma: u32,
        // Parsed totalCoeff per block, indexed exactly as the parse's `iz`:
        // [0..16] luma 4x4 z-order (for t8, the 8x8 count sits at `id8*4`),
        // [16..24] chroma AC as `16 + c*4 + id4`. The parser already counted
        // every significant coefficient; re-deriving the counts here scanned
        // 16-64 array elements per block (~400 loads/MB) for information the
        // caller was holding — the diagnosis's stage-boundary re-derivation tax.
        nnzs: &[u8; 24],
    ) {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecResidAdd);
        let qp = self.cur_qp;
        let qpc = self.chroma_qp_for(qp);
        let (w4r, w2r) = (self.mb_w * 4, self.mb_w * 2);
        if let Some(l8) = luma8 {
            // INTER 8x8 luma: same primitives the I_8x8 and CAVLC paths use.
            for b8 in 0..4usize {
                let (b8x, b8y) = (b8 % 2, b8 / 2);
                // Summed, not slot 0: with per-4x4 counts in the CAVLC case, slot 0
                // is only the first sub-block and can be 0 while the block is coded.
                let nnz: u32 = (0..4).map(|k| nnzs[b8 * 4 + k] as u32).sum();
                let res8 = if nnz == 0 {
                    [0i32; 64]
                } else {
                    let raster = un_scan_8x8(&l8[b8]);
                    // list 1 = INTER 8x8 luma scaling list (0 is the intra one).
                    self.inv_quant8(&raster, qp, 1)
                };
                // The 4x4 inter path marks coded_y per block; the 8x8 branch must too,
                // or a later intra macroblock's neighbour availability is wrong.
                let predb: [i32; 64] =
                    std::array::from_fn(|i| pred_y[(b8y * 8 + i / 8) * 16 + (b8x * 8 + i % 8)] as i32);
                let recon = add_residual_8x8(&res8, &predb);
                let (px, py) = (mb_x * 16 + b8x * 8, mb_y * 16 + b8y * 8);
                for dy in 0..8 {
                    for dx in 0..8 {
                        self.rec_y[(py + dy) * self.cw + (px + dx)] = recon[dy * 8 + dx];
                    }
                }
            }
        }
        for (blk, &(lbx, lby)) in LUMA_4X4_SCAN_XY.iter().enumerate() {
            if luma8.is_some() {
                break;
            }
            let nnz = nnzs[blk];
            let cw = self.cw;
            let p_off = (lby * 4) * 16 + lbx * 4;
            let r_off = (mb_y * 4 + lby) * 4 * cw + (mb_x * 4 + lbx) * 4;
            if nnz == 0 {
                // Zero residual → recon == prediction EXACTLY (the integer IDCT is
                // linear so zeros map to zeros, and pred is already 0..=255) — copy
                // the pred rows straight into the plane. On real (sparse-cbp)
                // streams this is MOST of the 4×4 blocks.
                for r in 0..4 {
                    self.rec_y[r_off + r * cw..r_off + r * cw + 4]
                        .copy_from_slice(&pred_y[p_off + r * 16..p_off + r * 16 + 4]);
                }
                continue;
            }
            // DC-ONLY: the sole significant coefficient is scan position 0 (the
            // zig-zag starts at DC, and un_scan keeps it at raster 0), so the
            // whole dequant + IDCT collapses to one multiply and a flat add.
            if nnz == 1 && luma_scan[blk][0] != 0 {
                let f = self.dequant_dc4(luma_scan[blk][0], qp, 3);
                reconstruct_4x4_dc_into((f + 32) >> 6, pred_y, p_off, 16, &mut self.rec_y, r_off, cw);
            } else {
                // Fused un-scan + dequant over ONLY the significant coefficients,
                // then IDCT + add + clip straight into the plane — no `qb`, no
                // `deq`-from-dense, no `predb` gather, no `s`, no `store` call.
                //
                // HYBRID: the scatter walks scan positions with a data-dependent
                // branch per slot, which beats the branchless dense 16-multiply
                // loop only while the block is SPARSE. The DC/zero fast paths
                // already removed the sparsest blocks, so the population here
                // skews denser — above ~6 coefficients the dense loop wins.
                let deq = if nnz <= 6 {
                    dequant_scatter_4x4(&luma_scan[blk], nnz, 0, qp, self.scaling.as_ref().map(|sc| &sc[3]))
                } else {
                    self.dequant(&un_scan_4x4_dcac(&luma_scan[blk]), qp, 3)
                };
                reconstruct_4x4_into(&deq, pred_y, p_off, 16, &mut self.rec_y, r_off, cw);
            }
        }
        let mut c_dc = [[0i32; 4]; 2];
        if cbp_chroma != 0 {
            for c in 0..2 {
                c_dc[c] = self.dequant_chroma_dc(&cdc[c], qpc, 4 + c);
            }
        }
        let ccw = self.ccw;
        for c in 0..2 {
            for &(bx, by) in &CHROMA_4X4_SCAN_XY {
                let mut ac_nz = false;
                if cbp_chroma == 2 {
                    let n = nnzs[16 + c * 4 + by * 2 + bx];
                    ac_nz = n != 0;
                }
                let dc = c_dc[c][by * 2 + bx];
                let p_off = (by * 4) * 8 + bx * 4;
                let r_off = (mb_y * 2 + by) * 4 * ccw + (mb_x * 2 + bx) * 4;
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                if dc == 0 && !ac_nz {
                    // Zero residual (no AC, zero DC) → recon == prediction exactly.
                    for r in 0..4 {
                        plane[r_off + r * ccw..r_off + r * ccw + 4]
                            .copy_from_slice(&c_pred[c][p_off + r * 8..p_off + r * 8 + 4]);
                    }
                    continue;
                }
                // DC-ONLY (no coded AC — covers every cbp_chroma==1 block and the
                // AC-empty blocks of cbp_chroma==2): the chroma DC arrives ALREADY
                // dequantized, so the residual is `(dc + 32) >> 6` flat.
                if !ac_nz {
                    reconstruct_4x4_dc_into((dc + 32) >> 6, &c_pred[c], p_off, 8, plane, r_off, ccw);
                    continue;
                }
                // AC-only scan: index i is overall scan position i+1 (ac_shift=1).
                // Same sparse/dense hybrid as luma.
                let n = nnzs[16 + c * 4 + by * 2 + bx];
                let mut deq = if n <= 6 {
                    dequant_scatter_4x4(&cac[c][by * 2 + bx], n, 1, qpc, self.scaling.as_ref().map(|sc| &sc[4 + c]))
                } else {
                    let mut ac = [0i32; 16];
                    un_scan_4x4_ac_into(&cac[c][by * 2 + bx], &mut ac);
                    // Free-fn dequant: `self.dequant` borrows all of `self`, which
                    // conflicts with the live `plane` (&mut self.rec_u/v) borrow.
                    match &self.scaling {
                        Some(sc) => dequantize_weighted(&ac, qpc, &sc[4 + c]),
                        None => dequantize(&ac, qpc),
                    }
                };
                deq[0] = dc;
                reconstruct_4x4_into(&deq, &c_pred[c], p_off, 8, plane, r_off, ccw);
            }
        }
    }

    fn filter_row(&mut self, r: usize) {
        // The precomputed consumer path reads ONLY `bs` + `t8x8` (+ the qp grid
        // passed as a parameter) — verified when the path landed (WHYS Part 15).
        let info = rusty_h264_common::deblock::BlockInfo {
            inter: &[],
            nnz: &[],
            mv: &[],
            ref_id: &[],
            mv1: &[],
            ref_id1: &[],
            w4: self.mb_w * 4,
            t8x8: &self.t8_grid,
            bs: &self.bs_store,
            poc0: &[],
            poc1: &[],
            kind: &[],
        };
        rusty_h264_common::deblock::filter_frame_rows(
            &mut self.rec_y,
            &mut self.rec_u,
            &mut self.rec_v,
            self.mb_w,
            self.mb_h,
            r..r + 1,
            &self.qp_grid,
            self.chroma_qp_offset,
            self.db_oa,
            self.db_ob,
            &info,
        );
    }

    fn save_bak(&mut self, r: usize) {
        let y0 = (r * 16 + 15) * self.cw;
        self.bak_y.copy_from_slice(&self.rec_y[y0..y0 + self.cw]);
        let c0 = (r * 8 + 7) * self.ccw;
        self.bak_u.copy_from_slice(&self.rec_u[c0..c0 + self.ccw]);
        self.bak_v.copy_from_slice(&self.rec_v[c0..c0 + self.ccw]);
    }
}

/// The pixel worker: replays jobs in parse order, filters rows as their
/// messages arrive, and hands the whole context to the parse thread (and
/// back) around intra macroblocks. Returns the context at slice end.
/// E2 SEAM COUNTERS (D7). Deterministic — one run is the verdict, no pinning.
/// `RS_H264_EDC_STATS=1` prints at decode end. Counts, not clocks: the question
/// "does one intra macroblock drain the pipeline" is a COUNT question.
pub(crate) mod edcstat {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub static NEEDCTX: AtomicU64 = AtomicU64::new(0);
    pub static JOBS: AtomicU64 = AtomicU64::new(0);
    pub static ROWS: AtomicU64 = AtomicU64::new(0);
    pub static ROWBYTES: AtomicU64 = AtomicU64::new(0);
    pub static MBS: AtomicU64 = AtomicU64::new(0);
    pub static J_INTER: AtomicU64 = AtomicU64::new(0);
    pub static DOUBLED: AtomicU64 = AtomicU64::new(0);
    pub static J_NORES_SENT: AtomicU64 = AtomicU64::new(0);
    pub static BATCHES: AtomicU64 = AtomicU64::new(0);
    pub static DISPATCH_ON: AtomicU64 = AtomicU64::new(0);
    pub static DISPATCH_SEEN: AtomicU64 = AtomicU64::new(0);
    pub static J_INTER_NORES: AtomicU64 = AtomicU64::new(0);
    #[inline]
    pub fn bump(c: &AtomicU64, n: u64) {
        if on() {
            c.fetch_add(n, Relaxed);
        }
    }
    pub fn on() -> bool {
        static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *V.get_or_init(|| std::env::var_os("RS_H264_EDC_STATS").is_some())
    }
    pub fn report() {
        if !on() {
            return;
        }
        eprintln!(
            "EDCDISPATCH threaded_slices={} eligible_slices={}",
            DISPATCH_ON.load(Relaxed), DISPATCH_SEEN.load(Relaxed)
        );
        eprintln!(
            "EDCSIZE EdcMsg={} EdcJob={} PInterJob={} BJob={}",
            std::mem::size_of::<super::EdcMsg>(),
            std::mem::size_of::<super::EdcJob>(),
            std::mem::size_of::<super::PInterJob>(),
            std::mem::size_of::<super::BJob>(),
        );
        let (n, j, r, b, m) = (
            NEEDCTX.load(Relaxed), JOBS.load(Relaxed), ROWS.load(Relaxed),
            ROWBYTES.load(Relaxed), MBS.load(Relaxed),
        );
        eprintln!(
            "EDCSTAT needctx={n} jobs={j} rows={r} rowbytes={b} mbs={m} batches={} jobs_per_batch={:.1} needctx_per_1k_mb={:.1} jobs_per_needctx={:.1}",
            BATCHES.load(Relaxed),
            j as f64 / BATCHES.load(Relaxed).max(1) as f64,
            1000.0 * n as f64 / m.max(1) as f64,
            j as f64 / n.max(1) as f64
        );
        let (ji, jn) = (J_INTER.load(Relaxed), J_INTER_NORES.load(Relaxed));
        eprintln!(
            "EDCMIX doubled={} nores_sent={} inter={ji} inter_no_residual={jn} ({:.1}% of inter) wasted_bytes={:.1} MB of {:.1} MB total inter payload",
            DOUBLED.load(Relaxed),
            J_NORES_SENT.load(Relaxed),
            100.0 * jn as f64 / ji.max(1) as f64,
            (jn * 2784) as f64 / 1.048576e6,
            (ji * 2784) as f64 / 1.048576e6,
        );
    }
}

fn edc_worker(
    mut ctx: PixelCtx,
    rx: std::sync::mpsc::Receiver<EdcMsg>,
    ctx_tx: std::sync::mpsc::Sender<PixelCtx>,
    back_rx: std::sync::mpsc::Receiver<PixelCtx>,
) -> PixelCtx {
    while let Ok(msg) = rx.recv() {
        match msg {
            EdcMsg::Batch(jobs) => {
                for j in jobs {
                    match j {
                        EdcJob::Skip { mbx, mby, mv } => ctx.recon_p_skip(mbx, mby, mv),
                        EdcJob::Inter(j) => ctx.recon_p_inter(&j),
                        EdcJob::InterNoRes(j) => ctx.recon_p_inter(&j.to_full()),
                        EdcJob::B(j) => ctx.recon_b(&j),
                        EdcJob::BSkip { mbx, mby, regions } => ctx.recon_b_skip(mbx, mby, &regions),
                    }
                }
            }
            EdcMsg::Job(EdcJob::Skip { mbx, mby, mv }) => ctx.recon_p_skip(mbx, mby, mv),
            EdcMsg::Job(EdcJob::Inter(j)) => ctx.recon_p_inter(&j),
            EdcMsg::Job(EdcJob::InterNoRes(j)) => ctx.recon_p_inter(&j.to_full()),
            EdcMsg::Job(EdcJob::B(j)) => ctx.recon_b(&j),
            EdcMsg::Job(EdcJob::BSkip { mbx, mby, regions }) => ctx.recon_b_skip(mbx, mby, &regions),
            EdcMsg::Row { r, bs, qp, t8 } => {
                let (w, base) = (ctx.mb_w, r * ctx.mb_w);
                ctx.bs_store[base..base + w].copy_from_slice(&bs);
                ctx.qp_grid[base..base + w].copy_from_slice(&qp);
                ctx.t8_grid[base..base + w].copy_from_slice(&t8);
                if ctx.db_ena {
                    ctx.save_bak(r);
                    ctx.filter_row(r);
                    ctx.flt_rows = r + 1;
                }
            }
            EdcMsg::NeedCtx => {
                ctx_tx.send(ctx).expect("parse thread alive");
                ctx = back_rx.recv().expect("ctx returned after intra");
            }
        }
    }
    ctx
}


/// One motion-compensation region of a B macroblock, recorded at parse time
/// (E3). Weights are the RESOLVED implicit pair — computing them needs the
/// ref lists' POCs, which are parse-side state.
pub(crate) struct BRegion {
    px: usize,
    py: usize,
    rw: usize,
    rh: usize,
    refi0: i32,
    refi1: i32,
    mv0: (i32, i32),
    mv1: (i32, i32),
    w: Option<(i32, i32)>,
}

/// A B macroblock's deferred pixel work: replay the regions into fresh
/// prediction buffers, then either copy them out (skip/direct, no residual)
/// or run the residual add.
pub(crate) struct BJob {
    mbx: usize,
    mby: usize,
    t8: bool,
    qp: u8,
    cbp_chroma: u32,
    skip: bool,
    regions: Vec<BRegion>,
    luma_scan: [[i32; 16]; 16],
    luma8: [[i32; 64]; 4],
    cdc: [[i32; 4]; 2],
    cac: [[[i32; 16]; 4]; 2],
    nnzs: [u8; 24],
}

impl PixelCtx {
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
        wparam: Option<(i32, i32)>,
    ) {
        let _gb = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBMc);
        // Malformed-stream armor, mirroring the P path: now that B slices actually
        // PARSE ref_idx (they used to be hardcoded to 0), a mutated stream can hand
        // us an index past the end of either list. Clamp rather than panic — the
        // crate is `forbid(unsafe_code)` and fuzz-gated to never panic, and a
        // wrong picture on garbage input carries no conformance duty.
        let refi0 = if refi0 >= 0 { (refi0 as usize).min(self.refs.len().saturating_sub(1)) as i32 } else { -1 };
        let refi1 = if refi1 >= 0 { (refi1 as usize).min(self.refs1.len().saturating_sub(1)) as i32 } else { -1 };
        if (refi0 >= 0 && self.refs.is_empty()) || (refi1 >= 0 && self.refs1.is_empty()) {
            return;
        }
        let (ch, cch) = (self.mb_h * 16, self.mb_h * 8);
        // E3: implicit weights are PARSE-side (they read the ref lists' POCs);
        // the region carries the resolved pair.
        let weights = wparam;
        // Bi-prediction blend: the weights decision is LOOP-INVARIANT, so every
        // blend site below matches on `weights` ONCE and runs a branch-free
        // pixel loop — the unweighted `(p+q+1)>>1` average then autovectorizes
        // (the per-pixel closure this replaces hid the invariant behind a
        // capture, and its chroma form was a &dyn call PER PIXEL).
        // FULL-WIDTH regions (px == 0, rw == 16 — every 16×16/16×8 partition and
        // most direct regions) occupy contiguous rows of `pred_y`, so MC writes
        // the destination DIRECTLY: uni-pred needs no staging at all, bi-pred
        // stages only the second list and blends in place. The staging arrays
        // (512 B zeroed per call before this) now exist only on the branches
        // that read them. Same fusion as the P path's mc_rect (WHYS Part 8).
        let full = px == 0 && rw == 16;
        let _gl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBLuma);
        // One scratch borrow for the whole region — both bi-pred passes included.
        // The closure yields whether the arm already ran the chroma half (the
        // bi-pred full-width arm does, to keep its staging alive) — a plain
        // `return` inside would exit the CLOSURE only and chroma would run twice.
        let chroma_done = rusty_h264_common::inter::with_mc_scratch(|scr| match (refi0 >= 0, refi1 >= 0, full) {
            (true, false, true) => {
                let rf = &self.refs[refi0 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                false
            }
            (false, true, true) => {
                let rf = &self.refs1[refi1 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                false
            }
            (true, true, true) => {
                let rf = &self.refs[refi0 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut pred_y[py * 16..py * 16 + rw * rh]);
                let mut b = [0u8; 256];
                let rf = &self.refs1[refi1 as usize];
                rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut b[..rw * rh]);
                drop(_gl);
                let _gbl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBBlend);
                // SLICE-then-zip: proving the bounds ONCE lets rustc emit the whole
                // 256-byte average as 8 straight-line vpavgb ops (verified in
                // isolation, x86-64-v3); the indexed form kept a per-iteration
                // bounds check and a loop. A hand AVX2 kernel is refuted — the
                // compiler already emits the ideal instruction.
                let dst = &mut pred_y[py * 16..py * 16 + rw * rh];
                match weights {
                    None => {
                        for (d, s) in dst.iter_mut().zip(&b[..rw * rh]) {
                            *d = ((*d as u16 + *s as u16 + 1) >> 1) as u8;
                        }
                    }
                    Some((w0, w1)) => {
                        for (d, s) in dst.iter_mut().zip(&b[..rw * rh]) {
                            *d = ((*d as i32 * w0 + *s as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                        }
                    }
                }
                let _gc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBChroma);
                self.b_mc_chroma(mb_x, mb_y, px, py, rw, rh, refi0, mv0, refi1, mv1, c_pred, weights, cch);
                true
            }
            _ => {
                // Narrow region — rows are strided in `pred_y`; stage and copy.
                let (mut a, mut b) = ([0u8; 256], [0u8; 256]);
                if refi0 >= 0 {
                    let rf = &self.refs[refi0 as usize];
                    rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv0.0, mv0.1, &mut a[..rw * rh]);
                }
                if refi1 >= 0 {
                    let rf = &self.refs1[refi1 as usize];
                    rusty_h264_common::inter::mc_luma_padded_pre(scr, &rf.py, rf.lstride(), crate::LPAD, self.cw, ch, mb_x * 16 + px, mb_y * 16 + py, rw, rh, mv1.0, mv1.1, &mut b[..rw * rh]);
                }
                drop(_gl);
                let _gbl = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBBlend);
                match (refi0 >= 0, refi1 >= 0) {
                    (true, true) => {
                        for dy in 0..rh {
                            let (ar, br) = (&a[dy * rw..dy * rw + rw], &b[dy * rw..dy * rw + rw]);
                            let base = (py + dy) * 16 + px;
                            let dst = &mut pred_y[base..base + rw];
                            match weights {
                                None => {
                                    for ((d, p), q) in dst.iter_mut().zip(ar).zip(br) {
                                        *d = ((*p as u16 + *q as u16 + 1) >> 1) as u8;
                                    }
                                }
                                Some((w0, w1)) => {
                                    for ((d, p), q) in dst.iter_mut().zip(ar).zip(br) {
                                        *d = ((*p as i32 * w0 + *q as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                                    }
                                }
                            }
                        }
                    }
                    (true, false) => {
                        for dy in 0..rh {
                            let d = (py + dy) * 16 + px;
                            pred_y[d..d + rw].copy_from_slice(&a[dy * rw..dy * rw + rw]);
                        }
                    }
                    _ => {
                        for dy in 0..rh {
                            let d = (py + dy) * 16 + px;
                            pred_y[d..d + rw].copy_from_slice(&b[dy * rw..dy * rw + rw]);
                        }
                    }
                }
                false
            }
        });
        if chroma_done {
            return;
        }
        let _gc = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecBChroma);
        self.b_mc_chroma(mb_x, mb_y, px, py, rw, rh, refi0, mv0, refi1, mv1, c_pred, weights, cch);
    }

    fn b_mc_chroma(
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
        c_pred: &mut [[u8; 64]; 2],
        weights: Option<(i32, i32)>,
        cch: usize,
    ) {
        let (crx, cry, crw, crh) = (px / 2, py / 2, rw / 2, rh / 2);
        let full = crx == 0 && crw == 8;
        for c in 0..2 {
            match (refi0 >= 0, refi1 >= 0, full) {
                (true, false, true) => {
                    let rf = &self.refs[refi0 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                }
                (false, true, true) => {
                    let rf = &self.refs1[refi1 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                }
                (true, true, true) => {
                    let rf = &self.refs[refi0 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut c_pred[c][cry * 8..cry * 8 + crw * crh]);
                    let mut cb = [0u8; 64];
                    let rf = &self.refs1[refi1 as usize];
                    let pl = if c == 0 { &rf.pu } else { &rf.pv };
                    mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut cb[..crw * crh]);
                    let dst = &mut c_pred[c][cry * 8..cry * 8 + crw * crh];
                    match weights {
                        None => {
                            for (d, s) in dst.iter_mut().zip(&cb[..crw * crh]) {
                                *d = ((*d as u16 + *s as u16 + 1) >> 1) as u8;
                            }
                        }
                        Some((w0, w1)) => {
                            for (d, s) in dst.iter_mut().zip(&cb[..crw * crh]) {
                                *d = ((*d as i32 * w0 + *s as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                            }
                        }
                    }
                }
                _ => {
                    let (mut ca, mut cb) = ([0u8; 64], [0u8; 64]);
                    if refi0 >= 0 {
                        let rf = &self.refs[refi0 as usize];
                        let pl = if c == 0 { &rf.pu } else { &rf.pv };
                        mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv0.0, mv0.1, &mut ca[..crw * crh]);
                    }
                    if refi1 >= 0 {
                        let rf = &self.refs1[refi1 as usize];
                        let pl = if c == 0 { &rf.pu } else { &rf.pv };
                        mc_chroma_padded(pl, rf.cstride(), crate::CPAD, self.ccw, cch, mb_x * 8 + crx, mb_y * 8 + cry, crw, crh, mv1.0, mv1.1, &mut cb[..crw * crh]);
                    }
                    match (refi0 >= 0, refi1 >= 0) {
                        (true, true) => {
                            for dy in 0..crh {
                                let (pr, qr) = (&ca[dy * crw..dy * crw + crw], &cb[dy * crw..dy * crw + crw]);
                                let base = (cry + dy) * 8 + crx;
                                let dst = &mut c_pred[c][base..base + crw];
                                match weights {
                                    None => {
                                        for ((d, p), q) in dst.iter_mut().zip(pr).zip(qr) {
                                            *d = ((*p as u16 + *q as u16 + 1) >> 1) as u8;
                                        }
                                    }
                                    Some((w0, w1)) => {
                                        for ((d, p), q) in dst.iter_mut().zip(pr).zip(qr) {
                                            *d = ((*p as i32 * w0 + *q as i32 * w1 + 32) >> 6).clamp(0, 255) as u8;
                                        }
                                    }
                                }
                            }
                        }
                        (true, false) => {
                            for dy in 0..crh {
                                let d = (cry + dy) * 8 + crx;
                                c_pred[c][d..d + crw].copy_from_slice(&ca[dy * crw..dy * crw + crw]);
                            }
                        }
                        _ => {
                            for dy in 0..crh {
                                let d = (cry + dy) * 8 + crx;
                                c_pred[c][d..d + crw].copy_from_slice(&cb[dy * crw..dy * crw + crw]);
                            }
                        }
                    }
                }
            }
        }
    }

    /// B_Skip replay: regions into fresh prediction buffers, then the plane
    /// copy — no residual, no coefficient arrays.
    fn recon_b_skip(&mut self, mbx: usize, mby: usize, regions: &[BRegion]) {
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        for r in regions {
            self.b_mc(mbx, mby, r.px, r.py, r.rw, r.rh, r.refi0, r.mv0, r.refi1, r.mv1, &mut pred_y, &mut c_pred, r.w);
        }
        for dy in 0..16 {
            let d = (mby * 16 + dy) * self.cw + mbx * 16;
            self.rec_y[d..d + 16].copy_from_slice(&pred_y[dy * 16..dy * 16 + 16]);
        }
        for c in 0..2 {
            let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
            for dy in 0..8 {
                let d = (mby * 8 + dy) * self.ccw + mbx * 8;
                plane[d..d + 8].copy_from_slice(&c_pred[c][dy * 8..dy * 8 + 8]);
            }
        }
    }

    /// Replays one B macroblock's regions + residual (the worker half of the
    /// E3 seam). Mirrors the inline order exactly: MC regions in parse order
    /// into the prediction buffers, then the residual add (or the skip copy).
    fn recon_b(&mut self, j: &BJob) {
        let mut pred_y = [0u8; 256];
        let mut c_pred = [[0u8; 64]; 2];
        for r in &j.regions {
            self.b_mc(j.mbx, j.mby, r.px, r.py, r.rw, r.rh, r.refi0, r.mv0, r.refi1, r.mv1, &mut pred_y, &mut c_pred, r.w);
        }
        if j.skip {
            for dy in 0..16 {
                let d = (j.mby * 16 + dy) * self.cw + j.mbx * 16;
                self.rec_y[d..d + 16].copy_from_slice(&pred_y[dy * 16..dy * 16 + 16]);
            }
            for c in 0..2 {
                let plane = if c == 0 { &mut self.rec_u } else { &mut self.rec_v };
                for dy in 0..8 {
                    let d = (j.mby * 8 + dy) * self.ccw + j.mbx * 8;
                    plane[d..d + 8].copy_from_slice(&c_pred[c][dy * 8..dy * 8 + 8]);
                }
            }
        } else {
            self.cur_qp = j.qp;
            self.add_inter_residual(j.mbx, j.mby, &pred_y, &c_pred, &j.luma_scan, if j.t8 { Some(&j.luma8) } else { None }, &j.cdc, &j.cac, j.cbp_chroma, &j.nnzs);
        }
    }
}

/// One deferred pixel-reconstruction job (entropy-decouple E1 seam).
enum EdcJob {
    Skip { mbx: usize, mby: usize, mv: (i32, i32) },
    Inter(Box<PInterJob>),
    B(Box<BJob>),
    /// B_Skip / no-residual direct: regions only. The full `BJob` carried
    /// 2.6 KB of ZEROED coefficient arrays for ~60% of B macroblocks — the
    /// wall-time regression's main CPU tax (B-heavy MT arm measured +45% CPU).
    BSkip { mbx: usize, mby: usize, regions: Vec<BRegion> },
    /// P inter with `cbp == 0` — the P-side twin of `BSkip` (D9).
    InterNoRes(Box<PInterNoResJob>),
}

/// A P inter macroblock with NO residual (`cbp == 0`) — motion only.
///
/// D9. `PInterJob` is 2,784 bytes and **93% of that is coefficient arrays**
/// (`luma_scan` 1024 + `luma8` 1024 + `cac` 512 + `cdc` 32 = 2,592). When
/// `cbp == 0` every one of them is ZERO, and the seam was heap-allocating,
/// filling, channel-passing and freeing all 2,592 bytes of nothing —
/// 12.8-37.5% of inter jobs on the x264 corpus, 15.8-44.1 MB per 60-frame pass.
///
/// This is the same pathology `EdcJob::BSkip` was introduced to fix on the B
/// side ("2.6 KB of ZEROED coefficient arrays for ~60% of B macroblocks — the
/// wall-time regression's main CPU tax"). It was never ported to P.
///
/// The worker rebuilds the full job with zeroed coefficients and calls the SAME
/// `recon_p_inter`, so this cannot diverge from the full path: there is no
/// second reconstruction implementation, only a cheaper way to ship the inputs.
struct PInterNoResJob {
    mbx: usize,
    mby: usize,
    t8: bool,
    qp: u8,
    gmv: [(i32, i32); 16],
    gref: [u8; 16],
}

impl PInterNoResJob {
    /// Rebuild the full job. `cbp == 0` means every coefficient array is zero
    /// and `cbp_chroma`/`nnzs` are zero, which is exactly what this fills in.
    #[inline]
    fn to_full(&self) -> PInterJob {
        PInterJob {
            mbx: self.mbx,
            mby: self.mby,
            t8: self.t8,
            qp: self.qp,
            cbp_chroma: 0,
            gmv: self.gmv,
            gref: self.gref,
            luma_scan: [[0i32; 16]; 16],
            luma8: [[0i32; 64]; 4],
            cdc: [[0i32; 4]; 2],
            cac: [[[0i32; 16]; 4]; 2],
            nnzs: [0u8; 24],
        }
    }
}

/// The compact inputs of one CABAC P inter macroblock's reconstruction.
struct PInterJob {
    mbx: usize,
    mby: usize,
    t8: bool,
    qp: u8,
    cbp_chroma: u32,
    /// The committed per-block motion, copied at parse time so the worker
    /// never reads the parse thread's grids (E2). Ref indices clamped by the
    /// consumer, kept u8 (spec max 15).
    gmv: [(i32, i32); 16],
    gref: [u8; 16],
    luma_scan: [[i32; 16]; 16],
    luma8: [[i32; 64]; 4],
    cdc: [[i32; 4]; 2],
    cac: [[[i32; 16]; 4]; 2],
    nnzs: [u8; 24],
}

/// Entropy-decouple master knob — DEFAULT ON since 2026-08-05 (`RS_H264_EDC=0`
/// opts out). E1 was expected to be cost-neutral scaffolding for the E2
/// thread; it BANKED on its own: 13/15 pairs, z=2.84, median +4.0% (pooled
/// 19/24, z=2.86). Mechanism: LOOP FISSION — batching a row's parsing and
/// then a row's reconstruction keeps each large code path's I-cache and
/// branch state hot, instead of alternating two giant bodies per macroblock.
fn edc_on() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        0 => {
            let v = !std::env::var_os("RS_H264_EDC").is_some_and(|v| v == "0");
            ON.store(if v { 1 } else { 2 }, Ordering::Relaxed);
            v
        }
        n => n == 1,
    }
}

/// E2/E3 worker-thread knob — DEFAULT ON since 2026-08-05 (`RS_H264_EDC_MT=0`
/// opts out). Banked at wall-time 9/11 z=2.11, median +23.4% on P content
/// (measured on 2 cores — overlap is invisible to single-core CPU time).
/// D8: run every pixel reconstruction TWICE (the second pass is discarded work,
/// not discarded output -- recon is idempotent, so the bytes are unchanged).
/// `t(double) - t(single)` IS the pixel half's cost, which is the parallel
/// fraction the E2 seam can address. Byte-identity is the proof the ablation
/// did not change the program (unlike removing the stage, which cascades).
/// D9 compact no-residual P inter jobs. `RS_H264_NORES=0` restores the old
/// always-full-payload path for A/B (the arm must PIN the value, never inherit
/// a default -- an "off" arm that only omits an override measures
/// default-vs-default and prints all zeros).
/// D10 row batching — **DEFAULT ON. A DELIBERATE THROUGHPUT-OVER-LATENCY TRADE.**
///
/// Ships a row's pixel jobs in one message instead of one per macroblock:
/// 207,949 sends -> 3,086 (**67-70x fewer**), and **~3% less CPU**.
///
/// ⚠ IT COSTS 2-4% WALL on a single stream — 0.963x / 0.980x, **11/11 pairs on
/// two clips**, A/B'd against itself at a fixed queue bound. That is measured,
/// reproducible, and ACCEPTED, not an oversight. Do not "fix" it by flipping
/// the default; read this first.
///
/// WHY IT COSTS WALL: batching trades pipelining for synchronisation.
/// Per-macroblock sends let the worker start on job 1 immediately; a row batch
/// makes it idle until ~70 macroblocks are parsed, then hands it a burst.
///
/// WHY IT IS STILL THE RIGHT DEFAULT: wall time here is SINGLE-STREAM LATENCY;
/// CPU is THROUGHPUT. A host decoding many streams concurrently is CPU-bound,
/// not latency-bound, so 3% less CPU is ~3% more capacity while the 2-4% wall
/// cost falls on a dimension that is not the constraint. The 67x drop in
/// channel operations also removes a park/unpark storm that scales with the
/// number of runnable threads — it gets better, not worse, as the box fills.
///
/// `RS_H264_BATCH=0` restores per-macroblock sends for the latency-sensitive
/// single-stream case (playback, seek preview, anything where first-frame time
/// dominates).
fn batch_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| !std::env::var_os("RS_H264_BATCH").is_some_and(|v| v == "0"))
}

fn nores_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| !std::env::var_os("RS_H264_NORES").is_some_and(|v| v == "0"))
}

fn double_recon() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("RS_H264_DOUBLE_RECON").is_some())
}

fn edc_bound() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RS_H264_EDC_BOUND")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256)
    })
}

/// D12 — E2 THREADING DISPATCH. Fires on `720p-or-smaller AND bits/MB > 38.4`.
///
/// The seam threads unconditionally before this, and that shipped a REGRESSION:
/// 8-10% slower wall on main profile for 38-56% more CPU. It cannot pay in
/// general — the pixel half is only ~15.6% of decode, so Amdahl caps two
/// threads at 1.085x — but it DOES win on some streams, so the answer is a
/// dispatch, not abandonment.
///
/// Fitted with `bench/examples/gate_optimizer.rs` over **28 interleaved
/// configurations** (12 clips x core counts 4-8, `bench/pinmtx.ps1`):
/// **net +29.40 of +30.70 perfect**, train +25.10 AND holdout +4.30, worst
/// fired class **+2.94**, precision 0.80. It forgoes +0.40 of wins to avoid
/// **169.90** of losses. Calibration: depth-2 **2/300** rules passed (0.67%),
/// so the separation carries information.
///
/// Per clause, both load-bearing (dropping either: -20.50 / -50.60, 6-7 big
/// losers):
/// * `bits/MB > 38.4` — coefficient density is the runtime proxy for PIXEL
///   SHARE: more coefficients means more residual work on the far side of the
///   seam. Threshold sits in an open gap (highest excluded 35.4, lowest firing
///   41.4). Note this is the OPPOSITE direction to an earlier hand-fitted
///   `bits < 65`, which was fitted to one clip and falsified.
/// * frame <= 720p — every 1080p configuration measured loses, including a
///   low-density one, so density alone is not sufficient.
/// * `cabac` — ADDED 2026-08-07 after the CAVLC arm made those units
///   measurable. bits/MB DOES NOT TRANSFER ACROSS ENTROPY CODERS: CAVLC needs
///   more bits for the SAME coefficients, so its density (62-65) reads deep
///   inside the firing region while its pixel work is unchanged. Without this
///   clause the rule routed CAVLC into threading, where it measured 1.29-1.49x
///   SLOWER — net -52.30, worst class -40.85. With it, +29.40 and worst class
///   +2.94. `gate_optimizer` could not find this: the rule needs THREE clauses
///   and the search is depth-2 (both depth-2 pairs fail, -20.50 / -50.60).
///
/// The estimate comes from ALREADY-DECODED slices, so the first slice of a
/// stream runs INLINE (the safe arm) until a measurement exists. Both arms are
/// byte-identical, so the choice can never affect output.
/// `RS_H264_EDC_MT=0` forces inline, `=1` forces threaded (skips the gate).
fn edc_dispatch(mb_w: usize, mb_h: usize, bits_per_mb: f64, cabac: bool) -> bool {
    const BITS_MIN: f64 = 38.4;
    const MAX_MBS: usize = 5000; // 720p = 3600, 1080p = 8160
    // The `cabac` clause is NOT cosmetic — see the header note. Without it this
    // rule scores net -52.30 with worst class -40.85 once CAVLC units are in the
    // corpus, because CAVLC's bits/MB is inflated by a less efficient entropy
    // coder rather than by more pixel work.
    cabac && bits_per_mb > BITS_MIN && mb_w * mb_h <= MAX_MBS
}

fn edc_mt() -> Option<bool> {
    static V: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *V.get_or_init(|| match std::env::var("RS_H264_EDC_MT").ok().as_deref() {
        Some("0") => Some(false),
        Some("1") => Some(true),
        _ => None,
    })
}



/// Row-interleaved deblocking master knob: `RS_H264_ROWDB=0` opts out,
/// restoring the picture-end pipeline (WHYS Part 17) as the A/B comparator.
fn rowdb_on() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        0 => {
            let v = !std::env::var_os("RS_H264_ROWDB").is_some_and(|v| v == "0");
            ON.store(if v { 1 } else { 2 }, Ordering::Relaxed);
            v
        }
        n => n == 1,
    }
}

/// 4×4-block (z-order) → 30-entry (6-stride) mv/ref/mvd cache index (openh264
/// g_kCache30ScanIdx). Top neighbour = cache[idx-6], left = cache[idx-1].
const CACHE30: [usize; 16] = [7, 8, 13, 14, 9, 10, 15, 16, 19, 20, 25, 26, 21, 22, 27, 28];

/// z-order 4×4-block → raster index (openh264 g_kuiScan4). Per-MB mvd/ref state is
/// stored raster-indexed (matching how neighbour blocks 3/7/11/15 and 12..15 are read).
const G_SCAN4: [usize; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

/// P `sub_mb_type` CABAC (openh264 `ParseSubMBTypeCabac`, ctx 21). 0=8×8, 1=8×4, 2=4×8, 3=4×4.
fn parse_sub_mb_type_p_cabac(cab: &mut crate::cabac::Cabac) -> u32 {
    const S: usize = 21;
    if cab.decode_decision(S) != 0 {
        return 0;
    }
    if cab.decode_decision(S + 1) != 0 {
        3 - cab.decode_decision(S + 2)
    } else {
        1
    }
}

/// Intra `mb_type` sub-parse for P/B slices (openh264 `DecodeCabacIntraMbType`, `base`=32
/// for B). Returns 0 = I_4x4, 1..=24 = I_16x16, 25 = I_PCM (in the intra numbering).
fn parse_intra_mb_type_cabac(cab: &mut crate::cabac::Cabac, base: usize) -> u32 {
    if cab.decode_decision(base) == 0 {
        return 0; // I_4x4
    }
    if cab.decode_terminate() {
        return 25; // I_PCM
    }
    let mut t = 1 + 12 * cab.decode_decision(base + 1) as u32; // cbp_luma != 0
    if cab.decode_decision(base + 2) != 0 {
        t += 4 + 4 * cab.decode_decision(base + 2) as u32;
    }
    t += 2 * cab.decode_decision(base + 3) as u32;
    t += cab.decode_decision(base + 3) as u32;
    t
}

/// B `mb_type` CABAC (openh264 `ParseMBTypeBSliceCabac`, ctx base 27). `ctx_inc` = (left
/// avail & !direct) + (top avail & !direct). Returns 0 = B_Direct_16x16, 1..=21 = the
/// L0/L1/Bi 16×16/16×8/8×16 shapes, 22 = B_8x8, 23.. = intra (mb_type − 23).
/// Test-only alias so the ENCODER crate can gate `cb_mb_type_b` against this
/// parser directly — they are exact inverses, so a round-trip is a complete gate.
#[doc(hidden)]
pub fn parse_mb_type_b(cab: &mut crate::cabac::Cabac, ctx_inc: usize) -> u32 {
    parse_mb_type_b_cabac(cab, ctx_inc)
}

fn parse_mb_type_b_cabac(cab: &mut crate::cabac::Cabac, ctx_inc: usize) -> u32 {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
    const B: usize = 27;
    if cab.decode_decision(B + ctx_inc) == 0 {
        return 0; // B_Direct_16x16
    }
    if cab.decode_decision(B + 3) == 0 {
        return 1 + cab.decode_decision(B + 5) as u32; // 16×16 L0 / L1
    }
    let mut m = (cab.decode_decision(B + 4) as u32) << 3;
    m |= (cab.decode_decision(B + 5) as u32) << 2;
    m |= (cab.decode_decision(B + 5) as u32) << 1;
    m |= cab.decode_decision(B + 5) as u32;
    if m < 8 {
        return m + 3;
    }
    if m == 13 {
        return parse_intra_mb_type_cabac(cab, 32) + 23;
    }
    if m == 14 {
        return 11; // B_Bi_8x16
    }
    if m == 15 {
        return 22; // B_8x8
    }
    m = (m << 1) | cab.decode_decision(B + 5) as u32;
    m - 4
}

/// B `sub_mb_type` CABAC (openh264 `ParseBSubMBTypeCabac`, ctx base 36). Returns 0..=12
/// per spec Table 7-18 (0 = B_Direct_8x8, 1 = B_L0_8x8, …, 12 = B_Bi_4x4).
fn parse_sub_mb_type_b_cabac(cab: &mut crate::cabac::Cabac) -> u32 {
    const B: usize = 36;
    if cab.decode_decision(B) == 0 {
        return 0; // B_Direct_8x8
    }
    if cab.decode_decision(B + 1) == 0 {
        return 1 + cab.decode_decision(B + 3) as u32; // B_L0_8x8 / B_L1_8x8
    }
    let mut st = 3u32;
    if cab.decode_decision(B + 2) != 0 {
        if cab.decode_decision(B + 3) != 0 {
            return 11 + cab.decode_decision(B + 3) as u32; // B_L1_4x4 / B_Bi_4x4
        }
        st += 4;
    }
    st += 2 * cab.decode_decision(B + 3) as u32;
    st += cab.decode_decision(B + 3) as u32;
    st
}

/// Parse one motion partition's `mvd` (x,y) and splat it into the 30-entry cache + the
/// per-MB raster mvd/ref state. `part_idx` = the partition's top-left z-order block (for
/// the ctxInc neighbour lookup); `zblocks` = every z-order 4×4 block the partition covers.
fn parse_mvd_partition(
    cab: &mut crate::cabac::Cabac,
    part_idx: usize,
    zblocks: &[usize],
    mvdc: &mut [[i16; 2]; 30],
    refc: &mut [i8; 30],
    mmvd: &mut [[i16; 2]; 16],
    mref: &mut [i8; 16],
    ref_idx: i8,
) -> (i32, i32) {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
    let s = CACHE30[part_idx];
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
    let (cx, cy) = (ctx(0), ctx(1));
    let mvx = parse_mvd_cabac(cab, 0, cx);
    let mvy = parse_mvd_cabac(cab, 1, cy);
    for &zb in zblocks {
        mvdc[CACHE30[zb]] = [mvx, mvy];
        refc[CACHE30[zb]] = ref_idx;
        mmvd[G_SCAN4[zb]] = [mvx, mvy];
        mref[G_SCAN4[zb]] = ref_idx;
    }
    (mvx as i32, mvy as i32)
}

/// `ref_idx_l0` (P) CABAC — mirror of the encoder `cb_ref_idx`. Unary, ctxIdxOffset
/// 54: binIdx 0 → `ctx0` (condTermFlagA + 2·condTermFlagB), binIdx 1 → 4, binIdx ≥2 → 5.
fn parse_ref_idx_cabac(cab: &mut crate::cabac::Cabac, ctx0: usize) -> i8 {
    const B: usize = 54;
    let mut r = 0i8;
    let mut bin_idx = 0u32;
    // Cap the unary length: valid ref_idx ≤ 15 (16 refs max); the cap keeps a corrupt
    // stream from looping unboundedly. The MC clamps the index, so an over-range value
    // is decoded as garbage (never a panic) — the robustness contract, not correctness.
    while bin_idx < 32 {
        let ctx = match bin_idx {
            0 => ctx0,
            1 => 4,
            _ => 5,
        };
        if cab.decode_decision(B + ctx) == 0 {
            break;
        }
        r += 1;
        bin_idx += 1;
    }
    r
}

/// UEG3 mvd suffix (openh264 `DecodeUEGMvCabac`): TU prefix at `base + {0,1,2,3,3,..}`
/// (≤7), then EG3 bypass.
fn decode_ueg_mv(cab: &mut crate::cabac::Cabac, base: usize) -> u32 {
    const P2C: [usize; 8] = [0, 1, 2, 3, 3, 3, 3, 3];
    if cab.decode_decision(base) == 0 {
        return 0;
    }
    let mut code = 0u32;
    let mut count = 1usize;
    let mut tmp;
    loop {
        tmp = cab.decode_decision(base + P2C[count]);
        code += 1;
        count += 1;
        if tmp == 0 || count == 8 {
            break;
        }
    }
    if tmp != 0 {
        code += cabac_exp_bypass(cab, 3) + 1;
    }
    code
}

/// One `mvd` component (openh264 `ParseMvdInfoCabac`). `ctx_inc` (0/1/2) from the
/// neighbour |mvd| sum. ctxIdxOffset 40 (x) / 47 (y).
fn parse_mvd_cabac(cab: &mut crate::cabac::Cabac, comp: usize, ctx_inc: usize) -> i16 {
    let base = 40 + comp * 7; // NEW_CTX_OFFSET_MVD + comp*CTX_NUM_MVD
    if cab.decode_decision(base + ctx_inc) == 0 {
        return 0;
    }
    let mag = (decode_ueg_mv(cab, base + 3) + 1) as i16;
    if cab.decode_bypass() != 0 {
        -mag
    } else {
        mag
    }
}

/// `mb_skip_flag` CABAC (openh264 `ParseSkipFlagCabac`). `ctx_inc` = base 11 (P) or 24
/// (B) + (left avail & not-skip) + (top avail & not-skip). Returns true if skipped.
fn parse_mb_skip_cabac(cab: &mut crate::cabac::Cabac, ctx_inc: usize) -> bool {
    cab.decode_decision(ctx_inc) != 0
}

/// P-slice `mb_type` CABAC (openh264 `ParseMBTypePSliceCabac`). Returns 0..3 = inter
/// (P_L0_16x16 / P_16x8 / P_8x16 / P_8x8), 5 = I_4x4, 6..29 = I_16x16, 30 = I_PCM.
fn parse_mb_type_p_cabac(cab: &mut crate::cabac::Cabac) -> u32 {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
    const S: usize = 11; // NEW_CTX_OFFSET_SKIP; P mb_type contexts hang off it
    if cab.decode_decision(S + 3) == 0 {
        // inter
        return if cab.decode_decision(S + 4) != 0 {
            if cab.decode_decision(S + 6) != 0 { 1 } else { 2 }
        } else if cab.decode_decision(S + 5) != 0 {
            3
        } else {
            0
        };
    }
    // intra (prefix bit was 1)
    if cab.decode_decision(S + 6) == 0 {
        return 5; // I_4x4
    }
    if cab.decode_terminate() {
        return 30; // I_PCM
    }
    let mut t = 6 + cab.decode_decision(S + 7) * 12;
    if cab.decode_decision(S + 8) != 0 {
        t += 4;
        if cab.decode_decision(S + 8) != 0 {
            t += 4;
        }
    }
    t += cab.decode_decision(S + 9) << 1;
    t += cab.decode_decision(S + 9);
    t
}

/// I-slice `mb_type` CABAC parse (spec §9.3.2.5 / openh264 `ParseMBTypeISliceCabac`).
/// `ctx_inc` = (left MB is I_16x16/non-intra) + (top MB is …), i.e. 0..2; the corner
/// MB has no neighbours so `ctx_inc = 0`. Returns the raw mb_type: 0 = I_NxN (I_4x4/
/// I_8x8), 1..24 = I_16x16 (pred-mode/cbp packed), 25 = I_PCM.
fn parse_mb_type_i_cabac(cab: &mut crate::cabac::Cabac, ctx_inc: usize) -> u32 {
    const O: usize = 3; // ctxIdxOffset for I-slice mb_type
    if cab.decode_decision(O + ctx_inc) == 0 {
        return 0; // I_NxN
    }
    if cab.decode_terminate() {
        return 25; // I_PCM
    }
    let mut t = 1 + cab.decode_decision(O + 3) * 12; // CBP luma: 0 or 12
    if cab.decode_decision(O + 4) != 0 {
        t += 4; // CBP chroma 1 or 2
        if cab.decode_decision(O + 5) != 0 {
            t += 4;
        }
    }
    t += cab.decode_decision(O + 6) << 1; // I_16x16 pred mode (2 bins)
    t += cab.decode_decision(O + 7);
    t
}

/// One `Intra_4x4` (or `8x8`) pred-mode CABAC parse (openh264 `ParseIntraPredModeLuma
/// Cabac`): `prev_intra4x4_pred_mode_flag` (ctx 68) then, if 0, `rem_intra4x4_pred_mode`
/// (3 bins at ctx 69). Returns `-1` for "use predicted mode", else the 0..7 remainder.
fn parse_intra4x4_pred_mode_cabac(cab: &mut crate::cabac::Cabac) -> i32 {
    const IPR: usize = 68;
    if cab.decode_decision(IPR) == 1 {
        return -1; // prev_intra4x4_pred_mode_flag = 1
    }
    let mut m = cab.decode_decision(IPR + 1) as i32;
    m |= (cab.decode_decision(IPR + 1) as i32) << 1;
    m |= (cab.decode_decision(IPR + 1) as i32) << 2;
    m
}

/// `intra_chroma_pred_mode` CABAC parse (openh264 `ParseIntraPredModeChromaCabac`):
/// TU(cMax=3) — bin0 at ctx `64 + ctx_inc` (ctx_inc from neighbour chroma modes, 0 for
/// the corner MB), the rest at ctx 67. Returns the mode 0..3.
fn parse_intra_chroma_pred_mode_cabac(cab: &mut crate::cabac::Cabac, ctx_inc: usize) -> u32 {
    const CIPR: usize = 64;
    if cab.decode_decision(CIPR + ctx_inc) == 0 {
        return 0;
    }
    if cab.decode_decision(CIPR + 3) == 0 {
        return 1;
    }
    if cab.decode_decision(CIPR + 3) == 0 {
        return 2;
    }
    3
}

/// `coded_block_pattern` CABAC parse (openh264 `ParseCbpInfoCabac`), corner-MB variant
/// (top/left neighbours unavailable → their terms are 0). ctxIdxOffset 73 (luma) with 4
/// z-order 8×8 bins whose ctxInc uses the EARLIER-decoded bits within this MB, then
/// chroma bits at 77/81. Returns cbp: bits 0-3 = luma 8×8, bits 4-5 = chroma pattern.
fn parse_cbp_cabac(cab: &mut crate::cabac::Cabac, top: Option<u8>, left: Option<u8>) -> u32 {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Syntax);
    const CBP: usize = 73;
    let t = |m: u32| top.map_or(0u32, |c| ((c as u32 & m) == 0) as u32);
    let l = |m: u32| left.map_or(0u32, |c| ((c as u32 & m) == 0) as u32);
    let nb = |x: u32| (x == 0) as u32; // earlier 8×8 bin within this MB was NOT coded
    // Luma, 4 8×8 blocks in z-order. Top uses cbp bits 2/3, left uses 1/3.
    let b0 = cab.decode_decision(CBP + (l(1 << 1) + (t(1 << 2) << 1)) as usize);
    let b1 = cab.decode_decision(CBP + (nb(b0) + (t(1 << 3) << 1)) as usize);
    let b2 = cab.decode_decision(CBP + (l(1 << 3) + (nb(b0) << 1)) as usize);
    let b3 = cab.decode_decision(CBP + (nb(b2) + (nb(b1) << 1)) as usize);
    let mut cbp = b0 | (b1 << 1) | (b2 << 2) | (b3 << 3);
    // Chroma (4:2:0). ctxInc from neighbour chroma cbp (>>4).
    let ct = top.map_or(0u32, |c| ((c >> 4) != 0) as u32);
    let cl = left.map_or(0u32, |c| ((c >> 4) != 0) as u32);
    if cab.decode_decision(CBP + 4 + (cl + (ct << 1)) as usize) != 0 {
        let ct2 = top.map_or(0u32, |c| ((c >> 4) == 2) as u32);
        let cl2 = left.map_or(0u32, |c| ((c >> 4) == 2) as u32);
        let c1 = cab.decode_decision(CBP + 8 + (cl2 + (ct2 << 1)) as usize);
        cbp |= 1 << (4 + c1);
    }
    cbp
}

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
/// Test-only view of [`b_inter_layout`] for the ENCODER crate: `(mvmode, p0, p1)`
/// with pred coded 1 = L0, 2 = L1, 3 = Bi — the encoder's `b_part_mb_type` is the
/// exact inverse, so a round-trip over 4..=21 gates the two tables against drift.
pub fn b_inter_shape(mb_type: u32) -> (u8, u8, u8) {
    let (_, mvmode, preds) = b_inter_layout(mb_type);
    let code = |p: BPred| match (p.uses(0), p.uses(1)) {
        (true, true) => 3,
        (true, false) => 1,
        _ => 2,
    };
    (mvmode, code(preds[0]), code(preds[1]))
}

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

/// Copy a contiguous `w`x`h` block into a strided destination at `(x0, y0)`.
///
/// The width is SPECIALISED. Written as a per-pixel loop bounded by a runtime `w`,
/// this lowers to a bounds-checked store per pixel — and where it is a row copy of
/// runtime length, to a variable-length `memcpy` CALL per row. Both are the same
/// codegen trap the ENCODER fixed long ago ("H-17"); the decoder's copy of it was
/// never fixed, and it costs the most on exactly the streams a real encoder emits,
/// because x264's sub-16x16 partitions call it far more often than our own
/// 16x16-dominated bitstreams ever did. Byte-identical to the scalar form.
#[inline]
fn restride(dst: &mut [u8], dst_stride: usize, x0: usize, y0: usize, src: &[u8], w: usize, h: usize) {
    macro_rules! rows {
        ($n:expr) => {{
            for dy in 0..h {
                dst[(y0 + dy) * dst_stride + x0..][..$n].copy_from_slice(&src[dy * $n..][..$n]);
            }
        }};
    }
    match w {
        16 => rows!(16),
        8 => rows!(8),
        4 => rows!(4),
        2 => rows!(2),
        _ => {
            for dy in 0..h {
                dst[(y0 + dy) * dst_stride + x0..][..w].copy_from_slice(&src[dy * w..][..w]);
            }
        }
    }
}

fn store(plane: &mut [u8], stride: usize, x0: usize, y0: usize, s: &[u8; 16]) {
    let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Scatter);
    for dy in 0..4 {
        for dx in 0..4 {
            plane[(y0 + dy) * stride + (x0 + dx)] = s[dy * 4 + dx];
        }
    }
}

/// Un-scans an 8×8 block from frame zig-zag scan order to raster (spec Table 8-12).
fn un_scan_8x8(scan: &[i32; 64]) -> [i32; 64] {
    const ZZ8: [usize; 64] = [
        0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
        20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
        58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
    ];
    let mut out = [0i32; 64];
    for k in 0..64 {
        out[ZZ8[k]] = scan[k];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(qp: u8, offset: i32) -> FrameDecoder {
        FrameDecoder::new(1, 1, qp, offset, Vec::new(), 1, false, false, true)
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
