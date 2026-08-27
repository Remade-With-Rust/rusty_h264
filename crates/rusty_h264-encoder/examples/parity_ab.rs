//! x264-parity A/B harness — ONE instrument for every knob campaign
//! (bframes, keyint/scenecut, trellis, weightp, b-pyramid), so five campaigns
//! don't fork five copies of the same driver (the A6 instrument law; BD math
//! stays in bench/bdmath.py).
//!
//! Encodes a clip at 4 QPs under each ARM of one KNOB and reports
//! (bytes, luma PSNR) + a CSV block. Arm 0 is the anchor.
//!
//!   cargo run --release -p rusty_h264-encoder --example parity_ab -- \
//!       <clip.y4m> <frames> <knob> <arm> [arm...]
//!
//!   knobs: refs N | bframes N|auto | gop N | trellis 0|1 | weightp 0|1 | bpyr 0|1

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
    assert_eq!(recon.len(), src.len(), "decoded frame count mismatch — dropped/extra pictures");
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

/// Applies one knob arm to a config. Extended per campaign; unknown knobs
/// panic loudly rather than silently measuring the default.
fn apply(cfg: &mut EncoderConfig, knob: &str, arm: &str) {
    match knob {
        "refs" => cfg.num_ref_frames = arm.parse().expect("refs arm"),
        "bframes" => {
            if arm == "auto" {
                // Matches the CLI's `--bframes auto`: cap 3, adaptive picker.
                cfg.bframes = 3;
                cfg.bframes_adaptive = true;
            } else {
                cfg.bframes = arm.parse().expect("bframes arm");
                cfg.bframes_adaptive = false;
            }
            if cfg.bframes > 0 && cfg.profile == rusty_h264_common::Profile::ConstrainedBaseline {
                cfg.profile = rusty_h264_common::Profile::Main;
            }
        }
        "gop" => cfg.gop_size = arm.parse().expect("gop arm"),
        "bpyr" => {
            // Fixed bframes=3 so the pyramid structurally applies; arm 0/1
            // toggles it.
            cfg.bframes = 3;
            cfg.bframes_adaptive = false;
            cfg.b_pyramid = arm == "1";
        }
        _ => panic!("unknown knob {knob} — extend apply() for the new campaign"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("clip path");
    let nframes: usize = args.next().and_then(|s| s.parse().ok()).expect("frame count");
    let knob = args.next().expect("knob");
    let arms: Vec<String> = args.collect();
    assert!(arms.len() >= 2, "need an anchor arm and at least one test arm");
    let (w, h, frames) = read_y4m(&path, nframes);
    let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    println!("{name} {w}x{h} x{} knob={knob}", frames.len());

    let mut csv = String::from("CSV clip,knob,arm,qp,bytes,psnr\n");
    for arm in &arms {
        for qp in [22u8, 27, 32, 37] {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 30;
            cfg.preset = Preset::Quality;
            apply(&mut cfg, &knob, arm);
            let bytes: Vec<u8> = Encoder::new(cfg)
                .expect("cfg")
                .encode_all(&frames)
                .expect("encode")
                .concat();
            let db = psnr(&bytes, &frames);
            println!("  {knob}={arm} qp{qp}: {:>8} B  {db:.4} dB", bytes.len());
            csv.push_str(&format!("CSV {name},{knob},{arm},{qp},{},{db:.6}\n", bytes.len()));
        }
    }
    print!("{csv}");
}
