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

/// 8x8-block mean SSIM on luma — the SAME estimator `bdrate.rs` grades with.
///
/// mb-tree is a PERCEPTUAL tool: it moves bits by reference importance and
/// deliberately trades SSE for perceived quality. An SSE objective cannot see
/// what it buys — measured: `bus_cif` scored J +0.703 while its real BD-SSIM is
/// -0.80, a sign flip. Same wrong-currency family as SATD-vs-recon-SSE.
fn ssim_y(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let (mut acc, mut cnt) = (0f64, 0u64);
    let mut by = 0;
    while by + 8 <= h {
        let mut bx = 0;
        while bx + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
            for y in 0..8 {
                for x in 0..8 {
                    let (pa, pb) = (a[(by + y) * w + bx + x] as f64, b[(by + y) * w + bx + x] as f64);
                    sa += pa; sb += pb; saa += pa * pa; sbb += pb * pb; sab += pa * pb;
                }
            }
            let (ma, mb) = (sa / 64.0, sb / 64.0);
            let va = saa / 64.0 - ma * ma;
            let vb = sbb / 64.0 - mb * mb;
            let cov = sab / 64.0 - ma * mb;
            acc += ((2.0*ma*mb + C1) * (2.0*cov + C2)) / ((ma*ma + mb*mb + C1) * (va + vb + C2));
            cnt += 1;
            bx += 8;
        }
        by += 8;
    }
    acc / cnt.max(1) as f64
}

/// SSIM in dB, the domain BD-SSIM integrates over.
fn ssim_db(s: f64) -> f64 { -10.0 * (1.0 - s.clamp(0.0, 0.999_999)).log10() }

