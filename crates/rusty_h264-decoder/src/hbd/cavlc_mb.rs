//! CAVLC slice data and macroblock layer (`ff_h264_decode_mb_cavlc`), frame pictures only.
//! Residual blocks are read with the crate's CAVLC block decoder, which returns levels in
//! scan order; placement and dequantisation follow FFmpeg's `decode_residual`.

use super::mb::Ctx;
use super::tables::*;
use super::{SliceHeader, SliceType};
use crate::DecodeError;
use rusty_h264_common::cavlc::{decode_residual_block, read_cbp_inter, read_cbp_intra};
use rusty_h264_common::BitReader;

fn invalid(what: &'static str) -> DecodeError {
    DecodeError::Unsupported(what)
}

impl Ctx {
    pub fn decode_slice_cavlc(&mut self, rbsp: &[u8], h: &SliceHeader) -> Result<(), DecodeError> {
        let mut r = BitReader::new(rbsp);
        let mut left = h.data_bit;
        while left > 0 {
            let n = left.min(32);
            r.read_bits(n as u32)?;
            left -= n;
        }
        let total = self.mb_w * self.mb_h;
        let mut addr = h.first_mb;
        let mut skip_run: i64 = -1;
        loop {
            self.mb_x = addr % self.mb_w;
            self.mb_y = addr / self.mb_w;
            self.mb_xy = (self.mb_x + self.mb_y * self.mb_stride) as isize;
            if self.slice_type != SliceType::I {
                if skip_run < 0 {
                    skip_run = i64::from(r.read_ue()?);
                    if skip_run > total as i64 {
                        return Err(invalid("invalid mb_skip_run"));
                    }
                }
                if skip_run > 0 {
                    skip_run -= 1;
                    self.decode_mb_skip()?;
                    let t = self.t(self.mb_xy);
                    self.cbp_table[t] = 0;
                    self.cbp = 0;
                    self.hl_decode_mb()?;
                    addr += 1;
                    self.next_mb = addr;
                    if addr >= total || (skip_run == 0 && !r.more_rbsp_data()) {
                        return Ok(());
                    }
                    continue;
                }
                skip_run = -1;
            }
            self.decode_mb_cavlc(&mut r)?;
            self.hl_decode_mb()?;
            addr += 1;
            self.next_mb = addr;
            if addr >= total || !r.more_rbsp_data() {
                return Ok(());
            }
        }
    }

    fn read_ref_cavlc(r: &mut BitReader, rc: usize) -> Result<i8, DecodeError> {
        Ok(match rc {
            1 => 0,
            2 => i8::from(!r.read_bit()?),
            _ => {
                let v = r.read_ue()?;
                if v as usize >= rc {
                    return Err(invalid("reference index out of range"));
                }
                v as i8
            }
        })
    }

    fn read_mvd(r: &mut BitReader) -> Result<i32, DecodeError> {
        Ok(r.read_se()?)
    }

