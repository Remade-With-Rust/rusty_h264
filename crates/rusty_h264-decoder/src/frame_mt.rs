//! Campaign #1 — frame-level decoder threading.
//!
//! This is the ffmpeg-shaped parallel unit: one worker = one picture =
//! slice parse (`decode_slice_*_inner`) + recon (`hl_decode_mb` equivalent)
//! on the SAME thread. `edc_worker` must NOT spawn here (see `edc_spawn_worker`).
//!
//! Phase A: full-reference barrier (`RS_H264_FRAME_THREADS=N`).
//! Phase B: `RS_H264_ROW_PROGRESS=1` — early-start when deps are in-flight,
//! row watermarks on [`RefFrame::ready_rows`], MC via [`RefFrame::luma_guard`].
//!
//! Measure with `bench/pinmt.ps1` — not the 1T CPU race.

use crate::params::{Pps, Sps};
use crate::{DecodeError, Decoder, PocState, Ref, RefFrame};
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::{BitReader, NalUnitType, YuvFrame};
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

/// `RS_H264_FRAME_THREADS` — 0/unset/1 = serial; N>1 = worker count.
pub(crate) fn frame_threads() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RS_H264_FRAME_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}

pub(crate) fn row_progress_on() -> bool {
    // Default OFF (Phase A barrier) — Phase B early-start is opt-in until it
    // beats Phase A on pinmt wall. `RS_H264_ROW_PROGRESS=1` enables it.
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("RS_H264_ROW_PROGRESS").is_some_and(|v| v == "1"))
}

/// Incremental strip publish into live planes. Default OFF — early-start still
/// overlaps parse/setup while MC parks until finalize/freeze; strip copy was a
/// measured wall regression vs Phase A. Opt in with `RS_H264_ROW_PUB=1`.
pub(crate) fn row_publish_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("RS_H264_ROW_PUB").is_some_and(|v| v == "1"))
}

/// Min published luma rows on an in-flight dep before early-starting when strip
/// publish is on. Without strip publish, any in-flight dep is enough (MC parks).
const EARLY_LEAD_ROWS: usize = 32;

fn dep_allows_early(inflight: &HashMap<usize, Ref>, d: usize) -> bool {
    let Some(r) = inflight.get(&d) else {
        return false;
    };
    if r.frozen.get().is_some() {
        return true;
    }
    if !row_publish_on() {
        return true;
    }
    r.ready_rows.load(std::sync::atomic::Ordering::Acquire) >= EARLY_LEAD_ROWS
}

struct PicPacket {
    nals: Vec<Vec<u8>>,
    is_ref: bool,
    is_idr: bool,
    dep_refs: Vec<usize>,
    /// Cropped coded size from the active SPS (for Phase B progress slots).
    cw: usize,
    ch: usize,
    b_possible: bool,
    mb_w: usize,
}

struct JobOut {
    idx: usize,
    poc: i32,
    frame: YuvFrame,
    /// New reference for this picture, if `is_ref`. Marking applied by commit.
    new_ref: Option<Ref>,
    /// Worker decoder state after decode — carries mmco application helpers.
    worker: Decoder,
}

