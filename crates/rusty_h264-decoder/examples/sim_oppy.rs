//! HEADFUL BIG-OPPY SIM — ONE source video through TWO ARMS, side by side:
//!
//! * LEFT:  our encoder → our decoder                     (the whole codec)
//! * RIGHT: x264 (via ffmpeg, matched settings) → OUR decoder, cross-checked
//!          pixel-for-pixel against ffmpeg's own decode   (the reference)
//!
//! Same content, same QP — every number on screen is a direct comparison.
//! The default view is a SCOREBOARD: per-arm live rows (picture type, AU
//! bytes, decode ms, PSNR, conformance), two sparklines per arm sweeping a
//! playhead, and a full-width live GAP block (bytes gap, PSNR at matched QP,
//! our decoder's cost on each stream). The big-oppy docs' standing tables
//! live on a second view — press T — shown once, large, not duplicated.
//!
//! ```text
//! cargo run --release -p rusty_h264-decoder --features asm --example sim_oppy
//! cargo run --release -p rusty_h264-decoder --features asm --example sim_oppy -- clip.y4m [max=N] [qp=N] [maxw=N] [reps=N] [nowin]
//! ```
//!
//! Keys: SPACE pause/run · T live/docs view · R restart · Q/ESC quit.
//! `nowin` prints the end-of-clip tables and exits non-zero on any
//! conformance mismatch (CI-shaped).
//!
//! TIMING CONTRACT (same as `sim_sxs`): per-frame decode times are in-process
//! wall time on an unpinned box — INDICATIVE, best-of-N per AU, and honest as
//! a LEFT-vs-RIGHT comparison because both streams are decoded by the same
//! decoder in the same process. Cross-PROCESS wall ratios against ffmpeg are
//! not quoted at demo clip lengths; the standing pinned figures are printed
//! on the docs view instead.

use minifb::{Key, Window, WindowOptions};
use rusty_h264_common::types::YuvFrame;
use rusty_h264_decoder::{ContentRoute, Decoder};
use rusty_h264_encoder::{Encoder, EncoderConfig};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

