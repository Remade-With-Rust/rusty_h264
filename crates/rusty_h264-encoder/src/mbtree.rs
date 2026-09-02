//! Macroblock-tree lookahead adaptive quantization (temporal AQ).
//!
//! A cheap forward pass over a GOP's SOURCE frames estimates, per macroblock, how
//! much of the *future's* coding cost depends on it, then lowers the QP of
//! heavily-referenced macroblocks — investing bits where they pay off across many
//! later frames (a sharp reference makes every frame that predicts from it cheaper).
//! This is the temporal complement to the spatial AQ (`aq_qp_map`): AQ moves bits
//! by texture *within* a frame; mb-tree moves them by reference *importance across*
//! frames.
//!
//! Method (x264's mb-tree, adapted to our CQP GOP):
//!   1. Per frame, per MB: `intra` = spatial AC SATD; `inter` = best small-search
//!      motion-compensated residual SATD to the previous SOURCE frame (capped at
//!      `intra`), plus the winning MV. Source-domain (like x264's lowres lookahead)
//!      so no reconstruction is needed — it's a pure pre-pass.
//!   2. Backward propagation: walk frames last→first. Each MB's total importance is
//!      `intra + propagate_in`; the fraction its predictor earned — `(intra-inter)/
//!      intra` — is credited to the reference MBs it points to (bilinear by MV,
//!      area-weighted over the up-to-4 overlapped MBs) in the previous frame.
//!   3. QP offset `= -strength · log2((intra + propagate_in) / intra)` (≤ 0:
//!      heavily-referenced MBs get finer QP; leaves get 0). CENTERED per GOP
//!      (subtract the GOP-mean offset) so the average QP — hence the rate — is
//!      preserved and the effect is a pure redistribution of bits toward the MBs
//!      the future depends on.

#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;
#[allow(unused_imports)]
use rusty_h264_common::fmath::{F32Ext as _, F64Ext as _};
#[allow(unused_imports)]
use rusty_h264_common::once::OnceLock;

use crate::config::{EncoderConfig, LookaheadMode};
use rusty_h264_common::inter::mc_luma;
use rusty_h264_common::transform::hadamard_4x4;
use rusty_h264_common::YuvFrame;

/// Per-MB lookahead cost + motion for one frame.
#[derive(Clone, Copy)]
struct MbCost {
    intra: i32,     // spatial AC SATD, >= 1
    inter: i32,     // best MC-residual SATD to the previous frame, capped at `intra`
    mv: (i32, i32), // winning MV (quarter-pel) — propagation direction
}

/// SATD of a 4×4 residual (sum of |Hadamard coeffs|).
fn satd4(res: &[i32; 16]) -> i64 {
    hadamard_4x4(res)
        .iter()
        .map(|&v| v.unsigned_abs() as i64)
        .sum()
}

/// Edge-clamped coded-size luma (matches the encoder's source preparation).
pub(crate) fn coded_luma(cfg: &EncoderConfig, frame: &YuvFrame) -> Vec<u8> {
    let (cw, ch) = (cfg.mb_width() * 16, cfg.mb_height() * 16);
    let (w, h) = (frame.width, frame.height);
    let mut y = vec![0u8; cw * ch];
    // Row-slice copy + right-edge fill instead of a per-pixel double-`min`
    // gather — the `load_mb` shape (write into a PRE-SIZED destination), which
    // is the panic-round shape that works, as distinct from the append shape
    // that regressed. Interior rows become a `memcpy` + `memset`; the bottom
    // padding re-copies row `h-1`. Bit-identical to the per-pixel form by the
    // `coded_luma_matches_per_pixel` oracle.
    let wc = w.min(cw);
    for j in 0..ch {
        let src = &frame.y[j.min(h - 1) * w..][..w];
        let dst = &mut y[j * cw..][..cw];
        dst[..wc].copy_from_slice(&src[..wc]);
        dst[wc..].fill(src[w - 1]);
    }
    y
}

/// 2×2-average downsample of a luma plane to half resolution (both dims are MB
/// multiples → stay even). The Hybrid/HalfRes lookahead runs the MV search on this:
/// 4× fewer pixels, ~4× cheaper. The MV direction survives; only the COST accuracy
/// suffers on blurred detail — which the Hybrid mode fixes by re-scoring at full-res.
fn downsample2x(y: &[u8], cw: usize, ch: usize) -> (Vec<u8>, usize, usize) {
    let (hw, hh) = (cw / 2, ch / 2);
    let mut out = vec![0u8; hw * hh];
    // Row-pair slices + `chunks_exact(2)` instead of five multiplied indexings
    // per output pixel: the iterator shape carries the extents, so the four
    // source loads and the store lose their per-pixel bounds checks and the
    // row-base multiplies hoist to once per row. Same sums, same `(s+2)/4`
    // rounding: BIT-IDENTICAL.
    for j in 0..hh {
        let r0 = &y[2 * j * cw..][..cw];
        let r1 = &y[(2 * j + 1) * cw..][..cw];
        let dst = &mut out[j * hw..][..hw];
        for ((o, p0), p1) in dst
            .iter_mut()
            .zip(r0.chunks_exact(2))
            .zip(r1.chunks_exact(2))
        {
            let s = p0[0] as u32 + p0[1] as u32 + p1[0] as u32 + p1[1] as u32;
            *o = ((s + 2) / 4) as u8;
        }
    }
    (out, hw, hh)
}

