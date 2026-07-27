//! What does `InterPlan`'s dead `q8` field cost per macroblock?
//!
//! `plan_inter_mb` returns `InterPlan` for every coded inter macroblock. The
//! struct carries `q8: [[i32; 64]; 4]` — 1 KB, zero-initialised every time — but
//! `q8` is only ever read when the High-profile 8x8 transform is on, which is off
//! by default. This alternates a struct with and without that field so the two
//! arms see the same thermal state.
//!
//! ```text
//! cargo run --release -p rusty_h264-encoder --example interplan_bench
//! ```

#[derive(Clone, Copy)]
struct PlanWithQ8 {
    cbp: u32,
    q_blocks: [[i32; 16]; 16],
    c_dc_levels: [[i32; 4]; 2],
    c_q: [[[i32; 16]; 4]; 2],
    t8x8: bool,
    q8: [[i32; 64]; 4],
}

#[derive(Clone, Copy)]
struct PlanNoQ8 {
    cbp: u32,
    q_blocks: [[i32; 16]; 16],
    c_dc_levels: [[i32; 4]; 2],
    c_q: [[[i32; 16]; 4]; 2],
    t8x8: bool,
}

#[inline(never)]
fn build_with(seed: i32) -> PlanWithQ8 {
    let mut q_blocks = [[0i32; 16]; 16];
    let c_dc_levels = [[0i32; 4]; 2];
    let c_q = [[[0i32; 16]; 4]; 2];
    // the 8x8 buffer is materialised even though nothing reads it
    let q8 = [[0i32; 64]; 4];
    q_blocks[0][0] = seed;
    PlanWithQ8 { cbp: seed as u32, q_blocks, c_dc_levels, c_q, t8x8: false, q8 }
}

#[inline(never)]
fn build_without(seed: i32) -> PlanNoQ8 {
    let mut q_blocks = [[0i32; 16]; 16];
    let c_dc_levels = [[0i32; 4]; 2];
    let c_q = [[[0i32; 16]; 4]; 2];
    q_blocks[0][0] = seed;
    PlanNoQ8 { cbp: seed as u32, q_blocks, c_dc_levels, c_q, t8x8: false }
}

fn main() {
    let iters = 2_000_000u64;
    let mut best = [f64::MAX; 2];
    for pass in 0..12 {
        let arm = pass % 2;
        let t = std::time::Instant::now();
        let mut acc = 0u32;
        for i in 0..iters {
            if arm == 0 {
                let p = build_without(std::hint::black_box(i as i32));
                acc = acc.wrapping_add(p.cbp ^ p.q_blocks[0][0] as u32);
            } else {
                let p = build_with(std::hint::black_box(i as i32));
                acc = acc.wrapping_add(p.cbp ^ p.q_blocks[0][0] as u32 ^ p.q8[0][0] as u32);
            }
        }
        std::hint::black_box(acc);
        let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
        if ns < best[arm] {
            best[arm] = ns;
        }
    }
    println!("InterPlan construction, per macroblock (best-of-6 per arm, alternated):\n");
    println!("  with q8 (current) : {:>6.1} ns", best[1]);
    println!("  without q8        : {:>6.1} ns", best[0]);
    println!("  saving            : {:>6.1} ns/MB", best[1] - best[0]);
    println!(
        "\n  over 44078 MBs/clip: {:.2} ms  (enc-inter-code is 29.97 ms)",
        (best[1] - best[0]) * 44078.0 / 1e6
    );
}
