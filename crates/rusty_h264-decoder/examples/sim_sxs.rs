//! HEADFUL SIDE-BY-SIDE — our decoder and ffmpeg, decoding the same x264
//! stream, in one window, with a third panel showing the pixel difference.
//!
//! ```text
//! cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- main
//! cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- high
//! cargo run --release -p rusty_h264-decoder --features asm --example sim_sxs -- <s.264> [scale=N] [max=N]
//! ```
//!
//! Add `nowin` to skip the window and just print the pixel verdict (CI-shaped;
//! exits non-zero on any mismatch).
//!
//! Keys: SPACE pause/run · RIGHT step one frame · R restart · Q/ESC quit
//!
//! WHAT THE TIMING HERE IS AND IS NOT. Our per-frame milliseconds are measured
//! in-process and are real. They are NOT the benchmark: this process also holds
//! a window, converts YUV to RGB and blits three panels, and ffmpeg is a
//! subprocess feeding a pipe, so neither arm runs under the pinned,
//! ABBA-alternated, CPU-time conditions the standing numbers come from. The
//! ffmpeg figure is therefore measured ONCE up front by a separate `-f null`
//! pass over the same file, with nothing else running, and is shown as a
//! REFERENCE LINE rather than a paired comparison. For the real number use
//! `bash bench/nsmb_rerun.sh`.
//!
//! HOW FFMPEG IS TIMED, AND WHY IT TOOK THREE GOES. The first version ran one
//! `-f null` subprocess, unpinned, unrepeated, wall clock. Asked to resolve a
//! 1.84x ratio it produced, on a QUIET box, the same file five times: 1.72,
//! 1.46, 2.02, 1.65, 1.59 - and with a second copy of this viewer running, 0.80,
//! an apparent WIN. Two things were wrong; both are fixed here:
//!
//!   * ONE SAMPLE OF A NOISY QUANTITY. Now BEST-OF-N (`reps=`, default 3).
//!     Contention can only ever make a run SLOWER, so the minimum is the robust
//!     estimator - which is why `decode_bench` reports best-of-N too.
//!   * PROCESS STARTUP charged to 60 frames. Now measured separately
//!     (`-frames:v 1` on the same file = launch + demux open + one frame) and
//!     SUBTRACTED, the remainder spread over the frames that are left. A fixed
//!     per-invocation cost inflates the SHORTER arm by the larger fraction --
//!     the trap `decode_x264_speedtest.sh` concatenates streams to avoid. A
//!     viewer cannot concatenate, so it subtracts instead.
//!
//! It is still WALL time on an unpinned box, so it is still INDICATIVE and the
//! standing pinned figure is shown beside it - but it now lands near that figure
//! instead of contradicting it.
//!
//! WHAT IT IS FOR: watching the two decoders agree, pixel for pixel, on content
//! that exercises sub-8x8 partitions, multi-ref, B-pyramid, the 8x8 transform
//! and weighted prediction — and seeing instantly, in the DIFF panel, when they
//! do not.

use minifb::{Key, Window, WindowOptions};
use rusty_h264_common::YuvFrame;
use rusty_h264_decoder::{split_access_units, Decoder};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

// ---------------------------------------------------------------- 5x7 font --
// Column-major, LSB = top row. Uppercase only; lowercase is folded up.
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

/// Rendered width of `s` at scale `sc`. Layout is COMPUTED from this, never
/// from hand-picked offsets — see the layout note in `main`.
fn tw(s: &str, sc: usize) -> usize {
    s.chars().count() * 6 * sc
}

/// Blits `s` at `(x, y)` into `buf` (0RGB), `sc` screen pixels per font pixel.
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

