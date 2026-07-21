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

/// A smoothly-panning textured frame (B-favorable: bi-prediction + direct predict
/// it well, so the B decision exercises Direct/Skip/L0/L1/Bi).
fn pan_frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    let s = 3 * f as i64;
    for y in 0..h {
        for x in 0..w {
            let xx = x as i64 - s;
            fr.y[y * w + x] = ((xx * 3 + y as i64 * 5) ^ ((xx >> 2) * (y as i64 >> 1))) as u8;
        }
    }
    fr
}

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
    encode_clip_gop(w, h, qp, cabac, nframes, 1)
}

fn encode_clip_gop(w: usize, h: usize, qp: u8, cabac: bool, nframes: u64, gop: u32) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = gop;
    cfg.cabac = cabac;
    // These round-trip tests assert decode(CABAC) == decode(CAVLC), which requires
    // the SHARED plan (identical recon). RDOQ (default-on for CABAC I-slices) changes
    // the CABAC I-slice levels, so disable it here to test the pure entropy layer;
    // RDOQ has its own decode gate below.
    cfg.cabac_rdoq = 0.0;
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

#[test]
fn cabac_p_roundtrips_and_matches_cavlc() {
    // gop 2 → frame 0 IDR, then P-frames (exercises mb_skip / mb_type_p / mvd /
    // inter residual / intra-in-P). The `frame(f)` motion drives real mvds.
    let (w, h) = (96, 64);
    for &qp in &[8u8, 20, 30, 42] {
        let cabac = encode_clip_gop(w, h, qp, true, 5, 2);
        let cavlc = encode_clip_gop(w, h, qp, false, 5, 2);
        let df_cabac = decode_all(&cabac);
        let df_cavlc = decode_all(&cavlc);
        assert_eq!(df_cabac.len(), 5, "qp{qp}: CABAC frame count");
        for (i, (a, b)) in df_cabac.iter().zip(&df_cavlc).enumerate() {
            assert_eq!(a.y, b.y, "qp{qp} frame{i}: P luma CABAC != CAVLC");
            assert_eq!(a.u, b.u, "qp{qp} frame{i}: P Cb CABAC != CAVLC");
            assert_eq!(a.v, b.v, "qp{qp} frame{i}: P Cr CABAC != CAVLC");
        }
        assert!(cabac.len() <= cavlc.len(), "qp{qp}: P CABAC {} > CAVLC {}", cabac.len(), cavlc.len());
    }
}

#[test]
fn cabac_rdoq_decodes_and_shrinks() {
    // CABAC with RDOQ default-on (I-slices): must decode cleanly in our (ffmpeg-
    // conformant) decoder, and — since trellis quantization RD-optimizes the I-slice
    // levels — not be larger than RDOQ-off. All-intra so every frame exercises it.
    let (w, h) = (96, 64);
    for &qp in &[20u8, 32] {
        let mut on = EncoderConfig::new(w, h);
        on.qp = qp;
        on.gop_size = 1;
        on.cabac = true;
        on.profile = Profile::Main; // cabac_rdoq default 8.0 (on)
        let mut off = on.clone();
        off.cabac_rdoq = 0.0;
        let frames: Vec<YuvFrame> = (0..3).map(|f| frame(w, h, f)).collect();
        let enc_on = |cfg: EncoderConfig| -> Vec<u8> {
            let mut e = Encoder::new(cfg).expect("enc");
            frames.iter().flat_map(|f| e.encode(f)).collect()
        };
        let s_on = enc_on(on);
        let s_off = enc_on(off);
        let d = decode_all(&s_on);
        assert_eq!(d.len(), 3, "qp{qp}: RDOQ stream decoded frame count");
        assert!(
            s_on.len() <= s_off.len(),
            "qp{qp}: RDOQ {} should not exceed non-RDOQ {}",
            s_on.len(),
            s_off.len()
        );
    }
}

#[test]
fn transform_8x8_intra_decodes() {
    // High-profile 8x8 transform (I_8x8, CAVLC): the encoder picks per-MB between
    // I_16x16 / I_4x4 / I_8x8 by RD. Must decode cleanly in our (ffmpeg-conformant)
    // decoder — exercising the High SPS/PPS, transform_size_8x8_flag, and the 8x8
    // CAVLC residual (four interleaved 4x4 sub-blocks).
    let (w, h) = (96, 64);
    for &qp in &[18u8, 30] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 1; // all-intra
        cfg.preset = rusty_h264_encoder::Preset::Quality;
        cfg.transform_8x8 = true;
        cfg.profile = Profile::High;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let stream: Vec<u8> = (0..3).flat_map(|f| enc.encode(&frame(w, h, f))).collect();
        let decoded = decode_all(&stream);
        assert_eq!(decoded.len(), 3, "qp{qp}: 8x8 decoded frame count");
    }
}

