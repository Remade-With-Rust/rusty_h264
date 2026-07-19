//! Full primitive speed map — times EVERY hot kernel we expose (ns/call, Mcalls/s,
//! est. cycles/call) so each can be ranked and compared to x264's equivalent.
//! Run: `cargo run --release -p rusty_h264-accel --example primitive_map`
//! x86-64 only.
#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code, unused_imports))]

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    eprintln!("primitive_map: x86_64-only.");
}

#[cfg(target_arch = "x86_64")]
fn main() {
    use core::arch::x86_64::_rdtsc;
    use std::hint::black_box;
    use std::time::Instant;

    // Calibrate TSC → GHz for est. cycles/call.
    let ghz = {
        let t = Instant::now();
        let c0 = unsafe { _rdtsc() };
        while t.elapsed().as_millis() < 200 {}
        let c1 = unsafe { _rdtsc() };
        (c1 - c0) as f64 / t.elapsed().as_secs_f64() / 1e9
    };
    eprintln!("TSC ~{ghz:.2} GHz (ref); est. cycles = ns/call × GHz.\n");

    // One big 32-aligned arena; every op works at interior offset so no kernel can
    // read/write out of bounds (deblock reads -4 rows, MC reads ±3 taps, etc.).
    const S: usize = 128; // stride
    const OFF: usize = 20 * S + 32; // interior anchor, 32-aligned (deblock movdqa needs 16)
    #[repr(align(32))]
    struct Arena([u8; S * S]);
    let mut a = Arena([0u8; S * S]);
    let mut b = Arena([0u8; S * S]);
    for i in 0..S * S {
        a.0[i] = (i.wrapping_mul(7).wrapping_add(11) & 0xff) as u8;
        b.0[i] = (i.wrapping_mul(13).wrapping_add(5) & 0xff) as u8;
    }
    // 16-stride packed source MB for the SAD path (2 rows contiguous).
    #[repr(align(32))]
    struct Packed([u8; 256]);
    let mut packed = Packed([0u8; 256]);
    for i in 0..256 {
        packed.0[i] = (i.wrapping_mul(7).wrapping_add(11) & 0xff) as u8;
    }
    #[repr(align(32))]
    struct Ai([i16; 64]);
    let mut dct = Ai([0i16; 64]);
    #[repr(align(32))]
    struct Pred([u8; 256]);
    let mut pred16 = Pred([0u8; 256]);
    let mut pred8 = Pred([0u8; 256]);

    fn time<F: FnMut()>(iters: u64, mut f: F) -> f64 {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            for _ in 0..iters {
                f();
            }
            let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
            if ns < best {
                best = ns;
            }
        }
        best
    }

    let mut rows: Vec<(&str, &str, &str, f64)> = Vec::new();
    let n = 20_000_000u64;
    let acc = rusty_h264_accel::sad_16x16(&packed.0, 16, &b.0[OFF..], S); // warm
    let _ = acc;

    macro_rules! row {
        ($name:expr, $cat:expr, $isa:expr, $iters:expr, $body:expr) => {
            eprint!("  timing {} ...", $name);
            let _ns = time($iters, || $body);
            eprintln!(" {:.2} ns", _ns);
            rows.push(($name, $cat, $isa, _ns));
        };
    }

    // ---- SAD (fast-preset ME) ----
    row!("sad_16x16", "ME/SAD", "sse2", n, { black_box(rusty_h264_accel::sad_16x16(black_box(&packed.0), 16, black_box(&b.0[OFF..]), S)); });
    row!("sad_16x8", "ME/SAD", "sse2", n, { black_box(rusty_h264_accel::sad_16x8(black_box(&packed.0), 16, black_box(&b.0[OFF..]), S)); });
    row!("sad_8x16", "ME/SAD", "sse2", n, { black_box(rusty_h264_accel::sad_8x16(black_box(&packed.0), 16, black_box(&b.0[OFF..]), S)); });

    // ---- SATD (quality-preset mode decision) ----
    row!("satd_4x4", "ME/SATD", "avx2", n, { black_box(rusty_h264_accel::satd_4x4(black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S)); });
    row!("satd_8x8", "ME/SATD", "avx2", n, { black_box(rusty_h264_accel::satd_8x8(black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S)); });
    row!("satd_16x8", "ME/SATD", "avx2", n, { black_box(rusty_h264_accel::satd_16x8(black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S)); });
    row!("satd_8x16", "ME/SATD", "avx2", n, { black_box(rusty_h264_accel::satd_8x16(black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S)); });
    row!("satd_16x16", "ME/SATD", "avx2", n, { black_box(rusty_h264_accel::satd_16x16(black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S)); });

    // ---- Transform / quant / recon (per 4-block quad) ----
    let ff = [85i16; 8];
    let mf = [400i16; 8];
    row!("dct_four_t4", "Transform", "avx2", n, { rusty_h264_accel::dct_four_t4(black_box(&mut dct.0), black_box(&a.0[OFF..]), S, black_box(&b.0[OFF..]), S); });
    row!("quant_four_4x4", "Transform", "avx2", n, { rusty_h264_accel::quant_four_4x4(black_box(&mut dct.0), black_box(&ff), black_box(&mf)); });
    row!("idct_four_t4_rec", "Transform", "avx2", n, { rusty_h264_accel::idct_four_t4_rec(black_box(&mut a.0[OFF..]), S, black_box(&b.0[OFF..]), S, black_box(&dct.0)); });

    // ---- Motion compensation (half-pel; full-pel = memcpy, listed separately) ----
    row!("mc_hor20 16x16", "MC", "avx2", n / 2, { rusty_h264_accel::mc_hor20(black_box(&b.0), OFF, S, black_box(&mut a.0[OFF..]), 16, 16); });
    row!("mc_ver02 16x16", "MC", "avx2", n / 2, { rusty_h264_accel::mc_ver02(black_box(&b.0), OFF, S, black_box(&mut a.0[OFF..]), 16, 16); });
    row!("mc_centre 8x8", "MC", "sse2", n / 2, { rusty_h264_accel::mc_centre(black_box(&b.0[OFF..]), S, black_box(&mut a.0[OFF..]), 8, 8); });
    let abcd = [4u8, 4, 4, 4];
    row!("mc_chroma_w8 8x8", "MC", "sse2", n / 2, { rusty_h264_accel::mc_chroma_w8(black_box(&b.0[OFF..]), S, black_box(&mut a.0[OFF..]), 8, &abcd, 8); });

    // ---- Deblock (per edge) ----
    let tc = [2i8; 4];
    row!("deblock_luma_lt4_v", "Deblock", "ssse3", n / 2, { rusty_h264_accel::deblock_luma_lt4_v(black_box(&mut a.0[OFF..]), S, 12, 3, &tc); });
    row!("deblock_luma_eq4_v", "Deblock", "ssse3", n / 2, { rusty_h264_accel::deblock_luma_eq4_v(black_box(&mut a.0[OFF..]), S, 12, 3); });
    row!("deblock_luma_lt4_h", "Deblock", "ssse3", n / 2, { rusty_h264_accel::deblock_luma_lt4_h(black_box(&mut a.0[OFF..]), S, 12, 3, &tc); });
    row!("deblock_chroma_lt4_v", "Deblock", "ssse3", n / 2, { rusty_h264_accel::deblock_chroma_lt4_v(black_box(&mut a.0[OFF..]), black_box(&mut b.0[OFF..]), S, 12, 3, &tc); });

    // ---- Intra prediction (base 16-aligned so the kernel's aligned reads are safe) ----
    row!("i16x16_pred (V)", "Intra", "sse2", n, { rusty_h264_accel::i16x16_luma_pred(0, black_box(&mut pred16.0), black_box(&a.0[OFF..]), S + 16, S); });
    row!("i16x16_pred (Plane)", "Intra", "sse2", n, { rusty_h264_accel::i16x16_luma_pred(3, black_box(&mut pred16.0), black_box(&a.0[OFF..]), S + 16, S); });
    row!("chroma8x8_pred (Plane)", "Intra", "sse2", n, { rusty_h264_accel::chroma8x8_pred(3, black_box(&mut pred8.0[..64]), black_box(&a.0[OFF..]), S + 16, S); });

    rows.sort_by(|x, y| x.1.cmp(y.1).then(x.3.partial_cmp(&y.3).unwrap()));
    println!("{:<26} {:<10} {:<7} {:>9} {:>10} {:>9}", "primitive", "category", "isa", "ns/call", "Mcalls/s", "~cycles");
    println!("{}", "-".repeat(76));
    for (name, c, isa, ns) in &rows {
        println!("{:<26} {:<10} {:<7} {:>9.2} {:>10.1} {:>9.0}", name, c, isa, ns, 1000.0 / ns, ns * ghz);
    }
}
