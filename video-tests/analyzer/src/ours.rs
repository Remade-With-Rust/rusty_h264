//! The rusty_h264 side: encode/decode the corpus in-process, with the stage
//! profiler either off (honest throughput) or on (per-function breakdown).

use crate::y4m::Clip;
use crate::{Row, StageRow, CFG};
use rusty_h264_common::prof;
use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

pub const PRESETS: [(&str, Preset); 3] = [
    ("fast", Preset::Fast),
    ("balanced", Preset::Balanced),
    ("quality", Preset::Quality),
];

fn config(clip: &Clip, preset: Preset) -> EncoderConfig {
    let mut cfg = EncoderConfig::new(clip.width, clip.height);
    cfg.qp = CFG.qp;
    cfg.gop_size = CFG.keyint;
    cfg.framerate = clip.fps_f64() as f32;
    cfg.preset = preset;
    // Oracle knob: force sub-pel refinement in the fast preset (RS_SUBPEL=1).
    cfg.tune_subpel = std::env::var_os("RS_SUBPEL").is_some();
    cfg.tune_rd_skip = std::env::var_os("RS_RDSKIP").is_some();
    cfg
}

/// Best-of-N encode. Returns (best wall, bitstream from the best run).
pub fn encode_speed(clip: &Clip, preset: Preset, reps: usize) -> (std::time::Duration, Vec<u8>) {
    let mut best = std::time::Duration::MAX;
    let mut out = Vec::new();
    for _ in 0..reps {
        let mut enc = Encoder::new(config(clip, preset)).expect("encoder init");
        let mut bits = Vec::new();
        let t = std::time::Instant::now();
        for f in &clip.frames {
            bits.extend_from_slice(&enc.encode(f));
        }
        let e = t.elapsed();
        if e < best {
            best = e;
            out = bits;
        }
    }
    (best, out)
}

/// Best-of-N decode of an Annex-B stream with OUR decoder.
pub fn decode_speed(stream: &[u8], reps: usize) -> Result<(std::time::Duration, usize), String> {
    let mut best = std::time::Duration::MAX;
    let mut n = 0;
    for _ in 0..reps {
        let t = std::time::Instant::now();
        let frames = Decoder::new().decode_stream(stream).map_err(|e| format!("{e:?}"))?;
        let e = t.elapsed();
        n = frames.len();
        best = best.min(e);
    }
    Ok((best, n))
}

/// One profiled pass (needs the `profile` feature to produce non-zero buckets).
/// Returns the per-stage `(ms, calls)` snapshot, median over `passes` runs.
pub fn profile_median(mut run: impl FnMut(), passes: usize) -> [(f64, u64); prof::N] {
    let mut per: Vec<[(f64, u64); prof::N]> = Vec::new();
    for _ in 0..passes {
        prof::reset();
        run();
        per.push(prof::snapshot());
    }
    let mut out = [(0.0f64, 0u64); prof::N];
    for (i, o) in out.iter_mut().enumerate() {
        let mut ms: Vec<f64> = per.iter().map(|s| s[i].0).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        *o = (ms[passes / 2], per[passes / 2][i].1);
    }
    out
}

