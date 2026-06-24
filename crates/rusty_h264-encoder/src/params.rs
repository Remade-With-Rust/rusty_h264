//! Sequence and picture parameter sets (SPS / PPS) generation.
//!
//! Follows the H.264 spec syntax (§7.3.2.1.1 / §7.3.2.2) restricted to the
//! Constrained Baseline feature set: `frame_mbs_only_flag = 1`, no scaling
//! matrices, CAVLC entropy coding, no chroma/luma bit-depth extensions.

use crate::config::EncoderConfig;
use rusty_h264_common::{BitWriter, NalUnit, NalUnitType};

/// Sequence parameter set, carrying only the fields a CBP encoder emits.
#[derive(Debug, Clone)]
pub struct Sps {
    pub profile_idc: u8,
    pub constraint_set1_flag: bool,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    pub log2_max_frame_num_minus4: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub max_num_ref_frames: u32,
    pub pic_width_in_mbs_minus1: u32,
    pub pic_height_in_map_units_minus1: u32,
    /// Cropping in chroma-sample units (right, bottom) when the coded MB grid
    /// overshoots the requested luma resolution.
    pub frame_crop_right: u32,
    pub frame_crop_bottom: u32,
}

impl Sps {
    /// Derives the SPS from an encoder configuration.
    pub fn from_config(cfg: &EncoderConfig) -> Self {
        let mb_w = cfg.mb_width();
        let mb_h = cfg.mb_height();
        // Crop offsets are expressed in units of CropUnitX/Y. For 4:2:0 and
        // frame_mbs_only_flag=1, CropUnitX=2, CropUnitY=2.
        let crop_right = (mb_w * 16 - cfg.width) / 2;
        let crop_bottom = (mb_h * 16 - cfg.height) / 2;
        Self {
            profile_idc: cfg.profile.profile_idc(),
            constraint_set1_flag: true, // constrained baseline
            level_idc: cfg.level_idc,
            seq_parameter_set_id: 0,
            log2_max_frame_num_minus4: 0, // log2_max_frame_num = 4
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0, // log2_max_poc_lsb = 4
            max_num_ref_frames: cfg.num_ref_frames.max(1),
            pic_width_in_mbs_minus1: (mb_w - 1) as u32,
            pic_height_in_map_units_minus1: (mb_h - 1) as u32,
            frame_crop_right: crop_right as u32,
            frame_crop_bottom: crop_bottom as u32,
        }
    }

    /// Writes the SPS RBSP (without NAL header) including trailing bits.
    pub fn write_rbsp(&self, w: &mut BitWriter) {
        w.write_bits(self.profile_idc as u32, 8);
        // constraint_set0..5 flags + 2 reserved zero bits = u(8).
        let mut constraints = 0u32;
        if self.constraint_set1_flag {
            constraints |= 1 << 6; // constraint_set1_flag is bit position 6 (MSB-first)
        }
        w.write_bits(constraints, 8);
        w.write_bits(self.level_idc as u32, 8);
        w.write_ue(self.seq_parameter_set_id);
        // CBP (profile_idc 66) => no chroma_format_idc / scaling-list block.
        w.write_ue(self.log2_max_frame_num_minus4);
        w.write_ue(self.pic_order_cnt_type);
        if self.pic_order_cnt_type == 0 {
            w.write_ue(self.log2_max_pic_order_cnt_lsb_minus4);
        }
        w.write_ue(self.max_num_ref_frames);
        w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
        w.write_ue(self.pic_width_in_mbs_minus1);
        w.write_ue(self.pic_height_in_map_units_minus1);
        w.write_bit(true); // frame_mbs_only_flag = 1
        w.write_bit(false); // direct_8x8_inference_flag
        let cropping = self.frame_crop_right != 0 || self.frame_crop_bottom != 0;
        w.write_bit(cropping); // frame_cropping_flag
        if cropping {
            w.write_ue(0); // frame_crop_left_offset
            w.write_ue(self.frame_crop_right);
            w.write_ue(0); // frame_crop_top_offset
            w.write_ue(self.frame_crop_bottom);
        }
        w.write_bit(false); // vui_parameters_present_flag
        w.rbsp_trailing_bits();
    }

    /// Builds the SPS as a complete NAL unit.
    pub fn to_nal(&self) -> NalUnit {
        let mut w = BitWriter::new();
        self.write_rbsp(&mut w);
        NalUnit::new(3, NalUnitType::Sps, w.into_bytes())
    }
}

