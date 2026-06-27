//! Inter prediction primitives: motion compensation and motion-vector
//! prediction. Shared by the encoder (reconstruction) and decoder so the two
//! agree bit-for-bit. Motion vectors are in quarter-pel units; phase 4b uses
//! the integer part only (full-pel), with the 6-tap/bilinear sub-pel filters
//! arriving in 4c.

/// Median of three values (`a + b + c − min − max`).
#[inline]
pub fn median3(a: i32, b: i32, c: i32) -> i32 {
    a + b + c - a.min(b).min(c) - a.max(b).max(c)
}

/// A motion-vector predictor neighbor: whether the neighbor block is available
/// (inside the picture/decoded), its motion vector, and its reference index
/// (`-1` for intra/unavailable neighbors, which contribute a zero MV with a
/// non-matching reference).
#[derive(Clone, Copy)]
pub struct MvNeighbor {
    pub available: bool,
    pub mv: (i32, i32),
    pub ref_idx: i32,
}

impl MvNeighbor {
    pub const NONE: MvNeighbor = MvNeighbor {
        available: false,
        mv: (0, 0),
        ref_idx: -1,
    };
}

/// Median motion-vector prediction for a partition (spec §8.4.1.3.1), against
/// the current partition's reference `cur_ref`. `a`/`b`/`c` are the left, above,
/// and above-right neighbors. If exactly one neighbor shares `cur_ref`, its MV is
/// the predictor; otherwise the component-wise median is used.
pub fn predict_mv(a: MvNeighbor, b: MvNeighbor, c: MvNeighbor, cur_ref: i32) -> (i32, i32) {
    // Per-neighbor (mv, refIdx): a usable inter neighbor keeps its mv+ref, else
    // a zero MV with ref −1 (never matches a valid `cur_ref` ≥ 0).
    let resolve = |n: MvNeighbor| -> ((i32, i32), i32) {
        if n.available && n.ref_idx >= 0 {
            (n.mv, n.ref_idx)
        } else {
            ((0, 0), -1)
        }
    };
    let (mva, ra) = resolve(a);
    let (mut mvb, mut rb) = resolve(b);
    let (mut mvc, mut rc) = resolve(c);

    // If both B and C are unavailable but A is available, B and C take A.
    if !b.available && !c.available && a.available {
        mvb = mva;
        rb = ra;
        mvc = mva;
        rc = ra;
    }

    let matches = (ra == cur_ref) as i32 + (rb == cur_ref) as i32 + (rc == cur_ref) as i32;
    if matches == 1 {
        if ra == cur_ref {
            mva
        } else if rb == cur_ref {
            mvb
        } else {
            mvc
        }
    } else {
        (median3(mva.0, mvb.0, mvc.0), median3(mva.1, mvb.1, mvc.1))
    }
}

/// Directional MV prediction for a sub-partition (spec §8.4.1.3.2) against the
/// partition's reference `cur_ref`. `mode` is the inter `mb_type` (0 = 16×16,
/// 1 = 16×8, 2 = 8×16). 16×8/8×16 use a specific neighbor directly when it shares
/// `cur_ref`; otherwise (and always for 16×16) the median.
pub fn predict_partition_mv(
    mode: u8,
    part: usize,
    a: MvNeighbor,
    b: MvNeighbor,
    c: MvNeighbor,
    cur_ref: i32,
) -> (i32, i32) {
    let m = |n: MvNeighbor| n.available && n.ref_idx == cur_ref;
    match (mode, part) {
        (1, 0) if m(b) => b.mv, // 16×8 top → above
        (1, 1) if m(a) => a.mv, // 16×8 bottom → left
        (2, 0) if m(a) => a.mv, // 8×16 left → left
        (2, 1) if m(c) => c.mv, // 8×16 right → above-right
        _ => predict_mv(a, b, c, cur_ref),
    }
}

/// Inter `mb_type` → luma partition regions `(x, y, w, h)` in samples.
pub fn inter_partitions(mode: u8) -> &'static [(usize, usize, usize, usize)] {
    match mode {
        1 => &[(0, 0, 16, 8), (0, 8, 16, 8)], // P_16x8
        2 => &[(0, 0, 8, 16), (8, 0, 8, 16)], // P_8x16
        _ => &[(0, 0, 16, 16)],               // P_L0_16x16
    }
}

/// Reference sample with edge clamping.
#[inline]
fn at(reference: &[u8], cw: usize, ch: usize, x: isize, y: isize) -> i32 {
    let xx = x.clamp(0, cw as isize - 1) as usize;
    let yy = y.clamp(0, ch as isize - 1) as usize;
    reference[yy * cw + xx] as i32
}

#[inline]
fn clip_u8(v: i32) -> i32 {
    v.clamp(0, 255)
}

