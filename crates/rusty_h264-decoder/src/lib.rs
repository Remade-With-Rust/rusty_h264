//! Pure-Rust H.264 decoder — Constrained Baseline **+ B-slices + most of High
//! profile**, CAVLC and CABAC.
//!
//! Validated **bit-exact against Cisco's `h264dec`** on 35 of 35 clean streams
//! from openh264's conformance corpus; the CABAC paths were brought up
//! symbol-by-symbol against an instrumented openh264 oracle and are gated
//! **pixel-exact vs ffmpeg**. The reconstruction path is shared with the encoder
//! (via `rusty_h264-common`), so the two halves agree bit-for-bit by
//! construction.
//!
//! Decodes: full intra (`I_16x16`/`I_4x4`/`I_8x8`/`I_PCM`), inter
//! (`P_Skip`/16×16/16×8/8×16/`P_8x8`) with quarter-pel motion compensation,
//! B-slices (temporal + spatial direct, implicit + explicit weighted
//! prediction), the 8×8 transform and 8×8 intra prediction, scaling matrices,
//! in-loop deblocking, and a multi-reference DPB with POC reordering and MMCO.
//! CABAC covers I, P and B slices incl. High-profile 8×8 residual (not yet: `I_PCM`).
//!
//! This crate is `#![forbid(unsafe_code)]` and is **fuzzed to never panic or
//! hang** on malformed input — errors surface as [`DecodeError`].
//!
//! [`Decoder::decode_stream`] is the one-call entry point (frames in display
//! order); [`Decoder::decode`] is the streaming form (one picture per access
//! unit, in decode order — pair it with [`Decoder::last_poc`]).

mod cabac;
/// Profile-only re-export of the CABAC bin census for benchmarking harnesses.
#[cfg(feature = "profile")]
pub use cabac::bin_census;
mod frame_mt;
mod mb16;
mod params;

pub use params::{Pps, Sps};
pub use mb16::{MvField, MV_DUMP};

/// Print the E2 worker-seam counters (D7) if `RS_H264_EDC_STATS` is set.
pub fn edc_stats_report() {
    mb16::edcstat::report();
}

/// Test-only re-export of the CABAC arithmetic *decoder* so the encoder crate can
/// round-trip-validate its CABAC *encoder* against the exact reference engine.
#[doc(hidden)]
/// MEASUREMENT KNOB — `RFF_ABL_DEBLOCK=1` skips the loop filter so it can be
/// priced by ablation on the UNINSTRUMENTED binary. Read once; inert when unset.
fn abl_deblock() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RFF_ABL_DEBLOCK").map_or(false, |v| v != "0"))
}

pub mod cabac_test {
    pub use crate::cabac::Cabac;
    pub use crate::mb16::b_inter_shape;
    pub use crate::mb16::parse_cbp_cabac;
    pub use crate::mb16::parse_mb_qp_delta_cabac;
    pub use crate::mb16::parse_mb_type_b;
    pub use crate::mb16::parse_ref_idx_cabac;
}

use mb16::{FrameDecoder, GridPool, WeightTable};
use rusty_h264_common::bit_reader::OutOfData;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::{BitReader, NalUnitType, YuvFrame};

/// Decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Bitstream ended unexpectedly.
    Truncated,
    /// A required parameter set was missing before a slice.
    MissingParameterSet,
    /// A coding tool outside the implemented subset appeared in the stream.
    Unsupported(&'static str),
}

impl From<OutOfData> for DecodeError {
    fn from(_: OutOfData) -> Self {
        DecodeError::Truncated
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("bitstream truncated"),
            DecodeError::MissingParameterSet => f.write_str("slice before SPS/PPS"),
            DecodeError::Unsupported(s) => write!(f, "unsupported coding tool: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A reference picture: deblocked reconstruction at coded resolution.
/// Stored now (4a); read by motion compensation in 4b.
/// Shared handle to a reference picture. The DPB and every per-slice reference
/// list hold `Arc`s: list construction used to DEEP-CLONE each entry's planes +
/// motion grids per slice (H-32 found ~600 KB+/slice of pure memcpy on B
/// streams); an `Arc` clone is a refcount bump reading the same bytes, so the
/// change is byte-identical by construction. `Arc::make_mut` covers the one
/// mutation site (MMCO long-term marking).
pub(crate) type Ref = std::sync::Arc<RefFrame>;

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    /// EDGE-PADDED planes (openh264 `ExpandPicture`): built ONCE per reference
    /// frame so motion compensation reads them in place — the per-MC-call
    /// clamped-tile extraction (~400 B copied per call, ~100 MB/clip on real
    /// streams) dies with this. Luma pad [`LPAD`], chroma pad [`CPAD`]; strides
    /// via [`RefFrame::lstride`]/[`RefFrame::cstride`].
    pub py: Vec<u8>,
    pub pu: Vec<u8>,
    pub pv: Vec<u8>,
    pub cw: usize,
    pub ch: usize,
    /// Frame-MT Phase B: luma rows whose reconstruction (+row deblock when
    /// enabled) is visible to other threads' MC. Phase A publishes `ch` (fully
    /// ready) when the picture commits. `0` means not yet usable.
    pub ready_rows: std::sync::atomic::AtomicUsize,
    /// Phase B concurrent planes. When set, producers publish filtered rows into
    /// these locks and consumers wait on [`Self::ready_rows`] then read here;
    /// [`Self::py`]/[`Self::pu`]/[`Self::pv`] stay unused until commit copies
    /// them out (serial / Phase A leave this `None` — zero lock tax on 1T).
    pub live: Option<std::sync::Arc<LivePlanes>>,
    /// After picture finalize: lock-free planes for steady-state DPB MC.
    /// Preferred over [`Self::live`] / empty [`Self::py`] once set.
    pub frozen: std::sync::OnceLock<FrozenPlanes>,
    /// `frame_num` of the picture, for PicNum-based reference-list reordering.
    pub frame_num: u32,
    /// `PicOrderCnt` of the picture, for B-slice reference-list ordering.
    pub poc: i32,
    /// Per-4×4-block List-0 motion field (motion vector + reference index, `-1`
    /// for intra), and the block-grid width. Read as the *co-located* picture's
    /// motion for B-slice direct prediction (`colZeroFlag`, temporal direct).
    pub mv: Vec<(i32, i32)>,
    pub ref_idx: Vec<i32>,
    /// Per-4×4-block **List-1** motion. Needed because the co-located motion
    /// derivation (spec §8.4.1.2.1) falls back to List-1 when the co-located block
    /// has no List-0 prediction (`predFlagL0Col == 0`). A co-located picture only
    /// contains L1-only blocks when it is itself a B picture — which is precisely
    /// what b-pyramid produces, so this stayed unexercised until B-references
    /// appeared.
    pub mv1: Vec<(i32, i32)>,
    pub ref_idx1: Vec<i32>,
    /// Per-4×4-block POC of the List-0 picture each block referenced (`i32::MIN`
    /// for intra). Used by temporal direct's `MapColToList0` (the co-located
    /// reference index alone is meaningless in the current list).
    pub ref_poc: Vec<i32>,
    pub w4: usize,
    /// Long-term reference state. Long-term refs sit after short-term ones in
    /// `RefPicList0` (ordered by `long_term_idx` ascending) and survive the
    /// sliding window until explicitly unmarked (spec §8.2.4).
    pub long_term: bool,
    pub long_term_idx: u32,
}

impl Clone for RefFrame {
    fn clone(&self) -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        let frozen = std::sync::OnceLock::new();
        if let Some(p) = self.frozen.get() {
            let _ = frozen.set(p.clone());
        }
        Self {
            py: self.py.clone(),
            pu: self.pu.clone(),
            pv: self.pv.clone(),
            cw: self.cw,
            ch: self.ch,
            ready_rows: std::sync::atomic::AtomicUsize::new(self.ready_rows.load(Relaxed)),
            live: self.live.clone(),
            frozen,
            frame_num: self.frame_num,
            poc: self.poc,
            mv: self.mv.clone(),
            ref_idx: self.ref_idx.clone(),
            mv1: self.mv1.clone(),
            ref_idx1: self.ref_idx1.clone(),
            ref_poc: self.ref_poc.clone(),
            w4: self.w4,
            long_term: self.long_term,
            long_term_idx: self.long_term_idx,
        }
    }
}

/// Luma / chroma pad of every [`RefFrame`] plane. Luma 16 serves MVs overshooting
/// the picture by up to ~14 px in place (chroma: half that, matching); wilder MVs
/// take `mc_*_padded`'s clamped-halo fallback — correct, just slower.
pub(crate) const LPAD: usize = 16;
pub(crate) const CPAD: usize = 8;

thread_local! {
    /// Per-thread MB-row MC watermark (`rows_needed_for_mb`). `usize::MAX` = unset.
    static MC_ROW_NEED: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// Lock-free padded planes after a Phase B progress slot is finalized.
#[derive(Debug, Clone)]
pub(crate) struct FrozenPlanes {
    pub py: Vec<u8>,
    pub pu: Vec<u8>,
    pub pv: Vec<u8>,
}

/// Concurrent padded planes + metadata for frame-MT Phase B (row-progress).
#[derive(Debug)]
pub(crate) struct LivePlanes {
    pub py: std::sync::RwLock<Vec<u8>>,
    pub pu: std::sync::RwLock<Vec<u8>>,
    pub pv: std::sync::RwLock<Vec<u8>>,
    /// Identity + coloc motion; written at submit (fn/poc) and finalize (mv).
    pub meta: std::sync::RwLock<LiveMeta>,
    /// Park consumers until [`RefFrame::ready_rows`] advances (no spin tax).
    pub wait: std::sync::Mutex<()>,
    pub cv: std::sync::Condvar,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveMeta {
    pub frame_num: u32,
    pub poc: i32,
    pub long_term: bool,
    pub long_term_idx: u32,
    pub mv: Vec<(i32, i32)>,
    pub ref_idx: Vec<i32>,
    pub mv1: Vec<(i32, i32)>,
    pub ref_idx1: Vec<i32>,
    pub ref_poc: Vec<i32>,
    pub w4: usize,
    /// True once finalize has published coloc motion (temporal direct may read).
    pub motion_ready: bool,
}

/// Guard over a luma/chroma plane (borrowed or Phase B live lock).
pub(crate) enum PlaneGuard<'a> {
    Borrowed(&'a [u8]),
    Locked(std::sync::RwLockReadGuard<'a, Vec<u8>>),
}

impl std::ops::Deref for PlaneGuard<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Borrowed(s) => s,
            Self::Locked(g) => g.as_slice(),
        }
    }
}

impl RefFrame {
    /// Conservative luma rows needed before MC of MB row `mb_y` (MV overshoot pad).
    #[inline]
    pub(crate) fn rows_needed_for_mb(mb_y: usize, ch: usize) -> usize {
        ((mb_y + 1) * 16 + LPAD).min(ch.max(1))
    }

