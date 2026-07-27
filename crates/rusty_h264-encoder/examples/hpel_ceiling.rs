//! CEILING PROBE for the half-pel plane cache — run BEFORE building it.
//!
//! Today every sub-pel motion-search candidate re-runs the 6-tap filter through
//! `mc_luma`. x264 instead filters three half-pel planes ONCE per reference frame
//! (H, V, and the centre/"j" plane) and then every sub-pel position is either a
//! strided COPY from one plane or a 2-tap AVERAGE of two.
//!
//! This probe prices that swap without building it:
//!   prize = Σ_mix( current mc_luma cost )  −  ( plane build per frame + Σ_mix( read cost ) )
//!
//! The mix is the real one, measured by `inter::mcstats` on this clip/preset (see
//! `encode_hash --features profile`), so the weighting is not a guess.
//!
//! Arms are interleaved round-robin with alternating direction; verdict is the
//! per-arm median, because the wall-clock null floor on this machine is ~5%.

use rusty_h264_common::inter::mc_luma;
use std::time::Instant;

const W: usize = 352;
const H: usize = 288;

fn textured(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let mut s: u32 = 0x1234_5678;
    for p in v.iter_mut() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *p = (s >> 24) as u8;
    }
    v
}

/// The real, measured call mix for preset=quality on mobile_cif
/// (size, bw, bh, phase, share%) — phase: 0 full, 1 half H/V, 2 half centre, 3 quarter.
const MIX: [(&str, usize, usize, u8, f64); 12] = [
    ("16x16", 16, 16, 0, 3.01),
    ("16x16", 16, 16, 1, 5.04),
    ("16x16", 16, 16, 2, 2.26),
    ("16x16", 16, 16, 3, 6.92),
    ("16x8", 16, 8, 0, 7.09),
    ("16x8", 16, 8, 1, 12.37),
    ("16x8", 16, 8, 2, 5.54),
    ("16x8", 16, 8, 3, 16.88),
    ("8x8", 8, 8, 0, 5.75),
    ("8x8", 8, 8, 1, 12.37),
    ("8x8", 8, 8, 2, 5.54),
    ("8x8", 8, 8, 3, 17.24),
];

fn phase_mv(phase: u8) -> (i32, i32) {
    match phase {
        0 => (0, 0),
        1 => (2, 0),
        2 => (2, 2),
        _ => (1, 1),
    }
}

