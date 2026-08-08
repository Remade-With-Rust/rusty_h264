//! **Forward/inverse 4x4 transform and quantization** — portable (rip-ASM Phase 5b).
//!
//! Replaces openh264's `WelsDctFourT4`, `WelsIDctFourT4Rec` and `WelsQuantFour4x4`
//! (dct.asm 1,036 + decoder/dct.asm 72 + quant.asm 507 LOC).
//!
//! ## Deliberately scalar-with-a-vectorizable-shape, not hand-written SIMD
//!
//! Each of these processes four 4x4 blocks — 64 values. The butterflies are pure
//! integer add/sub/shift over `i32`, which LLVM auto-vectorizes; the campaign's own
//! rule is to check the emitted code and MEASURE before reaching for intrinsics,
//! because "the compiler already did it" is the single most common refutation.
//! Fixed-size arrays and index-free iteration are used so the bounds checks fold away.
//! If measurement says these are hot, the intrinsics go in behind the same API.
//!
//! Everything here is bit-exact against `common`'s `forward_core` / `inverse_core`
//! and against openh264's `WELS_NEW_QUANT`, pinned by the tests that previously held
//! the assembly to those same references.

/// One-dimensional forward butterfly (a row of `Cf`). Mirrors `common::transform`.
#[inline]
fn fwd_1d(x0: i32, x1: i32, x2: i32, x3: i32) -> (i32, i32, i32, i32) {
    let (t0, t1, t2, t3) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
    (t0 + t1, 2 * t3 + t2, t0 - t1, t3 - 2 * t2)
}

/// One-dimensional inverse butterfly (a row of `Ci`).
#[inline]
fn inv_1d(d0: i32, d1: i32, d2: i32, d3: i32) -> (i32, i32, i32, i32) {
    let (e0, e1) = (d0 + d2, d0 - d2);
    let (e2, e3) = ((d1 >> 1) - d3, d1 + (d3 >> 1));
    (e0 + e3, e1 + e2, e1 - e2, e0 - e3)
}

/// `W = Cf · X · Cfᵀ` over one row-major 4x4 block.
#[inline]
fn forward_core(m: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, b, c, d) = fwd_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = fwd_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
}

/// Inverse core + the final `(x + 32) >> 6`.
///
/// ROW-FIRST, THEN COLUMN — the order is NOT interchangeable. The `>> 1` inside
/// `inv_1d` makes the integer transform non-separable, so a column-first pass diverges
/// by +/-1 on asymmetric blocks (only visible at low QP / high-frequency content).
/// Spec 8.5.12.2 fixes the order and the decoder must agree exactly.
#[inline]
fn inverse_core(m: &mut [i32; 16]) {
    for r in 0..4 {
        let (a, b, c, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
    for v in m.iter_mut() {
        *v = (*v + 32) >> 6;
    }
}

/// The four 4x4 sub-blocks of an 8x8 region, in openh264's order.
const SUBBLOCKS: [(usize, usize); 4] = [(0, 0), (4, 0), (0, 4), (4, 4)];

/// Forward-transform the residual of four 4x4 blocks covering an 8x8 region.
///
/// `dct` receives 64 coefficients, block-major in `SUBBLOCKS` order.
pub fn dct_four_t4(dct: &mut [i16], src: &[u8], stride_src: usize, pred: &[u8], stride_pred: usize) {
    assert!(dct.len() >= 64);
    assert!(src.len() >= 7 * stride_src + 8 && pred.len() >= 7 * stride_pred + 8);
    for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
        let mut m = [0i32; 16];
        for dy in 0..4 {
            for dx in 0..4 {
                m[dy * 4 + dx] = src[(oy + dy) * stride_src + ox + dx] as i32
                    - pred[(oy + dy) * stride_pred + ox + dx] as i32;
            }
        }
        forward_core(&mut m);
        for i in 0..16 {
            dct[k * 16 + i] = m[i] as i16;
        }
    }
}

/// Inverse-transform four 4x4 coefficient blocks and add them to `pred`, writing the
/// reconstruction into `rec`.
pub fn idct_four_t4_rec(
    rec: &mut [u8], stride_rec: usize, pred: &[u8], stride_pred: usize, dct: &[i16],
) {
    assert!(dct.len() >= 64);
    assert!(rec.len() >= 7 * stride_rec + 8 && pred.len() >= 7 * stride_pred + 8);
    for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
        let mut m = [0i32; 16];
        for i in 0..16 {
            m[i] = dct[k * 16 + i] as i32;
        }
        inverse_core(&mut m);
        for dy in 0..4 {
            for dx in 0..4 {
                let v = pred[(oy + dy) * stride_pred + ox + dx] as i32 + m[dy * 4 + dx];
                rec[(oy + dy) * stride_rec + ox + dx] = v.clamp(0, 255) as u8;
            }
        }
    }
}