    /// Publish the current MB-row MC watermark for this thread (parse or EDC worker).
    /// [`Self::luma_guard`] / chroma take `min(caller_need, hint)` so legacy
    /// `guard(ch)` call sites still early-start correctly under Phase B.
    #[inline]
    pub(crate) fn set_mc_row_need(mb_y: usize, ch: usize) {
        // 1T / Phase A: no in-flight watermarks. TLS write on every MC was a
        // no-op consumer of the hint (guards still waited on ready_rows).
        if !crate::frame_mt::row_progress_on() {
            return;
        }
        MC_ROW_NEED.with(|c| c.set(Self::rows_needed_for_mb(mb_y, ch)));
    }

    #[inline]
    fn effective_need(&self, need_rows: usize) -> usize {
        let hint = MC_ROW_NEED.with(|c| c.get());
        need_rows.min(hint).min(self.ch.max(1))
    }

    /// Luma plane for MC, waiting on Phase B row watermark when needed.
    /// Prefers frozen / plain planes (no lock); only in-flight slots take `live`.
    #[inline]
    pub(crate) fn luma_guard(&self, need_rows: usize) -> PlaneGuard<'_> {
        if self.live.is_none() {
            // 1T / Phase A: planes are complete. Skip TLS + ready_rows.
            if let Some(f) = self.frozen.get() {
                return PlaneGuard::Borrowed(&f.py);
            }
            return PlaneGuard::Borrowed(&self.py);
        }
        let need = self.effective_need(need_rows);
        self.wait_ready_rows(need);
        if let Some(f) = self.frozen.get() {
            return PlaneGuard::Borrowed(&f.py);
        }
        if !self.py.is_empty() {
            return PlaneGuard::Borrowed(&self.py);
        }
        if let Some(live) = &self.live {
            PlaneGuard::Locked(live.py.read().unwrap())
        } else {
            PlaneGuard::Borrowed(&self.py)
        }
    }

    #[inline]
    pub(crate) fn chroma_guard(&self, plane: usize, need_rows: usize) -> PlaneGuard<'_> {
        if self.live.is_none() {
            if let Some(f) = self.frozen.get() {
                return if plane == 0 {
                    PlaneGuard::Borrowed(&f.pu)
                } else {
                    PlaneGuard::Borrowed(&f.pv)
                };
            }
            return if plane == 0 {
                PlaneGuard::Borrowed(&self.pu)
            } else {
                PlaneGuard::Borrowed(&self.pv)
            };
        }
        let need = self.effective_need(need_rows);
        self.wait_ready_rows(need);
        if let Some(f) = self.frozen.get() {
            return if plane == 0 {
                PlaneGuard::Borrowed(&f.pu)
            } else {
                PlaneGuard::Borrowed(&f.pv)
            };
        }
        if !self.py.is_empty() {
            return if plane == 0 {
                PlaneGuard::Borrowed(&self.pu)
            } else {
                PlaneGuard::Borrowed(&self.pv)
            };
        }
        if let Some(live) = &self.live {
            if plane == 0 {
                PlaneGuard::Locked(live.pu.read().unwrap())
            } else {
                PlaneGuard::Locked(live.pv.read().unwrap())
            }
        } else if plane == 0 {
            PlaneGuard::Borrowed(&self.pu)
        } else {
            PlaneGuard::Borrowed(&self.pv)
        }
    }
    pub(crate) fn new_progress_slot(cw: usize, ch: usize, b_possible: bool, mb_w: usize) -> Ref {
        let (lpw, lph) = (cw + 2 * LPAD, ch + 2 * LPAD);
        let (cpw, cph) = (cw / 2 + 2 * CPAD, ch / 2 + 2 * CPAD);
        let w4 = if b_possible { mb_w * 4 } else { 0 };
        let n4 = if b_possible {
            mb_w * 4 * (ch / 16) * 4
        } else {
            0
        };
        // Fat live planes only when strip-publishing; otherwise meta+CV only
        // (MC parks until finalize freezes lock-free planes).
        let (py, pu, pv) = if crate::frame_mt::row_publish_on() {
            (
                vec![0; lpw * lph],
                vec![0; cpw * cph],
                vec![0; cpw * cph],
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        std::sync::Arc::new(RefFrame {
            py: Vec::new(),
            pu: Vec::new(),
            pv: Vec::new(),
            cw,
            ch,
            ready_rows: std::sync::atomic::AtomicUsize::new(0),
            live: Some(std::sync::Arc::new(LivePlanes {
                py: std::sync::RwLock::new(py),
                pu: std::sync::RwLock::new(pu),
                pv: std::sync::RwLock::new(pv),
                meta: std::sync::RwLock::new(LiveMeta {
                    mv: vec![(0, 0); n4],
                    ref_idx: vec![-1; n4],
                    mv1: vec![(0, 0); n4],
                    ref_idx1: vec![-1; n4],
                    ref_poc: vec![i32::MIN; n4],
                    w4,
                    ..LiveMeta::default()
                }),
                wait: std::sync::Mutex::new(()),
                cv: std::sync::Condvar::new(),
            })),
            frozen: std::sync::OnceLock::new(),
            frame_num: 0,
            poc: 0,
            mv: vec![(0, 0); n4],
            ref_idx: vec![-1; n4],
            mv1: vec![(0, 0); n4],
            ref_idx1: vec![-1; n4],
            ref_poc: vec![i32::MIN; n4],
            w4,
            long_term: false,
            long_term_idx: 0,
        })
    }

    /// Set identity while the progress Arc is still unique (submit thread).
    pub(crate) fn init_progress_identity(slot: &mut Ref, frame_num: u32, poc: i32) {
        if let Some(s) = std::sync::Arc::get_mut(slot) {
            s.frame_num = frame_num;
            s.poc = poc;
            if let Some(live) = &s.live {
                let mut m = live.meta.write().unwrap();
                m.frame_num = frame_num;
                m.poc = poc;
            }
        }
    }

    #[inline]
    pub(crate) fn fn_num(&self) -> u32 {
        if let Some(live) = &self.live {
            live.meta.read().unwrap().frame_num
        } else {
            self.frame_num
        }
    }

    #[inline]
    pub(crate) fn pic_poc(&self) -> i32 {
        if let Some(live) = &self.live {
            live.meta.read().unwrap().poc
        } else {
            self.poc
        }
    }

    #[inline]
    pub(crate) fn is_long_term(&self) -> bool {
        if let Some(live) = &self.live {
            live.meta.read().unwrap().long_term
        } else {
            self.long_term
        }
    }

    #[inline]
    pub(crate) fn lt_idx(&self) -> u32 {
        if let Some(live) = &self.live {
            live.meta.read().unwrap().long_term_idx
        } else {
            self.long_term_idx
        }
    }

    pub(crate) fn set_long_term_marks(&self, long_term: bool, idx: u32) {
        if let Some(live) = &self.live {
            let mut m = live.meta.write().unwrap();
            m.long_term = long_term;
            m.long_term_idx = idx;
        }
    }

    pub(crate) fn set_frame_num_live(&self, frame_num: u32) {
        if let Some(live) = &self.live {
            live.meta.write().unwrap().frame_num = frame_num;
        }
    }

    /// Wait until coloc motion is published (picture fully finalized).
    pub(crate) fn wait_motion_ready(&self) {
        if self.live.is_none() {
            return;
        }
        if let Some(live) = &self.live {
            if live.meta.read().unwrap().motion_ready {
                return;
            }
        }
        if let Some(live) = &self.live {
            let mut g = live.wait.lock().unwrap();
            while !live.meta.read().unwrap().motion_ready {
                g = live.cv.wait(g).unwrap();
            }
        }
    }

    /// Mark this reference fully ready (Phase A commit / serial path).
    #[inline]
    pub fn mark_fully_ready(&self) {
        if let Some(live) = &self.live {
            let _g = live.wait.lock().unwrap();
            self.ready_rows
                .store(self.ch, std::sync::atomic::Ordering::Release);
            live.cv.notify_all();
        } else {
            self.ready_rows
                .store(self.ch, std::sync::atomic::Ordering::Release);
        }
    }

    /// Frame-MT Phase B: publish that luma rows `[0, rows)` are MC-safe.
    #[inline]
    pub fn publish_ready_rows(&self, rows: usize) {
        let r = rows.min(self.ch);
        if let Some(live) = &self.live {
            let _g = live.wait.lock().unwrap();
            let prev = self
                .ready_rows
                .fetch_max(r, std::sync::atomic::Ordering::Release);
            if r > prev {
                live.cv.notify_all();
            }
        } else {
            let _ = self
                .ready_rows
                .fetch_max(r, std::sync::atomic::Ordering::Release);
        }
    }

    /// Block until at least `rows` luma rows are ready (Phase B). Phase A refs
    /// are published fully ready, so this returns immediately.
    #[inline]
    pub fn wait_ready_rows(&self, rows: usize) {
        use std::sync::atomic::Ordering::Acquire;
        let need = rows.min(self.ch);
        if need == 0 || self.ready_rows.load(Acquire) >= need {
            return;
        }
        if self.frozen.get().is_some() {
            return;
        }
        if let Some(live) = &self.live {
            let mut g = live.wait.lock().unwrap();
            while self.ready_rows.load(Acquire) < need {
                g = live.cv.wait(g).unwrap();
            }
        } else {
            while self.ready_rows.load(Acquire) < need {
                std::thread::yield_now();
            }
        }
    }

    #[inline]
    pub fn lstride(&self) -> usize {
        self.cw + 2 * LPAD
    }
    #[inline]
    pub fn cstride(&self) -> usize {
        self.cw / 2 + 2 * CPAD
    }
}

/// A memory-management control operation (`dec_ref_pic_marking`, spec §7.4.3.3).
#[derive(Clone, Copy)]
enum Mmco {
    /// 1: mark a short-term reference (by PicNum) as unused.
    Unref(u32),
    /// 2: mark a long-term reference (by LongTermPicNum) as unused.
    UnrefLong(u32),
    /// 3: assign a short-term reference (by PicNum) a LongTermFrameIdx.
    AssignLong(u32, u32),
    /// 4: drop long-term references with idx ≥ max_long_term_frame_idx_plus1.
    MaxLong(u32),
    /// 5: empty the DPB (and reset the current picture's frame_num to 0).
    Reset,
    /// 6: mark the current picture long-term with this LongTermFrameIdx.
    CurrentLong(u32),
}

/// A picture being assembled from one or more slices (spec allows a picture to
/// be split into multiple slices). Finalized — deblocked, output, and entered
/// into the DPB — once all its macroblocks are decoded.
struct PendingPic {
    fd: mb16::FrameDecoder,
    frame_num: u32,
    poc: i32,
    next_mb: usize,
    total_mb: usize,
    slice_count: u16,
    deblock: bool,
    filter_offset_a: i32,
    filter_offset_b: i32,
    crop_r: usize,
    crop_b: usize,
    max_refs: usize,
    log2_max_frame_num: u32,
    /// `false` for a non-reference picture (nal_ref_idc == 0): output it but do
    /// not enter it into the DPB.
    is_reference: bool,
    idr_long_term: bool,
    mmco_ops: Vec<Mmco>,
}

/// Measurement knob: disable grid pooling, restoring per-picture allocation.
fn no_pool() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        0 => {
            let v = std::env::var_os("RS_H264_NO_POOL").is_some_and(|v| v == "1");
            ON.store(if v { 1 } else { 2 }, Ordering::Relaxed);
            v
        }
        n => n == 1,
    }
}

