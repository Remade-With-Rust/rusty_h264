//! **A1 — init-table laws: the CEILING first.**
//!
//! A1 proposes replacing CABAC's `state0 = clamp((m·QP)>>4 + n)` linear init
//! with a discovered law. Before distilling anything, bound what ANY init can
//! win — the init only governs the bins a context codes BEFORE the FSM adapts,
//! so the prize is set by amortization, not by how clever the law is.
//!
//! Three arms per slice, all replaying the real FSM over the recorded stream:
//!   * **shipping** — the spec init from `CTX_INIT` (what we ship).
//!   * **ORACLE init** — each context starts at the state closest to its OWN
//!     empirical rate in this slice. Non-causal (it reads the future), so it is
//!     an upper bound no real law can reach, only approach.
//!   * **worst-case init** — every context starts at state 0 (p≈½), the "no
//!     information" floor. Shipping-vs-this is what the EXISTING tables buy.
//!
//! Also reports the bins-per-context distribution, because that is the physical
//! quantity that decides A1: a context coding hundreds of bins amortizes any
//! init error to nothing, while short slices with many contexts do not.
//!
//!   cargo run --release --example casc_a1 -- bench/harvest_out2/cabac-bins.jsonl

use rusty_h264_common::cabac_tables::{CTX_INIT, RANGE_LPS, STATE_TRANS};
use std::io::{BufRead, BufReader};

const NCTX: usize = 460;

