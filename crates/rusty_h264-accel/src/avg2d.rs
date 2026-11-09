//! **2-D rounding average** `dst = (a + b + 1) >> 1` over a `w x h` block whose
//! three planes each have their own stride: the decoder's B_Skip zero-motion band
//! (`bz_recon_band_bi`), which averages a whole luma or chroma rectangle of a run
//! of macroblocks straight from the two padded reference planes into the frame.
//!
//! The safe per-row form (`common::inter::avg_row_into`) already vectorises its
//! row to `vpavgb ymm`, but each row paid ~40 instructions of glue around that:
//! three slice bases recomputed with an `imul` each, six bounds checks, and loop
//! state spilled across the call boundary. Here the extents are asserted once per
//! plane and each row is three pointer increments. Bit-identical: `vpavgb` is the
//! exact `(a + b + 1) >> 1` of unsigned bytes, and the scalar tail and the scalar
//! twin compute the same expression.
//!
//! AVX2 is the x86-64-v3 baseline this workspace builds for; every other target
//! takes the scalar twin (the per-row form it replaces). A NEON twin is not
//! written yet.

/// Scalar twin and oracle.
pub fn avg_2d_scalar(
    dst: &mut [u8],
    d_stride: usize,
    a: &[u8],
    a_stride: usize,
    b: &[u8],
    b_stride: usize,
    w: usize,
    h: usize,
) {
    for r in 0..h {
        let (ar, br) = (&a[r * a_stride..][..w], &b[r * b_stride..][..w]);
        let dr = &mut dst[r * d_stride..][..w];
        for (d, (&x, &y)) in dr.iter_mut().zip(ar.iter().zip(br)) {
            *d = ((x as u16 + y as u16 + 1) >> 1) as u8;
        }
    }
}

/// `dst[r * d_stride + c] = (a[r * a_stride + c] + b[r * b_stride + c] + 1) >> 1`
/// for `r < h`, `c < w`. Panics (like slice indexing) if a block does not fit its
/// plane.
#[inline]
pub fn avg_2d(
    dst: &mut [u8],
    d_stride: usize,
    a: &[u8],
    a_stride: usize,
    b: &[u8],
    b_stride: usize,
    w: usize,
    h: usize,
) {
    if w == 0 || h == 0 {
        return;
    }
    assert!(
        (h - 1) * d_stride + w <= dst.len()
            && (h - 1) * a_stride + w <= a.len()
            && (h - 1) * b_stride + w <= b.len()
    );
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        crate::census::AVG_2D.wide();
        // SAFETY: the three w x h blocks are inside their planes (asserted above);
        // AVX2 is enabled for this compilation.
        unsafe {
            x86::avg_2d_avx2(
                dst.as_mut_ptr(),
                d_stride,
                a.as_ptr(),
                a_stride,
                b.as_ptr(),
                b_stride,
                w,
                h,
            )
        };
        return;
    }
    #[allow(unreachable_code)]
    {
        crate::census::AVG_2D.scalar();
        avg_2d_scalar(dst, d_stride, a, a_stride, b, b_stride, w, h)
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
mod x86 {
    use core::arch::x86_64::*;

    /// # Safety
    /// Rows `0..h`, columns `0..w` of all three blocks are in bounds at their strides.
    #[inline(always)]
    pub(super) unsafe fn avg_2d_avx2(
        mut d: *mut u8,
        ds: usize,
        mut a: *const u8,
        as_: usize,
        mut b: *const u8,
        bs: usize,
        w: usize,
        h: usize,
    ) {
        for _ in 0..h {
            let mut x = 0;
            while x + 32 <= w {
                let va = _mm256_loadu_si256(a.add(x) as *const __m256i);
                let vb = _mm256_loadu_si256(b.add(x) as *const __m256i);
                _mm256_storeu_si256(d.add(x) as *mut __m256i, _mm256_avg_epu8(va, vb));
                x += 32;
            }
            if x + 16 <= w {
                let va = _mm_loadu_si128(a.add(x) as *const __m128i);
                let vb = _mm_loadu_si128(b.add(x) as *const __m128i);
                _mm_storeu_si128(d.add(x) as *mut __m128i, _mm_avg_epu8(va, vb));
                x += 16;
            }
            if x + 8 <= w {
                let va = _mm_loadl_epi64(a.add(x) as *const __m128i);
                let vb = _mm_loadl_epi64(b.add(x) as *const __m128i);
                _mm_storel_epi64(d.add(x) as *mut __m128i, _mm_avg_epu8(va, vb));
                x += 8;
            }
            while x < w {
                *d.add(x) = ((*a.add(x) as u16 + *b.add(x) as u16 + 1) >> 1) as u8;
                x += 1;
            }
            d = d.add(ds);
            a = a.add(as_);
            b = b.add(bs);
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
    fn avg_2d_matches_scalar() {
        let mut s = 3u32;
        for w in [
            1usize, 7, 8, 15, 16, 24, 31, 32, 40, 48, 56, 64, 80, 112, 128, 136,
        ] {
            for h in [1usize, 2, 8, 16] {
                let (ds, as_, bs) = (
                    w + (lcg(&mut s) % 9) as usize,
                    w + (lcg(&mut s) % 13) as usize,
                    w + (lcg(&mut s) % 5) as usize,
                );
                let mut a = vec![0u8; (h - 1) * as_ + w + 3];
                let mut b = vec![0u8; (h - 1) * bs + w + 1];
                for v in a.iter_mut().chain(b.iter_mut()) {
                    *v = lcg(&mut s) as u8;
                }
                let mut d1 = vec![0u8; (h - 1) * ds + w + 2];
                for v in d1.iter_mut() {
                    *v = lcg(&mut s) as u8;
                }
                let mut d2 = d1.clone();
                avg_2d(&mut d1, ds, &a, as_, &b, bs, w, h);
                avg_2d_scalar(&mut d2, ds, &a, as_, &b, bs, w, h);
                assert_eq!(d1, d2, "w={w} h={h}");
            }
        }
    }
}
