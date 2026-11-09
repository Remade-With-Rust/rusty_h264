//! **Per-block 8x8 inverse transform + residual add** for the DECODER (i32 exact):
//! the SIMD twin of `common::transform::inverse_core_8x8` + add + clip, the High
//! profile 8x8 residual.
//!
//! Before this kernel the 8x8 arm ran four scalar passes: the 1-D butterflies
//! (324 instructions a block), a 64-sample gather of the prediction into an
//! `[i32; 64]`, an add-and-clip into an `[u8; 64]`, and eight row copies into the
//! plane. Here the block stays in eight 256-bit registers (one row of eight i32
//! each): an in-register 8x8 transpose puts rows in lanes for the row pass, a
//! second puts columns in lanes for the column pass, and each output row is
//! rounded, narrowed, added to its eight prediction bytes and stored, all in
//! registers. Bit-identical to the scalar form on every input: i32 add/sub wrap
//! in both (the shifts see the same wrapped values whatever the order of the
//! sums), and the saturating chain `packs_epi32 -> adds_epi16 -> packus` lands on
//! exactly `clamp(pred + res, 0, 255)` (the argument made in `idct4x4`).
//!
//! AVX2 is the x86-64-v3 baseline this workspace builds for. Other targets (and an
//! x86-64 build without AVX2) take the scalar twin, which is what the decoder ran
//! before the kernel existed. A NEON twin is not written yet.

/// One-dimensional inverse 8x8 transform (spec 8.5.13.2), wrapping i32: the
/// release semantics of `common::transform::inv_1d_8x8`.
#[inline]
fn inv_1d(d: [i32; 8]) -> [i32; 8] {
    let a0 = d[0].wrapping_add(d[4]);
    let a4 = d[0].wrapping_sub(d[4]);
    let a2 = (d[2] >> 1).wrapping_sub(d[6]);
    let a6 = d[2].wrapping_add(d[6] >> 1);
    let b0 = a0.wrapping_add(a6);
    let b2 = a4.wrapping_add(a2);
    let b4 = a4.wrapping_sub(a2);
    let b6 = a0.wrapping_sub(a6);
    let a1 = d[5]
        .wrapping_sub(d[3])
        .wrapping_sub(d[7])
        .wrapping_sub(d[7] >> 1);
    let a3 = d[1]
        .wrapping_add(d[7])
        .wrapping_sub(d[3])
        .wrapping_sub(d[3] >> 1);
    let a5 = d[7]
        .wrapping_sub(d[1])
        .wrapping_add(d[5])
        .wrapping_add(d[5] >> 1);
    let a7 = d[3]
        .wrapping_add(d[5])
        .wrapping_add(d[1])
        .wrapping_add(d[1] >> 1);
    let b1 = a1.wrapping_add(a7 >> 2);
    let b7 = a7.wrapping_sub(a1 >> 2);
    let b3 = a3.wrapping_add(a5 >> 2);
    let b5 = (a3 >> 2).wrapping_sub(a5);
    [
        b0.wrapping_add(b7),
        b2.wrapping_add(b5),
        b4.wrapping_add(b3),
        b6.wrapping_add(b1),
        b6.wrapping_sub(b1),
        b4.wrapping_sub(b3),
        b2.wrapping_sub(b5),
        b0.wrapping_sub(b7),
    ]
}

/// Scalar oracle: rows then columns, `(x + 32) >> 6`, add + clip, row-major 8x8
/// into strided planes.
pub fn idct8x8_add_scalar(
    coeffs: &[i32; 64],
    pred: &[u8],
    p_off: usize,
    p_stride: usize,
    rec: &mut [u8],
    r_off: usize,
    r_stride: usize,
) {
    let mut m = *coeffs;
    for r in 0..8 {
        let o = inv_1d(core::array::from_fn(|k| m[r * 8 + k]));
        m[r * 8..r * 8 + 8].copy_from_slice(&o);
    }
    for c in 0..8 {
        let o = inv_1d(core::array::from_fn(|k| m[k * 8 + c]));
        for k in 0..8 {
            m[k * 8 + c] = o[k];
        }
    }
    for r in 0..8 {
        let src = &pred[p_off + r * p_stride..][..8];
        let dst = &mut rec[r_off + r * r_stride..][..8];
        for c in 0..8 {
            let res = m[r * 8 + c].wrapping_add(32) >> 6;
            dst[c] = (src[c] as i32).wrapping_add(res).clamp(0, 255) as u8;
        }
    }
}

