//! BD-rate trial harness — the measurement spine for the decision-layer tuning
//! campaign. Encodes a REAL clip at several QPs for each candidate parameter
//! value, measures (rate = total bytes, distortion = avg Y-PSNR via our decoder),
//! fits the RD curve, and reports Bjøntegaard-Delta rate vs the anchor (the
//! default parameter). Negative BD-rate = fewer bits for equal quality = a WIN.
//!
//! Real content only (synthetic clips misdirect BD-rate). Run:
//!   RUSTY_BDRATE_YUV=bench/_map/clip240.yuv RUSTY_BDRATE_WH=832x480 \
//!     cargo run --release -p rusty_h264-bench --example bdrate --features ...
//! (built through the facade; pass --features to reach asm if desired.)

use rusty_h264::{Decoder, Encoder, EncoderConfig, Preset, YuvFrame};

fn load_clip(path: &str, w: usize, h: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    (0..raw.len() / fsz)
        .map(|i| {
            let b = &raw[i * fsz..];
            YuvFrame { width: w, height: h, y: b[..ys].to_vec(), u: b[ys..ys + cs].to_vec(), v: b[ys + cs..ys + 2 * cs].to_vec() }
        })
        .collect()
}

/// Mean SSIM over 8×8 non-overlapping luma windows (Wang et al. constants at L=255).
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
            let nrm = 64.0;
            let (ma, mb) = (sa / nrm, sb / nrm);
            let va = saa / nrm - ma * ma;
            let vb = sbb / nrm - mb * mb;
            let cov = sab / nrm - ma * mb;
            acc += ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            cnt += 1;
            bx += 8;
        }
        by += 8;
    }
    acc / cnt.max(1) as f64
}

/// (rate = total bytes, Y-PSNR dB, mean SSIM) for one config over the clip.
/// `param` names which knob `val` sets (the other stays at its default).
fn rd_point(frames: &[YuvFrame], w: usize, h: usize, qp: u8, gop: u32, param: &str, val: f64) -> (f64, f64, f64) {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = gop;
    cfg.preset = Preset::Fast;
    match param {
        "intra" => cfg.tune_intra_penalty = val,
        "satd" => cfg.tune_satd_q = val,
        // combo: `val` encodes lambda_scale*1000 + intra_penalty (intra in 0..999).
        "combo" => {
            cfg.tune_lambda_scale = (val / 1000.0).floor();
            cfg.tune_intra_penalty = val % 1000.0;
        }
        _ => cfg.tune_lambda_scale = val,
    }
    let enc = Encoder::new(cfg).expect("cfg");
    let aus = enc.encode_all(frames).expect("encode");
    let bytes: usize = aus.iter().map(Vec::len).sum();
    let mut dec = Decoder::new();
    let (mut se, mut n, mut ssim_acc, mut sframes) = (0f64, 0u64, 0f64, 0u64);
    for (au, src) in aus.iter().zip(frames) {
        if let Ok(Some(r)) = dec.decode(au) {
            for (a, b) in src.y.iter().zip(&r.y) {
                let d = *a as f64 - *b as f64;
                se += d * d;
                n += 1;
            }
            ssim_acc += ssim_y(&src.y, &r.y, w, h);
            sframes += 1;
        }
    }
    let mse = se / n as f64;
    let psnr = if mse <= 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
    (bytes as f64, psnr, ssim_acc / sframes.max(1) as f64)
}

/// SSIM → a dB-like scale so BD-rate integrates it like PSNR: −10·log10(1−SSIM).
fn ssim_db(s: f64) -> f64 {
    -10.0 * (1.0 - s).max(1e-9).log10()
}

