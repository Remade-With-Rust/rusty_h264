//! `rusty_h264` command-line tool — encode raw YUV420p to an Annex-B `.264`
//! stream and decode it back. Mirrors openh264's `codec/console` apps.
//!
//! Usage:
//!   rusty_h264 encode --width W --height H [--qp N] --in in.yuv --out out.264
//!   rusty_h264 decode --width W --height H --in in.264 --out out.yuv

use rusty_h264::{Decoder, Encoder, EncoderConfig, Preset, YuvFrame};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("encode") => cmd_encode(&args[1..]),
        Some("decode") => cmd_decode(&args[1..]),
        Some("--help") | Some("-h") | None => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command: {other}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "rusty_h264 — pure-Rust H.264 codec\n\n\
         USAGE:\n  \
         rusty_h264 encode --width W --height H [--qp N] [--gop N] [--preset fast|quality] [--bitrate BPS --fps F] --in in.yuv --out out.264\n  \
         rusty_h264 decode --width W --height H --in in.264 --out out.yuv\n\n\
         Defaults: --qp 26  --gop 30 (keyframe interval; 1 = all-intra, 250 = best size)  --preset fast  --cabac 1.\n  \
         --cabac 0|1: CABAC entropy coding (Main profile; ~5-17%% smaller than CAVLC). Default ON,\n  \
         matching the library; --cabac 0 (or RUSTY_H264_LEGACY_CAVLC=1) restores Baseline+CAVLC.\n  \
         --satd-q F (0..1): route the top-F fraction of highest-variance MBs to the SATD mode\n  \
         decision (0 = pure SAD/default; 0.5 ~= -2.3%% BD-rate, +6%% time; 1 ~= -4.3%%, +13%%).\n  \
         --bframes N|auto (Main profile): N B-frames per anchor gap; `auto` codes B-frames only\n  \
         on B-favorable (smooth-motion) content and falls back to P-only on busy content.\n  \
         --iqp-offset D (default -3): per-GOP I-frame QP cascade base (ip_ratio) — the GOP's\n  \
         I-frame is coded finer, content-adaptively deeper on predictable GOPs. `0` opts out.\n  \
         --bqp-offset D (default 2): B-frame QP offset base; content-adaptively coarser on\n  \
         near-static GOPs (B is non-reference). Both offsets are per-GOP, driven by one signal.\n  \
         --aq S (default 1.0, on): adaptive quantization — per-MB QP finer on flat / coarser on\n  \
         busy regions (perceptual/SSIM win); strength self-limits on pathological content. `0` off.\n  \
         Input/output YUV is raw planar 4:2:0 (I420), one frame after another."
    );
}

/// Minimal `--key value` parser.
fn parse_opts(args: &[String]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut map = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i]
            .strip_prefix("--")
            .ok_or_else(|| format!("expected --flag, got {}", args[i]))?;
        let val = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for --{key}"))?;
        map.insert(key.to_string(), val.clone());
        i += 2;
    }
    Ok(map)
}

fn req<'a>(
    opts: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Result<&'a String, String> {
    opts.get(key).ok_or_else(|| format!("missing --{key}"))
}

