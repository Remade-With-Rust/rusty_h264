//! Deblock anatomy bench — isolates where `filter_frame` spends its time.
//!
//! Motivated by the video-tests function-level comparison, which put our
//! deblocking well behind x264's. x264 splits the work in two: boundary
//! strengths are derived in its MB-encode loop by an asm kernel
//! (`deblock_strength_avx2`, ~15 ns/MB measured in-context), and
//! `frame_deblock_row` only filters. Ours does both inside `filter_frame`, so a
//! stage-vs-stage read overstates the gap — and hides which half is actually slow.
//!
//! Four scenarios separate the two halves. The key one is `inter-bs0`: every
//! boundary strength derives to 0, so NO filter kernel runs and the number is
//! pure bS-derivation + loop glue.
//!
//! ```text
//! cargo run --release -p rusty_h264-common --features asm --example deblock_anatomy
//! ```

use rusty_h264_common::deblock::{filter_frame, BlockInfo};

struct Grid {
    inter: Vec<bool>,
    nnz: Vec<u8>,
    mv: Vec<(i32, i32)>,
    ref_id: Vec<i32>,
    t8x8: Vec<bool>,
}

const NO_REF: i32 = i32::MIN;

/// Build the per-4×4-block state for one scenario.
fn grid(kind: &str, mb_w: usize, mb_h: usize) -> Grid {
    let (w4, h4) = (mb_w * 4, mb_h * 4);
    let n = w4 * h4;
    let mut g = Grid {
        inter: vec![true; n],
        nnz: vec![0u8; n],
        mv: vec![(0i32, 0i32); n],
        ref_id: vec![0i32; n],
        t8x8: Vec::new(),
        };
    match kind {
        // Every block intra: bS is 4 on MB edges, 3 internally — maximum
        // filtering, and the bS derivation never reaches the motion comparison.
        "all-intra" => {
            g.inter.iter_mut().for_each(|b| *b = false);
            g.ref_id.iter_mut().for_each(|r| *r = NO_REF);
        }
        // Every MB a skip: all inter, no coefficients, one shared (ref, mv).
        // The flat-inter gate fires, so only MB edges are considered.
        "skip" => {}
        // Every block has coefficients: bS = 2 everywhere, so every edge filters.
        "inter-coded" => {
            g.nnz.iter_mut().for_each(|c| *c = 4);
        }
        // The isolator: all inter, no coefficients, motion that VARIES (so the
        // flat-inter gate cannot fire) but by less than one full sample (so every
        // bS still derives to 0 and no filter kernel runs). What remains is the
        // bS derivation and the loop around it, and nothing else.
        "inter-bs0" => {
            for (i, m) in g.mv.iter_mut().enumerate() {
                // ±1 quarter-pel: |Δ| < 4 for every neighbour pair ⇒ bS 0.
                *m = ((i % 2) as i32, ((i / 2) % 2) as i32);
            }
        }
        // Real content, unlike every scenario above, mixes intra/inter, coded and
        // uncoded blocks, and varying motion WITHIN a macroblock — so the bS
        // derivation's branches are unpredictable. The uniform scenarios let the
        // predictor get every branch right, which is why they came in at ~240
        // ns/MB while real clips measure ~650.
        "mixed" => {
            let mut st = 0x12345678u32;
            let mut rnd = || {
                st ^= st << 13;
                st ^= st >> 17;
                st ^= st << 5;
                st
            };
            for i in 0..n {
                let r = rnd();
                g.inter[i] = r & 7 != 0; // ~1 in 8 blocks intra
                g.nnz[i] = if r & 0x30 != 0 { (r >> 8 & 7) as u8 } else { 0 };
                g.mv[i] = (((r >> 11) & 15) as i32 - 8, ((r >> 15) & 15) as i32 - 8);
                g.ref_id[i] = if g.inter[i] { ((r >> 19) & 1) as i32 } else { NO_REF };
            }
        }
        _ => panic!("unknown scenario {kind}"),
    }
    g
}