/// TODAY: one `mc_luma` call at this size/phase.
fn arm_current(reference: &[u8], bw: usize, bh: usize, phase: u8, iters: usize) -> f64 {
    let (fx, fy) = phase_mv(phase);
    let mut out = [0u8; 256];
    let mut acc = 0u64;
    let t = Instant::now();
    for i in 0..iters {
        let x0 = 8 + (i * 7) % (W - bw - 24);
        let y0 = 8 + (i * 13) % (H - bh - 24);
        mc_luma(reference, W, H, x0, y0, bw, bh, fx, fy, &mut out);
        acc = acc.wrapping_add(out[0] as u64);
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

// Const-width block ops. A real plane-cache implementation would specialise on the
// handful of legal widths exactly like `mc_luma` already does for its 16×16 full-pel
// path — with a RUNTIME `bw` each row lowers to a variable-length `memcpy` call and
// the average loop will not vectorise, which would make this a strawman arm and
// under-measure the prize.
#[inline]
fn copy_blk<const BW: usize>(p: &[u8], base: usize, bh: usize, out: &mut [u8]) {
    for dy in 0..bh {
        out[dy * BW..dy * BW + BW].copy_from_slice(&p[base + dy * W..][..BW]);
    }
}

#[inline]
fn avg_blk<const BW: usize>(pa: &[u8], pb: &[u8], base: usize, bh: usize, out: &mut [u8]) {
    for dy in 0..bh {
        let sa = &pa[base + dy * W..][..BW];
        let sb = &pb[base + dy * W..][..BW];
        let o = &mut out[dy * BW..dy * BW + BW];
        for dx in 0..BW {
            o[dx] = ((sa[dx] as u16 + sb[dx] as u16 + 1) >> 1) as u8;
        }
    }
}

/// WITH PLANES: full/half = strided copy from one plane; quarter = 2-tap average
/// of two planes. This is exactly the work x264's sub-pel search does per candidate.
fn arm_planes(pa: &[u8], pb: &[u8], bw: usize, bh: usize, phase: u8, iters: usize) -> f64 {
    let mut out = [0u8; 256];
    let mut acc = 0u64;
    let t = Instant::now();
    for i in 0..iters {
        let x0 = 8 + (i * 7) % (W - bw - 24);
        let y0 = 8 + (i * 13) % (H - bh - 24);
        let base = y0 * W + x0;
        match (phase == 3, bw) {
            (true, 16) => avg_blk::<16>(pa, pb, base, bh, &mut out),
            (true, _) => avg_blk::<8>(pa, pb, base, bh, &mut out),
            (false, 16) => copy_blk::<16>(pa, base, bh, &mut out),
            (false, _) => copy_blk::<8>(pa, base, bh, &mut out),
        }
        acc = acc.wrapping_add(out[0] as u64);
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

/// Cost of BUILDING the three half-pel planes for one frame (the tax).
/// Scalar separable 6-tap — a conservative upper bound; x264's is asm.
fn build_planes(reference: &[u8]) -> (f64, Vec<u8>, Vec<u8>) {
    let tap = |a: i32, b: i32, c: i32, d: i32, e: i32, f: i32| a - 5 * b + 20 * c + 20 * d - 5 * e + f;
    let t = Instant::now();
    let mut ph = vec![0u8; W * H]; // horizontal half-pel
    let mut pv = vec![0u8; W * H]; // vertical half-pel
    let mut pc = vec![0u8; W * H]; // centre ("j")
    let px = |x: isize, y: isize| -> i32 {
        reference[(y.clamp(0, H as isize - 1) as usize) * W + x.clamp(0, W as isize - 1) as usize] as i32
    };
    for y in 0..H as isize {
        for x in 0..W as isize {
            let h = tap(px(x - 2, y), px(x - 1, y), px(x, y), px(x + 1, y), px(x + 2, y), px(x + 3, y));
            ph[y as usize * W + x as usize] = ((h + 16) >> 5).clamp(0, 255) as u8;
            let v = tap(px(x, y - 2), px(x, y - 1), px(x, y), px(x, y + 1), px(x, y + 2), px(x, y + 3));
            pv[y as usize * W + x as usize] = ((v + 16) >> 5).clamp(0, 255) as u8;
        }
    }
    // Centre plane: vertical 6-tap over the *unrounded* horizontal intermediates.
    for y in 0..H as isize {
        for x in 0..W as isize {
            let hh = |yy: isize| tap(px(x - 2, yy), px(x - 1, yy), px(x, yy), px(x + 1, yy), px(x + 2, yy), px(x + 3, yy));
            let j = tap(hh(y - 2), hh(y - 1), hh(y), hh(y + 1), hh(y + 2), hh(y + 3));
            pc[y as usize * W + x as usize] = ((j + 512) >> 10).clamp(0, 255) as u8;
        }
    }
    let e = t.elapsed().as_secs_f64() * 1e3;
    std::hint::black_box(&pc);
    (e, ph, pv)
}

fn main() {
    let reference = textured(W * H);
    let iters: usize = std::env::var("HP_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(150_000);
    let rounds: usize = std::env::var("HP_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(7);

    let (build_ms, ph, pv) = build_planes(&reference);

    // Interleaved measurement of both arms across the whole mix.
    let mut cur = vec![Vec::new(); MIX.len()];
    let mut pln = vec![Vec::new(); MIX.len()];
    for r in 0..rounds {
        let order: Vec<usize> = if r % 2 == 0 { (0..MIX.len()).collect() } else { (0..MIX.len()).rev().collect() };
        for &i in &order {
            let (_, bw, bh, phase, _) = MIX[i];
            if r % 2 == 0 {
                cur[i].push(arm_current(&reference, bw, bh, phase, iters));
                pln[i].push(arm_planes(&ph, &pv, bw, bh, phase, iters));
            } else {
                pln[i].push(arm_planes(&ph, &pv, bw, bh, phase, iters));
                cur[i].push(arm_current(&reference, bw, bh, phase, iters));
            }
        }
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };

    println!("half-pel plane cache — CEILING probe ({W}x{H}, {iters} calls/arm, {rounds} rounds)\n");
    println!("{:<8} {:<9} {:>7} {:>11} {:>11} {:>9}", "size", "phase", "share%", "now ns", "planes ns", "speedup");
    println!("{}", "-".repeat(62));

    let (mut w_now, mut w_pln) = (0.0f64, 0.0f64);
    for (i, (name, _, _, phase, share)) in MIX.iter().enumerate() {
        let (c, p) = (med(&mut cur[i]), med(&mut pln[i]));
        let pl = ["fullpel", "half-HV", "half-ctr", "quarter"][*phase as usize];
        println!("{:<8} {:<9} {:>7.2} {:>11.1} {:>11.1} {:>8.2}x", name, pl, share, c, p, c / p);
        w_now += share / 100.0 * c;
        w_pln += share / 100.0 * p;
    }

    // Mix-weighted per-call cost, then scale to the real encode.
    // Measured on mobile_cif 30f, preset=quality: inter-mc = 459.6 ms / 3,177,868 calls.
    const REAL_MS: f64 = 459.6;
    const REAL_CALLS: f64 = 3_177_868.0;
    const FRAMES: f64 = 30.0;
    let scale = (REAL_MS * 1e6 / REAL_CALLS) / w_now; // reconcile probe ns -> in-context ns
    let pln_ms = w_pln * scale * REAL_CALLS / 1e6;
    let build_total = build_ms * FRAMES;

    println!("\n--- mix-weighted ---");
    println!("  now        {w_now:>8.1} ns/call");
    println!("  planes     {w_pln:>8.1} ns/call   ({:.2}x cheaper)", w_now / w_pln);
    println!("  in-context reconciliation factor: {scale:.2}x (probe -> real encode)");
    println!("\n--- projected on the real encode (mobile_cif 30f, quality) ---");
    println!("  inter-mc now                {REAL_MS:>9.1} ms");
    println!("  inter-mc with planes        {pln_ms:>9.1} ms");
    println!("  plane build tax (3 planes x {FRAMES:.0} frames, SCALAR){build_total:>9.1} ms");
    let net = REAL_MS - pln_ms - build_total;
    println!("  --------------------------------------------");
    println!("  NET PRIZE                   {net:>9.1} ms  ({:.1}% of inter-mc)", 100.0 * net / REAL_MS);
    println!("\n  encode TOTAL was 716.3 ms -> projected {:.1} ms ({:.2}x)", 716.3 - net, 716.3 / (716.3 - net));
    println!("\n  NOTE: build tax is scalar here; x264's asm hpel-filter costs ~0.19 ns/px");
    println!("        (188 ms for a ~993 Mpx corpus), i.e. ~{:.2} ms for this clip.", 0.19 * (W * H) as f64 * FRAMES / 1e6);
}
