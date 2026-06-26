//! Optional hand-tuned x86 assembly acceleration using **openh264's BSD-2 kernels**.
//!
//! This crate is deliberately **not** `#![forbid(unsafe_code)]`: it links and calls
//! hand-written assembly through FFI. It is the opt-in "speed over the pure-safe-Rust
//! guarantee" path — the rest of the codec stays `forbid(unsafe)` and falls back to
//! the scalar/`wide` implementations when this crate is not enabled.
//!
//! openh264 asm is BSD-2 licensed; attribution lives in `openh264/LICENSE`.
#![allow(non_snake_case)]

extern "C" {
    fn WelsSampleSatd4x4_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSatd8x8_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSatd16x8_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSatd8x16_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSatd16x16_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSad16x16_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSad16x8_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsSampleSad8x16_sse2(p1: *const u8, s1: i32, p2: *const u8, s2: i32) -> i32;
    fn WelsQuantFour4x4_sse2(p_dct: *mut i16, p_ff: *const i16, p_mf: *const i16);
    fn DeblockLumaLt4V_ssse3(pix: *mut u8, stride: i32, alpha: i32, beta: i32, tc: *const i8);
    fn DeblockLumaEq4V_ssse3(pix: *mut u8, stride: i32, alpha: i32, beta: i32);
    fn DeblockChromaLt4V_ssse3(cb: *mut u8, cr: *mut u8, stride: i32, alpha: i32, beta: i32, tc: *const i8);
    fn DeblockChromaEq4V_ssse3(cb: *mut u8, cr: *mut u8, stride: i32, alpha: i32, beta: i32);
    fn DeblockChromaLt4H_ssse3(cb: *mut u8, cr: *mut u8, stride: i32, alpha: i32, beta: i32, tc: *const i8);
    fn DeblockChromaEq4H_ssse3(cb: *mut u8, cr: *mut u8, stride: i32, alpha: i32, beta: i32);
    fn DeblockLumaTransposeH2V_sse2(pix: *const u8, stride: i32, dst: *mut u8);
    fn DeblockLumaTransposeV2H_sse2(pix: *mut u8, stride: i32, src: *const u8);
    fn WelsI16x16LumaPredV_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredH_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredDc_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredPlane_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsDctFourT4_sse2(p_dct: *mut i16, p1: *const u8, s1: i32, p2: *const u8, s2: i32);
    fn WelsIDctFourT4Rec_sse2(
        p_rec: *mut u8,
        stride: i32,
        p_pred: *const u8,
        pred_stride: i32,
        p_dct: *const i16,
    );
}

/// SAD of a 16×16 luma block against another via openh264's SSE2 `psadbw` kernel.
/// Trivially bit-identical to `Σ|a−b|`. `stride*` in samples.
#[inline]
pub fn sad_16x16(pix1: &[u8], stride1: usize, pix2: &[u8], stride2: usize) -> i32 {
    assert!(pix1.len() >= 15 * stride1 + 16 && pix2.len() >= 15 * stride2 + 16);
    // SAFETY: bounds asserted; pure function reading two 16×16 blocks at the strides.
    unsafe { WelsSampleSad16x16_sse2(pix1.as_ptr(), stride1 as i32, pix2.as_ptr(), stride2 as i32) }
}

/// SAD of a 16×8 luma block. Bit-identical to `Σ|a−b|`.
#[inline]
pub fn sad_16x8(pix1: &[u8], stride1: usize, pix2: &[u8], stride2: usize) -> i32 {
    assert!(pix1.len() >= 7 * stride1 + 16 && pix2.len() >= 7 * stride2 + 16);
    // SAFETY: bounds asserted; pure function reading two 16×8 blocks.
    unsafe { WelsSampleSad16x8_sse2(pix1.as_ptr(), stride1 as i32, pix2.as_ptr(), stride2 as i32) }
}

/// SAD of an 8×16 luma block. Bit-identical to `Σ|a−b|`.
#[inline]
pub fn sad_8x16(pix1: &[u8], stride1: usize, pix2: &[u8], stride2: usize) -> i32 {
    assert!(pix1.len() >= 15 * stride1 + 8 && pix2.len() >= 15 * stride2 + 8);
    // SAFETY: bounds asserted; pure function reading two 8×16 blocks.
    unsafe { WelsSampleSad8x16_sse2(pix1.as_ptr(), stride1 as i32, pix2.as_ptr(), stride2 as i32) }
}

