//! Pure-Rust H.264 encoder — raw I420 frames in, a conformant Annex-B stream out.
//!
//! Every frame it emits decodes **bit-exactly under ffmpeg across QP 0–51**,
//! intra and inter. The crate is `#![forbid(unsafe_code)]`; the optional SIMD
//! kernels behind the `asm` feature keep their `unsafe` quarantined in
//! `rusty_h264-accel`, so that guarantee holds either way.
//!
//! Coding tools, default-on: `I_16x16`/`I_4x4`/`I_PCM` intra with λ-based
//! RD/SATD mode decision; P-frames (`P_Skip`, 16×16/16×8/8×16) with quarter-pel
//! motion compensation, rate-aware ME and a multi-reference DPB; **CABAC**
//! entropy coding (Main profile — set `RUSTY_H264_LEGACY_CAVLC=1` to restore
//! the Constrained Baseline + CAVLC bitstream byte-for-byte); **adaptive
//! quantization**; the per-GOP I-frame QP cascade; in-loop deblocking; and
//! average-bitrate rate control. Opt-in via [`EncoderConfig`]: B-frames (fixed
//! or content-adaptive), the 8×8 transform, mb-tree temporal AQ, sub-8×8
//! partitions and RD `P_Skip`.
//!
//! [`Preset`] picks the speed/quality trade-off — `Fast` (SAD, integer-pel),
//! `Balanced` (adds sub-pel refinement; the default) or `Quality` (full RD
//! trial-encode). The bitstream is valid either way; only the effort differs.
//!
//! ```
//! use rusty_h264_encoder::{Encoder, EncoderConfig};
//! use rusty_h264_common::YuvFrame;
//!
//! let cfg = EncoderConfig::new(16, 16);
//! let mut enc = Encoder::new(cfg).unwrap();
//! let frame = YuvFrame::black(16, 16);
//! // The default config carries a lookahead (mb-tree), so `encode()` may
//! // buffer — `flush()` at end of stream is part of the streaming contract.
//! let mut bitstream = enc.encode(&frame);
//! bitstream.extend_from_slice(&enc.flush());
//! assert!(!bitstream.is_empty());
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[allow(unused_imports)]
use rusty_h264_common::once::OnceLock;
extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(all(not(feature = "std"), not(feature = "libm")))]
compile_error!(
    "rusty_h264-encoder without `std` needs the `libm` feature for its floating-point math"
);

// ---------------------------------------------------------------------------
// `no_std` shims (see rusty_h264-common for the knob shim). Every `std` use
// left in this crate is a diagnostic or a per-thread convenience; without
// `std` a knob reads unset, a print is a no-op and a thread-local is built
// fresh per `with` call. Defined before the modules: textual scope.
// ---------------------------------------------------------------------------
#[cfg(not(feature = "std"))]
#[allow(unused_macros)]
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
/// `thread_local!` without threads: each `NAME.with(|v| ..)` builds the value
/// fresh. The encoder uses these for recycled per-frame scratch, so without
/// `std` that scratch is allocated per frame instead of recycled.
#[cfg(not(feature = "std"))]
macro_rules! thread_local {
    () => {};
    ($(#[$m:meta])* $vis:vis static $name:ident: $ty:ty = const { $init:expr }; $($rest:tt)*) => {
        $(#[$m])* #[allow(non_camel_case_types)] $vis struct $name;
        impl $name {
            #[allow(dead_code)]
            pub fn with<R>(&self, f: impl FnOnce(&$ty) -> R) -> R {
                let v: $ty = $init;
                f(&v)
            }
        }
        thread_local!($($rest)*);
    };
    ($(#[$m:meta])* $vis:vis static $name:ident: $ty:ty = $init:expr; $($rest:tt)*) => {
        $(#[$m])* #[allow(non_camel_case_types)] $vis struct $name;
        impl $name {
            #[allow(dead_code)]
            pub fn with<R>(&self, f: impl FnOnce(&$ty) -> R) -> R {
                let v: $ty = $init;
                f(&v)
            }
        }
        thread_local!($($rest)*);
    };
}

#[allow(unused_imports)]
use alloc::string::{String, ToString};
#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;
#[allow(unused_imports)]
use rusty_h264_common::fmath::{F32Ext as _, F64Ext as _};

pub mod bitacct;
mod cabac;
mod config;
mod fastmath;
mod lookahead;
pub mod mb16;
mod mbtree;

/// Prometheus telemetry hooks — the CABAC entropy-bin tap for offline
/// probability-law discovery by the private Prometheus refinery (CASC
/// campaign). Opt-in behind the `prometheus-telemetry` feature; the
/// production build is byte-identical without it (and with it — the tap
/// only observes the emit path, never steers it).
#[cfg(feature = "prometheus-telemetry")]
pub mod telemetry;
#[cfg(feature = "prometheus-telemetry")]
pub mod prometheus_telemetry {
    pub use crate::telemetry::{enable, p_zero_q8, take, CabacBin, SliceTap};
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;
}

/// Lookahead candidate evaluations so far (mb-tree cost instrument, H-36) — a
/// deterministic stand-in for wall time, which this box cannot measure at the
/// precision the content effect needs. `reset` before an encode, read after.
pub fn mbtree_satd_calls() -> u64 {
    mbtree::SATD_CALLS.load(core::sync::atomic::Ordering::Relaxed)
}
/// Zeroes [`mbtree_satd_calls`].
pub fn mbtree_satd_reset() {
    mbtree::SATD_CALLS.store(0, core::sync::atomic::Ordering::Relaxed)
}
/// Gate fire-rate census (Tier 1 of the gate-regression harness): `(fired,
/// seen)` per tracked gate, in [`gate_census_names`] order. Deterministic —
/// one run is the verdict. See `signals::census`.
/// Per-GOP mb-tree gate telemetry (Front-B harvest seam).
pub use mbtree::gopstats;

pub fn gate_census() -> Vec<(u64, u64)> {
    signals::census::snapshot().to_vec()
}
/// Per-gate `(fired, seen)` split by the macroblock's TRANSFORM SIZE:
/// `[0]` = macroblocks coded 4x4, `[1]` = coded 8x8, each in [`gate_census_names`]
/// order. A LABEL on the existing counters, not a new gate — it answers whether a
/// per-transform-size threshold could ever be worth fitting, before one is.
pub fn gate_census_by_t8() -> [Vec<(u64, u64)>; 2] {
    let s = signals::census::snapshot_by_t8();
    [s[0].to_vec(), s[1].to_vec()]
}
/// Per-reference LUMA weight estimation for one P picture — the weightp fade
/// detector (x264-parity `weightp`). A DC-ratio fit at denom 6 per reference,
/// kept only when a subsampled zero-MV SAD improves by >1% (the x264-style
/// keep test); identity `(64, 0)` otherwise, so non-fade content's slices
/// carry only the table's flag bits. LUMA only — matching the streams x264's
/// own weightp emits (its chroma stays unweighted there too).
fn estimate_luma_weights(
    cfg: &EncoderConfig,
    frame: &YuvPlanes<'_>,
    refs: &[RefFrame],
) -> Vec<(i32, i32)> {
    let cw = cfg.mb_width() * 16;
    let (w, h) = (frame.width.min(cw), frame.height);
    // The CURRENT frame's subsample grid is identical for every reference, yet
    // it was re-walked per ref — twice per ref when the keep test ran (up to 6
    // full grid passes at the default refs = 3). One pass now collects the sum
    // AND the samples; each reference likewise caches its samples on its means
    // pass so the keep test re-reads a compact buffer instead of re-striding
    // the plane. Same samples in the same row-major order: BIT-IDENTICAL.
    let mut cur: Vec<u8> = Vec::new();
    let mut sc = 0u64;
    let mut y = 0;
    while y < h {
        // Row slice (`w <= frame.width` by construction): the loop guard
        // `x < w == row.len()` lets LLVM discharge the indexing, where the
        // multiplied form re-proved bounds per sample.
        let row = &frame.y[y * frame.width..][..w];
        let mut x = 0;
        while x < w {
            let p = row[x];
            sc += p as u64;
            cur.push(p);
            x += 4;
        }
        y += 4;
    }
    let n = cur.len() as u64;
    let mut rbuf: Vec<u8> = Vec::with_capacity(cur.len());
    refs.iter()
        .map(|r| {
            // Subsampled reference mean (every 4th pixel, both axes).
            rbuf.clear();
            let mut sr = 0u64;
            let mut y = 0;
            while y < h {
                let row = &r.y[y * cw..][..w]; // w <= cw by construction
                let mut x = 0;
                while x < w {
                    let p = row[x];
                    sr += p as u64;
                    rbuf.push(p);
                    x += 4;
                }
                y += 4;
            }
            if n == 0 || sr == 0 {
                return (64, 0);
            }
            let (mc, mr) = (sc as f64 / n as f64, sr as f64 / n as f64);
            let lw = (rusty_h264_common::fmath::round(mc * 64.0 / mr) as i32).clamp(1, 127);
            let lo = (rusty_h264_common::fmath::round(mc - (lw as f64) * mr / 64.0) as i32)
                .clamp(-128, 127);
            if (lw, lo) == (64, 0) {
                return (64, 0);
            }
            // Keep test: the weighted reference must actually predict better.
            let (mut sad_u, mut sad_w) = (0u64, 0u64);
            for (&c, &rr) in cur.iter().zip(rbuf.iter()) {
                let (c, rr) = (c as i32, rr as i32);
                let rw = (((rr * lw + 32) >> 6) + lo).clamp(0, 255);
                sad_u += c.abs_diff(rr) as u64;
                sad_w += c.abs_diff(rw) as u64;
            }
            if sad_w * 100 < sad_u * 99 {
                (lw, lo)
            } else {
                (64, 0)
            }
        })
        .collect()
}

/// B-frame gate signal probe (harness surface for the bframes-v2 dispatch
/// fit): per-GOP `(bi_residual_1gap, gmc_residual, mgain, dcfrac, is_screen,
/// grain_signature)` — the same estimators the shipping gates consult, on the
/// GOP's leading frames. Frame dimensions must be MB multiples (probe use).
pub fn bframes_gate_signals(
    cfg: &EncoderConfig,
    frames: &[YuvFrame],
) -> (f64, f64, f64, f64, bool, bool) {
    let (w, h) = (cfg.width, cfg.height);
    let bi = gop_bi_residual(frames, w, h, 1);
    if frames.len() < 2 || w % 16 != 0 || h % 16 != 0 {
        return (bi, f64::INFINITY, 0.0, 0.0, false, false);
    }
    let sig = signals::FrameSignals::new(&frames[1].y, w, w / 16, h / 16, Some(&frames[0].y));
    let (mg, dc) = sig.mgain_dc();
    (
        bi,
        sig.gmc_residual(),
        mg,
        dc,
        sig.is_screen(),
        sig.grain_signature(),
    )
}

/// Scene-cut pair ratios for a frame sequence (calibration probe surface —
/// the same detector `segment_gops` consults; index `i` is the pair
/// `(frames[i], frames[i+1])`).
pub fn scene_cut_ratios(cfg: &EncoderConfig, frames: &[YuvFrame]) -> Vec<f64> {
    lookahead::all_pair_ratios(cfg, frames)
}

/// Deterministic WORK counts (`best_part`, `mb_plan`, `mb_coded`) — the speed
/// instrument that needs no pinning. See `signals::census`.
pub fn gate_work() -> Vec<u64> {
    signals::census::work_snapshot().to_vec()
}
/// Names for [`gate_work`], same order.
pub fn gate_work_names() -> &'static [&'static str] {
    &signals::census::WORK_NAMES
}
/// Names for [`gate_census`], same order.
pub fn gate_census_names() -> &'static [&'static str] {
    &signals::census::NAMES
}
/// Zeroes the gate census.
pub fn gate_census_reset() {
    signals::census::reset()
}

/// LIVENESS tap: dump `gate,fired,seen` to `$RFF_CENSUS_CSV`, once, at the end
/// of an encode. No-op when the env var is unset.
///
/// `fired` says a gate routed a unit. `seen` says its decision site was
/// CONSULTED AT ALL — and that second number is the one no refit harness here
/// could previously read. Without it, three states are indistinguishable:
///
/// * consulted, never routed  -> the corpus lacks the content (extend it)
/// * NEVER CONSULTED          -> the path is dead (fix the configuration)
/// * routed, output unchanged -> the arm is a no-op (delete the gate)
///
/// They have opposite fixes, so collapsing them sends you to the wrong work.
/// The case that motivated this: `sub8_grain` sits behind `num_refs == 1`, so
/// an audit run at `--refs 3` measured a gate that was switched off and
/// reported it as neutral. A hand-written comment caught that one; this makes
/// it mechanical.
///
/// Why here and not `gatecheck`: that binary reads the same counters, but it
/// builds its OWN `EncoderConfig`, so its numbers describe a different encode
/// than the one a refit run is judging — which is exactly how the `--refs 3`
/// mismatch survived. This tap fires from the same process, same flags, same
/// encode the harness is measuring.
#[cfg(not(feature = "std"))]
pub fn gate_census_dump_csv() {}
#[cfg(feature = "std")]
pub fn gate_census_dump_csv() {
    use core::fmt::Write as _;
    use std::io::Write as _;
    let Ok(path) = std::env::var("RFF_CENSUS_CSV") else {
        return;
    };
    let snap = signals::census::snapshot();
    let mut s = String::from("gate,fired,seen\n");
    for (i, name) in signals::census::NAMES.iter().enumerate() {
        let _ = writeln!(s, "{name},{},{}", snap[i].0, snap[i].1);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(s.as_bytes());
    }
}

mod mvd_cost_tab;
mod params;
mod rc;
mod signals;
mod slice;

#[cfg(feature = "std")]
pub use crate::mb16::EXT_MV;

pub use crate::mb16::{ME_PROBE, MVCMP, MVCMP_FRAME};

/// Test-only surface for gating the CABAC *encoder* against the decoder's parser.
#[doc(hidden)]
pub mod cabac_enc_test {
    pub use crate::cabac::CabacEncoder;
    pub use crate::mb16::b_part_mb_type;
    pub use crate::mb16::cb_cbp;
    pub use crate::mb16::cb_mb_qp_delta;
    pub use crate::mb16::cb_mb_type_b;
    pub use crate::mb16::cb_ref_idx;
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;
}
pub use config::{EncoderConfig, LookaheadMode, MemoryEstimate, Preset};
pub use params::{Pps, Sps};
pub use rc::RateControl;

use rusty_h264_common::{
    BitWriter, ChromaFormat, NalUnit, NalUnitType, Profile, YuvFrame, YuvPlanes,
};

/// Errors that can arise constructing or driving the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// A feature outside the implemented Constrained Baseline subset was asked for.
    Unsupported(&'static str),
    /// The supplied frame's dimensions or plane sizes don't match the config.
    FrameMismatch,
    /// The caller's output buffer cannot hold the access unit; `needed` is the
    /// size it would have taken. The frame has been coded and the encoder's
    /// state advanced (references, GOP position), so the caller treats it as a
    /// dropped picture and continues — the next call still works.
    BufferTooSmall {
        /// Bytes the access unit needed.
        needed: usize,
    },
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EncodeError::Unsupported(s) => write!(f, "unsupported: {s}"),
            EncodeError::FrameMismatch => write!(f, "frame dimensions do not match encoder config"),
            EncodeError::BufferTooSmall { needed } => {
                write!(
                    f,
                    "output buffer too small: the access unit needs {needed} bytes"
                )
            }
        }
    }
}