// ---------------------------------------------------------------- 5x7 font --
// Column-major, LSB = top row. Uppercase only; lowercase folds up.
const GLYPHS: &[(char, [u8; 5])] = &[
    (' ', [0x00, 0x00, 0x00, 0x00, 0x00]),
    ('!', [0x00, 0x00, 0x5F, 0x00, 0x00]),
    ('#', [0x14, 0x7F, 0x14, 0x7F, 0x14]),
    ('%', [0x23, 0x13, 0x08, 0x64, 0x62]),
    ('(', [0x00, 0x1C, 0x22, 0x41, 0x00]),
    (')', [0x00, 0x41, 0x22, 0x1C, 0x00]),
    ('*', [0x14, 0x08, 0x3E, 0x08, 0x14]),
    ('+', [0x08, 0x08, 0x3E, 0x08, 0x08]),
    (',', [0x00, 0x56, 0x36, 0x00, 0x00]),
    ('-', [0x08, 0x08, 0x08, 0x08, 0x08]),
    ('.', [0x00, 0x60, 0x60, 0x00, 0x00]),
    ('/', [0x20, 0x10, 0x08, 0x04, 0x02]),
    ('0', [0x3E, 0x51, 0x49, 0x45, 0x3E]),
    ('1', [0x00, 0x42, 0x7F, 0x40, 0x00]),
    ('2', [0x42, 0x61, 0x51, 0x49, 0x46]),
    ('3', [0x21, 0x41, 0x45, 0x4B, 0x31]),
    ('4', [0x18, 0x14, 0x12, 0x7F, 0x10]),
    ('5', [0x27, 0x45, 0x45, 0x45, 0x39]),
    ('6', [0x3C, 0x4A, 0x49, 0x49, 0x30]),
    ('7', [0x01, 0x71, 0x09, 0x05, 0x03]),
    ('8', [0x36, 0x49, 0x49, 0x49, 0x36]),
    ('9', [0x06, 0x49, 0x49, 0x29, 0x1E]),
    (':', [0x00, 0x36, 0x36, 0x00, 0x00]),
    ('<', [0x08, 0x14, 0x22, 0x41, 0x00]),
    ('=', [0x14, 0x14, 0x14, 0x14, 0x14]),
    ('>', [0x00, 0x41, 0x22, 0x14, 0x08]),
    ('[', [0x00, 0x7F, 0x41, 0x41, 0x00]),
    (']', [0x00, 0x41, 0x41, 0x7F, 0x00]),
    ('|', [0x00, 0x00, 0x7F, 0x00, 0x00]),
    ('A', [0x7E, 0x11, 0x11, 0x11, 0x7E]),
    ('B', [0x7F, 0x49, 0x49, 0x49, 0x36]),
    ('C', [0x3E, 0x41, 0x41, 0x41, 0x22]),
    ('D', [0x7F, 0x41, 0x41, 0x22, 0x1C]),
    ('E', [0x7F, 0x49, 0x49, 0x49, 0x41]),
    ('F', [0x7F, 0x09, 0x09, 0x09, 0x01]),
    ('G', [0x3E, 0x41, 0x49, 0x49, 0x7A]),
    ('H', [0x7F, 0x08, 0x08, 0x08, 0x7F]),
    ('I', [0x00, 0x41, 0x7F, 0x41, 0x00]),
    ('J', [0x20, 0x40, 0x41, 0x3F, 0x01]),
    ('K', [0x7F, 0x08, 0x14, 0x22, 0x41]),
    ('L', [0x7F, 0x40, 0x40, 0x40, 0x40]),
    ('M', [0x7F, 0x02, 0x0C, 0x02, 0x7F]),
    ('N', [0x7F, 0x04, 0x08, 0x10, 0x7F]),
    ('O', [0x3E, 0x41, 0x41, 0x41, 0x3E]),
    ('P', [0x7F, 0x09, 0x09, 0x09, 0x06]),
    ('Q', [0x3E, 0x41, 0x51, 0x21, 0x5E]),
    ('R', [0x7F, 0x09, 0x19, 0x29, 0x46]),
    ('S', [0x46, 0x49, 0x49, 0x49, 0x31]),
    ('T', [0x01, 0x01, 0x7F, 0x01, 0x01]),
    ('U', [0x3F, 0x40, 0x40, 0x40, 0x3F]),
    ('V', [0x1F, 0x20, 0x40, 0x20, 0x1F]),
    ('W', [0x3F, 0x40, 0x38, 0x40, 0x3F]),
    ('X', [0x63, 0x14, 0x08, 0x14, 0x63]),
    ('Y', [0x07, 0x08, 0x70, 0x08, 0x07]),
    ('Z', [0x61, 0x51, 0x49, 0x45, 0x43]),
];

fn glyph(c: char) -> [u8; 5] {
    let c = c.to_ascii_uppercase();
    GLYPHS.iter().find(|g| g.0 == c).map(|g| g.1).unwrap_or([0x7F; 5])
}

fn text(buf: &mut [u32], w: usize, h: usize, x: usize, y: usize, s: &str, col: u32, sc: usize) {
    let mut cx = x;
    for ch in s.chars() {
        for (dx, colmask) in glyph(ch).iter().enumerate() {
            for dy in 0..7 {
                if colmask & (1 << dy) == 0 {
                    continue;
                }
                for ry in 0..sc {
                    for rx in 0..sc {
                        let (px, py) = (cx + dx * sc + rx, y + dy * sc + ry);
                        if px < w && py < h {
                            buf[py * w + px] = col;
                        }
                    }
                }
            }
        }
        cx += 6 * sc;
    }
}

fn hline(buf: &mut [u32], dw: usize, dh: usize, x: usize, y: usize, w: usize, col: u32) {
    if y >= dh {
        return;
    }
    for px in x..(x + w).min(dw) {
        buf[y * dw + px] = col;
    }
}

