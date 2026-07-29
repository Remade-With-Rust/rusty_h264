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
fn sad_x4_matches_scalar() {
    let mut st = 0x0dd0_c0de_1234_5678u64;
    let mut tested = 0u64;
    for _ in 0..6000 {
        let ss = 16 + (lcg(&mut st) as usize % 48);
        let rs = 16 + (lcg(&mut st) as usize % 48);
        let src: Vec<u8> = (0..15 * ss + 16 + 8).map(|_| lcg(&mut st)).collect();
        let base: Vec<u8> = (0..15 * rs + 16 + 4 * rs + 64).map(|_| lcg(&mut st)).collect();
        let mut o = [0usize; 4];
        for oi in &mut o {
            *oi = (lcg(&mut st) as usize % 4) * rs + (lcg(&mut st) as usize % 48);
        }
        let Some(x4) = rusty_h264_accel::sad_x4(&src, ss, &base, o, rs, 16, 16) else {
            eprintln!("sad_x4: AVX2 unavailable — kernel not in play on this host");
            return;
        };
        for k in 0..4 {
            let mut s = 0u32;
            for r in 0..16 {
                let a = &src[r * ss..][..16];
                let b = &base[o[k] + r * rs..][..16];
                s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
            }
            assert_eq!(x4[k], s, "sad_x4 lane {k} ss={ss} rs={rs}");
            tested += 1;
        }
    }
    eprintln!("sad_16x16_x4: {tested} lanes byte-exact");
}

#[test]
fn satd_avg_x4_matches_scalar() {
    let mut st = 0xabcd_ef01_2345_6789u64;
    let mut tested = 0u64;
    for _ in 0..3000 {
        let ss = 16 + (lcg(&mut st) as usize % 32);
        let rs = 16 + (lcg(&mut st) as usize % 32);
        let src: Vec<u8> = (0..15 * ss + 16 + 8).map(|_| lcg(&mut st)).collect();
        let planes: Vec<Vec<u8>> =
            (0..3).map(|_| (0..15 * rs + 16 + 4 * rs + 64).map(|_| lcg(&mut st)).collect()).collect();
        let mut pairs_idx = [(0usize, 0usize, 0usize, 0usize); 4];
        for p in &mut pairs_idx {
            *p = (
                lcg(&mut st) as usize % 3,
                (lcg(&mut st) as usize % 4) * rs + (lcg(&mut st) as usize % 32),
                lcg(&mut st) as usize % 3,
                (lcg(&mut st) as usize % 4) * rs + (lcg(&mut st) as usize % 32),
            );
        }
        let pairs = [
            (&planes[pairs_idx[0].0][..], pairs_idx[0].1, &planes[pairs_idx[0].2][..], pairs_idx[0].3),
            (&planes[pairs_idx[1].0][..], pairs_idx[1].1, &planes[pairs_idx[1].2][..], pairs_idx[1].3),
            (&planes[pairs_idx[2].0][..], pairs_idx[2].1, &planes[pairs_idx[2].2][..], pairs_idx[2].3),
            (&planes[pairs_idx[3].0][..], pairs_idx[3].1, &planes[pairs_idx[3].2][..], pairs_idx[3].3),
        ];
        let Some(x4) = rusty_h264_accel::satd_avg_x4(&src, ss, pairs, rs, 16, 16) else {
            eprintln!("satd_avg_x4: AVX2 unavailable — kernel not in play on this host");
            return;
        };
        for k in 0..4 {
            let (pa, oa, pb, ob) = pairs[k];
            let want = reference(&src, ss, &pa[oa..], &pb[ob..], rs, 16, 16);
            assert_eq!(x4[k] as i64, want, "satd_avg_x4 lane {k} ss={ss} rs={rs}");
            tested += 1;
        }
    }
    eprintln!("satd_avg_16x16_x4: {tested} lanes byte-exact");
}

/// The x4 family across EVERY ME partition shape (16×16/16×8/8×16/8×8):
/// `sad_x4`, `satd_x4` (base+offsets), `satd_x4p` (independent planes) and
/// `satd_avg_x4` all pinned to scalar. Plain SATD reuses `reference` with b == a
/// (avg(a,a) = a exactly).
#[test]
fn x4_family_all_shapes_match_scalar() {
    let mut st = 0x7777_1234_aaaa_5555u64;
    let mut tested = 0u64;
    for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8)] {
        for _ in 0..1500 {
            let ss = w + (lcg(&mut st) as usize % 32);
            let rs = w + (lcg(&mut st) as usize % 32);
            let src: Vec<u8> = (0..(h - 1) * ss + w + 8).map(|_| lcg(&mut st)).collect();
            let base: Vec<u8> =
                (0..(h - 1) * rs + w + 4 * rs + 64).map(|_| lcg(&mut st)).collect();
            let b2: Vec<u8> = (0..base.len()).map(|_| lcg(&mut st)).collect();
            let mut o = [0usize; 4];
            for oi in &mut o {
                *oi = (lcg(&mut st) as usize % 4) * rs + (lcg(&mut st) as usize % 32);
            }
            let (Some(sad), Some(satd), Some(satdp), Some(avg)) = (
                rusty_h264_accel::sad_x4(&src, ss, &base, o, rs, w, h),
                rusty_h264_accel::satd_x4(&src, ss, &base, o, rs, w, h),
                rusty_h264_accel::satd_x4p(
                    &src, ss,
                    [(&base, o[0]), (&base, o[1]), (&base, o[2]), (&base, o[3])],
                    rs, w, h,
                ),
                rusty_h264_accel::satd_avg_x4(
                    &src, ss,
                    [(&base, o[0], &b2, o[1]), (&base, o[1], &b2, o[2]),
                     (&base, o[2], &b2, o[3]), (&base, o[3], &b2, o[0])],
                    rs, w, h,
                ),
            ) else {
                eprintln!("x4 family: AVX2 unavailable — kernels not in play");
                return;
            };
            for k in 0..4 {
                let mut s = 0u32;
                for r in 0..h {
                    let a = &src[r * ss..][..w];
                    let bb = &base[o[k] + r * rs..][..w];
                    s += a.iter().zip(bb).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
                }
                assert_eq!(sad[k], s, "sad_x4 {w}x{h} lane {k}");
                let want = reference(&src, ss, &base[o[k]..], &base[o[k]..], rs, w, h);
                assert_eq!(satd[k] as i64, want, "satd_x4 {w}x{h} lane {k}");
                assert_eq!(satdp[k] as i64, want, "satd_x4p {w}x{h} lane {k}");
                let (oa, ob) = (o[k], o[(k + 1) % 4]);
                let wanta = reference(&src, ss, &base[oa..], &b2[ob..], rs, w, h);
                assert_eq!(avg[k] as i64, wanta, "satd_avg_x4 {w}x{h} lane {k}");
                tested += 4;
            }
        }
    }
    eprintln!("x4 family: {tested} lane-checks byte-exact across all shapes");
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
