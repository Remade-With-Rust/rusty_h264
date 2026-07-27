//! Function-level speed analyzer — rusty_h264 vs x264, on the fixed `video-tests`
//! corpus, across the full preset ladder, encoder AND decoder.
//!
//! Three sub-commands, because throughput and per-function breakdown cannot be
//! measured by the same binary (the profiler's rdtsc scopes inflate wall time —
//! see `rusty_h264-common::prof`):
//!
//! ```text
//! cargo run --release -- speed     # profiler OFF  -> results/speed.tsv
//! cargo run --release --features profile -- stages # profiler ON -> results/stages.tsv
//! cargo run --release -- report    # merge both    -> results/REPORT.md
//! ```
//!
//! `run_analysis.sh` drives all three. Everything is deterministic: same clips,
//! same frame counts, same QP/keyint, best-of-N, single-threaded on both sides.

mod ours;
mod quality;
mod report;
mod x264;
mod y4m;

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------

pub struct Config {
    pub qp: u8,
    pub keyint: u32,
    /// Best-of-N repetitions for every timed measurement.
    pub reps: usize,
    /// Passes for the median-of-N stage profile.
    pub profile_passes: usize,
}

pub static CFG: Config = Config { qp: 26, keyint: 60, reps: 3, profile_passes: 5 };

/// One measured (clip × codec × preset × arm × encode|decode) result.
#[derive(Clone)]
pub struct Row {
    pub clip: String,
    pub class: String,
    pub width: usize,
    pub height: usize,
    pub frames: usize,
    pub codec: String,
    pub preset: String,
    pub arm: String,
    pub kind: String,
    pub wall_ms: f64,
    pub fps: f64,
    pub mpx_s: f64,
    pub bytes: usize,
    pub kbps: f64,
    pub psnr: f64,
    pub ssim: f64,
}

/// One stage/function bucket from either encoder's profiler.
#[derive(Clone)]
pub struct StageRow {
    pub clip: String,
    pub codec: String,
    pub preset: String,
    pub arm: String,
    pub kind: String,
    pub stage: String,
    pub nested_in: String,
    pub ms: f64,
    pub calls: u64,
    pub pct: f64,
}

// ---------------------------------------------------------------------------

pub struct ClipEntry {
    pub name: String,
    pub class: String,
    pub frames: usize,
    pub path: PathBuf,
}

fn root() -> PathBuf {
    // <repo>/video-tests/analyzer/  ->  <repo>/video-tests/
    let mut p = std::env::current_dir().expect("cwd");
    if p.ends_with("analyzer") {
        p.pop();
    }
    p
}

fn manifest() -> Vec<ClipEntry> {
    let base = root();
    let txt = std::fs::read_to_string(base.join("manifest.tsv"))
        .unwrap_or_else(|e| panic!("read manifest.tsv: {e} (run video-tests/fetch_clips.sh first)"));
    let filter: Vec<String> = std::env::var("CLIPS")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    txt.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 6 {
                return None;
            }
            let name = f[0].to_string();
            if !filter.is_empty() && !filter.contains(&name) {
                return None;
            }
            let path = base.join("clips").join(format!("{name}.y4m"));
            if !path.exists() {
                eprintln!("  ! missing clip {name} — skipped");
                return None;
            }
            Some(ClipEntry { name, class: f[5].to_string(), frames: f[4].parse().unwrap_or(0), path })
        })
        .collect()
}

fn results_dir() -> PathBuf {
    let d = root().join("results");
    std::fs::create_dir_all(&d).expect("create results dir");
    d
}

fn scratch() -> PathBuf {
    let d = root().join("results").join("_tmp");
    std::fs::create_dir_all(&d).expect("create scratch dir");
    d
}

// ---------------------------------------------------------------------------