/// Turn a profiler snapshot into report rows. `Total` is the denominator and the
/// residue (`Total − Σ disjoint stages`) is emitted as `mgmt/other`, which is
/// where unnamed per-MB glue hides — the line the analyzer skill says to chase.
///
/// Which buckets form the DISJOINT partition depends on the direction, and this
/// matters: summing the wrong set double-counts and drives the residue to zero.
///   * encode — the `Enc*` stages (14..=20) partition `encode()`; every other
///     bucket (inter-mc, entropy, reconstruct, deblock, T/Q, …) NESTS inside one
///     of them and is within-stage detail.
///   * decode — buckets 0..=13 partition `decode()`; no `Enc*` bucket fires.
pub fn stage_rows(clip: &str, preset: &str, kind: &str, s: &[(f64, u64); prof::N]) -> Vec<StageRow> {
    let total_i = prof::Stage::Total as usize;
    // Deblock is NOT covered by the Enc* partition: verified empirically against
    // the repo's own `profile_encode_stages` dump, where enc-finalize measures
    // 0.5 ms while deblock measures 14.5 ms over the same 60 calls. It runs as a
    // top-level per-frame pass, so it joins the disjoint set for encode.
    let is_encode = kind == "encode";
    let disjoint: std::ops::RangeInclusive<usize> = if is_encode {
        (prof::Stage::EncSource as usize)..=(prof::Stage::EncFinal as usize)
    } else {
        0..=(prof::Stage::DpbClone as usize)
    };
    // Also top-level during encode, outside the Enc* range: the frame deblock pass
    // and NAL assembly. EncPrep/EncMbLoop are INFO — they CONTAIN the stages above,
    // so counting them would double-count and drive the residue to zero.
    let extra_disjoint = [
        prof::Stage::Deblock as usize,
        prof::Stage::EncNal as usize,
        prof::Stage::EncEmit as usize,
    ];
    let total = s[total_i].0.max(1e-9);
    let mut rows = Vec::new();
    let mut sum = 0.0;
    let mut all_calls: u64 = 0;
    for i in 0..total_i {
        if s[i].1 == 0 {
            continue;
        }
        all_calls += s[i].1;
        let is_disjoint = disjoint.contains(&i) || (is_encode && extra_disjoint.contains(&i));
        if is_disjoint {
            sum += s[i].0;
        }
        rows.push(StageRow {
            clip: clip.into(),
            codec: "ours".into(),
            preset: preset.into(),
            arm: "-".into(),
            kind: kind.into(),
            stage: prof::name(i).into(),
            nested_in: if is_disjoint { "-".into() } else { "nested".into() },
            ms: s[i].0,
            calls: s[i].1,
            pct: 100.0 * s[i].0 / total,
        });
    }
    rows.push(StageRow {
        clip: clip.into(),
        codec: "ours".into(),
        preset: preset.into(),
        arm: "-".into(),
        kind: kind.into(),
        stage: "mgmt/other".into(),
        nested_in: "-".into(),
        ms: (total - sum).max(0.0),
        calls: 0,
        pct: 100.0 * (total - sum).max(0.0) / total,
    });
    // The residue is only interpretable next to what the profiler itself costs:
    // ~2 rdtsc reads per scope entry, and the encode path opens ~1M of them. The
    // analyzer methodology says to compute this explicitly — when the residue
    // matches the overhead, there is no hidden work left, only the measurement.
    rows.push(StageRow {
        clip: clip.into(),
        codec: "ours".into(),
        preset: preset.into(),
        arm: "-".into(),
        kind: kind.into(),
        stage: "profiler-overhead(est)".into(),
        nested_in: "info".into(),
        ms: all_calls as f64 * scope_ns() / 1e6,
        calls: all_calls,
        pct: 100.0 * (all_calls as f64 * scope_ns() / 1e6) / total,
    });
    rows.push(StageRow {
        clip: clip.into(),
        codec: "ours".into(),
        preset: preset.into(),
        arm: "-".into(),
        kind: kind.into(),
        stage: "TOTAL".into(),
        nested_in: "-".into(),
        ms: total,
        calls: s[total_i].1,
        pct: 100.0,
    });
    rows
}

/// Self-calibrated cost of ONE profiler scope (enter + drop), in ns. Measured
/// once per process against an empty scope loop, so the overhead estimate tracks
/// this machine rather than a hardcoded guess.
pub fn scope_ns() -> f64 {
    use std::sync::OnceLock;
    static NS: OnceLock<f64> = OnceLock::new();
    *NS.get_or_init(|| {
        const N: u64 = 4_000_000;
        // Baseline: the empty loop itself.
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(0u64);
        }
        let base = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        for _ in 0..N {
            let g = prof::scope(prof::Stage::Total);
            std::hint::black_box(&g);
        }
        let with = t.elapsed().as_secs_f64();
        prof::reset(); // don't leave the calibration in the buckets
        ((with - base) * 1e9 / N as f64).max(0.0)
    })
}

pub fn row(clip: &Clip, name: &str, class: &str, preset: &str, kind: &str, wall: std::time::Duration, bytes: usize) -> Row {
    let secs = wall.as_secs_f64().max(1e-9);
    Row {
        clip: name.into(),
        class: class.into(),
        width: clip.width,
        height: clip.height,
        frames: clip.frames.len(),
        codec: "ours".into(),
        preset: preset.into(),
        arm: "-".into(),
        kind: kind.into(),
        wall_ms: secs * 1e3,
        fps: clip.frames.len() as f64 / secs,
        mpx_s: clip.pixels() as f64 / secs / 1e6,
        bytes,
        kbps: bytes as f64 * 8.0 / (clip.frames.len() as f64 / clip.fps_f64()) / 1e3,
        psnr: f64::NAN,
        ssim: f64::NAN,
    }
}
