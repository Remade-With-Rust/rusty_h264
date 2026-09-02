//! The chip-facing encoder API: borrowed frames (`encode_planes`), a caller
//! buffer (`encode_into`), a keyframe request, and the `baseline` constructor.
//!
//! Every new entry is gated **byte-identical** against the owned-frame path
//! the whole existing suite already gates, so nothing here re-proves
//! conformance: it proves the new doors lead into the same room.

use rusty_h264_common::{Profile, YuvFrame, YuvPlanes};
use rusty_h264_encoder::{EncodeError, Encoder, EncoderConfig, Preset};

/// A moving gradient with a little texture, distinct per frame so P-frames
/// have real motion to code.
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

/// The same picture as a **padded** buffer: each row followed by `pad` bytes
/// of junk, so a view over it is not tight.
fn padded(f: &YuvFrame, pad: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize, usize) {
    let (w, h, cw, ch) = (f.width, f.height, f.width / 2, f.height / 2);
    let (sy, sc) = (w + pad, cw + pad);
    let mut y = vec![0xEEu8; sy * h];
    let mut u = vec![0xEEu8; sc * ch];
    let mut v = vec![0xEEu8; sc * ch];
    for r in 0..h {
        y[r * sy..r * sy + w].copy_from_slice(&f.y[r * w..(r + 1) * w]);
    }
    for r in 0..ch {
        u[r * sc..r * sc + cw].copy_from_slice(&f.u[r * cw..(r + 1) * cw]);
        v[r * sc..r * sc + cw].copy_from_slice(&f.v[r * cw..(r + 1) * cw]);
    }
    (y, u, v, sy, sc)
}

fn stream_owned(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    let mut out = Vec::new();
    for f in frames {
        out.extend(enc.encode(f));
    }
    out.extend(enc.flush());
    out
}

