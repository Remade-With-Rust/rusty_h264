//! **A0 — `cabac_init_idc` per-slice dispatch.** The rung-0 CASC pilot.
//!
//! For P/B slices the encoder signals `cabac_init_idc` (0..2) in the slice
//! header, selecting one of three context-init tables. rs_h264 ships a constant
//! **0**. The knob is already normative and already signalled, so choosing it
//! per slice costs **no extra bits** and every conformant decoder handles all
//! three — census category 3, zero bitstream risk.
//!
//! The offline three-arm sim: bin VALUES do not depend on the probability model
//! (only the bits spent do), so the recorded `(ctx, bin)` stream can be replayed
//! from each init table. For each slice segment, re-initialise the 460 contexts
//! from table 0/1/2, run the real FSM forward over the recorded bins, and score
//! with the same `p_zero` estimator the harvest used.
//!
//! ⚠ Estimated bits, not range-coded bits (`prometheus-bridge.md`'s binding
//! p_zero caveat). DELTAS are meaningful; a banked verdict needs the real-bits
//! confirm — encode with the chosen idc and `cmp` the slice bytes.
//!
//!   cargo run --release --example casc_a0 -- bench/harvest_out/cabac-bins.jsonl

use rusty_h264_common::cabac_tables::{CTX_INIT, STATE_TRANS};
use std::io::{BufRead, BufReader};

/// Spec §9.3.1.1 context init, mirroring the encoder's `init_ctx`.
fn init_ctx(qp: i32, init_idc: u32, is_i: bool) -> Vec<(u8, u8)> {
    let model = if is_i { 0 } else { ((init_idc + 1) as usize).min(3) };
    let q = qp.clamp(0, 51);
    (0..460)
        .map(|i| {
            let (m, n) = CTX_INIT[i][model];
            let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
            if pre <= 63 { ((63 - pre) as u8, 0u8) } else { ((pre - 64) as u8, 1u8) }
        })
        .collect()
}

/// The harvest's estimator: rangeTabLPS-derived P(bin==0), q8.
fn p_zero(state: u8, mps: u8) -> f64 {
    use rusty_h264_common::cabac_tables::RANGE_LPS;
    const MID: [f64; 4] = [288.0, 352.0, 416.0, 480.0];
    let s = state.min(62) as usize;
    let p_lps: f64 = (0..4).map(|q| RANGE_LPS[s][q] as f64 / MID[q]).sum::<f64>() / 4.0;
    let p = if mps == 0 { 1.0 - p_lps } else { p_lps };
    p.clamp(1.0 / 256.0, 255.0 / 256.0)
}

#[inline]
fn bits(p0: f64, bin: u8) -> f64 {
    let p = if bin == 0 { p0 } else { 1.0 - p0 };
    -p.log2()
}

/// One slice's replay under one init table.
fn score(bins: &[(u16, u8)], qp: i32, idc: u32) -> f64 {
    let mut ctx = init_ctx(qp, idc, false);
    let mut total = 0.0;
    for &(c, bin) in bins {
        let (state, mps) = ctx[c as usize];
        total += bits(p_zero(state, mps), bin);
        // Real FSM update (mirrors `encode_decision`).
        ctx[c as usize] = if bin != mps {
            let nm = if state == 0 { 1 - mps } else { mps };
            (STATE_TRANS[state as usize][0], nm)
        } else {
            (STATE_TRANS[state as usize][1], mps)
        };
    }
    total
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bench/harvest_out/cabac-bins.jsonl".into());
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut hdr = String::new();
    rd.read_line(&mut hdr).expect("header");

    // Accumulate per slice segment: (clip, frame) -> bins, plus qp/is_i.
    let mut seg: Option<(u32, u32, i32, bool)> = None;
    let mut bins: Vec<(u16, u8)> = Vec::new();
    let mut tot = [0f64; 3];
    let mut best_tot = 0f64;
    let (mut n_slices, mut n_pb, mut n_flip) = (0u64, 0u64, 0u64);
    let mut picks = [0u64; 3];

    let mut flush = |s: Option<(u32, u32, i32, bool)>, b: &mut Vec<(u16, u8)>| {
        let Some((_c, _f, qp, is_i)) = s else { b.clear(); return };
        if b.is_empty() { return }
        n_slices += 1;
        if is_i {
            b.clear();
            return; // I slices use the fixed table; no idc to choose.
        }
        n_pb += 1;
        let sc = [score(b, qp, 0), score(b, qp, 1), score(b, qp, 2)];
        for i in 0..3 { tot[i] += sc[i]; }
        let (bi, bv) = sc.iter().enumerate().fold((0usize, f64::MAX), |acc, (i, &v)| {
            if v < acc.1 { (i, v) } else { acc }
        });
        best_tot += bv;
        picks[bi] += 1;
        if bi != 0 { n_flip += 1; }
        b.clear();
    };

    for line in rd.lines() {
        let line = line.expect("read");
        if !line.starts_with('[') { continue }
        let mut it = line[1..].split(',');
        let clip: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let frame: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let ctx: u16 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v) => v, None => continue };
        let bin: u8 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v) => v, None => continue };
        let _pz: u32 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v) => v, None => continue };
        // features: [ctx,state,mps,qp,is_i]
        let feats: Vec<i64> = line[line.find('[').map(|i| i + 1).unwrap_or(0)..]
            .rsplit_once('[')
            .map(|(_, t)| t.trim_end_matches(&[']', ' '][..])
                .split(',')
                .filter_map(|v| v.trim().parse().ok())
                .collect())
            .unwrap_or_default();
        let (qp, is_i) = (feats.get(3).copied().unwrap_or(26) as i32,
                          feats.get(4).copied().unwrap_or(0) != 0);
        if seg != Some((clip, frame, qp, is_i)) {
            flush(seg, &mut bins);
            seg = Some((clip, frame, qp, is_i));
        }
        if ctx < 460 { bins.push((ctx, bin)); }
    }
    flush(seg, &mut bins);

    println!("A0 — cabac_init_idc per-slice dispatch (offline 3-arm FSM sim)\n");
    println!("  slices: {n_slices}  ({n_pb} P/B — I slices have no idc to choose)\n");
    if n_pb == 0 {
        println!("  NO P/B SLICES IN THIS HARVEST — A0 is unmeasurable here.");
        println!("  The harvest's clips must contain inter slices for this rung to mean");
        println!("  anything; re-harvest with a longer GOP before reading any verdict.");
        return;
    }
    for i in 0..3 {
        println!("  fixed idc={i}: {:>12.0} bits  {:>+7.3}% vs shipping (idc=0)",
                 tot[i], 100.0 * (tot[i] - tot[0]) / tot[0]);
    }
    println!("  PER-SLICE best: {:>12.0} bits  {:>+7.3}% vs shipping",
             best_tot, 100.0 * (best_tot - tot[0]) / tot[0]);
    println!("\n  per-slice picks: idc0 {}  idc1 {}  idc2 {}   ({n_flip} slices would change)",
             picks[0], picks[1], picks[2]);
    println!("\n  ⚠ estimated bits (p_zero), not range-coded bits. Confirm any banked");
    println!("    verdict by encoding with the chosen idc and comparing slice BYTES.");
}
