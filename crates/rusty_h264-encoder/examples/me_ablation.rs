//! Lever 4 — is the Quality preset's extra motion-estimation work worth its time?
//!
//! Quality costs 2.4–3.8× Balanced. This ablates the three knobs that separate them
//! and prices each on a REAL 4-QP BD-rate curve (both PSNR and SSIM), because a
//! single-QP size comparison of a decision-layer change is exactly the mirage the
//! dispatch skill warns about.
//!
//! Arms (all vs the full Quality preset as anchor):
//!   * `quality`        — anchor
//!   * `-sub8x8`        — P_8x8 sub-partition search off (4 extra ME searches/MB)
//!   * `-me_wide`       — the +-16 stalled-diamond rescue grid off
//!   * `-both`          — both off
//!   * `balanced`       — the next preset down (the retire-the-preset option)
//!
//! A negative BD-rate means the arm is BETTER than full Quality. A small positive
//! BD-rate for a large time saving is the case for cutting the knob.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example me_ablation \
//!     -- video-tests/clips/mobile_cif.y4m

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

#[derive(Clone, Copy)]
struct Arm {
    name: &'static str,
    preset: Preset,
    sub8x8: Option<bool>,
    me_wide: Option<bool>,
    /// U1 sub-pel refinement pattern; `None` leaves the process default.
    subpel_pat: Option<u32>,
    /// U1 online dispatcher on/off; `None` leaves the process default.
    subpel_disp: Option<bool>,
    /// U2 λ-normalised split-search threshold; `None` leaves the default.
    split_t: Option<u32>,
    /// U3: force sub-pel refinement on in a preset that normally skips it.
    force_subpel: bool,
    /// U6: tools implemented but default-OFF.
    cabac: bool,
    t8x8: bool,
    /// U5-struct: defer sub-pel to the winning partition.
    defer: Option<bool>,
    dia: Option<u32>,
}

/// One clip × the arm set → per-arm (psnr curve, ssim curve, total ms).
fn run_clip(path: &str, nframes: usize, qps: &[u8], arms: &[Arm]) -> (usize, usize, usize, Vec<(Vec<(f64, f64)>, Vec<(f64, f64)>, f64)>) {
    let (w, h, frames) = read_y4m(path, nframes);
    let mut curves: Vec<(Vec<(f64, f64)>, Vec<(f64, f64)>, f64)> = Vec::new();
    for arm in arms {
        let (mut psnr_c, mut ssim_c, mut total_ms) = (Vec::new(), Vec::new(), 0.0);
        for &qp in qps {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 30;
            cfg.preset = arm.preset;
            if arm.force_subpel {
                cfg.tune_subpel = true;
            }
            if arm.cabac {
                cfg.cabac = true;
                cfg.profile = rusty_h264_common::Profile::Main;
            }
            if arm.t8x8 {
                cfg.transform_8x8 = true;
                cfg.profile = rusty_h264_common::Profile::High;
            }
            cfg.sub_8x8 = arm.sub8x8;
            cfg.me_wide = arm.me_wide;
            if let Some(p) = arm.subpel_pat {
                rusty_h264_encoder::set_subpel_pattern(p);
            }
            rusty_h264_encoder::set_subpel_dispatch(arm.subpel_disp.unwrap_or(false));
            rusty_h264_encoder::set_split_t(arm.split_t.unwrap_or(0));
            rusty_h264_encoder::set_defer_subpel(arm.defer.unwrap_or(false));
            rusty_h264_encoder::set_dia_mask(arm.dia.unwrap_or(rusty_h264_encoder::DIA_DEFAULT_MASK));
            let enc = Encoder::new(cfg).expect("cfg");
            let t = std::time::Instant::now();
            let aus = enc.encode_all(&frames).expect("encode");
            total_ms += t.elapsed().as_secs_f64() * 1e3;
            let bytes: usize = aus.iter().map(Vec::len).sum();
            let mut dec = rusty_h264_decoder::Decoder::new();
            let (mut se, mut n, mut sacc, mut sn) = (0f64, 0u64, 0f64, 0u64);
            for (au, src) in aus.iter().zip(&frames) {
                if let Ok(Some(r)) = dec.decode(au) {
                    for (a, b) in src.y.iter().zip(&r.y) {
                        let d = *a as f64 - *b as f64;
                        se += d * d;
                        n += 1;
                    }
                    sacc += ssim_y(&src.y, &r.y, w, h);
                    sn += 1;
                }
            }
            let psnr = 10.0 * (255.0f64 * 255.0 / (se / n as f64)).log10();
            psnr_c.push((bytes as f64, psnr));
            ssim_c.push((bytes as f64, ssim_db(sacc / sn.max(1) as f64)));
        }
        curves.push((psnr_c, ssim_c, total_ms));
    }
    (w, h, frames.len(), curves)
}

