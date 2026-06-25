//! External Cisco openh264 baseline — driven as a separate process.
//!
//! IMPORTANT: this is *only* a measurement baseline for the headline A/B table.
//! Cisco's codec is never linked into, bound to, or built by rusty_h264. We spawn a
//! separately-installed `ffmpeg` (built with `libopenh264`) on the exact same
//! clip, then read back its output. Our build stays 100% pure Rust.
//!
//! Enable by pointing the harness at an ffmpeg binary:
//!   --ffmpeg /path/to/ffmpeg     (or env RUSTY_H264_BENCH_FFMPEG)

use crate::clip::ClipSpec;
use crate::Report;
use rusty_h264::YuvFrame;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Locates the ffmpeg baseline from a CLI override or environment.
pub fn locate(cli_path: Option<&str>) -> Option<PathBuf> {
    cli_path
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("RUSTY_H264_BENCH_FFMPEG").map(PathBuf::from))
}

/// Writes the clip as a raw I420 file ffmpeg can consume.
fn write_i420(path: &std::path::Path, frames: &[YuvFrame]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for fr in frames {
        f.write_all(&fr.y)?;
        f.write_all(&fr.u)?;
        f.write_all(&fr.v)?;
    }
    f.flush()
}

/// Reads a raw I420 file back into frames.
fn read_i420(path: &std::path::Path, spec: &ClipSpec) -> std::io::Result<Vec<YuvFrame>> {
    let buf = std::fs::read(path)?;
    let fsz = spec.frame_bytes();
    let (w, h) = (spec.width, spec.height);
    let cs = (w / 2) * (h / 2);
    let mut out = Vec::new();
    for chunk in buf.chunks(fsz) {
        if chunk.len() < fsz {
            break;
        }
        out.push(YuvFrame {
            width: w,
            height: h,
            y: chunk[..w * h].to_vec(),
            u: chunk[w * h..w * h + cs].to_vec(),
            v: chunk[w * h + cs..w * h + 2 * cs].to_vec(),
        });
    }
    Ok(out)
}

/// The external reference H.264 encoder to race against.
pub struct RefCodec {
    /// ffmpeg `-c:v` value, e.g. `libopenh264` or `libx264`.
    pub ffmpeg_codec: &'static str,
    /// Human-readable report label.
    pub name: &'static str,
}

impl RefCodec {
    /// Cisco's openh264 (the rebuild target) — only present in a libopenh264-
    /// enabled ffmpeg.
    pub fn openh264() -> Self {
        Self {
            ffmpeg_codec: "libopenh264",
            name: "openh264 (Cisco/C)",
        }
    }
    /// x264 — the other dominant C H.264 encoder, in virtually every ffmpeg.
    pub fn x264() -> Self {
        Self {
            ffmpeg_codec: "libx264",
            name: "x264 (C)",
        }
    }
    pub fn from_name(s: &str) -> Self {
        match s {
            "libopenh264" | "openh264" => Self::openh264(),
            _ => Self::x264(),
        }
    }
}

/// Runs the external C baseline: encode the clip at the same QP, GOP and
/// reference count as rusty_h264 (baseline profile), measure size and median
/// encode time, then decode it back via ffmpeg to measure PSNR against the source.
///
/// Apples-to-apples: same clip, same QP, same GOP, same reference count, and PSNR
/// is measured by decoding through the same ffmpeg for both encoders.
#[allow(clippy::too_many_arguments)]
pub fn run(
    ffmpeg: &std::path::Path,
    codec: &RefCodec,
    spec: &ClipSpec,
    frames: &[YuvFrame],
    qp: u8,
    gop: u32,
    refs: u32,
    runs: usize,
) -> std::io::Result<Report> {
    let tmp = std::env::temp_dir();
    let src_yuv = tmp.join("rusty_h264_bench_src.yuv");
    let enc_264 = tmp.join("rusty_h264_bench_ref.264");
    let dec_yuv = tmp.join("rusty_h264_bench_ref_dec.yuv");
    write_i420(&src_yuv, frames)?;

    let size_arg = format!("{}x{}", spec.width, spec.height);
    let qp_arg = qp.to_string();
    let gop_arg = gop.max(1).to_string();
    let refs_arg = refs.max(1).to_string();

    // Encode: raw I420 -> H.264, intra-only at the matched QP. Median over `runs`.
    let mut durations = Vec::new();
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        let status = Command::new(ffmpeg)
            .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .args(["-s", &size_arg, "-i"])
            .arg(&src_yuv)
            .args(["-c:v", codec.ffmpeg_codec, "-qp", &qp_arg, "-g", &gop_arg])
            .args(["-refs", &refs_arg])
            .args(["-profile:v", "baseline", "-f", "h264"])
            .arg(&enc_264)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "ffmpeg encode failed (is {} built in?)",
                codec.ffmpeg_codec
            )));
        }
        durations.push(t.elapsed());
    }
    durations.sort();
    let median_encode = durations[durations.len() / 2];
    let total_bytes = std::fs::metadata(&enc_264)?.len() as usize;

    // Decode the Cisco stream back to YUV for PSNR.
    let dec_status = Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&enc_264)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&dec_yuv)
        .status()?;
    let (avg_psnr_y, decoded_ok) = if dec_status.success() {
        let recon = read_i420(&dec_yuv, spec).unwrap_or_default();
        let ys: Vec<Option<f64>> = frames
            .iter()
            .zip(&recon)
            .map(|(s, r)| crate::metrics::FramePsnr::compute(s, r).y)
            .collect();
        (crate::metrics::avg_psnr(&ys), !recon.is_empty())
    } else {
        (None, false)
    };

    Ok(Report {
        name: codec.name,
        total_bytes,
        frames: frames.len(),
        median_encode,
        avg_psnr_y,
        decoded_ok,
    })
}

/// Convenience: a zero-cost placeholder duration for reporting when unavailable.
#[allow(dead_code)]
pub const UNAVAILABLE: Duration = Duration::ZERO;