// ------------------------------------------------------------- YUV -> 0RGB --
/// Blit with integer ZOOM (pixel replication) so small sources fill their
/// column instead of postage-stamping.
fn yuv_to_rgb(fr: &YuvFrame, dst: &mut [u32], dw: usize, dh: usize, ox: usize, oy: usize, zoom: usize) {
    let (w, h) = (fr.width, fr.height);
    let cw = w.div_ceil(2);
    for py in 0..h {
        for px in 0..w {
            let y = fr.y[py * w + px] as i32;
            let ci = (py / 2) * cw + px / 2;
            let (u, v) = (fr.u[ci] as i32 - 128, fr.v[ci] as i32 - 128);
            let c = (y - 16) * 298;
            let r = ((c + 409 * v + 128) >> 8).clamp(0, 255) as u32;
            let g = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u32;
            let b = ((c + 516 * u + 128) >> 8).clamp(0, 255) as u32;
            let rgb = (r << 16) | (g << 8) | b;
            for zy in 0..zoom {
                let ty = oy + py * zoom + zy;
                if ty >= dh {
                    break;
                }
                for zx in 0..zoom {
                    let tx = ox + px * zoom + zx;
                    if tx < dw {
                        dst[ty * dw + tx] = rgb;
                    }
                }
            }
        }
    }
}

/// Nearest-neighbour shrink to panel size (frames are STORED at panel size).
fn shrink(fr: &YuvFrame, step: usize) -> YuvFrame {
    if step <= 1 {
        return fr.clone();
    }
    let (w, h) = (fr.width, fr.height);
    let (sw, sh) = ((w / step).max(1), (h / step).max(1));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (scw, sch) = (sw.div_ceil(2), sh.div_ceil(2));
    let mut y = vec![0u8; sw * sh];
    for r in 0..sh {
        for c in 0..sw {
            y[r * sw + c] = fr.y[(r * step) * w + c * step];
        }
    }
    let mut u = vec![0u8; scw * sch];
    let mut v = vec![0u8; scw * sch];
    for r in 0..sch {
        let sr = (r * step).min(ch - 1);
        for c in 0..scw {
            let sc = (c * step).min(cw - 1);
            u[r * scw + c] = fr.u[sr * cw + sc];
            v[r * scw + c] = fr.v[sr * cw + sc];
        }
    }
    YuvFrame { width: sw, height: sh, y, u, v }
}

// ---------------------------------------------------------------- y4m read --
fn read_y4m(path: &str, max_frames: usize) -> (usize, usize, f64, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let hdr_end = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..hdr_end]).expect("utf8 header");
    let (mut w, mut h, mut fps) = (0usize, 0usize, 25.0f64);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().expect("width"),
            Some(b'H') => h = tok[1..].parse().expect("height"),
            Some(b'F') => {
                if let Some((n, d)) = tok[1..].split_once(':') {
                    let (n, d): (f64, f64) = (n.parse().unwrap_or(25.0), d.parse().unwrap_or(1.0));
                    if d > 0.0 {
                        fps = n / d;
                    }
                }
            }
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let mut frames = Vec::new();
    let mut p = hdr_end + 1;
    while frames.len() < max_frames {
        let Some(rel) = raw[p..].iter().position(|&b| b == b'\n') else { break };
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
    (w, h, fps, frames)
}

// ------------------------------------------------------------------- arms --
const HDR: u32 = 0x66DDFF; // headers / ours accent
const ENC: u32 = 0xFFCC66; // x264 accent
const LIVE: u32 = 0xE8E8E8;
const STAND: u32 = 0x8899AA;
const GOOD: u32 = 0x66FF99;
const BAD: u32 = 0xFF4444;
const WARN: u32 = 0xFFAA66;
const DIM: u32 = 0x556677;

/// Per-DISPLAY-frame measurements, revealed as the playhead reaches them.
struct FrameStat {
    kind: u8, // 0 = IDR, 1 = reference (I/P/ref-B), 2 = non-ref B leaf
    au_bytes: usize,
    dec_ms: f64, // OUR decoder, best-of-N per-AU wall (indicative)
    route: Option<ContentRoute>,
    psnr_y: f64, // vs the shared source
    ndiff: usize, // our decode vs ffmpeg's decode of the SAME stream
    maxd: u8,
}

struct Arm {
    label: String,
    accent: u32,
    panels: Vec<YuvFrame>, // OUR decode of this arm's stream, display order
    pw: usize,
    ph: usize,
    stats: Vec<FrameStat>,
    bytes: usize,
    enc_ms_per_frame: f64,
    have_ff: bool,
    mismatch: bool,
}

