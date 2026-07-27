//! Quality measurement + the reference decode bar.
//!
//! Both encoders' output is decoded by the SAME external ffmpeg, so neither side
//! is scored by its own reconstruction — but the metric itself is computed here,
//! in-process, frame index against frame index.
//!
//! Why not ffmpeg's `psnr`/`ssim` filters: they pair the two inputs through
//! framesync, i.e. by TIMESTAMP. A raw Annex-B stream carries no container
//! timing, so ffmpeg assumes 25 fps and compares it against a 29.97 fps y4m with
//! the frames misaligned — which shows up as a large, entirely fictitious quality
//! loss. Decoding to raw planes and indexing frame `i` against frame `i` has no
//! such failure mode.

use crate::y4m::Clip;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn ffmpeg() -> String {
    std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// Decode an Annex-B stream to raw I420 with ffmpeg (the neutral reference decoder).
fn decode_to_yuv(stream: &Path) -> Option<Vec<u8>> {
    let o = Command::new(ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !o.status.success() || o.stdout.is_empty() {
        return None;
    }
    Some(o.stdout)
}

fn psnr_from_mse(mse: f64) -> f64 {
    if mse <= 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Mean SSIM over 8×8 non-overlapping luma windows (Wang et al. constants, L=255).
fn ssim_y(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let (mut acc, mut cnt) = (0f64, 0u64);
    let mut by = 0;
    while by + 8 <= h {
        let mut bx = 0;
        while bx + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
            for y in 0..8 {
                for x in 0..8 {
                    let (pa, pb) = (a[(by + y) * w + bx + x] as f64, b[(by + y) * w + bx + x] as f64);
                    sa += pa; sb += pb; saa += pa * pa; sbb += pb * pb; sab += pa * pb;
                }
            }
            let n = 64.0;
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb) = (saa / n - ma * ma, sbb / n - mb * mb);
            let cov = sab / n - ma * mb;
            acc += ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            cnt += 1;
            bx += 8;
        }
        by += 8;
    }
    acc / cnt.max(1) as f64
}

/// (mean per-frame luma PSNR in dB, mean luma SSIM) of `stream` against `src`.
/// Returns `None` if the stream fails to decode or the frame count disagrees —
/// a silent frame-count mismatch would quietly corrupt the metric.
pub fn measure(stream: &Path, src: &Clip) -> Option<(f64, f64)> {
    let raw = decode_to_yuv(stream)?;
    let (w, h) = (src.width, src.height);
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    let n = raw.len() / fsz;
    if n == 0 || n != src.frames.len() {
        eprintln!("    ! decoded {n} frames, source has {} — quality skipped", src.frames.len());
        return None;
    }
    let (mut psnr, mut ssim) = (0.0, 0.0);
    for (i, f) in src.frames.iter().enumerate() {
        let dec = &raw[i * fsz..i * fsz + ys];
        let mse = dec
            .iter()
            .zip(&f.y)
            .map(|(&a, &b)| {
                let d = a as f64 - b as f64;
                d * d
            })
            .sum::<f64>()
            / ys as f64;
        psnr += psnr_from_mse(mse);
        ssim += ssim_y(dec, &f.y, w, h);
    }
    Some((psnr / n as f64, ssim / n as f64))
}

/// Fixed cost of spawning ffmpeg and doing essentially no work, measured once.
///
/// This is NOT a rounding detail: it is tens of milliseconds, which on a QCIF
/// clip is several times the actual decode. Left in, every stream would report
/// the same ~90 Mpx/s — the startup, not the decoder. Our own decoder runs
/// in-process and pays nothing comparable, so the bar has to be net of it.
fn ffmpeg_startup() -> std::time::Duration {
    use std::sync::OnceLock;
    static T: OnceLock<std::time::Duration> = OnceLock::new();
    *T.get_or_init(|| {
        let mut best = std::time::Duration::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let ok = Command::new(ffmpeg())
                .args([
                    "-hide_banner", "-loglevel", "error", "-nostdin",
                    "-f", "lavfi", "-i", "nullsrc=s=16x16", "-frames:v", "1",
                    "-f", "null", "-",
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                best = best.min(t.elapsed());
            }
        }
        if best == std::time::Duration::MAX { std::time::Duration::ZERO } else { best }
    })
}

/// Decode time for ffmpeg's native h264 decoder, single-threaded, **net of
/// process startup**. The reference bar for our decoder.
///
/// NOTE: the shipped ffmpeg is fully stripped (0 symbols), so this is a TOTAL
/// only — no per-function attribution is possible on its side.
pub fn decode_wall(stream: &Path, reps: usize) -> Option<std::time::Duration> {
    let mut best = std::time::Duration::MAX;
    for _ in 0..reps {
        let t = std::time::Instant::now();
        let o = Command::new(ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(stream)
            .args(["-f", "null", "-"])
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        best = best.min(t.elapsed());
    }
    Some(best.saturating_sub(ffmpeg_startup()).max(std::time::Duration::from_micros(1)))
}
