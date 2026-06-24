//! Deterministic A/B benchmark: **rusty_h264** (pure Rust) vs **Cisco openh264**.
//!
//! Same synthetic clip, same parameters, single-threaded. Output size and PSNR
//! are exactly reproducible run-to-run; encode time is reported as the median
//! of `--runs` repetitions (the one inherently noisy metric). The Cisco side is
//! compiled in only with `--features cisco`.
//!
//! ```text
//! cargo run --release -- --width 352 --height 288 --frames 30 --qp 26 --runs 5
//! cargo run --release --features cisco -- --width 352 --height 288 --frames 30
//! ```

mod clip;
mod metrics;
mod reference;

use clip::ClipSpec;
use metrics::{avg_psnr, fmt_psnr, FramePsnr};
use rusty_h264::{Decoder, Encoder, EncoderConfig};
use std::time::{Duration, Instant};

/// One encoder's result on the clip.
pub struct Report {
    pub name: &'static str,
    pub total_bytes: usize,
    pub frames: usize,
    pub median_encode: Duration,
    /// Average Y-PSNR vs source; `None` = lossless.
    pub avg_psnr_y: Option<f64>,
    /// `true` if a real reference decoder accepted the stream.
    pub decoded_ok: bool,
}

impl Report {
    fn pixels_per_sec(&self, spec: &ClipSpec) -> f64 {
        let px = (spec.width * spec.height * self.frames) as f64;
        px / self.median_encode.as_secs_f64()
    }

    fn bits_per_pixel(&self, spec: &ClipSpec) -> f64 {
        let px = (spec.width * spec.height * self.frames) as f64;
        (self.total_bytes * 8) as f64 / px
    }
}

struct Args {
    width: usize,
    height: usize,
    frames: usize,
    qp: u8,
    runs: usize,
    /// Frames between IDR pictures. `1` = all-intra; `>1` enables P-frames.
    gop: u32,
    /// Optional external ffmpeg path for the C baseline.
    ffmpeg: Option<String>,
    /// Reference codec: `libopenh264` (Cisco) or `libx264`.
    ref_codec: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        width: 352,
        height: 288,
        frames: 30,
        qp: 26,
        runs: 5,
        gop: 1,
        ffmpeg: None,
        ref_codec: "libx264".to_string(),
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let key = argv[i].clone();
        let mut take = || {
            i += 1;
            argv.get(i).expect("missing value").clone()
        };
        match key.as_str() {
            "--width" => a.width = take().parse().expect("width"),
            "--height" => a.height = take().parse().expect("height"),
            "--frames" => a.frames = take().parse().expect("frames"),
            "--qp" => a.qp = take().parse().expect("qp"),
            "--gop" => a.gop = take().parse().expect("gop"),
            "--runs" => a.runs = take().parse().expect("runs"),
            "--ffmpeg" => a.ffmpeg = Some(take()),
            "--ref-codec" => a.ref_codec = take(),
            other => panic!("unknown flag {other}"),
        }
        i += 1;
    }
    a
}

fn main() {
    let args = parse_args();
    let spec = ClipSpec::new(args.width, args.height, args.frames);
    let frames = clip::all_frames(&spec);

    println!("rusty_h264 benchmark — deterministic A/B");
    println!(
        "clip: {}x{}, {} frames, qp {}, gop {} ({}), {} timing run(s)\n",
        spec.width,
        spec.height,
        spec.frames,
        args.qp,
        args.gop,
        if args.gop <= 1 { "all-intra" } else { "I+P" },
        args.runs
    );

    let mut reports = vec![run_rusty_h264(&args, &spec, &frames)];

    // C baseline: external ffmpeg process, never built by us.
    match reference::locate(args.ffmpeg.as_deref()) {
        Some(ffmpeg) => {
            let codec = reference::RefCodec::from_name(&args.ref_codec);
            match reference::run(&ffmpeg, &codec, &spec, &frames, args.qp, args.gop, args.runs) {
                Ok(report) => reports.push(report),
                Err(e) => eprintln!("C baseline skipped: {e}"),
            }
        }
        None => eprintln!(
            "C baseline skipped: pass --ffmpeg <path> or set RUSTY_H264_BENCH_FFMPEG.\n\
             It runs as an external process; no C/C++ is ever built into rusty_h264."
        ),
    }

    print_table(&spec, &reports);
}

/// Runs the pure-Rust encoder and validates output with our own decoder.
fn run_rusty_h264(args: &Args, spec: &ClipSpec, frames: &[rusty_h264::YuvFrame]) -> Report {
    let mut cfg = EncoderConfig::new(spec.width, spec.height);
    cfg.qp = args.qp;
    cfg.gop_size = args.gop.max(1);

    // Median encode time over `runs`.
    let mut durations = Vec::new();
    let mut last_aus: Vec<Vec<u8>> = Vec::new();
    for _ in 0..args.runs.max(1) {
        let mut enc = Encoder::new(cfg.clone()).expect("config");
        let mut aus = Vec::with_capacity(frames.len());
        let t = Instant::now();
        for f in frames {
            aus.push(enc.encode(f));
        }
        durations.push(t.elapsed());
        last_aus = aus;
    }
    durations.sort();
    let median_encode = durations[durations.len() / 2];

    let total_bytes = last_aus.iter().map(Vec::len).sum();

    // Validate + measure quality with our decoder.
    let mut dec = Decoder::new();
    let mut psnrs = Vec::new();
    let mut decoded_ok = true;
    for (au, src) in last_aus.iter().zip(frames) {
        match dec.decode(au) {
            Ok(Some(recon)) => psnrs.push(FramePsnr::compute(src, &recon).y),
            _ => decoded_ok = false,
        }
    }

    Report {
        name: "rusty_h264 (Rust)",
        total_bytes,
        frames: frames.len(),
        median_encode,
        avg_psnr_y: avg_psnr(&psnrs),
        decoded_ok,
    }
}

fn print_table(spec: &ClipSpec, reports: &[Report]) {
    println!(
        "{:<18} {:>12} {:>10} {:>12} {:>14} {:>8}",
        "encoder", "bytes", "bits/px", "encode (ms)", "Mpx/s", "decoded"
    );
    println!("{}", "-".repeat(78));
    for r in reports {
        println!(
            "{:<18} {:>12} {:>10.3} {:>12.2} {:>14.2} {:>8}",
            r.name,
            r.total_bytes,
            r.bits_per_pixel(spec),
            r.median_encode.as_secs_f64() * 1000.0,
            r.pixels_per_sec(spec) / 1.0e6,
            if r.decoded_ok { "yes" } else { "NO" },
        );
    }
    println!();
    for r in reports {
        println!("  {} — avg Y-PSNR: {}", r.name, fmt_psnr(r.avg_psnr_y));
    }

    // Head-to-head deltas when both sides are present.
    if reports.len() == 2 {
        let (a, b) = (&reports[0], &reports[1]);
        let size_ratio = a.total_bytes as f64 / b.total_bytes as f64;
        let speed_ratio = b.median_encode.as_secs_f64() / a.median_encode.as_secs_f64();
        println!(
            "\nhead-to-head ({} vs {}):",
            a.name, b.name
        );
        println!("  size:  {:.2}x  (lower is better for {})", size_ratio, a.name);
        println!("  speed: {:.2}x  (higher is better for {})", speed_ratio, a.name);
    }
}
