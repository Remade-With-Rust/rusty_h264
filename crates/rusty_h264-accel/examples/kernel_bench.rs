//! Per-kernel scalar-vs-asm microbenchmark — the seed of the h264 test kit.
//!
//! Answers the question that decides the whole asm campaign: does openh264's
//! hand-tuned SSE2 beat our auto-vectorized scalar, and by how much, per kernel?
//! Run: `cargo run --release -p rusty_h264-accel --example kernel_bench`
//!
//! x86-64 only (it benchmarks against the x86 SIMD kernels). On other targets only a
//! stub `main` remains so `cargo build --examples` / `cargo test` still succeed.
#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code, unused_imports))]
use std::hint::black_box;
use std::time::Instant;

#[repr(align(16))]
struct A16([u8; 256]);

// Naive i32 abs-diff — does NOT auto-vectorize well.
fn sad_naive(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
    let mut s = 0i32;
    for i in 0..h {
        for j in 0..w {
            s += (a[i * sa + j] as i32 - b[i * sb + j] as i32).abs();
        }
    }
    s
}

// The encoder's ACTUAL mc_sad form: u8::abs_diff over row slices → auto-vec to psadbw.
fn sad_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
    let mut s = 0u32;
    for i in 0..h {
        let ra = &a[i * sa..i * sa + w];
        let rb = &b[i * sb..i * sb + w];
        s += ra.iter().zip(rb).map(|(&x, &y)| x.abs_diff(y) as u32).sum::<u32>();
    }
    s as i32
}

// Scalar SATD: sum of 4x4 Hadamard SATDs, openh264 semantics ((sum+1)>>1 per block).
fn satd4_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
    let mut m = [[0i32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            m[i][j] = a[i * sa + j] as i32 - b[i * sb + j] as i32;
        }
    }
    for row in m.iter_mut() {
        let (s0, s1, s2, s3) = (row[0] + row[2], row[1] + row[3], row[0] - row[2], row[1] - row[3]);
        *row = [s0 + s1, s2 + s3, s2 - s3, s0 - s1];
    }
    let mut sum = 0i32;
    for j in 0..4 {
        let (s0, s1, s2, s3) = (m[0][j] + m[2][j], m[1][j] + m[3][j], m[0][j] - m[2][j], m[1][j] - m[3][j]);
        sum += (s0 + s1).abs() + (s2 + s3).abs() + (s2 - s3).abs() + (s0 - s1).abs();
    }
    (sum + 1) >> 1
}

fn satd16_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
    let mut s = 0i32;
    for by in (0..16).step_by(4) {
        for bx in (0..16).step_by(4) {
            s += satd4_scalar(&a[by * sa + bx..], sa, &b[by * sb + bx..], sb);
        }
    }
    s
}

fn bench(name: &str, iters: u64, mut scalar: impl FnMut() -> i32, mut asm: impl FnMut() -> i32) {
    // Speed-only (byte-exactness is covered by the unit tests; some kernels here
    // compare against a different-scale SATD purely for throughput).
    black_box(scalar());
    black_box(asm());
    let t = Instant::now();
    let mut acc = 0i64;
    for _ in 0..iters {
        acc = acc.wrapping_add(scalar() as i64);
    }
    let ds = t.elapsed().as_secs_f64();
    black_box(acc);
    let t = Instant::now();
    let mut acc2 = 0i64;
    for _ in 0..iters {
        acc2 = acc2.wrapping_add(asm() as i64);
    }
    let da = t.elapsed().as_secs_f64();
    black_box(acc2);
    let gs = iters as f64 / ds / 1e6;
    let ga = iters as f64 / da / 1e6;
    println!(
        "  {name:14}  scalar {gs:8.1} M/s   asm {ga:8.1} M/s   asm {:.2}x {}",
        ga / gs,
        if ga > gs { "FASTER" } else { "(scalar wins)" }
    );
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    eprintln!("kernel_bench: x86_64-only (openh264 SIMD kernels); nothing to benchmark here.");
}

