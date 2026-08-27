//! Deterministic gates for the multi-ref P campaign (refs default 3).
//!
//! Two claims, both COUNT-verified (no clock, no pinning):
//!  * the multi-ref search actually runs — `ref_search` exceeds `best_part`
//!    (gate-must-prove-the-tool-ran: a refs-3 default whose searcher never
//!    left ref 0 would "pass" every BD row while measuring nothing);
//!  * the exact `ref_bits` prune fires — `ref_search` stays BELOW the
//!    `3 x best_part` ceiling a pruneless triple search would pay.

use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

/// Deterministic synthetic motion: textured background, a block translating
/// 4px/frame — multi-ref matter: occlusion/uncovering gives past-past frames
/// something the immediate ref lacks.
fn synth(w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    (0..n)
        .map(|f| {
            let mut y = vec![0u8; w * h];
            for j in 0..h {
                for i in 0..w {
                    let mut v = 60 + ((i * 5 + j * 7) % 90) as i32;
                    // Two blocks moving at different speeds: crossing occlusion.
                    let s1 = (8 + f * 4) % (w - 24);
                    let s2 = (w - 32).wrapping_sub(f * 2) % (w - 24);
                    if (s1..s1 + 20).contains(&i) && (16..40).contains(&j) {
                        v = 210 - ((i * 3 + j) % 60) as i32;
                    } else if (s2..s2 + 20).contains(&i) && (24..48).contains(&j) {
                        v = 30 + ((i + j * 3) % 50) as i32;
                    }
                    y[j * w + i] = v.clamp(0, 255) as u8;
                }
            }
            YuvFrame { width: w, height: h, y, u: vec![128; (w / 2) * (h / 2)], v: vec![128; (w / 2) * (h / 2)] }
        })
        .collect()
}

#[test]
fn multiref_searches_and_prunes() {
    // Cached on first read inside the encoder — set before any encode.
    std::env::set_var("RFF_GATE_CENSUS", "1");
    let (w, h, n) = (128usize, 96usize, 12usize);
    let frames = synth(w, h, n);
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = 30;
    cfg.gop_size = 30;
    cfg.preset = Preset::Quality;
    assert_eq!(cfg.num_ref_frames, 3, "the x264-parity default regressed");

    let before = rusty_h264_encoder::gate_work();
    let _ = Encoder::new(cfg).expect("cfg").encode_all(&frames).expect("encode");
    let after = rusty_h264_encoder::gate_work();
    let names = rusty_h264_encoder::gate_work_names();
    let idx = |n: &str| names.iter().position(|&x| x == n).unwrap();
    let best_part = after[idx("best_part")] - before[idx("best_part")];
    let ref_search = after[idx("ref_search")] - before[idx("ref_search")];

    assert!(best_part > 0, "no partition searches ran");
    // The tool ran: more per-ref searches than partitions => refs beyond 0
    // were genuinely searched.
    assert!(
        ref_search > best_part,
        "multi-ref never searched past ref 0 ({ref_search} vs {best_part})"
    );
    // The prune fired: strictly under the pruneless 3x ceiling.
    assert!(
        ref_search < 3 * best_part,
        "ref_bits prune never fired ({ref_search} vs 3x{best_part})"
    );
}
