//! Average-bitrate rate control.
//!
//! A frame-level controller that varies the per-frame quantization parameter to
//! converge the average bitrate on a target. It combines a **complexity model**
//! (predict a frame's bits at a candidate QP from recent history) with a
//! **leaky-bucket buffer** (correct accumulated over/undershoot), the standard
//! pairing in practical H.264 encoders. The decoder needs no cooperation: each
//! frame's QP rides in its `slice_qp_delta`, which conformant decoders honour.
//!
//! The model rests on the observation that, for a given frame, coded bits are
//! roughly inversely proportional to the quantizer step `Qstep`. So the product
//! `bits · Qstep` is a QP-independent *complexity*; we smooth it per frame type
//! (I vs P) and invert it to pick a QP for the next frame's bit budget.

/// H.264 quantizer step for a QP: `Qstep` doubles every 6 QP (spec §8.6.1).
fn qstep(qp: f64) -> f64 {
    0.625 * 2f64.powf(qp / 6.0)
}

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
    /// Smoothed scene complexity (`bits · Qstep`) learned from P-frames; the
    /// I-frame estimate is seeded from the first IDR. `0` = uninitialized.
    cplx_p: f64,
    cplx_i: f64,
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
            cplx_p: 0.0,
            cplx_i: 0.0,
            last_qp: qp as f64,
        }
    }

    /// Picks the QP for the next frame (`is_idr` selects the I model and a
    /// quality bump, since every later frame predicts from the IDR).
    pub fn pick_qp(&self, is_idr: bool) -> u8 {
        // Buffer-adjusted bit budget: spend less when the bucket is filling,
        // draining any accumulated deviation over roughly a buffer's worth.
        let deviation = self.fullness - self.buffer_size * 0.5;
        let frames_to_correct = (self.buffer_size / self.target_per_frame).max(4.0);
        let budget =
            (self.target_per_frame - deviation / frames_to_correct).max(self.target_per_frame * 0.2);

        let cplx = if is_idr { self.cplx_i } else { self.cplx_p };
        let qp = if cplx <= 0.0 {
            // No model yet: lean on the base QP, nudged by buffer state.
            self.base_qp + deviation / self.buffer_size * 8.0 - if is_idr { 2.0 } else { 0.0 }
        } else {
            // Invert the complexity model: Qstep = complexity / budget.
            4.0 + 6.0 * (cplx / budget).log2()
        };

        // Limit per-frame swing for stable quality, then clamp to the window.
        qp.clamp(self.last_qp - 4.0, self.last_qp + 4.0)
            .clamp(self.qp_min, self.qp_max)
            .round() as u8
    }

    /// Feeds back the bits a frame actually cost at its chosen QP.
    pub fn update(&mut self, is_idr: bool, bits: usize, qp: u8) {
        let complexity = bits as f64 * qstep(qp as f64);
        let ema = |old: f64, new: f64| if old <= 0.0 { new } else { 0.5 * old + 0.5 * new };
        if is_idr {
            self.cplx_i = ema(self.cplx_i, complexity);
            // Seed the P model from the IDR so frame 2 already has an estimate
            // (an I-frame costs ~4× a P-frame of the same scene).
            if self.cplx_p <= 0.0 {
                self.cplx_p = complexity / 4.0;
            }
        } else {
            self.cplx_p = ema(self.cplx_p, complexity);
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
        let first = rc.pick_qp(true);
        // Feed back frames far larger than the per-frame budget repeatedly.
        for _ in 0..30 {
            let qp = rc.pick_qp(false);
            rc.update(false, (rc.target_per_frame as usize) * 4, qp);
        }
        assert!(rc.pick_qp(false) > first, "QP should climb to curb overshoot");
    }

    #[test]
    fn lowers_qp_when_undershooting() {
        let mut rc = RateControl::new(1_000_000, 30.0, 40);
        let start = rc.pick_qp(false);
        for _ in 0..30 {
            let qp = rc.pick_qp(false);
            rc.update(false, (rc.target_per_frame as usize) / 8, qp);
        }
        assert!(rc.pick_qp(false) < start, "QP should fall to use the budget");
    }
}