macro_rules! satd_wrapper {
    ($name:ident, $sym:ident, $w:expr, $h:expr) => {
        #[doc = concat!("SATD of a ", stringify!($w), "×", stringify!($h),
            " block pair via openh264's SSE2 Hadamard kernel. Bit-identical to the sum of the constituent 4×4 SATDs.")]
        #[inline]
        pub fn $name(pix1: &[u8], stride1: usize, pix2: &[u8], stride2: usize) -> i32 {
            assert!(pix1.len() >= ($h - 1) * stride1 + $w && pix2.len() >= ($h - 1) * stride2 + $w);
            // SAFETY: bounds asserted; pure function reading two blocks at the strides.
            unsafe { $sym(pix1.as_ptr(), stride1 as i32, pix2.as_ptr(), stride2 as i32) }
        }
    };
}
satd_wrapper!(satd_8x8, WelsSampleSatd8x8_sse2, 8, 8);
satd_wrapper!(satd_16x8, WelsSampleSatd16x8_sse2, 16, 8);
satd_wrapper!(satd_8x16, WelsSampleSatd8x16_sse2, 8, 16);
satd_wrapper!(satd_16x16, WelsSampleSatd16x16_sse2, 16, 16);

/// In-place loop filter of a **horizontal luma edge** (`bS < 4`) via openh264's
/// `DeblockLumaLt4V_ssse3`. The "V" filter direction is *vertical* (`p0 = pPix[-stride]`),
/// applied across a horizontal edge's 16 columns; `tc[i]` per 4-column segment (`−1`
/// = skip). `p3` starts at `p3` = 4 rows above `q0` (same column); `pPix = q0 = +4·stride`.
/// Bit-identical to the spec filter (our `filter_luma_line`).
#[inline]
pub fn deblock_luma_lt4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(p3.len() >= 7 * stride + 16);
    // SAFETY: bounds asserted; pPixY = p3 + 4·stride = q0; the kernel reads/writes rows
    // [−4,3]·stride over 16 columns, all within `p3`.
    unsafe {
        DeblockLumaLt4V_ssse3(p3.as_mut_ptr().add(4 * stride), stride as i32, alpha, beta, tc.as_ptr())
    }
}

/// In-place loop filter of a **horizontal luma edge** (`bS == 4`, strong) via openh264's
/// `DeblockLumaEq4V_ssse3`. `p3` as in [`deblock_luma_lt4_v`].
#[inline]
pub fn deblock_luma_eq4_v(p3: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(p3.len() >= 7 * stride + 16);
    // SAFETY: bounds asserted; pPixY = p3 + 4·stride = q0; rows [−4,3]·stride × 16 cols.
    unsafe { DeblockLumaEq4V_ssse3(p3.as_mut_ptr().add(4 * stride), stride as i32, alpha, beta) }
}

/// In-place loop filter of a **vertical luma edge** (`bS < 4`) via transpose →
/// `DeblockLumaLt4V` → transpose-back (openh264's `DeblockLumaLt4H` C wrapper). `p4`
/// starts at `p3` = column `x−4` of the top row; the kernels transpose the 16×8 region,
/// filter the now-horizontal edge, and write back. Bit-identical to our spec filter.
#[inline]
pub fn deblock_luma_lt4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(p4.len() >= 15 * stride + 8);
    #[repr(align(16))]
    struct Buf([u8; 128]);
    let mut buf = Buf([0; 128]);
    // SAFETY: bounds asserted; p4 = pPixY−4. Transpose reads 16×8 from p4 into the
    // aligned buf, filters, transposes back into p4. All within `p4`.
    unsafe {
        DeblockLumaTransposeH2V_sse2(p4.as_ptr(), stride as i32, buf.0.as_mut_ptr());
        DeblockLumaLt4V_ssse3(buf.0.as_mut_ptr().add(4 * 16), 16, alpha, beta, tc.as_ptr());
        DeblockLumaTransposeV2H_sse2(p4.as_mut_ptr(), stride as i32, buf.0.as_ptr());
    }
}

