// Dev/test target, not shipped: a panic here IS the diagnostic (that is what an
// assertion is). The workspace's unwrap/expect/panic denials exist to keep them
// off the decoder's untrusted-input path, so they are relaxed for this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Loop-filter-off P pictures must reconstruct their trailing P_Skip span (#15).
//!
//! A slice that ends in a run of skipped macroblocks leaves that run pending as a
//! deferred span. Only `deblock` flushed it, and finalization calls `deblock`
//! only when the filter is on, so with `disable_deblocking_filter_idc == 1` the
//! span was never reconstructed: zero samples in the bottom macroblock row,
//! which later P pictures then copied forward. x264 also writes idc 1 by itself
//! when QP is low enough that the filter would do nothing, which is why the
//! issue saw the same failure at QP <= 15 with deblocking "on".
//!
//! The vector is x264 via ffmpeg 8.1, 128x32, six frames, IPPPPP, CAVLC:
//! `ffmpeg -f lavfi -i testsrc2=s=128x32:r=24:d=0.25 -c:v libx264 -qp 24 -g 48
//! -bf 0 -x264-params no-deblock=1:cabac=0 -bsf:v h264_mp4toannexb -f h264`.
//! The reference YUV is ffmpeg 8.1's decode. Before the fix, 5 of the 6 frames
//! differed in macroblock row 1 (two-sided gate: the vector provably
//! distinguishes).

use rusty_h264_decoder::Decoder;

#[test]
fn nodeblock_skip_tail_matches_ffmpeg() {
    let stream = include_bytes!("../../../tests/nodeblock_data/nodeblock_skip_tail.264");
    let reference = include_bytes!("../../../tests/nodeblock_data/nodeblock_skip_tail_ref.yuv");

    let frames = Decoder::new().decode_stream(stream).expect("decode");
    assert_eq!(frames.len(), 6);

    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (128, 32));
        ours.extend_from_slice(&f.y);
        ours.extend_from_slice(&f.u);
        ours.extend_from_slice(&f.v);
    }
    assert!(
        ours == reference.as_slice(),
        "loop-filter-off recon differs from ffmpeg"
    );
}
