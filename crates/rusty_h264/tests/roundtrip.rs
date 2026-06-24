//! End-to-end: encode with rusty_h264, decode with rusty_h264, and check the
//! reconstruction is faithful. Generation 2 codes I_16x16 (DC-predicted,
//! transform + CAVLC), so this is *lossy* — we assert a high PSNR rather than
//! bit-exact equality. (Bit-exact agreement with a reference decoder is
//! verified separately against ffmpeg.)

use rusty_h264::{Decoder, Encoder, EncoderConfig, YuvFrame};

/// Deterministic gradient + texture so we exercise real residuals.
fn make_frame(width: usize, height: usize) -> YuvFrame {
    let cw = width / 2;
    let ch = height / 2;
    let mut y = vec![0u8; width * height];
    for j in 0..height {
        for i in 0..width {
            y[j * width + i] = ((i * 7 + j * 13 + i * j / 3) % 256) as u8;
        }
    }
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for j in 0..ch {
        for i in 0..cw {
            u[j * cw + i] = ((i * 3 + j * 5) % 256) as u8;
            v[j * cw + i] = ((i * 11 + j * 2) % 256) as u8;
        }
    }
    YuvFrame { width, height, y, u, v }
}

/// Luma PSNR in dB between two equal-size frames.
fn luma_psnr(a: &YuvFrame, b: &YuvFrame) -> f64 {
    let n = a.y.len();
    let mse: f64 = a
        .y
        .iter()
        .zip(&b.y)
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum::<f64>()
        / n as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

fn assert_faithful(width: usize, height: usize, min_psnr: f64) {
    let frame = make_frame(width, height);
    let mut enc = Encoder::new(EncoderConfig::new(width, height)).unwrap();
    let bitstream = enc.encode(&frame);

    let mut dec = Decoder::new();
    let decoded = dec.decode(&bitstream).expect("decode ok").expect("a frame");

    assert_eq!(decoded.width, width);
    assert_eq!(decoded.height, height);
    let psnr = luma_psnr(&frame, &decoded);
    assert!(
        psnr >= min_psnr,
        "{width}x{height}: luma PSNR {psnr:.1} dB below {min_psnr} dB"
    );
}

#[test]
fn faithful_aligned_16() {
    assert_faithful(64, 48, 30.0);
}

#[test]
fn faithful_cropped_dims() {
    // Non-multiple-of-16 dims exercise SPS cropping + edge macroblocks.
    assert_faithful(50, 34, 30.0);
}

#[test]
fn faithful_small() {
    assert_faithful(16, 16, 30.0);
}

#[test]
fn faithful_wide() {
    assert_faithful(176, 32, 30.0);
}

#[test]
fn deterministic_output() {
    // Same input must always produce the same bitstream.
    let frame = make_frame(48, 48);
    let mut e1 = Encoder::new(EncoderConfig::new(48, 48)).unwrap();
    let mut e2 = Encoder::new(EncoderConfig::new(48, 48)).unwrap();
    assert_eq!(e1.encode(&frame), e2.encode(&frame));
}

#[test]
fn lower_qp_is_higher_quality() {
    let frame = make_frame(64, 64);
    let psnr = |qp: u8| {
        let mut cfg = EncoderConfig::new(64, 64);
        cfg.qp = qp;
        let mut enc = Encoder::new(cfg).unwrap();
        let bs = enc.encode(&frame);
        let mut dec = Decoder::new();
        luma_psnr(&frame, &dec.decode(&bs).unwrap().unwrap())
    };
    assert!(psnr(20) > psnr(40), "lower QP should give higher PSNR");
}
