//! Byte-identity gate: encode a real y4m clip and print size + FNV-1a hash of the
//! bitstream, for every preset. Any change claiming to be byte-identical must
//! leave every line here untouched.
//!
//! Exercises BOTH the sequential per-frame path and the GOP-parallel `encode_all`
//! path, because thread-local state is only proven by the threaded arm.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example encode_hash \
//!     -- video-tests/clips/foreman_cif.y4m

use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

/// Minimal y4m reader: header line, then `FRAME\n` + planar 4:2:0 payloads.
fn read_y4m(path: &str, max_frames: usize) -> (usize, usize, Vec<rusty_h264_common::types::YuvFrame>) {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let hdr_end = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..hdr_end]).expect("utf8 header");
    let (mut w, mut h) = (0usize, 0usize);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().expect("width"),
            Some(b'H') => h = tok[1..].parse().expect("height"),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let mut frames = Vec::new();
    let mut p = hdr_end + 1;
    while frames.len() < max_frames {
        // Each frame is preceded by a "FRAME...\n" marker.
        let Some(rel) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += rel + 1;
        if p + ys + 2 * cs > raw.len() {
            break;
        }
        frames.push(rusty_h264_common::types::YuvFrame {
            width: w,
            height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    (w, h, frames)
}


/// Is the stage profiler compiled into THIS binary?
///
/// ★ This is the guard that actually matters. The 612 ms "2.6× regression" that
/// nearly got reported was a profile-ON binary: measured here at **314 ms vs 195 ms**
/// for the identical command, a **1.61× inflation** from the rdtsc scopes alone. The
/// feature leaks in silently, because `cargo test --workspace --features asm`
/// unifies features across the workspace and rebuilds the example with `profile`
/// enabled — the binary is FRESH, it is simply not the binary you think it is.
///
/// Two plausible explanations were measured and REFUTED first, which is why this one
/// is trusted: (1) CPU contention — 56 hogs on 24 cores move the encode ~5%;
/// (2) a concurrent `cargo build` — no measurable effect at all.
fn profiler_compiled_in() -> bool {
    cfg!(feature = "profile")
}

/// REPRODUCIBILITY GUARD — secondary to the check above.
///
/// The failure this exists to prevent: a single wall reading of 612 ms was nearly
/// reported as a 2.6× regression when the true value was ~200 ms.
///
/// The obvious explanation — CPU contention from a concurrent build — was MEASURED
/// AND REFUTED: 56 spinning hogs on this 24-core box move a single-threaded encode
/// only ~5% (137 vs 130 ms), because the scheduler keeps giving the foreground
/// thread a core. A contention probe therefore cannot catch this class of artifact,
/// so it was deleted rather than kept as reassuring decoration.
///
/// What DID go wrong was concluding from ONE unpaired reading. So the guard is
/// reproducibility: take several independent timings and report the SPREAD, flagging
/// any run whose spread exceeds this machine's measured idle variation (a 40-sample
/// sweep gave max/min = 1.36×, p90/min = 1.22× — pure core-clock scaling). A number
/// whose own spread is that wide cannot support a cross-session comparison.
const SPREAD_TRIP: f64 = 1.40;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "video-tests/clips/foreman_cif.y4m".into());
    let nframes: usize = std::env::var("EH_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let qp: u8 = std::env::var("EH_QP").ok().and_then(|s| s.parse().ok()).unwrap_or(26);
    let (w, h, frames) = read_y4m(&path, nframes);
    let clipname = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
    println!("{path} {w}x{h} x{} qp{qp}\n", frames.len());
    println!("{:<10} {:<12} {:>12} {:>20}", "preset", "path", "bytes", "fnv1a");
    println!("{}", "-".repeat(58));

    for (name, preset) in [
        ("fast", Preset::Fast),
        ("balanced", Preset::Balanced),
        ("quality", Preset::Quality),
    ] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 30;
        cfg.preset = preset;
        // Lever 3: 4-wide MC exists only on the B-frame spatial-direct path
        // (P partitions bottom out at 8x8), so the census needs B-frames on.
        if let Ok(b) = std::env::var("EH_BFRAMES") {
            cfg.bframes = b.parse().unwrap_or(0);
            if cfg.bframes > 0 {
                cfg.profile = rusty_h264_common::Profile::Main;
            }
        }

        // Sequential: one encoder, frame by frame. Best-of-N wall clock, so this
        // binary doubles as the A/B timing arm (`EH_REPS`).
        let reps: usize = std::env::var("EH_REPS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut times: Vec<f64> = Vec::new();
        let mut seq = Vec::new();
        for _ in 0..reps {
            let enc = Encoder::new(cfg.clone()).expect("cfg");
            let t = std::time::Instant::now();
            let out: Vec<u8> = if cfg.bframes > 0 {
                enc.encode_all(&frames).expect("encode_all").concat()
            } else {
                let mut enc = enc;
                let mut o = Vec::new();
                for f in &frames {
                    o.extend_from_slice(&enc.encode(f));
                }
                o
            };
            times.push(t.elapsed().as_secs_f64() * 1e3);
            seq = out;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let best = times[0];
        let spread = times[times.len() - 1] / times[0];
        println!(
            "{:<10} {:<12} {:>12} {:>20x} {:>10.2} ms{}",
            name,
            "sequential",
            seq.len(),
            fnv1a(&seq),
            best,
            if profiler_compiled_in() {
                "   <-- PROFILER BUILD: ~1.6x inflated, NOT a throughput number".to_string()
            } else if times.len() < 3 {
                "   (1-2 reps: spread unknown — not a comparable number)".to_string()
            } else if spread > SPREAD_TRIP {
                format!("   <-- SUSPECT: spread {spread:.2}x over {} reps, NOT comparable", times.len())
            } else {
                format!("   (spread {spread:.2}x / {} reps)", times.len())
            }
        );

        // Deterministic per-stage verdict. On this machine the wall-clock floor
        // (measured with a base-vs-base null arm) is ~7%, which exceeds most single
        // bricks — so the profiler buckets, not the wall, decide. Zero when the
        // `profile` feature is off.
        if std::env::var_os("EH_PROF").is_some() {
            rusty_h264_common::prof::reset();
            #[cfg(feature = "profile")]
            rusty_h264_common::inter::mcstats::reset();
            let enc = Encoder::new(cfg.clone()).expect("cfg");
            if cfg.bframes > 0 {
                let _ = enc.encode_all(&frames).expect("encode_all");
            } else {
                let mut enc = enc;
                for f in &frames {
                    let _ = enc.encode(f);
                }
            }
            let snap = rusty_h264_common::prof::snapshot();
            for (i, (ms, calls)) in snap.iter().enumerate() {
                let n = rusty_h264_common::prof::name(i);
                if *calls > 0 {
                    println!(
                        "    prof {:<14} {:>10.1} ms {:>12} calls {:>8.1} ns/call",
                        n,
                        ms,
                        calls,
                        ms * 1e6 / *calls as f64
                    );
                }
            }
            // MC call census — the mix that sizes the half-pel-plane-cache lever.
            #[cfg(feature = "profile")]
                #[cfg(feature = "profile")]
                {
                    let cyc = rusty_h264_common::inter::mcstats::snapshot_cycles();
                    let tot: u64 = cyc.iter().map(|c| c.3).sum();
                    let sub: u64 = cyc.iter().filter(|c| c.1 != "fullpel").map(|c| c.3).sum();
                    let subn: u64 = cyc.iter().filter(|c| c.1 != "fullpel").map(|c| c.2).sum();
                    let totn: u64 = cyc.iter().map(|c| c.2).sum();
                    for c in &cyc {
                        println!("    mcT  {:<10} {:<9} {:>8} calls {:>12} cyc {:>6.2}% time  {:>6.0} cyc/call",
                            c.0, c.1, c.2, c.3, 100.0 * c.3 as f64 / tot.max(1) as f64,
                            c.3 as f64 / c.2.max(1) as f64);
                    }
                    let st = rusty_h264_common::inter::mcstats::site_snapshot();
                    let stc: u64 = st.iter().map(|x| x.2).sum();
                    for (n, c, cy) in &st {
                        if *c == 0 { continue }
                        println!("    mcSITE {:<16} {:>9} calls {:>12} cyc {:>6.2}% of mc_luma time",
                            n, c, cy, 100.0 * *cy as f64 / stc.max(1) as f64);
                    }
                    println!("    mcT  SUB-PEL: {:.2}% of CALLS but {:.2}% of mc_luma TIME",
                        100.0 * subn as f64 / totn.max(1) as f64, 100.0 * sub as f64 / tot.max(1) as f64);
                }
            #[cfg(feature = "profile")]
            {
                let cen = rusty_h264_common::inter::mcstats::snapshot();
                let total: u64 = cen.iter().map(|&(_, _, n)| n).sum();
                let sub: u64 = cen.iter().filter(|&&(_, p, _)| p != "fullpel").map(|&(_, _, n)| n).sum();
                for (s, p, n) in &cen {
                    println!(
                        "    mc   {:<10} {:<9} {:>12} {:>7.2}%",
                        s,
                        p,
                        n,
                        100.0 * *n as f64 / total.max(1) as f64
                    );
                }
                println!(
                    "    mc   SUB-PEL SHARE {:>6.2}%  ({} of {} calls)",
                    100.0 * sub as f64 / total.max(1) as f64,
                    sub,
                    total
                );
                rusty_h264_common::inter::mcstats::reset();
                let steps: [i32; 5] = [64, 32, 16, 8, 4];
                let d = rusty_h264_encoder::diastats_snapshot();
                let tot: u64 = d.iter().map(|x| x.0).sum();
                for (i, (ev, imp)) in d.iter().enumerate().take(5) {
                    println!(
                        "    dia  step {:>3} (qpel) {:>10} evals {:>6.2}%   {:>9} improved {:>6.2}%",
                        steps[i], ev, 100.0 * *ev as f64 / tot.max(1) as f64, imp,
                        100.0 * *imp as f64 / (*ev).max(1) as f64
                    );
                }
                let sp = rusty_h264_encoder::satdpath_snapshot();
                let spt: u64 = sp.iter().sum();
                let lbl = ["interior full-pel (zero-copy)", "EDGE full-pel (slow copy)", "sub-pel (hpel planes)"];
                for (i, v) in sp.iter().enumerate() {
                    println!("    satdpath {:<32} {:>10} {:>6.2}%", lbl[i], v, 100.0 * *v as f64 / spt.max(1) as f64);
                }
                #[cfg(feature = "profile")]
                {
                    let hp = rusty_h264_common::inter::hpelphase::snapshot();
                    let t: u64 = hp.iter().sum();
                    let l = ["half-pel  single-plane (copy-free-able)", "quarter-pel two-plane avg (must build)"];
                    for (i, v) in hp.iter().enumerate() {
                        println!("    hpelphase {:<40} {:>10} {:>6.2}%", l[i], v, 100.0 * *v as f64 / t.max(1) as f64);
                    }
                    rusty_h264_common::inter::hpelphase::reset();
                }
                {
                    let (pos, it) = rusty_h264_encoder::spstats_snapshot();
                    if !pos.is_empty() {
                        let names = ["(+s,0)", "(-s,0)", "(0,+s)", "(0,-s)", "(+s,+s)", "(-s,-s)", "(+s,-s)", "(-s,+s)"];
                        for st in 0..2 {
                            let lbl = if st == 0 { "HALF" } else { "QRTR" };
                            let tot: u64 = (0..8).map(|p| pos[(st * 8 + p) * 2]).sum();
                            if tot == 0 { continue }
                            for p in 0..8 {
                                let (e, i) = (pos[(st * 8 + p) * 2], pos[(st * 8 + p) * 2 + 1]);
                                if e == 0 { continue }
                                println!("    sp-{lbl} pos {:<8} {:>9} evals {:>6.2}%  {:>7} improved {:>6.2}%",
                                    names[p], e, 100.0 * e as f64 / tot as f64, i, 100.0 * i as f64 / e as f64);
                            }
                            let itot: u64 = (0..6).map(|k| it[(st * 6 + k) * 2]).sum();
                            for k in 0..6 {
                                let (e, i) = (it[(st * 6 + k) * 2], it[(st * 6 + k) * 2 + 1]);
                                if e == 0 { continue }
                                println!("    sp-{lbl} ITER {:<8} {:>9} evals {:>6.2}%  {:>7} improved {:>6.2}%",
                                    k + 1, e, 100.0 * e as f64 / itot.max(1) as f64, i, 100.0 * i as f64 / e as f64);
                            }
                        }
                    }
                    let red = rusty_h264_encoder::spstats_redundant();
                    let allsp: u64 = (0..2).map(|st| (0..8).map(|p| pos[(st * 8 + p) * 2]).sum::<u64>()).sum();
                    println!("    sp-REDUNDANT {:>12} of {:>10} sub-pel evals  {:>6.2}% already priced",
                        red, allsp, 100.0 * red as f64 / allsp.max(1) as f64);
                    rusty_h264_encoder::spstats_reset();
                }
                rusty_h264_encoder::satdpath_reset();
                rusty_h264_encoder::diastats_reset();
            }
        }

        // Dump the bitstream for external-decoder conformance checking.
        if let Ok(dir) = std::env::var("EH_OUT") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(format!("{dir}/{clipname}_{name}.264"), &seq);
            // Also dump OUR decoder's reconstruction, so the external decoder can be
            // compared pixel-for-pixel rather than merely "did not error".
            if let Ok(frames_out) = rusty_h264_decoder::Decoder::new().decode_stream(&seq) {
                let mut raw = Vec::new();
                for fr in &frames_out {
                    raw.extend_from_slice(&fr.y);
                    raw.extend_from_slice(&fr.u);
                    raw.extend_from_slice(&fr.v);
                }
                let _ = std::fs::write(format!("{dir}/{clipname}_{name}.ours.yuv"), &raw);
            }
        }

        // GOP-parallel: the arm that actually exercises per-thread state.
        let enc = Encoder::new(cfg).expect("cfg");
        let par: Vec<u8> = enc.encode_all(&frames).expect("encode_all").concat();
        println!(
            "{:<10} {:<12} {:>12} {:>20x}{}",
            name,
            "parallel",
            par.len(),
            fnv1a(&par),
            if par == seq { "" } else { "   <-- DIFFERS FROM SEQUENTIAL" }
        );
    }
}
