//! The encoder's ONE fastmath home (fast-transcendentals plan, addendum A4).
//!
//! Consolidation law: a workspace once held three independent range-reduced
//! `exp` implementations, differing in exactly the line that decides whether
//! the win happens. This module exists so that never happens here — every
//! fast-transcendental kernel and every transcendental-derived table lives in
//! THIS file, beside its oracle tests. Grep result 2026-08-26: this repo had
//! NO prior fastmath kernel anywhere, so these are the first.
//!
//! Two tiers, and the line between them is the whole design:
//!
//! * **EXACT (wired):** [`lambda_qp`] and [`qstep_qp`] — `OnceLock` tables
//!   built by the SAME expression the call sites used, evaluated once per
//!   process instead of per block/MB/slice/frame. Bit-identical by
//!   construction (the `build_mv_cost` pattern), gated by encoder
//!   byte-identity.
//! * **APPROXIMATE (wired behind [`polytier_on`], Round 10):** [`log2_poly`]
//!   and [`round_ties_even_fast`] — the ★★ tier's kernels at sites 5–8 (the
//!   AQ and mb-tree per-MB loops). In PRINCIPLE these change encoder
//!   decisions; in MEASURED fact the downstream decisions are integer with
//!   fat margins, exact-.5 round ties are measure-zero on
//!   transcendental-derived values, and the Round-10 gate (per-clip 4-QP
//!   two-arm bitstream hashing + both-arms goldens) is what establishes
//!   whether output moves at all. `RFF_POLYTIER=0` is the libm bisection
//!   anchor either way.

use std::sync::OnceLock;

/// The poly-tier switch (Round 10, the plan's BD round): `RFF_POLYTIER=0`
/// restores libm `log2`/`round` bit-exactly — the bisection anchor. Read per
/// call and deliberately NOT `OnceLock`-cached, so one process can run both
/// arms back-to-back (the `polytier_gate` harness and the goldens do). The
/// call sites are per-frame/per-GOP setup, never per-MB, so the env read
/// costs nothing that matters.
pub(crate) fn polytier_on() -> bool {
    #[cfg(test)]
    if let Some(v) = TEST_POLYTIER.with(|c| c.get()) {
        return v;
    }
    std::env::var_os("RFF_POLYTIER")
        .map(|v| v != "0")
        .unwrap_or(true)
}

