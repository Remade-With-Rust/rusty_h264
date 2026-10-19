//! Macroblock reconstruction on 16-bit samples: intra prediction (8.3), motion compensation
//! with weighted prediction (8.4) and the inverse transforms (8.5).
//!
//! Everything here is fully specified by ITU-T H.264, so it's written from the
//! specification's equations; FFmpeg's structure (which partitions are predicted, when a
//! block's residual is added, how its caches name the neighbours) is kept so the port reads
//! against `h264_mb.c`. Reference samples outside the coded picture are clamped to its edge
//! (FFmpeg's `emulated_edge_mc`), and intra neighbours outside it, which a valid stream never
//! uses, read as mid-grey rather than panicking.

use super::mb::Ctx;
use super::tables::*;
use crate::DecodeError;

/// A plane of the picture being reconstructed or of a reference picture.
#[derive(Clone, Copy)]
struct PlaneRef<'a> {
    data: &'a [u16],
    stride: usize,
    width: usize,
    height: usize,
}

impl PlaneRef<'_> {
    /// A reference sample, clamped to the picture (8.4.2.2).
    #[inline]
    fn clamped(&self, x: isize, y: isize) -> i32 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        i32::from(self.data[y * self.stride + x])
    }
}

/// An intra neighbour; `half` outside the picture.
#[inline]
fn nb(
    plane: &[u16],
    stride: usize,
    width: usize,
    height: usize,
    x: isize,
    y: isize,
    half: i32,
) -> i32 {
    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
        half
    } else {
        i32::from(plane[y as usize * stride + x as usize])
    }
}

const fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

/// Copies the `ww` x `wh` window of reference samples at `(x0, y0)` into `win` (row stride
/// `W`), clamping coordinates to the picture only when the window crosses its edge.
fn fetch_window<const W: usize>(
    src: PlaneRef,
    x0: isize,
    y0: isize,
    ww: usize,
    wh: usize,
    win: &mut [i32],
) {
    if x0 >= 0 && y0 >= 0 && x0 as usize + ww <= src.width && y0 as usize + wh <= src.height {
        for r in 0..wh {
            let row = &src.data[(y0 as usize + r) * src.stride + x0 as usize..][..ww];
            for (d, &v) in win[r * W..r * W + ww].iter_mut().zip(row) {
                *d = i32::from(v);
            }
        }
    } else {
        for r in 0..wh {
            for c in 0..ww {
                win[r * W + c] = src.clamped(x0 + c as isize, y0 + r as isize);
            }
        }
    }
}

/// Luma sample interpolation (8.4.2.2.1) of a `bw` x `bh` block whose top-left integer
/// sample is `(xi, yi)`, at quarter-sample fraction `(fx, fy)`. The half samples are
/// computed once per block: `b` rows (horizontal), `h` columns (vertical) and `j` (the
/// vertical filter over unscaled horizontal sums), then combined per Table 8-12.
#[allow(clippy::too_many_arguments)]
fn luma_mc(
    src: PlaneRef,
    xi: isize,
    yi: isize,
    fx: usize,
    fy: usize,
    bw: usize,
    bh: usize,
    maxv: i32,
    dst: &mut [i32; 256],
) {
    const W: usize = 16 + 5;
    let mut win = [0i32; W * W];
    let clip = |v: i32| v.clamp(0, maxv);
    let avg = |a: i32, b: i32| (a + b + 1) >> 1;
    if fx == 0 && fy == 0 {
        fetch_window::<W>(src, xi, yi, bw, bh, &mut win);
        for y in 0..bh {
            dst[16 * y..16 * y + bw].copy_from_slice(&win[W * y..W * y + bw]);
        }
        return;
    }
    fetch_window::<W>(src, xi - 2, yi - 2, bw + 5, bh + 5, &mut win);
    // G(x, y) is win[(y + 2) * W + x + 2].
    let g = |x: usize, y: usize| win[(y + 2) * W + x + 2];
    // Unscaled horizontal sums between columns x and x + 1, at window row r.
    let h1 = |x: usize, r: usize| {
        let b = r * W + x;
        tap6(
            win[b],
            win[b + 1],
            win[b + 2],
            win[b + 3],
            win[b + 4],
            win[b + 5],
        )
    };
    // Unscaled vertical sums between rows y and y + 1, at window column c.
    let v1 = |c: usize, y: usize| {
        let b = y * W + c;
        tap6(
            win[b],
            win[b + W],
            win[b + 2 * W],
            win[b + 3 * W],
            win[b + 4 * W],
            win[b + 5 * W],
        )
    };
    if fy == 0 {
        for y in 0..bh {
            for x in 0..bw {
                let b = clip((h1(x, y + 2) + 16) >> 5);
                dst[16 * y + x] = match fx {
                    1 => avg(g(x, y), b),
                    2 => b,
                    _ => avg(g(x + 1, y), b),
                };
            }
        }
        return;
    }
    if fx == 0 {
        for y in 0..bh {
            for x in 0..bw {
                let h = clip((v1(x + 2, y) + 16) >> 5);
                dst[16 * y + x] = match fy {
                    1 => avg(g(x, y), h),
                    2 => h,
                    _ => avg(g(x, y + 1), h),
                };
            }
        }
        return;
    }
    // Diagonal positions. `hb[r][x]`: b at window row r; `hv[y][c]`: h at block column
    // c (0..=bw, so m = hv[y][x + 1]).
    let mut hb = [0i32; W * 16];
    let mut hv = [0i32; 16 * 17];
    let rows_b = if fy == 2 { 0..0 } else { 2..bh + 4 };
    for r in rows_b {
        for x in 0..bw {
            hb[r * 16 + x] = clip((h1(x, r) + 16) >> 5);
        }
    }
    if fx != 2 {
        for y in 0..bh {
            for c in 0..=bw {
                hv[y * 17 + c] = clip((v1(c + 2, y) + 16) >> 5);
            }
        }
    }
    let b = |x: usize, y: usize| hb[(y + 2) * 16 + x];
    let s = |x: usize, y: usize| hb[(y + 3) * 16 + x];
    let h = |x: usize, y: usize| hv[y * 17 + x];
    let m = |x: usize, y: usize| hv[y * 17 + x + 1];
    if fx != 2 && fy != 2 {
        for y in 0..bh {
            for x in 0..bw {
                dst[16 * y + x] = match (fx, fy) {
                    (1, 1) => avg(b(x, y), h(x, y)),
                    (3, 1) => avg(b(x, y), m(x, y)),
                    (1, 3) => avg(h(x, y), s(x, y)),
                    _ => avg(m(x, y), s(x, y)),
                };
            }
        }
        return;
    }
    // j: the vertical filter over the unscaled horizontal sums.
    let mut raw = [0i32; W * 16];
    for r in 0..bh + 5 {
        for x in 0..bw {
            raw[r * 16 + x] = h1(x, r);
        }
    }
    for y in 0..bh {
        for x in 0..bw {
            let k = y * 16 + x;
            let j1 = tap6(
                raw[k],
                raw[k + 16],
                raw[k + 32],
                raw[k + 48],
                raw[k + 64],
                raw[k + 80],
            );
            let j = clip((j1 + 512) >> 10);
            dst[16 * y + x] = match (fx, fy) {
                (2, 2) => j,
                (2, 1) => avg(b(x, y), j),
                (2, 3) => avg(j, s(x, y)),
                (1, 2) => avg(h(x, y), j),
                _ => avg(j, m(x, y)), // (3, 2)
            };
        }
    }
}

