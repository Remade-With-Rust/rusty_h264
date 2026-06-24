//! Deterministic synthetic test clip generation.
//!
//! Determinism is the whole point of the harness: the same clip every run, on
//! every machine, so output sizes and PSNR are exactly reproducible and the
//! only noisy metric is wall-clock time. No RNG, no I/O — a closed-form moving
//! pattern with both smooth gradients (favourable to a real transform codec)
//! and sharp edges (stress for prediction).

use rusty_h264::YuvFrame;

/// Parameters of a synthetic clip.
#[derive(Clone, Copy)]
pub struct ClipSpec {
    pub width: usize,
    pub height: usize,
    pub frames: usize,
}

impl ClipSpec {
    pub fn new(width: usize, height: usize, frames: usize) -> Self {
        Self {
            width,
            height,
            frames,
        }
    }

    /// I420 bytes for one frame.
    pub fn frame_bytes(&self) -> usize {
        self.width * self.height * 3 / 2
    }
}

/// Generates frame `t` of the clip: a diagonal gradient plus a moving bright
/// box (a hard edge that translates frame-to-frame, exercising motion).
pub fn frame(spec: &ClipSpec, t: usize) -> YuvFrame {
    let (w, h) = (spec.width, spec.height);
    let (cw, ch) = (w / 2, h / 2);
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];

    // Moving box position (wraps around).
    let bx = (t * 4) % w;
    let by = (t * 2) % h;
    let bw = w / 4;
    let bh = h / 4;

    for j in 0..h {
        for i in 0..w {
            let grad = ((i + j + t * 3) & 0xff) as u8;
            let in_box = i >= bx && i < bx + bw && j >= by && j < by + bh;
            y[j * w + i] = if in_box { 235 } else { grad };
        }
    }
    for j in 0..ch {
        for i in 0..cw {
            u[j * cw + i] = (128 + (((i + t) & 0x1f) as i32 - 16)) as u8;
            v[j * cw + i] = (128 + (((j + t) & 0x1f) as i32 - 16)) as u8;
        }
    }
    YuvFrame {
        width: w,
        height: h,
        y,
        u,
        v,
    }
}

/// Materializes the whole clip.
pub fn all_frames(spec: &ClipSpec) -> Vec<YuvFrame> {
    (0..spec.frames).map(|t| frame(spec, t)).collect()
}
