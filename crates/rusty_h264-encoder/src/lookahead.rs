//! Cheap pre-encode frame-complexity estimate for look-ahead rate control.
//!
//! Before a frame is encoded, the controller needs to know how hard it will be
//! to code so it can allocate bits proportionally — spending more on busy /
//! high-motion frames and less on simple ones, instead of reacting a frame late.
//! This module produces a single relative complexity score: spatial AC energy
//! for an IDR, and the best small-search motion-compensated residual SATD for a
//! P-frame. It is deliberate that this is far cheaper than a real encode.

#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;

use crate::config::EncoderConfig;
use crate::RefFrame;
use rusty_h264_common::inter::mc_luma;
use rusty_h264_common::transform::hadamard_4x4;
use rusty_h264_common::{YuvFrame, YuvPlanes};

/// SATD of a 4×4 residual (sum of absolute Hadamard-transform coefficients).
fn satd4(res: &[i32; 16]) -> i64 {
    hadamard_4x4(res)
        .iter()
        .map(|&v| v.unsigned_abs() as i64)
        .sum()
}

/// Edge-clamped coded-size luma (matches the encoder's source preparation).
fn coded_luma(cfg: &EncoderConfig, frame: &YuvPlanes<'_>) -> (Vec<u8>, usize, usize) {
    let (cw, ch) = (cfg.mb_width() * 16, cfg.mb_height() * 16);
    let (w, h) = (frame.width, frame.height);
    let mut y = vec![0u8; cw * ch];
    // Row-slice copy + right-edge fill instead of the per-pixel double-`min`
    // gather — the same rewrite `mbtree::coded_luma` already carries (and the
    // same `coded_luma_matches_per_pixel`-class oracle guards it there): the
    // interior becomes a `memcpy` + `memset` per row, the bottom padding
    // re-copies row `h-1`. Bit-identical output, two `min`s + a mul + a bounds
    // check per PIXEL removed.
    let wc = w.min(cw);
    for j in 0..ch {
        let src = &frame.y[j.min(h - 1) * w..][..w];
        let dst = &mut y[j * cw..][..cw];
        dst[..wc].copy_from_slice(&src[..wc]);
        dst[wc..].fill(src[w - 1]);
    }
    (y, cw, ch)
}

/// A cheap relative complexity score for the frame. For an IDR (`reference` =
/// `None`) it sums per-4×4-block spatial AC energy; for a P-frame it sums each
/// macroblock's best motion-compensated residual SATD over a small fixed full-pel
/// candidate set. Always ≥ 1 so the controller never divides by zero.
pub fn complexity(cfg: &EncoderConfig, frame: &YuvPlanes<'_>, reference: Option<&RefFrame>) -> f64 {
    let (sy, cw, ch) = coded_luma(cfg, frame);
    let (mb_w, mb_h) = (cfg.mb_width(), cfg.mb_height());
    let mut total = 0i64;
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            total += match reference {
                None => intra_activity(&sy, cw, mb_x, mb_y),
                Some(r) => inter_activity(&sy, cw, ch, &r.y, mb_x, mb_y),
            };
        }
    }
    (total as f64).max(1.0)
}

/// Spatial activity of a macroblock: per-block AC SATD (DC excluded), summed.
fn intra_activity(sy: &[u8], cw: usize, mb_x: usize, mb_y: usize) -> i64 {
    let mut s = 0;
    for by in 0..4 {
        for bx in 0..4 {
            let mut blk = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    blk[dy * 4 + dx] =
                        sy[(mb_y * 16 + by * 4 + dy) * cw + mb_x * 16 + bx * 4 + dx] as i32;
                }
            }
            let h = hadamard_4x4(&blk);
            s += h[1..].iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
        }
    }
    s
}

// The pair ratio is the mb-tree lookahead estimator (half-res DIAMOND search;
// `mbtree::pair_ratio_prepped`). A first draft used this module's ±2px
// `inter_activity` and the corpus probe refuted it immediately: six of twelve
// clips fired on nearly every pair (bus 59/59, shields 58/59) because a probe
// that cannot reach the true motion vector reads every fast pan as a scene
// change. The RC keeps the cheap estimator (relative complexity is fine at
// ±2px); a CUT decision is not.

/// GOP segmentation for a batch of frames: the start index of every IDR
/// segment, honoring `gop_size` (the forced-refresh ceiling), `min_keyint`
/// (no cut-IDR closer than this) and `scenecut` (the x264 threshold rule —
/// cut when the pair ratio reaches `1 - scenecut/100`).
///
/// CAUSAL by construction (each decision reads only the pair `(i-1, i)`), so
/// the streaming path reproduces it frame-by-frame and streaming == batch
/// holds under scene cuts exactly as it did under fixed cadence. With
/// `scenecut == 0` this returns the fixed `chunks(gop_size)` boundaries —
/// byte-identical to the pre-scenecut encoder, the bisection anchor.
/// The spike margin over the recent baseline a cut must clear. A cut is a
/// DISCONTINUITY; hard-to-predict content is a PLATEAU — the corpus probe
/// showed six clips (bus, football, city, crew, grain, mobile) sitting above
/// any workable flat threshold on nearly every pair, with zero spikes, while
/// a genuine splice jumps ~0.3 → ~0.95. Calibrated 2026-08-26: 12-clip scan,
/// 0 false fires at 0.25 (one shields pair at ratio 0.999 fires — a
/// near-total prediction failure that merits the IDR either way).
pub(crate) const SCENECUT_SPIKE: f64 = 0.25;