/// Least-squares degree-3 polyfit of y=f(x) via normal equations (4x4 solve).
fn polyfit3(x: &[f64], y: &[f64]) -> [f64; 4] {
    // A[j][k] = Σ x^(j+k), b[j] = Σ y·x^j, for j,k in 0..4.
    let mut a = [[0f64; 4]; 4];
    let mut b = [0f64; 4];
    for i in 0..x.len() {
        let mut xp = [0f64; 7];
        xp[0] = 1.0;
        for p in 1..7 {
            xp[p] = xp[p - 1] * x[i];
        }
        for j in 0..4 {
            for k in 0..4 {
                a[j][k] += xp[j + k];
            }
            b[j] += y[i] * xp[j];
        }
    }
    // Gaussian elimination (partial pivot).
    for c in 0..4 {
        let mut piv = c;
        for r in c + 1..4 {
            if a[r][c].abs() > a[piv][c].abs() {
                piv = r;
            }
        }
        a.swap(c, piv);
        b.swap(c, piv);
        for r in 0..4 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..4 {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    // coeffs c0 + c1 x + c2 x^2 + c3 x^3
    [b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]]
}

/// Bjøntegaard-Delta rate (%) of `test` vs `anchor`. Each is (rate, psnr) points.
/// Fits log10(rate) = cubic(psnr), integrates over the overlapping PSNR range.
fn bd_rate(anchor: &[(f64, f64)], test: &[(f64, f64)]) -> f64 {
    let prep = |p: &[(f64, f64)]| -> (Vec<f64>, Vec<f64>) {
        let mut v: Vec<(f64, f64)> = p.iter().map(|&(r, d)| (d, r.log10())).collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        (v.iter().map(|q| q.0).collect(), v.iter().map(|q| q.1).collect())
    };
    let (da, la) = prep(anchor);
    let (dt, lt) = prep(test);
    let ca = polyfit3(&da, &la);
    let ct = polyfit3(&dt, &lt);
    let lo = da[0].max(dt[0]);
    let hi = da[da.len() - 1].min(dt[dt.len() - 1]);
    let integ = |c: &[f64; 4], x: f64| c[0] * x + c[1] * x * x / 2.0 + c[2] * x.powi(3) / 3.0 + c[3] * x.powi(4) / 4.0;
    let int_a = integ(&ca, hi) - integ(&ca, lo);
    let int_t = integ(&ct, hi) - integ(&ct, lo);
    let avg = (int_t - int_a) / (hi - lo);
    (10f64.powf(avg) - 1.0) * 100.0
}

fn main() {
    let path = std::env::var("RUSTY_BDRATE_YUV").unwrap_or_else(|_| "bench/_map/clip240.yuv".into());
    let wh = std::env::var("RUSTY_BDRATE_WH").unwrap_or_else(|_| "832x480".into());
    let (w, h): (usize, usize) = {
        let mut it = wh.split('x');
        (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
    };
    let gop: u32 = std::env::var("RUSTY_BDRATE_GOP").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let param = std::env::var("RUSTY_BDRATE_PARAM").unwrap_or_else(|_| "lambda".into());
    let anchor_val: f64 = if param == "intra" { 24.0 } else if param == "combo" { 1024.0 } else if param == "satd" { 0.0 } else { 1.0 };
    let frames = load_clip(&path, w, h);
    let qps: Vec<u8> = std::env::var("RUSTY_BDRATE_QPS")
        .unwrap_or_else(|_| "22,27,32,37".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let scales: Vec<f64> = std::env::var("RUSTY_BDRATE_SCALES")
        .unwrap_or_else(|_| "0.60,0.75,0.85,1.00,1.15,1.30,1.50".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();

    println!("BD-rate {param} sweep — {path} {w}x{h} x{} gop{gop}, QPs {qps:?}", frames.len());
    println!("(anchor = {param} {anchor_val}; negative BD-rate = fewer bits @ equal quality = WIN)\n");

    // Each RD point is (bytes, psnr, ssim). Build PSNR and SSIM curves.
    let sample = |s: f64| -> Vec<(f64, f64, f64)> { qps.iter().map(|&q| rd_point(&frames, w, h, q, gop, &param, s)).collect() };
    let psnr_curve = |c: &[(f64, f64, f64)]| -> Vec<(f64, f64)> { c.iter().map(|&(r, p, _)| (r, p)).collect() };
    let ssim_curve = |c: &[(f64, f64, f64)]| -> Vec<(f64, f64)> { c.iter().map(|&(r, _, s)| (r, ssim_db(s))).collect() };

    let anchor = sample(anchor_val);
    println!("  anchor ({anchor_val}): {}", anchor.iter().zip(&qps).map(|((r, p, s), q)| format!("qp{q}:{:.0}KiB/{p:.2}dB/{:.4}ssim", r / 1024.0, s)).collect::<Vec<_>>().join("  "));
    println!();
    println!("  {:<8} {:>11} {:>11}   (both must agree for a real win)", param, "BD-PSNR%", "BD-SSIM%");
    for &s in &scales {
        let curve = sample(s);
        let bd_p = bd_rate(&psnr_curve(&anchor), &psnr_curve(&curve));
        let bd_s = bd_rate(&ssim_curve(&anchor), &ssim_curve(&curve));
        let real = if s == anchor_val { " (anchor)" } else if bd_p < -0.1 && bd_s < -0.1 { " <-- WIN (both)" } else if bd_p < -0.1 && bd_s > 0.0 { " <-- PSNR-only (gaming)" } else { "" };
        println!("  {:<8.2} {:>+11.3} {:>+11.3}{}", s, bd_p, bd_s, real);
    }
}
