//! Optional hand-tuned x86 assembly acceleration using **openh264's BSD-2 kernels**.
//!
//! This crate is deliberately **not** `#![forbid(unsafe_code)]`: it links and calls
//! hand-written assembly through FFI. It is the opt-in "speed over the pure-safe-Rust
//! guarantee" path — the rest of the codec stays `forbid(unsafe)` and falls back to
//! the scalar/`wide` implementations when this crate is not enabled.
//!
//! openh264 asm is BSD-2 licensed; attribution lives in `openh264/LICENSE`.
//!
//! The vendored kernels are **x86-64 only**. On every other architecture this crate
//! compiles to an empty lib (the whole module is gated on `target_arch = "x86_64"`) and
//! callers fall back to the pure-Rust scalar path — selected by the `accel` cfg that the
//! consumer crates' build scripts set only for x86_64 + the `asm` feature. This is what
//! lets a downstream default-features build (e.g. `rff`) succeed on arm64 macOS.
#![allow(non_snake_case)]

#[path = "hpel.rs"]
mod hpel;
#[path = "mectx.rs"]
mod mectx;
#[path = "satd_avg.rs"]
pub(crate) mod satd_avg;
pub use hpel::hpel_fused;
pub use mectx::MeCtx;
pub use satd_avg::{sad_x4, satd_avg, satd_avg_x4, satd_x4, satd_x4p};

extern "C" {
    fn WelsQuantFour4x4_sse2(p_dct: *mut i16, p_ff: *const i16, p_mf: *const i16);
    fn WelsI16x16LumaPredV_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredH_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredDc_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsI16x16LumaPredPlane_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsIChromaPredV_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsIChromaPredPlane_sse2(pred: *mut u8, refp: *const u8, stride: i32);
    fn WelsDctFourT4_sse2(p_dct: *mut i16, p1: *const u8, s1: i32, p2: *const u8, s2: i32);
    fn WelsIDctFourT4Rec_sse2(
        p_rec: *mut u8,
        stride: i32,
        p_pred: *const u8,
        pred_stride: i32,
        p_dct: *const i16,
    );
    // AVX2 twins of the hot arithmetic kernels. openh264 uses these via the same
    // ISA-dispatch function-pointer tables as the `_sse2` ones, so their outputs are
    // bit-identical layouts (256-bit lanes process the same 4×4 blocks in one pass).
    // All access `pDct` with `vmovdqu` (no 32-byte alignment needed) and `vzeroupper`
    // before returning. Selected at runtime by `has_avx2()`; byte-identity is proven
    // by the encoder's full-bitstream `cmp` gate.
    fn WelsDctFourT4_avx2(p_dct: *mut i16, p1: *const u8, s1: i32, p2: *const u8, s2: i32);
    fn WelsIDctFourT4Rec_avx2(
        p_rec: *mut u8,
        stride: i32,
        p_pred: *const u8,
        pred_stride: i32,
        p_dct: *const i16,
    );
    fn WelsQuantFour4x4_avx2(p_dct: *mut i16, p_ff: *const i16, p_mf: *const i16);
}

/// Whether the running CPU supports AVX2 (cached). Gates the AVX2 MC kernels —
/// calling a VEX-encoded kernel on a non-AVX2 CPU would fault.
#[inline]
fn has_avx2() -> bool {
    use std::sync::OnceLock;
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::is_x86_feature_detected!("avx2"))
}









/// 8×8 chroma intra prediction into `pred` (16-aligned, ≥64 bytes) via openh264's
/// `WelsIChromaPred{V,Plane}_sse2`. `rec[base]` = chroma MB top-left; reads the top row /
/// left col from the aligned plane. `mode`: 2=Vertical, 3=Plane (the only modes with sse2;
/// DC/Horizontal are C-only → caller uses scalar). Bit-identical to `chroma8x8_pred`.
#[inline]
pub(crate) fn asm_chroma8x8_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 64 && pred.as_ptr() as usize % 16 == 0);
    assert!(base >= stride + 1 && base + 7 * stride <= rec.len());
    let s = stride as i32;
    // SAFETY: pred 16-aligned ≥64; rec[base] + neighbors asserted in-bounds.
    unsafe {
        let p = pred.as_mut_ptr();
        let r = rec.as_ptr().add(base);
        match mode {
            2 => WelsIChromaPredV_sse2(p, r, s),
            _ => WelsIChromaPredPlane_sse2(p, r, s),
        }
    }
}