#[test]
fn transform_8x8_inter_decodes() {
    // High-profile 8x8 transform on P-frames (transform_size_8x8_flag on inter MBs,
    // CAVLC). The encoder RD-picks per MB between the 4x4 and 8x8 luma residual
    // transform; the 8x8 residual is coded as four interleaved 4x4 sub-blocks. Must
    // decode cleanly in our (ffmpeg-conformant) decoder — exercising the inter t8x8
    // flag (after cbp) + the 8x8 inter residual path. gop 4 → I then P-frames with
    // real motion from `frame(f)`.
    let (w, h) = (96, 64);
    for &qp in &[20u8, 32] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 4;
        cfg.preset = rusty_h264_encoder::Preset::Quality;
        cfg.transform_8x8 = true;
        cfg.profile = Profile::High;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let stream: Vec<u8> = (0..6).flat_map(|f| enc.encode(&frame(w, h, f))).collect();
        let decoded = decode_all(&stream);
        assert_eq!(decoded.len(), 6, "qp{qp}: inter-8x8 decoded frame count");
    }
}

/// A frame with a STATIC textured left half (heavily referenced across the GOP →
/// high mb-tree propagation) and a per-frame changing right half (low propagation).
/// The spatial contrast makes mb-tree's per-MB QP offsets non-uniform, so it
/// measurably changes the stream (uniform-noise content backs off to ~zero offsets).
fn split_frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            fr.y[y * w + x] = if x < w / 2 {
                ((x as u64 * 7) ^ (y as u64 * 13)) as u8 // static → referenced
            } else {
                ((x as u64 * 3 + y as u64 * 5 + f * 29) ^ (x as u64 * y as u64)) as u8 // changing
            };
        }
    }
    fr
}

#[test]
fn mbtree_decodes_and_off_is_byte_identical() {
    // Macroblock-tree temporal AQ: a per-GOP source lookahead lowers QP on
    // heavily-referenced MBs. It only moves per-MB QP (always legal), so the stream
    // stays conformant; this gate confirms it decodes cleanly in our (ffmpeg-
    // conformant) decoder, and that OFF is byte-identical to a plain encode (the
    // feature is a pure opt-in). Uses the batch path (mb-tree needs the GOP's frames).
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..8).map(|f| split_frame(w, h, f)).collect();
    for &qp in &[22u8, 32] {
        let mut on = EncoderConfig::new(w, h);
        on.qp = qp;
        on.gop_size = 8;
        on.mbtree = true;
        let mut off = on.clone();
        off.mbtree = false;

        let s_on: Vec<Vec<u8>> = Encoder::new(on).expect("enc").encode_all(&frames).expect("on");
        let s_off: Vec<Vec<u8>> = Encoder::new(off.clone()).expect("enc").encode_all(&frames).expect("off");
        // mb-tree changed the QP allocation → the stream differs from plain.
        assert_ne!(s_on.concat(), s_off.concat(), "qp{qp}: mb-tree should change the stream");
        // OFF must equal a from-scratch non-mb-tree encode (pure opt-in).
        let s_plain: Vec<Vec<u8>> = Encoder::new(off).expect("enc").encode_all(&frames).expect("plain");
        assert_eq!(s_off.concat(), s_plain.concat(), "qp{qp}: mb-tree OFF must be byte-identical");
        // The mb-tree stream decodes cleanly, every frame.
        let decoded = decode_all(&s_on.concat());
        assert_eq!(decoded.len(), 8, "qp{qp}: mb-tree decoded frame count");
    }
}

#[test]
fn cabac_b_slices_decode() {
    // B-slice CABAC (I + P + B) via the encode_all reorder path. The stream is
    // conformant vs ffmpeg (checked in bring-up); this CI gate confirms every frame
    // decodes cleanly in our (ffmpeg-conformant) decoder and the count is right.
    let (w, h) = (96, 64);
    for &qp in &[16u8, 30] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 12;
        cfg.bframes = 2;
        cfg.cabac = true;
        cfg.profile = Profile::Main;
        let enc = Encoder::new(cfg).expect("encoder");
        let frames: Vec<YuvFrame> = (0..9).map(|f| pan_frame(w, h, f)).collect();
        let aus = enc.encode_all(&frames).expect("encode_all");
        let stream: Vec<u8> = aus.concat();
        let decoded = decode_all(&stream);
        assert_eq!(decoded.len(), 9, "qp{qp}: B-CABAC decoded frame count");
    }
}