/// Per-pixel quarter-pel luma sample (spec §8.4.2.2.1) — the readable reference
/// kept as the bit-exactness oracle for the block-kernel MC below.
#[cfg(test)]
fn luma_sample(reference: &[u8], cw: usize, ch: usize, ix: isize, iy: isize, fx: i32, fy: i32) -> i32 {
    let g = |dx: isize, dy: isize| at(reference, cw, ch, ix + dx, iy + dy);
    if fx == 0 && fy == 0 {
        return g(0, 0);
    }
    let hor6 = |dy: isize| g(-2, dy) - 5 * g(-1, dy) + 20 * g(0, dy) + 20 * g(1, dy) - 5 * g(2, dy) + g(3, dy);
    let ver6 = |dx: isize| g(dx, -2) - 5 * g(dx, -1) + 20 * g(dx, 0) + 20 * g(dx, 1) - 5 * g(dx, 2) + g(dx, 3);
    let b = || clip_u8((hor6(0) + 16) >> 5);
    let h = || clip_u8((ver6(0) + 16) >> 5);
    let m = || clip_u8((ver6(1) + 16) >> 5);
    let s = || clip_u8((hor6(1) + 16) >> 5);
    let j = || {
        let j1 = hor6(-2) - 5 * hor6(-1) + 20 * hor6(0) + 20 * hor6(1) - 5 * hor6(2) + hor6(3);
        clip_u8((j1 + 512) >> 10)
    };
    match (fx, fy) {
        (1, 0) => (g(0, 0) + b() + 1) >> 1,
        (2, 0) => b(),
        (3, 0) => (g(1, 0) + b() + 1) >> 1,
        (0, 1) => (g(0, 0) + h() + 1) >> 1,
        (1, 1) => (b() + h() + 1) >> 1,
        (2, 1) => (b() + j() + 1) >> 1,
        (3, 1) => (b() + m() + 1) >> 1,
        (0, 2) => h(),
        (1, 2) => (h() + j() + 1) >> 1,
        (2, 2) => j(),
        (3, 2) => (j() + m() + 1) >> 1,
        (0, 3) => (g(0, 1) + h() + 1) >> 1,
        (1, 3) => (h() + s() + 1) >> 1,
        (2, 3) => (j() + s() + 1) >> 1,
        _ => (m() + s() + 1) >> 1,
    }
}

// ---- Block-level luma MC, mirroring openh264 `mc.cpp` (`McHorVerNN_c`) ----
//
// openh264 computes each half-pel plane once per block (not per pixel) and
// averages clipped planes for the quarter positions. Our `luma_sample` recomputed
// the 6-tap for every pixel; these kernels compute each plane once. Bit-identical:
// the 6-tap is separable with exact (un-rounded) integer intermediates, so
// horizontal-then-vertical equals vertical-then-horizontal, and the clamped tile
// reproduces `at()` exactly.

/// Max luma interpolation tile: a 16×16 block plus the 6-tap halo (2 left/up,
/// 3 right/down) → 21×21.
const LUMA_TILE: usize = 21;

/// Extracts the `(bw+5)×(bh+5)` reference neighbourhood around the full-pel origin
/// `(ix0,iy0)`, clamping at the frame border — the edge-extended input openh264's
/// kernels assume. The block's top-left sample lands at tile `(2,2)`.
fn luma_tile(
    reference: &[u8],
    cw: usize,
    ch: usize,
    ix0: isize,
    iy0: isize,
    bw: usize,
    bh: usize,
) -> ([u8; LUMA_TILE * LUMA_TILE], usize) {
    let ts = bw + 5;
    let mut t = [0u8; LUMA_TILE * LUMA_TILE];
    // Interior fast path: the whole `(bw+5)×(bh+5)` halo is inside the frame, so no
    // edge clamp is needed. Extract by contiguous row copies (a vectorized memcpy)
    // — the unconditional per-pixel `clamp` on the slow path defeats
    // autovectorization even when (as here) it would always be a no-op.
    if ix0 - 2 >= 0
        && iy0 - 2 >= 0
        && ix0 - 2 + ts as isize <= cw as isize
        && iy0 - 2 + (bh + 5) as isize <= ch as isize
    {
        let (rx0, ry0) = ((ix0 - 2) as usize, (iy0 - 2) as usize);
        for ty in 0..bh + 5 {
            let src = (ry0 + ty) * cw + rx0;
            t[ty * ts..ty * ts + ts].copy_from_slice(&reference[src..src + ts]);
        }
        return (t, ts);
    }
    for ty in 0..bh + 5 {
        let ry = (iy0 - 2 + ty as isize).clamp(0, ch as isize - 1) as usize * cw;
        for tx in 0..ts {
            let rx = (ix0 - 2 + tx as isize).clamp(0, cw as isize - 1) as usize;
            t[ty * ts + tx] = reference[ry + rx];
        }
    }
    (t, ts)
}

