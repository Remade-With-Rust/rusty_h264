//! CEILING PROBE — is the bS derivation's 25x gap vs x264 really about LAYOUT?
//!
//! `docs/WHYS-decoder-perf.md` established: bS derivation is ~20% of decode at
//! ~400 ns/MB, against x264's `deblock_strength_avx2` at ~15 ns/MB, and the Step 0
//! audit showed our derivation emits ZERO packed ops because `gather_tile` is a
//! gather — 24 blocks across 4-6 SEPARATE frame-wide arrays.
//!
//! The hypothesis is that x264 wins on LAYOUT (packed per-MB nnz bitmask + per-MB
//! contiguous ref/mv) and that the SIMD is downstream of it. That implies a large,
//! risky refactor of the decoder's macroblock loop, so PROVE THE CEILING FIRST:
//! measure the gather alone, current layout vs packed, with no decoder changes.
//!
//! The probe deliberately measures ONLY the gather. If packed-vs-strided is a small
//! ratio here, the refactor cannot pay however good a kernel sits behind it — and
//! that is a whole day saved for twenty minutes of probe.
//!
//! Also reports a DETERMINISTIC cache-line count, because this box cannot currently
//! resolve timing (see the noise-floor sections of the WHYS doc): the line count is
//! the part of the answer that machine load cannot touch.
//!
//! ```text
//! cargo run --release -p rusty_h264-common --example bs_layout_ceiling
//! ```

use std::hint::black_box;
use std::time::Instant;

const MB_W: usize = 80; // 1280/16
const MB_H: usize = 45; // 720/16
const W4: usize = MB_W * 4;

/// The CURRENT shape: separate frame-wide arrays, one per field.
struct Soa {
    inter: Vec<bool>,
    nnz: Vec<u8>,
    mv: Vec<(i32, i32)>,
    ref_id: Vec<i32>,
}

/// The PACKED shape, mirroring x264's per-macroblock cache: everything one
/// macroblock's derivation needs, contiguous, in raster order.
///
/// `nnz` collapses to a 16-BIT MASK — the derivation only ever asks "is this block
/// coded", never for the count — which is also what makes the coefficient half of
/// the strength test a shift-and-or on a single register instead of 32 byte loads.
#[derive(Clone, Copy, Default)]
#[repr(C, align(64))]
struct MbRec {
    nnz_mask: u16,
    inter: bool,
    _pad: u8,
    ref_id: [i16; 16],
    mv: [(i16, i16); 16],
}

fn build_soa() -> Soa {
    let n = W4 * MB_H * 4;
    // Deterministic pseudo-random content; a plausible mix of coded/uncoded and
    // varying motion, so neither layout gets an unrealistically friendly pattern.
    let mut s = 0x2545F491u32;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let mut soa =
        Soa { inter: vec![true; n], nnz: vec![0; n], mv: vec![(0, 0); n], ref_id: vec![0; n] };
    for i in 0..n {
        let r = rnd();
        soa.inter[i] = (r & 0x1f) != 0; // ~3% intra
        soa.nnz[i] = if (r >> 5) & 3 == 0 { 1 } else { 0 };
        soa.mv[i] = (((r >> 7) & 0x3f) as i32 - 32, ((r >> 13) & 0x3f) as i32 - 32);
        soa.ref_id[i] = ((r >> 19) & 1) as i32;
    }
    soa
}

fn build_packed(soa: &Soa) -> Vec<MbRec> {
    let mut v = vec![MbRec::default(); MB_W * MB_H];
    for mb_y in 0..MB_H {
        for mb_x in 0..MB_W {
            let rec = &mut v[mb_y * MB_W + mb_x];
            let (bx0, by0) = (mb_x * 4, mb_y * 4);
            rec.inter = soa.inter[by0 * W4 + bx0];
            for r in 0..4 {
                for c in 0..4 {
                    let i = (by0 + r) * W4 + bx0 + c;
                    let k = r * 4 + c;
                    if soa.nnz[i] != 0 {
                        rec.nnz_mask |= 1 << k;
                    }
                    rec.ref_id[k] = soa.ref_id[i] as i16;
                    rec.mv[k] = (soa.mv[i].0 as i16, soa.mv[i].1 as i16);
                }
            }
        }
    }
    v
}

/// ARM A — the gather as it exists today: 24 blocks, each pulling from 4 separate
/// frame-wide arrays at a strided index.
#[inline(never)]
fn gather_strided(soa: &Soa, mb_x: usize, mb_y: usize) -> u64 {
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let mut acc = 0u64;
    let mut take = |i: usize| {
        acc = acc
            .wrapping_add(soa.inter[i] as u64)
            .wrapping_add(soa.nnz[i] as u64)
            .wrapping_add(soa.ref_id[i] as u64)
            .wrapping_add(soa.mv[i].0 as u64)
            .wrapping_add(soa.mv[i].1 as u64);
    };
    for r in 0..4 {
        for c in 0..4 {
            take((by0 + r) * W4 + bx0 + c);
        }
    }
    if mb_x > 0 {
        for r in 0..4 {
            take((by0 + r) * W4 + bx0 - 1);
        }
    }
    if mb_y > 0 {
        for c in 0..4 {
            take((by0 - 1) * W4 + bx0 + c);
        }
    }
    acc
}

