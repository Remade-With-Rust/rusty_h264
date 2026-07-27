//! Does x264's motion field PREDICT better than ours?
//!
//! Two earlier designs were built and discarded as confounded:
//!   1. transplanting a single x264 vector into our encoder — invalid, because
//!      `mvd` is coded against the NEIGHBOURS' vectors, so a lone foreign vector
//!      prices against the wrong predictor (it read +106% bits, an artifact);
//!   2. transplanting x264's whole field — invalid, because x264's vectors point
//!      into x264's RECONSTRUCTION; against ours they mispredict, and forcing them
//!      degrades our reference further, compounding over the GOP (it read +118%
//!      size, also an artifact).
//!
//! A motion vector is only meaningful relative to the reference and neighbour field
//! it was chosen in. So this compares the two fields REFERENCE-NEUTRALLY: both are
//! evaluated against the same ORIGINAL previous frame, measuring prediction quality
//! alone. Both encoders' streams are parsed by our own decoder, so no external
//! MV-export tooling is involved.
use rusty_h264_common::{inter::mc_luma, YuvFrame};
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn load(path: &str, w: usize, h: usize, n: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).expect("clip");
    let fsz = w * h * 3 / 2;
    raw.chunks_exact(fsz).take(n).map(|c| {
        let mut fr = YuvFrame::black(w, h);
        fr.y.copy_from_slice(&c[..w * h]);
        fr.u.copy_from_slice(&c[w * h..w * h + w * h / 4]);
        fr.v.copy_from_slice(&c[w * h + w * h / 4..]);
        fr
    }).collect()
}

/// (mv field, inter mask) per frame that carries motion, in order.
fn drain_fields() -> Vec<(Vec<(i32, i32)>, Vec<bool>, Vec<i32>, usize)> {
    let mut d = rusty_h264_decoder::MV_DUMP.lock().unwrap();
    let out = d.iter()
        .filter(|f| f.inter.iter().any(|&b| b))
        .map(|f| (f.mv.clone(), f.inter.clone(), f.ref_idx.clone(), f.mb_w))
        .collect();
    d.clear();
    out
}

/// A macroblock is comparable only if it is a single 16x16 partition against
/// reference 0. x264 `medium` uses multiple references and sub-partitions; scoring
/// a ref_idx>0 vector against frame N-1, or a sub-partition's vector as if it
/// covered the whole macroblock, is meaningless.
fn uniform_ref0(mv: &[(i32, i32)], refi: &[i32], inter: &[bool], b0: usize, w4: usize) -> Option<(i32, i32)> {
    let m = *mv.get(b0)?;
    for r in 0..4 {
        for c in 0..4 {
            let i = b0 + r * w4 + c;
            if !inter.get(i).copied().unwrap_or(false) { return None; }
            if refi.get(i).copied().unwrap_or(-1) != 0 { return None; }
            if *mv.get(i)? != m { return None; }
        }
    }
    Some(m)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (ws, hs) = a[1].split_once('x').unwrap();
    let (w, h): (usize, usize) = (ws.parse().unwrap(), hs.parse().unwrap());
    let nf: usize = std::env::var("RS_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let qp: u8 = std::env::var("RS_QP").ok().and_then(|v| v.parse().ok()).unwrap_or(27);
    let gop: u32 = 30;
    let frames = load(&a[0], w, h, nf);

    // --- x264's field ---
    let x264 = std::env::var("X264")
        .unwrap_or_else(|_| "C:/Users/talmo/coding/_ref_x264/x264.exe".into());
    let tmp = std::env::temp_dir().join("mvcmp_x264.264");
    let st = std::process::Command::new(&x264)
        .args(["--threads", "1", "--preset", "medium", "--profile", "baseline",
               "--qp", &qp.to_string(), "--keyint", &gop.to_string(),
               "--frames", &nf.to_string(), "--input-res", &format!("{w}x{h}")])
        .arg("-o").arg(&tmp).arg(&a[0])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().expect("run x264");
    assert!(st.success(), "x264 failed");
    let xbits = std::fs::read(&tmp).unwrap();
    let _ = rusty_h264_decoder::Decoder::new().decode_stream(&xbits).expect("x264 decode");
    let xf = drain_fields();

    // --- our field ---
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = qp; cfg.gop_size = gop; cfg.preset = Preset::Balanced;
    let mut enc = Encoder::new(cfg).expect("enc");
    let mut ours_bytes = 0usize;
    let mut obits = Vec::new();
    for f in &frames { let p = enc.encode(f); ours_bytes += p.len(); obits.extend_from_slice(&p); }
    let _ = rusty_h264_decoder::Decoder::new().decode_stream(&obits).expect("our decode");
    let of = drain_fields();

    println!("x264 {} bytes / {} motion frames    ours {} bytes / {} motion frames",
             xbits.len(), xf.len(), ours_bytes, of.len());
    let nfr = xf.len().min(of.len());

    // --- reference-neutral prediction quality ---
    let (mut sso, mut ssx, mut n, mut diff) = (0u64, 0u64, 0u64, 0u64);
    let mb_w = w / 16;
    for fi in 0..nfr {
        let refp = &frames[fi].y;      // ORIGINAL previous frame: identical for both
        let cur = &frames[fi + 1].y;
        let (omv, oin, oref, ow4) = (&of[fi].0, &of[fi].1, &of[fi].2, of[fi].3 * 4);
        let (xmv, xin, xref, xw4) = (&xf[fi].0, &xf[fi].1, &xf[fi].2, xf[fi].3 * 4);
        for mby in 0..h / 16 {
            for mbx in 0..mb_w {
                let (ob, xb) = ((mby * 4) * ow4 + mbx * 4, (mby * 4) * xw4 + mbx * 4);
                // BOTH must be a single 16x16 partition on reference 0
                let (mo, mx) = match (uniform_ref0(omv, oref, oin, ob, ow4),
                                      uniform_ref0(xmv, xref, xin, xb, xw4)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => continue,
                };
                let mut po = [0u8; 256];
                let mut px = [0u8; 256];
                mc_luma(refp, w, h, mbx * 16, mby * 16, 16, 16, mo.0, mo.1, &mut po);
                mc_luma(refp, w, h, mbx * 16, mby * 16, 16, 16, mx.0, mx.1, &mut px);
                for dy in 0..16 {
                    for dx in 0..16 {
                        let s = cur[(mby * 16 + dy) * w + mbx * 16 + dx] as i64;
                        let a = s - po[dy * 16 + dx] as i64;
                        let b = s - px[dy * 16 + dx] as i64;
                        sso += (a * a) as u64;
                        ssx += (b * b) as u64;
                    }
                }
                n += 1;
                diff += (mo != mx) as u64;
            }
        }
    }
    let nn = n.max(1);
    println!("\nreference-neutral prediction quality over {n} macroblocks (both inter)");
    println!("  MVs differing            {:.1}%", 100.0 * diff as f64 / nn as f64);
    println!("  mean SSD  ours           {:>10.1}", sso as f64 / nn as f64);
    println!("  mean SSD  x264           {:>10.1}", ssx as f64 / nn as f64);
    println!("  ---> x264 predicts {:+.2}% {}", 100.0 * (ssx as f64 - sso as f64) / sso as f64,
             if ssx < sso { "BETTER (our ME/cost function is behind)" } else { "WORSE (our vectors are fine)" });
}
