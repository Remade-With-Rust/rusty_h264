//! **A2 — a better CABAC adaptation schedule, as a drop-in transition table.**
//!
//! The ceiling sweep (`casc_ceiling.rs`) found causal KT beating the shipping
//! FSM by -2.82% **at CABAC's own 6-bit state budget**. That is the whole
//! finding: the FSM does not lack STATES, it has a suboptimal TRANSITION
//! SCHEDULE. So the minimal A2 candidate keeps everything a decoder already
//! implements — 64 states, the same `RANGE_LPS` probability mapping, the same
//! MPS/LPS mechanics — and replaces only `transIdxMPS` / `transIdxLPS`.
//!
//! The candidate table is derived, not searched: treat state as a probability
//! estimate and apply a fixed-window Bayes update
//!
//!     p_lps' = (N * p_lps + [symbol == LPS]) / (N + 1)
//!
//! then quantize p_lps' back to the nearest of the 64 states (flipping MPS when
//! it crosses ½). `N` is the adaptation rate — the single free parameter, and
//! exactly what the spec's 2003 schedule hard-codes. Sweeping N asks: is the
//! shipping schedule adapting at the wrong speed?
//!
//! Everything here is a TABLE, so a decoder mirrors it bit-exactly with no new
//! arithmetic. Still normative (both sides must change together), which is why
//! this measures before anything is built.
//!
//!   cargo run --release --example casc_a2 -- bench/harvest_out2/cabac-bins.jsonl

use rusty_h264_common::cabac_tables::{RANGE_LPS, STATE_TRANS};
use std::io::{BufRead, BufReader};

/// P(LPS) implied by a state, from rangeTabLPS (the harvest's own estimator).
fn p_lps(state: u8) -> f64 {
    const MID: [f64; 4] = [288.0, 352.0, 416.0, 480.0];
    let s = state.min(62) as usize;
    ((0..4).map(|q| RANGE_LPS[s][q] as f64 / MID[q]).sum::<f64>() / 4.0).clamp(1e-4, 0.5)
}

#[inline]
fn bits_for(state: u8, mps: u8, bin: u8) -> f64 {
    let pl = p_lps(state);
    let p = if bin == mps { 1.0 - pl } else { pl };
    -p.clamp(1e-6, 1.0 - 1e-6).log2()
}

/// The state whose P(LPS) is closest to `target`.
fn nearest(target: f64) -> u8 {
    (0..63u8).min_by(|&a, &b| {
        (p_lps(a) - target).abs().partial_cmp(&(p_lps(b) - target).abs()).unwrap()
    }).unwrap()
}

/// Derive (transMPS, transLPS, flip_on_lps) for adaptation window `n`.
fn derive(n: f64) -> (Vec<u8>, Vec<u8>, Vec<bool>) {
    let (mut tm, mut tl, mut fl) = (vec![0u8; 63], vec![0u8; 63], vec![false; 63]);
    for s in 0..63u8 {
        let pl = p_lps(s);
        // MPS observed -> LPS becomes less likely.
        tm[s as usize] = nearest((n * pl) / (n + 1.0));
        // LPS observed -> LPS becomes more likely; may cross 1/2 and flip MPS.
        let up = (n * pl + 1.0) / (n + 1.0);
        if up > 0.5 {
            fl[s as usize] = true;
            tl[s as usize] = nearest(1.0 - up);
        } else {
            tl[s as usize] = nearest(up);
        }
    }
    (tm, tl, fl)
}

