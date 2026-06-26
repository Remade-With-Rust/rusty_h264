//! SPS/PPS parsing for Constrained Baseline.
//!
//! Parses any conformant Baseline SPS/PPS and **rejects** (never misparses or
//! panics on) profiles and tools outside Constrained Baseline.

use crate::DecodeError;
use rusty_h264_common::BitReader;

/// Profiles that carry the High-profile SPS prefix (`chroma_format_idc`,
/// bit-depths, scaling matrices). Decoding their SPS with the Baseline layout
/// would shift every later field — so we reject them up front rather than
/// misparse. (Spec Table A-1 / §7.3.2.1.1.)
const HIGH_PROFILE_IDCS: &[u8] = &[
    100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135,
];

/// Upper bound on the coded frame size, in macroblocks. Above this we reject the
/// SPS rather than attempt a multi-gigabyte allocation from a hostile header.
/// (≈ 4× H.264 Level 5.2's MaxFS of 36 864 MBs — generous but finite.)
const MAX_FRAME_MBS: u64 = 36_864 * 4;

/// Parsed sequence parameter set fields the decoder needs.
#[derive(Debug, Clone)]
pub struct Sps {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    pub log2_max_frame_num: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_pic_order_cnt_lsb: u32,
    /// `delta_pic_order_always_zero_flag` (only meaningful for POC type 1).
    pub delta_pic_order_always_zero: bool,
    /// `gaps_in_frame_num_value_allowed_flag`: when set, `frame_num` may skip
    /// values and the decoder must synthesize placeholder reference frames.
    pub gaps_in_frame_num_allowed: bool,
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