/// A Constrained Baseline H.264 decoder. Holds the most recent parameter sets
/// and the previous decoded picture (the inter reference) across calls.
#[derive(Default)]
pub struct Decoder {
    /// Active parameter sets, keyed by id — a stream may carry several and switch
    /// between them per slice (spec §7.3.2.1/.2).
    pub(crate) sps: std::collections::HashMap<u32, Sps>,
    pub(crate) pps: std::collections::HashMap<u32, Pps>,
    /// Decoded-picture buffer (most-recent first); `ref_idx` indexes into this.
    pub(crate) refs: Vec<Ref>,
    /// The picture currently being assembled from its slices, if any.
    cur: Option<PendingPic>,
    /// Picture-order-count state (spec §8.2.1). Tracks the previous reference
    /// picture's MSB/LSB (type 0) and frame-num offset (types 1/2) so display
    /// order can be recovered — needed once B-pictures (out-of-order) land.
    pub(crate) poc: PocState,
    /// `PicOrderCnt` of the most recently returned picture (display-order key).
    pub(crate) last_poc: i32,
    /// `frame_num` of the previous short-term reference picture, for detecting
    /// gaps in `frame_num` (spec §8.2.5.2).
    pub(crate) prev_ref_frame_num: u32,
    /// Per-picture grid allocations, handed from the finished picture to the next
    /// one instead of being freed and re-allocated. See `mb16::GridPool`.
    grid_pool: GridPool,
    /// Recycled padded-plane buffers from evicted reference frames, drawn by
    /// `as_reference_pooled`. Bounded (see `reclaim_retired`).
    plane_pool: Vec<Vec<u8>>,
    /// Reference frames evicted from the DPB whose planes have not been
    /// reclaimed yet. Reclamation must wait until the evicting picture's
    /// `FrameDecoder` is consumed — while it lives it still holds `Arc` clones
    /// of its ref lists, so `Arc::try_unwrap` would fail at eviction time.
    retired: Vec<Ref>,
    /// Frame-MT: when set, finalize does not apply DPB marking; the new ref is
    /// stashed in [`Self::detached_ref`] for the scheduler to commit in order.
    pub(crate) detach_dpb: bool,
    /// Detached reference as `Arc` (Phase B progress slot or freshly wrapped).
    pub(crate) detached_ref: Option<Ref>,
    /// Phase B: pre-installed progress Arc filled during decode / finalize.
    pub(crate) progress_slot: Option<Ref>,
    detached_mmco: Vec<Mmco>,
    detached_frame_num: u32,
    detached_log2_max_frame_num: u32,
    detached_max_refs: usize,
    detached_idr_long_term: bool,
    /// Frame-MT Phase B: publish row watermarks while decoding.
    pub(crate) frame_mt_row_progress: bool,
}

/// Running picture-order-count derivation state.
#[derive(Clone, Default)]
pub(crate) struct PocState {
    prev_msb: i32,
    prev_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: i64,
}

impl PocState {
    /// Derives `PicOrderCnt` for the current picture (spec §8.2.1) and advances
    /// this state. Types 0 and 2 are exact; type 1 is approximated by frame-num
    /// order (no B-stream in scope uses it).
    pub(crate) fn compute_poc(
        &mut self,
        sps: &Sps,
        is_idr: bool,
        nal_ref_idc: u8,
        frame_num: u32,
        poc_lsb: u32,
        delta_bottom: i32,
    ) -> i32 {
        match sps.pic_order_cnt_type {
            0 => {
                let max_lsb = 1i32 << sps.log2_max_pic_order_cnt_lsb;
                let (prev_msb, prev_lsb) =
                    if is_idr { (0, 0) } else { (self.prev_msb, self.prev_lsb) };
                let lsb = poc_lsb as i32;
                let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                    prev_msb + max_lsb
                } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                    prev_msb - max_lsb
                } else {
                    prev_msb
                };
                let top = msb + lsb;
                let poc = top.min(top + delta_bottom);
                if nal_ref_idc != 0 {
                    self.prev_msb = msb;
                    self.prev_lsb = lsb;
                }
                poc
            }
            2 => {
                let max_fn = 1i64 << sps.log2_max_frame_num;
                let offset = if is_idr {
                    0
                } else if self.prev_frame_num > frame_num {
                    self.prev_frame_num_offset + max_fn
                } else {
                    self.prev_frame_num_offset
                };
                let poc = if is_idr {
                    0
                } else {
                    2 * (offset + frame_num as i64) - i64::from(nal_ref_idc == 0)
                };
                self.prev_frame_num_offset = offset;
                self.prev_frame_num = frame_num;
                poc as i32
            }
            _ => {
                self.prev_frame_num = frame_num;
                frame_num as i32 * 2
            }
        }
    }
}

