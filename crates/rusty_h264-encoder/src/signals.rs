//! The per-frame content-signal vector — Great Gate P1 (docs/great-gate.md §6).
//!
//! One place where every frame-level content probe lives, computed LAZILY and
//! MEMOIZED, so (a) a probe runs at most once per frame however many gates read
//! it (the CABAC-P driver used to run `global_mc_residual` twice — once inside
//! `me_lambda_scale`, once for the me_wide coherence gate), (b) a configuration
//! that consults no signal pays nothing (the default path stays byte-identical
//! AND tax-free), and (c) the harvest tap can emit the whole vector without any
//! driver knowing which columns exist.
//!
//! Rules binding every signal here (great-gate.md §2):
//! - O(pixels) or cheaper, harvested AT DECISION TIME (a tap placed after the
//!   action measures the action);
//! - validated against a brute-force oracle or per-clip truth table BEFORE any
//!   gate is wired to it;
//! - thresholds are re-calibrated on the DEPLOYED estimator, not an offline
//!   probe of the same name.
//!
//! The probe implementations are moved VERBATIM from `mb16.rs` — their values
//! are calibration targets (me_wide truth table, B2 truth table, lme clip
//! table), so consolidation must not change a single output bit. The two new
//! axes (synthetic-vs-natural, grain floor) are additions with NO consumer yet:
//! harvest-only until a P2 fit earns them a gate.

#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;
#[allow(unused_imports)]
use rusty_h264_common::fmath::{F32Ext as _, F64Ext as _};
#[allow(unused_imports)]
use rusty_h264_common::once::OnceLock;

use core::cell::OnceCell;

/// 256·variance of one 16×16 luma macroblock (monotone in variance; the ×256 is
/// never divided out because every consumer compares, not reports).
// Accumulate in u32, not i64: the sum of 256 bytes maxes at 65280 and the sum
// of squares at 16.6M, so 64-bit accumulators (and a 64-bit multiply per
// pixel) were pure width — and they stop LLVM vectorising what is otherwise a
// textbook pair of reductions over 16 contiguous bytes.
pub(crate) fn mb_variance(sy: &[u8], cw: usize, mb_x: usize, mb_y: usize) -> i64 {
    let base = mb_y * 16 * cw + mb_x * 16;
    let (mut s, mut ss) = (0u32, 0u32);
    for r in 0..16 {
        let row = &sy[base + r * cw..base + r * cw + 16];
        for &p in row {
            let v = p as u32;
            s += v;
            ss += v * v;
        }
    }
    // Widen once at the end: s*s reaches 4.26e9, which only just fits u32.
    ss as i64 - (s as i64) * (s as i64) / 256 // 256·variance, monotone in variance
}

/// The B2 dispatch signal: mean over ~24 sampled interior MBs of
/// `(SAD@zeroMV − bestSAD over a ±8 step-4 full-pel grid) / SAD@zeroMV` — how much
/// a plain TRANSLATIONAL full-pel search improves on zero motion, i.e. exactly the
/// surface B2's SAD diamond exploits. Offline (b2_signals, 16-clip truth table) it
/// separates every B2 loss (crew flash 0.070, city 0.110, tempete 0.008) from
/// every meaningful win (bus 0.323, football/foreman 0.164/0.165, shields 0.361);
/// notably `me_wide_headroom` CANNOT be reused here — crew's headroom is high (20)
/// but B2 loses there, because SAD overprices the DC shifts of its camera flashes.
/// Returns `(mgain, dcfrac)`. `dcfrac` — mean `|Σcur − Σref| / SAD0` per sampled
/// block — is the FLASH detector: under an illumination change the zero-MV residual
/// is mostly a DC shift, which SAD prices fully but the Hadamard largely discounts,
/// so SAD misranks candidates exactly there. Justified by the one clip the
/// single-term gate got wrong (crew: high mgain on its motion frames, +0.54 BD).
fn b2_mgain(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8]) -> (f64, f64) {
    const WIDE: isize = 8;
    const STEP: isize = 4;
    const TARGET: usize = 24;
    let sad16 = |bx: usize, by: usize, rx: isize, ry: isize| -> Option<u32> {
        if rx < 0 || ry < 0 || rx as usize + 16 > cw || ry as usize + 16 > ch {
            return None;
        }
        let (rx, ry) = (rx as usize, ry as usize);
        let mut s = 0u32;
        for dy in 0..16 {
            let a = &sy[(by + dy) * cw + bx..][..16];
            let b = &ref_y[(ry + dy) * cw + rx..][..16];
            s += a
                .iter()
                .zip(b)
                .map(|(&p, &q)| p.abs_diff(q) as u32)
                .sum::<u32>();
        }
        Some(s)
    };
    let (mbw, mbh) = (cw / 16, ch / 16);
    if mbw < 6 || mbh < 6 {
        return (0.0, 0.0);
    }
    let inner = (mbw - 4) * (mbh - 4);
    let stride = (inner / TARGET).max(1);
    let (mut acc, mut dc, mut n) = (0.0f64, 0.0f64, 0u32);
    // Carried (rx, ry) ≡ (i % w, i / w) by induction — the decoder slice
    // loops' compare-and-wrap, replacing a variable-divisor div+mod pair per
    // sample. `stride` can exceed `w`, hence `while`, not `if`.
    let w = mbw - 4;
    let (mut rx, mut ry) = (0usize, 0usize);
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + rx, 2 + ry);
        let (bx, by) = (mx * 16, my * 16);
        if let Some(s0) = sad16(bx, by, bx as isize, by as isize) {
            let (mut ms, mut mr) = (0u32, 0u32);
            for dy in 0..16 {
                ms += sy[(by + dy) * cw + bx..][..16]
                    .iter()
                    .map(|&v| v as u32)
                    .sum::<u32>();
                mr += ref_y[(by + dy) * cw + bx..][..16]
                    .iter()
                    .map(|&v| v as u32)
                    .sum::<u32>();
            }
            dc += ms.abs_diff(mr) as f64 / (s0 + 1) as f64;
            let mut best = s0;
            let mut dy = -WIDE;
            while dy <= WIDE {
                let mut dx = -WIDE;
                while dx <= WIDE {
                    if let Some(s) = sad16(bx, by, bx as isize + dx, by as isize + dy) {
                        best = best.min(s);
                    }
                    dx += STEP;
                }
                dy += STEP;
            }
            acc += (s0 - best) as f64 / (s0 + 1) as f64;
            n += 1;
        }
        i += stride;
        rx += stride;
        while rx >= w {
            rx -= w;
            ry += 1;
        }
    }
    if n == 0 {
        (0.0, 0.0)
    } else {
        (acc / n as f64, dc / n as f64)
    }
}