fn kind_of(hb: u8) -> u8 {
    match hb & 0x1F {
        5 => 0,
        _ if (hb >> 5) & 3 == 0 => 2,
        _ => 1,
    }
}

/// First slice NAL header byte inside one access unit (scans start codes).
fn first_slice_hdr(au: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let hb = au[i + 3];
            if matches!(hb & 0x1F, 1 | 5) {
                return Some(hb);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

/// Everything downstream of a bitstream is ARM-AGNOSTIC: decode per AU with
/// OUR decoder (times, POC, route), reorder each IDR-delimited group by POC
/// (the `decode_stream` reorder — the ffmpeg pairing below lights up
/// immediately if this permutation ever disagreed), cross-check every pixel
/// against ffmpeg's decode of the same stream, and score PSNR vs the source.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn run_arm(
    label: &str,
    accent: u32,
    idx: usize,
    src: &[YuvFrame],
    stream: &[u8],
    enc_ms_per_frame: f64,
    reps: usize,
    panel_budget: usize,
) -> Arm {
    let (w, h) = (src[0].width, src[0].height);
    let aus: Vec<&[u8]> = rusty_h264_decoder::split_access_units(stream);
    eprintln!("[{label}] decoding per-AU with OUR decoder (best of {reps}) ...");
    struct Rec {
        fr: Option<YuvFrame>,
        poc: i32,
        kind: u8,
        au_bytes: usize,
        ms: f64,
        route: Option<ContentRoute>,
    }
    let mut recs: Vec<Rec> = Vec::new();
    for pass in 0..reps {
        let mut dec = Decoder::new();
        let mut k = 0usize;
        for au in &aus {
            let t = Instant::now();
            let out = dec.decode(au).expect("decode");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let Some(fr) = out else { continue };
            if pass == 0 {
                recs.push(Rec {
                    fr: Some(fr),
                    poc: dec.last_poc(),
                    kind: first_slice_hdr(au).map(kind_of).unwrap_or(1),
                    au_bytes: au.len(),
                    ms,
                    route: dec.content_route(),
                });
            } else if let Some(r) = recs.get_mut(k) {
                r.ms = r.ms.min(ms);
            }
            k += 1;
        }
    }
    // Display order: sort each IDR-delimited group by POC (stable).
    let mut order: Vec<usize> = Vec::with_capacity(recs.len());
    {
        let mut gs = 0usize;
        for i in 0..=recs.len() {
            if i == recs.len() || (recs[i].kind == 0 && i > gs) {
                let mut g: Vec<usize> = (gs..i).collect();
                g.sort_by_key(|&k| recs[k].poc);
                order.extend(g);
                gs = i;
            }
        }
    }

    // ffmpeg cross-decode of the SAME stream (conformance oracle).
    let tmp = std::env::temp_dir().join(format!("sim_oppy_{idx}.264"));
    std::fs::write(&tmp, stream).expect("write temp stream");
    let tmps = tmp.to_string_lossy().to_string();
    let mut pipe = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-threads", "1", "-i", &tmps, "-f", "rawvideo",
            "-pix_fmt", "yuv420p", "-",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .ok();
    let mut ffout = pipe.as_mut().and_then(|p| p.stdout.take());
    let have_ff = ffout.is_some();

    let step = {
        let mut s = 1;
        while w / s > panel_budget {
            s += 1;
        }
        s
    };
    let (ysz, csz) = (w * h, w.div_ceil(2) * h.div_ceil(2));
    let mut panels: Vec<YuvFrame> = Vec::with_capacity(order.len());
    let mut stats: Vec<FrameStat> = Vec::with_capacity(order.len());
    let mut mismatch = false;
    eprintln!("[{label}] pairing (per-frame conformance + PSNR) ...");
    for (di, &ri) in order.iter().enumerate() {
        let fr = recs[ri].fr.take().expect("frame");
        let (mut ndiff, mut maxd) = (0usize, 0u8);
        if let Some(out) = ffout.as_mut() {
            let mut raw = vec![0u8; ysz + 2 * csz];
            if out.read_exact(&mut raw).is_ok() {
                for (a, b) in fr.y.iter().chain(fr.u.iter()).chain(fr.v.iter()).zip(raw.iter()) {
                    let d = a.abs_diff(*b);
                    if d > 0 {
                        ndiff += 1;
                        maxd = maxd.max(d);
                    }
                }
                if ndiff > 0 {
                    mismatch = true;
                }
            }
        }
        let psnr_y = src
            .get(di)
            .map(|s| {
                let mut se = 0f64;
                for (a, b) in s.y.iter().zip(fr.y.iter()) {
                    let e = *a as f64 - *b as f64;
                    se += e * e;
                }
                let mse = se / s.y.len().max(1) as f64;
                if mse <= 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / mse).log10() }
            })
            .unwrap_or(0.0);
        stats.push(FrameStat {
            kind: recs[ri].kind,
            au_bytes: recs[ri].au_bytes,
            dec_ms: recs[ri].ms,
            route: recs[ri].route,
            psnr_y,
            ndiff,
            maxd,
        });
        panels.push(shrink(&fr, step));
    }
    if let Some(mut p) = pipe {
        let _ = p.wait();
    }
    let _ = std::fs::remove_file(&tmp);

    Arm {
        label: label.to_string(),
        accent,
        panels,
        pw: (w / step).max(1),
        ph: (h / step).max(1),
        stats,
        bytes: stream.len(),
        enc_ms_per_frame,
        have_ff,
        mismatch,
    }
}

