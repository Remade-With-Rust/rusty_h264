//! CABAC I-slice conformance gate (the bringup-encoder verification spine).
//!
//! For each QP × content, encode an all-intra clip TWICE — once CABAC, once CAVLC —
//! and decode both with our (independently-validated, ffmpeg-conformant) decoder.
//! Two invariants must hold:
//!   1. The CABAC stream decodes cleanly (no error) — the syntax is legal.
//!   2. decode(CABAC) == decode(CAVLC), pixel-exact — because both paths share
//!      `plan_mb` (identical mode decision / transform / reconstruction), the CABAC
//!      entropy layer is a lossless re-encoding, so the reconstructions must match.
//! And CABAC must not be larger than CAVLC (it re-codes the same data more tightly).
//!
//! (Cross-checked pixel-exact against ffmpeg's decoder in the bring-up matrix; this
//! CI test uses our decoder so it needs no external dependency.)

use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig};
use rusty_h264_common::{Profile, YuvFrame};

/// Deterministic textured + moving frame (defeats trivial prediction so residuals,
/// significance maps, and multi-bin coeff levels are all exercised).
fn frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            fr.y[y * w + x] = ((x as u64 * 3 + y as u64 * 5 + f * 11)
                ^ ((x as u64 >> 2) * (y as u64 >> 1))) as u8;
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            let v = (128 + (x as i64 * 2 - y as i64 * 3 + f as i64 * 7)) as u8;
            fr.u[y * cw + x] = v;
            fr.v[y * cw + x] = v.wrapping_add(17);
        }
    }
    fr
}

fn encode_clip(w: usize, h: usize, qp: u8, cabac: bool, nframes: u64) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = 1; // all-intra
    cfg.cabac = cabac;
    if cabac {
        cfg.profile = Profile::Main;
    }
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut out = Vec::new();
    for f in 0..nframes {
        out.extend_from_slice(&enc.encode(&frame(w, h, f)));
    }
    out
}

fn decode_all(stream: &[u8]) -> Vec<YuvFrame> {
    let mut dec = Decoder::new();
    dec.decode_stream(stream).expect("decode")
}

#[test]
fn cabac_intra_roundtrips_and_matches_cavlc() {
    let (w, h) = (96, 64); // a few MBs wide/tall, so neighbour contexts are exercised
    for &qp in &[6u8, 18, 26, 34, 44] {
        let cabac = encode_clip(w, h, qp, true, 3);
        let cavlc = encode_clip(w, h, qp, false, 3);

        // 1 + 2: CABAC decodes, and to the SAME pixels as CAVLC (shared plan_mb).
        let df_cabac = decode_all(&cabac);
        let df_cavlc = decode_all(&cavlc);
        assert_eq!(df_cabac.len(), 3, "qp{qp}: CABAC frame count");
        assert_eq!(df_cavlc.len(), 3, "qp{qp}: CAVLC frame count");
        for (i, (a, b)) in df_cabac.iter().zip(&df_cavlc).enumerate() {
            assert_eq!(a.y, b.y, "qp{qp} frame{i}: luma CABAC != CAVLC");
            assert_eq!(a.u, b.u, "qp{qp} frame{i}: Cb CABAC != CAVLC");
            assert_eq!(a.v, b.v, "qp{qp} frame{i}: Cr CABAC != CAVLC");
        }

        // CABAC re-codes the same data more tightly — never larger than CAVLC.
        assert!(
            cabac.len() <= cavlc.len(),
            "qp{qp}: CABAC {} should not exceed CAVLC {}",
            cabac.len(),
            cavlc.len()
        );
    }
}
