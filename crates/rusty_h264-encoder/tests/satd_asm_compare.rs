//! Phase 1 of docs/satd-asm-plan.md — characterize the openh264 asm SATD vs our Rust
//! `satd_4x4_sum` EXACTLY, so we know whether wiring the asm in is (≈)bit-exact or an
//! RD-revalidation change. Rust SATD is `Σ|H·d|`; openh264 asm is documented as
//! `(Σ|H·d|+1)>>1` (≈ half). This test measures the precise relationship at 4×4/8×8/
//! 16×16 over many random (src, pred) pairs. Run:
//!   cargo test -p rusty_h264-encoder --release --features asm satd_asm_compare -- --nocapture
#![cfg(accel)]

use rusty_h264_common::transform::satd_4x4_sum;

// Deterministic LCG so the test is reproducible without rand.
fn lcg(state: &mut u64) -> u8 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) as u8
}

/// Rust SATD of an NxN luma block: build 4×4 residual sub-blocks (src-pred) and sum
/// their Hadamard abs — exactly what the encoder's `satd_16x16`/`satd_8x8`/`satd_4x4`
/// do (`Σ|H·d|`, no normalization).
fn rust_satd(src: &[u8], pred: &[u8], n: usize) -> i64 {
    let mut blocks = Vec::new();
    for by in (0..n).step_by(4) {
        for bx in (0..n).step_by(4) {
            let mut blk = [0i32; 16];
            for dy in 0..4 {
                for dx in 0..4 {
                    blk[dy * 4 + dx] =
                        src[(by + dy) * n + bx + dx] as i32 - pred[(by + dy) * n + bx + dx] as i32;
                }
            }
            blocks.push(blk);
        }
    }
    satd_4x4_sum(&blocks)
}

#[test]
fn satd_asm_compare() {
    let mut st = 0x1234_5678_9abc_def0u64;
    for &n in &[4usize, 8, 16] {
        let (mut all_exact_half, mut max_2asm_minus_rust, mut min_2asm_minus_rust) =
            (true, i64::MIN, i64::MAX);
        let mut samples = 0;
        for _ in 0..20000 {
            let src: Vec<u8> = (0..n * n).map(|_| lcg(&mut st)).collect();
            let pred: Vec<u8> = (0..n * n).map(|_| lcg(&mut st)).collect();
            let rust = rust_satd(&src, &pred, n);
            let asm = match n {
                4 => rusty_h264_accel::satd_4x4(&src, n, &pred, n),
                8 => rusty_h264_accel::satd_8x8(&src, n, &pred, n),
                16 => rusty_h264_accel::satd_16x16(&src, n, &pred, n),
                _ => unreachable!(),
            } as i64;
            // Hypothesis: asm == (rust + 1) >> 1.
            if asm != (rust + 1) >> 1 {
                all_exact_half = false;
            }
            let d = 2 * asm - rust; // how far ×2-scaled asm lands from the Rust value
            max_2asm_minus_rust = max_2asm_minus_rust.max(d);
            min_2asm_minus_rust = min_2asm_minus_rust.min(d);
            samples += 1;
        }
        eprintln!(
            "{n}x{n}: samples={samples}  asm==(rust+1)>>1 ALWAYS: {all_exact_half}  \
             (2*asm - rust) range = [{min_2asm_minus_rust}, {max_2asm_minus_rust}]",
        );
        // If the ×2 recovery is within a couple of units, the swap is ~bit-exact.
        assert!(
            (-2..=2).contains(&min_2asm_minus_rust) && (-2..=2).contains(&max_2asm_minus_rust),
            "2*asm strayed >2 from rust at {n}x{n} — swap would drift mode decisions materially",
        );
    }
}
