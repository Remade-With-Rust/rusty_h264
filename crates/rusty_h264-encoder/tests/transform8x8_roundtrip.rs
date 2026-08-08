//! **8x8-transform conformance gate**, for both entropy coders and both slice types.
//!
//! This gate exists because the 8x8 transform managed to be broken end-to-end while
//! every existing test passed:
//!
//!   * CABAC 8x8 could not be ENCODED at all (the writer had no
//!     `transform_size_8x8_flag` and no ctxBlockCat-5 residual), and the encoder
//!     refused the combination — so no test could reach it.
//!   * CAVLC 8x8 could be encoded, ffmpeg accepted the streams, and OUR decoder
//!     could not read them. The recon helper broadcast one aggregate coefficient
//!     count over all four 4x4 cells of each 8x8 block, clobbering the per-4x4
//!     counts that CAVLC's `nC` predictor reads — a parse desync several
//!     macroblocks later. It hid because the THREADED decode path never wrote that
//!     grid, and because no test decoded a CAVLC 8x8 stream at all.
//!
//! The second one is why this file decodes rather than merely encoding: a bitstream
//! that a reference decoder accepts can still be one we cannot read back, and only a
//! round-trip says so. CAVLC additionally pins the INLINE decode path specifically —
//! the threading dispatch only ever engages for CABAC, so a CAVLC round-trip is the
//! one that exercises the code where the defect lived.
//!
//! MUTATION-PROVEN: re-introducing the aggregate broadcast fails this gate
//! (`decode failed: Truncated` on the first CAVLC case). Its FIRST version did not —
//! the clip was high-frequency, so 8x8 was chosen on a handful of macroblocks and
//! the defect slipped through. Hence the explicit "the 8x8 stream must be smaller"
//! assertion below: a conformance gate for a content-adaptive tool has to prove the
//! tool was actually selected, or it is only testing the fallback.

use rusty_h264_common::{Profile, YuvFrame};
use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig};

/// SMOOTH, large-scale content with gentle structure and a slow pan.
///
/// The content choice is load-bearing, and getting it wrong made the first version
/// of this gate worthless: a high-frequency XOR texture made the 8x8 vs 4x4 streams
/// differ by 6-38 bytes out of ~20,000, i.e. the 8x8 transform was chosen on a
/// handful of macroblocks and a deliberately re-introduced desync still passed. The
/// 8x8 transform wins where energy is LOW-frequency, so the clip has to be smooth
/// for the path under test to actually run.
fn frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    let sh = f as f64 * 1.7;
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f64 + sh, y as f64);
            let v = 128.0
                + 60.0 * (xf / 19.0).sin() * (yf / 23.0).cos()
                + 30.0 * ((xf + yf) / 37.0).sin()
                + 8.0 * (xf / 5.0).cos();
            fr.y[y * w + x] = v.clamp(0.0, 255.0) as u8;
        }
    }
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let (xf, yf) = (cx as f64 + sh / 2.0, cy as f64);
            fr.u[cy * (w / 2) + cx] =
                (128.0 + 40.0 * (xf / 11.0).sin()).clamp(0.0, 255.0) as u8;
            fr.v[cy * (w / 2) + cx] =
                (128.0 + 40.0 * (yf / 13.0).cos()).clamp(0.0, 255.0) as u8;
        }
    }
    fr
}

fn encode(w: usize, h: usize, qp: u8, cabac: bool, t8: bool, bframes: u32, n: u64) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = 8;
    cfg.cabac = cabac;
    cfg.bframes = bframes;
    cfg.transform_8x8 = t8;
    // The 8x8 transform is a High-profile tool under both entropy coders: the flag
    // only exists when the SPS can signal transform_8x8_mode_flag.
    cfg.profile = Profile::High;
    let enc = Encoder::new(cfg).expect("encoder must accept High + 8x8 on both coders");
    // `encode_all`, not the streaming `encode`: B-frames need the reorder pipeline
    // (the future anchor is coded before the B frames that reference it), and the
    // streaming API rejects them outright.
    let frames: Vec<YuvFrame> = (0..n).map(|f| frame(w, h, f)).collect();
    enc.encode_all(&frames).expect("encode_all").concat()
}

