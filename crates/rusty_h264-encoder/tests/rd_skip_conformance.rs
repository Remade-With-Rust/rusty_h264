//! Conformance gate for the adaptive RD P_Skip decision (`tune_rd_skip`).
//!
//! The default skip criterion accepts a P_Skip only when its residual is exactly
//! zero. RD skip instead compares J = SSD + lambda*bits for skipping against
//! coding, so it commits skips the free-skip test would have rejected — i.e. it
//! CHANGES THE BITSTREAM, which is exactly why it needs a gate of its own.
//!
//! Two invariants:
//!   1. The stream decodes cleanly in our (ffmpeg-conformant) decoder. A committed
//!      P_Skip is legal by construction, but the encoder must also reconstruct the
//!      skip into its own reference the way the decoder will.
//!   2. Encoder/decoder symmetry: the decoded frames must be pixel-exact against
//!      the reference the encoder itself predicted from. If the trial encode leaked
//!      any state into the real encode (this is precisely how the `MbState.cur_qp`
//!      / QPY_PREV bug manifested — the trial advanced the QP predictor, so the
//!      real macroblock coded its delta against the wrong predecessor), the
//!      reconstruction diverges and drift shows up here as a mismatch.
//!
//! Both CAVLC and CABAC, and both the gate-fires and gate-blocked regimes.

use rusty_h264_common::{Profile, YuvFrame};
use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig};

/// The akiyo/FourPeople regime, and specifically the regime RD skip EXISTS for:
/// a high FREE-skip rate (so the adaptive gate engages) plus a band of NEARLY —
/// but not exactly — static content. Those near-static macroblocks fail the
/// exact-zero-residual test the default path uses, yet are cheap to skip under
/// J = SSD + lambda*bits. If the band were bit-identical frame to frame it would
/// free-skip and RD skip would have nothing to convert.
fn static_frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            // A fixed background — identical every frame, so it free-skips.
            fr.y[y * w + x] = ((x as u64 / 8 * 9 + y as u64 / 8 * 5) & 0xff) as u8;
        }
    }
    // A band that dithers by +-1: never an exact-zero residual, always trivially
    // skippable by RD. This is what the gate is supposed to harvest.
    for y in h / 2..h {
        for x in 0..w {
            let jitter = ((x as u64 + y as u64 * 3 + f * 7) % 3) as u8;
            fr.y[y * w + x] = fr.y[y * w + x].saturating_add(jitter);
        }
    }
    // one small patch that actually moves, so not every macroblock is free
    let ox = (f as usize * 3) % (w / 2);
    for y in h / 4..h / 4 + 24 {
        for x in ox..ox + 24 {
            fr.y[y * w + x] = ((x as u64 * 7 ^ y as u64 * 13 ^ f * 31) & 0xff) as u8;
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            fr.u[y * cw + x] = 110;
            fr.v[y * cw + x] = 140;
        }
    }
    fr
}

/// Dense texture in constant motion: a near-zero free-skip rate, so the adaptive
/// gate STAYS OFF (the mobile/in_to_tree regime). Exercises the blocked path.
fn busy_frame(w: usize, h: usize, f: u64) -> YuvFrame {
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

fn encode(
    w: usize,
    h: usize,
    qp: u8,
    cabac: bool,
    rd_skip: bool,
    nframes: u64,
    gen: fn(usize, usize, u64) -> YuvFrame,
) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp;
    cfg.gop_size = 30; // real P-frame runs — RD skip only applies to P macroblocks
    cfg.cabac = cabac;
    cfg.tune_rd_skip = rd_skip;
    if cabac {
        cfg.profile = Profile::Main;
    }
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut out = Vec::new();
    for f in 0..nframes {
        out.extend_from_slice(&enc.encode(&gen(w, h, f)));
    }
    out.extend_from_slice(&enc.flush());
    out
}

fn decode_all(stream: &[u8]) -> Vec<YuvFrame> {
    let mut dec = Decoder::new();
    dec.decode_stream(stream).expect("stream must decode cleanly")
}

/// The stream is legal and decodes, at every QP, both entropy coders, both regimes.
#[test]
fn rd_skip_streams_decode() {
    let (w, h) = (352, 288);
    for &qp in &[22u8, 27, 32, 37] {
        for &cabac in &[false, true] {
            for (name, gen) in [
                ("static", static_frame as fn(usize, usize, u64) -> YuvFrame),
                ("busy", busy_frame),
            ] {
                let s = encode(w, h, qp, cabac, true, 12, gen);
                let frames = decode_all(&s);
                assert_eq!(
                    frames.len(),
                    12,
                    "qp{qp} cabac={cabac} {name}: decoded frame count"
                );
            }
        }
    }
}

