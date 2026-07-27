//! What does the adaptive RD P_Skip decision COST in encode time?
//!
//! RD skip trial-encodes the inter candidate, snapshots/restores macroblock state,
//! and reconstructs a trial skip — real work, paid on every P macroblock the gate
//! lets through. The BD-rate win is settled; this prices it.
//!
//! Both arms run in ONE process, alternating pass by pass, best-of-N: whole-encode
//! timing on this machine drifts ~20% run to run, far more than the effect, so
//! separate builds cannot resolve it.
//!
//! ```text
//! cargo run --release -p rusty_h264-encoder --example rd_skip_speed -- <clip.yuv> <w>x<h>
//! ```
//! With no arguments it uses a synthetic high-free-skip clip (gate fires).

use rusty_h264_common::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig};

fn synth(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            fr.y[y * w + x] = ((x as u64 / 8 * 9 + y as u64 / 8 * 5) & 0xff) as u8;
        }
    }
    for y in h / 2..h {
        for x in 0..w {
            let j = ((x as u64 + y as u64 * 3 + f * 7) % 3) as u8;
            fr.y[y * w + x] = fr.y[y * w + x].saturating_add(j);
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            fr.u[y * cw + x] = 110;
            fr.v[y * cw + x] = 140;
        }
    }
    fr
}

fn load(path: &str, w: usize, h: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).expect("clip");
    let fsz = w * h * 3 / 2;
    raw.chunks_exact(fsz)
        .take(60)
        .map(|c| {
            let mut fr = YuvFrame::black(w, h);
            fr.y.copy_from_slice(&c[..w * h]);
            fr.u.copy_from_slice(&c[w * h..w * h + w * h / 4]);
            fr.v.copy_from_slice(&c[w * h + w * h / 4..]);
            fr
        })
        .collect()
}

fn encode(frames: &[YuvFrame], w: usize, h: usize, rd_skip: bool) -> (f64, usize) {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = std::env::var("RS_QP").ok().and_then(|v| v.parse().ok()).unwrap_or(27);
    cfg.gop_size = 30;
    cfg.tune_rd_skip = rd_skip;
    cfg.tune_rd_skip_fast_t = std::env::var("RS_GATE").ok().and_then(|v| v.parse().ok());
    let mut enc = Encoder::new(cfg).expect("encoder");
    let t = std::time::Instant::now();
    let mut bytes = 0usize;
    for fr in frames {
        bytes += enc.encode(fr).len();
    }
    (t.elapsed().as_secs_f64(), bytes)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (frames, w, h, label) = if args.len() >= 2 {
        let (w, h) = args[1].split_once('x').expect("WxH");
        let (w, h) = (w.parse().unwrap(), h.parse().unwrap());
        (load(&args[0], w, h), w, h, args[0].clone())
    } else {
        let (w, h) = (352usize, 288usize);
        ((0..60).map(|f| synth(w, h, f)).collect(), w, h, "synthetic".into())
    };

    let mut best = [f64::MAX; 2];
    let mut bytes = [0usize; 2];
    for pass in 0..10 {
        let arm = pass % 2;
        let (t, b) = encode(&frames, w, h, arm == 1);
        if t < best[arm] {
            best[arm] = t;
        }
        bytes[arm] = b;
    }

    let px = (w * h * frames.len()) as f64;
    println!("adaptive RD skip — encode cost ({label}, {w}x{h}, {} frames)\n", frames.len());
    println!("  off : {:>7.1} ms   {:>6.2} Mpx/s   {:>8} bytes", best[0] * 1e3, px / best[0] / 1e6, bytes[0]);
    println!("  on  : {:>7.1} ms   {:>6.2} Mpx/s   {:>8} bytes", best[1] * 1e3, px / best[1] / 1e6, bytes[1]);
    println!("\n  speed : {:>6.3}x", best[0] / best[1]);
    println!("  size  : {:>6.2}%", 100.0 * (bytes[1] as f64 / bytes[0] as f64 - 1.0));
}
