//! CABAC slice data and macroblock layer (`ff_h264_decode_mb_cabac` and its helpers), frame
//! pictures only.

use super::mb::Ctx;
use super::tables::*;
use super::{SliceHeader, SliceType};
use crate::cabac::Cabac;
use crate::DecodeError;

fn invalid(what: &'static str) -> DecodeError {
    DecodeError::Unsupported(what)
}

impl Ctx {
    pub fn decode_slice_cabac(&mut self, rbsp: &[u8], h: &SliceHeader) -> Result<(), DecodeError> {
        let start = h.data_bit.div_ceil(8);
        if start > rbsp.len() {
            return Err(DecodeError::Truncated);
        }
        let mut cab = Cabac::new(
            rbsp,
            start,
            h.qp,
            h.cabac_init_idc,
            h.slice_type == SliceType::I,
        );
        let mut addr = h.first_mb;
        let total = self.mb_w * self.mb_h;
        loop {
            self.mb_x = addr % self.mb_w;
            self.mb_y = addr / self.mb_w;
            self.mb_xy = (self.mb_x + self.mb_y * self.mb_stride) as isize;
            self.decode_mb_cabac(&mut cab, rbsp)?;
            self.hl_decode_mb()?;
            let eos = cab.decode_terminate();
            addr += 1;
            self.next_mb = addr;
            if eos || addr >= total {
                return Ok(());
            }
        }
    }

