//! Deterministic decode benchmark + stage profiler driver — the instrument for
//! decoder performance work (see the optimize-codec playbook).
//!
//! Throughput (the honest number) — run with the profiler OFF so there's no timer
//! overhead:
//!
//! ```text
//! cargo test -p rusty_h264-decoder --release profile_decode -- --ignored --nocapture
//! ```
//!
//! Stage breakdown (decomposes the "OTHER" bucket) — run with the profiler ON:
//!
//! ```text
//! cargo test -p rusty_h264-decoder --release --features profile profile_decode -- --ignored --nocapture
//! ```
//!
//! Add `--features asm` (needs nasm) to profile the SIMD-accelerated deployment;
//! the default (scalar) run keeps every kernel in `rusty_h264-common` so the
//! breakdown captures the whole pipeline cleanly.

use rusty_h264_common::YuvFrame;
use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig};

/// A deterministic clip: a textured background that pans left + four moving
/// textured boxes — enough intra detail, inter motion, and residual energy to
/// exercise MC, intra prediction, reconstruction, and deblocking representatively.
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
            // Mild chroma texture so the chroma path isn't entirely skipped.
            let chroma = |off: usize| -> Vec<u8> {
                (0..cw * ch)
                    .map(|idx| (128 + (((idx + t + off) >> 3) & 0x0f) as i32 - 8) as u8)
                    .collect()
            };
            YuvFrame {
                width: w,
                height: h,
                y,
                u: chroma(0),
                v: chroma(5),
            }
        })
        .collect()
}

#[test]
#[ignore = "perf instrument: run with --release [--features profile] -- --ignored --nocapture"]
fn profile_decode() {
    let (w, h, n) = (1280, 720, 10);
    let frames = make_clip(w, h, n);

    // One IDR then P-frames, moderate QP so blocks carry real residual.
    let mut cfg = EncoderConfig::new(w, h);
    cfg.gop_size = n as u32;
    cfg.qp = 26;
    let mut enc = Encoder::new(cfg).expect("encoder init");
    let aus: Vec<Vec<u8>> = frames.iter().map(|f| enc.encode(f)).collect();
    let stream_bytes: usize = aus.iter().map(|a| a.len()).sum();

    // Throughput: best-of-N full-sequence decode (each from a fresh decoder).
    const REPS: usize = 5;
    let mut best = std::time::Duration::MAX;
    let mut px = 0usize;
    for _ in 0..REPS {
        let mut dec = Decoder::new();
        let t = std::time::Instant::now();
        let mut p = 0;
        for au in &aus {
            if let Some(fr) = dec.decode(au).expect("decode") {
                p += fr.width * fr.height;
            }
        }
        best = best.min(t.elapsed());
        px = p;
    }
    let mpx_s = px as f64 / best.as_secs_f64() / 1e6;

    eprintln!(
        "\n=== profile_decode — {w}x{h} x{n} frames (1 IDR + {} P), {} KiB stream ===",
        n - 1,
        stream_bytes / 1024
    );
    eprintln!(
        "throughput: {:.1} Mpx/s   (best of {REPS}: {:.1} ms/run, {px} luma px)",
        mpx_s,
        best.as_secs_f64() * 1e3
    );
    eprintln!("  measure throughput WITHOUT --features profile; the breakdown below needs it ON");

    // A clean single-pass stage breakdown.
    rusty_h264_common::prof::reset();
    let mut dec = Decoder::new();
    for au in &aus {
        let _ = dec.decode(au).expect("decode");
    }
    rusty_h264_common::prof::dump();
}