/// Per-frame HEAD-ROOM probe for the `me_wide` rescue: on a small subsample of
/// interior blocks, how much does a WIDE (±24 step-4) full-pel search beat a
/// PREDICTOR-LOCAL (±2) one? Measures what the rescue actually buys, before the
/// macroblock loop and without committing any vector. Returns the mean relative
/// SAD improvement, in percent. Truth table + threshold calibration live with
/// `me_wide_hr_thresh` in `mb16.rs` (docs/WHYS-speed-gap.md R5).
fn me_wide_headroom(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8]) -> f64 {
    const LOCAL: isize = 2; // a well-seeded diamond's effective reach
    const WIDE: isize = 24; // the rescue grid's half-extent
    const STEP: isize = 4; // coarse: this is a frame-level statistic, not a search
    const TARGET: usize = 24; // samples per frame — keep the probe ~0.5% of a frame
    let sad16 = |bx: usize, by: usize, rx: isize, ry: isize| -> Option<u32> {
        if rx < 0 || ry < 0 || rx as usize + 16 > cw || ry as usize + 16 > ch {
            return None;
        }
        let (rx, ry) = (rx as usize, ry as usize);
        let mut s = 0u32;
        for dy in 0..16 {
            let a = &sy[(by + dy) * cw + bx..][..16];
            let b = &ref_y[(ry + dy) * cw + rx..][..16];
            s += a
                .iter()
                .zip(b)
                .map(|(&p, &q)| p.abs_diff(q) as u32)
                .sum::<u32>();
        }
        Some(s)
    };
    // Interior blocks only (the probe must not measure edge clamping), spread over
    // the frame so one moving object cannot dominate.
    let (mbw, mbh) = (cw / 16, ch / 16);
    if mbw < 6 || mbh < 6 {
        return 0.0;
    }
    let inner = (mbw - 4) * (mbh - 4);
    let stride = (inner / TARGET).max(1);
    let (mut acc, mut n) = (0.0f64, 0u32);
    // Same carried-wrap walk as `b2_mgain` — see the note there.
    let w = mbw - 4;
    let (mut rx, mut ry) = (0usize, 0usize);
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + rx, 2 + ry);
        let (bx, by) = (mx * 16, my * 16);
        let mut best_local = u32::MAX;
        for dy in -LOCAL..=LOCAL {
            for dx in -LOCAL..=LOCAL {
                if let Some(s) = sad16(bx, by, bx as isize + dx, by as isize + dy) {
                    best_local = best_local.min(s);
                }
            }
        }
        let mut best_wide = best_local;
        let mut dy = -WIDE;
        while dy <= WIDE {
            let mut dx = -WIDE;
            while dx <= WIDE {
                if let Some(s) = sad16(bx, by, bx as isize + dx, by as isize + dy) {
                    best_wide = best_wide.min(s);
                }
                dx += STEP;
            }
            dy += STEP;
        }
        if best_local > 0 {
            acc += (best_local - best_wide) as f64 / best_local as f64;
            n += 1;
        }
        i += stride;
        rx += stride;
        while rx >= w {
            rx -= w;
            ry += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        100.0 * acc / n as f64
    }
}

/// Mean per-sampled-pixel residual after GLOBAL-motion compensation of `sy` from
/// `ref_y` (coarse ±12 global ME + ±3 refine, subsampled interior). ~0 on a PURE pan
/// (a single MV predicts the whole frame) — precisely the content where the local ME
/// diamond never genuinely STALLS (its seed = the median = the pan MV is already
/// right), so the `me_wide` rescue can only find SPURIOUS MVs that wreck the B-frame
/// spatial-direct predictors. Gates `me_wide` off there — non-uniform content
/// (real stalls, where me_wide wins) reads well above 0. Also the MOTION term of
/// the lme dispatch (`me_lambda_scale` clip table).
fn global_mc_residual(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8]) -> f64 {
    if cw < 48 || ch < 48 {
        return f64::INFINITY;
    }
    let sad = |dx: isize, dy: isize| -> u64 {
        let mut s = 0u64;
        let mut y = 16;
        while y < ch - 16 {
            let cbase = (y * cw) as isize;
            let rbase = (y as isize + dy) * cw as isize + dx;
            let mut x = 16isize;
            while x < (cw - 16) as isize {
                let c = sy[(cbase + x) as usize] as i32;
                let r = ref_y[(rbase + x) as usize] as i32;
                s += (c - r).unsigned_abs() as u64;
                x += 8;
            }
            y += 8;
        }
        s
    };
    let (mut best, mut bc) = ((0isize, 0isize), u64::MAX);
    let mut dy = -12;
    while dy <= 12 {
        let mut dx = -12;
        while dx <= 12 {
            let c = sad(dx, dy);
            if c < bc {
                bc = c;
                best = (dx, dy);
            }
            dx += 4;
        }
        dy += 4;
    }
    for dy in best.1 - 3..=best.1 + 3 {
        for dx in best.0 - 3..=best.0 + 3 {
            let c = sad(dx, dy);
            if c < bc {
                bc = c;
            }
        }
    }
    let nx = (16..cw - 16).step_by(8).count();
    let ny = (16..ch - 16).step_by(8).count();
    bc as f64 / (nx * ny).max(1) as f64
}

