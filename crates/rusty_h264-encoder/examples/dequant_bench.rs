//! What does the reconstruction path's dequantize actually cost?
//!
//! `SkipRecon` measures 198.9 ns per macroblock in-context. Per coded 8x8 quad it
//! calls the scalar `dequantize` four times — 16 per macroblock — each returning
//! `[i32; 16]` by value, and then narrows every coefficient back to i16 to feed
//! `idct_four_t4_rec`. This prices that sequence on its own, because whole-encode
//! timing on this machine drifts more than the effect.
//!
//! ```text
//! cargo run --release -p rusty_h264-encoder --features asm --example dequant_bench
//! ```

use rusty_h264_common::transform::dequantize;

fn main() {
    const QP: u8 = 26;
    // One macroblock's worth of quantized levels: 16 blocks, sparse like real
    // residual (most coefficients zero, energy at low frequencies).
    let mut st = 0x2545f491u32;
    let mut rnd = || {
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        st
    };
    let q_blocks: Vec<[i32; 16]> = (0..16)
        .map(|_| {
            std::array::from_fn(|i| {
                let r = rnd();
                // ~40% non-zero, biased to the first coefficients
                if r % 5 < 2 && i < 8 { ((r >> 8) % 9) as i32 - 4 } else { 0 }
            })
        })
        .collect();

    let mut dct_in = [0i16; 256];
    let mut best = f64::MAX;
    let iters = 200_000u64;
    for _ in 0..7 {
        let t = std::time::Instant::now();
        for _ in 0..iters {
            // exactly what the reconstruction does for a fully-coded macroblock
            for (blk, q) in q_blocks.iter().enumerate() {
                let deq = dequantize(std::hint::black_box(q), QP);
                for i in 0..16 {
                    dct_in[blk * 16 + i] = deq[i] as i16;
                }
            }
            std::hint::black_box(&dct_in);
        }
        let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
        if ns < best {
            best = ns;
        }
    }
    println!("dequantize + narrow, 16 blocks (one macroblock):");
    println!("  {best:.1} ns/MB");
    println!("\n  SkipRecon in-context is 198.9 ns/MB, so this is {:.0}% of it.",
             100.0 * best / 198.9);
}
