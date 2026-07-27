//! CROSS-ENCODER BD-rate: rusty_h264 vs x264, the honest side-by-side.
//!
//! Every previous attempt in this repo compared the two at a FIXED QP and then
//! matched presets by nearest PSNR. That is invalid: at one QP x264's ten presets
//! differ mostly in SIZE, so their PSNRs cluster inside ~0.2 dB and "nearest PSNR"
//! keeps selecting `placebo`, producing absurdities like "97× faster at equal
//! quality". Rate and distortion have to be swept together.
//!
//! So: both encoders over the SAME QP ladder, both bitstreams decoded by OUR
//! decoder (conformant, and it removes any timestamp-alignment trap), PSNR compared
//! frame-by-INDEX against the source, then Bjontegaard-Delta rate.
//!
//! Reported per (our preset × x264 preset):
//!   BD-rate  — % more bits WE spend for the same quality (positive = we are worse)
//!   speed    — our Mpx/s ÷ theirs, at a mid-ladder QP
//!
//! A pair where BD-rate is small AND speed > 1 is a point we win outright.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example x264_bdrate \
//!     -- video-tests/clips/foreman_cif.y4m

use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};
use std::path::Path;
use std::process::Command;

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

fn psnr_vs_source(stream: &[u8], src: &[YuvFrame], w: usize, h: usize) -> Option<f64> {
    let dec = rusty_h264_decoder::Decoder::new().decode_stream(stream).ok()?;
    // Frame-count guard: a short decode would otherwise score only the frames it
    // produced while its bitrate divides by the full duration.
    if dec.len() != src.len() {
        eprintln!("  ! frame count {} != {} — point dropped", dec.len(), src.len());
        return None;
    }
    let (mut se, mut n) = (0f64, 0u64);
    for (r, s) in dec.iter().zip(src) {
        for (a, b) in s.y.iter().zip(&r.y) {
            let d = *a as f64 - *b as f64;
            se += d * d;
            n += 1;
        }
    }
    let _ = (w, h);
    Some(10.0 * (255.0 * 255.0 / (se / n as f64)).log10())
}