/// Median subsampled MB variance over the frame — the TEXTURE term of the lme
/// dispatch. NOTE: deliberately a DIFFERENT estimator from [`mb_variance`]
/// (4:1 subsampled, per-sample normalized) — the lme clip table (akiyo 61 …
/// mobile 1554) was calibrated on THIS formula, so it must not be unified with
/// the AQ/SATD variance without re-calibrating that table.
fn frame_median_mb_var(sy: &[u8], cw: usize, mb_w: usize, mb_h: usize) -> i64 {
    let mut vs: Vec<i64> = Vec::with_capacity(mb_w * mb_h);
    for my in 0..mb_h {
        for mx in 0..mb_w {
            let (mut sum, mut sq) = (0i64, 0i64);
            for r in (0..16).step_by(2) {
                let row = (my * 16 + r) * cw + mx * 16;
                for c in (0..16).step_by(2) {
                    let v = sy[row + c] as i64;
                    sum += v;
                    sq += v * v;
                }
            }
            let n = 64i64;
            vs.push((sq - sum * sum / n) / n);
        }
    }
    if vs.is_empty() {
        return 0;
    }
    // `select_nth_unstable` guarantees the element at `mid` is the one a full
    // sort would put there — the same median EXACTLY, at O(n) instead of
    // O(n log n). Only the median is consumed, so the rest of the order was
    // paid for and thrown away.
    let mid = vs.len() / 2;
    *vs.select_nth_unstable(mid).1
}

/// SYNTHETIC-VS-NATURAL axis (great-gate.md §2, "build in P1"): one pass over
/// every 4th row computing BOTH tells at once. No consumer yet — harvest-only.
///
/// - `flat_run`: mean horizontal run length of EXACTLY-equal adjacent luma
///   samples. Screen content / graphics / text overlays are built from constant
///   spans (runs of tens to hundreds); camera content — even smooth gradients —
///   almost never repeats a byte exactly once sensor noise exists (runs ≈ 1–2).
/// - `hist_top16`: fraction of sampled pixels whose value falls in the 16 most
///   populated luma bins — palette concentration. Rendered content draws from a
///   small palette (→ 1.0); natural exposure spreads mass across the range.
fn flat_hist(sy: &[u8], cw: usize, ch: usize) -> (f64, f64) {
    let mut hist = [0u64; 256];
    let (mut runs, mut px) = (0u64, 0u64);
    let mut y = 0;
    while y < ch {
        let row = &sy[y * cw..y * cw + cw];
        let mut prev = 0x100u32; // sentinel: no run continues across a row seam
        for &p in row {
            hist[p as usize] += 1;
            if p as u32 != prev {
                runs += 1;
                prev = p as u32;
            }
        }
        px += cw as u64;
        y += 4;
    }
    let mut bins = hist;
    bins.sort_unstable_by(|a, b| b.cmp(a));
    let top16: u64 = bins[..16].iter().sum();
    (
        px as f64 / runs.max(1) as f64,
        top16 as f64 / px.max(1) as f64,
    )
}

/// GRAIN / NOISE axis (great-gate.md §2, "build in P1"): the temporal residual
/// FLOOR at zero motion — noise never predicts, so on grainy content even the
/// best-predicted blocks carry residual. No consumer yet — harvest-only.
///
/// Samples ~48 interior MBs (same interior walk as the B2/head-room probes),
/// takes each block's zero-MV SAD per pixel, and returns the 25th percentile:
/// the LOW end of the distribution, i.e. the blocks prediction serves best. On
/// clean static content this floor ≈ 0 whatever the motion elsewhere; grain
/// lifts it uniformly. Motion raises all samples too — interpret jointly with
/// `gmc_residual` (offline, in the truth table), never alone.
fn grain_floor(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8]) -> f64 {
    const TARGET: usize = 48;
    let (mbw, mbh) = (cw / 16, ch / 16);
    if mbw < 6 || mbh < 6 {
        return 0.0;
    }
    let inner = (mbw - 4) * (mbh - 4);
    let stride = (inner / TARGET).max(1);
    let mut floors: Vec<f64> = Vec::with_capacity(TARGET + 1);
    // Same carried-wrap walk as `b2_mgain` — see the note there.
    let w = mbw - 4;
    let (mut rx, mut ry) = (0usize, 0usize);
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + rx, 2 + ry);
        let (bx, by) = (mx * 16, my * 16);
        let mut s = 0u32;
        for dy in 0..16 {
            let a = &sy[(by + dy) * cw + bx..][..16];
            let b = &ref_y[(by + dy) * cw + bx..][..16];
            s += a
                .iter()
                .zip(b)
                .map(|(&p, &q)| p.abs_diff(q) as u32)
                .sum::<u32>();
        }
        floors.push(s as f64 / 256.0);
        i += stride;
        rx += stride;
        while rx >= w {
            rx -= w;
            ry += 1;
        }
    }
    if floors.is_empty() {
        return 0.0;
    }
    floors.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    floors[floors.len() / 4]
}

