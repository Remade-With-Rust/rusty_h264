//! Cheap pre-encode frame-complexity estimate for look-ahead rate control.
//!
//! Before a frame is encoded, the controller needs to know how hard it will be
//! to code so it can allocate bits proportionally — spending more on busy /
//! high-motion frames and less on simple ones, instead of reacting a frame late.
//! This module produces a single relative complexity score: spatial AC energy
//! for an IDR, and the best small-search motion-compensated residual SATD for a
//! P-frame. It is deliberate that this is far cheaper than a real encode.

use crate::config::EncoderConfig;
use crate::RefFrame;
use rusty_h264_common::inter::mc_luma;
use rusty_h264_common::transform::hadamard_4x4;
use rusty_h264_common::YuvFrame;

/// SATD of a 4×4 residual (sum of absolute Hadamard-transform coefficients).
fn satd4(res: &[i32; 16]) -> i64 {
    hadamard_4x4(res).iter().map(|&v| v.unsigned_abs() as i64).sum()
}

/// Edge-clamped coded-size luma (matches the encoder's source preparation).
fn coded_luma(cfg: &EncoderConfig, frame: &YuvFrame) -> (Vec<u8>, usize, usize) {
    let (cw, ch) = (cfg.mb_width() * 16, cfg.mb_height() * 16);
    let (w, h) = (frame.width, frame.height);
    let mut y = vec![0u8; cw * ch];
    for j in 0..ch {
        for i in 0..cw {
            y[j * cw + i] = frame.y[j.min(h - 1) * w + i.min(w - 1)];
        }
    }
    (y, cw, ch)
}

/// A cheap relative complexity score for the frame. For an IDR (`reference` =
/// `None`) it sums per-4×4-block spatial AC energy; for a P-frame it sums each
/// macroblock's best motion-compensated residual SATD over a small fixed full-pel
/// candidate set. Always ≥ 1 so the controller never divides by zero.
pub fn complexity(cfg: &EncoderConfig, frame: &YuvFrame, reference: Option<&RefFrame>) -> f64 {
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

/// Best motion-compensated residual SATD of a macroblock over a small full-pel
/// candidate set (a cheap stand-in for the encoder's real motion search).
fn inter_activity(sy: &[u8], cw: usize, ch: usize, ref_y: &[u8], mb_x: usize, mb_y: usize) -> i64 {
    // MVs in quarter-pel units: (0,0) and ±1 / ±2 full samples on each axis.
    const CANDS: [(i32, i32); 9] = [
        (0, 0), (4, 0), (-4, 0), (0, 4), (0, -4), (8, 0), (-8, 0), (0, 8), (0, -8),
    ];
    let mut best = i64::MAX;
    for &(mvx, mvy) in &CANDS {
        let mut pred = [0u8; 256];
        mc_luma(ref_y, cw, ch, mb_x * 16, mb_y * 16, 16, 16, mvx, mvy, &mut pred);
        let mut s = 0;
        for by in 0..4 {
            for bx in 0..4 {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        res[dy * 4 + dx] = sy
                            [(mb_y * 16 + by * 4 + dy) * cw + mb_x * 16 + bx * 4 + dx]
                            as i32
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
