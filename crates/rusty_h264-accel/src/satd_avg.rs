//! Challenge-1 A3: fused (a+b+1)>>1 average + SATD — a **custom Rust-intrinsics
//! kernel** (not vendored; openh264 ships nothing of this shape).
//!
//! The ME's quarter-pel candidates are predicted as the rounded average of two
//! half-pel planes. The old path materialized that average into a 256-byte temp
//! (`avg_rows`), then re-loaded it inside the SATD kernel — a store/reload round
//! trip plus an FFI call per candidate, at ~40% of all sub-pel evaluations. This
//! kernel computes `Σ|H·(src − avg(a,b))|` in one register pass: `vpavgb` produces
//! the exact `(a+b+1)>>1` the spec requires, and the 4×4 Hadamard runs in 16-bit
//! lanes (max |2-D coeff| = 16·255 = 4080 < i16::MAX, so no overflow is possible).
//!
//! **Byte-identity argument:** the butterfly network below computes a row-permuted
//! Sylvester H4 (`[1,1,1,1],[1,-1,1,-1],[1,1,-1,-1],[1,-1,-1,1]` vs the scalar
//! `hadamard_1d`'s `[1,1,1,1],[1,1,-1,-1],[1,-1,-1,1],[1,-1,1,-1]`). For any row
//! permutation P, `(PH)D(PH)ᵀ = P(HDHᵀ)Pᵀ` — the same coefficient multiset — so
//! `Σ|·|` is EXACTLY equal, in integer arithmetic, for every input. The
//! `satd_avg_matches_materialized_scalar` test pins this empirically as well.

use core::arch::x86_64::*;

/// One 4-row×16-col band: four 4×4 blocks side by side, diffs already in i16 lanes.
/// Adds each block's `Σ|H·d|` into `acc`'s eight i32 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hadamard4_abs_acc(
    d0: __m256i,
    d1: __m256i,
    d2: __m256i,
    d3: __m256i,
    acc: __m256i,
) -> __m256i {
    // Vertical (across the four rows), lane-wise.
    let t0 = _mm256_add_epi16(d0, d1);
    let t1 = _mm256_sub_epi16(d0, d1);
    let t2 = _mm256_add_epi16(d2, d3);
    let t3 = _mm256_sub_epi16(d2, d3);
    let m0 = _mm256_add_epi16(t0, t2);
    let m1 = _mm256_add_epi16(t1, t3);
    let m2 = _mm256_sub_epi16(t0, t2);
    let m3 = _mm256_sub_epi16(t1, t3);
    // Transpose each 4×4 group (all unpacks are 128-lane-local, so the two lanes'
    // block pairs stay independent).
    let u0 = _mm256_unpacklo_epi16(m0, m1);
    let u1 = _mm256_unpackhi_epi16(m0, m1);
    let v0 = _mm256_unpacklo_epi16(m2, m3);
    let v1 = _mm256_unpackhi_epi16(m2, m3);
    let w0 = _mm256_unpacklo_epi32(u0, v0);
    let w1 = _mm256_unpackhi_epi32(u0, v0);
    let w2 = _mm256_unpacklo_epi32(u1, v1);
    let w3 = _mm256_unpackhi_epi32(u1, v1);
    let r0 = _mm256_unpacklo_epi64(w0, w2);
    let r1 = _mm256_unpackhi_epi64(w0, w2);
    let r2 = _mm256_unpacklo_epi64(w1, w3);
    let r3 = _mm256_unpackhi_epi64(w1, w3);
    // Horizontal (across the four columns, now rows post-transpose), lane-wise.
    let s0 = _mm256_add_epi16(r0, r1);
    let s1 = _mm256_sub_epi16(r0, r1);
    let s2 = _mm256_add_epi16(r2, r3);
    let s3 = _mm256_sub_epi16(r2, r3);
    let f0 = _mm256_add_epi16(s0, s2);
    let f1 = _mm256_add_epi16(s1, s3);
    let f2 = _mm256_sub_epi16(s0, s2);
    let f3 = _mm256_sub_epi16(s1, s3);
    // Σ|coeff| into i32 lanes: |f| ≤ 4080, pairs ≤ 8160 — well inside i32.
    let ones = _mm256_set1_epi16(1);
    let mut a = acc;
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f0), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f1), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f2), ones));
    a = _mm256_add_epi32(a, _mm256_madd_epi16(_mm256_abs_epi16(f3), ones));
    a
}

