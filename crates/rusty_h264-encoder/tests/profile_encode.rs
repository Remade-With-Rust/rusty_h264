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
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

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
    run_preset(label, w, h, n, gop, Preset::Fast);
}

fn run_preset(label: &str, w: usize, h: usize, n: usize, gop: u32, preset: Preset) {
    let frames = make_clip(w, h, n);
    const REPS: usize = 5;
    let mut best = std::time::Duration::MAX;
    let mut bytes = 0usize;
    for _ in 0..REPS {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = gop;
        cfg.qp = 26;
        cfg.preset = preset;
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
    eprintln!("-- FAST preset (default; SAD/psadbw mode decision) --");
    run_preset("INTER", 832, 480, 12, 12, Preset::Fast); // 1 IDR + 11 P
    run_preset("ALL-INTRA", 832, 480, 12, 1, Preset::Fast); // every frame IDR
    eprintln!("-- QUALITY preset (SATD mode decision — the asm-SATD target) --");
    run_preset("INTER", 832, 480, 12, 12, Preset::Quality);
    run_preset("ALL-INTRA", 832, 480, 12, 1, Preset::Quality);
}

/// Stage breakdown (needs `--features profile[,asm]`): encodes a 60-frame clip
/// sequentially and dumps the per-stage buckets. The Enc* stages are a disjoint
/// top-level partition of encode(); IntraPred/InterMc/Reconstruct/Deblock/Entropy
/// are shared-primitive scopes that NEST inside them (read as within-stage detail,
/// don't sum them with the Enc* lines).
#[test]
#[ignore]
fn profile_encode_stages() {
    use rusty_h264_common::prof;
    let (w, h, n) = (832usize, 480usize, 60usize);
    let frames = make_clip(w, h, n);
    for &(label, gop) in &[("INTER gop30", 30u32), ("ALL-INTRA", 1u32)] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = gop;
        cfg.qp = 26;
        cfg.preset = Preset::Fast;
        // warmup (populate caches/JIT-ish effects), then measured run
        let mut enc = Encoder::new(cfg.clone()).unwrap();
        for f in frames.iter().take(6) {
            let _ = enc.encode(f);
        }
        prof::reset();
        let mut enc = Encoder::new(cfg).unwrap();
        let t = std::time::Instant::now();
        let mut bytes = 0usize;
        for f in &frames {
            bytes += enc.encode(f).len();
        }
        let wall = t.elapsed();
        eprintln!("\n=== stages: {label} {w}x{h} x{n} QP26 fast — wall {:.1} ms, {} bytes ===", wall.as_secs_f64()*1e3, bytes);
        prof::dump();
    }
}

/// Median-of-N per-stage breakdown (INTER, fast, the deployment case) — the honest
/// A/B instrument on a thermally-drifting box. Prints per-stage MEDIAN ms over N
/// full encodes plus min/max, so non-overlapping ranges are a reliable verdict.
#[test]
#[ignore]
fn profile_encode_median() {
    use rusty_h264_common::prof;
    let (w, h, n, passes) = (832usize, 480usize, 60usize, 9usize);
    let frames = make_clip(w, h, n);
    let mut cfg = EncoderConfig::new(w, h);
    cfg.gop_size = 30;
    cfg.qp = 26;
    cfg.preset = Preset::Fast;
    // warmup
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    for f in frames.iter().take(12) {
        let _ = enc.encode(f);
    }
    let mut per: Vec<[(f64, u64); rusty_h264_common::prof::N]> = Vec::new();
    for _ in 0..passes {
        prof::reset();
        let mut enc = Encoder::new(cfg.clone()).unwrap();
        for f in &frames {
            let _ = enc.encode(f);
        }
        per.push(prof::snapshot());
    }
    eprintln!("\n=== median-of-{passes} stages: INTER {w}x{h} x{n} QP26 fast ===");
    for i in 0..rusty_h264_common::prof::N {
        let mut ms: Vec<f64> = per.iter().map(|s| s[i].0).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let calls = per[0][i].1;
        if ms[passes - 1] > 0.05 {
            eprintln!(
                "  {:<28} med {:>8.1} ms   [{:>7.1} .. {:>7.1}]  ({} calls)",
                prof::name(i), ms[passes / 2], ms[0], ms[passes - 1], calls
            );
        }
    }
}

/// INTERLEAVED A/B of the P_Skip MB-gate knob under one thermal state: alternate
/// arms pass-by-pass in one process, report per-arm per-stage medians.
#[test]
#[ignore]
fn profile_skip_ab() {
    use rusty_h264_common::prof;
    let (w, h, n, passes) = (832usize, 480usize, 60usize, 7usize);
    let frames = make_clip(w, h, n);
    let mk = |gate: bool| {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 30;
        cfg.qp = 26;
        cfg.preset = Preset::Fast;
        cfg.tune_skip_accel_check = gate;
        cfg
    };
    // warmup
    let mut enc = Encoder::new(mk(true)).unwrap();
    for f in frames.iter().take(12) { let _ = enc.encode(f); }
    let idx_skip = prof::Stage::EncSkip as usize;
    let idx_free = prof::Stage::EncFree as usize;
    let idx_tot = prof::Stage::Total as usize;
    let mut arms: [Vec<[f64; 3]>; 2] = [Vec::new(), Vec::new()];
    for p in 0..passes * 2 {
        let gate = p % 2 == 0; // alternate every pass
        prof::reset();
        let mut enc = Encoder::new(mk(gate)).unwrap();
        for f in &frames { let _ = enc.encode(f); }
        let s = prof::snapshot();
        arms[if gate { 1 } else { 0 }].push([s[idx_skip].0, s[idx_free].0, s[idx_tot].0]);
    }
    for (name, a) in [("gate OFF", &arms[0]), ("gate ON ", &arms[1])] {
        for (li, label) in ["enc-skip-check", "enc-skip-freecheck", "TOTAL"].iter().enumerate() {
            let mut v: Vec<f64> = a.iter().map(|r| r[li]).collect();
            v.sort_by(|x, y| x.partial_cmp(y).unwrap());
            eprintln!("  {name} {label:<20} med {:>7.1} ms  [{:>6.1} .. {:>6.1}]", v[passes / 2], v[0], v[passes - 1]);
        }
    }
}