#[cfg(target_arch = "x86_64")]
fn main() {
    let mut a = A16([0u8; 256]);
    let mut b = A16([0u8; 256]);
    for i in 0..256 {
        a.0[i] = (i * 7 + 11) as u8;
        b.0[i] = (i * 13 + 5) as u8;
    }
    let n = 50_000_000u64;
    println!("per-kernel scalar-vs-asm (aligned 16x16, {n} iters):");
    let (a, b) = (&a.0, &b.0);
    bench(
        "SAD naive",
        n,
        || sad_naive(black_box(a), 16, black_box(b), 16, 16, 16),
        || rusty_h264_accel::sad_16x16(black_box(a), 16, black_box(b), 16),
    );
    bench(
        "SAD psadbw",
        n,
        || sad_scalar(black_box(a), 16, black_box(b), 16, 16, 16),
        || rusty_h264_accel::sad_16x16(black_box(a), 16, black_box(b), 16),
    );
    bench(
        "SATD16 naive",
        n,
        || satd16_scalar(black_box(a), 16, black_box(b), 16),
        || rusty_h264_accel::satd_16x16(black_box(a), 16, black_box(b), 16),
    );
    // Our REAL SATD: the wide-SIMD satd_4x4_sum over the 16 residual blocks.
    let mut blocks = [[0i32; 16]; 16];
    for by in 0..4 {
        for bx in 0..4 {
            for dy in 0..4 {
                for dx in 0..4 {
                    blocks[by * 4 + bx][dy * 4 + dx] =
                        a[(by * 4 + dy) * 16 + bx * 4 + dx] as i32
                            - b[(by * 4 + dy) * 16 + bx * 4 + dx] as i32;
                }
            }
        }
    }
    bench(
        "SATD16 wide",
        n,
        || rusty_h264_common::transform::satd_4x4_sum(black_box(&blocks)) as i32,
        || rusty_h264_accel::satd_16x16(black_box(a), 16, black_box(b), 16),
    );

    // quant: openh264 asm (in-place, ((|c|+FF)*MF)>>16) vs our scalar quantize
    // ((|c|*MF+F)>>qbits). NOT bit-identical — speed-only ranking. The asm path pays
    // a refill copy each iter (conservative). 4 blocks per call.
    #[repr(align(16))]
    struct A16i([i16; 64]);
    let qin: [i32; 16] = std::array::from_fn(|i| ((i as i32 * 53) % 400) - 200);
    let qin16: [i16; 64] = std::array::from_fn(|i| (((i as i32 * 53) % 400) - 200) as i16);
    let ff = [85i16; 8];
    let mf = [400i16; 8];
    let mut dctw = A16i([0; 64]);
    bench(
        "quant 4x(4x4)",
        n / 4,
        || {
            let mut s = 0i32;
            for _ in 0..4 {
                for v in rusty_h264_common::transform::quantize(black_box(&qin), 26, 6) {
                    s = s.wrapping_add(v);
                }
            }
            s
        },
        || {
            dctw.0.copy_from_slice(black_box(&qin16));
            rusty_h264_accel::quant_four_4x4(&mut dctw.0, &ff, &mf);
            dctw.0.iter().map(|&v| v as i32).sum()
        },
    );
}

#[test]
fn op2_may_be_unaligned() {
    #[repr(align(16))]
    struct A16([u8; 320]);
    let mut a = A16([0u8; 320]);
    let mut b = A16([0u8; 320]);
    for i in 0..320 { a.0[i] = (i*7) as u8; b.0[i] = (i*13+5) as u8; }
    // op1 aligned (offset 0), op2 deliberately misaligned by 1,2,..,15 bytes.
    for off in 0..16usize {
        let s = rusty_h264_accel::sad_16x16(&a.0, 16, &b.0[off..], 16);
        assert!(s >= 0);
    }
}