#[cfg(test)]
thread_local! {
    /// Test-only arm pin. Tests run THREADED in one process, so `set_var`
    /// would race between a golden pinning libm and a test exercising poly;
    /// a thread-local override cannot.
    pub(crate) static TEST_POLYTIER: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// `2^((qp − 12) / 3)` for every `u8` QP — THE lambda exponential, shared by
/// nine call sites (rdoq per 4x4 block, plan_mb / plan_inter_mb / cabac-P
/// per MB, four slice coders per slice). 256 entries so every possible input
/// is table-exact and no range fallback exists to get wrong; built ONCE per
/// process from the identical expression the sites inlined.
pub(crate) fn lambda_qp(qp: u8) -> f64 {
    static TAB: OnceLock<[f64; 256]> = OnceLock::new();
    TAB.get_or_init(|| std::array::from_fn(|q| 2f64.powf((q as f64 - 12.0) / 3.0)))[qp as usize]
}

/// `0.625 · 2^(qp / 6)` — the H.264 quantizer step (spec §8.6.1), the rate
/// controller's model input. Same table discipline as [`lambda_qp`].
pub(crate) fn qstep_qp(qp: u8) -> f64 {
    static TAB: OnceLock<[f64; 256]> = OnceLock::new();
    TAB.get_or_init(|| std::array::from_fn(|q| 0.625 * 2f64.powf(q as f64 / 6.0)))[qp as usize]
}

/// Range-reduced polynomial `log2` for finite positive normal `x` — the ★★
/// kernel for sites 5/6 (`log2(var+1)`, `log2(total/intra)`), live behind
/// [`polytier_on`]. Not bit-identical to libm (< 1e-11 worst error, oracle
/// below); the Round-10 corpus gate owns the decision-identity claim.
///
/// Shape: exponent from the bit pattern, mantissa normalized into
/// [1/√2·2, √2) by the sqrt-2 split (skill: "split at sqrt 2 so it converges
/// fast"), then the atanh series in `s = (m−1)/(m+1)`, `|s| ≤ 0.1716`.
/// Coefficients are the series' own rationals — derived, not transcribed.
/// Exact at every power of two BY CONSTRUCTION (`m == 1 ⇒ s == 0`), which is
/// the landmark the oracle asserts.
pub(crate) fn log2_poly(x: f64) -> f64 {
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    // ln(m) = 2s·(1 + s²/3 + s⁴/5 + …); log2(m) = ln(m)/ln2.
    const TWO_OVER_LN2: f64 = 2.0 / std::f64::consts::LN_2;
    let p = 1.0
        + s2 * (1.0 / 3.0
            + s2 * (1.0 / 5.0
                + s2 * (1.0 / 7.0 + s2 * (1.0 / 9.0 + s2 * (1.0 / 11.0 + s2 * (1.0 / 13.0))))));
    e as f64 + TWO_OVER_LN2 * s * p
}

/// 1.5 · 2^52 — the f64 magic number: adding it forces any `|x| < 2^51` into
/// the mantissa's last bit at integer granularity, so `(x + M) − M` rounds to
/// nearest-EVEN with no call and no branch. The value is pinned against its
/// derivation in the oracle (a trimmed digit silently selects a different
/// constant — the skill's transcription trap).
const MAGIC_F64: f64 = 6_755_399_441_055_744.0;

/// Branch-free, call-free round-to-nearest-EVEN for `|x| < 2^51` — the ★★
/// kernel for sites 7/8, live behind [`polytier_on`]. Rust's `.round()` is
/// ties-AWAY, this is ties-EVEN — they differ ONLY on exact .5 ties, which
/// the plan's footnote feared and Round 10's corpus gate measures (on
/// transcendental-derived arguments an exact tie is measure-zero; the hash
/// gate is the proof, not the argument). Two documented deviations from
/// `f64::round_ties_even`: a negative input rounding to zero yields +0.0
/// (not −0.0) — immaterial to sites 7/8, which cast to i32 — and inputs at
/// or beyond 2^51 are OUT OF DOMAIN.
pub(crate) fn round_ties_even_fast(x: f64) -> f64 {
    (x + MAGIC_F64) - MAGIC_F64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wired tables must be bit-identical to the expressions their call
    /// sites used — same libm, same input, cached.
    #[test]
    fn tables_match_the_site_expressions() {
        for q in 0..=255u8 {
            assert_eq!(
                lambda_qp(q).to_bits(),
                (2f64.powf((q as f64 - 12.0) / 3.0)).to_bits(),
                "lambda_qp({q})"
            );
            assert_eq!(
                qstep_qp(q).to_bits(),
                (0.625 * 2f64.powf(q as f64 / 6.0)).to_bits(),
                "qstep_qp({q})"
            );
        }
        // The spec landmark the rc test relied on: Qstep doubles every 6 QP.
        assert!((qstep_qp(28) / qstep_qp(22) - 2.0).abs() < 1e-9);
    }

    /// A3 gate, deterministic half: dense sweep vs libm with the
    /// relative-OR-absolute metric, landmarks exact, monotone.
    #[test]
    fn log2_poly_oracle() {
        // Landmarks: exact at powers of two, by construction.
        for k in -40..=40i32 {
            let x = (2f64).powi(k);
            assert_eq!(log2_poly(x).to_bits(), (k as f64).to_bits(), "2^{k}");
        }
        // Dense sweep over the sites' domains: log2(v+1) for integer v
        // (site 5, v up to ~2^24) and ratios ≥ 1 (site 6).
        let mut worst = 0.0f64;
        let mut prev = f64::NEG_INFINITY;
        let mut v = 1u64;
        while v <= (1 << 24) + 3 {
            let x = (v + 1) as f64;
            let (a, b) = (log2_poly(x), x.log2());
            let abs = (a - b).abs();
            let err = if b.abs() > f64::MIN_POSITIVE {
                (abs / b.abs()).min(abs)
            } else {
                abs
            };
            worst = worst.max(err);
            assert!(a >= prev, "not monotone at v={v}");
            prev = a;
            v = v + 1 + v / 512; // dense low, log-spaced high
        }
        let mut st = 0x1234_5678u64;
        for _ in 0..20_000 {
            st = st
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let r = 1.0 + (st >> 11) as f64 / (1u64 << 53) as f64 * 4094.0; // [1, 4095)
            let (a, b) = (log2_poly(r), r.log2());
            let abs = (a - b).abs();
            let err = if b.abs() > f64::MIN_POSITIVE {
                (abs / b.abs()).min(abs)
            } else {
                abs
            };
            worst = worst.max(err);
        }
        assert!(
            worst < 1e-11,
            "worst log2_poly error {worst:e} exceeds 1e-11"
        );
    }

    /// The magic number equals its derivation, and the kernel is
    /// VALUE-identical to `f64::round_ties_even` across the domain —
    /// including exact .5 ties — with the sign-of-zero deviation pinned.
    #[test]
    fn round_ties_even_fast_oracle() {
        assert_eq!(MAGIC_F64.to_bits(), (1.5 * (2f64).powi(52)).to_bits());
        let mut st = 0x9e37_79b9u64;
        for i in 0..200_000u64 {
            let x = if i % 4 == 0 {
                // Exact ties: k + 0.5 — where ties-even and ties-away differ.
                (i as f64 / 4.0).copysign(if i % 8 == 0 { 1.0 } else { -1.0 }) + 0.5
            } else {
                st = st
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let m = (st >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
                (m - 0.5) * (2f64).powi((i % 50) as i32) // ± up to 2^49
            };
            let (a, b) = (round_ties_even_fast(x), x.round_ties_even());
            assert!(a == b, "x={x:e}: fast {a:e} vs libm {b:e}");
        }
        // The documented sign-of-zero deviation, pinned so it stays documented.
        assert_eq!(round_ties_even_fast(-0.3).to_bits(), 0f64.to_bits());
        assert_eq!((-0.3f64).round_ties_even().to_bits(), (-0.0f64).to_bits());
        // Anti-aliasing pin (skill §4): this is NOT `.round()` — a future
        // refactor that aliases them must fail here, not in a BD run.
        assert_eq!(round_ties_even_fast(2.5), 2.0);
        assert_eq!((2.5f64).round(), 3.0);
        assert_eq!(round_ties_even_fast(0.5), 0.0);
        assert_eq!((0.5f64).round(), 1.0);
    }
}