impl core::error::Error for EncodeError {}

/// Where an access unit goes: a `Vec` the caller receives, or the caller's own
/// buffer, written in place. Both take the same NAL bytes from the same
/// emulation-prevention scan; the slice form keeps counting past its end so a
/// [`EncodeError::BufferTooSmall`] can say exactly how much was needed.
enum Sink<'a> {
    Vec(&'a mut Vec<u8>),
    Slice {
        buf: &'a mut [u8],
        len: usize,
        needed: usize,
    },
}

impl Sink<'_> {
    /// Append one Annex-B NAL unit: start code, header, emulation-prevented payload.
    fn nal(&mut self, ref_idc: u8, nal_type: NalUnitType, rbsp: &[u8]) {
        let header = rusty_h264_common::nal::nal_header_byte(ref_idc, nal_type);
        match self {
            Sink::Vec(v) => {
                v.extend_from_slice(&[0, 0, 0, 1, header]);
                rusty_h264_common::nal::emulation_prevent_into(rbsp, v);
            }
            Sink::Slice { buf, len, needed } => {
                let mut put = |b: u8| {
                    if *len < buf.len() {
                        buf[*len] = b;
                        *len += 1;
                    }
                    *needed += 1;
                };
                for b in [0, 0, 0, 1, header] {
                    put(b);
                }
                rusty_h264_common::nal::emulation_prevent_with(rbsp, put);
            }
        }
    }
}

/// Copy an access unit into a caller-owned buffer, or say how big it needed to be.
fn copy_out(au: &[u8], out: &mut [u8]) -> Result<usize, EncodeError> {
    if out.len() < au.len() {
        return Err(EncodeError::BufferTooSmall { needed: au.len() });
    }
    out[..au.len()].copy_from_slice(au);
    Ok(au.len())
}

/// A Constrained Baseline H.264 encoder.
#[derive(Debug)]
pub struct Encoder {
    cfg: EncoderConfig,
    sps: Sps,
    pps: Pps,
    /// Count of frames fed so far; drives IDR placement via `gop_size`.
    frame_index: u32,
    /// `frame_num` of the next picture (resets to 0 at each IDR).
    next_frame_num: u32,
    /// Index of the current picture within its GOP (0 at IDR), for POC.
    gop_index: u32,
    /// Decoded-picture buffer: recent **deblocked** reconstructions (coded size),
    /// most-recent first, used as inter references (`ref_idx` 0 = front).
    refs: Vec<RefFrame>,
    /// Average-bitrate controller; `None` for constant-QP encoding.
    rc: Option<RateControl>,
    /// Per-MB QP offset for the NEXT `encode()` (mb-tree temporal AQ). Set by the
    /// batch path before each frame; consumed (and cleared) by `try_encode`. Empty /
    /// `None` → no offset (byte-identical).
    pending_qpo: Option<Vec<i32>>,
    /// AQ grain probe for the NEXT frame IF it is an IDR: the previous
    /// display-order SOURCE frame (docs/gate-ledger.md aq-grain-veto — an IDR
    /// has no coding reference, so the veto's temporal signals read
    /// source-vs-source). Set by the batch paths, consumed per frame; `None`
    /// (streaming, or the stream's first frame) → the veto fails open.
    pending_aq_probe: Option<YuvFrame>,
    /// Frames held by the streaming lookahead (mb-tree needs a whole GOP before it
    /// can assign any of its QPs). Drained a GOP at a time by `try_encode`, and at
    /// end of stream by `flush`.
    la_queue: Vec<YuvFrame>,
    /// Frames coded since the last IDR (0 = the next frame IS an IDR). The
    /// scenecut counter that replaced `frame_index % gop_size` — cadence is no
    /// longer periodic once cuts place IDRs (x264-parity keyint/min-keyint).
    since_idr: u32,
    /// One-shot IDR request (scene cut, or a batch segment boundary). Consumed
    /// by `encode_direct`.
    force_idr: bool,
    /// A caller's [`request_keyframe`](Self::request_keyframe), pending until the
    /// next submitted frame. Kept apart from `force_idr`, which a scene cut
    /// raises for the FIRST buffered frame: a lookahead drain must spend that
    /// one and not this one.
    keyframe_requested: bool,
    /// Previous display-order SOURCE frame, retained by the streaming path for
    /// the causal scene-cut pair AND as the cut-IDR's AQ grain probe. `None`
    /// with scenecut off (no clone cost on the anchor path).
    last_src: Option<YuvFrame>,
    /// `last_src`'s detector preparation (coded + half-res planes), carried so
    /// each pair ratio preps only the NEW frame — the previous frame was
    /// prepped by the last call (`None` on the first pair; rebuilt on demand).
    last_prep: Option<mbtree::PairPrep>,
    /// The two most recent scene-cut pair ratios (the spike-rule baseline),
    /// most recent first. `1.0` = no history yet (nothing can spike over it).
    cut_hist: [f64; 2],
    /// Scratch for a padded (non-tight) [`YuvPlanes`] view: rows are gathered
    /// here once per call and the frame is reused, never reallocated.
    gather: Option<YuvFrame>,
    /// The per-frame slice bit writer, kept across frames so its buffer is
    /// allocated once (`BitWriter::clear` keeps the capacity).
    bw: BitWriter,
    /// SPS and PPS as NAL units, built once; every IDR writes them.
    sps_nal: NalUnit,
    pps_nal: NalUnit,
}

#[cfg(feature = "std")]
fn panicking() -> bool {
    std::thread::panicking()
}
#[cfg(not(feature = "std"))]
fn panicking() -> bool {
    false
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // Dropping with frames still buffered means the caller never flushed and has
        // silently lost the tail of its stream. Loud in debug, free in release.
        debug_assert!(
            self.la_queue.is_empty() || panicking(),
            "Encoder dropped with {} frame(s) still in the lookahead queue — call flush()",
            self.la_queue.len()
        );
    }
}

/// A reference picture: deblocked reconstruction at coded (MB-grid) resolution.
/// Stored now (4a); read by motion compensation in 4b.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    // 16-byte aligned (moved from the encoder's aligned rec planes) so the openh264
    // MC asm can load aligned reference row chunks.
    pub y: rusty_h264_common::aligned::AlignedBytes,
    pub u: rusty_h264_common::aligned::AlignedBytes,
    pub v: rusty_h264_common::aligned::AlignedBytes,
    /// Picture Order Count — the DISPLAY position. B ref-lists order L0/L1 by POC
    /// relative to the current picture; P ignores it.
    pub poc: i32,
    /// The picture's `frame_num` (reference frames only advance it).
    pub frame_num: u32,
    /// Per-4×4-block List-0 motion (raster, `mb_w*4` wide). Populated for anchors;
    /// read as the co-located picture (`RefPicList1[0]`) when deriving a B-frame's
    /// spatial-direct `colZeroFlag`. `ref_idx == -1` marks intra/uncoded blocks.
    pub mv: Vec<(i32, i32)>,
    pub ref_idx: Vec<i32>,
    /// List-1 motion of the picture (b-pyramid: a REFERENCE B can be the
    /// co-located picture, and its L1-only blocks read from here — the exact
    /// List-1 colZeroFlag defect the decoder already root-caused and fixed;
    /// the encoder's direct derivation must mirror it or pyramid recon
    /// drifts). EMPTY for P/I references (no List 1 exists there).
    pub mv1: Vec<(i32, i32)>,
    pub ref_idx1: Vec<i32>,
    /// Blocks-wide (`mb_w*4`), so the co-located index is `by*w4 + bx`.
    pub w4: usize,
    /// Cached half-pel luma planes, built on first sub-pel motion-search use.
    ///
    /// ENCODER-SIDE ONLY, and lazily: the motion search makes ~300 `mc_luma` calls
    /// per macroblock while final reconstruction makes ~1, so this pays enormously
    /// in the search and would be pure tax anywhere else. `Arc` so cloning a
    /// `RefFrame` (the DPB does) does not copy three frame-sized planes.
    pub hpel: HpelOnce<alloc::sync::Arc<rusty_h264_common::inter::HpelPlanes>>,
}

/// The once-cell behind a reference frame's half-pel planes: `OnceLock` with
/// `std` (frames cross threads in the parallel GOP path), `OnceCell` without.
#[cfg(feature = "std")]
pub type HpelOnce<T> = rusty_h264_common::once::OnceLock<T>;
/// See the `std` variant.
#[cfg(not(feature = "std"))]
pub type HpelOnce<T> = core::cell::OnceCell<T>;

impl RefFrame {
    /// The half-pel planes for this picture, filtering them once on first use.
    pub(crate) fn hpel(&self, cw: usize, ch: usize) -> &rusty_h264_common::inter::HpelPlanes {
        self.hpel.get_or_init(|| {
            alloc::sync::Arc::new(rusty_h264_common::inter::build_hpel_planes(&self.y, cw, ch))
        })
    }
}

/// Sets the sub-pel refinement pattern (U1) for subsequent encodes in this process.
/// 0 = 8-point ring + iterate, 1 = 4-point diamond + iterate, 2 = 8-point single
/// pass, 3 = 4-point single pass. Exposed so the pattern can be A/B'd inside ONE
/// binary, which is the only comparison this machine can resolve.
/// Enables/disables the U1 online sub-pel dispatcher for subsequent encodes.
/// Sets the λ-normalised partition-split search threshold (U2). 0 = off.
/// Enables the U5-struct deferred sub-pel refinement (search all partition shapes at
/// full-pel, refine only the winner). Bitstream-changing → BD-gated.
/// Descent B: ME cost-path census [interior-fullpel, edge-fullpel, sub-pel].
#[cfg(feature = "profile")]
pub fn satdpath_snapshot() -> Vec<u64> {
    crate::mb16::satdpath::snapshot()
}
#[cfg(not(feature = "profile"))]
pub fn satdpath_snapshot() -> Vec<u64> {
    Vec::new()
}
#[cfg(feature = "profile")]
pub fn satdpath_reset() {
    crate::mb16::satdpath::reset()
}
#[cfg(not(feature = "profile"))]
pub fn satdpath_reset() {}

