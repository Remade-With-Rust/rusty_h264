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
}

/// Sentinel for an unused reference slot.
const NO_REF: i32 = i32::MIN;

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
        } else if self.inter_bs1(p, q) {
            1
        } else {
            0
        }
    }

    /// Whether two residual-free inter blocks get boundary strength 1: they use
    /// different reference pictures, a different number of motion vectors, or a
    /// motion vector differs by ≥ 1 full sample (matched by reference picture, so
    /// the same picture in different lists is recognised). Spec §8.7.2.1.
    fn inter_bs1(&self, p: usize, q: usize) -> bool {
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
            let mb_t8 = info.t8x8[mb_y * mb_w + mb_x];
            // ---- luma vertical edges (block columns 0..4) ----
            for be in 0..4usize {
                if be == 0 && mb_x == 0 {
                    continue;
                }
                // 8×8-transform MBs: internal 4×4 edges (be 1, 3) aren't filtered.
                if mb_t8 && (be == 1 || be == 3) {
                    continue;
                }
                let mb_edge = be == 0;
                let qpav = if mb_edge {
                    (qpy(mb_x - 1, mb_y) + qpy(mb_x, mb_y) + 1) >> 1
                } else {
                    qpy(mb_x, mb_y)
                };
                let (alpha_y, beta_y, tc0a) = thresholds(qpav, offset_a, offset_b);
                let tc0_luma = |bs: i32| if (1..4).contains(&bs) { tc0a[bs as usize - 1] } else { 0 };
                let abx = mb_x * 4 + be;
                let x = mb_x * 16 + be * 4;
                let mut bs4 = [0i32; 4];
                for (seg, b) in bs4.iter_mut().enumerate() {
                    let aby = mb_y * 4 + seg;
                    *b = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
                }
                if bs4.iter().all(|&b| b == 0) {
                    continue;
                }
                // Vertical edge via openh264's transpose → V-filter → transpose-back
                // (the `DeblockLumaLt4H` wrapper). tc per 4-row segment (−1 = skip).
                #[cfg(feature = "asm")]
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
                #[cfg(not(feature = "asm"))]
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
                if mb_t8 && (be == 1 || be == 3) {
                    continue;
                }
                let mb_edge = be == 0;
                let qpav = if mb_edge {
                    (qpy(mb_x, mb_y - 1) + qpy(mb_x, mb_y) + 1) >> 1
                } else {
                    qpy(mb_x, mb_y)
                };
                let (alpha_y, beta_y, tc0a) = thresholds(qpav, offset_a, offset_b);
                let tc0_luma = |bs: i32| if (1..4).contains(&bs) { tc0a[bs as usize - 1] } else { 0 };
                let aby = mb_y * 4 + be;
                let yy = mb_y * 16 + be * 4;
                let mut bs4 = [0i32; 4];
                for (seg, b) in bs4.iter_mut().enumerate() {
                    let abx = mb_x * 4 + seg;
                    *b = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                }
                if bs4.iter().all(|&b| b == 0) {
                    continue;
                }
                // openh264's DeblockLumaLt4V/Eq4V filter the whole 16-column horizontal
                // edge at once (p/q vertical; plane 16-aligned via AlignedBytes).
                // bit-identical spec filter; tc per 4-column segment (−1 = skip).
                #[cfg(feature = "asm")]
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
                #[cfg(not(feature = "asm"))]
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
            #[cfg(feature = "asm")]
            {
                // Per-edge chroma thresholds from the two MBs' averaged QPc.
                let cur_qpc = qpc(qpy(mb_x, mb_y));
                let (alpha_cv, beta_cv, tc0cv) = if mb_x > 0 {
                    thresholds((qpc(qpy(mb_x - 1, mb_y)) + cur_qpc + 1) >> 1, offset_a, offset_b)
                } else {
                    (0, 0, [0; 3])
                };
                let (alpha_ch, beta_ch, tc0ch) = if mb_y > 0 {
                    thresholds((qpc(qpy(mb_x, mb_y - 1)) + cur_qpc + 1) >> 1, offset_a, offset_b)
                } else {
                    (0, 0, [0; 3])
                };
                let (alpha_ci, beta_ci, tc0ci) = thresholds(cur_qpc, offset_a, offset_b);
                let tc0_of = |arr: [i32; 3], bs: i32| if (1..4).contains(&bs) { arr[bs as usize - 1] } else { 0 };
                // vertical chroma edges → DeblockChromaLt4H/Eq4H (Cb+Cr together).
                for cxe in [0usize, 4] {
                    if cxe == 0 && mb_x == 0 {
                        continue;
                    }
                    let mb_edge = cxe == 0;
                    let (alpha_c, beta_c, tc0c) =
                        if mb_edge { (alpha_cv, beta_cv, tc0cv) } else { (alpha_ci, beta_ci, tc0ci) };
                    let abx = mb_x * 4 + cxe / 2;
                    let x = mb_x * 8 + cxe;
                    let mut bs4 = [0i32; 4];
                    for (seg, b) in bs4.iter_mut().enumerate() {
                        let aby = mb_y * 4 + seg;
                        *b = info.bs(info.at(abx - 1, aby), info.at(abx, aby), mb_edge);
                    }
                    if bs4.iter().all(|&b| b == 0) {
                        continue;
                    }
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
                    let mb_edge = cye == 0;
                    let (alpha_c, beta_c, tc0c) =
                        if mb_edge { (alpha_ch, beta_ch, tc0ch) } else { (alpha_ci, beta_ci, tc0ci) };
                    let aby = mb_y * 4 + cye / 2;
                    let yy = mb_y * 8 + cye;
                    let mut bs4 = [0i32; 4];
                    for (seg, b) in bs4.iter_mut().enumerate() {
                        let abx = mb_x * 4 + seg;
                        *b = info.bs(info.at(abx, aby - 1), info.at(abx, aby), mb_edge);
                    }
                    if bs4.iter().all(|&b| b == 0) {
                        continue;
                    }
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
            #[cfg(not(feature = "asm"))]
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
