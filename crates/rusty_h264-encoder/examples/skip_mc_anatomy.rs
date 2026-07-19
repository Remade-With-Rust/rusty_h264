//! Anatomy of the P_Skip motion-compensation glue (`skip_predict_luma` +
//! `skip_predict_chroma`), measured profile-OFF so the numbers are the REAL cost —
//! not the double-nested rdtsc scopes that inflate it in the stage profiler.
//! Run: `cargo run --release -p rusty_h264-accel --example skip_mc_anatomy`
#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]

use rusty_h264_common::inter::{mc_chroma, mc_luma};
use std::hint::black_box;
use std::time::Instant;

fn time<F: FnMut()>(iters: u64, mut f: F) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..7 {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
        best = best.min(ns);
    }
    best
}

fn main() {
    // Reference planes at a realistic stride (832-wide luma, 416 chroma).
    let (cw, ch) = (832usize, 480usize);
    let (ccw, cch) = (416usize, 240usize);
    let refy: Vec<u8> = (0..cw * ch).map(|i| (i * 7 + 11) as u8).collect();
    let refu: Vec<u8> = (0..ccw * cch).map(|i| (i * 13 + 5) as u8).collect();
    let refv: Vec<u8> = (0..ccw * cch).map(|i| (i * 5 + 3) as u8).collect();
    let (mbx, mby) = (20usize, 12usize); // interior MB

    let n = 20_000_000u64;
    println!("skip-MC anatomy (profile-OFF, real cost), ns/call:\n");

    // --- component: luma MC, full-pel (0,0) — the common static-background skip ---
    let mut py = [0u8; 256];
    let t_luma_fp = time(n, || {
        mc_luma(black_box(&refy), cw, ch, mbx * 16, mby * 16, 16, 16, 0, 0, black_box(&mut py));
    });
    // --- component: luma MC, half-pel (2,2) — the interpolated skip case ---
    let t_luma_hp = time(n / 2, || {
        mc_luma(black_box(&refy), cw, ch, mbx * 16, mby * 16, 16, 16, 2, 2, black_box(&mut py));
    });
    // --- component: chroma MC, full-pel ---
    let mut pc = [0u8; 64];
    let t_chroma_fp = time(n, || {
        mc_chroma(black_box(&refu), ccw, cch, mbx * 8, mby * 8, 8, 8, 0, 0, black_box(&mut pc));
    });
    // --- component: chroma MC, eighth-pel (interpolated) ---
    let t_chroma_ep = time(n, || {
        mc_chroma(black_box(&refu), ccw, cch, mbx * 8, mby * 8, 8, 8, 3, 5, black_box(&mut pc));
    });

    // --- unit: skip_predict_luma (buffer alloc + return-by-value + mc) ---
    let t_pred_luma = time(n, || {
        let mut pred_y = [0u8; 256];
        mc_luma(black_box(&refy), cw, ch, mbx * 16, mby * 16, 16, 16, 0, 0, &mut pred_y);
        black_box(&pred_y);
    });
    // --- unit: skip_predict_chroma (both planes, [[u8;64];2] return) ---
    let t_pred_chroma = time(n, || {
        let mut pred_c = [[0u8; 64]; 2];
        for (c, rc) in [&refu, &refv].iter().enumerate() {
            mc_chroma(black_box(rc), ccw, cch, mbx * 8, mby * 8, 8, 8, 0, 0, &mut pred_c[c]);
        }
        black_box(&pred_c);
    });
    // --- whole skip prediction (luma + both chroma), the freecheck's input ---
    let t_full = time(n, || {
        let mut pred_y = [0u8; 256];
        mc_luma(black_box(&refy), cw, ch, mbx * 16, mby * 16, 16, 16, 0, 0, &mut pred_y);
        let mut pred_c = [[0u8; 64]; 2];
        for (c, rc) in [&refu, &refv].iter().enumerate() {
            mc_chroma(black_box(rc), ccw, cch, mbx * 8, mby * 8, 8, 8, 0, 0, &mut pred_c[c]);
        }
        black_box((&pred_y, &pred_c));
    });

    // --- BRICK candidate: full-pel 16x16 copy variants (the 38ns dominant cost) ---
    let (rx, ry) = (mbx * 16, mby * 16);
    let mut cout = [0u8; 256];
    // current: 16× copy_from_slice(16)
    let t_copy_cur = time(n, || {
        for dy in 0..16 {
            cout[dy * 16..dy * 16 + 16].copy_from_slice(&refy[(ry + dy) * cw + rx..][..16]);
        }
        black_box(&cout);
    });
    // variant A: array-chunk copy (read [u8;16] per row)
    let t_copy_arr = time(n, || {
        for dy in 0..16 {
            let row: &[u8; 16] = refy[(ry + dy) * cw + rx..][..16].try_into().unwrap();
            cout[dy * 16..dy * 16 + 16].copy_from_slice(row);
        }
        black_box(&cout);
    });
    println!("  {:<34} {:>7.1} ns", "  copy: 16x copy_from_slice (const)", t_copy_cur);
    println!("  {:<34} {:>7.1} ns", "  copy: array-chunk", t_copy_arr);
    println!("  {}", "-".repeat(46));
    println!("  {:<34} {:>7.1} ns", "mc_luma 16x16 full-pel (copy)", t_luma_fp);
    println!("  {:<34} {:>7.1} ns", "mc_luma 16x16 half-pel (2,2)", t_luma_hp);
    println!("  {:<34} {:>7.1} ns", "mc_chroma 8x8 full-pel", t_chroma_fp);
    println!("  {:<34} {:>7.1} ns", "mc_chroma 8x8 eighth-pel", t_chroma_ep);
    println!("  {}", "-".repeat(46));
    println!("  {:<34} {:>7.1} ns", "skip_predict_luma (full-pel)", t_pred_luma);
    println!("  {:<34} {:>7.1} ns", "skip_predict_chroma (full-pel)", t_pred_chroma);
    println!("  {:<34} {:>7.1} ns", "FULL skip prediction (fp)", t_full);
    println!("\n  profiler 'neighbors' (luma only) reported 89 ns/call — the gap is");
    println!("  the double-nested rdtsc scopes (Neighbors + InterMc), not real work.");
}