/// Chroma sample interpolation (8.4.2.2.2), eighth-sample fraction `(fx, fy)`.
#[allow(clippy::too_many_arguments)]
fn chroma_mc(
    src: PlaneRef,
    xi: isize,
    yi: isize,
    fx: i32,
    fy: i32,
    bw: usize,
    bh: usize,
    dst: &mut [i32; 64],
) {
    const W: usize = 9;
    let mut win = [0i32; W * W];
    fetch_window::<W>(src, xi, yi, bw + 1, bh + 1, &mut win);
    let (wa, wb, wc, wd) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    for y in 0..bh {
        for x in 0..bw {
            let k = y * W + x;
            dst[y * 8 + x] =
                (wa * win[k] + wb * win[k + 1] + wc * win[k + W] + wd * win[k + W + 1] + 32) >> 6;
        }
    }
}

/// The 4x4 inverse transform (8.5.12), added to the prediction at `(x, y)`.
fn idct4_add(plane: &mut [u16], stride: usize, x: usize, y: usize, blk: &[i32], maxv: i32) {
    let mut d = [0i64; 16];
    for (o, &c) in d.iter_mut().zip(blk) {
        *o = i64::from(c);
    }
    for i in 0..4 {
        let r = &mut d[4 * i..4 * i + 4];
        let e0 = r[0] + r[2];
        let e1 = r[0] - r[2];
        let e2 = (r[1] >> 1) - r[3];
        let e3 = r[1] + (r[3] >> 1);
        r[0] = e0 + e3;
        r[1] = e1 + e2;
        r[2] = e1 - e2;
        r[3] = e0 - e3;
    }
    for j in 0..4 {
        let (f0, f1, f2, f3) = (d[j], d[4 + j], d[8 + j], d[12 + j]);
        let g0 = f0 + f2;
        let g1 = f0 - f2;
        let g2 = (f1 >> 1) - f3;
        let g3 = f1 + (f3 >> 1);
        d[j] = g0 + g3;
        d[4 + j] = g1 + g2;
        d[8 + j] = g1 - g2;
        d[12 + j] = g0 - g3;
    }
    for yy in 0..4 {
        let row = &mut plane[(y + yy) * stride + x..(y + yy) * stride + x + 4];
        for (xx, p) in row.iter_mut().enumerate() {
            *p = (i64::from(*p) + ((d[4 * yy + xx] + 32) >> 6)).clamp(0, i64::from(maxv)) as u16;
        }
    }
}

/// One dimension of the 8x8 inverse transform (8.5.13).
#[inline]
fn idct8_1d(d: [i64; 8]) -> [i64; 8] {
    let e0 = d[0] + d[4];
    let e1 = -d[3] + d[5] - d[7] - (d[7] >> 1);
    let e2 = d[0] - d[4];
    let e3 = d[1] + d[7] - d[3] - (d[3] >> 1);
    let e4 = (d[2] >> 1) - d[6];
    let e5 = -d[1] + d[7] + d[5] + (d[5] >> 1);
    let e6 = d[2] + (d[6] >> 1);
    let e7 = d[3] + d[5] + d[1] + (d[1] >> 1);
    let f0 = e0 + e6;
    let f1 = e1 + (e7 >> 2);
    let f2 = e2 + e4;
    let f3 = e3 + (e5 >> 2);
    let f4 = e2 - e4;
    let f5 = (e3 >> 2) - e5;
    let f6 = e0 - e6;
    let f7 = e7 - (e1 >> 2);
    [
        f0 + f7,
        f2 + f5,
        f4 + f3,
        f6 + f1,
        f6 - f1,
        f4 - f3,
        f2 - f5,
        f0 - f7,
    ]
}

