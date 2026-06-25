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