fn polyfit3(x: &[f64], y: &[f64]) -> [f64; 4] {
    let (mut a, mut b) = ([[0f64; 4]; 4], [0f64; 4]);
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

/// BD-rate of `test` vs `anchor`, each `(bytes, psnr)`. Positive = test spends more.
fn bd_rate(anchor: &[(f64, f64)], test: &[(f64, f64)]) -> (f64, f64) {
    let prep = |p: &[(f64, f64)]| {
        let mut v: Vec<(f64, f64)> = p.iter().map(|&(r, d)| (d, r.log10())).collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        (v.iter().map(|q| q.0).collect::<Vec<_>>(), v.iter().map(|q| q.1).collect::<Vec<_>>())
    };
    let (da, la) = prep(anchor);
    let (dt, lt) = prep(test);
    let lo = da[0].max(dt[0]);
    let hi = da[da.len() - 1].min(dt[dt.len() - 1]);
    // Report the overlap width: a BD over a thin overlap is not trustworthy.
    if hi <= lo {
        return (f64::NAN, 0.0);
    }
    let (ca, ct) = (polyfit3(&da, &la), polyfit3(&dt, &lt));
    let integ = |c: &[f64; 4], x: f64| c[0] * x + c[1] * x * x / 2.0 + c[2] * x.powi(3) / 3.0 + c[3] * x.powi(4) / 4.0;
    let avg = ((integ(&ct, hi) - integ(&ct, lo)) - (integ(&ca, hi) - integ(&ca, lo))) / (hi - lo);
    ((10f64.powf(avg) - 1.0) * 100.0, hi - lo)
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "video-tests/clips/foreman_cif.y4m".into());
    let nframes: usize = std::env::var("XB_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    let qps: Vec<u8> = std::env::var("XB_QPS").unwrap_or_else(|_| "22,27,32,37".into())
        .split(',').map(|s| s.parse().unwrap()).collect();
    let x264 = std::env::var("X264_BIN").unwrap_or_else(|_| "../_ref_x264/x264.exe".into());
    let x_presets: Vec<String> = std::env::var("XB_PRESETS")
        .unwrap_or_else(|_| "superfast,veryfast,faster,fast,medium,slow".into())
        .split(',').map(String::from).collect();
    let (w, h, frames) = read_y4m(&path, nframes);
    let name = Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    let tmp = std::env::temp_dir();
    println!("cross-encoder BD-rate — {name} {w}x{h} x{} QPs {qps:?}", frames.len());
    println!("both encoders decoded by OUR decoder, PSNR frame-by-index vs source\n");

    // --- our curves ---
    let mut ours: Vec<(&str, Vec<(f64, f64)>, f64)> = Vec::new();
    for (pname, preset) in [("fast", Preset::Fast), ("balanced", Preset::Balanced), ("quality", Preset::Quality)] {
        let mut curve = Vec::new();
        let mut mid_mpx = 0.0;
        for &qp in &qps {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 60;
            cfg.preset = preset;
            // XB_CABAC=1: turn on the tools we implement but leave OFF by default
            // (CABAC, 8x8 transform), and move x264 to --profile main/high so the
            // toolsets still match. This measures CAPABILITY, not the shipped default.
            if std::env::var_os("XB_CABAC").is_some() {
                cfg.cabac = true;
            }
            let enc = Encoder::new(cfg).expect("cfg");
            let t = std::time::Instant::now();
            let aus = enc.encode_all(&frames).expect("encode");
            let secs = t.elapsed().as_secs_f64();
            let stream: Vec<u8> = aus.concat();
            if qp == qps[1] {
                mid_mpx = (w * h * frames.len()) as f64 / secs / 1e6;
            }
            if let Some(p) = psnr_vs_source(&stream, &frames, w, h) {
                curve.push((stream.len() as f64, p));
            }
        }
        println!("  ours/{pname:<9} {}", curve.iter().zip(&qps)
            .map(|((b, p), q)| format!("qp{q}:{:.0}KiB/{p:.2}dB", b / 1024.0)).collect::<Vec<_>>().join("  "));
        ours.push((pname, curve, mid_mpx));
    }

    // --- x264 curves (baseline profile = our implemented toolset) ---
    let mut refs: Vec<(String, Vec<(f64, f64)>, f64)> = Vec::new();
    for xp in &x_presets {
        let mut curve = Vec::new();
        let mut mid_mpx = 0.0;
        for &qp in &qps {
            let out = tmp.join(format!("xb_{name}_{xp}_{qp}.264"));
            let _ = std::fs::remove_file(&out); // never score a stale artifact
            let o = Command::new(&x264)
                // --frames: both arms MUST encode the same pictures. Without it x264
                // consumed all 120 frames of the clip while we encoded 24, and the
                // frame-count guard (correctly) dropped every point.
                .args(["--threads", "1", "--profile",
                       if std::env::var_os("XB_CABAC").is_some() { "main" } else { "baseline" },
                       "--preset", xp,
                       "--qp", &qp.to_string(), "--keyint", "60",
                       "--frames", &frames.len().to_string(),
                       "--bframes", "0", "-o"])
                .arg(&out).arg(&path).output().expect("spawn x264");
            let log = String::from_utf8_lossy(&o.stderr).into_owned();
            let tail = log.rsplit("encoded ").next().unwrap_or("");
            let mut tok = tail.split_whitespace();
            let nf: usize = tok.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let fps: f64 = tok.nth(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            if qp == qps[1] && fps > 0.0 {
                mid_mpx = (w * h) as f64 * fps / 1e6;
            }
            let _ = nf;
            let stream = std::fs::read(&out).unwrap_or_default();
            if let Some(p) = psnr_vs_source(&stream, &frames, w, h) {
                curve.push((stream.len() as f64, p));
            }
            let _ = std::fs::remove_file(&out);
        }
        if curve.len() == qps.len() {
            refs.push((xp.clone(), curve, mid_mpx));
        }
    }
    println!();
    for (xp, c, _) in &refs {
        println!("  x264/{xp:<9} {}", c.iter().zip(&qps)
            .map(|((b, p), q)| format!("qp{q}:{:.0}KiB/{p:.2}dB", b / 1024.0)).collect::<Vec<_>>().join("  "));
    }

    println!("\n{:<12}{:>12}{:>11}{:>10}{:>9}{:>10}", "our preset", "vs x264", "BD-rate%", "overlap", "speed", "verdict");
    println!("{}", "-".repeat(66));
    for (pname, oc, ompx) in &ours {
        for (xp, xc, xmpx) in &refs {
            let (bd, ov) = bd_rate(xc, oc);
            let sp = if *xmpx > 0.0 { ompx / xmpx } else { f64::NAN };
            let verdict = if bd.is_nan() { "no overlap" }
                else if bd < 0.0 && sp > 1.0 { "WIN both" }
                else if sp > 1.0 { "faster" }
                else { "" };
            println!("{pname:<12}{xp:>12}{bd:>+10.1}%{ov:>9.2}dB{sp:>8.2}x{verdict:>10}");
        }
        println!("{}", "-".repeat(66));
    }
    println!("BD-rate = % MORE bits we spend for the same quality (negative = we win).");
    println!("speed   = our Mpx/s / theirs at qp{}.", qps[1]);
}
