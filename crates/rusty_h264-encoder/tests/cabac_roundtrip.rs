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
    // Same reason, second knob: `cabac_lambda_scale` scales the ME rate term for
    // the CABAC path ONLY (shipped default 1.25), so the two coders would search
    // to DIFFERENT motion vectors and the reconstructions would legitimately
    // differ. Pin it to CAVLC's 1.0 so this test keeps asserting what it was built
    // to assert -- that the ENTROPY LAYER alone does not change reconstruction --
    // rather than silently becoming a lambda-calibration test.
    // The shipped lambda has its own gate: `cabac_shipped_me_lambda_roundtrips`.
    cfg.cabac_lambda_scale = 1.0;
    // NOTE: no `profile = Main` override for the CABAC arm. It used to be needed
    // because CABAC is illegal in Baseline and the default profile was Main; the
    // default is now High, which carries CABAC fine. Keeping the override made the
    // arms ASYMMETRIC once the 8x8 transform became default-on and clamps to the
    // profile: the CABAC arm would drop to 4x4 while the CAVLC arm kept 8x8, and
    // this test's whole premise is that the two arms share `plan_mb`.
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut out = Vec::new();
    for f in 0..nframes {
        out.extend_from_slice(&enc.encode(&frame(w, h, f)));
    }
    out.extend_from_slice(&enc.flush());
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
fn cabac_shipped_me_lambda_roundtrips() {
    // The parity test above pins `cabac_lambda_scale` to 1.0 so it can compare the
    // two entropy coders' reconstructions. That leaves the SHIPPED value untested,
    // which is how a default silently stops being exercised. This gates it: encode
    // P-frames with the real default and require a clean round-trip through our
    // (ffmpeg-conformant) decoder.
    //
    // It deliberately does NOT assert CABAC <= CAVLC in size: with different ME
    // lambdas the two search to different motion vectors, so a size ordering
    // between them is not a property either one owes.
    let (w, h) = (96, 64);
    for &qp in &[8u8, 20, 30, 42] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = qp;
        cfg.gop_size = 2;
        cfg.cabac = true;
        cfg.profile = Profile::Main;
        assert_ne!(
            cfg.cabac_lambda_scale, 0.0,
            "shipped ME lambda scale must be set; this test exists to exercise it"
        );
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut out = Vec::new();
        for f in 0..5u64 {
            out.extend_from_slice(&enc.encode(&frame(w, h, f)));
        }
        out.extend_from_slice(&enc.flush());
        let d = decode_all(&out);
        assert_eq!(d.len(), 5, "qp{qp}: shipped-lambda CABAC frame count");
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
            { let mut v: Vec<u8> = frames.iter().flat_map(|f| e.encode(f)).collect(); v.extend_from_slice(&e.flush()); v }
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
        // The 8x8 transform is High-profile CAVLC (our decoder has no CABAC 8x8), so
        // it must opt OUT of the now-default CABAC explicitly.
        cfg.cabac = false;
        cfg.transform_8x8 = true;
        cfg.profile = Profile::High;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut stream: Vec<u8> = (0..3).flat_map(|f| enc.encode(&frame(w, h, f))).collect();
        stream.extend_from_slice(&enc.flush());
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
        // The 8x8 transform is High-profile CAVLC (our decoder has no CABAC 8x8), so
        // it must opt OUT of the now-default CABAC explicitly.
        cfg.cabac = false;
        cfg.transform_8x8 = true;
        cfg.profile = Profile::High;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut stream: Vec<u8> = (0..6).flat_map(|f| enc.encode(&frame(w, h, f))).collect();
        stream.extend_from_slice(&enc.flush());
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
fn mbtree_cabac_and_bframes_decode() {
    // mb-tree threads through the CABAC (I/P) and B-frame reorder paths too — the
    // temporal AQ runs over the anchor reference chain (B's are non-reference leaves).
    // Each must decode cleanly in our (ffmpeg-conformant) decoder, and OFF stays
    // byte-identical to the same config without mb-tree.
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..8).map(|f| split_frame(w, h, f)).collect();
    let base = |cabac: bool, bframes: u32| {
        let mut c = EncoderConfig::new(w, h);
        c.qp = 28;
        c.gop_size = 8;
        c.profile = Profile::Main;
        c.cabac = cabac;
        c.bframes = bframes;
        c.mbtree = false; // explicit: the default is ON since 0.5.0
        c
    };
    for &(cabac, bframes) in &[(true, 0u32), (false, 2u32), (true, 2u32)] {
        let mut on = base(cabac, bframes);
        on.mbtree = true;
        // Pin the differentiation latch OFF: this asserts the mb-tree MECHANISM
        // is live, and the 96x64 synthetic has undifferentiated propagation, so
        // the shipped gate correctly abstains on it. The gate itself is covered
        // by `mbtree_spread_latch_is_mbtree_off`.
        on.mbtree_spread_min = 0.0;
        let off = base(cabac, bframes);
        let s_on: Vec<u8> = Encoder::new(on).expect("enc").encode_all(&frames).expect("on").concat();
        let s_off: Vec<u8> = Encoder::new(off).expect("enc").encode_all(&frames).expect("off").concat();
        assert_eq!(decode_all(&s_on).len(), 8, "cabac={cabac} b={bframes}: mb-tree decodes");
        // For B-frame streams the reorder means decode_all yields display-ordered
        // frames; the count is what matters here (conformance checked vs ffmpeg).
        assert_ne!(s_on, s_off, "cabac={cabac} b={bframes}: mb-tree should change the stream");
    }
}

