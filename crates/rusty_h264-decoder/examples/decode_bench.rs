//! In-process decode throughput — DECODE only, no output path.
//!
//! `bench/decode_speedtest.sh` times the CLI, and the CLI accumulates every decoded
//! frame into a `Vec<YuvFrame>`, concatenates all of them into one buffer, and
//! writes that buffer. At 720p x240 that is ~331 MB of allocation, copy and write
//! charged to "decode" — and the script's differential does NOT cancel it, because
//! it scales with the frame count that the differential is taken over.
//!
//! This harness decodes access unit by access unit and drops each frame, which is
//! what ffmpeg's `-f null` does. It is the arm the ffmpeg comparison needs.
//!
//! ```text
//! cargo run --release -p rusty_h264-decoder --features asm --example decode_bench -- stream.264 [reps]
//! ```
//!
//! Prints frames decoded (the WORK COUNT — compare it against ffmpeg's, a
//! divergence voids the comparison), best-of-N ms, and Mpx/s.

use rusty_h264_decoder::Decoder;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: decode_bench <stream.264> [reps]");
    let reps: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(5);
    let input = std::fs::read(path).expect("read stream");

    let (mut best, mut worst) = (f64::MAX, 0f64);
    let (mut frames, mut px) = (0usize, 0usize);
    for _ in 0..reps {
        let mut dec = Decoder::new();
        let t = std::time::Instant::now();
        // decode_stream would build the whole Vec; feed the same split it uses but
        // drop each picture, so only DECODE is on the clock.
        let (mut f, mut p) = (0usize, 0usize);
        for au in rusty_h264_decoder::split_access_units(&input) {
            if let Some(fr) = dec.decode(au).expect("decode") {
                f += 1;
                p += fr.width * fr.height;
            }
        }
        let e = t.elapsed().as_secs_f64();
        best = best.min(e);
        worst = worst.max(e);
        frames = f;
        px = p;
    }
    println!(
        "{}: frames={frames} px={px} best={:.1}ms worst={:.1}ms spread={:.1}% -> {:.1} Mpx/s",
        path,
        best * 1e3,
        worst * 1e3,
        (worst - best) * 100.0 / best,
        px as f64 / best / 1e6
    );

    // With `--features profile`, one extra clean pass gives the stage breakdown.
    // Shares are read rather than absolute ms: on a contended box every stage is
    // slowed alike, so the RANKING survives noise that the wall clock does not.
    //
    // GATED ON THE FEATURE, and that gate is load-bearing. This pass used to run
    // unconditionally, so a `--features asm` build decoded the stream TWICE while
    // printing `frames=` from the timed pass alone. Any harness that measures whole
    // PROCESS cpu time — `bench/pinvs.ps1`, i.e. every ffmpeg comparison — therefore
    // charged us two decodes against ffmpeg's one, and the frame-count parity check
    // could not see it. Measured on long_cavlc: process 24,344 ms vs one decode
    // 14,690 ms. Do not un-gate this (codec-measurement §4).
    #[cfg(feature = "profile")]
    {
        rusty_h264_common::prof::reset();
        rusty_h264_common::deblock::census::reset();
        let mut dec = Decoder::new();
        for au in rusty_h264_decoder::split_access_units(&input) {
            let _ = dec.decode(au).expect("decode");
        }
        rusty_h264_common::prof::dump();
        rusty_h264_common::deblock::census::dump();
        let (d, b, t) = rusty_h264_decoder::bin_census::snapshot();
        let r = rusty_h264_decoder::bin_census::renorms();
        eprintln!(
            "--- CABAC bin census: {d} decisions ({r} renorm = {:.1}%), {b} bypasses, {t} terminates  (total {} bins) ---",
            100.0 * r as f64 / d.max(1) as f64,
            d + b + t
        );
    }

    // `dump()` prints only stages 0..Total. Everything past it — DecSetup, the
    // per-MB CABAC branch stages, the b_mc decomposition — is INFO and never
    // reaches the screen, so instrumentation that already exists reads as if it
    // did not. Print the whole table; a nested stage that overlaps a printed one
    // is labelled, not silently summed.
    let snap = rusty_h264_common::prof::snapshot();
    let total = snap[rusty_h264_common::prof::Stage::Total as usize].0.max(1e-9);
    let first = rusty_h264_common::prof::Stage::Total as usize + 1;
    if snap[first..].iter().all(|s| s.1 == 0) {
        return; // built without `--features profile`; nothing to show
    }
    // MC size x phase census, weighted by CYCLES. A call-count census is the wrong
    // denominator (a full-pel copy and a quarter-pel 6-tap differ ~10x), and this
    // table decides which const-width fast paths are worth building.
    #[cfg(feature = "profile")]
    {
    let mc = rusty_h264_common::inter::mcstats::snapshot_cycles();
    if !mc.is_empty() {
        let tot: u64 = mc.iter().map(|r| r.3).sum();
        eprintln!("--- MC census (size x phase), by CYCLES  [total {tot} cyc] ---");
        let mut rows = mc;
        rows.sort_by_key(|r| std::cmp::Reverse(r.3));
        for (size, phase, n, cyc) in rows.iter().take(12) {
            eprintln!(
                "  {size:<10} {phase:<8} {n:>9} calls  {:>5.1}% cycles  {:>6.1} cyc/call",
                100.0 * *cyc as f64 / tot.max(1) as f64,
                *cyc as f64 / (*n).max(1) as f64
            );
        }
    }
    }
    eprintln!("--- INFO / nested stages (not part of the residue sum) ---");
    for i in first..snap.len() {
        if snap[i].1 == 0 {
            continue;
        }
        eprintln!(
            "  {:<20} {:>8.1} ms  {:>5.1}%   ({} calls)",
            rusty_h264_common::prof::name(i),
            snap[i].0,
            100.0 * snap[i].0 / total,
            snap[i].1
        );
    }
}