fn write_rows(path: &Path, rows: &[Row]) {
    let mut s = String::from(
        "clip\tclass\twidth\theight\tframes\tcodec\tpreset\tarm\tkind\twall_ms\tfps\tmpx_s\tbytes\tkbps\tpsnr\tssim\n",
    );
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{:.3}\t{}\t{:.1}\t{:.3}\t{:.5}\n",
            r.clip, r.class, r.width, r.height, r.frames, r.codec, r.preset, r.arm, r.kind,
            r.wall_ms, r.fps, r.mpx_s, r.bytes, r.kbps, r.psnr, r.ssim
        ));
    }
    std::fs::write(path, s).expect("write rows");
    eprintln!("wrote {}", path.display());
}

fn write_stages(path: &Path, rows: &[StageRow]) {
    let mut s = String::from("clip\tcodec\tpreset\tarm\tkind\tstage\tnested_in\tms\tcalls\tns_per_call\tpct\n");
    for r in rows {
        let nspc = if r.calls > 0 { r.ms * 1e6 / r.calls as f64 } else { 0.0 };
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{:.1}\t{:.2}\n",
            r.clip, r.codec, r.preset, r.arm, r.kind, r.stage, r.nested_in, r.ms, r.calls, nspc, r.pct
        ));
    }
    std::fs::write(path, s).expect("write stages");
    eprintln!("wrote {}", path.display());
}

// ---------------------------------------------------------------------------

fn cmd_speed() {
    if cfg!(feature = "profile") {
        eprintln!("!! built WITH the `profile` feature — throughput numbers would be inflated.");
        eprintln!("   Build without it for `speed`. Aborting.");
        std::process::exit(2);
    }
    let clips = manifest();
    let tmp = scratch();
    let mut rows: Vec<Row> = Vec::new();

    for c in &clips {
        eprintln!("\n=== {} ({}) ===", c.name, c.class);
        let clip = match y4m::read(&c.path, 0) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  ! {e}");
                continue;
            }
        };
        eprintln!("  {}x{} x{} frames @ {:.2} fps", clip.width, clip.height, clip.frames.len(), clip.fps_f64());

        // ---- ours: encode at every preset, then decode our own stream --------
        for (pname, preset) in ours::PRESETS {
            let (wall, bits) = ours::encode_speed(&clip, preset, CFG.reps);
            let stream = tmp.join(format!("{}.ours-{pname}.264", c.name));
            std::fs::write(&stream, &bits).ok();
            let mut r = ours::row(&clip, &c.name, &c.class, pname, "encode", wall, bits.len());
            if let Some((p, s)) = quality::measure(&stream, &clip) {
                r.psnr = p;
                r.ssim = s;
            }
            eprintln!(
                "  ours/{pname:<8} enc {:>8.1} ms  {:>7.2} fps  {:>7.3} Mpx/s  {:>6.0} kb/s  {:.2} dB",
                r.wall_ms, r.fps, r.mpx_s, r.kbps, r.psnr
            );
            rows.push(r);

            // our decoder on our own stream
            match ours::decode_speed(&bits, CFG.reps) {
                Ok((w, n)) => {
                    let mut d = ours::row(&clip, &c.name, &c.class, &format!("ours-{pname}"), "decode", w, bits.len());
                    d.frames = n;
                    eprintln!("  ours/dec         {:>8.1} ms  {:>7.2} fps  {:>7.3} Mpx/s", d.wall_ms, d.fps, d.mpx_s);
                    rows.push(d);
                }
                Err(e) => eprintln!("  ours/dec         FAILED: {e}"),
            }
            // ffmpeg decoding the same stream — the reference decode bar
            if let Some(w) = quality::decode_wall(&stream, CFG.reps) {
                let secs = w.as_secs_f64();
                rows.push(Row {
                    codec: "ffmpeg".into(),
                    preset: format!("ours-{pname}"),
                    kind: "decode".into(),
                    wall_ms: secs * 1e3,
                    fps: clip.frames.len() as f64 / secs,
                    mpx_s: clip.pixels() as f64 / secs / 1e6,
                    ..ours::row(&clip, &c.name, &c.class, "", "decode", w, bits.len())
                });
            }
        }

        // ---- x264: the whole ladder, both feature arms -----------------------
        for (arm, arm_args) in x264::ARMS {
            for p in x264::PRESETS {
                let out = tmp.join(format!("{}.x264-{arm}-{p}.264", c.name));
                match x264::encode(&c.path, &out, p, arm_args, false) {
                    Ok(run) => {
                        let mut r = x264::row(
                            &c.name, &c.class, clip.width, clip.height, clip.frames.len(),
                            clip.fps_f64(), p, arm, &run,
                        );
                        if let Some((ps, ss)) = quality::measure(&out, &clip) {
                            r.psnr = ps;
                            r.ssim = ss;
                        }
                        eprintln!(
                            "  x264/{arm:<8}/{p:<9} enc {:>8.1} ms  {:>7.2} fps  {:>7.3} Mpx/s  {:>6.0} kb/s  {:.2} dB",
                            r.wall_ms, r.fps, r.mpx_s, r.kbps, r.psnr
                        );
                        rows.push(r);

                        // our decoder against x264's bitstream — the conformance-side
                        // decode comparison (baseline arm is in our supported subset).
                        if let Ok(bits) = std::fs::read(&out) {
                            if let Ok((w, n)) = ours::decode_speed(&bits, CFG.reps) {
                                let secs = w.as_secs_f64();
                                rows.push(Row {
                                    preset: format!("x264-{arm}-{p}"),
                                    kind: "decode".into(),
                                    frames: n,
                                    wall_ms: secs * 1e3,
                                    fps: n as f64 / secs,
                                    mpx_s: (clip.width * clip.height * n) as f64 / secs / 1e6,
                                    bytes: bits.len(),
                                    ..ours::row(&clip, &c.name, &c.class, "", "decode", w, bits.len())
                                });
                            }
                            if let Some(w) = quality::decode_wall(&out, CFG.reps) {
                                let secs = w.as_secs_f64();
                                rows.push(Row {
                                    codec: "ffmpeg".into(),
                                    preset: format!("x264-{arm}-{p}"),
                                    kind: "decode".into(),
                                    wall_ms: secs * 1e3,
                                    fps: clip.frames.len() as f64 / secs,
                                    mpx_s: clip.pixels() as f64 / secs / 1e6,
                                    ..ours::row(&clip, &c.name, &c.class, "", "decode", w, run.bytes)
                                });
                            }
                        }
                    }
                    Err(e) => eprintln!("  x264/{arm}/{p}: {e}"),
                }
                let _ = std::fs::remove_file(&out);
            }
        }
    }
    write_rows(&results_dir().join("speed.tsv"), &rows);
}