/// Spatial AC SATD of a `bs`×`bs` block at pixel `(bx0, by0)` (DC excluded, summed
/// over 4×4 sub-blocks). The intra "cost" floor — how expensive with no prediction.
fn intra_cost(sy: &[u8], cw: usize, bx0: usize, by0: usize, bs: usize) -> i32 {
    let mut s = 0i64;
    for by in 0..bs / 4 {
        for bx in 0..bs / 4 {
            let mut blk = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    blk[dy * 4 + dx] = sy[(by0 + by * 4 + dy) * cw + bx0 + bx * 4 + dx] as i32;
                }
            }
            let h = hadamard_4x4(&blk);
            s += h[1..].iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
        }
    }
    (s.min(i32::MAX as i64) as i32).max(1)
}

/// Full-pel MC-residual SATD of a `bs`×`bs` block at a given (plane) quarter-pel MV.
/// DETERMINISTIC cost instrument for the lookahead (H-36). Wall-clock on this box
/// swings ±40 points run-to-run on an IDENTICAL config, which is far larger than
/// the content effect being measured — so the lookahead's cost is judged by its
/// WORK COUNT (candidate evaluations), which is exactly reproducible.
pub(crate) static SATD_CALLS: rusty_h264_common::atomic::AtomicU64 =
    rusty_h264_common::atomic::AtomicU64::new(0);

fn mc_satd(
    sy: &[u8],
    cw: usize,
    ch: usize,
    ref_y: &[u8],
    bx0: usize,
    by0: usize,
    bs: usize,
    mv: (i32, i32),
) -> i64 {
    SATD_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    // H-35: the diamond below only ever probes FULL-PEL vectors (`dx * 4` in
    // quarter-pel units), so nearly every call can read the reference IN PLACE and
    // hand both planes to the vendored asm SATD — instead of copying a bs×bs block
    // out of `mc_luma` and then running a SCALAR per-4×4 Hadamard. ("Exported ≠
    // wired": the main ME has used the asm kernel for months; the lookahead had its
    // own scalar twin, which is why mb-tree cost far more than its half-res search
    // should.) Identical value: `satd_px`'s scalar arm IS this function's old sum,
    // and its asm arm is pinned byte-exact to that by the accel oracles.
    let (ix, iy) = (
        bx0 as isize + (mv.0 >> 2) as isize,
        by0 as isize + (mv.1 >> 2) as isize,
    );
    if mv.0 & 3 == 0
        && mv.1 & 3 == 0
        && ix >= 0
        && iy >= 0
        && ix as usize + bs <= cw
        && iy as usize + bs <= ch
        && by0 + bs <= ch
        && bx0 + bs <= cw
    {
        return crate::mb16::satd_px(
            &sy[by0 * cw + bx0..],
            cw,
            &ref_y[iy as usize * cw + ix as usize..],
            cw,
            bs,
            bs,
        );
    }
    // Sub-pel seed probe or an edge-overhanging vector: the general path.
    let mut pred = [0u8; 256]; // bs ≤ 16 → fits; stride = bs
    mc_luma(ref_y, cw, ch, bx0, by0, bs, bs, mv.0, mv.1, &mut pred);
    let mut s = 0i64;
    for by in 0..bs / 4 {
        for bx in 0..bs / 4 {
            let mut res = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    res[dy * 4 + dx] = sy[(by0 + by * 4 + dy) * cw + bx0 + bx * 4 + dx] as i32
                        - pred[(by * 4 + dy) * bs + (bx * 4 + dx)] as i32;
                }
            }
            s += satd4(&res);
        }
    }
    s
}