#[test]
fn mbtree_rate_control_decodes_and_off_identical() {
    // mb-tree in RATE-CONTROL mode: the controller supplies each frame's base QP,
    // mb-tree's per-GOP-centered offsets ride on top (rate-neutral per GOP). Must
    // decode cleanly; OFF must be byte-identical to plain RC (pure opt-in).
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..8).map(|f| split_frame(w, h, f)).collect();
    let base = || {
        let mut c = EncoderConfig::new(w, h);
        c.gop_size = 4;
        c.bitrate = 2_000_000;
        c.framerate = 30.0;
        c.mbtree = false; // explicit: the default is ON since 0.5.0
        c
    };
    let mut on = base();
    on.mbtree = true;
    on.mbtree_spread_min = 0.0; // mechanism test — see the note in the sibling test
    let s_on: Vec<u8> = Encoder::new(on).expect("enc").encode_all(&frames).expect("on").concat();
    let s_off: Vec<u8> = Encoder::new(base()).expect("enc").encode_all(&frames).expect("off").concat();
    assert_ne!(s_on, s_off, "mb-tree should change the RC stream");
    let s_plain: Vec<u8> = Encoder::new(base()).expect("enc").encode_all(&frames).expect("plain").concat();
    assert_eq!(s_off, s_plain, "mb-tree OFF must be byte-identical to plain RC");
    assert_eq!(decode_all(&s_on).len(), 8, "RC mb-tree decoded frame count");
}

/// REGRESSION GUARD for the parse/pixel split in `decode_p8x8` (and every other
/// deferred site).
///
/// The E2 seam defers pixel work to a worker. Deferring must skip PIXEL work and
/// never PARSE work — but `decode_p8x8` originally interleaved the two in one
/// loop, so the first attempt at deferring skipped the `mvd` reads, desynced the
/// bitstream, and mis-parsed as a B-slice `sub_mb_type` on a BASELINE stream.
/// That one crashed; a desync landing in range would have produced plausible
/// GARBAGE instead, which no crash test would catch.
///
/// Both seam arms are byte-identical by contract, so this asserts exactly that
/// on content that exercises P_8x8 and intra-in-P (the ordering hazard). It
/// fails loudly on any future edit that lets a skip reach the bitstream.
#[test]
fn edc_seam_arms_are_byte_identical() {
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..10).map(|f| split_frame(w, h, f)).collect();
    for &cabac in &[false, true] {
        let mut c = EncoderConfig::new(w, h);
        c.qp = 27;
        c.gop_size = 5; // several P slices after each IDR
        c.cabac = cabac;
        c.preset = rusty_h264_encoder::Preset::Quality;
        let stream: Vec<u8> =
            Encoder::new(c).expect("enc").encode_all(&frames).expect("encode").concat();

        // The arms are selected inside the decoder by env; set it around each
        // decode. Serialised by `--test-threads` being irrelevant here because
        // both decodes happen back-to-back in this one test.
        std::env::set_var("RS_H264_EDC_MT", "0");
        let inline: Vec<YuvFrame> = decode_all(&stream);
        std::env::set_var("RS_H264_EDC_MT", "1");
        let threaded: Vec<YuvFrame> = decode_all(&stream);
        std::env::remove_var("RS_H264_EDC_MT");

        assert_eq!(inline.len(), threaded.len(), "cabac={cabac}: frame count");
        for (i, (a, b)) in inline.iter().zip(&threaded).enumerate() {
            assert_eq!(a.y, b.y, "cabac={cabac} frame {i}: LUMA differs between seam arms");
            assert_eq!(a.u, b.u, "cabac={cabac} frame {i}: U differs between seam arms");
            assert_eq!(a.v, b.v, "cabac={cabac} frame {i}: V differs between seam arms");
        }
    }
}

