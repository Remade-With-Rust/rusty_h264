//! Slice header coding. The slice body (macroblock layer) is produced by
//! [`crate::mb16`], which codes I_16x16 macroblocks.

use crate::config::EncoderConfig;
use rusty_h264_common::BitWriter;

/// `slice_type` 7 = I slice, "all slices in the picture are I" variant.
const SLICE_TYPE_I_ALL: u32 = 7;
/// `slice_type` 5 = P slice, "all slices in the picture are P" variant.
const SLICE_TYPE_P_ALL: u32 = 5;
/// `slice_type` 6 = B slice, "all slices in the picture are B" variant.
const SLICE_TYPE_B_ALL: u32 = 6;

/// Writes the slice header for a B-slice. B-frames are **non-reference**
/// (`nal_ref_idc == 0`), so `dec_ref_pic_marking` is omitted. `num_l0`/`num_l1`
/// are the active List-0/List-1 reference counts; `direct_spatial_mv_pred_flag` is
/// signalled as 1 (spatial direct — the derivation our decoder implements).
#[allow(clippy::too_many_arguments)]
pub fn write_b_slice_header(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    qp: u8,
    frame_num: u32,
    poc_lsb: u32,
    num_l0: usize,
    num_l1: usize,
) {
    w.write_ue(0); // first_mb_in_slice
    w.write_ue(SLICE_TYPE_B_ALL); // slice_type = B
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(frame_num, 4); // frame_num (log2_max_frame_num = 4)
    w.write_bits(poc_lsb, 4); // pic_order_cnt_lsb (poc type 0, log2_max = 4)
    w.write_bit(true); // direct_spatial_mv_pred_flag = 1
    w.write_bit(true); // num_ref_idx_active_override_flag
    w.write_ue(num_l0.max(1) as u32 - 1); // num_ref_idx_l0_active_minus1
    w.write_ue(num_l1.max(1) as u32 - 1); // num_ref_idx_l1_active_minus1
    w.write_bit(false); // ref_pic_list_modification_flag_l0
    w.write_bit(false); // ref_pic_list_modification_flag_l1
    // No pred_weight_table (weighted_bipred_idc = 0). No dec_ref_pic_marking
    // (nal_ref_idc = 0). deblocking_filter_control present in our PPS.
    w.write_se(qp as i32 - cfg.qp as i32); // slice_qp_delta
    w.write_ue(0); // disable_deblocking_filter_idc = 0
    w.write_se(0); // slice_alpha_c0_offset_div2
    w.write_se(0); // slice_beta_offset_div2
}

/// Writes the slice header for a non-IDR P-slice. Single reference (the previous
/// picture), default ref-index count, sliding-window ref management.
pub fn write_p_slice_header(
    w: &mut BitWriter,
    cfg: &EncoderConfig,
    qp: u8,
    frame_num: u32,
    poc_lsb: u32,
    num_ref_idx_active: usize,
) {
    w.write_ue(0); // first_mb_in_slice
    w.write_ue(SLICE_TYPE_P_ALL); // slice_type = P
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(frame_num, 4); // frame_num (log2_max_frame_num = 4)
    w.write_bits(poc_lsb, 4); // pic_order_cnt_lsb (poc type 0, log2_max = 4)
    // With multiple references configured, override the active count to the
    // current DPB size (it warms up over the first GOP); single-reference keeps
    // the PPS default and writes no override (byte-identical to before).
    if cfg.num_ref_frames > 1 {
        w.write_bit(true); // num_ref_idx_active_override_flag
        w.write_ue(num_ref_idx_active.max(1) as u32 - 1); // num_ref_idx_l0_active_minus1
    } else {
        w.write_bit(false); // num_ref_idx_active_override_flag (use PPS default = 1)
    }
    w.write_bit(false); // ref_pic_list_modification_flag_l0
    // dec_ref_pic_marking (nal_ref_idc != 0, non-IDR):
    w.write_bit(false); // adaptive_ref_pic_marking_mode_flag (sliding window)
    // cabac_init_idc (spec 7.3.3): present for CABAC non-I slices, before slice_qp_delta.
    if cfg.cabac {
        w.write_ue(0); // cabac_init_idc = 0 (matches CabacEncoder::new init_idc)
    }
    w.write_se(qp as i32 - cfg.qp as i32); // slice_qp_delta (SliceQP - pic_init_qp)
    // deblocking_filter_control_present_flag = 1 in our PPS: filter on, 0 offsets.
    w.write_ue(0); // disable_deblocking_filter_idc = 0
    w.write_se(0); // slice_alpha_c0_offset_div2
    w.write_se(0); // slice_beta_offset_div2
}

/// Writes the slice header for an IDR I-slice. `frame_num`/`poc` are 0 for an
/// IDR. Assumes `pic_order_cnt_type = 0` and `log2_max_*_minus4 = 0` (so both
/// `frame_num` and `pic_order_cnt_lsb` are 4-bit fields), matching the SPS.
pub fn write_idr_slice_header(w: &mut BitWriter, cfg: &EncoderConfig, qp: u8) {
    w.write_ue(0); // first_mb_in_slice
    w.write_ue(SLICE_TYPE_I_ALL); // slice_type
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(0, 4); // frame_num (log2_max_frame_num = 4)
    // frame_mbs_only_flag = 1 -> no field_pic_flag.
    w.write_ue(0); // idr_pic_id (IDR only)
    w.write_bits(0, 4); // pic_order_cnt_lsb (poc type 0, log2_max = 4)
    // dec_ref_pic_marking (nal_ref_idc != 0, IDR):
    w.write_bit(false); // no_output_of_prior_pics_flag
    w.write_bit(false); // long_term_reference_flag
    w.write_se(qp as i32 - cfg.qp as i32); // slice_qp_delta (SliceQP - pic_init_qp)
    // deblocking_filter_control_present_flag = 1 in our PPS: enable the in-loop
    // filter (idc = 0) with zero offsets. The decoder applies it as a post-pass.
    w.write_ue(0); // disable_deblocking_filter_idc = 0 (filter on)
    w.write_se(0); // slice_alpha_c0_offset_div2
    w.write_se(0); // slice_beta_offset_div2
}
