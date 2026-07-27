//! Interleaved A/B of the per-macroblock variance kernel behind adaptive
//! quantization (`mb_variance`), which `aq_qp_map` runs for every macroblock of
//! every frame — measured at ~3.3% of encode.
//!
//! Whole-encode timing on this machine currently drifts more than the effect, so
//! the kernel is measured on its own: both variants in ONE binary, alternated
//! pass by pass, best-of-N. The two implementations are transcribed from the
//! encoder; `assert_eq` on every macroblock keeps them honest.
//!
//! ```text
//! cargo run --release -p rusty_h264-encoder --example variance_bench
//! ```

/// Original: i64 accumulators, so a 64-bit multiply per pixel.
fn var_i64(sy: &[u8], cw: usize, mb_x: usize, mb_y: usize) -> i64 {
    let base = mb_y * 16 * cw + mb_x * 16;
    let (mut s, mut ss) = (0i64, 0i64);
    for r in 0..16 {
        let row = &sy[base + r * cw..base + r * cw + 16];
        for &p in row {
            let v = p as i64;
            s += v;
            ss += v * v;
        }
    }
    ss - s * s / 256
}

/// Current: u32 accumulators. The sum of 256 bytes maxes at 65280 and the sum of
/// squares at 16.6M, so 64-bit was pure width — and it blocked vectorisation.
fn var_u32(sy: &[u8], cw: usize, mb_x: usize, mb_y: usize) -> i64 {
    let base = mb_y * 16 * cw + mb_x * 16;
    let (mut s, mut ss) = (0u32, 0u32);
    for r in 0..16 {
        let row = &sy[base + r * cw..base + r * cw + 16];
        for &p in row {
            let v = p as u32;
            s += v;
            ss += v * v;
        }
    }
    ss as i64 - (s as i64) * (s as i64) / 256
}

fn main() {
    // A CIF-sized plane of deterministic, non-uniform content.
    let (w, h) = (352usize, 288usize);
    let (mb_w, mb_h) = (w / 16, h / 16);
    let sy: Vec<u8> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (((x * 7) ^ (y * 13) ^ ((x * y) >> 5)) & 0xff) as u8
        })
        .collect();

    // Correctness: the variants must agree on every macroblock.
    for my in 0..mb_h {
        for mx in 0..mb_w {
            assert_eq!(var_i64(&sy, w, mx, my), var_u32(&sy, w, mx, my), "mb ({mx},{my})");
        }
    }

    let mbs = mb_w * mb_h;
    let mut best = [f64::MAX; 2];
    for pass in 0..40 {
        let arm = pass % 2;
        let t = std::time::Instant::now();
        let mut acc = 0i64;
        for my in 0..mb_h {
            for mx in 0..mb_w {
                acc += if arm == 0 {
                    var_u32(&sy, w, mx, my)
                } else {
                    var_i64(&sy, w, mx, my)
                };
            }
        }
        let e = t.elapsed().as_secs_f64();
        std::hint::black_box(acc);
        if e < best[arm] {
            best[arm] = e;
        }
    }
    let ns = |t: f64| t * 1e9 / mbs as f64;
    println!("per-macroblock variance, best-of-20 per arm, arms alternated\n");
    println!("  i64 accumulators : {:>7.1} ns/MB", ns(best[1]));
    println!("  u32 accumulators : {:>7.1} ns/MB", ns(best[0]));
    println!("  speedup          : {:>7.2}x", ns(best[1]) / ns(best[0]));
    println!("\n  frame ({mb_w}x{mb_h} = {mbs} MBs): {:.1} -> {:.1} us",
             best[1] * 1e6, best[0] * 1e6);
}