#[test]
fn mbtree_spread_latch_is_mbtree_off() {
    // The DIFFERENTIATION LATCH (Great Gate P3 item 4): when mb-tree's own
    // NOTE the latch is OFF BY DEFAULT since 2026-08-08; this test pins it on to
    // check its semantics, and is no longer a statement about shipped behaviour.
    // propagation offsets carry no dispersion, it must abstain — and abstaining
    // has to be EXACTLY mb-tree off, not merely close, or the gate is shipping a
    // third behaviour nobody measured. This synthetic is undifferentiated, so a
    // default-configured encode must equal an mb-tree-off encode byte for byte,
    // while the ungated arm (latch pinned to 0) must differ from both.
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..8).map(|f| split_frame(w, h, f)).collect();
    let base = || {
        let mut c = EncoderConfig::new(w, h);
        c.gop_size = 4;
        c.qp = 27;
        c
    };
    let mut gated = base();
    gated.mbtree = true;
    // PIN the latch. This arm used to read `mbtree_spread_min` from the DEFAULT, so
    // when the default became 0.0 (the latch was audited off on 2026-08-08 — every
    // measured firing was a loss: harbour -0.88%, foreman -1.20%) the "gated" arm
    // silently became the ungated one and this test failed. The invariant below is
    // still worth pinning as a property of the latch WHEN ENABLED; it just must not
    // infer the arm from a default that can move.
    gated.mbtree_spread_min = 1.0 / 0.9;
    let mut off = base();
    off.mbtree = false;
    let mut ungated = base();
    ungated.mbtree = true;
    ungated.mbtree_spread_min = 0.0;
    let enc = |c: EncoderConfig| -> Vec<u8> {
        Encoder::new(c).expect("enc").encode_all(&frames).expect("encode").concat()
    };
    let (s_gated, s_off, s_ungated) = (enc(gated), enc(off), enc(ungated));
    assert_eq!(s_gated, s_off, "a latched-off mb-tree must be byte-identical to mb-tree OFF");
    assert_ne!(s_ungated, s_off, "the ungated arm must actually apply offsets here");
}

#[test]
fn p8x8_subpartitions_decode_and_off_identical() {
    // P_8x8 sub-partition motion: a P macroblock may split into four 8×8 partitions
    // (mb_type 3, four sub_mb_type, four MVs). The encoder RD-picks it per MB vs
    // 16×16/16×8/8×16. Must decode cleanly in our (ffmpeg-conformant) decoder in BOTH
    // CAVLC and CABAC, and OFF must be byte-identical to a plain encode. split_frame
    // (static + moving halves) gives a motion boundary that triggers the split.
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..6).map(|f| split_frame(w, h, f)).collect();
    for &cabac in &[false, true] {
        let mut on = EncoderConfig::new(w, h);
        on.qp = 27;
        on.gop_size = 6;
        on.preset = rusty_h264_encoder::Preset::Quality;
        on.sub_8x8 = Some(true);
        if cabac {
            on.cabac = true;
            on.profile = Profile::Main;
        }
        let mut off = on.clone();
        off.sub_8x8 = Some(false);
        // The Quality preset auto-enables sub_8x8 when the flag is left at its default.
        let mut deflt = on.clone();
        deflt.sub_8x8 = None;

        let s_on: Vec<u8> = Encoder::new(on).expect("enc").encode_all(&frames).expect("on").concat();
        let s_off: Vec<u8> = Encoder::new(off.clone()).expect("enc").encode_all(&frames).expect("off").concat();
        assert_ne!(s_on, s_off, "cabac={cabac}: P_8x8 should change the stream");
        let s_plain: Vec<u8> = Encoder::new(off).expect("enc").encode_all(&frames).expect("plain").concat();
        let s_deflt: Vec<u8> = Encoder::new(deflt).expect("enc").encode_all(&frames).expect("deflt").concat();
        assert_eq!(s_off, s_plain, "cabac={cabac}: sub_8x8 OFF must be deterministic");
        assert_eq!(s_on, s_deflt, "cabac={cabac}: Quality preset must default sub_8x8 ON");
        assert_eq!(decode_all(&s_on).len(), 6, "cabac={cabac}: P_8x8 decoded frame count");
    }
}

