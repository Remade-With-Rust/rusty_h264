//! `RUSTY_H264_LEGACY_CAVLC` is a host-only convenience that makes
//! `EncoderConfig::new` return `EncoderConfig::baseline`: the same fields and
//! the same bytes. This test lives in its own binary because the knob is read
//! once per process (`cached_knob!`) — set here, it would leak into every other
//! test's `EncoderConfig::new`.
use rusty_h264_common::{Profile, YuvFrame};
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn frame(w: usize, h: usize, t: usize) -> YuvFrame {
    let mut f = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = ((x + 3 * t) * 255 / w) as u32 + ((y * 7 + x * 3 + t) % 23) as u32;
            f.y[y * w + x] = v.min(255) as u8;
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            f.u[y * cw + x] = (96 + (x + t) % 64) as u8;
            f.v[y * cw + x] = (160 - (y + 2 * t) % 64) as u8;
        }
    }
    f
}

fn stream(cfg: EncoderConfig, frames: &[YuvFrame]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg).unwrap();
    let mut out = Vec::new();
    for f in frames {
        out.extend(enc.encode(f));
    }
    out.extend(enc.flush());
    out
}

#[test]
fn the_legacy_knob_selects_the_baseline_constructor() {
    std::env::set_var("RUSTY_H264_LEGACY_CAVLC", "1");
    let (w, h) = (64, 48);
    let frames: Vec<YuvFrame> = (0..6).map(|t| frame(w, h, t)).collect();

    let knob = EncoderConfig::new(w, h);
    let ctor = EncoderConfig::baseline(w, h);
    assert_eq!(knob.profile, Profile::ConstrainedBaseline);
    assert!(!knob.cabac && !knob.transform_8x8 && knob.bframes == 0);
    assert_eq!(
        (
            knob.num_ref_frames,
            knob.preset,
            knob.lookahead,
            knob.scenecut,
            knob.mbtree
        ),
        (
            ctor.num_ref_frames,
            ctor.preset,
            ctor.lookahead,
            ctor.scenecut,
            ctor.mbtree
        )
    );
    assert_eq!(knob.preset, Preset::Fast);

    let a = stream(knob, &frames);
    let b = stream(ctor, &frames);
    assert!(a.len() > 6 * 8, "the stream has bytes");
    assert_eq!(a, b, "knob-selected stream != EncoderConfig::baseline");
}
