//! **Per-block 4x4 inverse transform + residual add** for the DECODER (i32 exact).
//!
//! The decoder's dequantised coefficients are i32 (a CAVLC level times a scale
//! factor shifted by qp/6 can exceed i16), so the encoder's i16 `idct_four_t4_rec`
//! cannot serve it bit-exactly. This kernel is the SIMD twin of
//! `common::transform::inverse_core` + add + clip: four coefficient rows in
//! registers, an in-register 4x4 transpose (8 shuffles) around each 1-D pass,
//! `(v + 32) >> 6`, then the prediction added in saturating 16-bit and packed to
//! bytes. No scalar gathers, no stack traffic. Bit-identical to the scalar form
//! on every input: the saturating chain `packs_epi32 -> adds_epi16 -> packus`
//! is monotone and lands on exactly `clamp(pred + res, 0, 255)`.
//!
//! Why not the batched `inverse_dct_blocks`: lane-per-block layout needs a
//! 16 x 8 scalar gather AND scatter per eight blocks -- ~99 instructions per
//! block against ~60 for the scalar butterflies, measured as a LOSS on all-intra
//! content (2026-09-05). Per-block with register transposes is ~68.

#[inline]
fn inv_1d(d0: i32, d1: i32, d2: i32, d3: i32) -> (i32, i32, i32, i32) {
    let (e0, e1) = (d0.wrapping_add(d2), d0.wrapping_sub(d2));
    let (e2, e3) = ((d1 >> 1).wrapping_sub(d3), d1.wrapping_add(d3 >> 1));
    (e0.wrapping_add(e3), e1.wrapping_add(e2), e1.wrapping_sub(e2), e0.wrapping_sub(e3))
}

/// Scalar oracle: `inverse_core` + add + clip, row-major 4x4 into strided planes.
pub fn idct4x4_add_scalar(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    let mut m = *coeffs;
    for r in 0..4 {
        let (a, b, c, d) = inv_1d(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
        m[r * 4] = a; m[r * 4 + 1] = b; m[r * 4 + 2] = c; m[r * 4 + 3] = d;
    }
    for c in 0..4 {
        let (a, b, cc, d) = inv_1d(m[c], m[4 + c], m[8 + c], m[12 + c]);
        m[c] = a; m[4 + c] = b; m[8 + c] = cc; m[12 + c] = d;
    }
    for r in 0..4 {
        let src = &pred[p_off + r * p_stride..][..4];
        let dst = &mut rec[r_off + r * r_stride..][..4];
        for c in 0..4 {
            let res = m[r * 4 + c].wrapping_add(32) >> 6;
            dst[c] = (src[c] as i32).wrapping_add(res).clamp(0, 255) as u8;
        }
    }
}

/// Scalar oracle of the flat (DC-only) add.
pub fn flat_add_4x4_scalar(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    for r in 0..4 {
        let src = &pred[p_off + r * p_stride..][..4];
        let dst = &mut rec[r_off + r * r_stride..][..4];
        for c in 0..4 {
            dst[c] = (src[c] as i32).wrapping_add(rval).clamp(0, 255) as u8;
        }
    }
}

/// `rec[4x4] = clip(pred[4x4] + idct(coeffs))`. Panics (like slice indexing) if a
/// 4x4 window at the given offsets/strides does not fit either plane.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn idct4x4_add(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    assert!(p_off + 3 * p_stride + 4 <= pred.len() && r_off + 3 * r_stride + 4 <= rec.len());
    #[cfg(target_arch = "x86_64")]
    // SAFETY: both 4x4 windows are inside their planes (asserted above); coeffs is a fixed [i32; 16].
    return unsafe { x86::idct4x4_add_sse2(coeffs, pred, p_off, p_stride, rec, r_off, r_stride) };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above.
    return unsafe { arm::idct4x4_add_neon(coeffs, pred, p_off, p_stride, rec, r_off, r_stride) };
    #[allow(unreachable_code)]
    idct4x4_add_scalar(coeffs, pred, p_off, p_stride, rec, r_off, r_stride)
}