impl Decoder {
    /// Creates a decoder with no parameter sets yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes a complete Annex-B access unit, returning the reconstructed,
    /// cropped frame if the access unit contained a coded picture.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<YuvFrame>, DecodeError> {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Total);
        let mut frame = None;
        // The Annex-B scan and the RBSP unescape are each a FULL byte-wise pass over
        // the stream, and neither was timed — they landed in the unnamed residue that
        // the anatomy measured at 23-30% of decode outside the MB bodies.
        let nals = {
            let _s = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecNalSplit);
            split_annex_b(annex_b)
        };
        for nal in nals {
            if nal.is_empty() {
                continue;
            }
            let nal_type = NalUnitType::from_id(nal[0]);
            let rbsp = {
                let _s = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecRbsp);
                emulation_unprevent(&nal[1..])
            };
            match nal_type {
                NalUnitType::Sps => {
                    let s = Sps::parse(&rbsp)?;
                    self.sps.insert(s.seq_parameter_set_id, s);
                }
                NalUnitType::Pps => {
                    let p = Pps::parse(&rbsp)?;
                    self.pps.insert(p.pic_parameter_set_id, p);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    let nal_ref_idc = (nal[0] >> 5) & 3;
                    let is_idr = nal_type == NalUnitType::IdrSlice;
                    if let Some(f) = self.decode_slice(&rbsp, is_idr, nal_ref_idc)? {
                        frame = Some(f);
                    }
                }
                _ => {} // SEI, AUD, etc. ignored
            }
        }
        Ok(frame)
    }

    /// Decodes a complete Annex-B byte stream and returns every picture in
    /// **display order** (`PicOrderCnt` within each GOP; an IDR ends a GOP).
    ///
    /// This is the convenient whole-stream entry point — it handles access-unit
    /// splitting, multi-slice picture assembly, and B-picture reordering — versus
    /// the lower-level per-access-unit [`Decoder::decode`], which returns pictures
    /// in decode order.
    ///
    /// When `RS_H264_FRAME_THREADS` is set to N>1 (or the caller uses
    /// [`Decoder::decode_stream_threaded`]), pictures decode on a worker pool
    /// under a full-reference barrier (campaign #1 Phase A). Measure with
    /// `bench/pinmt.ps1` (WALL+CPU, multi-core mask) — not the 1T CPU race.
    pub fn decode_stream(&mut self, annex_b: &[u8]) -> Result<Vec<YuvFrame>, DecodeError> {
        let n = frame_mt::frame_threads();
        if n > 1 {
            return frame_mt::decode_stream_threaded(annex_b, n);
        }
        self.decode_stream_serial(annex_b)
    }

    /// Force frame-MT with an explicit worker count (0/1 = serial).
    pub fn decode_stream_threaded(
        &mut self,
        annex_b: &[u8],
        threads: usize,
    ) -> Result<Vec<YuvFrame>, DecodeError> {
        if threads <= 1 {
            return self.decode_stream_serial(annex_b);
        }
        frame_mt::decode_stream_threaded(annex_b, threads)
    }

    /// Frame-MT decode that invokes `sink` for each display-ordered frame
    /// (avoids retaining all YUV — use from `decode_bench` timed path).
    pub fn decode_stream_threaded_sink(
        &mut self,
        annex_b: &[u8],
        threads: usize,
        sink: impl FnMut(YuvFrame),
    ) -> Result<usize, DecodeError> {
        if threads <= 1 {
            let frames = self.decode_stream_serial(annex_b)?;
            let n = frames.len();
            let mut sink = sink;
            for f in frames {
                sink(f);
            }
            return Ok(n);
        }
        frame_mt::decode_stream_threaded_sink(annex_b, threads, sink)
    }

    fn decode_stream_serial(&mut self, annex_b: &[u8]) -> Result<Vec<YuvFrame>, DecodeError> {
        let mut out = Vec::new();
        let mut gop: Vec<(i32, YuvFrame)> = Vec::new();
        for au in split_access_units(annex_b) {
            if au_is_idr(au) {
                flush_gop(&mut gop, &mut out); // emit the prior GOP before the IDR
            }
            if let Some(frame) = self.decode(au)? {
                gop.push((self.last_poc, frame));
            }
        }
        flush_gop(&mut gop, &mut out);
        Ok(out)
    }

    fn decode_slice(
        &mut self,
        rbsp: &[u8],
        is_idr: bool,
        nal_ref_idc: u8,
    ) -> Result<Option<YuvFrame>, DecodeError> {
        let mut r = BitReader::new(rbsp);
        // --- slice_header ---
        let first_mb_in_slice = r.read_ue()? as usize;
        let slice_type = r.read_ue()?;
        let is_p = matches!(slice_type, 0 | 5);
        let is_b = matches!(slice_type, 1 | 6);
        let is_i = matches!(slice_type, 2 | 7);
        if !is_p && !is_b && !is_i {
            return Err(DecodeError::Unsupported("SP/SI slices"));
        }
        // Resolve the parameter sets this slice references (by id).
        let pic_parameter_set_id = r.read_ue()?;
        let pps = self.pps.get(&pic_parameter_set_id).cloned().ok_or(DecodeError::MissingParameterSet)?;
        let sps = self.sps.get(&pps.seq_parameter_set_id).cloned().ok_or(DecodeError::MissingParameterSet)?;
        let sps = &sps;
        let pps = &pps;
        // CABAC (entropy_coding_mode_flag=1) has an entirely different slice-data parse
        // (docs/cabac-decode-plan.md). I-slice CABAC is being brought up; the CABAC MB
        // loop gates P/B until Phase 3. `cabac_init_idc` (P/B only) is read below.
        let cabac = pps.entropy_coding_mode_flag;
        let frame_num = r.read_bits(sps.log2_max_frame_num)?;
        if is_idr {
            let _idr_pic_id = r.read_ue()?;
        }
        // pic_order_cnt fields (spec §7.3.3). `field_pic_flag` is always 0
        // (frame_mbs_only). Captured to derive PicOrderCnt for display ordering.
        let mut poc_lsb = 0u32;
        let mut delta_poc_bottom = 0i32;
        if sps.pic_order_cnt_type == 0 {
            poc_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb)?;
            if pps.bottom_field_pic_order_present {
                delta_poc_bottom = r.read_se()?;
            }
        } else if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero {
            let _delta_pic_order_cnt_0 = r.read_se()?;
            if pps.bottom_field_pic_order_present {
                let _delta_pic_order_cnt_1 = r.read_se()?;
            }
        }
        // PicOrderCnt is determined by the first slice of the picture; later
        // slices share it (and must not re-advance the POC state).
        let pic_poc = if first_mb_in_slice == 0 {
            self.compute_poc(sps, is_idr, nal_ref_idc, frame_num, poc_lsb, delta_poc_bottom)
        } else {
            self.cur.as_ref().map_or(0, |p| p.poc)
        };
        // redundant_pic_cnt: a non-zero value marks a *redundant* coded picture
        // (an alternative representation of the primary picture). A primary
        // decoder discards it (spec §7.4.3, §8.2.5 note). Must be read here or the
        // rest of the slice header desyncs.
        if pps.redundant_pic_cnt_present_flag {
            let redundant_pic_cnt = r.read_ue()?;
            if redundant_pic_cnt != 0 {
                return Ok(None);
            }
        }
        if std::env::var_os("RH264_DUMP_MB").is_some() {
            eprintln!(
                "SLICE fn={frame_num} poc={pic_poc} nal_ref_idc={nal_ref_idc} is_p={is_p} is_b={is_b} first_mb={first_mb_in_slice}"
            );
        }
        // B slices choose direct-mode derivation here (spec §7.3.3).
        let direct_spatial = if is_b { r.read_bit()? } else { true };
        let mut num_ref_idx_l0 = pps.num_ref_idx_l0_default as usize;
        let mut num_ref_idx_l1 = pps.num_ref_idx_l1_default as usize;
        let mut reorder_l0: Vec<(u32, u32)> = Vec::new();
        let mut reorder_l1: Vec<(u32, u32)> = Vec::new();
        if is_p || is_b {
            // num_ref_idx_active_override_flag
            if r.read_bit()? {
                num_ref_idx_l0 = (r.read_ue()? + 1) as usize;
                if is_b {
                    num_ref_idx_l1 = (r.read_ue()? + 1) as usize;
                }
            }
            // ref_pic_list_modification_flag_l0
            if r.read_bit()? {
                parse_ref_pic_list_modification(&mut r, &mut reorder_l0)?;
            }
            if is_b && r.read_bit()? {
                // ref_pic_list_modification_flag_l1
                parse_ref_pic_list_modification(&mut r, &mut reorder_l1)?;
            }
        }
        // Explicit weighted prediction carries a pred_weight_table() here. P
        // (weighted_pred) uses single-list weights; B explicit bipred (idc 1) is
        // not yet wired into the bi-pred averaging, so refuse that. Implicit
        // bipred (idc 2) carries no table.
        let weights = if is_p && pps.weighted_pred {
            Some(parse_pred_weight_table(&mut r, num_ref_idx_l0, 0, false)?)
        } else if is_b && pps.weighted_bipred_idc == 1 {
            return Err(DecodeError::Unsupported("explicit B weighted prediction"));
        } else {
            None
        };
        // dec_ref_pic_marking (spec §7.3.3.3) — present only for reference
        // pictures (nal_ref_idc != 0). Reading it for a non-reference slice would
        // desync the rest of the header.
        let mut idr_long_term = false;
        let mut mmco_ops: Vec<Mmco> = Vec::new();
        if nal_ref_idc == 0 {
            // non-reference picture: no marking syntax
        } else if is_idr {
            let _no_output_of_prior_pics = r.read_bit()?;
            idr_long_term = r.read_bit()?; // long_term_reference_flag
        } else if r.read_bit()? {
            // adaptive_ref_pic_marking_mode_flag
            loop {
                let op = r.read_ue()?;
                match op {
                    0 => break,
                    1 => mmco_ops.push(Mmco::Unref(r.read_ue()?)),
                    2 => mmco_ops.push(Mmco::UnrefLong(r.read_ue()?)),
                    3 => {
                        let diff = r.read_ue()?;
                        let idx = r.read_ue()?;
                        mmco_ops.push(Mmco::AssignLong(diff, idx));
                    }
                    4 => mmco_ops.push(Mmco::MaxLong(r.read_ue()?)),
                    5 => mmco_ops.push(Mmco::Reset),
                    6 => mmco_ops.push(Mmco::CurrentLong(r.read_ue()?)),
                    _ => return Err(DecodeError::Unsupported("invalid MMCO")),
                }
                if mmco_ops.len() > 128 {
                    return Err(DecodeError::Truncated);
                }
            }
        }
        // cabac_init_idc (spec §7.3.3) — CABAC context-model preset, P/B slices only.
        // Spec range [0,2]; a larger (corrupt) value would index the 4-model context-init
        // table out of bounds, so reject it here.
        let cabac_init_idc = if cabac && !is_i {
            let v = r.read_ue()?;
            if v > 2 {
                return Err(DecodeError::Unsupported("invalid cabac_init_idc"));
            }
            v
        } else {
            0
        };
        let slice_qp_delta = r.read_se()?;
        // When deblocking_filter_control_present_flag is 0 the slice carries no
        // disable_deblocking_filter_idc and it is inferred 0 — i.e. the in-loop
        // filter is ON by default (spec §7.4.3). (Our own encoder always signals
        // the control explicitly, so this default was previously untested.)
        let mut deblock = true;
        let (mut filter_offset_a, mut filter_offset_b) = (0i32, 0i32);
        if pps.deblocking_filter_control_present_flag {
            let disable_deblocking_filter_idc = r.read_ue()?;
            // idc 1 = filter off; idc 0 = on; idc 2 = on but not across slice
            // boundaries (equivalent to on for single-slice pictures).
            deblock = disable_deblocking_filter_idc != 1;
            if disable_deblocking_filter_idc != 1 {
                // FilterOffset = slice_*_offset_div2 × 2 (spec §7.4.3).
                filter_offset_a = r.read_se()? * 2;
                filter_offset_b = r.read_se()? * 2;
            }
        }
        let slice_qp = (pps.pic_init_qp + slice_qp_delta).clamp(0, 51) as u8;

        // Synthesize placeholder short-term references for any gap in frame_num
        // (spec §8.2.5.2) so the DPB / PicNum mapping stays correct.
        if first_mb_in_slice == 0 && !is_idr && sps.gaps_in_frame_num_allowed {
            self.insert_frame_num_gaps(
                frame_num,
                1u32 << sps.log2_max_frame_num,
                sps.max_num_ref_frames.max(1) as usize,
                sps.pic_width_in_mbs * 16,
                sps.pic_height_in_mbs * 16,
            );
        }

        // Build the reference list(s) for this slice. P uses RefPicList0 only;
        // B uses RefPicList0 and RefPicList1 (POC-ordered).
        let max_fn = 1u32 << sps.log2_max_frame_num;
        let (ref_list0, ref_list1) = if is_b {
            build_ref_list_b(
                &self.refs, pic_poc, frame_num, max_fn,
                num_ref_idx_l0, num_ref_idx_l1, &reorder_l0, &reorder_l1,
            )?
        } else if is_p {
            (build_ref_list_p(&self.refs, frame_num, max_fn, num_ref_idx_l0, &reorder_l0)?, Vec::new())
        } else {
            (Vec::new(), Vec::new())
        };
        // --- picture assembly ---
        // first_mb_in_slice == 0 starts a new picture; otherwise this slice
        // continues the one in flight. An IDR clears the DPB at its first slice.
        if first_mb_in_slice == 0 {
            if is_idr {
                self.refs.clear();
            }
            // H-49: the CABAC macroblock loop never decodes `transform_size_8x8_flag`
            // — both reads of it sit on the CAVLC `BitReader`, and `decode_i8x8` only
            // accepts one. A PPS with `transform_8x8_mode_flag` set therefore desyncs
            // the arithmetic decoder within a few macroblocks, and the failure surfaces
            // as a bogus `CABAC I_PCM` far from its cause (the mb_type parse lands on
            // 25 out of garbage). Fail fast and accurately instead: a wrong error that
            // points at the wrong feature costs more than a missing feature does.
            // Removing this guard requires the CABAC 8×8 residual path — see H-49.
            // DecSetup was declared in the Stage enum but never actually scoped, so
            // the per-picture grid allocation had been invisible in every profile.
            let _g_setup = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DecSetup);
            let mut fd = FrameDecoder::with_pool(
                sps.pic_width_in_mbs,
                sps.pic_height_in_mbs,
                slice_qp,
                pps.chroma_qp_index_offset,
                ref_list0,
                num_ref_idx_l0,
                pps.constrained_intra_pred_flag,
                pps.transform_8x8_mode_flag,
                sps.profile_idc != 66, // b_possible: Baseline/Constrained Baseline (66) forbid B
                // `RS_H264_NO_POOL=1` reproduces the pre-pool behaviour exactly (a
                // fresh allocation per picture) so the pool can be A/B'd paired on
                // one binary, with no rebuild between arms.
                if no_pool() { GridPool::default() } else { std::mem::take(&mut self.grid_pool) },
            );
            if is_b {
                fd.set_b_context(
                    ref_list1,
                    num_ref_idx_l1,
                    direct_spatial,
                    pic_poc,
                    pps.weighted_bipred_idc,
                    sps.direct_8x8_inference,
                );
            }
            if sps.has_scaling || pps.pic_scaling_matrix_present {
                let (s4, s8) = resolve_scaling(sps, pps);
                fd.set_scaling(s4, s8);
            }
            fd.set_transform_bypass(sps.transform_bypass);
            if let Some(w) = weights {
                fd.set_weights(w);
            }
            if let Some(slot) = self.progress_slot.clone() {
                fd.set_progress_slot(slot);
            }
            // A pending picture still here means the previous one never reached
            // total_mb and a new picture is now displacing it. That is a DECODER
            // desync, and dropping it silently is how the missing-B-slice-ref_idx
            // defect stayed hidden: the picture simply never entered the DPB, and
            // the failure surfaced hundreds of macroblocks later as "bitstream
            // truncated" from a reference-list modification asking for it. Refuse
            // to swallow it -- an incomplete picture must announce itself.
            if let Some(prev) = self.cur.take() {
                if prev.next_mb < prev.total_mb {
                    return Err(DecodeError::Truncated);
                }
            }
            self.cur = Some(PendingPic {
                fd,
                frame_num,
                poc: pic_poc,
                next_mb: 0,
                total_mb: sps.pic_width_in_mbs * sps.pic_height_in_mbs,
                slice_count: 0,
                deblock,
                filter_offset_a,
                filter_offset_b,
                crop_r: sps.frame_crop_right as usize,
                crop_b: sps.frame_crop_bottom as usize,
                max_refs: sps.max_num_ref_frames.max(1) as usize,
                log2_max_frame_num: sps.log2_max_frame_num,
                is_reference: nal_ref_idc != 0,
                idr_long_term,
                mmco_ops,
            });
        } else {
            // Continuation slice: reset the per-slice QP + reference list.
            let Some(pic) = self.cur.as_mut() else {
                return Err(DecodeError::Unsupported("slice continues a missing picture"));
            };
            pic.fd.begin_slice(slice_qp, ref_list0, num_ref_idx_l0);
            if is_b {
                pic.fd.set_b_context(
                    ref_list1,
                    num_ref_idx_l1,
                    direct_spatial,
                    pic.poc,
                    pps.weighted_bipred_idc,
                    sps.direct_8x8_inference,
                );
            }
            if sps.has_scaling || pps.pic_scaling_matrix_present {
                let (s4, s8) = resolve_scaling(sps, pps);
                pic.fd.set_scaling(s4, s8);
            }
            pic.fd.set_transform_bypass(sps.transform_bypass);
            if let Some(w) = weights {
                pic.fd.set_weights(w);
            }
            // Latest slice's marking/deblock parameters win at finalization.
            pic.deblock = deblock;
            pic.filter_offset_a = filter_offset_a;
            pic.filter_offset_b = filter_offset_b;
            pic.idr_long_term |= idr_long_term;
            pic.mmco_ops.extend(mmco_ops);
        }

        let pic = self.cur.as_mut().expect("pending picture set above");
        // Row-interleave (mb16::row_hook) needs the CURRENT slice's deblock
        // parameters during decode; `abl_deblock` resolved here so mb16 stays
        // knob-agnostic.
        pic.fd.set_deblock_params(deblock && !abl_deblock(), filter_offset_a, filter_offset_b);
        let first = first_mb_in_slice.min(pic.total_mb);
        let next = if cabac {
            // cabac_alignment_one_bit → the slice data is byte-aligned from here.
            r.align_to_byte().map_err(|_| DecodeError::Truncated)?;
            let (data, start) = (r.data(), r.bit_pos() / 8);
            pic.fd
                .decode_slice_data_cabac(data, start, slice_qp, cabac_init_idc, is_i, is_p, first)
        } else {
            pic.fd.decode_slice_data(&mut r, is_p, first)
        }
        .map_err(|e| match e {
            mb16::MbError::Truncated => DecodeError::Truncated,
            mb16::MbError::Unsupported(s) => DecodeError::Unsupported(s),
        })?;
        pic.next_mb = next;
        pic.slice_count += 1;
        if std::env::var_os("RH264_DUMP_MB").is_some() {
            eprintln!(
                "  slice decoded {}/{} MBs{}",
                next,
                pic.total_mb,
                if next < pic.total_mb { "   <-- INCOMPLETE" } else { "" }
            );
        }

        if pic.next_mb < pic.total_mb {
            return Ok(None); // picture not yet complete
        }

        // --- finalize the completed picture ---
        let pic = self.cur.take().expect("pending picture");
        let PendingPic {
            mut fd,
            frame_num,
            poc,
            deblock,
            filter_offset_a,
            filter_offset_b,
            crop_r,
            crop_b,
            max_refs,
            log2_max_frame_num,
            is_reference,
            idr_long_term,
            mmco_ops,
            ..
        } = pic;
        self.last_poc = poc;
        // MEASUREMENT KNOB (`RFF_ABL_DEBLOCK=1`): skip the loop filter to price it
        // with ZERO instrument tax. The scope-based profiler charges an rdtsc pair
        // per scope, and at ~20M per-MB scopes that tax reached 1.3-1.4x of the
        // whole decode -- so a per-MB stage's share cannot be read off it. Ablation
        // on the UNINSTRUMENTED binary is the honest price. Output is wrong while
        // set; decode WORK is unchanged (the filter reads and writes samples but
        // decides nothing), so the timing stays comparable.
        if deblock && !abl_deblock() {
            fd.deblock(filter_offset_a, filter_offset_b);
        }
        // The necessary DPB plane clone (rec_y/u/v → RefFrame) — measured as its own
        // stage, OUTSIDE the Finalize scope so the two don't double-count.
        let reference = if is_reference {
            let _dg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::DpbClone);
            Some(fd.as_reference_pooled(&mut self.plane_pool))
        } else {
            None
        };
        let _fg = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Finalize);
        if let Some(mut reference) = reference {
            reference.frame_num = frame_num;
            reference.poc = poc;
            if std::env::var_os("RH264_DUMP_MB").is_some() {
                eprintln!("DPB-ADD fn={frame_num} poc={poc}");
            }
            if idr_long_term {
                reference.long_term = true;
                reference.long_term_idx = 0;
            }
            let reference = reference; // RefFrame from as_reference_pooled
            if self.detach_dpb {
                self.detached_mmco = mmco_ops;
                self.detached_frame_num = frame_num;
                self.detached_log2_max_frame_num = log2_max_frame_num;
                self.detached_max_refs = max_refs;
                self.detached_idr_long_term = idr_long_term;
                if let Some(slot) = self.progress_slot.take() {
                    // Phase B: fold finished planes into the pre-shared Arc.
                    Self::fill_progress_slot(&slot, reference);
                    slot.mark_fully_ready();
                    self.detached_ref = Some(slot);
                } else {
                    let arc = std::sync::Arc::new(reference);
                    arc.mark_fully_ready();
                    self.detached_ref = Some(arc);
                }
            } else {
                reference.mark_fully_ready();
                self.prev_ref_frame_num = self.apply_ref_marking(
                    reference,
                    &mmco_ops,
                    frame_num,
                    log2_max_frame_num,
                    max_refs,
                );
            }
        }
        let (frame, pool) = fd.into_frame_recycle(crop_r, crop_b);
        self.grid_pool = pool;
        self.reclaim_retired();
        Ok(Some(frame))
    }

    /// Frame-MT: commit an already-shared reference Arc (Phase B progress slot).
    /// Applies the detached picture's reference + MMCO onto `self.refs` and
    /// returns the updated `prev_ref_frame_num`.
    pub(crate) fn commit_detached_ref_arc(
        &mut self,
        reference: Ref,
    ) -> Result<u32, DecodeError> {
        reference.mark_fully_ready();
        let mmco = std::mem::take(&mut self.detached_mmco);
        let frame_num = self.detached_frame_num;
        let log2 = self.detached_log2_max_frame_num;
        let max_refs = self.detached_max_refs;
        self.prev_ref_frame_num =
            self.apply_ref_marking_arc(reference, &mmco, frame_num, log2, max_refs);
        Ok(self.prev_ref_frame_num)
    }

    /// Fold a finished `as_reference_pooled` snapshot into a Phase B progress Arc:
    /// lock-free frozen planes + coloc meta (no steady-state RwLock on DPB MC).
    fn fill_progress_slot(slot: &Ref, mut finished: crate::RefFrame) {
        let planes = FrozenPlanes {
            py: std::mem::take(&mut finished.py),
            pu: std::mem::take(&mut finished.pu),
            pv: std::mem::take(&mut finished.pv),
        };
        if let Some(live) = &slot.live {
            let _w = live.wait.lock().unwrap();
            {
                let mut m = live.meta.write().unwrap();
                m.frame_num = finished.frame_num;
                m.poc = finished.poc;
                m.long_term = finished.long_term;
                m.long_term_idx = finished.long_term_idx;
                m.mv = std::mem::take(&mut finished.mv);
                m.ref_idx = std::mem::take(&mut finished.ref_idx);
                m.mv1 = std::mem::take(&mut finished.mv1);
                m.ref_idx1 = std::mem::take(&mut finished.ref_idx1);
                m.ref_poc = std::mem::take(&mut finished.ref_poc);
                m.w4 = finished.w4;
                m.motion_ready = true;
            }
            let _ = slot.frozen.set(planes);
            slot.ready_rows
                .store(slot.ch, std::sync::atomic::Ordering::Release);
            live.cv.notify_all();
        } else {
            let _ = slot.frozen.set(planes);
            slot.mark_fully_ready();
        }
    }

    /// Moves the padded planes of retired (DPB-evicted) reference frames into
    /// the recycle pool. Called after the current picture's `FrameDecoder` is
    /// consumed, at which point a retired frame's `Arc` is normally unique; a
    /// frame something still holds (it shouldn't) is simply dropped un-recycled.
    fn reclaim_retired(&mut self) {
        for arc in self.retired.drain(..) {
            if let Ok(mut rf) = std::sync::Arc::try_unwrap(arc) {
                if let Some(f) = rf.frozen.take() {
                    if !f.py.is_empty() {
                        self.plane_pool.push(f.py);
                        self.plane_pool.push(f.pu);
                        self.plane_pool.push(f.pv);
                    }
                } else if !rf.py.is_empty() {
                    self.plane_pool.push(rf.py);
                    self.plane_pool.push(rf.pu);
                    self.plane_pool.push(rf.pv);
                }
            }
        }
        // Bound the pool: 6 pictures' worth of planes (3 each) covers any
        // realistic ref churn; beyond that we'd just be hoarding memory.
        self.plane_pool.truncate(18);
    }

    /// Inserts "non-existing" short-term reference frames for each `frame_num`
    /// skipped since the previous reference picture (spec §8.2.5.2). Their samples
    /// are unspecified (a conformant stream never references them); we use mid-grey
    /// so any accidental reference is benign. They occupy DPB slots and advance the
    /// sliding window, keeping PicNum/ref-list derivation correct.
    fn insert_frame_num_gaps(&mut self, frame_num: u32, max_fn: u32, max_refs: usize, w: usize, h: usize) {
        if max_fn == 0 {
            return;
        }
        let start = (self.prev_ref_frame_num + 1) % max_fn;
        let gap = (frame_num + max_fn - start) % max_fn;
        if gap == 0 {
            return;
        }
        // Each placeholder is inserted at the front then the DPB is truncated to
        // `max_refs`, so for a gap larger than that only the most recent `max_refs`
        // placeholders can survive. Materialise just those — a malformed stream can
        // declare a gap of MaxFrameNum-1 (up to 65535), and allocating that many
        // full frames would be a CPU/memory DoS.
        let cap = max_refs.max(1);
        let n = (gap as usize).min(cap);
        let (cw, ch) = (w, h);
        let mut expected = (frame_num + max_fn - n as u32) % max_fn;
        for _ in 0..n {
            self.refs.insert(
                0,
                std::sync::Arc::new(RefFrame {
                    // Uniform grey: the padded plane of a uniform plane is itself.
                    py: vec![128; (cw + 2 * LPAD) * (ch + 2 * LPAD)],
                    pu: vec![128; (cw / 2 + 2 * CPAD) * (ch / 2 + 2 * CPAD)],
                    pv: vec![128; (cw / 2 + 2 * CPAD) * (ch / 2 + 2 * CPAD)],
                    cw,
                    ch,
                    ready_rows: std::sync::atomic::AtomicUsize::new(ch),
                    live: None,
                    frozen: std::sync::OnceLock::new(),
                    frame_num: expected,
                    poc: 0,
                    mv: Vec::new(),
                    ref_idx: Vec::new(),
                    mv1: Vec::new(),
                    ref_idx1: Vec::new(),
                    ref_poc: Vec::new(),
                    w4: 0,
                    long_term: false,
                    long_term_idx: 0,
                }),
            );
            self.refs.truncate(cap);
            expected = (expected + 1) % max_fn;
        }
        self.prev_ref_frame_num = (frame_num + max_fn - 1) % max_fn;
    }

    /// The `PicOrderCnt` of the most recently returned picture. Pictures are
    /// returned in decode order; sorting them by this value yields display order
    /// (the only difference is reordered B-pictures).
    pub fn last_poc(&self) -> i32 {
        self.last_poc
    }

    fn compute_poc(
        &mut self,
        sps: &Sps,
        is_idr: bool,
        nal_ref_idc: u8,
        frame_num: u32,
        poc_lsb: u32,
        delta_bottom: i32,
    ) -> i32 {
        self.poc
            .compute_poc(sps, is_idr, nal_ref_idc, frame_num, poc_lsb, delta_bottom)
    }

    /// Inserts the just-decoded picture into the DPB and marks references
    /// (spec §8.2.5). With no MMCO commands this is the sliding window (evict the
    /// oldest short-term reference past capacity); with MMCO it is adaptive
    /// marking, including long-term assignment.
    ///
    /// Takes `reference` BY VALUE and MOVES it into the DPB (the caller's local is
    /// dropped right after) — the old `&mut` + `insert(0, reference.clone())` cloned
    /// all three planes (~1.35 MB/frame) a second time, on top of `as_reference`'s
    /// necessary clone. Returns the picture's final `frame_num` (0 after MMCO 5) for
    /// the caller's gap-detection tracking, since `reference` is gone after the move.
    fn apply_ref_marking(
        &mut self,
        mut reference: RefFrame,
        ops: &[Mmco],
        frame_num: u32,
        log2_max_frame_num: u32,
        max_refs: usize,
    ) -> u32 {
        let max = 1i64 << log2_max_frame_num;
        let curr = frame_num as i64;
        let pic_num = |rf: &RefFrame| -> i64 {
            if (rf.frame_num as i64) > curr {
                rf.frame_num as i64 - max
            } else {
                rf.frame_num as i64
            }
        };

        if ops.is_empty() {
            // Sliding window: insert the current (short-term) picture, then evict
            // the oldest short-term reference while over capacity (long-term refs
            // are retained).
            let out_fn = reference.frame_num;
            self.refs.insert(0, std::sync::Arc::new(reference));
            while self.refs.len() > max_refs {
                match self.refs.iter().rposition(|r| !r.long_term) {
                    Some(pos) => {
                        // Park the evicted frame; its planes are reclaimed on the
                        // next picture boundary (see `reclaim_retired`).
                        let evicted = self.refs.remove(pos);
                        self.retired.push(evicted);
                    }
                    None => break,
                }
            }
            return out_fn;
        }

        // Adaptive marking (MMCO), applied in order.
        for &op in ops {
            match op {
                Mmco::Unref(diff) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs.retain(|r| r.long_term || pic_num(r) != target);
                }
                Mmco::UnrefLong(ltpn) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == ltpn));
                }
                Mmco::AssignLong(diff, idx) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == idx));
                    for r in self.refs.iter_mut() {
                        if !r.long_term && pic_num(r) == target {
                            // Rare op; make_mut only copies if a slice still holds it.
                            let r = std::sync::Arc::make_mut(r);
                            r.long_term = true;
                            r.long_term_idx = idx;
                        }
                    }
                }
                Mmco::MaxLong(max_plus1) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx + 1 > max_plus1));
                }
                Mmco::Reset => {
                    self.refs.clear();
                    reference.frame_num = 0;
                }
                Mmco::CurrentLong(idx) => {
                    self.refs.retain(|r| !(r.long_term && r.long_term_idx == idx));
                    reference.long_term = true;
                    reference.long_term_idx = idx;
                }
            }
        }
        let out_fn = reference.frame_num;
        self.refs.insert(0, std::sync::Arc::new(reference));
        // Safety net so a malformed marking stream can't grow the DPB unbounded.
        let cap = max_refs.max(16);
        if self.refs.len() > cap {
            self.refs.truncate(cap);
        }
        out_fn
    }

    /// Like [`Self::apply_ref_marking`] but inserts an existing `Arc` (Phase B
    /// progress slot / detached worker output).
    fn apply_ref_marking_arc(
        &mut self,
        mut reference: Ref,
        ops: &[Mmco],
        frame_num: u32,
        log2_max_frame_num: u32,
        max_refs: usize,
    ) -> u32 {
        let max = 1i64 << log2_max_frame_num;
        let curr = frame_num as i64;
        let pic_num = |rf: &RefFrame| -> i64 {
            let f = rf.fn_num() as i64;
            if f > curr {
                f - max
            } else {
                f
            }
        };

        if ops.is_empty() {
            let out_fn = reference.fn_num();
            self.refs.insert(0, reference);
            while self.refs.len() > max_refs {
                match self.refs.iter().rposition(|r| !r.is_long_term()) {
                    Some(pos) => {
                        let evicted = self.refs.remove(pos);
                        self.retired.push(evicted);
                    }
                    None => break,
                }
            }
            return out_fn;
        }

        for &op in ops {
            match op {
                Mmco::Unref(diff) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs.retain(|r| r.is_long_term() || pic_num(r) != target);
                }
                Mmco::UnrefLong(ltpn) => {
                    self.refs
                        .retain(|r| !(r.is_long_term() && r.lt_idx() == ltpn));
                }
                Mmco::AssignLong(diff, idx) => {
                    let target = curr - (diff as i64 + 1);
                    self.refs
                        .retain(|r| !(r.is_long_term() && r.lt_idx() == idx));
                    for r in self.refs.iter_mut() {
                        if !r.is_long_term() && pic_num(r) == target {
                            if r.live.is_some() {
                                r.set_long_term_marks(true, idx);
                            } else {
                                let r = std::sync::Arc::make_mut(r);
                                r.long_term = true;
                                r.long_term_idx = idx;
                            }
                        }
                    }
                }
                Mmco::MaxLong(max_plus1) => {
                    self.refs
                        .retain(|r| !(r.is_long_term() && r.lt_idx() + 1 > max_plus1));
                }
                Mmco::Reset => {
                    self.refs.clear();
                    reference.set_frame_num_live(0);
                    if let Some(r) = std::sync::Arc::get_mut(&mut reference) {
                        r.frame_num = 0;
                    }
                }
                Mmco::CurrentLong(idx) => {
                    self.refs
                        .retain(|r| !(r.is_long_term() && r.lt_idx() == idx));
                    reference.set_long_term_marks(true, idx);
                    if let Some(r) = std::sync::Arc::get_mut(&mut reference) {
                        r.long_term = true;
                        r.long_term_idx = idx;
                    }
                }
            }
        }
        let out_fn = reference.fn_num();
        self.refs.insert(0, reference);
        let cap = max_refs.max(16);
        if self.refs.len() > cap {
            self.refs.truncate(cap);
        }
        out_fn
    }
}