#[test]
fn me_wide_decodes_and_off_identical() {
    // Adaptive wide motion search (flat-block grid). It only changes which MVs are
    // chosen — every MV is still coded as a correct mvd — so the stream stays
    // conformant; this gate confirms it decodes cleanly and OFF is byte-identical.
    let (w, h) = (96, 64);
    let frames: Vec<YuvFrame> = (0..6).map(|f| pan_frame(w, h, f)).collect(); // smooth motion → flat blocks
    for &cabac in &[false, true] {
        let mut on = EncoderConfig::new(w, h);
        on.qp = 27;
        on.gop_size = 6;
        on.preset = rusty_h264_encoder::Preset::Quality;
        on.me_wide = Some(true);
        if cabac {
            on.cabac = true;
            on.profile = Profile::Main;
        }
        let mut off = on.clone();
        off.me_wide = Some(false);
        // The Quality preset auto-enables me_wide when the flag is left at its default.
        let mut deflt = on.clone();
        deflt.me_wide = None;
        let s_on: Vec<u8> = Encoder::new(on).expect("enc").encode_all(&frames).expect("on").concat();
        let s_off: Vec<u8> = Encoder::new(off.clone()).expect("enc").encode_all(&frames).expect("off").concat();
        let s_plain: Vec<u8> = Encoder::new(off).expect("enc").encode_all(&frames).expect("plain").concat();
        let s_deflt: Vec<u8> = Encoder::new(deflt).expect("enc").encode_all(&frames).expect("deflt").concat();
        assert_eq!(s_off, s_plain, "cabac={cabac}: me_wide OFF must be deterministic");
        assert_eq!(s_on, s_deflt, "cabac={cabac}: Quality preset must default me_wide ON");
        assert_eq!(decode_all(&s_on).len(), 6, "cabac={cabac}: me_wide decoded frame count");
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

/// The B `mb_type` emitter must be the EXACT inverse of the decoder's parser over
/// the FULL spec range 0..=22 — not just the four 16x16 modes the encoder currently
/// selects. This is a complete gate for the emitter on its own: the two functions
/// are inverses by construction, so a round-trip over every type either matches or
/// the binarization is wrong. Written BEFORE the B-partition mode decision exists,
/// so the emitter is correct the moment that search starts producing types 4..21.
#[test]
fn cb_mb_type_b_roundtrips_every_spec_type() {
    // Encode a run of B mb_types into one CABAC stream, then decode it back with
    // the decoder's own engine and parser.
    let types: Vec<u32> = (0..=22).filter(|t| *t != 22).chain(std::iter::once(22)).collect();
    for &ctx_inc in &[0usize, 1, 2] {
        let mut enc = rusty_h264_encoder::cabac_enc_test::CabacEncoder::new(26, 0, false);
        for &t in &types {
            rusty_h264_encoder::cabac_enc_test::cb_mb_type_b(&mut enc, ctx_inc, t);
        }
        enc.encode_terminate(true);
        let bytes = enc.into_bytes();
        let mut dec = rusty_h264_decoder::cabac_test::Cabac::new(&bytes, 0, 26, 0, false);
        for &t in &types {
            let got = rusty_h264_decoder::cabac_test::parse_mb_type_b(&mut dec, ctx_inc);
            assert_eq!(got, t, "ctx_inc {ctx_inc}: B mb_type {t} round-trip");
        }
    }
}

/// The encoder's `(p0, p1, mvmode) -> mb_type` table and the decoder's
/// `mb_type -> (mvmode, p0, p1)` table are exact inverses over the whole B
/// two-partition range (Table 7-14, types 4..=21). They live in different crates
/// and were written from the spec independently, so nothing but this test stops
/// one from drifting; a single swapped entry silently encodes (L0,Bi) as (Bi,L0),
/// which stays perfectly DECODABLE and merely reconstructs the wrong picture.
#[test]
fn b_part_mb_type_inverts_the_decoder_layout_table() {
    for t in 4..=21u32 {
        let (mvmode, p0, p1) = rusty_h264_decoder::cabac_test::b_inter_shape(t);
        assert_eq!(
            rusty_h264_encoder::cabac_enc_test::b_part_mb_type(p0, p1, mvmode),
            t,
            "mb_type {t} -> (mvmode {mvmode}, p0 {p0}, p1 {p1}) did not round-trip"
        );
    }
}
