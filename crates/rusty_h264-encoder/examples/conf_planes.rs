//! The borrowed-frame gate as the chip plan wrote it, on real content: for each
//! conformance clip, `encode_all` over owned frames, per-frame `encode_planes`
//! over borrowed views and `encode_into` into a caller buffer must produce the
//! SAME BYTES — on `baseline()` and on the default configuration — and ffmpeg's
//! reconstruction of that stream must be pixel-identical to our decoder's (the
//! ffmpeg decode gate, unchanged by the new entry points).
//!
//!   cargo run --release -p rusty_h264-encoder --features asm \
//!     --example conf_planes -- video-tests/clips/*.y4m
use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{EncodeError, Encoder, EncoderConfig};
use std::process::Command;

fn read_y4m(path: &str, max: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap();
    let he = raw.iter().position(|&b| b == b'\n').unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for t in std::str::from_utf8(&raw[..he]).unwrap().split_whitespace() {
        match t.as_bytes().first() {
            Some(b'W') => w = t[1..].parse().unwrap(),
            Some(b'H') => h = t[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (mut f, mut p) = (Vec::new(), he + 1);
    while f.len() < max {
        let Some(r) = raw[p..].iter().position(|&b| b == b'\n') else {
            break;
        };
        p += r + 1;
        if p + ys + 2 * cs > raw.len() {
            break;
        }
        f.push(YuvFrame {
            width: w,
            height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    (w, h, f)
}

fn configs(w: usize, h: usize) -> Vec<(&'static str, EncoderConfig)> {
    let mut baseline = EncoderConfig::baseline(w, h);
    baseline.gop_size = 30;
    baseline.min_keyint = 30;
    let mut default = EncoderConfig::new(w, h);
    default.gop_size = 30;
    vec![("baseline", baseline), ("default", default)]
}

/// The reference: owned frames through `encode_all`.
fn owned(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<u8> {
    Encoder::new(cfg.clone())
        .unwrap()
        .encode_all(frames)
        .unwrap()
        .concat()
}

/// Borrowed views, one frame at a time, then the flush.
fn borrowed(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    let mut out = Vec::new();
    for f in frames {
        out.extend(enc.encode_planes(&f.as_planes()).unwrap());
    }
    out.extend(enc.flush());
    out
}

/// Borrowed views into a caller-owned buffer, then `flush_into`. The buffer is
/// sized so nothing is ever too small: a short buffer loses that picture by
/// design, which is a different test (`tests/chip_api.rs`).
fn caller_buffer(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    let (w, h) = (cfg.width, cfg.height);
    let mut buf = vec![0u8; frames.len() * w * h * 3 + (1 << 16)];
    let mut out = Vec::new();
    for f in frames {
        match enc.encode_into(&f.as_planes(), &mut buf) {
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(EncodeError::BufferTooSmall { needed }) => panic!("buffer sized wrong: {needed}"),
            Err(e) => panic!("{e:?}"),
        }
    }
    let n = enc.flush_into(&mut buf).unwrap();
    out.extend_from_slice(&buf[..n]);
    out
}

/// ffmpeg's reconstruction of `stream` vs ours: the number of differing samples,
/// or an error string when ffmpeg rejects the stream or the counts disagree.
fn ffmpeg_pixel_diffs(stream: &[u8], w: usize, h: usize, tag: &str) -> Result<usize, String> {
    let tmp = std::env::temp_dir();
    let f264 = tmp.join(format!("conf_planes_{tag}.264"));
    let fyuv = tmp.join(format!("conf_planes_{tag}.yuv"));
    let _ = std::fs::remove_file(&fyuv);
    std::fs::write(&f264, stream).unwrap();
    let o = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&f264)
        .args(["-pix_fmt", "yuv420p", "-f", "rawvideo"])
        .arg(&fyuv)
        .output()
        .unwrap();
    if !o.status.success() {
        return Err(format!(
            "ffmpeg REJECTED — {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    let ff = std::fs::read(&fyuv).unwrap();
    let ours = rusty_h264_decoder::Decoder::new()
        .decode_stream(stream)
        .unwrap();
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    if ff.len() / fsz != ours.len() {
        return Err(format!(
            "FRAME COUNT ffmpeg={} ours={}",
            ff.len() / fsz,
            ours.len()
        ));
    }
    let mut diff = 0usize;
    for (i, r) in ours.iter().enumerate() {
        let b = &ff[i * fsz..];
        diff += r.y.iter().zip(&b[..ys]).filter(|(a, c)| a != c).count()
            + r.u
                .iter()
                .zip(&b[ys..ys + cs])
                .filter(|(a, c)| a != c)
                .count()
            + r.v
                .iter()
                .zip(&b[ys + cs..ys + 2 * cs])
                .filter(|(a, c)| a != c)
                .count();
    }
    let _ = std::fs::remove_file(&f264);
    let _ = std::fs::remove_file(&fyuv);
    Ok(diff)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let frames_per_clip: usize = std::env::var("CONF_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let (mut pass, mut fail) = (0, 0);
    for path in &args {
        let name = std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let (w, h, frames) = read_y4m(path, frames_per_clip);
        for (cn, cfg) in configs(w, h) {
            let a = owned(&cfg, &frames);
            let b = borrowed(&cfg, &frames);
            let c = caller_buffer(&cfg, &frames);
            let mut ok = true;
            if a != b {
                println!(
                    "  {name}/{cn}: encode_planes DIFFERS from encode_all ({} vs {} bytes)",
                    b.len(),
                    a.len()
                );
                ok = false;
            }
            if a != c {
                println!(
                    "  {name}/{cn}: encode_into DIFFERS from encode_all ({} vs {} bytes)",
                    c.len(),
                    a.len()
                );
                ok = false;
            }
            match ffmpeg_pixel_diffs(&b, w, h, &format!("{name}_{cn}")) {
                Ok(0) => {}
                Ok(d) => {
                    println!("  {name}/{cn}: {d} PIXEL DIFFS vs ffmpeg");
                    ok = false;
                }
                Err(e) => {
                    println!("  {name}/{cn}: {e}");
                    ok = false;
                }
            }
            if ok {
                pass += 1;
                println!(
                    "  {name}/{cn}: identical x3 ({} bytes), ffmpeg pixel-exact",
                    a.len()
                );
            } else {
                fail += 1;
            }
        }
    }
    println!("\n=== borrowed-frame gate: {pass} PASS / {fail} FAIL ===");
    if fail > 0 {
        std::process::exit(1)
    }
}
