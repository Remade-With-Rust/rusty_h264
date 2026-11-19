// Dev/test target, not shipped: a panic here IS the diagnostic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Screen-content scroll probe: a synthetic text page scrolling N rows per
//! frame, coded with the zero-latency desktop configuration a remote-desktop
//! host uses (`EncoderConfig::baseline`, constant QP 26, no keyframe timer).
//! The page generator and the BGRA -> I420 (BT.601 limited) conversion copy a
//! downstream remote-desktop benchmark's workload, so the numbers compare with
//! the hardware encoders it was measured against.
//!
//!   cargo run --release -p rusty_h264-encoder --example scroll_probe -- \
//!       [WxH=1920x1080] [workload=scroll|window|static|typing|y4m=<clip>] \
//!       [frames=30] [rows=3] [out.h264]
//!
//! `window` scrolls only a viewport beside a static sidebar; `y4m=<clip>`
//! codes a real clip instead (WxH is then ignored). `SCROLL_PROBE_CFG=default`
//! swaps the desktop configuration for the crate default (CABAC, 3 refs) with
//! zero latency; `SCROLL_PROBE_FRAMES=1` prints every frame.
//!
//! Prints the IDR size, mean bytes per P-frame, encode time per P-frame (p50
//! and min of single `encode` calls, wall clock), and the mean absolute luma
//! error and Y-PSNR of rusty_h264's own decode against the source. With an
//! output path it writes the Annex-B stream and, as `<out>.dec.yuv`, our
//! decoder's pictures, for a byte comparison against an external decoder.

use std::time::Instant;

use rusty_h264_common::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig};

/// `window`: only the rectangle (x0, y0, x1, y1) scrolls; outside it the
/// page stays put (a browser viewport inside a static desktop).
fn page(
    w: usize,
    h: usize,
    scroll: usize,
    tick: usize,
    typing: bool,
    window: Option<(usize, usize, usize, usize)>,
) -> Vec<u8> {
    let mut v = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let inside =
                window.is_none_or(|(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1);
            let sy = if inside { y + scroll } else { y };
            let p = (y * w + x) * 4;
            let line = sy % 22;
            let glyph = (x / 9 + sy / 22 * 7) % 11;
            let ink = line > 4 && line < 18 && glyph < 7 && ((x * 31 + sy * 17) % 5 < 2);
            let c = if ink { 30 } else { 245 };
            v[p] = c;
            v[p + 1] = c;
            v[p + 2] = if ink { 30 } else { 250 };
            v[p + 3] = 255;
        }
    }
    if typing {
        for y in 100..140.min(h) {
            for x in 100..300.min(w) {
                let p = (y * w + x) * 4;
                let on = (x + y + tick * 7) % 13 < 4;
                let c = if on { 20 } else { 240 };
                v[p..p + 3].copy_from_slice(&[c, c, c]);
            }
        }
    }
    v
}

fn bgra_to_i420(src: &[u8], w: usize, h: usize) -> YuvFrame {
    let luma = |r: i32, g: i32, b: i32| (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
    let chroma = |r: i32, g: i32, b: i32| {
        let u = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        let v = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        (u, v)
    };
    let mut f = YuvFrame::black(w, h);
    let cw = w / 2;
    let px = |x: usize, y: usize| {
        let p = (y * w + x) * 4;
        (
            i32::from(src[p + 2]),
            i32::from(src[p + 1]),
            i32::from(src[p]),
        )
    };
    for cy in 0..h / 2 {
        for cx in 0..cw {
            let (mut rs, mut gs, mut bs) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (2 * cx + dx, 2 * cy + dy);
                let (r, g, b) = px(x, y);
                f.y[y * w + x] = luma(r, g, b);
                rs += r;
                gs += g;
                bs += b;
            }
            let (u, v) = chroma(rs / 4, gs / 4, bs / 4);
            f.u[cy * cw + cx] = u;
            f.v[cy * cw + cx] = v;
        }
    }
    f
}

/// The remote-desktop host's configuration (`SCROLL_PROBE_CFG` unset or
/// `desktop`: Constrained Baseline, constant QP 26, keyframes on request
/// only), or the crate default with zero-latency settings
/// (`default`: High, CABAC, three references, `Preset::Fast`, no mb-tree) — the
/// other configuration the fast-preset search serves.
fn probe_cfg(w: usize, h: usize) -> EncoderConfig {
    let mut cfg = if std::env::var("SCROLL_PROBE_CFG").as_deref() == Ok("default") {
        let mut c = EncoderConfig::new(w, h);
        c.mbtree = false;
        c.lookahead = 0;
        c.scenecut = 0;
        c
    } else {
        EncoderConfig::baseline(w, h)
    };
    cfg.qp = 26;
    cfg.bitrate = 0;
    cfg.framerate = 30.0;
    cfg.gop_size = u32::MAX / 2;
    cfg.min_keyint = 1;
    cfg.level_idc = if w * h > 1280 * 720 { 41 } else { 31 };
    cfg
}

