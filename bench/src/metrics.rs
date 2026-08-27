//! Objective quality + timing metrics.

use rusty_h264::YuvFrame;

/// Mean squared error over a pair of equal-length sample planes.
fn plane_mse(a: &[u8], b: &[u8]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    if a.is_empty() {
        return 0.0;
    }
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum();
    sum / a.len() as f64
}

/// PSNR in dB from an MSE; `None` (infinite) when the signals are identical.
fn psnr_from_mse(mse: f64) -> Option<f64> {
    if mse == 0.0 {
        None
    } else {
        Some(10.0 * (255.0f64 * 255.0 / mse).log10())
    }
}

/// Per-component and combined PSNR between a source and reconstructed frame.
#[derive(Clone, Copy)]
pub struct FramePsnr {
    /// `None` means lossless (infinite PSNR).
    pub y: Option<f64>,
    // Computed for completeness; the headline table reports Y-PSNR (standard
    // for video quality), so chroma is retained but not yet surfaced.
    #[allow(dead_code)]
    pub u: Option<f64>,
    #[allow(dead_code)]
    pub v: Option<f64>,
}

impl FramePsnr {
    pub fn compute(src: &YuvFrame, recon: &YuvFrame) -> Self {
        Self {
            y: psnr_from_mse(plane_mse(&src.y, &recon.y)),
            u: psnr_from_mse(plane_mse(&src.u, &recon.u)),
            v: psnr_from_mse(plane_mse(&src.v, &recon.v)),
        }
    }
}

/// Formats an optional-PSNR as dB or the lossless marker.
pub fn fmt_psnr(p: Option<f64>) -> String {
    match p {
        None => "∞ (lossless)".to_string(),
        Some(db) => format!("{db:.2} dB"),
    }
}

/// Average of a slice of `Option<f64>` PSNRs. `None` entries (lossless) are
/// treated as the perfect case and skipped; if every entry is lossless the
/// average is `None`.
pub fn avg_psnr(values: &[Option<f64>]) -> Option<f64> {
    let present: Vec<f64> = values.iter().filter_map(|v| *v).collect();
    if present.is_empty() {
        None
    } else {
        Some(present.iter().sum::<f64>() / present.len() as f64)
    }
}

// ---- The campaign-gating BD arithmetic — ONE home (plan addendum A6) ----
//
// Every function below existed as 2-3 divergent copies across the bench
// examples before consolidation. Where two POLICIES genuinely exist (the dB
// cap, the bin-bits clamp) BOTH are here, named, with the divergence pinned
// by test — a difference you can read beats one you discover mid-campaign.

/// SSIM → a dB-like scale so BD-rate integrates it like PSNR:
/// `−10·log10(1−SSIM)`, floored at 1e-9 (caps at 90 dB). The canonical form —
/// every Rust and Python BD tool uses this except the mb-tree GOP harvest.
pub fn ssim_db(s: f64) -> f64 {
    -10.0 * (1.0 - s).max(1e-9).log10()
}

/// The mb-tree GOP harvest's variant: clamps SSIM to 0.999_999 (caps at
/// 60 dB). Kept as ITS OWN name because the harvest's recorded CSVs were
/// produced with this cap — silently switching it to [`ssim_db`] would make
/// new harvests incomparable with banked ones on near-lossless GOPs.
pub fn ssim_db_capped60(s: f64) -> f64 {
    -10.0 * (1.0 - s.clamp(0.0, 0.999_999)).log10()
}

/// Estimated bits to code `bin` under P(bin=0)=`p0`: `−log2(p)`. UNCLAMPED —
/// a probability of 0 reads as +inf bits, which the CASC sims that use this
/// rely on to disqualify an arm rather than mask it.
pub fn bin_bits(p0: f64, bin: u8) -> f64 {
    let p = if bin == 0 { p0 } else { 1.0 - p0 };
    -p.log2()
}

/// The clamped variant (`p` held inside [1e-6, 1−1e-6], ≤ ~20 bits): for
/// ceiling estimates that must stay FINITE under a degenerate estimator.
/// Same name collision as the old per-example copies — now a visible choice.
pub fn bin_bits_clamped(p0: f64, bin: u8) -> f64 {
    let p = if bin == 0 { p0 } else { 1.0 - p0 };
    -p.clamp(1e-6, 1.0 - 1e-6).log2()
}

