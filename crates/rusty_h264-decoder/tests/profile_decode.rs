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
