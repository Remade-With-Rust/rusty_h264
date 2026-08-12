//! Phase 0 census for campaign #1 (frame-MT): slice counts + full-ref-barrier
//! dependency / in-flight ceiling on a stream.
//!
//! ```text
//! cargo run --release -p rusty_h264-decoder --example fmt_census -- stream.264
//! ```

use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_common::{BitReader, NalUnitType};
use rusty_h264_decoder::{Pps, Sps};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
struct PicMeta {
    decode_idx: usize,
    frame_num: u32,
    is_idr: bool,
    is_ref: bool,
    is_b: bool,
    is_p: bool,
    is_i: bool,
    slices: u32,
    /// Under full-ref barrier: decode indices that must finish first.
    /// Conservative: all prior reference pictures (any nal_ref_idc!=0) that are
    /// still "live" in a sliding window of max_refs — approximated as the set of
    /// prior refs not yet superseded beyond max_num_ref_frames.
    deps: u32, // count of prior unfinished refs at submit time (sim)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: fmt_census <stream.264>");
    let data = std::fs::read(&path).expect("read");
    let mut sps_map: HashMap<u32, Sps> = HashMap::new();
    let mut pps_map: HashMap<u32, Pps> = HashMap::new();
    let mut pics: Vec<PicMeta> = Vec::new();
    let mut cur: Option<(PicMeta, usize)> = None; // meta, total_mb
    let mut max_refs = 1usize;

    for nal in split_annex_b(&data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = NalUnitType::from_id(nal[0]);
        let nal_ref_idc = (nal[0] >> 5) & 3;
        let rbsp = emulation_unprevent(&nal[1..]);
        match nal_type {
            NalUnitType::Sps => {
                if let Ok(s) = Sps::parse(&rbsp) {
                    max_refs = s.max_num_ref_frames.max(1) as usize;
                    sps_map.insert(s.seq_parameter_set_id, s);
                }
            }
            NalUnitType::Pps => {
                if let Ok(p) = Pps::parse(&rbsp) {
                    pps_map.insert(p.pic_parameter_set_id, p);
                }
            }
            NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                let is_idr = nal_type == NalUnitType::IdrSlice;
                let mut r = BitReader::new(&rbsp);
                let Ok(first_mb) = r.read_ue().map(|v| v as usize) else { continue };
                let Ok(slice_type) = r.read_ue() else { continue };
                let is_p = matches!(slice_type, 0 | 5);
                let is_b = matches!(slice_type, 1 | 6);
                let is_i = matches!(slice_type, 2 | 7);
                let Ok(pps_id) = r.read_ue() else { continue };
                let Some(pps) = pps_map.get(&pps_id) else { continue };
                let Some(sps) = sps_map.get(&pps.seq_parameter_set_id) else { continue };
                let Ok(frame_num) = r.read_bits(sps.log2_max_frame_num) else { continue };
                let total_mb = sps.pic_width_in_mbs * sps.pic_height_in_mbs;

                if first_mb == 0 {
                    if let Some((mut prev, _)) = cur.take() {
                        pics.push(prev);
                    }
                    let m = PicMeta {
                        decode_idx: pics.len() + if cur.is_some() { 1 } else { 0 },
                        frame_num,
                        is_idr,
                        is_ref: nal_ref_idc != 0,
                        is_b,
                        is_p,
                        is_i,
                        slices: 1,
                        deps: 0,
                    };
                    // fix decode_idx properly after push
                    cur = Some((
                        PicMeta {
                            decode_idx: pics.len(),
                            ..m
                        },
                        total_mb,
                    ));
                } else if let Some((ref mut m, _)) = cur {
                    m.slices += 1;
                }
                let _ = (is_p, is_b, is_i, frame_num);
            }
            _ => {}
        }
    }
    if let Some((prev, _)) = cur.take() {
        pics.push(prev);
    }

    // Re-assign decode indices and simulate full-ref barrier.
    for (i, p) in pics.iter_mut().enumerate() {
        p.decode_idx = i;
    }

    // Barrier model: a picture may start when every previously submitted
    // reference picture has finished. Non-ref B/P may start as soon as the
    // refs they need exist — conservative approx: wait for all prior refs
    // still in a window of `max_refs` most-recent refs.
    // Discrete-event sim: each pic has start_time = max(dep finish times),
    // duration = 1 (unit work), finish = start+1. Measure max concurrency.
    let n = pics.len();
    let mut start = vec![0u32; n];
    let mut finish = vec![0u32; n];

    for i in 0..n {
        let p = &pics[i];
        let mut t0 = 0u32;
        // Deps: all prior reference pics (full barrier on DPB contents).
        // Tightened: only the last `max_refs` refs (sliding window approx).
        let prior_refs: Vec<usize> = (0..i).filter(|&j| pics[j].is_ref).collect();
        let window = if prior_refs.len() > max_refs {
            &prior_refs[prior_refs.len() - max_refs..]
        } else {
            &prior_refs[..]
        };
        for &j in window {
            t0 = t0.max(finish[j]);
        }
        // IDR clears DPB — no deps.
        if p.is_idr {
            // Bitstream order: IDR may start after the previous picture finishes.
            t0 = if i > 0 { finish[i - 1] } else { 0 };
        }
        start[i] = t0;
        finish[i] = t0 + 1;
        pics[i].deps = window.len() as u32;
    }

    // Max in-flight: at each time tick, count pics with start <= t < finish
    let tmax = finish.iter().copied().max().unwrap_or(0);
    let mut max_inflight = 0u32;
    let mut sum_inflight = 0u64;
    for t in 0..tmax {
        let c = (0..n)
            .filter(|&i| start[i] <= t && t < finish[i])
            .count() as u32;
        max_inflight = max_inflight.max(c);
        sum_inflight += c as u64;
    }
    let avg_inflight = if tmax > 0 {
        sum_inflight as f64 / tmax as f64
    } else {
        0.0
    };
    // Serial time = n; parallel lower bound = tmax under infinite threads + unit work
    let ceiling = if tmax > 0 {
        n as f64 / tmax as f64
    } else {
        1.0
    };

    let slices: u32 = pics.iter().map(|p| p.slices).sum();
    let multi = pics.iter().filter(|p| p.slices > 1).count();
    let n_i = pics.iter().filter(|p| p.is_i && !p.is_b).count();
    let n_p = pics.iter().filter(|p| p.is_p).count();
    let n_b = pics.iter().filter(|p| p.is_b).count();
    let n_ref = pics.iter().filter(|p| p.is_ref).count();

    println!("stream={path}");
    println!(
        "pictures={n} slices={slices} slices_per_pic={:.2} multi_slice_pics={multi} ({:.1}%)",
        slices as f64 / n.max(1) as f64,
        100.0 * multi as f64 / n.max(1) as f64
    );
    println!("types: I~{n_i} P={n_p} B={n_b}  refs={n_ref}  max_num_ref_frames={max_refs}");
    println!(
        "full-ref-barrier (unit-work discrete event): serial={n} parallel_span={tmax}  ceiling={ceiling:.3}x  max_inflight={max_inflight} avg_inflight={avg_inflight:.2}"
    );
    println!(
        "slice-MT note: mean slices/pic={:.2} — {} as primary lever",
        slices as f64 / n.max(1) as f64,
        if (slices as f64 / n.max(1) as f64) < 4.0 {
            "WEAK"
        } else {
            "viable"
        }
    );
}
