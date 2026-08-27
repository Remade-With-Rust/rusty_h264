//! ME cost anatomy — the deterministic instrument for the `best_part` campaign.
//!
//! `best_part` CALL counts (from `gate_work()`) cannot tell "one search that
//! walked 40 candidates" from "one that walked 4", and walk length is the whole
//! hypothesis: the sub-8x8 split search multiplies searches ~9x inside P_8x8,
//! and each of those searches walks a coarse-to-fine ladder sized for a 16x16
//! block that has no predictor — while a sub-partition is seeded from its
//! parent's already-converged MV.
//!
//! Prints, per diamond rung: evaluations, improvements, and the HIT RATE. A rung
//! whose hit rate is ~0 is pure toll. Deterministic — one run is the verdict, no
//! pinning, no z-score (codec-measurement §15).
//!
//! Needs the `profile` feature (the per-rung counters live behind it):
//!   cargo run --release --features profile --example mecost -- <clip.yuv> WxH

use rusty_h264::{Encoder, EncoderConfig, Preset, YuvFrame};

fn load(path: &str, w: usize, h: usize, max: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    (0..(raw.len() / fsz).min(max))
        .map(|i| {
            let b = &raw[i * fsz..];
            YuvFrame {
                width: w,
                height: h,
                y: b[..ys].to_vec(),
                u: b[ys..ys + cs].to_vec(),
                v: b[ys + cs..ys + 2 * cs].to_vec(),
            }
        })
        .collect()
}

fn run(label: &str, frames: &[YuvFrame], w: usize, h: usize, sub8: bool) {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = 27;
    cfg.gop_size = 30;
    cfg.preset = Preset::Quality;
    cfg.tune_sub8x8_split = sub8;
    cfg.tune_sub8_rd = sub8;

    rusty_h264::diastats_reset();
    rusty_h264::gate_census_reset();
    let enc = Encoder::new(cfg).expect("cfg");
    let aus = enc.encode_all(frames).expect("encode");
    let bytes: usize = aus.iter().map(Vec::len).sum();

    let dia = rusty_h264::diastats_snapshot();
    let work = rusty_h264::gate_work();
    let names = rusty_h264::gate_work_names();
    let total: u64 = dia.iter().map(|(e, _)| e).sum();

    println!("\n=== {label} === {bytes} bytes");
    if total == 0 {
        println!("  (no rung data — build with --features profile)");
    }
    println!("  {:<6} {:>12} {:>8} {:>10} {:>8}", "rung", "evals", "share", "improved", "hit%");
    // DIA_RUNGS in quarter-pel; the ladder default is [16,8,4] = 4px/2px/1px.
    for (i, (e, imp)) in dia.iter().enumerate() {
        if *e == 0 {
            continue;
        }
        println!(
            "  {:<6} {:>12} {:>7.1}% {:>10} {:>7.2}%",
            format!("s{i}"),
            e,
            100.0 * *e as f64 / total.max(1) as f64,
            imp,
            100.0 * *imp as f64 / *e as f64
        );
    }
    println!("  {:<6} {:>12}", "TOTAL", total);
    for (n, v) in names.iter().zip(&work) {
        if *v > 0 {
            println!("  work.{n:<14} {v:>12}");
        }
    }
    if let (Some(c), Some(ev)) = (work.first(), Some(total)) {
        if *c > 0 {
            println!("  evals per best_part call: {:.1}", ev as f64 / *c as f64);
        }
    }
}

fn main() {
    // The gate census is default-off on the shipping path (E15 W10); this
    // harness is its consumer, so it turns the instrument on for itself.
    std::env::set_var("RFF_GATE_CENSUS", "1");
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (path, wh) = (a[0].clone(), a[1].clone());
    let (w, h) = wh.split_once('x').expect("WxH");
    let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
    let frames = load(&path, w, h, 30);
    // Both arms, so the split search's contribution is a DIFFERENCE, not a guess.
    run("sub8 OFF (pre-P3.3 search)", &frames, w, h, false);
    run("sub8 ON  (shipped default)", &frames, w, h, true);
}
