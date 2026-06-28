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
    /// Wraps the whole `decode()` call — the denominator.
    Total = 5,
}

/// Number of buckets.
pub const N: usize = 6;

#[cfg(feature = "profile")]
mod imp {
    use super::{Stage, N};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    /// Index of the first non-`Total` stage — the residue sum runs `0..SUB`.
    const SUB: usize = Stage::Total as usize;

    const NAMES: [&str; N] = [
        "entropy/cavlc",
        "intra-pred",
        "inter-mc",
        "reconstruct",
        "deblock",
        "TOTAL decode()",
    ];

    static NS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    /// RAII timer: accumulates `Instant::now()..drop` into the stage's bucket.
    pub struct Guard {
        stage: usize,
        start: Instant,
    }

    impl Drop for Guard {
        #[inline]
        fn drop(&mut self) {
            let ns = self.start.elapsed().as_nanos() as u64;
            NS[self.stage].fetch_add(ns, Ordering::Relaxed);
            CALLS[self.stage].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn scope(s: Stage) -> Guard {
        Guard {
            stage: s as usize,
            start: Instant::now(),
        }
    }

    /// Zero all buckets — call before a clean measurement run.
    pub fn reset() {
        for a in NS.iter().chain(CALLS.iter()) {
            a.store(0, Ordering::Relaxed);
        }
    }

    /// Print the per-stage breakdown (does not reset).
    pub fn dump() {
        let load = |i: usize| NS[i].load(Ordering::Relaxed);
        let total = load(Stage::Total as usize).max(1);
        let sub_sum: u64 = (0..SUB).map(load).sum();
        let mgmt = total.saturating_sub(sub_sum);
        let pct = |ns: u64| 100.0 * ns as f64 / total as f64;
        let ms = |ns: u64| ns as f64 / 1e6;

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