fn assemble(
    stream: &[u8],
) -> Result<(HashMap<u32, Sps>, HashMap<u32, Pps>, Vec<PicPacket>, usize), DecodeError> {
    let mut sps_map = HashMap::new();
    let mut pps_map = HashMap::new();
    let mut pics: Vec<PicPacket> = Vec::new();
    let mut cur: Option<PicPacket> = None;
    let mut max_refs = 1usize;

    for nal in split_annex_b(stream) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = NalUnitType::from_id(nal[0]);
        let nal_ref_idc = (nal[0] >> 5) & 3;
        let rbsp = emulation_unprevent(&nal[1..]);
        match nal_type {
            NalUnitType::Sps => {
                let s = Sps::parse(&rbsp)?;
                max_refs = s.max_num_ref_frames.max(1) as usize;
                sps_map.insert(s.seq_parameter_set_id, s);
            }
            NalUnitType::Pps => {
                let p = Pps::parse(&rbsp)?;
                pps_map.insert(p.pic_parameter_set_id, p);
            }
            NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                let is_idr = nal_type == NalUnitType::IdrSlice;
                let mut r = BitReader::new(&rbsp);
                let first_mb = r.read_ue()? as usize;
                let _slice_type = r.read_ue()?;
                let pps_id = r.read_ue()?;
                let pps = pps_map
                    .get(&pps_id)
                    .ok_or(DecodeError::MissingParameterSet)?;
                let sps = sps_map
                    .get(&pps.seq_parameter_set_id)
                    .ok_or(DecodeError::MissingParameterSet)?;
                let mb_w = sps.pic_width_in_mbs as usize;
                let mb_h = sps.pic_height_in_mbs as usize;
                let cw = mb_w * 16;
                let ch = mb_h * 16;
                let b_possible = sps.profile_idc != 66;

                if first_mb == 0 {
                    if let Some(p) = cur.take() {
                        pics.push(p);
                    }
                    cur = Some(PicPacket {
                        nals: vec![nal.to_vec()],
                        is_ref: nal_ref_idc != 0,
                        is_idr,
                        dep_refs: Vec::new(),
                        cw,
                        ch,
                        b_possible,
                        mb_w,
                    });
                } else {
                    let Some(p) = cur.as_mut() else {
                        return Err(DecodeError::Unsupported(
                            "slice continues a missing picture",
                        ));
                    };
                    p.nals.push(nal.to_vec());
                    p.is_ref |= nal_ref_idc != 0;
                }
            }
            _ => {}
        }
    }
    if let Some(p) = cur.take() {
        pics.push(p);
    }

    let mut prior_refs: Vec<usize> = Vec::new();
    for i in 0..pics.len() {
        if pics[i].is_idr {
            pics[i].dep_refs.clear();
            prior_refs.clear();
        } else {
            let start = prior_refs.len().saturating_sub(max_refs);
            pics[i].dep_refs = prior_refs[start..].to_vec();
        }
        if pics[i].is_ref {
            prior_refs.push(i);
        }
    }
    Ok((sps_map, pps_map, pics, max_refs))
}

/// Advance POC state from the first slice of a picture (submit-thread copy).
/// Returns `(frame_num, pic_poc)` for progress-slot identity.
fn advance_poc(
    poc: &mut PocState,
    sps: &HashMap<u32, Sps>,
    pps: &HashMap<u32, Pps>,
    packet: &PicPacket,
) -> Result<(u32, i32), DecodeError> {
    let nal = packet.nals.first().ok_or(DecodeError::Truncated)?;
    let &hdr = nal.first().ok_or(DecodeError::Truncated)?;
    let nal_ref_idc = (hdr >> 5) & 3;
    let is_idr = NalUnitType::from_id(hdr) == NalUnitType::IdrSlice;
    let rbsp = emulation_unprevent(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let _first_mb = r.read_ue()?;
    let _slice_type = r.read_ue()?;
    let pps_id = r.read_ue()?;
    let pps = pps.get(&pps_id).ok_or(DecodeError::MissingParameterSet)?;
    let sps = sps
        .get(&pps.seq_parameter_set_id)
        .ok_or(DecodeError::MissingParameterSet)?;
    let frame_num = r.read_bits(sps.log2_max_frame_num)?;
    if is_idr {
        let _idr_pic_id = r.read_ue()?;
    }
    let mut poc_lsb = 0u32;
    let mut delta_poc_bottom = 0i32;
    if sps.pic_order_cnt_type == 0 {
        poc_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb)?;
        if pps.bottom_field_pic_order_present {
            delta_poc_bottom = r.read_se()?;
        }
    } else if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero {
        let _ = r.read_se()?;
        if pps.bottom_field_pic_order_present {
            let _ = r.read_se()?;
        }
    }
    let pic_poc = poc.compute_poc(sps, is_idr, nal_ref_idc, frame_num, poc_lsb, delta_poc_bottom);
    Ok((frame_num, pic_poc))
}

