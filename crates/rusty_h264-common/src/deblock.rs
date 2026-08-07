//! In-loop deblocking filter (spec §8.7), all-intra case.
//!
//! Smooths block-edge discontinuities on the reconstructed frame. Because intra
//! prediction uses *pre*-deblocking samples, this runs as a post-pass over the
//! fully-reconstructed frame: macroblocks in raster order, vertical edges then
//! horizontal, filtered in place. For an all-intra picture the boundary
//! strength is positional — 4 on macroblock edges, 3 on internal 4×4 edges.

/// `α` threshold indexed by `indexA` (= clipped QP), spec Table 8-16.
#[rustfmt::skip]
const ALPHA: [i32; 52] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    4,4,5,6,7,8,9,10,12,13,15,17,20,22,25,28,
    32,36,40,45,50,56,63,71,80,90,101,113,127,144,162,182,203,226,255,255,
];

/// `β` threshold indexed by `indexB`.
#[rustfmt::skip]
const BETA: [i32; 52] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    2,2,2,3,3,3,3,4,4,4,6,6,7,7,8,8,
    9,9,10,10,11,11,12,12,13,13,14,14,15,15,16,16,17,17,18,18,
];

/// `tc0` indexed by `[indexA][bS-1]` for bS ∈ {1,2,3}.
#[rustfmt::skip]
const TC0: [[i32; 3]; 52] = [
    [0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],
    [0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],
    [0,0,0],[0,0,1],[0,0,1],[0,0,1],[0,0,1],[0,1,1],[0,1,1],[1,1,1],
    [1,1,1],[1,1,1],[1,1,1],[1,1,2],[1,1,2],[1,1,2],[1,1,2],[1,2,3],
    [1,2,3],[2,2,3],[2,2,4],[2,3,4],[2,3,4],[3,3,5],[3,4,6],[3,4,6],
    [4,5,7],[4,5,8],[4,6,9],[5,7,10],[6,8,11],[6,8,13],[7,10,14],[8,11,16],
    [9,12,18],[10,13,20],[11,15,23],[13,17,25],
];

#[inline]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// One sample line crossing an edge: `p3..p0 | q0..q3` (indices 0..3 from the
/// edge outward). Reads/writes a plane along `stride`-spaced positions.
struct Line {
    /// Byte offset of q0 (the first sample on the "right"/"below" side).
    base: usize,
    /// Step between adjacent samples across the edge (1 horizontally, `stride`
    /// vertically).
    step: isize,
}

/// Filters luma samples across one edge line. `bs` is 3 (internal) or 4 (MB edge).
#[allow(clippy::too_many_arguments)]
fn filter_luma_line(plane: &mut [u8], line: &Line, bs: i32, alpha: i32, beta: i32, tc0: i32) {
    let at = |i: isize| -> i32 {
        plane[(line.base as isize + i * line.step) as usize] as i32
    };
    let (p0, p1, p2, p3) = (at(-1), at(-2), at(-3), at(-4));
    let (q0, q1, q2, q3) = (at(0), at(1), at(2), at(3));

    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let set = |plane: &mut [u8], i: isize, v: u8| {
        plane[(line.base as isize + i * line.step) as usize] = v;
    };
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();

    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
        set(plane, -1, clip1(p0 + delta));
        set(plane, 0, clip1(q0 - delta));
        if ap < beta {
            let d = clip3(-tc0, tc0, (p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1);
            set(plane, -2, clip1(p1 + d));
        }
        if aq < beta {
            let d = clip3(-tc0, tc0, (q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1);
            set(plane, 1, clip1(q1 + d));
        }
    } else {
        let strong = (p0 - q0).abs() < (alpha >> 2) + 2;
        if strong && ap < beta {
            set(plane, -1, clip1((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3));
            set(plane, -2, clip1((p2 + p1 + p0 + q0 + 2) >> 2));
            set(plane, -3, clip1((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3));
        } else {
            set(plane, -1, clip1((2 * p1 + p0 + q1 + 2) >> 2));
        }
        if strong && aq < beta {
            set(plane, 0, clip1((q2 + 2 * q1 + 2 * q0 + 2 * p0 + p1 + 4) >> 3));
            set(plane, 1, clip1((q2 + q1 + q0 + p0 + 2) >> 2));
            set(plane, 2, clip1((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3));
        } else {
            set(plane, 0, clip1((2 * q1 + q0 + p1 + 2) >> 2));
        }
    }
}

/// Filters chroma samples across one edge line (only p0/q0 are modified).
fn filter_chroma_line(plane: &mut [u8], line: &Line, bs: i32, alpha: i32, beta: i32, tc0: i32) {
    let at = |i: isize| -> i32 {
        plane[(line.base as isize + i * line.step) as usize] as i32
    };
    let (p0, p1) = (at(-1), at(-2));
    let (q0, q1) = (at(0), at(1));
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let set = |plane: &mut [u8], i: isize, v: u8| {
        plane[(line.base as isize + i * line.step) as usize] = v;
    };
    if bs < 4 {
        let tc = tc0 + 1;
        let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
        set(plane, -1, clip1(p0 + delta));
        set(plane, 0, clip1(q0 - delta));
    } else {
        set(plane, -1, clip1((2 * p1 + p0 + q1 + 2) >> 2));
        set(plane, 0, clip1((2 * q1 + q0 + p1 + 2) >> 2));
    }
}

/// Per-4×4-block macroblock info driving boundary-strength derivation.
pub struct BlockInfo<'a> {
    /// `true` if the block is **inter**-coded (an intra block is `!inter`). Taking
    /// the caller's existing inter mask avoids allocating an inverted intra mask
    /// per frame in both the decoder and encoder.
    pub inter: &'a [bool],
    /// Non-zero coefficient count of the block.
    pub nnz: &'a [u8],
    /// List-0 block motion vector (quarter-pel); ignored for intra.
    pub mv: &'a [(i32, i32)],
    /// List-0 reference *picture identity* (a stable per-picture id — PicOrderCnt
    /// for the decoder, ref index for the encoder; `i32::MIN` = unused/intra).
    /// Boundary strength compares the *set* of reference pictures, so the same
    /// picture used via different lists matches (spec §8.7.2.1).
    pub ref_id: &'a [i32],
    /// List-1 motion + reference identity for B blocks (`ref_id1 = i32::MIN`
    /// everywhere for P/I, so the extra slot is a no-op there).
    pub mv1: &'a [(i32, i32)],
    pub ref_id1: &'a [i32],
    /// Block-grid width (`mb_w * 4`).
    pub w4: usize,
    /// Per-macroblock `transform_size_8x8_flag` (length `mb_w * mb_h`). When set,
    /// the macroblock's internal 4×4 luma edges (sample columns/rows 4 and 12)
    /// are *not* transform boundaries and must not be filtered (spec §8.7).
    pub t8x8: &'a [bool],
    /// Boundary strengths already derived by the caller, one entry per macroblock
    /// in raster order. Empty = derive them here (the decoder path).
    pub bs: &'a [MbBs],
    /// OPTIONAL reference→identity maps (Brick: WHYS Part 15 item 2). When
    /// non-empty, `ref_id`/`ref_id1` hold RAW per-block reference INDICES
    /// (negative = none) and these ≤16-entry tables map index → picture
    /// identity (POC). This lets the decoder pass its `ref_idx` grids directly
    /// instead of materializing two frame-wide pre-mapped `Vec<i32>` shims
    /// (230-460 KB + 57,600 mapped elements per frame). Empty = `ref_id`
    /// already carries identities (the encoder path and all older callers).
    pub poc0: &'a [i32],
    pub poc1: &'a [i32],
    /// Per-macroblock derivation CLASS (`MB_KIND_*`), one entry per macroblock in
    /// raster order. Empty, or `MB_KIND_UNSET`, means "not classified" and the
    /// blind gather+derive runs for that macroblock.
    ///
    /// SAFETY-BY-DESIGN: an unclassified macroblock costs SPEED, never
    /// correctness. That inversion is deliberate — the alternative design, where
    /// the producer must hit every macroblock exit point or strengths silently go
    /// wrong, is exactly the failure `MbBs::UNSET` exists to guard against.
    ///
    /// The producer must be CONSERVATIVE. In particular `B_Skip` and
    /// `B_Direct_16x16` are NOT `MB_KIND_SKIP`: their motion comes from direct
    /// prediction and may differ per 4x4 sub-block, so internal edges can legally
    /// reach strength 1. Only classify what is uniform BY SYNTAX.
    pub kind: &'a [u8],
}

/// Not classified — take the blind path. Any value outside `0..=3` behaves this way.
pub const MB_KIND_UNSET: u8 = 255;
pub const MB_KIND_INTRA: u8 = 0;
pub const MB_KIND_SKIP: u8 = 1;
pub const MB_KIND_INTER_UNIFORM: u8 = 2;
pub const MB_KIND_INTER: u8 = 3;

impl MbKind {
    /// Decode a producer-supplied class byte. `None` = derive blindly.
    #[inline]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            MB_KIND_INTRA => Some(MbKind::Intra),
            MB_KIND_SKIP => Some(MbKind::Skip),
            MB_KIND_INTER_UNIFORM => Some(MbKind::InterUniform),
            MB_KIND_INTER => Some(MbKind::Inter),
            _ => None,
        }
    }
}

/// One macroblock's boundary strengths: `[edge group][segment]` per direction.
///
/// 32 bytes, mirroring x264's `uint8_t bs[2][8][4]`. Deriving these during ENCODE
/// lets the deblocking pass skip the neighbourhood gather and the derivation.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MbBs {
    /// Vertical edges (block columns 0..4); index 0 is the macroblock edge.
    pub v: [[u8; 4]; 4],
    /// Horizontal edges (block rows 0..4); index 0 is the macroblock edge.
    pub h: [[u8; 4]; 4],
}

impl MbBs {
    /// "Not yet derived". The macroblock loop has several exits (free skip,
    /// greedy skip, coded) and every one must store its strengths; missing one
    /// leaves zeros, silently disabling deblocking for that macroblock — which is
    /// exactly the bug the byte-identical gate caught during bring-up.
    pub const UNSET: MbBs = MbBs { v: [[0xFF; 4]; 4], h: [[0xFF; 4]; 4] };
}

/// Sentinel for an unused reference slot.
const NO_REF: i32 = i32::MIN;

/// One 4×4 block's deblocking state, gathered into a per-macroblock tile.
///
/// The frame-wide `inter`/`nnz`/`mv`/`ref_id` arrays are indexed `by * w4 + bx`,
/// so deriving boundary strengths straight from them costs ~290 scattered loads
/// per macroblock (each block is re-read by up to four edges, and vertical edge
/// groups stride by `w4`). Gathering the 24 blocks an MB can touch into this
/// contiguous tile once turns all 48 edge decisions into stack/L1 reads. x264
/// gets the same effect from its `scan8` cache, which is what lets its
/// `deblock_strength` kernel be SIMD at ~15 ns/MB.
#[derive(Clone, Copy, Default)]
struct Blk {
    inter: bool,
    /// `nnz != 0` — the count itself never matters, only whether it is non-zero.
    nz: bool,
    ref_id: i32,
    mvx: i32,
    mvy: i32,
    /// List-1 slot (B slices). `NO_REF` on P/I tiles and on uni-L0 blocks, which
    /// keeps every comparison below on its single-list fast path there.
    ref1: i32,
    mv1x: i32,
    mv1y: i32,
}

impl Blk {
    #[inline]
    fn load(info: &BlockInfo, i: usize) -> Self {
        #[cfg(feature = "profile")]
        census::BLK_LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (mvx, mvy) = info.mv[i];
        let (ref1, (mv1x, mv1y)) = if info.ref_id1.is_empty() {
            (NO_REF, (0, 0))
        } else {
            (info.rid1(i), info.mv1[i])
        };
        Blk { inter: info.inter[i], nz: info.nnz[i] != 0, ref_id: info.rid(i), mvx, mvy, ref1, mv1x, mv1y }
    }

    /// Whether two blocks are identical for flat-inter purposes (both lists —
    /// on P tiles the list-1 fields are uniformly `NO_REF`/0, so the extra
    /// compares are always-true and the P behaviour is unchanged).
    #[inline]
    fn same_motion(&self, o: &Blk) -> bool {
        self.ref_id == o.ref_id
            && self.mvx == o.mvx
            && self.mvy == o.mvy
            && self.ref1 == o.ref1
            && self.mv1x == o.mv1x
            && self.mv1y == o.mv1y
    }
}

/// The bS==1 motion test on tile entries — mirrors [`BlockInfo::inter_bs1`]
/// exactly (its oracle), including the two-slot B rule, but reads registers
/// instead of strided frame arrays. The single-list fast path fires whenever
/// neither side carries a List-1 slot: all of P, and B edges between uni-L0
/// blocks (equivalent to the general rule at n<=1 — a differing ref covers the
/// differing-slot-count case, two unused slots give false).
#[inline]
fn bs1_tile(p: &Blk, q: &Blk) -> bool {
    if (p.ref1 == NO_REF) & (q.ref1 == NO_REF) {
        let far = ((p.mvx - q.mvx).abs() >= 4) | ((p.mvy - q.mvy).abs() >= 4);
        return (p.ref_id != q.ref_id) | ((p.ref_id != NO_REF) & far);
    }
    let used = |b: &Blk| {
        let mut v = [(0i32, (0i32, 0i32)); 2];
        let mut n = 0usize;
        if b.ref_id != NO_REF {
            v[n] = (b.ref_id, (b.mvx, b.mvy));
            n += 1;
        }
        if b.ref1 != NO_REF {
            v[n] = (b.ref1, (b.mv1x, b.mv1y));
            n += 1;
        }
        (v, n)
    };
    let (pv, pn) = used(p);
    let (qv, qn) = used(q);
    if pn != qn {
        return true;
    }
    let far = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() >= 4 || (a.1 - b.1).abs() >= 4;
    match pn {
        0 => false,
        1 => pv[0].0 != qv[0].0 || far(pv[0].1, qv[0].1),
        _ => {
            let direct = !far(pv[0].1, qv[0].1) && !far(pv[1].1, qv[1].1);
            let swap = !far(pv[0].1, qv[1].1) && !far(pv[1].1, qv[0].1);
            if pv[0].0 == pv[1].0 {
                qv[0].0 != pv[0].0 || qv[1].0 != pv[0].0 || !(direct || swap)
            } else if pv[0].0 == qv[0].0 && pv[1].0 == qv[1].0 {
                !direct
            } else if pv[0].0 == qv[1].0 && pv[1].0 == qv[0].0 {
                !swap
            } else {
                true
            }
        }
    }
}

/// Boundary strength from tile entries (spec §8.7.2.1).
/// Branchless for the same reason as [`BlockInfo::bs_branchless`], but now with
/// every operand already in a register rather than behind a strided load.
#[inline]
fn bs_tile(p: &Blk, q: &Blk, mb_edge: bool) -> i32 {
    let intra = !(p.inter & q.inter);
    let nz = p.nz | q.nz;
    let moved = bs1_tile(p, q);
    let intra_bs = if mb_edge { 4 } else { 3 };
    let non_intra = if nz { 2 } else { moved as i32 };
    if intra {
        intra_bs
    } else {
        non_intra
    }
}

