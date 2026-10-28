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

/// Decode `stream` and require it to match ffmpeg's decode byte-for-byte.
fn assert_matches_ffmpeg(stream: &[u8], reference: &[u8], w: usize, h: usize, n: usize) {
    let frames = Decoder::new().decode_stream(stream).expect("decode");
    assert_eq!(frames.len(), n);

    let mut ours = Vec::with_capacity(reference.len());
    for f in &frames {
        assert_eq!((f.width, f.height), (w, h));
        ours.extend_from_slice(&f.y);
        ours.extend_from_slice(&f.u);
        ours.extend_from_slice(&f.v);
    }
    assert!(ours == reference, "loop-filter-off recon differs from ffmpeg");
}

#[test]
fn nodeblock_skip_tail_matches_ffmpeg() {
    assert_matches_ffmpeg(
        include_bytes!("../../../tests/nodeblock_data/nodeblock_skip_tail.264"),
        include_bytes!("../../../tests/nodeblock_data/nodeblock_skip_tail_ref.yuv"),
        128,
        32,
        6,
    );
}

/// The issue's "deblocking ON, QP <= 15" case. Nothing asks for the filter to
/// be off here; x264 writes `disable_deblocking_filter_idc = 1` on every slice
/// by itself because at QP 12 alpha is 0 and the filter would be a no-op. So
/// the stream takes the same filter-off finalization path.
///
/// x264 via ffmpeg 8.1.2, 64x48, six frames, IPPPPP, High/CABAC, one slice:
/// `ffmpeg -f lavfi -i testsrc2=s=64x48:r=24:d=0.25 -c:v libx264 -qp 12 -g 48
/// -bf 0 -bsf:v h264_mp4toannexb -f h264`. Reference is ffmpeg 8.1.2's decode.
/// Before the fix, 5 of 6 frames differed (macroblock row 2).
#[test]
fn low_qp_auto_filter_off_matches_ffmpeg() {
    assert_matches_ffmpeg(
        include_bytes!("../../../tests/nodeblock_data/lowqp_auto_idc1.264"),
        include_bytes!("../../../tests/nodeblock_data/lowqp_auto_idc1_ref.yuv"),
        64,
        48,
        6,
    );
}

/// Multi-slice P pictures (a second reporter hit #15 on a Constrained Baseline
/// stream with two slices per frame). Only the LAST slice's trailing skip run
/// was ever left pending -- an earlier slice's run is flushed when the next
/// slice starts -- so this gates that the fix is not single-slice-only.
///
/// x264 via ffmpeg 8.1.2, 64x48, six frames, IPPPPP, Baseline/CAVLC, two
/// slices per picture (first_mb 0 and 8), QP 12 (filter off on every slice):
/// `ffmpeg -f lavfi -i testsrc2=s=64x48:r=24:d=0.25 -c:v libx264 -profile:v
/// baseline -qp 12 -g 48 -bf 0 -x264-params slices=2 -bsf:v h264_mp4toannexb
/// -f h264`. Before the fix, 5 of 6 frames differed (macroblock row 2).
#[test]
fn multi_slice_filter_off_matches_ffmpeg() {
    assert_matches_ffmpeg(
        include_bytes!("../../../tests/nodeblock_data/mslice_baseline.264"),
        include_bytes!("../../../tests/nodeblock_data/mslice_baseline_ref.yuv"),
        64,
        48,
        6,
    );
}