fn idct8_add(plane: &mut [u16], stride: usize, x: usize, y: usize, blk: &[i32], maxv: i32) {
    let mut d = [0i64; 64];
    for (o, &c) in d.iter_mut().zip(blk) {
        *o = i64::from(c);
    }
    for i in 0..8 {
        let row: [i64; 8] = core::array::from_fn(|k| d[8 * i + k]);
        d[8 * i..8 * i + 8].copy_from_slice(&idct8_1d(row));
    }
    for j in 0..8 {
        let col: [i64; 8] = core::array::from_fn(|k| d[8 * k + j]);
        let out = idct8_1d(col);
        for k in 0..8 {
            d[8 * k + j] = out[k];
        }
    }
    for yy in 0..8 {
        let row = &mut plane[(y + yy) * stride + x..(y + yy) * stride + x + 8];
        for (xx, p) in row.iter_mut().enumerate() {
            *p = (i64::from(*p) + ((d[8 * yy + xx] + 32) >> 6)).clamp(0, i64::from(maxv)) as u16;
        }
    }
}

/// Intra 4x4 / 8x8 luma prediction (8.3.1.2, 8.3.2.2) of an `n` x `n` block from its top
/// row (`top[0..2n]`, the top-right half already substituted), left column and top-left
/// sample; `mode` is FFmpeg's numbering (the spec's 0-8, plus the DC variants 9-11).
fn pred_nxn(
    mode: i8,
    n: usize,
    top: &[i32; 16],
    left: &[i32; 8],
    tl: i32,
    half: i32,
    out: &mut [i32; 64],
) {
    // p[x, -1] for x >= -1 and p[-1, y] for y >= -1.
    let pt = |x: isize| if x < 0 { tl } else { top[x as usize] };
    let pl = |y: isize| if y < 0 { tl } else { left[y as usize] };
    let log2n = if n == 4 { 2 } else { 3 };
    let sum_top: i32 = top[..n].iter().sum();
    let sum_left: i32 = left[..n].iter().sum();
    let last_hu = 2 * n as isize - 3;
    for y in 0..n {
        for x in 0..n {
            let (xi, yi) = (x as isize, y as isize);
            let v = match mode {
                VERT_PRED => top[x],
                HOR_PRED => left[y],
                DC_PRED => (sum_top + sum_left + n as i32) >> (log2n + 1),
                LEFT_DC_PRED => (sum_left + (n as i32 >> 1)) >> log2n,
                TOP_DC_PRED => (sum_top + (n as i32 >> 1)) >> log2n,
                DIAG_DOWN_LEFT_PRED => {
                    if x == n - 1 && y == n - 1 {
                        (top[2 * n - 2] + 3 * top[2 * n - 1] + 2) >> 2
                    } else {
                        (top[x + y] + 2 * top[x + y + 1] + top[x + y + 2] + 2) >> 2
                    }
                }
                DIAG_DOWN_RIGHT_PRED => {
                    if x > y {
                        let k = xi - yi;
                        (pt(k - 2) + 2 * pt(k - 1) + pt(k) + 2) >> 2
                    } else if x < y {
                        let k = yi - xi;
                        (pl(k - 2) + 2 * pl(k - 1) + pl(k) + 2) >> 2
                    } else {
                        (pt(0) + 2 * tl + pl(0) + 2) >> 2
                    }
                }
                VERT_RIGHT_PRED => {
                    let z = 2 * xi - yi;
                    let k = xi - (yi >> 1);
                    if z >= 0 && z & 1 == 0 {
                        (pt(k - 1) + pt(k) + 1) >> 1
                    } else if z > 0 {
                        (pt(k - 2) + 2 * pt(k - 1) + pt(k) + 2) >> 2
                    } else if z == -1 {
                        (pl(0) + 2 * tl + pt(0) + 2) >> 2
                    } else {
                        let k = yi - 2 * xi;
                        (pl(k - 1) + 2 * pl(k - 2) + pl(k - 3) + 2) >> 2
                    }
                }
                HOR_DOWN_PRED => {
                    let z = 2 * yi - xi;
                    let k = yi - (xi >> 1);
                    if z >= 0 && z & 1 == 0 {
                        (pl(k - 1) + pl(k) + 1) >> 1
                    } else if z > 0 {
                        (pl(k - 2) + 2 * pl(k - 1) + pl(k) + 2) >> 2
                    } else if z == -1 {
                        (pl(0) + 2 * tl + pt(0) + 2) >> 2
                    } else {
                        let k = xi - 2 * yi;
                        (pt(k - 1) + 2 * pt(k - 2) + pt(k - 3) + 2) >> 2
                    }
                }
                VERT_LEFT_PRED => {
                    let k = x + (y >> 1);
                    if y & 1 == 0 {
                        (top[k] + top[k + 1] + 1) >> 1
                    } else {
                        (top[k] + 2 * top[k + 1] + top[k + 2] + 2) >> 2
                    }
                }
                HOR_UP_PRED => {
                    let z = xi + 2 * yi;
                    let k = y + (x >> 1);
                    if z < last_hu && z & 1 == 0 {
                        (left[k] + left[k + 1] + 1) >> 1
                    } else if z < last_hu {
                        (left[k] + 2 * left[k + 1] + left[k + 2] + 2) >> 2
                    } else if z == last_hu {
                        (left[n - 2] + 3 * left[n - 1] + 2) >> 2
                    } else {
                        left[n - 1]
                    }
                }
                _ => half, // DC_128_PRED
            };
            out[y * n + x] = v;
        }
    }
}

/// Weighted sample prediction of one partition from its per-list predictions (8.4.2.3).
#[derive(Clone, Copy)]
enum Weighting {
    /// Default: one list copied, or both averaged.
    Default,
    /// Explicit single list: `(weight, offset)` at `log2_denom`.
    Single { log2: u32, w: i32, o: i32 },
    /// Two lists with weights and the summed offsets (explicit, or implicit with 5 and 0).
    Bi { log2: u32, w0: i32, w1: i32, o: i32 },
}