/// CORPUS MODE: `AB_CORPUS=1` runs every clip given as an argument and prints a
/// PER-CLIP truth table for the `-me_wide` arm. This is the table that decides
/// whether me_wide earns its default-on: the dispatch discipline says get the
/// per-clip truth table BEFORE designing (or trusting) the gate, and a sign-flip
/// across clips is itself the dispatch signal.
///
/// BD-rate columns are DETERMINISTIC (rate + PSNR/SSIM), so they are valid on a
/// loaded machine; the ms columns are indicative only.
fn corpus_mode(paths: &[String], qps: &[u8]) {
    let arms = [
        Arm { name: "quality (anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        Arm { name: "-me_wide", preset: Preset::Quality, sub8x8: None, me_wide: Some(false), subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
    ];
    println!("me_wide corpus truth table — anchor = Quality (me_wide ON), test = me_wide OFF");
    println!("NEGATIVE BD = turning me_wide OFF is BETTER. POSITIVE = me_wide earns its cost.\n");
    println!("{:<26} {:>5} {:>7} {:>10} {:>11} {:>11}", "clip", "n", "speedup", "anchor ms", "BD-PSNR%", "BD-SSIM%");
    println!("{}", "-".repeat(76));

    let (mut sp, mut ss, mut nclips) = (0.0f64, 0.0f64, 0usize);
    let (mut worst_p, mut worst_c) = (f64::NEG_INFINITY, String::new());
    for path in paths {
        // Keep total work bounded: fewer frames on the big rungs.
        let px = {
            let raw = std::fs::read(path).map(|r| r.len()).unwrap_or(0);
            raw
        };
        let nframes = if px > 400_000_000 { 8 } else if px > 100_000_000 { 12 } else { 24 };
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let (_, _, n, c) = run_clip(path, nframes, qps, &arms);
        let (bp, bs) = (bd_rate(&c[0].0, &c[1].0), bd_rate(&c[0].1, &c[1].1));
        println!(
            "{:<26} {:>5} {:>6.2}x {:>10.0} {:>+11.2} {:>+11.2}",
            name, n, c[0].2 / c[1].2, c[0].2, bp, bs
        );
        if bp.is_finite() {
            sp += bp;
            ss += bs;
            nclips += 1;
            if bp > worst_p {
                worst_p = bp;
                worst_c = name;
            }
        }
    }
    println!("{}", "-".repeat(76));
    println!("  mean over {nclips} clips: BD-PSNR {:+.2}%  BD-SSIM {:+.2}%", sp / nclips as f64, ss / nclips as f64);
    println!("  clip where me_wide helps MOST (worst for turning it off): {worst_c} {worst_p:+.2}%");
    println!("\n  Read: a clip with a clearly POSITIVE BD is one me_wide earns its 1.4x on.");
    println!("  All-negative/zero across the corpus => me_wide does not earn default-on for this content.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let nframes: usize = std::env::var("AB_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    let qps: Vec<u8> = std::env::var("AB_QPS")
        .unwrap_or_else(|_| "22,27,32,37".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();

    if std::env::var_os("AB_SPCAP").is_some() {
        // Track-B B3: the sub-pel iteration budget, alone and paired with B2
        // (set RFF_ME_SADL=0.5 in the environment for the B2 arms' λ). The
        // hypothesis under test: the cap is what unlocks B2's speed, because the
        // convergence-driven ring is what eats the SAD savings.
        let base = Arm {
            name: "anchor", preset: Preset::Quality, sub8x8: None, me_wide: None,
            subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false,
            cabac: false, t8x8: false, defer: None, dia: None,
        };
        let arms: [(&str, bool, u32); 5] = [
            ("anchor (uncapped)", false, 0),
            ("cap2", false, 2),
            ("cap3", false, 3),
            ("B2+cap2", true, 2),
            ("B2+cap3", true, 3),
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let mut curves = Vec::new();
            for &(_, sadfp, cap) in &arms {
                rusty_h264_encoder::set_me_sadfp(sadfp);
                rusty_h264_encoder::set_sp_maxit(cap);
                let (_, _, _, c) = run_clip(path, nframes, &qps, &[base]);
                curves.push(c.into_iter().next().unwrap());
            }
            rusty_h264_encoder::set_me_sadfp(false);
            rusty_h264_encoder::set_sp_maxit(0);
            println!("\n=== {name} — anchor = uncapped SATD-fp quality ===");
            println!("{:<22}{:>9}{:>10}{:>11}{:>11}", "arm", "ms", "speed", "BD-PSNR%", "BD-SSIM%");
            for (i, &(an, _, _)) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&curves[0].0, &curves[i].0), bd_rate(&curves[0].1, &curves[i].1));
                println!(
                    "{:<22}{:>9.0}{:>9.2}x{:>+11.2}{:>+11.2}",
                    an, curves[i].2, curves[0].2 / curves[i].2, bp, bs
                );
            }
        }
        return;
    }
    if std::env::var_os("AB_SADFP").is_some() {
        // Track-B B2: SAD full-pel + SATD-from-sub-pel (x264's cost split on every
        // preset). Bitstream-changing, so the per-clip 4-QP BD table IS the gate:
        // monotone non-regression ⇒ flip the default; a sign-flip ⇒ dispatch it;
        // uniform loss ⇒ prune. The knob is process-global (`set_me_sadfp`), so each
        // arm runs as its own single-arm `run_clip` with the knob set around it.
        let base = Arm {
            name: "SATD fullpel (anchor)", preset: Preset::Quality, sub8x8: None,
            me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None,
            force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None,
        };
        println!("B2 SAD-fullpel truth table — anchor = Quality, SATD full-pel (today's default)");
        println!("POSITIVE BD = SAD full-pel costs quality; NEGATIVE = it wins outright.");
        println!("The −wide pair isolates the RESCUE interaction (me_wide off both arms).\n");
        println!(
            "{:<20} {:>4} | {:>8} {:>9} {:>9} | {:>8} {:>9} {:>9}",
            "clip", "n", "speed", "BD-PSNR%", "BD-SSIM%", "spd -w", "BDP -w", "BDS -w"
        );
        println!("{}", "-".repeat(92));
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            // The −wide diagnostic pair costs 2× the encodes; corpus runs skip it
            // unless AB_SADFP_WIDE is set.
            let diag_wide = std::env::var_os("AB_SADFP_WIDE").is_some();
            let nowide = Arm { name: "-wide", me_wide: Some(false), ..base };
            rusty_h264_encoder::set_me_sadfp(false);
            let (_, _, n, c0) = run_clip(path, nframes, &qps, &[base]);
            let c0w = diag_wide.then(|| run_clip(path, nframes, &qps, &[nowide]).3);
            // AB_SADFP_MODE=1 measures the DISPATCHED arm instead of force-on.
            if std::env::var("AB_SADFP_MODE").as_deref() == Ok("1") {
                rusty_h264_encoder::set_me_sadfp_mode(1);
            } else {
                rusty_h264_encoder::set_me_sadfp(true);
            }
            let (_, _, _, c1) = run_clip(path, nframes, &qps, &[Arm { name: "SAD fullpel", ..base }]);
            let c1w = diag_wide.then(|| run_clip(path, nframes, &qps, &[Arm { name: "SAD -wide", ..nowide }]).3);
            let (bp, bs) = (bd_rate(&c0[0].0, &c1[0].0), bd_rate(&c0[0].1, &c1[0].1));
            if let (Some(c0w), Some(c1w)) = (&c0w, &c1w) {
                let (bpw, bsw) = (bd_rate(&c0w[0].0, &c1w[0].0), bd_rate(&c0w[0].1, &c1w[0].1));
                println!(
                    "{:<20} {:>4} | {:>7.2}x {:>+9.2} {:>+9.2} | {:>7.2}x {:>+9.2} {:>+9.2}",
                    name, n, c0[0].2 / c1[0].2, bp, bs, c0w[0].2 / c1w[0].2, bpw, bsw
                );
            } else {
                println!(
                    "{:<20} {:>4} | {:>7.2}x {:>+9.2} {:>+9.2} |",
                    name, n, c0[0].2 / c1[0].2, bp, bs
                );
            }
        }
        rusty_h264_encoder::set_me_sadfp(false);
        return;
    }
    if std::env::var_os("AB_SP").is_some() {
        // Descent D: the sub-pel ring. Census says the 4 DIAGONAL positions improve
        // 0.94-6.5% of the time against the axes' 9.5-19.5%, and ITERATION 2 is 35-40%
        // of evals at a 1.4-2.5% hit rate. A low hit rate is not the same as low VALUE
        // (Descent A found coarse rungs were actively harmful; that need not repeat
        // here, since a diagonal is a legitimate NEARBY position, not a distant jump),
        // so price every combination on a real 4-QP BD curve.
        let mk = |name: &'static str, pat: u32| Arm {
            name, preset: Preset::Quality, sub8x8: None, me_wide: None,
            subpel_pat: Some(pat), subpel_disp: None, split_t: None, force_subpel: false,
            cabac: false, t8x8: false, defer: None, dia: None,
        };
        let arms = [
            mk("ring8 iter (anchor)", 0),
            mk("ring4 iter", 1),
            mk("ring8 single-pass", 2),
            mk("ring4 single-pass", 3),
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = ring8 + iterate ===");
            println!("{:<22}{:>9}{:>11}{:>11}", "pattern", "ms", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<22}{:>9.0}{:>+11.2}{:>+11.2}", a.name, c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_DIA").is_some() {
        // Descent A: the coarse-to-fine ladder [64,32,16,8,4]. The per-rung census says
        // the four COARSE rungs are 76-80% of full-pel evals at a 0.05-1.0% hit rate.
        // A low hit rate is not the same as low VALUE (a coarse hit escapes a local
        // minimum), so price each ablation on a real 4-QP BD curve, not eval counts.
        let mk = |name: &'static str, dia: u32| Arm {
            name, preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None,
            subpel_disp: None, split_t: None, force_subpel: false, cabac: false,
            t8x8: false, defer: None, dia: Some(dia),
        };
        let arms = [
            mk("64,32,16,8,4 (anch)", 0b11111),
            mk("16,8,4", 0b11100),
            mk("16,4", 0b10100),
            mk("8,4", 0b11000),
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = full 5-rung ladder ===");
            println!("{:<22}{:>9}{:>10}{:>11}{:>11}", "ladder", "ms", "speed", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<22}{:>9.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_DEFER").is_some() {
        let arms = [
            Arm { name: "defer OFF(anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: Some(false), dia: None },
            Arm { name: "defer ON", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: Some(true), dia: None },
            Arm { name: "defer ON balanced", preset: Preset::Balanced, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: Some(true), dia: None },
            Arm { name: "balanced OFF", preset: Preset::Balanced, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: Some(false), dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = quality, defer OFF ===");
            println!("{:<20}{:>9}{:>10}{:>11}{:>11}", "arm", "ms", "speed", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<20}{:>9.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_U6").is_some() {
        // U6: what do the implemented-but-default-OFF tools actually buy?
        let arms = [
            Arm { name: "default(anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "+CABAC", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: true, t8x8: false, defer: None, dia: None },
            Arm { name: "+8x8", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: true, defer: None, dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = shipped default ===");
            println!("{:<17}{:>9}{:>10}{:>11}{:>11}", "tool", "ms", "speed", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<17}{:>9.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_U3").is_some() {
        // U3: the fast preset's +56..+118% BD gap is "no sub-pel at all". Does a CHEAP
        // sub-pel (single pass, no iteration) buy most of it back?
        let arms = [
            Arm { name: "fast (anchor)", preset: Preset::Fast, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "fast+subpel1p", preset: Preset::Fast, sub8x8: None, me_wide: None, subpel_pat: Some(2), subpel_disp: Some(false), split_t: None, force_subpel: true, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "fast+subpel4p", preset: Preset::Fast, sub8x8: None, me_wide: None, subpel_pat: Some(3), subpel_disp: Some(false), split_t: None, force_subpel: true, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "fast+subpelFull", preset: Preset::Fast, sub8x8: None, me_wide: None, subpel_pat: Some(0), subpel_disp: Some(false), split_t: None, force_subpel: true, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "balanced", preset: Preset::Balanced, sub8x8: None, me_wide: None, subpel_pat: Some(0), subpel_disp: Some(false), split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = fast (no sub-pel) ===");
            println!("{:<17}{:>9}{:>10}{:>11}{:>11}", "arm", "ms", "vs fast", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<17}{:>9.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_SPLIT").is_some() {
        let arms = [
            Arm { name: "T=0 (anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: Some(0), force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "T=400",        preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: Some(400), force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "T=600",        preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: Some(600), force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "T=800",        preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: Some(800), force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = split gate OFF ===");
            println!("{:<15}{:>10}{:>10}{:>11}{:>11}", "arm", "ms", "speedup", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<15}{:>10.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_SPDISP").is_some() {
        let arms = [
            Arm { name: "pat0 (anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(0), subpel_disp: Some(false), split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "pat2 always",   preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(2), subpel_disp: Some(false), split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "DISPATCHED",    preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(0), subpel_disp: Some(true), split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = pat0 ===");
            println!("{:<16}{:>10}{:>10}{:>11}{:>11}", "arm", "ms", "speedup", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<16}{:>10.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_SUBPEL").is_some() {
        // U1: the four sub-pel refinement patterns, anchored on the current default.
        let arms = [
            Arm { name: "pat0 8pt+iter*", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(0), subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "pat1 4pt+iter",  preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(1), subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "pat2 8pt 1pass", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(2), subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
            Arm { name: "pat3 4pt 1pass", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: Some(3), subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        ];
        for path in &args {
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let (_, _, n, c) = run_clip(path, nframes, &qps, &arms);
            println!("
=== {name} (x{n}) — anchor = pat0 (current default) ===");
            println!("{:<16}{:>10}{:>10}{:>11}{:>11}", "pattern", "ms", "speedup", "BD-PSNR%", "BD-SSIM%");
            for (i, a) in arms.iter().enumerate() {
                let (bp, bs) = (bd_rate(&c[0].0, &c[i].0), bd_rate(&c[0].1, &c[i].1));
                println!("{:<16}{:>10.0}{:>9.2}x{:>+11.2}{:>+11.2}", a.name, c[i].2, c[0].2 / c[i].2, bp, bs);
            }
        }
        return;
    }
    if std::env::var_os("AB_CORPUS").is_some() {
        corpus_mode(&args, &qps);
        return;
    }

    let path = args.first().cloned().unwrap_or_else(|| "video-tests/clips/mobile_cif.y4m".into());
    let (w, h, frames) = read_y4m(&path, nframes);

    let arms = [
        Arm { name: "quality (anchor)", preset: Preset::Quality, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        Arm { name: "-sub8x8", preset: Preset::Quality, sub8x8: Some(false), me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        Arm { name: "-me_wide", preset: Preset::Quality, sub8x8: None, me_wide: Some(false), subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        Arm { name: "-both", preset: Preset::Quality, sub8x8: Some(false), me_wide: Some(false), subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
        Arm { name: "balanced", preset: Preset::Balanced, sub8x8: None, me_wide: None, subpel_pat: None, subpel_disp: None, split_t: None, force_subpel: false, cabac: false, t8x8: false, defer: None, dia: None },
    ];

    println!("ME ablation — {path} {w}x{h} x{} QPs {qps:?}\n", frames.len());
    println!("anchor = full Quality. NEGATIVE BD = better than Quality.");
    println!("A small POSITIVE BD for a large time cut is the case for cutting the knob.\n");

    let mut curves: Vec<(Vec<(f64, f64)>, Vec<(f64, f64)>, f64)> = Vec::new();
    for arm in arms {
        let (mut psnr_c, mut ssim_c, mut total_ms) = (Vec::new(), Vec::new(), 0.0);
        for &qp in &qps {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 30;
            cfg.preset = arm.preset;
            if arm.force_subpel {
                cfg.tune_subpel = true;
            }
            if arm.cabac {
                cfg.cabac = true;
                cfg.profile = rusty_h264_common::Profile::Main;
            }
            if arm.t8x8 {
                cfg.transform_8x8 = true;
                cfg.profile = rusty_h264_common::Profile::High;
            }
            cfg.sub_8x8 = arm.sub8x8;
            cfg.me_wide = arm.me_wide;

            let enc = Encoder::new(cfg).expect("cfg");
            let t = std::time::Instant::now();
            let aus = enc.encode_all(&frames).expect("encode");
            total_ms += t.elapsed().as_secs_f64() * 1e3;

            let bytes: usize = aus.iter().map(Vec::len).sum();
            let mut dec = rusty_h264_decoder::Decoder::new();
            let (mut se, mut n, mut sacc, mut sn) = (0f64, 0u64, 0f64, 0u64);
            for (au, src) in aus.iter().zip(&frames) {
                if let Ok(Some(r)) = dec.decode(au) {
                    for (a, b) in src.y.iter().zip(&r.y) {
                        let d = *a as f64 - *b as f64;
                        se += d * d;
                        n += 1;
                    }
                    sacc += ssim_y(&src.y, &r.y, w, h);
                    sn += 1;
                }
            }
            let psnr = 10.0 * (255.0f64 * 255.0 / (se / n as f64)).log10();
            psnr_c.push((bytes as f64, psnr));
            ssim_c.push((bytes as f64, ssim_db(sacc / sn.max(1) as f64)));
        }
        curves.push((psnr_c, ssim_c, total_ms));
    }

    println!("{:<18} {:>10} {:>10} {:>11} {:>11}", "arm", "ms(4 QPs)", "speedup", "BD-PSNR%", "BD-SSIM%");
    println!("{}", "-".repeat(64));
    let anchor_ms = curves[0].2;
    for (i, arm) in arms.iter().enumerate() {
        let (p, s, ms) = (&curves[i].0, &curves[i].1, curves[i].2);
        let (bp, bs) = (bd_rate(&curves[0].0, p), bd_rate(&curves[0].1, s));
        println!(
            "{:<18} {:>10.0} {:>9.2}x {:>+11.2} {:>+11.2}",
            arm.name, ms, anchor_ms / ms, bp, bs
        );
    }
    println!("\nper-QP anchor points:");
    for (j, &qp) in qps.iter().enumerate() {
        println!("  qp{qp}: {:.0} KiB / {:.2} dB", curves[0].0[j].0 / 1024.0, curves[0].0[j].1);
    }
}
