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
//! ## The one subtlety: rounding is PER 4x4 BLOCK — and it is an exact halving
//!
//! openh264 defines the region SATD as `Σ_blocks ((Σ|H·d| + 1) >> 1)` — the `+1>>1`
//! lands on **each 4x4 block**, not on the region total. But every block's `Σ|H·d|`
//! is EVEN: each of the 16 Hadamard coefficients is a ±1 combination of the 16
//! differences, so all of them share the parity of the block sum, and sixteen terms
//! of one parity sum to an even number. So `(Σ + 1) >> 1 == Σ / 2` for every block,
//! halving distributes over the sum, and the region SATD is `(Σ_all-blocks Σ|H·d|) / 2`
//! exactly. V10 brick 2 (encoder Ir campaign) relies on this: the x86 kernels carry
//! one accumulator across every band and halve once (they used to finalise each band
//! with a horizontal add, a +1, a shift and four extracts). The NEON path still
//! finalises per band, which is equally exact.
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

    /// `Σ_blocks ((Σ_b + 1) >> 1)` from one accumulator holding every block's
    /// `Σ_b` across all bands: each `Σ_b` is even (module docs), so the
    /// per-block rounding is an exact halving of the total. Lanes stay below
    /// 4 x 32,640 for four bands — far inside i32.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn finish(acc: __m256i) -> i32 {
        (crate::x86_asm::satd_avg::hsum_epi32(acc) >> 1) as i32
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn satd_w16(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut acc = _mm256_setzero_si256();
        let mut r = 0;
        while r < h {
            acc = crate::x86_asm::satd_avg::hadamard4_abs_acc(
                diff16(a.add(r * sa), b.add(r * sb)),
                diff16(a.add((r + 1) * sa), b.add((r + 1) * sb)),
                diff16(a.add((r + 2) * sa), b.add((r + 2) * sb)),
                diff16(a.add((r + 3) * sa), b.add((r + 3) * sb)),
                acc,
            );
            r += 4;
        }
        finish(acc)
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn satd_w8(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut acc = _mm256_setzero_si256();
        let mut r = 0;
        while r < h {
            acc = crate::x86_asm::satd_avg::hadamard4_abs_acc(
                diff8x2(a.add(r * sa), b.add(r * sb), sa, sb),
                diff8x2(a.add((r + 1) * sa), b.add((r + 1) * sb), sa, sb),
                diff8x2(a.add((r + 2) * sa), b.add((r + 2) * sb), sa, sb),
                diff8x2(a.add((r + 3) * sa), b.add((r + 3) * sb), sa, sb),
                acc,
            );
            r += 8;
        }
        finish(acc)
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

    /// Fused `SAD(s, (a + b + 1) >> 1)` for `w` 16 or 8 (V6 brick 3): `pavgb` IS
    /// the rounding average, so the quarter-pel prediction is never materialized.
    /// For `w` 8 the upper eight lanes load as zero on all three operands and
    /// contribute `|0 - avg(0, 0)| = 0`. Per-half sums stay below 2^16 for `h` <= 16.
    #[inline]
    pub unsafe fn sad_avg(
        s: *const u8, ss: usize, a: *const u8, b: *const u8, rs: usize, w: usize, h: usize,
    ) -> i32 {
        let mut acc = _mm_setzero_si128();
        for r in 0..h {
            let (ps, pa, pb) = (s.add(r * ss), a.add(r * rs), b.add(r * rs));
            let (vs, va, vb) = if w == 16 {
                (_mm_loadu_si128(ps as *const __m128i), _mm_loadu_si128(pa as *const __m128i),
                 _mm_loadu_si128(pb as *const __m128i))
            } else {
                (_mm_loadl_epi64(ps as *const __m128i), _mm_loadl_epi64(pa as *const __m128i),
                 _mm_loadl_epi64(pb as *const __m128i))
            };
            acc = _mm_add_epi32(acc, _mm_sad_epu8(vs, _mm_avg_epu8(va, vb)));
        }
        _mm_cvtsi128_si32(acc) + _mm_extract_epi16::<4>(acc)
    }

    /// One row of a full-pel SATD search grid (V7 brick 3): `out[i]` is the SATD of
    /// the `w`x`h` source block against the reference block at `b0 + i * step`,
    /// computed by the same `satd_w16`/`satd_w8` band kernels the per-shape safe
    /// wrappers call, so each value is exactly what `satd_16x16` etc. return. The
    /// shape branch is once per row and the kernels inline into the loop.
    #[target_feature(enable = "avx2")]
    pub unsafe fn satd_grid_row(
        a: *const u8, sa: usize, b0: *const u8, sb: usize, step: usize, n: usize,
        w: usize, h: usize, out: *mut i32,
    ) {
        if w == 16 {
            for i in 0..n {
                *out.add(i) = satd_w16(a, sa, b0.add(i * step), sb, h);
            }
        } else {
            for i in 0..n {
                *out.add(i) = satd_w8(a, sa, b0.add(i * step), sb, h);
            }
        }
    }

    /// `(sum, sum of squares)` of a 16x16 block (V2 brick 3): `psadbw` against
    /// zero for the sum, `pmaddwd` of the zero-extended halves for the squares
    /// (each `v*v <= 65025`, a pair sum `<= 130050`, a lane `<= 4.2e6`: no
    /// overflow anywhere). SSE2 is x86-64 baseline.
    #[inline]
    pub unsafe fn sum_sq16x16(a: *const u8, sa: usize) -> (u32, u32) {
        let z = _mm_setzero_si128();
        let (mut sum, mut sq) = (_mm_setzero_si128(), _mm_setzero_si128());
        for r in 0..16 {
            let v = _mm_loadu_si128(a.add(r * sa) as *const __m128i);
            sum = _mm_add_epi64(sum, _mm_sad_epu8(v, z));
            let (lo, hi) = (_mm_unpacklo_epi8(v, z), _mm_unpackhi_epi8(v, z));
            sq = _mm_add_epi32(sq, _mm_add_epi32(_mm_madd_epi16(lo, lo), _mm_madd_epi16(hi, hi)));
        }
        let s = (_mm_cvtsi128_si32(sum) + _mm_cvtsi128_si32(_mm_unpackhi_epi64(sum, sum))) as u32;
        let t = _mm_add_epi32(sq, _mm_shuffle_epi32::<0b01_00_11_10>(sq));
        let t = _mm_add_epi32(t, _mm_shuffle_epi32::<0b10_11_00_01>(t));
        (s, _mm_cvtsi128_si32(t) as u32)
    }

    /// One 4x4 SATD, `(Σ|H·d| + 1) >> 1`, in two 128-bit registers (V10 brick 1;
    /// the band kernels need four blocks side by side). Rows 0|1 and 2|3 of the
    /// difference sit in the i16 lanes of `d01`/`d23`; two vertical butterfly
    /// stages, then one horizontal (adjacent-lane swap), and the LAST stage is
    /// replaced by the exact identity `|a+b| + |a-b| = 2·max(|a|,|b|)`. The 16
    /// coefficients all share the block-sum parity, so `Σ|H·d|` is even and
    /// `(Σ + 1) >> 1 == Σ >> 1`: the lane total of the maxima IS `Σ|H·d|`.
    /// Lanes stay within ±2040 (eight differences), so i16 never overflows.
    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn satd4x4(a: *const u8, sa: usize, b: *const u8, sb: usize) -> i32 {
        let ld = |p: *const u8| _mm_cvtsi32_si128((p as *const i32).read_unaligned());
        let a01 = _mm_unpacklo_epi32(ld(a), ld(a.add(sa)));
        let a23 = _mm_unpacklo_epi32(ld(a.add(2 * sa)), ld(a.add(3 * sa)));
        let b01 = _mm_unpacklo_epi32(ld(b), ld(b.add(sb)));
        let b23 = _mm_unpacklo_epi32(ld(b.add(2 * sb)), ld(b.add(3 * sb)));
        let d01 = _mm_sub_epi16(_mm_cvtepu8_epi16(a01), _mm_cvtepu8_epi16(b01));
        let d23 = _mm_sub_epi16(_mm_cvtepu8_epi16(a23), _mm_cvtepu8_epi16(b23));
        // Vertical: (r0±r2 | r1±r3), then (r0+r2)±(r1+r3) | (r0-r2)±(r1-r3).
        let s = _mm_add_epi16(d01, d23);
        let t = _mm_sub_epi16(d01, d23);
        let u = _mm_unpacklo_epi64(s, t);
        let w = _mm_unpackhi_epi64(s, t);
        let p = _mm_add_epi16(u, w);
        let q = _mm_sub_epi16(u, w);
        // Horizontal stage 1: pairs (x0,x1),(x2,x3) of each row; keep the sum in
        // even lanes and the difference (sign irrelevant to |.|) in odd lanes.
        let swap16 = _mm_setr_epi8(2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13);
        let sp = _mm_shuffle_epi8(p, swap16);
        let sq = _mm_shuffle_epi8(q, swap16);
        let y = _mm_blend_epi16::<0xAA>(_mm_add_epi16(p, sp), _mm_sub_epi16(p, sp));
        let z = _mm_blend_epi16::<0xAA>(_mm_add_epi16(q, sq), _mm_sub_epi16(q, sq));
        // Horizontal stage 2 as 2·max: pair lane k with lane k+2 of each row.
        let ay = _mm_abs_epi16(y);
        let az = _mm_abs_epi16(z);
        let my = _mm_max_epi16(ay, _mm_shuffle_epi32::<0b10_11_00_01>(ay));
        let mz = _mm_max_epi16(az, _mm_shuffle_epi32::<0b10_11_00_01>(az));
        let t32 = _mm_madd_epi16(_mm_add_epi16(my, mz), _mm_set1_epi16(1));
        let t2 = _mm_add_epi32(t32, _mm_shuffle_epi32::<0b01_00_11_10>(t32));
        let t1 = _mm_add_epi32(t2, _mm_shuffle_epi32::<0b10_11_00_01>(t2));
        _mm_cvtsi128_si32(t1) >> 1
    }
}

// ---------------------------------------------------------------------------------
// aarch64 NEON — SAD (`vabdl` + pairwise widening add) and the hadamard SATD bands
// until the Hadamard band kernel gets a NEON twin.
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    #[inline(always)]
    unsafe fn diff8(a: *const u8, b: *const u8) -> int16x8_t {
        vsubq_s16(
            vreinterpretq_s16_u16(vmovl_u8(vld1_u8(a))),
            vreinterpretq_s16_u16(vmovl_u8(vld1_u8(b))),
        )
    }

    /// Transpose a PAIR of 4x4 i16 blocks held as rows of `[L | R]` lanes.
    #[inline(always)]
    unsafe fn transpose_pair(
        a: int16x8_t, b: int16x8_t, c: int16x8_t, d: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        let t0 = vzip1q_s16(a, b);
        let t1 = vzip2q_s16(a, b);
        let t2 = vzip1q_s16(c, d);
        let t3 = vzip2q_s16(c, d);
        let u0 = vreinterpretq_s16_s32(vzip1q_s32(vreinterpretq_s32_s16(t0), vreinterpretq_s32_s16(t2)));
        let u1 = vreinterpretq_s16_s32(vzip2q_s32(vreinterpretq_s32_s16(t0), vreinterpretq_s32_s16(t2)));
        let u2 = vreinterpretq_s16_s32(vzip1q_s32(vreinterpretq_s32_s16(t1), vreinterpretq_s32_s16(t3)));
        let u3 = vreinterpretq_s16_s32(vzip2q_s32(vreinterpretq_s32_s16(t1), vreinterpretq_s32_s16(t3)));
        (
            vcombine_s16(vget_low_s16(u0), vget_low_s16(u2)),
            vcombine_s16(vget_high_s16(u0), vget_high_s16(u2)),
            vcombine_s16(vget_low_s16(u1), vget_low_s16(u3)),
            vcombine_s16(vget_high_s16(u1), vget_high_s16(u3)),
        )
    }

    /// 4-point hadamard butterfly, lane-wise (both stages use this shape; the
    /// two stages produce the transpose of the scalar's ordering, which the
    /// abs-sum cannot see).
    #[inline(always)]
    unsafe fn butterfly(
        r0: int16x8_t, r1: int16x8_t, r2: int16x8_t, r3: int16x8_t,
    ) -> (int16x8_t, int16x8_t, int16x8_t, int16x8_t) {
        let s0 = vaddq_s16(r0, r2);
        let s1 = vaddq_s16(r1, r3);
        let d0 = vsubq_s16(r0, r2);
        let d1 = vsubq_s16(r1, r3);
        (
            vaddq_s16(s0, s1),
            vaddq_s16(d0, d1),
            vsubq_s16(d0, d1),
            vsubq_s16(s0, s1),
        )
    }

    /// SATD of a 4-row x 8-col band = two 4x4 blocks, each rounded per block:
    /// `((sumL + 1) >> 1) + ((sumR + 1) >> 1)`.
    #[inline(always)]
    unsafe fn satd_band8(a: *const u8, sa: usize, b: *const u8, sb: usize) -> i32 {
        let r0 = diff8(a, b);
        let r1 = diff8(a.add(sa), b.add(sb));
        let r2 = diff8(a.add(2 * sa), b.add(2 * sb));
        let r3 = diff8(a.add(3 * sa), b.add(3 * sb));
        let (v0, v1, v2, v3) = butterfly(r0, r1, r2, r3);
        let (t0, t1, t2, t3) = transpose_pair(v0, v1, v2, v3);
        let (h0, h1, h2, h3) = butterfly(t0, t1, t2, t3);
        // |coef| <= 4080, and a block's 16 abs values sum to <= 65280 — u16-safe.
        let acc = vaddq_u16(
            vaddq_u16(vreinterpretq_u16_s16(vabsq_s16(h0)), vreinterpretq_u16_s16(vabsq_s16(h1))),
            vaddq_u16(vreinterpretq_u16_s16(vabsq_s16(h2)), vreinterpretq_u16_s16(vabsq_s16(h3))),
        );
        let sl = vaddlv_u16(vget_low_u16(acc)) as i32;
        let sr = vaddlv_u16(vget_high_u16(acc)) as i32;
        ((sl + 1) >> 1) + ((sr + 1) >> 1)
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn satd_w16(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut total = 0;
        let mut r = 0;
        while r < h {
            total += satd_band8(a.add(r * sa), sa, b.add(r * sb), sb);
            total += satd_band8(a.add(r * sa + 8), sa, b.add(r * sb + 8), sb);
            r += 4;
        }
        total
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn satd_w8(a: *const u8, sa: usize, b: *const u8, sb: usize, h: usize) -> i32 {
        let mut total = 0;
        let mut r = 0;
        while r < h {
            total += satd_band8(a.add(r * sa), sa, b.add(r * sb), sb);
            r += 4;
        }
        total
    }

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

    /// NEON twin of `x86::sad_avg`: `vrhadd` is the same `(a + b + 1) >> 1`.
    pub unsafe fn sad_avg(
        s: *const u8, ss: usize, a: *const u8, b: *const u8, rs: usize, w: usize, h: usize,
    ) -> i32 {
        let mut acc = vdupq_n_u32(0);
        for r in 0..h {
            let (ps, pa, pb) = (s.add(r * ss), a.add(r * rs), b.add(r * rs));
            if w == 16 {
                let d = vabdq_u8(vld1q_u8(ps), vrhaddq_u8(vld1q_u8(pa), vld1q_u8(pb)));
                acc = vpadalq_u16(acc, vpaddlq_u8(d));
            } else {
                let d = vabd_u8(vld1_u8(ps), vrhadd_u8(vld1_u8(pa), vld1_u8(pb)));
                acc = vpadalq_u16(acc, vcombine_u16(vpaddl_u8(d), vdup_n_u16(0)));
            }
        }
        vaddvq_u32(acc) as i32
    }

    /// NEON twin of `x86::sum_sq16x16`: `vmull_u8` squares fit u16 (<= 65025).
    pub unsafe fn sum_sq16x16(a: *const u8, sa: usize) -> (u32, u32) {
        let (mut s, mut q) = (vdupq_n_u32(0), vdupq_n_u32(0));
        for r in 0..16 {
            let v = vld1q_u8(a.add(r * sa));
            s = vpadalq_u16(s, vpaddlq_u8(v));
            q = vpadalq_u16(q, vmull_u8(vget_low_u8(v), vget_low_u8(v)));
            q = vpadalq_u16(q, vmull_u8(vget_high_u8(v), vget_high_u8(v)));
        }
        (vaddvq_u32(s), vaddvq_u32(q))
    }

    /// NEON twin of `x86::satd4x4` — the same butterflies and the same exact
    /// last-stage `2·max` identity (see there).
    pub unsafe fn satd4x4(a: *const u8, sa: usize, b: *const u8, sb: usize) -> i32 {
        let ld2 = |p: *const u8, s: usize| {
            let lo = (p as *const u32).read_unaligned() as u64;
            let hi = (p.add(s) as *const u32).read_unaligned() as u64;
            vcreate_u8(lo | (hi << 32))
        };
        let d01 = vreinterpretq_s16_u16(vsubl_u8(ld2(a, sa), ld2(b, sb)));
        let d23 = vreinterpretq_s16_u16(vsubl_u8(ld2(a.add(2 * sa), sa), ld2(b.add(2 * sb), sb)));
        let s = vaddq_s16(d01, d23);
        let t = vsubq_s16(d01, d23);
        let u = vcombine_s16(vget_low_s16(s), vget_low_s16(t));
        let w = vcombine_s16(vget_high_s16(s), vget_high_s16(t));
        let p = vaddq_s16(u, w);
        let q = vsubq_s16(u, w);
        let sp = vrev32q_s16(p);
        let sq = vrev32q_s16(q);
        let y = vtrn1q_s16(vaddq_s16(p, sp), vsubq_s16(p, sp));
        let z = vtrn1q_s16(vaddq_s16(q, sq), vsubq_s16(q, sq));
        let ay = vabsq_s16(y);
        let az = vabsq_s16(z);
        let my = vmaxq_s16(ay, vreinterpretq_s16_s32(vrev64q_s32(vreinterpretq_s32_s16(ay))));
        let mz = vmaxq_s16(az, vreinterpretq_s16_s32(vrev64q_s32(vreinterpretq_s32_s16(az))));
        vaddlvq_s16(vaddq_s16(my, mz)) >> 1
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
            #[cfg(target_arch = "aarch64")]
            if std::arch::is_aarch64_feature_detected!("neon") {
                // SAFETY: bounds asserted above; NEON checked here.
                return unsafe { arm::$simd(a.as_ptr(), sa, b.as_ptr(), sb, $h) };
            }
            #[allow(unreachable_code)]
            satd_region_scalar(a, sa, b, sb, $w, $h)
        }
    };
}

satd_fn!(satd_16x16, 16, 16, satd_w16);
satd_fn!(satd_16x8, 16, 8, satd_w16);
satd_fn!(satd_8x16, 8, 16, satd_w8);
satd_fn!(satd_8x8, 8, 8, satd_w8);

/// SATD of two 4x4 blocks. Too small for the band kernel (which does four at
/// once), so it has its own single-block kernel (V10 brick 1: was scalar, ~237
/// Ir a call, and the quality preset's Intra_4x4 search makes ~460K of them).
pub fn satd_4x4(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
    assert!(a.len() >= 3 * sa + 4 && b.len() >= 3 * sb + 4);
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: four rows of four bytes at each stride asserted above; AVX2
        // (and with it SSE4.1/SSSE3) checked here.
        return unsafe { x86::satd4x4(a.as_ptr(), sa, b.as_ptr(), sb) };
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: as above; NEON checked here.
        return unsafe { arm::satd4x4(a.as_ptr(), sa, b.as_ptr(), sb) };
    }
    #[allow(unreachable_code)]
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
// V6 brick 1 (encoder Ir campaign): the 8-wide row path of `x86::sad` already
// existed (`_mm_loadl_epi64`), but no 8x8 wrapper did, so the encoder's 8x8 SAD
// (the B2 full-pel phase of every P_8x8 partition) ran a scalar row loop.
sad_fn!(sad_8x8, 8, 8);

/// Scalar oracle of [`sad_avg`]: `sum |s - ((a + b + 1) >> 1)|`.
pub fn sad_avg_scalar(s: &[u8], ss: usize, a: &[u8], b: &[u8], rs: usize, w: usize, h: usize) -> i32 {
    let mut t = 0i32;
    for r in 0..h {
        for i in 0..w {
            let p = ((a[r * rs + i] as u16 + b[r * rs + i] as u16 + 1) >> 1) as i32;
            t += (s[r * ss + i] as i32 - p).abs();
        }
    }
    t
}

/// Fused quarter-pel SAD `sum |s - ((a + b + 1) >> 1)|` over a `w`x`h` block, `a`
/// and `b` sharing stride `rs` (V6 brick 3, encoder Ir campaign). `None` for a
/// width the kernels do not cover (anything but 16 or 8) or an `h` above 16 --
/// the caller keeps its scalar loop for those. Exact: `pavgb`/`vrhadd` compute
/// the identical rounding average.
pub fn sad_avg(s: &[u8], ss: usize, a: &[u8], b: &[u8], rs: usize, w: usize, h: usize) -> Option<i32> {
    if !(w == 16 || w == 8) || h == 0 || h > 16 {
        return None;
    }
    assert!(s.len() >= (h - 1) * ss + w && a.len() >= (h - 1) * rs + w && b.len() >= (h - 1) * rs + w);
    #[cfg(target_arch = "x86_64")]
    // SAFETY: lengths asserted above; SSE2 is x86-64 baseline.
    return Some(unsafe { x86::sad_avg(s.as_ptr(), ss, a.as_ptr(), b.as_ptr(), rs, w, h) });
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: lengths asserted above; NEON checked here.
        return Some(unsafe { arm::sad_avg(s.as_ptr(), ss, a.as_ptr(), b.as_ptr(), rs, w, h) });
    }
    #[allow(unreachable_code)]
    None
}