    fn decode_mb_cabac(&mut self, cab: &mut Cabac, rbsp: &[u8]) -> Result<(), DecodeError> {
        let t = self.t(self.mb_xy);
        let mut dct8x8_allowed = self.transform_8x8_mode;
        let mut partition_count: u32;
        let mut mb_type: u32;
        let mut cbp: u32 = 0;

        if self.slice_type != SliceType::I && self.decode_cabac_mb_skip(cab) {
            self.decode_mb_skip()?;
            self.cbp = 0;
            self.cbp_table[t] = 0;
            self.chroma_pred_mode_table[t] = 0;
            self.last_qscale_diff = 0;
            return Ok(());
        }
        self.prev_mb_skipped = false;
        self.fill_decode_neighbors();

        let mut intra_type: Option<usize> = None;
        if self.slice_type == SliceType::B {
            let mut ctx = 0;
            if !is_direct(self.left_type.wrapping_sub(1)) {
                ctx += 1;
            }
            if !is_direct(self.top_type.wrapping_sub(1)) {
                ctx += 1;
            }
            let idx: usize;
            if cab.decode_decision(27 + ctx) == 0 {
                idx = 0;
            } else if cab.decode_decision(27 + 3) == 0 {
                idx = 1 + cab.decode_decision(27 + 5) as usize;
            } else {
                let mut bits = (cab.decode_decision(27 + 4) as usize) << 3;
                bits += (cab.decode_decision(27 + 5) as usize) << 2;
                bits += (cab.decode_decision(27 + 5) as usize) << 1;
                bits += cab.decode_decision(27 + 5) as usize;
                if bits < 8 {
                    idx = bits + 3;
                } else if bits == 13 {
                    intra_type = Some(self.decode_cabac_intra_mb_type(cab, 32, false));
                    idx = 0;
                } else if bits == 14 {
                    idx = 11;
                } else if bits == 15 {
                    idx = 22;
                } else {
                    bits = (bits << 1) + cab.decode_decision(27 + 5) as usize;
                    idx = bits - 4;
                }
            }
            (mb_type, partition_count) = B_MB_TYPE_INFO[idx];
        } else if self.slice_type == SliceType::P {
            if cab.decode_decision(14) == 0 {
                let idx = if cab.decode_decision(15) == 0 {
                    3 * cab.decode_decision(16) as usize
                } else {
                    2 - cab.decode_decision(17) as usize
                };
                (mb_type, partition_count) = P_MB_TYPE_INFO[idx];
            } else {
                intra_type = Some(self.decode_cabac_intra_mb_type(cab, 17, false));
                (mb_type, partition_count) = (0, 0);
            }
        } else {
            intra_type = Some(self.decode_cabac_intra_mb_type(cab, 3, true));
            (mb_type, partition_count) = (0, 0);
        }
        if let Some(it) = intra_type {
            let info = I_MB_TYPE_INFO[it];
            partition_count = 0;
            cbp = info.2;
            self.intra16x16_pred_mode = info.1;
            mb_type = info.0;
        }

        self.slice_table[t] = self.slice_num;

        if is_pcm(mb_type) {
            // The samples follow the arithmetic codeword, byte-aligned.
            let mut p = cab.pcm_start_byte();
            let bd = self.bit_depth as usize;
            let bits_needed = 384 * bd;
            if (rbsp.len().saturating_sub(p)) * 8 < bits_needed {
                return Err(DecodeError::Truncated);
            }
            let mut br = rusty_h264_common::BitReader::new(&rbsp[p..]);
            for s in self.pcm.iter_mut() {
                *s = br.read_bits(bd as u32)? as u16;
            }
            p += bits_needed.div_ceil(8);
            cab.reinit_at(p);
            self.cbp_table[t] = 0xf7ef;
            self.chroma_pred_mode_table[t] = 0;
            self.qscale_table[t] = 0;
            self.non_zero_count[t] = [16; 48];
            self.mb_type[t] = mb_type;
            self.last_qscale_diff = 0;
            self.cbp = 0;
            return Ok(());
        }

        self.fill_decode_caches(mb_type);

        if is_intra(mb_type) {
            if is_intra4x4(mb_type) {
                if dct8x8_allowed && cab.decode_decision(399 + self.neighbor_transform_size) != 0 {
                    mb_type |= MB_TYPE_8X8DCT;
                    for i in (0..16).step_by(4) {
                        let pred = self.pred_intra_mode(i);
                        let mode = self.decode_cabac_mb_intra4x4_pred_mode(cab, pred);
                        let s = SCAN8[i];
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.intra4x4_pred_mode_cache[k] = mode;
                        }
                    }
                } else {
                    for i in 0..16 {
                        let pred = self.pred_intra_mode(i);
                        self.intra4x4_pred_mode_cache[SCAN8[i]] =
                            self.decode_cabac_mb_intra4x4_pred_mode(cab, pred);
                    }
                }
                self.write_back_intra_pred_mode();
                self.check_intra4x4_pred_mode()?;
            } else {
                self.intra16x16_pred_mode =
                    self.check_intra_pred_mode(self.intra16x16_pred_mode, false)?;
            }
            let pred_mode = self.decode_cabac_mb_chroma_pre_mode(cab);
            self.chroma_pred_mode_table[t] = pred_mode as u8;
            self.chroma_pred_mode = self.check_intra_pred_mode(pred_mode, true)?;
        } else if partition_count == 4 {
            let mut sub_partition_count = [0u32; 4];
            if self.slice_type == SliceType::B {
                for i in 0..4 {
                    let st = self.decode_cabac_b_mb_sub_type(cab);
                    (self.sub_mb_type[i], sub_partition_count[i]) = B_SUB_MB_TYPE_INFO[st];
                }
                if self.sub_mb_type.iter().any(|&s| is_direct(s)) {
                    self.pred_direct_motion(&mut mb_type)?;
                    for list in 0..2 {
                        self.ref_cache[list][SCAN8[4]] = PART_NOT_AVAILABLE;
                        self.ref_cache[list][SCAN8[12]] = PART_NOT_AVAILABLE;
                    }
                    for i in 0..4 {
                        let s = SCAN8[4 * i];
                        let v = ((self.sub_mb_type[i] >> 1) & 0xFF) as u8;
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.direct_cache[k] = v;
                        }
                    }
                }
            } else {
                for i in 0..4 {
                    let st = self.decode_cabac_p_mb_sub_type(cab);
                    (self.sub_mb_type[i], sub_partition_count[i]) = P_SUB_MB_TYPE_INFO[st];
                }
            }
            for list in 0..self.list_count {
                for i in 0..4 {
                    if is_direct(self.sub_mb_type[i]) {
                        continue;
                    }
                    let r: i8 = if is_dir(self.sub_mb_type[i], 0, list) {
                        let rc = self.ref_count[list];
                        if rc > 1 {
                            let r = self.decode_cabac_mb_ref(cab, list, 4 * i)?;
                            if r as usize >= rc {
                                return Err(invalid("reference index out of range"));
                            }
                            r
                        } else {
                            0
                        }
                    } else {
                        LIST_NOT_USED
                    };
                    let s = SCAN8[4 * i];
                    self.ref_cache[list][s + 1] = r;
                    self.ref_cache[list][s + 8] = r;
                    self.ref_cache[list][s + 9] = r;
                }
            }
            if dct8x8_allowed {
                dct8x8_allowed = self.get_dct8x8_allowed();
            }
            for list in 0..self.list_count {
                for i in 0..4 {
                    let s = SCAN8[4 * i];
                    self.ref_cache[list][s] = self.ref_cache[list][s + 1];
                    if is_direct(self.sub_mb_type[i]) {
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.mvd_cache[list][k] = [0; 2];
                        }
                        continue;
                    }
                    if is_dir(self.sub_mb_type[i], 0, list) {
                        let sub = self.sub_mb_type[i];
                        let block_width = if sub & (MB_TYPE_16X16 | MB_TYPE_16X8) != 0 {
                            2
                        } else {
                            1
                        };
                        for j in 0..sub_partition_count[i] as usize {
                            let index = 4 * i + block_width * j;
                            let c = SCAN8[index];
                            let (mx0, my0) =
                                self.pred_motion(index, block_width, list, self.ref_cache[list][c]);
                            let (mxd, myd, mpx, mpy) = self.decode_mvd_pair(cab, list, index)?;
                            let mv = [(mx0 + mxd) as i16, (my0 + myd) as i16];
                            let mvd = [mpx, mpy];
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
                                self.mvd_cache[list][c + o] = mvd;
                            }
                        }
                    } else {
                        for k in [s, s + 1, s + 8, s + 9] {
                            self.mv_cache[list][k] = [0; 2];
                            self.mvd_cache[list][k] = [0; 2];
                        }
                    }
                }
            }
        } else if is_direct(mb_type) {
            self.pred_direct_motion(&mut mb_type)?;
            for list in 0..2 {
                for y in 0..4 {
                    let s = SCAN8[0] + 8 * y;
                    self.mvd_cache[list][s..s + 4].fill([0; 2]);
                }
            }
            dct8x8_allowed &= self.direct_8x8_inference;
        } else {
            self.decode_inter_partitions(cab, mb_type)?;
        }

        if is_inter(mb_type) {
            self.chroma_pred_mode_table[t] = 0;
            self.write_back_motion(mb_type);
        }

        if !is_intra16x16(mb_type) {
            cbp = self.decode_cabac_mb_cbp_luma(cab);
            cbp |= self.decode_cabac_mb_cbp_chroma(cab) << 4;
        }
        self.cbp_table[t] = cbp as u16;
        self.cbp = cbp;

        if dct8x8_allowed
            && cbp & 15 != 0
            && !is_intra(mb_type)
            && cab.decode_decision(399 + self.neighbor_transform_size) != 0
        {
            mb_type |= MB_TYPE_8X8DCT;
        }
        self.mb_type[t] = mb_type;

        if cbp != 0 || is_intra16x16(mb_type) {
            // mb_qp_delta
            if cab.decode_decision(60 + usize::from(self.last_qscale_diff != 0)) != 0 {
                let mut val = 1i32;
                let mut ctx = 2;
                while cab.decode_decision(60 + ctx) != 0 {
                    ctx = 3;
                    val += 1;
                    if val > 2 * self.max_qp {
                        return Err(invalid("mb_qp_delta out of range"));
                    }
                }
                let val = if val & 1 != 0 {
                    (val + 1) >> 1
                } else {
                    -((val + 1) >> 1)
                };
                self.last_qscale_diff = val;
                let mut q = self.qscale + val;
                if q < 0 {
                    q += self.max_qp + 1;
                } else if q > self.max_qp {
                    q -= self.max_qp + 1;
                }
                if !(0..=self.max_qp).contains(&q) {
                    return Err(invalid("mb_qp_delta out of range"));
                }
                self.set_qscale(q);
            } else {
                self.last_qscale_diff = 0;
            }

            self.mb.fill(0);
            self.decode_cabac_luma_residual(cab, mb_type, cbp);
            if cbp & 0x30 != 0 {
                for c in 0..2 {
                    self.chroma_dc[c] = [0; 4];
                    self.decode_cabac_residual_dc(cab, 3, CHROMA_DC_BLOCK_INDEX + c, 4);
                }
            }
            if cbp & 0x20 != 0 {
                for c in 0..2 {
                    let list = c + 1 + if is_intra(mb_type) { 0 } else { 3 };
                    let q = self.chroma_qp[c] as usize;
                    for i in 0..4 {
                        let index = 16 + 16 * c + i;
                        self.decode_cabac_residual_nondc(cab, 4, index, 15, Qmul::Four(list, q));
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
            self.last_qscale_diff = 0;
        }
        self.qscale_table[t] = self.qscale as u8;
        self.write_back_non_zero_count();
        Ok(())
    }

    fn decode_inter_partitions(
        &mut self,
        cab: &mut Cabac,
        mb_type: u32,
    ) -> Result<(), DecodeError> {
        let s0 = SCAN8[0];
        if mb_type & MB_TYPE_16X16 != 0 {
            for list in 0..self.list_count {
                if is_dir(mb_type, 0, list) {
                    let r = self.read_ref(cab, list, 0)?;
                    for y in 0..4 {
                        self.ref_cache[list][s0 + 8 * y..s0 + 8 * y + 4].fill(r);
                    }
                }
            }
            for list in 0..self.list_count {
                if is_dir(mb_type, 0, list) {
                    let (mx, my) = self.pred_motion(0, 4, list, self.ref_cache[list][s0]);
                    let (mxd, myd, mpx, mpy) = self.decode_mvd_pair(cab, list, 0)?;
                    let mv = [(mx + mxd) as i16, (my + myd) as i16];
                    for y in 0..4 {
                        self.mv_cache[list][s0 + 8 * y..s0 + 8 * y + 4].fill(mv);
                        self.mvd_cache[list][s0 + 8 * y..s0 + 8 * y + 4].fill([mpx, mpy]);
                    }
                }
            }
        } else if mb_type & MB_TYPE_16X8 != 0 {
            for list in 0..self.list_count {
                for i in 0..2 {
                    let r = if is_dir(mb_type, i as u32, list) {
                        self.read_ref(cab, list, 8 * i)?
                    } else {
                        LIST_NOT_USED
                    };
                    for y in 0..2 {
                        let s = s0 + 16 * i + 8 * y;
                        self.ref_cache[list][s..s + 4].fill(r);
                    }
                }
            }
            for list in 0..self.list_count {
                for i in 0..2 {
                    let (mv, mvd) = if is_dir(mb_type, i as u32, list) {
                        let (mx, my) =
                            self.pred_16x8_motion(8 * i, list, self.ref_cache[list][s0 + 16 * i]);
                        let (mxd, myd, mpx, mpy) = self.decode_mvd_pair(cab, list, 8 * i)?;
                        ([(mx + mxd) as i16, (my + myd) as i16], [mpx, mpy])
                    } else {
                        ([0; 2], [0; 2])
                    };
                    for y in 0..2 {
                        let s = s0 + 16 * i + 8 * y;
                        self.mv_cache[list][s..s + 4].fill(mv);
                        self.mvd_cache[list][s..s + 4].fill(mvd);
                    }
                }
            }
        } else {
            for list in 0..self.list_count {
                for i in 0..2 {
                    let r = if is_dir(mb_type, i as u32, list) {
                        self.read_ref(cab, list, 4 * i)?
                    } else {
                        LIST_NOT_USED
                    };
                    for y in 0..4 {
                        let s = s0 + 2 * i + 8 * y;
                        self.ref_cache[list][s..s + 2].fill(r);
                    }
                }
            }
            for list in 0..self.list_count {
                for i in 0..2 {
                    let (mv, mvd) = if is_dir(mb_type, i as u32, list) {
                        let (mx, my) =
                            self.pred_8x16_motion(4 * i, list, self.ref_cache[list][s0 + 2 * i]);
                        let (mxd, myd, mpx, mpy) = self.decode_mvd_pair(cab, list, 4 * i)?;
                        ([(mx + mxd) as i16, (my + myd) as i16], [mpx, mpy])
                    } else {
                        ([0; 2], [0; 2])
                    };
                    for y in 0..4 {
                        let s = s0 + 2 * i + 8 * y;
                        self.mv_cache[list][s..s + 2].fill(mv);
                        self.mvd_cache[list][s..s + 2].fill(mvd);
                    }
                }
            }
        }
        Ok(())
    }

    fn read_ref(&mut self, cab: &mut Cabac, list: usize, n: usize) -> Result<i8, DecodeError> {
        let rc = self.ref_count[list];
        if rc > 1 {
            let r = self.decode_cabac_mb_ref(cab, list, n)?;
            if r as usize >= rc {
                return Err(invalid("reference index out of range"));
            }
            Ok(r)
        } else {
            Ok(0)
        }
    }

    fn decode_cabac_intra_mb_type(
        &mut self,
        cab: &mut Cabac,
        ctx_base: usize,
        intra_slice: bool,
    ) -> usize {
        let mut state = ctx_base;
        if intra_slice {
            let mut ctx = 0;
            if self.left_type & (MB_TYPE_INTRA16X16 | MB_TYPE_INTRA_PCM) != 0 {
                ctx += 1;
            }
            if self.top_type & (MB_TYPE_INTRA16X16 | MB_TYPE_INTRA_PCM) != 0 {
                ctx += 1;
            }
            if cab.decode_decision(state + ctx) == 0 {
                return 0;
            }
            state += 2;
        } else if cab.decode_decision(state) == 0 {
            return 0;
        }
        if cab.decode_terminate() {
            return 25;
        }
        let is = usize::from(intra_slice);
        let mut mb_type = 1;
        mb_type += 12 * cab.decode_decision(state + 1) as usize;
        if cab.decode_decision(state + 2) != 0 {
            mb_type += 4 + 4 * cab.decode_decision(state + 2 + is) as usize;
        }
        mb_type += 2 * cab.decode_decision(state + 3 + is) as usize;
        mb_type += cab.decode_decision(state + 3 + 2 * is) as usize;
        mb_type
    }

    fn decode_cabac_mb_skip(&mut self, cab: &mut Cabac) -> bool {
        let mba = self.mb_xy - 1;
        let mbb = self.mb_xy - self.mb_stride as isize;
        let mut ctx = 0;
        if self.slice_table[self.t(mba)] == self.slice_num && !is_skip(self.mb_type[self.t(mba)]) {
            ctx += 1;
        }
        if self.slice_table[self.t(mbb)] == self.slice_num && !is_skip(self.mb_type[self.t(mbb)]) {
            ctx += 1;
        }
        if self.slice_type == SliceType::B {
            ctx += 13;
        }
        cab.decode_decision(11 + ctx) != 0
    }

    fn decode_cabac_mb_intra4x4_pred_mode(&mut self, cab: &mut Cabac, pred_mode: i8) -> i8 {
        if cab.decode_decision(68) != 0 {
            return pred_mode;
        }
        let mut mode = cab.decode_decision(69) as i8;
        mode += 2 * cab.decode_decision(69) as i8;
        mode += 4 * cab.decode_decision(69) as i8;
        mode + i8::from(mode >= pred_mode)
    }

    fn decode_cabac_mb_chroma_pre_mode(&mut self, cab: &mut Cabac) -> i32 {
        let mut ctx = 0;
        if self.left_type != 0 && self.chroma_pred_mode_table[self.t(self.left_mb_xy)] != 0 {
            ctx += 1;
        }
        if self.top_type != 0 && self.chroma_pred_mode_table[self.t(self.top_mb_xy)] != 0 {
            ctx += 1;
        }
        if cab.decode_decision(64 + ctx) == 0 {
            return 0;
        }
        if cab.decode_decision(64 + 3) == 0 {
            return 1;
        }
        if cab.decode_decision(64 + 3) == 0 {
            2
        } else {
            3
        }
    }

    fn decode_cabac_mb_cbp_luma(&mut self, cab: &mut Cabac) -> u32 {
        let cbp_a = self.left_cbp;
        let cbp_b = self.top_cbp;
        let nz = |v: u32| usize::from(v == 0);
        let mut cbp = 0u32;
        let ctx = nz(cbp_a & 0x02) + 2 * nz(cbp_b & 0x04);
        cbp += cab.decode_decision(73 + ctx);
        let ctx = nz(cbp & 0x01) + 2 * nz(cbp_b & 0x08);
        cbp += cab.decode_decision(73 + ctx) << 1;
        let ctx = nz(cbp_a & 0x08) + 2 * nz(cbp & 0x01);
        cbp += cab.decode_decision(73 + ctx) << 2;
        let ctx = nz(cbp & 0x04) + 2 * nz(cbp & 0x02);
        cbp += cab.decode_decision(73 + ctx) << 3;
        cbp
    }

    fn decode_cabac_mb_cbp_chroma(&mut self, cab: &mut Cabac) -> u32 {
        let cbp_a = (self.left_cbp >> 4) & 3;
        let cbp_b = (self.top_cbp >> 4) & 3;
        let mut ctx = 0;
        if cbp_a > 0 {
            ctx += 1;
        }
        if cbp_b > 0 {
            ctx += 2;
        }
        if cab.decode_decision(77 + ctx) == 0 {
            return 0;
        }
        let mut ctx = 4;
        if cbp_a == 2 {
            ctx += 1;
        }
        if cbp_b == 2 {
            ctx += 2;
        }
        1 + cab.decode_decision(77 + ctx)
    }

    fn decode_cabac_p_mb_sub_type(&mut self, cab: &mut Cabac) -> usize {
        if cab.decode_decision(21) != 0 {
            return 0;
        }
        if cab.decode_decision(22) == 0 {
            return 1;
        }
        if cab.decode_decision(23) != 0 {
            return 2;
        }
        3
    }

    fn decode_cabac_b_mb_sub_type(&mut self, cab: &mut Cabac) -> usize {
        if cab.decode_decision(36) == 0 {
            return 0;
        }
        if cab.decode_decision(37) == 0 {
            return 1 + cab.decode_decision(39) as usize;
        }
        let mut t = 3;
        if cab.decode_decision(38) != 0 {
            if cab.decode_decision(39) != 0 {
                return 11 + cab.decode_decision(39) as usize;
            }
            t += 4;
        }
        t += 2 * cab.decode_decision(39) as usize;
        t += cab.decode_decision(39) as usize;
        t
    }

    fn decode_cabac_mb_ref(
        &mut self,
        cab: &mut Cabac,
        list: usize,
        n: usize,
    ) -> Result<i8, DecodeError> {
        let s = SCAN8[n];
        let refa = self.ref_cache[list][s - 1];
        let refb = self.ref_cache[list][s - 8];
        let mut ctx = 0;
        if self.slice_type == SliceType::B {
            if refa > 0 && self.direct_cache[s - 1] & (MB_TYPE_DIRECT2 >> 1) as u8 == 0 {
                ctx += 1;
            }
            if refb > 0 && self.direct_cache[s - 8] & (MB_TYPE_DIRECT2 >> 1) as u8 == 0 {
                ctx += 2;
            }
        } else {
            if refa > 0 {
                ctx += 1;
            }
            if refb > 0 {
                ctx += 2;
            }
        }
        let mut r = 0i8;
        while cab.decode_decision(54 + ctx) != 0 {
            r += 1;
            ctx = (ctx >> 2) + 4;
            if r >= 32 {
                return Err(invalid("reference index out of range"));
            }
        }
        Ok(r)
    }

    /// One `mvd` component; returns (value, clipped magnitude for later contexts).
    fn decode_cabac_mb_mvd(
        &mut self,
        cab: &mut Cabac,
        ctxbase: usize,
        amvd: i32,
    ) -> Result<(i32, u8), DecodeError> {
        let inc = usize::from(amvd > 2) + usize::from(amvd > 32);
        if cab.decode_decision(ctxbase + inc) == 0 {
            return Ok((0, 0));
        }
        let mut mvd = 1i32;
        let mut ctx = ctxbase + 3;
        while mvd < 9 && cab.decode_decision(ctx) != 0 {
            if mvd < 4 {
                ctx += 1;
            }
            mvd += 1;
        }
        let mvda = if mvd >= 9 {
            let mut k = 3;
            while cab.decode_bypass() != 0 {
                mvd += 1 << k;
                k += 1;
                if k > 24 {
                    return Err(invalid("mvd overflow"));
                }
            }
            while k > 0 {
                k -= 1;
                mvd += (cab.decode_bypass() as i32) << k;
            }
            mvd.min(70) as u8
        } else {
            mvd as u8
        };
        let v = if cab.decode_bypass() != 0 { -mvd } else { mvd };
        Ok((v, mvda))
    }

    fn decode_mvd_pair(
        &mut self,
        cab: &mut Cabac,
        list: usize,
        n: usize,
    ) -> Result<(i32, i32, u8, u8), DecodeError> {
        let s = SCAN8[n];
        let a = self.mvd_cache[list][s - 1];
        let b = self.mvd_cache[list][s - 8];
        let amvd0 = i32::from(a[0]) + i32::from(b[0]);
        let amvd1 = i32::from(a[1]) + i32::from(b[1]);
        let (mx, mpx) = self.decode_cabac_mb_mvd(cab, 40, amvd0)?;
        let (my, mpy) = self.decode_cabac_mb_mvd(cab, 47, amvd1)?;
        Ok((mx, my, mpx, mpy))
    }

    // --- Residual -------------------------------------------------------------------------

    fn cbf_ctx(&self, cat: usize, idx: usize, is_dc: bool) -> usize {
        let (nza, nzb) = if is_dc {
            if cat == 3 {
                let i = idx - CHROMA_DC_BLOCK_INDEX;
                (
                    (self.left_cbp >> (6 + i)) & 1,
                    (self.top_cbp >> (6 + i)) & 1,
                )
            } else {
                let i = idx - LUMA_DC_BLOCK_INDEX;
                (self.left_cbp & (0x100 << i), self.top_cbp & (0x100 << i))
            }
        } else {
            (
                u32::from(self.non_zero_count_cache[SCAN8[idx] - 1]),
                u32::from(self.non_zero_count_cache[SCAN8[idx] - 8]),
            )
        };
        CBF_BASE[cat] + usize::from(nza > 0) + 2 * usize::from(nzb > 0)
    }

    fn decode_cabac_luma_residual(&mut self, cab: &mut Cabac, mb_type: u32, cbp: u32) {
        let q = self.qscale as usize;
        if is_intra16x16(mb_type) {
            self.mb_luma_dc = [0; 16];
            self.decode_cabac_residual_dc(cab, 0, LUMA_DC_BLOCK_INDEX, 16);
            if cbp & 15 != 0 {
                for i4x4 in 0..16 {
                    self.decode_cabac_residual_nondc(cab, 1, i4x4, 15, Qmul::Four(0, q));
                }
            } else {
                for y in 0..4 {
                    self.non_zero_count_cache[SCAN8[0] + 8 * y..SCAN8[0] + 8 * y + 4].fill(0);
                }
            }
        } else {
            let intra = is_intra(mb_type);
            for i8x8 in 0..4 {
                if cbp & (1 << i8x8) != 0 {
                    if is_8x8dct(mb_type) {
                        let list = if intra { 0 } else { 1 };
                        self.decode_cabac_residual_nondc(
                            cab,
                            5,
                            4 * i8x8,
                            64,
                            Qmul::Eight(list, q),
                        );
                    } else {
                        let list = if intra { 0 } else { 3 };
                        for i4x4 in 0..4 {
                            self.decode_cabac_residual_nondc(
                                cab,
                                2,
                                4 * i8x8 + i4x4,
                                16,
                                Qmul::Four(list, q),
                            );
                        }
                    }
                } else {
                    let s = SCAN8[4 * i8x8];
                    for k in [s, s + 1, s + 8, s + 9] {
                        self.non_zero_count_cache[k] = 0;
                    }
                }
            }
        }
    }

    fn decode_cabac_residual_dc(
        &mut self,
        cab: &mut Cabac,
        cat: usize,
        n: usize,
        max_coeff: usize,
    ) {
        if cab.decode_decision(self.cbf_ctx(cat, n, true)) == 0 {
            self.non_zero_count_cache[SCAN8[n]] = 0;
            return;
        }
        self.decode_cabac_residual_internal(cab, cat, n, max_coeff, true, Qmul::None);
    }

    fn decode_cabac_residual_nondc(
        &mut self,
        cab: &mut Cabac,
        cat: usize,
        n: usize,
        max_coeff: usize,
        qmul: Qmul,
    ) {
        // coded_block_flag of 8x8 blocks is only coded for 4:4:4.
        if cat != 5 && cab.decode_decision(self.cbf_ctx(cat, n, false)) == 0 {
            self.non_zero_count_cache[SCAN8[n]] = 0;
            return;
        }
        self.decode_cabac_residual_internal(cab, cat, n, max_coeff, false, qmul);
    }

    fn decode_cabac_residual_internal(
        &mut self,
        cab: &mut Cabac,
        cat: usize,
        n: usize,
        max_coeff: usize,
        is_dc: bool,
        qmul: Qmul,
    ) {
        let sig_base = SIG_BASE[cat];
        let last_base = LAST_BASE[cat];
        let abs_base = ABS_BASE[cat];
        let mut index = [0usize; 64];
        let mut coeff_count = 0usize;

        if !is_dc && max_coeff == 64 {
            let mut last = 0;
            let mut ended = false;
            while last < 63 {
                if cab.decode_decision(sig_base + usize::from(SIG_8X8[last])) != 0 {
                    index[coeff_count] = last;
                    coeff_count += 1;
                    if cab.decode_decision(last_base + usize::from(LAST_8X8[last])) != 0 {
                        ended = true;
                        break;
                    }
                }
                last += 1;
            }
            if !ended {
                index[coeff_count] = 63;
                coeff_count += 1;
            }
        } else {
            let coefs = max_coeff - 1;
            let mut last = 0;
            let mut ended = false;
            while last < coefs {
                if cab.decode_decision(sig_base + last) != 0 {
                    index[coeff_count] = last;
                    coeff_count += 1;
                    if cab.decode_decision(last_base + last) != 0 {
                        ended = true;
                        break;
                    }
                }
                last += 1;
            }
            if !ended {
                index[coeff_count] = coefs;
                coeff_count += 1;
            }
        }

        if is_dc {
            let t = self.t(self.mb_xy);
            if cat == 3 {
                self.cbp_table[t] |= 0x40 << (n - CHROMA_DC_BLOCK_INDEX);
            } else {
                self.cbp_table[t] |= 0x100 << (n - LUMA_DC_BLOCK_INDEX);
            }
            self.non_zero_count_cache[SCAN8[n]] = coeff_count as u8;
        } else if max_coeff == 64 {
            let s = SCAN8[n];
            for k in [s, s + 1, s + 8, s + 9] {
                self.non_zero_count_cache[k] = coeff_count as u8;
            }
        } else {
            self.non_zero_count_cache[SCAN8[n]] = coeff_count as u8;
        }

        // Coefficient positions: the scan skips the DC for AC blocks.
        let scan_pos = |i: usize| -> usize {
            match cat {
                0 | 2 => ZIGZAG4[i],
                1 | 4 => ZIGZAG4[i + 1],
                3 => i,
                _ => ZIGZAG8[i],
            }
        };
        let dq = |c: &Ctx, pos: usize| -> i32 {
            match qmul {
                Qmul::None => 1,
                Qmul::Four(list, q) => c.dequant4[list][q][pos],
                Qmul::Eight(list, q) => c.dequant8[list][q][pos],
            }
        };
        let mut node_ctx = 0usize;
        while coeff_count > 0 {
            coeff_count -= 1;
            let j = scan_pos(index[coeff_count]);
            let level: i32 = if cab.decode_decision(abs_base + ABS_LEVEL1_CTX[node_ctx]) == 0 {
                node_ctx = ABS_LEVEL_TRANSITION[0][node_ctx];
                1
            } else {
                let mut coeff_abs: i32 = 2;
                let ctx = abs_base + ABS_LEVELGT1_CTX[node_ctx];
                node_ctx = ABS_LEVEL_TRANSITION[1][node_ctx];
                while coeff_abs < 15 && cab.decode_decision(ctx) != 0 {
                    coeff_abs += 1;
                }
                if coeff_abs >= 15 {
                    let mut k = 0;
                    loop {
                        if cab.decode_bypass() == 0 || k >= 16 + 7 {
                            break;
                        }
                        k += 1;
                    }
                    coeff_abs = 1;
                    while k > 0 {
                        k -= 1;
                        coeff_abs = coeff_abs
                            .wrapping_add(coeff_abs)
                            .wrapping_add(cab.decode_bypass() as i32);
                    }
                    coeff_abs = coeff_abs.wrapping_add(14);
                }
                coeff_abs
            };
            let level = if cab.decode_bypass() != 0 {
                -level
            } else {
                level
            };
            let value = if is_dc {
                level
            } else {
                (level.wrapping_mul(dq(self, j)).wrapping_add(32)) >> 6
            };
            match cat {
                0 => self.mb_luma_dc[j] = value,
                3 => self.chroma_dc[n - CHROMA_DC_BLOCK_INDEX][j] = value,
                5 => self.mb[16 * n + j] = value,
                _ => self.mb[16 * n + j] = value,
            }
        }
    }

    /// `ff_h264_check_intra4x4_pred_mode`: remap modes whose neighbours are missing.
    pub fn check_intra4x4_pred_mode(&mut self) -> Result<(), DecodeError> {
        const TOP: [i8; 12] = [-1, 0, LEFT_DC_PRED, -1, -1, -1, -1, -1, 0, 0, 0, 0];
        const LEFT: [i8; 12] = [0, -1, TOP_DC_PRED, 0, -1, -1, -1, 0, -1, DC_128_PRED, 0, 0];
        let s0 = SCAN8[0];
        if self.top_samples_available & 0x8000 == 0 {
            for i in 0..4 {
                let m = self.intra4x4_pred_mode_cache[s0 + i];
                let status = *TOP
                    .get(m as usize)
                    .ok_or(invalid("invalid intra 4x4 mode"))?;
                if status < 0 {
                    return Err(invalid("top block unavailable for intra mode"));
                } else if status != 0 {
                    self.intra4x4_pred_mode_cache[s0 + i] = status;
                }
            }
        }
        if self.left_samples_available & 0x8888 != 0x8888 {
            const MASK: [u32; 4] = [0x8000, 0x2000, 0x80, 0x20];
            for i in 0..4 {
                if self.left_samples_available & MASK[i] == 0 {
                    let m = self.intra4x4_pred_mode_cache[s0 + 8 * i];
                    let status = *LEFT
                        .get(m as usize)
                        .ok_or(invalid("invalid intra 4x4 mode"))?;
                    if status < 0 {
                        return Err(invalid("left block unavailable for intra mode"));
                    } else if status != 0 {
                        self.intra4x4_pred_mode_cache[s0 + 8 * i] = status;
                    }
                }
            }
        }
        Ok(())
    }

    /// `ff_h264_check_intra_pred_mode` for 16x16 luma and chroma.
    pub fn check_intra_pred_mode(&self, mode: i32, is_chroma: bool) -> Result<i32, DecodeError> {
        const TOP: [i32; 4] = [LEFT_DC_PRED8X8, 1, -1, -1];
        const LEFT: [i32; 5] = [TOP_DC_PRED8X8, -1, 2, -1, DC_128_PRED8X8];
        let _ = is_chroma;
        if !(0..=3).contains(&mode) {
            return Err(invalid("out of range intra pred mode"));
        }
        let mut mode = mode;
        if self.top_samples_available & 0x8000 == 0 {
            mode = TOP[mode as usize];
            if mode < 0 {
                return Err(invalid("top block unavailable for intra mode"));
            }
        }
        if self.left_samples_available & 0x8080 != 0x8080 {
            mode = LEFT[mode as usize];
            if mode < 0 {
                return Err(invalid("left block unavailable for intra mode"));
            }
        }
        Ok(mode)
    }
}

/// Which dequantisation factors a residual block takes: none (DC, dequantised later), a 4x4
/// list, or an 8x8 list, at a QP.
#[derive(Clone, Copy)]
pub(crate) enum Qmul {
    None,
    Four(usize, usize),
    Eight(usize, usize),
}
