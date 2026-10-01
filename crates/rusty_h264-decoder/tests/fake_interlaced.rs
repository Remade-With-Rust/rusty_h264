// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `frame_mbs_only_flag == 0` without field coding (x264 `fake-interlaced`).
//!
//! The flag only permits field coding. x264's fake-interlaced (required for Blu-ray
//! 1080p25/30) sets it while coding every picture as a progressive frame, so such a
//! stream decodes like any other, with two differences in the SPS arithmetic: the height
//! is counted in field map units (doubled), and the vertical crop unit is 4. The vector is
//! x264 (128x48 testsrc2, two B-frames, `fake-interlaced=1`: 2 map units, so 4 macroblock
//! rows, cropped by 4 units of 4 rows); the reference is ffmpeg 8.1's decode. The pre-fix
//! decoder refused it at the SPS.
//! MBAFF (x264 `interlaced=1`) really is interlace coding and is still refused.

use rusty_h264_decoder::{DecodeError, Decoder};

#[test]
fn fake_interlaced_decodes_like_progressive() {
    let stream = include_bytes!("../../../tests/cabac_data/fake_interlaced.264");
    let reference = include_bytes!("../../../tests/cabac_data/fake_interlaced_ref.yuv");
    let frames = Decoder::new().decode_stream(stream).expect("decode");
    assert_eq!(frames.len(), 6);
    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (128, 48));
        ours.extend_from_slice(&f.y);
        ours.extend_from_slice(&f.u);
        ours.extend_from_slice(&f.v);
    }
    assert!(
        ours == reference.as_slice(),
        "fake-interlaced decode differs from ffmpeg"
    );
}

#[test]
fn mbaff_is_still_refused() {
    let stream = include_bytes!("../../../tests/cabac_data/mbaff.264");
    match Decoder::new().decode_stream(stream) {
        Err(DecodeError::Unsupported(what)) => assert!(what.contains("MBAFF"), "{what}"),
        other => panic!(
            "expected Unsupported(MBAFF), got {:?}",
            other.map(|f| f.len())
        ),
    }
}
