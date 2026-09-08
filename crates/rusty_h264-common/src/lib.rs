//! Shared primitives for the `rusty_h264` pure-Rust H.264 codec.
//!
//! # Global allocator
//!
//! The `global-alloc` feature (on by **default**) installs [`rusty_alloc`] as the
//! process-wide allocator so measured and shipped routes share one allocator.
//!
//! `#[global_allocator]` is process-wide (exactly one per program). Downstream
//! crates that declare their own allocator should depend on this crate with
//! `default-features = false` (then re-enable `asm` / other features as needed).
//!
//! This crate is the foundation both the encoder and decoder sit on. It is
//! `#![forbid(unsafe_code)]`: the bit-twiddling core of an H.264 codec is
//! exactly where memory-safety bugs hide in the C implementations, so we keep
//! it provably safe.
//!
//! Modules mirror the concerns shared across `codec/common` in Cisco's
//! openh264:
//! - [`bit_writer`] / [`bit_reader`] — MSB-first bit packing + Exp-Golomb.
//! - [`nal`] — NAL units, Annex-B framing, RBSP emulation prevention.
//! - [`types`] — shared enums and the raw YUV frame container.
//!
//! The shipped build is `#![forbid(unsafe_code)]`. The `profile` feature (a
//! measurement-only dev build, never shipped) relaxes this to unlock the `rdtsc`
//! timer in [`prof`]; that is the *only* unsafe in the crate and only under `profile`.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(feature = "profile"), forbid(unsafe_code))]

extern crate alloc;
#[cfg(test)]
extern crate std;
#[cfg(all(not(feature = "std"), not(feature = "libm")))]
compile_error!(
    "rusty_h264-common without `std` needs the `libm` feature for its floating-point math"
);

// ---------------------------------------------------------------------------
// `no_std` shims. Every `std` use left in this crate is a diagnostic: an
// environment knob (`RS_H264_*` / `RFF_*`), a stderr census, the profiler.
// Without `std` there is no environment and no stderr, so a knob reads as
// unset and a print is a no-op: the shipped defaults, which is what a chip
// runs. Defined here, before the modules, so they are in textual scope.
// ---------------------------------------------------------------------------

/// Read an environment knob. `None` without `std` (no environment).
#[cfg(feature = "std")]
#[doc(hidden)]
pub fn knob(name: &str) -> Option<alloc::string::String> {
    std::env::var(name).ok()
}
/// Read an environment knob. `None` without `std` (no environment).
#[cfg(not(feature = "std"))]
#[doc(hidden)]
pub fn knob(_name: &str) -> Option<alloc::string::String> {
    None
}

/// A knob evaluated once and cached (`OnceLock` under `std`); evaluated per
/// call without `std`, where `knob` is always `None` and the expression
/// folds to its default.
#[doc(hidden)]
#[macro_export]
macro_rules! cached_knob {
    ($ty:ty, $init:expr) => {{
        #[cfg(feature = "std")]
        {
            static V: ::std::sync::OnceLock<$ty> = ::std::sync::OnceLock::new();
            *V.get_or_init(|| $init)
        }
        #[cfg(not(feature = "std"))]
        {
            // No environment: `knob` yields `None`, so `$init` folds to its
            // own default. Evaluated per call; it is a few Option combinators.
            $init
        }
    }};
    ($ty:ty, $default:expr, $init:expr) => {{
        let _ = $default;
        $crate::cached_knob!($ty, $init)
    }};
}

#[cfg(not(feature = "std"))]
macro_rules! eprintln {
    ($($t:tt)*) => {{
        let _ = ::core::format_args!($($t)*);
    }};
}
#[cfg(not(feature = "std"))]
#[allow(unused_macros)]
macro_rules! println {
    ($($t:tt)*) => {{
        let _ = ::core::format_args!($($t)*);
    }};
}

/// Process-wide allocator (see the crate docs).
#[cfg(all(feature = "global-alloc", not(feature = "profile")))]
#[global_allocator]
static ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

/// COUNTING TWIN, `profile` builds only. A recycle pool that stops being fed
/// does not fail -- it degrades silently to "always allocate", and no
/// correctness gate can see that, because where a buffer came from cannot
/// change decoded output. The decoder's padded-plane pool sat at a 100% miss
/// rate behind a green 68-stream byte-identity gate for exactly that reason.
/// Auditing pools one at a time found it; counting allocations finds the NEXT
/// one without an audit.
///
/// Wrapping the allocator needs `unsafe`, which is why this is gated on
/// `profile` -- the same measurement-only build that already relaxes
/// `forbid(unsafe_code)` for the rdtsc timer. The shipped build keeps the bare
/// declaration above and counts nothing.
#[cfg(all(feature = "global-alloc", feature = "profile"))]
mod counting_alloc {
    use core::alloc::{GlobalAlloc, Layout};
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub static CALLS: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    /// Big allocations are the diagnostic ones: a picture-sized buffer that
    /// recurs is a pool that is not recycling. Small churn is ordinary.
    pub static BIG_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static BIG_BYTES: AtomicU64 = AtomicU64::new(0);
    pub static REALLOCS: AtomicU64 = AtomicU64::new(0);
    pub static REALLOC_128: AtomicU64 = AtomicU64::new(0);
    const BIG: usize = 64 * 1024;

