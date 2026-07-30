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
        let (mvx, mvy) = info.mv[i];
        let (ref1, (mv1x, mv1y)) = if info.ref_id1.is_empty() {
            (NO_REF, (0, 0))
        } else {
            (info.ref_id1[i], info.mv1[i])
        };
        Blk { inter: info.inter[i], nz: info.nnz[i] != 0, ref_id: info.ref_id[i], mvx, mvy, ref1, mv1x, mv1y }
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
    mb_t8: bool,
    bs_v: &mut [[i32; 4]; 4],
    bs_h: &mut [[i32; 4]; 4],
) {
    let cur_intra = !tile[1][1].inter;
    // A coded inter macroblock whose 16 blocks share one (ref, mv) — every
    // single-partition P_L0_16x16, which is the fast preset's only inter mode —
    // cannot reach internal strength 1, so its internal edges depend on
    // coefficients alone and the motion comparisons can be skipped entirely.
    let uniform_motion = !cur_intra && {
        let b0 = &tile[1][1];
        (1..5).all(|r| (1..5).all(|c| tile[r][c].inter && tile[r][c].same_motion(b0)))
    };

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
pub fn derive_mb(info: &BlockInfo, mb_x: usize, mb_y: usize, mb_t8: bool) -> MbBs {
    let tile = gather_tile(info, mb_x, mb_y);
    let b0 = &tile[1][1];
    let flat_inter = b0.inter
        && (1..5).all(|r| {
            (1..5).all(|c| {
                let b = &tile[r][c];
                b.inter && !b.nz && b.same_motion(b0)
            })
        });
    let (mut bs_v, mut bs_h) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
    derive_mb_bs(&tile, mb_x, mb_y, flat_inter, mb_t8, &mut bs_v, &mut bs_h);
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

    for mb_y in 0..mb_h {
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
            let precomputed = !info.bs.is_empty();
            // H-33: the tile arm now carries the two-list B rule (`bs1_tile`), so
            // real-world B frames no longer fall back to the strided per-edge path.
            let use_tile = !precomputed && deblock_tile();
            let have_bs = precomputed || use_tile;
            let tile = if use_tile { gather_tile(info, mb_x, mb_y) } else { Default::default() };

            let flat_inter = if precomputed {
                false // the stored zeros already encode it
            } else if use_tile {
                // Same predicate as below, read off the tile — this also replaces
                // a separate 16-block × 4-array scan of the frame arrays.
                let b0 = &tile[1][1];
                b0.inter
                    && (1..5).all(|r| {
                        (1..5).all(|c| {
                            let b = &tile[r][c];
                            b.inter && !b.nz && b.same_motion(b0)
                        })
                    })
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
            let mut bs_v = [[0i32; 4]; 4];
            let mut bs_h = [[0i32; 4]; 4];
            if precomputed {
                let m = &info.bs[mb_y * mb_w + mb_x];
                for e in 0..4 {
                    for sg in 0..4 {
                        bs_v[e][sg] = m.v[e][sg] as i32;
                        bs_h[e][sg] = m.h[e][sg] as i32;
                    }
                }
            } else if use_tile {
                derive_mb_bs(&tile, mb_x, mb_y, flat_inter, mb_t8, &mut bs_v, &mut bs_h);
            }
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
                        let abx = mb_x * 4 + cxe / 2; // co-located luma block column
                        let x = mb_x * 8 + cxe;
                        for row in 0..8 {
                            let aby = mb_y * 4 + (row * 2) / 4; // co-located luma block row
                            let bs = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
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
                        let aby = mb_y * 4 + cye / 2;
                        let yy = mb_y * 8 + cye;
                        for col in 0..8 {
                            let abx = mb_x * 4 + (col * 2) / 4;
                            let bs = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
            bs: &[],
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
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[],
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
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[],
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
            mv1: &[], ref_id1: &[], w4, t8x8: &[], bs: &[],
        };

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
                for &mb_t8 in &[false, true] {
                    let (mut bv, mut bh) = ([[0i32; 4]; 4], [[0i32; 4]; 4]);
                    derive_mb_bs(&tile, mb_x, mb_y, flat, mb_t8, &mut bv, &mut bh);
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
