//! Temporal-decay harvest (Great Gate P3 item 4 -- the mb-tree pan loser).
//!
//! Prints the `2-gap / 1-gap` motion-compensated residual ratio per clip: the
//! TEMPORAL predictability axis the mb-tree dispatch has been blocked on. The
//! previously-fitted candidate (`lv_spread & flat_run`) was refused because both
//! clauses are SPATIAL proxies for this quantity.
//!
//! Deterministic -- one run is the verdict (codec-measurement 15), no pinning.
//!
//!   cargo run --release --example tdecay -- <clip.yuv> WxH [frames]

use rusty_h264::{temporal_decay_ratio, YuvFrame};

fn load(path: &str, w: usize, h: usize, max: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    (0..(raw.len() / fsz).min(max))
        .map(|i| {
            let b = &raw[i * fsz..];
            YuvFrame {
                width: w,
                height: h,
                y: b[..ys].to_vec(),
                u: b[ys..ys + cs].to_vec(),
                v: b[ys + cs..ys + 2 * cs].to_vec(),
            }
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (w, h) = a[1].split_once('x').expect("WxH");
    let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let frames = load(&a[0], w, h, n);
    // Per-GOP grain: mb-tree's objective completes over a GOP, so report the
    // per-GOP ratios AND the clip median rather than one whole-clip number.
    let gop = 30usize;
    let mut rs: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + gop <= frames.len() {
        let r = temporal_decay_ratio(&frames[i..i + gop], w, h);
        if r.is_finite() {
            rs.push(r);
        }
        i += gop;
    }
    let whole = temporal_decay_ratio(&frames, w, h);
    rs.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let med = if rs.is_empty() { f64::NAN } else { rs[rs.len() / 2] };
    println!("{:.4},{:.4},{}", whole, med, rs.len());
}