/// 16×16 luma intra prediction into `pred` (must be 16-aligned, ≥256 bytes) via
/// openh264's `WelsI16x16LumaPred{V,H,Dc,Plane}_sse2`. `rec[base]` = MB top-left; the
/// kernel reads the top row (`rec[base−stride+i]`) and/or left col (`rec[base−1+i·stride]`)
/// and writes the 16×16 prediction. `mode`: 0=V, 1=H, 2=DC, 3=Plane — caller ensures the
/// required neighbors exist (both for DC/Plane). Bit-identical to the spec predictor.
#[inline]
pub(crate) fn asm_i16x16_luma_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
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





/// In-place quantization of **four** 4×4 DCT-coefficient blocks (64 `i16`) via
/// openh264's `WelsQuantFour4x4_sse2`: `level = sign·(((|c| + FF)·MF) >> 16)` with
/// the per-position `FF`/`MF` tables (8 entries each, reused for both halves).
/// NOTE: this is openh264's quantizer (deadzone added *before* the multiply, fixed
/// `>>16`), structurally different from our `(|c|·MF + F) >> qbits` — so it is NOT
/// bit-identical to our `quantize`. Exposed for the kernel ranking + an
/// openh264-semantics path; `dct` must be 16-byte aligned.
#[inline]
pub(crate) fn asm_quant_four_4x4(dct: &mut [i16], ff: &[i16; 8], mf: &[i16; 8]) {
    assert!(dct.len() >= 64);
    // The kernel `movdqa`-loads FF/MF, so they must be 16-byte aligned; copy them into
    // aligned locals (16 bytes each, cheap) so callers need only align `dct`.
    #[repr(align(16))]
    struct A([i16; 8]);
    let (ffa, mfa) = (A(*ff), A(*mf));
    // SAFETY: bounds asserted; `dct` is the caller's aligned 64-i16 buffer; FF/MF are
    // aligned here. The kernel reads/writes exactly 64 i16 + 8+8 table entries. The
    // AVX2 twin `vmovdqu`s `dct` (no 32B alignment needed) and `vbroadcasti128`s the
    // 8-entry FF/MF into both YMM lanes — same math, bit-identical result.
    unsafe {
        if has_avx2() {
            WelsQuantFour4x4_avx2(dct.as_mut_ptr(), ffa.0.as_ptr(), mfa.0.as_ptr())
        } else {
            WelsQuantFour4x4_sse2(dct.as_mut_ptr(), ffa.0.as_ptr(), mfa.0.as_ptr())
        }
    }
}

