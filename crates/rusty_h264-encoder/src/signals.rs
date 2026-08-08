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

use std::cell::OnceCell;

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
            s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
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
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + i % (mbw - 4), 2 + i / (mbw - 4));
        let (bx, by) = (mx * 16, my * 16);
        if let Some(s0) = sad16(bx, by, bx as isize, by as isize) {
            let (mut ms, mut mr) = (0u32, 0u32);
            for dy in 0..16 {
                ms += sy[(by + dy) * cw + bx..][..16].iter().map(|&v| v as u32).sum::<u32>();
                mr += ref_y[(by + dy) * cw + bx..][..16].iter().map(|&v| v as u32).sum::<u32>();
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
    }
    if n == 0 { (0.0, 0.0) } else { (acc / n as f64, dc / n as f64) }
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
            s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
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
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + i % (mbw - 4), 2 + i / (mbw - 4));
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
    vs.sort_unstable();
    vs.get(vs.len() / 2).copied().unwrap_or(0)
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
    (px as f64 / runs.max(1) as f64, top16 as f64 / px.max(1) as f64)
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
    let mut i = 0usize;
    while i < inner {
        let (mx, my) = (2 + i % (mbw - 4), 2 + i / (mbw - 4));
        let (bx, by) = (mx * 16, my * 16);
        let mut s = 0u32;
        for dy in 0..16 {
            let a = &sy[(by + dy) * cw + bx..][..16];
            let b = &ref_y[(by + dy) * cw + bx..][..16];
            s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
        }
        floors.push(s as f64 / 256.0);
        i += stride;
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
            let lv: Vec<f64> =
                self.mb_vars().iter().map(|&v| ((v + 1) as f64).log2()).collect();
            let n = lv.len().max(1) as f64;
            let mean = lv.iter().sum::<f64>() / n;
            let std = (lv.iter().map(|&l| (l - mean).powi(2)).sum::<f64>() / n).sqrt();
            (lv, mean, std)
        })
    }

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
        *self.flat_hist.get_or_init(|| flat_hist(self.sy, self.cw, self.ch))
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
            && self.median_var() < 200
            && self.grain_floor() > 5.0
            && self.mgain_dc().0 < 0.1
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
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// One (fired, seen) pair per tracked gate. Order must match [`NAMES`].
    pub const N: usize = 9;
    pub static NAMES: [&str; N] = [
        "aq_grain_veto",   // frame: AQ vetoed on grain
        "mbtree_grain",    // GOP:   mb-tree vetoed on grain
        "mbtree_backoff",  // GOP:   mb-tree latched off (residual_frac < res_min)
        "sub8_grain",      // frame: sub-8x8 split search vetoed on grain
        "sub8_split",      // quad:  a SPLIT arm won
        "sub8_rd_revert",  // MB:    RD pricing overturned the SATD split pick
        "intra_rd_flip",   // MB:    RD pricing overturned the SATD intra/inter pick
        "shape_rd_flip",   // MB:    RD pricing overturned the SATD partition SHAPE
        "mbtree_spread",   // GOP:   mb-tree latched off (offsets undifferentiated)
    ];
    static FIRED: [AtomicU64; N] = [
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static SEEN: [AtomicU64; N] = [
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    /// Records one consultation of gate `i`, and whether it fired.
    #[inline]
    pub fn bump(i: usize, fired: bool) {
        SEEN[i].fetch_add(1, Relaxed);
        if fired {
            FIRED[i].fetch_add(1, Relaxed);
        }
    }

    /// `(fired, seen)` per gate, in [`NAMES`] order.
    pub fn snapshot() -> [(u64, u64); N] {
        std::array::from_fn(|i| (FIRED[i].load(Relaxed), SEEN[i].load(Relaxed)))
    }

    /// Zeroes every counter (call before an encode the census will read).
    pub fn reset() {
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
    pub const WN: usize = 3;
    pub static WORK_NAMES: [&str; WN] = [
        "best_part",  // motion searches (the split search multiplies these)
        "mb_plan",    // full MB plans: MC + transform + quantize + reconstruct
        "mb_coded",   // macroblocks reaching the coded path (the denominator)
    ];
    static WORK: [AtomicU64; WN] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

    #[inline]
    pub fn work(i: usize) {
        WORK[i].fetch_add(1, Relaxed);
    }
    pub fn work_snapshot() -> [u64; WN] {
        std::array::from_fn(|i| WORK[i].load(Relaxed))
    }
    pub const W_BEST_PART: usize = 0;
    pub const W_MB_PLAN: usize = 1;
    pub const W_MB_CODED: usize = 2;

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

fn sink() -> &'static Option<std::sync::Mutex<std::fs::File>> {
    use std::io::Write;
    static S: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| {
        std::env::var("RFF_SIGNALS_CSV").ok().and_then(|p| {
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
pub(crate) fn harvest(sig: &FrameSignals, slice: char, qp: u8, d: &GateDecisions) {
    use std::io::Write;
    if let Some(m) = sink() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