fn cmd_stages() {
    if !cfg!(feature = "profile") {
        eprintln!("!! built WITHOUT the `profile` feature — every stage bucket would read 0.");
        eprintln!("   Rebuild with --features profile. Aborting.");
        std::process::exit(2);
    }
    let clips = manifest();
    let tmp = scratch();
    let mut rows: Vec<StageRow> = Vec::new();

    for c in &clips {
        eprintln!("\n=== stages: {} ===", c.name);
        let clip = match y4m::read(&c.path, 0) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  ! {e}");
                continue;
            }
        };

        // The stage medians are stable well before 5 passes, and each pass is a
        // full encode — at 1080p that is tens of seconds. Spend the passes where
        // they buy stability (small clips, noisy short runs) and not where they
        // only buy wall time.
        let passes = if clip.pixels() > 60_000_000 { 3 } else { CFG.profile_passes };

        for (pname, preset) in ours::PRESETS {
            // encode breakdown
            let s = ours::profile_median(
                || {
                    let (_, _) = ours::encode_speed(&clip, preset, 1);
                },
                passes,
            );
            rows.extend(ours::stage_rows(&c.name, pname, "encode", &s));

            // decode breakdown of our own stream
            let (_, bits) = ours::encode_speed(&clip, preset, 1);
            let s = ours::profile_median(
                || {
                    let _ = ours::decode_speed(&bits, 1);
                },
                passes,
            );
            rows.extend(ours::stage_rows(&c.name, &format!("ours-{pname}"), "decode", &s));
            eprintln!("  ours/{pname}: encode + decode stages captured");
        }

        for (arm, arm_args) in x264::ARMS {
            for p in x264::PRESETS {
                let out = tmp.join(format!("{}.x264p-{arm}-{p}.264", c.name));
                match x264::encode(&c.path, &out, p, arm_args, true) {
                    Ok(run) => {
                        rows.extend(x264::stage_rows(&c.name, p, arm, &run));
                        eprintln!("  x264/{arm}/{p}: {} stage buckets", run.stages.len());
                    }
                    Err(e) => eprintln!("  x264/{arm}/{p}: {e}"),
                }
                let _ = std::fs::remove_file(&out);
            }
        }
    }
    write_stages(&results_dir().join("stages.tsv"), &rows);
}

