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
    let input = std::fs::read(req(&opts, "in")?).map_err(|e| format!("read input: {e}"))?;

    let mut cfg = EncoderConfig::new(width, height);
    cfg.qp = qp;
    cfg.gop_size = gop.max(1);
    cfg.bitrate = bitrate;
    cfg.framerate = fps;
    cfg.num_ref_frames = refs.clamp(1, 16);
    cfg.preset = preset;
    let enc = Encoder::new(cfg).map_err(|e| e.to_string())?;

    let frame_size = width * height * 3 / 2;
    if frame_size == 0 || input.len() % frame_size != 0 {
        return Err(format!(
            "input size {} is not a multiple of one I420 frame ({frame_size} bytes)",
            input.len()
        ));
    }

    // Decode all frames up front, then batch-encode — `encode_all` runs the GOPs in
    // parallel across cores (byte-identical to sequential at constant QP).
    let frames: Vec<YuvFrame> =
        input.chunks(frame_size).map(|c| frame_from_i420(c, width, height)).collect();
    let n = frames.len();
    let aus = enc.encode_all(&frames).map_err(|e| e.to_string())?;
    let out: Vec<u8> = aus.concat();
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("encoded {n} frame(s) -> {} bytes", out.len());
    Ok(())
}

fn cmd_decode(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let input = std::fs::read(req(&opts, "in")?).map_err(|e| format!("read input: {e}"))?;
    let mut dec = Decoder::new();
    // Split the Annex-B stream into access units (each begins with an SPS).
    let mut out = Vec::new();
    let mut frames = 0;
    for au in split_access_units(&input) {
        if let Some(frame) = dec.decode(au).map_err(|e| e.to_string())? {
            out.extend_from_slice(&frame.y);
            out.extend_from_slice(&frame.u);
            out.extend_from_slice(&frame.v);
            frames += 1;
        }
    }
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("decoded {frames} frame(s) -> {} bytes", out.len());
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

/// Splits a multi-picture Annex-B stream into access units, cutting after each
/// VCL (slice) NAL. Each picture is one slice plus any parameter sets that
/// precede it (SPS/PPS lead the IDR; P-pictures carry only their slice — the
/// decoder retains parameter-set state across calls).
fn split_access_units(stream: &[u8]) -> Vec<&[u8]> {
    use rusty_h264_common::nal::NalUnitType;
    // Offsets of all 4-byte start codes and the type of the NAL they begin.
    let mut nals: Vec<(usize, NalUnitType)> = Vec::new();
    let mut i = 0;
    while i + 4 <= stream.len() {
        if stream[i..i + 4] == [0, 0, 0, 1] {
            if let Some(&hdr) = stream.get(i + 4) {
                nals.push((i, NalUnitType::from_id(hdr)));
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    if nals.is_empty() {
        return vec![stream];
    }
    let is_slice =
        |t: NalUnitType| matches!(t, NalUnitType::IdrSlice | NalUnitType::NonIdrSlice);
    let mut aus = Vec::new();
    let mut start = nals[0].0;
    for (idx, &(_off, t)) in nals.iter().enumerate() {
        if is_slice(t) {
            let end = nals.get(idx + 1).map(|n| n.0).unwrap_or(stream.len());
            aus.push(&stream[start..end]);
            start = end;
        }
    }
    aus
}