/// Boundary strength for an edge where BOTH sides are inter — the only case left
/// once the per-macroblock intra fills in [`derive_mb_bs`] have run. It can never
/// return 3 or 4, which is precisely why x264's `deblock_strength` kernel is
/// branch-light enough to vectorise: the intra strengths are not its job.
#[inline]
fn bs_inter(p: &Blk, q: &Blk) -> i32 {
    let nz = p.nz | q.nz;
    let moved = bs1_tile(p, q);
    if nz {
        2
    } else {
        moved as i32
    }
}

/// MB-KIND CENSUS — measurement only, `--features profile`, so the shipped and
/// benchmarked binaries carry no counter at all (atomics in a 6.5M-call loop have
/// measured ~15% wall inflation elsewhere in this workspace).
///
/// Exists to size the prize of a KIND-AWARE derivation arithmetically before one
/// is built. `derive_mb_kind` already reads nothing for Intra, 9 blocks for Skip
/// and 16 nnz bytes for InterUniform, against 24 blocks across 5-7 grids for the
/// blind `derive_mb` the decoder currently calls — but that only pays if the
/// cheap kinds actually dominate real content. Count first.
#[cfg(feature = "profile")]
pub mod census {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static INTRA: AtomicU64 = AtomicU64::new(0);
    pub static SKIP: AtomicU64 = AtomicU64::new(0);
    pub static UNIFORM: AtomicU64 = AtomicU64::new(0);
    pub static INTER: AtomicU64 = AtomicU64::new(0);
    /// Block VISITS made by the predicate walk(s). The timing instruments on this
    /// box cannot resolve the fusion (two whole-decode runs disagreed in sign; the
    /// anatomy bench's within-arm spread reached 45%), so the bankable evidence is
    /// the work actually removed, which no amount of drift can move.
    pub static PRED_VISITS: AtomicU64 = AtomicU64::new(0);
    /// Every `Blk::load` — the per-block, multi-array gather that Brick 2 exists to
    /// avoid. This is the PRIMARY evidence for the brick (codec-measurement §15):
    /// one run, drift-immune, and it sizes the win as well as proving it.
    pub static BLK_LOADS: AtomicU64 = AtomicU64::new(0);
    /// Macroblocks that took the PACKED derivation. Coverage matters: a
    /// byte-identical result proves nothing if the path never engaged.
    pub static PACKED_MB: AtomicU64 = AtomicU64::new(0);
    /// Macroblocks that actually REACH the motion-mask derivation (not intra, not
    /// uniform) — the denominator for any kernel prize.
    pub static MASKS_CALLS: AtomicU64 = AtomicU64::new(0);
    /// ...of those, served by the AVX2 single-list kernel (`l1_used == 0`).
    pub static MASKS_KERNEL: AtomicU64 = AtomicU64::new(0);
    /// ...of those, forced to the scalar two-list set-match (`l1_used != 0`).
    /// THIS is the population a two-list kernel would win, and nothing else.
    pub static MASKS_L1: AtomicU64 = AtomicU64::new(0);

    pub fn reset() {
        for c in [
            &INTRA, &SKIP, &UNIFORM, &INTER, &PRED_VISITS, &BLK_LOADS, &PACKED_MB,
            &MASKS_CALLS, &MASKS_KERNEL, &MASKS_L1,
        ] {
            c.store(0, Ordering::Relaxed);
        }
    }

    /// Per-kind gather cost in BLOCK LOADS, from `derive_mb_kind`'s own structure.
    /// The projection below is a load-count model, not a timing — it says what is
    /// worth measuring, and the measurement still has to happen.
    pub fn dump() {
        let (i, s, u, n) = (
            INTRA.load(Ordering::Relaxed),
            SKIP.load(Ordering::Relaxed),
            UNIFORM.load(Ordering::Relaxed),
            INTER.load(Ordering::Relaxed),
        );
        let tot = (i + s + u + n).max(1);
        eprintln!("--- MB-kind census (deblock bS derivation) — {tot} macroblocks ---");
        for (name, n_mb, blocks) in
            [("Intra", i, 0u64), ("Skip", s, 9), ("InterUniform", u, 16), ("Inter", n, 24)]
        {
            eprintln!(
                "  {name:<13} {n_mb:>10} MB  {:>5.1}%   kind-aware gather: {blocks:>2} blocks vs 24 blind",
                100.0 * n_mb as f64 / tot as f64
            );
        }
        eprintln!(
            "  Blk::load GATHER loads:      {}  ({:.2} per MB)",
            BLK_LOADS.load(Ordering::Relaxed),
            BLK_LOADS.load(Ordering::Relaxed) as f64 / tot as f64
        );
        eprintln!(
            "  predicate-walk block VISITS: {}  ({:.2} per MB)",
            PRED_VISITS.load(Ordering::Relaxed),
            PRED_VISITS.load(Ordering::Relaxed) as f64 / tot as f64
        );
        eprintln!(
            "  PACKED-path macroblocks:     {}",
            PACKED_MB.load(Ordering::Relaxed)
        );
        let mc = MASKS_CALLS.load(Ordering::Relaxed).max(1);
        eprintln!(
            "  mask derivations:            {}  (AVX2 kernel {} = {:.1}%, scalar two-list {} = {:.1}%)",
            MASKS_CALLS.load(Ordering::Relaxed),
            MASKS_KERNEL.load(Ordering::Relaxed),
            100.0 * MASKS_KERNEL.load(Ordering::Relaxed) as f64 / mc as f64,
            MASKS_L1.load(Ordering::Relaxed),
            100.0 * MASKS_L1.load(Ordering::Relaxed) as f64 / mc as f64,
        );
        let blind = 24.0 * tot as f64;
        let aware = (0 * i + 9 * s + 16 * u + 24 * n) as f64;
        eprintln!(
            "  => block loads {:.0} -> {:.0}  ({:.1}% of the gather removed)",
            blind,
            aware,
            100.0 * (1.0 - aware / blind)
        );
    }
}

/// Derive one macroblock's 32 boundary strengths (2 directions × 4 edge groups ×
/// 4 segments), x264-style.
///
/// The structural point: `mb_type` is a per-MACROBLOCK syntax element, so a
/// macroblock is wholly intra or wholly inter and intra-ness is not a per-edge
/// property. That turns the two expensive cases into constant fills —
///   * intra macroblock → internal edges are the constant 3, its own macroblock
///     edges the constant 4;
///   * flat inter (skip) macroblock → every internal strength is 0;
/// — and leaves [`bs_inter`] for the rest. Pinned by `derive_matches_per_edge`.
#[inline]
fn derive_mb_bs(
    tile: &Tile,
    mb_x: usize,
    mb_y: usize,
    flat_inter: bool,
    uniform_motion: bool,
    mb_t8: bool,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) {
    let cur_intra = !tile[1][1].inter;

    #[cfg(feature = "profile")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        let c = if cur_intra {
            &census::INTRA
        } else if flat_inter {
            &census::SKIP
        } else if uniform_motion {
            &census::UNIFORM
        } else {
            &census::INTER
        };
        c.fetch_add(1, Relaxed);
    }

    // ---- macroblock edges: 4 if EITHER side is intra, else an inter compare ----
    if mb_x > 0 {
        bs_v[0] = if cur_intra || !tile[1][0].inter {
            [4; 4]
        } else {
            std::array::from_fn(|seg| bs_inter(&tile[seg + 1][0], &tile[seg + 1][1]))
        };
    }
    if mb_y > 0 {
        bs_h[0] = if cur_intra || !tile[0][1].inter {
            [4; 4]
        } else {
            std::array::from_fn(|seg| bs_inter(&tile[0][seg + 1], &tile[1][seg + 1]))
        };
    }

    // ---- internal edges ----
    if flat_inter {
        return; // all zero by construction (no coefficients, one shared ref+mv)
    }
    for be in 1..4usize {
        // An 8×8-transform macroblock has no transform boundary at 4×4 edges 1/3.
        if mb_t8 && (be == 1 || be == 3) {
            continue;
        }
        if cur_intra {
            bs_v[be] = [3; 4];
            bs_h[be] = [3; 4];
        } else if uniform_motion {
            // Coefficients alone; 0 or 2, no motion compare.
            bs_v[be] = std::array::from_fn(|seg| {
                2 * (tile[seg + 1][be].nz | tile[seg + 1][be + 1].nz) as i32
            });
            bs_h[be] = std::array::from_fn(|seg| {
                2 * (tile[be][seg + 1].nz | tile[be + 1][seg + 1].nz) as i32
            });
        } else {
            // Both sides are inside this macroblock, so no neighbour read at all.
            bs_v[be] = std::array::from_fn(|seg| bs_inter(&tile[seg + 1][be], &tile[seg + 1][be + 1]));
            bs_h[be] = std::array::from_fn(|seg| bs_inter(&tile[be][seg + 1], &tile[be + 1][seg + 1]));
        }
    }
}

/// What the encoder already knows about a macroblock the moment it finishes
/// coding it. Passing it in turns most macroblocks into constant fills and
/// removes the neighbourhood gather that made deriving in the encode loop cost
/// MORE than deriving in the deblocking pass (the loop's working set is far more
/// contended, so a 24-block gather there evicts live encoder data).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MbKind {
    /// Intra: internal strengths are the constant 3, its own macroblock edges 4.
    /// Needs NO block reads at all.
    Intra,
    /// Skip — no coefficients, one shared reference and motion vector — so every
    /// internal strength is 0 and only the two macroblock edges are derived.
    Skip,
    /// Coded inter with a SINGLE partition (P_L0_16x16 — the fast preset's only
    /// inter mode). All 16 blocks share one reference and motion vector, so no
    /// internal edge can reach strength 1: internal strengths depend on
    /// coefficients alone, and the derivation reads 16 nnz bytes instead of
    /// gathering 24 blocks across four grids.
    InterUniform,
    /// Coded inter, multiple partitions: the full derivation.
    Inter,
}

/// Boundary strengths for a macroblock whose kind the caller already knows.
///
/// `Intra` reads nothing; `Skip` reads one of its own blocks plus the neighbour
/// column/row (9 instead of 24); only `Inter` pays the full gather.
/// `derive_mb_kind` adapted to the deblock loop's `i32` strength arrays.
///
/// NOTE, accurately: this is currently a WRAPPER — it still builds the packed `MbBs`
/// and expands it, so it does not yet remove the 32 u8->i32 stores per macroblock.
/// It exists as the seam where a direct-write implementation belongs if the stores
/// ever measure. They were NOT the dominant cost of the first regression: the
/// 800-byte per-macroblock `Tile` zero-init was (~4 GB of memset added), and the
/// stores are ~0.5% by arithmetic. Fix the measured thing first; leave the seam.
pub fn derive_mb_kind_into(
    info: &BlockInfo,
    mb_x: usize,
    mb_y: usize,
    kind: MbKind,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) {
    let m = derive_mb_kind(info, mb_x, mb_y, kind);
    for e in 0..4 {
        for sg in 0..4 {
            bs_v[e][sg] = m.v[e][sg] as i32;
            bs_h[e][sg] = m.h[e][sg] as i32;
        }
    }
}

pub fn derive_mb_kind(info: &BlockInfo, mb_x: usize, mb_y: usize, kind: MbKind) -> MbBs {
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let w4 = info.w4;
    match kind {
        MbKind::Intra => {
            let mut m = MbBs::default();
            if mb_x > 0 {
                m.v[0] = [4; 4];
            }
            if mb_y > 0 {
                m.h[0] = [4; 4];
            }
            for e in 1..4 {
                m.v[e] = [3; 4];
                m.h[e] = [3; 4];
            }
            m
        }
        MbKind::Skip => {
            // Internal strengths stay 0: no coefficients and one shared (ref, mv)
            // means no internal edge can reach strength 1 or 2.
            let mut m = MbBs::default();
            let me = Blk::load(info, by0 * w4 + bx0); // all 16 blocks are identical
            if mb_x > 0 {
                m.v[0] = std::array::from_fn(|seg| {
                    let p = Blk::load(info, (by0 + seg) * w4 + bx0 - 1);
                    if p.inter { bs_inter(&p, &me) as u8 } else { 4 }
                });
            }
            if mb_y > 0 {
                m.h[0] = std::array::from_fn(|seg| {
                    let p = Blk::load(info, (by0 - 1) * w4 + bx0 + seg);
                    if p.inter { bs_inter(&p, &me) as u8 } else { 4 }
                });
            }
            m
        }
        MbKind::InterUniform => {
            let mut m = MbBs::default();
            // Internal edges: uniform motion means only coefficients can raise a
            // strength, and then only to 2.
            for e in 1..4usize {
                m.v[e] = std::array::from_fn(|seg| {
                    let i = (by0 + seg) * w4 + bx0 + e;
                    2 * ((info.nnz[i] != 0) | (info.nnz[i - 1] != 0)) as u8
                });
                m.h[e] = std::array::from_fn(|seg| {
                    let i = (by0 + e) * w4 + bx0 + seg;
                    2 * ((info.nnz[i] != 0) | (info.nnz[i - w4] != 0)) as u8
                });
            }
            // Macroblock edges still cross into the neighbour, and our own
            // coefficients vary per block, so both sides are read per segment.
            if mb_x > 0 {
                m.v[0] = std::array::from_fn(|seg| {
                    let qi = (by0 + seg) * w4 + bx0;
                    let p = Blk::load(info, qi - 1);
                    if p.inter { bs_inter(&p, &Blk::load(info, qi)) as u8 } else { 4 }
                });
            }
            if mb_y > 0 {
                m.h[0] = std::array::from_fn(|seg| {
                    let qi = by0 * w4 + bx0 + seg;
                    let p = Blk::load(info, qi - w4);
                    if p.inter { bs_inter(&p, &Blk::load(info, qi)) as u8 } else { 4 }
                });
            }
            m
        }
        MbKind::Inter => derive_mb(info, mb_x, mb_y, false),
    }
}