/// `rec[4x4] = clip(pred[4x4] + rval)` -- the DC-only residual.
#[inline]
pub fn flat_add_4x4(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
    assert!(p_off + 3 * p_stride + 4 <= pred.len() && r_off + 3 * r_stride + 4 <= rec.len());
    #[cfg(target_arch = "x86_64")]
    // SAFETY: windows asserted in bounds.
    return unsafe { x86::flat_add_4x4_sse2(rval, pred, p_off, p_stride, rec, r_off, r_stride) };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above.
    return unsafe { arm::flat_add_4x4_neon(rval, pred, p_off, p_stride, rec, r_off, r_stride) };
    #[allow(unreachable_code)]
    flat_add_4x4_scalar(rval, pred, p_off, p_stride, rec, r_off, r_stride)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    /// 4x4 i32 transpose: rows in, columns out (8 shuffles).
    #[inline(always)]
    unsafe fn transpose4(r0: __m128i, r1: __m128i, r2: __m128i, r3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let t0 = _mm_unpacklo_epi32(r0, r1);
        let t1 = _mm_unpackhi_epi32(r0, r1);
        let t2 = _mm_unpacklo_epi32(r2, r3);
        let t3 = _mm_unpackhi_epi32(r2, r3);
        (
            _mm_unpacklo_epi64(t0, t2),
            _mm_unpackhi_epi64(t0, t2),
            _mm_unpacklo_epi64(t1, t3),
            _mm_unpackhi_epi64(t1, t3),
        )
    }

    /// Lane-wise `inv_1d` over four vectors.
    #[inline(always)]
    unsafe fn inv1d(d0: __m128i, d1: __m128i, d2: __m128i, d3: __m128i) -> (__m128i, __m128i, __m128i, __m128i) {
        let e0 = _mm_add_epi32(d0, d2);
        let e1 = _mm_sub_epi32(d0, d2);
        let e2 = _mm_sub_epi32(_mm_srai_epi32::<1>(d1), d3);
        let e3 = _mm_add_epi32(d1, _mm_srai_epi32::<1>(d3));
        (_mm_add_epi32(e0, e3), _mm_add_epi32(e1, e2), _mm_sub_epi32(e1, e2), _mm_sub_epi32(e0, e3))
    }

    /// Two rows of 16-bit residual (8 lanes: row a then row b) + the two 4-byte
    /// prediction rows, saturating, packed and stored as 2 x 4 bytes.
    #[inline(always)]
    unsafe fn add2rows(res: __m128i, pa: *const u8, pb: *const u8, ra: *mut u8, rb: *mut u8) {
        let zero = _mm_setzero_si128();
        let a = _mm_cvtsi32_si128((pa as *const i32).read_unaligned());
        let b = _mm_cvtsi32_si128((pb as *const i32).read_unaligned());
        let p8 = _mm_unpacklo_epi32(a, b); // 8 bytes: row a, row b
        let p16 = _mm_unpacklo_epi8(p8, zero);
        let s = _mm_adds_epi16(res, p16);
        let o = _mm_packus_epi16(s, s);
        (ra as *mut i32).write_unaligned(_mm_cvtsi128_si32(o));
        (rb as *mut i32).write_unaligned(_mm_cvtsi128_si32(_mm_srli_si128::<4>(o)));
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn idct4x4_add_sse2(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let c = coeffs.as_ptr() as *const __m128i;
        let (r0, r1, r2, r3) = (_mm_loadu_si128(c), _mm_loadu_si128(c.add(1)), _mm_loadu_si128(c.add(2)), _mm_loadu_si128(c.add(3)));
        // Row pass runs ALONG each row = across lanes: transpose so lanes index rows.
        let (c0, c1, c2, c3) = transpose4(r0, r1, r2, r3);
        let (a0, a1, a2, a3) = inv1d(c0, c1, c2, c3);
        // Column pass runs along each column: transpose back, lanes index columns.
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = inv1d(t0, t1, t2, t3);
        let k = _mm_set1_epi32(32);
        let rnd = |v: __m128i| _mm_srai_epi32::<6>(_mm_add_epi32(v, k));
        let r01 = _mm_packs_epi32(rnd(o0), rnd(o1));
        let r23 = _mm_packs_epi32(rnd(o2), rnd(o3));
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        add2rows(r01, pp, pp.add(p_stride), rp, rp.add(r_stride));
        add2rows(r23, pp.add(2 * p_stride), pp.add(3 * p_stride), rp.add(2 * r_stride), rp.add(3 * r_stride));
    }

    #[inline(always)]
    pub(super) unsafe fn flat_add_4x4_sse2(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        // Saturate the flat residual to i16 once; the saturating chain below then
        // lands on clamp(pred + rval, 0, 255) exactly.
        let v = _mm_set1_epi32(rval);
        let res = _mm_packs_epi32(v, v);
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        add2rows(res, pp, pp.add(p_stride), rp, rp.add(r_stride));
        add2rows(res, pp.add(2 * p_stride), pp.add(3 * p_stride), rp.add(2 * r_stride), rp.add(3 * r_stride));
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use core::arch::aarch64::*;

    #[inline(always)]
    unsafe fn transpose4(r0: int32x4_t, r1: int32x4_t, r2: int32x4_t, r3: int32x4_t) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let t01 = vtrnq_s32(r0, r1);
        let t23 = vtrnq_s32(r2, r3);
        (
            vcombine_s32(vget_low_s32(t01.0), vget_low_s32(t23.0)),
            vcombine_s32(vget_low_s32(t01.1), vget_low_s32(t23.1)),
            vcombine_s32(vget_high_s32(t01.0), vget_high_s32(t23.0)),
            vcombine_s32(vget_high_s32(t01.1), vget_high_s32(t23.1)),
        )
    }

    #[inline(always)]
    unsafe fn inv1d(d0: int32x4_t, d1: int32x4_t, d2: int32x4_t, d3: int32x4_t) -> (int32x4_t, int32x4_t, int32x4_t, int32x4_t) {
        let e0 = vaddq_s32(d0, d2);
        let e1 = vsubq_s32(d0, d2);
        let e2 = vsubq_s32(vshrq_n_s32::<1>(d1), d3);
        let e3 = vaddq_s32(d1, vshrq_n_s32::<1>(d3));
        (vaddq_s32(e0, e3), vaddq_s32(e1, e2), vsubq_s32(e1, e2), vsubq_s32(e0, e3))
    }

    /// One row: 4 x i16 residual + 4 prediction bytes, saturating, 4 bytes out.
    #[inline(always)]
    unsafe fn add_row(res: int16x4_t, p: *const u8, r: *mut u8) {
        let pb = vreinterpret_u8_u32(vld1_dup_u32(p as *const u32));
        let p16 = vreinterpret_s16_u16(vget_low_u16(vmovl_u8(pb)));
        let s = vqadd_s16(res, p16);
        let o = vqmovun_s16(vcombine_s16(s, s));
        vst1_lane_u32::<0>(r as *mut u32, vreinterpret_u32_u8(o));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn idct4x4_add_neon(coeffs: &[i32; 16], pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let c = coeffs.as_ptr();
        let (r0, r1, r2, r3) = (vld1q_s32(c), vld1q_s32(c.add(4)), vld1q_s32(c.add(8)), vld1q_s32(c.add(12)));
        let (c0, c1, c2, c3) = transpose4(r0, r1, r2, r3);
        let (a0, a1, a2, a3) = inv1d(c0, c1, c2, c3);
        let (t0, t1, t2, t3) = transpose4(a0, a1, a2, a3);
        let (o0, o1, o2, o3) = inv1d(t0, t1, t2, t3);
        // vrshrq_n_s32::<6> is exactly `(v + 32) >> 6`.
        let rows = [o0, o1, o2, o3];
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        for (k, o) in rows.iter().enumerate() {
            let res = vqmovn_s32(vrshrq_n_s32::<6>(*o));
            add_row(res, pp.add(k * p_stride), rp.add(k * r_stride));
        }
    }

    pub(super) unsafe fn flat_add_4x4_neon(rval: i32, pred: &[u8], p_off: usize, p_stride: usize, rec: &mut [u8], r_off: usize, r_stride: usize) {
        let res = vqmovn_s32(vdupq_n_s32(rval));
        let pp = pred.as_ptr().add(p_off);
        let rp = rec.as_mut_ptr().add(r_off);
        for k in 0..4 {
            add_row(res, pp.add(k * p_stride), rp.add(k * r_stride));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        *s >> 8
    }

    #[test]
    fn idct4x4_add_matches_scalar() {
        let mut seed = 7u32;
        for trial in 0..4000 {
            // Magnitude ladder: small residuals, dequant-sized, then i16-overflowing.
            let mag: i32 = [4, 64, 1024, 40_000, 300_000][trial % 5];
            let mut co = [0i32; 16];
            for c in co.iter_mut() {
                *c = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            }
            let (ps, rs) = (13usize, 17usize);
            let mut pred = vec![0u8; ps * 4 + 8];
            for p in pred.iter_mut() {
                *p = lcg(&mut seed) as u8;
            }
            let mut a = vec![0u8; rs * 4 + 8];
            let mut b = a.clone();
            idct4x4_add_scalar(&co, &pred, 3, ps, &mut a, 5, rs);
            idct4x4_add(&co, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "trial {trial} mag {mag}");
            let rv = (lcg(&mut seed) as i32 % (2 * mag + 1)) - mag;
            flat_add_4x4_scalar(rv, &pred, 3, ps, &mut a, 5, rs);
            flat_add_4x4(rv, &pred, 3, ps, &mut b, 5, rs);
            assert_eq!(a, b, "flat trial {trial}");
        }
    }
}