/// ARM B — the same information out of packed per-macroblock records: one
/// contiguous record for this macroblock plus the left and top neighbours'.
#[inline(never)]
fn gather_packed(recs: &[MbRec], mb_x: usize, mb_y: usize) -> u64 {
    let cur = &recs[mb_y * MB_W + mb_x];
    let mut acc = cur.nnz_mask as u64 + cur.inter as u64;
    for k in 0..16 {
        acc = acc
            .wrapping_add(cur.ref_id[k] as u64)
            .wrapping_add(cur.mv[k].0 as u64)
            .wrapping_add(cur.mv[k].1 as u64);
    }
    if mb_x > 0 {
        let l = &recs[mb_y * MB_W + mb_x - 1];
        for r in 0..4 {
            let k = r * 4 + 3; // right-hand column of the left neighbour
            acc = acc
                .wrapping_add(l.ref_id[k] as u64)
                .wrapping_add(l.mv[k].0 as u64)
                .wrapping_add(l.mv[k].1 as u64);
        }
    }
    if mb_y > 0 {
        let t = &recs[(mb_y - 1) * MB_W + mb_x];
        for c in 0..4 {
            let k = 12 + c; // bottom row of the top neighbour
            acc = acc
                .wrapping_add(t.ref_id[k] as u64)
                .wrapping_add(t.mv[k].0 as u64)
                .wrapping_add(t.mv[k].1 as u64);
        }
    }
    acc
}

fn main() {
    let soa = build_soa();
    let recs = build_packed(&soa);
    let mbs = MB_W * MB_H;

    // ---- DETERMINISTIC part: distinct 64-byte cache lines touched per macroblock.
    // Immune to machine load, which is why it leads.
    let line = |addr: usize| addr / 64;
    let mut a_lines = std::collections::HashSet::new();
    let mut b_lines = std::collections::HashSet::new();
    let (mb_x, mb_y) = (MB_W / 2, MB_H / 2);
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let base = |p: *const u8| p as usize;
    for r in 0..4 {
        for c in 0..4 {
            let i = (by0 + r) * W4 + bx0 + c;
            a_lines.insert(line(base(soa.inter.as_ptr() as *const u8) + i));
            a_lines.insert(line(base(soa.nnz.as_ptr()) + i));
            a_lines.insert(line(base(soa.ref_id.as_ptr() as *const u8) + i * 4));
            a_lines.insert(line(base(soa.mv.as_ptr() as *const u8) + i * 8));
        }
    }
    let rec_base = base(recs.as_ptr() as *const u8) + (mb_y * MB_W + mb_x) * size_of::<MbRec>();
    for off in (0..size_of::<MbRec>()).step_by(64) {
        b_lines.insert(line(rec_base + off));
    }

    println!("bS-derivation LAYOUT ceiling probe — 720p, {mbs} macroblocks\n");
    println!("DETERMINISTIC (machine load cannot move these):");
    println!("  MbRec size                     {:>6} bytes", size_of::<MbRec>());
    println!("  cache lines / MB, strided SoA  {:>6}   (own 16 blocks x 4 arrays)", a_lines.len());
    println!("  cache lines / MB, packed       {:>6}", b_lines.len());
    println!(
        "  ratio                          {:>6.2}x fewer lines\n",
        a_lines.len() as f64 / b_lines.len().max(1) as f64
    );

    // ---- TIMED part: best-of-N, arms ABBA-alternated. Caveated on purpose.
    let reps = 30;
    let (mut best_a, mut best_b) = (f64::MAX, f64::MAX);
    for rep in 0..reps {
        let run_a = || {
            let t = Instant::now();
            let mut acc = 0u64;
            for y in 0..MB_H {
                for x in 0..MB_W {
                    acc = acc.wrapping_add(gather_strided(black_box(&soa), x, y));
                }
            }
            (t.elapsed().as_secs_f64(), black_box(acc))
        };
        let run_b = || {
            let t = Instant::now();
            let mut acc = 0u64;
            for y in 0..MB_H {
                for x in 0..MB_W {
                    acc = acc.wrapping_add(gather_packed(black_box(&recs), x, y));
                }
            }
            (t.elapsed().as_secs_f64(), black_box(acc))
        };
        // Alternate which arm runs first so warm-up bias cancels.
        let (ta, tb) = if rep % 2 == 0 {
            let (ta, _) = run_a();
            let (tb, _) = run_b();
            (ta, tb)
        } else {
            let (tb, _) = run_b();
            let (ta, _) = run_a();
            (ta, tb)
        };
        best_a = best_a.min(ta);
        best_b = best_b.min(tb);
    }
    let ns = |t: f64| t * 1e9 / mbs as f64;
    println!("TIMED (best-of-{reps}, arms alternated — treat as INDICATIVE only;");
    println!("       this box has shown 45% within-arm spread under foreign load):");
    println!("  strided SoA gather   {:>7.1} ns/MB", ns(best_a));
    println!("  packed gather        {:>7.1} ns/MB", ns(best_b));
    println!("  ratio                {:>7.2}x", best_a / best_b);
    println!("\nFor scale: the whole derivation measures ~400 ns/MB in context;");
    println!("x264's deblock_strength_avx2 does the entire job in ~15 ns/MB.");
}