/// Frames of a 4:2:0 `.y4m` clip (the `y4m=<path>` workload).
fn read_y4m(path: &str, max: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap();
    let hdr_end = raw.iter().position(|&b| b == b'\n').unwrap();
    let hdr = std::str::from_utf8(&raw[..hdr_end]).unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().unwrap(),
            Some(b'H') => h = tok[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let mut frames = Vec::new();
    let mut p = hdr_end + 1;
    while frames.len() < max {
        let Some(rel) = raw[p..].iter().position(|&b| b == b'\n') else {
            break;
        };
        p += rel + 1;
        if p + ys + 2 * cs > raw.len() {
            break;
        }
        frames.push(YuvFrame {
            width: w,
            height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    frames
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (w, h) = args
        .first()
        .and_then(|s| s.split_once('x'))
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .unwrap_or((1920usize, 1080usize));
    let workload = args.get(1).map(String::as_str).unwrap_or("scroll");
    let n: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .max(2);
    let rows: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let out = args.get(4);

    let frames: Vec<YuvFrame> = if let Some(path) = workload.strip_prefix("y4m=") {
        read_y4m(path, n)
    } else {
        (0..n)
            .map(|i| {
                let (scroll, typing) = match workload {
                    "static" => (0, false),
                    "typing" => (0, true),
                    _ => (i * rows, false),
                };
                // `window`: a 60%-wide, 70%-tall viewport right of a static sidebar.
                let window =
                    (workload == "window").then_some((w * 3 / 10, h / 10, w * 9 / 10, h * 8 / 10));
                bgra_to_i420(&page(w, h, scroll, i, typing, window), w, h)
            })
            .collect()
    };
    let (w, h, n) = (frames[0].width, frames[0].height, frames.len());

    let mut enc = Encoder::new(probe_cfg(w, h)).unwrap();
    let mut stream = Vec::new();
    let (mut pbytes, mut idr) = (0usize, 0usize);
    let mut times = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let t0 = Instant::now();
        let au = enc.encode(f);
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if std::env::var_os("SCROLL_PROBE_FRAMES").is_some() {
            println!("  frame {i}: {} B, {ms:.2} ms", au.len());
        }
        if i == 0 {
            idr = au.len();
        } else {
            pbytes += au.len();
            times.push(ms);
        }
        stream.extend_from_slice(&au);
    }
    stream.extend(enc.flush());
    let pics = rusty_h264_decoder::Decoder::new()
        .decode_stream(&stream)
        .unwrap();
    assert_eq!(pics.len(), n, "one decoded picture per frame");
    let (mut err, mut sse) = (0.0f64, 0u64);
    for (pic, f) in pics.iter().zip(&frames) {
        let (mut s, mut q) = (0u64, 0u64);
        for (a, b) in pic.y[..w * h].iter().zip(&f.y) {
            let d = u64::from(a.abs_diff(*b));
            s += d;
            q += d * d;
        }
        err += s as f64 / (w * h) as f64;
        sse += q;
    }
    let psnr = 10.0 * (255.0f64 * 255.0 * (w * h * n) as f64 / sse.max(1) as f64).log10();
    times.sort_by(f64::total_cmp);
    println!(
        "{w}x{h} {workload} rows={rows} frames={n}: IDR {idr} B | P mean {:.0} B ({:.2}% of IDR) | \
         P ms p50 {:.2} min {:.2} | luma err {:.3} | Y-PSNR {psnr:.3} dB",
        pbytes as f64 / (n - 1) as f64,
        100.0 * pbytes as f64 / (n - 1) as f64 / idr as f64,
        times[times.len() / 2],
        times[0],
        err / n as f64
    );
    if let Some(path) = out {
        std::fs::write(path, &stream).unwrap();
        let mut dec = Vec::new();
        for p in &pics {
            dec.extend_from_slice(&p.y[..w * h]);
            dec.extend_from_slice(&p.u[..(w / 2) * (h / 2)]);
            dec.extend_from_slice(&p.v[..(w / 2) * (h / 2)]);
        }
        std::fs::write(format!("{path}.dec.yuv"), dec).unwrap();
    }
}
