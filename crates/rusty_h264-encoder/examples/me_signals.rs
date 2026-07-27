//! Candidate DISPATCH SIGNALS for `me_wide`, measured per clip and correlated
//! against the known per-clip BD truth table (docs/WHYS-speed-gap.md R5).
//!
//! me_wide rescues the motion search when the predictor-seeded diamond STALLS and
//! misses a far-but-better vector. Its current gate scores its own SATD cost-cut
//! after committing the MVs, which separates static content and nothing else.
//!
//! So measure what the tool actually STRESSES, as a cheap PRE-PASS on source pixels
//! only (no encoder state, available before frame 0 is coded):
//!
//!   headroom = mean over blocks of (SAD_local - SAD_wide) / SAD_local
//!
//! i.e. how much a WIDE search beats a PREDICTOR-LOCAL one. That is precisely the
//! quantity me_wide buys. Reported overall, and restricted to FLAT blocks (me_wide's
//! own target set), plus three cheaper rivals so the axis is chosen on evidence:
//! motion magnitude, block variance, and temporal activity.
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example me_signals \
//!     -- video-tests/clips/*.y4m

use rusty_h264_common::types::YuvFrame;

fn read_y4m(path: &str, max_frames: usize) -> (usize, usize, Vec<YuvFrame>) {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let hdr_end = raw.iter().position(|&b| b == b'\n').expect("y4m header");
    let hdr = std::str::from_utf8(&raw[..hdr_end]).unwrap();
    let (mut w, mut h) = (0usize, 0usize);
    for tok in hdr.split_whitespace() {
        match tok.as_bytes().first() {
            Some(b'W') => w = tok[1..].parse().unwrap(),
            Some(b'H') => h = tok[1..].parse().unwrap(),
            _ => {}
        }
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let mut frames = Vec::new();
    let mut p = hdr_end + 1;
    while frames.len() < max_frames {
        let Some(rel) = raw[p..].iter().position(|&b| b == b'\n') else { break };
        p += rel + 1;
        if p + ys + 2 * cs > raw.len() {
            break;
        }
        frames.push(YuvFrame {
            width: w,
            height: h,
            y: raw[p..p + ys].to_vec(),
            u: raw[p + ys..p + ys + cs].to_vec(),
            v: raw[p + ys + cs..p + ys + 2 * cs].to_vec(),
        });
        p += ys + 2 * cs;
    }
    (w, h, frames)
}

/// SAD of a 16×16 source block against `reference` at full-pel `(rx, ry)`.
/// Returns `None` when the position is not fully inside the picture.
fn sad16(src: &[u8], w: usize, bx: usize, by: usize, r: &[u8], rx: isize, ry: isize, rw: usize, rh: usize) -> Option<u32> {
    if rx < 0 || ry < 0 || rx as usize + 16 > rw || ry as usize + 16 > rh {
        return None;
    }
    let (rx, ry) = (rx as usize, ry as usize);
    let mut s = 0u32;
    for dy in 0..16 {
        let a = &src[(by + dy) * w + bx..][..16];
        let b = &r[(ry + dy) * rw + rx..][..16];
        s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
    }
    Some(s)
}

fn variance16(src: &[u8], w: usize, bx: usize, by: usize) -> f64 {
    let (mut s, mut ss) = (0u64, 0u64);
    for dy in 0..16 {
        for dx in 0..16 {
            let v = src[(by + dy) * w + bx + dx] as u64;
            s += v;
            ss += v * v;
        }
    }
    let n = 256u64;
    (ss as f64 - (s * s) as f64 / n as f64) / n as f64
}

struct Sig {
    headroom: f64,      // wide-vs-local SAD improvement, all blocks
    headroom_flat: f64, // same, restricted to FLAT blocks (me_wide's target set)
    flat_frac: f64,     // fraction of blocks that are flat
    motion: f64,        // mean |wide-search MV| in pixels
    mvdiv: f64,         // spatial DIVERGENCE of the MV field (affine tell)
    var: f64,           // mean block variance
    tdiff: f64,         // mean |frame difference| per pixel
}

/// One clip → the candidate signals. Source pixels only; a real dispatcher would
/// compute this in the lookahead.
fn signals(w: usize, h: usize, frames: &[YuvFrame]) -> Sig {
    const LOCAL: isize = 2; // the diamond's effective local reach from a good predictor
    const WIDE: isize = 24; // me_wide's grid half-extent
    const FLAT_VAR: f64 = 800.0; // me_wide_var default
    let (mut hr, mut hrf, mut nb, mut nbf) = (0.0, 0.0, 0u64, 0u64);
    let (mut mot, mut vsum, mut td, mut ntd) = (0.0, 0.0, 0.0, 0u64);
    // MV field, for the divergence term: rotation/zoom make neighbouring blocks'
    // best vectors disagree systematically, while a pan makes them agree. Pure
    // translational headroom cannot see affine motion (syn_rot 1.19% / syn_zoom
    // 0.90% headroom yet BD +4.89 / +2.63), so this is the term that gap demands.
    let mut mvs: Vec<(f64, f64)> = Vec::new();
    // A few frame pairs is plenty — these are clip-level statistics.
    let pairs: Vec<(usize, usize)> = (1..frames.len().min(9)).map(|i| (i - 1, i)).collect();
    for &(pi, ci) in &pairs {
        let (rf, cf) = (&frames[pi], &frames[ci]);
        let mut acc = 0u64;
        for (a, b) in cf.y.iter().zip(&rf.y) {
            acc += a.abs_diff(*b) as u64;
        }
        td += acc as f64 / (w * h) as f64;
        ntd += 1;
        // Subsample blocks so 1080p stays affordable.
        let step = if w >= 1280 { 4 } else { 2 };
        let mut by = 24;
        while by + 40 < h {
            let mut bx = 24;
            while bx + 40 < w {
                let v = variance16(&cf.y, w, bx, by);
                vsum += v;
                // LOCAL: +-2 around the co-located position (a well-seeded diamond's reach).
                let mut best_local = u32::MAX;
                for dy in -LOCAL..=LOCAL {
                    for dx in -LOCAL..=LOCAL {
                        if let Some(s) = sad16(&cf.y, w, bx, by, &rf.y, bx as isize + dx, by as isize + dy, w, h) {
                            best_local = best_local.min(s);
                        }
                    }
                }
                // WIDE: +-24 step 2, exactly me_wide's grid.
                let (mut best_wide, mut bmv) = (u32::MAX, (0isize, 0isize));
                let mut dy = -WIDE;
                while dy <= WIDE {
                    let mut dx = -WIDE;
                    while dx <= WIDE {
                        if let Some(s) = sad16(&cf.y, w, bx, by, &rf.y, bx as isize + dx, by as isize + dy, w, h) {
                            if s < best_wide {
                                best_wide = s;
                                bmv = (dx, dy);
                            }
                        }
                        dx += 2;
                    }
                    dy += 2;
                }
                if best_local != u32::MAX && best_wide != u32::MAX && best_local > 0 {
                    let gain = (best_local.saturating_sub(best_wide)) as f64 / best_local as f64;
                    hr += gain;
                    nb += 1;
                    mot += ((bmv.0 * bmv.0 + bmv.1 * bmv.1) as f64).sqrt();
                    mvs.push((bmv.0 as f64, bmv.1 as f64));
                    if v < FLAT_VAR {
                        hrf += gain;
                        nbf += 1;
                    }
                }
                bx += 16 * step;
            }
            by += 16 * step;
        }
    }
    let n = nb.max(1) as f64;
    // Divergence = RMS deviation of the MV field from its own mean (the global
    // translation). A pan has ~0; rotation/zoom have a large systematic spread.
    let mvdiv = if mvs.len() > 1 {
        let (mx, my) = (
            mvs.iter().map(|v| v.0).sum::<f64>() / mvs.len() as f64,
            mvs.iter().map(|v| v.1).sum::<f64>() / mvs.len() as f64,
        );
        (mvs.iter().map(|v| (v.0 - mx).powi(2) + (v.1 - my).powi(2)).sum::<f64>() / mvs.len() as f64).sqrt()
    } else {
        0.0
    };
    Sig {
        mvdiv,
        headroom: 100.0 * hr / n,
        headroom_flat: 100.0 * hrf / nbf.max(1) as f64,
        flat_frac: nbf as f64 / n,
        motion: mot / n,
        var: vsum / n,
        tdiff: td / ntd.max(1) as f64,
    }
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let nframes: usize = std::env::var("SIG_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(9);
    println!("me_wide candidate dispatch signals (source-pixel pre-pass, no encoder state)\n");
    println!(
        "{:<26} {:>9} {:>10} {:>9} {:>8} {:>9} {:>8}",
        "clip", "headroom", "hr(flat)", "mvdiv", "motion", "var", "tdiff"
    );
    println!("{}", "-".repeat(86));
    for p in &paths {
        let (w, h, frames) = read_y4m(p, nframes);
        if frames.len() < 2 {
            continue;
        }
        let s = signals(w, h, &frames);
        let name = std::path::Path::new(p).file_stem().unwrap().to_string_lossy().to_string();
        println!(
            "{:<26} {:>8.2}% {:>9.2}% {:>8.2} {:>8.2} {:>9.0} {:>8.2}",
            name, s.headroom, s.headroom_flat, s.mvdiv, s.motion, s.var, s.tdiff
        );
    }
    println!("\nCorrelate against the per-clip BD column in docs/WHYS-speed-gap.md (R5).");
    println!("A usable axis must SEPARATE the winners (blue_sky +4.70, bus +4.57, football");
    println!("+1.51) from the losers (foreman_qcif -1.08, tempete -0.12, mobile -0.03).");
}
