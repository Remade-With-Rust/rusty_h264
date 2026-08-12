//! CABAC I_PCM conformance gate.
//!
//! The vector is x264 (CABAC, qp 0) over per-frame random noise, which makes
//! x264 code EVERY macroblock as I_PCM — 1 I-slice + 5 P-slices, so both the
//! I-slice mb_type path and the P-slice path (mbt 30) hit the PCM arm, and the
//! engine's byte-realign + §9.3.1.2 re-init runs 96 times at effectively random
//! bit phases (any position error desyncs the very next mb_type parse).
//! The reference YUV is ffmpeg 8.1's decode of the same stream.
//!
//! This gate exists because the decoder shipped `decode_ipcm` wired only into
//! the CAVLC reader for months while all three CABAC entry points returned
//! `Unsupported("CABAC I_PCM (WIP)")` — a refusal, not a missing feature.

use rusty_h264_decoder::Decoder;

#[test]
fn cabac_ipcm_ip_matches_ffmpeg() {
    let stream = include_bytes!("../../../tests/cabac_data/cabac_ipcm_ip.264");
    let reference = include_bytes!("../../../tests/cabac_data/cabac_ipcm_ip_ref.yuv");

    let frames = Decoder::new().decode_stream(stream).expect("decode");
    assert_eq!(frames.len(), 6, "1 I + 5 P frames");

    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (64, 64));
        ours.extend_from_slice(&f.y);
        ours.extend_from_slice(&f.u);
        ours.extend_from_slice(&f.v);
    }
    assert_eq!(ours.len(), reference.len());
    // Byte-exact: PCM samples are lossless, so any engine mis-position or
    // context damage shows as a hard mismatch, not a tolerance question.
    assert!(ours == reference.as_slice(), "PCM recon differs from ffmpeg");
}