/// src row minus avg row, 16 columns, as 16 i16 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn diff_row16(src: *const u8, a: *const u8, b: *const u8) -> __m256i {
    let s = _mm256_cvtepu8_epi16(_mm_loadu_si128(src as *const __m128i));
    let av = _mm_avg_epu8(
        _mm_loadu_si128(a as *const __m128i),
        _mm_loadu_si128(b as *const __m128i),
    );
    _mm256_sub_epi16(s, _mm256_cvtepu8_epi16(av))
}

/// Rows `r` and `r+4` of an 8-wide block packed as [row r | row r+4] in i16 lanes.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn diff_row8x2(
    src: *const u8,
    a: *const u8,
    b: *const u8,
    ss: usize,
    st: usize,
) -> __m256i {
    let s = _mm_unpacklo_epi64(
        _mm_loadl_epi64(src as *const __m128i),
        _mm_loadl_epi64(src.add(4 * ss) as *const __m128i),
    );
    let pa = _mm_unpacklo_epi64(
        _mm_loadl_epi64(a as *const __m128i),
        _mm_loadl_epi64(a.add(4 * st) as *const __m128i),
    );
    let pb = _mm_unpacklo_epi64(
        _mm_loadl_epi64(b as *const __m128i),
        _mm_loadl_epi64(b.add(4 * st) as *const __m128i),
    );
    _mm256_sub_epi16(
        _mm256_cvtepu8_epi16(s),
        _mm256_cvtepu8_epi16(_mm_avg_epu8(pa, pb)),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum_epi32(v: __m256i) -> u32 {
    let s = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b0100_1110));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b1011_0001));
    _mm_cvtsi128_si32(s) as u32
}

/// 16-wide (16×16 / 16×8): one 4-row band per iteration, four blocks per band.
#[target_feature(enable = "avx2")]
unsafe fn satd_avg_w16(
    src: *const u8,
    ss: usize,
    a: *const u8,
    b: *const u8,
    st: usize,
    h: usize,
) -> u32 {
    let mut acc = _mm256_setzero_si256();
    let mut r = 0;
    while r < h {
        let d0 = diff_row16(src.add(r * ss), a.add(r * st), b.add(r * st));
        let d1 = diff_row16(src.add((r + 1) * ss), a.add((r + 1) * st), b.add((r + 1) * st));
        let d2 = diff_row16(src.add((r + 2) * ss), a.add((r + 2) * st), b.add((r + 2) * st));
        let d3 = diff_row16(src.add((r + 3) * ss), a.add((r + 3) * st), b.add((r + 3) * st));
        acc = hadamard4_abs_acc(d0, d1, d2, d3, acc);
        r += 4;
    }
    hsum_epi32(acc)
}

/// 8-wide (8×16 / 8×8): one 8-row band per iteration — rows r and r+4 share a
/// vector's two 128-lanes, so the same four-block band kernel applies.
#[target_feature(enable = "avx2")]
unsafe fn satd_avg_w8(
    src: *const u8,
    ss: usize,
    a: *const u8,
    b: *const u8,
    st: usize,
    h: usize,
) -> u32 {
    let mut acc = _mm256_setzero_si256();
    let mut r = 0;
    while r < h {
        let d0 = diff_row8x2(src.add(r * ss), a.add(r * st), b.add(r * st), ss, st);
        let d1 = diff_row8x2(src.add((r + 1) * ss), a.add((r + 1) * st), b.add((r + 1) * st), ss, st);
        let d2 = diff_row8x2(src.add((r + 2) * ss), a.add((r + 2) * st), b.add((r + 2) * st), ss, st);
        let d3 = diff_row8x2(src.add((r + 3) * ss), a.add((r + 3) * st), b.add((r + 3) * st), ss, st);
        acc = hadamard4_abs_acc(d0, d1, d2, d3, acc);
        r += 8;
    }
    hsum_epi32(acc)
}

