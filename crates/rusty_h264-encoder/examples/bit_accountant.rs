//! BIT ACCOUNTANT (codec-analyzer instrument #6) — where do our bits go, and how
//! does that compare with x264's own MOTION/TEXTURE/MISC split at a matched
//! operating point?
//!
//! The remaining ~4% BD-rate gap vs x264 veryfast is a RATE question, so it needs
//! the rate instrument, not the stage profiler. Buckets are exact CABAC bit
//! deltas, so they reconcile against the real payload — the line that separates
//! an instrument from a model.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example bit_accountant \
//!     -- video-tests/clips/foreman_cif.y4m
use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn read_y4m(path: &str, max: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap();
    let e = raw.iter().position(|&b| b == b'\n').unwrap();
    let hdr = std::str::from_utf8(&raw[..e]).unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for t in hdr.split_whitespace() {
        match t.as_bytes().first() {
            Some(b'W') => w = t[1..].parse().unwrap(),
            Some(b'H') => h = t[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (mut f, mut p) = (Vec::new(), e + 1);
    while f.len() < max {
        let Some(r) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += r + 1;
        if p + ys + 2 * cs > raw.len() { break }
        f.push(YuvFrame {
            width: w, height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    (w, h, f)
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "video-tests/clips/foreman_cif.y4m".into());
    let n: usize = std::env::var("BA_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let qp: u8 = std::env::var("BA_QP").ok().and_then(|v| v.parse().ok()).unwrap_or(27);
    let (w, h, frames) = read_y4m(&path, n);
    let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    for (pname, preset) in [("quality", Preset::Quality)] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 60;
        cfg.preset = preset;
        rusty_h264_encoder::bitacct::reset();
        rusty_h264_encoder::bitacct::set_enabled(true);
        let mut enc = Encoder::new(cfg).unwrap();
        let mut total = 0usize;
        for f in &frames {
            total += enc.encode(f).len();
        }
        rusty_h264_encoder::bitacct::add_actual_bytes(total);
        rusty_h264_encoder::bitacct::set_enabled(false);
        let mbs = (w.div_ceil(16) * h.div_ceil(16) * frames.len()) as u64;
        rusty_h264_encoder::bitacct::dump(
            &format!("{name} {pname} qp{qp} x{} ({} bytes)", frames.len(), total),
            mbs,
        );
    }
}
