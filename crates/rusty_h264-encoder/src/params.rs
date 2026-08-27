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
        // LEVEL FLOOR from what the stream actually signals (Table A-1):
        // the frame must fit the level's MaxFS, and `max_num_ref_frames`
        // frames must fit its MaxDpbMbs — or the stream nominally violates
        // its own level_idc. Both clauses have real instances here: 720p at
        // refs 3 exceeds 3.0's DPB (needs 3.1, exactly what x264 signals),
        // and 1080p exceeds 3.0's MaxFS at ANY ref count (needs 4.0) — the
        // fixed `level_idc: 30` default had been signalling that violation
        // on every 1080p encode, surfaced the day this floor was tested.
        // Only ever RAISES the caller's level, so every already-conformant
        // configuration is byte-identical.
        let frame_mbs = (mb_w * mb_h) as u32;
        let dpb_mbs = frame_mbs * cfg.num_ref_frames.max(1);
        const LEVEL_CAPS: [(u8, u32, u32); 16] = [
            // (level_idc, MaxFS, MaxDpbMbs)
            (10, 99, 396), (11, 396, 900), (12, 396, 2376), (13, 396, 2376),
            (20, 396, 2376), (21, 792, 4752), (22, 1620, 8100), (30, 1620, 8100),
            (31, 3600, 18000), (32, 5120, 20480), (40, 8192, 32768),
            (41, 8192, 32768), (42, 8704, 34816), (50, 22080, 110400),
            (51, 36864, 184320), (52, 36864, 184320),
        ];
        let level_floor = LEVEL_CAPS
            .iter()
            .find(|&&(_, max_fs, max_dpb)| max_fs >= frame_mbs && max_dpb >= dpb_mbs)
            .map(|&(l, _, _)| l)
            .unwrap_or(52);
        Self {
            profile_idc: cfg.profile.profile_idc(),
            constraint_set1_flag: true, // constrained baseline
            level_idc: cfg.level_idc.max(level_floor),
            seq_parameter_set_id: 0,
            log2_max_frame_num_minus4: 0, // log2_max_frame_num = 4
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4, // log2_max_poc_lsb = 8 (256): b-pyramid interleaves reference POCs in coding order, and a 16-value lsb put consecutive-reference steps past the §8.2.1 half-range — ffmpeg (correctly) lost the msb and every list ordering after it
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
        // High-profile prefix (profile_idc >= 100, spec §7.3.2.1.1): chroma_format_idc,
        // bit-depths, transform-bypass, scaling matrices. Baseline/Main (66/77) omit it.
        if self.profile_idc >= 100 {
            w.write_ue(1); // chroma_format_idc = 1 (4:2:0)
            w.write_ue(0); // bit_depth_luma_minus8
            w.write_ue(0); // bit_depth_chroma_minus8
            w.write_bit(false); // qpprime_y_zero_transform_bypass_flag
            w.write_bit(false); // seq_scaling_matrix_present_flag (flat dequant)
        }
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
        // direct_8x8_inference_flag = 1: REQUIRED by the spec for level_idc >= 30
        // (every 720p+ stream), and the only value x264/ffmpeg ever emit. The
        // encoder's direct derivation (b_direct corner colZero) and the
        // transform_size_8x8_flag conditions (allow_t8 / allow8) are keyed to
        // this value IN LOCKSTEP — flipping it back without them desyncs B+8x8.
        w.write_bit(true); // direct_8x8_inference_flag
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
    /// `2` = IMPLICIT weighted bi-prediction (POC-distance weights) — needed so
    /// unequal-distance B-frames (`bframes > 1`) blend correctly; `0` otherwise
    /// (keeps the B-less PPS byte-identical, and `bframes == 1`'s equidistant B
    /// gets 32:32 weights == the plain average anyway).
    pub weighted_bipred_idc: u8,
    /// Explicit weighted prediction for P slices (x264 parity — its `weightp`
    /// default is on). The DECODER side has supported this for months
    /// (validated against x264 weightp streams); the encoder emits identity
    /// weights except where the per-slice fade estimator finds a real gain.
    pub weighted_pred_flag: bool,
    /// CABAC (`1`) vs CAVLC (`0`). Set from [`EncoderConfig::cabac`].
    pub entropy_coding_mode_flag: bool,
    /// `transform_8x8_mode_flag` (High-profile PPS extension). Set from
    /// [`EncoderConfig::transform_8x8`]; requires profile_idc 100.
    pub transform_8x8_mode_flag: bool,
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
            weighted_bipred_idc: if cfg.bframes > 0 { 2 } else { 0 },
            weighted_pred_flag: cfg.weightp,
            entropy_coding_mode_flag: cfg.cabac,
            transform_8x8_mode_flag: cfg.transform_8x8,
        }
    }

    /// Writes the PPS RBSP (without NAL header) including trailing bits.
    pub fn write_rbsp(&self, w: &mut BitWriter) {
        w.write_ue(self.pic_parameter_set_id);
        w.write_ue(self.seq_parameter_set_id);
        w.write_bit(self.entropy_coding_mode_flag); // entropy_coding_mode_flag (0=CAVLC, 1=CABAC)
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(self.num_ref_idx_l0_default_active_minus1);
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(self.weighted_pred_flag); // weighted_pred_flag (explicit P WP)
        w.write_bits(self.weighted_bipred_idc as u32, 2); // weighted_bipred_idc
        w.write_se(self.pic_init_qp_minus26);
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(0); // chroma_qp_index_offset
        w.write_bit(self.deblocking_filter_control_present_flag);
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        // High-profile PPS extension (present iff more RBSP data). We only emit it to
        // signal the 8×8 transform; no picture scaling matrices (flat dequant).
        if self.transform_8x8_mode_flag {
            w.write_bit(true); // transform_8x8_mode_flag
            w.write_bit(false); // pic_scaling_matrix_present_flag
            w.write_se(0); // second_chroma_qp_index_offset
        }
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

    /// The shipped default is now HIGH + CABAC + the 8x8 transform (R6, 2026-08-08;
    /// previously Main + CABAC). Assert all three reach the BITSTREAM, not merely the
    /// config: `transform_8x8` is inert unless the PPS actually carries
    /// `transform_8x8_mode_flag`, so checking the struct field would pin nothing.
    #[test]
    fn default_config_signals_high_profile_cabac_and_8x8() {
        let cfg = EncoderConfig::new(320, 240);
        let sps = Sps::from_config(&cfg);
        let nal = sps.to_nal();
        let rbsp = emulation_unprevent(&nal.rbsp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.read_bits(8).unwrap(), 100, "profile_idc should be High");

        let pps = Pps::from_config(&cfg);
        let nal = pps.to_nal();
        let rbsp = emulation_unprevent(&nal.rbsp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.read_ue().unwrap(), 0); // pic_parameter_set_id
        assert_eq!(r.read_ue().unwrap(), 0); // seq_parameter_set_id
        assert!(r.read_bit().unwrap(), "entropy_coding_mode should be CABAC");
        // Walk the rest of the base PPS to reach the extension (spec 7.3.2.2).
        r.read_bit().unwrap(); // bottom_field_pic_order_in_frame_present_flag
        assert_eq!(r.read_ue().unwrap(), 0, "num_slice_groups_minus1"); // no slice groups
        r.read_ue().unwrap(); // num_ref_idx_l0_default_active_minus1
        r.read_ue().unwrap(); // num_ref_idx_l1_default_active_minus1
        r.read_bit().unwrap(); // weighted_pred_flag
        r.read_bits(2).unwrap(); // weighted_bipred_idc
        r.read_se().unwrap(); // pic_init_qp_minus26
        r.read_se().unwrap(); // pic_init_qs_minus26
        r.read_se().unwrap(); // chroma_qp_index_offset
        r.read_bit().unwrap(); // deblocking_filter_control_present_flag
        r.read_bit().unwrap(); // constrained_intra_pred_flag
        r.read_bit().unwrap(); // redundant_pic_cnt_present_flag
        assert!(
            r.read_bit().unwrap(),
            "transform_8x8_mode_flag should be set — without it in the PPS the 8x8              transform cannot be signalled at all and the default is inert"
        );
    }

    #[test]
    fn sps_roundtrips_through_reader() {
        // Pin the toolset this test is ABOUT (Baseline + CAVLC) rather than inheriting
        // it from the default, which now ships Main + CABAC.
        let mut cfg = EncoderConfig::new(1920, 1080); // 1080 not a multiple of 16 -> cropping
        cfg.profile = rusty_h264_common::Profile::ConstrainedBaseline;
        cfg.cabac = false;
        cfg.num_ref_frames = 1; // pinned like profile/cabac: the default is now 3, and at 1080p that (correctly) raises the level floor this test isn't about
        let sps = Sps::from_config(&cfg);
        let nal = sps.to_nal();

        let rbsp = emulation_unprevent(&nal.rbsp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.read_bits(8).unwrap(), 66); // profile_idc
        let constraints = r.read_bits(8).unwrap();
        assert_eq!((constraints >> 6) & 1, 1); // constraint_set1_flag
        // 40, not the config's 30: 1080p is 8160 MBs and level 3.0's MaxFS is
        // 1620 — the old fixed level was a latent spec violation on every
        // 1080p stream, surfaced (and fixed) by the Table A-1 level floor.
        assert_eq!(r.read_bits(8).unwrap(), 40); // level_idc
        assert_eq!(r.read_ue().unwrap(), 0); // sps id
        assert_eq!(r.read_ue().unwrap(), 0); // log2_max_frame_num_minus4
        assert_eq!(r.read_ue().unwrap(), 0); // poc type
        assert_eq!(r.read_ue().unwrap(), 4); // log2_max_poc_lsb_minus4 (256-value lsb — the b-pyramid POC fix)
        assert_eq!(r.read_ue().unwrap(), 1); // max_num_ref_frames
        assert!(!r.read_bit().unwrap()); // gaps
        assert_eq!(r.read_ue().unwrap(), 119); // 1920/16 - 1
        assert_eq!(r.read_ue().unwrap(), 67); // ceil(1080/16)-1 = 68-1
        assert!(r.read_bit().unwrap()); // frame_mbs_only
        assert!(r.read_bit().unwrap()); // direct_8x8_inference_flag = 1 (level >= 3.0 requirement)
        assert!(r.read_bit().unwrap()); // cropping present (1080)
        assert_eq!(r.read_ue().unwrap(), 0); // crop left
        assert_eq!(r.read_ue().unwrap(), 0); // crop right
        assert_eq!(r.read_ue().unwrap(), 0); // crop top
        assert_eq!(r.read_ue().unwrap(), 4); // crop bottom: (1088-1080)/2
    }

    /// The DPB-derived level floor (Table A-1 MaxDpbMbs): raises `level_idc`
    /// exactly when the signalled reference count cannot fit the caller's
    /// level, and never otherwise.
    #[test]
    fn level_floor_tracks_dpb() {
        let lvl = |w: usize, h: usize, refs: u32| {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.num_ref_frames = refs;
            Sps::from_config(&cfg).level_idc
        };
        assert_eq!(lvl(352, 288, 1), 30); // CIF: caller's 3.0 stands
        assert_eq!(lvl(352, 288, 3), 30); // CIF x3 = 1188 MBs, still fits 3.0
        assert_eq!(lvl(1280, 720, 1), 31); // 720p = 3600 MBs > 3.0's MaxFS 1620 (latent; floor fixes it)
        assert_eq!(lvl(1280, 720, 3), 31); // 720p x3 = 10800 fits 3.1's DPB 18000
        assert_eq!(lvl(1920, 1080, 1), 40); // 1080p = 8160 MBs > 3.2's MaxFS (the latent 1080p violation)
        assert_eq!(lvl(1920, 1080, 3), 40); // and x3 = 24480 fits 4.0's DPB 32768
        assert_eq!(lvl(1920, 1080, 16), 51); // stress: 16-ref 1080p
    }

    #[test]
    fn pps_roundtrips_through_reader() {
        let mut cfg = EncoderConfig::new(640, 480);
        cfg.profile = rusty_h264_common::Profile::ConstrainedBaseline;
        cfg.cabac = false;
        cfg.num_ref_frames = 1; // pinned: this test is about PPS syntax, not the refs default (now 3)
        cfg.weightp = false; // pinned likewise: weightp defaults ON since the x264-parity landing
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