/// Encode one arm; return per-GOP (bits, luma SSE) plus the gate rows.
fn run(frames: &[YuvFrame], w: usize, h: usize, qp: u8, mbtree: bool)
    -> (Vec<(f64, f64, f64, u32)>, Vec<gopstats::GopRow>)
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
    let mut per_gop: Vec<(f64, f64, f64, u32)> = Vec::new(); // bits, sse, ssim_sum, n
    for chunk in aus.chunks(GOP as usize) {
        per_gop.push((chunk.iter().map(|a| a.len() as f64 * 8.0).sum(), 0.0, 0.0, 0));
    }
    // Per-GOP distortion: decode once and compare to source.
    let stream: Vec<u8> = aus.concat();
    // `decode_stream` returns DISPLAY order, which matches `frames` here (no
    // B-frames on this path), so index i lines up with source frame i.
    let dec = rusty_h264::Decoder::new().decode_stream(&stream).unwrap_or_default();
    for (i, f) in dec.iter().enumerate() {
        if i >= frames.len() { break }
        let g = i / GOP as usize;
        if g < per_gop.len() {
            per_gop[g].1 += sse(&f.y, &frames[i].y);
            per_gop[g].2 += ssim_y(&f.y, &frames[i].y, w, h);
            per_gop[g].3 += 1;
        }
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
        "unit,class,split,net_gain_bdssim,sd,sd_raw,residual_frac,bits_off,dbits_pct
");
    let mut rate_dev: Vec<f64> = Vec::new();
    let mut n = 0usize;
    for (path, w, h) in &clips {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let frames = load(path, *w, *h, FRAMES);
        // TWO-POINT LADDER. A single-QP objective CANNOT serve a BD-rate gate:
        // BD measures RATE AT EQUAL QUALITY, so spending 7% more bits for
        // +0.19 dB reads as a win at one QP and is a loss to BD. Measured —
        // single-QP dSSIM disagreed with clip BD-SSIM on 3 of 14 clips
        // (park_joy +0.19 dB vs -14.9 BD). Two points give the rate-quality
        // SLOPE, which is what the gate is actually judged on.
        let (qp_lo, qp_hi) = (24u8, 32u8);
        let (on_lo, rows_on) = run(&frames, *w, *h, qp_lo, true);
        let (off_lo, _) = run(&frames, *w, *h, qp_lo, false);
        let (on_hi, _) = run(&frames, *w, *h, qp_hi, true);
        let (off_hi, _) = run(&frames, *w, *h, qp_hi, false);
        let (on, off) = (&on_lo, &off_lo);
        assert_eq!(on_lo.len(), on_hi.len(), "{name}: ladder arms disagree on GOP count");
        assert_eq!(
            on.len(), rows_on.len(),
            "{name}: {} AU-chunks vs {} gate rows — positional pairing would              mismatch signals to outcomes. Do not 'fix' by truncating.",
            on.len(), rows_on.len()
        );
        for (g, ((b_on, _s_on, q_on, n_on), (b_off, _s_off, q_off, n_off))) in
            on.iter().zip(off.iter()).enumerate()
        {
            let Some(r) = rows_on.get(g) else { continue };
            if *n_on == 0 || *n_off == 0 || *b_off <= 0.0 { continue }
            let (Some((bh_on, _, qh_on, nh_on)), Some((bh_off, _, qh_off, nh_off))) =
                (on_hi.get(g), off_hi.get(g)) else { continue };
            if *nh_on == 0 || *nh_off == 0 || *bh_off <= 0.0 { continue }
            // Two (log-rate, SSIM-dB) points per arm; BD-rate is the mean
            // horizontal gap between the curves over their shared quality
            // range — the same quantity `bdrate.rs` integrates with four.
            let pt = |b: f64, q: f64, n: u32| (b.max(1.0).ln(), ssim_db(q / n as f64));
            let (a1, a2) = (pt(*b_on, *q_on, *n_on), pt(*bh_on, *qh_on, *nh_on));
            let (o1, o2) = (pt(*b_off, *q_off, *n_off), pt(*bh_off, *qh_off, *nh_off));
            // log-rate as a linear function of quality, per arm.
            let slope = |p: (f64, f64), q: (f64, f64)| {
                if (p.1 - q.1).abs() < 1e-9 { None } else { Some((p.0 - q.0) / (p.1 - q.1)) }
            };
            let (Some(sa), Some(so)) = (slope(a1, a2), slope(o1, o2)) else { continue };
            let lo = a1.1.min(a2.1).max(o1.1.min(o2.1));
            let hi = a1.1.max(a2.1).min(o1.1.max(o2.1));
            if hi <= lo { continue }
            let mid = 0.5 * (lo + hi);
            let ra = a1.0 + sa * (mid - a1.1);
            let ro = o1.0 + so * (mid - o1.1);
            // +ve gain = mb-tree needs FEWER bits at equal quality = a BD win.
            let gain = 100.0 * (1.0 - (ra - ro).exp());
            let dbits = 100.0 * (b_on - b_off) / b_off;
            rate_dev.push(dbits.abs());
            // Split BY CLIP so a rule cannot memorise a clip across its GOPs.
            let split = if n % 2 == 0 { "train" } else { "holdout" };
            csv.push_str(&format!(
                "{name}_g{g},{name},{split},{gain:.4},{:.4},{:.4},{:.4},{b_off:.0},{dbits:.3}\n",
                r.sd, r.sd_raw, r.residual_frac));
        }
        eprintln!("  {name}: {} GOPs", on.len().min(rows_on.len()));
        n += 1;
    }
    // RATE-NEUTRALITY CHECK. dSSIM alone is only a fair objective if mb-tree
    // did not simply spend more bits. Per-GOP centring should hold this near 0.
    rate_dev.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = rate_dev.get(rate_dev.len() / 2).copied().unwrap_or(0.0);
    let worst = rate_dev.last().copied().unwrap_or(0.0);
    eprintln!("  rate neutrality: |dbits| median {med:.2}%, worst {worst:.2}%");
    if worst > 5.0 {
        eprintln!("  ⚠ NOT rate-neutral — dSSIM alone overstates mb-tree where it spent bits.");
    }
    std::fs::write(&out_path, &csv).expect("write csv");
    eprintln!("wrote {out_path} ({} rows)", csv.lines().count() - 1);
}