/// Derive one macroblock's boundary strengths from the block grids — the entry
/// point for computing them during ENCODE.
///
/// `info.ref_id` may carry the encoder's raw reference indices (negative for
/// intra) rather than the `NO_REF` sentinel: reference identity is only ever
/// compared between two INTER blocks, which always hold a valid index.
/// PACKED per-macroblock boundary-strength inputs — the x264-shaped layout.
///
/// Everything one macroblock's derivation needs for its own 16 blocks, contiguous,
/// in raster order (`k = row * 4 + col`). This is the layout `deblock_strength_avx2`
/// works from, and the reason x264 can vectorise a job we currently do 32 times
/// scalar and branchy.
///
/// Sized by the CEILING PROBE (`examples/bs_layout_ceiling.rs`), which measured, at
/// 720p on synthetic-but-representative content:
///
/// * cache lines touched per macroblock: **16-20 strided vs 2 packed**
/// * gather cost (cache-warm): **31.1 ns/MB strided vs 12.5 ns/MB packed** (2.49x)
///
/// Read that second row carefully, because it REFUTED the original justification for
/// this work. The whole derivation costs ~400 ns/MB in context, so eliminating the
/// gather buys only ~19 ns/MB — about 5% of the derivation, ~1% of decode. **The
/// derivation is COMPUTE-bound, not gather-bound**: ~370 ns/MB is the 32 per-edge
/// `bs_inter` evaluations themselves (32 edges x ~20-30 cycles ~= 210-320 ns @ 3 GHz,
/// which matches). So this layout does NOT pay for itself as a memory optimisation.
/// It exists to make the ARITHMETIC vectorisable — `nnz` collapses to shift-and-or on
/// a 16-bit mask, and the motion test becomes i16 subtract/abs/compare across 16
/// lanes. That distinction decides whether the SIMD stage is worth building, and it
/// is the good case: this codebase's flat SIMD results were all memory-bound kernels,
/// its wins (DCT/SATD) were compute-bound.
///
/// SINGLE-LIST ONLY. B macroblocks carrying a List-1 slot fall back to the blind
/// path; the two-list rule in `bs1_tile` is not reproduced here.
#[derive(Clone, Copy)]
#[repr(C, align(32))]
pub struct MbPack {
    /// Bit `k` set = block `k` has coefficients. The derivation only ever asks
    /// "is this block coded", never for the count.
    pub nnz_mask: u16,
    /// `mb_type` is a per-MACROBLOCK syntax element, so intra-ness is not per-block.
    pub inter: bool,
    _pad: u8,
    /// Quarter-pel motion, per block, SPLIT into x and y planes.
    ///
    /// Structure-of-arrays on purpose: interleaved `(x,y)` pairs would force the SIMD
    /// twin to OR adjacent lanes together (a pairwise reduction), whereas split planes
    /// let the x and y compares live in separate registers and OR whole-register.
    /// 16 x i16 is exactly one 256-bit load each.
    pub mvx: [i16; 16],
    pub mvy: [i16; 16],
    /// Bit k set = block k carries a List-1 slot. When this is 0 for a macroblock,
    /// every internal edge takes the single-list rule and the AVX2 kernel applies;
    /// otherwise the two-list set-matching rule runs scalar for that macroblock.
    pub l1_used: u16,
    pub mvx1: [i16; 16],
    pub mvy1: [i16; 16],
    pub ref1: [i32; 16],
    /// Reference PICTURE IDENTITY per block (`NO_REF` = intra/unused). Kept i32
    /// because the decoder supplies a POC, which must stay comparable across slices
    /// whose reference lists differ — a `ref_idx` would compare equal across slices
    /// that mean different pictures.
    pub ref_id: [i32; 16],
}

impl BlockInfo<'_> {
    /// Block `i`'s List-0 reference identity, applying the optional index map.
    #[inline]
    fn rid(&self, i: usize) -> i32 {
        let r = self.ref_id[i];
        if self.poc0.is_empty() {
            r
        } else if r >= 0 {
            self.poc0.get(r as usize).copied().unwrap_or(NO_REF)
        } else {
            NO_REF
        }
    }

    /// Block `i`'s List-1 reference identity (mapped); `NO_REF` when no List-1.
    #[inline]
    fn rid1(&self, i: usize) -> i32 {
        let r = self.ref_id1[i];
        if self.poc1.is_empty() {
            r
        } else if r >= 0 {
            self.poc1.get(r as usize).copied().unwrap_or(NO_REF)
        } else {
            NO_REF
        }
    }
}

impl Default for MbPack {
    fn default() -> Self {
        Self {
            nnz_mask: 0,
            inter: false,
            _pad: 0,
            mvx: [0; 16],
            mvy: [0; 16],
            l1_used: 0,
            mvx1: [0; 16],
            mvy1: [0; 16],
            ref1: [NO_REF; 16],
            ref_id: [NO_REF; 16],
        }
    }
}

/// Build the packed records for a whole frame in ONE streaming pass.
///
/// Each frame-wide array is read once, sequentially and prefetch-friendly, instead of
/// being hit 3600 times at scattered per-macroblock offsets. Returns `None` when the
/// frame carries List-1 data (B slices), which this layout does not model.
pub fn pack_frame(info: &BlockInfo, mb_w: usize, mb_h: usize) -> Option<Vec<MbPack>> {
    let mut out = Vec::new();
    pack_frame_into(info, mb_w, mb_h, &mut out);
    Some(out)
}

/// [`pack_frame`] into a RECYCLED buffer (Brick: WHYS Part 15 item 1). A fresh
/// `Vec<MbPack>` was ~1.1 MB allocated + first-touch-faulted EVERY frame — the
/// same per-frame-materialization mechanism the banked GridPool brick removed.
/// Each record is built in a local and pushed, so a reused allocation is never
/// pre-filled: one write per byte, no defaults pass, no page faults.
pub fn pack_frame_into(info: &BlockInfo, mb_w: usize, mb_h: usize, out: &mut Vec<MbPack>) {
    let has1 = !info.ref_id1.is_empty();
    out.clear();
    out.reserve(mb_w * mb_h);
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            out.push(pack_mb(info, has1, mb_x, mb_y));
        }
    }
}

/// One macroblock's packed record — the unit of both `pack_frame_into` and the
/// rolling-window precompute pass.
#[inline]
pub fn pack_mb(info: &BlockInfo, has1: bool, mb_x: usize, mb_y: usize) -> MbPack {
    let mut rec = MbPack::default();
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    rec.inter = info.inter[by0 * info.w4 + bx0];
    for r in 0..4 {
        let row = (by0 + r) * info.w4 + bx0;
        for c in 0..4 {
            let i = row + c;
            let k = r * 4 + c;
            if info.nnz[i] != 0 {
                rec.nnz_mask |= 1 << k;
            }
            rec.mvx[k] = info.mv[i].0 as i16;
            rec.mvy[k] = info.mv[i].1 as i16;
            rec.ref_id[k] = info.rid(i);
            if has1 {
                let r1 = info.rid1(i);
                rec.ref1[k] = r1;
                rec.mvx1[k] = info.mv1[i].0 as i16;
                rec.mvy1[k] = info.mv1[i].1 as i16;
                if r1 != NO_REF {
                    rec.l1_used |= 1 << k;
                }
            }
        }
    }
    rec
}

/// FUSED pack+derive over the whole frame, with a TWO-ROW ROLLING WINDOW of
/// packed records (WHYS Part 16 root-cause lever, bounded form).
///
/// The frame-buffer pipeline was: pack ALL 3600 records into a ~1.1 MB Vec,
/// then a second interleaved loop re-reads them (plus a per-MB gate ladder) to
/// derive strengths. Here each record is DERIVED the moment it is built —
/// while it and its left/top neighbours are L1-hot — and only `2 × mb_w`
/// records (~50 KB at 720p) ever exist. Output is one `MbBs` (32 B) per
/// macroblock, which `filter_frame` consumes over its existing PRECOMPUTED
/// path, collapsing the per-MB derivation machinery in the hot loop entirely.
///
/// Byte-identical by the existing oracle chain: `derive_mb_records` is
/// `derive_mb_packed`'s own core (pinned to the tile twin by
/// `packed_matches_tile`), and the precomputed consumer path is pinned by the
/// encoder's use of it. `flat_inter` loop-skips are subsumed by the stored
/// zeros + the consuming loops' all-zero early-outs (documented there).
pub fn precompute_bs_frame(info: &BlockInfo, mb_w: usize, mb_h: usize, out: &mut Vec<MbBs>) {
    let has1 = !info.ref_id1.is_empty();
    out.clear();
    out.reserve(mb_w * mb_h);
    let mut prev_row: Vec<MbPack> = Vec::with_capacity(mb_w);
    let mut cur_row: Vec<MbPack> = Vec::with_capacity(mb_w);
    for mb_y in 0..mb_h {
        cur_row.clear();
        for mb_x in 0..mb_w {
            cur_row.push(pack_mb(info, has1, mb_x, mb_y));
            let cur = &cur_row[mb_x];
            let left = if mb_x > 0 { Some(&cur_row[mb_x - 1]) } else { None };
            let top = if mb_y > 0 { Some(&prev_row[mb_x]) } else { None };
            let mb_t8 = !info.t8x8.is_empty() && info.t8x8[mb_y * mb_w + mb_x];
            let (mut bv, mut bh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
            derive_mb_records(cur, left, top, mb_t8, &mut bv, &mut bh);
            let mut m = MbBs { v: [[0; 4]; 4], h: [[0; 4]; 4] };
            for e in 0..4 {
                for sg in 0..4 {
                    m.v[e][sg] = bv[e][sg] as u8;
                    m.h[e][sg] = bh[e][sg] as u8;
                }
            }
            out.push(m);
        }
        std::mem::swap(&mut prev_row, &mut cur_row);
    }
}

thread_local! {
    /// Recycled `pack_frame` buffer (~1.1 MB at 720p), `Cell` so `filter_frame`
    /// can `take`/`set` without holding a borrow across its whole MB loop.
    static PACK_SCRATCH: core::cell::Cell<Vec<MbPack>> = const { core::cell::Cell::new(Vec::new()) };
}

/// THE KERNEL INTERFACE — the whole motion half of the derivation as two bitmasks.
///
/// `bit k` of the returned pair answers "does block `k` differ in motion from its
/// LEFT neighbour block `k-1`" and "...from its ABOVE neighbour block `k-4`", using
/// the §8.7.2.1 rule that `bs_inter` applies per edge:
///
/// ```text
///   differs(a,b) = ref[a] != ref[b]  ||  (ref[a] != NO_REF && (|dmvx| >= 4 || |dmvy| >= 4))
/// ```
///
/// Bits at `k % 4 == 0` (left) and `k < 4` (up) are DON'T-CARE: those edges are
/// macroblock edges, derived separately against the neighbouring record. That is not
/// an accident of convenience — it is what makes the SIMD twin cheap, because the
/// lane positions a within-128-bit-lane shift corrupts are exactly those positions.
///
/// Collapsing the derivation to masks is the point of the packed layout: the 24
/// internal edges stop being 24 branchy `bs_inter` calls and become bit tests against
/// these masks OR'd with `nnz_mask`, which is the form `deblock_strength_avx2` works in.
/// Scalar twin of the uniform-motion test — the oracle, and the path on any CPU
/// without AVX2. Short-circuits, which is why the SIMD twin's win concentrates on
/// UNIFORM macroblocks (where this walks all 15 comparisons).
#[inline]
pub fn mb_uniform_scalar(p: &MbPack) -> bool {
    p.inter
        && (1..16).all(|k| {
            p.ref_id[k] == p.ref_id[0]
                && p.mvx[k] == p.mvx[0]
                && p.mvy[k] == p.mvy[0]
                && p.ref1[k] == p.ref1[0]
                && p.mvx1[k] == p.mvx1[0]
                && p.mvy1[k] == p.mvy1[0]
        })
}

/// Dispatcher: AVX2 when present, scalar otherwise. `p.inter` is a per-macroblock
/// flag and is checked here, so the kernel only ever answers the 16-lane question.
#[inline]
pub fn mb_uniform(p: &MbPack) -> bool {
    #[cfg(all(target_arch = "x86_64", feature = "asm"))]
    if p.inter {
        if let Some(u) =
            rusty_h264_accel::mb_uniform(&p.mvx, &p.mvy, &p.ref_id, &p.mvx1, &p.mvy1, &p.ref1)
        {
            return u;
        }
    }
    mb_uniform_scalar(p)
}

#[inline]
pub fn bs_motion_masks_scalar(p: &MbPack) -> (u16, u16) {
    let differs = |a: usize, b: usize| -> bool { pk_differs(p, a, p, b) };
    let (mut left, mut up) = (0u16, 0u16);
    for k in 0..16usize {
        if k % 4 != 0 && differs(k, k - 1) {
            left |= 1 << k;
        }
        if k >= 4 && differs(k, k - 4) {
            up |= 1 << k;
        }
    }
    // Don't-care bits are already zero here by construction; the masks are spelled out
    // so the SIMD twin (which must clear them explicitly) is comparable on the FULL u16.
    (left & 0xEEEE, up & 0xFFF0)
}

/// Dispatcher: the AVX2 twin when the CPU has it, the scalar oracle otherwise.
///
/// The scalar version stays the default on any non-AVX2 CPU and remains the
/// reference the SIMD twin is gated against — it is never deleted.
#[inline]
pub fn bs_motion_masks(p: &MbPack) -> (u16, u16) {
    #[cfg(feature = "profile")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        census::MASKS_CALLS.fetch_add(1, Relaxed);
        if p.l1_used == 0 {
            census::MASKS_KERNEL.fetch_add(1, Relaxed);
        } else {
            census::MASKS_L1.fetch_add(1, Relaxed);
        }
    }
    #[cfg(all(target_arch = "x86_64", feature = "asm"))]
    if p.l1_used == 0 {
        // Single-list fast kernel: all of P and uni-L0 B macroblocks.
        if let Some(m) = rusty_h264_accel::bs_motion_masks(&p.mvx, &p.mvy, &p.ref_id, NO_REF) {
            return m;
        }
    } else {
        // TWO-LIST kernel (WHYS Part 16's named lever): the §8.7.2.1
        // set-matching rule vectorized, so B macroblocks with List-1 slots no
        // longer fall to the scalar per-edge walk. Gated bit-exact against the
        // scalar twin by `bs_motion_masks_two_list_matches_scalar` (accel) and
        // the unmasked runtime oracle (`RS_H264_VERIFY_PACKED=1`).
        if let Some(m) = rusty_h264_accel::bs_motion_masks_two_list(
            &p.mvx, &p.mvy, &p.ref_id, &p.mvx1, &p.mvy1, &p.ref1, NO_REF,
        ) {
            return m;
        }
    }
    bs_motion_masks_scalar(p)
}