/// Descent D-2: sub-pel evaluations that re-price an already-priced MV.
#[cfg(feature = "profile")]
pub fn spstats_redundant() -> u64 {
    crate::mb16::spstats::redundant_count()
}
#[cfg(not(feature = "profile"))]
pub fn spstats_redundant() -> u64 {
    0
}

/// Descent D: sub-pel ring census (profile builds only).
#[cfg(feature = "profile")]
pub fn spstats_snapshot() -> (Vec<u64>, Vec<u64>) {
    crate::mb16::spstats::snapshot()
}
#[cfg(not(feature = "profile"))]
pub fn spstats_snapshot() -> (Vec<u64>, Vec<u64>) {
    (Vec::new(), Vec::new())
}
#[cfg(feature = "profile")]
pub fn spstats_reset() {
    crate::mb16::spstats::reset()
}
#[cfg(not(feature = "profile"))]
pub fn spstats_reset() {}

/// Default diamond rung mask (`[16,8,4]`).
pub const DIA_DEFAULT_MASK: u32 = crate::mb16::DIA_DEFAULT;

/// Descent A: select which rungs of the [64,32,16,8,4] diamond ladder to walk.
pub fn set_dia_mask(m: u32) {
    crate::mb16::set_dia_mask(m)
}
/// Track-B B2: SAD-domain full-pel search phase (SATD from sub-pel on) — x264's
/// cost split. Bitstream-changing; BD-gated; off = byte-identical to pre-B2.
pub fn set_me_sadfp(on: bool) {
    crate::mb16::set_me_sadfp(on)
}
/// B2 mode: 0 off, 1 dispatched per frame by the `b2_mgain` probe, 2 force-on.
pub fn set_me_sadfp_mode(m: u32) {
    crate::mb16::set_me_sadfp_mode(m)
}
/// Fixed-centre batched diamond passes (both cost domains). Off = cascade.
pub fn set_me_fc(on: bool) {
    crate::mb16::set_me_fc(on)
}
/// H-13 split-dispatch threshold in milli-units of the mgain probe (0 = always
/// search splits, byte-identical to pre-gate). Default 30 (= 0.03).
pub fn set_split_mg(milli: u32) {
    crate::mb16::set_split_mg(milli)
}
/// H-23: smooth (x264-shape) mvd cost model in ME. Off = Exp-Golomb step fn.
pub fn set_mv_smooth(on: bool) {
    crate::mb16::set_mv_smooth(on)
}
/// H-24 mv-cost mode: 0 off, 1 dispatched per frame by mgain, 2 force-on.
pub fn set_mv_smooth_mode(m: u32) {
    crate::mb16::set_mv_smooth_mode(m)
}
/// Fixed-centre batched HALF-PEL sub-pel ring (satd_x4p). Off = cascade.
pub fn set_sp_fc(on: bool) {
    crate::mb16::set_sp_fc(on)
}

/// The x264-style SUB-PEL EFFORT LADDER (H-10): one level selects a priced
/// (ring pattern × iteration budget) rung — closing the ~24-vs-9 eval-count gap
/// vs x264 as a BUDGET choice instead of a blanket cut.
///
/// 5 = ring8, iterate to convergence (the quality preset's default — max effort);
/// 4 = ring8, ≤3 iterations/step; 3 = ring8, ≤2 iterations/step;
/// 2 = ring8, single pass (= today's balanced preset); 1 = ring4, single pass.
/// Levels ≥5 restore the defaults. Equivalent env knobs: `RFF_SUBPEL_PAT` +
/// `RFF_SP_MAXIT`.
pub fn set_subme(level: u32) {
    let (pat, cap) = match level {
        1 => (3, 0),
        2 => (2, 0),
        3 => (0, 2),
        4 => (0, 3),
        _ => (0, 0),
    };
    set_subpel_pattern(pat);
    crate::mb16::set_sp_maxit(cap);
}

/// The SUPERFAST-CLASS rung (H-11/H-12): the Quality preset at x264 superfast's
/// partition SHAPE — P16×16-only (splits gated off), everything else (sub-pel
/// ladder, B2 dispatch) at defaults. Measured fair-run on foreman: **1.81× faster
/// than default quality and STILL −0.9% BD vs x264 superfast itself.** The
/// further effort cuts (subme 2 + SAD-fp force) were measured and REJECTED from
/// this rung: no speed on top of shape-only (0.27× vs 0.28×) while costing BD
/// (+1.9% foreman / +8.4% bus) — compose them manually via `set_subme` /
/// `set_me_sadfp_mode` if wanted. Split-heavy content (bus-class) pays more at
/// this rung; the per-frame split DISPATCH (H-11 next-brick b) is the eventual
/// no-tax answer. Env twin: `RFF_SPLIT_T=10000000`.
pub fn set_turbo(on: bool) {
    set_split_t(if on { 10_000_000 } else { 0 });
}
/// Track-B B3: sub-pel iteration budget (0 = unlimited = byte-identical) — the
/// bounded walk x264's subme levels have; pairs with B2. BD-gated.
pub fn set_sp_maxit(n: u32) {
    crate::mb16::set_sp_maxit(n)
}

/// Descent A: diamond per-step evaluation census (profile builds only).
#[cfg(feature = "profile")]
pub fn diastats_snapshot() -> Vec<(u64, u64)> {
    crate::mb16::diastats::snapshot()
}
#[cfg(not(feature = "profile"))]
pub fn diastats_snapshot() -> Vec<(u64, u64)> {
    Vec::new()
}
#[cfg(feature = "profile")]
pub fn diastats_reset() {
    crate::mb16::diastats::reset()
}
#[cfg(not(feature = "profile"))]
pub fn diastats_reset() {}

pub fn set_defer_subpel(on: bool) {
    crate::mb16::DEFER_SUBPEL.store(
        if on { 1 } else { 0 },
        core::sync::atomic::Ordering::Relaxed,
    );
}

pub fn set_split_t(t: u32) {
    crate::mb16::SPLIT_T.store(t, core::sync::atomic::Ordering::Relaxed);
}

pub fn set_subpel_dispatch(on: bool) {
    crate::mb16::SP_DISPATCH.store(
        if on { 1 } else { 0 },
        core::sync::atomic::Ordering::Relaxed,
    );
}

pub fn set_subpel_pattern(p: u32) {
    crate::mb16::SUBPEL_PAT.store(p, core::sync::atomic::Ordering::Relaxed);
}

impl Encoder {
    /// Creates an encoder, validating that the configuration is within the
    /// implemented subset.
    pub fn new(cfg: EncoderConfig) -> Result<Self, EncodeError> {
        if !matches!(
            cfg.profile,
            Profile::ConstrainedBaseline | Profile::Baseline | Profile::Main | Profile::High
        ) {
            return Err(EncodeError::Unsupported("unsupported profile"));
        }
        // The 8x8 transform is a High-profile feature, available under BOTH entropy
        // coders since R6: `transform_size_8x8_flag` at both of its syntax positions
        // (ctxIdxOffset 399) plus the ctxBlockCat-5 residual writer (sig 402 / last
        // 417 / levels 426, and NO coded_block_flag -- presence comes from
        // CodedBlockPatternLuma).
        //
        // CLAMPED, not refused. It is default-ON, so a caller who narrows the profile
        // to Main is asking for Main-compatible output, not asking for an error -- and
        // refusing here would make `EncoderConfig::new()` plus `profile = Main`, an
        // entirely reasonable pair, fail outright. A profile is a compatibility
        // ceiling; High-only tools clamp to it, exactly as `-profile:v` behaves
        // elsewhere. The CLI promotes to High whenever `--transform-8x8 1` is passed,
        // so an explicit request is never silently dropped there.
        let mut cfg = cfg;
        if cfg.transform_8x8 && !matches!(cfg.profile, Profile::High) {
            cfg.transform_8x8 = false;
        }
        // B-frames are illegal in Baseline / Constrained Baseline (the decoder
        // enforces this too). HIGH is a superset of Main and permits B slices —
        // this used to demand Main exactly, which rejected the perfectly legal
        // High + B-frames combination and blocked the 8x8 + B measurement.
        // R6-5: 8x8 + B-frames. The B rule for transform_size_8x8_flag is derived
        // in `plan_inter_mb` (`allow_t8`) and mirrored at the B emit: with
        // direct_8x8_inference_flag = 0, a B_Direct_16x16 macroblock may not carry
        // the flag, so the plan must not pick 8x8 for one. Gating at PLAN time (not
        // emit time) is what keeps our reconstruction and the decoder's in step.
        if cfg.bframes > 0 && !matches!(cfg.profile, Profile::Main | Profile::High) {
            return Err(EncodeError::Unsupported(
                "B-frames require Main or High profile",
            ));
        }
        if cfg.chroma != ChromaFormat::Yuv420 {
            return Err(EncodeError::Unsupported("only 4:2:0 chroma"));
        }
        if cfg.width == 0 || cfg.height == 0 || cfg.width % 2 != 0 || cfg.height % 2 != 0 {
            return Err(EncodeError::Unsupported(
                "dimensions must be positive and even",
            ));
        }
        let sps = Sps::from_config(&cfg);
        let pps = Pps::from_config(&cfg);
        let rc = (cfg.bitrate > 0).then(|| RateControl::new(cfg.bitrate, cfg.framerate, cfg.qp));
        let (cfg_w, cfg_h) = (cfg.width, cfg.height);
        let sps_nal = sps.to_nal();
        let pps_nal = pps.to_nal();
        Ok(Self {
            cfg,
            sps,
            pps,
            frame_index: 0,
            next_frame_num: 0,
            gop_index: 0,
            refs: Vec::new(),
            rc,
            pending_qpo: None,
            pending_aq_probe: None,
            la_queue: Vec::new(),
            since_idr: 0,
            force_idr: false,
            keyframe_requested: false,
            last_src: None,
            last_prep: None,
            cut_hist: [1.0, 1.0],
            gather: None,
            // Pre-size the slice writer to a generous fraction of the raw frame so
            // the CAVLC hot loop never reallocs mid-frame (byte-identical; just capacity).
            bw: BitWriter::with_capacity(cfg_w * cfg_h / 2 + 4096),
            sps_nal,
            pps_nal,
        })
    }

    /// Sets the per-MB QP offset applied to the NEXT [`encode`](Self::encode) call
    /// (mb-tree temporal AQ). One entry per macroblock (raster). Consumed once.
    pub(crate) fn set_pending_qpo(&mut self, qpo: Vec<i32>) {
        self.pending_qpo = Some(qpo);
    }

    /// Sets the AQ grain probe (the previous display-order SOURCE frame) for the
    /// NEXT call if it codes an IDR. Consumed once per frame either way.
    pub(crate) fn set_aq_probe(&mut self, f: YuvFrame) {
        self.pending_aq_probe = Some(f);
    }

    /// The active configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Encodes one frame, returning zero or more Annex-B access units. IDR
    /// placement follows the keyint model (`gop_size` ceiling, `min_keyint`,
    /// scene cuts), each IDR prefixed with SPS/PPS.
    ///
    /// With a lookahead feature active (mb-tree, on by default) frames buffer
    /// until a window fills, so a call may return EMPTY bytes — call
    /// [`flush`](Self::flush) at end of stream to drain the tail.
    pub fn encode(&mut self, frame: &YuvFrame) -> Vec<u8> {
        self.try_encode(frame).expect("frame matched config")
    }

    /// [`encode`](Self::encode) over a **borrowed** frame — the camera's DMA
    /// buffer, not a copy. A tight view feeds the coder directly; a padded one
    /// is gathered into a reused scratch frame first. Byte-identical to
    /// `encode(&view.to_frame())`.
    pub fn encode_planes(&mut self, frame: &YuvPlanes<'_>) -> Result<Vec<u8>, EncodeError> {
        if frame.width != self.cfg.width || frame.height != self.cfg.height {
            return Err(EncodeError::FrameMismatch);
        }
        if frame.is_valid() {
            return self.try_encode_planes(frame);
        }
        let mut g = self
            .gather
            .take()
            .unwrap_or_else(|| YuvFrame::black(frame.width, frame.height));
        frame.copy_into(&mut g);
        let r = self.try_encode_planes(&g.as_planes());
        self.gather = Some(g);
        r
    }

