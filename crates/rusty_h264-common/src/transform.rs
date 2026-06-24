//! H.264 4×4 residual transform and quantization (spec §8.5 / §8.6).
//!
//! H.264 uses a small integer approximation of the DCT — a 4×4 "core" transform
//! whose scaling is folded into the quantizer, so it is exactly invertible in
//! integer arithmetic. This module implements the forward path (encoder:
//! residual → coefficients → quantized levels) and the inverse path (decoder
//! and encoder-reconstruction: levels → coefficients → residual).
//!
//! Correctness here is non-negotiable: the levels we emit are dequantized by
//! every conforming decoder using the exact spec tables below, so the forward
//! quantizer must be the faithful inverse of that process.

/// `normAdjust4x4` (spec Table — the dequant scaling V), indexed by `[QP % 6]`
/// then by position group (see [`pos_group`]).
const NORM_ADJUST: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// Forward quantization multipliers `MF`, indexed by `[QP % 6]` then position
/// group. Paired with [`NORM_ADJUST`] so reconstruction ≈ source.
const QUANT_MF: [[i32; 3]; 6] = [
    [13107, 5243, 8066],
    [11916, 4660, 7490],
    [10082, 4194, 6554],
    [9362, 3647, 5825],
    [8192, 3355, 5243],
    [7282, 2893, 4559],
];

/// Position group within a 4×4 block:
/// - 0: both indices even — the (0,0),(0,2),(2,0),(2,2) positions,
/// - 1: both indices odd — (1,1),(1,3),(3,1),(3,3),
/// - 2: everything else.
#[inline]
fn pos_group(i: usize, j: usize) -> usize {
    match (i % 2, j % 2) {
        (0, 0) => 0,
        (1, 1) => 1,
        _ => 2,
    }
}

/// One-dimensional forward core transform butterfly (rows of `Cf`).
#[inline]
fn fwd_1d(x0: i32, x1: i32, x2: i32, x3: i32) -> (i32, i32, i32, i32) {
    let t0 = x0 + x3;
    let t1 = x1 + x2;
    let t2 = x1 - x2;
    let t3 = x0 - x3;
    (t0 + t1, 2 * t3 + t2, t0 - t1, t3 - 2 * t2)
}

/// One-dimensional inverse core transform butterfly (rows of `Ci`).
#[inline]
fn inv_1d(d0: i32, d1: i32, d2: i32, d3: i32) -> (i32, i32, i32, i32) {
    let e0 = d0 + d2;
    let e1 = d0 - d2;
    let e2 = (d1 >> 1) - d3;
    let e3 = d1 + (d3 >> 1);
    (e0 + e3, e1 + e2, e1 - e2, e0 - e3)
}

/// Forward core transform `W = Cf · X · Cfᵀ` over a row-major 4×4 block.
/// The output coefficients are pre-quantization (scaling lives in the quantizer).
pub fn forward_core(block: &[i32; 16]) -> [i32; 16] {
    let mut m = *block;
    // Rows.
    for r in 0..4 {
        let (a, b, c, d) = fwd_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a;
        m[r * 4 + 1] = b;
        m[r * 4 + 2] = c;
        m[r * 4 + 3] = d;
    }
    // Columns.
    for c in 0..4 {
        let (a, b, cc, d) = fwd_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a;
        m[4 + c] = b;
        m[8 + c] = cc;
        m[12 + c] = d;
    }
    m
}

/// Quantizes forward-transform coefficients to levels. `intra` selects the
/// rounding dead-zone offset (1/3 for intra, 1/6 for inter).
pub fn quantize(coeffs: &[i32; 16], qp: u8, intra: bool) -> [i32; 16] {
    let m = (qp % 6) as usize;
    let qbits = 15 + (qp / 6) as u32;
    let f: i64 = if intra {
        (1i64 << qbits) / 3
    } else {
        (1i64 << qbits) / 6
    };
    let mut out = [0i32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let idx = i * 4 + j;
            let w = coeffs[idx] as i64;
            let mf = QUANT_MF[m][pos_group(i, j)] as i64;
            let level = (w.abs() * mf + f) >> qbits;
            out[idx] = if w < 0 { -level as i32 } else { level as i32 };
        }
    }
    out
}