/// In-place loop filter of a **vertical luma edge** (`bS == 4`, strong) via transpose →
/// `DeblockLumaEq4V` → transpose-back. `p4` as in [`deblock_luma_lt4_h`].
#[inline]
pub fn deblock_luma_eq4_h(p4: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(p4.len() >= 15 * stride + 8);
    #[repr(align(16))]
    struct Buf([u8; 128]);
    let mut buf = Buf([0; 128]);
    // SAFETY: as in deblock_luma_lt4_h.
    unsafe {
        DeblockLumaTransposeH2V_sse2(p4.as_ptr(), stride as i32, buf.0.as_mut_ptr());
        DeblockLumaEq4V_ssse3(buf.0.as_mut_ptr().add(4 * 16), 16, alpha, beta);
        DeblockLumaTransposeV2H_sse2(p4.as_mut_ptr(), stride as i32, buf.0.as_ptr());
    }
}

/// 16×16 luma intra prediction into `pred` (must be 16-aligned, ≥256 bytes) via
/// openh264's `WelsI16x16LumaPred{V,H,Dc,Plane}_sse2`. `rec[base]` = MB top-left; the
/// kernel reads the top row (`rec[base−stride+i]`) and/or left col (`rec[base−1+i·stride]`)
/// and writes the 16×16 prediction. `mode`: 0=V, 1=H, 2=DC, 3=Plane — caller ensures the
/// required neighbors exist (both for DC/Plane). Bit-identical to the spec predictor.
#[inline]
pub fn i16x16_luma_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 256 && pred.as_ptr() as usize % 16 == 0);
    assert!(base >= stride + 1 && base + 15 * stride <= rec.len());
    let s = stride as i32;
    // SAFETY: pred 16-aligned ≥256; rec[base] + its neighbors asserted in-bounds.
    unsafe {
        let p = pred.as_mut_ptr();
        let r = rec.as_ptr().add(base);
        match mode {
            0 => WelsI16x16LumaPredV_sse2(p, r, s),
            1 => WelsI16x16LumaPredH_sse2(p, r, s),
            2 => WelsI16x16LumaPredDc_sse2(p, r, s),
            _ => WelsI16x16LumaPredPlane_sse2(p, r, s),
        }
    }
}

/// Chroma loop filter of a **horizontal edge** (`bS < 4`), Cb+Cr together, via
/// `DeblockChromaLt4V_ssse3` (p/q vertical). `*_p1` start at `p1` = 2 rows above `q0`;
/// `pPix = p1 + 2·stride`. `tc[i]` per 2-sample segment (the spec chroma `tc0+1`; `0`
/// = skip). Bit-identical to our `filter_chroma_line`.
#[inline]
pub fn deblock_chroma_lt4_v(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(cb_p1.len() >= 3 * stride + 8 && cr_p1.len() >= 3 * stride + 8);
    // SAFETY: bounds asserted; pPix = p1 + 2·stride = q0; reads rows [−2,1]·stride × 8 cols.
    unsafe {
        DeblockChromaLt4V_ssse3(cb_p1.as_mut_ptr().add(2 * stride), cr_p1.as_mut_ptr().add(2 * stride), stride as i32, alpha, beta, tc.as_ptr())
    }
}

/// Chroma strong filter (`bS == 4`) of a **horizontal edge**, Cb+Cr, via `DeblockChromaEq4V_ssse3`.
#[inline]
pub fn deblock_chroma_eq4_v(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(cb_p1.len() >= 3 * stride + 8 && cr_p1.len() >= 3 * stride + 8);
    // SAFETY: bounds asserted; pPix = p1 + 2·stride = q0.
    unsafe {
        DeblockChromaEq4V_ssse3(cb_p1.as_mut_ptr().add(2 * stride), cr_p1.as_mut_ptr().add(2 * stride), stride as i32, alpha, beta)
    }
}