// ---------------------------------------------------------------------------

/// Bitstream digest per (clip × preset) — the byte-identical gate for encoder
/// changes that are supposed to be behaviour-preserving. Deblocking in particular
/// feeds the reference frames, so any real behavioural change moves these hashes.
fn cmd_hash() {
    for c in &manifest() {
        let Ok(clip) = y4m::read(&c.path, 0) else { continue };
        for (pname, preset) in ours::PRESETS {
            let (_, bits) = ours::encode_speed(&clip, preset, 1);
            // FNV-1a 64 — no dependency, and collision risk is irrelevant for an
            // A/B of the same encoder before and after a refactor.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in &bits {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            println!("{:<24} {:<8} {:016x}  {} bytes", c.name, pname, h, bits.len());
        }
    }
}

/// Interleaved A/B of the two boundary-strength arms on REAL content, the
/// deployment-truth verdict for deblocking work. Both arms run in ONE process,
/// alternating pass by pass, because separate runs drift ~20% on this machine —
/// more than the effect being measured.
#[cfg(feature = "profile")]
fn cmd_dbstats() {
    eprintln!("accel (SIMD kernels compiled in): {}", rusty_h264_common::ACCEL);
    for c in &manifest() {
        let Ok(clip) = y4m::read(&c.path, 0) else { continue };
        let _mbs = ((clip.width / 16) * (clip.height / 16) * clip.frames.len()).max(1) as f64;
        for (pname, preset) in ours::PRESETS {
            // Arm 0 = boundary strengths precomputed in the ENCODE loop (phase 1);
            // arm 1 = derived inside the filter (phase 2). Both TOTALs are taken,
            // because phase 1 only moves work — the deblock stage alone would
            // flatter it. Alternated pass by pass under one thermal state.
            let mut arm_ns = [Vec::new(), Vec::new()];
            let mut arm_tot = [Vec::new(), Vec::new()];
            let mut arm_loop = [Vec::new(), Vec::new()];
            for pass in 0..8 {
                let precomp = pass % 2 == 0;
                rusty_h264_common::deblock::set_precomputed_bs(precomp);
                rusty_h264_common::prof::reset();
                let (_, _) = ours::encode_speed(&clip, preset, 1);
                let snap = rusty_h264_common::prof::snapshot();
                let ms = snap[rusty_h264_common::prof::Stage::Deblock as usize].0;
                let tot = snap[rusty_h264_common::prof::Stage::Total as usize].0;
                let loop_ms = snap[rusty_h264_common::prof::Stage::EncMbLoop as usize].0;
                let a = if precomp { 0 } else { 1 };
                arm_ns[a].push(ms);
                arm_tot[a].push(tot);
                arm_loop[a].push(loop_ms);
            }
            let med = |v: &mut Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            let (dp, dd) = (med(&mut arm_ns[0]), med(&mut arm_ns[1]));
            let (tp, td) = (med(&mut arm_tot[0]), med(&mut arm_tot[1]));
            let (lp, ld) = (med(&mut arm_loop[0]), med(&mut arm_loop[1]));
            // Where did the deblock saving go? If the encode loop grows by what
            // deblocking shed, the work moved rather than disappeared.
            println!(
                "{:<20} {:<8} deblock {:>7.1}->{:>7.1} ({:+6.1})   mb-loop {:>7.1}->{:>7.1} ({:+6.1})   TOTAL {:>7.1}->{:>7.1} ({:+6.1} ms)",
                c.name, pname, dd, dp, dp - dd, ld, lp, lp - ld, td, tp, tp - td
            );
            rusty_h264_common::deblock::set_precomputed_bs(true);
        }
    }
}
#[cfg(not(feature = "profile"))]
fn cmd_dbstats() {
    eprintln!("dbstats needs --features profile");
}

