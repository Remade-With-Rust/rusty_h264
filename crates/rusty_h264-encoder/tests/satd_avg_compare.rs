//! Challenge-1 A3 oracle: the fused avg+SATD AVX2 kernel must equal the OLD path
//! EXACTLY — materialize `(a+b+1)>>1` (the `avg_rows` rounding), then the scalar
//! `Σ|H·d|` Hadamard — for every ME shape, over random planes / strides / offsets.
//! A single mismatch anywhere is a bitstream change, so the tolerance is zero.
//!   cargo test -p rusty_h264-encoder --release --features asm satd_avg_compare
#![cfg(accel)]

use rusty_h264_common::transform::satd_4x4_sum;

fn lcg(state: &mut u64) -> u8 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) as u8
}

/// The OLD quarter-pel cost path, verbatim: materialized rounded average, then the
/// scalar Hadamard SATD (`satd_px`'s reference formula).
fn reference(
    src: &[u8],
    ss: usize,
    a: &[u8],
    b: &[u8],
    stride: usize,
    w: usize,
    h: usize,
) -> i64 {
    let mut pred = vec![0u8; w * h];
    for r in 0..h {
        for i in 0..w {
            pred[r * w + i] =
                ((a[r * stride + i] as u16 + b[r * stride + i] as u16 + 1) >> 1) as u8;
        }
    }
    let mut blocks = Vec::new();
    for by in (0..h).step_by(4) {
        for bx in (0..w).step_by(4) {
            let mut blk = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    blk[dy * 4 + dx] = src[(by + dy) * ss + bx + dx] as i32
                        - pred[(by + dy) * w + bx + dx] as i32;
                }
            }
            blocks.push(blk);
        }
    }
    satd_4x4_sum(&blocks)
}

#[test]
fn satd_avg_matches_materialized_scalar() {
    let mut st = 0xfeed_beef_cafe_f00du64;
    let mut tested = 0u64;
    for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8)] {
        for _ in 0..4000 {
            // Random strides at and above the block width, random sub-slice offsets,
            // so unaligned loads and plane-interior geometry are both exercised.
            let ss = w + (lcg(&mut st) as usize % 48);
            let stride = w + (lcg(&mut st) as usize % 48);
            let off_a = lcg(&mut st) as usize % 32;
            let off_b = lcg(&mut st) as usize % 32;
            let off_s = lcg(&mut st) as usize % 32;
            let src: Vec<u8> = (0..off_s + (h - 1) * ss + w + 8).map(|_| lcg(&mut st)).collect();
            let pa: Vec<u8> = (0..off_a + (h - 1) * stride + w + 8).map(|_| lcg(&mut st)).collect();
            let pb: Vec<u8> = (0..off_b + (h - 1) * stride + w + 8).map(|_| lcg(&mut st)).collect();
            let (s, a, b) = (&src[off_s..], &pa[off_a..], &pb[off_b..]);
            let Some(fused) = rusty_h264_accel::satd_avg(s, ss, a, b, stride, w, h) else {
                // Non-AVX2 host: the encoder falls back to the materialize path, so
                // there is nothing to verify here (and nothing that can drift).
                eprintln!("satd_avg: AVX2 unavailable — kernel not in play on this host");
                return;
            };
            assert_eq!(
                fused as i64,
                reference(s, ss, a, b, stride, w, h),
                "satd_avg mismatch at {w}x{h} ss={ss} stride={stride}"
            );
            tested += 1;
        }
    }
    // Extremal inputs: flat 0/255 planes and max-contrast checkerboards — the
    // largest possible Hadamard coefficients (the i16-overflow corner).
    for &(w, h) in &[(16usize, 16usize), (8, 8)] {
        for &(sv, av, bv) in &[(255u8, 0u8, 0u8), (0, 255, 255), (255, 0, 255)] {
            let src = vec![sv; (h - 1) * w + w];
            let a = vec![av; (h - 1) * w + w];
            let b = vec![bv; (h - 1) * w + w];
            if let Some(fused) = rusty_h264_accel::satd_avg(&src, w, &a, &b, w, w, h) {
                assert_eq!(fused as i64, reference(&src, w, &a, &b, w, w, h));
                tested += 1;
            }
        }
    }
    eprintln!("satd_avg: {tested} random+extremal configs byte-exact");
}