/// The §8.7.2.1 motion test over packed operands — the EXACT twin of `bs1_tile`,
/// including the two-list set-matching rule (which is order-independent: a block pair
/// whose two reference pictures match after a SWAP is not "different motion").
///
/// The single-list fast path fires whenever neither side carries a List-1 slot, which
/// is all of P and most uni-predicted B blocks; only the remainder pays the set match.
#[inline]
fn pk_differs(p: &MbPack, pk: usize, q: &MbPack, qk: usize) -> bool {
    let p_has1 = p.ref1[pk] != NO_REF;
    let q_has1 = q.ref1[qk] != NO_REF;
    if !p_has1 && !q_has1 {
        let far = ((p.mvx[pk] - q.mvx[qk]).abs() >= 4) | ((p.mvy[pk] - q.mvy[qk]).abs() >= 4);
        return (p.ref_id[pk] != q.ref_id[qk]) | ((p.ref_id[pk] != NO_REF) & far);
    }
    // Collect the USED reference slots, exactly as `bs1_tile::used` does.
    let used = |b: &MbPack, k: usize| {
        let mut v = [(0i32, (0i32, 0i32)); 2];
        let mut n = 0usize;
        if b.ref_id[k] != NO_REF {
            v[n] = (b.ref_id[k], (b.mvx[k] as i32, b.mvy[k] as i32));
            n += 1;
        }
        if b.ref1[k] != NO_REF {
            v[n] = (b.ref1[k], (b.mvx1[k] as i32, b.mvy1[k] as i32));
            n += 1;
        }
        (v, n)
    };
    let (pv, pn) = used(p, pk);
    let (qv, qn) = used(q, qk);
    if pn != qn {
        return true;
    }
    let far = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() >= 4 || (a.1 - b.1).abs() >= 4;
    match pn {
        0 => false,
        1 => pv[0].0 != qv[0].0 || far(pv[0].1, qv[0].1),
        _ => {
            let direct = !far(pv[0].1, qv[0].1) && !far(pv[1].1, qv[1].1);
            let swap = !far(pv[0].1, qv[1].1) && !far(pv[1].1, qv[0].1);
            if pv[0].0 == pv[1].0 {
                qv[0].0 != pv[0].0 || qv[1].0 != pv[0].0 || !(direct || swap)
            } else if pv[0].0 == qv[0].0 && pv[1].0 == qv[1].0 {
                !direct
            } else if pv[0].0 == qv[1].0 && pv[1].0 == qv[0].0 {
                !swap
            } else {
                true
            }
        }
    }
}

#[inline]
fn pk_nz(p: &MbPack, k: usize) -> bool {
    (p.nnz_mask >> k) & 1 != 0
}

/// `bs_inter` over packed operands — both sides inter, so 3 and 4 are unreachable.
/// Mirrors `bs1_tile`'s single-list fast path exactly.
#[inline]
fn pk_bs_inter(p: &MbPack, pk: usize, q: &MbPack, qk: usize) -> i32 {
    if pk_nz(p, pk) | pk_nz(q, qk) {
        return 2;
    }
    let (pr, qr) = (p.ref_id[pk], q.ref_id[qk]);
    let _ = (pr, qr);
    pk_differs(p, pk, q, qk) as i32
}

/// Derive one macroblock's strengths from PACKED records — the byte-identical twin of
/// `derive_mb_bs`, pinned against it by `packed_matches_tile`.
pub fn derive_mb_packed(
    packs: &[MbPack],
    mb_w: usize,
    mb_x: usize,
    mb_y: usize,
    mb_t8: bool,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) -> bool {
    let cur = &packs[mb_y * mb_w + mb_x];
    let left = if mb_x > 0 { Some(&packs[mb_y * mb_w + mb_x - 1]) } else { None };
    let top = if mb_y > 0 { Some(&packs[(mb_y - 1) * mb_w + mb_x]) } else { None };
    derive_mb_records(cur, left, top, mb_t8, bs_v, bs_h)
}

/// The record-based core of [`derive_mb_packed`]: derive one macroblock's
/// strengths from ITS OWN record plus the left/top neighbours' — the shape the
/// rolling-window precompute pass ([`precompute_bs_frame`]) needs, where only
/// two rows of records exist at a time.
pub fn derive_mb_records(
    cur: &MbPack,
    left: Option<&MbPack>,
    top: Option<&MbPack>,
    mb_t8: bool,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) -> bool {
    let cur_intra = !cur.inter;

    // Both predicates from the packed record: uniform motion needs no neighbour, and
    // "no coefficients" is `nnz_mask == 0` — a single register test replacing a
    // 16-block walk.
    //
    // REFUTED (WHYS Part 16, z=-2.11 REGRESSION, reverted): deriving uniformity
    // from `bs_motion_masks` to save "the second kernel call". The two calls are
    // ASYMMETRIC: this one is a cheap AVX2 compare-to-block-0 that handles
    // two-list B data in vector code, while the masks kernel falls to the SCALAR
    // set-matching walk whenever `l1_used != 0` — most B inter MBs. The
    // population reaching this function on B frames is uniform-heavy, so the
    // "fusion" replaced their one cheap vector call with an expensive scalar
    // walk. Do not retry without making the masks kernel two-list-capable first.
    let uniform = mb_uniform(cur);
    let flat_inter = uniform && cur.nnz_mask == 0;

    if let Some(l) = left {
        bs_v[0] = if cur_intra || !l.inter {
            [4; 4]
        } else {
            std::array::from_fn(|seg| pk_bs_inter(l, seg * 4 + 3, cur, seg * 4))
        };
    }
    if let Some(t) = top {
        bs_h[0] = if cur_intra || !t.inter {
            [4; 4]
        } else {
            std::array::from_fn(|seg| pk_bs_inter(t, 12 + seg, cur, seg))
        };
    }

    if flat_inter {
        return true; // internal strengths are 0 by construction
    }
    // Only the general path consumes these; intra fills constants and uniform motion
    // reads coefficients alone.
    let masks = if cur_intra || uniform { (0, 0) } else { bs_motion_masks(cur) };
    for be in 1..4usize {
        if mb_t8 && (be == 1 || be == 3) {
            continue;
        }
        if cur_intra {
            bs_v[be] = [3; 4];
            bs_h[be] = [3; 4];
        } else if uniform {
            // Coefficients alone; the whole edge group is a shift-and-or on the mask.
            bs_v[be] = std::array::from_fn(|seg| {
                2 * (pk_nz(cur, seg * 4 + be) | pk_nz(cur, seg * 4 + be - 1)) as i32
            });
            bs_h[be] = std::array::from_fn(|seg| {
                2 * (pk_nz(cur, be * 4 + seg) | pk_nz(cur, (be - 1) * 4 + seg)) as i32
            });
        } else {
            // The general path, now pure bit tests: coefficients from `nnz_mask`,
            // motion from the precomputed masks. No per-edge branching at all.
            let (left, up) = masks;
            bs_v[be] = std::array::from_fn(|seg| {
                let k = seg * 4 + be;
                if pk_nz(cur, k) | pk_nz(cur, k - 1) {
                    2
                } else {
                    ((left >> k) & 1) as i32
                }
            });
            bs_h[be] = std::array::from_fn(|seg| {
                let k = be * 4 + seg;
                if pk_nz(cur, k) | pk_nz(cur, k - 4) {
                    2
                } else {
                    ((up >> k) & 1) as i32
                }
            });
        }
    }
    false
}

/// ONE walk of the macroblock's own 16 blocks yielding BOTH predicates the
/// derivation needs: `(uniform_motion, flat_inter)`.
///
/// They are the same walk, by construction:
/// ```text
///   uniform_motion = b0.inter AND every block { inter AND same_motion(b0) }
///   flat_inter     = uniform_motion AND no block carries coefficients
/// ```
/// They used to be two independent 16-block scans — and the `uniform_motion` one
/// ran BEFORE `derive_mb_bs`'s `if flat_inter { return }` early-out, so every Skip
/// macroblock paid a full 16-block × 6-compare motion scan whose result was then
/// discarded. The MB-kind census measures Skip at 36.4% (CAVLC) / 65.0% (main) /
/// 57.8% (high) of macroblocks, so that dead scan ran on most of the corpus.
///
/// Byte-identical to the two-scan form: when the walk breaks early both
/// predicates are false, exactly as both original `.all()` chains would be.
#[inline]
fn scan_uniform_flat(tile: &Tile) -> (bool, bool) {
    let b0 = &tile[1][1];
    if !b0.inter {
        return (false, false); // intra MB — neither predicate applies
    }
    let mut any_nz = false;
    for r in 1..5 {
        for c in 1..5 {
            let b = &tile[r][c];
            #[cfg(feature = "profile")]
            census::PRED_VISITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if !b.inter || !b.same_motion(b0) {
                return (false, false);
            }
            any_nz |= b.nz;
        }
    }
    (true, !any_nz)
}

/// The pre-fusion arm, kept verbatim as the measurement baseline AND the oracle.
#[inline]
fn scan_two_pass(tile: &Tile) -> (bool, bool) {
    let b0 = &tile[1][1];
    let flat = b0.inter
        && (1..5).all(|r| {
            (1..5).all(|c| {
                let b = &tile[r][c];
                #[cfg(feature = "profile")]
                census::PRED_VISITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                b.inter && !b.nz && b.same_motion(b0)
            })
        });
    let uniform = b0.inter
        && (1..5).all(|r| {
            (1..5).all(|c| {
                #[cfg(feature = "profile")]
                census::PRED_VISITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tile[r][c].inter && tile[r][c].same_motion(b0)
            })
        });
    (uniform, flat)
}

/// MEASUREMENT SWITCH — `RS_H264_BS_TWOPASS=1` restores the two independent
/// scans. Both arms live in ONE binary so a bench can alternate them under one
/// thermal state; separate builds on this box drift ~20% run-to-run, which cannot
/// resolve an effect this size. Read once; the branch predicts perfectly.
static BS_TWOPASS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Resolve the arm ONCE — hoist this out of any per-macroblock path.
///
/// It used to be read inside `scan_predicates`, i.e. an atomic load plus a branch
/// on every one of 6.48M macroblocks, present in BOTH arms. That is measurement
/// overhead added in order to take a ~2% measurement, and it is the same mistake
/// this workspace has recorded before (a dedup that replaced cheap work with a
/// dependent load + branch and went backwards). Resolve per frame, pass the bool.
fn bs_twopass() -> bool {
    use std::sync::atomic::Ordering;
    match BS_TWOPASS.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RS_H264_BS_TWOPASS").is_some_and(|v| v != "0");
            BS_TWOPASS.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// MEASUREMENT SWITCH — `RS_H264_NO_MBKIND=1` ignores producer-supplied classes and
/// forces the blind gather for every macroblock, so the kind-aware brick can be
/// alternated against its own baseline inside ONE binary. Resolved ONCE per frame by
/// the caller and passed down: reading it per macroblock would put an atomic load and
/// a branch in both arms of the very thing being measured (see `bs_twopass`).
static NO_MBKIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// CORRECTNESS GATE — `RS_H264_VERIFY_MBKIND=1` derives EVERY kind-classified
/// macroblock BOTH ways and asserts they agree. This is the oracle for the whole
/// brick: a producer that mislabels a macroblock (say, calling a `B_Skip` a `Skip`
/// when direct-derived motion differs per 4x4) changes strengths silently, and the
/// byte-identical corpus gate can only tell you THAT something broke, not where.
/// Run it over the corpus once per producer change; it is far too slow to ship.
fn verify_kind() -> bool {
    use std::sync::atomic::Ordering;
    static V: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    match V.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RS_H264_VERIFY_MBKIND").is_some_and(|v| v != "0");
            V.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Compare kind-derived against blind-derived strengths, looking ONLY at the edges
/// the consuming loops actually read. `derive_mb_kind` fills internal groups 1 and 3
/// even for an 8x8-transform macroblock (whose consumers skip them), so a raw
/// array compare would report a difference that cannot reach a pixel.
#[allow(clippy::too_many_arguments)]
fn verify_kind_matches_blind(
    info: &BlockInfo,
    mb_x: usize,
    mb_y: usize,
    mb_t8: bool,
    kind: MbKind,
    flat_inter: bool,
    bs_v: &[[i32; 4]; 4],
    bs_h: &[[i32; 4]; 4],
) {
    let tile = gather_tile(info, mb_x, mb_y);
    let (u, f) = scan_uniform_flat(&tile);
    let (mut vv, mut vh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
    derive_mb_bs(&tile, mb_x, mb_y, f, u, mb_t8, &mut vv, &mut vh);
    assert_eq!(
        flat_inter, f,
        "MB ({mb_x},{mb_y}) kind {kind:?}: flat_inter {flat_inter} but blind derived {f}"
    );
    for be in 0..4usize {
        if be == 0 {
            if mb_x > 0 {
                assert_eq!(bs_v[0], vv[0], "MB ({mb_x},{mb_y}) kind {kind:?}: left MB edge");
            }
            if mb_y > 0 {
                assert_eq!(bs_h[0], vh[0], "MB ({mb_x},{mb_y}) kind {kind:?}: top MB edge");
            }
            continue;
        }
        if flat_inter || (mb_t8 && (be == 1 || be == 3)) {
            continue; // consumers skip these entirely
        }
        assert_eq!(bs_v[be], vv[be], "MB ({mb_x},{mb_y}) kind {kind:?}: v internal edge {be}");
        assert_eq!(bs_h[be], vh[be], "MB ({mb_x},{mb_y}) kind {kind:?}: h internal edge {be}");
    }
}

/// OPT-IN — `RS_H264_BS_PACKED=1` derives strengths from the packed per-macroblock
/// records instead of the strided gather. Default OFF: the ceiling probe measured the
/// direct memory saving at only ~19 ns/MB of a ~400 ns/MB derivation, so this exists
/// as the ENABLER for a vectorised kernel, not as a memory win in its own right.
/// Resolved once per frame and passed down, never read per macroblock.
/// CORRECTNESS PROBE — `RS_H264_VERIFY_PACKED=1` derives every packed macroblock BOTH
/// ways on real bitstreams and reports the first divergence with its coordinates. The
/// unit oracle passes on a synthetic grid; the corpus does not. This names WHERE.
fn verify_packed() -> bool {
    use std::sync::atomic::Ordering;
    static V: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    match V.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("RS_H264_VERIFY_PACKED").is_some_and(|v| v != "0");
            V.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

fn bs_packed_on() -> bool {
    use std::sync::atomic::Ordering;
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    match ON.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            // DEFAULT ON since 2026-08-02. Polarity written as an explicit opt-OUT so
            // it is a decision, not a flag accident: an ABSENT variable means the fast
            // path, and only the literal "0" restores the blind gather.
            //
            // Promoted on three independent interleaved runs (main corpus, 1800 frames
            // 720p, pinned, CPU time, ABBA), each with a null arm in the same session:
            //   packed layout alone   median +1.7%    8/9,  z = 2.33
            //   + both AVX2 kernels   median +6.7%   11/15, z = 1.81
            //   + both AVX2 kernels   median +3.3%   12/15, z = 2.32  <- deciding run
            //   null arms             1.000 / 1.039         z = -1.13 / 0.38
            // The two full-stack runs pool to 23/30, z = 2.92.
            //
            // Honest shape: single-run medians ranged 1.7-6.7% because this box drifts,
            // so the WIN RATE carries the verdict; the median is the effect-size
            // estimate, not the proof.
            let off = std::env::var_os("RS_H264_BS_PACKED").is_some_and(|v| v == "0");
            ON.store(if off { 2 } else { 1 }, Ordering::Relaxed);
            !off
        }
    }
}

fn kind_gate_off() -> bool {
    use std::sync::atomic::Ordering;
    match NO_MBKIND.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let off = std::env::var_os("RS_H264_NO_MBKIND").is_some_and(|v| v != "0");
            NO_MBKIND.store(if off { 1 } else { 2 }, Ordering::Relaxed);
            off
        }
    }
}