/// Track-B batch kernel: SADs of ONE 16×16 source block against FOUR candidate
/// positions in the same plane (shared stride) — the x264 `sad_x4` shape. Each
/// source row is loaded ONCE and two `vpsadbw` cover all four candidates (vs four
/// separate loads+psadbw), amortizing the loads the flat single-SAD experiments
/// proved dominant.
#[target_feature(enable = "avx2")]
unsafe fn sad_16x16_x4_avx2(
    src: *const u8,
    ss: usize,
    r0: *const u8,
    r1: *const u8,
    r2: *const u8,
    r3: *const u8,
    rs: usize,
) -> [u32; 4] {
    let mut a01 = _mm256_setzero_si256();
    let mut a23 = _mm256_setzero_si256();
    for r in 0..16 {
        let s = _mm_loadu_si128(src.add(r * ss) as *const __m128i);
        let sb = _mm256_set_m128i(s, s);
        let p01 = _mm256_set_m128i(
            _mm_loadu_si128(r1.add(r * rs) as *const __m128i),
            _mm_loadu_si128(r0.add(r * rs) as *const __m128i),
        );
        let p23 = _mm256_set_m128i(
            _mm_loadu_si128(r3.add(r * rs) as *const __m128i),
            _mm_loadu_si128(r2.add(r * rs) as *const __m128i),
        );
        a01 = _mm256_add_epi64(a01, _mm256_sad_epu8(sb, p01));
        a23 = _mm256_add_epi64(a23, _mm256_sad_epu8(sb, p23));
    }
    // Each accumulator holds [q0,q1 | q2,q3] u64: candidate k's row sums live in
    // one 128-lane's two quads.
    let mut buf = [0u64; 4];
    let mut out = [0u32; 4];
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a01);
    out[0] = (buf[0] + buf[1]) as u32;
    out[1] = (buf[2] + buf[3]) as u32;
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, a23);
    out[2] = (buf[0] + buf[1]) as u32;
    out[3] = (buf[2] + buf[3]) as u32;
    out
}

/// The SATD sibling of `sad_16x16_x4`: `Σ|H·d|` of one 16×16 source against FOUR
/// candidate positions in the same plane. The source band is converted to i16
/// ONCE per 4-row band and reused by all four candidates' Hadamards.
#[target_feature(enable = "avx2")]
unsafe fn satd_16x16_x4_avx2(
    src: *const u8,
    ss: usize,
    r: [*const u8; 4],
    rs: usize,
) -> [u32; 4] {
    let mut acc = [_mm256_setzero_si256(); 4];
    let mut row = 0;
    while row < 16 {
        // Source band, loaded once for all four candidates.
        let s0 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(row * ss) as *const __m128i));
        let s1 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 1) * ss) as *const __m128i));
        let s2 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 2) * ss) as *const __m128i));
        let s3 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add((row + 3) * ss) as *const __m128i));
        for k in 0..4 {
            let p = r[k];
            let d0 = _mm256_sub_epi16(
                s0,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(row * rs) as *const __m128i)),
            );
            let d1 = _mm256_sub_epi16(
                s1,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 1) * rs) as *const __m128i)),
            );
            let d2 = _mm256_sub_epi16(
                s2,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 2) * rs) as *const __m128i)),
            );
            let d3 = _mm256_sub_epi16(
                s3,
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add((row + 3) * rs) as *const __m128i)),
            );
            acc[k] = hadamard4_abs_acc(d0, d1, d2, d3, acc[k]);
        }
        row += 4;
    }
    [hsum_epi32(acc[0]), hsum_epi32(acc[1]), hsum_epi32(acc[2]), hsum_epi32(acc[3])]
}

