//! The deblocking filter (8.7), run over the whole picture once it's decoded.
//!
//! FFmpeg filters each macroblock row as soon as it's decoded, saving the unfiltered bottom
//! rows for intra prediction of the next row; filtering the finished picture in macroblock
//! order is equivalent, and simpler. Boundary strengths follow FFmpeg's `filter_mb_dir` and
//! `check_mv`, including the shortcut of its x86 fast path (every edge of an 8x8-transform
//! macroblock whose first three 8x8 blocks are coded is strength 2) and its I_PCM
//! quantiser of 0.

use super::mb::{Ctx, SliceParams};
use super::tables::*;

/// One luma or chroma edge: `pos` is the first sample of the q side, `across` the step
/// between p and q samples, `along` the step between lines.
struct Edge {
    pos: usize,
    across: usize,
    along: usize,
}

fn filter_luma(
    plane: &mut [u16],
    e: &Edge,
    bs: [u8; 4],
    index_a: usize,
    index_b: usize,
    shift: u32,
    maxv: i32,
) {
    let alpha = i32::from(ALPHA[index_a]) << shift;
    let beta = i32::from(BETA[index_b]) << shift;
    if alpha == 0 || beta == 0 {
        return;
    }
    let (x, a) = (e.across, e.along);
    let at = |plane: &[u16], i: usize| i32::from(plane[i]);
    if bs[0] == 4 {
        for line in 0..16 {
            let p = e.pos + line * a;
            let (p0, p1, p2) = (at(plane, p - x), at(plane, p - 2 * x), at(plane, p - 3 * x));
            let (q0, q1, q2) = (at(plane, p), at(plane, p + x), at(plane, p + 2 * x));
            if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
                continue;
            }
            if (p0 - q0).abs() < (alpha >> 2) + 2 {
                if (p2 - p0).abs() < beta {
                    let p3 = at(plane, p - 4 * x);
                    plane[p - x] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u16;
                    plane[p - 2 * x] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u16;
                    plane[p - 3 * x] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u16;
                } else {
                    plane[p - x] = ((2 * p1 + p0 + q1 + 2) >> 2) as u16;
                }
                if (q2 - q0).abs() < beta {
                    let q3 = at(plane, p + 3 * x);
                    plane[p] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u16;
                    plane[p + x] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u16;
                    plane[p + 2 * x] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u16;
                } else {
                    plane[p] = ((2 * q1 + q0 + p1 + 2) >> 2) as u16;
                }
            } else {
                plane[p - x] = ((2 * p1 + p0 + q1 + 2) >> 2) as u16;
                plane[p] = ((2 * q1 + q0 + p1 + 2) >> 2) as u16;
            }
        }
        return;
    }
    for (seg, &s) in bs.iter().enumerate() {
        if s == 0 {
            continue;
        }
        let tc0 = i32::from(TC0[index_a][usize::from(s) - 1]) << shift;
        for line in 4 * seg..4 * seg + 4 {
            let p = e.pos + line * a;
            let (p0, p1, p2) = (at(plane, p - x), at(plane, p - 2 * x), at(plane, p - 3 * x));
            let (q0, q1, q2) = (at(plane, p), at(plane, p + x), at(plane, p + 2 * x));
            if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
                continue;
            }
            let mut tc = tc0;
            if (p2 - p0).abs() < beta {
                if tc0 != 0 {
                    plane[p - 2 * x] =
                        (p1 + (((p2 + ((p0 + q0 + 1) >> 1)) >> 1) - p1).clamp(-tc0, tc0)) as u16;
                }
                tc += 1;
            }
            if (q2 - q0).abs() < beta {
                if tc0 != 0 {
                    plane[p + x] =
                        (q1 + (((q2 + ((p0 + q0 + 1) >> 1)) >> 1) - q1).clamp(-tc0, tc0)) as u16;
                }
                tc += 1;
            }
            let delta = ((((q0 - p0) * 4) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
            plane[p - x] = (p0 + delta).clamp(0, maxv) as u16;
            plane[p] = (q0 - delta).clamp(0, maxv) as u16;
        }
    }
}

