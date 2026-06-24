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
