//! Round-10 BD gate for the poly tier (fast-transcendentals plan, A3/Round 10).
//!
//! For each clip: encode at 4 QPs under BOTH arms (`RFF_POLYTIER=0` libm vs
//! `1` poly) in two pipeline configs — AQ (default-on, sites 5/7) and
//! AQ+mb-tree (sites 5/6/7/8) — and compare BITSTREAM hashes.
//!
//!   * All hashes equal ⇒ the poly tier is DECISION-IDENTICAL on this clip:
//!     the BD toll is 0.000% by identity, no rate/quality model needed.
//!   * Any hash differs ⇒ the harness prints (bytes, PSNR) per point; feed
//!     those to `bench/bdmath.py` for the BD verdict.
//!
//!   cargo run --release -p rusty_h264-encoder --example polytier_gate -- <clip.y4m> [frames]

use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

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

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Global luma PSNR of the decoded stream vs the (edge-clamped) source.
fn psnr(stream: &[u8], src: &[rusty_h264_common::types::YuvFrame]) -> f64 {
    let recon = rusty_h264_decoder::Decoder::new()
        .decode_stream(stream)
        .expect("decode own stream");
    let (mut se, mut n) = (0f64, 0u64);
    for (s, r) in src.iter().zip(&recon) {
        let (w, h) = (s.width.min(r.width), s.height.min(r.height));
        for y in 0..h {
            for x in 0..w {
                let d = s.y[y * s.width + x] as f64 - r.y[y * r.width + x] as f64;
                se += d * d;
                n += 1;
            }
        }
    }
    let mse = se / n.max(1) as f64;
    if mse <= 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "video-tests/clips/foreman_cif.y4m".into());
    let nframes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let (w, h, frames) = read_y4m(&path, nframes);
    let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    println!("{name} {w}x{h} x{}", frames.len());

    let mut all_identical = true;
    for (pname, mbtree) in [("aq", false), ("aq+mbtree", true)] {
        println!("  pipeline {pname}:");
        for qp in [22u8, 27, 32, 37] {
            let mut out: [Option<(usize, u64, Vec<u8>)>; 2] = [None, None];
            for (arm, env) in [(0usize, "0"), (1, "1")] {
                std::env::set_var("RFF_POLYTIER", env);
                let mut cfg = EncoderConfig::new(w, h);
                cfg.qp = qp;
                cfg.gop_size = 30;
                cfg.preset = Preset::Quality;
                cfg.mbtree = mbtree;
                let bytes: Vec<u8> = Encoder::new(cfg)
                    .expect("cfg")
                    .encode_all(&frames)
                    .expect("encode")
                    .concat();
                out[arm] = Some((bytes.len(), fnv1a(&bytes), bytes));
            }
            let (la, ha, sa) = out[0].take().unwrap();
            let (lb, hb, sb) = out[1].take().unwrap();
            if ha == hb {
                println!("    qp{qp}: IDENTICAL  ({la} bytes, fnv {ha:016x})");
            } else {
                all_identical = false;
                let (pa, pb) = (psnr(&sa, &frames), psnr(&sb, &frames));
                println!(
                    "    qp{qp}: DIFFERS  libm {la} B / {pa:.4} dB  vs  poly {lb} B / {pb:.4} dB"
                );
            }
        }
    }
    println!(
        "{}",
        if all_identical {
            "VERDICT: decision-identical on this clip — BD = 0.000% by identity."
        } else {
            "VERDICT: output moved — feed the (bytes, dB) points to bench/bdmath.py."
        }
    );
}
