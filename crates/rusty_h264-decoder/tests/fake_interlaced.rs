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

/// A field picture (`field_pic_flag == 1`) is refused per slice with an error,
/// never decoded as a frame and never a panic -- through `decode_stream` and
/// through per-access-unit `decode`.
///
/// The vector is `fake_interlaced.264` with ONE bit set: `field_pic_flag` in the
/// first IDR slice header (NAL bit 21, right after the 4-bit `frame_num`; ffmpeg's
/// `trace_headers` then reads it as a bottom field). Nothing after the flag is
/// consistent with field coding, which is the point: the decoder must stop at the
/// flag rather than parse on. Real field-coded streams (JVT conformance
/// CVFI1_Sony_D, all field pictures; CVPA1_TOSHIBA_B, PAFF) were checked to be
/// refused the same way when this test was written; they are too large to vendor.
#[test]
fn field_picture_is_refused() {
    let stream = include_bytes!("../../../tests/cabac_data/field_pic.264");
    match Decoder::new().decode_stream(stream) {
        Err(DecodeError::Unsupported(what)) => {
            assert!(what.contains("field picture"), "{what}")
        }
        other => panic!(
            "expected Unsupported(field picture), got {:?}",
            other.map(|f| f.len())
        ),
    }

    let mut dec = Decoder::new();
    let aus = rusty_h264_decoder::split_access_units(stream);
    match dec.decode(aus[0]) {
        Err(DecodeError::Unsupported(what)) => {
            assert!(what.contains("field picture"), "{what}")
        }
        other => panic!(
            "expected Unsupported(field picture), got {:?}",
            other.map(|f| f.is_some())
        ),
    }
    // The rest of the stream references the refused picture; it must keep
    // returning errors (or nothing), not panic and not invent frames.
    for au in &aus[1..] {
        assert!(!matches!(dec.decode(au), Ok(Some(_))));
    }
}