fn decode_pic_detached(
    sps: &HashMap<u32, Sps>,
    pps: &HashMap<u32, Pps>,
    refs: Vec<Ref>,
    packet: &PicPacket,
    row_progress: bool,
    prev_ref_frame_num: u32,
    poc_state: PocState,
    progress: Option<Ref>,
) -> Result<JobOut, DecodeError> {
    let mut d = Decoder::default();
    d.sps = sps.clone();
    d.pps = pps.clone();
    d.refs = refs;
    d.prev_ref_frame_num = prev_ref_frame_num;
    d.detach_dpb = true;
    d.frame_mt_row_progress = row_progress;
    d.poc = poc_state;
    d.progress_slot = progress;
    let mut last_frame = None;
    for nal in &packet.nals {
        let mut au = Vec::with_capacity(4 + nal.len());
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(nal);
        if let Some(f) = d.decode(&au)? {
            last_frame = Some(f);
        }
    }
    let frame = last_frame.ok_or(DecodeError::Truncated)?;
    let poc = d.last_poc;
    let new_ref = d.detached_ref.take();
    Ok(JobOut {
        idx: 0,
        poc,
        frame,
        new_ref,
        worker: d,
    })
}

fn snapshot_refs(
    dpb: &[Ref],
    pics: &[PicPacket],
    inflight: &HashMap<usize, Ref>,
    deps: &[usize],
    ref_done: &[bool],
    row_progress: bool,
) -> Vec<Ref> {
    let mut refs = dpb.to_vec();
    if !row_progress {
        return refs;
    }
    // Most-recent unfinished dep first (DPB is most-recent-first).
    for &d in deps.iter().rev() {
        if ref_done.get(d).copied().unwrap_or(false) {
            continue;
        }
        if let Some(arc) = inflight.get(&d) {
            if !refs.iter().any(|r| std::sync::Arc::ptr_eq(r, arc)) {
                refs.insert(0, arc.clone());
            }
        }
    }
    let _ = pics;
    refs
}

pub(crate) fn decode_stream_threaded(
    annex_b: &[u8],
    threads: usize,
) -> Result<Vec<YuvFrame>, DecodeError> {
    let mut out = Vec::new();
    decode_stream_threaded_sink(annex_b, threads, |f| out.push(f))?;
    Ok(out)
}