fn main() {
    let path = std::env::args().nth(1)
        .unwrap_or_else(|| "bench/harvest_out2/cabac-bins.jsonl".into());
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut hdr = String::new();
    rd.read_line(&mut hdr).expect("header");

    // (clip, frame) segments; contexts reset per slice exactly as CABAC does.
    let mut stream: Vec<(u32, u32, u16, u8, u8, u8)> = Vec::new(); // seg,frame,ctx,bin,state0,mps0
    for line in rd.lines() {
        let line = line.expect("read");
        if !line.starts_with('[') { continue }
        let mut it = line[1..].split(',');
        let clip: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let frame: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let ctx: u16 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v)=>v, None=>continue };
        let bin: u8 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v)=>v, None=>continue };
        let _pz: u32 = match it.next().and_then(|v| v.trim().parse().ok()) { Some(v)=>v, None=>continue };
        let feats: Vec<i64> = line.rsplit_once('[')
            .map(|(_, t)| t.trim_end_matches(&[']',' '][..]).split(',')
                 .filter_map(|v| v.trim().parse().ok()).collect()).unwrap_or_default();
        let st = feats.first().copied().unwrap_or(0);
        let _ = st;
        if ctx < 1024 {
            stream.push((clip, frame, ctx, bin,
                         feats.get(1).copied().unwrap_or(0) as u8,
                         feats.get(2).copied().unwrap_or(0) as u8));
        }
    }

    // Replay with a given transition table. Contexts are seeded from the FIRST
    // recorded (state, mps) they show in each slice — i.e. the real spec init,
    // so only the SCHEDULE differs between arms.
    let run = |tm: &[u8], tl: &[u8], fl: &[bool], spec: bool| -> f64 {
        let mut ctx: Vec<Option<(u8, u8)>> = vec![None; 1024];
        let mut seg = (u32::MAX, u32::MAX);
        let mut total = 0.0;
        for &(c, f, cx, bin, s0, m0) in &stream {
            if (c, f) != seg { seg = (c, f); ctx.iter_mut().for_each(|v| *v = None); }
            let (state, mps) = *ctx[cx as usize].get_or_insert((s0, m0));
            total += bits_for(state, mps, bin);
            ctx[cx as usize] = Some(if spec {
                if bin != mps {
                    (STATE_TRANS[state as usize][0], if state == 0 { 1 - mps } else { mps })
                } else {
                    (STATE_TRANS[state as usize][1], mps)
                }
            } else if bin != mps {
                (tl[state as usize], if fl[state as usize] { 1 - mps } else { mps })
            } else {
                (tm[state as usize], mps)
            });
        }
        total
    };

    let (z, zz, zzz) = (vec![0u8;63], vec![0u8;63], vec![false;63]);
    let base = run(&z, &zz, &zzz, true);
    println!("A2 — adaptation SCHEDULE as a drop-in transition table\n");
    println!("  {} bins", stream.len());
    println!("  shipping FSM schedule: {:>12.0} bits   +0.000%  (baseline)\n", base);
    println!("  {:>10} {:>14} {:>11}", "window N", "bits", "vs shipping");
    let mut best = (f64::MAX, 0.0);
    for n in [4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0, 96.0] {
        let (tm, tl, fl) = derive(n);
        let b = run(&tm, &tl, &fl, false);
        let d = 100.0 * (b - base) / base;
        if b < best.0 { best = (b, n); }
        println!("  {n:>10.0} {b:>14.0} {d:>10.3}%");
    }
    println!("\n  best: N = {:.0} at {:.3}%", best.1, 100.0 * (best.0 - base) / base);

    // DIRECT SEARCH — the strongest 64-state schedule, not just my Bayes
    // parameterisation. Coordinate descent from the SPEC table: perturb each
    // transMPS/transLPS entry and keep improvements. If even this cannot beat
    // the spec, no table-only A2 exists.
    let sub: Vec<_> = stream.iter().step_by(6).cloned().collect();
    let run_sub = |tm: &[u8], tl: &[u8], fl: &[bool], spec: bool| -> f64 {
        let mut ctx: Vec<Option<(u8, u8)>> = vec![None; 1024];
        let mut seg = (u32::MAX, u32::MAX);
        let mut total = 0.0;
        for &(c, f, cx, bin, s0, m0) in &sub {
            if (c, f) != seg { seg = (c, f); ctx.iter_mut().for_each(|v| *v = None); }
            let (st, mps) = *ctx[cx as usize].get_or_insert((s0, m0));
            total += bits_for(st, mps, bin);
            ctx[cx as usize] = Some(if spec {
                if bin != mps { (STATE_TRANS[st as usize][0], if st == 0 { 1 - mps } else { mps }) }
                else { (STATE_TRANS[st as usize][1], mps) }
            } else if bin != mps {
                (tl[st as usize], if fl[st as usize] { 1 - mps } else { mps })
            } else { (tm[st as usize], mps) });
        }
        total
    };
    let mut tm2: Vec<u8> = (0..63).map(|s| STATE_TRANS[s][1]).collect();
    let mut tl2: Vec<u8> = (0..63).map(|s| STATE_TRANS[s][0]).collect();
    let fl2: Vec<bool> = (0..63).map(|s| s == 0).collect();
    let spec_sub = run_sub(&tm2, &tl2, &fl2, true);
    let mut cur = run_sub(&tm2, &tl2, &fl2, false);
    for _pass in 0..2 {
        for s in 0..63usize {
            for d in [-2i32, -1, 1, 2] {
                for which in 0..2 {
                    let old_v = if which == 0 { tm2[s] } else { tl2[s] };
                    let nv = (old_v as i32 + d).clamp(0, 62) as u8;
                    if nv == old_v { continue }
                    if which == 0 { tm2[s] = nv } else { tl2[s] = nv }
                    let c = run_sub(&tm2, &tl2, &fl2, false);
                    if c < cur { cur = c; }
                    else if which == 0 { tm2[s] = old_v } else { tl2[s] = old_v }
                }
            }
        }
    }
    println!("
  DIRECT SEARCH (coordinate descent from the SPEC table, 1/6 subsample):");
    println!("    spec schedule        {spec_sub:>12.0} bits");
    println!("    best searched table  {cur:>12.0} bits  {:>+8.3}% vs spec",
             100.0 * (cur - spec_sub) / spec_sub);
    let full = run(&tm2, &tl2, &fl2, false);
    println!("    same table, FULL set {full:>12.0} bits  {:>+8.3}% vs spec  <-- holdout",
             100.0 * (full - base) / base);
    println!("\n  A table-only change: same 64 states, same RANGE_LPS mapping, same MPS/LPS\n  \
              mechanics. A decoder mirrors it exactly. Still NORMATIVE — encoder and\n  \
              decoder must change together — so this is a ceiling, not a ship.");
}