/// Emits a GOP's buffered pictures in display order (sorted by `PicOrderCnt`).
pub(crate) fn flush_gop(gop: &mut Vec<(i32, YuvFrame)>, out: &mut Vec<YuvFrame>) {
    gop.sort_by_key(|(poc, _)| *poc);
    out.extend(gop.drain(..).map(|(_, f)| f));
}

/// Whether an access unit contains an IDR coded-slice NAL.
///
/// Public for harnesses that reimplement `decode_stream`'s display-order emit
/// with an early stop (e.g. correctness probes that only need the first N pictures).
pub fn au_is_idr(au: &[u8]) -> bool {
    split_annex_b(au)
        .iter()
        .any(|n| !n.is_empty() && NalUnitType::from_id(n[0]) == NalUnitType::IdrSlice)
}

/// Splits an Annex-B byte stream into access units, each ending after a VCL
/// (coded-slice) NAL with any preceding parameter-set/SEI NALs attached. Start
/// codes are preserved so each unit can be passed straight to [`Decoder::decode`].
///
/// Public because [`Decoder::decode`] takes ONE access unit: a caller that wants
/// decode-order pictures, or wants to drop each picture as it arrives instead of
/// accumulating the stream like [`Decoder::decode_stream`], needs this to feed it.
pub fn split_access_units(stream: &[u8]) -> Vec<&[u8]> {
    // (offset of the start code, whether the NAL it begins is a VCL slice).
    let mut codes: Vec<(usize, bool)> = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let nal_type = NalUnitType::from_id(stream.get(i + 3).copied().unwrap_or(0));
            let is_vcl = matches!(nal_type, NalUnitType::IdrSlice | NalUnitType::NonIdrSlice);
            // Include a leading zero (4-byte start code) in the unit boundary.
            let sc = if i > 0 && stream[i - 1] == 0 { i - 1 } else { i };
            codes.push((sc, is_vcl));
            i += 3;
        } else {
            i += 1;
        }
    }
    if codes.is_empty() {
        return vec![stream];
    }
    let mut aus = Vec::new();
    let mut start = codes[0].0;
    for k in 0..codes.len() {
        if codes[k].1 {
            let end = codes.get(k + 1).map_or(stream.len(), |c| c.0);
            aus.push(&stream[start..end]);
            start = end;
        }
    }
    aus
}

