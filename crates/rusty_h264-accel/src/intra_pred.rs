//! **16x16 luma / 8x8 chroma intra prediction** — portable (rip-ASM Phase 5c).
//!
//! Replaces openh264's `WelsI16x16LumaPred{V,H,Dc,Plane}_sse2` and
//! `WelsIChromaPred{V,Plane}_sse2` (the last vendored intra_pred consumers).
//! The predictors are spec §8.3.3 forms — straight row copies (V), per-row
//! broadcasts (H), one average (DC) and a linear ramp (Plane); memory-shaped
//! work LLVM vectorises well, so these are deliberately plain Rust with
//! fixed-size inner loops. Pinned bit-exact against the LIVE assembly by the
//! `*_matches_asm` differential tests below (the strongest oracle available
//! while the asm is still in the tree); `RS_H264_ASM_TQ=1` selects the asm arm
//! in production for the paired A/B, same knob as the transform/quant trio.

/// 16x16 luma prediction into `pred` (>= 256 bytes). `rec[base]` is the MB
/// top-left; the top row is `rec[base - stride ..]`, the left column
/// `rec[base - 1 + y*stride]`. `mode`: 0=V, 1=H, 2=DC, 3=Plane — the caller
/// guarantees the needed neighbours exist (both for DC/Plane), exactly the
/// contract the asm wrapper had.
pub fn i16x16_luma_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 256);
    assert!(base >= stride + 1 && base + 15 * stride <= rec.len());
    #[cfg(target_arch = "x86_64")]
    if crate::transform_quant::asm_tq() {
        return crate::x86_asm::asm_i16x16_luma_pred(mode, pred, rec, base, stride);
    }
    match mode {
        0 => {
            let top = &rec[base - stride..base - stride + 16];
            for y in 0..16 {
                pred[y * 16..y * 16 + 16].copy_from_slice(top);
            }
        }
        1 => {
            for y in 0..16 {
                let l = rec[base - 1 + y * stride];
                pred[y * 16..y * 16 + 16].fill(l);
            }
        }
        2 => {
            let mut sum = 16u32;
            for i in 0..16 {
                sum += rec[base - stride + i] as u32;
                sum += rec[base - 1 + i * stride] as u32;
            }
            pred[..256].fill((sum >> 5) as u8);
        }
        _ => {
            // Spec §8.3.3.4. p[x,-1] = rec[base - stride + x], p[-1,y] = left,
            // p[-1,-1] = rec[base - stride - 1].
            let px = |x: isize| -> i32 {
                if x < 0 {
                    rec[base - stride - 1] as i32
                } else {
                    rec[base - stride + x as usize] as i32
                }
            };
            let py = |y: isize| -> i32 {
                if y < 0 {
                    rec[base - stride - 1] as i32
                } else {
                    rec[base - 1 + y as usize * stride] as i32
                }
            };
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..8i32 {
                h += (i + 1) * (px(8 + i as isize) - px(6 - i as isize));
                v += (i + 1) * (py(8 + i as isize) - py(6 - i as isize));
            }
            let a = 16 * (px(15) + py(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..16i32 {
                    let p = (a + b * (x - 7) + c * (y - 7) + 16) >> 5;
                    pred[(y * 16 + x) as usize] = p.clamp(0, 255) as u8;
                }
            }
        }
    }
}

/// 8x8 chroma prediction into `pred` (>= 64 bytes). `mode`: 2=Vertical,
/// 3(=else)=Plane — the only modes the asm served; DC/Horizontal stay with the
/// caller's scalar, exactly the old wrapper's contract.
pub fn chroma8x8_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 64);
    assert!(base >= stride + 1 && base + 7 * stride <= rec.len());
    #[cfg(target_arch = "x86_64")]
    if crate::transform_quant::asm_tq() {
        return crate::x86_asm::asm_chroma8x8_pred(mode, pred, rec, base, stride);
    }
    if mode == 2 {
        let top = &rec[base - stride..base - stride + 8];
        for y in 0..8 {
            pred[y * 8..y * 8 + 8].copy_from_slice(top);
        }
    } else {
        // Spec §8.3.4.4 (chroma plane).
        let px = |x: isize| -> i32 {
            if x < 0 {
                rec[base - stride - 1] as i32
            } else {
                rec[base - stride + x as usize] as i32
            }
        };
        let py = |y: isize| -> i32 {
            if y < 0 {
                rec[base - stride - 1] as i32
            } else {
                rec[base - 1 + y as usize * stride] as i32
            }
        };
        let mut h = 0i32;
        let mut v = 0i32;
        for i in 0..4i32 {
            h += (i + 1) * (px(4 + i as isize) - px(2 - i as isize));
            v += (i + 1) * (py(4 + i as isize) - py(2 - i as isize));
        }
        let a = 16 * (px(7) + py(7));
        let b = (17 * h + 16) >> 5;
        let c = (17 * v + 16) >> 5;
        for y in 0..8i32 {
            for x in 0..8i32 {
                let p = (a + b * (x - 3) + c * (y - 3) + 16) >> 5;
                pred[(y * 8 + x) as usize] = p.clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    fn lcg(state: &mut u64) -> u32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*state >> 33) as u32
    }

    /// Aligned buffers (the asm arm demands 16-byte alignment of `pred` AND of
    /// `rec[base]` — macroblock positions on aligned planes always are, so the
    /// alignment is part of the asm's implicit contract).
    #[repr(align(16))]
    struct A<const N: usize>([u8; N]);

    #[test]
    fn i16x16_pred_matches_asm_all_modes() {
        let mut st = 0xabcdu64;
        let stride = 64usize;
        for round in 0..500usize {
            let mut recb = A([0u8; 64 * 64]);
            for v in recb.0.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            let rec = &recb.0[..];
            // Deep inside the buffer, 16-aligned like a real MB position (the
            // asm's movdqa loads fault on anything else), varying by whole MBs.
            let base = 4 * stride + 16 + 16 * (round % 3);
            for mode in 0..4u8 {
                let mut a = A([0u8; 256]);
                let mut b = A([0u8; 256]);
                super::i16x16_luma_pred(mode, &mut a.0, rec, base, stride);
                crate::x86_asm::asm_i16x16_luma_pred(mode, &mut b.0, rec, base, stride);
                assert_eq!(a.0, b.0, "round {round} mode {mode}");
            }
        }
    }

    #[test]
    fn chroma8x8_pred_matches_asm_both_modes() {
        let mut st = 0x9999u64;
        let stride = 48usize;
        for round in 0..500usize {
            let mut recb = A([0u8; 48 * 48]);
            for v in recb.0.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            let rec = &recb.0[..];
            let base = 4 * stride + 16 + 8 * (round % 3);
            for mode in [2u8, 3] {
                let mut a = A([0u8; 64]);
                let mut b = A([0u8; 64]);
                super::chroma8x8_pred(mode, &mut a.0, rec, base, stride);
                crate::x86_asm::asm_chroma8x8_pred(mode, &mut b.0, rec, base, stride);
                assert_eq!(a.0, b.0, "round {round} mode {mode}");
            }
        }
    }
}
