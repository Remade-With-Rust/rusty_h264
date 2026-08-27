//! CASC ceiling AT THE CONSTRAINT — how much of the KT oracle's win survives a
//! decoder-mirrorable adapter?
//!
//! `prom entropy replay` reports the unbounded causal-KT oracle: per-context
//! counters of arbitrary size and precision, **−3.82%** against the shipping
//! FSM on the 4.69M-bin harvest. That is the ceiling for an IDEAL adapter, and
//! it is not the deliverable. CABAC's adaptation is **6 bits of state driving a
//! fixed transition table that the DECODER must mirror exactly**; any A2
//! replacement has to fit a comparable budget and be bit-exactly reproducible.
//!
//! So: sweep the state budget. KT with counts windowed to `C` total
//! observations (halve both on overflow) is the standard bounded-counter form a
//! mirrorable design would actually use. If the win collapses as C shrinks
//! toward the FSM's budget, A2 is not worth a bitstream-changing campaign and
//! A1 (init tables, encoder-side) is the whole prize.
//!
//! Reads the harvest written by `cabac_harvest.rs` — the JSONL IS the coupling,
//! so this needs no dependency on the Prometheus workspace.
//!
//!   cargo run --release --example casc_ceiling -- bench/harvest_out/cabac-bins.jsonl

use std::io::{BufRead, BufReader};

/// Bits to code `bin` under P(bin==0) = `p0`, clamped away from 0/1.
// CLAMPED estimated bits (finite ceiling under a degenerate estimator) — the
// deliberately-different twin of casc_a0/a1's unclamped `bits`; both now live
// named in `metrics`, with the divergence pinned by test.
use rusty_h264_bench::metrics::bin_bits_clamped as bits;

/// Causal KT with a bounded observation window: once a context has seen `cap`
/// bins, halve both counts. `cap = usize::MAX` is the unbounded oracle.
struct Kt {
    n: Vec<[u32; 2]>,
    cap: u32,
}
impl Kt {
    fn new(cap: u32) -> Self {
        Self { n: vec![[0, 0]; 1024], cap }
    }
    #[inline]
    fn p0(&self, ctx: usize) -> f64 {
        let [a, b] = self.n[ctx];
        (a as f64 + 0.5) / (a as f64 + b as f64 + 1.0)
    }
    #[inline]
    fn update(&mut self, ctx: usize, bin: u8) {
        let c = &mut self.n[ctx];
        c[bin as usize] += 1;
        if c[0] + c[1] >= self.cap {
            c[0] /= 2;
            c[1] /= 2;
        }
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bench/harvest_out/cabac-bins.jsonl".into());
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut rd = BufReader::with_capacity(1 << 20, f);

    let mut header = String::new();
    rd.read_line(&mut header).expect("header");
    println!("harvest: {}", header.trim());

    // The FSM's own budget is 6 bits of state; sweep around and past it so the
    // shape of the degradation is visible, not just one point.
    let caps: Vec<u32> = vec![16, 32, 64, 128, 256, 1024, u32::MAX];
    let mut models: Vec<Kt> = caps.iter().map(|&c| Kt::new(c)).collect();
    let mut model_bits = vec![0f64; caps.len()];
    let mut inc_bits = 0f64;
    let mut n = 0u64;

    // SLICE RESET. CABAC re-initialises every context at each slice start, so a
    // model that accumulates ACROSS slices is not the same experiment — it
    // averages incompatible content. Omitting this made the UNBOUNDED oracle
    // score +7.19% (worse than incumbent, and worse than a 64-bin window of
    // itself), which is impossible and is what exposed the bug: the small
    // windows were only winning because they FORGET across clip boundaries.
    let mut cur_seg = (u32::MAX, u32::MAX);
    for line in rd.lines() {
        let line = line.expect("read");
        let b = line.as_bytes();
        if b.first() != Some(&b'[') {
            continue;
        }
        // [clip, frame, ctx_idx, bin, p_zero, [...]] — take the first five ints.
        let mut it = line[1..].split(',');
        let clip: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let frame: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        if (clip, frame) != cur_seg {
            cur_seg = (clip, frame);
            for m in models.iter_mut() {
                m.n.iter_mut().for_each(|c| *c = [0, 0]);
            }
        }
        let ctx: usize = match it.next().and_then(|v| v.trim().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let bin: u8 = match it.next().and_then(|v| v.trim().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let pz: u32 = match it.next().and_then(|v| v.trim().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        if ctx >= 1024 {
            continue;
        }
        inc_bits += bits(pz as f64 / 256.0, bin);
        for (i, m) in models.iter_mut().enumerate() {
            model_bits[i] += bits(m.p0(ctx), bin);
            m.update(ctx, bin);
        }
        n += 1;
    }

    println!("\n{n} bins");
    println!("  incumbent (recorded FSM p_zero): {:>12.0} bits", inc_bits);
    println!("\n  KT window   bits          vs incumbent   note");
    for (i, &c) in caps.iter().enumerate() {
        let d = 100.0 * (model_bits[i] - inc_bits) / inc_bits;
        let label = if c == u32::MAX {
            "unbounded (what `prom entropy replay` reports)".to_string()
        } else {
            let st = (c as f64).log2();
            format!("~{st:.0} bits of counter state per context")
        };
        println!("  {:>9} {:>12.0}  {:>+9.2}%    {label}",
                 if c == u32::MAX { "inf".into() } else { c.to_string() }, model_bits[i], d);
    }
    println!(
        "\nA2 is only worth a bitstream-changing campaign if the win SURVIVES at a\n\
         budget a decoder can mirror. Compare the small-window rows against the\n\
         unbounded one — that gap is the part of -3.82% that is NOT deliverable."
    );
}