    /// Parses an SPS RBSP (emulation bytes already removed). Rejects anything
    /// outside Constrained Baseline cleanly; never panics.
    pub fn parse(rbsp: &[u8]) -> Result<Self, DecodeError> {
        let mut r = BitReader::new(rbsp);
        let profile_idc = r.read_bits(8)? as u8;
        let _constraints = r.read_bits(8)?;
        let level_idc = r.read_bits(8)? as u8;
        // High/Main-prefix profiles add chroma_format_idc, bit-depths, and
        // scaling matrices here; the Baseline layout below would misread them.
        if HIGH_PROFILE_IDCS.contains(&profile_idc) {
            return Err(DecodeError::Unsupported("non-Baseline profile (High/4:2:2/etc.)"));
        }
        let seq_parameter_set_id = r.read_ue()?;
        // CBP/Baseline: no chroma_format_idc / scaling-list section.
        let log2_max_frame_num = r.read_ue()? + 4;
        let pic_order_cnt_type = r.read_ue()?;
        let mut log2_max_pic_order_cnt_lsb = 0;
        let mut delta_pic_order_always_zero = false;
        if pic_order_cnt_type == 0 {
            log2_max_pic_order_cnt_lsb = r.read_ue()? + 4;
        } else if pic_order_cnt_type == 1 {
            // Parse the type-1 cycle so later fields stay aligned; CBP output
            // order is decode order, so only the always-zero flag is retained
            // (the slice header needs it to know whether delta_pic_order_cnt is
            // present).
            delta_pic_order_always_zero = r.read_bit()?;
            let _offset_for_non_ref_pic = r.read_se()?;
            let _offset_for_top_to_bottom = r.read_se()?;
            let n = r.read_ue()?;
            if n > 255 {
                return Err(DecodeError::Unsupported("oversized poc cycle"));
            }
            for _ in 0..n {
                let _offset = r.read_se()?;
            }
        } else if pic_order_cnt_type != 2 {
            return Err(DecodeError::Unsupported("invalid pic_order_cnt_type"));
        }
        let max_num_ref_frames = r.read_ue()?;
        let gaps_in_frame_num_allowed = r.read_bit()?;
        let pic_width_in_mbs = (r.read_ue()? as u64 + 1) as usize;
        let pic_height_in_mbs = (r.read_ue()? as u64 + 1) as usize;
        // Guard against a hostile SPS demanding a giant allocation.
        if (pic_width_in_mbs as u64) * (pic_height_in_mbs as u64) > MAX_FRAME_MBS {
            return Err(DecodeError::Unsupported("frame too large"));
        }
        let frame_mbs_only_flag = r.read_bit()?;
        if !frame_mbs_only_flag {
            return Err(DecodeError::Unsupported("interlace / field coding"));
        }
        let _direct_8x8 = r.read_bit()?;
        let cropping = r.read_bit()?;
        let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
        if cropping {
            cl = r.read_ue()?;
            cr = r.read_ue()?;
            ct = r.read_ue()?;
            cb = r.read_ue()?;
            // Reject crop windows that exceed the coded frame (would underflow
            // the display-size subtraction in into_frame / display_*).
            if (cl + cr) as usize * 2 >= pic_width_in_mbs * 16
                || (ct + cb) as usize * 2 >= pic_height_in_mbs * 16
            {
                return Err(DecodeError::Unsupported("crop exceeds frame"));
            }
        }
        // vui_parameters_present_flag and trailing bits ignored.
        Ok(Self {
            profile_idc,
            level_idc,
            seq_parameter_set_id,
            log2_max_frame_num,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb,
            delta_pic_order_always_zero,
            gaps_in_frame_num_allowed,
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
    /// `bottom_field_pic_order_in_frame_present_flag` (a.k.a. pic_order_present):
    /// when set, slice headers carry an extra `delta_pic_order_cnt` value.
    pub bottom_field_pic_order_present: bool,
    pub num_ref_idx_l0_default: u32,
    pub num_ref_idx_l1_default: u32,
    pub weighted_pred: bool,
    pub weighted_bipred_idc: u8,
    pub pic_init_qp: i32,
    /// Signed offset applied when mapping luma QP to chroma QP (§8.5.8).
    pub chroma_qp_index_offset: i32,
    pub deblocking_filter_control_present_flag: bool,
    pub constrained_intra_pred_flag: bool,
    pub redundant_pic_cnt_present_flag: bool,
}

impl Pps {
    /// Parses a PPS RBSP. Rejects FMO/slice-groups cleanly; never panics.
    pub fn parse(rbsp: &[u8]) -> Result<Self, DecodeError> {
        let mut r = BitReader::new(rbsp);
        let pic_parameter_set_id = r.read_ue()?;
        let seq_parameter_set_id = r.read_ue()?;
        let entropy_coding_mode_flag = r.read_bit()?;
        let bottom_field_pic_order_present = r.read_bit()?;
        let num_slice_groups_minus1 = r.read_ue()?;
        if num_slice_groups_minus1 != 0 {
            // FMO: a slice_group map follows here that we neither parse nor
            // support — reject before the syntax shifts under us.
            return Err(DecodeError::Unsupported("slice groups (FMO)"));
        }
        let num_ref_idx_l0_default = r.read_ue()? + 1;
        let num_ref_idx_l1_default = r.read_ue()? + 1;
        let weighted_pred = r.read_bit()?;
        let weighted_bipred_idc = r.read_bits(2)? as u8;
        let pic_init_qp = 26 + r.read_se()?;
        let _pic_init_qs = r.read_se()?;
        let chroma_qp_index_offset = r.read_se()?;
        let deblocking_filter_control_present_flag = r.read_bit()?;
        let constrained_intra_pred_flag = r.read_bit()?;
        let redundant_pic_cnt_present_flag = r.read_bit()?;
        Ok(Self {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            bottom_field_pic_order_present,
            num_ref_idx_l0_default,
            num_ref_idx_l1_default,
            weighted_pred,
            weighted_bipred_idc,
            pic_init_qp,
            chroma_qp_index_offset,
            deblocking_filter_control_present_flag,
            constrained_intra_pred_flag,
            redundant_pic_cnt_present_flag,
        })
    }
}