    pub struct Counting(pub rusty_alloc_api::RustyAlloc);

    /// Size histogram, so "776,000 small allocations" becomes a place to look
    /// rather than a number to guess about.
    pub static BUCKETS: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];
    pub const EDGES: [usize; 10] = [32, 40, 48, 64, 80, 96, 128, 512, 65536, usize::MAX];

    /// WHICH CALL SITE. A size histogram localises an allocation to a band; it
    /// cannot name the code. Capturing a backtrace INSIDE the allocator is
    /// re-entrant by construction -- `Backtrace::force_capture` allocates -- so
    /// a thread-local guard lets the nested allocations through uncaptured.
    /// Sampled once, past warm-up, and only when `RS_H264_ALLOC_SITE` names a
    /// byte size: an instrument that answers "who" and then gets out of the way.
    #[cfg(feature = "std")]
    pub mod site {
        use core::cell::Cell;
        use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

        std::thread_local! {
            static BUSY: Cell<bool> = const { Cell::new(false) };
        }
        pub static WANT: AtomicUsize = AtomicUsize::new(usize::MAX);
        pub static SKIP: AtomicU64 = AtomicU64::new(0);
        pub static CAUGHT: std::sync::Mutex<Option<std::string::String>> =
            std::sync::Mutex::new(None);

        /// Read the knob once; `usize::MAX` means "never sample".
        pub fn arm() {
            if let Ok(v) = std::env::var("RS_H264_ALLOC_SITE") {
                if let Ok(n) = v.parse::<usize>() {
                    WANT.store(n, Relaxed);
                }
            }
        }

        pub fn maybe(n: usize) {
            let want = WANT.load(Relaxed);
            // Match a 32-byte WINDOW starting at the knob: a histogram band,
            // not an exact size, is what the census actually hands you.
            if want == usize::MAX || n < want || n > want + 31 {
                return;
            }
            // Let the first N through: the interesting site is the steady-state
            // one, not whatever ran during start-up.
            if SKIP.fetch_add(1, Relaxed) < 5_000 {
                return;
            }
            BUSY.with(|b| {
                if b.get() {
                    return;
                }
                b.set(true);
                if let Ok(mut g) = CAUGHT.try_lock() {
                    if g.is_none() {
                        *g = Some(std::format!("{}", std::backtrace::Backtrace::force_capture()));
                    }
                }
                b.set(false);
            });
        }
    }

    #[inline]
    fn note(n: usize) {
        #[cfg(feature = "std")]
        site::maybe(n);
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n as u64, Relaxed);
        let mut b = 0;
        while b < 9 && n > EDGES[b] {
            b += 1;
        }
        BUCKETS[b].fetch_add(1, Relaxed);
        if n >= BIG {
            BIG_CALLS.fetch_add(1, Relaxed);
            BIG_BYTES.fetch_add(n as u64, Relaxed);
        }
    }

    // SAFETY: every method forwards verbatim to the wrapped allocator with the
    // caller's own layout and pointer. The counters are relaxed atomics and
    // change no allocation behaviour, so the safety contract is exactly the
    // wrapped allocator's, which is the one the shipped build uses.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { self.0.alloc(l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { self.0.alloc_zeroed(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { self.0.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            REALLOCS.fetch_add(1, Relaxed);
            if n > 96 && n <= 128 {
                REALLOC_128.fetch_add(1, Relaxed);
            }
            note(n.saturating_sub(l.size()));
            unsafe { self.0.realloc(p, l, n) }
        }
    }
}

#[cfg(all(feature = "global-alloc", feature = "profile"))]
#[global_allocator]
static ALLOC: counting_alloc::Counting = counting_alloc::Counting(rusty_alloc_api::RustyAlloc);

/// Arms the allocation-site sampler from `RS_H264_ALLOC_SITE` (a byte size).
/// No-op unless built with `profile`.
pub fn alloc_site_arm() {
    #[cfg(all(feature = "global-alloc", feature = "profile", feature = "std"))]
    counting_alloc::site::arm();
}

