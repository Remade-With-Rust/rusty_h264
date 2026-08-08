//! **SATD / SAD** block-difference metrics — portable SIMD (rip-ASM Phase 5a).
//!
//! Replaces openh264's `WelsSampleSatd{4x4,8x8,8x16,16x8,16x16}` and
//! `WelsSampleSad{16x16,16x8,8x16}` (satd_sad.asm, 2,734 LOC — the largest remaining
//! assembly file). These are ENCODER kernels: motion estimation is ~81% of the
//! encoder's speed gap to x264, and it is these two metrics it spends that time in.
//!
//! ## Composed, not written from scratch
//!
//! `satd_avg::hadamard4_abs_acc` already implements the 4x4 Hadamard + Σ|coeff| over a
//! 4-row x 16-col band (four blocks at once) in AVX2, and is already pinned bit-exact.
//! This module reuses it and only supplies a two-operand row-differencer, which is the
//! campaign's own "compose, don't write kernels" result applied again.
//!
//! ## The one subtlety: rounding is PER 4x4 BLOCK
//!
//! openh264 defines the region SATD as `Σ_blocks ((Σ|H·d| + 1) >> 1)` — the `+1>>1`
//! lands on **each 4x4 block**, not on the region total. So the accumulator cannot be
//! carried across bands and halved once at the end; each band must be finalised. That
//! is why this does not simply call `satd_avg_w16`, which returns an unrounded total
//! for a different consumer and would differ by up to one count per block.
//!
//! Within a band, `hadamard4_abs_acc`'s 8 i32 lanes hold four blocks' partial sums as
//! adjacent pairs — block `b` occupies lanes `2b` and `2b+1` — so one `hadd` collapses
//! them to four per-block totals ready for the rounding shift.
//!
//! ## Fallbacks
//!
//! AVX2 for SATD, SSE2 (baseline, ungated) for SAD, scalar for everything else. A
//! non-AVX2 x86-64 machine now takes the scalar SATD where it used to take openh264's
//! SSE2 kernel; that is a deliberate, stated trade — such hardware predates ~2013 and
//! the correctness gate covers it, but it is slower there.

// ---------------------------------------------------------------------------------
// Scalar references — the oracles, and the fallback.
// ---------------------------------------------------------------------------------

/// One 4x4 SATD: `(Σ|H·d| + 1) >> 1`. Mirrors openh264's `WelsSampleSatd4x4_c`.
fn satd4x4_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
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
        let (s0, s1, s2, s3) =
            (m[0][j] + m[2][j], m[1][j] + m[3][j], m[0][j] - m[2][j], m[1][j] - m[3][j]);
        let (c0, c1, c2, c3) = (s0 + s1, s2 + s3, s2 - s3, s0 - s1);
        sum += c0.abs() + c1.abs() + c2.abs() + c3.abs();
    }
    (sum + 1) >> 1
}

fn satd_region_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
    let mut s = 0;
    let mut by = 0;
    while by < h {
        let mut bx = 0;
        while bx < w {
            s += satd4x4_scalar(&a[by * sa + bx..], sa, &b[by * sb + bx..], sb);
            bx += 4;
        }
        by += 4;
    }
    s
}

fn sad_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
    let mut s = 0i32;
    for i in 0..h {
        for j in 0..w {
            s += (a[i * sa + j] as i32 - b[i * sb + j] as i32).abs();
        }
    }
    s
}

// ---------------------------------------------------------------------------------
// x86-64
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
pub(crate) mod x86 {
    use std::arch::x86_64::*;

    /// 16 columns of `a - b` as 16 i16 lanes (four 4x4 blocks side by side).
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn diff16(a: *const u8, b: *const u8) -> __m256i {
        let va = _mm256_cvtepu8_epi16(_mm_loadu_si128(a as *const __m128i));
        let vb = _mm256_cvtepu8_epi16(_mm_loadu_si128(b as *const __m128i));
        _mm256_sub_epi16(va, vb)
    }

