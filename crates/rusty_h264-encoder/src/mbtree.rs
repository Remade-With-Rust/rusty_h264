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

use crate::config::EncoderConfig;
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
    hadamard_4x4(res).iter().map(|&v| v.unsigned_abs() as i64).sum()
}

/// Edge-clamped coded-size luma (matches the encoder's source preparation).
fn coded_luma(cfg: &EncoderConfig, frame: &YuvFrame) -> Vec<u8> {
    let (cw, ch) = (cfg.mb_width() * 16, cfg.mb_height() * 16);
    let (w, h) = (frame.width, frame.height);
    let mut y = vec![0u8; cw * ch];
    for j in 0..ch {
        for i in 0..cw {
            y[j * cw + i] = frame.y[j.min(h - 1) * w + i.min(w - 1)];
        }
    }
    y
}

/// 2×2-average downsample of a luma plane to half resolution (both dims are MB
/// multiples → stay even). The half-res lookahead runs the ME on this: 4× fewer
/// pixels, so ~4× cheaper, at the cost of ½-pel-of-half-res motion granularity —
/// plenty for the mb-tree propagation DIRECTION (BD-rate-verified to hold).
fn downsample2x(y: &[u8], cw: usize, ch: usize) -> (Vec<u8>, usize, usize) {
    let (hw, hh) = (cw / 2, ch / 2);
    let mut out = vec![0u8; hw * hh];
    for j in 0..hh {
        for i in 0..hw {
            let s = y[2 * j * cw + 2 * i] as u32
                + y[2 * j * cw + 2 * i + 1] as u32
                + y[(2 * j + 1) * cw + 2 * i] as u32
                + y[(2 * j + 1) * cw + 2 * i + 1] as u32;
            out[j * hw + i] = ((s + 2) / 4) as u8;
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
fn mc_satd(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8], bx0: usize, by0: usize, bs: usize, mv: (i32, i32)) -> i64 {
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
fn inter_cost(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8], bx0: usize, by0: usize, bs: usize, seed: (i32, i32)) -> (i32, (i32, i32)) {
    let mut best_mv = (0, 0);
    let mut best = mc_satd(sy, cw, ch, ref_y, bx0, by0, bs, (0, 0));
    if seed != (0, 0) {
        let s = mc_satd(sy, cw, ch, ref_y, bx0, by0, bs, seed);
        if s < best {
            best = s;
            best_mv = seed;
        }
    }
    let mut step = 8i32;
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

/// Per-MB costs for one frame. Blocks are `bs`×`bs` (bs=16 full-res, 8 half-res);
/// MVs are scaled by `16/bs` back to FULL-res quarter-pel so propagation is
/// resolution-independent. `ref_y = None` (the IDR) → intra-only.
fn frame_costs(sy: &[u8], cw: usize, ch: usize, mb_w: usize, mb_h: usize, ref_y: Option<&[u8]>, bs: usize) -> Vec<MbCost> {
    let mv_scale = (16 / bs) as i32;
    let mut out: Vec<MbCost> = Vec::with_capacity(mb_w * mb_h);
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            let (bx0, by0) = (mb_x * bs, mb_y * bs);
            let intra = intra_cost(sy, cw, bx0, by0, bs);
            let (inter, mv) = match ref_y {
                Some(r) => {
                    // Seed from the neighbour's MV (left, else above) — a pan is
                    // spatially coherent. Stored MVs are FULL-res quarter-pel, so
                    // convert to plane units for the search, then scale the result back.
                    let seed_full = if mb_x > 0 {
                        out[mb_y * mb_w + mb_x - 1].mv
                    } else if mb_y > 0 {
                        out[(mb_y - 1) * mb_w + mb_x].mv
                    } else {
                        (0, 0)
                    };
                    let seed = (seed_full.0 / mv_scale, seed_full.1 / mv_scale);
                    let (ic, mvp) = inter_cost(sy, cw, ch, r, bx0, by0, bs, seed);
                    (ic.min(intra), (mvp.0 * mv_scale, mvp.1 * mv_scale)) // → full-res qpel
                }
                None => (intra, (0, 0)),
            };
            out.push(MbCost { intra, inter, mv });
        }
    }
    out
}

/// Distribute `amount` from frame `f`'s MB (referencing the previous frame at MV
/// `mv`) into `prev`'s per-MB propagation accumulator, area-weighted over the up-to-4
/// macroblocks the referenced 16×16 block overlaps (edge-clamped).
fn propagate_to(prev: &mut [f64], mb_w: usize, mb_h: usize, mb_x: usize, mb_y: usize, mv: (i32, i32), amount: f64) {
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

/// mb-tree per-frame per-MB QP offsets for a GOP of SOURCE frames (display order,
/// the IDR first). `strength <= 0` returns all-zero (no-op / byte-identical). The
/// offsets are centered per GOP so the mean QP — hence the rate — is preserved.
pub fn gop_qp_offsets(cfg: &EncoderConfig, frames: &[YuvFrame], strength: f64) -> Vec<Vec<i32>> {
    let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
    let n = frames.len();
    if strength <= 0.0 || n == 0 || mb_w * mb_h == 0 {
        return vec![vec![0i32; mb_w * mb_h]; n];
    }
    // HALF-RES lookahead (OPT-IN speed lever, `cfg.mbtree_halfres`): run the ME on
    // 2×-downsampled planes (8×8 blocks, ~4× cheaper → ~33% faster encode). It's a
    // measured speed/QUALITY TRADE — downsampling blurs fine detail, so the cost/
    // propagation estimates lose accuracy (mand −0.19%→+0.12%, tsrc −1.80%→−1.28%);
    // hence full-res is the DEFAULT (never regresses) and this is off unless asked.
    // The propagation is resolution-independent (MVs scaled back to full-res below).
    let half = cfg.mbtree_halfres || std::env::var("RFF_MBTREE_HALFRES").is_ok();
    let (cw, ch, bs) = if half { (mb_w * 8, mb_h * 8, 8) } else { (mb_w * 16, mb_h * 16, 16) };
    let luma: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| {
            let full = coded_luma(cfg, f);
            if half {
                downsample2x(&full, mb_w * 16, mb_h * 16).0
            } else {
                full
            }
        })
        .collect();
    // 1. per-frame per-MB costs (frame 0 = IDR, intra-only).
    let costs: Vec<Vec<MbCost>> = (0..n)
        .map(|f| {
            let r = if f == 0 { None } else { Some(luma[f - 1].as_slice()) };
            frame_costs(&luma[f], cw, ch, mb_w, mb_h, r, bs)
        })
        .collect();
    // 2. backward propagation: each MB credits the fraction its predictor earned to
    //    the previous frame's referenced MBs.
    let mut propagate: Vec<Vec<f64>> = vec![vec![0.0; mb_w * mb_h]; n];
    for f in (1..n).rev() {
        let (head, tail) = propagate.split_at_mut(f);
        let cur = &tail[0];
        let prev = &mut head[f - 1];
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let m = mb_y * mb_w + mb_x;
                let c = costs[f][m];
                let total = c.intra as f64 + cur[m];
                // Fraction of this MB's cost the previous frame's reference "carries".
                let frac = (c.intra - c.inter) as f64 / c.intra as f64; // in [0,1]
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
    const MBTREE_RES_MIN: f64 = 0.10;
    let (mut fsum, mut fc) = (0f64, 0f64);
    for f in 1..n {
        for m in 0..mb_w * mb_h {
            let c = costs[f][m];
            fsum += (c.intra - c.inter) as f64 / c.intra as f64;
            fc += 1.0;
        }
    }
    let residual_frac = 1.0 - if fc > 0.0 { fsum / fc } else { 0.0 };
    let eff_strength = strength * (residual_frac / MBTREE_RES_MIN).clamp(0.0, 1.0);
    // 3. QP offset per MB (≤ 0), then center per GOP to preserve the mean QP.
    let mut offs: Vec<Vec<f64>> = (0..n)
        .map(|f| {
            (0..mb_w * mb_h)
                .map(|m| {
                    let intra = costs[f][m].intra as f64;
                    let total = intra + propagate[f][m];
                    -eff_strength * (total / intra).log2()
                })
                .collect()
        })
        .collect();
    let cnt = (n * mb_w * mb_h) as f64;
    let mean: f64 = offs.iter().flatten().sum::<f64>() / cnt;
    for fr in &mut offs {
        for o in fr.iter_mut() {
            *o -= mean;
        }
    }
    // The spread of the (centered) offsets measures how much mb-tree DIFFERENTIATES
    // macroblocks: high spread = distinct referenced/leaf regions (it helps); low
    // spread = near-uniform importance (a pan — nothing to redistribute toward, so it
    // only adds QP noise). The dispatch signal.
    if std::env::var("RFF_MBTREE_DBG").is_ok() {
        let sd = (offs.iter().flatten().map(|o| o * o).sum::<f64>() / cnt).sqrt();
        eprintln!("MBTREE_DBG spread={sd:.3} residual_frac={residual_frac:.3} eff={eff_strength:.3}");
    }
    // Round + clamp to a sane per-MB QP swing.
    const MBTREE_DQP_MAX: i32 = 6;
    offs.iter()
        .map(|fr| {
            fr.iter()
                .map(|&o| (o.round() as i32).clamp(-MBTREE_DQP_MAX, MBTREE_DQP_MAX))
                .collect()
        })
        .collect()
}