/// The lazy, memoized per-frame signal vector. Built once at the top of every
/// slice driver; every gate reads through it. `ref_y` is the List-0 reference
/// (None on intra frames — the temporal signals then return their neutral
/// values, and every current call site guards on having a reference anyway).
pub(crate) struct FrameSignals<'a> {
    sy: &'a [u8],
    cw: usize,
    ch: usize,
    mb_w: usize,
    mb_h: usize,
    ref_y: Option<&'a [u8]>,
    vars: OnceCell<Vec<i64>>,
    vars_sorted: OnceCell<Vec<i64>>,
    lv: OnceCell<(Vec<f64>, f64, f64)>, // (log2(var+1) per MB, mean, spread)
    median_var: OnceCell<i64>,
    gmc: OnceCell<f64>,
    headroom: OnceCell<f64>,
    mgain_dc: OnceCell<(f64, f64)>,
    flat_hist: OnceCell<(f64, f64)>,
    grain: OnceCell<f64>,
}

impl<'a> FrameSignals<'a> {
    pub(crate) fn new(
        sy: &'a [u8],
        cw: usize,
        mb_w: usize,
        mb_h: usize,
        ref_y: Option<&'a [u8]>,
    ) -> Self {
        FrameSignals {
            sy,
            cw,
            ch: mb_h * 16,
            mb_w,
            mb_h,
            ref_y,
            vars: OnceCell::new(),
            vars_sorted: OnceCell::new(),
            lv: OnceCell::new(),
            median_var: OnceCell::new(),
            gmc: OnceCell::new(),
            headroom: OnceCell::new(),
            mgain_dc: OnceCell::new(),
            flat_hist: OnceCell::new(),
            grain: OnceCell::new(),
        }
    }

    pub(crate) fn has_ref(&self) -> bool {
        self.ref_y.is_some()
    }

    pub(crate) fn n_mbs(&self) -> usize {
        self.mb_w * self.mb_h
    }

    /// Per-MB [`mb_variance`] in raster order — shared by the SATD-dispatch
    /// percentile, the AQ map, and any per-MB gate (one walk, N consumers).
    pub(crate) fn mb_vars(&self) -> &[i64] {
        self.vars.get_or_init(|| {
            (0..self.mb_h)
                .flat_map(|my| (0..self.mb_w).map(move |mx| (mx, my)))
                .map(|(mx, my)| mb_variance(self.sy, self.cw, mx, my))
                .collect()
        })
    }

    /// The population-shaping threshold (per-frame-percentile law): the variance
    /// at or above which an MB is in the top `q` fraction of THIS frame. Exactly
    /// the SATD-dispatch formula — routed fraction is content-invariant by
    /// construction. `q` must be > 0.
    pub(crate) fn var_percentile_thresh(&self, q: f64) -> i64 {
        let vars = self.vars_sorted.get_or_init(|| {
            let mut v = self.mb_vars().to_vec();
            v.sort_unstable();
            v
        });
        let idx = (((1.0 - q) * vars.len() as f64) as usize).min(vars.len() - 1);
        vars[idx]
    }

    /// Per-MB `log2(var+1)`, its mean, and its SPREAD (std). The spread is the
    /// AQ back-off signal and a synthetic-vs-natural tell in its own right:
    /// natural content reads ~1, synthetic pans ~6 (see `aq_qp_map`).
    pub(crate) fn log_vars(&self) -> &(Vec<f64>, f64, f64) {
        self.lv.get_or_init(|| {
            // Site 5's exact tier (fast-transcendentals A3): a FLAT MB
            // (`v == 0`) computes `log2(1.0)`, which C11 Annex F requires to
            // be exactly +0.0 — so the libm call is skipped bit-identically
            // (the identity is asserted in `signal_probes_golden`, so a
            // nonconforming libm would fail the suite, not drift silently).
            // Flat MBs are the common case on screen/synthetic/letterboxed
            // content and absent on noisy natural video — content-scaled, not
            // by-construction. The ★★ poly-log2 replacement for the nonzero
            // arm stays a BD-gated change (addendum A3), deliberately NOT
            // taken here.
            // Site 5's ★★ arm (Round 10): the poly `log2` frees the loop of
            // its libm call. `RFF_POLYTIER=0` = the libm anchor.
            let poly = crate::fastmath::polytier_on();
            let lv: Vec<f64> = self
                .mb_vars()
                .iter()
                .map(|&v| {
                    if v == 0 {
                        0.0
                    } else if poly {
                        crate::fastmath::log2_poly((v + 1) as f64)
                    } else {
                        rusty_h264_common::fmath::log2((v + 1) as f64)
                    }
                })
                .collect();
            let n = lv.len().max(1) as f64;
            let mean = lv.iter().sum::<f64>() / n;
            let std = rusty_h264_common::fmath::sqrt(
                lv.iter()
                    .map(|&l| rusty_h264_common::fmath::powi(l - mean, 2))
                    .sum::<f64>()
                    / n,
            );
            (lv, mean, std)
        })
    }

    #[cfg_attr(not(feature = "std"), allow(dead_code))]

    pub(crate) fn lv_spread(&self) -> f64 {
        self.log_vars().2
    }

    /// The lme TEXTURE term — see [`frame_median_mb_var`]'s estimator caveat.
    pub(crate) fn median_var(&self) -> i64 {
        *self
            .median_var
            .get_or_init(|| frame_median_mb_var(self.sy, self.cw, self.mb_w, self.mb_h))
    }

    /// Global-MC residual vs List-0 (INFINITY without a reference or on tiny
    /// frames — every comparison site treats "no evidence" as "not a pan").
    pub(crate) fn gmc_residual(&self) -> f64 {
        *self.gmc.get_or_init(|| match self.ref_y {
            Some(r) => global_mc_residual(self.sy, self.cw, self.ch, r),
            None => f64::INFINITY,
        })
    }

    /// me_wide head-room probe vs List-0 (0.0 without a reference).
    pub(crate) fn headroom(&self) -> f64 {
        *self.headroom.get_or_init(|| match self.ref_y {
            Some(r) => me_wide_headroom(self.sy, self.cw, self.ch, r),
            None => 0.0,
        })
    }

