//! Is our motion search FINDING the best vector available to it?
//!
//! `enc-me` is 62% of encode and runs at x264-medium's cost per macroblock while
//! delivering worse compression than x264-veryfast. That is either a SEARCH
//! failure (we don't find the good vectors) or an efficiency failure elsewhere
//! (we find them and lose the benefit downstream). This separates the two by
//! pricing our chosen vector against an exhaustive +-24 full-pel search using the
//! IDENTICAL cost function and sub-pel pass.
//!
//! ```text
//! RFF_ME_ORACLE=1 cargo run --release -p rusty_h264-encoder --example me_oracle -- <clip.yuv> <w>x<h>
//! ```
use rusty_h264_common::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn load(path: &str, w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).expect("clip");
    let fsz = w * h * 3 / 2;
    raw.chunks_exact(fsz)
        .take(n)
        .map(|c| {
            let mut fr = YuvFrame::black(w, h);
            fr.y.copy_from_slice(&c[..w * h]);
            fr.u.copy_from_slice(&c[w * h..w * h + w * h / 4]);
            fr.v.copy_from_slice(&c[w * h + w * h / 4..]);
            fr
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (w, h) = a[1].split_once('x').unwrap();
    let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
    let nf: usize = std::env::var("RS_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let frames = load(&a[0], w, h, nf);

    let preset = match std::env::var("RS_PRESET").unwrap_or_default().as_str() {
        "fast" => Preset::Fast,
        "quality" => Preset::Quality,
        _ => Preset::Balanced,
    };
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = std::env::var("RS_QP").ok().and_then(|v| v.parse().ok()).unwrap_or(27);
    cfg.gop_size = 30;
    cfg.preset = preset;
    let mut enc = Encoder::new(cfg).expect("encoder");
    for f in &frames {
        let _ = enc.encode(f);
    }

    let p: Vec<u64> = rusty_h264_encoder::ME_PROBE
        .iter()
        .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        .collect();
    let (n, ours, oracle, worse, evals) = (p[0].max(1), p[1], p[2], p[3], p[4]);
    let (oracle_sp, worse_sp) = (p[5], p[6]);
    println!("ME oracle — {} ({w}x{h}, {} frames, {preset:?})\n", a[0], frames.len());
    println!("  searches                {n}");
    println!("  mean cost   ours        {:>10.1}", ours as f64 / n as f64);
    println!("  mean cost   exhaustive  {:>10.1}", oracle as f64 / n as f64);
    println!("  ---> we are {:>6.2}% above the achievable minimum",
             100.0 * (ours as f64 - oracle as f64) / oracle as f64);
    println!("  searches the oracle beat {:>9} ({:.1}%)", worse, 100.0 * worse as f64 / n as f64);
    // the oracle's own 49x49 grid + 8 sub-pel probes are included in `evals`
    println!("
  + exhaustive SUB-PEL (all quarter-pel in +-3):");
    println!("  mean cost   exhaustive  {:>10.1}", oracle_sp as f64 / n as f64);
    println!("  ---> we are {:>6.2}% above the achievable minimum",
             100.0 * (ours as f64 - oracle_sp as f64) / oracle_sp as f64);
    println!("  searches it beat        {:>9} ({:.1}%)", worse_sp, 100.0 * worse_sp as f64 / n as f64);
    let oracle_evals = 0;
    println!("\n  cost() evals/search     {:>8.1}  (ours, oracle's {oracle_evals} excluded)",
             evals as f64 / n as f64 - oracle_evals as f64);
}