fn nal_types(stream: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 4 <= stream.len() {
        if stream[i..i + 3] == [0, 0, 1] {
            v.push(stream[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

fn has_idr(au: &[u8]) -> bool {
    nal_types(au).contains(&5)
}

/// The two configurations that matter: the chip's, and the default (High,
/// CABAC, mb-tree lookahead), so the lookahead paths take the view too.
fn configs(w: usize, h: usize) -> Vec<(&'static str, EncoderConfig)> {
    let mut default = EncoderConfig::new(w, h);
    default.gop_size = 8;
    default.min_keyint = 4;
    default.lookahead = 4;
    let mut baseline = EncoderConfig::baseline(w, h);
    baseline.gop_size = 6;
    baseline.min_keyint = 6;
    vec![("baseline", baseline), ("default", default)]
}

#[test]
fn borrowed_planes_code_identically_to_owned_frames() {
    let (w, h) = (64, 48);
    let frames: Vec<YuvFrame> = (0..12).map(|t| frame(w, h, t)).collect();
    for (name, cfg) in configs(w, h) {
        let want = stream_owned(&cfg, &frames);

        // Tight views straight over the owned planes.
        let mut enc = Encoder::new(cfg.clone()).unwrap();
        let mut got = Vec::new();
        for f in &frames {
            got.extend(enc.encode_planes(&f.as_planes()).unwrap());
        }
        got.extend(enc.flush());
        assert_eq!(got, want, "{name}: tight view");

        // Padded views: gathered into the encoder's scratch, same bytes.
        let mut enc = Encoder::new(cfg.clone()).unwrap();
        let mut got = Vec::new();
        for f in &frames {
            let (y, u, v, sy, sc) = padded(f, 13);
            let view = YuvPlanes::new(w, h, &y, &u, &v, sy, sc).unwrap();
            assert!(!view.is_tight() && !view.is_valid());
            assert_eq!(view.to_frame().y, f.y);
            got.extend(enc.encode_planes(&view).unwrap());
        }
        got.extend(enc.flush());
        assert_eq!(got, want, "{name}: padded view");
    }
}

#[test]
fn views_are_validated_up_front() {
    let f = frame(32, 16, 0);
    assert!(YuvPlanes::tight(32, 16, &f.y, &f.u, &f.v).is_some());
    assert!(
        YuvPlanes::tight(33, 16, &f.y, &f.u, &f.v).is_none(),
        "odd width"
    );
    assert!(
        YuvPlanes::tight(32, 16, &f.y[..100], &f.u, &f.v).is_none(),
        "short luma"
    );
    assert!(
        YuvPlanes::new(32, 16, &f.y, &f.u, &f.v, 31, 16).is_none(),
        "stride < width"
    );
    let mut enc = Encoder::new(EncoderConfig::baseline(64, 48)).unwrap();
    assert!(matches!(
        enc.encode_planes(&f.as_planes()),
        Err(EncodeError::FrameMismatch)
    ));
}

#[test]
fn encode_into_matches_and_reports_the_needed_size() {
    let (w, h) = (64, 48);
    let frames: Vec<YuvFrame> = (0..6).map(|t| frame(w, h, t)).collect();
    let mut cfg = EncoderConfig::baseline(w, h);
    cfg.gop_size = 4;
    cfg.min_keyint = 4;

    // A parallel owned-frame encoder is the oracle for every access unit.
    let mut oracle = Encoder::new(cfg.clone()).unwrap();
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    let mut buf = vec![0u8; w * h * 3 / 2];

    let want0 = oracle.encode(&frames[0]);
    let n = enc.encode_into(&frames[0].as_planes(), &mut buf).unwrap();
    assert_eq!(&buf[..n], &want0[..]);

    // Frame 1 into a buffer that is too small: the size comes back, the
    // picture is gone, and the encoder is still in step with the oracle.
    let want1 = oracle.encode(&frames[1]);
    let mut small = [0u8; 16];
    match enc.encode_into(&frames[1].as_planes(), &mut small) {
        Err(EncodeError::BufferTooSmall { needed }) => assert_eq!(needed, want1.len()),
        other => panic!("expected BufferTooSmall, got {other:?}"),
    }
    for f in &frames[2..] {
        let want = oracle.encode(f);
        let n = enc.encode_into(&f.as_planes(), &mut buf).unwrap();
        assert_eq!(&buf[..n], &want[..], "in step after the dropped picture");
    }
    let tail = oracle.flush();
    let n = enc.flush_into(&mut buf).unwrap();
    assert_eq!(&buf[..n], &tail[..]);
}

#[test]
fn request_keyframe_makes_the_next_picture_an_idr() {
    let (w, h) = (64, 48);
    let frames: Vec<YuvFrame> = (0..10).map(|t| frame(w, h, t)).collect();

    // The chip configuration, with rate control on: the IDR comes at once and
    // the controller keeps going (the next picture is a P, not a new stream).
    let mut cfg = EncoderConfig::baseline(w, h);
    cfg.gop_size = 30;
    cfg.min_keyint = 30;
    cfg.bitrate = 200_000;
    cfg.framerate = 15.0;
    let mut enc = Encoder::new(cfg).unwrap();
    for f in &frames[..4] {
        let au = enc.encode(f);
        assert!(!au.is_empty());
    }
    enc.request_keyframe();
    let au = enc.encode(&frames[4]);
    assert!(has_idr(&au), "the picture after the request is an IDR");
    assert!(nal_types(&au).contains(&7), "with its SPS");
    let au = enc.encode(&frames[5]);
    assert!(!has_idr(&au), "and the one after it is a P again");

    // With a lookahead active the buffered pictures come out first and the
    // request lands on the frame submitted with it.
    let mut cfg = EncoderConfig::new(w, h);
    cfg.gop_size = 30;
    cfg.min_keyint = 30;
    cfg.lookahead = 6;
    cfg.scenecut = 0;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut out = Vec::new();
    for f in &frames[..3] {
        out.extend(enc.encode(f));
    }
    assert!(out.is_empty(), "three frames are buffered by the lookahead");
    enc.request_keyframe();
    let flushed = enc.encode(&frames[3]);
    let types = nal_types(&flushed);
    assert_eq!(
        types.iter().filter(|&&t| t == 5).count(),
        1,
        "frames 0..3: the stream's own IDR only"
    );
    assert_eq!(
        types.iter().filter(|&&t| t == 1).count(),
        2,
        "then two P pictures"
    );
    out.extend(enc.encode(&frames[4]));
    out.extend(enc.encode(&frames[5]));
    let tail = enc.flush();
    let types = nal_types(&tail);
    assert_eq!(
        types.iter().filter(|&&t| t == 5).count(),
        1,
        "frame 3 is the requested IDR"
    );
    assert_eq!(types[0], 7, "SPS first");
}

#[test]
fn baseline_is_the_chip_configuration() {
    let cfg = EncoderConfig::baseline(320, 240);
    assert_eq!(cfg.profile, Profile::ConstrainedBaseline);
    assert!(!cfg.cabac && !cfg.transform_8x8);
    assert_eq!((cfg.bframes, cfg.num_ref_frames), (0, 1));
    assert_eq!((cfg.lookahead, cfg.scenecut), (0, 0));
    assert_eq!(cfg.preset, Preset::Fast);

    // The stream says Constrained Baseline: profile_idc 66, constraint_set1.
    let mut enc = Encoder::new(EncoderConfig::baseline(64, 48)).unwrap();
    let au = enc.encode(&frame(64, 48, 0));
    let sps = au
        .windows(4)
        .position(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 7)
        .expect("SPS");
    assert_eq!(au[sps + 4], 66, "profile_idc");
    assert_ne!(au[sps + 5] & 0x40, 0, "constraint_set1_flag");

    // One access unit per frame, nothing buffered, nothing on flush.
    let au = enc.encode(&frame(64, 48, 1));
    assert!(!au.is_empty() && !has_idr(&au));
    assert!(enc.flush().is_empty());
}