    /// [`encode_planes`](Self::encode_planes) into a caller-owned buffer — the
    /// packetizer's, on a chip — returning the access unit's length.
    ///
    /// An access unit that does not fit is
    /// [`EncodeError::BufferTooSmall`] with the size it needed; the picture is
    /// then lost (the encoder has already coded it and moved on) and the next
    /// call still works, so size the buffer for the worst case: an IDR at low
    /// QP can approach `width * height * 3 / 2`. Bytes are identical to
    /// [`encode_planes`].
    pub fn encode_into(
        &mut self,
        frame: &YuvPlanes<'_>,
        out: &mut [u8],
    ) -> Result<usize, EncodeError> {
        if frame.width != self.cfg.width || frame.height != self.cfg.height {
            return Err(EncodeError::FrameMismatch);
        }
        // A configuration that buffers (lookahead) or looks at frame pairs
        // (scene cut) hands out access units on its own schedule; those go
        // through the `Vec` path and are copied. The chip configuration —
        // `baseline()`: no lookahead, no scene cut — is written in place:
        // no access-unit `Vec`, no second copy, the slice writer's buffer
        // reused from frame to frame.
        let buffered = self.lookahead_active() || (self.cfg.scenecut > 0 && self.cfg.gop_size > 1);
        if buffered {
            let au = self.encode_planes(frame)?;
            return copy_out(&au, out);
        }
        if core::mem::take(&mut self.keyframe_requested) {
            self.force_idr = true;
        }
        let mut sink = Sink::Slice {
            buf: out,
            len: 0,
            needed: 0,
        };
        if frame.is_valid() {
            self.encode_direct_into(frame, &mut sink)?;
        } else {
            let mut g = self
                .gather
                .take()
                .unwrap_or_else(|| YuvFrame::black(frame.width, frame.height));
            frame.copy_into(&mut g);
            let r = self.encode_direct_into(&g.as_planes(), &mut sink);
            self.gather = Some(g);
            r?;
        }
        match sink {
            Sink::Slice { len, needed, .. } if needed > len => {
                Err(EncodeError::BufferTooSmall { needed })
            }
            Sink::Slice { len, .. } => Ok(len),
            Sink::Vec(_) => unreachable!("the direct path writes to the slice sink"),
        }
    }

    /// [`flush`](Self::flush) into a caller-owned buffer; see
    /// [`encode_into`](Self::encode_into) for the buffer contract.
    pub fn flush_into(&mut self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let tail = self.try_flush()?;
        copy_out(&tail, out)
    }

    /// Make the next picture an IDR (a late joiner on the mesh needs one now,
    /// not at the next GOP boundary). Rate-control state, the frame counter and
    /// the scene-cut history survive; only the DPB restarts, as at any IDR.
    /// With a lookahead active the buffered frames are coded first and the IDR
    /// lands on the next frame *submitted*, in coding order.
    pub fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    /// Fallible [`encode`](Self::encode).
    ///
    /// With a lookahead feature active (currently mb-tree, on by default in the
    /// constant-QP path) this BUFFERS: mb-tree needs a whole GOP of future frames
    /// before it can assign any of their QPs, so the returned `Vec` is empty while
    /// the GOP fills and then carries that entire GOP's access units at once. The
    /// concatenation of every return value plus [`flush`](Self::flush) is exactly
    /// what [`encode_all`](Self::encode_all) produces — byte for byte.
    ///
    /// **You must call [`flush`](Self::flush) at end of stream** or the final
    /// partial GOP is never emitted. (A debug build asserts if the encoder is
    /// dropped with frames still buffered.) For zero added latency set
    /// `cfg.mbtree = false`, which restores one-AU-per-call behaviour.
    pub fn try_encode(&mut self, frame: &YuvFrame) -> Result<Vec<u8>, EncodeError> {
        self.try_encode_planes(&frame.as_planes())
    }

    /// The streaming entry over a tight view. Every `encode*` lands here.
    fn try_encode_planes(&mut self, frame: &YuvPlanes<'_>) -> Result<Vec<u8>, EncodeError> {
        // A keyframe request with frames still buffered: code them first (they
        // predate the request; a scene-cut IDR pending for the first of them
        // is spent by that drain, as it should be), then the request becomes
        // THIS frame's forced IDR.
        if core::mem::take(&mut self.keyframe_requested) {
            let mut out = if self.la_queue.is_empty() {
                Vec::new()
            } else {
                self.try_flush()?
            };
            self.force_idr = true;
            out.extend_from_slice(&self.try_encode_planes(frame)?);
            return Ok(out);
        }
        // CAUSAL scene-cut detection (x264 keyint parity), BEFORE any
        // buffering: the pair is (previous source, this source) — exactly the
        // pair `segment_gops` reads in the batch path, so streaming == batch
        // holds under cuts. On a cut: flush whatever the lookahead holds (an
        // mb-tree window must not straddle an IDR), then request the IDR and
        // hand the detector's retained frame over as the grain probe.
        if self.cfg.scenecut > 0 && self.cfg.gop_size > 1 {
            if self.last_src.is_some() {
                // History advances on EVERY pair (the spike baseline must see
                // the pairs before a legal cut position too — this is what
                // keeps streaming == batch, whose `segment_gops` scores the
                // same pairs). Two exact reductions, mirroring the batch
                // scorer's rolling lazy cursor:
                //  * prep only the NEW frame — the previous frame's coded +
                //    half-res planes were built by the last call (`last_prep`);
                //  * skip the ratio entirely while no decision can read it: a
                //    decision fires at counter >= min_keyint and consults this
                //    pair's ratio for at most the two following frames, so a
                //    pair at counter < min_keyint - 2 is UNREADABLE. Its slot
                //    rolls a placeholder that is never consulted (at the
                //    earliest decision, both baseline slots came from counters
                //    >= min_keyint - 2 — computed).
                let counter = self.since_idr + self.la_queue.len() as u32;
                let minki = self.cfg.min_keyint.max(1);
                let r = if counter + 2 >= minki {
                    let cur_prep = mbtree::pair_prep(&self.cfg, frame);
                    let prev_prep = match self.last_prep.take() {
                        Some(p) => p,
                        None => mbtree::pair_prep(
                            &self.cfg,
                            &self.last_src.as_ref().expect("guarded").as_planes(),
                        ),
                    };
                    let r = mbtree::pair_ratio_prepped(&self.cfg, &cur_prep, &prev_prep);
                    self.last_prep = Some(cur_prep);
                    r
                } else {
                    self.last_prep = None;
                    1.0 // placeholder: unreadable (see above)
                };
                let (p1, p2) = (self.cut_hist[0], self.cut_hist[1]);
                self.cut_hist = [r, p1];
                if counter >= minki && lookahead::is_scene_cut(&self.cfg, r, p1, p2) {
                    let flushed = self.try_flush()?;
                    self.force_idr = true;
                    self.pending_aq_probe = self.last_src.take();
                    self.last_src = Some(frame.to_frame());
                    let mut out = flushed;
                    out.extend_from_slice(&self.try_encode_inner(frame)?);
                    return Ok(out);
                }
            }
            self.last_src = Some(frame.to_frame());
        }
        self.try_encode_inner(frame)
    }

    fn try_encode_inner(&mut self, frame: &YuvPlanes<'_>) -> Result<Vec<u8>, EncodeError> {
        if !self.lookahead_active() {
            return self.encode_direct(frame);
        }
        if frame.width != self.cfg.width || frame.height != self.cfg.height || !frame.is_valid() {
            return Err(EncodeError::FrameMismatch);
        }
        self.la_queue.push(frame.to_frame());
        // mb-tree's window: bounded by `lookahead` (x264's rc-lookahead — a
        // 250-frame keyint must not mean a 250-frame buffer) and by the
        // frames REMAINING until the forced IDR, so a window never straddles
        // an IDR and ends exactly where the batch path's segment does (the
        // streaming == batch identity under the keyint model). `since_idr %
        // gop` treats the just-completed GOP (`since_idr == gop`) as a fresh
        // one — the next frame IS the IDR.
        let gop = self.cfg.gop_size.max(1);
        let rem_to_idr = (gop - (self.since_idr % gop)) as usize;
        let window = (self.cfg.lookahead.max(1) as usize).min(rem_to_idr);
        if self.la_queue.len() >= window {
            self.emit_lookahead_gop()
        } else {
            Ok(Vec::new())
        }
    }

    /// True when a feature needs future frames, so [`try_encode`] must buffer.
    /// B-frames already refuse the streaming API, and rate control drives its own
    /// sequential path, so mb-tree in constant-QP mode is the only case.
    fn lookahead_active(&self) -> bool {
        self.cfg.mbtree && self.cfg.bframes == 0 && self.cfg.bitrate == 0
    }

