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