    /// B2 `(mgain, dcfrac)` vs List-0 (`(0.0, 0.0)` without a reference).
    pub(crate) fn mgain_dc(&self) -> (f64, f64) {
        *self.mgain_dc.get_or_init(|| match self.ref_y {
            Some(r) => b2_mgain(self.sy, self.cw, self.ch, r),
            None => (0.0, 0.0),
        })
    }

    /// Synthetic axis: mean exact-equal horizontal run length (harvest-only).
    /// SCREEN / RENDERED content, crossing BOTH synthetic tells.
    ///
    /// Wired 2026-08-08 as the 8x8-transform veto. The 8x8 transform LOSES on screen
    /// content in every coding structure and on BOTH screen clips — screen_text
    /// +0.07/+0.19/+0.14, screen_ui (holdout) +0.05/+0.19/+0.28 BD-SSIM — and the
    /// loss is INVARIANT to the RD margin (0.07/0.19/0.14 at margins 0, 8 and 24),
    /// which says the picks are RD-decisive and the proxy simply disagrees with SSIM
    /// on sharp text edges. A decision gate cannot fix a metric disagreement; a class
    /// veto can.
    ///
    /// BOTH tells are required, not either. `flat_run` alone would misfire on
    /// letterboxed natural video, whose black bars are long exactly-equal runs while
    /// its histogram stays natural. Measured separation is categorical, not marginal:
    ///
    /// ```text
    /// screen_text 22.73 / 0.976     screen_ui 13.68 / 0.933
    /// six natural clips  1.03-1.27 / 0.157-0.271
    /// ```
    ///
    /// so the thresholds sit in an empty band an order of magnitude wide and any
    /// value inside it classifies identically — this is a category test, not a fit.
    pub(crate) fn is_screen(&self) -> bool {
        self.flat_run() >= 4.0 && self.hist_top16() >= 0.5
    }

    pub(crate) fn flat_run(&self) -> f64 {
        self.flat_hist_pair().0
    }

    /// Synthetic axis: top-16-bin luma histogram mass fraction (harvest-only).
    pub(crate) fn hist_top16(&self) -> f64 {
        self.flat_hist_pair().1
    }

    fn flat_hist_pair(&self) -> (f64, f64) {
        *self
            .flat_hist
            .get_or_init(|| flat_hist(self.sy, self.cw, self.ch))
    }

    /// THE GRAIN SIGNATURE (docs/gate-ledger.md `aq-grain-veto`) — one
    /// definition, three consumers: spatial AQ, mb-tree, and the sub-8x8 split
    /// search. All three break on grain for the SAME physical reason (noise is
    /// "busy" but carries no maskable texture, no propagatable structure and no
    /// compressible detail), so they must agree on what grain IS; a second copy
    /// of these thresholds would drift the moment one is re-fitted.
    ///
    /// "Unexplained temporal residual: not texture, not motion -> noise."
    ///   `median_var < 200`  the residual is NOT explained by texture
    ///                       (protects mobile 1346+, city 259+; grain <= 134)
    ///   `grain_floor > 5`   even the best-predicted MBs carry residual
    ///   `mgain < 0.1`       a full-pel search cannot reduce it (not motion)
    ///
    /// Fitted per-frame on the 24-clip harvest: 58/58 grain frames, ~0 winner
    /// frames, threshold-insensitive across var<150..250. Fails OPEN without a
    /// reference and on textured grain (var >= 200) — a miss is the status quo,
    /// never a new regression. PROVISIONAL: one textured-grain exemplar.
    pub(crate) fn grain_signature(&self) -> bool {
        self.has_ref()
            && self.median_var() < grain_var_max()
            && self.grain_floor() > grain_floor_min()
            && self.mgain_dc().0 < grain_mgain_max()
    }

    /// Grain axis: zero-MV residual floor, p25 over sampled interior MBs
    /// (0.0 without a reference; harvest-only).
    pub(crate) fn grain_floor(&self) -> f64 {
        *self.grain.get_or_init(|| match self.ref_y {
            Some(r) => grain_floor(self.sy, self.cw, self.ch, r),
            None => 0.0,
        })
    }
}

/// The three `grain_signature` clause thresholds, each overridable so
/// `bench/gate_refit.py` can refit them. They were previously literals, which made
/// the conjunction un-refittable: the only knobs were the three CONSUMERS'
/// on/off switches (`RFF_AQ_GRAIN`, `RFF_SUB8_GRAIN`, `RFF_MBTREE_GRAIN`), and
/// turning a gate off is not the same experiment as moving its line.
///
/// NOTE these are ONE decision consulted from three places. The census lists
/// `aq_grain_veto`, `sub8_grain` and `mbtree_grain` as three gates, but all three
/// call `grain_signature()`, so they cannot diverge and a refit here moves all three.
///
/// Defaults are the fitted values: "unexplained temporal residual — not texture
/// (var < 200), not motion (mgain < 0.1), but present (floor > 5)".
fn grain_var_max() -> i64 {
    rusty_h264_common::cached_knob!(i64, {
        rusty_h264_common::knob("RFF_GRAIN_VARMAX")
            .and_then(|v| v.parse().ok())
            .unwrap_or(200)
    })
}
fn grain_floor_min() -> f64 {
    rusty_h264_common::cached_knob!(f64, env_f64("RFF_GRAIN_FLOORMIN").unwrap_or(5.0))
}
fn grain_mgain_max() -> f64 {
    rusty_h264_common::cached_knob!(f64, env_f64("RFF_GRAIN_MGAINMAX").unwrap_or(0.1))
}
fn env_f64(k: &str) -> Option<f64> {
    rusty_h264_common::knob(k).and_then(|v| v.parse().ok())
}