/// MEASUREMENT KNOB — see `idct_four_t4_rec`. Read once; inert when unset.
#[inline]
fn abl_recon() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RFF_ABL_RECON").is_some_and(|v| v != "0");
            ON.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Inverse 4×4 core DCT + add prediction + clip, over an **8×8 region** (four
/// blocks), via openh264's `WelsIDctFourT4Rec_sse2`. `dct` holds the 64
/// **dequantized** coefficients (blocks in `(0,0),(4,0),(0,4),(4,4)` order). The
/// inverse butterfly + `(x+32)>>6` is bit-identical to our `inverse_core` /
/// `reconstruct_4x4`, so the reconstruction is byte-for-byte ours.
#[inline]
pub(crate) fn asm_idct_four_t4_rec(
    rec: &mut [u8],
    stride_rec: usize,
    pred: &[u8],
    stride_pred: usize,
    dct: &[i16],
) {
    assert!(dct.len() >= 64);
    assert!(rec.len() >= 7 * stride_rec + 8);
    assert!(pred.len() >= 7 * stride_pred + 8);
    // MEASUREMENT KNOB (`RFF_ABL_RECON=1`): copy the prediction through and skip the
    // inverse transform + residual add, so the recon stage can be priced by ablation
    // on the UNINSTRUMENTED binary. The scalar twin in `common::predict` carries the
    // same knob; this one covers the DEFAULT (accel) path. Output is wrong while set.
    if abl_recon() {
        for r in 0..8 {
            rec[r * stride_rec..r * stride_rec + 8]
                .copy_from_slice(&pred[r * stride_pred..r * stride_pred + 8]);
        }
        return;
    }
    // SAFETY: bounds asserted; the kernel reads 64 i16 + an 8×8 pred region and
    // writes an 8×8 reconstruction region at the given strides. AVX2 twin is
    // ISA-dispatch-interchangeable (unaligned `dct` access) => bit-identical recon.
    unsafe {
        if has_avx2() {
            WelsIDctFourT4Rec_avx2(
                rec.as_mut_ptr(),
                stride_rec as i32,
                pred.as_ptr(),
                stride_pred as i32,
                dct.as_ptr(),
            );
        } else {
            WelsIDctFourT4Rec_sse2(
                rec.as_mut_ptr(),
                stride_rec as i32,
                pred.as_ptr(),
                stride_pred as i32,
                dct.as_ptr(),
            );
        }
    }
}

/// Forward 4×4 core DCT of an **8×8 region** (four 4×4 blocks) of the residual
/// `src - pred`, via openh264's `WelsDctFourT4_sse2`. Writes 64 `i16` coefficients
/// to `dct`: blocks in `(0,0),(4,0),(0,4),(4,4)` order, raster within each block.
/// The integer core transform is bit-identical to our scalar `forward_core`
/// (`out0=s0+s1, out1=2·s3+s2, out2=s0-s1, out3=s3-2·s2`), so quantizing these
/// coefficients yields identical levels — a pure speedup, byte-for-byte.
#[inline]
pub(crate) fn asm_dct_four_t4(dct: &mut [i16], src: &[u8], stride_src: usize, pred: &[u8], stride_pred: usize) {
    assert!(dct.len() >= 64);
    assert!(src.len() >= 7 * stride_src + 8);
    assert!(pred.len() >= 7 * stride_pred + 8);
    // SAFETY: bounds asserted; the kernel reads an 8×8 region from each plane at
    // the given strides and writes exactly 64 i16. AVX2 twin is ISA-dispatch-
    // interchangeable in openh264 (unaligned `dct` store) => bit-identical coeffs.
    unsafe {
        if has_avx2() {
            WelsDctFourT4_avx2(
                dct.as_mut_ptr(),
                src.as_ptr(),
                stride_src as i32,
                pred.as_ptr(),
                stride_pred as i32,
            );
        } else {
            WelsDctFourT4_sse2(
                dct.as_mut_ptr(),
                src.as_ptr(),
                stride_src as i32,
                pred.as_ptr(),
                stride_pred as i32,
            );
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    // chroma MC moved to the portable `chroma_mc` module (rip-ASM Phase 1); these
    // pre-existing tests pinned the openh264 asm against scalar and now pin the
    // SSE2/NEON replacement against the same oracle.
    use crate::{mc_chroma_w4, mc_chroma_w8};
    // SATD/SAD moved to the portable `satd_sad` module (rip-ASM Phase 5a); these
    // pre-existing openh264-reference tests now pin the Rust kernels to the same
    // C reference the assembly was held to.
    use crate::{sad_16x16, sad_16x8, sad_8x16, satd_16x16, satd_16x8, satd_4x4, satd_8x16, satd_8x8};

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

    #[test]
    fn mc_chroma_w8_matches_scalar() {
        // Deterministic 9×9 clamped tile; test every eighth-pel (fx,fy) phase.
        let mut tile = [0u8; 9 * 9];
        let mut s = 0x12345u32;
        for v in tile.iter_mut() {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            *v = (s >> 16) as u8;
        }
        for fy in 0..8i32 {
            for fx in 0..8i32 {
                let (wa, wb, wc, wd) =
                    ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
                let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
                let mut got = [0u8; 64];
                mc_chroma_w8(&tile, 9, &mut got, 8, &abcd, 8);
                let mut want = [0u8; 64];
                for r in 0..8 {
                    for c in 0..8 {
                        let p = r * 9 + c;
                        let v = wa * tile[p] as i32
                            + wb * tile[p + 1] as i32
                            + wc * tile[p + 9] as i32
                            + wd * tile[p + 9 + 1] as i32;
                        want[r * 8 + c] = ((v + 32) >> 6) as u8;
                    }
                }
                assert_eq!(got, want, "fx={fx} fy={fy}");
            }
        }
    }

    /// H-38 oracle for the newly-wired 4-wide chroma kernel: every eighth-pel phase
    /// must match the scalar bilinear exactly, at both block heights the decoder
    /// asks for (4-wide blocks are 4 or 2 tall after rect coalescing).
    #[test]
    fn mc_chroma_w4_matches_scalar() {
        let mut tile = [0u8; 5 * 9];
        let mut s = 0xbeef1u32;
        for v in tile.iter_mut() {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            *v = (s >> 16) as u8;
        }
        for h in [2usize, 4, 8] {
            for fy in 0..8i32 {
                for fx in 0..8i32 {
                    let (wa, wb, wc, wd) =
                        ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
                    let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
                    let mut got = [0u8; 32];
                    mc_chroma_w4(&tile, 5, &mut got[..h * 4], 4, &abcd, h);
                    let mut want = [0u8; 32];
                    for r in 0..h {
                        for c in 0..4 {
                            let p = r * 5 + c;
                            let v = wa * tile[p] as i32
                                + wb * tile[p + 1] as i32
                                + wc * tile[p + 5] as i32
                                + wd * tile[p + 5 + 1] as i32;
                            want[r * 4 + c] = ((v + 32) >> 6) as u8;
                        }
                    }
                    assert_eq!(got[..h * 4], want[..h * 4], "h={h} fx={fx} fy={fy}");
                }
            }
        }
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
            asm_dct_four_t4(&mut dct, &src, 8, &pred, 8);
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
            asm_idct_four_t4_rec(&mut rec, 8, &pred, 8, dct);
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
            asm_quant_four_4x4(&mut dctw.0, &ff, &mf);
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

/// AVX2 boundary-strength MOTION MASKS — the vectorised twin of
/// `rusty_h264_common::deblock::bs_motion_masks_scalar`.
///
/// Computes, for one macroblock's 16 blocks laid out in raster order:
///   `left` bit k = block k differs in motion from block k-1
///   `up`   bit k = block k differs in motion from block k-4
/// where "differs" is §8.7.2.1's inter rule:
///   `ref[a] != ref[b] || (ref[a] != NO_REF && (|dmvx| >= 4 || |dmvy| >= 4))`
///
/// This replaces 24 branchy scalar `bs_inter` evaluations — the part the ceiling
/// probe identified as COMPUTE-bound (~370 of the derivation's ~400 ns/MB), as
/// opposed to the gather, which was only ~31 ns/MB.
///
/// LANE-BOUNDARY TRICK, and it is load-bearing: the `left` comparison uses a
/// WITHIN-128-bit-lane byte shift, which corrupts the lanes at each 128-bit boundary
/// — for i16 that is k=0 and k=8, for i32 k=0 and k=4. Every corrupted position has
/// `k % 4 == 0`, and those are exactly the macroblock-edge blocks whose strengths are
/// derived separately against the neighbouring record. So the cheap shift is correct
/// where it matters and garbage only where the result is discarded. The `up`
/// comparison genuinely crosses lanes and uses `permute2x128`.
///
/// Both masks are returned with the don't-care bits FORCED TO ZERO so the scalar and
/// SIMD twins are bit-identical on the whole u16, not merely on the bits consumed.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bs_motion_masks_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    no_ref: i32,
) -> (u16, u16) {
    use std::arch::x86_64::*;

    let vx = _mm256_loadu_si256(mvx.as_ptr() as *const __m256i);
    let vy = _mm256_loadu_si256(mvy.as_ptr() as *const __m256i);
    let r0 = _mm256_loadu_si256(ref_id.as_ptr() as *const __m256i);
    let r1 = _mm256_loadu_si256(ref_id.as_ptr().add(8) as *const __m256i);

    let four = _mm256_set1_epi16(4);
    let nr = _mm256_set1_epi32(no_ref);

    // |a - b| >= 4, per i16 lane, as `|a-b| > 3`.
    //
    // NOT the `or(subs(a,b), subs(b,a))` idiom recorded elsewhere in this workspace:
    // that one needs UNSIGNED saturating subtract, where the wrong direction clamps to
    // zero. Motion vectors are SIGNED, so `subs_epi16` leaves the wrong direction
    // negative and the OR yields garbage — the `*_matches_scalar` gate caught exactly
    // that. `abs_epi16(sub_epi16(..))` also matches the scalar twin's overflow
    // behaviour: both wrap in i16, and both map i16::MIN to itself.
    let far = |a: __m256i, b: __m256i| -> __m256i {
        let d = _mm256_abs_epi16(_mm256_sub_epi16(a, b));
        _mm256_cmpgt_epi16(d, _mm256_sub_epi16(four, _mm256_set1_epi16(1)))
    };

    // ---- LEFT (k-1): within-lane shifts; corrupted lanes are all k%4==0. ----
    let vxl = _mm256_bslli_epi128(vx, 2);
    let vyl = _mm256_bslli_epi128(vy, 2);
    let farl = _mm256_or_si256(far(vx, vxl), far(vy, vyl));

    let r0l = _mm256_bslli_epi128(r0, 4);
    let r1l = _mm256_bslli_epi128(r1, 4);
    // ref differs, and the "far" term is gated on ref[a] != NO_REF.
    let neq0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, r0l), _mm256_set1_epi32(-1));
    let neq1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, r1l), _mm256_set1_epi32(-1));
    let live0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, nr), _mm256_set1_epi32(-1));
    let live1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, nr), _mm256_set1_epi32(-1));
    // Narrow the i32 ref predicates to i16 lanes so they combine with `far`.
    // packs_epi32 interleaves the two 128-bit halves, so permute4x64 restores order.
    let pack = |a: __m256i, b: __m256i| {
        _mm256_permute4x64_epi64(_mm256_packs_epi32(a, b), 0b11_01_10_00)
    };
    let left = _mm256_or_si256(
        pack(neq0, neq1),
        _mm256_and_si256(pack(live0, live1), farl),
    );

    // ---- UP (k-4): genuinely crosses the 128-bit boundary. ----
    // i16: shift the whole 256-bit register right by 8 bytes (4 blocks).
    let shift4_i16 = |v: __m256i| {
        let lo = _mm256_permute2x128_si256(v, v, 0x08); // [0, low_lane]
        _mm256_alignr_epi8(v, lo, 8)
    };
    let vxu = shift4_i16(vx);
    let vyu = shift4_i16(vy);
    let faru = _mm256_or_si256(far(vx, vxu), far(vy, vyu));

    // i32: k-4 is exactly one 128-bit lane back.
    let r0u = _mm256_permute2x128_si256(r0, r0, 0x08); // [0, ref0..3]
    let r1u = _mm256_permute2x128_si256(r0, r1, 0x21); // [ref4..7, ref8..11]
    let uneq0 = _mm256_xor_si256(_mm256_cmpeq_epi32(r0, r0u), _mm256_set1_epi32(-1));
    let uneq1 = _mm256_xor_si256(_mm256_cmpeq_epi32(r1, r1u), _mm256_set1_epi32(-1));
    let up = _mm256_or_si256(
        pack(uneq0, uneq1),
        _mm256_and_si256(pack(live0, live1), faru),
    );

    // One bit per i16 lane out of a byte-granular movemask: take every other bit.
    let bits = |v: __m256i| -> u16 {
        let m = _mm256_movemask_epi8(v) as u32;
        let mut out = 0u16;
        let mut k = 0;
        while k < 16 {
            out |= (((m >> (k * 2)) & 1) as u16) << k;
            k += 1;
        }
        out
    };
    // Force the don't-care bits to zero so the twins match on the FULL u16.
    (bits(left) & 0xEEEE, bits(up) & 0xFFF0)
}