/// Macroblock-type distribution per clip — the physical diagnostic for why a
/// content class codes badly. Skip / coded-inter / intra shares come from the
/// profiler's call counts, which are exact (not timings), so they are immune to
/// the thermal drift that makes wall-clock unreliable here.
#[cfg(feature = "profile")]
fn cmd_mbstats() {
    use rusty_h264_common::prof;
    println!("{:<24} {:>8} {:>9} {:>9} {:>9} {:>10} {:>9}",
             "clip", "preset", "skip%", "inter%", "intra%", "kb/s", "Mpx/s");
    for c in &manifest() {
        let Ok(clip) = y4m::read(&c.path, 0) else { continue };
        for (pname, preset) in ours::PRESETS {
            prof::reset();
            let (_, bits) = ours::encode_speed(&clip, preset, 1);
            let s = prof::snapshot();
            let g = |st: prof::Stage| s[st as usize].1 as f64;
            // EncSkip fires once per P macroblock; EncIntraCode fires for I-FRAME
            // macroblocks too, so subtract those or the skip count is corrupted
            // (it was, and clamped to 0 — which read as "we never skip").
            let mbs_per_frame = ((clip.width + 15) / 16 * ((clip.height + 15) / 16)) as f64;
            let idr_frames = (clip.frames.len() as f64 / CFG.keyint as f64).ceil().max(1.0);
            let tested = g(prof::Stage::EncSkip).max(1.0);
            let inter = g(prof::Stage::EncInterCode);
            let intra = (g(prof::Stage::EncIntraCode) - mbs_per_frame * idr_frames).max(0.0);
            let skip = (tested - inter - intra).max(0.0);
            let secs = clip.frames.len() as f64 / clip.fps_f64();
            // profiler ON inflates wall time; take a separate profiler-free timing
            let (wall, _) = ours::encode_speed(&clip, preset, 2);
            let mpxs = clip.pixels() as f64 / wall.as_secs_f64() / 1e6;
            println!("{:<24} {:>8} {:>8.1}% {:>8.1}% {:>8.1}% {:>10.0} {:>9.1}",
                     c.name, pname,
                     100.0 * skip / tested, 100.0 * inter / tested, 100.0 * intra / tested,
                     bits.len() as f64 * 8.0 / secs / 1e3, mpxs);
        }
    }
}
#[cfg(not(feature = "profile"))]
fn cmd_mbstats() { eprintln!("mbstats needs --features profile"); }

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match mode.as_str() {
        "dbstats" => cmd_dbstats(),
        "mbstats" => cmd_mbstats(),
        "speed" => cmd_speed(),
        "stages" => cmd_stages(),
        "hash" => cmd_hash(),
        "report" => report::generate(&results_dir()),
        _ => {
            eprintln!("usage: analyzer <speed|stages|report>");
            eprintln!("  speed   profiler OFF — throughput/size/quality across the ladder");
            eprintln!("  stages  profiler ON  — per-function breakdown (build --features profile)");
            eprintln!("  report  merge results/*.tsv into results/REPORT.md");
            eprintln!("\nenv: CLIPS=a,b restrict the corpus; X264_BIN / X264_PROF_BIN / FFMPEG override binaries");
            std::process::exit(1);
        }
    }
}