/// GATE FIRE-RATE CENSUS — Tier 1 of the gate-regression harness
/// (docs/gate-ledger.md; great-gate.md P4).
///
/// Every shipped gate bumps `fired` when it routes a unit to its non-default
/// arm and `seen` when it is consulted at all. These are DETERMINISTIC counts:
/// one run is the verdict, no pinning, no noise floor, no z-score — the
/// counter-before-clock law. A per-clip census is therefore an EXACT
/// comparison against recorded values, and it moves the instant anything
/// upstream of a gate changes, which is the failure this harness exists to
/// catch (the AQ grain fix silently flipped mb-tree's grain verdict from
/// -0.63% to +4.41% BD-SSIM; only a re-run found it).
///
/// Tier 1 is the CANARY, not the verdict: a moved count says "re-run the BD
/// table for this gate", it does not itself say better or worse.
pub mod census {
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    use rusty_h264_common::atomic::AtomicU64;
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;

    use core::sync::atomic::Ordering::Relaxed;

    /// One (fired, seen) pair per tracked gate. Order must match [`NAMES`].
    pub const N: usize = 9;
    pub static NAMES: [&str; N] = [
        "aq_grain_veto",  // frame: AQ vetoed on grain
        "mbtree_grain",   // GOP:   mb-tree vetoed on grain
        "mbtree_backoff", // GOP:   mb-tree latched off (residual_frac < res_min)
        "sub8_grain",     // frame: sub-8x8 split search vetoed on grain
        "sub8_split",     // quad:  a SPLIT arm won
        "sub8_rd_revert", // MB:    RD pricing overturned the SATD split pick
        "intra_rd_flip",  // MB:    RD pricing overturned the SATD intra/inter pick
        "shape_rd_flip",  // MB:    RD pricing overturned the SATD partition SHAPE
        "mbtree_spread",  // GOP:   mb-tree latched off (offsets undifferentiated)
    ];
    static FIRED: [AtomicU64; N] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static SEEN: [AtomicU64; N] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    // ---- TRANSFORM-SIZE LABELLING (not a new gate, and deliberately not one) ----
    //
    // The question this answers: does any gate behave DIFFERENTLY on macroblocks that
    // ended up using the 8x8 transform than on 4x4 ones? If not, a per-transform-size
    // threshold is dead before anyone builds it — and every threshold we add is a
    // surface that can be fitted on an axis its corpus never varied.
    //
    // Why a pending buffer rather than a bucket argument at `bump` time: the gates
    // fire DURING mode decision, and the macroblock's transform size is not decided
    // until after them. Tagging at bump time would label every gate with the previous
    // macroblock's answer. So consultations are held per macroblock and committed once
    // the size is known.
    thread_local! {
        static PENDING: core::cell::RefCell<Vec<(u8, bool)>> =
            const { core::cell::RefCell::new(Vec::new()) };
    }
    /// `[t8][gate]` — index 0 = the macroblock coded 4x4, 1 = it coded 8x8.
    static BY_T8: [[(AtomicU64, AtomicU64); N]; 2] = [
        [const { (AtomicU64::new(0), AtomicU64::new(0)) }; N],
        [const { (AtomicU64::new(0), AtomicU64::new(0)) }; N],
    ];

    /// E15 W10 (inline-execution.md 11.8): every consultation paid TWO atomic
    /// RMWs plus a thread-local RefCell push, and commit_mb a TLS borrow+drain,
    /// PER MACROBLOCK, unconditionally in release - instrument cost on the
    /// shipping path (the two `lock` ops the E15 asm read found). The census is
    /// a harness instrument (gatecheck/mecost set the knob); default OFF.
    #[inline]
    pub fn on() -> bool {
        use core::sync::atomic::{AtomicU8, Ordering};
        static ON: AtomicU8 = AtomicU8::new(0);
        match ON.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                let on = rusty_h264_common::knob("RFF_GATE_CENSUS").is_some_and(|v| v != "0");
                ON.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                on
            }
        }
    }

    /// Commits this macroblock's held consultations under its final transform size.
    /// Call once per macroblock, AFTER the plan is chosen. Gates that are frame- or
    /// GOP-scoped never reach here, which is correct: bucketing them by a macroblock
    /// property would be meaningless.
    pub fn commit_mb(t8: bool) {
        if !on() {
            return;
        }
        let b = &BY_T8[t8 as usize];
        PENDING.with(|p| {
            for (i, fired) in p.borrow_mut().drain(..) {
                b[i as usize].1.fetch_add(1, Relaxed);
                if fired {
                    b[i as usize].0.fetch_add(1, Relaxed);
                }
            }
        });
    }

    /// `(fired, seen)` per gate for `[4x4, 8x8]` macroblocks.
    pub fn snapshot_by_t8() -> [[(u64, u64); N]; 2] {
        core::array::from_fn(|t| {
            core::array::from_fn(|i| (BY_T8[t][i].0.load(Relaxed), BY_T8[t][i].1.load(Relaxed)))
        })
    }

    /// Records one consultation of gate `i`, and whether it fired.
    #[inline]
    pub fn bump(i: usize, fired: bool) {
        if !on() {
            return;
        }
        SEEN[i].fetch_add(1, Relaxed);
        if fired {
            FIRED[i].fetch_add(1, Relaxed);
        }
        PENDING.with(|p| p.borrow_mut().push((i as u8, fired)));
    }

    /// `(fired, seen)` per gate, in [`NAMES`] order.
    pub fn snapshot() -> [(u64, u64); N] {
        core::array::from_fn(|i| (FIRED[i].load(Relaxed), SEEN[i].load(Relaxed)))
    }

    /// Zeroes every counter (call before an encode the census will read).
    pub fn reset() {
        PENDING.with(|p| p.borrow_mut().clear());
        for t in 0..2 {
            for i in 0..N {
                BY_T8[t][i].0.store(0, Relaxed);
                BY_T8[t][i].1.store(0, Relaxed);
            }
        }
        for i in 0..N {
            FIRED[i].store(0, Relaxed);
            SEEN[i].store(0, Relaxed);
        }
        for w in WORK.iter() {
            w.store(0, Relaxed);
        }
    }

    /// WORK counters — the deterministic half of the speed gate. A feature's
    /// cost is a COUNT of the expensive things it causes before it is a
    /// duration: counts need no pinning, no ABBA, no z-score, and one run is
    /// the verdict (`codec-measurement` §15). The clock (bench/pinvs.ps1)
    /// converts a count ratio into wall/CPU; it never replaces it.
    pub const WN: usize = 4;
    pub static WORK_NAMES: [&str; WN] = [
        "best_part",  // motion searches (the split search multiplies these)
        "mb_plan",    // full MB plans: MC + transform + quantize + reconstruct
        "mb_coded",   // macroblocks reaching the coded path (the denominator)
        "ref_search", // per-REFERENCE motion searches (multi-ref multiplies best_part by up to num_refs; the ref_bits prune is what keeps it below that)
    ];
    static WORK: [AtomicU64; WN] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    #[inline]
    pub fn work(i: usize) {
        if !on() {
            return;
        }
        WORK[i].fetch_add(1, Relaxed);
    }
    pub fn work_snapshot() -> [u64; WN] {
        core::array::from_fn(|i| WORK[i].load(Relaxed))
    }
    pub const W_BEST_PART: usize = 0;
    pub const W_MB_PLAN: usize = 1;
    pub const W_MB_CODED: usize = 2;
    pub const W_REF_SEARCH: usize = 3;

    pub const AQ_GRAIN: usize = 0;
    pub const MBTREE_GRAIN: usize = 1;
    pub const MBTREE_BACKOFF: usize = 2;
    pub const SUB8_GRAIN: usize = 3;
    pub const SUB8_SPLIT: usize = 4;
    pub const SUB8_RD_REVERT: usize = 5;
    pub const INTRA_RD_FLIP: usize = 6;
    pub const SHAPE_RD_FLIP: usize = 7;
    pub const MBTREE_SPREAD_LATCH: usize = 8;
}

