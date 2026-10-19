// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! High 10 through [`HbdDecoder`]: every picture byte-identical to ffmpeg's.
//!
//! The vectors are x264 High 10 (`-profile:v high10 -pix_fmt yuv420p10le`), 64x48
//! testsrc2, 8 frames in one IDR period, `-crf 30`; the references are ffmpeg 8.1's
//! decodes as `yuv420p10le` (little-endian 16-bit samples, display order):
//!
//! - `high10_cabac_b`: CABAC, 3 B-frames with B-pyramid, explicit weighted P,
//!   implicit weighted B, spatial direct, 8x8 transform.
//! - `high10_cavlc_temporal_cqm`: CAVLC, 2 B-frames with both spatial and temporal
//!   direct, scaling matrices in the PPS (x264 `cqm=jvt`).
//! - `high10_two_slices`: CABAC, 2 B-frames, two slices per picture.
//!
//! The 8-bit [`rusty_h264_decoder::Decoder`] refuses all three (bit depth 10).

use rusty_h264_decoder::{split_access_units, HbdDecoder};

const W: usize = 64;
const H: usize = 48;

fn check(stream: &[u8], reference: &[u8], name: &str) {
    let mut decoder = HbdDecoder::new();
    let mut frames = Vec::new();
    for unit in split_access_units(stream) {
        if let Some(frame) = decoder.decode(unit).expect(name) {
            frames.push(frame);
        }
    }
    // Pictures come back in decoding order; one IDR period, so POC order is display order.
    frames.sort_by_key(|f| f.poc);
    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (W, H), "{name}: size");
        assert_eq!(
            (f.bit_depth_luma, f.bit_depth_chroma),
            (10, 10),
            "{name}: depth"
        );
        for plane in [&f.y, &f.u, &f.v] {
            for sample in plane.iter() {
                ours.extend_from_slice(&sample.to_le_bytes());
            }
        }
    }
    assert_eq!(ours.len(), reference.len(), "{name}: picture count");
    let size = W * H * 3 / 2 * 2;
    for (k, (a, b)) in ours.chunks(size).zip(reference.chunks(size)).enumerate() {
        assert!(a == b, "{name}: picture {k} differs from ffmpeg");
    }
}

#[test]
fn cabac_b_pyramid_weighted_prediction() {
    check(
        include_bytes!("../../../tests/cabac_data/high10_cabac_b.264"),
        include_bytes!("../../../tests/cabac_data/high10_cabac_b_ref.yuv"),
        "CABAC B-pyramid",
    );
}

#[test]
fn cavlc_temporal_direct_pps_scaling_matrices() {
    check(
        include_bytes!("../../../tests/cabac_data/high10_cavlc_temporal_cqm.264"),
        include_bytes!("../../../tests/cabac_data/high10_cavlc_temporal_cqm_ref.yuv"),
        "CAVLC temporal direct + cqm",
    );
}

#[test]
fn two_slices_per_picture() {
    check(
        include_bytes!("../../../tests/cabac_data/high10_two_slices.264"),
        include_bytes!("../../../tests/cabac_data/high10_two_slices_ref.yuv"),
        "two slices",
    );
}

#[test]
fn the_8_bit_decoder_still_refuses_high_10() {
    let stream = include_bytes!("../../../tests/cabac_data/high10_cabac_b.264");
    let mut decoder = rusty_h264_decoder::Decoder::new();
    let first = split_access_units(stream)[0];
    assert!(
        decoder.decode(first).is_err(),
        "bit depth 10 must be declined"
    );
}
