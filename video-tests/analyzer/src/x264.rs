//! The x264 side: drive the external reference binaries built in `_ref_x264`.
//!
//! Two binaries, deliberately:
//!   * `x264.exe`      — stock, zero instrumentation → the honest throughput arm.
//!   * `x264-prof.exe` — rdtsc stage taps (`-DX264_PROF`) → the breakdown arm.
//! Measuring speed on the instrumented build would tax x264 with overhead our own
//! profiler-off build does not pay.

use crate::{Row, StageRow, CFG};
use std::path::{Path, PathBuf};
use std::process::Command;

/// x264's full speed ladder, slowest-changing knob first in each preset.
pub const PRESETS: [&str; 10] = [
    "ultrafast", "superfast", "veryfast", "faster", "fast",
    "medium", "slow", "slower", "veryslow", "placebo",
];

/// Feature arms. `high` is stock x264 (its real default: CABAC, B-frames, 8x8
/// transform, weighted pred). `baseline` clamps it to the toolset rusty_h264
/// actually implements by default, which is the apples-to-apples implementation
/// comparison — otherwise we are comparing feature sets, not code.
pub const ARMS: [(&str, &[&str]); 2] = [
    ("high", &[]),
    ("baseline", &["--profile", "baseline"]),
];

pub fn bin(prof: bool) -> PathBuf {
    let key = if prof { "X264_PROF_BIN" } else { "X264_BIN" };
    if let Ok(p) = std::env::var(key) {
        return PathBuf::from(p);
    }
    let name = if prof { "x264-prof.exe" } else { "x264.exe" };
    PathBuf::from(format!("../../../_ref_x264/{name}"))
}

pub struct Run {
    /// Whole-process wall clock, INCLUDING startup/init/teardown.
    pub wall: std::time::Duration,
    /// x264's own reported encode-loop time, derived from the fps it prints.
    /// This is the number to compare against our in-process encode loop: process
    /// startup is ~10–20 ms here, which would swamp a 25 ms QCIF encode and make
    /// x264 look several times slower than it is.
    pub encode: std::time::Duration,
    pub bytes: usize,
    /// Frames x264 reported encoding (sanity-checks the y4m was consumed whole).
    pub frames: usize,
    pub stages: Vec<(String, String, f64, u64)>, // (stage, nested_in, ms, calls)
}

/// Encode one clip. `prof` selects the instrumented binary and harvests its taps.
pub fn encode(
    clip_path: &Path,
    out: &Path,
    preset: &str,
    arm_args: &[&str],
    prof: bool,
) -> Result<Run, String> {
    let exe = bin(prof);
    let prof_out = out.with_extension("prof.tsv");
    let qp = CFG.qp.to_string();
    let keyint = CFG.keyint.to_string();

    let mut cmd = Command::new(&exe);
    cmd.args(["--threads", "1", "--preset", preset, "--qp", &qp, "--keyint", &keyint])
        .args(arm_args)
        .arg("-o")
        .arg(out)
        .arg(clip_path);
    if prof {
        cmd.env("X264_PROF_OUT", &prof_out);
    } else {
        cmd.env_remove("X264_PROF_OUT");
    }

    let t = std::time::Instant::now();
    let o = cmd.output().map_err(|e| format!("spawn {}: {e}", exe.display()))?;
    let wall = t.elapsed();
    if !o.status.success() {
        return Err(format!(
            "x264 {preset} failed: {}",
            String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("?")
        ));
    }
    // "encoded 120 frames, 1720.98 fps, 43.61 kb/s"
    let log = String::from_utf8_lossy(&o.stderr).into_owned();
    let tail = log.rsplit("encoded ").next().unwrap_or("");
    let mut tok = tail.split_whitespace();
    let frames: usize = tok.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let self_fps: f64 = tok.nth(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let encode = if self_fps > 0.0 && frames > 0 {
        std::time::Duration::from_secs_f64(frames as f64 / self_fps)
    } else {
        wall
    };

    let stages = if prof {
        let txt = std::fs::read_to_string(&prof_out).unwrap_or_default();
        let _ = std::fs::remove_file(&prof_out);
        txt.lines()
            .filter(|l| l.starts_with("x264prof\t"))
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                if f.len() < 6 {
                    return None;
                }
                Some((
                    f[1].to_string(),
                    f[2].to_string(),
                    f[5].parse().ok()?,
                    f[4].parse().ok()?,
                ))
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(Run {
        wall,
        encode,
        bytes: std::fs::metadata(out).map(|m| m.len() as usize).unwrap_or(0),
        frames,
        stages,
    })
}

pub fn row(
    name: &str, class: &str, w: usize, h: usize, nframes: usize, fps: f64,
    preset: &str, arm: &str, r: &Run,
) -> Row {
    // Encode-loop time, not process wall — see `Run::encode`.
    let secs = r.encode.as_secs_f64().max(1e-9);
    Row {
        clip: name.into(),
        class: class.into(),
        width: w,
        height: h,
        frames: nframes,
        codec: "x264".into(),
        preset: preset.into(),
        arm: arm.into(),
        kind: "encode".into(),
        wall_ms: secs * 1e3,
        fps: nframes as f64 / secs,
        mpx_s: (w * h * nframes) as f64 / secs / 1e6,
        bytes: r.bytes,
        kbps: r.bytes as f64 * 8.0 / (nframes as f64 / fps) / 1e3,
        psnr: f64::NAN,
        ssim: f64::NAN,
    }
}

pub fn stage_rows(clip: &str, preset: &str, arm: &str, r: &Run) -> Vec<StageRow> {
    let total = r
        .stages
        .iter()
        .find(|(s, ..)| s == "TOTAL")
        .map(|(_, _, ms, _)| *ms)
        .unwrap_or(0.0)
        .max(1e-9);
    // The residue: TOTAL minus every stage that is not nested inside another and
    // is not TOTAL itself — x264's per-MB glue, ratecontrol, DPB, bitstream mgmt.
    let named: f64 = r
        .stages
        .iter()
        .filter(|(s, nest, ..)| s != "TOTAL" && s != "_WALL" && nest.is_empty())
        .map(|(_, _, ms, _)| *ms)
        .sum();
    let mut rows: Vec<StageRow> = r
        .stages
        .iter()
        .filter(|(s, ..)| s != "_WALL")
        .map(|(stage, nest, ms, calls)| StageRow {
            clip: clip.into(),
            codec: "x264".into(),
            preset: preset.into(),
            arm: arm.into(),
            kind: "encode".into(),
            stage: stage.clone(),
            nested_in: if nest.is_empty() { "-".into() } else { nest.clone() },
            ms: *ms,
            calls: *calls,
            pct: 100.0 * ms / total,
        })
        .collect();
    rows.push(StageRow {
        clip: clip.into(),
        codec: "x264".into(),
        preset: preset.into(),
        arm: arm.into(),
        kind: "encode".into(),
        stage: "mgmt/other".into(),
        nested_in: "-".into(),
        ms: (total - named).max(0.0),
        calls: 0,
        pct: 100.0 * (total - named).max(0.0) / total,
    });
    rows
}
