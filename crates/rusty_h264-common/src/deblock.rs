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
    /// `true` if the block belongs to an intra macroblock.
    pub intra: &'a [bool],
    /// Non-zero coefficient count of the block.
    pub nnz: &'a [u8],
    /// Block motion vector (quarter-pel); ignored for intra.
    pub mv: &'a [(i32, i32)],
    /// Reference index per block (`-1` for intra); two inter blocks with
    /// different indices reference different pictures (boundary strength 1).
    pub ref_idx: &'a [i32],
    /// Block-grid width (`mb_w * 4`).
    pub w4: usize,
}

impl BlockInfo<'_> {
    #[inline]
    fn at(&self, bx: usize, by: usize) -> usize {
        by * self.w4 + bx
    }

    /// Boundary strength between left/above block `p` and current block `q`
    /// (spec §8.7.2.1). `mb_edge` is true on macroblock boundaries.
    fn bs(&self, p: usize, q: usize, mb_edge: bool) -> i32 {
        if self.intra[p] || self.intra[q] {
            if mb_edge {
                4
            } else {
                3
            }
        } else if self.nnz[p] > 0 || self.nnz[q] > 0 {
            2
        } else {
            // Two inter blocks with no residual: bS 1 if they reference different
            // pictures or their motion vectors differ by ≥ 1 full sample.
            let (px, py) = self.mv[p];
            let (qx, qy) = self.mv[q];
            if self.ref_idx[p] != self.ref_idx[q]
                || (px - qx).abs() >= 4
                || (py - qy).abs() >= 4
            {
                1
            } else {
                0
            }
        }
    }
}

/// Applies the deblocking filter in place to a fully-reconstructed frame. `qp`
/// is the (constant) luma QP, `qpc` the chroma QP, and `info` supplies the
/// per-block state used to derive boundary strengths (for an all-intra frame
/// this reduces to the fixed 4/3 strengths).
#[allow(clippy::too_many_arguments)]
pub fn filter_frame(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    mb_w: usize,
    mb_h: usize,
    qp: u8,
    qpc: u8,
    info: &BlockInfo,
) {
    let cw = mb_w * 16;
    let ccw = mb_w * 8;
    let (alpha_y, beta_y) = (ALPHA[qp as usize], BETA[qp as usize]);
    let (alpha_c, beta_c) = (ALPHA[qpc as usize], BETA[qpc as usize]);
    let tc0_luma = |bs: i32| if (1..4).contains(&bs) { TC0[qp as usize][bs as usize - 1] } else { 0 };
    let tc0_chroma = |bs: i32| if (1..4).contains(&bs) { TC0[qpc as usize][bs as usize - 1] } else { 0 };

    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            // ---- luma vertical edges (block columns 0..4) ----
            for be in 0..4usize {
                if be == 0 && mb_x == 0 {
                    continue;
                }
                let mb_edge = be == 0;
                let abx = mb_x * 4 + be;
                for seg in 0..4usize {
                    let aby = mb_y * 4 + seg;
                    let bs = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
                    if bs == 0 {
                        continue;
                    }
                    let tc0 = tc0_luma(bs);
                    let x = mb_x * 16 + be * 4;
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
                let mb_edge = be == 0;
                let aby = mb_y * 4 + be;
                for seg in 0..4usize {
                    let abx = mb_x * 4 + seg;
                    let bs = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                    if bs == 0 {
                        continue;
                    }
                    let tc0 = tc0_luma(bs);
                    let yy = mb_y * 16 + be * 4;
                    for col in 0..4 {
                        let x = mb_x * 16 + seg * 4 + col;
                        let line = Line { base: yy * cw + x, step: cw as isize };
                        filter_luma_line(y, &line, bs, alpha_y, beta_y, tc0);
                    }
                }
            }
            // ---- chroma edges (8×8): bS taken from the co-located luma edge ----
            for (plane, alpha_c, beta_c) in [(&mut *u, alpha_c, beta_c), (&mut *v, alpha_c, beta_c)] {
                for cxe in [0usize, 4] {
                    if cxe == 0 && mb_x == 0 {
                        continue;
                    }
                    let mb_edge = cxe == 0;
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
                        filter_chroma_line(plane, &line, bs, alpha_c, beta_c, tc0_chroma(bs));
                    }
                }
                for cye in [0usize, 4] {
                    if cye == 0 && mb_y == 0 {
                        continue;
                    }
                    let mb_edge = cye == 0;
                    let aby = mb_y * 4 + cye / 2;
                    let yy = mb_y * 8 + cye;
                    for col in 0..8 {
                        let abx = mb_x * 4 + (col * 2) / 4;
                        let bs = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                        if bs == 0 {
                            continue;
                        }
                        let line = Line { base: yy * ccw + (mb_x * 8 + col), step: ccw as isize };
                        filter_chroma_line(plane, &line, bs, alpha_c, beta_c, tc0_chroma(bs));
                    }
                }
            }
        }
    }
}
