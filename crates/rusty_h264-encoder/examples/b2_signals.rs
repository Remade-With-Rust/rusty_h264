//! Candidate DISPATCH SIGNALS for B2 (SAD full-pel), measured per clip and printed
//! against the known 16-clip BD truth table (WHYS Descent H, λ=0.5).
//!
//! B2 wins where SAD's candidate ranking matches (or beats) SATD's — fast
//! translational motion — and loses where SAD misranks: illumination change
//! (crew's flashes: SAD is dominated by the DC shift the Hadamard discounts) and
//! fine-detail content. Per the R6 law, instrument SEVERAL candidates against the
//! truth table before choosing the axis. All signals are O(subsampled pixels) on
//! SOURCE frames only (usable as a pre-pass, no encoder state):
//!
//!   dcfrac — mean 256·|Δblockmean| / SAD0: DC fraction of the zero-MV residual
//!            (the flash detector — the direct SAD-misleads mechanism)
//!   tact   — mean SAD0 per pixel (temporal activity)
//!   mgain  — (SAD0 − SADbest)/SAD0 over a ±8 step-4 grid (translational motion
//!            a full-pel search can actually capture — what B2 stresses)
//!   satdr  — mean SATD0 / SAD0 at zero MV (structure-vs-flat residual character)
//!   grad   — mean |∇x|+|∇y| per pixel (spatial detail / content scale)
//!
//!   cargo run --release -p rusty_h264-encoder --features asm --example b2_signals \
//!     -- video-tests/clips/*.y4m

use rusty_h264_common::transform::hadamard_4x4;
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

fn sad16(cur: &[u8], prev: &[u8], w: usize, h: usize, bx: usize, by: usize, dx: isize, dy: isize) -> Option<u32> {
    let (rx, ry) = (bx as isize + dx, by as isize + dy);
    if rx < 0 || ry < 0 || rx as usize + 16 > w || ry as usize + 16 > h {
        return None;
    }
    let (rx, ry) = (rx as usize, ry as usize);
    let mut s = 0u32;
    for r in 0..16 {
        let a = &cur[(by + r) * w + bx..][..16];
        let b = &prev[(ry + r) * w + rx..][..16];
        s += a.iter().zip(b).map(|(&p, &q)| p.abs_diff(q) as u32).sum::<u32>();
    }
    Some(s)
}

fn satd16_0(cur: &[u8], prev: &[u8], w: usize, bx: usize, by: usize) -> i64 {
    let mut total = 0i64;
    for sy in 0..4 {
        for sx in 0..4 {
            let mut blk = [0i32; 16];
            for r in 0..4 {
                for c in 0..4 {
                    let i = (by + sy * 4 + r) * w + bx + sx * 4 + c;
                    blk[r * 4 + c] = cur[i] as i32 - prev[i] as i32;
                }
            }
            total += hadamard_4x4(&blk).iter().map(|&v| v.unsigned_abs() as i64).sum::<i64>();
        }
    }
    total
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let npairs: usize = std::env::var("B2S_PAIRS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    // The Descent-H truth table (BD-PSNR %, λ=0.5): NEGATIVE = B2 wins there.
    let truth: &[(&str, f64)] = &[
        ("bus_cif", -1.71), ("football_cif", -1.90), ("foreman_cif", -0.46),
        ("foreman_qcif", -0.19), ("mobile_cif", -0.11), ("akiyo_cif", -0.10),
        ("shields", -0.07), ("FourPeople", -0.06), ("stockholm", -0.03),
        ("akiyo_qcif", -0.02), ("soccer_4cif", 0.07), ("tempete_cif", 0.13),
        ("harbour_4cif", 0.19), ("in_to_tree", 0.24), ("city_4cif", 0.35),
        ("crew_4cif", 0.91),
    ];
    println!(
        "{:<26} {:>7} | {:>7} {:>7} {:>7} {:>7} {:>7}",
        "clip", "BD", "dcfrac", "tact", "mgain", "satdr", "grad"
    );
    println!("{}", "-".repeat(84));
    for path in &args {
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let bd = truth
            .iter()
            .find(|(k, _)| name.contains(k))
            .map(|&(_, v)| v);
        let (w, h, frames) = read_y4m(path, npairs + 1);
        if frames.len() < 2 {
            continue;
        }
        let (mut dcfrac, mut tact, mut mgain, mut satdr, mut grad) = (0f64, 0f64, 0f64, 0f64, 0f64);
        let mut nb = 0u64;
        // Block grid subsampled 2× in each axis; interior only so the ±8 grid fits.
        let step = 32usize;
        for fi in 1..frames.len() {
            let (cur, prev) = (&frames[fi].y, &frames[fi - 1].y);
            let mut by = 16;
            while by + 32 <= h {
                let mut bx = 16;
                while bx + 32 <= w {
                    let s0 = sad16(cur, prev, w, h, bx, by, 0, 0).unwrap();
                    // DC fraction of the zero-MV residual.
                    let (mut ms, mut mp) = (0u32, 0u32);
                    for r in 0..16 {
                        ms += cur[(by + r) * w + bx..][..16].iter().map(|&v| v as u32).sum::<u32>();
                        mp += prev[(by + r) * w + bx..][..16].iter().map(|&v| v as u32).sum::<u32>();
                    }
                    dcfrac += ms.abs_diff(mp) as f64 / (s0 + 1) as f64;
                    tact += s0 as f64 / 256.0;
                    // Translational gain over a ±8 step-4 full-pel grid.
                    let mut best = s0;
                    let mut d = -8isize;
                    while d <= 8 {
                        let mut e = -8isize;
                        while e <= 8 {
                            if let Some(s) = sad16(cur, prev, w, h, bx, by, d, e) {
                                best = best.min(s);
                            }
                            e += 4;
                        }
                        d += 4;
                    }
                    mgain += (s0 - best) as f64 / (s0 + 1) as f64;
                    satdr += satd16_0(cur, prev, w, bx, by) as f64 / (s0 as f64 + 1.0);
                    // Spatial detail.
                    let mut g = 0u32;
                    for r in 0..16 {
                        for c in 0..16 {
                            let i = (by + r) * w + bx + c;
                            g += cur[i].abs_diff(cur[i + 1]) as u32
                                + cur[i].abs_diff(cur[i + w]) as u32;
                        }
                    }
                    grad += g as f64 / 256.0;
                    nb += 1;
                    bx += step;
                }
                by += step;
            }
        }
        let n = nb.max(1) as f64;
        println!(
            "{:<26} {:>+7.2} | {:>7.3} {:>7.2} {:>7.3} {:>7.2} {:>7.2}",
            name,
            bd.unwrap_or(f64::NAN),
            dcfrac / n,
            tact / n,
            mgain / n,
            satdr / n,
            grad / n
        );
    }
}