// ------------------------------------------------------------- YUV -> 0RGB --
/// BT.601 limited range — the conversion ffplay shows these streams with.
fn yuv_to_rgb(
    fr: &YuvFrame,
    dst: &mut [u32],
    dw: usize,
    dh: usize,
    ox: usize,
    oy: usize,
    step: usize,
) {
    let (w, h) = (fr.width, fr.height);
    let cw = w.div_ceil(2);
    for py in (0..h).step_by(step) {
        let ty = oy + py / step;
        if ty >= dh {
            break;
        }
        for px in (0..w).step_by(step) {
            let tx = ox + px / step;
            if tx >= dw {
                break;
            }
            let y = fr.y[py * w + px] as i32;
            let ci = (py / 2) * cw + px / 2;
            let (u, v) = (fr.u[ci] as i32 - 128, fr.v[ci] as i32 - 128);
            let c = (y - 16) * 298;
            let r = ((c + 409 * v + 128) >> 8).clamp(0, 255) as u32;
            let g = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u32;
            let b = ((c + 516 * u + 128) >> 8).clamp(0, 255) as u32;
            dst[ty * dw + tx] = (r << 16) | (g << 8) | b;
        }
    }
}

/// Draws |ours - ffmpeg| on luma, amplified so a single-code difference is
/// visible, and returns the WHOLE-FRAME verdict over Y, U and V.
fn diff_panel(
    a: &YuvFrame,
    b: &YuvFrame,
    dst: &mut [u32],
    dw: usize,
    dh: usize,
    ox: usize,
    oy: usize,
    step: usize,
) -> (u8, usize) {
    let (w, h) = (a.width, a.height);
    let (mut maxd, mut ndiff) = (0u8, 0usize);
    let planes = a
        .y
        .iter()
        .zip(b.y.iter())
        .chain(a.u.iter().zip(b.u.iter()))
        .chain(a.v.iter().zip(b.v.iter()));
    for (x, y) in planes {
        let d = x.abs_diff(*y);
        if d > 0 {
            ndiff += 1;
            maxd = maxd.max(d);
        }
    }
    for py in (0..h).step_by(step) {
        let ty = oy + py / step;
        if ty >= dh {
            break;
        }
        for px in (0..w).step_by(step) {
            let tx = ox + px / step;
            if tx >= dw {
                break;
            }
            let d = a.y[py * w + px].abs_diff(b.y[py * w + px]) as u32;
            let s = (d * 16).min(255);
            // Identical reads as near-black; any difference glows red.
            dst[ty * dw + tx] = if d == 0 { 0x0A0A0A } else { (s << 16) | ((s / 3) << 8) };
        }
    }
    (maxd, ndiff)
}

