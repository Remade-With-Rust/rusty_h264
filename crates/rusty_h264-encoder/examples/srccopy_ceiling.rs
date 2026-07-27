//! Ceiling probe for the `enc-source-copy` brick.
//!
//! `coded_source` returns three OWNED planes; on an MB-aligned frame (the common
//! case) that is `frame.y.clone()` etc — a fresh heap allocation plus a full-frame
//! memcpy, three times per frame. The proposed brick returns `Cow::Borrowed`
//! instead, removing BOTH the allocation and the copy.
//!
//! This prices exactly that: clone-3-planes vs borrow-3-planes, per frame, at the
//! corpus's resolution rungs. Measured at 579 ms / 1920 calls = 301 us/frame in the
//! corpus profile (4.0% of the fast preset).
//!
//! Interleaved ABBA, median of N — the wall-clock null floor here is ~5%.

use std::time::Instant;

fn planes(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut s: u32 = 0x1234_5678;
    let mut mk = |n: usize| {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect::<Vec<u8>>()
    };
    (mk(w * h), mk(w * h / 4), mk(w * h / 4))
}

/// TODAY: three owned clones (alloc + memcpy each).
fn arm_clone(y: &[u8], u: &[u8], v: &[u8], iters: usize) -> f64 {
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        let (a, b, c) = (y.to_vec(), u.to_vec(), v.to_vec());
        acc = acc.wrapping_add(a[0] as u64 + b[0] as u64 + c[0] as u64);
        std::hint::black_box((&a, &b, &c));
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

/// BRICK: borrow the frame's planes (what `Cow::Borrowed` compiles to).
fn arm_borrow(y: &[u8], u: &[u8], v: &[u8], iters: usize) -> f64 {
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        let (a, b, c): (&[u8], &[u8], &[u8]) = (y, u, v);
        acc = acc.wrapping_add(a[0] as u64 + b[0] as u64 + c[0] as u64);
        std::hint::black_box((&a, &b, &c));
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

// Verbatim copies of the two edge-extension forms (the encoder's `clamp_plane` and
// its per-pixel oracle) so the probe can price the SECOND, larger half of this stage:
// the non-MB-aligned path that every 1080p frame takes.
fn clamp_rowwise(plane: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<u8> {
    let mut out = vec![0u8; ow * oh];
    for y in 0..oh {
        let sy = y.min(h - 1);
        let src = &plane[sy * w..sy * w + w];
        let dst = &mut out[y * ow..y * ow + ow];
        if ow <= w {
            dst.copy_from_slice(&src[..ow]);
        } else {
            dst[..w].copy_from_slice(src);
            dst[w..].fill(src[w - 1]);
        }
    }
    out
}

fn clamp_per_pixel(plane: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<u8> {
    let mut out = vec![0u8; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            out[y * ow + x] = plane[y.min(h - 1) * w + x.min(w - 1)];
        }
    }
    out
}

/// Prices the unaligned path: one frame = luma + two chroma planes, edge-extended.
fn clamp_frame(f: fn(&[u8], usize, usize, usize, usize) -> Vec<u8>, y: &[u8], u: &[u8], v: &[u8], iters: usize) -> f64 {
    let (w, h, ow, oh) = (1920usize, 1080usize, 1920usize, 1088usize);
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        let a = f(y, w, h, ow, oh);
        let b = f(u, w / 2, h / 2, ow / 2, oh / 2);
        let c = f(v, w / 2, h / 2, ow / 2, oh / 2);
        acc = acc.wrapping_add(a[0] as u64 + b[0] as u64 + c[0] as u64);
        std::hint::black_box((&a, &b, &c));
    }
    let e = t.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    e * 1e9 / iters as f64
}

fn main() {
    // The corpus rungs, with their frame counts, so the projection is weighted the
    // way the real 1920-frame corpus is.
    let rungs: [(&str, usize, usize, usize); 5] = [
        ("QCIF 176x144", 176, 144, 240),
        ("CIF 352x288", 352, 288, 720),
        ("4CIF 704x576", 704, 576, 480),
        ("720p 1280x720", 1280, 720, 240),
        ("1080p 1920x1080", 1920, 1080, 240),
    ];
    let rounds = 7;

    println!("enc-source-copy ceiling — clone-3-planes vs borrow, per frame\n");
    println!("{:<18} {:>7} {:>12} {:>12} {:>10}", "rung", "frames", "clone ns", "borrow ns", "saved ns");
    println!("{}", "-".repeat(64));

    let mut total_saved_ms = 0.0;
    let mut total_frames = 0usize;
    for (name, w, h, frames) in rungs {
        let (y, u, v) = planes(w, h);
        let iters = (2_000_000 / (w * h / 1000).max(1)).clamp(200, 20_000);
        let (mut ca, mut ba) = (Vec::new(), Vec::new());
        for r in 0..rounds {
            if r % 2 == 0 {
                ca.push(arm_clone(&y, &u, &v, iters));
                ba.push(arm_borrow(&y, &u, &v, iters));
            } else {
                ba.push(arm_borrow(&y, &u, &v, iters));
                ca.push(arm_clone(&y, &u, &v, iters));
            }
        }
        let med = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let (c, b) = (med(&mut ca), med(&mut ba));
        println!("{:<18} {:>7} {:>12.0} {:>12.0} {:>10.0}", name, frames, c, b, c - b);
        total_saved_ms += (c - b) * frames as f64 / 1e6;
        total_frames += frames;
    }

    // The unaligned (edge-extension) path — the other, larger half of this stage.
    {
        let (y, u, v) = planes(1920, 1080);
        let (mut pa, mut ra) = (Vec::new(), Vec::new());
        for r in 0..rounds {
            if r % 2 == 0 {
                pa.push(clamp_frame(clamp_per_pixel, &y, &u, &v, 12));
                ra.push(clamp_frame(clamp_rowwise, &y, &u, &v, 12));
            } else {
                ra.push(clamp_frame(clamp_rowwise, &y, &u, &v, 12));
                pa.push(clamp_frame(clamp_per_pixel, &y, &u, &v, 12));
            }
        }
        let med = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let (p, rw) = (med(&mut pa), med(&mut ra));
        println!("\n--- unaligned path (1080p: 1080 -> coded 1088), per frame ---");
        println!("  per-pixel clamp   {:>10.0} ns", p);
        println!("  row-wise clamp    {:>10.0} ns   ({:.2}x faster)", rw, p / rw);
        println!("  saved over 240 1080p frames: {:.1} ms", (p - rw) * 240.0 / 1e6);
    }

    println!("\n--- projected on the 1920-frame corpus ---");
    println!("  frames                       {total_frames}");
    println!("  enc-source-copy measured     579.0 ms  (4.0% of the fast preset's 14319 ms)");
    println!("  removable by borrowing       {total_saved_ms:>5.1} ms");
    println!(
        "  => {:.1}% of the stage, {:.2}% of the fast preset",
        100.0 * total_saved_ms / 579.0,
        100.0 * total_saved_ms / 14319.0
    );
}