    /// Codes every buffered frame with mb-tree's per-GOP QP offsets and returns
    /// their access units concatenated. Identical to what an `encode_all` worker
    /// does for the same GOP.
    fn emit_lookahead_gop(&mut self) -> Result<Vec<u8>, EncodeError> {
        let frames = core::mem::take(&mut self.la_queue);
        let offs = mbtree::gop_qp_offsets(&self.cfg, &frames, self.cfg.mbtree_strength);
        let mut out = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            if let Some(o) = offs.get(i) {
                self.pending_qpo = Some(o.clone());
            }
            out.extend_from_slice(&self.encode_direct(&f.as_planes())?);
        }
        Ok(out)
    }

    /// Emits any frames still held by the lookahead queue (end of stream).
    ///
    /// Returns the trailing access units, or empty when nothing is buffered — so it
    /// is always safe to call, including when no lookahead feature is active.
    pub fn flush(&mut self) -> Vec<u8> {
        self.try_flush()
            .expect("buffered frames matched the config when accepted")
    }

    /// Fallible [`flush`](Self::flush).
    pub fn try_flush(&mut self) -> Result<Vec<u8>, EncodeError> {
        if self.la_queue.is_empty() {
            return Ok(Vec::new());
        }
        self.emit_lookahead_gop()
    }

    /// The unbuffered single-frame path: codes `frame` immediately. This is what the
    /// batch path's workers call, since they compute the lookahead themselves.
    fn encode_direct(&mut self, frame: &YuvPlanes<'_>) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode_direct_into(frame, &mut Sink::Vec(&mut out))?;
        Ok(out)
    }

    /// [`encode_direct`](Self::encode_direct) into a [`Sink`]: the one body
    /// behind both the `Vec` and the caller-buffer paths.
    fn encode_direct_into(
        &mut self,
        frame: &YuvPlanes<'_>,
        out: &mut Sink<'_>,
    ) -> Result<(), EncodeError> {
        let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::Total);
        if frame.width != self.cfg.width || frame.height != self.cfg.height || !frame.is_valid() {
            return Err(EncodeError::FrameMismatch);
        }

        // B-frames need lookahead (a future anchor coded before the B), which the
        // one-frame-in streaming API can't provide — use `encode_all` for B.
        if self.cfg.bframes > 0 {
            return Err(EncodeError::Unsupported(
                "B-frames need encode_all (lookahead)",
            ));
        }
        // GOP placement (x264 keyint model): an IDR when forced (scene cut or
        // batch segment boundary), at stream start / after reset, or when the
        // since-IDR counter reaches `gop_size` (the keyint ceiling). The
        // counter replaced `frame_index % gop_size` — cadence is not periodic
        // once cuts place IDRs; with `scenecut = 0` the counter reproduces the
        // modulo exactly (the bisection anchor).
        let forced = core::mem::take(&mut self.force_idr);
        let is_idr = self.cfg.gop_size <= 1
            || forced
            || self.since_idr == 0
            || self.since_idr >= self.cfg.gop_size;
        if is_idr {
            self.gop_index = 0;
            self.next_frame_num = 0;
            self.refs.clear();
        }
        let frame_num = self.next_frame_num;
        let poc_lsb = (2 * self.gop_index) % 256;
        // mb-tree per-MB QP offset for this frame (empty = none / byte-identical).
        let qpo = self.pending_qpo.take().unwrap_or_default();
        // AQ grain probe (IDR only; consumed unconditionally so it cannot go stale).
        let aq_probe_owned = self.pending_aq_probe.take();
        let aq_probe = aq_probe_owned.as_ref().map(YuvFrame::as_planes);

        // Rate control (if enabled) chooses this frame's QP from a cheap
        // look-ahead complexity estimate; otherwise the QP is fixed.
        let complexity = if self.rc.is_some() {
            lookahead::complexity(
                &self.cfg,
                frame,
                if is_idr { None } else { self.refs.first() },
            )
        } else {
            0.0
        };
        let qp = match &self.rc {
            Some(rc) => rc.pick_qp(is_idr, complexity),
            // Constant-QP: apply the per-GOP I-frame cascade offset (0 by default →
            // byte-identical). Keeps the P-only path consistent with `code_picture`.
            None if is_idr => (self.cfg.qp as i32 + self.cfg.i_qp_offset).clamp(0, 51) as u8,
            None => self.cfg.qp,
        };

        // The reused slice writer: its buffer was sized once in `new`.
        let mut w = core::mem::take(&mut self.bw);
        w.clear();
        let (nal_type, mut reference) = if is_idr {
            // SPS/PPS precede every IDR so the stream is independently decodable.
            out.nal(
                self.sps_nal.ref_idc,
                self.sps_nal.nal_type,
                &self.sps_nal.rbsp,
            );
            out.nal(
                self.pps_nal.ref_idc,
                self.pps_nal.nal_type,
                &self.pps_nal.rbsp,
            );
            slice::write_idr_slice_header(&mut w, &self.cfg, qp);
            // The batch paths park the previous source frame in
            // `pending_aq_probe` (taken above); pure streaming callers have no
            // previous frame retained — the grain veto fails open there.
            let r = if self.cfg.cabac {
                mb16::encode_slice_data_cabac_intra(
                    &mut w,
                    &self.cfg,
                    frame,
                    qp,
                    &qpo,
                    aq_probe.as_ref(),
                )
            } else {
                mb16::encode_slice_data(
                    &mut w,
                    &self.cfg,
                    frame,
                    qp,
                    false,
                    &[],
                    &qpo,
                    aq_probe.as_ref(),
                    &[],
                )
            };
            (NalUnitType::IdrSlice, r)
        } else {
            // weightp (x264 parity): estimate per-reference luma weights; the
            // slice header's table and the coder's post-MC apply must be the
            // SAME list or the stream and the recon disagree.
            let wp: Vec<(i32, i32)> = if self.cfg.weightp {
                estimate_luma_weights(&self.cfg, frame, &self.refs)
            } else {
                Vec::new()
            };
            let wp_hdr = if self.cfg.weightp {
                Some(wp.as_slice())
            } else {
                None
            };
            slice::write_p_slice_header(
                &mut w,
                &self.cfg,
                qp,
                frame_num,
                poc_lsb,
                self.refs.len(),
                wp_hdr,
            );
            let r = if self.cfg.cabac {
                mb16::encode_slice_data_cabac_p(&mut w, &self.cfg, frame, qp, &self.refs, &qpo, &wp)
            } else {
                mb16::encode_slice_data(
                    &mut w, &self.cfg, frame, qp, true, &self.refs, &qpo, None, &wp,
                )
            };
            (NalUnitType::NonIdrSlice, r)
        };
        // POC/frame_num carried on the reference so B-frame ref-lists (when enabled)
        // can order L0/L1 by display position. Unused on the P-only path.
        reference.poc = 2 * self.gop_index as i32;
        reference.frame_num = frame_num;
        let slice_bytes = w.finish();
        // Feed the coded slice size (the picture's own bits) back to the controller.
        if let Some(rc) = &mut self.rc {
            rc.update(is_idr, slice_bytes.len() * 8, qp, complexity);
        }
        {
            let _g = rusty_h264_common::prof::scope(rusty_h264_common::prof::Stage::EncNal);
            out.nal(3, nal_type, slice_bytes);
        }
        self.bw = w;

        // The deblocked reconstruction enters the DPB (most-recent first), which
        // is kept to `max_num_ref_frames` by a sliding window.
        self.refs.insert(0, reference);
        self.refs.truncate(self.cfg.num_ref_frames.max(1) as usize);
        self.frame_index += 1;
        self.gop_index += 1;
        self.next_frame_num = (self.next_frame_num + 1) % 16;
        // SELF-FILL the AQ grain probe: if this encoder's NEXT frame opens a GOP,
        // retain THIS source frame as its probe (one clone per GOP, none on other
        // frames). Sequential paths (streaming, RC) get IDR coverage for free and
        // byte-match the batch workers, which set the same frame externally
        // (fresh encoder per GOP — no state to self-fill from).
        self.since_idr = if is_idr { 1 } else { self.since_idr + 1 };
        if self.cfg.gop_size > 1 && self.since_idr >= self.cfg.gop_size {
            self.pending_aq_probe = Some(frame.to_frame());
        }
        Ok(())
    }

    /// Batch-encodes every frame, returning one Annex-B access unit per frame.
    ///
    /// At constant QP the GOPs are independent — each begins with an IDR that
    /// resets the DPB, `frame_num` and POC, and SPS/PPS precede every IDR — so they
    /// are encoded **in parallel across CPU cores** and the result is
    /// **byte-identical** to calling [`encode`](Self::encode) frame-by-frame. With
    /// rate control enabled the per-frame QP depends on history, so this falls back
    /// to sequential encoding. Within a GOP, P-frames are inherently sequential
    /// (each predicts from the previous reconstruction); the parallelism is across
    /// GOPs, so it scales with the number of GOPs in the clip.
    pub fn encode_all(&self, frames: &[YuvFrame]) -> Result<Vec<Vec<u8>>, EncodeError> {
        for f in frames {
            if f.width != self.cfg.width || f.height != self.cfg.height || !f.is_valid() {
                return Err(EncodeError::FrameMismatch);
            }
        }
        // SUB-PEL GRAIN VETO, decided ONCE for the sequence (grain is a property of the
        // SOURCE, not of a frame). Sub-pel interpolates, and on grain it interpolates
        // NOISE. `RFF_GRAIN_SUBPEL=0` opts out.
        let grain_seq = self.cfg.preset != Preset::Fast
            && rusty_h264_common::knob("RFF_GRAIN_SUBPEL")
                .map(|v| v != "0")
                .unwrap_or(true)
            && frames.len() >= 2
            && {
                // CODED-size planes, not the raw display planes: FrameSignals
                // walks the MB grid (`mb_h*16` rows), and a display height
                // that is not a multiple of 16 (1080p!) is SHORTER than that —
                // this exact line crashed on blue_sky_1080p the first time a
                // non-MB-multiple clip went through `encode_all` (pre-existing;
                // surfaced by the bframes-v2 holdout run).
                let cl0 = mbtree::coded_luma(&self.cfg, &frames[0].as_planes());
                let cl1 = mbtree::coded_luma(&self.cfg, &frames[1].as_planes());
                crate::signals::FrameSignals::new(
                    &cl1,
                    self.cfg.mb_width() * 16,
                    self.cfg.mb_width(),
                    self.cfg.mb_height(),
                    Some(&cl0),
                )
                .grain_signature()
            };
        let _guard = mb16::SeqFastPath::set(grain_seq);
        // B-frames need a reorder pipeline (code the future anchor before the B's
        // that reference it) — a separate sequential path.
        if self.cfg.bframes > 0 {
            // Content-adaptive dispatch, PER GOP (codec-content-adaptive-dispatch):
            // code B-frames only in GOPs whose motion is predictable enough to pay,
            // so a mixed clip gets B on its smooth segments and P on its busy ones.
            // Scene-cut segmentation first (x264 keyint model) — every per-GOP
            // signal below is per-SEGMENT, so the dispatch decides on real
            // scene units instead of arbitrary fixed windows.
            let seg_starts = lookahead::segment_gops(&self.cfg, frames);
            let seg_range = |k: usize| {
                (
                    seg_starts[k],
                    seg_starts.get(k + 1).copied().unwrap_or(frames.len()),
                )
            };
            let n_gops = seg_starts.len();
            let (w, h) = (self.cfg.width, self.cfg.height);
            // One cheap per-GOP signal drives BOTH content-adaptive knobs: the B/P
            // structure dispatch AND the I-frame QP-cascade depth.
            let gop_sig: Vec<f64> = (0..n_gops)
                .map(|g| {
                    let (s, e) = seg_range(g);
                    gop_bi_residual(&frames[s..e], w, h, 1)
                })
                .collect();
            // bframes-v2 dispatch: the SEGMENT level carries only the vetoes
            // (screen content — the +1.19% leak class, whose bi-residual is
            // near zero and sails under any threshold); the bi-residual
            // decision itself moved to the ANCHOR-GAP level inside
            // `encode_all_bframes`, where crew-class episodic content (flash
            // gaps P, calm gaps B) is separable in a way no clip-level scalar
            // was.
            let gop_fav: Vec<bool> = if self.cfg.bframes_adaptive {
                (0..n_gops)
                    .map(|g| {
                        let (s, e) = seg_range(g);
                        if e - s < 2 {
                            return bframes_favorable(gop_sig[g]);
                        }
                        let cl0 = mbtree::coded_luma(&self.cfg, &frames[s].as_planes());
                        let cl1 = mbtree::coded_luma(&self.cfg, &frames[s + 1].as_planes());
                        let cw = self.cfg.mb_width() * 16;
                        let sig = signals::FrameSignals::new(
                            &cl1,
                            cw,
                            self.cfg.mb_width(),
                            self.cfg.mb_height(),
                            Some(&cl0),
                        );
                        !sig.is_screen()
                    })
                    .collect()
            } else {
                vec![true; n_gops]
            };
            let gop_iqp: Vec<i32> = gop_sig
                .iter()
                .map(|&s| gop_iqp_offset(s, self.cfg.i_qp_offset))
                .collect();
            let gop_bqp: Vec<i32> = gop_sig
                .iter()
                .map(|&s| gop_bframe_qp_offset(s, self.cfg.bframe_qp_offset))
                .collect();
            // Adaptive B-COUNT: how many B's per anchor gap. Fixed `bframes` unless
            // `auto`, where the 2-gap/1-gap bi-residual RATIO picks it — content that
            // survives wider anchor spacing (low ratio) carries more cheap B's; simple
            // translation (high ratio) wants a single equidistant B.
            let bcount = if self.cfg.bframes_adaptive {
                adaptive_bcount(frames, w, h, self.cfg.bframes as usize)
            } else {
                self.cfg.bframes as usize
            };
            if gop_fav.iter().any(|&f| f) {
                return Ok(self.encode_all_bframes(
                    frames,
                    bcount,
                    &seg_starts,
                    &gop_fav,
                    &gop_iqp,
                    &gop_bqp,
                ));
            }
            // No GOP is B-favorable → pure P-only (byte-identical to bframes=0).
            let mut pcfg = self.cfg.clone();
            pcfg.bframes = 0;
            return Encoder::new(pcfg)?.encode_all(frames);
        }
        // Rate control threads state across frames → it must stay sequential. mb-tree
        // runs in RC mode too: per-GOP lookahead → per-MB offsets (per-GOP centered, so
        // rate-neutral per GOP), and the controller supplies each frame's base QP.
        // (MEASURED: routing the cross-frame allocation through the RC's complexity
        // instead of centering was worse — the centered offsets carry it correctly.)
        if self.cfg.bitrate > 0 {
            let mut enc = Encoder::new(self.cfg.clone())?;
            // Same segmentation as the CQP path (scene-cut IDRs under the
            // keyint ceiling), mb-tree windowed to `lookahead` within each
            // segment; segment starts force the IDR through the controller's
            // sequential encoder.
            let seg_starts = lookahead::segment_gops(&self.cfg, frames);
            let offs: Vec<Vec<i32>> = if self.cfg.mbtree {
                seg_starts
                    .iter()
                    .enumerate()
                    .flat_map(|(k, &s)| {
                        let end = seg_starts.get(k + 1).copied().unwrap_or(frames.len());
                        frames[s..end].chunks(self.cfg.lookahead.max(1) as usize)
                    })
                    .flat_map(|w| mbtree::gop_qp_offsets(&self.cfg, w, self.cfg.mbtree_strength))
                    .collect()
            } else {
                Vec::new()
            };
            let mut next_seg = 1usize; // seg_starts[0] == 0 is the natural first IDR
            return frames
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    if seg_starts.get(next_seg) == Some(&i) {
                        enc.force_idr = true;
                        next_seg += 1;
                    }
                    if let Some(qpo) = offs.get(i) {
                        enc.pending_qpo = Some(qpo.clone());
                    }
                    // Bypass the streaming lookahead buffer: this path supplies the
                    // offsets itself, so buffering here would double-compute them.
                    enc.encode_direct(&f.as_planes())
                })
                .collect();
        }
        // Variable GOP segmentation (x264 keyint model): scene-cut driven IDRs
        // under the `gop_size` ceiling. With `scenecut = 0` this IS
        // `chunks(gop_size)` — byte-identical to the fixed-cadence encoder.
        let seg_starts = lookahead::segment_gops(&self.cfg, frames);
        let gops: Vec<&[YuvFrame]> = seg_starts
            .iter()
            .enumerate()
            .map(|(k, &s)| &frames[s..seg_starts.get(k + 1).copied().unwrap_or(frames.len())])
            .collect();
        if gops.is_empty() || frames.is_empty() {
            return Ok(Vec::new());
        }
        // Each GOP is encoded with a fresh encoder (an IDR resets all state), so
        // the GOPs are independent: with `std` they distribute across worker
        // threads, without it they run in order. Same closure, same bytes.
        let mut out: Vec<Option<Vec<Vec<u8>>>> = (0..gops.len()).map(|_| None).collect();
        let cfg = &self.cfg;
        let gops_ref = &gops;
        let encode_gop = |i: usize| -> Vec<Vec<u8>> {
            let mut enc = Encoder::new(cfg.clone()).expect("config");
            // mb-tree temporal AQ: a per-GOP lookahead over the GOP's
            // source frames yields per-frame per-MB QP offsets (the GOP
            // is the natural window — the IDR resets references). Off →
            // empty → byte-identical.
            // mb-tree windows of ≤ `lookahead` frames within
            // the segment (a 250-frame scenecut GOP must not
            // be one propagation window); aligned to the
            // segment start, exactly like the streaming path.
            let offs: Vec<Vec<i32>> = if cfg.mbtree {
                gops_ref[i]
                    .chunks(cfg.lookahead.max(1) as usize)
                    .flat_map(|w| mbtree::gop_qp_offsets(cfg, w, cfg.mbtree_strength))
                    .collect()
            } else {
                Vec::new()
            };
            // The GOP's IDR probes grain against the PREVIOUS GOP's
            // last source frame — `gops_ref` is the full shared
            // slice, so this is identical under any thread count,
            // and it is exactly the frame `encode_direct`'s
            // self-fill would have retained in a sequential run
            // (the documented streaming==batch invariant). The
            // stream's FIRST IDR fails open on every path — pure
            // streaming cannot see frame 1 at frame 0.
            if i > 0 {
                if let Some(pf) = gops_ref[i - 1].last() {
                    enc.set_aq_probe(pf.clone());
                }
            }
            let aus: Vec<Vec<u8>> = gops_ref[i]
                .iter()
                .enumerate()
                .map(|(fi, f)| {
                    if let Some(o) = offs.get(fi) {
                        enc.set_pending_qpo(o.clone());
                    }
                    enc.encode_direct(&f.as_planes())
                        .expect("frame matched config")
                })
                .collect();
            aus
        };
        #[cfg(feature = "std")]
        {
            let n = rusty_h264_common::knob("RUSTY_THREADS")
                .and_then(|v| v.parse().ok())
                .or_else(|| std::thread::available_parallelism().map(|n| n.get()).ok())
                .unwrap_or(1)
                .min(gops.len());
            let encode_gop = &encode_gop;
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..n)
                    .map(|t| {
                        s.spawn(move || {
                            // The flag is thread-local; each worker carries
                            // the caller's decision.
                            let _fast = mb16::SeqFastPath::set(grain_seq);
                            let mut local = Vec::new();
                            let mut i = t;
                            while i < gops_ref.len() {
                                local.push((i, encode_gop(i)));
                                i += n;
                            }
                            local
                        })
                    })
                    .collect();
                for h in handles {
                    for (i, aus) in h.join().expect("encode worker panicked") {
                        out[i] = Some(aus);
                    }
                }
            });
        }
        #[cfg(not(feature = "std"))]
        {
            for i in 0..gops.len() {
                out[i] = Some(encode_gop(i));
            }
        }
        Ok(out.into_iter().flatten().flatten().collect())
    }

    /// B-frame reorder pipeline (sequential). Produces access units in **coding
    /// order** (the decoder reorders to display order by POC). Structure: an IDR
    /// at each `gop_size` boundary, a P anchor every `bframes+1` frames within a
    /// GOP, `bframes` non-reference B-frames between consecutive anchors, and the
    /// last frame forced to an anchor so trailing B's always have a future
    /// reference. Each anchor is coded before the B's that reference it.
    /// `gop_favorable[g]` (content-adaptive): GOP `g` codes B-frames only when
    /// `true`; a `false` GOP is coded all-P (every frame an anchor) so busy segments
    /// of a mixed clip don't regress. Non-adaptive callers pass all-`true`.
    fn encode_all_bframes(
        &self,
        frames: &[YuvFrame],
        bcount: usize,
        seg_starts: &[usize],
        gop_favorable: &[bool],
        gop_iqp: &[i32],
        gop_bqp: &[i32],
    ) -> Vec<Vec<u8>> {
        let n = frames.len();
        if n == 0 {
            return Vec::new();
        }
        let step = bcount.max(1) + 1; // B's per anchor gap + 1 (adaptive in `auto`)
                                      // Per-display-index segment map (variable GOPs under scenecut; with
                                      // scenecut=0 the segments are the old fixed `gop_size` chunks and every
                                      // derived quantity below reproduces the old `% gop` arithmetic).
        let mut seg_of = vec![0usize; n];
        let mut seg_start_of = vec![0usize; n];
        {
            let mut k = 0usize;
            for d in 0..n {
                if k + 1 < seg_starts.len() && seg_starts[k + 1] == d {
                    k += 1;
                }
                seg_of[d] = k;
                seg_start_of[d] = seg_starts[k];
            }
        }
        // A B-capable config: Main profile + ≥2 refs so the DPB holds both anchors.
        let mut cfg = self.cfg.clone();
        cfg.num_ref_frames = cfg.num_ref_frames.max(if cfg.b_pyramid { 3 } else { 2 }); // pyramid: 2 anchors + 1 B-ref must fit the DPB
        let sps = Sps::from_config(&cfg);
        let pps = Pps::from_config(&cfg);

        // Anchor display-indices: IDR at GOP starts, P anchors every `step`, plus
        // the frame right before each IDR boundary and the clip's last frame — a
        // trailing B with no future reference IN ITS OWN GOP would otherwise be
        // coded after the next GOP's IDR (which clears the DPB), losing its anchors.
        let mut is_anchor = vec![false; n];
        // Segment-OUTER derivation: the favorability lookup, segment start and
        // next-boundary test hoist to once per SEGMENT (each was a bounds-
        // checked lookup per frame), and `off % step` — an integer divide per
        // frame, `step` being runtime-valued — becomes a rolling phase counter
        // (`phase == 0` exactly when `(d - seg_start) % step == 0`). A
        // non-favorable GOP is coded all-P (every frame an anchor); a
        // favorable one uses the B structure.
        for (k, &s) in seg_starts.iter().enumerate() {
            let e = seg_starts.get(k + 1).copied().unwrap_or(n).min(n);
            if !gop_favorable.get(k).copied().unwrap_or(true) {
                is_anchor[s..e].fill(true);
                continue;
            }
            // `d + 1 == next segment start` marked the frame right before each
            // IDR boundary; the clip's own end is handled by the line below.
            let boundary = k + 1 < seg_starts.len();
            let mut phase = 0usize;
            for d in s..e {
                is_anchor[d] = phase == 0 || (boundary && d + 1 == e);
                phase += 1;
                if phase == step {
                    phase = 0;
                }
            }
        }
        is_anchor[n - 1] = true;
        // bframes-v2: PER-GAP favorability (adaptive mode only). Each anchor
        // gap is priced by its OWN bi-prediction residual; an unfavorable gap
        // codes all-P while its neighbours keep their B's — episodic content
        // (crew's camera flashes, one busy passage of a calm clip) dispatches
        // at the scale the phenomenon actually has. Fixed `--bframes N` keeps
        // x264's flat structure.
        if self.cfg.bframes_adaptive {
            let (w, h) = (self.cfg.width, self.cfg.height);
            // Frame means, MEMOIZED across pairs and gaps: consecutive pairs
            // share a frame and adjacent gaps share their anchor, so the eager
            // per-pair closure computed every interior mean twice (and anchor
            // means once per adjoining gap). NaN = not yet computed; the
            // sample count is `ceil(len/64)` — exactly what the counting loop
            // produced. Same sums, same divide: BIT-IDENTICAL, and the
            // short-circuiting `any` still computes no mean it never reads.
            fn memo_mean(frames: &[YuvFrame], means: &mut [f64], x: usize) -> f64 {
                if means[x].is_nan() {
                    let f = &frames[x];
                    let mut s = 0u64;
                    let mut i = 0;
                    while i < f.y.len() {
                        s += f.y[i] as u64;
                        i += 64;
                    }
                    let c = f.y.len().div_ceil(64) as u64;
                    means[x] = s as f64 / c.max(1) as f64;
                }
                means[x]
            }
            let mut means = vec![f64::NAN; n];
            let mut a = 0usize;
            while a + 1 < n {
                if !is_anchor[a] {
                    a += 1;
                    continue;
                }
                // The gap = frames (a, next_anchor); slice inclusive of both ends.
                let mut b = a + 1;
                while b < n && !is_anchor[b] {
                    b += 1;
                }
                if b > a + 1 && b < n {
                    // FLASH VETO, per pair: a camera flash is a global DC jump
                    // between adjacent frames — B-averaging across it blends
                    // two exposures (crew's +5.72% at fixed B; the bi-residual
                    // alone let its calmer flash gaps through at +3.34%). A
                    // subsampled mean-luma delta is ~free and pair-precise in
                    // a way no clip-level scalar was. Gradual fades move ~1
                    // level per frame and stay far under the threshold.
                    let flash = (a..b).any(|x| {
                        (memo_mean(frames, &mut means, x) - memo_mean(frames, &mut means, x + 1))
                            .abs()
                            > 2.5
                    });
                    let res = gop_bi_residual(&frames[a..=b], w, h, 1);
                    if flash || !bframes_favorable(res) {
                        for x in a + 1..b {
                            is_anchor[x] = true;
                        }
                    }
                }
                a = b;
            }
        }

        // mb-tree temporal AQ over the ANCHOR reference chain: B-frames are
        // non-reference leaves (mb-tree offsets them at ~0 anyway), so the lookahead
        // runs over each GOP's anchor sub-sequence — the frames that actually form the
        // reference chain — and only anchors receive an offset. `mbtree_off[d]` is that
        // anchor's per-MB offset (empty for B's / when off → byte-identical).
        let mbtree_off: Vec<Vec<i32>> = if cfg.mbtree {
            let mut off = vec![Vec::new(); n];
            for (k, &s) in seg_starts.iter().enumerate() {
                let seg_end = seg_starts.get(k + 1).copied().unwrap_or(n).min(n);
                let anchors: Vec<usize> = (s..seg_end).filter(|&d| is_anchor[d]).collect();
                // Anchor chains windowed to `lookahead` (a 250-frame scenecut
                // segment's chain must not be one propagation window).
                for aw in anchors.chunks(cfg.lookahead.max(1) as usize) {
                    // Borrowed window (gop_qp_offsets_refs): no per-anchor frame
                    // deep-clones. `offs` is owned — MOVE the rows into place
                    // instead of cloning a per-MB Vec per anchor.
                    let aframes: Vec<&YuvFrame> = aw.iter().map(|&d| &frames[d]).collect();
                    let offs = mbtree::gop_qp_offsets_refs(&cfg, &aframes, cfg.mbtree_strength);
                    for (o, &d) in offs.into_iter().zip(aw.iter()) {
                        off[d] = o;
                    }
                }
            }
            off
        } else {
            Vec::new()
        };

        // b-pyramid (x264 `normal` parity): in gaps carrying 2+ B's, the
        // display-middle B becomes a REFERENCE — coded right after its future
        // anchor, entered into the DPB, so the remaining leaves bracket
        // against it (the nearest-POC L0/L1 selection finds it naturally) at
        // half the prediction distance. CABAC-path v1.
        let pyramid = cfg.b_pyramid && cfg.cabac && step >= 3;
        let mut is_bref = vec![false; n];
        // Coding order: each anchor (display order), then — pyramid — the
        // gap's reference B, then the leaf B's.
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut prev: Option<usize> = None;
        for d in 0..n {
            if !is_anchor[d] {
                continue;
            }
            order.push(d);
            if let Some(p) = prev {
                if pyramid && d - p > 2 {
                    let m = (p + d) / 2;
                    is_bref[m] = true;
                    order.push(m);
                    order.extend(((p + 1)..d).filter(|&x| x != m));
                } else {
                    order.extend((p + 1)..d);
                }
            }
            prev = Some(d);
        }

        let mut dpb: Vec<RefFrame> = Vec::new();
        let mut aus: Vec<Vec<u8>> = Vec::with_capacity(n);
        let mut frame_num: u32 = 0;
        for &d in &order {
            let is_idr = d == seg_start_of[d];
            if is_idr {
                dpb.clear();
                frame_num = 0;
            }
            let is_b = !is_anchor[d];
            let bref = is_b && is_bref[d];
            let poc = ((d - seg_start_of[d]) as i32) * 2; // POC = display position within the GOP
            let iqp = gop_iqp.get(seg_of[d]).copied().unwrap_or(cfg.i_qp_offset);
            let bqp_leaf = gop_bqp
                .get(seg_of[d])
                .copied()
                .unwrap_or(cfg.bframe_qp_offset);
            // A REFERENCE B must not take the full "quantize harder, nothing
            // depends on it" leaf offset — leaves predict FROM it. Half, like
            // x264's pyramid B-ref QP sitting between P and leaf-B.
            let bqp = if bref { (bqp_leaf + 1) / 2 } else { bqp_leaf };
            let qpo: &[i32] = mbtree_off.get(d).map(|v| v.as_slice()).unwrap_or(&[]);
            // AQ grain probe for IDRs: an IDR has no coding reference, but the AQ
            // grain veto needs a temporal signal, and the PREVIOUS SOURCE frame is
            // an even better probe than a reconstruction (no quantization in the
            // loop). The first frame of the stream has none — the veto fails open.
            let aq_probe = if is_idr && d > 0 {
                frames.get(d - 1).map(YuvFrame::as_planes)
            } else {
                None
            };
            let (au, recon) = code_picture(
                &cfg,
                &sps,
                &pps,
                &frames[d].as_planes(),
                is_idr,
                is_b,
                bref,
                poc,
                frame_num,
                &dpb,
                iqp,
                bqp,
                qpo,
                aq_probe.as_ref(),
            );
            aus.push(au);
            if !is_b {
                if let Some(r) = recon {
                    dpb.insert(0, r);
                    dpb.truncate(cfg.num_ref_frames as usize);
                }
                frame_num = (frame_num + 1) % 16;
            } else if let Some(r) = recon {
                // Reference B: into the DPB, frame_num advances (reference
                // pictures only) — mirroring the decoder's sliding window.
                dpb.insert(0, r);
                dpb.truncate(cfg.num_ref_frames as usize);
                frame_num = (frame_num + 1) % 16;
            }
        }
        aus
    }
}

