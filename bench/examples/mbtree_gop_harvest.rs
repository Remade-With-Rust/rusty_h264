//! **The Front-B B4/B5 harvest seam: a PER-GOP objective for the mb-tree gate.**
//!
//! The gate decides per GOP, but `bdrate.rs` only produces per-CLIP BD. Fitting
//! a per-GOP rule on per-clip labels is a unit mismatch, and it is not
//! hypothetical: football's three GOPs measure sd 0.747 / 0.480 / 0.402 and
//! straddle the 0.48 threshold, so "does football fire?" has no single answer.
//! A law fitted that way scores well and means nothing.
//!
//! This pairs, for every GOP: the gate's own inputs (`gopstats`) with the RD
//! outcome of mb-tree ON vs OFF on that same GOP.
//!
//! GOPs are closed units here (IDR at each start, `gop_size` frames each), so
//! per-GOP bits and distortion are directly comparable between the two arms.
//! Objective is the encoder's own currency, `J = SSE + lambda*bits`, with
//! lambda from the slice QP — negative dJ means mb-tree HELPED that GOP.
//!
//! Emits `gate_optimizer`-ready CSV: one row per GOP.
//!
//!   cargo run --release --example mbtree_gop_harvest -- <out.csv> [clip.yuv WxH]...

use rusty_h264::{gopstats, Encoder, EncoderConfig, Preset, YuvFrame};

const GOP: u32 = 20;
const FRAMES: usize = 80;

fn load(path: &str, w: usize, h: usize, max: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    (0..(raw.len() / fsz).min(max))
        .map(|i| {
            let b = &raw[i * fsz..];
            YuvFrame { width: w, height: h, y: b[..ys].to_vec(),
                       u: b[ys..ys + cs].to_vec(), v: b[ys + cs..ys + 2 * cs].to_vec() }
        })
        .collect()
}

fn sse(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| { let d = x as f64 - y as f64; d * d }).sum()
}

/// Encode one arm; return per-GOP (bits, luma SSE) plus the gate rows.
fn run(frames: &[YuvFrame], w: usize, h: usize, qp: u8, mbtree: bool)
    -> (Vec<(f64, f64)>, Vec<gopstats::GopRow>)
{
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = GOP;
    cfg.preset = Preset::Quality;
    cfg.mbtree = mbtree;
    // The ON arm must be UNGATED. With the shipped latch active every GOP below
    // the threshold is zeroed, so both arms are byte-identical and every gain
    // reads 0.0000 — which is what the first run produced. The law needs the
    // COUNTERFACTUAL: what mb-tree would deliver on this GOP if it fired.
    cfg.mbtree_spread_min = 0.0;
    let _ = gopstats::take(); // clear
    let aus = Encoder::new(cfg).expect("cfg").encode_all(frames).expect("encode");
    let rows = gopstats::take();

    // Per-GOP bits: access units come out in coding order, gop_size per GOP.
    let mut per_gop: Vec<(f64, f64)> = Vec::new();
    for chunk in aus.chunks(GOP as usize) {
        per_gop.push((chunk.iter().map(|a| a.len() as f64 * 8.0).sum(), 0.0));
    }
    // Per-GOP distortion: decode once and compare to source.
    let stream: Vec<u8> = aus.concat();
    // `decode_stream` returns DISPLAY order, which matches `frames` here (no
    // B-frames on this path), so index i lines up with source frame i.
    let dec = rusty_h264::Decoder::new().decode_stream(&stream).unwrap_or_default();
    for (i, f) in dec.iter().enumerate() {
        if i >= frames.len() { break }
        let g = i / GOP as usize;
        if g < per_gop.len() { per_gop[g].1 += sse(&f.y, &frames[i].y); }
    }
    (per_gop, rows)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out_path = args.first().cloned().unwrap_or_else(|| "mbtree_gops.csv".into());
    let clips: Vec<(String, usize, usize)> = args[1..]
        .chunks(2)
        .filter_map(|c| {
            let (w, h) = c.get(1)?.split_once('x')?;
            Some((c[0].clone(), w.parse().ok()?, h.parse().ok()?))
        })
        .collect();
    assert!(!clips.is_empty(), "usage: mbtree_gop_harvest <out.csv> <clip.yuv> <WxH> ...");
    assert!(gopstats::on(), "set RFF_MBTREE_GOPSTATS=1 — the gate telemetry is off");
    // GOPs encode IN PARALLEL by default and the telemetry rows would then
    // arrive in completion order, mismatching signals to per-GOP objectives.
    std::env::set_var("RUSTY_THREADS", "1");

    let mut csv = String::from(
        "unit,class,split,net_gain,sd,sd_raw,residual_frac,bits_off,dsse_pct\n");
    let mut n = 0usize;
    for (path, w, h) in &clips {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let frames = load(path, *w, *h, FRAMES);
        let qp = 28u8;
        // lambda in the encoder's own currency for this QP.
        let lambda = 0.85 * 2f64.powf((qp as f64 - 12.0) / 3.0);
        let (on, rows_on) = run(&frames, *w, *h, qp, true);
        let (off, _) = run(&frames, *w, *h, qp, false);
        assert_eq!(
            on.len(), rows_on.len(),
            "{name}: {} AU-chunks vs {} gate rows — positional pairing would              mismatch signals to outcomes. Do not 'fix' by truncating.",
            on.len(), rows_on.len()
        );
        for (g, ((b_on, d_on), (b_off, d_off))) in on.iter().zip(&off).enumerate() {
            let Some(r) = rows_on.get(g) else { continue };
            let j_on = d_on + lambda * b_on;
            let j_off = d_off + lambda * b_off;
            if j_off <= 0.0 { continue }
            // +ve gain = mb-tree HELPED this GOP (lower J).
            let gain = 100.0 * (j_off - j_on) / j_off;
            let dsse = if *d_off > 0.0 { 100.0 * (d_off - d_on) / d_off } else { 0.0 };
            // Split BY CLIP so a rule cannot memorise a clip across its GOPs.
            let split = if n % 2 == 0 { "train" } else { "holdout" };
            csv.push_str(&format!(
                "{name}_g{g},{name},{split},{gain:.4},{:.4},{:.4},{:.4},{b_off:.0},{dsse:.3}\n",
                r.sd, r.sd_raw, r.residual_frac));
        }
        eprintln!("  {name}: {} GOPs", on.len().min(rows_on.len()));
        n += 1;
    }
    std::fs::write(&out_path, &csv).expect("write csv");
    eprintln!("wrote {out_path} ({} rows)", csv.lines().count() - 1);
}