/// Best MC-residual SATD of a `bs`×`bs` block and its winning (plane) MV, via a
/// full-pel diamond search SEEDED from a predictor (the neighbour's MV, for pan
/// coherence). The diamond (step 8→1 full-pel) tracks large motion a fixed ±2px set
/// missed — a wrong MV gives mb-tree a wrong propagation DIRECTION (misdirects bits).
fn inter_cost(
    sy: &[u8],
    cw: usize,
    ch: usize,
    ref_y: &[u8],
    bx0: usize,
    by0: usize,
    bs: usize,
    seed: (i32, i32),
    max_step: i32,
) -> (i32, (i32, i32)) {
    let mut best_mv = (0, 0);
    let mut best = mc_satd(sy, cw, ch, ref_y, bx0, by0, bs, (0, 0));
    // H-45: a PROVABLY byte-identical early-out. SATD is a sum of absolute values,
    // so every candidate is ≥ 0; once `best == 0` the guard `s < best` can never
    // fire again, and both the winning MV and the cost are already final. Without
    // this the diamond still pays its fixed floor — 4 probes at each of the 4 step
    // levels (8→4→2→1), because the round that TERMINATES a level costs 4 probes
    // that do not move — which is why the lookahead's eval count came out nearly
    // content-invariant (16–18 /MB/frame on flat AND busy clips) and why mb-tree's
    // relative cost was WORST on the static content it helps most (+18.8% akiyo).
    if best == 0 {
        return (0, best_mv);
    }
    if seed != (0, 0) {
        let s = mc_satd(sy, cw, ch, ref_y, bx0, by0, bs, seed);
        if s < best {
            best = s;
            best_mv = seed;
        }
    }
    // `max_step` bounds the initial diamond hop: 8 for a from-scratch search, small
    // (2) for the hybrid's full-res refine around an already-good coarse MV.
    let mut step = max_step;
    while step >= 1 {
        loop {
            let mut moved = false;
            for &(dx, dy) in &[(step, 0), (-step, 0), (0, step), (0, -step)] {
                let mv = (best_mv.0 + dx * 4, best_mv.1 + dy * 4); // quarter-pel units
                let s = mc_satd(sy, cw, ch, ref_y, bx0, by0, bs, mv);
                if s < best {
                    best = s;
                    best_mv = mv;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        step >>= 1;
    }
    (best.min(i32::MAX as i64) as i32, best_mv)
}

/// Per-MB costs for one frame, at the lookahead `mode`'s resolution(s). MVs are
/// always returned in FULL-res quarter-pel (propagation is resolution-independent).
/// - `FullRes`: search + score at full-res (16×16).
/// - `HalfRes`: search + score at half-res (8×8) — MV scaled ×2.
/// - `Hybrid`: search the MV on half-res, then REFINE + score intra/inter at
///   full-res (the cost accuracy the pure-half-res path lost, at ~its speed).
///
/// `ref_full`/`ref_half` are `None` for the IDR (intra-only, nothing to propagate).
#[allow(clippy::too_many_arguments)]
fn frame_costs(
    full: &[u8],
    cwf: usize,
    chf: usize,
    half: &[u8],
    cwh: usize,
    chh: usize,
    mb_w: usize,
    mb_h: usize,
    ref_full: Option<&[u8]>,
    ref_half: Option<&[u8]>,
    mode: LookaheadMode,
) -> Vec<MbCost> {
    let mut out: Vec<MbCost> = Vec::with_capacity(mb_w * mb_h);
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            // Neighbour MV seed (full-res quarter-pel), for pan coherence.
            let seed_full = if mb_x > 0 {
                out[mb_y * mb_w + mb_x - 1].mv
            } else if mb_y > 0 {
                out[(mb_y - 1) * mb_w + mb_x].mv
            } else {
                (0, 0)
            };
            // Intra cost at the scoring resolution (full for FullRes/Hybrid, half for HalfRes).
            let intra = if mode == LookaheadMode::HalfRes {
                intra_cost(half, cwh, mb_x * 8, mb_y * 8, 8)
            } else {
                intra_cost(full, cwf, mb_x * 16, mb_y * 16, 16)
            };
            let (inter, mv) = match (mode, ref_full, ref_half) {
                (LookaheadMode::FullRes, Some(rf), _) => {
                    let (ic, mv) =
                        inter_cost(full, cwf, chf, rf, mb_x * 16, mb_y * 16, 16, seed_full, 8);
                    (ic.min(intra), mv)
                }
                (LookaheadMode::HalfRes, _, Some(rh)) => {
                    let seed = (seed_full.0 / 2, seed_full.1 / 2);
                    let (ic, mvp) = inter_cost(half, cwh, chh, rh, mb_x * 8, mb_y * 8, 8, seed, 8);
                    (ic.min(intra), (mvp.0 * 2, mvp.1 * 2))
                }
                (LookaheadMode::Hybrid, Some(rf), Some(rh)) => {
                    // Coarse MV from the cheap half-res search…
                    let seed = (seed_full.0 / 2, seed_full.1 / 2);
                    let (_, mvp) = inter_cost(half, cwh, chh, rh, mb_x * 8, mb_y * 8, 8, seed, 8);
                    let coarse = (mvp.0 * 2, mvp.1 * 2); // → full-res quarter-pel
                                                         // …then a SMALL full-res refine that also gives the accurate cost.
                    let (ic, mv) =
                        inter_cost(full, cwf, chf, rf, mb_x * 16, mb_y * 16, 16, coarse, 2);
                    (ic.min(intra), mv)
                }
                _ => (intra, (0, 0)), // IDR (no reference)
            };
            out.push(MbCost { intra, inter, mv });
        }
    }
    out
}

/// Distribute `amount` from frame `f`'s MB (referencing the previous frame at MV
/// `mv`) into `prev`'s per-MB propagation accumulator, area-weighted over the up-to-4
/// macroblocks the referenced 16×16 block overlaps (edge-clamped).
fn propagate_to(
    prev: &mut [f64],
    mb_w: usize,
    mb_h: usize,
    mb_x: usize,
    mb_y: usize,
    mv: (i32, i32),
    amount: f64,
) {
    if amount <= 0.0 {
        return;
    }
    // Referenced block top-left in pixels (integer part of the quarter-pel MV),
    // clamped so it stays inside the frame.
    let rx = (mb_x as i32 * 16 + (mv.0 >> 2)).clamp(0, (mb_w as i32 - 1) * 16);
    let ry = (mb_y as i32 * 16 + (mv.1 >> 2)).clamp(0, (mb_h as i32 - 1) * 16);
    let cx0 = (rx / 16) as usize;
    let cy0 = (ry / 16) as usize;
    // Overlap widths with the left/top MB column/row (the remaining area spills into
    // the right/bottom neighbour when the block isn't MB-aligned).
    let fx = (rx % 16) as f64;
    let fy = (ry % 16) as f64;
    let wl = 16.0 - fx; // area in column cx0
    let wt = 16.0 - fy; // area in row    cy0
    for (dy, wy) in [(0usize, wt), (1, fy)] {
        if wy <= 0.0 {
            continue;
        }
        let cy = (cy0 + dy).min(mb_h - 1);
        for (dx, wx) in [(0usize, wl), (1, fx)] {
            if wx <= 0.0 {
                continue;
            }
            let cx = (cx0 + dx).min(mb_w - 1);
            prev[cy * mb_w + cx] += amount * (wx * wy) / 256.0;
        }
    }
}

/// One frame's detector preparation: coded-size luma + its half-res plane —
/// the per-FRAME half of the scene-cut pair ratio, split out so pair scorers
/// can prepare each frame ONCE. Scoring a batch through `windows(2)` prepped
/// every interior frame TWICE (as one pair's `cur`, then the next pair's
/// `prev`); the rolling caches in `lookahead::segment_gops` and the streaming
/// encoder hand the previous pair's `cur` prep across, halving the dominant
/// detector cost. Same planes from the same functions: BIT-IDENTICAL ratios.
pub(crate) struct PairPrep {
    full: Vec<u8>,
    half: Vec<u8>,
}

// Manual Debug: the derived form would dump both planes byte-by-byte through
// the `Encoder` derive that requires this.
impl core::fmt::Debug for PairPrep {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PairPrep({} + {} bytes)",
            self.full.len(),
            self.half.len()
        )
    }
}