fn decode_all(stream: &[u8], what: &str) -> Vec<YuvFrame> {
    Decoder::new()
        .decode_stream(stream)
        .unwrap_or_else(|e| panic!("{what}: decode failed: {e:?}"))
}

/// Every 8x8 configuration must round-trip: encodable, and readable back. Crossed
/// over entropy coder x slice types x QP, because the writer has TWO distinct
/// syntax positions for the flag (I_NxN before the pred modes, inter after cbp) and
/// the B path has its own presence rule.
#[test]
fn transform8x8_roundtrips_on_both_coders_and_slice_types() {
    let (w, h) = (176, 144); // several MBs each way, so neighbour contexts are live
    for &cabac in &[false, true] {
        for &bframes in &[0u32, 2] {
            // QPs where the 8x8 transform is genuinely exercised on this clip.
            // qp36 is deliberately absent: at that rate a smooth clip codes to
            // almost all-skip, 8x8 shrinks the stream by only ~0.7%, and the case
            // would sit under the "path actually ran" bar below. Better to cover
            // three rates that test the tool than four where one tests the fallback.
            for &qp in &[16u8, 22, 28] {
                let tag = format!(
                    "{}/{}/qp{qp}",
                    if cabac { "cabac" } else { "cavlc" },
                    if bframes > 0 { "I+P+B" } else { "I+P" }
                );
                let s8 = encode(w, h, qp, cabac, true, bframes, 6);
                let s4 = encode(w, h, qp, cabac, false, bframes, 6);
                // PROVE THE PATH RAN. Without this the gate can pass while the 8x8
                // transform is never selected -- exactly how its first version scored
                // a false pass against a deliberately re-introduced desync. On this
                // clip 8x8 wins by 6-14%; require a clear margin over pure noise.
                let shrink = 1.0 - s8.len() as f64 / s4.len() as f64;
                assert!(
                    shrink > 0.01,
                    "8x8 {tag}: 8x8 stream is only {:.2}% smaller than 4x4 — the 8x8                      transform is not being selected, so this case tests nothing",
                    shrink * 100.0
                );
                let d8 = decode_all(&s8, &format!("8x8 {tag}"));
                assert_eq!(d8.len(), 6, "8x8 {tag}: frame count");

                // A desync does not always error -- it can also decode to the wrong
                // pixels. The 8x8 arm must land near the 4x4 arm of the same config:
                // the transform choice is a modest RD decision, not a different
                // picture. A parse desync moves this by tens of dB.
                let d4 = decode_all(&s4, "4x4 ref");
                for (i, (a, b)) in d8.iter().zip(&d4).enumerate() {
                    let se: u64 = a
                        .y
                        .iter()
                        .zip(&b.y)
                        .map(|(&p, &q)| {
                            let d = p as i64 - q as i64;
                            (d * d) as u64
                        })
                        .sum();
                    let mse = se as f64 / a.y.len() as f64;
                    let psnr = if mse == 0.0 {
                        99.0
                    } else {
                        10.0 * (255.0f64 * 255.0 / mse).log10()
                    };
                    assert!(
                        psnr > 25.0,
                        "8x8 {tag} frame{i}: 8x8 vs 4x4 decode only {psnr:.1} dB apart \
                         — that is a desync, not a transform-size decision"
                    );
                }
            }
        }
    }
}

/// The encoder must ACCEPT High + 8x8 + CABAC. Pinned separately from the round-trip
/// so that re-adding a blanket refusal fails loudly here rather than silently
/// reducing the matrix above to the CAVLC half.
#[test]
fn high_profile_8x8_is_accepted_on_both_coders() {
    for &cabac in &[false, true] {
        for &bframes in &[0u32, 2] {
            let mut cfg = EncoderConfig::new(64, 48);
            cfg.profile = Profile::High;
            cfg.transform_8x8 = true;
            cfg.cabac = cabac;
            cfg.bframes = bframes;
            assert!(
                Encoder::new(cfg).is_ok(),
                "High + 8x8 must be accepted (cabac={cabac}, bframes={bframes})"
            );
        }
    }
}
