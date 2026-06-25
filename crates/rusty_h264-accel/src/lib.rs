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
    fn WelsDctFourT4_sse2(p_dct: *mut i16, p1: *const u8, s1: i32, p2: *const u8, s2: i32);
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