/// Writes a partition's prediction (`p0`, and `p1` for two lists; row stride `ps`) into
/// the `w` x `h` block of `dst` (row stride `ds`), weighted per `wt` (FFmpeg's
/// `weight_h264_pixels` / `biweight_h264_pixels` forms, which equal the spec's).
#[allow(clippy::too_many_arguments)]
fn weigh_block(
    dst: &mut [u16],
    ds: usize,
    p0: &[i32],
    p1: Option<&[i32]>,
    ps: usize,
    w: usize,
    h: usize,
    wt: Weighting,
    bd_shift: u32,
    maxv: i32,
) {
    match (wt, p1) {
        (Weighting::Default, None) => {
            for y in 0..h {
                let (d, a) = (&mut dst[y * ds..y * ds + w], &p0[y * ps..y * ps + w]);
                for (d, &a) in d.iter_mut().zip(a) {
                    *d = a as u16;
                }
            }
        }
        (Weighting::Default, Some(p1)) => {
            for y in 0..h {
                let b = &p1[y * ps..y * ps + w];
                let (d, a) = (&mut dst[y * ds..y * ds + w], &p0[y * ps..y * ps + w]);
                for ((d, &a), &b) in d.iter_mut().zip(a).zip(b) {
                    *d = ((a + b + 1) >> 1) as u16;
                }
            }
        }
        (Weighting::Single { log2, w: wgt, o }, _) => {
            let mut off = o.wrapping_shl(log2 + bd_shift);
            if log2 > 0 {
                off = off.wrapping_add(1 << (log2 - 1));
            }
            for y in 0..h {
                let (d, a) = (&mut dst[y * ds..y * ds + w], &p0[y * ps..y * ps + w]);
                for (d, &a) in d.iter_mut().zip(a) {
                    *d = (a.wrapping_mul(wgt).wrapping_add(off) >> log2).clamp(0, maxv) as u16;
                }
            }
        }
        (Weighting::Bi { log2, w0, w1, o }, p1) => {
            let off = (o.wrapping_shl(bd_shift).wrapping_add(1) | 1).wrapping_shl(log2);
            for y in 0..h {
                let b = p1.map(|p1| &p1[y * ps..y * ps + w]);
                let (d, a) = (&mut dst[y * ds..y * ds + w], &p0[y * ps..y * ps + w]);
                for (k, (d, &a)) in d.iter_mut().zip(a).enumerate() {
                    let bb = b.map_or(a, |b| b[k]);
                    *d = (a
                        .wrapping_mul(w0)
                        .wrapping_add(bb.wrapping_mul(w1))
                        .wrapping_add(off)
                        >> (log2 + 1))
                        .clamp(0, maxv) as u16;
                }
            }
        }
    }
}

impl Ctx {
    /// Reconstructs the current macroblock (FFmpeg's `hl_decode_mb`, 4:2:0 frames).
    pub fn hl_decode_mb(&mut self) -> Result<(), DecodeError> {
        let t = self.t(self.mb_xy);
        let mb_type = self.mb_type[t];
        self.finish_mb_tables();
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        let (ly, lc) = (self.ly, self.lc);
        if is_pcm(mb_type) {
            for r in 0..16 {
                self.y[(y0 + r) * ly + x0..(y0 + r) * ly + x0 + 16]
                    .copy_from_slice(&self.pcm[16 * r..16 * r + 16]);
            }
            let (cx, cy) = (x0 / 2, y0 / 2);
            for r in 0..8 {
                self.cb[(cy + r) * lc + cx..(cy + r) * lc + cx + 8]
                    .copy_from_slice(&self.pcm[256 + 8 * r..256 + 8 * r + 8]);
                self.cr[(cy + r) * lc + cx..(cy + r) * lc + cx + 8]
                    .copy_from_slice(&self.pcm[320 + 8 * r..320 + 8 * r + 8]);
            }
            return Ok(());
        }
        if is_intra(mb_type) {
            self.pred_chroma_intra();
            self.pred_luma_intra(mb_type);
        } else {
            self.hl_motion(mb_type)?;
        }
        self.idct_luma(mb_type);
        if self.cbp & 0x30 != 0 {
            self.idct_chroma(mb_type);
        }
        Ok(())
    }

    // --- Intra prediction ----------------------------------------------------------------

    fn half(&self) -> i32 {
        1 << (self.bit_depth - 1)
    }

    fn pred_chroma_intra(&mut self) {
        let half = self.half();
        let (w, h) = (self.mb_w * 8, self.mb_h * 8);
        let (x0, y0) = (self.mb_x as isize * 8, self.mb_y as isize * 8);
        let mode = self.chroma_pred_mode;
        let maxv = self.pixel_max;
        let lc = self.lc;
        for plane in [&mut self.cb, &mut self.cr] {
            let top: [i32; 8] =
                core::array::from_fn(|x| nb(plane, lc, w, h, x0 + x as isize, y0 - 1, half));
            let left: [i32; 8] =
                core::array::from_fn(|y| nb(plane, lc, w, h, x0 - 1, y0 + y as isize, half));
            let tl = nb(plane, lc, w, h, x0 - 1, y0 - 1, half);
            let mut out = [0i32; 64];
            let st = |a: usize| top[a..a + 4].iter().sum::<i32>();
            let sl = |a: usize| left[a..a + 4].iter().sum::<i32>();
            match mode {
                DC_PRED8X8 | LEFT_DC_PRED8X8 | TOP_DC_PRED8X8 | DC_128_PRED8X8 => {
                    // Each 4x4 quarter (8.3.4.1-3).
                    for (qy, qx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        let v = match mode {
                            DC_PRED8X8 => match (qx, qy) {
                                (0, 0) => (st(0) + sl(0) + 4) >> 3,
                                (1, 1) => (st(4) + sl(4) + 4) >> 3,
                                (1, 0) => (st(4) + 2) >> 2,
                                _ => (sl(4) + 2) >> 2,
                            },
                            LEFT_DC_PRED8X8 => (sl(4 * qy) + 2) >> 2,
                            TOP_DC_PRED8X8 => (st(4 * qx) + 2) >> 2,
                            _ => half,
                        };
                        for y in 0..4 {
                            for x in 0..4 {
                                out[(4 * qy + y) * 8 + 4 * qx + x] = v;
                            }
                        }
                    }
                }
                HOR_PRED8X8 => {
                    for y in 0..8 {
                        out[8 * y..8 * y + 8].fill(left[y]);
                    }
                }
                VERT_PRED8X8 => {
                    for y in 0..8 {
                        out[8 * y..8 * y + 8].copy_from_slice(&top);
                    }
                }
                _ => {
                    // Plane (8.3.4.4).
                    let pt = |x: isize| if x < 0 { tl } else { top[x as usize] };
                    let pl = |y: isize| if y < 0 { tl } else { left[y as usize] };
                    let mut hh = 0;
                    let mut vv = 0;
                    for k in 0..4isize {
                        hh += (k as i32 + 1) * (pt(4 + k) - pt(2 - k));
                        vv += (k as i32 + 1) * (pl(4 + k) - pl(2 - k));
                    }
                    let a = 16 * (left[7] + top[7]);
                    let b = (34 * hh + 32) >> 6;
                    let c = (34 * vv + 32) >> 6;
                    for y in 0..8 {
                        for x in 0..8 {
                            out[8 * y + x] =
                                (a + b * (x as i32 - 3) + c * (y as i32 - 3) + 16) >> 5;
                        }
                    }
                }
            }
            for y in 0..8 {
                let row = &mut plane[(y0 as usize + y) * lc + x0 as usize..][..8];
                for (x, p) in row.iter_mut().enumerate() {
                    *p = out[8 * y + x].clamp(0, maxv) as u16;
                }
            }
        }
    }