/// Safe dispatcher: AVX2 when present, else the caller's scalar twin.
/// Returns `None` when AVX2 is unavailable so the caller keeps its own oracle path.
#[cfg(target_arch = "x86_64")]
pub fn bs_motion_masks(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    no_ref: i32,
) -> Option<(u16, u16)> {
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: all three inputs are fixed-size arrays of exactly the width the
        // kernel loads (16 x i16 = 32 B for each mv plane, 16 x i32 = 64 B for refs),
        // so every load below is in bounds by construction.
        Some(unsafe { bs_motion_masks_avx2(mvx, mvy, ref_id, no_ref) })
    } else {
        None
    }
}

/// TWO-LIST boundary-strength motion masks (WHYS Part 16's named lever): the
/// §8.7.2.1 set-matching rule, vectorized. Until this kernel, any macroblock
/// with a List-1 slot fell to the scalar per-edge walk — most B inter MBs.
///
/// The branchless per-lane formula (proven case-equal to the scalar
/// `pk_differs` decision tree, including its slot-COMPACTION cases):
///
/// ```text
///   differs = !( (e0 & e1 & !farStraight) | (c0 & c1 & !farCross) )
///     e0/e1 = ref0/ref1 equal to neighbour's ref0/ref1 (straight)
///     c0/c1 = ref0/ref1 equal to neighbour's ref1/ref0 (crossed)
///     far*  = any |Δmv| ≥ 4 under that pairing
/// ```
///
/// The compaction cases work WITHOUT per-lane branching because unused-slot
/// motion is NEUTRALIZED to zero inside the kernel (`mv &= (ref != NO_REF)`):
/// a missing slot then always compares "near" against another missing slot and
/// its ref comparisons (NO_REF vs X) drive the set logic — exhaustively checked
/// against the scalar twin over random two-list inputs by
/// `bs_motion_masks_two_list_matches_scalar`.
///
/// # Safety
/// AVX2 only; caller (the safe dispatcher below) has verified the feature.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bs_motion_masks_two_list_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref0: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
    no_ref: i32,
) -> (u16, u16) {
    use std::arch::x86_64::*;

    let x0 = _mm256_loadu_si256(mvx.as_ptr() as *const __m256i);
    let y0 = _mm256_loadu_si256(mvy.as_ptr() as *const __m256i);
    let x1 = _mm256_loadu_si256(mvx1.as_ptr() as *const __m256i);
    let y1 = _mm256_loadu_si256(mvy1.as_ptr() as *const __m256i);
    let r0lo = _mm256_loadu_si256(ref0.as_ptr() as *const __m256i);
    let r0hi = _mm256_loadu_si256(ref0.as_ptr().add(8) as *const __m256i);
    let r1lo = _mm256_loadu_si256(ref1.as_ptr() as *const __m256i);
    let r1hi = _mm256_loadu_si256(ref1.as_ptr().add(8) as *const __m256i);

    let nr = _mm256_set1_epi32(no_ref);
    let ones = _mm256_set1_epi32(-1);
    // packs_epi32 interleaves 128-bit halves; permute4x64 restores lane order.
    let pack = |a: __m256i, b: __m256i| {
        _mm256_permute4x64_epi64(_mm256_packs_epi32(a, b), 0b11_01_10_00)
    };
    // NEUTRALIZE unused-slot motion: the formula's correctness on compaction
    // cases requires missing slots to carry (0,0) REGARDLESS of what the caller
    // stored (see the doc comment).
    let live0 = pack(
        _mm256_xor_si256(_mm256_cmpeq_epi32(r0lo, nr), ones),
        _mm256_xor_si256(_mm256_cmpeq_epi32(r0hi, nr), ones),
    );
    let live1 = pack(
        _mm256_xor_si256(_mm256_cmpeq_epi32(r1lo, nr), ones),
        _mm256_xor_si256(_mm256_cmpeq_epi32(r1hi, nr), ones),
    );
    let x0 = _mm256_and_si256(x0, live0);
    let y0 = _mm256_and_si256(y0, live0);
    let x1 = _mm256_and_si256(x1, live1);
    let y1 = _mm256_and_si256(y1, live1);

    let three = _mm256_set1_epi16(3);
    let far = |a: __m256i, b: __m256i| -> __m256i {
        let d = _mm256_abs_epi16(_mm256_sub_epi16(a, b));
        _mm256_cmpgt_epi16(d, three)
    };
    // One bit per i16 lane from the byte-granular movemask (same as the
    // single-list kernel).
    let bits = |v: __m256i| -> u16 {
        let m = _mm256_movemask_epi8(v) as u32;
        let mut out = 0u16;
        for k in 0..16 {
            out |= (((m >> (2 * k)) & 1) as u16) << k;
        }
        out
    };
    // differs for one shifted pairing, given the six shifted operands.
    let differs = |x0s: __m256i,
                   y0s: __m256i,
                   x1s: __m256i,
                   y1s: __m256i,
                   e0: __m256i,
                   e1: __m256i,
                   c0: __m256i,
                   c1: __m256i|
     -> __m256i {
        let far_s = _mm256_or_si256(
            _mm256_or_si256(far(x0, x0s), far(y0, y0s)),
            _mm256_or_si256(far(x1, x1s), far(y1, y1s)),
        );
        let far_x = _mm256_or_si256(
            _mm256_or_si256(far(x0, x1s), far(y0, y1s)),
            _mm256_or_si256(far(x1, x0s), far(y1, y0s)),
        );
        let ok_s = _mm256_andnot_si256(far_s, _mm256_and_si256(e0, e1));
        let ok_x = _mm256_andnot_si256(far_x, _mm256_and_si256(c0, c1));
        _mm256_xor_si256(_mm256_or_si256(ok_s, ok_x), _mm256_set1_epi16(-1))
    };

    // ---- LEFT (k-1): within-lane shifts; corrupted lanes are all k%4==0. ----
    let sh16 = |v: __m256i| _mm256_bslli_epi128(v, 2);
    let sh32 = |v: __m256i| _mm256_bslli_epi128(v, 4);
    let e0l = pack(
        _mm256_cmpeq_epi32(r0lo, sh32(r0lo)),
        _mm256_cmpeq_epi32(r0hi, sh32(r0hi)),
    );
    let e1l = pack(
        _mm256_cmpeq_epi32(r1lo, sh32(r1lo)),
        _mm256_cmpeq_epi32(r1hi, sh32(r1hi)),
    );
    let c0l = pack(
        _mm256_cmpeq_epi32(r0lo, sh32(r1lo)),
        _mm256_cmpeq_epi32(r0hi, sh32(r1hi)),
    );
    let c1l = pack(
        _mm256_cmpeq_epi32(r1lo, sh32(r0lo)),
        _mm256_cmpeq_epi32(r1hi, sh32(r0hi)),
    );
    let left = differs(sh16(x0), sh16(y0), sh16(x1), sh16(y1), e0l, e1l, c0l, c1l);

    // ---- UP (k-4): one 128-bit lane back for i32, alignr for i16. ----
    let shu16 = |v: __m256i| {
        let lo = _mm256_permute2x128_si256(v, v, 0x08);
        _mm256_alignr_epi8(v, lo, 8)
    };
    let shu32 = |lo: __m256i, hi: __m256i| -> (__m256i, __m256i) {
        (
            _mm256_permute2x128_si256(lo, lo, 0x08),
            _mm256_permute2x128_si256(lo, hi, 0x21),
        )
    };
    let (r0ulo, r0uhi) = shu32(r0lo, r0hi);
    let (r1ulo, r1uhi) = shu32(r1lo, r1hi);
    let e0u = pack(_mm256_cmpeq_epi32(r0lo, r0ulo), _mm256_cmpeq_epi32(r0hi, r0uhi));
    let e1u = pack(_mm256_cmpeq_epi32(r1lo, r1ulo), _mm256_cmpeq_epi32(r1hi, r1uhi));
    let c0u = pack(_mm256_cmpeq_epi32(r0lo, r1ulo), _mm256_cmpeq_epi32(r0hi, r1uhi));
    let c1u = pack(_mm256_cmpeq_epi32(r1lo, r0ulo), _mm256_cmpeq_epi32(r1hi, r0uhi));
    let up = differs(shu16(x0), shu16(y0), shu16(x1), shu16(y1), e0u, e1u, c0u, c1u);

    (bits(left) & 0xEEEE, bits(up) & 0xFFF0)
}