/// `rec[8x8] = clip(pred[8x8] + idct8(coeffs))`. Panics (like slice indexing) if an
/// 8x8 window at the given offsets/strides does not fit either plane.
#[inline]
pub fn idct8x8_add(
    coeffs: &[i32; 64],
    pred: &[u8],
    p_off: usize,
    p_stride: usize,
    rec: &mut [u8],
    r_off: usize,
    r_stride: usize,
) {
    assert!(p_off + 7 * p_stride + 8 <= pred.len() && r_off + 7 * r_stride + 8 <= rec.len());
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        crate::census::IDCT8X8_ADD.wide();
        // SAFETY: both 8x8 windows are inside their planes (asserted above); coeffs
        // is a fixed [i32; 64]; AVX2 is enabled for this compilation.
        return unsafe {
            x86::idct8x8_add_avx2(coeffs, pred, p_off, p_stride, rec, r_off, r_stride)
        };
    }
    #[allow(unreachable_code)]
    {
        crate::census::IDCT8X8_ADD.scalar();
        idct8x8_add_scalar(coeffs, pred, p_off, p_stride, rec, r_off, r_stride)
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
mod x86 {
    use core::arch::x86_64::*;

    /// 8x8 i32 transpose: rows in, columns out (8 unpack-32, 8 unpack-64, 8 lane
    /// permutes).
    #[inline(always)]
    unsafe fn transpose8(r: [__m256i; 8]) -> [__m256i; 8] {
        let t0 = _mm256_unpacklo_epi32(r[0], r[1]);
        let t1 = _mm256_unpackhi_epi32(r[0], r[1]);
        let t2 = _mm256_unpacklo_epi32(r[2], r[3]);
        let t3 = _mm256_unpackhi_epi32(r[2], r[3]);
        let t4 = _mm256_unpacklo_epi32(r[4], r[5]);
        let t5 = _mm256_unpackhi_epi32(r[4], r[5]);
        let t6 = _mm256_unpacklo_epi32(r[6], r[7]);
        let t7 = _mm256_unpackhi_epi32(r[6], r[7]);
        let u0 = _mm256_unpacklo_epi64(t0, t2);
        let u1 = _mm256_unpackhi_epi64(t0, t2);
        let u2 = _mm256_unpacklo_epi64(t1, t3);
        let u3 = _mm256_unpackhi_epi64(t1, t3);
        let u4 = _mm256_unpacklo_epi64(t4, t6);
        let u5 = _mm256_unpackhi_epi64(t4, t6);
        let u6 = _mm256_unpacklo_epi64(t5, t7);
        let u7 = _mm256_unpackhi_epi64(t5, t7);
        [
            _mm256_permute2x128_si256::<0x20>(u0, u4),
            _mm256_permute2x128_si256::<0x20>(u1, u5),
            _mm256_permute2x128_si256::<0x20>(u2, u6),
            _mm256_permute2x128_si256::<0x20>(u3, u7),
            _mm256_permute2x128_si256::<0x31>(u0, u4),
            _mm256_permute2x128_si256::<0x31>(u1, u5),
            _mm256_permute2x128_si256::<0x31>(u2, u6),
            _mm256_permute2x128_si256::<0x31>(u3, u7),
        ]
    }

    /// Lane-wise `inv_1d` over eight vectors (wrapping i32, like the oracle).
    #[inline(always)]
    unsafe fn inv1d(d: [__m256i; 8]) -> [__m256i; 8] {
        let add = |a, b| _mm256_add_epi32(a, b);
        let sub = |a, b| _mm256_sub_epi32(a, b);
        let h = |a| _mm256_srai_epi32::<1>(a);
        let q = |a| _mm256_srai_epi32::<2>(a);
        let a0 = add(d[0], d[4]);
        let a4 = sub(d[0], d[4]);
        let a2 = sub(h(d[2]), d[6]);
        let a6 = add(d[2], h(d[6]));
        let b0 = add(a0, a6);
        let b2 = add(a4, a2);
        let b4 = sub(a4, a2);
        let b6 = sub(a0, a6);
        let a1 = sub(sub(d[5], d[3]), add(d[7], h(d[7])));
        let a3 = sub(add(d[1], d[7]), add(d[3], h(d[3])));
        let a5 = add(sub(d[7], d[1]), add(d[5], h(d[5])));
        let a7 = add(add(d[3], d[5]), add(d[1], h(d[1])));
        let b1 = add(a1, q(a7));
        let b7 = sub(a7, q(a1));
        let b3 = add(a3, q(a5));
        let b5 = sub(q(a3), a5);
        [
            add(b0, b7),
            add(b2, b5),
            add(b4, b3),
            add(b6, b1),
            sub(b6, b1),
            sub(b4, b3),
            sub(b2, b5),
            sub(b0, b7),
        ]
    }

    #[inline(always)]
    pub(super) unsafe fn idct8x8_add_avx2(
        coeffs: &[i32; 64],
        pred: &[u8],
        p_off: usize,
        p_stride: usize,
        rec: &mut [u8],
        r_off: usize,
        r_stride: usize,
    ) {
        let c = coeffs.as_ptr() as *const __m256i;
        let rows: [__m256i; 8] = core::array::from_fn(|i| _mm256_loadu_si256(c.add(i)));
        // Row pass runs ALONG each row = across lanes: transpose so lanes index rows.
        let a = inv1d(transpose8(rows));
        // Column pass: transpose back so lanes index columns; o[k] is output row k.
        let o = inv1d(transpose8(a));
        let k32 = _mm256_set1_epi32(32);
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        for (r, v) in o.iter().enumerate() {
            let res = _mm256_srai_epi32::<6>(_mm256_add_epi32(*v, k32));
            let r16 = _mm_packs_epi32(
                _mm256_castsi256_si128(res),
                _mm256_extracti128_si256::<1>(res),
            );
            let p16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(pp.add(r * p_stride) as *const __m128i));
            let s = _mm_adds_epi16(r16, p16);
            _mm_storel_epi64(rp.add(r * r_stride) as *mut __m128i, _mm_packus_epi16(s, s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        *s
    }

    #[test]
    fn idct8x8_add_matches_scalar() {
        let mut s = 7u32;
        // Magnitudes from typical residuals up to values that saturate the i16
        // narrowing and the final clamp, plus extremes that wrap the i32 sums.
        for &mag in &[0i32, 1, 8, 64, 512, 4096, 1 << 15, 1 << 20, i32::MAX] {
            for _ in 0..200 {
                let coeffs: [i32; 64] = core::array::from_fn(|_| {
                    let v = lcg(&mut s) as i32;
                    if mag == i32::MAX {
                        v
                    } else if mag == 0 {
                        0
                    } else {
                        v % (mag + 1)
                    }
                });
                let p_stride = 16 + (lcg(&mut s) % 8) as usize;
                let r_stride = 24 + (lcg(&mut s) % 8) as usize;
                let p_off = (lcg(&mut s) % 5) as usize;
                let r_off = (lcg(&mut s) % 7) as usize;
                let mut pred = vec![0u8; p_off + 8 * p_stride + 8];
                for b in pred.iter_mut() {
                    *b = lcg(&mut s) as u8;
                }
                let mut a = vec![0u8; r_off + 8 * r_stride + 8];
                for b in a.iter_mut() {
                    *b = lcg(&mut s) as u8;
                }
                let mut b = a.clone();
                idct8x8_add(&coeffs, &pred, p_off, p_stride, &mut a, r_off, r_stride);
                idct8x8_add_scalar(&coeffs, &pred, p_off, p_stride, &mut b, r_off, r_stride);
                assert_eq!(a, b, "mag={mag}");
            }
        }
    }
}
