//! SPS/PPS parsing (the subset our encoder emits).

use rusty_h264_common::{BitReader, bit_reader::OutOfData};

/// Parsed sequence parameter set fields the decoder needs.
#[derive(Debug, Clone)]
pub struct Sps {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    pub log2_max_frame_num: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_pic_order_cnt_lsb: u32,
    pub max_num_ref_frames: u32,
    pub pic_width_in_mbs: usize,
    pub pic_height_in_mbs: usize,
    pub frame_crop_left: u32,
    pub frame_crop_right: u32,
    pub frame_crop_top: u32,
    pub frame_crop_bottom: u32,
}

impl Sps {
    /// Coded luma width/height in samples (MB grid * 16).
    pub fn coded_width(&self) -> usize {
        self.pic_width_in_mbs * 16
    }
    pub fn coded_height(&self) -> usize {
        self.pic_height_in_mbs * 16
    }

    /// Displayed luma width after cropping (CropUnitX = 2 for 4:2:0).
    pub fn display_width(&self) -> usize {
        self.coded_width() - 2 * (self.frame_crop_left + self.frame_crop_right) as usize
    }
    /// Displayed luma height after cropping (CropUnitY = 2 for 4:2:0, frame-only).
    pub fn display_height(&self) -> usize {
        self.coded_height() - 2 * (self.frame_crop_top + self.frame_crop_bottom) as usize
    }

    /// Parses an SPS RBSP (emulation bytes already removed).
    pub fn parse(rbsp: &[u8]) -> Result<Self, OutOfData> {
        let mut r = BitReader::new(rbsp);
        let profile_idc = r.read_bits(8)? as u8;
        let _constraints = r.read_bits(8)?;
        let level_idc = r.read_bits(8)? as u8;
        let seq_parameter_set_id = r.read_ue()?;
        // CBP/Baseline: no chroma_format_idc / scaling-list section.
        let log2_max_frame_num = r.read_ue()? + 4;
        let pic_order_cnt_type = r.read_ue()?;
        let mut log2_max_pic_order_cnt_lsb = 0;
        if pic_order_cnt_type == 0 {
            log2_max_pic_order_cnt_lsb = r.read_ue()? + 4;
        }
        let max_num_ref_frames = r.read_ue()?;
        let _gaps = r.read_bit()?;
        let pic_width_in_mbs = (r.read_ue()? + 1) as usize;
        let pic_height_in_mbs = (r.read_ue()? + 1) as usize;
        let frame_mbs_only_flag = r.read_bit()?;
        debug_assert!(frame_mbs_only_flag, "interlace unsupported");
        let _direct_8x8 = r.read_bit()?;
        let cropping = r.read_bit()?;
        let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
        if cropping {
            cl = r.read_ue()?;
            cr = r.read_ue()?;
            ct = r.read_ue()?;
            cb = r.read_ue()?;
        }
        // vui_parameters_present_flag and trailing bits ignored.
        Ok(Self {
            profile_idc,
            level_idc,
            seq_parameter_set_id,
            log2_max_frame_num,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb,
            max_num_ref_frames,
            pic_width_in_mbs,
            pic_height_in_mbs,
            frame_crop_left: cl,
            frame_crop_right: cr,
            frame_crop_top: ct,
            frame_crop_bottom: cb,
        })
    }
}

/// Parsed picture parameter set fields the decoder needs.
#[derive(Debug, Clone)]
pub struct Pps {
    pub pic_parameter_set_id: u32,
    pub seq_parameter_set_id: u32,
    pub entropy_coding_mode_flag: bool,
    pub pic_init_qp: i32,
    pub deblocking_filter_control_present_flag: bool,
}

impl Pps {
    /// Parses a PPS RBSP.
    pub fn parse(rbsp: &[u8]) -> Result<Self, OutOfData> {
        let mut r = BitReader::new(rbsp);
        let pic_parameter_set_id = r.read_ue()?;
        let seq_parameter_set_id = r.read_ue()?;
        let entropy_coding_mode_flag = r.read_bit()?;
        let _bottom_field = r.read_bit()?;
        let num_slice_groups_minus1 = r.read_ue()?;
        debug_assert_eq!(num_slice_groups_minus1, 0, "slice groups unsupported");
        let _num_ref_idx_l0 = r.read_ue()?;
        let _num_ref_idx_l1 = r.read_ue()?;
        let _weighted_pred = r.read_bit()?;
        let _weighted_bipred_idc = r.read_bits(2)?;
        let pic_init_qp = 26 + r.read_se()?;
        let _pic_init_qs = r.read_se()?;
        let _chroma_qp_index_offset = r.read_se()?;
        let deblocking_filter_control_present_flag = r.read_bit()?;
        Ok(Self {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            pic_init_qp,
            deblocking_filter_control_present_flag,
        })
    }
}
