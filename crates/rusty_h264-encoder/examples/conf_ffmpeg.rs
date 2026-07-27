//! STRICT EXTERNAL CONFORMANCE: encode a real clip, then require that FFMPEG's decoder
//! and OUR decoder produce PIXEL-IDENTICAL reconstructions.
//!
//! Our own decoder round-tripping is necessary but NOT sufficient — a self-consistent
//! stream can still be illegal (codec-bringup-encoder, gate 2). Any bitstream-changing
//! default (here: the ME diamond's rung set) must clear this before it flips.
//!
//!   RFF_DIA_LADDER=16,8,4 cargo run --release -p rusty_h264-encoder --features asm \
//!     --example conf_ffmpeg -- video-tests/clips/*.y4m
use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};
use std::process::Command;

fn read_y4m(path: &str, max: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap();
    let he = raw.iter().position(|&b| b == b'\n').unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for t in std::str::from_utf8(&raw[..he]).unwrap().split_whitespace() {
        match t.as_bytes().first() {
            Some(b'W') => w = t[1..].parse().unwrap(),
            Some(b'H') => h = t[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (mut f, mut p) = (Vec::new(), he + 1);
    while f.len() < max {
        let Some(r) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += r + 1;
        if p + ys + 2 * cs > raw.len() { break }
        f.push(YuvFrame { width: w, height: h, y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(), v: raw[p + ys + cs..p + ys + 2 * cs].to_vec() });
        p += ys + 2 * cs;
    }
    (w, h, f)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tmp = std::env::temp_dir();
    let (mut pass, mut fail) = (0, 0);
    for path in &args {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let (w, h, frames) = read_y4m(path, 12);
        for (pn, preset) in [("balanced", Preset::Balanced), ("quality", Preset::Quality)] {
            for qp in [24u8, 33] {
                let mut cfg = EncoderConfig::new(w, h);
                cfg.qp = qp; cfg.gop_size = 30; cfg.preset = preset;
                let stream: Vec<u8> = Encoder::new(cfg).unwrap().encode_all(&frames).unwrap().concat();
                let f264 = tmp.join(format!("conf_{name}_{pn}_{qp}.264"));
                let fyuv = tmp.join(format!("conf_{name}_{pn}_{qp}.yuv"));
                let _ = std::fs::remove_file(&fyuv); // never score a stale artifact
                std::fs::write(&f264, &stream).unwrap();
                let o = Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-i"])
                    .arg(&f264).args(["-pix_fmt", "yuv420p", "-f", "rawvideo"]).arg(&fyuv).output().unwrap();
                if !o.status.success() {
                    println!("  {name}/{pn}/qp{qp}: ffmpeg REJECTED — {}", String::from_utf8_lossy(&o.stderr).trim());
                    fail += 1; continue;
                }
                let ff = std::fs::read(&fyuv).unwrap();
                let ours = rusty_h264_decoder::Decoder::new().decode_stream(&stream).unwrap();
                let (ys, cs) = (w * h, (w / 2) * (h / 2));
                let fsz = ys + 2 * cs;
                if ff.len() / fsz != ours.len() {
                    println!("  {name}/{pn}/qp{qp}: FRAME COUNT ffmpeg={} ours={}", ff.len() / fsz, ours.len());
                    fail += 1; continue;
                }
                let mut diff = 0usize;
                for (i, r) in ours.iter().enumerate() {
                    let b = &ff[i * fsz..];
                    diff += r.y.iter().zip(&b[..ys]).filter(|(a, c)| a != c).count()
                        + r.u.iter().zip(&b[ys..ys + cs]).filter(|(a, c)| a != c).count()
                        + r.v.iter().zip(&b[ys + cs..ys + 2 * cs]).filter(|(a, c)| a != c).count();
                }
                if diff == 0 { pass += 1 } else {
                    println!("  {name}/{pn}/qp{qp}: {diff} PIXEL DIFFS vs ffmpeg");
                    fail += 1;
                }
                let _ = std::fs::remove_file(&f264);
                let _ = std::fs::remove_file(&fyuv);
            }
        }
        println!("{name:<26} done  (pass={pass} fail={fail})");
    }
    println!("\n=== ffmpeg pixel-exact: {pass} PASS / {fail} FAIL ===");
    if fail > 0 { std::process::exit(1) }
}