/// openh264's `WELS_NEW_QUANT` over four 4x4 blocks, in place.
///
/// `level = sign(c) * ((|c| + FF[pos]) * MF[pos]) >> 16`, with `pos = (row & 1)*4 + col`
/// — the tables hold two rows and are reused for rows 2/3. NOTE this is openh264's
/// quantizer (dead-zone added BEFORE the multiply, fixed `>>16`), structurally
/// different from our own `(|c|*MF + F) >> qbits`, and deliberately kept as-is.
pub fn quant_four_4x4(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
    assert!(dct.len() >= 64);
    for blk in 0..4 {
        for row in 0..4 {
            for col in 0..4 {
                let idx = blk * 16 + row * 4 + col;
                let pos = (row & 1) * 4 + col;
                let c = dct[idx] as i32;
                let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                dct[idx] = (if c < 0 { -lvl } else { lvl }) as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dct_matches_reference() {
        for seed in 0..128usize {
            let (mut src, mut pred) = ([0u8; 64], [0u8; 64]);
            for y in 0..8 {
                for x in 0..8 {
                    src[y * 8 + x] = ((y * 31 + x * 17 + seed * 7) & 0xff) as u8;
                    pred[y * 8 + x] = ((y * 13 + x * 41 + seed * 5 + 9) & 0xff) as u8;
                }
            }
            let mut dct = [0i16; 64];
            dct_four_t4(&mut dct, &src, 8, &pred, 8);
            for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
                let mut want = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        want[dy * 4 + dx] = src[(oy + dy) * 8 + ox + dx] as i32
                            - pred[(oy + dy) * 8 + ox + dx] as i32;
                    }
                }
                forward_core(&mut want);
                for i in 0..16 {
                    assert_eq!(dct[k * 16 + i] as i32, want[i], "seed {seed} blk {k} c {i}");
                }
            }
        }
    }

    #[test]
    fn idct_rec_matches_reference() {
        for seed in 0..128usize {
            let mut pred = [0u8; 64];
            let mut dct = [0i16; 64];
            for i in 0..64 {
                pred[i] = ((i * 7 + seed * 3) & 0xff) as u8;
                dct[i] = (((i as i32 * 53 + seed as i32 * 29) % 4096) - 2048) as i16;
            }
            let mut rec = [0u8; 64];
            idct_four_t4_rec(&mut rec, 8, &pred, 8, &dct);
            for (k, (ox, oy)) in SUBBLOCKS.iter().enumerate() {
                let mut m = [0i32; 16];
                for i in 0..16 {
                    m[i] = dct[k * 16 + i] as i32;
                }
                inverse_core(&mut m);
                for dy in 0..4 {
                    for dx in 0..4 {
                        let want = (pred[(oy + dy) * 8 + ox + dx] as i32 + m[dy * 4 + dx])
                            .clamp(0, 255) as u8;
                        assert_eq!(rec[(oy + dy) * 8 + ox + dx], want, "seed {seed} blk {k}");
                    }
                }
            }
        }
    }

    /// openh264's WELS_NEW_QUANT, spelled out independently of the implementation.
    #[test]
    fn quant_matches_openh264_c() {
        let ff: [i16; 8] = [80, 85, 80, 85, 90, 95, 90, 95];
        let mf: [i16; 8] = [410, 420, 410, 420, 430, 440, 430, 440];
        for seed in 0..64i32 {
            let mut input = [0i16; 64];
            for (k, v) in input.iter_mut().enumerate() {
                *v = (((k as i32 * 37 + seed * 53) % 2000) - 1000) as i16;
            }
            let mut got = input;
            quant_four_4x4(&mut got, &ff, &mf);
            for blk in 0..4 {
                for row in 0..4 {
                    for col in 0..4 {
                        let idx = blk * 16 + row * 4 + col;
                        let pos = (row & 1) * 4 + col;
                        let c = input[idx] as i32;
                        let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                        let want = (if c < 0 { -lvl } else { lvl }) as i16;
                        assert_eq!(got[idx], want, "seed {seed} blk {blk} ({row},{col})");
                    }
                }
            }
        }
    }

    /// The inverse transform's row-then-column order is load-bearing: a column-first
    /// pass differs by +/-1 on asymmetric blocks. Pin it so a "tidy-up" cannot swap
    /// them. Searches for a witness rather than asserting one hand-picked input
    /// differs — the first version picked an input where the two orders happened to
    /// agree, so the test passed while proving nothing.
    #[test]
    fn inverse_order_is_row_then_column() {
        fn col_first(mut m: [i32; 16]) -> [i32; 16] {
            for c in 0..4 {
                let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
                m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
            }
            for r in 0..4 {
                let (a, b, c2, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
                m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c2; m[r * 4 + 3] = d;
            }
            for v in m.iter_mut() { *v = (*v + 32) >> 6; }
            m
        }
        let mut witnesses = 0;
        for seed in 0..512i32 {
            let mut m = [0i32; 16];
            for (i, v) in m.iter_mut().enumerate() {
                *v = ((i as i32 * 71 + seed * 137) % 2048) - 1024;
            }
            let mut row = m;
            inverse_core(&mut row);
            if row != col_first(m) { witnesses += 1; }
        }
        assert!(witnesses > 0,
            "no input distinguished the two orders — this test would pass on a swapped              implementation and is therefore worthless as written");
    }
}