fn cmd_encode(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let width: usize = req(&opts, "width")?.parse().map_err(|_| "bad --width")?;
    let height: usize = req(&opts, "height")?.parse().map_err(|_| "bad --height")?;
    let qp: u8 = opts.get("qp").map_or(Ok(26), |s| s.parse()).map_err(|_| "bad --qp")?;
    // Default keyframe interval: a 1-second (30-frame) P-frame GOP, NOT the
    // all-intra `gop=1` that made a no-flag encode the slowest, largest possible
    // mode. 30 is a sweet spot for this encoder's per-GOP threading — enough GOPs
    // to feed all cores on typical clips — while P-frames land within ~2% of the
    // best compression (`--gop 250`). `--gop 1` still forces all-intra explicitly.
    let gop: u32 = opts.get("gop").map_or(Ok(30), |s| s.parse()).map_err(|_| "bad --gop")?;
    let bitrate: u32 = opts.get("bitrate").map_or(Ok(0), |s| s.parse()).map_err(|_| "bad --bitrate")?;
    let fps: f32 = opts.get("fps").map_or(Ok(30.0), |s| s.parse()).map_err(|_| "bad --fps")?;
    let refs: u32 = opts.get("refs").map_or(Ok(1), |s| s.parse()).map_err(|_| "bad --refs")?;
    let satd_q: f64 = opts.get("satd-q").map_or(Ok(0.5), |s| s.parse()).map_err(|_| "bad --satd-q")?;
    // --bframes N = fixed N B-frames; --bframes auto = content-adaptive (code
    // B-frames only where they help; P-only on busy content).
    let (bframes, bframes_adaptive): (u32, bool) = match opts.get("bframes").map(|s| s.as_str()) {
        None => (0, false),
        Some("auto") => (3, true), // 3 = the max; the adaptive picker chooses 1..3
        Some(s) => (s.parse().map_err(|_| "bad --bframes")?, false),
    };
    let preset = match opts.get("preset").map(String::as_str) {
        None | Some("fast") => Preset::Fast,
        Some("quality") | Some("slow") => Preset::Quality,
        Some(o) => return Err(format!("bad --preset {o} (fast|quality)")),
    };
    let mut cfg = EncoderConfig::new(width, height);
    cfg.qp = qp;
    cfg.gop_size = gop.max(1);
    cfg.bitrate = bitrate;
    cfg.framerate = fps;
    cfg.num_ref_frames = refs.clamp(1, 16);
    cfg.preset = preset;
    cfg.tune_satd_q = satd_q.clamp(0.0, 1.0);
    cfg.bframes = bframes;
    cfg.bframes_adaptive = bframes_adaptive;
    // Absent flag keeps the calibrated library default (−3); `--iqp-offset 0` opts
    // out (byte-identical to the pre-cascade output).
    cfg.i_qp_offset = opts.get("iqp-offset").map_or(Ok(cfg.i_qp_offset), |s| s.parse()).map_err(|_| "bad --iqp-offset")?;
    cfg.bframe_qp_offset = opts.get("bqp-offset").map_or(Ok(cfg.bframe_qp_offset), |s| s.parse()).map_err(|_| "bad --bqp-offset")?;
    // Absent flag keeps the calibrated library default (1.0, self-limiting); `--aq 0`
    // opts out (uniform QP).
    cfg.aq_strength = opts.get("aq").map_or(Ok(cfg.aq_strength), |s| s.parse()).map_err(|_| "bad --aq")?;
    // --cabac 0|1: CABAC entropy coding (Main profile). Absent = the LIBRARY
    // default (on, unless RUSTY_H264_LEGACY_CAVLC=1) — the CLI silently pinning
    // this to CAVLC while the library shipped CABAC-on is exactly the
    // "CLI defaults ≠ library defaults" drift the Great Gate hygiene batch
    // exists to kill (docs/great-gate.md §3.5), and it made every CLI benchmark
    // without an explicit --cabac 1 measure the wrong entropy coder.
    cfg.cabac = opts.get("cabac").map(|s| s == "1" || s == "true").unwrap_or(cfg.cabac);
    // --transform-8x8 1: High-profile 8x8 transform (I_8x8, CAVLC). Forces High profile.
    cfg.transform_8x8 = opts.get("transform-8x8").map(|s| s == "1" || s == "true").unwrap_or(false);
    // --sub8x8 1/0: P_8x8 sub-partition motion (four 8x8 MVs per MB, per-MB RD).
    // DEFAULT-ON for --preset quality; --sub8x8 0 forces off, 1 forces on. Absent
    // (None) follows the preset.
    cfg.sub_8x8 = opts.get("sub8x8").map(|s| s == "1" || s == "true");
    // --me-wide 1/0: adaptive wide ME grid on flat blocks (fixes the diamond's
    // flat-surface stall; big win on smooth content). DEFAULT-ON for --preset
    // quality; pass --me-wide 0 to force it off, --me-wide 1 to force on. Absent
    // (None) follows the preset.
    cfg.me_wide = opts.get("me-wide").map(|s| s == "1" || s == "true");
    // --sub8 0|1 / --sub8-rd 0|1 (Great Gate P3.3, opt-in): the sub-8x8 split
    // search and its RD pricing. Flags rather than env so the ONE compliant
    // timing harness (bench/pinvs.ps1) can differ the two arms by ARGV —
    // Start-Process inherits the parent environment, so an env-only knob makes
    // both arms identical and the harness prints a confident 1.000x.
    cfg.tune_sub8x8_split = opts.get("sub8").map(|s| s == "1" || s == "true").unwrap_or(cfg.tune_sub8x8_split);
    // --sub8-minpay N: online split-payoff census (see mb16 `sub8_pay_cfg`).
    // A CLI flag, not just the env knob, so bench/pinvs.ps1 can differ the arms
    // by ARGV — Start-Process inherits the parent env, which would make both
    // arms identical and print a confident 1.000x.
    if let Some(v) = opts.get("sub8-minpay") {
        std::env::set_var("RFF_SUB8_MINPAY", v);
    }
    cfg.tune_intra_rd = opts.get("intra-rd").map(|s| s == "1" || s == "true").unwrap_or(cfg.tune_intra_rd);
    cfg.tune_sub8_rd = opts.get("sub8-rd").map(|s| s == "1" || s == "true").unwrap_or(cfg.tune_sub8_rd);
    cfg.cabac_init_idc = opts.get("cabac-init").map_or(Ok(0), |s| s.parse()).map_err(|_| "bad --cabac-init")?;
    cfg.cabac_lambda_scale = opts.get("cabac-lambda").map_or(Ok(1.0), |s| s.parse()).map_err(|_| "bad --cabac-lambda")?;
    cfg.cabac_dz_div = opts.get("cabac-dz").map_or(Ok(0), |s| s.parse()).map_err(|_| "bad --cabac-dz")?;
    cfg.cabac_rdoq = opts.get("cabac-rdoq").map_or(Ok(cfg.cabac_rdoq), |s| s.parse()).map_err(|_| "bad --cabac-rdoq")?;
    // --mbtree 1: macroblock-tree lookahead temporal AQ (CQP, CAVLC). Routes through
    // encode_all (needs the GOP's future frames). --mbtree-strength X tunes it.
    cfg.mbtree = opts.get("mbtree").map(|s| s == "1" || s == "true").unwrap_or(cfg.mbtree);
    cfg.mbtree_strength = opts.get("mbtree-strength").map_or(Ok(cfg.mbtree_strength), |s| s.parse()).map_err(|_| "bad --mbtree-strength")?;
    // --mbtree-lookahead full|hybrid|half: mb-tree lookahead ME resolution. Absent →
    // the library default (half-res = fastest); `hybrid` recovers full-res quality.
    if let Some(m) = opts.get("mbtree-lookahead") {
        cfg.mbtree_lookahead = match m.as_str() {
            "full" => rusty_h264::LookaheadMode::FullRes,
            "hybrid" => rusty_h264::LookaheadMode::Hybrid,
            "half" => rusty_h264::LookaheadMode::HalfRes,
            other => return Err(format!("bad --mbtree-lookahead {other} (full|hybrid|half)")),
        };
    }
    if bframes > 0 || cfg.cabac {
        cfg.profile = rusty_h264::Profile::Main; // B / CABAC are illegal in Baseline
    }
    if cfg.transform_8x8 {
        cfg.profile = rusty_h264::Profile::High; // 8x8 transform requires High profile
    }

    let frame_size = width * height * 3 / 2;
    let in_path = req(&opts, "in")?;
    let file_len = std::fs::metadata(in_path).map_err(|e| format!("stat input: {e}"))? .len() as usize;
    if frame_size == 0 || file_len % frame_size != 0 {
        return Err(format!(
            "input size {file_len} is not a multiple of one I420 frame ({frame_size} bytes)"
        ));
    }
    let n = file_len / frame_size;

    // Streaming GOP pipeline (constant QP): read each GOP's frames off the file and
    // hand them to a worker thread immediately, overlapping I/O with encoding — the
    // same per-GOP fresh-Encoder scheme as `encode_all`, so the output is
    // byte-identical to it (and to sequential encoding). Rate control threads QP
    // state across frames, so that path stays sequential via `encode_all`.
    let single = std::env::var("RUSTY_THREADS").ok().as_deref() == Some("1");
    let ys = width * height;
    let cs = (width / 2) * (height / 2);
    // Streaming encode, no whole-file buffer and no per-frame allocations: each
    // path reads I420 planes straight into a REUSED YuvFrame and encodes it.
    let out: Vec<u8> = if bframes > 0 || cfg.mbtree {
        // B-frames AND mb-tree need the whole clip in memory: the reorder pipeline
        // codes a future anchor before its B's; mb-tree runs a per-GOP lookahead over
        // the source frames. Both go through `encode_all`. AUs come back in coding
        // order; the decoder reorders to display order by POC.
        use std::io::Read;
        let mut file = std::io::BufReader::new(
            std::fs::File::open(in_path).map_err(|e| format!("open input: {e}"))?,
        );
        let mut all = Vec::with_capacity(n);
        for _ in 0..n {
            let mut fr = YuvFrame { width, height, y: vec![0u8; ys], u: vec![0u8; cs], v: vec![0u8; cs] };
            file.read_exact(&mut fr.y).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.u).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.v).map_err(|e| format!("read: {e}"))?;
            all.push(fr);
        }
        let enc = Encoder::new(cfg.clone()).map_err(|e| e.to_string())?;
        enc.encode_all(&all).map_err(|e| e.to_string())?.concat()
    } else if bitrate == 0 && !single {
        // Parallel: one worker per GOP; each opens its own handle, seeks to its
        // GOP and streams frames. Same per-GOP fresh-Encoder scheme as
        // `encode_all` => output byte-identical to it (and to sequential).
        use std::io::{Read, Seek, SeekFrom};
        let gop_len = cfg.gop_size.max(1) as usize;
        let n_gops = n.div_ceil(gop_len);
        let cfg_ref = &cfg;
        let mut parts: Vec<Option<Vec<u8>>> = (0..n_gops).map(|_| None).collect();
        std::thread::scope(|sc| -> Result<(), String> {
            let mut handles = Vec::new();
            for g in 0..n_gops {
                handles.push(sc.spawn(move || -> Result<Vec<u8>, String> {
                    let frames_in_gop = gop_len.min(n - g * gop_len);
                    let mut file =
                        std::fs::File::open(in_path).map_err(|e| format!("open input: {e}"))?;
                    file.seek(SeekFrom::Start((g * gop_len * frame_size) as u64))
                        .map_err(|e| format!("seek: {e}"))?;
                    let mut file = std::io::BufReader::with_capacity(1 << 20, file);
                    let mut fr = YuvFrame {
                        width,
                        height,
                        y: vec![0u8; ys],
                        u: vec![0u8; cs],
                        v: vec![0u8; cs],
                    };
                    let mut enc = Encoder::new(cfg_ref.clone()).map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    for _ in 0..frames_in_gop {
                        file.read_exact(&mut fr.y).map_err(|e| format!("read: {e}"))?;
                        file.read_exact(&mut fr.u).map_err(|e| format!("read: {e}"))?;
                        file.read_exact(&mut fr.v).map_err(|e| format!("read: {e}"))?;
                        bytes.extend_from_slice(&enc.encode(&fr));
                    }
                    bytes.extend_from_slice(&enc.flush()); // lookahead tail
                    Ok(bytes)
                }));
            }
            for (g, h) in handles.into_iter().enumerate() {
                parts[g] = Some(h.join().map_err(|_| "encoder thread panicked".to_string())??);
            }
            Ok(())
        })?;
        parts.into_iter().flatten().flatten().collect()
    } else if bitrate == 0 {
        // Sequential (RUSTY_THREADS=1): same streaming loop on one thread.
        use std::io::Read;
        let mut file = std::io::BufReader::with_capacity(
            1 << 20,
            std::fs::File::open(in_path).map_err(|e| format!("open input: {e}"))?,
        );
        let gop_len = cfg.gop_size.max(1) as usize;
        let mut fr = YuvFrame {
            width,
            height,
            y: vec![0u8; ys],
            u: vec![0u8; cs],
            v: vec![0u8; cs],
        };
        let mut bytes = Vec::new();
        let mut enc = Encoder::new(cfg.clone()).map_err(|e| e.to_string())?;
        for i in 0..n {
            if i % gop_len == 0 && i > 0 {
                enc = Encoder::new(cfg.clone()).map_err(|e| e.to_string())?; // fresh per GOP == encode_all
            }
            file.read_exact(&mut fr.y).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.u).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.v).map_err(|e| format!("read: {e}"))?;
            bytes.extend_from_slice(&enc.encode(&fr));
        }
        bytes.extend_from_slice(&enc.flush()); // lookahead tail
        bytes
    } else {
        // Rate control threads state across frames: sequential, streaming.
        use std::io::Read;
        let mut file = std::io::BufReader::with_capacity(
            1 << 20,
            std::fs::File::open(in_path).map_err(|e| format!("open input: {e}"))?,
        );
        let mut fr = YuvFrame {
            width,
            height,
            y: vec![0u8; ys],
            u: vec![0u8; cs],
            v: vec![0u8; cs],
        };
        let mut bytes = Vec::new();
        let mut enc = Encoder::new(cfg).map_err(|e| e.to_string())?;
        for _ in 0..n {
            file.read_exact(&mut fr.y).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.u).map_err(|e| format!("read: {e}"))?;
            file.read_exact(&mut fr.v).map_err(|e| format!("read: {e}"))?;
            bytes.extend_from_slice(&enc.encode(&fr));
        }
        bytes.extend_from_slice(&enc.flush()); // lookahead tail
        bytes
    };
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("encoded {n} frame(s) -> {} bytes", out.len());
    Ok(())
}

fn cmd_decode(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let input = std::fs::read(req(&opts, "in")?).map_err(|e| format!("read input: {e}"))?;
    let mut dec = Decoder::new();
    // One call: access-unit split, multi-slice assembly, and display-order output.
    let frames = dec.decode_stream(&input).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for f in &frames {
        out.extend_from_slice(&f.y);
        out.extend_from_slice(&f.u);
        out.extend_from_slice(&f.v);
    }
    std::fs::write(req(&opts, "out")?, &out).map_err(|e| format!("write output: {e}"))?;
    eprintln!("decoded {} frame(s) -> {} bytes", frames.len(), out.len());
    Ok(())
}

fn frame_from_i420(buf: &[u8], width: usize, height: usize) -> YuvFrame {
    let ys = width * height;
    let cs = (width / 2) * (height / 2);
    YuvFrame {
        width,
        height,
        y: buf[..ys].to_vec(),
        u: buf[ys..ys + cs].to_vec(),
        v: buf[ys + cs..ys + 2 * cs].to_vec(),
    }
}

