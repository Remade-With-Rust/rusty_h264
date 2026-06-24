//! Average-bitrate rate control with a look-ahead complexity model.
//!
//! A frame-level controller that varies the per-frame quantization parameter to
//! converge the average bitrate on a target. It combines a **look-ahead
//! complexity estimate** (a cheap pre-encode score of *this* frame — see
//! [`crate::lookahead`]) with a **leaky-bucket buffer** (correct accumulated
//! over/undershoot). Using the upcoming frame's own complexity, rather than a
//! lagging average of past frames, spends bits where they are needed and keeps
//! quality steadier across complexity changes. The decoder needs no cooperation:
//! each frame's QP rides in its `slice_qp_delta`, which conformant decoders honour.
//!
//! The model rests on the observation that coded bits are roughly inversely
//! proportional to the quantizer step `Qstep`, and proportional to a frame's
//! complexity. So `k = bits · Qstep / complexity` is a slowly-varying constant we
//! learn (per frame type), then invert against the look-ahead complexity to pick
//! a QP for the frame's bit budget.

/// H.264 quantizer step for a QP: `Qstep` doubles every 6 QP (spec §8.6.1).
fn qstep(qp: f64) -> f64 {
    0.625 * 2f64.powf(qp / 6.0)
}

/// Quantizer-curve compression (x264's `qcomp`): 0 = constant bits per frame
/// (varying quality), 1 = constant quality (bits ∝ complexity). The default
/// blend spends ~`complexity^qcomp` bits, smoothing quality across complexity
/// changes without fully sacrificing rate efficiency.
const QCOMP: f64 = 0.6;

/// Frame-level average-bitrate controller.
#[derive(Debug, Clone)]
pub struct RateControl {
    /// Channel drain per coded frame, `bitrate / framerate` (bits).
    target_per_frame: f64,
    /// Leaky-bucket capacity (bits); ~one second of output.
    buffer_size: f64,
    /// Current buffer occupancy (bits); steered toward half-full.
    fullness: f64,
    /// Base/fallback QP (the configured `qp`), used before the model calibrates.
    base_qp: f64,
    qp_min: f64,
    qp_max: f64,
    /// Learned `bits · Qstep / complexity` per frame type (`0` = uninitialized).
    /// I- and P-frame complexity scores live in different domains (spatial vs
    /// motion-compensated SATD), so their constants are tracked separately.
    k_p: f64,
    k_i: f64,
    /// Smoothed look-ahead complexity per frame type, the reference point for
    /// the complexity-proportional bit allocation (`0` = uninitialized).
    avg_c_p: f64,
    avg_c_i: f64,
    /// Last QP actually used, to limit frame-to-frame swing.
    last_qp: f64,
}

impl RateControl {
    /// Builds a controller for `bitrate` bits/sec at `framerate` fps, with `qp`
    /// as the base/fallback quality. QP is clamped to a sane window around it.
    pub fn new(bitrate: u32, framerate: f32, qp: u8) -> Self {
        let bitrate = bitrate as f64;
        let framerate = (framerate as f64).max(1.0);
        let target_per_frame = bitrate / framerate;
        let buffer_size = bitrate.max(target_per_frame * 2.0); // ≥ ~1s, ≥ 2 frames
        RateControl {
            target_per_frame,
            buffer_size,
            fullness: buffer_size * 0.5,
            base_qp: qp as f64,
            qp_min: (qp as f64 - 18.0).max(10.0),
            qp_max: (qp as f64 + 18.0).min(51.0),
            k_p: 0.0,
            k_i: 0.0,
            avg_c_p: 0.0,
            avg_c_i: 0.0,
            last_qp: qp as f64,
        }
    }