/// The cut decision for pair ratio `r` against the two preceding pair ratios
/// (the causal baseline both the batch and streaming paths carry).
pub(crate) fn is_scene_cut(cfg: &EncoderConfig, r: f64, prev1: f64, prev2: f64) -> bool {
    let thresh = 1.0 - (cfg.scenecut.min(100) as f64 / 100.0);
    r >= thresh && r >= prev1.min(prev2) + SCENECUT_SPIKE
}

/// All pair ratios for a batch (index `p` = pair `frames[p] → frames[p+1]`),
/// with a ROLLING per-frame prep: each frame's coded+half-res planes are built
/// once instead of twice (`windows(2)` re-prepped every interior frame as the
/// next pair's `prev`). Same ratios, half the preparation.
pub(crate) fn all_pair_ratios(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames.len().saturating_sub(1));
    let mut prev = match frames.first() {
        Some(f) => crate::mbtree::pair_prep(cfg, &f.as_planes()),
        None => return out,
    };
    for f in &frames[1..] {
        let cur = crate::mbtree::pair_prep(cfg, &f.as_planes());
        out.push(crate::mbtree::pair_ratio_prepped(cfg, &cur, &prev));
        prev = cur;
    }
    out
}

pub(crate) fn segment_gops(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<usize> {
    let keyint = cfg.gop_size.max(1) as usize;
    let minki = (cfg.min_keyint.max(1) as usize).min(keyint);
    let detect = cfg.scenecut > 0 && keyint > 1 && frames.len() > 1;
    // LAZY pair scorer with a ROLLING per-frame prep — two exact savings over
    // the eager `windows(2)` map this replaces:
    //  1. each interior frame was prepped TWICE (as a pair's `cur`, then as
    //     the next pair's `prev`); the rolling cache preps it once;
    //  2. a decision at `i` reads `scores[i-1]` and baselines back to
    //     `scores[i-3]`, and only fires when `i - last >= minki` — so the
    //     first `minki - 3` pairs after every segment start are UNREADABLE and
    //     were scored anyway. The cursor fills forward on demand and jumps
    //     over them.
    // Every score any decision reads is computed by the same functions on the
    // same frames: the segmentation is BIT-IDENTICAL (skipped entries were
    // never consulted — they stay NaN and a read would poison the comparison
    // into `false`, not silently pass).
    let mut scores = vec![f64::NAN; frames.len().saturating_sub(1)];
    let mut scored_to = 0usize; // scores[..scored_to] are filled
    let mut prep: Option<(usize, crate::mbtree::PairPrep)> = None; // (frame, its prep)
    let mut starts = vec![0usize];
    let mut last = 0usize;
    for i in 1..frames.len() {
        let force = i - last >= keyint;
        let cut = !force && detect && i - last >= minki && {
            // Jump the cursor past pairs no decision (this one or any later —
            // reads are monotone in `i`) can consult, then fill to `i`.
            if scored_to < i.saturating_sub(3) {
                scored_to = i - 3;
                prep = None;
            }
            while scored_to < i {
                let p = scored_to;
                let prev_prep = match prep.take() {
                    Some((idx, pp)) if idx == p => pp,
                    _ => crate::mbtree::pair_prep(cfg, &frames[p].as_planes()),
                };
                let cur_prep = crate::mbtree::pair_prep(cfg, &frames[p + 1].as_planes());
                scores[p] = crate::mbtree::pair_ratio_prepped(cfg, &cur_prep, &prev_prep);
                prep = Some((p + 1, cur_prep));
                scored_to = p + 1;
            }
            let r = scores[i - 1];
            let prev1 = if i >= 2 { scores[i - 2] } else { 1.0 };
            let prev2 = if i >= 3 { scores[i - 3] } else { prev1 };
            is_scene_cut(cfg, r, prev1, prev2)
        };
        if force || cut {
            starts.push(i);
            last = i;
        }
    }
    starts
}

#[cfg(test)]
mod scenecut_tests {
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

    fn frame(w: usize, h: usize, style: u8, t: usize) -> YuvFrame {
        let mut y = vec![0u8; w * h];
        for j in 0..h {
            for i in 0..w {
                y[j * w + i] = match style {
                    // Scene A: textured diagonal pan (t shifts the pattern).
                    0 => 60 + (((i + t * 2) * 7 + j * 13) % 120) as u8,
                    // Scene B: STATIC inverted coarse checkerboard — visually
                    // unrelated to A, continuous with itself. (A first draft
                    // phase-flipped it every 8 frames via `t/8` — and the
                    // detector correctly fired on that full-frame flip, which
                    // is a point for its sensitivity and against the content.)
                    _ => 200u8.wrapping_sub((((i / 8 + j / 8) % 2) as u8) * 140),
                };
            }
        }
        YuvFrame {
            width: w,
            height: h,
            y,
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
        }
    }

    /// The keyint model end to end at the segmentation level: a splice of two
    /// unrelated scenes cuts EXACTLY at the splice; a continuous pan never
    /// cuts; `min_keyint` suppresses a too-early cut; the `gop_size` ceiling
    /// forces refresh; `scenecut = 0` reproduces fixed chunks (the anchor).
    #[test]
    fn segmentation_cuts_at_the_splice_and_only_there() {
        let (w, h) = (96, 80);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 250;
        cfg.min_keyint = 5;
        cfg.scenecut = 40;
        // 12 frames of scene A, then 12 of scene B.
        let spliced: Vec<YuvFrame> = (0..12)
            .map(|t| frame(w, h, 0, t))
            .chain((0..12).map(|t| frame(w, h, 1, t)))
            .collect();
        assert_eq!(
            segment_gops(&cfg, &spliced),
            vec![0, 12],
            "cut exactly at the splice"
        );
        // Continuous pan: no cuts (the false-positive guard the corpus scan
        // extends to real content).
        let pan: Vec<YuvFrame> = (0..24).map(|t| frame(w, h, 0, t)).collect();
        assert_eq!(
            segment_gops(&cfg, &pan),
            vec![0],
            "no cut on a continuous pan"
        );
        // min_keyint suppression: same splice, floor above it.
        cfg.min_keyint = 20;
        assert_eq!(
            segment_gops(&cfg, &spliced),
            vec![0],
            "min_keyint suppresses the cut"
        );
        // keyint ceiling forces refresh regardless of content.
        cfg.min_keyint = 5;
        cfg.gop_size = 10;
        assert_eq!(
            segment_gops(&cfg, &pan),
            vec![0, 10, 20],
            "forced refresh at keyint"
        );
        // scenecut = 0: fixed chunks — the bisection anchor.
        cfg.scenecut = 0;
        assert_eq!(
            segment_gops(&cfg, &spliced),
            vec![0, 10, 20],
            "scenecut=0 is fixed cadence"
        );
    }

    /// Streaming == batch WITH A FIRING CUT, through the real encoder API —
    /// the identity the lazy batch cursor and the streaming skip-window must
    /// preserve (their exactness argument is "skipped scores are unreadable";
    /// this is the gate that proves it on content where the detector actually
    /// routes). Also asserts the cut fired (2 IDRs), so the gate cannot pass
    /// on the fallback (gate-must-prove-the-tool-ran).
    #[test]
    fn streaming_equals_batch_with_a_firing_cut() {
        let (w, h) = (96, 80);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 250;
        cfg.min_keyint = 5;
        cfg.scenecut = 40;
        let spliced: Vec<YuvFrame> = (0..12)
            .map(|t| frame(w, h, 0, t))
            .chain((0..12).map(|t| frame(w, h, 1, t)))
            .collect();
        let batch: Vec<u8> = crate::Encoder::new(cfg.clone())
            .unwrap()
            .encode_all(&spliced)
            .unwrap()
            .concat();
        let mut enc = crate::Encoder::new(cfg).unwrap();
        let mut stream = Vec::new();
        for f in &spliced {
            stream.extend_from_slice(&enc.try_encode(f).unwrap());
        }
        stream.extend_from_slice(&enc.flush());
        assert_eq!(
            stream, batch,
            "streaming must reproduce the batch bytes under a cut"
        );
        let idrs = stream
            .windows(5)
            .filter(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 5)
            .count();
        assert_eq!(
            idrs, 2,
            "the splice cut must actually fire (start IDR + cut IDR)"
        );
    }
}

/// Best motion-compensated residual SATD of a macroblock over a small full-pel
/// candidate set (a cheap stand-in for the encoder's real motion search).
fn inter_activity(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8], mb_x: usize, mb_y: usize) -> i64 {
    // MVs in quarter-pel units: (0,0) and ±1 / ±2 full samples on each axis.
    const CANDS: [(i32, i32); 9] = [
        (0, 0),
        (4, 0),
        (-4, 0),
        (0, 4),
        (0, -4),
        (8, 0),
        (-8, 0),
        (0, 8),
        (0, -8),
    ];
    let mut best = i64::MAX;
    for &(mvx, mvy) in &CANDS {
        let mut pred = [0u8; 256];
        mc_luma(
            ref_y,
            cw,
            ch,
            mb_x * 16,
            mb_y * 16,
            16,
            16,
            mvx,
            mvy,
            &mut pred,
        );
        let mut s = 0;
        for by in 0..4 {
            for bx in 0..4 {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        res[dy * 4 + dx] =
                            sy[(mb_y * 16 + by * 4 + dy) * cw + mb_x * 16 + bx * 4 + dx] as i32
                                - pred[(by * 4 + dy) * 16 + (bx * 4 + dx)] as i32;
                    }
                }
                s += satd4(&res);
            }
        }
        best = best.min(s);
    }
    best
}
