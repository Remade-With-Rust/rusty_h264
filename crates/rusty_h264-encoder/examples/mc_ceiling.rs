//! D5 ceiling probe — what does one `mc_luma` call actually cost, and how much
//! of that is fixed overhead rather than real filtering?
//!
//! The descent (`docs/WHYS-inter-gap.md`) put `inter-mc` at 166 s of a 266 s
//! quality-preset encode over 991 M calls. This probe answers *why a call is
//! expensive* by sweeping block size × sub-pel phase.
//!
//! The tell we are looking for: work scales with `bw*bh` (16x16 = 16x the pixels
//! of 4x4), so if a 4x4 sub-pel call costs anywhere near a 16x16 one, the cost is
//! NOT the filter — it is the per-call fixed overhead (`luma_tile`'s 441-byte
//! zeroed array returned BY VALUE, plus `mc_luma_subpel`'s two 256-byte zeroed
//! scratch buffers ~= 1.4 KB of memset+copy per call, independent of block size).
//!
//! Reports ns/call and ns/pixel. A flat ns/CALL across sizes = fixed overhead
//! dominates. A flat ns/PIXEL = the filter dominates (what we would want).
//!
//! Interleaved round-robin over the arms so a drifting machine perturbs every arm
//! equally; verdict is the per-arm median across rounds.

use rusty_h264_common::inter::mc_luma;
use std::time::Instant;

const W: usize = 352;
const H: usize = 288;

fn make_ref() -> Vec<u8> {
    // Deterministic textured content — a flat plane would let the filter's
    // multiply-add chain collapse and understate real cost.
    let mut v = vec![0u8; W * H];
    let mut s: u32 = 0x1234_5678;
    for p in v.iter_mut() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *p = (s >> 24) as u8;
    }
    v
}

/// One arm: `iters` MC calls of `bw`x`bh` at sub-pel phase `(fx,fy)`.
/// Positions walk so we are not measuring a single hot cache line.
fn arm(reference: &[u8], bw: usize, bh: usize, fx: i32, fy: i32, iters: usize) -> f64 {
    let mut out = [0u8; 256];
    let mut acc = 0u64;
    let t = Instant::now();
    for i in 0..iters {
        // Stay well inside the frame so every call takes the interior path.
        let x0 = 8 + (i * 7) % (W - bw - 24);
        let y0 = 8 + (i * 13) % (H - bh - 24);
        let mvx = ((x0 as i32) << 2) - ((x0 as i32) << 2) + fx;
        let mvy = ((y0 as i32) << 2) - ((y0 as i32) << 2) + fy;
        mc_luma(reference, W, H, x0, y0, bw, bh, mvx, mvy, &mut out);
        acc = acc.wrapping_add(out[0] as u64);
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

fn main() {
    let reference = make_ref();
    let iters: usize = std::env::var("MC_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let rounds: usize = std::env::var("MC_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(9);

    // (label, bw, bh, fx, fy)
    let arms: Vec<(String, usize, usize, i32, i32)> = {
        let sizes = [(16usize, 16usize), (8, 8), (4, 4)];
        let phases = [("fullpel", 0, 0), ("halfH", 2, 0), ("halfHV", 2, 2), ("qpel", 1, 1)];
        let mut v = Vec::new();
        for &(bw, bh) in &sizes {
            for &(pn, fx, fy) in &phases {
                v.push((format!("{bw}x{bh} {pn}"), bw, bh, fx, fy));
            }
        }
        v
    };

    // Round-robin interleave; alternate direction each round so a warming machine
    // does not systematically favour whichever arm runs first.
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); arms.len()];
    for r in 0..rounds {
        let order: Vec<usize> = if r % 2 == 0 {
            (0..arms.len()).collect()
        } else {
            (0..arms.len()).rev().collect()
        };
        for &i in &order {
            let (_, bw, bh, fx, fy) = &arms[i];
            samples[i].push(arm(&reference, *bw, *bh, *fx, *fy, iters));
        }
    }

    println!("mc_luma ceiling probe — {iters} calls/arm, {rounds} interleaved rounds\n");
    println!("{:<18} {:>10} {:>12} {:>10}", "arm", "ns/call", "ns/pixel", "px");
    println!("{}", "-".repeat(54));
    let mut per_call: Vec<(String, f64, f64)> = Vec::new();
    for (i, (label, bw, bh, ..)) in arms.iter().enumerate() {
        let mut s = samples[i].clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = s[s.len() / 2];
        let px = (bw * bh) as f64;
        println!("{:<18} {:>10.1} {:>12.3} {:>10.0}", label, med, med / px, px);
        per_call.push((label.clone(), med, med / px));
        if label.ends_with("qpel") {
            println!();
        }
    }

    // The verdict the probe exists to deliver.
    let get = |name: &str| per_call.iter().find(|(l, ..)| l == name).map(|(_, c, _)| *c).unwrap_or(0.0);
    println!("\n--- verdict ---");
    for phase in ["halfH", "halfHV", "qpel"] {
        let c16 = get(&format!("16x16 {phase}"));
        let c4 = get(&format!("4x4 {phase}"));
        if c16 > 0.0 && c4 > 0.0 {
            println!(
                "{phase:<8} 4x4 costs {:.0}% of 16x16 per CALL despite 6.25% of the pixels \
                 -> fixed overhead {:.0}% of a 4x4 call",
                100.0 * c4 / c16,
                // work scales with pixels: a 4x4's "real" filter work is ~1/16 of 16x16's.
                100.0 * (c4 - c16 / 16.0).max(0.0) / c4
            );
        }
    }
}