    fn pred_luma_intra(&mut self, mb_type: u32) {
        let half = self.half();
        let (w, h) = (self.mb_w * 16, self.mb_h * 16);
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        let (ly, maxv) = (self.ly, self.pixel_max);
        if is_intra4x4(mb_type) {
            let size = if is_8x8dct(mb_type) { 8 } else { 4 };
            let step = if size == 8 { 4 } else { 1 };
            for i in (0..16).step_by(step) {
                let (bx, by) = block_xy(i);
                let (px, py) = ((x0 + bx) as isize, (y0 + by) as isize);
                let mode = self.intra4x4_pred_mode_cache[SCAN8[i]];
                let get = |c: &Ctx, x: isize, y: isize| nb(&c.y, ly, w, h, x, y, half);
                let mut top = [0i32; 16];
                let mut left = [0i32; 8];
                for (k, v) in top.iter_mut().enumerate().take(size) {
                    *v = get(self, px + k as isize, py - 1);
                }
                for (k, v) in left.iter_mut().enumerate().take(size) {
                    *v = get(self, px - 1, py + k as isize);
                }
                let mut tl = get(self, px - 1, py - 1);
                let mut out = [0i32; 64];
                if size == 4 {
                    let topright = (self.topright_samples_available << i) & 0x8000 != 0;
                    for k in 4..8 {
                        top[k] = if topright {
                            get(self, px + k as isize, py - 1)
                        } else {
                            top[3]
                        };
                    }
                    let left4: &[i32; 8] = &left;
                    let top16: &[i32; 16] = &top;
                    pred_nxn(mode, 4, top16, left4, tl, half, &mut out);
                } else {
                    let has_topleft = (self.topleft_samples_available << i) & 0x8000 != 0;
                    let has_topright = (self.topright_samples_available << i) & 0x4000 != 0;
                    for k in 8..16 {
                        top[k] = if has_topright {
                            get(self, px + k as isize, py - 1)
                        } else {
                            top[7]
                        };
                    }
                    // Reference sample filtering (8.3.2.2.1).
                    let mut ft = [0i32; 16];
                    ft[0] = if has_topleft {
                        (tl + 2 * top[0] + top[1] + 2) >> 2
                    } else {
                        (3 * top[0] + top[1] + 2) >> 2
                    };
                    for x in 1..15 {
                        ft[x] = (top[x - 1] + 2 * top[x] + top[x + 1] + 2) >> 2;
                    }
                    ft[15] = (top[14] + 3 * top[15] + 2) >> 2;
                    let mut fl = [0i32; 8];
                    fl[0] = if has_topleft {
                        (tl + 2 * left[0] + left[1] + 2) >> 2
                    } else {
                        (3 * left[0] + left[1] + 2) >> 2
                    };
                    for y in 1..7 {
                        fl[y] = (left[y - 1] + 2 * left[y] + left[y + 1] + 2) >> 2;
                    }
                    fl[7] = (left[6] + 3 * left[7] + 2) >> 2;
                    // Only the modes that need both neighbours read the corner.
                    tl = (left[0] + 2 * tl + top[0] + 2) >> 2;
                    top = ft;
                    left = fl;
                    pred_nxn(mode, 8, &top, &left, tl, half, &mut out);
                }
                for y in 0..size {
                    let row = &mut self.y[(py as usize + y) * ly + px as usize..][..size];
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = out[y * size + x].clamp(0, maxv) as u16;
                    }
                }
                // The residual is added before the next block is predicted from it.
                if self.non_zero_count_cache[SCAN8[i]] != 0 {
                    if size == 8 {
                        idct8_add(
                            &mut self.y,
                            ly,
                            px as usize,
                            py as usize,
                            &self.mb[16 * i..16 * i + 64],
                            maxv,
                        );
                    } else {
                        idct4_add(
                            &mut self.y,
                            ly,
                            px as usize,
                            py as usize,
                            &self.mb[16 * i..16 * i + 16],
                            maxv,
                        );
                    }
                }
            }
        } else {
            // Intra 16x16 (8.3.3).
            let (px, py) = (x0 as isize, y0 as isize);
            let top: [i32; 16] =
                core::array::from_fn(|x| nb(&self.y, ly, w, h, px + x as isize, py - 1, half));
            let left: [i32; 16] =
                core::array::from_fn(|y| nb(&self.y, ly, w, h, px - 1, py + y as isize, half));
            let tl = nb(&self.y, ly, w, h, px - 1, py - 1, half);
            let st: i32 = top.iter().sum();
            let sl: i32 = left.iter().sum();
            let mode = self.intra16x16_pred_mode;
            for y in 0..16 {
                for x in 0..16 {
                    let v = match mode {
                        VERT_PRED8X8 => top[x],
                        HOR_PRED8X8 => left[y],
                        DC_PRED8X8 => (st + sl + 16) >> 5,
                        LEFT_DC_PRED8X8 => (sl + 8) >> 4,
                        TOP_DC_PRED8X8 => (st + 8) >> 4,
                        PLANE_PRED8X8 => {
                            let pt = |x: isize| if x < 0 { tl } else { top[x as usize] };
                            let pl = |y: isize| if y < 0 { tl } else { left[y as usize] };
                            let mut hh = 0;
                            let mut vv = 0;
                            for k in 0..8isize {
                                hh += (k as i32 + 1) * (pt(8 + k) - pt(6 - k));
                                vv += (k as i32 + 1) * (pl(8 + k) - pl(6 - k));
                            }
                            let a = 16 * (left[15] + top[15]);
                            let b = (5 * hh + 32) >> 6;
                            let c = (5 * vv + 32) >> 6;
                            (a + b * (x as i32 - 7) + c * (y as i32 - 7) + 16) >> 5
                        }
                        _ => half,
                    };
                    self.y[(y0 + y) * ly + x0 + x] = v.clamp(0, maxv) as u16;
                }
            }
        }
    }

    // --- Residual ------------------------------------------------------------------------

    fn idct_luma(&mut self, mb_type: u32) {
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        let (ly, maxv) = (self.ly, self.pixel_max);
        if is_intra4x4(mb_type) {
            return; // added block by block during prediction
        }
        if is_intra16x16(mb_type) {
            if self.non_zero_count_cache[SCAN8[LUMA_DC_BLOCK_INDEX]] != 0 {
                // Luma DC: inverse Hadamard and scaling (8.5.10), into each block's DC.
                let c = &self.mb_luma_dc;
                let mut f = [0i32; 16];
                for i in 0..4 {
                    let z0 = c[4 * i].wrapping_add(c[4 * i + 1]);
                    let z1 = c[4 * i].wrapping_sub(c[4 * i + 1]);
                    let z2 = c[4 * i + 2].wrapping_sub(c[4 * i + 3]);
                    let z3 = c[4 * i + 2].wrapping_add(c[4 * i + 3]);
                    f[4 * i] = z0.wrapping_add(z3);
                    f[4 * i + 1] = z0.wrapping_sub(z3);
                    f[4 * i + 2] = z1.wrapping_sub(z2);
                    f[4 * i + 3] = z1.wrapping_add(z2);
                }
                let qmul = self.dequant4[0][self.qscale as usize][0];
                for j in 0..4 {
                    let z0 = f[j].wrapping_add(f[4 + j]);
                    let z1 = f[j].wrapping_sub(f[4 + j]);
                    let z2 = f[8 + j].wrapping_sub(f[12 + j]);
                    let z3 = f[8 + j].wrapping_add(f[12 + j]);
                    let out = [
                        z0.wrapping_add(z3),
                        z0.wrapping_sub(z3),
                        z1.wrapping_sub(z2),
                        z1.wrapping_add(z2),
                    ];
                    for (row, v) in out.into_iter().enumerate() {
                        let n = block_index(j, row);
                        self.mb[16 * n] = v.wrapping_mul(qmul).wrapping_add(128) >> 8;
                    }
                }
            }
            for i in 0..16 {
                let (bx, by) = block_xy(i);
                if self.non_zero_count_cache[SCAN8[i]] != 0 || self.mb[16 * i] != 0 {
                    idct4_add(
                        &mut self.y,
                        ly,
                        x0 + bx,
                        y0 + by,
                        &self.mb[16 * i..16 * i + 16],
                        maxv,
                    );
                }
            }
        } else if self.cbp & 15 != 0 {
            if is_8x8dct(mb_type) {
                for i in (0..16).step_by(4) {
                    if self.non_zero_count_cache[SCAN8[i]] != 0 {
                        let (bx, by) = block_xy(i);
                        idct8_add(
                            &mut self.y,
                            ly,
                            x0 + bx,
                            y0 + by,
                            &self.mb[16 * i..16 * i + 64],
                            maxv,
                        );
                    }
                }
            } else {
                for i in 0..16 {
                    if self.non_zero_count_cache[SCAN8[i]] != 0 {
                        let (bx, by) = block_xy(i);
                        idct4_add(
                            &mut self.y,
                            ly,
                            x0 + bx,
                            y0 + by,
                            &self.mb[16 * i..16 * i + 16],
                            maxv,
                        );
                    }
                }
            }
        }
    }

    fn idct_chroma(&mut self, mb_type: u32) {
        let (cx, cy) = (self.mb_x * 8, self.mb_y * 8);
        let (lc, maxv) = (self.lc, self.pixel_max);
        for c in 0..2 {
            if self.non_zero_count_cache[SCAN8[CHROMA_DC_BLOCK_INDEX + c]] != 0 {
                // Chroma DC: 2x2 transform and scaling (8.5.11), FFmpeg's `(f * qmul) >> 7`.
                let list = c + 1 + if is_intra(mb_type) { 0 } else { 3 };
                let qmul = self.dequant4[list][self.chroma_qp[c] as usize][0];
                let d = self.chroma_dc[c];
                let f = [
                    d[0].wrapping_add(d[1])
                        .wrapping_add(d[2])
                        .wrapping_add(d[3]),
                    d[0].wrapping_sub(d[1])
                        .wrapping_add(d[2])
                        .wrapping_sub(d[3]),
                    d[0].wrapping_add(d[1])
                        .wrapping_sub(d[2])
                        .wrapping_sub(d[3]),
                    d[0].wrapping_sub(d[1])
                        .wrapping_sub(d[2])
                        .wrapping_add(d[3]),
                ];
                for (i, v) in f.into_iter().enumerate() {
                    self.mb[16 * (16 + 16 * c + i)] = v.wrapping_mul(qmul) >> 7;
                }
            }
            let plane = if c == 0 { &mut self.cb } else { &mut self.cr };
            for i in 0..4 {
                let n = 16 + 16 * c + i;
                if self.non_zero_count_cache[SCAN8[n]] != 0 || self.mb[16 * n] != 0 {
                    let (bx, by) = (4 * (i & 1), 4 * (i >> 1));
                    idct4_add(
                        plane,
                        lc,
                        cx + bx,
                        cy + by,
                        &self.mb[16 * n..16 * n + 16],
                        maxv,
                    );
                }
            }
        }
    }

    // --- Inter prediction ----------------------------------------------------------------

    fn hl_motion(&mut self, mb_type: u32) -> Result<(), DecodeError> {
        let d = |t: u32, part: u32| (is_dir(t, part, 0), is_dir(t, part, 1));
        if mb_type & MB_TYPE_16X16 != 0 {
            self.mc_part(0, 0, 0, 16, 16, d(mb_type, 0))?;
        } else if mb_type & MB_TYPE_16X8 != 0 {
            self.mc_part(0, 0, 0, 16, 8, d(mb_type, 0))?;
            self.mc_part(8, 0, 8, 16, 8, d(mb_type, 1))?;
        } else if mb_type & MB_TYPE_8X16 != 0 {
            self.mc_part(0, 0, 0, 8, 16, d(mb_type, 0))?;
            self.mc_part(4, 8, 0, 8, 16, d(mb_type, 1))?;
        } else {
            for i in 0..4 {
                let sub = self.sub_mb_type[i];
                let n = 4 * i;
                let (x, y) = (8 * (i & 1), 8 * (i >> 1));
                let lists = d(sub, 0);
                if sub & MB_TYPE_16X16 != 0 {
                    self.mc_part(n, x, y, 8, 8, lists)?;
                } else if sub & MB_TYPE_16X8 != 0 {
                    self.mc_part(n, x, y, 8, 4, lists)?;
                    self.mc_part(n + 2, x, y + 4, 8, 4, lists)?;
                } else if sub & MB_TYPE_8X16 != 0 {
                    self.mc_part(n, x, y, 4, 8, lists)?;
                    self.mc_part(n + 1, x + 4, y, 4, 8, lists)?;
                } else {
                    for j in 0..4 {
                        self.mc_part(n + j, x + 4 * (j & 1), y + 4 * (j >> 1), 4, 4, lists)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Predicts one partition: block `n`'s motion, at luma offset `(x, y)` in the
    /// macroblock, `w` x `h` samples.
    fn mc_part(
        &mut self,
        n: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        lists: (bool, bool),
    ) -> Result<(), DecodeError> {
        let s = SCAN8[n];
        let maxv = self.pixel_max;
        let (lx, ly_) = (self.mb_x * 16 + x, self.mb_y * 16 + y);
        let (cx, cy) = (lx / 2, ly_ / 2);
        let (cw, ch) = (w / 2, h / 2);
        // Reject a reference from another geometry before any plane is indexed.
        // The check borrows `self`, so it has to happen before `mc_luma` is borrowed.
        for list in 0..2 {
            let on = if list == 0 { lists.0 } else { lists.1 };
            if !on {
                continue;
            }
            let r = self.ref_cache[list][s];
            let entry = usize::try_from(r)
                .ok()
                .and_then(|r| self.ref_list[list].get(r))
                .ok_or(DecodeError::Unsupported("invalid reference index"))?;
            if !self.reference_matches(&entry.pic) {
                return Err(DecodeError::Unsupported("reference picture size mismatch"));
            }
        }
        let luma = &mut self.mc_luma;
        let chroma = &mut self.mc_chroma;
        let mut refs = [0usize; 2];
        let mut used = [false; 2];
        for list in 0..2 {
            let on = if list == 0 { lists.0 } else { lists.1 };
            if !on {
                continue;
            }
            let r = self.ref_cache[list][s];
            let entry = usize::try_from(r)
                .ok()
                .and_then(|r| self.ref_list[list].get(r))
                .ok_or(DecodeError::Unsupported("invalid reference index"))?;
            refs[list] = r as usize;
            used[list] = true;
            let pic = entry.pic.clone();
            let mv = self.mv_cache[list][s];
            let (mvx, mvy) = (isize::from(mv[0]), isize::from(mv[1]));
            let plane = PlaneRef {
                data: &pic.y,
                stride: self.ly,
                width: self.mb_w * 16,
                height: self.mb_h * 16,
            };
            let mx = mvx + 4 * lx as isize;
            let my = mvy + 4 * ly_ as isize;
            luma_mc(
                plane,
                mx >> 2,
                my >> 2,
                (mx & 3) as usize,
                (my & 3) as usize,
                w,
                h,
                maxv,
                &mut luma[list],
            );
            let cmx = mvx + 8 * cx as isize;
            let cmy = mvy + 8 * cy as isize;
            for (c, data) in [&pic.cb, &pic.cr].into_iter().enumerate() {
                let plane = PlaneRef {
                    data,
                    stride: self.lc,
                    width: self.mb_w * 8,
                    height: self.mb_h * 8,
                };
                chroma_mc(
                    plane,
                    cmx >> 3,
                    cmy >> 3,
                    (cmx & 7) as i32,
                    (cmy & 7) as i32,
                    cw,
                    ch,
                    &mut chroma[list][c],
                );
            }
        }
        if !used[0] && !used[1] {
            return Err(DecodeError::Unsupported("partition without prediction"));
        }

        let pwt = &self.pwt;
        let bi = used[0] && used[1];
        let weighted = (pwt.use_weight == 2 && bi && pwt.implicit[refs[0]][refs[1]] != 32)
            || pwt.use_weight == 1;
        let (luma_wt, chroma_wt): (Weighting, [Weighting; 2]) = if !weighted {
            (Weighting::Default, [Weighting::Default; 2])
        } else if bi {
            if pwt.use_weight == 2 {
                let w0 = pwt.implicit[refs[0]][refs[1]];
                let wt = Weighting::Bi {
                    log2: 5,
                    w0,
                    w1: 64 - w0,
                    o: 0,
                };
                (wt, [wt; 2])
            } else {
                let (r0, r1) = (refs[0], refs[1]);
                let lw = Weighting::Bi {
                    log2: pwt.luma_log2_denom,
                    w0: pwt.luma[r0][0].0,
                    w1: pwt.luma[r1][1].0,
                    o: pwt.luma[r0][0].1 + pwt.luma[r1][1].1,
                };
                let cw = |c: usize| Weighting::Bi {
                    log2: pwt.chroma_log2_denom,
                    w0: pwt.chroma[r0][0][c].0,
                    w1: pwt.chroma[r1][1][c].0,
                    o: pwt.chroma[r0][0][c].1 + pwt.chroma[r1][1][c].1,
                };
                (lw, [cw(0), cw(1)])
            }
        } else {
            let list = usize::from(used[1]);
            let r = refs[list];
            let lw = Weighting::Single {
                log2: pwt.luma_log2_denom,
                w: pwt.luma[r][list].0,
                o: pwt.luma[r][list].1,
            };
            let cw = |c: usize| {
                if pwt.use_weight_chroma {
                    Weighting::Single {
                        log2: pwt.chroma_log2_denom,
                        w: pwt.chroma[r][list][c].0,
                        o: pwt.chroma[r][list][c].1,
                    }
                } else {
                    Weighting::Default
                }
            };
            (lw, [cw(0), cw(1)])
        };

        let bd_shift = self.bit_depth - 8;
        let first = usize::from(!used[0]);
        let (ly, lc) = (self.ly, self.lc);
        weigh_block(
            &mut self.y[ly_ * ly + lx..],
            ly,
            &luma[first],
            bi.then_some(&luma[1][..]),
            16,
            w,
            h,
            luma_wt,
            bd_shift,
            maxv,
        );
        for c in 0..2 {
            let plane = if c == 0 { &mut self.cb } else { &mut self.cr };
            weigh_block(
                &mut plane[cy * lc + cx..],
                lc,
                &chroma[first][c],
                bi.then_some(&chroma[1][c][..]),
                8,
                cw,
                ch,
                chroma_wt[c],
                bd_shift,
                maxv,
            );
        }
        Ok(())
    }
}

/// Luma 4x4 block `i`'s offset in its macroblock.
#[inline]
fn block_xy(i: usize) -> (usize, usize) {
    let x = 8 * ((i >> 2) & 1) + 4 * (i & 1);
    let y = 8 * (i >> 3) + 4 * ((i >> 1) & 1);
    (x, y)
}

/// The luma 4x4 block at column `bx`, row `by` (in blocks).
#[inline]
fn block_index(bx: usize, by: usize) -> usize {
    8 * (by >> 1) + 4 * (bx >> 1) + 2 * (by & 1) + (bx & 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_geometry_round_trips() {
        for i in 0..16 {
            let (x, y) = block_xy(i);
            assert_eq!(block_index(x / 4, y / 4), i);
        }
        assert_eq!(block_xy(5), (12, 0));
        assert_eq!(block_xy(10), (0, 12));
    }

    #[test]
    fn dc_only_idct_matches_the_shortcut() {
        let mut plane = [100u16; 16];
        let mut blk = [0i32; 16];
        blk[0] = 64 * 5 - 32;
        idct4_add(&mut plane, 4, 0, 0, &blk, 1023);
        assert!(plane.iter().all(|&p| p == 105));
    }

    #[test]
    fn weighted_prediction_matches_the_spec_forms() {
        let weigh = |a: i32, b: Option<i32>, wt: Weighting| {
            let mut out = [0u16; 1];
            let p1 = b.map(|b| [b]);
            weigh_block(
                &mut out,
                1,
                &[a],
                p1.as_ref().map(|p| &p[..]),
                1,
                1,
                1,
                wt,
                2,
                1023,
            );
            out[0]
        };
        // Explicit single list, logWD 5, w 40, o 3 at 10 bits: ((x*w + 16) >> 5) + (3 << 2).
        for x in [0, 1, 511, 1023] {
            let want = (((x * 40 + 16) >> 5) + 12).clamp(0, 1023);
            let got = weigh(
                x,
                None,
                Weighting::Single {
                    log2: 5,
                    w: 40,
                    o: 3,
                },
            );
            assert_eq!(i32::from(got), want);
        }
        // Explicit bi, o0 + o1 odd.
        for (a, b) in [(0, 0), (100, 900), (1023, 1023), (5, 6)] {
            let (w0, w1, o0, o1, d) = (30, 40, 3, -2, 5);
            let want = (((a * w0 + b * w1 + (1 << d)) >> (d + 1)) + ((o0 * 4 + o1 * 4 + 1) >> 1))
                .clamp(0, 1023);
            let got = weigh(
                a,
                Some(b),
                Weighting::Bi {
                    log2: d as u32,
                    w0,
                    w1,
                    o: o0 + o1,
                },
            );
            assert_eq!(i32::from(got), want, "{a} {b}");
        }
    }
}