/// Safe dispatcher for the two-list masks kernel; `None` when AVX2 is absent.
#[cfg(target_arch = "x86_64")]
pub fn bs_motion_masks_two_list(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref0: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
    no_ref: i32,
) -> Option<(u16, u16)> {
    if !std::arch::is_x86_feature_detected!("avx2") {
        return None;
    }
    // SAFETY: AVX2 presence verified above; all inputs are fixed-size arrays of
    // exactly the widths the kernel loads.
    Some(unsafe { bs_motion_masks_two_list_avx2(mvx, mvy, ref0, mvx1, mvy1, ref1, no_ref) })
}


/// AVX2 "does this macroblock have uniform motion" test — all 16 blocks sharing one
/// (ref, mv) on both lists.
///
/// Chosen over extending `bs_motion_masks_avx2` to two lists after COUNTING both
/// populations on the main corpus (see docs/WHYS-decoder-parf.md):
///
/// * uniform check runs on ALL 6,190,820 packed macroblocks  -> 557M compares
/// * mask derivation runs on 726,540 (11.7%)                 -> 105M compares
/// * the two-list subset is 203,090 (3.1% of all MBs)        ->  58M compares
///
/// The uniform check is 9.5x the two-list brick's work, and it is a broadcast-compare
/// rather than an order-independent set match — more prize for far less risk.
///
/// It also matters that the SCALAR version SHORT-CIRCUITS: a non-uniform macroblock
/// bails after a block or two, but a UNIFORM one (the common case — Skip plus
/// single-partition inter) walks all 15 comparisons. So the population paying full
/// scalar price is precisely the one this replaces with ~6 vector compares.
///
/// Returns `Some(uniform)`, or `None` when AVX2 is unavailable so the caller keeps
/// its scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mb_uniform_avx2(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
) -> bool {
    use std::arch::x86_64::*;
    // 16 x i16 = one register; broadcast lane 0 and compare the whole plane at once.
    let eq16 = |v: &[i16; 16]| -> __m256i {
        let a = _mm256_loadu_si256(v.as_ptr() as *const __m256i);
        _mm256_cmpeq_epi16(a, _mm256_set1_epi16(v[0]))
    };
    // 16 x i32 = two registers; `packs` narrows the two predicate halves to i16 lanes
    // so every plane's result combines in one register.
    let eq32 = |v: &[i32; 16]| -> __m256i {
        let b = _mm256_set1_epi32(v[0]);
        let lo = _mm256_cmpeq_epi32(_mm256_loadu_si256(v.as_ptr() as *const __m256i), b);
        let hi = _mm256_cmpeq_epi32(_mm256_loadu_si256(v.as_ptr().add(8) as *const __m256i), b);
        _mm256_permute4x64_epi64(_mm256_packs_epi32(lo, hi), 0b11_01_10_00)
    };
    let all = _mm256_and_si256(
        _mm256_and_si256(_mm256_and_si256(eq16(mvx), eq16(mvy)), eq32(ref_id)),
        _mm256_and_si256(_mm256_and_si256(eq16(mvx1), eq16(mvy1)), eq32(ref1)),
    );
    // Every lane must have compared equal.
    _mm256_movemask_epi8(all) == -1
}

