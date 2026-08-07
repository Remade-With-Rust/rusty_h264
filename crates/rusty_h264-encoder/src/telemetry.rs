//! CABAC entropy-bin telemetry for the private Prometheus refinery — the CASC
//! (context-adaptive symbolic coding) harvest tap. See
//! `remade_ffmpeg_rs/_greatgate/great-gate.md` §4 "Symbolic leaves" for the
//! binding deployment rules and `Prometheus/docs/casc-bridge.md` for the
//! pipeline this feeds.
//!
//! Records every **context-coded bin** (`encode_decision`) on the real
//! slice-encode path: the context index, the model state *before* the bin
//! (state, MPS — i.e. exactly what the shipping coder knew), and the coded
//! bin. Bypass bins are not recorded (p = ½ by construction, no law to
//! discover). The tap observes and never steers, so the bitstream is
//! byte-identical with the feature on or off.
//!
//! Slices are recorded as segments: [`CabacEncoder::new`] opens a segment
//! tagged (qp, init_idc, is_i) — the exact inputs of the context-init tables
//! the CASC campaign wants to beat. Drivers drain [`take`] after each
//! `Encoder::encode(frame)` call; records within a segment are in exact emit
//! order (the stream-order the replay scorer requires).

use std::cell::RefCell;

/// One context-coded bin, as the coder saw it at emit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CabacBin {
    /// Context model index (0..460).
    pub ctx_idx: u16,
    /// Model state BEFORE this bin (0..=62).
    pub state: u8,
    /// Most-probable symbol BEFORE this bin (0/1).
    pub mps: u8,
    /// The coded bin.
    pub bin: u8,
}

/// One slice's worth of bins, with the init inputs that seeded its contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceTap {
    /// Slice QP (the context-init tables are linear in this).
    pub qp: i32,
    /// `cabac_init_idc` used (0..=2; I-slices use their own fixed table).
    pub init_idc: u32,
    /// I-slice?
    pub is_i: bool,
    /// The context-coded bins, in exact emit order.
    pub bins: Vec<CabacBin>,
}

/// `P(bin == 0)` on the u8 grid (`1..=255` of 256) for a context in
/// `(state, mps)` — derived from the spec's own `rangeTabLPS` (mean of the
/// four range-quadrant ratios), so the estimate is the table the coder
/// actually ships, not an α-power idealization. Use this to fill the
/// prom-entropy `p_zero` column.
pub fn p_zero_q8(state: u8, mps: u8) -> u8 {
    use rusty_h264_common::cabac_tables::RANGE_LPS;
    // Quadrant midpoints of the renormalized range (256..511 in 4 bands).
    const MID: [f64; 4] = [288.0, 352.0, 416.0, 480.0];
    let s = state.min(62) as usize;
    let p_lps: f64 = (0..4)
        .map(|q| RANGE_LPS[s][q] as f64 / MID[q])
        .sum::<f64>()
        / 4.0;
    let p_zero = if mps == 0 { 1.0 - p_lps } else { p_lps };
    (p_zero * 256.0).round().clamp(1.0, 255.0) as u8
}

struct TapState {
    enabled: bool,
    slices: Vec<SliceTap>,
}

thread_local! {
    static TAP: RefCell<TapState> = const {
        RefCell::new(TapState {
            enabled: false,
            slices: Vec::new(),
        })
    };
}

/// Turn recording on/off for this thread. Off also clears the buffer.
pub fn enable(on: bool) {
    TAP.with(|t| {
        let mut t = t.borrow_mut();
        t.enabled = on;
        if !on {
            t.slices = Vec::new();
        }
    });
}

/// Drain every slice recorded on this thread since the last `take`.
pub fn take() -> Vec<SliceTap> {
    TAP.with(|t| std::mem::take(&mut t.borrow_mut().slices))
}

/// Open a new slice segment. Called by `CabacEncoder::new`.
#[inline]
pub(crate) fn begin_slice(qp: i32, init_idc: u32, is_i: bool) {
    TAP.with(|t| {
        let mut t = t.borrow_mut();
        if t.enabled {
            t.slices.push(SliceTap {
                qp,
                init_idc,
                is_i,
                bins: Vec::new(),
            });
        }
    });
}

/// Record one context-coded bin under the current slice. Called ONLY from
/// `CabacEncoder::encode_decision` — the real emit path.
#[inline]
pub(crate) fn record(ctx_idx: u16, state: u8, mps: u8, bin: u8) {
    TAP.with(|t| {
        let mut t = t.borrow_mut();
        if t.enabled {
            if let Some(s) = t.slices.last_mut() {
                s.bins.push(CabacBin {
                    ctx_idx,
                    state,
                    mps,
                    bin,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_records() {
        enable(true);
        begin_slice(26, 0, true);
        record(105, 0, 0, 1);
        record(105, 0, 1, 0);
        begin_slice(28, 1, false);
        record(11, 5, 1, 1);
        let s = take();
        enable(false);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].bins.len(), 2);
        assert_eq!((s[0].qp, s[0].init_idc, s[0].is_i), (26, 0, true));
        assert_eq!(s[1].bins[0].ctx_idx, 11);
    }

    #[test]
    fn disabled_records_nothing() {
        enable(false);
        begin_slice(26, 0, true);
        record(1, 0, 0, 0);
        assert!(take().is_empty());
    }

    #[test]
    fn p_zero_is_sane() {
        // Fresh context (state 0) is near-equiprobable.
        let p = p_zero_q8(0, 0);
        assert!((120..=136).contains(&p), "state0 ≈ ½, got {p}");
        // Deep MPS=1 state: bin 0 is the LPS — small probability.
        let p = p_zero_q8(62, 1);
        assert!(p <= 8, "deep state LPS should be tiny, got {p}");
        // Symmetry: flipping MPS mirrors the probability.
        assert_eq!(
            p_zero_q8(30, 0) as u32 + p_zero_q8(30, 1) as u32,
            256,
            "MPS flip must mirror on the grid"
        );
    }
}
