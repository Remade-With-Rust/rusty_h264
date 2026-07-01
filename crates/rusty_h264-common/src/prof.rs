//! Feature-gated decode/encode **stage profiler** — the instrument for perf work.
//!
//! Zero cost unless the `profile` feature is enabled: with it off, [`scope`] is a
//! no-op returning a ZST guard that the optimizer elides entirely, so release
//! builds are byte-identical and the hot path is untouched. With it on, each
//! kernel times itself into an atomic nanosecond bucket; [`dump`] prints the
//! per-stage breakdown.
//!
//! Design mirrors `rff-codec-mp3`'s `encode::prof`. The kernels (`mc_luma`,
//! `reconstruct_4x4`, `decode_residual_block`, the intra predictors, `deblock`)
//! each open a [`scope`] at their top, so every call is captured with one edit.
//! A [`Stage::Total`] scope wraps the whole `decode()` call; the **`mgmt/other`**
//! line is the residue (`Total − Σ stages`) — i.e. per-MB management, MV
//! prediction, nnz/grid bookkeeping, dequant — the bucket we most want to shrink.
//!
//! Caveat for honest reading: the fine-grained buckets (`reconstruct`, `entropy`)
//! are entered millions of times, so each carries ~one `Instant::now()` of timer
//! overhead — their share is mildly inflated and `mgmt/other` mildly deflated.
//! The `(N calls)` column lets you judge ns/call. Measure **throughput** with the
//! `profile` feature OFF (no timer overhead); use this breakdown only to rank
//! stages.

/// A timed pipeline stage. Order matters: everything before [`Total`](Stage::Total)
/// is a sub-component summed for the `mgmt/other` residue.
#[derive(Clone, Copy)]
pub enum Stage {
    Entropy = 0,
    IntraPred = 1,
    InterMc = 2,
    Reconstruct = 3,
    Deblock = 4,
    // --- Phase 1: decomposition of the former "mgmt/other" residue ---
    /// Inverse quantization (`dequantize*`, `inverse_quant_8x8`).
    Dequant = 5,
    /// Scattering a reconstructed block into the strided frame plane (`store`).
    Scatter = 6,
    /// Re-striding the MC output into the per-MB prediction buffer.
    PredBuf = 7,
    /// MV prediction + per-block motion/ref/coded grid writes.
    MvGrid = 8,
    // --- Phase 3 / ghost-tracking: further decomposition of the residue ---
    /// Neighbour derivation for prediction (MV/ref/intra-mode availability + reads).
    Neighbors = 9,
    /// P_Skip / B_Skip reconstruction (the pred→rec copies + grid writes, no residual).
    SkipRecon = 10,
    /// Per-frame finalize: output-frame build (crop), DPB / reference management.
    Finalize = 11,
    /// Per-MB non-residual syntax parse (mb_skip_run, mb_type, cbp, mb_qp_delta).
    Syntax = 12,
    /// Wraps the whole `decode()` call — the denominator.
    Total = 13,
}

/// Number of buckets.
pub const N: usize = 14;

#[cfg(feature = "profile")]
mod imp {
    use super::{Stage, N};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    /// Index of the first non-`Total` stage — the residue sum runs `0..SUB`.
    const SUB: usize = Stage::Total as usize;

