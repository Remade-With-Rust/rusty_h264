//! The encoder's memory model, held to a measurement.
//!
//! [`rusty_h264_encoder::EncoderConfig::memory_estimate`] is a formula in
//! width and height; the test here installs a counting allocator and compares
//! the formula with the bytes the encoder actually holds while coding a
//! P-frame. It lives in its own crate (and its own test binary) so no other
//! test's allocations land in the count, and because a `#[global_allocator]`
//! is `unsafe`, which the codec crates forbid.
//!
//! Run it as CI's scalar arm does:
//!
//! ```text
//! cargo test -p rusty_h264-memprobe --features probe --release -- --nocapture
//! ```

#[cfg(all(test, feature = "probe"))]
mod probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rusty_h264_common::YuvFrame;
    use rusty_h264_encoder::{Encoder, EncoderConfig, Preset};

    struct Counting;

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = System.alloc(l);
            if !p.is_null() {
                let n = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
                PEAK.fetch_max(n, Ordering::Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            System.dealloc(p, l)
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = System.realloc(p, l, new);
            if !q.is_null() {
                LIVE.fetch_sub(l.size(), Ordering::Relaxed);
                let n = LIVE.fetch_add(new, Ordering::Relaxed) + new;
                PEAK.fetch_max(n, Ordering::Relaxed);
            }
            q
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    fn frame(w: usize, h: usize, t: usize) -> YuvFrame {
        let mut f = YuvFrame::black(w, h);
        for y in 0..h {
            for x in 0..w {
                f.y[y * w + x] = (((x + 2 * t) * 255 / w) + (y * 5 + x) % 17).min(255) as u8;
            }
        }
        f
    }

    /// Peak bytes held while coding one P-frame (the steady state), beyond
    /// what the test itself holds, for `cfg`.
    fn measure(cfg: &EncoderConfig, label: &str) -> usize {
        let (w, h) = (cfg.width, cfg.height);
        let frames: Vec<YuvFrame> = (0..3).map(|t| frame(w, h, t)).collect();
        let base = LIVE.load(Ordering::Relaxed);
        let mut enc = Encoder::new(cfg.clone()).unwrap();
        let _ = enc.encode(&frames[0]);
        let _ = enc.encode(&frames[1]);
        // The steady state: references in place, now code a P-frame and watch the peak.
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
        let au = enc.encode(&frames[2]);
        let peak = PEAK.load(Ordering::Relaxed) - base;
        let resident = LIVE.load(Ordering::Relaxed) - base;
        let est = cfg.memory_estimate();
        eprintln!(
            "[memory] {label} {w}x{h}: peak {peak} B, resident {resident} B, AU {} B; estimate total {} B \
             (per_ref {} x{}, mb_arrays {}, hpel {}, scratch {})",
            au.len(),
            est.total,
            est.per_ref_frame,
            est.ref_frames,
            est.mb_arrays,
            est.hpel_cache,
            est.scratch
        );
        drop(enc);
        peak
    }

    #[test]
    fn estimate_is_within_a_stated_margin_of_the_measurement() {
        // The chip configuration at QVGA, and a sub-pel preset (half-pel cache).
        let mut chip = EncoderConfig::baseline(320, 240);
        chip.gop_size = 15;
        chip.min_keyint = 15;
        let mut subpel = EncoderConfig::baseline(320, 240);
        subpel.preset = Preset::Balanced;
        subpel.gop_size = 15;
        subpel.min_keyint = 15;
        // And a small odd size, so the MB-aligned rounding is exercised.
        let mut small = EncoderConfig::baseline(100, 60);
        small.gop_size = 15;
        small.min_keyint = 15;

        for (label, cfg) in [("chip", chip), ("subpel", subpel), ("small", small)] {
            let peak = measure(&cfg, label);
            let est = cfg.memory_estimate().total;
            // The model may be up to 25% above or below what the host holds.
            let (lo, hi) = (est * 3 / 4, est * 5 / 4);
            assert!(
                (lo..=hi).contains(&peak),
                "{label}: measured peak {peak} B outside {lo}..={hi} B (estimate {est} B)"
            );
        }
    }
}