/// A full-pel SATD search GRID (V7 brick 3, encoder Ir campaign): one `w`x`h`
/// source block against the `n`x`n` reference positions
/// `(x0 + i*step, y0 + j*step)`. The whole window, the source block and AVX2 are
/// validated ONCE in [`SatdGrid::new`]; [`SatdGrid::row`] then fills a row of
/// values with no per-point dispatch, bounds test or feature check. Each value is
/// exactly `satd_16x16`/`satd_16x8`/`satd_8x16`/`satd_8x8` of that block pair.
// Off x86-64 `new` always declines, so the fields are never read there.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub struct SatdGrid<'a> {
    src: &'a [u8],
    ss: usize,
    rf: &'a [u8],
    rs: usize,
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    n: usize,
    step: usize,
}

impl<'a> SatdGrid<'a> {
    /// `None` off x86-64, without AVX2, for a shape other than 16x16/16x8/8x16/8x8,
    /// for `n` outside 1..=64, or when any block of the window would leave `rf`
    /// (or the source block `src`) — the caller keeps its per-point path then.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        src: &'a [u8], ss: usize, rf: &'a [u8], rs: usize, w: usize, h: usize,
        x0: usize, y0: usize, n: usize, step: usize,
    ) -> Option<Self> {
        if !matches!((w, h), (16, 16) | (16, 8) | (8, 16) | (8, 8)) || n == 0 || n > 64 {
            return None;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (src, ss, rf, rs, x0, y0, step);
            None
        }
        #[cfg(target_arch = "x86_64")]
        {
            if !std::is_x86_feature_detected!("avx2") || src.len() < (h - 1) * ss + w {
                return None;
            }
            let span = (n - 1).checked_mul(step)?;
            let need = (y0.checked_add(span)?.checked_add(h - 1)?)
                .checked_mul(rs)?
                .checked_add(x0.checked_add(span)?.checked_add(w)?)?;
            if rf.len() < need || w > rs {
                return None;
            }
            Some(SatdGrid { src, ss, rf, rs, w, h, x0, y0, n, step })
        }
    }

    /// Points per axis.
    pub fn n(&self) -> usize {
        self.n
    }

    /// `out[i] = SATD(src, ref at (x0 + i*step, y0 + j*step))` for `i in 0..n`.
    pub fn row(&self, j: usize, out: &mut [i32]) {
        assert!(j < self.n && out.len() >= self.n);
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `new` proved every block of row `j < n` lies inside `rf`, the source
        // block inside `src`, and AVX2 present; `out` holds `n` values (asserted).
        unsafe {
            let b0 = self.rf.as_ptr().add((self.y0 + j * self.step) * self.rs + self.x0);
            x86::satd_grid_row(
                self.src.as_ptr(), self.ss, b0, self.rs, self.step, self.n, self.w, self.h,
                out.as_mut_ptr(),
            );
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = out;
    }
}