/// Meticulous per-stage measurement: many breakdown passes → per-stage MEDIAN ms
/// (+ min/max spread) to beat thermal noise, a self-calibrated per-scope timer
/// overhead, and an overhead-corrected "real glue" estimate cross-checked against the
/// profiler-OFF wall. Run with the profiler ON:
///
/// `cargo test -p rusty_h264-decoder --release --features profile profile_decode_meticulous -- --ignored --nocapture`
///
/// Add `--features asm` to measure the SIMD deployment path.
#[test]
#[ignore = "perf instrument: run with --release --features profile -- --ignored --nocapture"]
fn profile_decode_meticulous() {
    use rusty_h264_common::prof;

    let (w, h, n) = (1280, 720, 10);
    let frames = make_clip(w, h, n);
    let mut cfg = EncoderConfig::new(w, h);
    cfg.gop_size = n as u32;
    cfg.qp = 26;
    let mut enc = Encoder::new(cfg).expect("encoder init");
    let aus: Vec<Vec<u8>> = frames.iter().map(|f| enc.encode(f)).collect();
    let px: usize = n * w * h;

    let decode_all = |aus: &[Vec<u8>]| {
        let mut dec = Decoder::new();
        for au in aus {
            let _ = dec.decode(au).expect("decode");
        }
    };
    // Wall of a full decode in THIS binary. NB: with `--features profile` the scopes
    // are compiled in, so this is the profile-ON wall (≈ TOTAL below), not the honest
    // real-decode time — that comes from `TOTAL_on − overhead`, cross-checked against
    // the profiler-OFF throughput printed by the plain `profile_decode` test.
    let mut wall_best = f64::MAX;
    for _ in 0..15 {
        let t = std::time::Instant::now();
        decode_all(&aus);
        wall_best = wall_best.min(t.elapsed().as_secs_f64() * 1e3);
    }

    // --- self-calibrate the per-scope timer overhead (one enter+exit pair) ---
    // A tight loop of empty scopes; black_box keeps the Guard (its Drop writes the
    // buckets, so it can't be elided). Zero when the profiler is compiled out.
    const M: usize = 8_000_000;
    prof::reset();
    let t = std::time::Instant::now();
    for _ in 0..M {
        let g = prof::scope(prof::Stage::Neighbors);
        std::hint::black_box(&g);
    }
    let scope_ns = t.elapsed().as_nanos() as f64 / M as f64;

    // --- meticulous breakdown: PREPS passes, keep every snapshot ---
    const PREPS: usize = 31;
    let mut runs: Vec<[(f64, u64); rusty_h264_common::prof::N]> = Vec::with_capacity(PREPS);
    for _ in 0..PREPS {
        prof::reset();
        decode_all(&aus);
        runs.push(prof::snapshot());
    }
    prof::reset();

    let n_stages = rusty_h264_common::prof::N;
    let sub = n_stages - 1; // Total lives at the last index
    let median = |vals: &mut Vec<f64>| -> f64 {
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        vals[vals.len() / 2]
    };
    let stat = |i: usize| -> (f64, f64, f64, u64) {
        let mut v: Vec<f64> = runs.iter().map(|r| r[i].0).collect();
        let (mn, mx) = (
            v.iter().cloned().fold(f64::MAX, f64::min),
            v.iter().cloned().fold(0.0, f64::max),
        );
        let calls = runs[0][i].1;
        (median(&mut v), mn, mx, calls)
    };

    let profile_on = stat(sub).0 > 1.0; // Total ~0 ⇒ profiler compiled out
    eprintln!("\n=== profile_decode_meticulous — {w}x{h} x{n} ({PREPS} passes, median) ===");
    if !profile_on {
        eprintln!("  (profiler OFF — this IS the honest wall; rerun with --features profile for the breakdown)");
        eprintln!("  profiler-OFF wall: {wall_best:.1} ms  ({:.1} Mpx/s)", px as f64 / (wall_best / 1e3) / 1e6);
        return;
    }

    let total = stat(sub);
    // Sum of the per-scope timer overhead across every sub-stage entry.
    let total_calls: u64 = (0..sub).map(|i| runs[0][i].1).sum();
    let overhead_ms = total_calls as f64 * scope_ns / 1e6;
    let sub_ms_sum: f64 = (0..sub).map(|i| stat(i).0).sum();
    let mgmt = (total.0 - sub_ms_sum).max(0.0);

    eprintln!(
        "  per-scope timer overhead: {scope_ns:.1} ns  →  {total_calls} scope entries = {overhead_ms:.1} ms of the profile",
    );
    eprintln!("  {:<15} {:>8} {:>8} {:>8} {:>7}   {:>12}", "stage", "med ms", "min", "max", "% tot", "calls");
    let mut ordered: Vec<usize> = (0..sub).collect();
    ordered.sort_by(|&a, &b| stat(b).0.partial_cmp(&stat(a).0).unwrap());
    for &i in &ordered {
        let (med, mn, mx, calls) = stat(i);
        eprintln!(
            "  {:<15} {:>8.2} {:>8.2} {:>8.2} {:>6.1}%   {:>12}",
            prof::name(i), med, mn, mx, 100.0 * med / total.0, calls,
        );
    }
    eprintln!(
        "  {:<15} {:>8.2} {:>29.1}%   ({:.1} ms overhead + {:.1} ms real glue)",
        "mgmt/other", mgmt, 100.0 * mgmt / total.0,
        overhead_ms.min(mgmt), (mgmt - overhead_ms).max(0.0),
    );
    eprintln!("  {:<15} {:>8.2} {:>29.1}%   (profile-ON wall {wall_best:.1} ms)", "TOTAL (on)", total.0, 100.0);
    let real = total.0 - overhead_ms;
    eprintln!(
        "\n  real decode ≈ TOTAL_on {:.1} − timer overhead {:.1} = {:.1} ms = {:.1} Mpx/s",
        total.0, overhead_ms, real, px as f64 / (real / 1e3) / 1e6,
    );
    eprintln!("  (confirm vs the profiler-OFF throughput from the plain `profile_decode` test)");
    eprintln!("  the REAL per-stage levers are the median-ms column, ranked above.");
}