/// Parses a `pred_weight_table()` (spec §7.3.3.2) for the active reference lists
/// (4:2:0 → chroma weights always present). List 1 is parsed only for B slices.
fn parse_pred_weight_table(
    r: &mut BitReader,
    num_l0: usize,
    num_l1: usize,
    is_b: bool,
) -> Result<WeightTable, DecodeError> {
    let luma_log2_denom = r.read_ue()? as i32;
    let chroma_log2_denom = r.read_ue()? as i32;
    // Spec §7.4.3.2 constrains both weight denoms to [0, 7]; a malformed stream can
    // carry any ue(v). Reject before `1 << denom` (which overflows for denom ≥ 31)
    // so a corrupt bitstream is rejected gracefully, never panics.
    if !(0..=7).contains(&luma_log2_denom) || !(0..=7).contains(&chroma_log2_denom) {
        return Err(DecodeError::Unsupported("invalid weight denom"));
    }
    let mut wt = WeightTable {
        luma_log2_denom,
        chroma_log2_denom,
        ..Default::default()
    };
    let lists: &[(usize, usize)] = if is_b {
        &[(0, num_l0), (1, num_l1)]
    } else {
        &[(0, num_l0)]
    };
    for &(list, n) in lists {
        let mut luma = Vec::with_capacity(n);
        let mut chroma = Vec::with_capacity(n);
        for _ in 0..n {
            let (mut lw, mut lo) = (1 << luma_log2_denom, 0);
            if r.read_bit()? {
                lw = r.read_se()?;
                lo = r.read_se()?;
            }
            luma.push((lw, lo));
            let mut ch = [(1 << chroma_log2_denom, 0); 2];
            if r.read_bit()? {
                for slot in ch.iter_mut() {
                    *slot = (r.read_se()?, r.read_se()?);
                }
            }
            chroma.push(ch);
        }
        wt.luma[list] = luma;
        wt.chroma[list] = chroma;
    }
    Ok(wt)
}

