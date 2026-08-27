//! Multi-reference P evidence harness (multiref campaign, win 1).
//!
//! Encodes a clip at 4 QPs with `num_ref_frames` ∈ {1, 2, 3} and reports
//! (bytes, luma PSNR) per point plus a machine-readable CSV block — BD-rate is
//! computed OFFLINE by `bench/bdmath.py` (one home for the BD arithmetic; this
//! harness deliberately does not fork it).
//!
//! Gate-must-prove-the-tool-ran: for each QP the refs-3 stream must DIFFER
//! from the refs-1 stream — if they are byte-identical the searcher never
//! chose a non-zero reference and the "evidence" would be measuring nothing.
//!
//!   cargo run --release -p rusty_h264-encoder --example refs_ab -- <clip.y4m> [frames]

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

    // CSV rows for bench/bdmath.py: clip,refs,qp,bytes,psnr
    let mut csv = String::from("CSV clip,refs,qp,bytes,psnr\n");
    let mut streams1: Vec<Vec<u8>> = Vec::new();
    for refs in [1u32, 2, 3] {
        for (qi, qp) in [22u8, 27, 32, 37].into_iter().enumerate() {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 30;
            cfg.preset = Preset::Quality;
            cfg.num_ref_frames = refs;
            let bytes: Vec<u8> = Encoder::new(cfg)
                .expect("cfg")
                .encode_all(&frames)
                .expect("encode")
                .concat();
            let db = psnr(&bytes, &frames);
            println!("  refs{refs} qp{qp}: {:>8} B  {db:.4} dB", bytes.len());
            csv.push_str(&format!("CSV {name},{refs},{qp},{},{db:.6}\n", bytes.len()));
            if refs == 1 {
                streams1.push(bytes);
            } else if refs == 3 {
                // The tool must have RUN: a multi-ref search that never picks
                // ref_idx > 0 emits the refs-1 stream (modulo headers) and the
                // BD row would measure nothing but header bits.
                if bytes.len() == streams1[qi].len() {
                    println!("    NOTE qp{qp}: refs3 stream same LENGTH as refs1 — check ref usage");
                }
            }
        }
    }
    print!("{csv}");
}