#[inline]
fn scan_predicates(tile: &Tile, two_pass: bool) -> (bool, bool) {
    if two_pass {
        scan_two_pass(tile)
    } else {
        scan_uniform_flat(tile)
    }
}

pub fn derive_mb(info: &BlockInfo, mb_x: usize, mb_y: usize, mb_t8: bool) -> MbBs {
    let tile = gather_tile(info, mb_x, mb_y);
    let (uniform_motion, flat_inter) = scan_predicates(&tile, bs_twopass());
    let (mut bs_v, mut bs_h) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
    derive_mb_bs(&tile, mb_x, mb_y, flat_inter, uniform_motion, mb_t8, &mut bs_v, &mut bs_h);
    let pack = |a: [[i32; 4]; 4]| a.map(|e| e.map(|x| x as u8));
    MbBs { v: pack(bs_v), h: pack(bs_h) }
}

/// The 5×5 neighbourhood an MB's edges can reach: row/col 0 are the top and left
/// neighbour blocks, rows/cols 1..=4 the MB's own 4×4 grid. Entries outside the
/// picture stay `Default` and are never read (the frame-edge groups are skipped).
type Tile = [[Blk; 5]; 5];

/// Gather the tile for macroblock (`mb_x`, `mb_y`).
fn gather_tile(info: &BlockInfo, mb_x: usize, mb_y: usize) -> Tile {
    let mut t: Tile = Default::default();
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    // The MB's own blocks: four contiguous runs of four.
    for r in 0..4 {
        let row = (by0 + r) * info.w4 + bx0;
        for c in 0..4 {
            t[r + 1][c + 1] = Blk::load(info, row + c);
        }
    }
    if mb_x > 0 {
        for r in 0..4 {
            t[r + 1][0] = Blk::load(info, (by0 + r) * info.w4 + bx0 - 1);
        }
    }
    if mb_y > 0 {
        let row = (by0 - 1) * info.w4 + bx0;
        for c in 0..4 {
            t[0][c + 1] = Blk::load(info, row + c);
        }
    }
    // `mb_type` is a per-macroblock syntax element, so every 4×4 block of a
    // macroblock shares its intra/inter status. `derive_mb_bs` depends on this to
    // replace per-edge intra tests with per-macroblock constant fills.
    debug_assert!(
        (1..5).all(|r| (1..5).all(|c| t[r][c].inter == t[1][1].inter)),
        "macroblock ({mb_x},{mb_y}) mixes intra and inter 4x4 blocks"
    );
    t
}

/// Selects the boundary-strength derivation. Default (and shipped path) is the
/// per-MB TILE; `RS_H264_DEBLOCK_BRANCHY=1` restores the original per-edge
/// derivation straight off the frame arrays.
///
/// This exists so both arms live in ONE binary and a benchmark can alternate
/// them under the same thermal state — comparing separate builds on this machine
/// has ~20% run-to-run drift, which cannot resolve the effect being measured.
/// It doubles as the fallback switch. Read once; the branch predicts perfectly.
static BS_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn branchless_bs() -> bool {
    use std::sync::atomic::Ordering;
    match BS_MODE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let branchy = std::env::var_os("RS_H264_DEBLOCK_BRANCHY").is_some_and(|v| v != "0");
            BS_MODE.store(if branchy { 2 } else { 1 }, Ordering::Relaxed);
            !branchy
        }
    }
}

/// Whether the per-MB tile path is active.
fn deblock_tile() -> bool {
    branchless_bs()
}

/// DEFAULT OFF. Deriving boundary strengths in the encode loop makes the
/// deblocking stage 1.4-1.7x faster but does NOT reduce total encode time: the
/// block grids were never cold (~90 KB at CIF, L2-resident), so the derivation
/// costs the same in a streaming pass, and the encode loop's contended working
/// set makes it cost MORE there — measured, the loop grew about twice the
/// derivation's own cost. Kept behind this switch with its tests, because the
/// machinery is what a future commit-time derivation (values still in registers,
/// no grid re-read) would build on.
static BS_PRECOMP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Whether callers may supply precomputed boundary strengths (the encoder path).
pub fn precomputed_bs_enabled() -> bool {
    BS_PRECOMP.load(std::sync::atomic::Ordering::Relaxed) != 0
}

/// Toggle the precomputed-strength path so a benchmark can ALTERNATE the two
/// designs inside ONE process.
#[doc(hidden)]
pub fn set_precomputed_bs(on: bool) {
    BS_PRECOMP.store(on as u8, std::sync::atomic::Ordering::Relaxed);
}

/// Force the deblocking boundary-strength arm at runtime. Exists so a benchmark
/// can ALTERNATE the arms inside one process under one thermal state; comparing
/// separate builds cannot resolve the effect on this machine.
#[doc(hidden)]
pub fn set_branchless_bs(on: bool) {
    BS_MODE.store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
}

impl BlockInfo<'_> {
    #[inline]
    fn at(&self, bx: usize, by: usize) -> usize {
        by * self.w4 + bx
    }

    /// Boundary strength between left/above block `p` and current block `q`
    /// (spec §8.7.2.1). `mb_edge` is true on macroblock boundaries.
    ///
    /// Written branchlessly on purpose. Real content mixes intra/inter, coded and
    /// uncoded blocks, and per-block motion, so the natural short-circuit form
    /// (`if intra … else if nnz … else if motion …`) mispredicts on nearly every
    /// 4×4 edge: the anatomy bench measures the identical code at ~240 ns/MB on
    /// uniform data and ~515 ns/MB once the block state varies, which is the
    /// whole of our gap to x264 here (x264 derives bS with a branchless SIMD
    /// kernel). Evaluating all three candidates costs a few extra loads and beats
    /// paying a ~15-cycle mispredict per edge.
    #[inline]
    fn bs(&self, p: usize, q: usize, mb_edge: bool) -> i32 {
        if branchless_bs() {
            self.bs_branchless(p, q, mb_edge)
        } else {
            self.bs_branchy(p, q, mb_edge)
        }
    }

    /// The original short-circuit form, kept as the A/B arm and the fallback.
    /// Identical output to [`Self::bs_branchless`] by construction.
    fn bs_branchy(&self, p: usize, q: usize, mb_edge: bool) -> i32 {
        if !self.inter[p] || !self.inter[q] {
            if mb_edge {
                4
            } else {
                3
            }
        } else if self.nnz[p] > 0 || self.nnz[q] > 0 {
            2
        } else if self.inter_bs1(p, q) {
            1
        } else {
            0
        }
    }

    #[inline]
    fn bs_branchless(&self, p: usize, q: usize, mb_edge: bool) -> i32 {
        let intra = !(self.inter[p] & self.inter[q]);
        let nz = (self.nnz[p] | self.nnz[q]) != 0;
        let moved = self.inter_bs1(p, q);
        let intra_bs = if mb_edge { 4 } else { 3 };
        // Priority intra > coefficients > motion, as selects rather than branches.
        let motion_bs = moved as i32; // 1 or 0
        let non_intra = if nz { 2 } else { motion_bs };
        if intra {
            intra_bs
        } else {
            non_intra
        }
    }

    /// Whether two residual-free inter blocks get boundary strength 1: they use
    /// different reference pictures, a different number of motion vectors, or a
    /// motion vector differs by ≥ 1 full sample (matched by reference picture, so
    /// the same picture in different lists is recognised). Spec §8.7.2.1.
    fn inter_bs1(&self, p: usize, q: usize) -> bool {
        // Single-list fast path (P and I slices — `ref_id1` empty). This is the
        // overwhelmingly common case, and it collapses to two comparisons: the
        // general path below builds a two-slot [(ref, mv); 2] array per side and
        // matches on the slot count, which the anatomy bench showed dominates
        // deblocking. Exactly equivalent here: with one list, `pn`/`qn` are just
        // "is the slot used", so a differing ref_id covers both the
        // different-count and different-picture cases, and two unused slots give
        // pn == qn == 0 => false.
        if self.ref_id1.is_empty() {
            // Branchless (see `bs`): `|`/`&` rather than `||`/`&&` so there is no
            // data-dependent branch here either. The single `is_empty` test above
            // is uniform across a whole frame and predicts perfectly.
            let (rp, rq) = (self.ref_id[p], self.ref_id[q]);
            let (a, b) = (self.mv[p], self.mv[q]);
            let far = ((a.0 - b.0).abs() >= 4) | ((a.1 - b.1).abs() >= 4);
            // Differing refs ⇒ bS 1 (this also covers "one slot used, one not",
            // the general path's differing-count case). Both unused ⇒ 0.
            return (rp != rq) | ((rp != NO_REF) & far);
        }
        // (reference id, motion vector) for each used prediction slot.
        let used = |i: usize| {
            let mut v = [(0i32, (0i32, 0i32)); 2];
            let mut n = 0;
            if self.ref_id[i] != NO_REF {
                v[n] = (self.ref_id[i], self.mv[i]);
                n += 1;
            }
            // `ref_id1` may be empty (P frames have no List-1 — the caller skips
            // building it, since every entry would be NO_REF anyway).
            if !self.ref_id1.is_empty() && self.ref_id1[i] != NO_REF {
                v[n] = (self.ref_id1[i], self.mv1[i]);
                n += 1;
            }
            (v, n)
        };
        let (pv, pn) = used(p);
        let (qv, qn) = used(q);
        if pn != qn {
            return true; // different number of motion vectors
        }
        let far = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() >= 4 || (a.1 - b.1).abs() >= 4;
        match pn {
            0 => false,
            1 => pv[0].0 != qv[0].0 || far(pv[0].1, qv[0].1),
            _ => {
                // Two references each: the picture *sets* must match, and the
                // motion vectors for corresponding pictures must be close. If both
                // slots are the same picture, either pairing is acceptable.
                let direct = !far(pv[0].1, qv[0].1) && !far(pv[1].1, qv[1].1);
                let swap = !far(pv[0].1, qv[1].1) && !far(pv[1].1, qv[0].1);
                if pv[0].0 == pv[1].0 {
                    qv[0].0 != pv[0].0 || qv[1].0 != pv[0].0 || !(direct || swap)
                } else if pv[0].0 == qv[0].0 && pv[1].0 == qv[1].0 {
                    !direct
                } else if pv[0].0 == qv[1].0 && pv[1].0 == qv[0].0 {
                    !swap
                } else {
                    true // different picture sets
                }
            }
        }
    }
}

/// Applies the deblocking filter in place to a fully-reconstructed frame. `qp`
/// is the (constant) luma QP, `qpc` the chroma QP, and `info` supplies the
/// per-block state used to derive boundary strengths (for an all-intra frame
/// this reduces to the fixed 4/3 strengths).
/// Edge thresholds `(α, β, tc0[bS-1])` for a given averaged QP and the slice's
/// filter offsets (spec §8.7.2.2): α/tc0 indexed by `indexA`, β by `indexB`.
#[inline]
fn thresholds(qpav: i32, offset_a: i32, offset_b: i32) -> (i32, i32, [i32; 3]) {
    let ia = (qpav + offset_a).clamp(0, 51) as usize;
    let ib = (qpav + offset_b).clamp(0, 51) as usize;
    (ALPHA[ia], BETA[ib], TC0[ia])
}

#[allow(clippy::too_many_arguments)]
pub fn filter_frame(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    mb_w: usize,
    mb_h: usize,
    mb_qp: &[u8],
    chroma_qp_offset: i32,
    offset_a: i32,
    offset_b: i32,
    info: &BlockInfo,
) {
    filter_frame_rows(y, u, v, mb_w, mb_h, 0..mb_h, mb_qp, chroma_qp_offset, offset_a, offset_b, info)
}