fn fmt_kb(b: usize) -> String {
    if b >= 10_000 { format!("{:.1} KB", b as f64 / 1000.0) } else { format!("{b} B") }
}

fn route_name(r: Option<ContentRoute>) -> &'static str {
    match r {
        Some(ContentRoute::Light) => "LIGHT",
        Some(ContentRoute::Mid) => "MID",
        Some(ContentRoute::DenseInter) => "DENSE-INTER",
        Some(ContentRoute::EntropyExtreme) => "EXTREME",
        None => "-",
    }
}

/// LIVE rows for one arm at playhead `i` — short, big-type, one fact each.
fn live_rows(a: &Arm, i: usize) -> Vec<(String, u32)> {
    let n = a.stats.len();
    let s = &a.stats[i];
    let kind = match s.kind {
        0 => "IDR",
        1 => "REF",
        _ => "B",
    };
    let (mut cb, mut cms, mut cnd) = (0usize, 0f64, 0usize);
    for st in &a.stats[..=i] {
        cb += st.au_bytes;
        cms += st.dec_ms;
        cnd += st.ndiff;
    }
    vec![
        (format!("FRAME {:>3}/{n}  {kind:<3}  AU {}", i + 1, fmt_kb(s.au_bytes)), LIVE),
        (format!("DEC {:>5.2} MS   PSNR {:>5.2} DB", s.dec_ms, s.psnr_y), LIVE),
        (
            format!("ROUTE {}   TOTAL {}", route_name(s.route), fmt_kb(cb)),
            WARN,
        ),
        if !a.have_ff {
            ("NO FFMPEG CROSS-CHECK".into(), BAD)
        } else if cnd == 0 {
            (format!("FFMPEG MATCH {}/{}", i + 1, i + 1), GOOD)
        } else {
            (format!("{cnd} DIFFS, MAX {}", s.maxd), BAD)
        },
    ]
}

/// Full-width LIVE gap block: the offline doc numbers, measured on THIS clip
/// at the playhead.
fn gap_rows(ours: &Arm, x264: &Arm, i: usize, qp: u8) -> Vec<(String, u32)> {
    let upto = |a: &Arm| {
        let k = i.min(a.stats.len() - 1);
        let (mut b, mut ms, mut ps) = (0usize, 0f64, 0f64);
        for st in &a.stats[..=k] {
            b += st.au_bytes;
            ms += st.dec_ms;
            ps += st.psnr_y;
        }
        (b, ms / (k + 1) as f64, ps / (k + 1) as f64)
    };
    let (ob, oms, ops) = upto(ours);
    let (xb, xms, xps) = upto(x264);
    let gap = if xb > 0 { (ob as f64 / xb as f64 - 1.0) * 100.0 } else { f64::NAN };
    vec![
        (format!("LIVE GAP AT MATCHED QP {qp} (THIS CLIP, THIS FRAME)"), HDR),
        (
            format!("BITS   OURS {}   X264 {}   {gap:+.0}%", fmt_kb(ob), fmt_kb(xb)),
            WARN,
        ),
        (
            format!("PSNR-Y OURS {ops:.2} DB   X264 {xps:.2} DB   {:+.2}", ops - xps),
            LIVE,
        ),
        (
            format!("DECODE {oms:.2} VS {xms:.2} MS/FR (OUR DECODER, BOTH STREAMS)"),
            LIVE,
        ),
    ]
}

