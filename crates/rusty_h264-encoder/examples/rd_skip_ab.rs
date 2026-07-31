//! RD-SKIP A/B — does the RD skip decision reach the SHIPPED configuration, and
//! what is it worth on a real 4-QP per-clip BD table?
//!
//! WHY THIS EXISTS: `tune_rd_skip` is implemented in `encode_slice_data` only —
//! the CAVLC P encoder. `encode_slice_data_cabac_p` contains no reference to it,
//! and CABAC is the LIBRARY DEFAULT. So the recorded "RD P_Skip built + opt-in,
//! -10% BD-SSIM on Fast" was measured on an entropy coder we do not ship by
//! default. This harness settles that empirically instead of by grep, and prices
//! the lever where it actually matters.
//!
//! Every arm is a 4-QP BD-rate against the arm-0 anchor, PSNR *and* SSIM, per
//! clip — a single-QP size comparison of a decision-layer change is exactly the
//! mirage the dispatch skill warns about. Negative = the arm is BETTER.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example rd_skip_ab //!     -- video-tests/clips/mobile_cif.y4m [more clips...]
//!
//! Env: RS_FRAMES (default 24), RS_QPS (default 22,27,32,37).

use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn read_y4m(path: &str, max_frames: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let hdr_end = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..hdr_end]).unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().unwrap(),
            Some(b'H') => h = tok[1..].parse().unwrap(),
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
        frames.push(YuvFrame {
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

/// Mean SSIM over 8×8 luma windows (Wang et al. constants at L=255).
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
            acc += ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            cnt += 1;
            bx += 8;
        }
        by += 8;
    }
    acc / cnt.max(1) as f64
}

fn ssim_db(s: f64) -> f64 {
    -10.0 * (1.0 - s).max(1e-9).log10()
}

fn polyfit3(x: &[f64], y: &[f64]) -> [f64; 4] {
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
    [b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]]
}

/// Bjontegaard-Delta rate (%) of `test` vs `anchor`, each `(rate, distortion)`.
fn bd_rate(anchor: &[(f64, f64)], test: &[(f64, f64)]) -> f64 {
    let prep = |p: &[(f64, f64)]| -> (Vec<f64>, Vec<f64>) {
        let mut v: Vec<(f64, f64)> = p.iter().map(|&(r, d)| (d, r.log10())).collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        (v.iter().map(|q| q.0).collect(), v.iter().map(|q| q.1).collect())
    };
    let (da, la) = prep(anchor);
    let (dt, lt) = prep(test);
    let (ca, ct) = (polyfit3(&da, &la), polyfit3(&dt, &lt));
    let lo = da[0].max(dt[0]);
    let hi = da[da.len() - 1].min(dt[dt.len() - 1]);
    if hi <= lo {
        return f64::NAN; // non-overlapping curves — report, never silently fit
    }
    let integ = |c: &[f64; 4], x: f64| c[0] * x + c[1] * x * x / 2.0 + c[2] * x.powi(3) / 3.0 + c[3] * x.powi(4) / 4.0;
    let avg = ((integ(&ct, hi) - integ(&ct, lo)) - (integ(&ca, hi) - integ(&ca, lo))) / (hi - lo);
    (10f64.powf(avg) - 1.0) * 100.0
}


/// One clip × the arm set → per-arm (psnr curve, ssim curve, total ms).

#[derive(Clone, Copy)]
struct Arm {
    name: &'static str,
    cabac: bool,
    rd_skip: bool,
    /// `None` = leave the calibrated per-preset default; `Some(0)` forces RD skip
    /// on everywhere (the ungated arm — it LOSES on detailed content, which is
    /// exactly what the per-clip table has to show rather than hide).
    min_free: Option<u32>,
    /// RD B_Skip strength (RFF_BSKIP_T). 0 = off (byte-identical).
    bskip_t: f64,
    /// RD lambda scale (mode/quant decisions). 1.0 = shipped.
    lam: f64,
    /// ME lambda scale (motion search rate term). 1.0 = shipped.
    lme: f64,
}