/// The routed decisions a driver reports back into the harvest row — what the
/// gates DID, next to what they saw. `lme_scale` is the chosen ME λ multiplier
/// (1.0 where the path has none); the booleans are the post-gate states.
#[cfg_attr(not(feature = "std"), allow(dead_code))]
pub(crate) struct GateDecisions {
    pub me_wide: bool,
    pub sadfp: bool,
    pub mv_smooth: bool,
    pub do_splits: bool,
    pub lme_scale: f64,
    pub satd_thresh: i64,
}

impl Default for GateDecisions {
    fn default() -> Self {
        GateDecisions {
            me_wide: false,
            sadfp: false,
            mv_smooth: false,
            do_splits: true,
            lme_scale: 1.0,
            satd_thresh: i64::MAX,
        }
    }
}

#[cfg(not(feature = "std"))]
#[allow(dead_code)]

fn sink() -> &'static Option<()> {
    static NONE: Option<()> = None;

    &NONE
}

#[cfg(feature = "std")]

fn sink() -> &'static Option<std::sync::Mutex<std::fs::File>> {
    #[cfg(feature = "std")]
    use std::io::Write;
    static S: rusty_h264_common::once::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        rusty_h264_common::once::OnceLock::new();
    S.get_or_init(|| {
        rusty_h264_common::knob("RFF_SIGNALS_CSV").and_then(|p| {
            let mut f = std::fs::File::create(p).ok()?;
            let _ = writeln!(
                f,
                "seq,slice,qp,mb_w,mb_h,mgain,dcfrac,headroom,gmc,median_var,lv_spread,\
                 flat_run,hist_top16,grain_floor,me_wide,sadfp,mv_smooth,do_splits,\
                 lme_scale,satd_thresh"
            );
            Some(std::sync::Mutex::new(f))
        })
    })
}

