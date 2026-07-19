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

/// Real-clip in-process bench: reads a raw I420 file (env `RUSTY_BENCH_YUV`,
/// dims `RUSTY_BENCH_WH` = "WxH", gop `RUSTY_BENCH_GOP`) fully into RAM ONCE, then
/// times ONLY the `enc.encode` loop (no file I/O, no output copy). Isolates the
/// codec core from CLI/system overhead so the two can be attributed separately.
#[test]
#[ignore]
fn profile_bench_file() {
    let path = std::env::var("RUSTY_BENCH_YUV").expect("set RUSTY_BENCH_YUV");
    let wh = std::env::var("RUSTY_BENCH_WH").unwrap_or_else(|_| "832x480".into());
    let (w, h): (usize, usize) = {
        let mut it = wh.split('x');
        (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
    };
    let gop: u32 = std::env::var("RUSTY_BENCH_GOP").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
    let qp: u8 = std::env::var("RUSTY_BENCH_QP").ok().and_then(|s| s.parse().ok()).unwrap_or(26);
    let raw = std::fs::read(&path).expect("read yuv");
    let fsz = w * h * 3 / 2;
    let n = raw.len() / fsz;
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let frames: Vec<YuvFrame> = (0..n)
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
        .collect();
    let preset = match std::env::var("RUSTY_BENCH_PRESET").as_deref() {
        Ok("quality") | Ok("slow") => Preset::Quality,
        _ => Preset::Fast,
    };
    let mut best = std::time::Duration::MAX;
    let mut bytes = 0usize;
    for _ in 0..5 {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = gop;
        cfg.qp = qp;
        cfg.preset = preset;
        let mut enc = Encoder::new(cfg).unwrap();
        let t = std::time::Instant::now();
        let mut b = 0;
        for f in &frames {
            b += enc.encode(f).len();
        }
        best = best.min(t.elapsed());
        bytes = b;
    }
    eprintln!(
        "\n=== BENCH FILE {path} {w}x{h} x{n} gop{gop} QP{qp} fast: core {:.1} ms ({:.1} Mpx/s), {} KiB ===",
        best.as_secs_f64() * 1e3,
        (n * w * h) as f64 / best.as_secs_f64() / 1e6,
        bytes / 1024
    );
}

/// Side-by-side A/B of the two inter-coding paths (`encode_inter_mb` v1 vs the
/// isolated coefficient-fused v2), selected by the hidden `coded_path_v2` knob.
/// Encodes the same clip both ways in ONE binary and (1) asserts the bitstreams
/// are BYTE-IDENTICAL, (2) reports best-of-N core time for each — the honest
/// interleaved measure of whether the fused path is faster on the coded path.
#[test]
#[ignore]
fn coded_path_ab() {
    let (w, h, n, gop) = (832usize, 480usize, 60usize, 30u32);
    let frames = make_clip(w, h, n);
    let run = |v2: bool| -> (std::time::Duration, Vec<u8>) {
        let mut best = std::time::Duration::MAX;
        let mut out = Vec::new();
        for _ in 0..7 {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.gop_size = gop;
            cfg.qp = 26;
            cfg.preset = Preset::Fast;
            cfg.coded_path_v2 = v2;
            let mut enc = Encoder::new(cfg).unwrap();
            let t = std::time::Instant::now();
            let mut bytes = Vec::new();
            for f in &frames {
                bytes.extend_from_slice(&enc.encode(f));
            }
            let e = t.elapsed();
            if e < best {
                best = e;
                out = bytes;
            }
        }
        (best, out)
    };
    // Interleave arms to fight thermal drift: v1, v2, v1, v2 ...
    let (mut b1, mut b2) = (std::time::Duration::MAX, std::time::Duration::MAX);
    let (mut o1, mut o2) = (Vec::new(), Vec::new());
    for _ in 0..3 {
        let (t1, x1) = run(false);
        let (t2, x2) = run(true);
        if t1 < b1 { b1 = t1; o1 = x1; }
        if t2 < b2 { b2 = t2; o2 = x2; }
    }
    eprintln!("\n=== coded-path A/B (INTER {w}x{h} x{n} gop{gop} QP26 fast) ===");
    eprintln!("  v1 (current):  {:>7.1} ms   {} KiB", b1.as_secs_f64() * 1e3, o1.len() / 1024);
    eprintln!("  v2 (fused):    {:>7.1} ms   {} KiB", b2.as_secs_f64() * 1e3, o2.len() / 1024);
    let d = 100.0 * (b1.as_secs_f64() - b2.as_secs_f64()) / b1.as_secs_f64();
    eprintln!("  v2 vs v1:      {d:+.1}%   {}", if o1 == o2 { "BYTE-IDENTICAL ✓" } else { "*** DIFFERS ***" });
    assert_eq!(o1, o2, "v2 bitstream must be byte-identical to v1");
}