/// Safe wrapper: `Σ|H·d|` SATDs of `src` (16×16, stride `ss`) vs four offsets `o`
/// into `base` (stride `rs`) — the exact scalar-Hadamard value (`satd_px` domain,
/// NOT the `(Σ+1)>>1` the Wels wrappers return). `None` without AVX2.
#[inline]
pub fn satd_16x16_x4(
    src: &[u8],
    ss: usize,
    base: &[u8],
    o: [usize; 4],
    rs: usize,
) -> Option<[u32; 4]> {
    if !crate::has_avx2() {
        return None;
    }
    assert!(src.len() >= 15 * ss + 16);
    for &oi in &o {
        assert!(base.len() >= oi + 15 * rs + 16);
    }
    // SAFETY: AVX2 checked; all row reads inside the asserted bounds.
    unsafe {
        let b = base.as_ptr();
        Some(satd_16x16_x4_avx2(
            src.as_ptr(),
            ss,
            [b.add(o[0]), b.add(o[1]), b.add(o[2]), b.add(o[3])],
            rs,
        ))
    }
}

/// Safe wrapper: SADs of `src` (16×16, stride `ss`) vs four offsets `o` into
/// `base` (stride `rs`). `None` when AVX2 is unavailable — caller runs the scalar
/// per-candidate path. Values are exactly `Σ|a−b|` per candidate.
#[inline]
pub fn sad_16x16_x4(
    src: &[u8],
    ss: usize,
    base: &[u8],
    o: [usize; 4],
    rs: usize,
) -> Option<[u32; 4]> {
    if !crate::has_avx2() {
        return None;
    }
    assert!(src.len() >= 15 * ss + 16);
    for &oi in &o {
        assert!(base.len() >= oi + 15 * rs + 16);
    }
    // SAFETY: AVX2 checked; every row read of all five operands is inside the
    // asserted bounds.
    unsafe {
        Some(sad_16x16_x4_avx2(
            src.as_ptr(),
            ss,
            base.as_ptr().add(o[0]),
            base.as_ptr().add(o[1]),
            base.as_ptr().add(o[2]),
            base.as_ptr().add(o[3]),
            rs,
        ))
    }
}

/// Fused `SATD(src, (a+b+1)>>1)` of a `w`×`h` block — `Σ|H·d|`, the SAME value
/// `satd_px` computes on the materialized average (NOT the `(Σ+1)>>1` the
/// `WelsSampleSatd*` wrappers return). `None` when AVX2 is unavailable or the
/// size is unsupported — the caller then materializes and takes the old path,
/// so a non-AVX2 machine is byte-identical by construction.
#[inline]
pub fn satd_avg(
    src: &[u8],
    src_stride: usize,
    a: &[u8],
    b: &[u8],
    stride: usize,
    w: usize,
    h: usize,
) -> Option<u32> {
    if !crate::has_avx2() || !matches!((w, h), (16, 16) | (16, 8) | (8, 16) | (8, 8)) {
        return None;
    }
    assert!(src.len() >= (h - 1) * src_stride + w);
    assert!(a.len() >= (h - 1) * stride + w && b.len() >= (h - 1) * stride + w);
    // SAFETY: AVX2 checked above; every row read is inside the asserted bounds
    // ((h-1)·stride + w for all three operands; the 8-wide packer's `r+4`/`+4·st`
    // rows are ≤ (h-1) by the loop bound).
    unsafe {
        Some(match w {
            16 => satd_avg_w16(src.as_ptr(), src_stride, a.as_ptr(), b.as_ptr(), stride, h),
            _ => satd_avg_w8(src.as_ptr(), src_stride, a.as_ptr(), b.as_ptr(), stride, h),
        })
    }
}