/// The DOCS view (key T): the standing big-oppy tables, ONE copy, big type.
fn docs_left() -> Vec<(String, u32)> {
    vec![
        ("BIG-OPPY-ENCODER (STANDING, OFFLINE BD)".into(), HDR),
        ("".into(), STAND),
        ("VS X264 DEFAULTS    30% BEHIND (NATURAL)".into(), STAND),
        ("VS MATCHED TOOLS    2% BEHIND".into(), STAND),
        ("ALL-INTRA           WE WIN (TSRC CLASS)".into(), STAND),
        ("SPEED GAP OWNER     ME 81%, 10.3X/CALL".into(), STAND),
        ("".into(), STAND),
        ("THIS RUN (WHOLE CLIP):".into(), HDR),
    ]
}

fn docs_right() -> Vec<(String, u32)> {
    vec![
        ("BIG-OPPY-DECODER (STANDING, PINNED)".into(), HDR),
        ("".into(), STAND),
        ("VS FFMPEG, MPX/S    OURS   FFMPEG".into(), STAND),
        ("CAVLC VF   1.81X    280    508".into(), STAND),
        ("MAIN MED   1.84X    211    390".into(), STAND),
        ("HIGH SLOW  1.84X    179    324".into(), STAND),
        ("".into(), STAND),
        ("GATE1 ROUTES (NS/MB, N=51)".into(), HDR),
        ("LIGHT 265-1170      MID 908-1578".into(), STAND),
        ("DENSE 1101-3268     EXTREME 3136-6797".into(), STAND),
        ("".into(), STAND),
        ("CONFORMANCE (2026-08-27)".into(), HDR),
        ("ACCEL + SCALAR FFMPEG-EXACT, ALL PRESETS".into(), GOOD),
        ("SCALAR CHROMA P0 FIXED, MAIN DEFECT CLOSED".into(), STAND),
    ]
}

/// Per-frame bar sparkline across the WHOLE clip with a bright playhead.
#[allow(clippy::too_many_arguments)]
fn bars(
    buf: &mut [u32],
    dw: usize,
    dh: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    vals: &[f64],
    cur: usize,
    label: &str,
    hot: u32,
) {
    text(buf, dw, dh, x, y, label, DIM, 1);
    let y0 = y + 10;
    let vmax = vals.iter().cloned().fold(1e-9, f64::max);
    let n = vals.len().max(1);
    let bw = (w / n).max(2);
    for (k, v) in vals.iter().enumerate() {
        let bh = ((v / vmax) * (h.saturating_sub(1)) as f64).round() as usize;
        let col = if k == cur {
            hot
        } else if k < cur {
            0x4A5560
        } else {
            0x252B33
        };
        for px in 0..bw.saturating_sub(1) {
            let tx = x + k * bw + px;
            if tx >= x + w || tx >= dw {
                break;
            }
            for py in 0..bh.max(1) {
                let ty = y0 + h - 1 - py;
                if ty < dh {
                    buf[ty * dw + tx] = col;
                }
            }
        }
    }
}