/// Resolves the effective scaling matrices for a slice from the SPS lists and
/// any PPS override (fall-back rule B), returning them un-zig-zagged to raster
/// order: six 4×4 lists [Y/Cb/Cr intra, Y/Cb/Cr inter] and two 8×8 luma lists
/// [Y-intra, Y-inter].
fn resolve_scaling(sps: &Sps, pps: &Pps) -> ([[i32; 16]; 6], [[i32; 64]; 2]) {
    use crate::params::{
        DEFAULT_4X4_INTER, DEFAULT_4X4_INTRA, DEFAULT_8X8_INTER, DEFAULT_8X8_INTRA,
    };
    const ZZ4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
    // 8×8 frame zig-zag scan → raster index (spec Table 8-12).
    const ZZ8: [usize; 64] = [
        0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
        20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
        58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
    ];
    // Effective zig-zag lists: a PPS override (rule B) takes precedence; an absent
    // PPS list falls back to the SPS list (or the default / previous PPS list).
    let mut z4 = [[16u8; 16]; 6];
    for i in 0..6 {
        z4[i] = if pps.pic_scaling_matrix_present {
            if pps.scaling_present_4x4[i] {
                pps.scaling_4x4[i]
            } else {
                match i {
                    0 if sps.has_scaling => sps.scaling_4x4[0],
                    0 => DEFAULT_4X4_INTRA,
                    3 if sps.has_scaling => sps.scaling_4x4[3],
                    3 => DEFAULT_4X4_INTER,
                    _ => z4[i - 1],
                }
            }
        } else {
            sps.scaling_4x4[i]
        };
    }
    let mut z8 = [[16u8; 64]; 2];
    for (i, list) in z8.iter_mut().enumerate() {
        *list = if pps.pic_scaling_matrix_present {
            if pps.scaling_present_8x8[i] {
                pps.scaling_8x8[i]
            } else if sps.has_scaling {
                sps.scaling_8x8[i]
            } else if i == 0 {
                DEFAULT_8X8_INTRA
            } else {
                DEFAULT_8X8_INTER
            }
        } else {
            sps.scaling_8x8[i]
        };
    }
    let mut out4 = [[16i32; 16]; 6];
    for (li, list) in out4.iter_mut().enumerate() {
        for k in 0..16 {
            list[ZZ4[k]] = z4[li][k] as i32;
        }
    }
    let mut out8 = [[16i32; 64]; 2];
    for (li, list) in out8.iter_mut().enumerate() {
        for k in 0..64 {
            list[ZZ8[k]] = z8[li][k] as i32;
        }
    }
    (out4, out8)
}

/// Parses a `ref_pic_list_modification` command list (spec §7.3.3.1) into
/// `(modification_of_pic_nums_idc, value)` pairs, stopping at idc 3.
fn parse_ref_pic_list_modification(
    r: &mut BitReader,
    out: &mut Vec<(u32, u32)>,
) -> Result<(), DecodeError> {
    loop {
        let idc = r.read_ue()?;
        if idc == 3 {
            break;
        }
        if idc > 3 {
            return Err(DecodeError::Unsupported("invalid ref_pic_list_modification"));
        }
        let val = r.read_ue()?; // abs_diff_pic_num_minus1 / long_term_pic_num
        out.push((idc, val));
        if out.len() > 64 {
            return Err(DecodeError::Truncated); // runaway / corrupt
        }
    }
    Ok(())
}