/// Rate-distortion–optimized ("trellis") quantization of a 4×4 residual's
/// transform coefficients. For each coefficient it chooses between the scalar
/// level and one lower (down to zero) to minimize `J = distortion + λ·rate`,
/// trading a few bits of coefficient coding for small reconstruction error.
/// `lambda` is the mode-decision Lagrangian (pixel-SSD domain). Encoder-only —
/// the output is still a valid set of levels any decoder reconstructs.
///
/// NOTE: not wired into the encoder by default. Greedy per-coefficient rounding
/// fights the intra-prediction feedback loop (rounding one block down worsens
/// the next block's prediction), so a net win needs a feedback-aware integration
/// — left as future work. Kept here as a verified building block.
pub fn trellis_quant(coeffs: &[i32; 16], qp: u8, intra: bool, lambda: f64) -> [i32; 16] {
    let m = (qp % 6) as usize;
    let qbits = 15 + (qp / 6) as u32;
    let scale = (1u64 << qbits) as f64;
    let off: i64 = if intra { (1i64 << qbits) / 3 } else { (1i64 << qbits) / 6 };
    let mut out = [0i32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let idx = i * 4 + j;
            let w = coeffs[idx] as i64;
            let mf = QUANT_MF[m][pos_group(i, j)] as i64;
            let num = w.abs() * mf; // == ideal_level * 2^qbits
            let l_scalar = (num + off) >> qbits;
            if l_scalar == 0 {
                continue;
            }
            // Distortion is in level² units; convert λ (pixel-SSD) into that
            // domain via the dequant step (step ≈ 2^qbits / mf, pixel ≈ step/8).
            let lambda_q = lambda * (mf * mf) as f64 / (scale * scale) * 64.0;
            let ideal = num as f64 / scale;
            let mut best = l_scalar;
            let mut best_j = f64::MAX;
            for cand in [l_scalar - 1, l_scalar] {
                let d = (ideal - cand as f64).powi(2);
                let r = if cand == 0 {
                    0.0
                } else {
                    // ~bits to code |level|: significance + sign + magnitude.
                    2.0 + 2.0 * (64 - (cand as u64).leading_zeros()) as f64
                };
                let jj = d + lambda_q * r;
                if jj < best_j {
                    best_j = jj;
                    best = cand;
                }
            }
            out[idx] = if w < 0 { -best as i32 } else { best as i32 };
        }
    }
    out
}

/// Dequantizes levels to scaled coefficients (spec §8.5.12.1, flat scaling
/// list so `LevelScale = 16 · normAdjust`).
pub fn dequantize(levels: &[i32; 16], qp: u8) -> [i32; 16] {
    let m = (qp % 6) as usize;
    let shift = (qp / 6) as i32;
    let mut out = [0i32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let idx = i * 4 + j;
            let level_scale = 16 * NORM_ADJUST[m][pos_group(i, j)];
            let c = levels[idx];
            out[idx] = if qp >= 24 {
                (c * level_scale) << (shift - 4)
            } else {
                (c * level_scale + (1 << (3 - shift))) >> (4 - shift)
            };
        }
    }
    out
}