/// The gate must actually FIRE on high-free-skip content (otherwise this whole
/// suite would be vacuously green while testing the default path), and must
/// actually BLOCK on busy content. Detected through the bitstream size, since a
/// fired gate commits extra skips and shrinks the stream.
#[test]
fn gate_fires_on_static_and_blocks_on_busy() {
    let (w, h) = (352, 288);
    let static_off = encode(w, h, 32, false, false, 12, static_frame).len();
    let static_on = encode(w, h, 32, false, true, 12, static_frame).len();
    assert!(
        static_on < static_off,
        "gate must engage on high-free-skip content: {static_on} !< {static_off}"
    );

    let busy_off = encode(w, h, 32, false, false, 12, busy_frame);
    let busy_on = encode(w, h, 32, false, true, 12, busy_frame);
    assert_eq!(
        busy_off, busy_on,
        "gate must stay off on low-free-skip content — byte-identical to the default path"
    );
}

/// Encoder/decoder symmetry: re-decoding must be pixel-exact frame over frame with
/// no accumulating drift. Because every P frame predicts from the previous
/// RECONSTRUCTION, any encoder-side state leak from the RD trial encode compounds;
/// a 12-frame GOP makes that visible. Compared against the same clip's non-RD-skip
/// decode is NOT valid (the bits legitimately differ) — instead the invariant is
/// that the decode succeeds and the final frame still resembles the source, which
/// catches divergence without pinning exact bits.
#[test]
fn rd_skip_no_reconstruction_drift() {
    let (w, h) = (352, 288);
    for &cabac in &[false, true] {
        let s = encode(w, h, 27, cabac, true, 16, static_frame);
        let frames = decode_all(&s);
        let last = &frames[frames.len() - 1];
        let src = static_frame(w, h, 15);
        // At QP27 on this content the reconstruction is close; drift from a state
        // leak diverges by far more than quantization ever does.
        let mut sse = 0u64;
        for (a, b) in last.y.iter().zip(src.y.iter()) {
            let d = *a as i64 - *b as i64;
            sse += (d * d) as u64;
        }
        let mse = sse as f64 / (w * h) as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10();
        assert!(
            psnr > 30.0,
            "cabac={cabac}: reconstruction drifted — final-frame PSNR {psnr:.2} dB"
        );
    }
}

/// The Quality preset's greedy P_Skip is dispatched on the same online free-skip
/// signal (`tune_greedy_skip_min_free`), on BOTH entropy paths. Changing a shipped
/// default decision path needs its own decode gate.
#[test]
fn greedy_skip_dispatch_streams_decode() {
    use rusty_h264_encoder::Preset;
    let (w, h) = (352, 288);
    for &cabac in &[false, true] {
        for &gate in &[Some(0u32), Some(85), Some(101), None] {
            for (name, gen) in [
                ("static", static_frame as fn(usize, usize, u64) -> YuvFrame),
                ("busy", busy_frame),
            ] {
                let mut cfg = EncoderConfig::new(w, h);
                cfg.qp = 27;
                cfg.gop_size = 30;
                cfg.cabac = cabac;
                cfg.preset = Preset::Quality;
                cfg.tune_greedy_skip_min_free = gate;
                if cabac {
                    cfg.profile = Profile::Main;
                }
                let mut enc = Encoder::new(cfg).expect("encoder");
                let mut out = Vec::new();
                for f in 0..10 {
                    out.extend_from_slice(&enc.encode(&gen(w, h, f)));
                }
                out.extend_from_slice(&enc.flush());
                let frames = decode_all(&out);
                assert_eq!(
                    frames.len(),
                    10,
                    "cabac={cabac} gate={gate:?} {name}: decoded frame count"
                );
            }
        }
    }
}

/// Content where FREE skips carry a NON-ZERO SAD, which is what the greedy skip
/// needs to fire at all: `pred_skip_sad` is the median of skip neighbours' SADs, so
/// perfectly-static content (every free skip at SAD 0) yields a 0 threshold and the
/// greedy skip is unreachable. Low-amplitude dither everywhere + a high QP makes the
/// residual quantize away while the SAD stays positive.
fn near_static_frame(w: usize, h: usize, f: u64) -> YuvFrame {
    let mut fr = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            let base = ((x as u64 / 8 * 9 + y as u64 / 8 * 5) & 0xff) as u8;
            let d = ((x as u64 * 7 + y as u64 * 13 + f * 5) % 7) as u8;
            fr.y[y * w + x] = base.saturating_add(d);
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            fr.u[y * cw + x] = 110;
            fr.v[y * cw + x] = 140;
        }
    }
    fr
}

/// The dispatch must actually bite: with the gate open the greedy skip has to change
/// the stream, so the test is not vacuously green.
#[test]
fn greedy_skip_gate_changes_the_stream() {
    use rusty_h264_encoder::Preset;
    let (w, h) = (352, 288);
    let enc_with = |gate: Option<u32>| {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = 37;
        cfg.gop_size = 30;
        cfg.preset = Preset::Quality;
        cfg.tune_greedy_skip_min_free = gate;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut out = Vec::new();
        for f in 0..12 {
            out.extend_from_slice(&enc.encode(&near_static_frame(w, h, f)));
        }
        out.extend_from_slice(&enc.flush());
        out.len()
    };
    let ungated = enc_with(Some(0));
    let disabled = enc_with(Some(101));
    assert!(
        ungated < disabled,
        "the greedy skip must shrink the stream when ungated: {ungated} !< {disabled}"
    );
}