/// Horizontal half-pel plane (`McHorVer20`): `clip((6tapₕ + 16) >> 5)`, block
/// shifted by `(dr, dc)` tile rows/cols.
fn luma_h(t: &[u8], ts: usize, bw: usize, bh: usize, dr: usize, dc: usize, dst: &mut [u8]) {
    #[cfg(feature = "asm")]
    if bw == 16 || bw == 8 {
        rusty_h264_accel::mc_hor20(t, (2 + dr) * ts + 2 + dc, ts, dst, bw, bh);
        return;
    }
    for r in 0..bh {
        let base = (2 + r + dr) * ts + 2 + dc;
        for c in 0..bw {
            let p = base + c;
            let f = t[p - 2] as i32 - 5 * t[p - 1] as i32 + 20 * t[p] as i32 + 20 * t[p + 1] as i32
                - 5 * t[p + 2] as i32
                + t[p + 3] as i32;
            dst[r * bw + c] = clip_u8((f + 16) >> 5) as u8;
        }
    }
}

/// Vertical half-pel plane (`McHorVer02`): `clip((6tapᵥ + 16) >> 5)`.
fn luma_v(t: &[u8], ts: usize, bw: usize, bh: usize, dr: usize, dc: usize, dst: &mut [u8]) {
    #[cfg(feature = "asm")]
    if bw == 16 || bw == 8 {
        rusty_h264_accel::mc_ver02(t, (2 + dr) * ts + 2 + dc, ts, dst, bw, bh);
        return;
    }
    for r in 0..bh {
        let base = (2 + r + dr) * ts + 2 + dc;
        for c in 0..bw {
            let p = base + c;
            let f = t[p - 2 * ts] as i32 - 5 * t[p - ts] as i32 + 20 * t[p] as i32
                + 20 * t[p + ts] as i32
                - 5 * t[p + 2 * ts] as i32
                + t[p + 3 * ts] as i32;
            dst[r * bw + c] = clip_u8((f + 16) >> 5) as u8;
        }
    }
}

/// Centre half-pel plane (`McHorVer22`): vertical 6-tap to 16-bit intermediates,
/// then horizontal 6-tap — `clip((·+ 512) >> 10)`.
fn luma_centre(t: &[u8], ts: usize, bw: usize, bh: usize, dst: &mut [u8]) {
    #[cfg(feature = "asm")]
    if bw == 16 || bw == 8 {
        rusty_h264_accel::mc_centre(t, ts, dst, bw, bh);
        return;
    }
    let mut itmp = [0i32; LUMA_TILE];
    for r in 0..bh {
        let base = (2 + r) * ts;
        for (j, slot) in itmp[..bw + 5].iter_mut().enumerate() {
            let p = base + j;
            *slot = t[p - 2 * ts] as i32 - 5 * t[p - ts] as i32 + 20 * t[p] as i32
                + 20 * t[p + ts] as i32
                - 5 * t[p + 2 * ts] as i32
                + t[p + 3 * ts] as i32;
        }
        for c in 0..bw {
            let f = itmp[c] - 5 * itmp[c + 1] + 20 * itmp[c + 2] + 20 * itmp[c + 3] - 5 * itmp[c + 4]
                + itmp[c + 5];
            dst[r * bw + c] = clip_u8((f + 512) >> 10) as u8;
        }
    }
}

/// `PixelAvg_c`: `(a + b + 1) >> 1` of two clipped planes.
fn pixel_avg(a: &[u8], b: &[u8], n: usize, dst: &mut [u8]) {
    for i in 0..n {
        dst[i] = ((a[i] as i32 + b[i] as i32 + 1) >> 1) as u8;
    }
}

/// `PixelAvg_c` of a half-pel plane with a full-pel block shifted by `(dr, dc)`.
fn avg_full(t: &[u8], ts: usize, bw: usize, bh: usize, dr: usize, dc: usize, half: &[u8], dst: &mut [u8]) {
    for r in 0..bh {
        let base = (2 + r + dr) * ts + 2 + dc;
        for c in 0..bw {
            dst[r * bw + c] = ((t[base + c] as i32 + half[r * bw + c] as i32 + 1) >> 1) as u8;
        }
    }
}