/// Uniform-density clip for the cache probe: a high-frequency texture that shifts +
/// changes every frame, so ~every MB is coded (no skip bias) and per-MB cost is the
/// same at any frame size — isolating the working-set/cache effect from content.
fn probe_clip(w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    let (cw, ch) = (w / 2, h / 2);
    (0..n)
        .map(|t| {
            let y = (0..w * h)
                .map(|i| {
                    let (x, j) = (i % w, i / w);
                    (((x * 7 + j * 5 + t * 13) ^ (x.wrapping_mul(j) >> 4) ^ (t * 29)) & 0xff) as u8
                })
                .collect();
            let chroma = |o: usize| -> Vec<u8> {
                (0..cw * ch)
                    .map(|i| {
                        let (x, j) = (i % cw, i / cw);
                        (((x * 11 + j * 3 + t * 7 + o) ^ (t * 5)) & 0xff) as u8
                    })
                    .collect()
            };
            YuvFrame { width: w, height: h, y, u: chroma(0), v: chroma(64) }
        })
        .collect()
}

/// Phase 0 (decode-locality-plan): is the per-MB glue cache-bound? Decode the SAME
/// content density at frame sizes spanning the cache hierarchy. If per-pixel
/// throughput DROPS as the frame grows past L2, the strided-frame access is
/// cache-missing → MB-local tiles will help. If it's FLAT, the cost is
/// index-math/branch/dispatch and tiles won't — accept the floor.
///
/// `cargo test -p rusty_h264-decoder --release cache_probe -- --ignored --nocapture`
#[test]
#[ignore = "Phase 0 cache probe (see docs/decode-locality-plan.md)"]
fn cache_probe() {
    eprintln!("\n=== cache probe — per-pixel decode throughput vs frame size ===");
    eprintln!("(working set ~= 3 frames: rec + reference + grids; this machine L2≈2 MiB, L3≈24 MiB)");
    eprintln!("  if Mpx/s DROPS as the frame grows, the glue is cache-bound → tiles help\n");
    for &(w, h) in &[
        (256usize, 256usize),
        (384, 384),
        (512, 512),
        (768, 768),
        (1024, 1024),
        (1536, 1536),
        (1920, 1088),
    ] {
        let n = (8_000_000 / (w * h)).clamp(4, 32);
        let frames = probe_clip(w, h, n);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = n as u32; // 1 IDR + (n-1) P
        cfg.qp = 26;
        let mut enc = Encoder::new(cfg).expect("enc");
        let aus: Vec<Vec<u8>> = frames.iter().map(|f| enc.encode(f)).collect();

        const REPS: usize = 5;
        let mut best = std::time::Duration::MAX;
        let mut px = 0usize;
        for _ in 0..REPS {
            let mut dec = Decoder::new();
            let t = std::time::Instant::now();
            let mut p = 0;
            for au in &aus {
                if let Some(fr) = dec.decode(au).expect("dec") {
                    p += fr.width * fr.height;
                }
            }
            best = best.min(t.elapsed());
            px = p;
        }
        let mpx_s = px as f64 / best.as_secs_f64() / 1e6;
        let work_mib = (w * h + 2 * (w / 2) * (h / 2)) * 3 / (1024 * 1024);
        eprintln!(
            "  {w:>4}x{h:<4}  ({n:>2} frames, working set ~{work_mib:>2} MiB):  {mpx_s:>6.1} Mpx/s",
        );
    }
}