    /// 8 columns of two rows (`r` and `r+4`) packed into the two 128-bit lanes, so an
    /// 8-wide block still presents four 4x4 blocks to the band kernel.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn diff8x2(a: *const u8, b: *const u8, sa: usize, sb: usize) -> __m256i {
        let lo = _mm_unpacklo_epi64(
            _mm_loadl_epi64(a as *const __m128i),
            _mm_loadl_epi64(a.add(4 * sa) as *const __m128i));
        let hi = _mm_unpacklo_epi64(
            _mm_loadl_epi64(b as *const __m128i),
            _mm_loadl_epi64(b.add(4 * sb) as *const __m128i));
        _mm256_sub_epi16(_mm256_cvtepu8_epi16(lo), _mm256_cvtepu8_epi16(hi))
    }

    /// Collapse one band's accumulator to `Σ_blocks ((blocksum + 1) >> 1)`.
    ///
    /// Lanes are `[b0,b0, b1,b1 | b2,b2, b3,b3]`; `hadd` pairs them into the four block
    /// totals. Rounding MUST happen here, per block — see the module docs.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn finish_band(acc: __m256i) -> i32 {
        let pair = _mm256_hadd_epi32(acc, acc); // [b0,b1,b0,b1 | b2,b3,b2,b3]
        let r = _mm256_srai_epi32::<1>(_mm256_add_epi32(pair, _mm256_set1_epi32(1)));
        _mm256_extract_epi32::<0>(r) + _mm256_extract_epi32::<1>(r)
            + _mm256_extract_epi32::<4>(r) + _mm256_extract_epi32::<5>(r)
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn satd_w16(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut total = 0;
        let mut r = 0;
        while r < h {
            let acc = crate::x86_asm::satd_avg::hadamard4_abs_acc(
                diff16(a.add(r * sa), b.add(r * sb)),
                diff16(a.add((r + 1) * sa), b.add((r + 1) * sb)),
                diff16(a.add((r + 2) * sa), b.add((r + 2) * sb)),
                diff16(a.add((r + 3) * sa), b.add((r + 3) * sb)),
                _mm256_setzero_si256(),
            );
            total += finish_band(acc);
            r += 4;
        }
        total
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn satd_w8(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut total = 0;
        let mut r = 0;
        while r < h {
            let acc = crate::x86_asm::satd_avg::hadamard4_abs_acc(
                diff8x2(a.add(r * sa), b.add(r * sb), sa, sb),
                diff8x2(a.add((r + 1) * sa), b.add((r + 1) * sb), sa, sb),
                diff8x2(a.add((r + 2) * sa), b.add((r + 2) * sb), sa, sb),
                diff8x2(a.add((r + 3) * sa), b.add((r + 3) * sb), sa, sb),
                _mm256_setzero_si256(),
            );
            total += finish_band(acc);
            r += 8;
        }
        total
    }

    /// SAD. SSE2 is x86-64 baseline, so this is ungated and inlinable — `_mm_sad_epu8`
    /// produces two 16-bit-wide sums per 128-bit vector directly.
    #[inline]
    pub unsafe fn sad(a: *const u8, sa: usize, b: *const u8, sb: usize, w: usize, h: usize) -> i32 {
        let mut acc = _mm_setzero_si128();
        for r in 0..h {
            let (pa, pb) = (a.add(r * sa), b.add(r * sb));
            if w == 16 {
                acc = _mm_add_epi32(acc, _mm_sad_epu8(
                    _mm_loadu_si128(pa as *const __m128i), _mm_loadu_si128(pb as *const __m128i)));
            } else {
                acc = _mm_add_epi32(acc, _mm_sad_epu8(
                    _mm_loadl_epi64(pa as *const __m128i), _mm_loadl_epi64(pb as *const __m128i)));
            }
        }
        // lanes 0 and 2 hold the two halves' sums
        _mm_cvtsi128_si32(acc) + _mm_extract_epi16::<4>(acc)
    }
}

// ---------------------------------------------------------------------------------
// aarch64 NEON — SAD only (`vabdl` + pairwise widening add). SATD stays scalar here
// until the Hadamard band kernel gets a NEON twin.
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;
    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn sad(a: *const u8, sa: usize, b: *const u8, sb: usize, w: usize, h: usize) -> i32 {
        let mut acc = vdupq_n_u32(0);
        for r in 0..h {
            let (pa, pb) = (a.add(r * sa), b.add(r * sb));
            if w == 16 {
                let d = vabdq_u8(vld1q_u8(pa), vld1q_u8(pb));
                acc = vpadalq_u16(acc, vpaddlq_u8(d));
            } else {
                let d = vabd_u8(vld1_u8(pa), vld1_u8(pb));
                acc = vpadalq_u16(acc, vcombine_u16(vpaddl_u8(d), vdup_n_u16(0)));
            }
        }
        vaddvq_u32(acc) as i32
    }
}

// ---------------------------------------------------------------------------------
// Safe API — same signatures the assembly wrappers exposed.
// ---------------------------------------------------------------------------------

macro_rules! satd_fn {
    ($name:ident, $w:expr, $h:expr, $simd:ident) => {
        #[doc = concat!("SATD of two ", stringify!($w), "x", stringify!($h),
                        " blocks: `Σ_4x4 ((Σ|H·d| + 1) >> 1)`.")]
        pub fn $name(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
            assert!(a.len() >= ($h - 1) * sa + $w && b.len() >= ($h - 1) * sb + $w);
            #[cfg(target_arch = "x86_64")]
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: bounds asserted above; AVX2 checked here.
                return unsafe { x86::$simd(a.as_ptr(), sa, b.as_ptr(), sb, $h) };
            }
            satd_region_scalar(a, sa, b, sb, $w, $h)
        }
    };
}

satd_fn!(satd_16x16, 16, 16, satd_w16);
satd_fn!(satd_16x8, 16, 8, satd_w16);
satd_fn!(satd_8x16, 8, 16, satd_w8);
satd_fn!(satd_8x8, 8, 8, satd_w8);

/// SATD of two 4x4 blocks. Too small for the band kernel (which does four at once).
pub fn satd_4x4(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
    assert!(a.len() >= 3 * sa + 4 && b.len() >= 3 * sb + 4);
    satd4x4_scalar(a, sa, b, sb)
}

