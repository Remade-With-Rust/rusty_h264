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

use rusty_h264_common::YuvFrame;
use rusty_h264_decoder::Decoder;
use rusty_h264_decoder::edc_stats_report;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn feed_hash(h: &mut DefaultHasher, fr: &YuvFrame) {
    fr.y.hash(h);
    fr.u.hash(h);
    fr.v.hash(h);
    fr.width.hash(h);
    fr.height.hash(h);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: decode_bench <stream.264> [reps]");
    let reps: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(5);
    // A/B ARM PINNING. The E2 worker switch is read from the environment and
    // cached on first use, so it cannot be flipped inside one process. Accept
    // it as an ARGUMENT and set it here, before any decode touches the cache,
    // so a paired harness can invoke this binary DIRECTLY for both arms.
    // Wrapping the exe in `cmd /c set VAR=... && exe` instead makes the harness
    // time the wrapper: it dropped 4-9 of 9 samples and reported exactly
    // 1.000x (codec-measurement -- a sample the instrument failed to take is
    // not a tie).
    let mut max_frames: Option<usize> = None;
    let mut out_path: Option<String> = None;
    let mut fthreads: usize = 0;
    let mut hash_out = false;
    for a in &args {
        if let Some(v) = a.strip_prefix("mt=") {
            // 0 = inline (default, ffmpeg -threads 1 shape)
            // 1 = force edc_worker (old nested recon thread)
            // auto = pre-2026-08-11 content gate
            std::env::set_var("RS_H264_EDC_MT", v);
        }
        if let Some(v) = a.strip_prefix("edc=") {
            std::env::set_var("RS_H264_EDC", v);
        }
        if let Some(v) = a.strip_prefix("bound=") {
            std::env::set_var("RS_H264_EDC_BOUND", v);
        }
        if a == "double=1" {
            std::env::set_var("RS_H264_DOUBLE_RECON", "1");
        }
        if let Some(v) = a.strip_prefix("nores=") {
            std::env::set_var("RS_H264_NORES", v);
        }
        if let Some(v) = a.strip_prefix("fatslice=") {
            std::env::set_var("RS_H264_FAT_SLICE", v);
        }
        if let Some(v) = a.strip_prefix("slicepool=") {
            // slicepool=0 -> RS_H264_NO_SLICE_POOL=1 (old fresh-alloc path)
            if v == "0" { std::env::set_var("RS_H264_NO_SLICE_POOL", "1"); }
            else { std::env::remove_var("RS_H264_NO_SLICE_POOL"); }
        }
        if let Some(v) = a.strip_prefix("batch=") {
            std::env::set_var("RS_H264_BATCH", v);
        }
        // Frame-MT (campaign #1). 0/1 = serial; N>1 = worker pool.
        // Measure with bench/pinmt.ps1 (WALL+CPU, multi-core mask) — not ffmpeg_race.
        if let Some(v) = a.strip_prefix("fthreads=") {
            fthreads = v.parse().expect("fthreads=N");
            std::env::set_var("RS_H264_FRAME_THREADS", v);
        }
        if let Some(v) = a.strip_prefix("rowprog=") {
            // rowprog=1 = Phase B early-start; default (unset/0) = Phase A barrier.
            std::env::set_var("RS_H264_ROW_PROGRESS", v);
        }
        if let Some(v) = a.strip_prefix("rowpub=") {
            // rowpub=1 = incremental strip publish (experimental); default off.
            std::env::set_var("RS_H264_ROW_PUB", v);
        }
        if a == "rowhook=eager" {
            std::env::set_var("RS_H264_ROWHOOK_EAGER", "1");
        }
        if a == "dmemo=0" {
            std::env::set_var("RS_H264_DIRECT_MEMO", "0");
        }
        if a == "qpel=compose" {
            std::env::set_var("RS_H264_QPEL_COMPOSE", "1");
        }
        // Gate helpers (not on the timed race path): stop after N pictures, and/or
        // write I420 so a harness can SHA against ffmpeg without the CLI's
        // decode_stream Vec accumulate.
        if let Some(v) = a.strip_prefix("maxf=") {
            max_frames = Some(v.parse().expect("maxf=N"));
        }
        if let Some(v) = a.strip_prefix("out=") {
            out_path = Some(v.to_string());
        }
        if a == "hash=1" {
            hash_out = true;
        }
    }
    let input = std::fs::read(path).expect("read stream");

    let (mut best, mut worst) = (f64::MAX, 0f64);
    let (mut frames, mut px) = (0usize, 0usize);
    for rep in 0..reps {
        let mut dec = Decoder::new();
        let t = std::time::Instant::now();
        // Timed path (no out=): decode AU-by-AU and DROP pictures — same work as
        // ffmpeg -f null. Gate path (out=): Decoder::decode_stream so pictures are
        // in DISPLAY order (POC), matching ffmpeg. Dumping decode() order against
        // ffmpeg YUV falsely fails every B-frame stream (WHYS continuation D6-H5).
        //
        // Frame-MT (fthreads>1): whole-stream worker pool via decode_stream_threaded;
        // sink drops YUV on the timed path so long clips do not retain ~GB of frames.
        let (mut f, mut p) = (0usize, 0usize);
        let mut hasher = DefaultHasher::new();
        if hash_out && rep == 0 {
            let limit = max_frames.unwrap_or(usize::MAX);
            if fthreads > 1 {
                let _n = dec
                    .decode_stream_threaded_sink(&input, fthreads, |fr| {
                        if f < limit {
                            feed_hash(&mut hasher, &fr);
                            f += 1;
                            p += fr.width * fr.height;
                        }
                    })
                    .expect("frame-mt hash");
            } else {
                let frames = dec.decode_stream(&input).expect("decode_stream hash");
                for fr in frames.into_iter().take(limit) {
                    feed_hash(&mut hasher, &fr);
                    f += 1;
                    p += fr.width * fr.height;
                }
            }
            eprintln!("hash={:016x} frames={f}", hasher.finish());
        } else if fthreads > 1 {
            let limit = max_frames.unwrap_or(usize::MAX);
            if let Some(ref op) = out_path {
                if rep == 0 {
                    let mut out_buf: Vec<u8> = Vec::new();
                    let mut done = false;
                    let _n = dec
                        .decode_stream_threaded_sink(&input, fthreads, |fr| {
                            if done || f >= limit {
                                done = true;
                                return;
                            }
                            out_buf.extend_from_slice(&fr.y);
                            out_buf.extend_from_slice(&fr.u);
                            out_buf.extend_from_slice(&fr.v);
                            f += 1;
                            p += fr.width * fr.height;
                        })
                        .expect("decode_stream_threaded");
                    std::fs::write(op, &out_buf).expect("write out=");
                }
            } else {
                let _n = dec
                    .decode_stream_threaded_sink(&input, fthreads, |fr| {
                        if f < limit {
                            f += 1;
                            p += fr.width * fr.height;
                        }
                    })
                    .expect("frame-mt decode");
            }
        } else if out_path.is_some() && rep == 0 {
            // Display-order emit with early stop: buffer one GOP, flush on IDR,
            // stop once `maxf` pictures are written. Avoids decode_stream's
            // full-stream Vec (720p x 1800 ~= 1.7 GB) for a 30-frame probe.
            let limit = max_frames.unwrap_or(usize::MAX);
            let mut gop: Vec<(i32, YuvFrame)> = Vec::new();
            let mut out_buf: Vec<u8> = Vec::new();
            let mut done = false;
            for au in rusty_h264_decoder::split_access_units(&input) {
                if rusty_h264_decoder::au_is_idr(au) {
                    gop.sort_by_key(|pair| pair.0);
                    for (_, fr) in gop.drain(..) {
                        if f >= limit {
                            done = true;
                            break;
                        }
                        out_buf.extend_from_slice(&fr.y);
                        out_buf.extend_from_slice(&fr.u);
                        out_buf.extend_from_slice(&fr.v);
                        f += 1;
                        p += fr.width * fr.height;
                    }
                    if done {
                        break;
                    }
                }
                if let Some(frame) = dec.decode(au).expect("decode") {
                    gop.push((dec.last_poc(), frame));
                }
            }
            if !done {
                gop.sort_by_key(|pair| pair.0);
                for (_, fr) in gop.drain(..) {
                    if f >= limit {
                        break;
                    }
                    out_buf.extend_from_slice(&fr.y);
                    out_buf.extend_from_slice(&fr.u);
                    out_buf.extend_from_slice(&fr.v);
                    f += 1;
                    p += fr.width * fr.height;
                }
            }
            std::fs::write(out_path.as_ref().unwrap(), &out_buf).expect("write out=");
        } else {
            for au in rusty_h264_decoder::split_access_units(&input) {
                if let Some(fr) = dec.decode(au).expect("decode") {
                    f += 1;
                    p += fr.width * fr.height;
                    if max_frames.is_some_and(|m| f >= m) {
                        break;
                    }
                }
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
    // The E2 seam counters are deterministic and cost nothing when the env var
    // is unset, so they report on the DEFAULT build — unlike the profile pass
    // below, which decodes a second time.
    edc_stats_report();

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
