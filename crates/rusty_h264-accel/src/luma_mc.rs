//! Half-pel **luma** motion compensation — portable SIMD (rip-ASM Phase 2).
//!
//! Replaces openh264's `McHorVer20*`, `McHorVer02*`, `McHorVer22*` and `PixelAvg*`
//! (4,490 LOC of NASM) with Rust intrinsics. This is the largest SIMD-addressable
//! bucket in the decoder: inter-mc measures **10–17% of decode** in the anatomy.
//!
//! ## The filters (H.264 §8.4.2.2.1)
//!
//! The half-pel interpolator is the 6-tap `(1, −5, 20, 20, −5, 1)`:
//!
//! ```text
//! b (horizontal) = clip(( 6tap_h + 16) >>  5)
//! h (vertical)   = clip(( 6tap_v + 16) >>  5)
//! j (centre)     = clip(( 6tap applied twice, at full precision, + 512) >> 10)
//! quarter-pel    = (a + b + 1) >> 1   -- an average of two of the above
//! ```
//!
//! ## Three facts that shape the implementation
//!
//! **1. The single-pass filters fit in `i16`.** With 8-bit input the 6-tap ranges over
//! `[-2550, 10710]`, so `b` and `h` are computed entirely in 16-bit lanes — 8 pixels per
//! 128-bit register, no widening.
//!
//! **2. The centre filter does NOT.** `j` applies the 6-tap to 6-tap outputs, reaching
//! `±475320`, so the second pass must accumulate in `i32`. Pass one stays `i16` (it is
//! just `b` without the shift), pass two widens.
//!
//! **3. No multiplies are needed.** `20 = 16 + 4` and `5 = 4 + 1`, so every tap is
//! shifts and adds. That keeps the x86 path **SSE2-only** — `_mm_mullo_epi32` is SSE4.1
//! and would have forced either a raised baseline or a second code path.
//!
//! The saturating narrows do the clip for free: `_mm_packus_epi16` on x86, and on NEON
//! `vqrshrun_n_s16::<5>` performs round-add, shift, clamp and narrow in one instruction.
//!
//! Ordering note: the spec defines `j` via vertical-then-horizontal, and the scalar twin
//! in `common::inter` does it that way. This uses **horizontal-then-vertical**, which is
//! identical arithmetic — the convolution is separable and every intermediate is exact —
//! and is far better for SIMD, because the second pass then reads whole rows with no
//! lane shuffling. The tests pin it against the scalar oracle either way.

// ---------------------------------------------------------------------------------
// Scalar references. The oracles, and the fallback on other architectures.
// ---------------------------------------------------------------------------------

#[inline]
fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// 6-tap over six already-loaded samples.
#[inline]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

fn hor20_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2] as i32, src[p - 1] as i32, src[p] as i32,
                src[p + 1] as i32, src[p + 2] as i32, src[p + 3] as i32,
            );
            dst[r * w + c] = clip_u8((f + 16) >> 5);
        }
    }
}

fn ver02_scalar(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    for r in 0..h {
        for c in 0..w {
            let p = off + r * ts + c;
            let f = tap6(
                src[p - 2 * ts] as i32, src[p - ts] as i32, src[p] as i32,
                src[p + ts] as i32, src[p + 2 * ts] as i32, src[p + 3 * ts] as i32,
            );
            dst[r * w + c] = clip_u8((f + 16) >> 5);
        }
    }
}

fn centre_scalar(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize) {
    // horizontal-first at full precision, then vertical (see module docs)
    let mut hor = vec![0i32; (h + 5) * w];
    for rr in 0..h + 5 {
        for c in 0..w {
            let p = rr * ts + c;
            hor[rr * w + c] = tap6(
                t[p] as i32, t[p + 1] as i32, t[p + 2] as i32,
                t[p + 3] as i32, t[p + 4] as i32, t[p + 5] as i32,
            );
        }
    }
    for r in 0..h {
        for c in 0..w {
            let g = |k: usize| hor[(r + k) * w + c];
            let f = tap6(g(0), g(1), g(2), g(3), g(4), g(5));
            dst[r * w + c] = clip_u8((f + 512) >> 10);
        }
    }
}