macro_rules! sad_fn {
    ($name:ident, $w:expr, $h:expr) => {
        #[doc = concat!("SAD of two ", stringify!($w), "x", stringify!($h), " blocks.")]
        pub fn $name(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
            assert!(a.len() >= ($h - 1) * sa + $w && b.len() >= ($h - 1) * sb + $w);
            #[cfg(target_arch = "x86_64")]
            // SAFETY: bounds asserted; SSE2 is x86-64 baseline.
            return unsafe { x86::sad(a.as_ptr(), sa, b.as_ptr(), sb, $w, $h) };
            #[cfg(target_arch = "aarch64")]
            if std::arch::is_aarch64_feature_detected!("neon") {
                // SAFETY: bounds asserted; NEON checked here.
                return unsafe { arm::sad(a.as_ptr(), sa, b.as_ptr(), sb, $w, $h) };
            }
            #[allow(unreachable_code)]
            sad_scalar(a, sa, b, sb, $w, $h)
        }
    };
}

sad_fn!(sad_16x16, 16, 16);
sad_fn!(sad_16x8, 16, 8);
sad_fn!(sad_8x16, 8, 16);

/// `extern "C"` shims matching openh264's `WelsSampleSatd*` signature, so `MeCtx`'s
/// function-pointer table can point at the portable kernels unchanged. Callers must
/// have verified AVX2 (MeCtx::new does) — these go straight to the AVX2 path.
#[cfg(target_arch = "x86_64")]
pub(crate) mod cshim {
    macro_rules! shim {
        ($name:ident, $inner:ident, $h:expr) => {
            /// # Safety
            /// AVX2 must be available and both operands must cover `$h` rows at their
            /// strides. Matches `WelsSampleSatd*`'s contract exactly.
            pub(crate) unsafe extern "C" fn $name(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32 {
                super::x86::$inner(p1, s1 as usize, p2, s2 as usize, $h)
            }
        };
    }
    shim!(satd16x16, satd_w16, 16);
    shim!(satd16x8, satd_w16, 8);
    shim!(satd8x16, satd_w8, 16);
    shim!(satd8x8, satd_w8, 8);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(seed: usize) -> (Vec<u8>, Vec<u8>) {
        let (mut a, mut b) = (vec![0u8; 16 * 16], vec![0u8; 16 * 16]);
        for i in 0..16 {
            for j in 0..16 {
                a[i * 16 + j] = ((i * 37 + j * 101 + seed * 3) & 0xff) as u8;
                b[i * 16 + j] = ((i * 53 + j * 17 + seed * 29 + 7) & 0xff) as u8;
            }
        }
        (a, b)
    }

    #[test]
    fn satd_family_matches_scalar() {
        for seed in 0..96 {
            let (a, b) = corpus(seed);
            assert_eq!(satd_16x16(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 16, 16), "16x16 {seed}");
            assert_eq!(satd_16x8(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 16, 8), "16x8 {seed}");
            assert_eq!(satd_8x16(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 8, 16), "8x16 {seed}");
            assert_eq!(satd_8x8(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 8, 8), "8x8 {seed}");
            assert_eq!(satd_4x4(&a, 16, &b, 16), satd4x4_scalar(&a, 16, &b, 16), "4x4 {seed}");
        }
    }

    #[test]
    fn sad_family_matches_scalar() {
        for seed in 0..96 {
            let (a, b) = corpus(seed);
            assert_eq!(sad_16x16(&a, 16, &b, 16), sad_scalar(&a, 16, &b, 16, 16, 16), "16x16 {seed}");
            assert_eq!(sad_16x8(&a, 16, &b, 16), sad_scalar(&a, 16, &b, 16, 16, 8), "16x8 {seed}");
            assert_eq!(sad_8x16(&a, 16, &b, 16), sad_scalar(&a, 16, &b, 16, 8, 16), "8x16 {seed}");
        }
    }

    /// Extremes: the Hadamard can reach +/-4080 per coefficient, and SAD its maximum.
    #[test]
    fn extremes_match_scalar() {
        for (va, vb) in [(0u8, 255u8), (255, 0), (0, 0), (255, 255)] {
            let (a, b) = (vec![va; 16 * 16], vec![vb; 16 * 16]);
            assert_eq!(satd_16x16(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 16, 16));
            assert_eq!(sad_16x16(&a, 16, &b, 16), sad_scalar(&a, 16, &b, 16, 16, 16));
        }
        // alternating rows/cols: worst case for the vertical/horizontal butterflies
        let mut a = vec![0u8; 16 * 16];
        let mut b = vec![0u8; 16 * 16];
        for i in 0..16 {
            for j in 0..16 {
                a[i * 16 + j] = if (i + j) % 2 == 0 { 255 } else { 0 };
                b[i * 16 + j] = if i % 2 == 0 { 0 } else { 255 };
            }
        }
        assert_eq!(satd_16x16(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 16, 16));
        assert_eq!(satd_8x8(&a, 16, &b, 16), satd_region_scalar(&a, 16, &b, 16, 8, 8));
    }
}
