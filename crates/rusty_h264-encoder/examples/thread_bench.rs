//! Threading weigh-in: sequential `encode()` vs GOP-parallel `encode_all()`,
//! same config — byte-identity asserted (compression unchanged BY CONSTRUCTION),
//! wall speedup reported. Multi-GOP required for parallelism (keyint < frames).
use rusty_h264_common::types::YuvFrame;
use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

fn read_y4m(path: &str, max: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap();
    let e = raw.iter().position(|&b| b == b'\n').unwrap();
    let hdr = std::str::from_utf8(&raw[..e]).unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for t in hdr.split_whitespace() {
        match t.as_bytes().first() {
            Some(b'W') => w = t[1..].parse().unwrap(),
            Some(b'H') => h = t[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (mut f, mut p) = (Vec::new(), e + 1);
    while f.len() < max {
        let Some(r) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += r + 1;
        if p + ys + 2 * cs > raw.len() { break }
        f.push(YuvFrame { width: w, height: h, y: raw[p..p+ys].to_vec(), u: raw[p+ys..p+ys+cs].to_vec(), v: raw[p+ys+cs..p+ys+2*cs].to_vec() });
        p += ys + 2 * cs;
    }
    (w, h, f)
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let n: usize = std::env::var("TB_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(240);
    let gop: u32 = std::env::var("TB_GOP").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let (w, h, frames) = read_y4m(&path, n);
    for (pn, preset) in [("balanced", Preset::Balanced), ("quality", Preset::Quality)] {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.qp = 27; cfg.gop_size = gop; cfg.preset = preset;
        // best-of-3 each arm
        let mut seq_ms = f64::MAX; let mut seq_out = Vec::new();
        for _ in 0..3 {
            let mut enc = Encoder::new(cfg.clone()).unwrap();
            let t = std::time::Instant::now();
            let mut o = Vec::new();
            for f in &frames { o.extend_from_slice(&enc.encode(f)); }
            seq_ms = seq_ms.min(t.elapsed().as_secs_f64() * 1e3);
            seq_out = o;
        }
        let mut par_ms = f64::MAX; let mut par_out = Vec::new();
        for _ in 0..3 {
            let enc = Encoder::new(cfg.clone()).unwrap();
            let t = std::time::Instant::now();
            let o: Vec<u8> = enc.encode_all(&frames).unwrap().concat();
            par_ms = par_ms.min(t.elapsed().as_secs_f64() * 1e3);
            par_out = o;
        }
        assert_eq!(seq_out, par_out, "seq != parallel — compression WOULD be compromised");
        println!("{pn:<9} x{} gop{gop}: seq {seq_ms:.0} ms  parallel {par_ms:.0} ms  speedup {:.2}x  (byte-identical ✓)", frames.len(), seq_ms / par_ms);
    }
}