/// Allocation census for the run. No-op unless built with `profile`.
pub fn alloc_stats_report(frames: usize) {
    #[cfg(all(feature = "global-alloc", feature = "profile"))]
    {
        use core::sync::atomic::Ordering::Relaxed;
        let f = frames.max(1) as f64;
        std::eprintln!(
            "ALLOC calls={} bytes={} ({:.1}/frame, {:.0} B/frame)  BIG>=64K calls={} bytes={} ({:.2}/frame)",
            counting_alloc::CALLS.load(Relaxed),
            counting_alloc::BYTES.load(Relaxed),
            counting_alloc::CALLS.load(Relaxed) as f64 / f,
            counting_alloc::BYTES.load(Relaxed) as f64 / f,
            counting_alloc::BIG_CALLS.load(Relaxed),
            counting_alloc::BIG_BYTES.load(Relaxed),
            counting_alloc::BIG_CALLS.load(Relaxed) as f64 / f,
        );
        let names = ["<=32", "33-40", "41-48", "49-64", "65-80", "81-96", "97-128", "129-512", "513-64K", ">64K"];
        let mut s = std::string::String::new();
        for (i, n) in names.iter().enumerate() {
            let c = counting_alloc::BUCKETS[i].load(Relaxed);
            s.push_str(&std::format!("{n}={c} ({:.1}/frame)  ", c as f64 / f));
        }
        std::eprintln!("ALLOC sizes {s}");
        #[cfg(feature = "std")]
        if let Ok(g) = counting_alloc::site::CAUGHT.lock() {
            if let Some(bt) = g.as_ref() {
                std::eprintln!("ALLOC SITE sample:\n{bt}");
            }
        }
        std::eprintln!(
            "ALLOC reallocs={} of_which_97_128={}",
            counting_alloc::REALLOCS.load(Relaxed),
            counting_alloc::REALLOC_128.load(Relaxed),
        );
    }
    #[cfg(not(all(feature = "global-alloc", feature = "profile")))]
    let _ = frames;
}

/// Per-twin CALL census (scalar vs kernel share), re-exported so consumers can
/// read it without depending on the accel crate directly. Counters are live only
/// under the `census` feature; otherwise every method is a no-op.
#[cfg(accel)]
pub use rusty_h264_accel::census;

pub mod aligned;
pub mod arms;
pub mod bit_reader;
pub mod bit_writer;
pub mod cabac_tables;
pub mod cavlc;
pub mod deblock;

/// Whether the vendored SIMD kernels are compiled in (`asm` feature on x86-64).
/// Exposed so benchmarks can state which path they measured — a harness that
/// silently falls back to the scalar twin reports numbers that look like a
/// regression in the fast path.
pub const ACCEL: bool = cfg!(accel);
pub mod inter;
pub mod nal;
pub mod predict;
/// 64-bit atomics where the target has them, the portable fallback where not.
#[doc(hidden)]
pub mod atomic {
    #[cfg(target_has_atomic = "64")]
    pub use core::sync::atomic::AtomicU64;
    #[cfg(not(target_has_atomic = "64"))]
    pub use portable_atomic::AtomicU64;
}

/// `OnceLock` on both sides of the ladder: `std::sync::OnceLock` with `std`,
/// a `once_cell::race::OnceBox` (atomics + one heap box per cell) without.
/// Same `new` / `get_or_init` shape, so a lazily built table or a cached knob
/// in a `static` reads identically on the host and on a chip.
#[doc(hidden)]
pub mod once {
    #[cfg(feature = "std")]
    pub use std::sync::OnceLock;

    #[cfg(not(feature = "std"))]
    pub struct OnceLock<T>(once_cell::race::OnceBox<T>);

    #[cfg(not(feature = "std"))]
    impl<T> OnceLock<T> {
        /// An empty cell.
        pub const fn new() -> Self {
            OnceLock(once_cell::race::OnceBox::new())
        }
        /// The value, initialising it with `f` on first use.
        pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
            self.0.get_or_init(|| alloc::boxed::Box::new(f()))
        }
        /// The value if initialised.
        pub fn get(&self) -> Option<&T> {
            self.0.get()
        }
        /// Set the value if the cell is empty; otherwise hand it back.
        pub fn set(&self, v: T) -> Result<(), T> {
            self.0.set(alloc::boxed::Box::new(v)).map_err(|b| *b)
        }
    }

    #[cfg(not(feature = "std"))]
    impl<T> Default for OnceLock<T> {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod fmath;
pub mod prof;
pub mod transform;
pub mod types;

pub use bit_reader::BitReader;
pub use bit_writer::BitWriter;
pub use nal::{NalUnit, NalUnitType};
pub use types::{ChromaFormat, Profile, YuvFrame, YuvPlanes};