pub(crate) fn pair_prep(cfg: &EncoderConfig, f: &YuvFrame) -> PairPrep {
    let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
    let (cwf, chf) = (mb_w * 16, mb_h * 16);
    let full = coded_luma(cfg, f);
    let (half, _, _) = downsample2x(&full, cwf, chf);
    PairPrep { full, half }
}

/// Scene-cut pair ratio `Σ min(inter, intra) / Σ intra` on the LOOKAHEAD
/// estimator: half-res intra SATD vs half-res diamond-searched MC residual.
/// The diamond (step 8→1) is the load-bearing part — the cheap ±2px activity
/// set was already refuted IN THIS FILE for exactly the failure a scene-cut
/// detector cannot afford: a fast pan reads as "unpredictable" when the
/// probe cannot reach the true vector, and every pair looks like a cut.
pub(crate) fn pair_ratio_prepped(cfg: &EncoderConfig, cur: &PairPrep, prev: &PairPrep) -> f64 {
    let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
    let (cwf, chf) = (mb_w * 16, mb_h * 16);
    let (cwh, chh) = (cwf / 2, chf / 2);
    let costs = frame_costs(
        &cur.full,
        cwf,
        chf,
        &cur.half,
        cwh,
        chh,
        mb_w,
        mb_h,
        Some(&prev.full),
        Some(&prev.half),
        LookaheadMode::HalfRes,
    );
    let (mut num, mut den) = (0i64, 0i64);
    for c in &costs {
        num += c.inter.min(c.intra) as i64;
        den += c.intra as i64;
    }
    num as f64 / den.max(1) as f64
}

/// mb-tree per-frame per-MB QP offsets for a GOP of SOURCE frames (display order,
/// the IDR first). `strength <= 0` returns all-zero (no-op / byte-identical). The
/// offsets are centered per GOP so the mean QP — hence the rate — is preserved.
/// Per-GOP gate telemetry — the Front-B harvest seam.
///
/// The mb-tree latch decides PER GOP, so fitting a law on per-clip signals is a
/// unit mismatch: within one clip the GOPs straddle the threshold (football
/// measured 0.747 / 0.480 / 0.402). This records one row per GOP so the
/// refinery can pair signals with a per-GOP objective.
///
/// Observe-only and off unless `RFF_MBTREE_GOPSTATS` is set.
pub mod gopstats {
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;
    #[cfg(feature = "std")]
    use std::sync::Mutex;
    // GLOBAL, not thread_local: `encode_all` encodes GOPs IN PARALLEL, each on
    // its own worker thread with a fresh encoder. Thread-local rows land on the
    // worker and are invisible to the caller — the first version of this seam
    // silently harvested ZERO rows for exactly that reason.
    //
    // Rows therefore arrive in worker-completion order, NOT GOP order. Harvest
    // with `RUSTY_THREADS=1` so the order is the GOP order the objective is
    // keyed by; `take()` refuses to guess otherwise.
    #[cfg(feature = "std")]
    static ROWS: Mutex<Vec<GopRow>> = Mutex::new(Vec::new());
    /// One GOP's gate inputs and its latch decision.
    #[derive(Debug, Clone, Copy)]
    pub struct GopRow {
        /// Strength-invariant offset dispersion (the latch signal).
        pub sd: f64,
        /// Raw RMS before normalisation (scales with mbtree_strength).
        pub sd_raw: f64,
        /// 1 - mean predictability; the back-off axis.
        pub residual_frac: f64,
        /// Effective strength after the back-off latch (0 = latched off).
        pub eff_strength: f64,
        /// Did the differentiation latch ZERO this GOP's offsets?
        pub latched_off: bool,
    }
    pub fn on() -> bool {
        rusty_h264_common::cached_knob!(
            bool,
            rusty_h264_common::knob("RFF_MBTREE_GOPSTATS").is_some()
        )
    }
    #[cfg_attr(not(feature = "std"), allow(unused_variables))]
    pub(crate) fn push(r: GopRow) {
        #[cfg(feature = "std")]
        if on() {
            if let Ok(mut g) = ROWS.lock() {
                g.push(r);
            }
        }
    }
    /// Drain the rows recorded so far.
    ///
    /// ⚠ In GOP order ONLY when the encode ran single-threaded
    /// (`RUSTY_THREADS=1`). With parallel GOP encoding the rows interleave by
    /// completion, and pairing them positionally with a per-GOP objective would
    /// silently mismatch signals to outcomes.
    pub fn take() -> Vec<GopRow> {
        #[cfg(feature = "std")]
        {
            ROWS.lock()
                .map(|mut g| core::mem::take(&mut *g))
                .unwrap_or_default()
        }
        #[cfg(not(feature = "std"))]
        {
            Vec::new()
        }
    }
}

/// Minimum propagation-offset dispersion for mb-tree to apply at all.
/// `RFF_MBTREE_SDMIN=0` restores the ungated behaviour exactly.
fn mbtree_spread_min(cfg: &EncoderConfig) -> f64 {
    static V: rusty_h264_common::once::OnceLock<Option<f64>> =
        rusty_h264_common::once::OnceLock::new();
    V.get_or_init(|| rusty_h264_common::knob("RFF_MBTREE_SDMIN").and_then(|v| v.parse().ok()))
        .unwrap_or(cfg.mbtree_spread_min)
}