/// Safe dispatcher for [`mb_uniform_avx2`]; `None` means "no AVX2, use your scalar twin".
#[cfg(target_arch = "x86_64")]
#[allow(clippy::too_many_arguments)]
pub fn mb_uniform(
    mvx: &[i16; 16],
    mvy: &[i16; 16],
    ref_id: &[i32; 16],
    mvx1: &[i16; 16],
    mvy1: &[i16; 16],
    ref1: &[i32; 16],
) -> Option<bool> {
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: all six inputs are fixed-size arrays of exactly the width loaded
        // (16 x i16 = 32 B, 16 x i32 = 64 B), so every load is in bounds by construction.
        Some(unsafe { mb_uniform_avx2(mvx, mvy, ref_id, mvx1, mvy1, ref1) })
    } else {
        None
    }
}

/// AVX2 inverse quantization of one 4×4 block — `out[i] = f(levels[i] * ls[i])`,
/// the spec §8.5.12.1 flat/weighted dequant the decoder runs on every coded block.
///
/// **Why intrinsics here and not auto-vectorization** (the Step-0 gate, answered
/// empirically): the crate emits **zero `vpmulld`/`pmulld` and 502 scalar `imul`** —
/// LLVM's cost model declines to vectorize 32-bit integer multiply, because
/// `vpmulld` is 2 uops on this microarchitecture. It still wins decisively on uop
/// COUNT: 16 scalar `imul` become 2 `vpmulld`, and the shift/round becomes 2 more
/// vector ops instead of 16 scalar ones.
///
/// **Bit-identical, not merely close.** These are exact integer ops in the same order
/// as the scalar twin — same multiply, same rounding add, same arithmetic shift — so
/// this gates with `assert_eq!`, not a tolerance. `dequant_4x4_matches_scalar` pins it
/// over the full QP range including both sides of the `qp >= 24` branch.
///
/// The shift amount is a RUNTIME value, so this uses `_mm256_sll/sra_epi32` (variable
/// count in an xmm) rather than the immediate-only `_mm256_slli/srai_epi32`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dequant_4x4_avx2(out: &mut [i32; 16], levels: &[i32; 16], ls: &[i32; 16], qp: u8) {
    use std::arch::x86_64::*;
    let l0 = _mm256_loadu_si256(levels.as_ptr() as *const __m256i);
    let l1 = _mm256_loadu_si256(levels.as_ptr().add(8) as *const __m256i);
    let s0 = _mm256_loadu_si256(ls.as_ptr() as *const __m256i);
    let s1 = _mm256_loadu_si256(ls.as_ptr().add(8) as *const __m256i);
    let p0 = _mm256_mullo_epi32(l0, s0);
    let p1 = _mm256_mullo_epi32(l1, s1);
    let shift = (qp / 6) as i32;
    let (r0, r1) = if qp >= 24 {
        let c = _mm_cvtsi32_si128(shift - 4);
        (_mm256_sll_epi32(p0, c), _mm256_sll_epi32(p1, c))
    } else {
        let add = _mm256_set1_epi32(1 << (3 - shift));
        let c = _mm_cvtsi32_si128(4 - shift);
        (
            _mm256_sra_epi32(_mm256_add_epi32(p0, add), c),
            _mm256_sra_epi32(_mm256_add_epi32(p1, add), c),
        )
    };
    _mm256_storeu_si256(out.as_mut_ptr() as *mut __m256i, r0);
    _mm256_storeu_si256(out.as_mut_ptr().add(8) as *mut __m256i, r1);
}

/// Safe dispatcher. `None` = no AVX2, caller keeps its scalar twin (the oracle).
#[cfg(target_arch = "x86_64")]
pub fn dequant_4x4(out: &mut [i32; 16], levels: &[i32; 16], ls: &[i32; 16], qp: u8) -> bool {
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: all three are fixed [i32; 16] arrays — exactly the 2×256-bit the
        // kernel loads/stores. Every access is in bounds by construction.
        unsafe { dequant_4x4_avx2(out, levels, ls, qp) };
        true
    } else {
        false
    }
}
