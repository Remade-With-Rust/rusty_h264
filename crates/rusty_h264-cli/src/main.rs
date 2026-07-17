//! `rusty_h264` command-line tool — encode raw YUV420p to an Annex-B `.264`
//! stream and decode it back. Mirrors openh264's `codec/console` apps.
//!
//! Usage:
//!   rusty_h264 encode --width W --height H [--qp N] --in in.yuv --out out.264
//!   rusty_h264 decode --width W --height H --in in.264 --out out.yuv

use rusty_h264::{Decoder, Encoder, EncoderConfig, Preset, YuvFrame};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("encode") => cmd_encode(&args[1..]),
        Some("decode") => cmd_decode(&args[1..]),
        Some("--help") | Some("-h") | None => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command: {other}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "rusty_h264 — pure-Rust H.264 codec\n\n\
         USAGE:\n  \
         rusty_h264 encode --width W --height H [--qp N] [--gop N] [--bitrate BPS --fps F] --in in.yuv --out out.264\n  \
         rusty_h264 decode --width W --height H --in in.264 --out out.yuv\n\n\
         Input/output YUV is raw planar 4:2:0 (I420), one frame after another."
    );
}

/// Minimal `--key value` parser.
fn parse_opts(args: &[String]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut map = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i]
            .strip_prefix("--")
            .ok_or_else(|| format!("expected --flag, got {}", args[i]))?;
        let val = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for --{key}"))?;
        map.insert(key.to_string(), val.clone());
        i += 2;
    }
    Ok(map)
}

fn req<'a>(
    opts: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Result<&'a String, String> {
    opts.get(key).ok_or_else(|| format!("missing --{key}"))
}

fn cmd_encode(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let width: usize = req(&opts, "width")?.parse().map_err(|_| "bad --width")?;
    let height: usize = req(&opts, "height")?.parse().map_err(|_| "bad --height")?;
    let qp: u8 = opts.get("qp").map_or(Ok(26), |s| s.parse()).map_err(|_| "bad --qp")?;
    let gop: u32 = opts.get("gop").map_or(Ok(1), |s| s.parse()).map_err(|_| "bad --gop")?;
    let bitrate: u32 = opts.get("bitrate").map_or(Ok(0), |s| s.parse()).map_err(|_| "bad --bitrate")?;
    let fps: f32 = opts.get("fps").map_or(Ok(30.0), |s| s.parse()).map_err(|_| "bad --fps")?;
    let refs: u32 = opts.get("refs").map_or(Ok(1), |s| s.parse()).map_err(|_| "bad --refs")?;
    let preset = match opts.get("preset").map(String::as_str) {
        None | Some("fast") => Preset::Fast,
        Some("quality") | Some("slow") => Preset::Quality,
        Some(o) => return Err(format!("bad --preset {o} (fast|quality)")),
    };
    let mut cfg = EncoderConfig::new(width, height);
    cfg.qp = qp;
    cfg.gop_size = gop.max(1);
    cfg.bitrate = bitrate;
    cfg.framerate = fps;
    cfg.num_ref_frames = refs.clamp(1, 16);
    cfg.preset = preset;

    let frame_size = width * height * 3 / 2;
    let in_path = req(&opts, "in")?;
    let file_len = std::fs::metadata(in_path).map_err(|e| format!("stat input: {e}"))? .len() as usize;
    if frame_size == 0 || file_len % frame_size != 0 {
        return Err(format!(
            "input size {file_len} is not a multiple of one I420 frame ({frame_size} bytes)"
        ));
    }
    let n = file_len / frame_size;

    // Streaming GOP pipeline (constant QP): read each GOP's frames off the file and
    // hand them to a worker thread immediately, overlapping I/O with encoding — the
    // same per-GOP fresh-Encoder scheme as `encode_all`, so the output is
    // byte-identical to it (and to sequential encoding). Rate control threads QP
    // state across frames, so that path stays sequential via `encode_all`.
    let single = std::env::var("RUSTY_THREADS").ok().as_deref() == Some("1");
    let out: Vec<u8> = if bitrate == 0 && !single {
        use std::io::Read;
        let mut file = std::io::BufReader::with_capacity(
            1 << 20,
            std::fs::File::open(in_path).map_err(|e| format!("open input: {e}"))?,
        );
        let gop_len = cfg.gop_size.max(1) as usize;
        let n_gops = n.div_ceil(gop_len);
        let mut parts: Vec<Option<Vec<u8>>> = (0..n_gops).map(|_| None).collect();
        let cfg_ref = &cfg;
        std::thread::scope(|sc| -> Result<(), String> {
            let mut handles = Vec::new();
            for g in 0..n_gops {
                let frames_in_gop = gop_len.min(n - g * gop_len);
                let mut chunk = vec![0u8; frames_in_gop * frame_size];
                file.read_exact(&mut chunk).map_err(|e| format!("read input: {e}"))?;
                handles.push(sc.spawn(move || -> Result<Vec<u8>, String> {
                    let frames: Vec<YuvFrame> = chunk
                        .chunks(frame_size)
                        .map(|c| frame_from_i420(c, width, height))
                        .collect();
                    drop(chunk);
                    let mut enc = Encoder::new(cfg_ref.clone()).map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    for f in &frames {
                        bytes.extend_from_slice(&enc.encode(f));
                    }
                    Ok(bytes)
                }));
            }
            for (g, h) in handles.into_iter().enumerate() {
                parts[g] = Some(h.join().map_err(|_| "encoder thread panicked".to_string())??);
            }
            Ok(())
        })?;
        parts.into_iter().flatten().flatten().collect()
    } else {
        let input = std::fs::read(in_path).map_err(|e| format!("read input: {e}"))?;
        let frames: Vec<YuvFrame> =
            input.chunks(frame_size).map(|c| frame_from_i420(c, width, height)).collect();
        let enc = Encoder::new(cfg).map_err(|e| e.to_string())?;
        enc.encode_all(&frames).map_err(|e| e.to_string())?.concat()
    };
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("encoded {n} frame(s) -> {} bytes", out.len());
    Ok(())
}

fn cmd_decode(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let input = std::fs::read(req(&opts, "in")?).map_err(|e| format!("read input: {e}"))?;
    let mut dec = Decoder::new();
    // One call: access-unit split, multi-slice assembly, and display-order output.
    let frames = dec.decode_stream(&input).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for f in &frames {
        out.extend_from_slice(&f.y);
        out.extend_from_slice(&f.u);
        out.extend_from_slice(&f.v);
    }
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("decoded {} frame(s) -> {} bytes", frames.len(), out.len());
    Ok(())
}

fn frame_from_i420(buf: &[u8], width: usize, height: usize) -> YuvFrame {
    let ys = width * height;
    let cs = (width / 2) * (height / 2);
    YuvFrame {
        width,
        height,
        y: buf[..ys].to_vec(),
        u: buf[ys..ys + cs].to_vec(),
        v: buf[ys + cs..ys + 2 * cs].to_vec(),
    }
}