pub fn gop_qp_offsets(cfg: &EncoderConfig, frames: &[YuvFrame], strength: f64) -> Vec<Vec<i32>> {
    gop_qp_offsets_refs(cfg, &frames.iter().collect::<Vec<_>>(), strength)
}

/// [`gop_qp_offsets`] over BORROWED frames. The bframes path's mb-tree runs on
/// each GOP's anchor SUB-SEQUENCE — non-contiguous display indices — and used
/// to materialize every window by DEEP-CLONING the anchor frames (a full
/// Y+U+V copy per anchor per window, ~150 KB each at CIF, ~3 MB at 1080p).
/// A slice of references carries the same frames with zero copies; the owned
/// wrapper above keeps the contiguous callers unchanged.
pub fn gop_qp_offsets_refs(
    cfg: &EncoderConfig,
    frames: &[&YuvFrame],
    strength: f64,
) -> Vec<Vec<i32>> {
    let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
    let n = frames.len();
    if strength <= 0.0 || n == 0 || mb_w * mb_h == 0 {
        gopstats::push(gopstats::GopRow {
            sd: 0.0,
            sd_raw: 0.0,
            residual_frac: 0.0,
            eff_strength: 0.0,
            latched_off: true,
        });
        return vec![vec![0i32; mb_w * mb_h]; n];
    }
    // Lookahead resolution mode (Hybrid default: half-res MV search + full-res cost
    // scoring). `RFF_MBTREE_LA=full|hybrid|half` overrides for A/B.
    let mode = match rusty_h264_common::knob("RFF_MBTREE_LA").as_deref() {
        Some("full") => LookaheadMode::FullRes,
        Some("hybrid") => LookaheadMode::Hybrid,
        Some("half") => LookaheadMode::HalfRes,
        _ => cfg.mbtree_lookahead,
    };
    let (cwf, chf) = (mb_w * 16, mb_h * 16);
    let (cwh, chh) = (mb_w * 8, mb_h * 8);
    let full: Vec<Vec<u8>> = frames.iter().map(|f| coded_luma(cfg, f)).collect();
    // GRAIN LATCH (Great Gate P3 item 1 — docs/gate-ledger.md mbtree-grain-veto):
    // propagation credit is FICTION on noise (nothing persists), so mb-tree
    // redistributes on false gradients — measured +4.41% BD-SSIM on grain once
    // the AQ grain veto stopped masking it. Reuse the aq-grain-veto conjunction
    // ("unexplained temporal residual: not texture, not motion → noise"),
    // SOURCE-vs-SOURCE on the GOP's first frame pair — grain is stationary
    // (per-frame floor spread was 7.7–8.3 over 120 frames), and both probe
    // frames sit INSIDE the GOP, so a boundary scene cut cannot sit between
    // them. Fires → zero offsets, byte-identical to mb-tree off for the GOP.
    // Thresholds transfer from the AQ fit: its IDR arm already validated this
    // exact conjunction on source-vs-source signals. `RFF_MBTREE_GRAIN=0`
    // disables (bisection anchor). Single-frame GOPs fail open (no pair).
    let grain_veto = {
        rusty_h264_common::cached_knob!(
            bool,
            rusty_h264_common::knob("RFF_MBTREE_GRAIN")
                .map(|s| s != "0")
                .unwrap_or(true)
        )
    };
    if grain_veto && n >= 2 {
        let sig = crate::signals::FrameSignals::new(&full[1], cwf, mb_w, mb_h, Some(&full[0]));
        let grain = sig.grain_signature();
        crate::signals::census::bump(crate::signals::census::MBTREE_GRAIN, grain);
        if grain {
            if rusty_h264_common::knob("RFF_MBTREE_DBG").is_some() {
                eprintln!("MBTREE_DBG grain latch: eff=0.000 (zero offsets)");
            }
            // The grain veto IS a gate decision — record it, or the harvest
            // drops the GOP and every later row pairs with the wrong objective.
            gopstats::push(gopstats::GopRow {
                sd: 0.0,
                sd_raw: 0.0,
                residual_frac: 0.0,
                eff_strength: 0.0,
                latched_off: true,
            });
            return vec![vec![0i32; mb_w * mb_h]; n];
        }
    }
    // Half-res planes needed for Hybrid + HalfRes (the MV search); FullRes skips them.
    let need_half = mode != LookaheadMode::FullRes;
    let half: Vec<Vec<u8>> = if need_half {
        full.iter().map(|f| downsample2x(f, cwf, chf).0).collect()
    } else {
        Vec::new()
    };
    let empty: Vec<u8> = Vec::new();
    // 1. per-frame per-MB costs (frame 0 = IDR, intra-only).
    let costs: Vec<Vec<MbCost>> = (0..n)
        .map(|f| {
            let ref_full = if f == 0 {
                None
            } else {
                Some(full[f - 1].as_slice())
            };
            let ref_half = if f == 0 || !need_half {
                None
            } else {
                Some(half[f - 1].as_slice())
            };
            let hf = if need_half {
                half[f].as_slice()
            } else {
                &empty[..]
            };
            frame_costs(
                &full[f], cwf, chf, hf, cwh, chh, mb_w, mb_h, ref_full, ref_half, mode,
            )
        })
        .collect();
    // 2. backward propagation: each MB credits the fraction its predictor earned to
    //    the previous frame's referenced MBs.
    let mbs = mb_w * mb_h;
    // ONE flat accumulator instead of `Vec<Vec<f64>>` (n+1 allocations → 1);
    // the same `split_at_mut` cur/prev discipline, the same cells, the same
    // values. `frac_buf` records each MB's `(intra−inter)/intra` as it is
    // computed here, because the residual-fraction pass below was recomputing
    // the IDENTICAL divide for every MB of every frame — reading the stored
    // value in the same forward order keeps `fsum` bit-identical while
    // removing (n−1)·mbs float divides. (Rows are frames 1..n.)
    let mut propagate: Vec<f64> = vec![0.0; n * mbs];
    let mut frac_buf: Vec<f64> = vec![0.0; n.saturating_sub(1) * mbs];
    for f in (1..n).rev() {
        let (head, tail) = propagate.split_at_mut(f * mbs);
        let cur = &tail[..mbs];
        let prev = &mut head[(f - 1) * mbs..];
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let m = mb_y * mb_w + mb_x;
                let c = costs[f][m];
                let total = c.intra as f64 + cur[m];
                // Fraction of this MB's cost the previous frame's reference "carries".
                let frac = (c.intra - c.inter) as f64 / c.intra as f64; // in [0,1]
                frac_buf[(f - 1) * mbs + m] = frac;
                propagate_to(prev, mb_w, mb_h, mb_x, mb_y, c.mv, total * frac);
            }
        }
    }
    // CONTENT-ADAPTIVE STRENGTH (codec-content-adaptive-dispatch): mb-tree's benefit
    // scales with how many future bits it can redistribute, ∝ the mean residual
    // fraction `1 − pred` (pred = mean predictability `(intra−inter)/intra` over inter
    // frames). When prediction is near-perfect (pred → 1, frames near-free/mostly
    // skip — a slow, smooth pan) there is nothing to gain and QP perturbation only adds
    // noise, so mb-tree REGRESSES; ramp strength to 0 as the residual fraction falls
    // below `MBTREE_RES_MIN`. Natural detailed/mixed content sits well above it.
    // PREDICTABILITY BACK-OFF, re-fitted 2026-08-06 (Great Gate P2 —
    // docs/gate-ledger.md mbtree-backoff-refit). The original linear ramp
    // (eff = strength·min(rf/0.10, 1)) had the right AXIS and the wrong SHAPE
    // AND THRESHOLD: it throttled mb-tree's biggest real-content WINNERS
    // (akiyo_qcif rf 0.046 → eff 0.44, forgoing −5.09→−9.24% BD-SSIM;
    // screen_text rf 0.04 → eff 0.35, forgoing −4.53→−7.06) while the one
    // class that genuinely regresses at full strength — tsrc-class synthetic
    // (rf 0.023–0.025, +3.53% BD-SSIM unthrottled) — still leaked +0.50
    // through the ramp's partial strength. The measured populations are
    // disjoint with a 1.56× natural gap (tsrc ≤ 0.025 | winners ≥ 0.039), so
    // the honest form is the single-sided LATCH: OFF below `res_min` (a zero
    // qpo — byte-identical to mb-tree off for the GOP), FULL strength above.
    // Per-GOP steps are safe: each GOP's offsets are independent and centered.
    // `RFF_MBTREE_RESMIN` overrides (0 = no back-off, always full strength).
    let res_min: f64 = {
        rusty_h264_common::cached_knob!(f64, {
            rusty_h264_common::knob("RFF_MBTREE_RESMIN")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.03)
        })
    };
    // `frac_buf` holds frames 1..n in exactly this pass's iteration order, so
    // the running sum sees the SAME summands in the SAME order — bit-identical
    // `fsum` with zero divides. `fc` was `+= 1.0` per iteration: a count, and
    // every increment is exact (integers < 2^53), so the closed form is the
    // same number.
    let fsum: f64 = frac_buf.iter().sum();
    let fc = (n.saturating_sub(1) * mbs) as f64;
    let residual_frac = 1.0 - if fc > 0.0 { fsum / fc } else { 0.0 };
    let eff_strength = if res_min > 0.0 && residual_frac < res_min {
        0.0
    } else {
        strength
    };
    crate::signals::census::bump(crate::signals::census::MBTREE_BACKOFF, eff_strength == 0.0);
    if eff_strength == 0.0 {
        gopstats::push(gopstats::GopRow {
            sd: 0.0,
            sd_raw: 0.0,
            residual_frac,
            eff_strength: 0.0,
            latched_off: true,
        });
        // Latched off: zero offsets are byte-identical to mb-tree off.
        if rusty_h264_common::knob("RFF_MBTREE_DBG").is_some() {
            eprintln!(
                "MBTREE_DBG spread=0.000 residual_frac={residual_frac:.3} eff=0.000 (latched off)"
            );
        }
        return vec![vec![0i32; mb_w * mb_h]; n];
    }
    // 3. QP offset per MB (≤ 0), then center per GOP to preserve the mean QP.
    //
    // A2 (fast-transcendentals addendum): `propagate == 0` ⇒ `total == intra`
    // ⇒ the ratio is EXACTLY 1.0 (`intra >= 1` by construction, and `x/x` is
    // exact) ⇒ `log2(1.0)` is exactly +0.0 — so the libm `log2` call AND the
    // divide are skipped and the surviving multiply is the ORIGINAL expression
    // evaluated at its exact value (`-eff_strength * 0.0` keeps the -0.0 the
    // original produced). The whole LAST frame of every GOP takes this path —
    // backward propagation never credits it — plus every MB the future never
    // references; that is ≥ 1/n of all calls by construction, more on content
    // with dead regions. Flat `offs` (n allocations → 1) with the same
    // frame-major order everywhere downstream.
    // Site 6's ★★ arm (Round 10): poly `log2` behind the same switch as the
    // AQ pipeline; the zero-propagate shortcut is exact either way.
    let poly = crate::fastmath::polytier_on();
    let mut offs: Vec<f64> = Vec::with_capacity(n * mbs);
    for f in 0..n {
        for m in 0..mbs {
            let p = propagate[f * mbs + m];
            offs.push(if p == 0.0 {
                -eff_strength * 0.0
            } else {
                let intra = costs[f][m].intra as f64;
                let total = intra + p;
                let l = if poly {
                    crate::fastmath::log2_poly(total / intra)
                } else {
                    (total / intra).log2()
                };
                -eff_strength * l
            });
        }
    }
    // Per-GOP CENTERING: subtract the GOP-mean offset so the average QP — hence the
    // rate — is preserved. This is the right rate-neutralization in BOTH modes: in CQP
    // it holds the fixed QP; in RC mode (mb-tree runs per-GOP over the anchor chain, the
    // controller picks the frame base) it keeps the offsets rate-neutral per GOP so the
    // controller's model is undisturbed. MEASURED: routing the cross-frame allocation
    // through the RC's `complexity` instead (uncentered per-MB + a per-frame multiplier)
    // was WORSE — it destroyed the cross-frame redistribution the centered offsets carry
    // (tsrc −1.5% → +1.9%). Centering stays; the RC just supplies the base QP.
    let cnt = (n * mbs) as f64;
    // Same frame-major summation order as the nested `flatten` had.
    let mean: f64 = offs.iter().sum::<f64>() / cnt;
    for o in offs.iter_mut() {
        *o -= mean;
    }
    // DIFFERENTIATION LATCH (Great Gate P3 item 4 — the pan loser).
    //
    // mb-tree lowers QP on blocks whose quality PROPAGATES to the frames that
    // reference them. That only buys anything when propagation is DIFFERENTIAL
    // — some blocks matter much more than others (a static scene with a moving
    // subject; screen content with dead regions). On a smooth pan EVERY block
    // propagates about equally, so these centered offsets carry no information
    // and mb-tree just redistributes rate for no perceptual reason. It is the
    // clip class mb-tree has always lost on: stockholm +3.10, ducks +1.20,
    // shields +1.06, bus +0.68, in_to_tree +0.69, crowd_run +0.35, city +0.27
    // BD-SSIM, all REGRESSIONS IN THE SHIPPED DEFAULT before this latch.
    //
    // `sd` is the dispersion of mb-tree's OWN output. That is the whole point:
    // it is not a content proxy standing in for the phenomenon (the fit that
    // was refused here twice used `lv_spread`/`flat_run`, spatial statistics
    // that merely correlate with panning on this corpus), it is the tool
    // measuring whether it has anything to say. Below the line its offsets are
    // noise around a centered mean, and zeroing them is EXACTLY mb-tree off.
    //
    // Measured over 21 clips: fires on 5, net +26.82 with ZERO regressions,
    // versus +23.25 with SEVEN for always-on. Costs four modest forgone wins
    // (tempete -1.61, football -1.39, soccer -0.59, crew -0.19).
    //
    // The refutation arm that killed the alternative: a second clause
    // (`headroom>10 && tdecay>1.3`) recovered football/soccer at perfect fit on
    // the 16-clip table, then fired on bus_cif -- a FAST PAN -- and LOST +0.68.
    // Dropped. See gate-ledger `mbtree-dispatch`.
    // STRENGTH-INVARIANT. Each offset is `-eff_strength * log2(total/intra)`, so
    // a raw RMS scales LINEARLY with `mbtree_strength` — and the threshold was
    // fitted at the default 0.9. At `--mbtree-strength 0.5` every sd shrinks 44%
    // and clips that should fire latch OFF; at 2.0 the losers start firing.
    // Dividing by eff_strength measures the DIFFERENTIATION itself (RMS of the
    // log2 importance ratio) — what the gate is actually about, and invariant to
    // how hard the offsets are then applied.
    //
    // Same defect class as the CAVLC bits/MB bug: a threshold on a signal whose
    // SCALE depends on an axis the fitting corpus never varied.
    let sd_raw = (offs.iter().map(|o| o * o).sum::<f64>() / cnt).sqrt();
    let sd = sd_raw / eff_strength.max(1e-9);
    if rusty_h264_common::knob("RFF_MBTREE_DBG").is_some() {
        eprintln!("MBTREE_DBG spread={sd:.3} raw={sd_raw:.3} residual_frac={residual_frac:.3} eff={eff_strength:.3}");
    }
    let sd_min = mbtree_spread_min(cfg);
    gopstats::push(gopstats::GopRow {
        sd,
        sd_raw,
        residual_frac,
        eff_strength,
        latched_off: sd < sd_min,
    });
    crate::signals::census::bump(crate::signals::census::MBTREE_SPREAD_LATCH, sd < sd_min);
    if sd < sd_min {
        return vec![vec![0i32; mb_w * mb_h]; n];
    }
    // Round + clamp to a sane per-MB QP swing. (Indexed slicing, not
    // `chunks_exact` — the latter computes `len / mbs`, a runtime-divisor
    // `div` this campaign exists to remove.) Site 8's ★★ arm: magic-number
    // round behind the poly-tier switch.
    const MBTREE_DQP_MAX: i32 = 6;
    (0..n)
        .map(|f| {
            offs[f * mbs..][..mbs]
                .iter()
                .map(|&o| {
                    let r = if poly {
                        crate::fastmath::round_ties_even_fast(o)
                    } else {
                        o.round()
                    };
                    (r as i32).clamp(-MBTREE_DQP_MAX, MBTREE_DQP_MAX)
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;

    /// Deterministic synthetic GOP: textured static background, a textured
    /// 16x16 block translating 4px/frame, and a strip whose texture scrolls
    /// 1px/frame — motion the diamond can track, with enough residual that the
    /// predictability back-off would not zero it even if it were live.
    fn synth_frames(w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
        (0..n)
            .map(|f| {
                let mut y = vec![0u8; w * h];
                for j in 0..h {
                    for i in 0..w {
                        // Static checker+gradient background.
                        let mut v = (((i / 4 + j / 4) % 2) as i32 * 60
                            + (i as i32 * 3 + j as i32 * 2) % 90
                            + 40) as i32;
                        // Scrolling strip (1px/frame): partial predictability.
                        if (16..24).contains(&j) {
                            v = 30 + (((i + f) * 13) % 200) as i32;
                        }
                        // Moving textured square (4px/frame).
                        let sx = 4 + f * 4;
                        if (sx..sx + 16).contains(&i) && (28..44).contains(&j) {
                            v = 200 - ((i * 7 + j * 11) % 120) as i32;
                        }
                        y[j * w + i] = v.clamp(0, 255) as u8;
                    }
                }
                YuvFrame {
                    width: w,
                    height: h,
                    y,
                    u: vec![128; (w / 2) * (h / 2)],
                    v: vec![128; (w / 2) * (h / 2)],
                }
            })
            .collect()
    }

    fn fnv1a_i32s(offs: &[Vec<i32>]) -> u64 {
        let mut hsh: u64 = 0xcbf2_9ce4_8422_2325;
        for fr in offs {
            for &v in fr {
                for b in v.to_le_bytes() {
                    hsh ^= b as u64;
                    hsh = hsh.wrapping_mul(0x100_0000_01b3);
                }
            }
        }
        hsh
    }

    /// The row-slice `coded_luma` must equal the original per-pixel
    /// double-`min` gather (kept here as the oracle), including on frames
    /// whose display size is not an MB multiple (live right/bottom padding).
    #[test]
    fn coded_luma_matches_per_pixel() {
        for (w, h) in [(64usize, 48usize), (20, 13), (33, 17), (16, 16)] {
            let cfg = EncoderConfig::new(w, h);
            let frame = &synth_frames(w, h, 1)[0];
            let (cw, ch) = (cfg.mb_width() * 16, cfg.mb_height() * 16);
            let mut want = vec![0u8; cw * ch];
            for j in 0..ch {
                for i in 0..cw {
                    want[j * cw + i] = frame.y[j.min(h - 1) * w + i.min(w - 1)];
                }
            }
            assert_eq!(coded_luma(&cfg, frame), want, "{w}x{h}");
        }
    }

    /// End-to-end golden gate for `gop_qp_offsets`: the exact `Vec<Vec<i32>>`
    /// output is pinned by hash across all three lookahead modes, so any edit
    /// claiming bit-identity has a whole-pipeline oracle (costs → propagation
    /// → offsets → centering → latch → rounding). The grain veto and the
    /// residual back-off are pinned OPEN via their documented env anchors
    /// (`RFF_MBTREE_GRAIN=0`, `RFF_MBTREE_RESMIN=0` — both OnceLock-cached, so
    /// they are set before the first call) so the golden pins the ARITHMETIC,
    /// not a veto's early-out zeros.
    #[test]
    fn gop_qp_offsets_golden() {
        std::env::set_var("RFF_MBTREE_GRAIN", "0");
        std::env::set_var("RFF_MBTREE_RESMIN", "0");
        let (w, h, n) = (64usize, 48usize, 6usize);
        let frames = synth_frames(w, h, n);
        let mut cfg = EncoderConfig::new(w, h);

        crate::fastmath::TEST_POLYTIER.with(|c| c.set(Some(false)));
        cfg.mbtree_lookahead = LookaheadMode::HalfRes;
        let a = gop_qp_offsets(&cfg, &frames, 0.9);
        // The gate must prove the tool ran: an all-zero output would pin only
        // a latch, not the arithmetic under edit.
        assert!(
            a.iter().flatten().any(|&v| v != 0),
            "HalfRes offsets all zero"
        );
        assert_eq!(fnv1a_i32s(&a), 1359955132549194384, "HalfRes golden");

        cfg.mbtree_lookahead = LookaheadMode::Hybrid;
        let b = gop_qp_offsets(&cfg, &frames, 0.9);
        assert!(
            b.iter().flatten().any(|&v| v != 0),
            "Hybrid offsets all zero"
        );
        assert_eq!(fnv1a_i32s(&b), 7391805242828194773, "Hybrid golden");

        cfg.mbtree_lookahead = LookaheadMode::FullRes;
        let c = gop_qp_offsets(&cfg, &frames, 2.0);
        assert!(
            c.iter().flatten().any(|&v| v != 0),
            "FullRes offsets all zero"
        );
        assert_eq!(fnv1a_i32s(&c), 17244099396955043453, "FullRes golden");

        // Round 10 decision-identity: the POLY arm (poly log2 in the offs
        // loop + ties-even final round) must yield the IDENTICAL i32 offsets
        // in every mode — asserted, not argued.
        crate::fastmath::TEST_POLYTIER.with(|c| c.set(Some(true)));
        cfg.mbtree_lookahead = LookaheadMode::HalfRes;
        assert_eq!(
            gop_qp_offsets(&cfg, &frames, 0.9),
            a,
            "poly arm HalfRes differs"
        );
        cfg.mbtree_lookahead = LookaheadMode::Hybrid;
        assert_eq!(
            gop_qp_offsets(&cfg, &frames, 0.9),
            b,
            "poly arm Hybrid differs"
        );
        cfg.mbtree_lookahead = LookaheadMode::FullRes;
        assert_eq!(
            gop_qp_offsets(&cfg, &frames, 2.0),
            c,
            "poly arm FullRes differs"
        );
        crate::fastmath::TEST_POLYTIER.with(|c| c.set(None));
    }
}