/// Chroma loop filter of a **vertical edge** (`bS < 4`), Cb+Cr, via `DeblockChromaLt4H_ssse3`
/// (p/q horizontal). `*_p1` start at `p1` = 2 cols left of `q0`; `pPix = p1 + 2`. `tc` as `_v`.
#[inline]
pub fn deblock_chroma_lt4_h(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32, tc: &[i8; 4]) {
    assert!(cb_p1.len() >= 7 * stride + 4 && cr_p1.len() >= 7 * stride + 4);
    // SAFETY: bounds asserted; pPix = p1 + 2 = q0; reads cols [−2,1] over 8 rows.
    unsafe {
        DeblockChromaLt4H_ssse3(cb_p1.as_mut_ptr().add(2), cr_p1.as_mut_ptr().add(2), stride as i32, alpha, beta, tc.as_ptr())
    }
}

/// Chroma strong filter (`bS == 4`) of a **vertical edge**, Cb+Cr, via `DeblockChromaEq4H_ssse3`.
#[inline]
pub fn deblock_chroma_eq4_h(cb_p1: &mut [u8], cr_p1: &mut [u8], stride: usize, alpha: i32, beta: i32) {
    assert!(cb_p1.len() >= 7 * stride + 4 && cr_p1.len() >= 7 * stride + 4);
    // SAFETY: bounds asserted; pPix = p1 + 2 = q0.
    unsafe {
        DeblockChromaEq4H_ssse3(cb_p1.as_mut_ptr().add(2), cr_p1.as_mut_ptr().add(2), stride as i32, alpha, beta)
    }
}

/// In-place quantization of **four** 4×4 DCT-coefficient blocks (64 `i16`) via
/// openh264's `WelsQuantFour4x4_sse2`: `level = sign·(((|c| + FF)·MF) >> 16)` with
/// the per-position `FF`/`MF` tables (8 entries each, reused for both halves).
/// NOTE: this is openh264's quantizer (deadzone added *before* the multiply, fixed
/// `>>16`), structurally different from our `(|c|·MF + F) >> qbits` — so it is NOT
/// bit-identical to our `quantize`. Exposed for the kernel ranking + an
/// openh264-semantics path; `dct` must be 16-byte aligned.
#[inline]
pub fn quant_four_4x4(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
    assert!(dct.len() >= 64);
    // The kernel `movdqa`-loads FF/MF, so they must be 16-byte aligned; copy them into
    // aligned locals (16 bytes each, cheap) so callers need only align `dct`.
    #[repr(align(16))]
    struct A([i16; 8]);
    let (ffa, mfa) = (A(*ff), A(*mf));
    // SAFETY: bounds asserted; `dct` is the caller's aligned 64-i16 buffer; FF/MF are
    // aligned here. The kernel reads/writes exactly 64 i16 + 8+8 table entries.
    unsafe { WelsQuantFour4x4_sse2(dct.as_mut_ptr(), ffa.0.as_ptr(), mfa.0.as_ptr()) }
}

/// Inverse 4×4 core DCT + add prediction + clip, over an **8×8 region** (four
/// blocks), via openh264's `WelsIDctFourT4Rec_sse2`. `dct` holds the 64
/// **dequantized** coefficients (blocks in `(0,0),(4,0),(0,4),(4,4)` order). The
/// inverse butterfly + `(x+32)>>6` is bit-identical to our `inverse_core` /
/// `reconstruct_4x4`, so the reconstruction is byte-for-byte ours.
#[inline]
pub fn idct_four_t4_rec(
    rec: &mut [u8],
    stride_rec: usize,
    pred: &[u8],
    stride_pred: usize,
    dct: &[i16],
) {
    assert!(dct.len() >= 64);
    assert!(rec.len() >= 7 * stride_rec + 8);
    assert!(pred.len() >= 7 * stride_pred + 8);
    // SAFETY: bounds asserted; the kernel reads 64 i16 + an 8×8 pred region and
    // writes an 8×8 reconstruction region at the given strides.
    unsafe {
        WelsIDctFourT4Rec_sse2(
            rec.as_mut_ptr(),
            stride_rec as i32,
            pred.as_ptr(),
            stride_pred as i32,
            dct.as_ptr(),
        );
    }
}

