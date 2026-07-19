//! Anatomy of the per-MB residual-coding "scatter" loop (scan + CAVLC), measured
//! profile-OFF. The primitive map flagged it at ~446 ns/MB in-context — the
//! biggest remaining glue target. This isolates each component's real cost.
//! Run: `cargo run --release -p rusty_h264-encoder --example scatter_anatomy`

use rusty_h264_common::cavlc::{encode_residual_block, scan_4x4_dcac};
use rusty_h264_common::BitWriter;
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
    // Representative quantized 4x4 blocks (raster order) at QP26 inter residual:
    // energy concentrated in low frequencies, a handful of nonzero levels.
    let sparse: [i32; 16] = [3, -1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // 3 coeff
    let medium: [i32; 16] = [5, -2, 1, 0, -1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]; // 6 coeff
    let dense: [i32; 16] = [9, -4, 2, -1, 3, -2, 1, 1, -1, 1, 0, -1, 1, 0, 0, 0]; // 12 coeff
    let empty: [i32; 16] = [0; 16];

    let n = 20_000_000u64;

    // --- scan (raster -> zig-zag temp), per coded block ---
    let t_scan = time(n, || {
        black_box(scan_4x4_dcac(black_box(&medium)));
    });

    // --- encode_residual_block per density (includes its own BitWriter writes) ---
    let bench_erb = |coeffs: &[i32; 16]| {
        let scan = scan_4x4_dcac(coeffs);
        time(n / 4, || {
            // Fresh-ish writer each call would dominate with alloc; instead reuse
            // one writer and let it grow (amortized), measuring the CAVLC work.
            let mut w = BitWriter::new();
            for _ in 0..8 {
                encode_residual_block(&mut w, black_box(&scan), 16, 2);
            }
            black_box(w.bit_len());
        })
    };
    let t_sparse = bench_erb(&sparse) / 8.0;
    let t_medium = bench_erb(&medium) / 8.0;
    let t_dense = bench_erb(&dense) / 8.0;

    // --- a whole-MB scatter simulation: 16 luma blocks, ~half coded, into ONE
    //     BitWriter (as the encoder does), incl. the scan per coded block ---
    let mb: [[i32; 16]; 16] = std::array::from_fn(|i| match i % 3 {
        0 => empty,
        1 => sparse,
        _ => medium,
    });
    let t_mb = time(n / 16, || {
        let mut w = BitWriter::new();
        for blk in mb.iter() {
            let coded = blk.iter().any(|&v| v != 0);
            if coded {
                let scan = scan_4x4_dcac(blk);
                encode_residual_block(&mut w, &scan, 16, 2);
            }
        }
        black_box(w.bit_len());
    });

    // --- raw write_bits cost (the BitWriter path the CAVLC hammers) ---
    // ~15 writes of assorted small lengths, as a coded block emits. Pre-sized vs not.
    let lens = [3u32, 1, 5, 2, 7, 1, 4, 6, 2, 3, 8, 1, 5, 2, 4];
    let t_wb_grow = time(n / 16, || {
        let mut w = BitWriter::new();
        for _ in 0..16 {
            for &l in &lens {
                w.write_bits(black_box(0b1011), black_box(l));
            }
        }
        black_box(w.bit_len());
    }) / 16.0
        / lens.len() as f64;
    // encode_residual_block into a writer that already has room reserved via a
    // warm-up write (isolates the CAVLC logic from realloc growth).
    let scan_m = scan_4x4_dcac(&medium);
    let mut warm = BitWriter::new();
    for _ in 0..100000 {
        encode_residual_block(&mut warm, &scan_m, 16, 2);
    }
    let t_erb_warm = time(n / 4, || {
        for _ in 0..8 {
            encode_residual_block(&mut warm, black_box(&scan_m), 16, 2);
        }
        black_box(warm.bit_len());
    }) / 8.0;

    println!("scatter anatomy (profile-OFF, real cost), ns:\n");
    println!("  {:<32} {:>7.2} ns", "write_bits (per call, ~15/blk)", t_wb_grow);
    println!("  {:<32} {:>7.2} ns", "erb medium, warm writer", t_erb_warm);
    println!("  {:<32} {:>7.2} ns", "scan_4x4_dcac (per block)", t_scan);
    println!("  {:<32} {:>7.2} ns", "encode_residual_block sparse(3)", t_sparse);
    println!("  {:<32} {:>7.2} ns", "encode_residual_block medium(6)", t_medium);
    println!("  {:<32} {:>7.2} ns", "encode_residual_block dense(12)", t_dense);
    println!("  {}", "-".repeat(44));
    println!("  {:<32} {:>7.2} ns", "whole-MB scatter (16 blk, ~2/3 coded)", t_mb);
    println!("\n  primitive map reported scatter ~446 ns/MB in-context.");
}