fn p_zero(state: u8, mps: u8) -> f64 {
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

fn spec_init(qp: i32, init_idc: u32, is_i: bool) -> Vec<(u8, u8)> {
    let model = if is_i { 0 } else { ((init_idc + 1) as usize).min(3) };
    let q = qp.clamp(0, 51);
    (0..NCTX)
        .map(|i| {
            let (m, n) = CTX_INIT[i][model];
            let pre = (((m as i32 * q) >> 4) + n as i32).clamp(1, 126);
            if pre <= 63 { ((63 - pre) as u8, 0u8) } else { ((pre - 64) as u8, 1u8) }
        })
        .collect()
}

/// The (state, mps) whose modelled P(bin==0) is closest to `target`.
fn nearest_state(target: f64) -> (u8, u8) {
    let mut best = (0u8, 0u8);
    let mut bd = f64::MAX;
    for mps in 0..2u8 {
        for st in 0..63u8 {
            let d = (p_zero(st, mps) - target).abs();
            if d < bd { bd = d; best = (st, mps); }
        }
    }
    best
}

fn replay(bins: &[(u16, u8)], init: &[(u8, u8)]) -> f64 {
    let mut ctx = init.to_vec();
    let mut total = 0.0;
    for &(c, bin) in bins {
        let (state, mps) = ctx[c as usize];
        total += bits(p_zero(state, mps), bin);
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
    let path = std::env::args().nth(1)
        .unwrap_or_else(|| "bench/harvest_out2/cabac-bins.jsonl".into());
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut hdr = String::new();
    rd.read_line(&mut hdr).expect("header");

    let mut seg: Option<(u32, u32, i32, bool)> = None;
    let mut bins: Vec<(u16, u8)> = Vec::new();
    let (mut t_ship, mut t_oracle, mut t_zero) = (0f64, 0f64, 0f64);
    let mut n_slices = 0u64;
    let mut per_ctx_counts: Vec<u64> = Vec::new();
    let mut all_slices: Vec<(Vec<(u16, u8)>, i32, bool)> = Vec::new();

    let mut flush = |s: Option<(u32, u32, i32, bool)>, b: &mut Vec<(u16, u8)>| {
        let Some((_c, _f, qp, is_i)) = s else { b.clear(); return };
        if b.is_empty() { return }
        n_slices += 1;
        // Per-context empirical rate in THIS slice -> the oracle init.
        let mut cnt = vec![[0u64; 2]; NCTX];
        for &(c, bin) in b.iter() { cnt[c as usize][bin as usize] += 1; }
        let ship = spec_init(qp, 0, is_i);
        let oracle: Vec<(u8, u8)> = (0..NCTX)
            .map(|i| {
                let [z, o] = cnt[i];
                if z + o == 0 { ship[i] } else { nearest_state(z as f64 / (z + o) as f64) }
            })
            .collect();
        let zero = vec![(0u8, 0u8); NCTX];
        t_ship += replay(b, &ship);
        t_oracle += replay(b, &oracle);
        t_zero += replay(b, &zero);
        for i in 0..NCTX {
            let n = cnt[i][0] + cnt[i][1];
            if n > 0 { per_ctx_counts.push(n); }
        }
        all_slices.push((b.clone(), qp, is_i));
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
        let feats: Vec<i64> = line.rsplit_once('[')
            .map(|(_, t)| t.trim_end_matches(&[']', ' '][..]).split(',')
                .filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default();
        let (qp, is_i) = (feats.get(3).copied().unwrap_or(26) as i32,
                          feats.get(4).copied().unwrap_or(0) != 0);
        if seg != Some((clip, frame, qp, is_i)) {
            flush(seg, &mut bins);
            seg = Some((clip, frame, qp, is_i));
        }
        if (ctx as usize) < NCTX { bins.push((ctx, bin)); }
    }
    flush(seg, &mut bins);

    per_ctx_counts.sort_unstable();
    let med = per_ctx_counts.get(per_ctx_counts.len() / 2).copied().unwrap_or(0);
    let p10 = per_ctx_counts.get(per_ctx_counts.len() / 10).copied().unwrap_or(0);

    println!("A1 — init-table law CEILING ({n_slices} slices)\n");
    println!("  worst-case init (all state 0): {:>12.0} bits  {:>+7.3}%",
             t_zero, 100.0 * (t_zero - t_ship) / t_ship);
    println!("  SHIPPING spec init:            {:>12.0} bits   +0.000%  (baseline)", t_ship);
    println!("  ORACLE init (non-causal):      {:>12.0} bits  {:>+7.3}%  <-- A1's CEILING",
             t_oracle, 100.0 * (t_oracle - t_ship) / t_ship);
    // SLICE-SIZE SWEEP. The init governs only PRE-ADAPTATION bins, so its value
    // scales with how few bins each context sees. These slices are large (one
    // per frame, ~212K bins); real streaming often uses many small slices,
    // amortizing the same init error over far less. Re-chunk the recorded
    // stream into synthetic sub-slices (re-init at each boundary) to find where
    // A1 stops being negligible — the prune's reopen condition, MEASURED.
    println!("
  slice-size sweep — does the ceiling grow for SMALL slices?");
    println!("  {:>12} {:>16} {:>14}", "bins/slice", "oracle vs ship", "ship vs zero");
    for &chunk in &[500usize, 2_000, 10_000, 50_000, usize::MAX] {
        let (mut cs, mut co, mut cz) = (0f64, 0f64, 0f64);
        let zero = vec![(0u8, 0u8); NCTX];
        for (sb, qp, is_i) in all_slices.iter() {
            let step = chunk.min(sb.len().max(1));
            for part in sb.chunks(step) {
                let mut cnt = vec![[0u64; 2]; NCTX];
                for &(c, bin) in part { cnt[c as usize][bin as usize] += 1; }
                let ship = spec_init(*qp, 0, *is_i);
                let oracle: Vec<(u8, u8)> = (0..NCTX).map(|i| {
                    let [z, o] = cnt[i];
                    if z + o == 0 { ship[i] } else { nearest_state(z as f64 / (z + o) as f64) }
                }).collect();
                cs += replay(part, &ship);
                co += replay(part, &oracle);
                cz += replay(part, &zero);
            }
        }
        let label = if chunk == usize::MAX { "whole slice".to_string() } else { chunk.to_string() };
        println!("  {:>12} {:>15.3}% {:>13.3}%", label,
                 100.0 * (co - cs) / cs, 100.0 * (cz - cs) / cs);
    }

    println!("\n  bins per (slice, context): median {med}, p10 {p10}, n={}", per_ctx_counts.len());
    println!("\n  The spec tables already buy {:.2}% over a no-information init; the ORACLE\n  \
              buys {:.2}% more. A1 can only ever capture part of that second number —\n  \
              a real law is causal and cannot read the slice it is initialising.",
             100.0 * (t_zero - t_ship) / t_ship, 100.0 * (t_ship - t_oracle) / t_ship);
}