fn filter_chroma(
    plane: &mut [u16],
    e: &Edge,
    bs: [u8; 4],
    index_a: usize,
    index_b: usize,
    shift: u32,
    maxv: i32,
) {
    let alpha = i32::from(ALPHA[index_a]) << shift;
    let beta = i32::from(BETA[index_b]) << shift;
    if alpha == 0 || beta == 0 {
        return;
    }
    let (x, a) = (e.across, e.along);
    for (seg, &s) in bs.iter().enumerate() {
        if s == 0 {
            continue;
        }
        for line in 2 * seg..2 * seg + 2 {
            let p = e.pos + line * a;
            let p0 = i32::from(plane[p - x]);
            let p1 = i32::from(plane[p - 2 * x]);
            let q0 = i32::from(plane[p]);
            let q1 = i32::from(plane[p + x]);
            if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
                continue;
            }
            if bs[0] == 4 {
                plane[p - x] = ((2 * p1 + p0 + q1 + 2) >> 2) as u16;
                plane[p] = ((2 * q1 + q0 + p1 + 2) >> 2) as u16;
            } else {
                let tc = (i32::from(TC0[index_a][usize::from(s) - 1]) << shift) + 1;
                let delta = ((((q0 - p0) * 4) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
                plane[p - x] = (p0 + delta).clamp(0, maxv) as u16;
                plane[p] = (q0 - delta).clamp(0, maxv) as u16;
            }
        }
    }
}

/// `get_chroma_qp` for a chroma QP index offset (QP'Y to QP'C).
fn chroma_qp(offset: i32, qp: i32, max_qp: i32, qp_bd_offset: i32) -> i32 {
    let x = (qp + offset).clamp(0, max_qp);
    if x < qp_bd_offset {
        x
    } else {
        i32::from(CHROMA_QP[(x - qp_bd_offset) as usize]) + qp_bd_offset
    }
}

struct BlockInfo {
    coded: [bool; 16],
    refs: [[i32; 16]; 2],
    mvs: [[[i32; 2]; 16]; 2],
}

impl Default for BlockInfo {
    fn default() -> Self {
        BlockInfo {
            coded: [false; 16],
            refs: [[-1; 16]; 2],
            mvs: [[[0; 2]; 16]; 2],
        }
    }
}

/// `check_mv`: whether two 4x4 blocks' motion differs enough for bS 1.
fn mv_differs(p: &BlockInfo, k: usize, q: &BlockInfo, kq: usize, lists: usize) -> bool {
    let far = |a: [i32; 2], b: [i32; 2]| (a[0] - b[0]).abs() >= 4 || (a[1] - b[1]).abs() >= 4;
    let (r0, r0n) = (p.refs[0][k], q.refs[0][kq]);
    let (m0, m0n) = (p.mvs[0][k], q.mvs[0][kq]);
    let mut v = r0 != r0n;
    if !v && r0 != -1 {
        v = far(m0, m0n);
    }
    if lists == 2 {
        let (r1, r1n) = (p.refs[1][k], q.refs[1][kq]);
        let (m1, m1n) = (p.mvs[1][k], q.mvs[1][kq]);
        if !v {
            v = r1 != r1n || far(m1, m1n);
        }
        if v {
            if r0 != r1n || r1 != r0n {
                return true;
            }
            return far(m0, m1n) || far(m1, m0n);
        }
    }
    v
}

impl Ctx {
    pub fn deblock_picture(&mut self) {
        for mb_y in 0..self.mb_h {
            for mb_x in 0..self.mb_w {
                let mb_xy = (mb_x + mb_y * self.mb_stride) as isize;
                let slice = self.slice_table[self.t(mb_xy)];
                let Some(&sp) = self.slice_params.get(usize::from(slice)) else {
                    continue; // never decoded
                };
                if sp.deblock != 1 {
                    self.filter_mb(mb_x, mb_y, mb_xy, slice, sp);
                }
            }
        }
    }

    /// What the boundary strength needs of a macroblock's 4x4 luma blocks (raster order):
    /// whether each is coded, and each list's reference identity (-1: unused) and motion.
    fn block_info(&self, t: usize, mb_xy: isize, cavlc_8x8: bool) -> BlockInfo {
        let typ = self.mb_type[t];
        let mut info = BlockInfo::default();
        if cavlc_8x8 && is_8x8dct(typ) {
            // CAVLC keeps per-4x4 counts for its own prediction; the 8x8 flags are in cbp.
            let cbp = u32::from(self.cbp_table[t]);
            for (k, c) in info.coded.iter_mut().enumerate() {
                *c = cbp & (0x1000 << (2 * (k >> 3) + ((k & 3) >> 1))) != 0;
            }
        } else {
            let nnz = &self.non_zero_count[t];
            for (c, &n) in info.coded.iter_mut().zip(&nnz[..16]) {
                *c = n != 0;
            }
        }
        if is_intra(typ) {
            return info;
        }
        let b = self.mb2b(mb_xy) as usize;
        for list in 0..2 {
            if !uses_list(typ, list) {
                continue;
            }
            let ids = self.ref_ids[list][t];
            for k in 0..16 {
                let (x4, y4) = (k & 3, k >> 2);
                info.refs[list][k] = ids[2 * (y4 >> 1) + (x4 >> 1)];
                let m = self.motion_val[list][b + y4 * self.b_stride + x4];
                info.mvs[list][k] = [i32::from(m[0]), i32::from(m[1])];
            }
        }
        info
    }

    fn filter_mb(&mut self, mb_x: usize, mb_y: usize, mb_xy: isize, slice: u16, sp: SliceParams) {
        let t = self.t(mb_xy);
        let mb_type = self.mb_type[t];
        let qp = i32::from(self.qscale_table[t]);
        let lists = usize::from(self.list_counts[t]);
        let cbp = u32::from(self.cbp_table[t]);
        let shift = self.bit_depth - 8;
        let maxv = self.pixel_max;
        let off = self.qp_bd_offset;
        let max_qp = self.max_qp;
        let cqp = |c: usize, q: i32| chroma_qp(sp.chroma_qp_offset[c], q, max_qp, off);
        let index = |q: i32, o: i32| (q - off + o).clamp(0, 51) as usize;
        let cavlc_8x8 = !sp.cabac;
        // The x86 fast path FFmpeg takes whenever both chroma offsets agree.
        let all_two =
            sp.chroma_qp_offset[0] == sp.chroma_qp_offset[1] && is_8x8dct(mb_type) && cbp & 7 == 7;
        let cur = self.block_info(t, mb_xy, cavlc_8x8);

        for dir in 0..2 {
            let (nxy, present) = if dir == 0 {
                (mb_xy - 1, mb_x > 0)
            } else {
                (mb_xy - self.mb_stride as isize, mb_y > 0)
            };
            let tn = self.t(nxy);
            let n_slice = self.slice_table[tn];
            let n_type = if !present || n_slice == 0xFFFF || (sp.deblock == 2 && n_slice != slice) {
                0
            } else {
                self.mb_type[tn]
            };
            // Edges inside one motion partition can't differ in motion (FFmpeg's mask_edge).
            let same_motion = |edge: usize| {
                let whole = mb_type & MB_TYPE_16X16 != 0;
                let along = if dir == 0 { MB_TYPE_16X8 } else { MB_TYPE_8X16 };
                whole
                    || mb_type & along != 0
                    || (edge & 1 == 1 && mb_type & (MB_TYPE_16X8 | MB_TYPE_8X16) != 0)
            };
            let nb = if n_type != 0 && !is_intra(mb_type | n_type) && !all_two {
                Some(self.block_info(tn, nxy, cavlc_8x8))
            } else {
                None
            };
            for edge in 0..4usize {
                if edge > 0 && edge & 1 == 1 && is_8x8dct(mb_type) {
                    continue;
                }
                if edge == 0 && n_type == 0 {
                    continue;
                }
                let mut bs = [0u8; 4];
                if edge == 0 && is_intra(mb_type | n_type) {
                    bs = [4; 4];
                } else if edge > 0 && is_intra(mb_type) {
                    bs = [3; 4];
                } else if all_two {
                    bs = [2; 4];
                } else {
                    let skip_mv = edge > 0 && same_motion(edge);
                    for (i, b) in bs.iter_mut().enumerate() {
                        let k = if dir == 0 { 4 * i + edge } else { 4 * edge + i };
                        let (q, kq) = if edge == 0 {
                            let kq = if dir == 0 { 4 * i + 3 } else { 12 + i };
                            (nb.as_ref().unwrap_or(&cur), kq)
                        } else {
                            (&cur, if dir == 0 { k - 1 } else { k - 4 })
                        };
                        *b = if cur.coded[k] || q.coded[kq] {
                            2
                        } else if skip_mv {
                            0
                        } else {
                            u8::from(mv_differs(&cur, k, q, kq, lists))
                        };
                    }
                }
                if bs == [0; 4] {
                    continue;
                }
                let (lq, cq) = if edge == 0 {
                    let qn = i32::from(self.qscale_table[tn]);
                    (
                        (qp + qn + 1) >> 1,
                        [
                            (cqp(0, qp) + cqp(0, qn) + 1) >> 1,
                            (cqp(1, qp) + cqp(1, qn) + 1) >> 1,
                        ],
                    )
                } else {
                    (qp, [cqp(0, qp), cqp(1, qp)])
                };
                let (ly, lc) = (self.ly, self.lc);
                let (x0, y0) = (mb_x * 16, mb_y * 16);
                let luma = if dir == 0 {
                    Edge {
                        pos: y0 * ly + x0 + 4 * edge,
                        across: 1,
                        along: ly,
                    }
                } else {
                    Edge {
                        pos: (y0 + 4 * edge) * ly + x0,
                        across: ly,
                        along: 1,
                    }
                };
                filter_luma(
                    &mut self.y,
                    &luma,
                    bs,
                    index(lq, sp.alpha_offset),
                    index(lq, sp.beta_offset),
                    shift,
                    maxv,
                );
                if edge & 1 == 0 {
                    let (cx, cy) = (mb_x * 8, mb_y * 8);
                    let ce = if dir == 0 {
                        Edge {
                            pos: cy * lc + cx + 2 * edge,
                            across: 1,
                            along: lc,
                        }
                    } else {
                        Edge {
                            pos: (cy + 2 * edge) * lc + cx,
                            across: lc,
                            along: 1,
                        }
                    };
                    for c in 0..2 {
                        let plane = if c == 0 { &mut self.cb } else { &mut self.cr };
                        filter_chroma(
                            plane,
                            &ce,
                            bs,
                            index(cq[c], sp.alpha_offset),
                            index(cq[c], sp.beta_offset),
                            shift,
                            maxv,
                        );
                    }
                }
            }
        }
    }
}