fn pixel_avg_scalar(
    dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
) {
    for r in 0..h {
        for c in 0..w {
            dst[r * w + c] = ((a[r * a_stride + c] as u32 + b[r * b_stride + c] as u32 + 1) >> 1) as u8;
        }
    }
}

// ---------------------------------------------------------------------------------
// x86-64 SSE2
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// Load 8 bytes at `p`, zero-extend to 8 lanes of i16.
    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn ld8(p: *const u8) -> __m128i {
        _mm_unpacklo_epi8(_mm_loadl_epi64(p as *const __m128i), _mm_setzero_si128())
    }

    /// `a - 5b + 20c + 20d - 5e + f` in i16 lanes, via shifts (20=16+4, 5=4+1).
    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn tap6_epi16(a: __m128i, b: __m128i, c: __m128i, d: __m128i, e: __m128i, f: __m128i) -> __m128i {
        let s = _mm_add_epi16(b, e); // *5
        let t = _mm_add_epi16(c, d); // *20
        let five = _mm_add_epi16(_mm_slli_epi16::<2>(s), s);
        let twenty = _mm_add_epi16(_mm_slli_epi16::<4>(t), _mm_slli_epi16::<2>(t));
        _mm_add_epi16(_mm_sub_epi16(_mm_add_epi16(a, f), five), twenty)
    }

    /// Same shape in i32 lanes, for the centre filter's second pass.
    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn tap6_epi32(a: __m128i, b: __m128i, c: __m128i, d: __m128i, e: __m128i, f: __m128i) -> __m128i {
        let s = _mm_add_epi32(b, e);
        let t = _mm_add_epi32(c, d);
        let five = _mm_add_epi32(_mm_slli_epi32::<2>(s), s);
        let twenty = _mm_add_epi32(_mm_slli_epi32::<4>(t), _mm_slli_epi32::<2>(t));
        _mm_add_epi32(_mm_sub_epi32(_mm_add_epi32(a, f), five), twenty)
    }

    /// `clip((v + 16) >> 5)` for 8 i16 lanes -> 8 packed u8. `packus` does the clip.
    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn round_shift_pack(v: __m128i) -> __m128i {
        let r = _mm_srai_epi16::<5>(_mm_add_epi16(v, _mm_set1_epi16(16)));
        _mm_packus_epi16(r, r)
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2)), ld8(p.sub(1)), ld8(p),
                    ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)),
                );
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, round_shift_pack(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, round_shift_pack(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hor: &mut [i16]) {
        centre_pass1(t, ts, w, h, hor);
        centre_pass2(dst, w, h, hor);
    }

    /// Pass 1: horizontal 6-tap, full precision, into i16 (range [-2550, 10710]).
    #[target_feature(enable = "sse2")]
    pub unsafe fn centre_pass1(t: &[u8], ts: usize, w: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let row = tp.add(rr * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_epi16(
                    ld8(p), ld8(p.add(1)), ld8(p.add(2)),
                    ld8(p.add(3)), ld8(p.add(4)), ld8(p.add(5)),
                );
                _mm_storeu_si128(hor.as_mut_ptr().add(rr * w + c) as *mut __m128i, v);
                c += 8;
            }
        }
    }

    /// Pass 2: vertical 6-tap on the i16 intermediates, accumulating in i32.
    #[target_feature(enable = "sse2")]
    pub unsafe fn centre_pass2(dst: &mut [u8], w: usize, h: usize, hor: &[i16]) {
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        let round = _mm_set1_epi32(512);
        for r in 0..h {
            let mut c = 0;
            while c < w {
                // sign-extend i16 -> i32 for the low and high halves of each row
                let load = |k: usize| -> (__m128i, __m128i) {
                    let x = _mm_loadu_si128(hp.add((r + k) * w + c) as *const __m128i);
                    (
                        _mm_srai_epi32::<16>(_mm_unpacklo_epi16(x, x)),
                        _mm_srai_epi32::<16>(_mm_unpackhi_epi16(x, x)),
                    )
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                let lo = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a0, b0, c0, d0, e0, f0), round));
                let hi = _mm_srai_epi32::<10>(_mm_add_epi32(tap6_epi32(a1, b1, c1, d1, e1, f1), round));
                // packs (i32->i16, saturating) then packus (i16->u8, saturating) = clip
                let packed = _mm_packus_epi16(_mm_packs_epi32(lo, hi), _mm_setzero_si128());
                _mm_storel_epi64(dp.add(r * w + c) as *mut __m128i, packed);
                c += 8;
            }
        }
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn pixel_avg(
        dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
    ) {
        let (dp, ap, bp) = (dst.as_mut_ptr(), a.as_ptr(), b.as_ptr());
        for r in 0..h {
            let (ar, br, dr) = (ap.add(r * a_stride), bp.add(r * b_stride), dp.add(r * w));
            if w == 16 {
                // _mm_avg_epu8 IS (a + b + 1) >> 1 — the exact rounding the spec wants.
                let v = _mm_avg_epu8(_mm_loadu_si128(ar as *const __m128i), _mm_loadu_si128(br as *const __m128i));
                _mm_storeu_si128(dr as *mut __m128i, v);
            } else if w == 8 {
                let v = _mm_avg_epu8(_mm_loadl_epi64(ar as *const __m128i), _mm_loadl_epi64(br as *const __m128i));
                _mm_storel_epi64(dr as *mut __m128i, v);
            } else {
                let va = _mm_cvtsi32_si128(ar.cast::<u32>().read_unaligned() as i32);
                let vb = _mm_cvtsi32_si128(br.cast::<u32>().read_unaligned() as i32);
                let v = _mm_avg_epu8(va, vb);
                dr.cast::<u32>().write_unaligned(_mm_cvtsi128_si32(v) as u32);
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// x86-64 AVX2 — the 16-wide path.
//
// The openh264 assembly this replaces had AVX2 kernels (`McHorVer20_avx2`,
// `McHorVer02_avx2`), and dropping to SSE2-only measured a consistent (if
// floor-limited) ~2% on decode. A 16-wide block is exactly 16 i16 lanes = one 256-bit
// register, so the whole row is one pass instead of two.
//
// The pack is the only subtlety: `_mm256_packus_epi16` works WITHIN 128-bit lanes, so
// the halves come out interleaved and need a `permute4x64` to put them back in order.
// ---------------------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
mod x86_avx2 {
    use std::arch::x86_64::*;

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn ld16(p: *const u8) -> __m256i {
        _mm256_cvtepu8_epi16(_mm_loadu_si128(p as *const __m128i))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn tap6(a: __m256i, b: __m256i, c: __m256i, d: __m256i, e: __m256i, f: __m256i) -> __m256i {
        let s = _mm256_add_epi16(b, e);
        let t = _mm256_add_epi16(c, d);
        let five = _mm256_add_epi16(_mm256_slli_epi16::<2>(s), s);
        let twenty = _mm256_add_epi16(_mm256_slli_epi16::<4>(t), _mm256_slli_epi16::<2>(t));
        _mm256_add_epi16(_mm256_sub_epi16(_mm256_add_epi16(a, f), five), twenty)
    }

    /// clip((v + 16) >> 5) for 16 i16 lanes -> 16 packed u8, lane order restored.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn round_shift_pack16(v: __m256i) -> __m128i {
        let r = _mm256_srai_epi16::<5>(_mm256_add_epi16(v, _mm256_set1_epi16(16)));
        let p = _mm256_packus_epi16(r, r);
        _mm256_castsi256_si128(_mm256_permute4x64_epi64::<0b1101_1000>(p))
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn hor20_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(ld16(p.sub(2)), ld16(p.sub(1)), ld16(p), ld16(p.add(1)), ld16(p.add(2)), ld16(p.add(3)));
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, round_shift_pack16(v));
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn ver02_w16(src: &[u8], off: usize, ts: usize, dst: &mut [u8], h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let p = sp.add(r * ts);
            let v = tap6(
                ld16(p.sub(2 * ts)), ld16(p.sub(ts)), ld16(p),
                ld16(p.add(ts)), ld16(p.add(2 * ts)), ld16(p.add(3 * ts)),
            );
            _mm_storeu_si128(dp.add(r * 16) as *mut __m128i, round_shift_pack16(v));
        }
    }

    /// Centre pass 1 only (horizontal, full precision into i16). Pass 2 needs i32 and
    /// stays on the SSE2 path, which is already 4-lane-per-half and gains little here.
    #[target_feature(enable = "avx2")]
    pub unsafe fn centre_pass1_w16(t: &[u8], ts: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let p = tp.add(rr * ts);
            let v = tap6(ld16(p), ld16(p.add(1)), ld16(p.add(2)), ld16(p.add(3)), ld16(p.add(4)), ld16(p.add(5)));
            _mm256_storeu_si256(hor.as_mut_ptr().add(rr * 16) as *mut __m256i, v);
        }
    }
}

// ---------------------------------------------------------------------------------
// aarch64 NEON
// ---------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn ld8(p: *const u8) -> int16x8_t {
        vreinterpretq_s16_u16(vmovl_u8(vld1_u8(p)))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn tap6_s16(a: int16x8_t, b: int16x8_t, c: int16x8_t, d: int16x8_t, e: int16x8_t, f: int16x8_t) -> int16x8_t {
        let s = vaddq_s16(b, e);
        let t = vaddq_s16(c, d);
        vaddq_s16(vsubq_s16(vaddq_s16(a, f), vmulq_n_s16(s, 5)), vmulq_n_s16(t, 20))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn tap6_s32(a: int32x4_t, b: int32x4_t, c: int32x4_t, d: int32x4_t, e: int32x4_t, f: int32x4_t) -> int32x4_t {
        let s = vaddq_s32(b, e);
        let t = vaddq_s32(c, d);
        vaddq_s32(vsubq_s32(vaddq_s32(a, f), vmulq_n_s32(s, 5)), vmulq_n_s32(t, 20))
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(ld8(p.sub(2)), ld8(p.sub(1)), ld8(p), ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)));
                // round-add 16, shift 5, saturate to u8, narrow — one instruction.
                vst1_u8(dp.add(r * w + c), vqrshrun_n_s16::<5>(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
        let sp = src.as_ptr().add(off);
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let row = sp.add(r * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(
                    ld8(p.sub(2 * ts)), ld8(p.sub(ts)), ld8(p),
                    ld8(p.add(ts)), ld8(p.add(2 * ts)), ld8(p.add(3 * ts)),
                );
                vst1_u8(dp.add(r * w + c), vqrshrun_n_s16::<5>(v));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize, hor: &mut [i16]) {
        let tp = t.as_ptr();
        for rr in 0..h + 5 {
            let row = tp.add(rr * ts);
            let mut c = 0;
            while c < w {
                let p = row.add(c);
                let v = tap6_s16(ld8(p), ld8(p.add(1)), ld8(p.add(2)), ld8(p.add(3)), ld8(p.add(4)), ld8(p.add(5)));
                vst1q_s16(hor.as_mut_ptr().add(rr * w + c), v);
                c += 8;
            }
        }
        let hp = hor.as_ptr();
        let dp = dst.as_mut_ptr();
        for r in 0..h {
            let mut c = 0;
            while c < w {
                let load = |k: usize| -> (int32x4_t, int32x4_t) {
                    let x = vld1q_s16(hp.add((r + k) * w + c));
                    (vmovl_s16(vget_low_s16(x)), vmovl_high_s16(x))
                };
                let (a0, a1) = load(0);
                let (b0, b1) = load(1);
                let (c0, c1) = load(2);
                let (d0, d1) = load(3);
                let (e0, e1) = load(4);
                let (f0, f1) = load(5);
                // rounding narrow by 10 (adds 512), then saturate to u8
                let lo = vqrshrn_n_s32::<10>(tap6_s32(a0, b0, c0, d0, e0, f0));
                let hi = vqrshrn_n_s32::<10>(tap6_s32(a1, b1, c1, d1, e1, f1));
                vst1_u8(dp.add(r * w + c), vqmovun_s16(vcombine_s16(lo, hi)));
                c += 8;
            }
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn pixel_avg(
        dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
    ) {
        let (dp, ap, bp) = (dst.as_mut_ptr(), a.as_ptr(), b.as_ptr());
        for r in 0..h {
            let (ar, br, dr) = (ap.add(r * a_stride), bp.add(r * b_stride), dp.add(r * w));
            if w == 16 {
                // vrhaddq_u8 IS (a + b + 1) >> 1
                vst1q_u8(dr, vrhaddq_u8(vld1q_u8(ar), vld1q_u8(br)));
            } else if w == 8 {
                vst1_u8(dr, vrhadd_u8(vld1_u8(ar), vld1_u8(br)));
            } else {
                let va = vreinterpret_u8_u32(vdup_n_u32(ar.cast::<u32>().read_unaligned()));
                let vb = vreinterpret_u8_u32(vdup_n_u32(br.cast::<u32>().read_unaligned()));
                let v = vrhadd_u8(va, vb);
                let out = vget_lane_u32::<0>(vreinterpret_u32_u8(v));
                dr.cast::<u32>().write_unaligned(out);
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// Safe dispatch
// ---------------------------------------------------------------------------------

/// Horizontal half-pel plane: `clip((6tap_h + 16) >> 5)`, `w` in {8, 16}.
pub fn mc_hor20(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 && src.len() >= off + (h - 1) * ts + w + 3);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && std::is_x86_feature_detected!("avx2") {
            // SAFETY: bounds asserted; 16 lanes is exactly the block width.
            unsafe { x86_avx2::hor20_w16(src, off, ts, dst, h) };
            return;
        }
        if std::is_x86_feature_detected!("sse2") {
            // SAFETY: bounds asserted; taps span off-2 .. off+(h-1)*ts+w+2.
            unsafe { x86::hor20(src, off, ts, dst, w, h) };
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: as above.
        unsafe { arm::hor20(src, off, ts, dst, w, h) };
        return;
    }
    hor20_scalar(src, off, ts, dst, w, h);
}

/// Vertical half-pel plane: `clip((6tap_v + 16) >> 5)`, `w` in {8, 16}.
pub fn mc_ver02(src: &[u8], off: usize, ts: usize, dst: &mut [u8], w: usize, h: usize) {
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(off >= 2 * ts && src.len() >= off + (h + 2) * ts + w);
    #[cfg(target_arch = "x86_64")]
    {
        if w == 16 && std::is_x86_feature_detected!("avx2") {
            // SAFETY: as the SSE2 arm, 16 lanes wide.
            unsafe { x86_avx2::ver02_w16(src, off, ts, dst, h) };
            return;
        }
        if std::is_x86_feature_detected!("sse2") {
            // SAFETY: bounds asserted; taps span off-2ts .. off+(h+2)*ts+w.
            unsafe { x86::ver02(src, off, ts, dst, w, h) };
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: as above.
        unsafe { arm::ver02(src, off, ts, dst, w, h) };
        return;
    }
    ver02_scalar(src, off, ts, dst, w, h);
}

/// Centre half-pel plane: `clip((6tap applied twice + 512) >> 10)`, `w` in {8, 16}.
pub fn mc_centre(t: &[u8], ts: usize, dst: &mut [u8], w: usize, h: usize) {
    debug_assert!(w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(t.len() >= (h + 4) * ts + w + 5);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        // (h+5) rows x w cols of i16 intermediates; 16x16 worst case = 21*16.
        let mut hor = [0i16; 21 * 16];
        #[cfg(target_arch = "x86_64")]
        {
            if w == 16 && std::is_x86_feature_detected!("avx2") {
                // Pass 1 is 16 i16 lanes = one AVX2 register per row. Pass 2 needs i32
                // and is already 4-per-half on SSE2, so it stays there.
                // SAFETY: bounds asserted; scratch sized for the largest block.
                unsafe {
                    x86_avx2::centre_pass1_w16(t, ts, h, &mut hor);
                    x86::centre_pass2(dst, w, h, &hor);
                }
                return;
            }
            if std::is_x86_feature_detected!("sse2") {
                // SAFETY: bounds asserted; scratch is sized for the largest block.
                unsafe { x86::centre(t, ts, dst, w, h, &mut hor) };
                return;
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            // SAFETY: as above.
            unsafe { arm::centre(t, ts, dst, w, h, &mut hor) };
            return;
        }
        let _ = &mut hor;
    }
    centre_scalar(t, ts, dst, w, h);
}

/// `(a + b + 1) >> 1` of two planes — the quarter-pel average. `w` in {4, 8, 16}.
pub fn pixel_avg(
    dst: &mut [u8], a: &[u8], a_stride: usize, b: &[u8], b_stride: usize, w: usize, h: usize,
) {
    debug_assert!(w == 4 || w == 8 || w == 16);
    assert!(dst.len() >= w * h);
    assert!(a.len() >= (h - 1) * a_stride + w && b.len() >= (h - 1) * b_stride + w);
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse2") {
        // SAFETY: lengths asserted for both sources at their strides and the dst.
        unsafe { x86::pixel_avg(dst, a, a_stride, b, b_stride, w, h) };
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: as above.
        unsafe { arm::pixel_avg(dst, a, a_stride, b, b_stride, w, h) };
        return;
    }
    pixel_avg_scalar(dst, a, a_stride, b, b_stride, w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(buf: &mut [u8], mut seed: u32) {
        for b in buf.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (seed >> 24) as u8;
        }
    }

    #[test]
    fn hor20_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let off = 2 * ts + 2;
                    let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                    fill(&mut src, 0x1234 + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_hor20(&src, off, ts, &mut a, w, h);
                    hor20_scalar(&src, off, ts, &mut b, w, h);
                    assert_eq!(a, b, "hor20 w={w} h={h} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn ver02_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let off = 2 * ts + 2;
                    let mut src = vec![0u8; off + (h + 4) * ts + w + 8];
                    fill(&mut src, 0xABCD + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_ver02(&src, off, ts, &mut a, w, h);
                    ver02_scalar(&src, off, ts, &mut b, w, h);
                    assert_eq!(a, b, "ver02 w={w} h={h} seed={seed}");
                }
            }
        }
    }

    #[test]
    fn centre_matches_scalar() {
        for &w in &[8usize, 16] {
            for &h in &[4usize, 8, 16] {
                for seed in 0..6u32 {
                    let ts = w + 16;
                    let mut t = vec![0u8; (h + 6) * ts + w + 8];
                    fill(&mut t, 0x5EED + seed);
                    let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                    mc_centre(&t, ts, &mut a, w, h);
                    centre_scalar(&t, ts, &mut b, w, h);
                    assert_eq!(a, b, "centre w={w} h={h} seed={seed}");
                }
            }
        }
    }

    /// The centre filter is the one that can overflow i16 and the one whose clip
    /// actually bites, so hammer it at the extremes the random fill never reaches.
    #[test]
    fn centre_saturates_like_scalar() {
        for &w in &[8usize, 16] {
            let (h, ts) = (8usize, w + 16);
            for pattern in 0..3 {
                let mut t = vec![0u8; (h + 6) * ts + w + 8];
                for (i, v) in t.iter_mut().enumerate() {
                    *v = match pattern {
                        0 => 255,
                        1 => if (i / ts) % 2 == 0 { 255 } else { 0 },
                        _ => if (i % ts) % 2 == 0 { 255 } else { 0 },
                    };
                }
                let (mut a, mut b) = (vec![0u8; w * h], vec![0u8; w * h]);
                mc_centre(&t, ts, &mut a, w, h);
                centre_scalar(&t, ts, &mut b, w, h);
                assert_eq!(a, b, "centre saturation w={w} pattern={pattern}");
            }
        }
    }

    #[test]
    fn pixel_avg_matches_scalar() {
        for &w in &[4usize, 8, 16] {
            for &h in &[2usize, 4, 8, 16] {
                let (sa, sb) = (w + 7, w + 3);
                let mut a = vec![0u8; h * sa + 16];
                let mut b = vec![0u8; h * sb + 16];
                fill(&mut a, 0x11);
                fill(&mut b, 0x22);
                let (mut x, mut y) = (vec![0u8; w * h], vec![0u8; w * h]);
                pixel_avg(&mut x, &a, sa, &b, sb, w, h);
                pixel_avg_scalar(&mut y, &a, sa, &b, sb, w, h);
                assert_eq!(x, y, "pixel_avg w={w} h={h}");
            }
        }
    }
}