/// Codes ONE picture (IDR / P anchor / B) with explicit POC + frame_num + DPB.
/// Returns the access unit and, for reference pictures, the reconstruction to add
/// to the DPB (B-frames are non-reference → `None`). `dpb` is most-recent-first.
#[allow(clippy::too_many_arguments)]
fn code_picture(
    cfg: &EncoderConfig,
    sps: &Sps,
    pps: &Pps,
    frame: &YuvPlanes<'_>,
    is_idr: bool,
    is_b: bool,
    b_is_ref: bool,
    poc: i32,
    frame_num: u32,
    dpb: &[RefFrame],
    i_qp_offset: i32,
    b_qp_offset: i32,
    qpo: &[i32],
    aq_probe: Option<&YuvPlanes<'_>>,
) -> (Vec<u8>, Option<RefFrame>) {
    let mut out = Vec::new();
    let mut w = BitWriter::with_capacity(cfg.width * cfg.height / 2 + 4096);
    let poc_lsb = (poc as u32) & 0xFF; // log2_max_pic_order_cnt_lsb = 8
                                       // Per-GOP QP cascade, both offsets content-adaptive: B-frames are non-reference →
                                       // quantize HARDER (`b_qp_offset`, deeper on very predictable GOPs); the GOP's
                                       // I-frame is the root reference → quantize FINER (`i_qp_offset`, deeper on
                                       // predictable GOPs where the I dominates the bits).
    let qp = if is_b {
        (cfg.qp as i32 + b_qp_offset).clamp(0, 51) as u8
    } else if is_idr {
        (cfg.qp as i32 + i_qp_offset).clamp(0, 51) as u8
    } else {
        cfg.qp
    };
    let (nal_type, nal_ref_idc, recon) = if is_idr {
        sps.to_nal().write_annex_b(&mut out);
        pps.to_nal().write_annex_b(&mut out);
        slice::write_idr_slice_header(&mut w, cfg, qp);
        let mut r = if cfg.cabac {
            mb16::encode_slice_data_cabac_intra(&mut w, cfg, frame, qp, qpo, aq_probe)
        } else {
            mb16::encode_slice_data(&mut w, cfg, frame, qp, false, &[], qpo, aq_probe, &[])
        };
        r.poc = poc;
        r.frame_num = frame_num;
        (NalUnitType::IdrSlice, 3u8, Some(r))
    } else if is_b {
        // B is non-reference. We signal one active reference per list: L0[0] =
        // nearest PAST anchor (highest poc < current), L1[0] = nearest FUTURE anchor
        // (lowest poc > current) — the heads of the decoder's POC-ordered B lists.
        let l0 = dpb.iter().filter(|r| r.poc < poc).max_by_key(|r| r.poc);
        let l1 = dpb.iter().filter(|r| r.poc > poc).min_by_key(|r| r.poc);
        // b-pyramid: a reference B is CABAC-path v1 (the default config); the
        // CAVLC B coder stays leaf-only and `b_is_ref` is never set for it.
        let as_ref = b_is_ref && cfg.cabac && l0.is_some() && l1.is_some();
        slice::write_b_slice_header(&mut w, cfg, qp, frame_num, poc_lsb, 1, 1, as_ref);
        let mut b_recon: Option<RefFrame> = None;
        match (l0, l1) {
            // Leaf B's are non-reference — mb-tree offsets them at 0 anyway, so
            // `qpo` is `&[]` here (the anchor reference chain carries the temporal AQ).
            (Some(l0), Some(l1)) if cfg.cabac => {
                b_recon = mb16::encode_slice_data_cabac_b(
                    &mut w,
                    cfg,
                    frame,
                    qp,
                    poc,
                    l0,
                    l1,
                    &[],
                    as_ref,
                );
            }
            (Some(l0), Some(l1)) => {
                mb16::encode_slice_data_b(&mut w, cfg, frame, qp, poc, l0, l1, &[]);
            }
            // A B with no bracketing anchor pair can't be List-0/1 coded; fall back
            // to an all-B_Skip slice (spatial-direct) so the stream stays legal.
            _ => {
                let n = cfg.mb_width() * cfg.mb_height();
                if cfg.cabac {
                    mb16::encode_all_skip_b_cabac(&mut w, cfg, qp, n);
                } else {
                    w.write_ue(n as u32);
                    w.rbsp_trailing_bits();
                }
            }
        }
        if as_ref {
            if let Some(r) = &mut b_recon {
                r.poc = poc;
                r.frame_num = frame_num;
            }
            (NalUnitType::NonIdrSlice, 2u8, b_recon)
        } else {
            (NalUnitType::NonIdrSlice, 0u8, None)
        }
    } else {
        // P anchor: L0 = the DPB (past anchors), ordered most-recent-first. Both CAVLC
        // and CABAC now code ref_idx_l0 (cb_ref_idx / parse_ref_idx_cabac), so a P slice
        // searches + signals the full DPB (`--refs N`) under either entropy coder.
        let p_dpb: &[RefFrame] = dpb;
        let wp: Vec<(i32, i32)> = if cfg.weightp {
            estimate_luma_weights(cfg, frame, p_dpb)
        } else {
            Vec::new()
        };
        let wp_hdr = if cfg.weightp {
            Some(wp.as_slice())
        } else {
            None
        };
        slice::write_p_slice_header(&mut w, cfg, qp, frame_num, poc_lsb, p_dpb.len(), wp_hdr);
        let mut r = if cfg.cabac {
            mb16::encode_slice_data_cabac_p(&mut w, cfg, frame, qp, p_dpb, qpo, &wp)
        } else {
            mb16::encode_slice_data(&mut w, cfg, frame, qp, true, dpb, qpo, None, &wp)
        };
        r.poc = poc;
        r.frame_num = frame_num;
        (NalUnitType::NonIdrSlice, 3u8, Some(r))
    };
    let slice_bytes = w.into_bytes();
    NalUnit::new(nal_ref_idc, nal_type, slice_bytes).write_annex_b(&mut out);
    (out, recon)
}

