// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Dev tools also accumulate fields and helpers kept for the NEXT investigation;
// dead code here is a scratchpad, not a defect.
#![allow(dead_code, unused)]
#![allow(clippy::unnecessary_unwrap, clippy::zombie_processes)]
//! Multi-slice + `disable_deblocking_filter_idc == 2` conformance gate.
//!
//! The vector is x264 (`--slices 4`, CABAC, 128x128, I+P+B) with every slice
//! header bit-patched from idc 0 to idc 2 (x264 cannot emit idc 2), verified
//! field-exact by ffmpeg's trace_headers. It exercises BOTH 2026-08-12 fixes
//! at once: per-slice CABAC neighbour availability (every slice after the
//! first) and the idc==2 cross-slice-edge bS suppression. The reference YUV is
//! ffmpeg 8.1's decode. The pre-fix decoder DIFFed on this stream (two-sided
//! gate: the vector provably distinguishes).

use rusty_h264_decoder::Decoder;

#[test]
fn multislice_idc2_matches_ffmpeg() {
    let stream = include_bytes!("../../../tests/cabac_data/cabac_mslice_idc2.264");
    let reference = include_bytes!("../../../tests/cabac_data/cabac_mslice_idc2_ref.yuv");

    let frames = Decoder::new().decode_stream(stream).expect("decode");
    assert_eq!(frames.len(), 6);

    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (128, 128));
        ours.extend_from_slice(&f.y);
        ours.extend_from_slice(&f.u);
        ours.extend_from_slice(&f.v);
    }
    assert!(ours == reference.as_slice(), "multi-slice idc2 recon differs from ffmpeg");
}
