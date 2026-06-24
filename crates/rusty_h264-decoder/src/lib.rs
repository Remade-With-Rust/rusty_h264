//! Pure-Rust H.264 (Constrained Baseline) decoder.
//!
//! Parses SPS/PPS/IDR-slice headers and reconstructs I_16x16 (DC-predicted)
//! macroblocks: CAVLC residual decode, inverse transform (incl. luma/chroma DC
//! Hadamard), intra DC prediction. The reconstruction path is shared with the
//! encoder so the two agree bit-for-bit. Inter prediction and deblocking land in
//! later generations behind this same API.

mod mb16;
mod params;

pub use params::{Pps, Sps};

use mb16::FrameDecoder;
use rusty_h264_common::bit_reader::OutOfData;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::{BitReader, NalUnitType, YuvFrame};

/// Decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Bitstream ended unexpectedly.
    Truncated,
    /// A required parameter set was missing before a slice.
    MissingParameterSet,
    /// A coding tool outside the implemented subset appeared in the stream.
    Unsupported(&'static str),
}

impl From<OutOfData> for DecodeError {
    fn from(_: OutOfData) -> Self {
        DecodeError::Truncated
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("bitstream truncated"),
            DecodeError::MissingParameterSet => f.write_str("slice before SPS/PPS"),
            DecodeError::Unsupported(s) => write!(f, "unsupported coding tool: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A reference picture: deblocked reconstruction at coded resolution.
/// Stored now (4a); read by motion compensation in 4b.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct RefFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub cw: usize,
    pub ch: usize,
}

/// A Constrained Baseline H.264 decoder. Holds the most recent parameter sets
/// and the previous decoded picture (the inter reference) across calls.
#[derive(Debug, Default)]
pub struct Decoder {
    sps: Option<Sps>,
    pps: Option<Pps>,
    /// Decoded-picture buffer (most-recent first); `ref_idx` indexes into this.
    refs: Vec<RefFrame>,
}

impl Decoder {
    /// Creates a decoder with no parameter sets yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes a complete Annex-B access unit, returning the reconstructed,
    /// cropped frame if the access unit contained a coded picture.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<YuvFrame>, DecodeError> {
        let mut frame = None;
        for nal in split_annex_b(annex_b) {
            if nal.is_empty() {
                continue;
            }
            let nal_type = NalUnitType::from_id(nal[0]);
            let rbsp = emulation_unprevent(&nal[1..]);
            match nal_type {
                NalUnitType::Sps => self.sps = Some(Sps::parse(&rbsp)?),
                NalUnitType::Pps => self.pps = Some(Pps::parse(&rbsp)?),
                NalUnitType::IdrSlice => {
                    self.refs.clear();
                    frame = Some(self.decode_slice(&rbsp, nal_type)?);
                }
                NalUnitType::NonIdrSlice => {
                    frame = Some(self.decode_slice(&rbsp, nal_type)?);
                }
                _ => {} // SEI, AUD, etc. ignored
            }
        }
        Ok(frame)
    }

    fn decode_slice(
        &mut self,
        rbsp: &[u8],
        nal_type: NalUnitType,
    ) -> Result<YuvFrame, DecodeError> {
        let sps = self.sps.clone().ok_or(DecodeError::MissingParameterSet)?;
        let pps = self.pps.clone().ok_or(DecodeError::MissingParameterSet)?;
        let sps = &sps;
        let pps = &pps;
        if pps.entropy_coding_mode_flag {
            return Err(DecodeError::Unsupported("CABAC"));
        }

        let mut r = BitReader::new(rbsp);
        // --- slice_header ---
        let _first_mb_in_slice = r.read_ue()?;
        let slice_type = r.read_ue()?;
        let is_p = matches!(slice_type, 0 | 5);
        if !is_p && !matches!(slice_type, 2 | 7) {
            return Err(DecodeError::Unsupported("only I and P slices"));
        }
        let _pic_parameter_set_id = r.read_ue()?;
        let _frame_num = r.read_bits(sps.log2_max_frame_num)?;
        let is_idr = nal_type == NalUnitType::IdrSlice;
        if is_idr {
            let _idr_pic_id = r.read_ue()?;
        }
        if sps.pic_order_cnt_type == 0 {
            let _poc_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb)?;
        }
        if is_p {
            // num_ref_idx_active_override_flag
            if r.read_bit()? {
                let _num_ref_idx_l0_active_minus1 = r.read_ue()?;
            }
            // ref_pic_list_modification_flag_l0
            if r.read_bit()? {
                return Err(DecodeError::Unsupported("ref pic list modification"));
            }
        }
        if is_idr {
            let _no_output_of_prior_pics = r.read_bit()?;
            let _long_term_reference = r.read_bit()?;
        } else {
            let adaptive = r.read_bit()?;
            if adaptive {
                return Err(DecodeError::Unsupported("adaptive ref pic marking"));
            }
        }
        let slice_qp_delta = r.read_se()?;
        let mut deblock = false;
        if pps.deblocking_filter_control_present_flag {
            let disable_deblocking_filter_idc = r.read_ue()?;
            deblock = disable_deblocking_filter_idc != 1;
            if disable_deblocking_filter_idc != 1 {
                // We only support zero alpha/beta offsets (what our encoder emits).
                let a = r.read_se()?;
                let b = r.read_se()?;
                if a != 0 || b != 0 {
                    return Err(DecodeError::Unsupported("nonzero deblocking offsets"));
                }
            }
        }
        let slice_qp = (pps.pic_init_qp + slice_qp_delta).clamp(0, 51) as u8;

        // --- slice_data ---
        let mut fd = FrameDecoder::new(
            sps.pic_width_in_mbs,
            sps.pic_height_in_mbs,
            slice_qp,
            self.refs.clone(),
        );
        fd.decode_slice_data(&mut r, is_p).map_err(|e| match e {
            mb16::MbError::Truncated => DecodeError::Truncated,
            mb16::MbError::Unsupported(s) => DecodeError::Unsupported(s),
        })?;
        if deblock {
            fd.deblock();
        }
        // This deblocked picture enters the DPB (most-recent first), kept to
        // max_num_ref_frames by a sliding window — mirroring the encoder.
        self.refs.insert(0, fd.as_reference());
        self.refs.truncate(sps.max_num_ref_frames.max(1) as usize);
        Ok(fd.into_frame(sps.frame_crop_right as usize, sps.frame_crop_bottom as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_param_sets_errors() {
        let mut d = Decoder::new();
        // A lone (fake) IDR slice with no SPS/PPS.
        let nal = rusty_h264_common::NalUnit::new(3, NalUnitType::IdrSlice, vec![0x88]);
        let err = d.decode(&nal.to_annex_b()).unwrap_err();
        assert_eq!(err, DecodeError::MissingParameterSet);
    }
}