/// Forward 4×4 core DCT of an **8×8 region** (four 4×4 blocks) of the residual
/// `src - pred`, via openh264's `WelsDctFourT4_sse2`. Writes 64 `i16` coefficients
/// to `dct`: blocks in `(0,0),(4,0),(0,4),(4,4)` order, raster within each block.
/// The integer core transform is bit-identical to our scalar `forward_core`
/// (`out0=s0+s1, out1=2·s3+s2, out2=s0-s1, out3=s3-2·s2`), so quantizing these
/// coefficients yields identical levels — a pure speedup, byte-for-byte.
#[inline]
pub fn dct_four_t4(dct: &mut [i16], src: &[u8], stride_src: usize, pred: &[u8], stride_pred: usize) {
    assert!(dct.len() >= 64);
    assert!(src.len() >= 7 * stride_src + 8);
    assert!(pred.len() >= 7 * stride_pred + 8);
    // SAFETY: bounds asserted; the kernel reads an 8×8 region from each plane at
    // the given strides and writes exactly 64 i16.
    unsafe {
        WelsDctFourT4_sse2(
            dct.as_mut_ptr(),
            src.as_ptr(),
            stride_src as i32,
            pred.as_ptr(),
            stride_pred as i32,
        );
    }
}

/// SATD (sum of absolute Hadamard-transformed differences) of two 4×4 blocks via
/// openh264's SSE2 kernel. `stride*` are in samples (bytes). Bit-identical to
/// openh264's `WelsSampleSatd4x4_c` (`(Σ|H·d| + 1) >> 1`).
#[inline]
pub fn satd_4x4(pix1: &[u8], stride1: usize, pix2: &[u8], stride2: usize) -> i32 {
    assert!(pix1.len() >= 3 * stride1 + 4 && pix2.len() >= 3 * stride2 + 4);
    // SAFETY: bounds asserted above; the kernel is a pure function that reads a
    // 4×4 block from each pointer at the given stride.
    unsafe { WelsSampleSatd4x4_sse2(pix1.as_ptr(), stride1 as i32, pix2.as_ptr(), stride2 as i32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of openh264 `WelsSampleSatd4x4_c` — the exact reference the asm matches.
    fn satd_ref(a: &[u8], sa: usize, b: &[u8], sb: usize) -> i32 {
        let mut m = [[0i32; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                m[i][j] = a[i * sa + j] as i32 - b[i * sb + j] as i32;
            }
        }
        for row in m.iter_mut() {
            let (s0, s1, s2, s3) =
                (row[0] + row[2], row[1] + row[3], row[0] - row[2], row[1] - row[3]);
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

    /// Our scalar `forward_core` butterfly (spec / openh264 `WelsDctT4_c`), on a
    /// 4×4 residual block in raster order.
    fn forward_core(b: &[i32; 16]) -> [i32; 16] {
        let f = |x0: i32, x1: i32, x2: i32, x3: i32| {
            let (t0, t1, t2, t3) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
            (t0 + t1, 2 * t3 + t2, t0 - t1, t3 - 2 * t2)
        };
        let mut m = *b;
        for r in 0..4 {
            let (a, c, d, e) = f(m[r * 4], m[r * 4 + 1], m[r * 4 + 2], m[r * 4 + 3]);
            m[r * 4] = a;
            m[r * 4 + 1] = c;
            m[r * 4 + 2] = d;
            m[r * 4 + 3] = e;
        }
        for c in 0..4 {
            let (a, b2, d, e) = f(m[c], m[4 + c], m[8 + c], m[12 + c]);
            m[c] = a;
            m[4 + c] = b2;
            m[8 + c] = d;
            m[12 + c] = e;
        }
        m
    }

    #[test]
    fn dct_four_t4_matches_forward_core() {
        // 8×8 source + prediction tiles (stride 8 for the test).
        for seed in 0..128usize {
            let mut src = [0u8; 64];
            let mut pred = [0u8; 64];
            for y in 0..8 {
                for x in 0..8 {
                    src[y * 8 + x] = ((y * 31 + x * 17 + seed * 7) & 0xff) as u8;
                    pred[y * 8 + x] = ((y * 13 + x * 41 + seed * 5 + 9) & 0xff) as u8;
                }
            }
            let mut dct = [0i16; 64];
            dct_four_t4(&mut dct, &src, 8, &pred, 8);
            // Reference: the four 4×4 sub-blocks at (bx,by) px-units (0,0),(4,0),(0,4),(4,4).
            for (k, (ox, oy)) in [(0, 0), (4, 0), (0, 4), (4, 4)].iter().enumerate() {
                let mut res = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        res[dy * 4 + dx] =
                            src[(oy + dy) * 8 + ox + dx] as i32 - pred[(oy + dy) * 8 + ox + dx] as i32;
                    }
                }
                let want = forward_core(&res);
                for i in 0..16 {
                    assert_eq!(
                        dct[k * 16 + i] as i32,
                        want[i],
                        "seed {seed} block {k} coeff {i}"
                    );
                }
            }
        }
    }

    /// Port of openh264 `WelsIDctT4Rec_c` (== our `inverse_core` + add pred + clip),
    /// on one 4×4 block of dequantized coeffs.
    fn idct_rec_block(pred: &[i32; 16], dct: &[i32; 16]) -> [u8; 16] {
        let mut t = [0i32; 16];
        for i in 0..4 {
            let d = &dct[i * 4..i * 4 + 4];
            let (su, de) = (d[0] + d[2], d[0] - d[2]);
            let (sd, dd) = (d[1] + (d[3] >> 1), (d[1] >> 1) - d[3]);
            t[i * 4] = su + sd;
            t[i * 4 + 1] = de + dd;
            t[i * 4 + 2] = de - dd;
            t[i * 4 + 3] = su - sd;
        }
        let mut out = [0u8; 16];
        for i in 0..4 {
            let (sl, dl) = (t[i] + t[8 + i], t[i] - t[8 + i]);
            let (dr, sr) = ((t[4 + i] >> 1) - t[12 + i], t[4 + i] + (t[12 + i] >> 1));
            let r = [sl + sr, dl + dr, dl - dr, sl - sr];
            for k in 0..4 {
                out[k * 4 + i] = (pred[k * 4 + i] + ((r[k] + 32) >> 6)).clamp(0, 255) as u8;
            }
        }
        out
    }

    #[repr(align(16))]
    struct Align16<T>(T);

    #[test]
    fn idct_four_t4_rec_matches_scalar() {
        for seed in 0..128usize {
            let mut pred = [0u8; 64];
            // dct coeffs must be 16-byte aligned (the kernel uses movdqa loads).
            let mut dctw = Align16([0i16; 64]);
            for i in 0..64 {
                pred[i] = ((i * 7 + seed * 3) & 0xff) as u8;
                // dequantized-coeff-like values (signed, modest magnitude)
                dctw.0[i] = (((i as i32 * 53 + seed as i32 * 29) % 4096) - 2048) as i16;
            }
            let dct = &dctw.0;
            let mut rec = [0u8; 64];
            idct_four_t4_rec(&mut rec, 8, &pred, 8, dct);
            // Reference: 4 sub-blocks at (0,0),(4,0),(0,4),(4,4).
            for (k, (ox, oy)) in [(0, 0), (4, 0), (0, 4), (4, 4)].iter().enumerate() {
                let mut pb = [0i32; 16];
                let mut db = [0i32; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        pb[dy * 4 + dx] = pred[(oy + dy) * 8 + ox + dx] as i32;
                        db[dy * 4 + dx] = dct[k * 16 + dy * 4 + dx] as i32;
                    }
                }
                let want = idct_rec_block(&pb, &db);
                for dy in 0..4 {
                    for dx in 0..4 {
                        assert_eq!(
                            rec[(oy + dy) * 8 + ox + dx],
                            want[dy * 4 + dx],
                            "seed {seed} block {k} ({dx},{dy})"
                        );
                    }
                }
            }
        }
    }

    fn sad_ref(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
        let mut s = 0i32;
        for i in 0..h {
            for j in 0..w {
                s += (a[i * sa + j] as i32 - b[i * sb + j] as i32).abs();
            }
        }
        s
    }
    // openh264's NxM SATD = sum of the constituent 4×4 SATDs (each `(Σ|H·d|+1)>>1`).
    fn satd_region_ref(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> i32 {
        let mut s = 0i32;
        let mut by = 0;
        while by < h {
            let mut bx = 0;
            while bx < w {
                s += satd_ref(&a[by * sa + bx..], sa, &b[by * sb + bx..], sb);
                bx += 4;
            }
            by += 4;
        }
        s
    }

    #[test]
    fn sad_satd_family_matches_reference() {
        // 16-byte aligned, stride-16 tiles — the SSE2 SAD/SATD kernels use aligned
        // (movdqa) loads, so input must be 16-aligned with a 16-multiple stride
        // (which the encoder's planes are, at 16-aligned MB offsets).
        let (sa, sb) = (16usize, 16usize);
        let mut aw = Align16([0u8; 16 * 16]);
        let mut bw = Align16([0u8; 16 * 16]);
        for seed in 0..96usize {
            for i in 0..16 {
                for j in 0..16 {
                    aw.0[i * sa + j] = ((i * 37 + j * 101 + seed * 3) & 0xff) as u8;
                    bw.0[i * sb + j] = ((i * 53 + j * 17 + seed * 29 + 7) & 0xff) as u8;
                }
            }
            let (a, b): (&[u8], &[u8]) = (&aw.0, &bw.0);
            assert_eq!(sad_16x16(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 16, 16), "sad16x16 {seed}");
            assert_eq!(sad_16x8(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 16, 8), "sad16x8 {seed}");
            assert_eq!(sad_8x16(a, sa, b, sb), sad_ref(&a, sa, &b, sb, 8, 16), "sad8x16 {seed}");
            assert_eq!(satd_8x8(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 8, 8), "satd8x8 {seed}");
            assert_eq!(satd_16x8(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 16, 8), "satd16x8 {seed}");
            assert_eq!(satd_8x16(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 8, 16), "satd8x16 {seed}");
            assert_eq!(satd_16x16(a, sa, b, sb), satd_region_ref(&a, sa, &b, sb, 16, 16), "satd16x16 {seed}");
        }
    }

    #[test]
    fn quant_four_matches_openh264_c() {
        // openh264 WELS_NEW_QUANT: level = sign(c) * ((|c| + FF[pos]) * MF[pos]) >> 16,
        // pos = (row&1)*4 + col within each 4x4 block.
        #[repr(align(16))]
        struct A16i([i16; 64]);
        let ff: [i16; 8] = [80, 85, 80, 85, 90, 95, 90, 95];
        let mf: [i16; 8] = [410, 420, 410, 420, 430, 440, 430, 440];
        for seed in 0..64i32 {
            let mut input = [0i16; 64];
            for (k, v) in input.iter_mut().enumerate() {
                *v = (((k as i32 * 37 + seed * 53) % 2000) - 1000) as i16;
            }
            let mut dctw = A16i(input);
            quant_four_4x4(&mut dctw.0, &ff, &mf);
            for blk in 0..4 {
                for row in 0..4 {
                    for col in 0..4 {
                        let idx = blk * 16 + row * 4 + col;
                        let pos = (row & 1) * 4 + col;
                        let c = input[idx] as i32;
                        let lvl = ((c.abs() + ff[pos] as i32) * mf[pos] as i32) >> 16;
                        let want = (if c < 0 { -lvl } else { lvl }) as i16;
                        assert_eq!(dctw.0[idx], want, "seed {seed} blk {blk} ({row},{col})");
                    }
                }
            }
        }
    }

    #[test]
    fn satd_matches_openh264_c_reference() {
        for seed in 0..256u32 {
            let mut a = [0u8; 64];
            let mut b = [0u8; 64];
            for i in 0..4 {
                for j in 0..4 {
                    let s = seed as usize;
                    a[i * 16 + j] = ((i * 37 + j * 101 + s * 3) & 0xff) as u8;
                    b[i * 16 + j] = ((i * 53 + j * 17 + s * 29 + 7) & 0xff) as u8;
                }
            }
            let got = satd_4x4(&a, 16, &b, 16);
            let want = satd_ref(&a, 16, &b, 16);
            assert_eq!(got, want, "seed {seed}: asm {got} != openh264-C ref {want}");
        }
    }
}