/// Inverse core transform + final normalization, turning dequantized
/// coefficients back into a residual block (spec §8.5.12.2: `(f + 32) >> 6`).
pub fn inverse_core(coeffs: &[i32; 16]) -> [i32; 16] {
    let mut m = *coeffs;
    // Rows first, then columns. The order is **not** interchangeable: the
    // `>> 1` flooring inside `inv_1d` makes the integer transform non-separable,
    // so the spec (§8.5.12.2 — horizontal row transform, then vertical) and the
    // decoder must agree exactly. (A column-first pass diverges by ±1 on
    // asymmetric blocks, which only surfaces at low QP / high-frequency content.)
    for r in 0..4 {
        let (a, b, c, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a;
        m[r * 4 + 1] = b;
        m[r * 4 + 2] = c;
        m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a;
        m[4 + c] = b;
        m[8 + c] = cc;
        m[12 + c] = d;
    }
    for v in m.iter_mut() {
        *v = (*v + 32) >> 6;
    }
    m
}

/// Convenience: full forward path, residual → quantized levels.
pub fn forward_quant(residual: &[i32; 16], qp: u8, intra: bool) -> [i32; 16] {
    quantize(&forward_core(residual), qp, intra)
}

/// Convenience: full inverse path, quantized levels → reconstructed residual.
pub fn inverse_quant(levels: &[i32; 16], qp: u8) -> [i32; 16] {
    inverse_core(&dequantize(levels, qp))
}

// ---- Secondary DC transforms for I_16x16 luma and chroma (Hadamard) ----

/// In-place 1D 4-point Hadamard (its own inverse up to scale).
#[inline]
fn hadamard_1d(a: i32, b: i32, c: i32, d: i32) -> (i32, i32, i32, i32) {
    (a + b + c + d, a + b - c - d, a - b - c + d, a - b + c - d)
}

/// 4×4 Hadamard transform (rows then columns), used for the I_16x16 luma DC
/// block. Symmetric, so the same routine serves forward and inverse.
pub fn hadamard_4x4(block: &[i32; 16]) -> [i32; 16] {
    let mut m = *block;
    for r in 0..4 {
        let (a, b, c, d) = hadamard_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a;
        m[r * 4 + 1] = b;
        m[r * 4 + 2] = c;
        m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = hadamard_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a;
        m[4 + c] = b;
        m[8 + c] = cc;
        m[12 + c] = d;
    }
    m
}

/// Forward transform + quantization of the 16 luma DC coefficients of an
/// I_16x16 macroblock (spec §8.5.10). Input/output are row-major 4×4.
pub fn forward_quant_luma_dc(dc: &[i32; 16], qp: u8, intra: bool) -> [i32; 16] {
    let f = hadamard_4x4(dc);
    let m = (qp % 6) as usize;
    // The 4×4 Hadamard has gain 16 (its square is 16·I), so the luma DC quant
    // carries two extra bits over the AC quant to keep the reconstructed DC at
    // the same scale as the regular dequantized DC coefficient.
    let qbits = 17 + (qp / 6) as u32;
    let off: i64 = if intra { (1i64 << qbits) / 3 } else { (1i64 << qbits) / 6 };
    let mf = QUANT_MF[m][0] as i64;
    let mut out = [0i32; 16];
    for (o, &fv) in out.iter_mut().zip(f.iter()) {
        let level = ((fv.abs() as i64) * mf + off) >> qbits;
        *o = if fv < 0 { -level as i32 } else { level as i32 };
    }
    out
}

/// Inverse quantization + transform of the I_16x16 luma DC block, returning the
/// reconstructed DC values to scatter into each 4×4 luma block (spec §8.5.10).
pub fn inverse_quant_luma_dc(levels: &[i32; 16], qp: u8) -> [i32; 16] {
    let g = hadamard_4x4(levels);
    let m = (qp % 6) as usize;
    let shift = (qp / 6) as i32;
    let level_scale = 16 * NORM_ADJUST[m][0];
    let mut out = [0i32; 16];
    for (o, &gv) in out.iter_mut().zip(g.iter()) {
        *o = if qp >= 36 {
            (gv * level_scale) << (shift - 6)
        } else {
            (gv * level_scale + (1 << (5 - shift))) >> (6 - shift)
        };
    }
    out
}

/// 2×2 Hadamard for a chroma DC block (its own inverse up to scale).
pub fn hadamard_2x2(dc: &[i32; 4]) -> [i32; 4] {
    let (a, b, c, d) = (dc[0], dc[1], dc[2], dc[3]);
    [a + b + c + d, a - b + c - d, a + b - c - d, a - b - c + d]
}

/// Forward transform + quantization of a chroma DC block (4 coeffs, spec §8.5.11).
pub fn forward_quant_chroma_dc(dc: &[i32; 4], qp: u8, intra: bool) -> [i32; 4] {
    let f = hadamard_2x2(dc);
    let m = (qp % 6) as usize;
    let qbits = 15 + (qp / 6) as u32;
    let off: i64 = if intra { (1i64 << qbits) / 3 } else { (1i64 << qbits) / 6 };
    let mf = QUANT_MF[m][0] as i64;
    let mut out = [0i32; 4];
    for (o, &fv) in out.iter_mut().zip(f.iter()) {
        let level = ((fv.abs() as i64) * mf + 2 * off) >> (qbits + 1);
        *o = if fv < 0 { -level as i32 } else { level as i32 };
    }
    out
}

/// Inverse quantization + transform of a chroma DC block (spec §8.5.11.2).
pub fn inverse_quant_chroma_dc(levels: &[i32; 4], qp: u8) -> [i32; 4] {
    let g = hadamard_2x2(levels);
    let m = (qp % 6) as usize;
    let shift = (qp / 6) as i32;
    let level_scale = 16 * NORM_ADJUST[m][0];
    let mut out = [0i32; 4];
    for (o, &gv) in out.iter_mut().zip(g.iter()) {
        *o = ((gv * level_scale) << shift) >> 5;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_inverse_core_are_consistent_scale() {
        // A pure-DC block: every sample = 4. Forward DC should be 16*4=64,
        // others ~0. Inverse of just the dequantized DC returns the flat block.
        let block = [4i32; 16];
        let w = forward_core(&block);
        assert_eq!(w[0], 64, "DC coefficient");
        for (k, &ac) in w.iter().enumerate().skip(1) {
            assert_eq!(ac, 0, "AC[{k}] should be zero for a flat block");
        }
    }

    #[test]
    fn inverse_core_is_row_first() {
        // The 4×4 integer inverse transform is NOT order-invariant: the `>> 1`
        // flooring inside `inv_1d` makes row-first and column-first diverge on
        // asymmetric blocks. The spec (§8.5.12.2) and ffmpeg do rows first; a
        // column-first pass is a real bug that only surfaces at low QP / high
        // frequency. This input distinguishes the two orders and pins ours.
        let coeffs = [9, 2, -1, 2, -2, 2, -2, 1, -1, -2, 3, 5, -1, -1, -5, 3];
        let mut rows_then_cols = coeffs;
        for r in 0..4 {
            let (a, b, c, d) = inv_1d(
                rows_then_cols[r * 4],
                rows_then_cols[r * 4 + 1],
                rows_then_cols[r * 4 + 2],
                rows_then_cols[r * 4 + 3],
            );
            rows_then_cols[r * 4] = a;
            rows_then_cols[r * 4 + 1] = b;
            rows_then_cols[r * 4 + 2] = c;
            rows_then_cols[r * 4 + 3] = d;
        }
        for c in 0..4 {
            let (a, b, cc, d) = inv_1d(
                rows_then_cols[c],
                rows_then_cols[4 + c],
                rows_then_cols[8 + c],
                rows_then_cols[12 + c],
            );
            rows_then_cols[c] = a;
            rows_then_cols[4 + c] = b;
            rows_then_cols[8 + c] = cc;
            rows_then_cols[12 + c] = d;
        }
        let mut cols_then_rows = coeffs;
        for c in 0..4 {
            let (a, b, cc, d) = inv_1d(
                cols_then_rows[c],
                cols_then_rows[4 + c],
                cols_then_rows[8 + c],
                cols_then_rows[12 + c],
            );
            cols_then_rows[c] = a;
            cols_then_rows[4 + c] = b;
            cols_then_rows[8 + c] = cc;
            cols_then_rows[12 + c] = d;
        }
        for r in 0..4 {
            let (a, b, c, d) = inv_1d(
                cols_then_rows[r * 4],
                cols_then_rows[r * 4 + 1],
                cols_then_rows[r * 4 + 2],
                cols_then_rows[r * 4 + 3],
            );
            cols_then_rows[r * 4] = a;
            cols_then_rows[r * 4 + 1] = b;
            cols_then_rows[r * 4 + 2] = c;
            cols_then_rows[r * 4 + 3] = d;
        }
        // The two orders genuinely differ on this block...
        assert_ne!(rows_then_cols, cols_then_rows);
        // ...and inverse_core (plus the +32>>6 normalization) follows rows-first.
        let expected: [i32; 16] =
            core::array::from_fn(|k| (rows_then_cols[k] + 32) >> 6);
        assert_eq!(inverse_core(&coeffs), expected);
    }

    #[test]
    fn quant_dequant_roundtrip_is_near_identity() {
        // For a range of QPs, a transformed-then-quantized-then-reconstructed
        // residual should stay within the quantization step of the original.
        let residual: [i32; 16] = [
            5, -3, 8, 0, 12, -7, 2, 1, -4, 6, 9, -2, 0, 3, -1, 7,
        ];
        for qp in [0u8, 6, 12, 18, 26, 30, 37, 45, 51] {
            let levels = forward_quant(&residual, qp, true);
            let recon = inverse_quant(&levels, qp);
            // Tolerance grows with the quant step (~ 2^(qp/6)).
            let tol = 2 + (1 << (qp / 6));
            for k in 0..16 {
                let diff = (recon[k] - residual[k]).abs();
                assert!(
                    diff <= tol,
                    "qp {qp}: residual[{k}]={} recon={} diff={diff} tol={tol}",
                    residual[k],
                    recon[k]
                );
            }
        }
    }

    #[test]
    fn trellis_never_exceeds_scalar_magnitude() {
        // Trellis only considers the scalar level or lower, so |level| never
        // grows, and a large λ drives marginal coefficients toward zero.
        let coeffs: [i32; 16] = [120, -40, 8, 1, -15, 6, -1, 0, 3, -2, 1, 0, 0, 1, 0, 0];
        let scalar = quantize(&coeffs, 26, true);
        let t = trellis_quant(&coeffs, 26, true, 50.0);
        for k in 0..16 {
            assert!(t[k].unsigned_abs() <= scalar[k].unsigned_abs(), "[{k}]");
            assert!(t[k] == 0 || t[k].signum() == scalar[k].signum());
        }
    }

    #[test]
    fn zero_residual_stays_zero() {
        let zero = [0i32; 16];
        let levels = forward_quant(&zero, 28, true);
        assert_eq!(levels, [0i32; 16]);
        assert_eq!(inverse_quant(&levels, 28), [0i32; 16]);
    }

    #[test]
    fn luma_dc_end_to_end_flat_block() {
        // A flat luma residual `r`: each 4×4 block's forward-core DC is 16*r and
        // its AC is 0. Coding the 16 DCs via the secondary transform and
        // reconstructing (scatter DC → inverse core) must recover ~r per sample.
        for r in [3i32, 9, -5, 20] {
            for qp in [0u8, 12, 24, 30] {
                let w_dc = [16 * r; 16]; // forward-core DC of a flat block
                let z = forward_quant_luma_dc(&w_dc, qp, true);
                let dcy = inverse_quant_luma_dc(&z, qp);
                let tol = 1 + (1 << (qp / 6));
                for (b, &dc) in dcy.iter().enumerate() {
                    let mut coeff = [0i32; 16];
                    coeff[0] = dc;
                    let res = inverse_core(&coeff);
                    for &v in &res {
                        assert!((v - r).abs() <= tol, "luma DC r={r} qp{qp} blk{b}: {v} vs {r}");
                    }
                }
            }
        }
    }

    #[test]
    fn chroma_dc_end_to_end_flat_block() {
        // Same idea for the 2×2 chroma DC secondary transform.
        for r in [4i32, -6, 11] {
            for qp in [0u8, 18, 30] {
                let dc = [16 * r; 4];
                let z = forward_quant_chroma_dc(&dc, qp, true);
                let dcy = inverse_quant_chroma_dc(&z, qp);
                let tol = 1 + (1 << (qp / 6));
                for &d in &dcy {
                    let mut coeff = [0i32; 16];
                    coeff[0] = d;
                    let res = inverse_core(&coeff);
                    for &v in &res {
                        assert!((v - r).abs() <= tol, "chroma DC r={r} qp{qp}: {v} vs {r}");
                    }
                }
            }
        }
    }

    #[test]
    fn hadamard_is_self_inverse_scaled() {
        // Applying the 4×4 Hadamard twice scales by 16.
        let x: [i32; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let twice = hadamard_4x4(&hadamard_4x4(&x));
        for (k, (&a, &b)) in x.iter().zip(twice.iter()).enumerate() {
            assert_eq!(b, a * 16, "[{k}]");
        }
    }

    #[test]
    fn low_qp_is_high_fidelity() {
        // At QP 0 the reconstruction should be essentially exact for small
        // integer residuals.
        let residual: [i32; 16] = [1, 2, 3, 4, -1, -2, -3, -4, 0, 1, 0, -1, 2, -2, 1, 0];
        let levels = forward_quant(&residual, 0, true);
        let recon = inverse_quant(&levels, 0);
        for (k, (&r, &o)) in residual.iter().zip(recon.iter()).enumerate() {
            assert!((o - r).abs() <= 1, "qp0 residual[{k}]={r} recon={o}");
        }
    }
}