/// The B-favorability threshold on the per-GOP signal (`gop_bi_residual`): below it,
/// motion is predictable enough that B-frames pay AND the I-frame dominates the GOP's
/// bits (so it wants a deeper QP cascade); above it the GOP is busy.
const BI_THRESH: f64 = 4.0;

/// Whether a GOP's temporal residual makes B-frames pay (predictable motion).
/// B-favorability threshold on the bi-prediction residual, evaluated PER
/// ANCHOR GAP (bframes-v2). Refit 2026-08-26 from the 12-clip BD truth table:
/// the original 4.0 (shared with the QP-cascade ramps, which keep it) captured
/// only the near-static winners and left five pan/texture/noise clips worth
/// -11..-17% each on the table (mobile 7.98, shields 8.07, grain 7.61, city
/// 5.87, tempete 4.69 all WIN at fixed B); the documented fastmotion losers
/// sit above (football 15.1, park_joy 9.1, crowd_run 8.3). 8.2 splits them.
/// The per-GAP unit (not per clip) is what handles crew: its flash gaps read
/// unfavorable and code P while its calm gaps take the B win — the clip-level
/// scalars could not separate crew from tempete (0.006 apart in dcfrac, a
/// margin that is fitting noise, refused). Holdout-gated on six unseen clips.
const B_GAP_THRESH: f64 = 8.2;

fn bframes_favorable(residual: f64) -> bool {
    residual < B_GAP_THRESH
}

/// Content-adaptive per-GOP I-frame QP offset (the ip_ratio cascade, DISPATCHED by
/// content). `base` is the busy-GOP offset (`cfg.i_qp_offset`, default −3); a
/// predictable GOP — where the I-frame is a large fraction of the GOP's bits, so
/// investing in it pays outsized — gets up to 2 QP steps FINER, ramping from `base`
/// at the threshold to `base−2` at residual 0. Calibrated: busy ≈ −3, compressible
/// ≈ −5 (−11.6% vs −7.3% at −3). `base == 0` (the opt-out) disables it entirely so
/// the byte-identical escape hatch survives.
fn gop_iqp_offset(residual: f64, base: i32) -> i32 {
    if base == 0 {
        return 0;
    }
    let bonus =
        rusty_h264_common::fmath::round(2.0 * ((BI_THRESH - residual) / BI_THRESH).clamp(0.0, 1.0))
            as i32;
    base - bonus
}

/// Content-adaptive per-GOP B-frame QP offset. B-frames are non-reference, so on a
/// VERY predictable GOP (bi-pred + spatial-direct nail them → tiny residual) they can
/// be quantized much HARDER for near-free bits. But the optimum is KNIFE-EDGE in the
/// signal — measured ~+8 at residual 0.10 yet ~+2 by residual 0.29 (and a heavy LOSS
/// at +12 there) — so unlike the I-cascade this ramp is STEEP and confined to the
/// near-perfect-motion regime: `base` (default +2) everywhere, boosted up to +4 only
/// as residual → 0 (decaying to `base` by ~0.3/px). Deliberately conservative — it
/// helps near-static / clean-pan content and must never touch the common range.
fn gop_bframe_qp_offset(residual: f64, base: i32) -> i32 {
    const RAMP: f64 = 0.3; // residual above this gets no boost (steep — see calibration)
    let boost =
        rusty_h264_common::fmath::round(4.0 * ((RAMP - residual) / RAMP).clamp(0.0, 1.0)) as i32;
    base + boost
}

/// Adaptive B-COUNT (B-frames per anchor gap) for `auto` mode. The RATIO of the
/// 2-gap to 1-gap bi-prediction residual measures how fast bi-pred degrades as the
/// anchor spacing widens: LOW ratio (content survives wider gaps) carries MORE cheap
/// non-reference B's; HIGH ratio (simple translation — degrades fast, so wider anchors
/// cost more than the extra B's save) wants a single equidistant B. Calibrated on
/// pans/zoom: ratio ≥ 1.8 → 1, ≥ 1.4 → 2, else 3. Capped at `max_b` (the `auto` cap).
/// TEMPORAL PREDICTABILITY probe (Great Gate P3 item 4). Returns the
/// `2-gap / 1-gap` motion-compensated residual ratio for a frame window --
/// the axis the mb-tree dispatch has been waiting on.
///
/// Why THIS signal for mb-tree specifically: mb-tree lowers QP on blocks whose
/// quality PROPAGATES to the frames that reference them. That model assumes the
/// referenced pixels are still there N frames later. On a pan they translate out
/// of frame, so propagation decays and the lookahead over-credits the block. The
/// ratio measures exactly that decay -- how much worse prediction gets when the
/// reference gap doubles -- where `lv_spread`/`flat_run` (the REFUSED candidate)
/// are spatial statistics that merely correlate with panning on this corpus.
/// `f64::INFINITY` when the window is too short to measure.
pub fn temporal_decay_ratio(frames: &[YuvFrame], w: usize, h: usize) -> f64 {
    let g1 = gop_bi_residual(frames, w, h, 1);
    let g2 = gop_bi_residual(frames, w, h, 2);
    if !g1.is_finite() || !g2.is_finite() {
        return f64::INFINITY;
    }
    g2 / g1.max(1e-3)
}

