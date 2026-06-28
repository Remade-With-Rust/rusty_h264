//! Deterministic encode benchmark — the honest A/B measure for encoder asm bricks
//! (see the optimize-codec playbook). Throughput, best-of-N, in-process (no ffmpeg).
//!
//! ```text
//! # baseline (SSE2 asm) and after an AVX2 brick — compare the medians:
//! cargo test -p rusty_h264-encoder --release --features asm profile_encode -- --ignored --nocapture
//! # pure-Rust scalar reference:
//! cargo test -p rusty_h264-encoder --release          profile_encode -- --ignored --nocapture
//! ```

use rusty_h264_common::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig};

/// Deterministic clip: textured background panning left + four moving textured
/// boxes — enough intra detail + inter motion + residual to exercise the transform,
/// quant, ME, and reconstruction asm paths representatively.
fn make_clip(w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    let bg: Vec<u8> = (0..w * h)
        .map(|idx| {
            let (i, j) = (idx % w, idx / w);
            (((i * 3 + j * 2) ^ ((i * 7) & (j * 5)) ^ (i * j >> 5)) & 0xff) as u8
        })
        .collect();
    let (cw, ch) = (w / 2, h / 2);
    (0..n)
        .map(|t| {
            let mut y = vec![0u8; w * h];
            for j in 0..h {
                for i in 0..w {
                    y[j * w + i] = bg[j * w + ((i + t * 3) % w)];
                }
            }
            for (k, &(sx, sy, sp)) in [(40, 30, 5usize), (150, 90, 7), (250, 180, 4), (80, 200, 6)]
                .iter()
                .enumerate()
            {
                let bx = (sx + t * sp) % (w - 40);
                let by = (sy + t * sp.saturating_sub(2)) % (h - 40);
                let base = (40 + k * 50) as u8;
                for dy in 0..36 {
                    for dx in 0..36 {
                        y[(by + dy) * w + (bx + dx)] = base ^ (((dx ^ dy) & 0x1f) as u8);
                    }
                }
            }
            let chroma = |off: usize| -> Vec<u8> {
                (0..cw * ch)
                    .map(|idx| (128 + (((idx + t + off) >> 3) & 0x0f) as i32 - 8) as u8)
                    .collect()
            };
            YuvFrame { width: w, height: h, y, u: chroma(0), v: chroma(5) }
        })
        .collect()
}

fn run(label: &str, w: usize, h: usize, n: usize, gop: u32) {
    let frames = make_clip(w, h, n);
    const REPS: usize = 5;
    let mut best = std::time::Duration::MAX;
    let mut bytes = 0usize;
    for _ in 0..REPS {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = gop;
        cfg.qp = 26;
        let mut enc = Encoder::new(cfg).expect("encoder init");
        let t = std::time::Instant::now();
        let mut b = 0;
        for f in &frames {
            b += enc.encode(f).len();
        }
        best = best.min(t.elapsed());
        bytes = b;
    }
    let px = n * w * h;
    let mpx_s = px as f64 / best.as_secs_f64() / 1e6;
    eprintln!(
        "  {label:<10} {:>6.1} Mpx/s  (best of {REPS}: {:>6.1} ms, {} KiB out)",
        mpx_s,
        best.as_secs_f64() * 1e3,
        bytes / 1024
    );
}

#[test]
#[ignore = "perf instrument: run with --release [--features asm] -- --ignored --nocapture"]
fn profile_encode() {
    eprintln!("\n=== profile_encode (832x480 x12 frames, QP26) ===");
    run("INTER", 832, 480, 12, 12); // 1 IDR + 11 P
    run("ALL-INTRA", 832, 480, 12, 1); // every frame IDR
}