const ARMS: &[Arm] = &[
    Arm { name: "shipped",  cabac: true, rd_skip: false, min_free: None, bskip_t: 48.0, lam: 1.0, lme: 1.0 },
    Arm { name: "lme 1.40", cabac: true, rd_skip: false, min_free: None, bskip_t: 48.0, lam: 1.0, lme: 1.40 },
    Arm { name: "lme 1.80", cabac: true, rd_skip: false, min_free: None, bskip_t: 48.0, lam: 1.0, lme: 1.80 },
    Arm { name: "lme 2.20", cabac: true, rd_skip: false, min_free: None, bskip_t: 48.0, lam: 1.0, lme: 2.20 },
];

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let nframes: usize = std::env::var("RS_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let qps: Vec<u8> = std::env::var("RS_QPS").unwrap_or_else(|_| "22,27,32,37".into())
        .split(',').map(|s| s.parse().unwrap()).collect();
    println!("RD-SKIP A/B — 4-QP BD-rate vs arm 0, per clip (negative = better)
");

    for path in &paths {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let (w, h, frames) = read_y4m(path, nframes);
        let mut rows: Vec<(String, Vec<(f64, f64)>, Vec<(f64, f64)>, usize)> = Vec::new();
        for arm in ARMS {
            let (mut pc, mut sc) = (Vec::new(), Vec::new());
            let mut bytes_mid = 0usize;
            for &qp in &qps {
                let mut cfg = EncoderConfig::new(w, h);
                cfg.qp = qp;
                cfg.gop_size = 30;
                cfg.preset = Preset::Quality;
                cfg.cabac = arm.cabac;
                cfg.profile = rusty_h264_common::Profile::Main;
                cfg.bframes = 2;
                cfg.bframes_adaptive = false;
                cfg.num_ref_frames = 3;
                cfg.tune_rd_skip = arm.rd_skip;
                if let Some(m) = arm.min_free {
                    cfg.tune_rd_skip_min_free = Some(m);
                }
                // Set the CONFIG explicitly, never by clearing the env: once
                // `tune_bskip_rd` gained a non-None DEFAULT, an "off" arm that only
                // removed RFF_BSKIP_T fell through to that default and silently
                // measured T=48 against T=48. Always pin the arm's value.
                std::env::remove_var("RFF_BSKIP_T");
                cfg.tune_bskip_rd = if arm.bskip_t > 0.0 { Some(arm.bskip_t) } else { None };
                cfg.tune_lambda_scale = arm.lam;
                cfg.cabac_lambda_scale = arm.lme;
                let enc = Encoder::new(cfg).expect("cfg");
                let aus = enc.encode_all(&frames).expect("encode");
                let bytes: usize = aus.iter().map(Vec::len).sum();
                if qp == qps[qps.len() / 2] { bytes_mid = bytes; }
                // B-FRAME ORDERING TRAP: `Decoder::decode` yields pictures in DECODE
                // order, while `frames` is DISPLAY order. Pairing them positionally
                // silently scores every B-frame against the wrong source picture --
                // it produced impossible BD-SSIM values (4.9e9%) that gave the bug
                // away. `decode_stream` returns DISPLAY order, so index i is source i.
                let stream: Vec<u8> = aus.concat();
                let recon = rusty_h264_decoder::Decoder::new()
                    .decode_stream(&stream)
                    .expect("decode");
                assert_eq!(recon.len(), frames.len(), "frame count must match to score");
                let (mut se, mut n, mut sacc, mut sn) = (0f64, 0u64, 0f64, 0u64);
                for (src, r) in frames.iter().zip(&recon) {
                    for (a, b) in src.y.iter().zip(&r.y) {
                        let d = *a as f64 - *b as f64;
                        se += d * d;
                        n += 1;
                    }
                    sacc += ssim_y(&src.y, &r.y, w, h);
                    sn += 1;
                }
                let psnr = 10.0 * (255.0f64 * 255.0 / (se / n as f64)).log10();
                pc.push((bytes as f64, psnr));
                sc.push((bytes as f64, ssim_db(sacc / sn.max(1) as f64)));
            }
            rows.push((arm.name.to_string(), pc, sc, bytes_mid));
        }
        println!("=== {name} {w}x{h} x{} QPs {qps:?} ===", frames.len());
        println!("{:<22}{:>11}{:>12}{:>12}   {}", "arm", "bytes@mid", "BD-PSNR%", "BD-SSIM%", "vs anchor");
        let (abytes, apc, asc) = (rows[0].3, rows[0].1.clone(), rows[0].2.clone());
        for (n2, pc, sc, bm) in &rows {
            let (bp, bs) = (bd_rate(&apc, pc), bd_rate(&asc, sc));
            // IDENTICAL bytes at every QP is the signature of an INERT knob --
            // report it as such rather than printing a meaningless 0.00%.
            let inert = pc.iter().zip(&apc).all(|(a, b)| a.0 == b.0);
            let note = if inert && !std::ptr::eq(n2.as_str(), rows[0].0.as_str()) {
                "IDENTICAL — knob has NO EFFECT".to_string()
            } else {
                format!("{:+.1}% bytes", (*bm as f64 - abytes as f64) * 100.0 / abytes as f64)
            };
            println!("{n2:<22}{bm:>11}{bp:>11.2}%{bs:>11.2}%   {note}");
        }
        println!();
    }
}