fn adaptive_bcount(frames: &[YuvFrame], w: usize, h: usize, max_b: usize) -> usize {
    let cap = max_b.clamp(1, 3);
    let g1 = gop_bi_residual(frames, w, h, 1);
    let g2 = gop_bi_residual(frames, w, h, 2);
    if !g1.is_finite() || !g2.is_finite() {
        return 1;
    }
    let ratio = g2 / g1.max(1e-3);
    // Calibrated on this encoder's (subsampled global-ME) ratios: a simple
    // translation degrades to ~1.5 (→ 1 B), predictable-under-wide-gaps content sits
    // ~1.3 or below (→ 3 B).
    let c = if ratio >= 1.4 {
        1
    } else if ratio >= 1.3 {
        2
    } else {
        3
    };
    c.clamp(1, cap)
}

/// Cheap content signal for the content-adaptive dispatch: the mean per-pixel
/// residual of a coarse GLOBAL-motion BI-prediction, over a subsample of interior
/// frames. Low = temporally predictable (bi-pred + spatial-direct cheap → B-frames
/// WIN, and the I-frame dominates → deeper QP cascade); high = busy motion.
/// `f64::INFINITY` when the GOP is too short to measure (treated as busy).
///
/// Global (not block) ME keeps it O(pixels)-cheap and biases toward "coherent
/// motion", which is what spatial-direct/skip exploit. Thresholds calibrated on
/// extremes (pan ~0.03/px, high-motion ~12.3/px); refine on a corpus.
fn gop_bi_residual(frames: &[YuvFrame], w: usize, h: usize, gap: usize) -> f64 {
    let n = frames.len();
    if n < 2 * gap + 1 || w < 48 || h < 48 {
        return f64::INFINITY;
    }
    // Subsampled SAD of `cur` vs `rf` shifted by (dx,dy): interior pixels only
    // (|shift| ≤ 15 stays in-bounds, no clamping), every 4th pixel for speed.
    let sad = |cur: &[u8], rf: &[u8], dx: isize, dy: isize| -> u64 {
        let mut s = 0u64;
        let mut y = 16;
        while y < h - 16 {
            let cbase = (y * w) as isize;
            let rbase = ((y as isize + dy) * w as isize) + dx;
            let mut x = 16isize;
            while x < (w - 16) as isize {
                let c = cur[(cbase + x) as usize] as i32;
                let r = rf[(rbase + x) as usize] as i32;
                s += (c - r).unsigned_abs() as u64;
                x += 8;
            }
            y += 8;
        }
        s
    };
    // Coarse global ME: ±12 step 4, then refine ±3 step 1.
    let global_me = |cur: &[u8], rf: &[u8]| -> (isize, isize) {
        let (mut best, mut bc) = ((0isize, 0isize), u64::MAX);
        let mut dy = -12;
        while dy <= 12 {
            let mut dx = -12;
            while dx <= 12 {
                let c = sad(cur, rf, dx, dy);
                if c < bc {
                    bc = c;
                    best = (dx, dy);
                }
                dx += 4;
            }
            dy += 4;
        }
        // The refine window re-evaluates its own CENTRE — the coarse best,
        // whose cost `bc` already carries. `c < bc` is strict and `bc` only
        // decreases, so that re-visit can never win: skipping it is
        // decision-identical and saves one full subsampled-SAD call per
        // `global_me`. The range expressions stay on `best` (NOT hoisted):
        // the inner range re-reads the current `best.0` per row exactly as it
        // always did, so the search visits the same points minus the centre.
        let centre = best;
        for dy in best.1 - 3..=best.1 + 3 {
            for dx in best.0 - 3..=best.0 + 3 {
                if (dx, dy) == centre {
                    continue;
                }
                let c = sad(cur, rf, dx, dy);
                if c < bc {
                    bc = c;
                    best = (dx, dy);
                }
            }
        }
        best
    };
    let mut n_samp = 0usize;
    {
        let mut y = 16;
        while y < h - 16 {
            let mut x = 16;
            while x < w - 16 {
                n_samp += 1;
                x += 8;
            }
            y += 8;
        }
    }
    let step = (n / 5).max(1);
    let (mut total, mut cnt) = (0f64, 0usize);
    // `gap` frames each side (1 = adjacent, for the B/P dispatch; 2 probes how well
    // bi-prediction survives WIDER anchor spacing, for the adaptive B-count).
    let mut d = gap;
    while d < n - gap {
        let (cur, past, fut) = (&frames[d].y, &frames[d - gap].y, &frames[d + gap].y);
        let (mpx, mpy) = global_me(cur, past);
        let (mfx, mfy) = global_me(cur, fut);
        let mut bi = 0u64;
        let mut y = 16;
        while y < h - 16 {
            let mut x = 16isize;
            while x < (w - 16) as isize {
                let c = cur[y * w + x as usize] as i32;
                let p = past[((y as isize + mpy) * w as isize + x + mpx) as usize] as i32;
                let f = fut[((y as isize + mfy) * w as isize + x + mfx) as usize] as i32;
                bi += (c - ((p + f + 1) >> 1)).unsigned_abs() as u64;
                x += 8;
            }
            y += 8;
        }
        total += bi as f64 / n_samp as f64;
        cnt += 1;
        d += step;
    }
    if cnt > 0 {
        total / cnt as f64
    } else {
        f64::INFINITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use alloc::{
        boxed::Box,
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[allow(unused_imports)]
    use rusty_h264_common::once::OnceLock;

    fn textured(w: usize, h: usize, t: usize) -> YuvFrame {
        let mut f = YuvFrame::black(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x + 3 * t) * 255 / w) as u32 + ((y * 7 + x * 3 + t) % 23) as u32;
                f.y[y * w + x] = v.min(255) as u8;
            }
        }
        f
    }

    /// The half-pel plane cache is built lazily and only when the search is
    /// not `Fast` — a fact, not a comment, because the chip configuration's
    /// memory model has no half-pel term. `baseline()` codes P-frames with
    /// every reference's cache empty; the same configuration on
    /// `Preset::Balanced` fills it.
    #[test]
    fn fast_preset_never_builds_the_half_pel_cache() {
        let (w, h) = (64, 48);
        let frames: Vec<YuvFrame> = (0..4).map(|t| textured(w, h, t)).collect();
        for refs in [1, 3] {
            let mut cfg = EncoderConfig::baseline(w, h);
            cfg.gop_size = 8;
            cfg.min_keyint = 8;
            cfg.num_ref_frames = refs;
            assert_eq!(cfg.preset, Preset::Fast);
            let mut enc = Encoder::new(cfg).unwrap();
            for f in &frames {
                let _ = enc.encode(f);
            }
            assert!(!enc.refs.is_empty());
            assert!(
                enc.refs.iter().all(|r| r.hpel.get().is_none()),
                "Fast built a half-pel cache (refs={refs})"
            );
        }
        let mut cfg = EncoderConfig::baseline(w, h);
        cfg.gop_size = 8;
        cfg.min_keyint = 8;
        cfg.num_ref_frames = 3;
        cfg.preset = Preset::Balanced;
        let mut enc = Encoder::new(cfg).unwrap();
        for f in &frames {
            let _ = enc.encode(f);
        }
        assert!(
            enc.refs.iter().any(|r| r.hpel.get().is_some()),
            "Balanced never built the half-pel cache"
        );
    }

    #[test]
    fn rejects_unsupported_config() {
        // R6 made High + 8x8 + CABAC a SUPPORTED combination (transform_size_8x8_flag
        // at both syntax positions + the ctxBlockCat-5 residual writer), verified
        // pixel-identical against ffmpeg. This test used to assert it was rejected;
        // asserting it is ACCEPTED is what keeps the capability from silently
        // regressing behind a re-added guard.
        let mut cfg = EncoderConfig::new(16, 16);
        cfg.profile = Profile::High;
        cfg.transform_8x8 = true;
        cfg.cabac = true;
        assert!(
            Encoder::new(cfg).is_ok(),
            "High + 8x8 + CABAC must be accepted"
        );
        // Narrowing the profile CLAMPS the 8x8 transform rather than failing: it is
        // default-on, and `EncoderConfig::new()` + `profile = Main` must stay a valid
        // pair. Assert the encoder builds AND that the PPS does not advertise a tool
        // Main cannot carry.
        let mut cfg = EncoderConfig::new(16, 16);
        cfg.profile = Profile::Main;
        cfg.transform_8x8 = true;
        let enc = Encoder::new(cfg).expect("Main + 8x8 must clamp, not fail");
        assert!(
            !enc.cfg.transform_8x8,
            "8x8 must be cleared when the profile cannot signal it"
        );
    }

    #[test]
    fn encodes_access_unit_with_sps_pps_idr() {
        use rusty_h264_common::nal::split_annex_b;
        let cfg = EncoderConfig::new(32, 32);
        let mut enc = Encoder::new(cfg).unwrap();
        let frame = YuvFrame::black(32, 32);
        // mb-tree defaults ON: the streaming path buffers one GOP, so a single
        // frame's access unit arrives on `flush` (same shape the fuzz seeds use).
        let mut au = enc.encode(&frame);
        au.extend(enc.flush());

        let nals = split_annex_b(&au);
        assert_eq!(nals.len(), 3);
        assert_eq!(NalUnitType::from_id(nals[0][0]), NalUnitType::Sps);
        assert_eq!(NalUnitType::from_id(nals[1][0]), NalUnitType::Pps);
        assert_eq!(NalUnitType::from_id(nals[2][0]), NalUnitType::IdrSlice);
    }

    #[test]
    fn encode_all_matches_sequential_cqp() {
        // GOP-parallel batch encoding must be byte-identical to frame-by-frame
        // sequential encoding at constant QP (GOPs are independent).
        let (w, h) = (48usize, 32usize);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 4; // 10 frames → 3 GOPs (4,4,2)
        let frames: Vec<YuvFrame> = (0..10u8)
            .map(|t| YuvFrame {
                width: w,
                height: h,
                y: (0..w * h)
                    .map(|i| (i as u8).wrapping_add(t.wrapping_mul(7)))
                    .collect(),
                u: vec![128u8.wrapping_add(t); (w / 2) * (h / 2)],
                v: vec![128u8.wrapping_sub(t); (w / 2) * (h / 2)],
            })
            .collect();
        let mut seq_enc = Encoder::new(cfg.clone()).unwrap();
        let mut seq: Vec<u8> = frames.iter().flat_map(|f| seq_enc.encode(f)).collect();
        seq.extend_from_slice(&seq_enc.flush()); // end of stream (lookahead tail)
        let par: Vec<u8> = Encoder::new(cfg)
            .unwrap()
            .encode_all(&frames)
            .unwrap()
            .concat();
        assert_eq!(seq, par, "GOP-parallel must equal sequential+flush at CQP");
    }

    #[test]
    fn encode_all_matches_sequential_quality_preset() {
        // Same invariant on the QUALITY preset, whose per-frame dispatch decisions
        // (b2_mgain SAD/mv-cost routing) once lived in a process-global and RACED
        // across GOP workers — divergence only appears with >1 GOP in flight, which
        // the single-GOP hash harness never exercised. Content varies per frame so
        // the per-frame routing decisions actually differ between GOPs.
        let (w, h) = (48usize, 32usize);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.gop_size = 3; // 12 frames → 4 GOPs, several workers in flight
        cfg.preset = crate::config::Preset::Quality;
        let frames: Vec<YuvFrame> = (0..12u8)
            .map(|t| YuvFrame {
                width: w,
                height: h,
                y: (0..w * h)
                    .map(|i| {
                        // alternate calm and busy frames so the mgain probe flips
                        let base = (i as u8).wrapping_add(t.wrapping_mul(3));
                        if t % 2 == 0 {
                            base
                        } else {
                            base.wrapping_mul(37).wrapping_add(i as u8)
                        }
                    })
                    .collect(),
                u: vec![128u8.wrapping_add(t); (w / 2) * (h / 2)],
                v: vec![128u8.wrapping_sub(t); (w / 2) * (h / 2)],
            })
            .collect();
        let mut seq_enc = Encoder::new(cfg.clone()).unwrap();
        let mut seq: Vec<u8> = frames.iter().flat_map(|f| seq_enc.encode(f)).collect();
        seq.extend_from_slice(&seq_enc.flush());
        let par: Vec<u8> = Encoder::new(cfg)
            .unwrap()
            .encode_all(&frames)
            .unwrap()
            .concat();
        assert_eq!(
            seq, par,
            "quality-preset GOP-parallel must equal sequential+flush"
        );
    }

    #[test]
    fn rejects_mismatched_frame() {
        let cfg = EncoderConfig::new(16, 16);
        let mut enc = Encoder::new(cfg).unwrap();
        let frame = YuvFrame::black(32, 16);
        assert_eq!(enc.try_encode(&frame), Err(EncodeError::FrameMismatch));
    }
}