/// The `McLuma_c` `[mvx&3][mvy&3]` dispatch over the clamped tile (sub-pel only;
/// `(0,0)` is handled by the full-pel copy path in [`mc_luma`]).
fn mc_luma_subpel(t: &[u8], ts: usize, bw: usize, bh: usize, fx: i32, fy: i32, out: &mut [u8]) {
    let n = bw * bh;
    let mut a = [0u8; 256];
    let mut b = [0u8; 256];
    match (fx, fy) {
        (2, 0) => luma_h(t, ts, bw, bh, 0, 0, out),
        (0, 2) => luma_v(t, ts, bw, bh, 0, 0, out),
        (2, 2) => luma_centre(t, ts, bw, bh, out),
        (1, 0) => {
            luma_h(t, ts, bw, bh, 0, 0, &mut a);
            avg_full(t, ts, bw, bh, 0, 0, &a, out);
        }
        (3, 0) => {
            luma_h(t, ts, bw, bh, 0, 0, &mut a);
            avg_full(t, ts, bw, bh, 0, 1, &a, out);
        }
        (0, 1) => {
            luma_v(t, ts, bw, bh, 0, 0, &mut a);
            avg_full(t, ts, bw, bh, 0, 0, &a, out);
        }
        (0, 3) => {
            luma_v(t, ts, bw, bh, 0, 0, &mut a);
            avg_full(t, ts, bw, bh, 1, 0, &a, out);
        }
        (1, 1) => {
            luma_h(t, ts, bw, bh, 0, 0, &mut a);
            luma_v(t, ts, bw, bh, 0, 0, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (3, 1) => {
            luma_h(t, ts, bw, bh, 0, 0, &mut a);
            luma_v(t, ts, bw, bh, 0, 1, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (1, 3) => {
            luma_h(t, ts, bw, bh, 1, 0, &mut a);
            luma_v(t, ts, bw, bh, 0, 0, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (3, 3) => {
            luma_h(t, ts, bw, bh, 1, 0, &mut a);
            luma_v(t, ts, bw, bh, 0, 1, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (2, 1) => {
            luma_h(t, ts, bw, bh, 0, 0, &mut a);
            luma_centre(t, ts, bw, bh, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (2, 3) => {
            luma_h(t, ts, bw, bh, 1, 0, &mut a);
            luma_centre(t, ts, bw, bh, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (1, 2) => {
            luma_v(t, ts, bw, bh, 0, 0, &mut a);
            luma_centre(t, ts, bw, bh, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        (3, 2) => {
            luma_v(t, ts, bw, bh, 0, 1, &mut a);
            luma_centre(t, ts, bw, bh, &mut b);
            pixel_avg(&a, &b, n, out);
        }
        _ => unreachable!("(0,0) is the full-pel path"),
    }
}

/// Quarter-pel motion compensation of a `bw`×`bh` luma block (`McLuma_c`).
#[allow(clippy::too_many_arguments)]
pub fn mc_luma(
    reference: &[u8],
    cw: usize,
    ch: usize,
    x0: usize,
    y0: usize,
    bw: usize,
    bh: usize,
    mvx: i32,
    mvy: i32,
    out: &mut [u8],
) {
    let (ix0, iy0) = (x0 as isize + (mvx >> 2) as isize, y0 as isize + (mvy >> 2) as isize);
    let (fx, fy) = (mvx & 3, mvy & 3);
    if fx == 0 && fy == 0 {
        // Full-pel: a verbatim copy of the reference (`McCopy_c`). Interior → a
        // row-wise slice copy (auto-vectorized); edge → per-pixel clamped.
        if ix0 >= 0
            && iy0 >= 0
            && ix0 + bw as isize <= cw as isize
            && iy0 + bh as isize <= ch as isize
        {
            let (rx, ry) = (ix0 as usize, iy0 as usize);
            for dy in 0..bh {
                out[dy * bw..dy * bw + bw]
                    .copy_from_slice(&reference[(ry + dy) * cw + rx..][..bw]);
            }
        } else {
            for dy in 0..bh {
                for dx in 0..bw {
                    out[dy * bw + dx] =
                        at(reference, cw, ch, ix0 + dx as isize, iy0 + dy as isize) as u8;
                }
            }
        }
        return;
    }
    // Sub-pel: extract the clamped tile once, then run the openh264 block kernels.
    let (t, ts) = luma_tile(reference, cw, ch, ix0, iy0, bw, bh);
    mc_luma_subpel(&t, ts, bw, bh, fx, fy, out);
}

/// Eighth-pel bilinear motion compensation of a `bw`×`bh` chroma block (spec
/// §8.4.2.2.2). The chroma motion vector equals the luma MV; for 4:2:0 it is
/// interpreted at eighth-chroma-sample resolution.
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma(
    reference: &[u8],
    cw: usize,
    ch: usize,
    x0: usize,
    y0: usize,
    bw: usize,
    bh: usize,
    mvx: i32,
    mvy: i32,
    out: &mut [u8],
) {
    let (ix0, iy0) = (x0 as isize + (mvx >> 3) as isize, y0 as isize + (mvy >> 3) as isize);
    let (fx, fy) = (mvx & 7, mvy & 7);
    // Full-pel and fully inside the frame: `(64·a + 32) >> 6 == a`, a verbatim copy.
    // Skip the per-pixel bilinear + 4× clamped `at()`; copy row-wise. Bit-identical.
    if fx == 0
        && fy == 0
        && ix0 >= 0
        && iy0 >= 0
        && ix0 + bw as isize <= cw as isize
        && iy0 + bh as isize <= ch as isize
    {
        let (rx, ry) = (ix0 as usize, iy0 as usize);
        for dy in 0..bh {
            let src = &reference[(ry + dy) * cw + rx..][..bw];
            out[dy * bw..dy * bw + bw].copy_from_slice(src);
        }
        return;
    }
    // Sub-pel (and full-pel edge): extract the clamped (bw+1)×(bh+1) tile once —
    // the edge-extended input `McChromaWithFragMv_c` reads — then the bilinear
    // `A·p + B·p₊₁ + C·p₊ₛ + D·p₊ₛ₊₁` (`g_kuiABCD` weights), `(·+32)>>6`. A chroma
    // block is at most 8×8 (half a 16×16 MB), so a 9×9 tile suffices. Bit-identical:
    // the clamped tile reproduces `at()`, and full-pel weights give `(64·a+32)>>6==a`.
    let ts = bw + 1;
    let mut t = [0u8; 9 * 9];
    // Interior fast path (see `luma_tile`): clamp-free contiguous row copies.
    if ix0 >= 0
        && iy0 >= 0
        && ix0 + ts as isize <= cw as isize
        && iy0 + (bh + 1) as isize <= ch as isize
    {
        let (rx0, ry0) = (ix0 as usize, iy0 as usize);
        for ty in 0..bh + 1 {
            let src = (ry0 + ty) * cw + rx0;
            t[ty * ts..ty * ts + ts].copy_from_slice(&reference[src..src + ts]);
        }
    } else {
        for ty in 0..bh + 1 {
            let ry = (iy0 + ty as isize).clamp(0, ch as isize - 1) as usize * cw;
            for tx in 0..ts {
                let rx = (ix0 + tx as isize).clamp(0, cw as isize - 1) as usize;
                t[ty * ts + tx] = reference[ry + rx];
            }
        }
    }
    let (wa, wb, wc, wd) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    // 8-wide chroma (full-MB and 16×16-partition chroma — the common case) → the
    // openh264 SSE2 bilinear over the same clamped tile. Width 2/4 stay scalar
    // (width-4 is only an MMX kernel; width 2 has none). Bit-identical.
    #[cfg(feature = "asm")]
    if bw == 8 {
        let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
        rusty_h264_accel::mc_chroma_w8(&t, ts, out, bw, &abcd, bh);
        return;
    }
    for r in 0..bh {
        for c in 0..bw {
            let p = r * ts + c;
            let v = wa * t[p] as i32
                + wb * t[p + 1] as i32
                + wc * t[p + ts] as i32
                + wd * t[p + ts + 1] as i32;
            out[r * bw + c] = ((v + 32) >> 6) as u8;
        }
    }
}

// ---- Padded-reference MC (openh264 ExpandPicture style) ----
//
// A reference plane is stored with a replicated-edge border (`PAD_L` luma /
// `PAD_C` chroma pixels), the picture origin at `(pad, pad)` and `stride` = the
// padded width. MC then reads the frame DIRECTLY at the MV offset — no per-call
// clamped tile — because reads into the border hit valid replicated pixels. For
// the rare MV whose 6-tap halo would exceed the border, we fall back to the
// clamped tile (reading the padded interior), which is bit-identical to the
// exact-frame path.
//
// STATUS: implemented + bit-exact (the `mc_*_padded_matches_exact` tests + a full
// decoder wiring verified 35/35 corpus MATCH), but measured **~0** vs the tile
// path on x86-64 — once `luma_tile`'s interior fast path made extraction a
// vectorized copy, the remaining win (skipping the copy) is offset by the
// padded direct read's worse kernel cache locality (full-frame stride vs the
// L1-resident tile) plus the per-frame expand/copy cost. Kept UNWIRED as a ready
// option for a workload/target where tile extraction dominates (e.g. slower
// memory, or hand-asm kernels tuned for the big-stride read). To wire: store
// `RefFrame` planes padded (`expand_plane` in `as_reference`) and call these
// instead of `mc_luma`/`mc_chroma`.

/// Luma reference border width (covers full-pel MVs to ±30 px before fallback).
pub const PAD_L: usize = 32;
/// Chroma reference border width (= `PAD_L`/2, matching the half-rate chroma MV).
pub const PAD_C: usize = 16;

/// Fills the `pad`-wide replicated-edge border of a plane whose picture (`pw×ph`)
/// sits at offset `(pad, pad)` with `stride`. Mirrors openh264 `ExpandPictureLuma_c`:
/// left/right cols replicate the edge pixel; top/bottom rows replicate the (already
/// edge-filled) first/last picture row, so the corners come out right.
pub fn expand_plane(buf: &mut [u8], stride: usize, pad: usize, pw: usize, ph: usize) {
    for y in 0..ph {
        let row = (y + pad) * stride;
        let (left, right) = (buf[row + pad], buf[row + pad + pw - 1]);
        for x in 0..pad {
            buf[row + x] = left;
            buf[row + pad + pw + x] = right;
        }
    }
    let first = pad * stride;
    let last = (pad + ph - 1) * stride;
    for y in 0..pad {
        buf.copy_within(first..first + stride, y * stride);
        buf.copy_within(last..last + stride, (pad + ph + y) * stride);
    }
}

/// Quarter-pel luma MC reading a padded reference directly (no clamped tile when
/// the halo is in-border). `x0,y0` are the block's picture coords; `stride`/`pad`
/// describe the padded plane; `pw,ph` are the picture dims. Bit-identical to
/// [`mc_luma`] on the equivalent exact frame.
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_padded(
    padded: &[u8],
    stride: usize,
    pad: usize,
    pw: usize,
    ph: usize,
    x0: usize,
    y0: usize,
    bw: usize,
    bh: usize,
    mvx: i32,
    mvy: i32,
    out: &mut [u8],
) {
    let (ix0, iy0) = (x0 as isize + (mvx >> 2) as isize, y0 as isize + (mvy >> 2) as isize);
    let (fx, fy) = (mvx & 3, mvy & 3);
    let p = pad as isize;
    let (lo_x, lo_y) = (ix0 - 2, iy0 - 2);
    let in_range = lo_x >= -p
        && lo_y >= -p
        && lo_x + (bw + 5) as isize <= pw as isize + p
        && lo_y + (bh + 5) as isize <= ph as isize + p;
    if in_range {
        if fx == 0 && fy == 0 {
            for dy in 0..bh {
                let src = ((iy0 + dy as isize + p) as usize) * stride + (ix0 + p) as usize;
                out[dy * bw..dy * bw + bw].copy_from_slice(&padded[src..src + bw]);
            }
        } else {
            let halo = ((lo_y + p) as usize) * stride + (lo_x + p) as usize;
            mc_luma_subpel(&padded[halo..], stride, bw, bh, fx, fy, out);
        }
        return;
    }
    // Extreme MV: clamp the halo to the real picture, read the padded interior.
    let ts = bw + 5;
    let mut t = [0u8; LUMA_TILE * LUMA_TILE];
    for ty in 0..bh + 5 {
        let py = (lo_y + ty as isize).clamp(0, ph as isize - 1) as usize;
        let ry = (py + pad) * stride;
        for tx in 0..ts {
            let px = (lo_x + tx as isize).clamp(0, pw as isize - 1) as usize;
            t[ty * ts + tx] = padded[ry + px + pad];
        }
    }
    if fx == 0 && fy == 0 {
        for dy in 0..bh {
            let s = (dy + 2) * ts + 2;
            out[dy * bw..dy * bw + bw].copy_from_slice(&t[s..s + bw]);
        }
    } else {
        mc_luma_subpel(&t, ts, bw, bh, fx, fy, out);
    }
}

/// Eighth-pel chroma MC reading a padded reference directly. Bit-identical to
/// [`mc_chroma`] on the equivalent exact frame.
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_padded(
    padded: &[u8],
    stride: usize,
    pad: usize,
    pw: usize,
    ph: usize,
    x0: usize,
    y0: usize,
    bw: usize,
    bh: usize,
    mvx: i32,
    mvy: i32,
    out: &mut [u8],
) {
    let (ix0, iy0) = (x0 as isize + (mvx >> 3) as isize, y0 as isize + (mvy >> 3) as isize);
    let (fx, fy) = (mvx & 7, mvy & 7);
    let p = pad as isize;
    let in_range = ix0 >= -p
        && iy0 >= -p
        && ix0 + (bw + 1) as isize <= pw as isize + p
        && iy0 + (bh + 1) as isize <= ph as isize + p;
    let (wa, wb, wc, wd) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    if in_range {
        if fx == 0 && fy == 0 {
            for dy in 0..bh {
                let src = ((iy0 + dy as isize + p) as usize) * stride + (ix0 + p) as usize;
                out[dy * bw..dy * bw + bw].copy_from_slice(&padded[src..src + bw]);
            }
            return;
        }
        let halo = ((iy0 + p) as usize) * stride + (ix0 + p) as usize;
        #[cfg(feature = "asm")]
        if bw == 8 {
            let abcd = [wa as u8, wb as u8, wc as u8, wd as u8];
            rusty_h264_accel::mc_chroma_w8(&padded[halo..], stride, out, bw, &abcd, bh);
            return;
        }
        for r in 0..bh {
            for c in 0..bw {
                let pp = halo + r * stride + c;
                let v = wa * padded[pp] as i32
                    + wb * padded[pp + 1] as i32
                    + wc * padded[pp + stride] as i32
                    + wd * padded[pp + stride + 1] as i32;
                out[r * bw + c] = ((v + 32) >> 6) as u8;
            }
        }
        return;
    }
    // Extreme MV: clamp the (bw+1)² halo to the real picture, read the padded interior.
    let ts = bw + 1;
    let mut t = [0u8; 9 * 9];
    for ty in 0..bh + 1 {
        let py = (iy0 + ty as isize).clamp(0, ph as isize - 1) as usize;
        let ry = (py + pad) * stride;
        for tx in 0..ts {
            let px = (ix0 + tx as isize).clamp(0, pw as isize - 1) as usize;
            t[ty * ts + tx] = padded[ry + px + pad];
        }
    }
    for r in 0..bh {
        for c in 0..bw {
            let pp = r * ts + c;
            let v = wa * t[pp] as i32
                + wb * t[pp + 1] as i32
                + wc * t[pp + ts] as i32
                + wd * t[pp + ts + 1] as i32;
            out[r * bw + c] = ((v + 32) >> 6) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rand_plane(pw: usize, ph: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..pw * ph)
            .map(|_| {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                (s >> 16) as u8
            })
            .collect()
    }

    fn make_padded(exact: &[u8], pw: usize, ph: usize, pad: usize) -> (Vec<u8>, usize) {
        let stride = pw + 2 * pad;
        let mut padded = vec![0u8; stride * (ph + 2 * pad)];
        for y in 0..ph {
            let d = (y + pad) * stride + pad;
            padded[d..d + pw].copy_from_slice(&exact[y * pw..y * pw + pw]);
        }
        expand_plane(&mut padded, stride, pad, pw, ph);
        (padded, stride)
    }

    #[test]
    fn mc_luma_padded_matches_exact() {
        let (pw, ph) = (48usize, 32usize);
        let exact = rand_plane(pw, ph, 0x77);
        let (padded, stride) = make_padded(&exact, pw, ph, PAD_L);
        for &(bw, bh) in &[(16usize, 16usize), (8, 8), (16, 8), (8, 16), (4, 4)] {
            for x0 in [0usize, 8, pw - bw] {
                for y0 in [0usize, 8, ph - bh] {
                    for mvx in [-40i32, -20, -3, 0, 1, 2, 3, 7, 20, 40] {
                        for mvy in [-40i32, -20, -3, 0, 1, 2, 3, 7, 20, 40] {
                            let mut a = vec![0u8; bw * bh];
                            let mut b = vec![0u8; bw * bh];
                            mc_luma(&exact, pw, ph, x0, y0, bw, bh, mvx, mvy, &mut a);
                            mc_luma_padded(
                                &padded, stride, PAD_L, pw, ph, x0, y0, bw, bh, mvx, mvy, &mut b,
                            );
                            assert_eq!(a, b, "luma bw={bw} x0={x0} y0={y0} mv=({mvx},{mvy})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mc_chroma_padded_matches_exact() {
        let (pw, ph) = (24usize, 16usize);
        let exact = rand_plane(pw, ph, 0x99);
        let (padded, stride) = make_padded(&exact, pw, ph, PAD_C);
        for &(bw, bh) in &[(8usize, 8usize), (4, 4), (8, 4), (4, 8), (2, 2)] {
            for x0 in [0usize, 4, pw - bw] {
                for y0 in [0usize, 4, ph - bh] {
                    for mvx in [-40i32, -16, -3, 0, 1, 5, 8, 16, 40] {
                        for mvy in [-40i32, -16, -3, 0, 1, 5, 8, 16, 40] {
                            let mut a = vec![0u8; bw * bh];
                            let mut b = vec![0u8; bw * bh];
                            mc_chroma(&exact, pw, ph, x0, y0, bw, bh, mvx, mvy, &mut a);
                            mc_chroma_padded(
                                &padded, stride, PAD_C, pw, ph, x0, y0, bw, bh, mvx, mvy, &mut b,
                            );
                            assert_eq!(a, b, "chroma bw={bw} x0={x0} y0={y0} mv=({mvx},{mvy})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mc_luma_block_kernels_match_per_pixel() {
        // The block-kernel MC must be bit-identical to the per-pixel `luma_sample`
        // reference for every quarter-pel position, across interior AND edge blocks
        // (negative / off-frame MVs that exercise the clamped tile vs `at()`).
        let (cw, ch) = (40usize, 32usize);
        let mut state = 0x1357_9bdfu32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        let reference: Vec<u8> = (0..cw * ch).map(|_| next()).collect();
        for &(bw, bh) in &[(16, 16), (8, 8), (4, 4), (8, 16), (16, 8)] {
            for &(x0, y0) in &[(8usize, 8usize), (0, 0), (cw - bw, ch - bh)] {
                for mvx in -9..=9 {
                    for mvy in -9..=9 {
                        let mut got = vec![0u8; bw * bh];
                        mc_luma(&reference, cw, ch, x0, y0, bw, bh, mvx, mvy, &mut got);
                        let ix0 = x0 as isize + (mvx >> 2) as isize;
                        let iy0 = y0 as isize + (mvy >> 2) as isize;
                        let (fx, fy) = (mvx & 3, mvy & 3);
                        for dy in 0..bh {
                            for dx in 0..bw {
                                let want = luma_sample(
                                    &reference, cw, ch,
                                    ix0 + dx as isize, iy0 + dy as isize, fx, fy,
                                ) as u8;
                                assert_eq!(
                                    got[dy * bw + dx], want,
                                    "bw{bw}x{bh} at ({x0},{y0}) mv({mvx},{mvy}) px({dx},{dy})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mc_chroma_block_matches_per_pixel() {
        let (cw, ch) = (24usize, 20usize);
        let mut state = 0xabcd_1234u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        let reference: Vec<u8> = (0..cw * ch).map(|_| next()).collect();
        let pp = |ix: isize, iy: isize, fx: i32, fy: i32| -> u8 {
            let a = at(&reference, cw, ch, ix, iy);
            let b = at(&reference, cw, ch, ix + 1, iy);
            let c = at(&reference, cw, ch, ix, iy + 1);
            let d = at(&reference, cw, ch, ix + 1, iy + 1);
            (((8 - fx) * (8 - fy) * a + fx * (8 - fy) * b + (8 - fx) * fy * c + fx * fy * d + 32) >> 6)
                as u8
        };
        for &(bw, bh) in &[(8, 8), (4, 4), (8, 4), (4, 8)] {
            for &(x0, y0) in &[(4usize, 4usize), (0, 0), (cw - bw, ch - bh)] {
                for mvx in -12..=12 {
                    for mvy in -12..=12 {
                        let mut got = vec![0u8; bw * bh];
                        mc_chroma(&reference, cw, ch, x0, y0, bw, bh, mvx, mvy, &mut got);
                        let ix0 = x0 as isize + (mvx >> 3) as isize;
                        let iy0 = y0 as isize + (mvy >> 3) as isize;
                        let (fx, fy) = (mvx & 7, mvy & 7);
                        for dy in 0..bh {
                            for dx in 0..bw {
                                assert_eq!(
                                    got[dy * bw + dx],
                                    pp(ix0 + dx as isize, iy0 + dy as isize, fx, fy),
                                    "bw{bw}x{bh} at ({x0},{y0}) mv({mvx},{mvy}) px({dx},{dy})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn median_of_three() {
        assert_eq!(median3(1, 2, 3), 2);
        assert_eq!(median3(3, 1, 2), 2);
        assert_eq!(median3(-5, 0, 5), 0);
        assert_eq!(median3(7, 7, 2), 7);
    }

    #[test]
    fn mv_predict_single_neighbor_uses_it() {
        let a = MvNeighbor { available: true, mv: (8, -4), ref_idx: 0 };
        // B and C unavailable, A available → predictor is A.
        assert_eq!(predict_mv(a, MvNeighbor::NONE, MvNeighbor::NONE, 0), (8, -4));
    }

    #[test]
    fn mv_predict_median_when_all_inter() {
        let a = MvNeighbor { available: true, mv: (4, 0), ref_idx: 0 };
        let b = MvNeighbor { available: true, mv: (8, 0), ref_idx: 0 };
        let c = MvNeighbor { available: true, mv: (12, 0), ref_idx: 0 };
        assert_eq!(predict_mv(a, b, c, 0), (8, 0));
    }

    #[test]
    fn mv_predict_one_matching_ref_wins() {
        // Only B references ref 0; A and C are intra → predictor is B.
        let a = MvNeighbor { available: true, mv: (0, 0), ref_idx: -1 };
        let b = MvNeighbor { available: true, mv: (5, 7), ref_idx: 0 };
        let c = MvNeighbor { available: true, mv: (0, 0), ref_idx: -1 };
        assert_eq!(predict_mv(a, b, c, 0), (5, 7));
    }

    #[test]
    fn mv_predict_distinguishes_refs() {
        // A references ref 1, B references ref 0, C intra. For cur_ref 0 only B
        // matches → B; for cur_ref 1 only A matches → A.
        let a = MvNeighbor { available: true, mv: (4, 4), ref_idx: 1 };
        let b = MvNeighbor { available: true, mv: (5, 7), ref_idx: 0 };
        let c = MvNeighbor { available: true, mv: (0, 0), ref_idx: -1 };
        assert_eq!(predict_mv(a, b, c, 0), (5, 7));
        assert_eq!(predict_mv(a, b, c, 1), (4, 4));
    }

    #[test]
    fn mc_luma_zero_mv_copies() {
        let reference = vec![
            0, 1, 2, 3, 10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33,
        ];
        let mut out = [0u8; 4];
        mc_luma(&reference, 4, 4, 1, 1, 2, 2, 0, 0, &mut out);
        assert_eq!(out, [11, 12, 21, 22]);
    }

    #[test]
    fn mc_luma_clamps_at_edges() {
        let reference = vec![5, 6, 7, 8];
        let mut out = [0u8; 4];
        mc_luma(&reference, 2, 2, 0, 0, 2, 2, -40, -40, &mut out);
        assert_eq!(out, [5, 5, 5, 5]);
    }

    #[test]
    fn mc_luma_halfpel_on_flat_is_flat() {
        // A flat reference must interpolate to the same flat value at any frac.
        let reference = vec![100u8; 8 * 8];
        let mut out = [0u8; 16];
        for &(fx, fy) in &[(2, 0), (0, 2), (2, 2), (1, 1), (3, 3)] {
            mc_luma(&reference, 8, 8, 2, 2, 4, 4, fx, fy, &mut out);
            assert!(out.iter().all(|&p| p == 100), "frac ({fx},{fy})");
        }
    }

    #[test]
    fn mc_chroma_zero_mv_copies() {
        let reference = vec![0, 1, 2, 3, 10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33];
        let mut out = [0u8; 4];
        mc_chroma(&reference, 4, 4, 1, 1, 2, 2, 0, 0, &mut out);
        assert_eq!(out, [11, 12, 21, 22]);
    }

    #[test]
    fn mc_chroma_bilinear_midpoint() {
        // Horizontal ramp 0,8; chroma frac 4 (half) → midpoint 4.
        let reference = vec![0u8, 8, 0, 8];
        let mut out = [0u8; 1];
        mc_chroma(&reference, 2, 2, 0, 0, 1, 1, 4, 0, &mut out);
        assert_eq!(out[0], 4);
    }
}