    /// A cheap monotonic tick. On x86_64 this is `rdtsc` (~5-10 ns, ~3-5× cheaper
    /// than `Instant::now()` = QueryPerformanceCounter ~20-30 ns on Windows), which
    /// is what dominated the profiler's own overhead (~1M scope entries × 2 calls).
    /// Buckets accumulate *ticks*; `dump()` converts to ns via a run-length TSC
    /// calibration (invariant TSC → ticks are wall-time-proportional). Elsewhere we
    /// fall back to `Instant` nanos so the profiler still builds cross-arch.
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn ticks() -> u64 {
        // SAFETY: `_rdtsc` is a pure timestamp read with no memory effects; it is
        // `unsafe` only because it is a target intrinsic. Reordering is immaterial to
        // coarse scope timing. Compiled only under `feature = "profile"` (dev tool).
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    fn ticks() -> u64 {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }

    /// (wall-clock, tick-count) sampled at `reset()` — the calibration anchor read at
    /// `dump()` to recover ns-per-tick. Touched twice per run, so its `Mutex` cost is
    /// irrelevant next to the per-scope path.
    static ANCHOR: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

    const NAMES: [&str; N] = [
        "entropy/cavlc",
        "intra-pred",
        "inter-mc",
        "reconstruct",
        "deblock",
        "dequant",
        "scatter(store)",
        "pred-buf copy",
        "mv+grid",
        "neighbors",
        "skip-recon",
        "finalize",
        "syntax-parse",
        "TOTAL decode()",
    ];

    static NS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    /// RAII timer: accumulates `ticks()..drop` (rdtsc cycles) into the stage's bucket.
    pub struct Guard {
        stage: usize,
        start: u64,
    }

    impl Drop for Guard {
        #[inline]
        fn drop(&mut self) {
            let d = ticks().wrapping_sub(self.start);
            NS[self.stage].fetch_add(d, Ordering::Relaxed);
            CALLS[self.stage].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn scope(s: Stage) -> Guard {
        Guard {
            stage: s as usize,
            start: ticks(),
        }
    }

    /// Zero all buckets and sample the calibration anchor — call before a clean run.
    pub fn reset() {
        for a in NS.iter().chain(CALLS.iter()) {
            a.store(0, Ordering::Relaxed);
        }
        *ANCHOR.lock().unwrap() = Some((Instant::now(), ticks()));
    }

    /// Print the per-stage breakdown (does not reset).
    pub fn dump() {
        let load = |i: usize| NS[i].load(Ordering::Relaxed);
        let total = load(Stage::Total as usize).max(1);
        let sub_sum: u64 = (0..SUB).map(load).sum();
        let mgmt = total.saturating_sub(sub_sum);
        // Recover ns-per-tick from the reset→now anchor: elapsed wall / elapsed ticks.
        // (Percentages are tick ratios and need no calibration; only ms display does.)
        let ns_per_tick = ANCHOR
            .lock()
            .unwrap()
            .map(|(t0, c0)| {
                let wall = t0.elapsed().as_nanos() as f64;
                let cyc = ticks().wrapping_sub(c0) as f64;
                if cyc > 0.0 {
                    wall / cyc
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0);
        let pct = |t: u64| 100.0 * t as f64 / total as f64;
        let ms = |t: u64| t as f64 * ns_per_tick / 1e6;

        eprintln!(
            "\n--- decode stage profile (decode() wall = {:.1} ms) ---",
            ms(total)
        );
        for i in 0..SUB {
            eprintln!(
                "  {:<15} {:>8.1} ms  {:>5.1}%   ({} calls)",
                NAMES[i],
                ms(load(i)),
                pct(load(i)),
                CALLS[i].load(Ordering::Relaxed),
            );
        }
        eprintln!(
            "  {:<15} {:>8.1} ms  {:>5.1}%   <- the OTHER bucket: mb mgmt / mv-pred / nnz / grid / dequant",
            "mgmt/other",
            ms(mgmt),
            pct(mgmt),
        );
        eprintln!("  {:<15} {:>8.1} ms  100.0%", NAMES[SUB], ms(total));
    }
}

#[cfg(not(feature = "profile"))]
mod imp {
    use super::Stage;

    /// No-op guard (ZST) — elided in release.
    pub struct Guard;

    #[inline(always)]
    pub fn scope(_s: Stage) -> Guard {
        Guard
    }
    #[inline(always)]
    pub fn reset() {}
    #[inline(always)]
    pub fn dump() {}
}

pub use imp::{dump, reset, scope, Guard};
