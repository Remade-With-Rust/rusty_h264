//! bframes-v2 gate fit harness: per-clip gate signals next to the measured
//! B-frame BD truth (fixed3-vs-0 sign). Fit on the 12-clip table; the unused
//! corpus clips are the HOLDOUTS (holdout-both-sides law).
//!
//!   cargo run --release -p rusty_h264-encoder --example bframes_signals -- <clip.y4m> [frames]

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
    let nframes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let (w, h, frames) = read_y4m(&path, nframes);
    let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    let cfg = EncoderConfig::new(w, h);
    let (bi, gmc, mg, dc, screen, grain) = rusty_h264_encoder::bframes_gate_signals(&cfg, &frames);
    println!(
        "{name:<24} bi={bi:>7.3} gmc={gmc:>8.3} mgain={mg:.3} dcfrac={dc:.3} screen={} grain={}",
        screen as u8, grain as u8
    );
}
