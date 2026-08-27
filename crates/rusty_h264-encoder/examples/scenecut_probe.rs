//! Scene-cut calibration probe (keyint campaign): prints each clip's MAX
//! frame-pair inter/intra ratio and the count of pairs at or above the cut
//! threshold — the false-positive evidence for the corpus, and the fire
//! evidence for spliced content. Uses the SHIPPING detector via the encoder's
//! own segmentation entry points (same arithmetic, no probe fork).
//!
//!   cargo run --release -p rusty_h264-encoder --example scenecut_probe -- <clip.y4m> [frames]

use rusty_h264_encoder::EncoderConfig;

fn read_y4m(path: &str, max_frames: usize) -> (usize, usize, Vec<rusty_h264_common::types::YuvFrame>) {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let hdr_end = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..hdr_end]).expect("utf8 header");
    let (mut w, mut h) = (0usize, 0usize);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().expect("width"),
            Some(b'H') => h = tok[1..].parse().expect("height"),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let mut frames = Vec::new();
    let mut p = hdr_end + 1;
    while frames.len() < max_frames {
        let Some(rel) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += rel + 1;
        if p + ys + 2 * cs > raw.len() {
            break;
        }
        frames.push(rusty_h264_common::types::YuvFrame {
            width: w,
            height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    (w, h, frames)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("clip");
    let nframes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let (w, h, frames) = read_y4m(&path, nframes);
    let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    let cfg = EncoderConfig::new(w, h); // defaults: scenecut 40 → threshold 0.60
    let ratios = rusty_h264_encoder::scene_cut_ratios(&cfg, &frames);
    let max = ratios.iter().cloned().fold(0.0f64, f64::max);
    let thresh = 1.0 - cfg.scenecut as f64 / 100.0;
    let flat = ratios.iter().filter(|&&r| r >= thresh).count();
    // v2 spike rule: high AND a jump over the recent baseline (min of the two
    // previous pair ratios) — a cut is a discontinuity, chaos is a plateau.
    let mut spike = 0usize;
    for i in 0..ratios.len() {
        let base = match i {
            0 => 1.0,
            1 => ratios[0],
            _ => ratios[i - 1].min(ratios[i - 2]),
        };
        if ratios[i] >= thresh && ratios[i] >= base + 0.25 {
            spike += 1;
        }
    }
    println!(
        "{name:<24} pairs={} max_ratio={max:.3} flat@{thresh:.2}={flat} spike={spike}",
        ratios.len()
    );
}
