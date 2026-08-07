//! CABAC coefficient/syntax-bin harvest driver — the h264 end of the CASC
//! bridge (see `_greatgate/prometheus-bridge.md` and, for the binding rules,
//! `remade_ffmpeg_rs/_greatgate/great-gate.md` §4 "Symbolic leaves").
//!
//! Encodes deterministic synthetic clips of two content characters through
//! the real encoder with the `prometheus-telemetry` tap on, and writes the
//! harvest in **prom-entropy JSONL** (the cross-repo interchange contract —
//! no dependency on the Prometheus workspace, the format IS the coupling):
//!
//! ```text
//! {"name":…,"clips":[…],"feature_names":["ctx","state","mps","qp","is_i"]}
//! [clip,frame,ctx_idx,bin,p_zero,[ctx,state,mps,qp,is_i]]
//! ```
//!
//! Analyze from `remade_ffmpeg_rs/Prometheus`:
//! `prom entropy replay <out>/cabac-bins.jsonl [--csv …]`
//! `prom entropy casc <out>/cabac-bins.jsonl --leaf … --input …`
//!
//! Run: `cargo run --example cabac_harvest --features prometheus-telemetry --release [-- out_dir]`
//! Swap the synthetic clips for corpus y4m when the campaign graduates; the
//! schema and everything downstream stay unchanged.

use rusty_h264::prometheus_telemetry as tap;
use rusty_h264::{Encoder, EncoderConfig, YuvFrame};
use std::fmt::Write as _;

/// Deterministic LCG (reproducible harvest, no ambient randomness).
struct Lcg(u64);
impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32
    }
}

const W: usize = 176;
const H: usize = 144;
const FRAMES: usize = 24;
/// Inter slices are the point: 1 IDR + (GOP-1) P per group.
const GOP: u32 = 12;

fn smooth_frame(t: usize) -> YuvFrame {
    let mut f = YuvFrame::black(W, H);
    for r in 0..H {
        for c in 0..W {
            f.y[r * W + c] = ((r + c + 3 * t) % 255) as u8;
        }
    }
    f
}

fn busy_frame(t: usize, rng: &mut Lcg) -> YuvFrame {
    let mut f = YuvFrame::black(W, H);
    for r in 0..H {
        for c in 0..W {
            let block = (r / 8) * (W / 8) + c / 8;
            let base = ((block * 37 + t * 5) % 200) as u32;
            f.y[r * W + c] = (base + (rng.next_u32() % 56)) as u8;
        }
    }
    f
}

/// One JSONL record row. `frame` is the driver's frame counter; slices within
/// a frame share it (this encoder codes one slice per frame).
fn push_row(out: &mut String, clip: usize, frame: usize, s: &tap::SliceTap) {
    for b in &s.bins {
        let p0 = tap::p_zero_q8(b.state, b.mps);
        // [clip,frame,ctx,bit,p_zero,[ctx,state,mps,qp,is_i]]
        writeln!(
            out,
            "[{},{},{},{},{},[{},{},{},{},{}]]",
            clip,
            frame,
            b.ctx_idx,
            b.bin,
            p0,
            b.ctx_idx,
            b.state,
            b.mps,
            s.qp,
            if s.is_i { 1 } else { 0 }
        )
        .unwrap();
    }
}

fn encode_clip(
    clip: usize,
    name: &str,
    qp: u32,
    mut frame_fn: impl FnMut(usize) -> YuvFrame,
    out: &mut String,
) -> usize {
    let mut cfg = EncoderConfig::new(W, H);
    cfg.qp = qp as u8;
    // `EncoderConfig::new` defaults `gop_size` to 1 — ALL-INTRA. Left unset, the
    // whole harvest contained zero P/B slices, which (a) makes rung A0
    // unmeasurable, since `cabac_init_idc` only exists for P/B, and (b) silently
    // scoped the -3.82% CASC headline to INTRA ONLY. Inter slices carry a
    // different context population entirely (MVs, skip flags, inter residual),
    // so a CASC verdict from intra alone does not transfer to them.
    cfg.gop_size = GOP;
    let mut enc = Encoder::new(cfg).expect("encoder config");
    let mut bins = 0usize;
    let mut drain = |out: &mut String, t: usize, bins: &mut usize| {
        for s in tap::take() {
            *bins += s.bins.len();
            push_row(out, clip, t, &s);
        }
    };
    for t in 0..FRAMES {
        let _bytes = enc.encode(&frame_fn(t));
        drain(out, t, &mut bins);
    }
    // `encode()` BUFFERS a whole GOP under the mb-tree lookahead, so without a
    // flush the trailing GOP is never coded and its bins never appear.
    let _tail = enc.flush();
    drain(out, FRAMES - 1, &mut bins);
    eprintln!("  {name}: {bins} context-coded bins");
    bins
}

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out_dir).expect("out dir");

    tap::enable(true);
    let mut body = String::new();
    eprintln!("encoding…");
    encode_clip(0, "smooth-qp24", 24, smooth_frame, &mut body);
    let mut r1 = Lcg(0xB05EBA11);
    encode_clip(1, "busy-qp24", 24, |t| busy_frame(t, &mut r1), &mut body);
    encode_clip(2, "smooth-qp34", 34, smooth_frame, &mut body);
    let mut r2 = Lcg(0x5EEDF00D);
    encode_clip(3, "busy-qp34", 34, |t| busy_frame(t, &mut r2), &mut body);
    tap::enable(false);

    let header = r#"{"name":"h264-cabac-bins-v1","clips":["smooth-qp24","busy-qp24","smooth-qp34","busy-qp34"],"feature_names":["ctx","state","mps","qp","is_i"]}"#;
    let path = format!("{out_dir}/cabac-bins.jsonl");
    std::fs::write(&path, format!("{header}\n{body}")).expect("write jsonl");
    println!("wrote {path}");
    println!(
        "analyze from remade_ffmpeg_rs/Prometheus:\n  \
         cargo run -p prom-cli --release -- entropy replay {path}"
    );
}