/// [`filter_frame`] restricted to macroblock rows `rows` — the row-interleave
/// campaign's unit (docs/row-interleave-plan.md R3): the decoder filters row
/// `r` the moment it finishes decoding, spec raster order preserved exactly.
#[allow(clippy::too_many_arguments)]
pub fn filter_frame_rows(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    mb_w: usize,
    mb_h: usize,
    rows: core::ops::Range<usize>,
    mb_qp: &[u8],
    chroma_qp_offset: i32,
    offset_a: i32,
    offset_b: i32,
    info: &BlockInfo,
) {
    let _g = crate::prof::scope(crate::prof::Stage::Deblock);
    let cw = mb_w * 16;
    let ccw = mb_w * 8;
    // Per-edge QP: deblock strength uses the average of the two adjacent
    // macroblocks' QPy (spec §8.7.2). For an internal edge both sides share the
    // current MB's QP. Chroma averages the two MBs' QPc.
    let qpy = |mx: usize, my: usize| mb_qp[my * mb_w + mx] as i32;
    let qpc = |qpy_val: i32| {
        crate::predict::chroma_qp((qpy_val + chroma_qp_offset).clamp(0, 51) as u8) as i32
    };
    // Arms resolved ONCE per frame, never per macroblock (see `bs_twopass`).
    let two_pass = bs_twopass();
    let kind_off = kind_gate_off();
    let verify_kinds = verify_kind();
    // ONE streaming pass over the frame arrays, not 3600 scattered gathers. `None`
    // when the frame carries List-1 data (B slices), which MbPack does not model —
    // those macroblocks keep the blind path.
    let scratch = PACK_SCRATCH.take();
    let packs: Option<Vec<MbPack>> = if bs_packed_on() && info.bs.is_empty() && deblock_tile() {
        let mut buf = scratch;
        {
            let _pg = crate::prof::scope(crate::prof::Stage::DebPack);
            pack_frame_into(info, mb_w, mb_h, &mut buf);
        }
        Some(buf)
    } else {
        PACK_SCRATCH.set(scratch);
        None
    };

    for mb_y in rows {
        for mb_x in 0..mb_w {
            // `t8x8` may be empty (no MB uses the 8×8 transform — Baseline); treat
            // an empty grid as all-false so the caller can skip allocating it.
            let mb_t8 = !info.t8x8.is_empty() && info.t8x8[mb_y * mb_w + mb_x];
            // A "flat inter MB" — every 4x4 inter, zero nnz, one (ref, mv) pair (e.g.
            // any skip MB) — has bs = 0 on ALL its internal edges by §8.7.2.1 (no
            // coefficients, same reference, identical motion), so the six internal
            // edge groups can be skipped wholesale. Byte-identical control flow.
            // Gather the MB's 4×4 block state once (see `Blk`). Every boundary
            // strength below then reads the tile instead of the strided frame
            // arrays. Only valid for a single reference list; B slices keep the
            // original per-edge path untouched.
            // `deblock_tile()` selects the arm so a bench can alternate the whole
            // brick (tile vs the original per-edge derivation) in one process.
            // Precomputed strengths short-circuit the gather AND the derivation;
            // flat_inter and the t8x8 skips are already baked into the stored
            // zeros, which the all-zero early-out below handles identically.
            let _dg = crate::prof::scope(crate::prof::Stage::DebDerive);
            let precomputed = !info.bs.is_empty();
            // H-33: the tile arm now carries the two-list B rule (`bs1_tile`), so
            // real-world B frames no longer fall back to the strided per-edge path.
            let use_tile = !precomputed && deblock_tile();
            let have_bs = precomputed || use_tile;

            // KIND-AWARE FAST PATH. When the producer has classified this
            // macroblock, its strengths follow from syntax and the 24-block
            // neighbourhood gather is not needed at all: Intra reads NOTHING, Skip
            // reads 9 blocks, InterUniform reads 16 nnz bytes. `Inter` and UNSET
            // fall through to the blind path below, so an unclassified macroblock
            // costs speed and never correctness.
            //
            // The gather is skipped by CONSTRUCTION here rather than by an early-out
            // inside it — building the 5x5 `Tile` at all is an 800-byte
            // default-initialisation before 24 of its 25 entries are overwritten.
            let fast_kind = if use_tile && !kind_off {
                match info.kind.get(mb_y * mb_w + mb_x).copied().and_then(MbKind::from_u8) {
                    Some(k) if k != MbKind::Inter => Some(k),
                    _ => None,
                }
            } else {
                None
            };

            let blind_tile = use_tile && fast_kind.is_none() && packs.is_none();
            // Declared before the predicate chain: the packed derivation produces
            // `flat_inter` and the strengths in ONE traversal, so it cannot be split
            // across the two chains the way the tile path is.
            let mut bs_v = [[0i32; 4]; 4];
            let mut bs_h = [[0i32; 4]; 4];
            let packed_mb = packs.as_ref().filter(|_| use_tile && fast_kind.is_none());
            // MATERIALISE THE TILE ONLY ON THE BLIND PATH. Writing
            // `else { Default::default() }` here cost an 800-byte zero-init of the
            // 5x5 `Tile` on every CLASSIFIED macroblock — i.e. this brick removed a
            // gather and added ~4 GB of memset, and measured 1.2-17.0% SLOWER in
            // 4/4 pairs. `Option` keeps the fast path free of it entirely.
            let tile = if blind_tile { Some(gather_tile(info, mb_x, mb_y)) } else { None };

            // ONE walk yields both predicates — see `scan_uniform_flat`. The
            // non-tile arm has no `uniform_motion` consumer (it derives per-edge
            // off the frame arrays), so it keeps returning `flat_inter` alone.
            let mut uniform_motion = false;
            let flat_inter = if precomputed {
                false // the stored zeros already encode it
            } else if let Some(k) = fast_kind {
                match k {
                    // No coefficients and one shared (ref, mv): every internal
                    // strength is 0 by construction.
                    MbKind::Skip => true,
                    // CAUGHT BY THE ORACLE: a single-partition inter macroblock
                    // with NO coefficients is flat too — uniform motion means the
                    // blind predicate reduces to exactly "no block has nnz".
                    // Hardcoding `false` here was pixel-identical (the strengths
                    // are all 0 either way) but made the consuming loops walk the
                    // internal edge groups instead of skipping them, throwing away
                    // part of the win. 16 contiguous nnz bytes — the same data
                    // `derive_mb_kind` already reads for this class.
                    MbKind::InterUniform => {
                        let (bx0, by0) = (mb_x * 4, mb_y * 4);
                        (0..4).all(|r| {
                            (0..4).all(|c| info.nnz[(by0 + r) * info.w4 + bx0 + c] == 0)
                        })
                    }
                    _ => false,
                }
            } else if let Some(pk) = packed_mb {
                #[cfg(feature = "profile")]
                census::PACKED_MB.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let pflat = derive_mb_packed(pk, mb_w, mb_x, mb_y, mb_t8, &mut bs_v, &mut bs_h);
                if verify_packed() {
                    // UNMASKED: all 32 strengths, not just the ones the consuming
                    // loops read. A masked oracle can pass while the corpus diverges.
                    let t = gather_tile(info, mb_x, mb_y);
                    let (u, f) = scan_uniform_flat(&t);
                    let (mut tv, mut th) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    derive_mb_bs(&t, mb_x, mb_y, f, u, mb_t8, &mut tv, &mut th);
                    assert_eq!(pflat, f, "MB ({mb_x},{mb_y}) t8={mb_t8}: flat_inter");
                    assert_eq!(bs_v, tv, "MB ({mb_x},{mb_y}) t8={mb_t8} flat={f}: bs_v");
                    assert_eq!(bs_h, th, "MB ({mb_x},{mb_y}) t8={mb_t8} flat={f}: bs_h");
                }
                pflat
            } else if blind_tile {
                let (u, f) = scan_predicates(tile.as_ref().unwrap(), two_pass);
                uniform_motion = u;
                f
            } else {
                let b0 = info.at(mb_x * 4, mb_y * 4);
                let mut ok = info.inter[b0];
                if ok {
                    let (r0, m0) = (info.ref_id[b0], info.mv[b0]);
                    let has1 = !info.ref_id1.is_empty();
                    let (r10, m10) = if has1 { (info.ref_id1[b0], info.mv1[b0]) } else { (NO_REF, (0, 0)) };
                    'scan: for by in 0..4 {
                        for bx in 0..4 {
                            let i = info.at(mb_x * 4 + bx, mb_y * 4 + by);
                            if !info.inter[i]
                                || info.nnz[i] != 0
                                || info.ref_id[i] != r0
                                || info.mv[i] != m0
                                || (has1 && (info.ref_id1[i] != r10 || info.mv1[i] != m10))
                            {
                                ok = false;
                                break 'scan;
                            }
                        }
                    }
                }
                ok
            };
            // ---- boundary strengths for the whole macroblock, derived ONCE ----
            // The chroma edge groups are CO-LOCATED with luma edges 0 and 2 and
            // derive identical strengths (pinned by `chroma_bs_matches_luma`), so
            // deriving them in the chroma loops recomputed 16 of the 48 per-MB
            // strengths. Guards mirror the consuming loops exactly, so nothing is
            // derived that was not derived before; edges left at zero are exactly
            // the edges those loops skip.
            if packed_mb.is_some() {
                // Already written by `derive_mb_packed` above: unlike the tile path,
                // the packed derivation produces `flat_inter` AND the strengths in one
                // traversal, so it cannot be split across the two chains.
            } else if precomputed {
                let m = &info.bs[mb_y * mb_w + mb_x];
                for e in 0..4 {
                    for sg in 0..4 {
                        bs_v[e][sg] = m.v[e][sg] as i32;
                        bs_h[e][sg] = m.h[e][sg] as i32;
                    }
                }
            } else if let Some(k) = fast_kind {
                // Write i32 strengths straight into the consuming arrays. Going via
                // `derive_mb_kind`'s packed `MbBs` cost 32 u8->i32 stores per
                // classified macroblock that the blind path never pays — pure
                // addition on the path that is supposed to be cheaper.
                derive_mb_kind_into(info, mb_x, mb_y, k, &mut bs_v, &mut bs_h);
                if verify_kinds {
                    verify_kind_matches_blind(
                        info, mb_x, mb_y, mb_t8, k, flat_inter, &bs_v, &bs_h,
                    );
                }
            } else if blind_tile {
                derive_mb_bs(
                    tile.as_ref().unwrap(),
                    mb_x,
                    mb_y,
                    flat_inter,
                    uniform_motion,
                    mb_t8,
                    &mut bs_v,
                    &mut bs_h,
                );
            }
            drop(_dg);
            // ---- luma vertical edges (block columns 0..4) ----
            for be in 0..4usize {
                if be == 0 && mb_x == 0 {
                    continue;
                }
                if flat_inter && be != 0 {
                    continue; // internal bs all 0 (flat inter MB)
                }
                // 8×8-transform MBs: internal 4×4 edges (be 1, 3) aren't filtered.
                if mb_t8 && (be == 1 || be == 3) {
                    continue;
                }
                let mb_edge = be == 0;
                let mut bs4 = [0i32; 4];
                if have_bs {
                    bs4 = bs_v[be];
                } else {
                    let abx = mb_x * 4 + be;
                    for (seg, b) in bs4.iter_mut().enumerate() {
                        let aby = mb_y * 4 + seg;
                        *b = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
                    }
                }
                if bs4.iter().all(|&b| b == 0) {
                    continue;
                }
                // Thresholds AFTER the all-zero early-out: on real content most
                // edges filter nothing, and computing α/β/tc0 (two clamps plus
                // three table loads, and a neighbour QP read on MB edges) for an
                // edge we are about to skip is pure waste.
                let qpav = if mb_edge {
                    (qpy(mb_x - 1, mb_y) + qpy(mb_x, mb_y) + 1) >> 1
                } else {
                    qpy(mb_x, mb_y)
                };
                let (alpha_y, beta_y, tc0a) = thresholds(qpav, offset_a, offset_b);
                let tc0_luma = |bs: i32| if (1..4).contains(&bs) { tc0a[bs as usize - 1] } else { 0 };
                let x = mb_x * 16 + be * 4;
                // Vertical edge via openh264's transpose → V-filter → transpose-back
                // (the `DeblockLumaLt4H` wrapper). tc per 4-row segment (−1 = skip).
                #[cfg(accel)]
                {
                    let base = mb_y * 16 * cw + (x - 4); // p3 column, top row
                    if bs4.iter().all(|&b| b == 4) {
                        rusty_h264_accel::deblock_luma_eq4_h(&mut y[base..], cw, alpha_y, beta_y);
                    } else {
                        let tc: [i8; 4] = std::array::from_fn(|i| {
                            if (1..4).contains(&bs4[i]) { tc0_luma(bs4[i]) as i8 } else { -1 }
                        });
                        rusty_h264_accel::deblock_luma_lt4_h(&mut y[base..], cw, alpha_y, beta_y, &tc);
                    }
                }
                #[cfg(not(accel))]
                for (seg, &bs) in bs4.iter().enumerate() {
                    if bs == 0 {
                        continue;
                    }
                    let tc0 = tc0_luma(bs);
                    for row in 0..4 {
                        let yy = mb_y * 16 + seg * 4 + row;
                        let line = Line { base: yy * cw + x, step: 1 };
                        filter_luma_line(y, &line, bs, alpha_y, beta_y, tc0);
                    }
                }
            }
            // ---- luma horizontal edges (block rows 0..4) ----
            for be in 0..4usize {
                if be == 0 && mb_y == 0 {
                    continue;
                }
                if flat_inter && be != 0 {
                    continue; // internal bs all 0 (flat inter MB)
                }
                if mb_t8 && (be == 1 || be == 3) {
                    continue;
                }
                let mb_edge = be == 0;
                let mut bs4 = [0i32; 4];
                if have_bs {
                    bs4 = bs_h[be];
                } else {
                    let aby = mb_y * 4 + be;
                    for (seg, b) in bs4.iter_mut().enumerate() {
                        let abx = mb_x * 4 + seg;
                        *b = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                    }
                }
                if bs4.iter().all(|&b| b == 0) {
                    continue;
                }
                // Thresholds after the early-out — see the vertical-edge note.
                let qpav = if mb_edge {
                    (qpy(mb_x, mb_y - 1) + qpy(mb_x, mb_y) + 1) >> 1
                } else {
                    qpy(mb_x, mb_y)
                };
                let (alpha_y, beta_y, tc0a) = thresholds(qpav, offset_a, offset_b);
                let tc0_luma = |bs: i32| if (1..4).contains(&bs) { tc0a[bs as usize - 1] } else { 0 };
                let yy = mb_y * 16 + be * 4;
                // openh264's DeblockLumaLt4V/Eq4V filter the whole 16-column horizontal
                // edge at once (p/q vertical; plane 16-aligned via AlignedBytes).
                // bit-identical spec filter; tc per 4-column segment (−1 = skip).
                #[cfg(accel)]
                {
                    let base = (yy - 4) * cw + mb_x * 16; // p3 row (4 rows above q0)
                    if bs4.iter().all(|&b| b == 4) {
                        rusty_h264_accel::deblock_luma_eq4_v(&mut y[base..], cw, alpha_y, beta_y);
                    } else {
                        let tc: [i8; 4] = std::array::from_fn(|i| {
                            if (1..4).contains(&bs4[i]) { tc0_luma(bs4[i]) as i8 } else { -1 }
                        });
                        rusty_h264_accel::deblock_luma_lt4_v(&mut y[base..], cw, alpha_y, beta_y, &tc);
                    }
                }
                #[cfg(not(accel))]
                for (seg, &bs) in bs4.iter().enumerate() {
                    if bs == 0 {
                        continue;
                    }
                    let tc0 = tc0_luma(bs);
                    for col in 0..4 {
                        let x = mb_x * 16 + seg * 4 + col;
                        let line = Line { base: yy * cw + x, step: cw as isize };
                        filter_luma_line(y, &line, bs, alpha_y, beta_y, tc0);
                    }
                }
            }
            // ---- chroma edges (8×8): bS taken from the co-located luma edge ----
            // The chroma `tc` is the spec `tc0+1` (no ap/aq adjustment); bS varies per
            // 2-chroma-sample segment (= one co-located luma 4×4 block).
            #[cfg(accel)]
            {
                let tc0_of = |arr: [i32; 3], bs: i32| if (1..4).contains(&bs) { arr[bs as usize - 1] } else { 0 };
                // Chroma thresholds are derived per edge, AFTER that edge is known
                // to filter. Deriving all three sets up front cost three
                // `chroma_qp` lookups and three table lookups on every macroblock,
                // including the majority whose chroma edges are all bS 0.
                let chroma_thresholds = |mb_edge: bool, nx: usize, ny: usize| {
                    let cur = qpc(qpy(mb_x, mb_y));
                    let q = if mb_edge { (qpc(qpy(nx, ny)) + cur + 1) >> 1 } else { cur };
                    thresholds(q, offset_a, offset_b)
                };
                // vertical chroma edges → DeblockChromaLt4H/Eq4H (Cb+Cr together).
                for cxe in [0usize, 4] {
                    if cxe == 0 && mb_x == 0 {
                        continue;
                    }
                    if flat_inter && cxe != 0 {
                        continue; // internal bs all 0 (flat inter MB)
                    }
                    let mb_edge = cxe == 0;
                    let x = mb_x * 8 + cxe;
                    let mut bs4 = [0i32; 4];
                    if have_bs {
                        bs4 = bs_v[cxe / 2]; // co-located luma edge, already derived
                    } else {
                        let abx = mb_x * 4 + cxe / 2;
                        for (seg, b) in bs4.iter_mut().enumerate() {
                            let aby = mb_y * 4 + seg;
                            *b = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
                        }
                    }
                    if bs4.iter().all(|&b| b == 0) {
                        continue;
                    }
                    let (alpha_c, beta_c, tc0c) =
                        chroma_thresholds(mb_edge, mb_x.wrapping_sub(1), mb_y);
                    let base = (mb_y * 8) * ccw + (x - 2); // p1 (2 cols left of q0)
                    if bs4.iter().all(|&b| b == 4) {
                        rusty_h264_accel::deblock_chroma_eq4_h(&mut u[base..], &mut v[base..], ccw, alpha_c, beta_c);
                    } else {
                        let tc: [i8; 4] = std::array::from_fn(|i| {
                            if (1..4).contains(&bs4[i]) { tc0_of(tc0c, bs4[i]) as i8 + 1 } else { 0 }
                        });
                        rusty_h264_accel::deblock_chroma_lt4_h(&mut u[base..], &mut v[base..], ccw, alpha_c, beta_c, &tc);
                    }
                }
                // horizontal chroma edges → DeblockChromaLt4V/Eq4V.
                for cye in [0usize, 4] {
                    if cye == 0 && mb_y == 0 {
                        continue;
                    }
                    if flat_inter && cye != 0 {
                        continue; // internal bs all 0 (flat inter MB)
                    }
                    let mb_edge = cye == 0;
                    let yy = mb_y * 8 + cye;
                    let mut bs4 = [0i32; 4];
                    if have_bs {
                        bs4 = bs_h[cye / 2]; // co-located luma edge, already derived
                    } else {
                        let aby = mb_y * 4 + cye / 2;
                        for (seg, b) in bs4.iter_mut().enumerate() {
                            let abx = mb_x * 4 + seg;
                            *b = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                        }
                    }
                    if bs4.iter().all(|&b| b == 0) {
                        continue;
                    }
                    let (alpha_c, beta_c, tc0c) =
                        chroma_thresholds(mb_edge, mb_x, mb_y.wrapping_sub(1));
                    let base = (yy - 2) * ccw + mb_x * 8; // p1 (2 rows above q0)
                    if bs4.iter().all(|&b| b == 4) {
                        rusty_h264_accel::deblock_chroma_eq4_v(&mut u[base..], &mut v[base..], ccw, alpha_c, beta_c);
                    } else {
                        let tc: [i8; 4] = std::array::from_fn(|i| {
                            if (1..4).contains(&bs4[i]) { tc0_of(tc0c, bs4[i]) as i8 + 1 } else { 0 }
                        });
                        rusty_h264_accel::deblock_chroma_lt4_v(&mut u[base..], &mut v[base..], ccw, alpha_c, beta_c, &tc);
                    }
                }
            }
            #[cfg(not(accel))]
            {
                // Chroma edge thresholds use the average of the two MBs' QPc.
                let cur_qpc = qpc(qpy(mb_x, mb_y));
                let (alpha_cv, beta_cv, tc0cv) = if mb_x > 0 {
                    thresholds((qpc(qpy(mb_x - 1, mb_y)) + cur_qpc + 1) >> 1, offset_a, offset_b)
                } else {
                    (0, 0, [0; 3]) // unused (cxe==0 skipped at frame edge)
                };
                let (alpha_ch, beta_ch, tc0ch) = if mb_y > 0 {
                    thresholds((qpc(qpy(mb_x, mb_y - 1)) + cur_qpc + 1) >> 1, offset_a, offset_b)
                } else {
                    (0, 0, [0; 3])
                };
                let (alpha_ci, beta_ci, tc0ci) = thresholds(cur_qpc, offset_a, offset_b);
                let tc0_of = |arr: [i32; 3], bs: i32| if (1..4).contains(&bs) { arr[bs as usize - 1] } else { 0 };
                // bS from the co-located luma edge — the STORED strengths when
                // available, exactly like the accel arm and the luma loops above.
                // This is not just the shared-derivation saving: on the precomputed
                // path the caller's view may carry NO syntax grids at all (the E2
                // worker's `PixelCtx::filter_row` passes `inter: &[]`), so live
                // derivation here is an out-of-bounds panic, not a slow path.
                let chroma_bs = |stored: &[[i32; 4]; 4], edge: usize, vertical: bool, mb_edge: bool| -> [i32; 4] {
                    if have_bs {
                        return stored[edge / 2]; // co-located luma edge, already derived
                    }
                    let mut bs4 = [0i32; 4];
                    for (seg, b) in bs4.iter_mut().enumerate() {
                        let (abx, aby) = if vertical {
                            (mb_x * 4 + edge / 2, mb_y * 4 + seg)
                        } else {
                            (mb_x * 4 + seg, mb_y * 4 + edge / 2)
                        };
                        let (p, q) = if vertical {
                            (info.at(abx - 1, aby), info.at(abx, aby))
                        } else {
                            (info.at(abx, aby - 1), info.at(abx, aby))
                        };
                        *b = info.bs(p, q, mb_edge);
                    }
                    bs4
                };
                for plane in [&mut *u, &mut *v] {
                    for cxe in [0usize, 4] {
                        if cxe == 0 && mb_x == 0 {
                            continue;
                        }
                        if flat_inter && cxe != 0 {
                            continue;
                        }
                        let mb_edge = cxe == 0;
                        // MB-left edge uses the cross-MB chroma avg; internal uses the MB's own.
                        let (alpha_c, beta_c, tc0c) =
                            if mb_edge { (alpha_cv, beta_cv, tc0cv) } else { (alpha_ci, beta_ci, tc0ci) };
                        let bs4 = chroma_bs(&bs_v, cxe, true, mb_edge);
                        let x = mb_x * 8 + cxe;
                        for row in 0..8 {
                            // Segment = the co-located luma block row (2 chroma rows each).
                            let bs = bs4[(row * 2) / 4];
                            if bs == 0 {
                                continue;
                            }
                            let yy = mb_y * 8 + row;
                            let line = Line { base: yy * ccw + x, step: 1 };
                            filter_chroma_line(plane, &line, bs, alpha_c, beta_c, tc0_of(tc0c, bs));
                        }
                    }
                    for cye in [0usize, 4] {
                        if cye == 0 && mb_y == 0 {
                            continue;
                        }
                        if flat_inter && cye != 0 {
                            continue;
                        }
                        let mb_edge = cye == 0;
                        let (alpha_c, beta_c, tc0c) =
                            if mb_edge { (alpha_ch, beta_ch, tc0ch) } else { (alpha_ci, beta_ci, tc0ci) };
                        let bs4 = chroma_bs(&bs_h, cye, false, mb_edge);
                        let yy = mb_y * 8 + cye;
                        for col in 0..8 {
                            // Segment = the co-located luma block column.
                            let bs = bs4[(col * 2) / 4];
                            if bs == 0 {
                                continue;
                            }
                            let line = Line { base: yy * ccw + (mb_x * 8 + col), step: ccw as isize };
                            filter_chroma_line(plane, &line, bs, alpha_c, beta_c, tc0_of(tc0c, bs));
                        }
                    }
                }
            }
        }
    }
    if let Some(buf) = packs {
        PACK_SCRATCH.set(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-list AVX2 masks kernel vs the scalar set-matching twin, over
    /// randomized two-list inputs. Unused slots deliberately carry GARBAGE
    /// motion: the scalar twin ignores it via slot compaction, and the kernel
    /// must neutralize it — equality here proves the neutralization.
    #[cfg(all(target_arch = "x86_64", feature = "asm"))]
    #[test]
    fn two_list_masks_match_scalar() {
        let mut seed = 0x2545_F491u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..50_000 {
            let mut p = MbPack::default();
            p.inter = true;
            for k in 0..16 {
                let r = rnd();
                p.ref_id[k] = match r & 3 {
                    0 => NO_REF,
                    1 => 10,
                    2 => 20,
                    _ => 30,
                };
                p.ref1[k] = match (r >> 2) & 3 {
                    0 => NO_REF,
                    1 => 10,
                    2 => 40,
                    _ => 20,
                };
                // Motion near the ±4 threshold; unused slots get garbage on purpose.
                p.mvx[k] = ((r >> 4) % 11) as i16 - 5;
                p.mvy[k] = ((r >> 8) % 11) as i16 - 5;
                p.mvx1[k] = ((r >> 12) % 11) as i16 - 5;
                p.mvy1[k] = ((r >> 16) % 11) as i16 - 5;
                if p.ref1[k] != NO_REF {
                    p.l1_used |= 1 << k;
                }
            }
            let scalar = bs_motion_masks_scalar(&p);
            let simd = rusty_h264_accel::bs_motion_masks_two_list(
                &p.mvx, &p.mvy, &p.ref_id, &p.mvx1, &p.mvy1, &p.ref1, NO_REF,
            )
            .expect("AVX2 present on the test box");
            assert_eq!(simd, scalar, "l1_used={:04x} ref0={:?} ref1={:?}", p.l1_used, p.ref_id, p.ref1);
        }
    }

    /// The two boundary-strength arms must be the same function. `bs_branchless`
    /// is the shipped path and `bs_branchy` the fallback/A-B arm, so any
    /// divergence would silently change the filtered reconstruction — and, since
    /// the reconstruction is the inter prediction reference, the bitstream.
    #[test]
    fn bs_arms_agree() {
        let (w4, h4) = (16usize, 16usize);
        let n = w4 * h4;
        // Deterministic pseudo-random block state covering every branch: intra vs
        // inter, coded vs uncoded, matching vs differing refs, near vs far motion.
        let mut st = 0x9e3779b9u32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        let mut inter = vec![false; n];
        let mut nnz = vec![0u8; n];
        let mut mv = vec![(0i32, 0i32); n];
        let mut ref_id = vec![0i32; n];
        for i in 0..n {
            let r = rnd();
            inter[i] = r & 3 != 0;
            nnz[i] = if r & 0x30 != 0 { (r >> 8 & 15) as u8 } else { 0 };
            // Span the |Δ| >= 4 boundary in both components.
            mv[i] = (((r >> 12) & 15) as i32 - 8, ((r >> 16) & 15) as i32 - 8);
            ref_id[i] = if inter[i] { ((r >> 20) & 3) as i32 } else { NO_REF };
        }
        let info = BlockInfo {
            inter: &inter,
            nnz: &nnz,
            mv: &mv,
            ref_id: &ref_id,
            mv1: &[],
            ref_id1: &[],
            w4,
            t8x8: &[],
            poc0: &[],
            poc1: &[],
            bs: &[], kind: &[],
        };
        let mut checked = 0;
        for q in 0..n {
            for &p in &[q.saturating_sub(1), q.saturating_sub(w4)] {
                for mb_edge in [false, true] {
                    assert_eq!(
                        info.bs_branchy(p, q, mb_edge),
                        info.bs_branchless(p, q, mb_edge),
                        "bS mismatch at p={p} q={q} mb_edge={mb_edge}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 1000, "expected broad coverage, checked {checked}");
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;

    /// The per-MB tile must reproduce the frame-array indexing exactly. This is
    /// where a transcription slip would hide: the tile's (row, col) origin is the
    /// top-left NEIGHBOUR, so every edge lookup is offset by one, and chroma edge
    /// groups index it at half the luma rate.
    #[test]
    fn tile_matches_frame_indexing() {
        let (mb_w, mb_h) = (5usize, 4usize);
        let w4 = mb_w * 4;
        let n = w4 * mb_h * 4;
        let mut st = 0xdeadbeefu32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        let mut inter = vec![false; n];
        let mut nnz = vec![0u8; n];
        let mut mv = vec![(0i32, 0i32); n];
        let mut ref_id = vec![0i32; n];
        for i in 0..n {
            let r = rnd();
            inter[i] = r & 3 != 0;
            nnz[i] = if r & 0x30 != 0 { (r >> 8 & 15) as u8 } else { 0 };
            mv[i] = (((r >> 12) & 15) as i32 - 8, ((r >> 16) & 15) as i32 - 8);
            ref_id[i] = if inter[i] { ((r >> 20) & 3) as i32 } else { NO_REF };
        }
        let info = BlockInfo {
            inter: &inter, nnz: &nnz, mv: &mv, ref_id: &ref_id,
            poc0: &[],
            poc1: &[],
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[], kind: &[],
        };

        let mut checked = 0;
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let tile = gather_tile(&info, mb_x, mb_y);
                for be in 0..4 {
                    let mb_edge = be == 0;
                    if mb_edge && mb_x == 0 {
                        continue;
                    }
                    // luma vertical
                    let abx = mb_x * 4 + be;
                    for seg in 0..4 {
                        let aby = mb_y * 4 + seg;
                        assert_eq!(
                            info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge),
                            bs_tile(&tile[seg + 1][be], &tile[seg + 1][be + 1], mb_edge),
                            "luma V mb=({mb_x},{mb_y}) be={be} seg={seg}"
                        );
                        checked += 1;
                    }
                }
                for be in 0..4 {
                    let mb_edge = be == 0;
                    if mb_edge && mb_y == 0 {
                        continue;
                    }
                    let aby = mb_y * 4 + be;
                    for seg in 0..4 {
                        let abx = mb_x * 4 + seg;
                        assert_eq!(
                            info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge),
                            bs_tile(&tile[be][seg + 1], &tile[be + 1][seg + 1], mb_edge),
                            "luma H mb=({mb_x},{mb_y}) be={be} seg={seg}"
                        );
                        checked += 1;
                    }
                }
                // chroma groups index the tile at half the luma rate
                for cxe in [0usize, 4] {
                    let mb_edge = cxe == 0;
                    if mb_edge && mb_x == 0 {
                        continue;
                    }
                    let abx = mb_x * 4 + cxe / 2;
                    for seg in 0..4 {
                        let aby = mb_y * 4 + seg;
                        assert_eq!(
                            info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge),
                            bs_tile(&tile[seg + 1][cxe / 2], &tile[seg + 1][cxe / 2 + 1], mb_edge),
                            "chroma V mb=({mb_x},{mb_y}) cxe={cxe} seg={seg}"
                        );
                        checked += 1;
                    }
                }
                for cye in [0usize, 4] {
                    let mb_edge = cye == 0;
                    if mb_edge && mb_y == 0 {
                        continue;
                    }
                    let aby = mb_y * 4 + cye / 2;
                    for seg in 0..4 {
                        let abx = mb_x * 4 + seg;
                        assert_eq!(
                            info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge),
                            bs_tile(&tile[cye / 2][seg + 1], &tile[cye / 2 + 1][seg + 1], mb_edge),
                            "chroma H mb=({mb_x},{mb_y}) cye={cye} seg={seg}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 800, "coverage too low: {checked}");
    }
}

#[cfg(test)]
mod chroma_bs_tests {
    use super::*;

    /// A chroma edge group is CO-LOCATED with a luma edge group — chroma edge 0
    /// with luma edge 0, chroma edge 4 with luma edge 2 — and derives the
    /// identical boundary strengths, because bS is a property of the 4×4 block
    /// pair, not of the plane. This test is the licence for the derivation to run
    /// once per luma edge and be reused by chroma; without it, 16 of the 48
    /// per-macroblock derivations are recomputes.
    #[test]
    fn chroma_bs_matches_luma() {
        let (mb_w, mb_h) = (5usize, 4usize);
        let w4 = mb_w * 4;
        let n = w4 * mb_h * 4;
        let mut st = 0x1badb002u32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        let (mut inter, mut nnz) = (vec![false; n], vec![0u8; n]);
        let (mut mv, mut ref_id) = (vec![(0i32, 0i32); n], vec![0i32; n]);
        for i in 0..n {
            let r = rnd();
            inter[i] = r & 3 != 0;
            nnz[i] = if r & 0x30 != 0 { (r >> 8 & 15) as u8 } else { 0 };
            mv[i] = (((r >> 12) & 15) as i32 - 8, ((r >> 16) & 15) as i32 - 8);
            ref_id[i] = if inter[i] { ((r >> 20) & 3) as i32 } else { NO_REF };
        }
        let info = BlockInfo {
            inter: &inter, nnz: &nnz, mv: &mv, ref_id: &ref_id,
            poc0: &[],
            poc1: &[],
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[], kind: &[],
        };
        let mut checked = 0;
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let tile = gather_tile(&info, mb_x, mb_y);
                for (cxe, be) in [(0usize, 0usize), (4, 2)] {
                    let mb_edge = cxe == 0;
                    for seg in 0..4 {
                        // vertical: chroma column cxe/2 == luma column `be`
                        assert_eq!(
                            bs_tile(&tile[seg + 1][cxe / 2], &tile[seg + 1][cxe / 2 + 1], mb_edge),
                            bs_tile(&tile[seg + 1][be], &tile[seg + 1][be + 1], mb_edge),
                            "V mb=({mb_x},{mb_y}) cxe={cxe} seg={seg}"
                        );
                        // horizontal: chroma row cye/2 == luma row `be`
                        assert_eq!(
                            bs_tile(&tile[cxe / 2][seg + 1], &tile[cxe / 2 + 1][seg + 1], mb_edge),
                            bs_tile(&tile[be][seg + 1], &tile[be + 1][seg + 1], mb_edge),
                            "H mb=({mb_x},{mb_y}) cye={cxe} seg={seg}"
                        );
                        checked += 2;
                    }
                }
            }
        }
        assert!(checked > 300, "coverage too low: {checked}");
    }
}

#[cfg(test)]
mod derive_tests {
    use super::*;

    /// `derive_mb_bs` must reproduce the per-edge derivation exactly. It replaces
    /// per-edge intra tests with per-macroblock constant fills, so the test data
    /// must respect the invariant that licences it: intra/inter is uniform across
    /// a macroblock's 16 blocks.
    /// The PACKED derivation must reproduce the tile derivation exactly — same
    /// strengths AND the same `flat_inter`. This is the oracle for the whole
    /// packed-layout brick: the packed path is a different traversal of the same
    /// rule, so a byte-identical corpus run can tell you THAT it broke but not where,
    /// and (as Brick 2 proved) a pixel-identical mistake can hide in `flat_inter`
    /// while silently costing the optimisation it was built for.
    /// The SIMD twin must equal the scalar oracle on the FULL u16, not merely on the
    /// bits the derivation reads — the don't-care lanes are masked to zero in both so
    /// a lane-boundary mistake cannot hide behind "that bit is unused anyway".
    ///
    /// Exercises the cases the kernel's shifts are sensitive to: NO_REF blocks, refs
    /// that differ, motion exactly at the |d| == 4 threshold (the `>= 4` boundary),
    /// and large opposite-signed vectors where a saturating subtract could go wrong.
    /// The uniform-motion SIMD twin must equal the scalar oracle. Cases are built to
    /// straddle the decision: genuinely uniform macroblocks, ones differing in exactly
    /// ONE lane of ONE plane (the case a broadcast-compare gets wrong if a plane is
    /// dropped), and ones differing only on List-1.
    #[test]
    fn mb_uniform_simd_matches_scalar() {
        let mut st = 0x51ed270bu32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        for case in 0..6000 {
            let mut p = MbPack::default();
            p.inter = case % 7 != 0; // exercise the intra short-circuit too
            for k in 0..16 {
                p.ref_id[k] = 3;
                p.mvx[k] = 7;
                p.mvy[k] = -9;
                p.ref1[k] = if case % 3 == 0 { NO_REF } else { 5 };
                p.mvx1[k] = 2;
                p.mvy1[k] = -4;
            }
            // Perturb exactly one lane of one plane, often — that is the boundary.
            if case % 2 == 0 {
                let k = 1 + (rnd() % 15) as usize;
                match rnd() % 6 {
                    0 => p.ref_id[k] += 1,
                    1 => p.mvx[k] += 1,
                    2 => p.mvy[k] += 1,
                    3 => p.ref1[k] = p.ref1[k].wrapping_add(1),
                    4 => p.mvx1[k] += 1,
                    _ => p.mvy1[k] += 1,
                }
            }
            assert_eq!(
                mb_uniform(&p),
                mb_uniform_scalar(&p),
                "case {case}: uniform SIMD != scalar"
            );
        }
    }

    #[test]
    fn bs_motion_masks_simd_matches_scalar() {
        let mut st = 0x9e3779b9u32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        for case in 0..4000 {
            let mut p = MbPack::default();
            p.inter = true;
            for k in 0..16 {
                let r = rnd();
                p.ref_id[k] = match case % 4 {
                    0 => (r & 1) as i32,          // two references, frequent changes
                    1 => 7,                        // all identical
                    2 if r & 7 == 0 => NO_REF,     // scattered intra/unused
                    _ => (r & 3) as i32,
                };
                // Straddle the >= 4 threshold deliberately, and include extremes.
                p.mvx[k] = match case % 3 {
                    0 => ((r >> 3) & 7) as i16 - 4,
                    1 => ((r >> 3) & 0x7fff) as i16 - 16384,
                    _ => ((r >> 3) & 1) as i16 * 4,
                };
                p.mvy[k] = match case % 3 {
                    0 => ((r >> 9) & 7) as i16 - 4,
                    1 => ((r >> 9) & 0x7fff) as i16 - 16384,
                    _ => ((r >> 9) & 1) as i16 * 4,
                };
            }
            let want = bs_motion_masks_scalar(&p);
            let got = bs_motion_masks(&p);
            assert_eq!(
                got, want,
                "case {case}: SIMD (left={:#06x} up={:#06x}) != scalar (left={:#06x} up={:#06x})",
                got.0, got.1, want.0, want.1
            );
        }
    }

    #[test]
    fn packed_matches_tile() {
        let (mb_w, mb_h) = (6usize, 5usize);
        let w4 = mb_w * 4;
        let n = w4 * mb_h * 4;
        let mut st = 0x1234abcdu32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        let (mut inter, mut nnz) = (vec![false; n], vec![0u8; n]);
        let (mut mv, mut ref_id) = (vec![(0i32, 0i32); n], vec![0i32; n]);
        for my in 0..mb_h {
            for mx in 0..mb_w {
                // intra/inter is a per-MACROBLOCK property; the packed record stores
                // one flag per macroblock and depends on that invariant.
                let mb_inter = rnd() & 3 != 0;
                // Deliberately include macroblocks that are uniform (exercising the
                // `uniform`/`flat_inter` fast paths) as well as fully varied ones.
                let uniform_mb = rnd() & 1 == 0;
                let (ur, umv) = ((rnd() & 1) as i32, ((rnd() & 7) as i32 - 4, (rnd() & 7) as i32 - 4));
                let zero_coeffs = rnd() & 1 == 0;
                for by in 0..4 {
                    for bx in 0..4 {
                        let i = (my * 4 + by) * w4 + mx * 4 + bx;
                        let r = rnd();
                        inter[i] = mb_inter;
                        nnz[i] = if uniform_mb && zero_coeffs {
                            0
                        } else if r & 0x30 != 0 {
                            (r >> 8 & 15) as u8
                        } else {
                            0
                        };
                        if mb_inter {
                            ref_id[i] = if uniform_mb { ur } else { (r >> 12 & 1) as i32 };
                            mv[i] = if uniform_mb {
                                umv
                            } else {
                                ((r >> 16 & 15) as i32 - 8, (r >> 20 & 15) as i32 - 8)
                            };
                        } else {
                            ref_id[i] = NO_REF;
                            mv[i] = (0, 0);
                        }
                    }
                }
            }
        }
        // A List-1 plane too, so the two-slot set-matching rule is exercised — that
        // rule is order-independent (a pair matching after a SWAP is NOT different
        // motion), which a single-list grid cannot test at all.
        let (mut mv1, mut ref_id1) = (vec![(0i32, 0i32); n], vec![NO_REF; n]);
        for my in 0..mb_h {
            for mx in 0..mb_w {
                let mb_bi = rnd() & 1 == 0; // some macroblocks bi-predicted
                for by in 0..4 {
                    for bx in 0..4 {
                        let i = (my * 4 + by) * w4 + mx * 4 + bx;
                        let r = rnd();
                        if inter[i] && mb_bi && r & 3 != 0 {
                            ref_id1[i] = (r >> 2 & 1) as i32;
                            mv1[i] = ((r >> 4 & 15) as i32 - 8, (r >> 8 & 15) as i32 - 8);
                        }
                    }
                }
            }
        }
        for (tag, m1, r1) in [
            ("single-list", &[][..], &[][..]),
            ("two-list", &mv1[..], &ref_id1[..]),
        ] {
        let info = BlockInfo {
            inter: &inter, nnz: &nnz, mv: &mv, ref_id: &ref_id,
            poc0: &[],
            poc1: &[],
            mv1: m1, ref_id1: r1, w4, t8x8: &[], bs: &[], kind: &[],
        };
        let packs = pack_frame(&info, mb_w, mb_h).expect("frame packs");
        let _ = tag;

        let mut checked = 0usize;
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let tile = gather_tile(&info, mb_x, mb_y);
                let (uniform, flat) = scan_uniform_flat(&tile);
                for &mb_t8 in &[false, true] {
                    let (mut tv, mut th) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    derive_mb_bs(&tile, mb_x, mb_y, flat, uniform, mb_t8, &mut tv, &mut th);

                    let (mut pv, mut ph) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    let pflat =
                        derive_mb_packed(&packs, mb_w, mb_x, mb_y, mb_t8, &mut pv, &mut ph);

                    assert_eq!(
                        pflat, flat,
                        "MB ({mb_x},{mb_y}) t8={mb_t8}: flat_inter packed={pflat} tile={flat}"
                    );
                    // Compare only the edges the consuming loops actually read — the
                    // same masking the kind-vs-blind oracle uses, for the same reason.
                    for be in 0..4usize {
                        if be == 0 {
                            if mb_x > 0 {
                                assert_eq!(pv[0], tv[0], "MB ({mb_x},{mb_y}) left MB edge");
                            }
                            if mb_y > 0 {
                                assert_eq!(ph[0], th[0], "MB ({mb_x},{mb_y}) top MB edge");
                            }
                            continue;
                        }
                        if flat || (mb_t8 && (be == 1 || be == 3)) {
                            continue;
                        }
                        assert_eq!(pv[be], tv[be], "MB ({mb_x},{mb_y}) t8={mb_t8} v edge {be}");
                        assert_eq!(ph[be], th[be], "MB ({mb_x},{mb_y}) t8={mb_t8} h edge {be}");
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, mb_w * mb_h * 2, "every macroblock checked both t8 ways ({tag})");
        }
    }

    #[test]
    fn derive_matches_per_edge() {
        let (mb_w, mb_h) = (6usize, 5usize);
        let w4 = mb_w * 4;
        let n = w4 * mb_h * 4;
        let mut st = 0xfeedfaceu32;
        let mut rnd = || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        let (mut inter, mut nnz) = (vec![false; n], vec![0u8; n]);
        let (mut mv, mut ref_id) = (vec![(0i32, 0i32); n], vec![0i32; n]);
        for my in 0..mb_h {
            for mx in 0..mb_w {
                // one intra/inter decision per MACROBLOCK, as the bitstream has
                let mb_inter = rnd() & 3 != 0;
                for by in 0..4 {
                    for bx in 0..4 {
                        let i = (my * 4 + by) * w4 + mx * 4 + bx;
                        let r = rnd();
                        inter[i] = mb_inter;
                        nnz[i] = if r & 0x30 != 0 { (r >> 8 & 15) as u8 } else { 0 };
                        mv[i] = (((r >> 12) & 15) as i32 - 8, ((r >> 16) & 15) as i32 - 8);
                        ref_id[i] = if mb_inter { ((r >> 20) & 3) as i32 } else { NO_REF };
                    }
                }
            }
        }
        let info = BlockInfo {
            inter: &inter, nnz: &nnz, mv: &mv, ref_id: &ref_id,
            poc0: &[],
            poc1: &[],
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[], kind: &[],
        };

        // The fused walk must agree with the two independent scans it replaced, on
        // every macroblock of this pseudo-random grid — the reference test the
        // fusion is gated on. Checked here rather than in its own test so it rides
        // the same varied intra/inter/nnz/motion mix.
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let tile = gather_tile(&info, mb_x, mb_y);
                assert_eq!(
                    scan_uniform_flat(&tile),
                    scan_two_pass(&tile),
                    "fused predicate walk disagrees with the two-scan oracle at ({mb_x},{mb_y})"
                );
            }
        }

        let mut checked = 0;
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let tile = gather_tile(&info, mb_x, mb_y);
                // the flat-inter predicate exactly as `filter_frame` computes it
                let b0 = &tile[1][1];
                let flat = b0.inter
                    && (1..5).all(|r| (1..5).all(|c| {
                        let b = &tile[r][c];
                        b.inter && !b.nz && b.same_motion(b0)
                    }));
                // …and `uniform_motion` likewise, derived here INDEPENDENTLY of
                // `scan_uniform_flat` so this stays an oracle for the fused walk
                // rather than a consumer of it.
                let uniform = b0.inter
                    && (1..5).all(|r| {
                        (1..5).all(|c| tile[r][c].inter && tile[r][c].same_motion(b0))
                    });
                for &mb_t8 in &[false, true] {
                    let (mut bv, mut bh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    derive_mb_bs(&tile, mb_x, mb_y, flat, uniform, mb_t8, &mut bv, &mut bh);
                    for be in 0..4usize {
                        let mb_edge = be == 0;
                        let skip_internal =
                            !mb_edge && (flat || (mb_t8 && (be == 1 || be == 3)));
                        for seg in 0..4 {
                            let want_v = if skip_internal || (mb_edge && mb_x == 0) {
                                0
                            } else {
                                bs_tile(&tile[seg + 1][be], &tile[seg + 1][be + 1], mb_edge)
                            };
                            let want_h = if skip_internal || (mb_edge && mb_y == 0) {
                                0
                            } else {
                                bs_tile(&tile[be][seg + 1], &tile[be + 1][seg + 1], mb_edge)
                            };
                            assert_eq!(bv[be][seg], want_v, "V mb=({mb_x},{mb_y}) be={be} seg={seg} t8={mb_t8}");
                            assert_eq!(bh[be][seg], want_h, "H mb=({mb_x},{mb_y}) be={be} seg={seg} t8={mb_t8}");
                            checked += 2;
                        }
                    }
                }
            }
        }
        assert!(checked > 1500, "coverage too low: {checked}");
    }
}