/// Scalar oracle of [`sum_sq_16x16`].
pub fn sum_sq_16x16_scalar(src: &[u8], stride: usize) -> (u32, u32) {
    let (mut s, mut q) = (0u32, 0u32);
    for r in 0..16 {
        for &p in &src[r * stride..r * stride + 16] {
            s += p as u32;
            q += p as u32 * p as u32;
        }
    }
    (s, q)
}

/// `(sum, sum of squares)` of the 16x16 block at `src` (row stride `stride`) —
/// the two moments of a macroblock variance (V2 brick 3, encoder Ir campaign).
/// Exact on every input: `sum <= 65280`, `sum of squares <= 16_646_400`.
pub fn sum_sq_16x16(src: &[u8], stride: usize) -> (u32, u32) {
    assert!(src.len() >= 15 * stride + 16);
    #[cfg(target_arch = "x86_64")]
    // SAFETY: 16 rows of 16 bytes at `stride` asserted above; SSE2 is baseline.
    return unsafe { x86::sum_sq16x16(src.as_ptr(), stride) };
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: as above; NEON checked here.
        return unsafe { arm::sum_sq16x16(src.as_ptr(), stride) };
    }
    #[allow(unreachable_code)]
    sum_sq_16x16_scalar(src, stride)
}

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
            assert_eq!(sad_8x8(&a, 16, &b, 16), sad_scalar(&a, 16, &b, 16, 8, 8), "8x8 {seed}");
        }
    }

    /// The single-block 4x4 kernel against the scalar oracle: random blocks at
    /// odd strides and unaligned offsets, mixed with saturated rows so every
    /// butterfly stage sees its extremes.
    #[test]
    fn satd_4x4_matches_scalar() {
        let mut st = 0x9e37_79b9u32;
        let mut next = move || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        for round in 0..20_000usize {
            let (sa, sb) = (4 + (next() % 29) as usize, 4 + (next() % 31) as usize);
            let (oa, ob) = ((next() % 7) as usize, (next() % 5) as usize);
            let mut a = vec![0u8; oa + 3 * sa + 4];
            let mut b = vec![0u8; ob + 3 * sb + 4];
            let mode = round % 4;
            for v in a.iter_mut().chain(b.iter_mut()) {
                let r = next();
                *v = match mode {
                    0 => r as u8,
                    1 => if r & 1 == 0 { 0 } else { 255 },
                    2 => 128u8.wrapping_add((r % 9) as u8).wrapping_sub(4),
                    _ => if r % 3 == 0 { 255 } else { (r >> 8) as u8 },
                };
            }
            assert_eq!(
                satd_4x4(&a[oa..], sa, &b[ob..], sb),
                satd4x4_scalar(&a[oa..], sa, &b[ob..], sb),
                "round {round} sa {sa} sb {sb}"
            );
        }
        for (va, vb) in [(0u8, 255u8), (255, 0), (0, 0), (255, 255)] {
            let (a, b) = (vec![va; 16], vec![vb; 16]);
            assert_eq!(satd_4x4(&a, 4, &b, 4), satd4x4_scalar(&a, 4, &b, 4));
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

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn lcg(state: &mut u64) -> u32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*state >> 33) as u32
    }

    /// Arch-agnostic differential: whatever the dispatch selects (AVX2 on x86,
    /// NEON on aarch64, scalar elsewhere) must equal the scalar oracle. This is
    /// the aarch64 SATD gate — the openh264-C reference tests live in the
    /// x86-only module and never run there.
    #[test]
    fn satd_dispatch_matches_scalar_all_shapes() {
        let mut st = 0x777u64;
        let (sa, sb) = (24usize, 20usize);
        for round in 0..400usize {
            let mut a = vec![0u8; sa * 20];
            let mut b = vec![0u8; sb * 20];
            for v in a.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            for v in b.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8)] {
                let got = match (w, h) {
                    (16, 16) => satd_16x16(&a, sa, &b, sb),
                    (16, 8) => satd_16x8(&a, sa, &b, sb),
                    (8, 16) => satd_8x16(&a, sa, &b, sb),
                    _ => satd_8x8(&a, sa, &b, sb),
                };
                assert_eq!(got, satd_region_scalar(&a, sa, &b, sb, w, h), "round {round} {w}x{h}");
            }
            for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8)] {
                let got = match (w, h) {
                    (16, 16) => sad_16x16(&a, sa, &b, sb),
                    (16, 8) => sad_16x8(&a, sa, &b, sb),
                    (8, 16) => sad_8x16(&a, sa, &b, sb),
                    _ => sad_8x8(&a, sa, &b, sb),
                };
                assert_eq!(got, sad_scalar(&a, sa, &b, sb, w, h), "round {round} sad {w}x{h}");
            }
        }
    }

    #[test]
    fn sum_sq_matches_scalar() {
        let mut st = 0x51u64;
        let stride = 37usize;
        for round in 0..300usize {
            let mut b = vec![0u8; stride * 16];
            for v in b.iter_mut() {
                *v = match round % 3 {
                    0 => lcg(&mut st) as u8,
                    1 => 255,
                    _ => (lcg(&mut st) & 1) as u8 * 255,
                };
            }
            assert_eq!(sum_sq_16x16(&b, stride), sum_sq_16x16_scalar(&b, stride), "round {round}");
        }
    }

    #[test]
    fn satd_grid_matches_wrappers() {
        let mut st = 0x9a1du64;
        let (ss, rs) = (40usize, 96usize);
        let mut src = vec![0u8; ss * 16];
        let mut rf = vec![0u8; rs * 96];
        for v in src.iter_mut().chain(rf.iter_mut()) {
            *v = lcg(&mut st) as u8;
        }
        for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8)] {
            let (x0, y0, n, step) = (3usize, 5usize, 17usize, 2usize);
            let Some(g) = SatdGrid::new(&src, ss, &rf, rs, w, h, x0, y0, n, step) else {
                #[cfg(target_arch = "x86_64")]
                assert!(!std::is_x86_feature_detected!("avx2"), "grid declined on an AVX2 host");
                continue;
            };
            let mut row = [0i32; 64];
            for j in 0..n {
                g.row(j, &mut row);
                for i in 0..n {
                    let b = &rf[(y0 + j * step) * rs + x0 + i * step..];
                    let want = match (w, h) {
                        (16, 16) => satd_16x16(&src, ss, b, rs),
                        (16, 8) => satd_16x8(&src, ss, b, rs),
                        (8, 16) => satd_8x16(&src, ss, b, rs),
                        _ => satd_8x8(&src, ss, b, rs),
                    };
                    assert_eq!(row[i], want, "{w}x{h} ({i},{j})");
                }
            }
            // A window that would leave the reference by one sample is declined.
            assert!(SatdGrid::new(&src, ss, &rf, rs, w, h, x0, 96 - h - 31, n, step).is_none());
        }
    }

    #[test]
    fn sad_avg_matches_scalar() {
        let mut st = 0x5eedu64;
        let (ss, rs) = (24usize, 40usize);
        for round in 0..400usize {
            let mut s = vec![0u8; ss * 17];
            let mut a = vec![0u8; rs * 17];
            let mut b = vec![0u8; rs * 17];
            for v in s.iter_mut().chain(a.iter_mut()).chain(b.iter_mut()) {
                *v = lcg(&mut st) as u8;
            }
            if round % 50 == 0 {
                // extremes: rounding of odd sums at both ends of the range
                for v in a.iter_mut() {
                    *v = 255;
                }
                for v in b.iter_mut() {
                    *v = if round % 100 == 0 { 254 } else { 0 };
                }
            }
            for &(w, h) in &[(16usize, 16usize), (16, 8), (8, 16), (8, 8), (8, 4), (16, 4)] {
                let got = sad_avg(&s, ss, &a, &b, rs, w, h);
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                assert_eq!(got, Some(sad_avg_scalar(&s, ss, &a, &b, rs, w, h)), "round {round} {w}x{h}");
                #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
                assert_eq!(got, None);
            }
            assert_eq!(sad_avg(&s, ss, &a, &b, rs, 4, 4), None);
        }
    }
}
