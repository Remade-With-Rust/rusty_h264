//! The BIT ACCOUNTANT — `codec-analyzer` instrument #6, the rate-domain twin of
//! the stage profiler.
//!
//! The stage profiler buckets nanoseconds per stage; this buckets BITS per
//! syntax element. A stage that is 5% of encode TIME can be 40% of the
//! BITRATE, and the remaining ~4% BD-rate gap vs x264 veryfast is a rate
//! question, so it needs the rate instrument.
//!
//! **Reconciliation is the whole design.** Buckets are deltas of the CABAC
//! coder's exact emitted-bit position (`CabacEncoder::pos`), so accounted bits
//! sum EXACTLY to the coded slice payload; `dump()` prints
//! accounted-vs-actual and the residue. An accountant that cannot reconcile is
//! measuring nothing (rav1e: 96.7% = working, 340% = broken).
//!
//! Observe-only and env-gated (`RFF_BITACCT=1`): when off, every tap is an
//! atomic load of a `bool` and the encoder's output is byte-identical.
//!
//! Buckets mirror x264's own `i_mv_bits` / `i_tex_bits` / `i_misc_bits` split
//! (so the comparison is like-for-like) but finer, because "misc" is where our
//! suspected overhead would hide.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Syntax-element buckets. Order is the dump order.
#[derive(Clone, Copy)]
pub enum B {
    SkipFlag = 0,
    MbType = 1,
    RefIdx = 2,
    Mvd = 3,
    IntraBody = 4,
    Cbp = 5,
    QpDelta = 6,
    ResidLuma = 7,
    ResidChroma = 8,
    Terminate = 9,
}

pub const N: usize = 10;
const NAMES: [&str; N] = [
    "mb_skip_flag",
    "mb_type/sub_type",
    "ref_idx",
    "mvd (MOTION)",
    "intra MB body (I+P)",
    "cbp",
    "mb_qp_delta",
    "residual luma (TEX)",
    "residual chroma (TEX)",
    "end_of_slice",
];

static ON: AtomicBool = AtomicBool::new(false);
static BITS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static COUNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
/// Actual coded payload bits, for the reconciliation line.
static ACTUAL: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Turn accounting on (from the harness) — never on by default.
pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn init_from_env() {
    if std::env::var("RFF_BITACCT").map(|v| v != "0").unwrap_or(false) {
        set_enabled(true);
    }
}

#[inline]
pub fn add(b: B, bits: u64) {
    BITS[b as usize].fetch_add(bits, Ordering::Relaxed);
    COUNT[b as usize].fetch_add(1, Ordering::Relaxed);
}

/// Record the real coded size of a finished slice payload (bytes → bits).
pub fn add_actual_bytes(n: usize) {
    ACTUAL.fetch_add(n as u64 * 8, Ordering::Relaxed);
}

pub fn reset() {
    for i in 0..N {
        BITS[i].store(0, Ordering::Relaxed);
        COUNT[i].store(0, Ordering::Relaxed);
    }
    ACTUAL.store(0, Ordering::Relaxed);
}

/// The report: per-element bits, share, and the reconciliation against the real
/// payload. `mbs` scales the per-MB column.
pub fn dump(label: &str, mbs: u64) {
    let vals: Vec<u64> = (0..N).map(|i| BITS[i].load(Ordering::Relaxed)).collect();
    let cnts: Vec<u64> = (0..N).map(|i| COUNT[i].load(Ordering::Relaxed)).collect();
    let accounted: u64 = vals.iter().sum();
    let actual = ACTUAL.load(Ordering::Relaxed);
    println!("\n=== BIT ACCOUNTANT — {label} ===");
    println!(
        "{:<24}{:>12}{:>9}{:>12}{:>12}",
        "syntax element", "bits", "share", "bits/MB", "elements"
    );
    println!("{}", "-".repeat(69));
    for i in 0..N {
        if cnts[i] == 0 && vals[i] == 0 {
            continue;
        }
        println!(
            "{:<24}{:>12}{:>8.1}%{:>12.1}{:>12}",
            NAMES[i],
            vals[i],
            100.0 * vals[i] as f64 / accounted.max(1) as f64,
            vals[i] as f64 / mbs.max(1) as f64,
            cnts[i]
        );
    }
    println!("{}", "-".repeat(69));
    // x264-comparable rollup (its i_mv_bits / i_tex_bits / i_misc_bits split).
    let mv = vals[B::Mvd as usize] + vals[B::RefIdx as usize];
    let tex = vals[B::ResidLuma as usize] + vals[B::ResidChroma as usize];
    let misc = accounted - mv - tex;
    let pc = |v: u64| 100.0 * v as f64 / accounted.max(1) as f64;
    println!(
        "x264-comparable:  MOTION {:.1}%   TEXTURE {:.1}%   MISC {:.1}%",
        pc(mv),
        pc(tex),
        pc(misc)
    );
    // THE line that makes this an instrument rather than a model.
    println!(
        "reconciliation:   accounted {accounted} / actual {actual} = {:.1}%  (residue {} bits = slice headers + NAL + flush)",
        100.0 * accounted as f64 / actual.max(1) as f64,
        actual as i64 - accounted as i64
    );
}