fn main() {
    let cases = [(176usize, 144usize, "QCIF"), (352, 288, "CIF"), (1280, 720, "720p")];
    let scenarios = ["all-intra", "inter-coded", "inter-bs0", "skip", "mixed"];

    println!("ns per macroblock, best-of-30 per arm, arms alternated pass by pass
");
    println!(
        "{:<8} {:<12} {:>12} {:>12} {:>10}",
        "size", "scenario", "branchy", "branchless", "speedup"
    );
    println!("{}", "-".repeat(60));

    for (w, h, label) in cases {
        let (mb_w, mb_h) = (w / 16, h / 16);
        let mbs = mb_w * mb_h;
        let (cw, ch) = (w / 2, h / 2);
        let mb_qp = vec![26u8; mbs];

        for kind in scenarios {
            let g = grid(kind, mb_w, mb_h);
            // Content shaped like a real RECONSTRUCTION, not noise. The filter
            // rejects an edge whose |p0-q0| >= alpha or |p1-p0| >= beta (~13/~6 at
            // QP 26), so high-frequency synthetic data makes it early-out and the
            // bench silently measures a filter that is barely running. A slow ramp
            // with a small step at each 4-sample block boundary — exactly the
            // blocking artefact deblocking exists to remove — keeps every edge
            // inside the thresholds, so the kernels do their full work.
            let px = |x: usize, yy: usize| -> u8 {
                let ramp = 100 + ((x / 8 + yy / 8) % 24);
                let step = ((x / 4) % 3) + ((yy / 4) % 2); // small block-edge discontinuity
                (ramp + step) as u8
            };
            let y0: Vec<u8> = (0..w * h).map(|i| px(i % w, i / w)).collect();
            let u0: Vec<u8> = (0..cw * ch).map(|i| px(i % cw, i / cw)).collect();
            let v0: Vec<u8> = (0..cw * ch).map(|i| px((i % cw) + 3, i / cw)).collect();
            let (mut y, mut u, mut v) = (y0.clone(), u0.clone(), v0.clone());

            // ALTERNATE the two bS arms pass by pass so both see the same
            // thermal state; separate builds drift ~20% on this machine, which is
            // larger than the effect being measured.
            let mut best = [f64::MAX; 2];
            for pass in 0..60 {
                let branchless = pass % 2 == 0;
                rusty_h264_common::deblock::set_branchless_bs(branchless);
                // CRITICAL: filter_frame smooths in place. Without restoring the
                // source each pass, successive iterations run on progressively
                // flatter data, the filter's α/β early-outs start firing, and
                // best-of-N reports the cheapest (most-smoothed) pass rather than
                // the real cost. Restore outside the timed region.
                y.copy_from_slice(&y0);
                u.copy_from_slice(&u0);
                v.copy_from_slice(&v0);
                let info = BlockInfo {
                    inter: &g.inter,
                    nnz: &g.nnz,
                    mv: &g.mv,
                    ref_id: &g.ref_id,
                    mv1: &[],
                    ref_id1: &[],
                    w4: mb_w * 4,
                    t8x8: &g.t8x8,
                    bs: &[], kind: &[],
        };
                let t = std::time::Instant::now();
                filter_frame(&mut y, &mut u, &mut v, mb_w, mb_h, &mb_qp, 0, 0, 0, &info);
                let e = t.elapsed().as_secs_f64();
                let slot = if branchless { 0 } else { 1 };
                if e < best[slot] {
                    best[slot] = e;
                }
                std::hint::black_box((&y, &u, &v));
            }
            let ns = |t: f64| t * 1e9 / mbs as f64;
            println!(
                "{:<8} {:<12} {:>12.1} {:>12.1} {:>9.2}x",
                label,
                kind,
                ns(best[1]),
                ns(best[0]),
                ns(best[1]) / ns(best[0])
            );
        }
        println!();
    }
}