// --------------------------------------------------------------------- main --
#[allow(clippy::too_many_lines)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let clip = args
        .iter()
        .find(|a| a.ends_with(".y4m"))
        .cloned()
        .unwrap_or_else(|| "video-tests/clips/foreman_cif.y4m".to_string());
    let arg = |k: &str| {
        args.iter().find_map(|a| a.strip_prefix(k).and_then(|v| v.parse::<usize>().ok()))
    };
    let max_frames = arg("max=").unwrap_or(60);
    let qp = arg("qp=").unwrap_or(26) as u8;
    let reps = arg("reps=").unwrap_or(3).max(1);
    let maxw = arg("maxw=").unwrap_or(1500);
    let panel_budget = (maxw.saturating_sub(36)) / 2;
    let name = std::path::Path::new(&clip)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| clip.clone());

    eprintln!("[{name}] reading ...");
    let (w, h, _fps, frames) = read_y4m(&clip, max_frames);
    assert!(!frames.is_empty(), "{clip}: no frames");
    let nsrc = frames.len();

    // ---- ARM 1: our encoder (matched tool set: refs 3, gop 250, bf 3) -----
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.bframes = 3;
    cfg.bframes_adaptive = true;
    cfg.profile = rusty_h264_common::Profile::Main;
    eprintln!("[{name}] encoding with OUR encoder (balanced, qp {qp}, bframes auto) ...");
    let t = Instant::now();
    let our_aus = Encoder::new(cfg).expect("cfg").encode_all(&frames).expect("encode");
    let our_enc_ms = t.elapsed().as_secs_f64() * 1000.0 / nsrc as f64;
    let our_stream: Vec<u8> = our_aus.concat();

    // ---- ARM 2: x264 via ffmpeg at MATCHED settings ------------------------
    eprintln!("[{name}] encoding with X264 (ffmpeg libx264, matched settings) ...");
    let xtmp = std::env::temp_dir().join("sim_oppy_x264.264");
    let xtmps = xtmp.to_string_lossy().to_string();
    let t = Instant::now();
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y", "-i", &clip, "-frames:v",
            &nsrc.to_string(), "-c:v", "libx264", "-preset", "medium", "-qp", &qp.to_string(),
            "-g", "250", "-bf", "3", "-refs", "3", "-threads", "1", "-f", "h264", &xtmps,
        ])
        .status();
    let x264_enc_ms = t.elapsed().as_secs_f64() * 1000.0 / nsrc as f64;
    if !matches!(st, Ok(s) if s.success()) {
        eprintln!("x264 encode failed - is ffmpeg (with libx264) on PATH?");
        std::process::exit(2);
    }
    let x264_stream = std::fs::read(&xtmp).expect("read x264 stream");
    let _ = std::fs::remove_file(&xtmp);

    let ours = run_arm("OURS", HDR, 0, &frames, &our_stream, our_enc_ms, reps, panel_budget);
    let x264 = run_arm("X264", ENC, 1, &frames, &x264_stream, x264_enc_ms, reps, panel_budget);

    if args.iter().any(|s| s == "nowin") {
        for a in [&ours, &x264] {
            println!("\n== {} == ({} B, ENC {:.1} MS/FR)", a.label, a.bytes, a.enc_ms_per_frame);
            for (line, _) in live_rows(a, a.stats.len() - 1) {
                println!("{line}");
            }
        }
        println!();
        for (line, _) in gap_rows(&ours, &x264, ours.stats.len() - 1, qp) {
            println!("{line}");
        }
        for (line, _) in docs_left().iter().chain(docs_right().iter()) {
            println!("{line}");
        }
        if ours.mismatch || x264.mismatch {
            std::process::exit(1);
        }
        return;
    }

    // ---- layout: big type (scale 2), videos ZOOMED to fill their columns,
    // one live view + a docs view on T --------------------------------------
    let s2 = 2usize;
    let row2 = 7 * s2 + 8; // 22 px rows
    let zoom = (panel_budget / ours.pw).clamp(1, 3);
    let (vw, vh) = (ours.pw * zoom, ours.ph * zoom);
    let colw = vw.max(46 * 6 * s2 + 8); // widest docs/live line at scale 2
    let margin = 12;
    let dw = margin + colw + margin + colw + margin;
    let cap_h = 7 * s2 + 12;
    let bar_h = 26;
    let bars_h = 2 * (bar_h + 14) + 6;
    let live_h = 4 * row2 + 6;
    let docs_h = docs_right().len() * row2;
    let lower_h = (bars_h + live_h).max(docs_h) + 8;
    let gaph = 4 * row2 + 10;
    let dh = cap_h + vh + 6 + lower_h + gaph + row2;
    let mut buf = vec![0u32; dw * dh];

    eprintln!("layout: window {dw}x{dh}, columns 2x{colw}, video zoom {zoom}x, text scale {s2}");
    let mut win = Window::new(
        &format!("big-oppy sim - {name} - OURS vs X264"),
        dw,
        dh,
        WindowOptions::default(),
    )
    .expect("open window");
    win.set_target_fps(25);

    let (mut tick, mut playing, mut docs) = (0usize, true, false);
    let n = ours.panels.len().max(1);
    while win.is_open() && !win.is_key_down(Key::Escape) && !win.is_key_down(Key::Q) {
        if win.is_key_pressed(Key::Space, minifb::KeyRepeat::No) {
            playing = !playing;
        }
        if win.is_key_pressed(Key::R, minifb::KeyRepeat::No) {
            tick = 0;
        }
        if win.is_key_pressed(Key::T, minifb::KeyRepeat::No) {
            docs = !docs;
        }

        buf.fill(0x0E1116);
        let i = tick % n;
        for (a, ox) in [(&ours, margin), (&x264, margin + colw + margin)] {
            let ai = i.min(a.panels.len().saturating_sub(1));
            let cap = if a.accent == HDR { "RUSTY_H264 (OURS)" } else { "X264 REFERENCE" };
            text(&mut buf, dw, dh, ox, 6, cap, a.accent, s2);
            let ctr = format!("{}/{}", ai + 1, a.panels.len());
            text(&mut buf, dw, dh, ox + colw - ctr.chars().count() * 6 * s2, 6, &ctr, DIM, s2);
            if let Some(fr) = a.panels.get(ai) {
                yuv_to_rgb(fr, &mut buf, dw, dh, ox + (colw - vw) / 2, cap_h, zoom);
            }
            let ly = cap_h + vh + 6;
            if docs {
                let rows = if a.accent == HDR { docs_left() } else { docs_right() };
                for (k, (line, col)) in rows.iter().enumerate() {
                    text(&mut buf, dw, dh, ox, ly + k * row2, line, *col, s2);
                }
                if a.accent == HDR {
                    // the "THIS RUN" facts under the encoder standing block
                    let extra = [
                        format!("OURS {}  ENC {:.1} MS/FR", fmt_kb(ours.bytes), ours.enc_ms_per_frame),
                        format!("X264 {}  ENC {:.1} MS/FR", fmt_kb(x264.bytes), x264.enc_ms_per_frame),
                    ];
                    for (k, line) in extra.iter().enumerate() {
                        text(
                            &mut buf,
                            dw,
                            dh,
                            ox,
                            ly + (docs_left().len() + k) * row2,
                            line,
                            LIVE,
                            s2,
                        );
                    }
                }
            } else {
                let au: Vec<f64> = a.stats.iter().map(|s| s.au_bytes as f64).collect();
                let ms: Vec<f64> = a.stats.iter().map(|s| s.dec_ms).collect();
                bars(&mut buf, dw, dh, ox, ly, colw, bar_h, &au, ai, "AU BYTES / FRAME", a.accent);
                bars(
                    &mut buf,
                    dw,
                    dh,
                    ox,
                    ly + bar_h + 14,
                    colw,
                    bar_h,
                    &ms,
                    ai,
                    "DECODE MS / FRAME",
                    a.accent,
                );
                let ty = ly + bars_h;
                for (k, (line, col)) in live_rows(a, ai).iter().enumerate() {
                    text(&mut buf, dw, dh, ox, ty + k * row2, line, *col, s2);
                }
            }
        }
        // full-width live gap block, separated by a rule
        let gy = cap_h + vh + 6 + lower_h;
        hline(&mut buf, dw, dh, margin, gy - 4, dw - 2 * margin, 0x2A3340);
        for (k, (line, col)) in gap_rows(&ours, &x264, i, qp).iter().enumerate() {
            text(&mut buf, dw, dh, margin, gy + k * row2, line, *col, s2);
        }
        text(
            &mut buf,
            dw,
            dh,
            margin,
            dh - row2 + 4,
            "SPACE PAUSE   T DOCS   R RESTART   Q QUIT",
            DIM,
            1,
        );

        win.update_with_buffer(&buf, dw, dh).expect("blit");
        if playing {
            tick += 1;
        }
    }

    if ours.mismatch || x264.mismatch {
        std::process::exit(1);
    }
}