/// Least-squares degree-3 polyfit of y=f(x) via normal equations (4x4 solve).
/// (Moved VERBATIM from the bdrate example.)
pub fn polyfit3(x: &[f64], y: &[f64]) -> [f64; 4] {
    // A[j][k] = Σ x^(j+k), b[j] = Σ y·x^j, for j,k in 0..4.
    let mut a = [[0f64; 4]; 4];
    let mut b = [0f64; 4];
    for i in 0..x.len() {
        let mut xp = [0f64; 7];
        xp[0] = 1.0;
        for p in 1..7 {
            xp[p] = xp[p - 1] * x[i];
        }
        for j in 0..4 {
            for k in 0..4 {
                a[j][k] += xp[j + k];
            }
            b[j] += y[i] * xp[j];
        }
    }
    // Gaussian elimination (partial pivot).
    for c in 0..4 {
        let mut piv = c;
        for r in c + 1..4 {
            if a[r][c].abs() > a[piv][c].abs() {
                piv = r;
            }
        }
        a.swap(c, piv);
        b.swap(c, piv);
        for r in 0..4 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..4 {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    // coeffs c0 + c1 x + c2 x^2 + c3 x^3
    [b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]]
}

/// Bjøntegaard-Delta rate (%) of `test` vs `anchor`. Each is (rate, quality)
/// points. Fits log10(rate) = cubic(quality), integrates over the overlapping
/// quality range. Negative = `test` needs fewer bits at equal quality.
/// (Moved VERBATIM from the bdrate example.)
pub fn bd_rate(anchor: &[(f64, f64)], test: &[(f64, f64)]) -> f64 {
    let prep = |p: &[(f64, f64)]| -> (Vec<f64>, Vec<f64>) {
        let mut v: Vec<(f64, f64)> = p.iter().map(|&(r, d)| (d, r.log10())).collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        (v.iter().map(|q| q.0).collect(), v.iter().map(|q| q.1).collect())
    };
    let (da, la) = prep(anchor);
    let (dt, lt) = prep(test);
    let ca = polyfit3(&da, &la);
    let ct = polyfit3(&dt, &lt);
    let lo = da[0].max(dt[0]);
    let hi = da[da.len() - 1].min(dt[dt.len() - 1]);
    let integ = |c: &[f64; 4], x: f64| c[0] * x + c[1] * x * x / 2.0 + c[2] * x.powi(3) / 3.0 + c[3] * x.powi(4) / 4.0;
    let int_a = integ(&ca, hi) - integ(&ca, lo);
    let int_t = integ(&ct, hi) - integ(&ct, lo);
    let avg = (int_t - int_a) / (hi - lo);
    (10f64.powf(avg) - 1.0) * 100.0
}

#[cfg(test)]
mod bd_tests {
    use super::*;

    /// The canonical forms must equal the expressions the tools inlined —
    /// pinned so a future "cleanup" of this module fails here, not in a
    /// campaign verdict.
    #[test]
    fn forms_match_the_original_expressions() {
        let mut s = 0.0f64;
        while s <= 1.0 {
            assert_eq!(
                ssim_db(s).to_bits(),
                (-10.0 * (1.0 - s).max(1e-9).log10()).to_bits()
            );
            assert_eq!(
                ssim_db_capped60(s).to_bits(),
                (-10.0 * (1.0 - s.clamp(0.0, 0.999_999)).log10()).to_bits()
            );
            s += 1.0 / 4096.0;
        }
        for i in 1..1000u32 {
            let p0 = i as f64 / 1000.0;
            for bin in [0u8, 1] {
                let p = if bin == 0 { p0 } else { 1.0 - p0 };
                assert_eq!(bin_bits(p0, bin).to_bits(), (-p.log2()).to_bits());
                assert_eq!(
                    bin_bits_clamped(p0, bin).to_bits(),
                    (-p.clamp(1e-6, 1.0 - 1e-6).log2()).to_bits()
                );
            }
        }
    }

    /// The two dB caps and the two clamp policies genuinely DIFFER — pinned
    /// (the anti-aliasing pattern) so nobody "unifies" them and silently
    /// changes a harvest or a ceiling.
    #[test]
    fn the_variants_are_not_aliases() {
        assert!(ssim_db(0.9999999) > 60.0);
        assert!((ssim_db_capped60(0.9999999) - 60.0).abs() < 1e-5);
        assert!(bin_bits(1e-9, 0) > 25.0);
        assert!((bin_bits_clamped(1e-9, 0) - (-(1e-6f64).log2())).abs() < 1e-12);
    }

    /// `bd_rate` on a known synthetic pair: a test curve at exactly half the
    /// anchor's rate at every quality must read −50%.
    #[test]
    fn bd_rate_halved_rate_reads_minus_50() {
        let anchor: Vec<(f64, f64)> =
            (0..4).map(|i| (1000.0 * 2f64.powi(i), 30.0 + 2.0 * i as f64)).collect();
        let test: Vec<(f64, f64)> = anchor.iter().map(|&(r, d)| (r / 2.0, d)).collect();
        let bd = bd_rate(&anchor, &test);
        assert!((bd - -50.0).abs() < 1e-6, "bd={bd}");
        assert!(bd_rate(&anchor, &anchor).abs() < 1e-9);
    }
}
