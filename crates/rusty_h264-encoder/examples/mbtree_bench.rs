//! mb-tree lookahead cost/benefit instrument: encode the same clip with the
//! lookahead OFF and ON, reporting each arm's wall (best-of-N) and an FNV hash
//! of the bitstream.
//!
//! The hash is the correctness gate for lookahead *speed* work: a faster cost
//! kernel that yields the same SATD values must leave the QP map — and therefore
//! the bitstream — byte-identical. The wall ratio is the overhead mb-tree is
//! asking us to pay for its BD win.
//!
//!   mbtree_bench <clip.y4m>     env: MB_FRAMES (48) MB_QP (27) MB_GOP (30) MB_REPS (3)
//!                                    MB_LA (full|hybrid|half — RFF_MBTREE_LA also works)

use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn fnv1a(d: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in d {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn read_y4m(path: &str, max: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).expect("read clip");
    let e = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..e]).expect("utf8 header");
    let (mut w, mut h) = (0usize, 0usize);
    for t in hdr.split_whitespace() {
        match t.as_bytes().first() {
            Some(b'W') => w = t[1..].parse().expect("W"),
            Some(b'H') => h = t[1..].parse().expect("H"),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (mut f, mut p) = (Vec::new(), e + 1);
    while f.len() < max {
        let Some(r) = raw[p..].iter().position(|&b| b == b'\n') else { break };
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

fn main() {
    let path = std::env::args().nth(1).expect("usage: mbtree_bench <clip.y4m>");
    let env = |k: &str, d: usize| -> usize {
        std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let (n, qp, gop, reps) = (env("MB_FRAMES", 48), env("MB_QP", 27), env("MB_GOP", 30), env("MB_REPS", 3));
    if let Ok(la) = std::env::var("MB_LA") {
        std::env::set_var("RFF_MBTREE_LA", la);
    }
    let (w, h, frames) = read_y4m(&path, n);
    println!("mbtree_bench {}x{} x{} qp{qp} gop{gop} (best-of-{reps})", w, h, frames.len());

    let mut off_ms = f64::MAX;
    let mut on_ms = f64::MAX;
    let (mut off_h, mut on_h, mut off_b, mut on_b) = (0u64, 0u64, 0usize, 0usize);
    let mut calls = 0u64;
    // PAIRED sampling (H-41/H-43). Alternating the arms is not enough: taking
    // `min` over each arm INDEPENDENTLY draws the two minima from different
    // moments, so drift never cancels and the overhead figure swings by more
    // than the effect. Keep a per-round RATIO instead — both arms of a ratio
    // are adjacent in time, so the pairing survives whatever the box is doing.
    let mut ratios: Vec<f64> = Vec::with_capacity(reps);
    for r in 0..reps {
        let (mut r_off, mut r_on) = (0.0f64, 0.0f64);
        // Swap which arm leads each round so a "second one is warmer" effect
        // cancels across rounds rather than accumulating into the ratio.
        let order: [bool; 2] = if r % 2 == 0 { [false, true] } else { [true, false] };
        for on in order {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp as u8;
            cfg.gop_size = gop as u32;
            cfg.preset = Preset::Quality;
            cfg.mbtree = on;
            let enc = Encoder::new(cfg).expect("cfg");
            rusty_h264_encoder::mbtree_satd_reset();
            let t = std::time::Instant::now();
            let out: Vec<u8> = enc.encode_all(&frames).expect("encode").concat();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if on {
                on_ms = on_ms.min(ms);
                on_h = fnv1a(&out);
                on_b = out.len();
                calls = rusty_h264_encoder::mbtree_satd_calls();
                r_on = ms;
            } else {
                off_ms = off_ms.min(ms);
                off_h = fnv1a(&out);
                off_b = out.len();
                r_off = ms;
            }
        }
        ratios.push(r_on / r_off);
    }
    println!("  mbtree OFF: {off_ms:8.1} ms  {off_b:>8} bytes  hash {off_h:016x}");
    println!("  mbtree ON : {on_ms:8.1} ms  {on_b:>8} bytes  hash {on_h:016x}");
    println!(
        "  lookahead work: {calls} candidate evals ({:.0}/MB/frame, DETERMINISTIC)",
        calls as f64 / ((w / 16 * (h / 16)) as f64 * frames.len() as f64)
    );
    // The unpaired figure, kept only so old logs stay comparable — do not quote it.
    println!(
        "  lookahead overhead (UNPAIRED, min-of-N per arm — noisy, historical): {:+.1}%",
        100.0 * (on_ms / off_ms - 1.0)
    );
    let mut sorted = ratios.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let med = sorted[sorted.len() / 2];
    let wins = ratios.iter().filter(|&&r| r < 1.0).count();
    let n = ratios.len();
    let z = (wins as f64 - n as f64 / 2.0) / (0.5 * (n as f64).sqrt());
    print!("  per-round ON/OFF ratios:");
    for r in &ratios {
        print!(" {r:.3}");
    }
    println!();
    println!(
        "  lookahead overhead (PAIRED median): {:+.1}%   [ON faster in {wins}/{n}, z={z:+.2} {}]",
        100.0 * (med - 1.0),
        if z.abs() > 2.0 { "VERDICT" } else { "no directional verdict" }
    );
    println!("  size {:+.2}%  (deterministic)", 100.0 * (on_b as f64 / off_b as f64 - 1.0));
}