/// Nearest-neighbour shrink to display size. The viewer used to hold every
/// frame at FULL resolution for both arms — fine for a 60-frame clip (166 MB),
/// impossible for a 25-second one (3.5 GB). Frames are shrunk to the panel size
/// as they are paired, the full-resolution pair is used ONCE to compute the
/// identity verdict, and then dropped.
fn shrink(fr: &YuvFrame, step: usize) -> YuvFrame {
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

/// Whole-frame identity verdict over Y, U and V at FULL resolution.
fn verdict(a: &YuvFrame, b: &YuvFrame) -> (u8, usize) {
    let (mut maxd, mut ndiff) = (0u8, 0usize);
    let planes = a
        .y
        .iter()
        .zip(b.y.iter())
        .chain(a.u.iter().zip(b.u.iter()))
        .chain(a.v.iter().zip(b.v.iter()));
    for (x, y) in planes {
        let d = x.abs_diff(*y);
        if d > 0 {
            ndiff += 1;
            maxd = maxd.max(d);
        }
    }
    (maxd, ndiff)
}

// --------------------------------------------------------------------- main --
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str).unwrap_or("main");
    // `main`/`high` prefer the 25-SECOND demo encode and fall back to the
    // 60-frame conformance clip. A 1.2 s loop is a poor demo; the benchmark
    // clips are short because they are benchmark clips.
    let pick = |long: &str, short: &str| {
        if std::path::Path::new(long).exists() {
            long.to_string()
        } else {
            short.to_string()
        }
    };
    let path = match first {
        "main" => pick("_xbench/demo25s__main.264", "_xbench/tt/720p50_shields_ter__main.264"),
        "high" => pick("_xbench/demo25s__high.264", "_xbench/tt/720p50_shields_ter__high.264"),
        p => p.to_string(),
    };
    let arg = |k: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(k).and_then(|v| v.parse::<usize>().ok()))
    };
    let max_frames = arg("max=").unwrap_or(usize::MAX);
    // The standing pinned ratios (docs/big-oppy-decoder.md, 2026-08-22 record).
    // Shown next to this viewer's own number so the two can never be confused.
    let standing = if path.contains("__high") {
        Some(("HIGH", 1.84))
    } else if path.contains("__main") {
        Some(("MAIN", 1.84))
    } else if path.contains("__cavlc") {
        Some(("CAVLC", 1.81))
    } else {
        None
    };
    let tier = if path.contains("__high") {
        "HIGH - X264 PRESET SLOWER"
    } else if path.contains("__main") {
        "MAIN - X264 PRESET MEDIUM"
    } else {
        "STREAM"
    };

    let input = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));

    // ---- ffmpeg TIMING: best-of-N, startup-corrected ----------------------
    let reps = arg("reps=").unwrap_or(3).max(1);
    let run_ms = |extra: &[&str]| -> f64 {
        let mut best = f64::MAX;
        for _ in 0..reps {
            let t = Instant::now();
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i", &path]);
            cmd.args(extra);
            cmd.args(["-f", "null", "-"]);
            let ok = matches!(cmd.status(), Ok(st) if st.success());
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ok {
                best = best.min(ms);
            }
        }
        best
    };
    eprint!("timing ffmpeg (best of {reps}, startup-corrected) ... ");
    let ff_full_ms = run_ms(&[]);
    let ff_start_ms = run_ms(&["-frames:v", "1"]);
    if !ff_full_ms.is_finite() || !ff_start_ms.is_finite() {
        eprintln!("FAILED - is ffmpeg on PATH?");
        return;
    }
    eprintln!("{ff_full_ms:.0} ms total, {ff_start_ms:.0} ms startup+1frame");

    // ---- ffmpeg PIXELS, streamed, for the visual comparison ---------------
    let mut ffpipe = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-threads", "1", "-i", &path, "-f", "rawvideo",
            "-pix_fmt", "yuv420p", "-",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ffmpeg pixel pipe");
    let mut ffout = ffpipe.stdout.take().expect("ffmpeg stdout");

    // ---- decode ours, in DISPLAY ORDER ------------------------------------
    // `decode_stream` reorders by PicOrderCnt within each GOP; the per-AU
    // `Decoder::decode` hands frames back in DECODE order. ffmpeg emits display
    // order, so pairing per-AU output against it positionally is wrong the
    // moment the stream has B-frames — which MAIN and HIGH both do. Doing
    // exactly that reported 50.6M differing samples on a stream the 68-stream
    // gate proves byte-identical: the harness was wrong, not the decoder. (The
    // same defect once produced a BD-SSIM of 4.9e9%.)
    //
    // Timing follows from that: a whole-stream call has no per-frame hook, so
    // both arms are quoted as total / frames — which is the shape the standing
    // benchmark uses anyway.
    // TIMING PASS: decode and DROP, which is the shape ffmpeg is timed in
    // (`-f null`) and the shape `decode_bench` uses. Timing `decode_stream`
    // instead charges our arm for accumulating every frame into a Vec - a
    // per-frame allocation and copy ffmpeg never pays - and that bias is not
    // small: it moved the ratio from ~1.9x to ~2.25x against a pinned 1.84x.
    // Same trap `decode_bench` was written to escape, arrived at from the other
    // direction.
    eprintln!("timing our decode (drop frames, best of {reps}) ...");
    let aus: Vec<&[u8]> = split_access_units(&input);
    let mut ours_total_ms = f64::MAX;
    for _ in 0..reps {
        let mut dec = Decoder::new();
        let t = Instant::now();
        for au in &aus {
            let _ = dec.decode(au).expect("decode");
        }
        ours_total_ms = ours_total_ms.min(t.elapsed().as_secs_f64() * 1000.0);
    }

    // DISPLAY PASS: `decode_stream` reorders by PicOrderCnt, which is what the
    // pixel comparison needs and what the timing pass above deliberately is not.
    eprintln!("decoding for display (POC order) ...");
    let mut ours: Vec<YuvFrame> =
        Decoder::new().decode_stream(&input).expect("decode_stream");
    ours.truncate(max_frames);
    let Some(f0) = ours.first() else {
        eprintln!("no frames decoded");
        return;
    };
    let (w, h) = (f0.width, f0.height);
    let (ysz, csz) = (w * h, w.div_ceil(2) * h.div_ceil(2));

    // Panel scale has to be known BEFORE pairing, because frames are stored at
    // panel size. Same budget the layout uses.
    let maxw = arg("maxw=").unwrap_or(1280);
    let step = arg("scale=").unwrap_or_else(|| {
        let mut sc = 1;
        while (w / sc) * 3 > maxw {
            sc += 1;
        }
        sc
    });

    // Pair each frame with ffmpeg's, judge at full resolution, keep only the
    // shrunk pair. Peak memory is one full-resolution ffmpeg frame, not all of
    // them.
    eprintln!("pairing against ffmpeg and shrinking to panel size ...");
    let mut panels: Vec<(YuvFrame, YuvFrame, u8, usize)> = Vec::new();
    for fr in ours.iter() {
        let mut raw = vec![0u8; ysz + 2 * csz];
        if ffout.read_exact(&mut raw).is_err() {
            break;
        }
        let ff = YuvFrame {
            width: w,
            height: h,
            y: raw[..ysz].to_vec(),
            u: raw[ysz..ysz + csz].to_vec(),
            v: raw[ysz + csz..].to_vec(),
        };
        let (maxd, nd) = verdict(fr, &ff);
        panels.push((shrink(fr, step), shrink(&ff, step), maxd, nd));
    }
    ours.clear();
    ours.shrink_to_fit();
    let _ = ffpipe.wait();
    let n = panels.len();
    if n == 0 {
        eprintln!("no comparable frames -- did ffmpeg decode this stream?");
        return;
    }
    let mbs = (w.div_ceil(16) * h.div_ceil(16)) as f64;
    let ours_mean_ms = ours_total_ms / n as f64;
    // Startup and the first frame come off the top; the remainder is spread over
    // the frames that are left. Both arms are then milliseconds per decoded frame.
    let ff_mean_ms = ((ff_full_ms - ff_start_ms) / (n.saturating_sub(1)).max(1) as f64).max(0.0);
    let ratio = if ff_mean_ms > 0.0 { ours_mean_ms / ff_mean_ms } else { f64::NAN };

    // ---- headless verdict: the same comparison with no window ------------
    // `nowin` runs the whole pipeline and prints the pixel verdict. It exists so
    // the comparison can be asserted in CI, and so the tool can answer "do they
    // agree" without a desktop.
    if args.iter().any(|a| a == "nowin") {
        // Verdicts were computed at FULL resolution while pairing.
        let (mut maxd, mut ndiff, mut at) = (0u8, 0usize, 0usize);
        for (k, (_, _, md, nd)) in panels.iter().enumerate() {
            ndiff += nd;
            if *md > maxd {
                maxd = *md;
                at = k;
            }
        }
        println!("frames compared: {n}  ({w}x{h}, {tier})");
        println!("ours   mean {:.2} ms/frame ({:.0} ns/MB)", ours_mean_ms, ours_mean_ms * 1e6 / mbs);
        println!(
            "ffmpeg {:.2} ms/frame ({:.0} ns/MB)  [best of {reps}, less {:.0} ms startup]",
            ff_mean_ms,
            ff_mean_ms * 1e6 / mbs,
            ff_start_ms
        );
        println!("ratio here {ratio:.2}x  (wall, unpinned -- indicative)");
        match standing {
            Some((t, r)) => println!("standing pinned {t} ratio {r:.2}x  (bench/decode_x264_speedtest.sh)"),
            None => println!("standing gap: see docs/big-oppy-decoder.md"),
        }
        if ndiff == 0 {
            println!("PIXEL-IDENTICAL on all {n} frames");
        } else {
            println!("MISMATCH: {ndiff} samples differ, worst delta {maxd} at frame {}", at + 1);
            std::process::exit(1);
        }
        return;
    }

    // ---- LAYOUT, DERIVED FROM REAL TEXT METRICS --------------------------
    // The first cut placed captions and three columns of stats at hand-picked
    // offsets sized for a ~1700px window, and they collided the moment the
    // panels were narrower. Everything here is computed instead: the panel scale
    // from the window budget, the FONT scale from the longest line it has to
    // hold, and the strip height from the row count. Each stat gets its own full
    // width row, so nothing can overlap at any window size.
    // `step` was fixed before pairing (frames are STORED at panel size), so the
    // layout just adopts it. Recomputing it here once produced a second,
    // different scale and the panels no longer matched the window.
    let (pw, ph) = ((w / step).max(1), (h / step).max(1));
    let dw = pw * 3;

    // Captions sit above their own panel, so they must fit ONE panel width.
    let cap_sc = if tw("DIFF - IDENTICAL", 2) + 12 <= pw { 2 } else { 1 };
    let cap_h = 7 * cap_sc + 10;

    // Stat rows are full width. Build the WIDEST string each row can ever hold,
    // then pick the largest font scale that still fits.
    let ratio_line = match standing {
        Some((t, r)) => format!("RATIO HERE 9.99X   STANDING PINNED {t} {r:.2}X  (WALL, UNPINNED)"),
        None => "STANDING GAP: SEE DOCS/BIG-OPPY-DECODER.MD".to_string(),
    };
    let widest = [
        format!("FRAME {n}/{n}   {w}X{h}   {tier}"),
        "OURS 99.99 MS/FRAME   9999 NS/MB   (BEST OF 9)".to_string(),
        "FFMPEG 99.99 MS/FRAME   9999 NS/MB   (BEST OF 9, LESS STARTUP)".to_string(),
        ratio_line.clone(),
        format!("ALL {n} FRAMES PIXEL-IDENTICAL"),
        "SPACE RUN/PAUSE   RIGHT STEP   R RESTART   Q QUIT".to_string(),
    ];
    let mut st_sc = 2usize;
    while st_sc > 1 && widest.iter().any(|l| tw(l, st_sc) + 12 > dw) {
        st_sc -= 1;
    }
    let row_h = 7 * st_sc + 6;
    let strip_h = widest.len() * row_h + 12;
    let dh = cap_h + ph + strip_h;
    let mut buf = vec![0u32; dw * dh];

    eprintln!(
        "layout: window {dw}x{dh}  (3 panels of {pw}x{ph}, source /{step}), caption scale {cap_sc}, stat scale {st_sc}, row {row_h}px  --  pass maxw=N to shrink"
    );
    let mut win = Window::new(
        &format!("rusty_h264 vs ffmpeg - {tier}"),
        dw,
        dh,
        WindowOptions::default(),
    )
    .expect("open window");
    win.set_target_fps(50);

    let (mut i, mut playing) = (0usize, true);
    let mut worst: (u8, usize, usize) = (0, 0, 0); // (max delta, frame, sample count)
    let sx = 6;
    let sy = cap_h + ph + 6;

    while win.is_open() && !win.is_key_down(Key::Escape) && !win.is_key_down(Key::Q) {
        if win.is_key_pressed(Key::Space, minifb::KeyRepeat::No) {
            playing = !playing;
        }
        if win.is_key_pressed(Key::R, minifb::KeyRepeat::No) {
            i = 0;
        }
        let step_once = win.is_key_pressed(Key::Right, minifb::KeyRepeat::Yes);

        buf.fill(0x101418);
        let (cur, ff, maxd, ndiff) = {
            let e = &panels[i];
            (&e.0, &e.1, e.2, e.3)
        };
        // Already shrunk to panel size, so no further subsampling here.
        yuv_to_rgb(cur, &mut buf, dw, dh, 0, cap_h, 1);
        yuv_to_rgb(ff, &mut buf, dw, dh, pw, cap_h, 1);
        // The DRAWN difference is the shrunk one; the VERDICT beside it was
        // computed at full resolution while pairing, so a sub-sampled pixel can
        // never hide a mismatch.
        diff_panel(cur, ff, &mut buf, dw, dh, pw * 2, cap_h, 1);
        if maxd > worst.0 {
            worst = (maxd, i, ndiff);
        }

        // captions — one per panel, on their own row above the images
        text(&mut buf, dw, dh, 6, 4, "RUSTY_H264", 0x66DDFF, cap_sc);
        text(&mut buf, dw, dh, pw + 6, 4, "FFMPEG", 0xFFCC66, cap_sc);
        let (dlabel, dcol) = if ndiff == 0 {
            ("DIFF - IDENTICAL", 0x66FF99)
        } else {
            ("DIFF - MISMATCH", 0xFF4444)
        };
        text(&mut buf, dw, dh, pw * 2 + 6, 4, dlabel, dcol, cap_sc);

        // stats — one full-width row each, in order
        let rows: [(String, u32); 6] = [
            (format!("FRAME {}/{}   {}X{}   {}", i + 1, n, w, h, tier), 0xE8E8E8),
            (
                format!(
                    "OURS {:.2} MS/FRAME   {:.0} NS/MB   (BEST OF {reps})",
                    ours_mean_ms,
                    ours_mean_ms * 1e6 / mbs
                ),
                0x66DDFF,
            ),
            (
                format!(
                    "FFMPEG {:.2} MS/FRAME   {:.0} NS/MB   (BEST OF {reps}, LESS STARTUP)",
                    ff_mean_ms,
                    ff_mean_ms * 1e6 / mbs
                ),
                0xFFCC66,
            ),
            (
                match standing {
                    Some((t, r)) => format!(
                        "RATIO HERE {ratio:.2}X   STANDING PINNED {t} {r:.2}X  (WALL, UNPINNED)"
                    ),
                    None => format!("RATIO HERE {ratio:.2}X  (WALL, UNPINNED)"),
                },
                0xFFAA66,
            ),
            if worst.0 == 0 {
                (format!("ALL {} FRAMES PIXEL-IDENTICAL", i + 1), 0x66FF99)
            } else {
                (
                    format!("WORST DELTA {} AT FRAME {}", worst.0, worst.1 + 1),
                    0xFF4444,
                )
            },
            (
                "SPACE RUN/PAUSE   RIGHT STEP   R RESTART   Q QUIT".to_string(),
                0x8899AA,
            ),
        ];
        for (k, (line, col)) in rows.iter().enumerate() {
            text(&mut buf, dw, dh, sx, sy + k * row_h, line, *col, st_sc);
        }

        win.update_with_buffer(&buf, dw, dh).expect("blit");
        if playing || step_once {
            i = (i + 1) % n;
        }
    }

    // Same summary the `nowin` path prints. Closing the window used to report
    // LESS than the headless run did, which is a good way to have two people
    // quote two different sets of numbers for the same tool.
    println!("frames compared: {n}  ({w}x{h}, {tier})");
    println!("ours   mean {:.2} ms/frame ({:.0} ns/MB)", ours_mean_ms, ours_mean_ms * 1e6 / mbs);
    println!(
        "ffmpeg {:.2} ms/frame ({:.0} ns/MB)  [best of {reps}, less {:.0} ms startup]",
        ff_mean_ms,
        ff_mean_ms * 1e6 / mbs,
        ff_start_ms
    );
    println!("ratio here {ratio:.2}x  (wall, unpinned -- indicative)");
    match standing {
        Some((t, r)) => println!(
            "standing pinned {t} ratio {r:.2}x  (bench/decode_x264_speedtest.sh)"
        ),
        None => println!("standing gap: see docs/big-oppy-decoder.md"),
    }
    if worst.0 == 0 {
        println!("PIXEL-IDENTICAL on all {n} frames");
    } else {
        println!("MISMATCH: worst delta {} at frame {}", worst.0, worst.1 + 1);
        std::process::exit(1);
    }
}