/// Builds the P-slice `RefPicList0`: short-term references ordered by descending
/// `FrameNumWrap`, then long-term by ascending idx (spec §8.2.4.2.1), with any
/// `ref_pic_list_modification` applied.
fn build_ref_list_p(
    dpb: &[Ref],
    curr_frame_num: u32,
    max_frame_num: u32,
    num_active: usize,
    mods: &[(u32, u32)],
) -> Result<Vec<Ref>, DecodeError> {
    let curr = curr_frame_num as i64;
    let max = max_frame_num as i64;
    let pic_num = |fnum: u32| -> i64 {
        let f = fnum as i64;
        if f > curr { f - max } else { f }
    };
    let mut init: Vec<Ref> = dpb.iter().filter(|r| !r.is_long_term()).cloned().collect();
    init.sort_by_key(|rf| core::cmp::Reverse(pic_num(rf.fn_num())));
    let mut long: Vec<Ref> = dpb.iter().filter(|r| r.is_long_term()).cloned().collect();
    long.sort_by_key(|rf| rf.lt_idx());
    init.extend(long);
    apply_list_modification(init, curr_frame_num, max_frame_num, num_active, mods)
}

/// Builds the B-slice `RefPicList0` and `RefPicList1` (spec §8.2.4.2.3), ordered
/// by `PicOrderCnt` relative to the current picture: List0 leads with nearer
/// past pictures, List1 with nearer future pictures. Long-term references follow.
/// Per-list `ref_pic_list_modification` is then applied.
#[allow(clippy::too_many_arguments)]
fn build_ref_list_b(
    dpb: &[Ref],
    curr_poc: i32,
    curr_frame_num: u32,
    max_frame_num: u32,
    num0: usize,
    num1: usize,
    mods0: &[(u32, u32)],
    mods1: &[(u32, u32)],
) -> Result<(Vec<Ref>, Vec<Ref>), DecodeError> {
    let mut less: Vec<Ref> = dpb
        .iter()
        .filter(|r| !r.is_long_term() && r.pic_poc() < curr_poc)
        .cloned()
        .collect();
    let mut greater: Vec<Ref> = dpb
        .iter()
        .filter(|r| !r.is_long_term() && r.pic_poc() > curr_poc)
        .cloned()
        .collect();
    let mut long: Vec<Ref> = dpb.iter().filter(|r| r.is_long_term()).cloned().collect();
    less.sort_by_key(|r| core::cmp::Reverse(r.pic_poc())); // nearest past first
    greater.sort_by_key(|r| r.pic_poc()); // nearest future first
    long.sort_by_key(|r| r.lt_idx());

    let mut init0 = less.clone();
    init0.extend(greater.clone());
    init0.extend(long.clone());
    let mut init1 = greater;
    init1.extend(less);
    init1.extend(long);

    // When List1 (truncated to its active length) equals List0 and has more than
    // one entry, swap its first two entries (spec §8.2.4.2.3).
    let eq_len = num0.min(num1).min(init0.len()).min(init1.len());
    if num1 > 1
        && init1.len() > 1
        && (0..eq_len).all(|i| same_picture(&init0[i], &init1[i]))
        && eq_len == num1.min(init1.len())
        && eq_len == num0.min(init0.len())
    {
        init1.swap(0, 1);
    }

    let list0 = apply_list_modification(init0, curr_frame_num, max_frame_num, num0, mods0)?;
    let list1 = apply_list_modification(init1, curr_frame_num, max_frame_num, num1, mods1)?;
    Ok((list0, list1))
}

/// Two DPB entries refer to the same picture (used for the List1 swap rule).
fn same_picture(a: &RefFrame, b: &RefFrame) -> bool {
    a.is_long_term() == b.is_long_term()
        && if a.is_long_term() {
            a.lt_idx() == b.lt_idx()
        } else {
            a.pic_poc() == b.pic_poc()
        }
}

/// Applies `ref_pic_list_modification` to an initialized reference list and
/// truncates it to `num_active` (spec §8.2.4.3). `init` is the full ordered list;
/// the result is `num_active` entries, possibly reordered. idc 0/1 reference
/// short-term pictures by PicNum, idc 2 long-term ones by LongTermFrameIdx.
fn apply_list_modification(
    init: Vec<Ref>,
    curr_frame_num: u32,
    max_frame_num: u32,
    num_active: usize,
    mods: &[(u32, u32)],
) -> Result<Vec<Ref>, DecodeError> {
    if mods.is_empty() {
        let mut init = init;
        init.truncate(num_active.max(1));
        return Ok(init);
    }
    let curr = curr_frame_num as i64;
    let max = max_frame_num as i64;
    let mut list = init.clone();
    let mut pic_num_pred = curr;
    let mut refidx = 0usize;
    for &(idc, val) in mods {
        let matches: Box<dyn Fn(&RefFrame) -> bool> = if idc == 2 {
            Box::new(move |r: &RefFrame| r.is_long_term() && r.lt_idx() == val)
        } else {
            let abs_diff = (val as i64) + 1;
            let no_wrap = if idc == 0 {
                let x = pic_num_pred - abs_diff;
                if x < 0 { x + max } else { x }
            } else {
                let x = pic_num_pred + abs_diff;
                if x >= max { x - max } else { x }
            };
            pic_num_pred = no_wrap;
            let target = if no_wrap > curr { no_wrap - max } else { no_wrap };
            Box::new(move |r: &RefFrame| {
                let f = r.fn_num() as i64;
                let pn = if f > curr { f - max } else { f };
                !r.is_long_term() && pn == target
            })
        };
        let found = init.iter().find(|r| matches(r)).cloned();
        let Some(found) = found else {
            if std::env::var_os("RH264_DUMP_MB").is_some() {
                let cand: Vec<String> = init
                    .iter()
                    .map(|r| {
                        let f = r.fn_num() as i64;
                        let pn = if f > curr { f - max } else { f };
                        format!(
                            "(fn={} poc={} lt={} picnum={})",
                            r.fn_num(),
                            r.pic_poc(),
                            r.is_long_term(),
                            pn
                        )
                    })
                    .collect();
                eprintln!(
                    "MODFAIL idc={idc} val={val}  curr_frame_num={curr} max={max}  init={}",
                    cand.join(" ")
                );
            }
            return Err(DecodeError::Truncated); // references a picture not in the DPB
        };
        if refidx > list.len() {
            break;
        }
        list.insert(refidx, found);
        if let Some(dup) = list.iter().enumerate().skip(refidx + 1).find(|(_, r)| matches(r)).map(|(i, _)| i) {
            list.remove(dup);
        }
        refidx += 1;
        if refidx >= num_active {
            break;
        }
    }
    list.truncate(num_active.max(1));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_at(poc: i32, fnum: u32) -> Ref {
        std::sync::Arc::new(RefFrame {
            py: vec![],
            pu: vec![],
            pv: vec![],
            cw: 0,
            ch: 0,
            ready_rows: std::sync::atomic::AtomicUsize::new(0),
            live: None,
            frozen: std::sync::OnceLock::new(),
            frame_num: fnum,
            poc,
            mv: Vec::new(),
            ref_idx: Vec::new(),
            mv1: Vec::new(),
            ref_idx1: Vec::new(),
            ref_poc: Vec::new(),
            w4: 0,
            long_term: false,
            long_term_idx: 0,
        })
    }

    #[test]
    fn b_ref_lists_ordered_by_poc() {
        // Current POC 4; DPB has past (0,2) and future (6,8) references.
        let dpb = vec![ref_at(8, 4), ref_at(6, 3), ref_at(2, 1), ref_at(0, 0)];
        let (l0, l1) = build_ref_list_b(&dpb, 4, 5, 16, 4, 4, &[], &[]).unwrap();
        // List0: nearer past first (desc), then nearer future (asc).
        assert_eq!(l0.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![2, 0, 6, 8]);
        // List1: nearer future first (asc), then nearer past (desc).
        assert_eq!(l1.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![6, 8, 2, 0]);
    }

    #[test]
    fn b_ref_list1_swap_when_equal() {
        // Only past references -> List0 and List1 initialize identically, so
        // List1's first two entries are swapped (spec §8.2.4.2.3).
        let dpb = vec![ref_at(4, 2), ref_at(2, 1), ref_at(0, 0)];
        let (l0, l1) = build_ref_list_b(&dpb, 6, 3, 16, 3, 3, &[], &[]).unwrap();
        assert_eq!(l0.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![4, 2, 0]);
        assert_eq!(l1.iter().map(|r| r.poc).collect::<Vec<_>>(), vec![2, 4, 0]);
    }

    #[test]
    fn frame_num_gaps_insert_placeholders() {
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 2;
        // frame_num jumps 2 -> 5: placeholders for the skipped 3 and 4.
        d.insert_frame_num_gaps(5, 16, 8, 16, 16);
        let fns: Vec<u32> = d.refs.iter().map(|r| r.frame_num).collect();
        assert_eq!(fns, vec![4, 3], "most-recent placeholder at the front");
        assert_eq!(d.prev_ref_frame_num, 4);
        assert!(d.refs.iter().all(|r| r.py.iter().all(|&p| p == 128)), "grey fill");
    }

    #[test]
    fn frame_num_gaps_wrap_and_noop() {
        // Wrap across MaxFrameNum: prev 14, frame_num 1 (max 16) -> fill 15, 0.
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 14;
        d.insert_frame_num_gaps(1, 16, 8, 16, 16);
        assert_eq!(d.refs.iter().map(|r| r.frame_num).collect::<Vec<_>>(), vec![0, 15]);
        // No gap (consecutive) inserts nothing.
        let mut d = Decoder::new();
        d.prev_ref_frame_num = 3;
        d.insert_frame_num_gaps(4, 16, 8, 16, 16);
        assert!(d.refs.is_empty());
    }

    #[test]
    fn missing_param_sets_errors() {
        let mut d = Decoder::new();
        // A lone (fake) IDR slice header: first_mb_in_slice=0, slice_type=7 (I),
        // pic_parameter_set_id=0 — then the PPS lookup fails (none stored).
        let nal = rusty_h264_common::NalUnit::new(3, NalUnitType::IdrSlice, vec![0x88, 0x80]);
        let err = d.decode(&nal.to_annex_b()).unwrap_err();
        assert_eq!(err, DecodeError::MissingParameterSet);
    }
}