/// Observe-only harvest tap (`RFF_SIGNALS_CSV=<path>`): one CSV row per encoded
/// slice with the FULL signal vector plus the gate decisions taken on it. Env
/// unset → `sink()` is None → not a single signal is forced (the lazy cells stay
/// cold) and the tap costs one branch per frame.
///
/// Traps inherited from the suppress_optimizer header (great-gate.md §1.5):
/// the rows record signals at decision time, BEFORE any gate filtered the
/// population — but `seq` is a process-wide counter, so under GOP-parallel
/// encode row ORDER interleaves nondeterministically. Harvest with
/// single-threaded encode when row order matters; join train/holdout splits
/// offline by clip, never here. Intra rows carry neutral temporal signals
/// (no reference) — filter by the `slice` column offline.
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

    /// Deterministic frame pair: a FLAT top MB row (variance exactly 0), a
    /// textured static field, and a block that moved 4px between ref and cur —
    /// so the probes see flat, static-predictable and moving content at once.
    fn synth_pair(cw: usize, ch: usize) -> (Vec<u8>, Vec<u8>) {
        let mut sy = vec![64u8; cw * ch];
        let mut ry = vec![64u8; cw * ch];
        for j in 16..ch {
            for i in 0..cw {
                let t = ((i * 7 + j * 13) % 97) as u8;
                sy[j * cw + i] = 60 + t;
                ry[j * cw + i] = 60 + t;
            }
        }
        for j in 24..40.min(ch) {
            for i in 0..16 {
                let px = (220 - (i as i32) * 3 - (j as i32 % 16)) as u8;
                if 20 + i < cw {
                    sy[j * cw + 20 + i] = px;
                }
                if 16 + i < cw {
                    ry[j * cw + 16 + i] = px;
                }
            }
        }
        (sy, ry)
    }

    fn h64(h: &mut u64, bits: u64) {
        *h ^= bits;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }

    /// Bit-exact golden over the full signal vector, at three frame sizes
    /// chosen so the interior sample walks cover stride == 1, stride > 1 AND
    /// stride > row-width (the multi-wrap case). These values feed CALIBRATED
    /// gate tables (me_wide, lme, grain, B2), so any edit here must not move
    /// one bit.
    // Runs on the platform-libm arm AND the `libm` arm: with every float on
    // the coding path routed through `fmath`, the `libm` build is what a chip
    // reproduces, and this pins it to the same vector on every host.
    #[test]
    fn signal_probes_golden() {
        // This golden hashes lv f64 BITS, so it pins the LIBM arm (the
        // reference arithmetic); the poly arm is compared against it below.
        // Thread-local, not env: tests run threaded in one process.
        crate::fastmath::TEST_POLYTIER.with(|c| c.set(Some(false)));
        // The libm identity the flat-MB log2 shortcut depends on (C11 Annex F
        // requires log2(1) == +0).
        assert_eq!(1f64.log2().to_bits(), 0f64.to_bits());
        let mut golden = [(160usize, 112usize, 0u64), (224, 160, 0), (512, 480, 0)];
        // One row for both arms: the pure-Rust `libm` reproduces the platform
        // libm bit for bit on these inputs (x86-64 Windows, 2026-09-02), and
        // CI's three hosts must all agree on it — the cross-platform
        // determinism gate a chip's oracle rests on.
        let rows = [
            14809846845904276818u64,
            2783330344417898965,
            5253786124937756537,
        ];
        for (g, r) in golden.iter_mut().zip(rows) {
            g.2 = r;
        }
        for (cw, ch, want) in golden {
            let (mb_w, mb_h) = (cw / 16, ch / 16);
            let (sy, ry) = synth_pair(cw, ch);
            let sig = FrameSignals::new(&sy, cw, mb_w, mb_h, Some(&ry));
            // Prove the flat-MB arm is exercised: zero-variance MBs exist and
            // their log-variance is exactly +0.0.
            assert!(
                sig.mb_vars().iter().any(|&v| v == 0),
                "{cw}x{ch}: no zero-variance MB"
            );
            let lvs = sig.log_vars();
            for (l, &v) in lvs.0.iter().zip(sig.mb_vars()) {
                if v == 0 {
                    assert_eq!(l.to_bits(), 0f64.to_bits(), "{cw}x{ch}: flat MB lv != +0.0");
                }
            }
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for l in &lvs.0 {
                h64(&mut h, l.to_bits());
            }
            h64(&mut h, lvs.1.to_bits());
            h64(&mut h, lvs.2.to_bits());
            h64(&mut h, sig.headroom().to_bits());
            let (mg, dc) = sig.mgain_dc();
            h64(&mut h, mg.to_bits());
            h64(&mut h, dc.to_bits());
            h64(&mut h, sig.grain_floor().to_bits());
            h64(&mut h, sig.median_var() as u64);
            h64(&mut h, sig.gmc_residual().to_bits());
            let (fr, ht) = (sig.flat_run(), sig.hist_top16());
            h64(&mut h, fr.to_bits());
            h64(&mut h, ht.to_bits());
            eprintln!("[signal_probes_golden] {cw}x{ch}: {h}");
            assert_eq!(h, want, "{cw}x{ch}: signal vector golden");

            // Poly arm (Round 10): same frames, poly log2 — every lv within
            // the kernel's oracle bound of the libm arm, flat MBs still
            // EXACTLY +0.0 (the shortcut precedes the kernel choice).
            crate::fastmath::TEST_POLYTIER.with(|c| c.set(Some(true)));
            let sigp = FrameSignals::new(&sy, cw, mb_w, mb_h, Some(&ry));
            let lvp = sigp.log_vars();
            for (i, (a, b)) in lvs.0.iter().zip(&lvp.0).enumerate() {
                if a == &0.0 {
                    assert_eq!(b.to_bits(), 0f64.to_bits(), "{cw}x{ch} mb{i} flat");
                } else {
                    assert!(
                        (a - b).abs() <= 1e-11 * a.abs().max(1.0),
                        "{cw}x{ch} mb{i}: {a} vs {b}"
                    );
                }
            }
            crate::fastmath::TEST_POLYTIER.with(|c| c.set(Some(false)));
        }
        crate::fastmath::TEST_POLYTIER.with(|c| c.set(None));
    }
}

#[cfg_attr(not(feature = "std"), allow(unused_variables))]
pub(crate) fn harvest(sig: &FrameSignals, slice: char, qp: u8, d: &GateDecisions) {
    #[cfg(feature = "std")]
    use std::io::Write;
    #[cfg(feature = "std")]
    if let Some(m) = sink() {
        static SEQ: rusty_h264_common::atomic::AtomicU64 =
            rusty_h264_common::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let (mg, dc) = sig.mgain_dc();
        if let Ok(mut f) = m.lock() {
            let _ = writeln!(
                f,
                "{seq},{slice},{qp},{},{},{mg:.4},{dc:.4},{:.3},{:.3},{},{:.4},{:.3},{:.4},{:.3},{},{},{},{},{:.3},{}",
                sig.mb_w,
                sig.mb_h,
                sig.headroom(),
                sig.gmc_residual(),
                sig.median_var(),
                sig.lv_spread(),
                sig.flat_run(),
                sig.hist_top16(),
                sig.grain_floor(),
                d.me_wide as u8,
                d.sadfp as u8,
                d.mv_smooth as u8,
                d.do_splits as u8,
                d.lme_scale,
                d.satd_thresh,
            );
        }
    }
}
