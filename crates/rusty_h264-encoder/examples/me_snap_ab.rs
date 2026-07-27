//! Interleaved A/B of the integer-pel diamond-centre snap (`tune_me_snap`).
//!
//! Both arms in ONE process, alternating pass by pass, best-of-N — this box drifts
//! ~20% run to run, far more than the effect.
//!
//! ```text
//! cargo run --release -p rusty_h264-encoder --example me_snap_ab -- <clip.yuv> <w>x<h>
//! ```
use rusty_h264_common::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn load(path: &str, w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).expect("clip");
    let fsz = w * h * 3 / 2;
    raw.chunks_exact(fsz).take(n).map(|c| {
        let mut fr = YuvFrame::black(w, h);
        fr.y.copy_from_slice(&c[..w * h]);
        fr.u.copy_from_slice(&c[w * h..w * h + w * h / 4]);
        fr.v.copy_from_slice(&c[w * h + w * h / 4..]);
        fr
    }).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (w, h) = a[1].split_once('x').unwrap();
    let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
    let frames = load(&a[0], w, h, std::env::var("RS_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(60));
    let preset = match std::env::var("RS_PRESET").unwrap_or_default().as_str() {
        "fast" => Preset::Fast, "quality" => Preset::Quality, _ => Preset::Balanced,
    };
    let arm_off: u32 = std::env::var("RS_ARM_OFF").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let arm_on: u32 = std::env::var("RS_ARM_ON").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let run = |on: bool| {
        let m = if on { arm_on } else { arm_off };
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = std::env::var("RS_QP").ok().and_then(|v| v.parse().ok()).unwrap_or(27);
        cfg.gop_size = 30;
        cfg.preset = preset;
        cfg.tune_me_snap = m & 1 != 0;
        cfg.tune_me_subpel_iter = m & 2 != 0;
        let mut enc = Encoder::new(cfg).expect("enc");
        let t = std::time::Instant::now();
        let mut b = 0usize;
        for f in &frames { b += enc.encode(f).len(); }
        (t.elapsed().as_secs_f64(), b)
    };
    let mut best = [f64::MAX; 2];
    let mut bytes = [0usize; 2];
    for pass in 0..10 {
        let arm = pass % 2;
        let (t, b) = run(arm == 1);
        if t < best[arm] { best[arm] = t; }
        bytes[arm] = b;
    }
    let px = (w * h * frames.len()) as f64;
    println!("arm {arm_off} -> {arm_on} — {} {w}x{h} {} frames {preset:?}", a[0], frames.len());
    println!("  off : {:>7.1} ms  {:>6.2} Mpx/s  {:>9} bytes", best[0]*1e3, px/best[0]/1e6, bytes[0]);
    println!("  on  : {:>7.1} ms  {:>6.2} Mpx/s  {:>9} bytes", best[1]*1e3, px/best[1]/1e6, bytes[1]);
    println!("  speed {:>6.3}x   size {:>+6.2}%", best[0]/best[1],
             100.0*(bytes[1] as f64/bytes[0] as f64 - 1.0));
}