/// Frame-MT decode that feeds each display-ordered frame to `sink` (pinmt /
/// `decode_bench` timed path can drop without retaining all YUV).
pub(crate) fn decode_stream_threaded_sink(
    annex_b: &[u8],
    threads: usize,
    mut sink: impl FnMut(YuvFrame),
) -> Result<usize, DecodeError> {
    let threads = threads.max(1);
    let row_progress = row_progress_on();
    let (sps, pps, pics, _max_refs) = assemble(annex_b)?;
    let n = pics.len();
    if n == 0 {
        return Ok(0);
    }

    let (job_tx, job_rx) = mpsc::sync_channel::<(usize, Vec<Ref>, u32, PocState, Option<Ref>)>(
        threads * 2,
    );
    let (res_tx, res_rx) = mpsc::channel::<Result<JobOut, DecodeError>>();

    let sps_w = sps.clone();
    let pps_w = pps.clone();
    let pics_w = pics.clone();
    thread::scope(|scope| {
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        for _ in 0..threads {
            let res_tx = res_tx.clone();
            let job_rx = std::sync::Arc::clone(&job_rx);
            let sps_w = sps_w.clone();
            let pps_w = pps_w.clone();
            let pics_w = pics_w.clone();
            scope.spawn(move || {
                loop {
                    let msg = { job_rx.lock().unwrap().recv() };
                    let Ok((idx, refs, prev_fn, poc_st, progress)) = msg else {
                        break;
                    };
                    let Some(pic_w) = pics_w.get(idx) else { break };
                    let mut out = match decode_pic_detached(
                        &sps_w,
                        &pps_w,
                        refs,
                        pic_w,
                        row_progress,
                        prev_fn,
                        poc_st,
                        progress,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            let _ = res_tx.send(Err(e));
                            break;
                        }
                    };
                    out.idx = idx;
                    if res_tx.send(Ok(out)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(res_tx);

        let mut ref_done = vec![false; n];
        let mut dpb: Vec<Ref> = Vec::new();
        let mut inflight: HashMap<usize, Ref> = HashMap::new();
        let mut prev_ref_fn = 0u32;
        let mut submit_poc = PocState::default();
        let mut next_submit = 0usize;
        let mut next_commit = 0usize;
        let mut inflight_n = 0usize;
        let mut pending: HashMap<usize, JobOut> = HashMap::new();
        let mut frames_out = 0usize;
        let mut gop: Vec<(i32, YuvFrame)> = Vec::new();

        while next_commit < n {
            while next_submit < n && inflight_n < threads * 2 {
                // Bind the picture ONCE: it was indexed six times in this block,
                // each a separate check against the same Vec.
                let Some(pic) = pics.get(next_submit) else { break };
                let deps_ok = pic.dep_refs.iter().all(|&d| {
                    ref_done.get(d).copied().unwrap_or(false)
                        || (row_progress && dep_allows_early(&inflight, d))
                });
                if !deps_ok {
                    break;
                }
                let poc_for_worker = submit_poc.clone();
                let (frame_num, pic_poc) =
                    advance_poc(&mut submit_poc, &sps, &pps, pic)?;

                let progress = if row_progress && pic.is_ref {
                    let mut slot = RefFrame::new_progress_slot(
                        pic.cw,
                        pic.ch,
                        pic.b_possible,
                        pic.mb_w,
                    );
                    RefFrame::init_progress_identity(&mut slot, frame_num, pic_poc);
                    inflight.insert(next_submit, slot.clone());
                    Some(slot)
                } else {
                    None
                };

                let refs = snapshot_refs(
                    &dpb,
                    &pics,
                    &inflight,
                    &pic.dep_refs,
                    &ref_done,
                    row_progress,
                );
                if job_tx
                    .send((
                        next_submit,
                        refs,
                        prev_ref_fn,
                        poc_for_worker,
                        progress,
                    ))
                    .is_err()
                {
                    return Err(DecodeError::Truncated);
                }
                inflight_n += 1;
                next_submit += 1;
            }

            let job = res_rx.recv().map_err(|_| DecodeError::Truncated)??;
            inflight_n -= 1;
            pending.insert(job.idx, job);

            while next_commit < n {
                let Some(mut job) = pending.remove(&next_commit) else {
                    break;
                };
                if pics.get(next_commit).is_some_and(|p| p.is_idr) {
                    dpb.clear();
                    gop.sort_by_key(|p| p.0);
                    for (_, fr) in gop.drain(..) {
                        sink(fr);
                        frames_out += 1;
                    }
                }
                if let Some(r) = job.new_ref.take() {
                    r.mark_fully_ready();
                    job.worker.refs = std::mem::take(&mut dpb);
                    job.worker.commit_detached_ref_arc(r)?;
                    dpb = std::mem::take(&mut job.worker.refs);
                    inflight.remove(&next_commit);
                    if let Some(f) = ref_done.get_mut(next_commit) {
                        *f = true;
                    }
                } else if pics.get(next_commit).is_some_and(|p| p.is_ref) {
                    return Err(DecodeError::Truncated);
                }
                prev_ref_fn = job.worker.prev_ref_frame_num;
                gop.push((job.poc, job.frame));
                next_commit += 1;
            }
        }

        gop.sort_by_key(|p| p.0);
        for (_, fr) in gop.drain(..) {
            sink(fr);
            frames_out += 1;
        }
        drop(job_tx);
        Ok(frames_out)
    })
}

impl Clone for PicPacket {
    fn clone(&self) -> Self {
        Self {
            nals: self.nals.clone(),
            is_ref: self.is_ref,
            is_idr: self.is_idr,
            dep_refs: self.dep_refs.clone(),
            cw: self.cw,
            ch: self.ch,
            b_possible: self.b_possible,
            mb_w: self.mb_w,
        }
    }
}