    fn decode_mb_cavlc(&mut self, r: &mut BitReader) -> Result<(), DecodeError> {
        let t = self.t(self.mb_xy);
        let mut dct8x8_allowed = self.transform_8x8_mode;
        self.prev_mb_skipped = false;
        let raw = r.read_ue()? as usize;
        let intra_idx = match self.slice_type {
            SliceType::B if raw < 23 => None,
            SliceType::B => Some(raw - 23),
            SliceType::P if raw < 5 => None,
            SliceType::P => Some(raw - 5),
            SliceType::I => Some(raw),
        };
        let (mut mb_type, partition_count, mut cbp) = match intra_idx {
            Some(i) => {
                let info = I_MB_TYPE_INFO
                    .get(i)
                    .ok_or(invalid("mb_type out of range"))?;
                self.intra16x16_pred_mode = info.1;
                (info.0, 0, info.2)
            }
            None if self.slice_type == SliceType::B => {
                (B_MB_TYPE_INFO[raw].0, B_MB_TYPE_INFO[raw].1, 0)
            }
            None => (P_MB_TYPE_INFO[raw].0, P_MB_TYPE_INFO[raw].1, 0),
        };
        self.slice_table[t] = self.slice_num;

        if is_pcm(mb_type) {
            // pcm_alignment_zero_bits, then the samples.
            r.align_to_byte()?;
            let bd = self.bit_depth;
            for s in self.pcm.iter_mut() {
                *s = r.read_bits(bd)? as u16;
            }
            self.qscale_table[t] = 0;
            self.non_zero_count[t] = [16; 48];
            self.mb_type[t] = mb_type;
            self.cbp_table[t] = 0;
            self.cbp = 0;
            return Ok(());
        }

        self.fill_decode_neighbors();
        self.fill_decode_caches(mb_type);

        if is_intra(mb_type) {
            if is_intra4x4(mb_type) {
                let mut di = 1;
                if dct8x8_allowed && r.read_bit()? {
                    mb_type |= MB_TYPE_8X8DCT;
                    di = 4;
                }
                for i in (0..16).step_by(di) {
                    let mut mode = self.pred_intra_mode(i);
                    if !r.read_bit()? {
                        let rem = r.read_bits(3)? as i8;
                        mode = rem + i8::from(rem >= mode);
                    }
                    let s = SCAN8[i];
                    if di == 4 {
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.intra4x4_pred_mode_cache[k] = mode;
                        }
                    } else {
                        self.intra4x4_pred_mode_cache[s] = mode;
                    }
                }
                self.write_back_intra_pred_mode();
                self.check_intra4x4_pred_mode()?;
            } else {
                self.intra16x16_pred_mode =
                    self.check_intra_pred_mode(self.intra16x16_pred_mode, false)?;
            }
            let pred = r.read_ue()?;
            if pred > 3 {
                return Err(invalid("invalid intra_chroma_pred_mode"));
            }
            self.chroma_pred_mode = self.check_intra_pred_mode(pred as i32, true)?;
        } else if partition_count == 4 {
            let mut sub_partition_count = [0u32; 4];
            if self.slice_type == SliceType::B {
                for i in 0..4 {
                    let st = r.read_ue()? as usize;
                    let info = B_SUB_MB_TYPE_INFO
                        .get(st)
                        .ok_or(invalid("B sub_mb_type out of range"))?;
                    (self.sub_mb_type[i], sub_partition_count[i]) = *info;
                }
                if self.sub_mb_type.iter().any(|&s| is_direct(s)) {
                    self.pred_direct_motion(&mut mb_type)?;
                    for list in 0..2 {
                        self.ref_cache[list][SCAN8[4]] = PART_NOT_AVAILABLE;
                        self.ref_cache[list][SCAN8[12]] = PART_NOT_AVAILABLE;
                    }
                }
            } else {
                for i in 0..4 {
                    let st = r.read_ue()? as usize;
                    let info = P_SUB_MB_TYPE_INFO
                        .get(st)
                        .ok_or(invalid("P sub_mb_type out of range"))?;
                    (self.sub_mb_type[i], sub_partition_count[i]) = *info;
                }
            }
            let mut refs = [[LIST_NOT_USED; 4]; 2];
            for list in 0..self.list_count {
                let rc = if mb_type & MB_TYPE_REF0 != 0 {
                    1
                } else {
                    self.ref_count[list]
                };
                for i in 0..4 {
                    if is_direct(self.sub_mb_type[i]) {
                        continue;
                    }
                    if is_dir(self.sub_mb_type[i], 0, list) {
                        refs[list][i] = Self::read_ref_cavlc(r, rc)?;
                    }
                }
            }
            if dct8x8_allowed {
                dct8x8_allowed = self.get_dct8x8_allowed();
            }
            for list in 0..self.list_count {
                for i in 0..4 {
                    let s = SCAN8[4 * i];
                    if is_direct(self.sub_mb_type[i]) {
                        self.ref_cache[list][s] = self.ref_cache[list][s + 1];
                        continue;
                    }
                    for k in [s, s + 1, s + 8, s + 9] {
                        self.ref_cache[list][k] = refs[list][i];
                    }
                    let sub = self.sub_mb_type[i];
                    if is_dir(sub, 0, list) {
                        let block_width = if sub & (MB_TYPE_16X16 | MB_TYPE_16X8) != 0 {
                            2
                        } else {
                            1
                        };
                        for j in 0..sub_partition_count[i] as usize {
                            let index = 4 * i + block_width * j;
                            let c = SCAN8[index];
                            let (mx, my) =
                                self.pred_motion(index, block_width, list, self.ref_cache[list][c]);
                            let mx = mx.wrapping_add(Self::read_mvd(r)?);
                            let my = my.wrapping_add(Self::read_mvd(r)?);
                            let mv = [mx as i16, my as i16];
                            let offsets: &[usize] = if sub & MB_TYPE_16X16 != 0 {
                                &[0, 1, 8, 9]
                            } else if sub & MB_TYPE_16X8 != 0 {
                                &[0, 1]
                            } else if sub & MB_TYPE_8X16 != 0 {
                                &[0, 8]
                            } else {
                                &[0]
                            };
                            for &o in offsets {
                                self.mv_cache[list][c + o] = mv;
                            }
                        }
                    } else {
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.mv_cache[list][k] = [0; 2];
                        }
                    }
                }
            }
        } else if is_direct(mb_type) {
            self.pred_direct_motion(&mut mb_type)?;
            dct8x8_allowed &= self.direct_8x8_inference;
        } else {
            self.decode_inter_partitions_cavlc(r, mb_type)?;
        }

        if is_inter(mb_type) {
            self.write_back_motion(mb_type);
        }

        if !is_intra16x16(mb_type) {
            cbp = if is_intra4x4(mb_type) {
                read_cbp_intra(r)?
            } else {
                read_cbp_inter(r)?
            };
        }
        if dct8x8_allowed && cbp & 15 != 0 && !is_intra(mb_type) && r.read_bit()? {
            mb_type |= MB_TYPE_8X8DCT;
        }
        self.cbp = cbp;
        self.cbp_table[t] = cbp as u16;
        self.mb_type[t] = mb_type;

        if cbp != 0 || is_intra16x16(mb_type) {
            let dquant = r.read_se()?;
            let mut q = self.qscale.wrapping_add(dquant);
            if !(0..=self.max_qp).contains(&q) {
                if q < 0 {
                    q += self.max_qp + 1;
                } else {
                    q -= self.max_qp + 1;
                }
                if !(0..=self.max_qp).contains(&q) {
                    return Err(invalid("dquant out of range"));
                }
            }
            self.set_qscale(q);
            self.mb.fill(0);
            let new_cbp = self.decode_luma_residual_cavlc(r, mb_type, cbp)?;
            self.cbp_table[t] |= (new_cbp << 12) as u16;
            if cbp & 0x30 != 0 {
                for c in 0..2 {
                    let n = CHROMA_DC_BLOCK_INDEX + c;
                    let (levels, total) = decode_residual_block(r, 4, -1)?;
                    self.non_zero_count_cache[SCAN8[n]] = total;
                    self.chroma_dc[c] = [levels[0], levels[1], levels[2], levels[3]];
                }
            }
            if cbp & 0x20 != 0 {
                for c in 0..2 {
                    let list = c + 1 + if is_intra(mb_type) { 0 } else { 3 };
                    let q = self.chroma_qp[c] as usize;
                    for i in 0..4 {
                        let n = 16 + 16 * c + i;
                        let nc = self.pred_non_zero_count(n);
                        let (levels, total) = decode_residual_block(r, 15, nc)?;
                        self.non_zero_count_cache[SCAN8[n]] = total;
                        for (k, &l) in levels[..15].iter().enumerate() {
                            if l != 0 {
                                let pos = ZIGZAG4[k + 1];
                                self.mb[16 * n + pos] =
                                    l.wrapping_mul(self.dequant4[list][q][pos]).wrapping_add(32)
                                        >> 6;
                            }
                        }
                    }
                }
            } else {
                for s in [SCAN8[16], SCAN8[32]] {
                    for y in 0..4 {
                        self.non_zero_count_cache[s + 8 * y..s + 8 * y + 4].fill(0);
                    }
                }
            }
        } else {
            for s in [SCAN8[0], SCAN8[16], SCAN8[32]] {
                for y in 0..4 {
                    self.non_zero_count_cache[s + 8 * y..s + 8 * y + 4].fill(0);
                }
            }
        }
        self.qscale_table[t] = self.qscale as u8;
        self.write_back_non_zero_count();
        Ok(())
    }

    fn decode_inter_partitions_cavlc(
        &mut self,
        r: &mut BitReader,
        mb_type: u32,
    ) -> Result<(), DecodeError> {
        let s0 = SCAN8[0];
        if mb_type & MB_TYPE_16X16 != 0 {
            for list in 0..self.list_count {
                if is_dir(mb_type, 0, list) {
                    let v = Self::read_ref_cavlc(r, self.ref_count[list])?;
                    for y in 0..4 {
                        self.ref_cache[list][s0 + 8 * y..s0 + 8 * y + 4].fill(v);
                    }
                }
            }
            for list in 0..self.list_count {
                if is_dir(mb_type, 0, list) {
                    let (mx, my) = self.pred_motion(0, 4, list, self.ref_cache[list][s0]);
                    let mx = mx.wrapping_add(Self::read_mvd(r)?);
                    let my = my.wrapping_add(Self::read_mvd(r)?);
                    for y in 0..4 {
                        self.mv_cache[list][s0 + 8 * y..s0 + 8 * y + 4]
                            .fill([mx as i16, my as i16]);
                    }
                }
            }
        } else if mb_type & MB_TYPE_16X8 != 0 {
            for list in 0..self.list_count {
                for i in 0..2 {
                    let v = if is_dir(mb_type, i as u32, list) {
                        Self::read_ref_cavlc(r, self.ref_count[list])?
                    } else {
                        LIST_NOT_USED
                    };
                    for y in 0..2 {
                        let s = s0 + 16 * i + 8 * y;
                        self.ref_cache[list][s..s + 4].fill(v);
                    }
                }
            }
            for list in 0..self.list_count {
                for i in 0..2 {
                    let mv = if is_dir(mb_type, i as u32, list) {
                        let (mx, my) =
                            self.pred_16x8_motion(8 * i, list, self.ref_cache[list][s0 + 16 * i]);
                        let mx = mx.wrapping_add(Self::read_mvd(r)?);
                        let my = my.wrapping_add(Self::read_mvd(r)?);
                        [mx as i16, my as i16]
                    } else {
                        [0; 2]
                    };
                    for y in 0..2 {
                        let s = s0 + 16 * i + 8 * y;
                        self.mv_cache[list][s..s + 4].fill(mv);
                    }
                }
            }
        } else {
            for list in 0..self.list_count {
                for i in 0..2 {
                    let v = if is_dir(mb_type, i as u32, list) {
                        Self::read_ref_cavlc(r, self.ref_count[list])?
                    } else {
                        LIST_NOT_USED
                    };
                    for y in 0..4 {
                        let s = s0 + 2 * i + 8 * y;
                        self.ref_cache[list][s..s + 2].fill(v);
                    }
                }
            }
            for list in 0..self.list_count {
                for i in 0..2 {
                    let mv = if is_dir(mb_type, i as u32, list) {
                        let (mx, my) =
                            self.pred_8x16_motion(4 * i, list, self.ref_cache[list][s0 + 2 * i]);
                        let mx = mx.wrapping_add(Self::read_mvd(r)?);
                        let my = my.wrapping_add(Self::read_mvd(r)?);
                        [mx as i16, my as i16]
                    } else {
                        [0; 2]
                    };
                    for y in 0..4 {
                        let s = s0 + 2 * i + 8 * y;
                        self.mv_cache[list][s..s + 2].fill(mv);
                    }
                }
            }
        }
        Ok(())
    }

    /// `pred_non_zero_count`: nC from the left and top blocks' counts (64: unavailable).
    fn pred_non_zero_count(&self, n: usize) -> i32 {
        let s = SCAN8[n];
        let mut i = i32::from(self.non_zero_count_cache[s - 1])
            + i32::from(self.non_zero_count_cache[s - 8]);
        if i < 64 {
            i = (i + 1) >> 1;
        }
        i & 31
    }

    /// Luma residual (`decode_luma_residual`); returns the 8x8 blocks with coefficients.
    fn decode_luma_residual_cavlc(
        &mut self,
        r: &mut BitReader,
        mb_type: u32,
        cbp: u32,
    ) -> Result<u32, DecodeError> {
        let q = self.qscale as usize;
        if is_intra16x16(mb_type) {
            self.mb_luma_dc = [0; 16];
            let nc = self.pred_non_zero_count(0);
            let (levels, total) = decode_residual_block(r, 16, nc)?;
            self.non_zero_count_cache[SCAN8[LUMA_DC_BLOCK_INDEX]] = total;
            for (k, &l) in levels.iter().enumerate() {
                self.mb_luma_dc[ZIGZAG4[k]] = l;
            }
            if cbp & 15 != 0 {
                for i8x8 in 0..4 {
                    for i4x4 in 0..4 {
                        let n = 4 * i8x8 + i4x4;
                        let nc = self.pred_non_zero_count(n);
                        let (levels, total) = decode_residual_block(r, 15, nc)?;
                        self.non_zero_count_cache[SCAN8[n]] = total;
                        for (k, &l) in levels[..15].iter().enumerate() {
                            if l != 0 {
                                let pos = ZIGZAG4[k + 1];
                                self.mb[16 * n + pos] =
                                    l.wrapping_mul(self.dequant4[0][q][pos]).wrapping_add(32) >> 6;
                            }
                        }
                    }
                }
                return Ok(0xf);
            }
            for y in 0..4 {
                self.non_zero_count_cache[SCAN8[0] + 8 * y..SCAN8[0] + 8 * y + 4].fill(0);
            }
            return Ok(0);
        }
        let intra = is_intra(mb_type);
        let mut new_cbp = 0;
        for i8x8 in 0..4 {
            if cbp & (1 << i8x8) == 0 {
                let s = SCAN8[4 * i8x8];
                for k in [s, s + 1, s + 8, s + 9] {
                    self.non_zero_count_cache[k] = 0;
                }
                continue;
            }
            if is_8x8dct(mb_type) {
                let list = if intra { 0 } else { 1 };
                for i4x4 in 0..4 {
                    let n = 4 * i8x8 + i4x4;
                    let nc = self.pred_non_zero_count(n);
                    let (levels, total) = decode_residual_block(r, 16, nc)?;
                    self.non_zero_count_cache[SCAN8[n]] = total;
                    for (k, &l) in levels.iter().enumerate() {
                        if l != 0 {
                            let pos = ZIGZAG8[4 * k + i4x4];
                            self.mb[64 * i8x8 + pos] =
                                l.wrapping_mul(self.dequant8[list][q][pos]).wrapping_add(32) >> 6;
                        }
                    }
                }
                let s = SCAN8[4 * i8x8];
                let c = &mut self.non_zero_count_cache;
                c[s] = c[s]
                    .wrapping_add(c[s + 1])
                    .wrapping_add(c[s + 8])
                    .wrapping_add(c[s + 9]);
                new_cbp |= u32::from(c[s] != 0) << i8x8;
            } else {
                let list = if intra { 0 } else { 3 };
                for i4x4 in 0..4 {
                    let n = 4 * i8x8 + i4x4;
                    let nc = self.pred_non_zero_count(n);
                    let (levels, total) = decode_residual_block(r, 16, nc)?;
                    self.non_zero_count_cache[SCAN8[n]] = total;
                    for (k, &l) in levels.iter().enumerate() {
                        if l != 0 {
                            let pos = ZIGZAG4[k];
                            self.mb[16 * n + pos] =
                                l.wrapping_mul(self.dequant4[list][q][pos]).wrapping_add(32) >> 6;
                        }
                    }
                    new_cbp |= u32::from(total) << i8x8;
                }
            }
        }
        Ok(new_cbp)
    }
}
