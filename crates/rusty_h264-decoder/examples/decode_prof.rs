//! Decode a real Annex-B `.264` file with wall timing + the stage profiler.
//!
//! Wall (honest number): build WITHOUT `profile`:
//!   cargo build --release -p rusty_h264-decoder --features asm --example decode_prof
//! Stage breakdown (~inflated wall): add `--features profile`.
//!
//!   decode_prof <stream.264>          env: DP_REPS (default 5)

use rusty_h264_decoder::Decoder;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_prof <stream.264>");
    let input = std::fs::read(&path).expect("read stream");
    let reps: usize = std::env::var("DP_REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);

    // Throughput: best-of-N full-stream decode, each from a fresh decoder.
    let mut best = f64::MAX;
    let mut px = 0usize;
    let mut nframes = 0usize;
    for _ in 0..reps {
        let mut dec = Decoder::new();
        let t = std::time::Instant::now();
        let frames = dec.decode_stream(&input).expect("decode");
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        px = frames.iter().map(|f| f.width * f.height).sum();
        nframes = frames.len();
    }
    let profiled = cfg!(feature = "profile");
    println!(
        "{path}: {nframes} frames, {:.1} KiB, best-of-{reps} {best:.2} ms = {:.1} Mpx/s{}",
        input.len() as f64 / 1024.0,
        px as f64 / (best / 1e3) / 1e6,
        if profiled { "  <-- PROFILER BUILD, wall inflated" } else { "" }
    );

    // One clean pass for the stage table.
    rusty_h264_common::prof::reset();
    let mut dec = Decoder::new();
    let _ = dec.decode_stream(&input).expect("decode");
    let snap = rusty_h264_common::prof::snapshot();
    for (i, (ms, calls)) in snap.iter().enumerate() {
        if *calls > 0 {
            println!(
                "    prof {:<16} {:>10.2} ms {:>12} calls {:>10.1} ns/call",
                rusty_h264_common::prof::name(i),
                ms,
                calls,
                ms * 1e6 / *calls as f64
            );
        }
    }
}