    /// Picks the QP for a frame given its look-ahead `complexity` score (`is_idr`
    /// selects the I model and a quality bump, since every later frame predicts
    /// from the IDR).
    pub fn pick_qp(&self, is_idr: bool, complexity: f64) -> u8 {
        // Buffer-adjusted bit budget: spend less when the bucket is filling,
        // draining any accumulated deviation over roughly a buffer's worth.
        let deviation = self.fullness - self.buffer_size * 0.5;
        let frames_to_correct = (self.buffer_size / self.target_per_frame).max(4.0);
        let buf_target =
            (self.target_per_frame - deviation / frames_to_correct).max(self.target_per_frame * 0.2);

        // Complexity-proportional allocation: a frame `r×` the average complexity
        // gets `r^qcomp ×` the budget (clamped so one frame can't drain the
        // buffer). This is what holds quality steady across complexity changes.
        let avg = if is_idr { self.avg_c_i } else { self.avg_c_p };
        let budget = if avg > 0.0 {
            buf_target * (complexity / avg).clamp(0.25, 4.0).powf(QCOMP)
        } else {
            buf_target
        };

        let k = if is_idr { self.k_i } else { self.k_p };
        let qp = if k <= 0.0 {
            // Not calibrated yet: lean on the base QP, nudged by buffer state.
            self.base_qp + deviation / self.buffer_size * 8.0 - if is_idr { 2.0 } else { 0.0 }
        } else {
            // Predict this frame's bits·Qstep from its look-ahead complexity, then
            // invert against the budget: Qstep = (k · complexity) / budget.
            4.0 + 6.0 * (k * complexity / budget).log2()
        };

        // Limit per-frame swing for stable quality, then clamp to the window. A
        // wider swing than the reactive model: the look-ahead means the change is
        // driven by real complexity, not lag, so let it track more aggressively.
        qp.clamp(self.last_qp - 6.0, self.last_qp + 6.0)
            .clamp(self.qp_min, self.qp_max)
            .round() as u8
    }

    /// Feeds back the bits a frame actually cost at its chosen QP and complexity,
    /// recalibrating `k = bits · Qstep / complexity` and the average complexity.
    pub fn update(&mut self, is_idr: bool, bits: usize, qp: u8, complexity: f64) {
        let k_new = bits as f64 * qstep(qp as f64) / complexity.max(1.0);
        let ema = |old: f64, new: f64| if old <= 0.0 { new } else { 0.5 * old + 0.5 * new };
        if is_idr {
            self.k_i = ema(self.k_i, k_new);
            self.avg_c_i = ema(self.avg_c_i, complexity);
        } else {
            self.k_p = ema(self.k_p, k_new);
            self.avg_c_p = ema(self.avg_c_p, complexity);
        }

        // Leaky bucket: add what we coded, drain the channel allotment.
        self.fullness =
            (self.fullness + bits as f64 - self.target_per_frame).clamp(0.0, self.buffer_size);
        self.last_qp = qp as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qstep_doubles_every_six_qp() {
        assert!((qstep(28.0) / qstep(22.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn raises_qp_when_overshooting() {
        let mut rc = RateControl::new(1_000_000, 30.0, 26);
        let c = 1.0e6; // fixed look-ahead complexity
        let first = rc.pick_qp(true, c);
        // Feed back frames far larger than the per-frame budget repeatedly.
        for _ in 0..30 {
            let qp = rc.pick_qp(false, c);
            rc.update(false, (rc.target_per_frame as usize) * 4, qp, c);
        }
        assert!(rc.pick_qp(false, c) > first, "QP should climb to curb overshoot");
    }

    #[test]
    fn lowers_qp_when_undershooting() {
        let mut rc = RateControl::new(1_000_000, 30.0, 40);
        let c = 1.0e6;
        let start = rc.pick_qp(false, c);
        for _ in 0..30 {
            let qp = rc.pick_qp(false, c);
            rc.update(false, (rc.target_per_frame as usize) / 8, qp, c);
        }
        assert!(rc.pick_qp(false, c) < start, "QP should fall to use the budget");
    }

    #[test]
    fn complex_frame_not_given_more_quality_than_simple() {
        // Once calibrated, with qcomp < 1 a more complex frame is coded at no
        // higher quality (no lower QP) than a simpler one at the same budget.
        let mut rc = RateControl::new(2_000_000, 30.0, 26);
        for _ in 0..20 {
            let qp = rc.pick_qp(false, 1.0e6);
            rc.update(false, rc.target_per_frame as usize, qp, 1.0e6);
        }
        let simple = rc.pick_qp(false, 0.5e6);
        let complex = rc.pick_qp(false, 4.0e6);
        assert!(complex >= simple);
    }
}