/// Picture parameter set for a CAVLC, single-slice-group CBP encoder.
#[derive(Debug, Clone)]
pub struct Pps {
    pub pic_parameter_set_id: u32,
    pub seq_parameter_set_id: u32,
    pub num_ref_idx_l0_default_active_minus1: u32,
    pub pic_init_qp_minus26: i32,
    pub deblocking_filter_control_present_flag: bool,
}

impl Pps {
    /// Derives the PPS from an encoder configuration.
    pub fn from_config(cfg: &EncoderConfig) -> Self {
        Self {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            num_ref_idx_l0_default_active_minus1: cfg.num_ref_frames.max(1) - 1,
            pic_init_qp_minus26: cfg.qp as i32 - 26,
            // We signal deblocking control in the slice so we can disable the
            // in-loop filter (not yet implemented); this keeps our (non-filtered)
            // reconstruction bit-identical to a reference decoder's.
            deblocking_filter_control_present_flag: true,
        }
    }

    /// Writes the PPS RBSP (without NAL header) including trailing bits.
    pub fn write_rbsp(&self, w: &mut BitWriter) {
        w.write_ue(self.pic_parameter_set_id);
        w.write_ue(self.seq_parameter_set_id);
        w.write_bit(false); // entropy_coding_mode_flag = 0 (CAVLC)
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(self.num_ref_idx_l0_default_active_minus1);
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(false); // weighted_pred_flag
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(self.pic_init_qp_minus26);
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(0); // chroma_qp_index_offset
        w.write_bit(self.deblocking_filter_control_present_flag);
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        w.rbsp_trailing_bits();
    }

    /// Builds the PPS as a complete NAL unit.
    pub fn to_nal(&self) -> NalUnit {
        let mut w = BitWriter::new();
        self.write_rbsp(&mut w);
        NalUnit::new(3, NalUnitType::Pps, w.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_h264_common::{nal::emulation_unprevent, BitReader};

    #[test]
    fn sps_roundtrips_through_reader() {
        let cfg = EncoderConfig::new(1920, 1080); // 1080 not a multiple of 16 -> cropping
        let sps = Sps::from_config(&cfg);
        let nal = sps.to_nal();

        let rbsp = emulation_unprevent(&nal.rbsp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.read_bits(8).unwrap(), 66); // profile_idc
        let constraints = r.read_bits(8).unwrap();
        assert_eq!((constraints >> 6) & 1, 1); // constraint_set1_flag
        assert_eq!(r.read_bits(8).unwrap(), 30); // level_idc
        assert_eq!(r.read_ue().unwrap(), 0); // sps id
        assert_eq!(r.read_ue().unwrap(), 0); // log2_max_frame_num_minus4
        assert_eq!(r.read_ue().unwrap(), 0); // poc type
        assert_eq!(r.read_ue().unwrap(), 0); // log2_max_poc_lsb_minus4
        assert_eq!(r.read_ue().unwrap(), 1); // max_num_ref_frames
        assert!(!r.read_bit().unwrap()); // gaps
        assert_eq!(r.read_ue().unwrap(), 119); // 1920/16 - 1
        assert_eq!(r.read_ue().unwrap(), 67); // ceil(1080/16)-1 = 68-1
        assert!(r.read_bit().unwrap()); // frame_mbs_only
        assert!(!r.read_bit().unwrap()); // direct_8x8
        assert!(r.read_bit().unwrap()); // cropping present (1080)
        assert_eq!(r.read_ue().unwrap(), 0); // crop left
        assert_eq!(r.read_ue().unwrap(), 0); // crop right
        assert_eq!(r.read_ue().unwrap(), 0); // crop top
        assert_eq!(r.read_ue().unwrap(), 4); // crop bottom: (1088-1080)/2
    }

    #[test]
    fn pps_roundtrips_through_reader() {
        let cfg = EncoderConfig::new(640, 480);
        let pps = Pps::from_config(&cfg);
        let nal = pps.to_nal();

        let rbsp = emulation_unprevent(&nal.rbsp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.read_ue().unwrap(), 0); // pps id
        assert_eq!(r.read_ue().unwrap(), 0); // sps id
        assert!(!r.read_bit().unwrap()); // entropy_coding_mode (CAVLC)
        assert!(!r.read_bit().unwrap()); // bottom_field
        assert_eq!(r.read_ue().unwrap(), 0); // num_slice_groups_minus1
        assert_eq!(r.read_ue().unwrap(), 0); // num_ref_idx_l0
        assert_eq!(r.read_ue().unwrap(), 0); // num_ref_idx_l1
        assert!(!r.read_bit().unwrap()); // weighted_pred
        assert_eq!(r.read_bits(2).unwrap(), 0); // weighted_bipred_idc
        assert_eq!(r.read_se().unwrap(), 0); // pic_init_qp_minus26 (qp 26)
    }
}
