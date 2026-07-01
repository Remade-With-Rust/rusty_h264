//! Mutation fuzzer: the decoder must **never panic** on any input.
//!
//! A decoder eats attacker-controlled bytes, so a panic (let alone UB) is a
//! security bug. This test generates valid Baseline streams with our encoder,
//! corrupts them thousands of ways, and feeds those plus pure-random buffers to
//! `Decoder::decode` under `catch_unwind`. The only acceptable outcomes are
//! `Ok(_)` or `Err(DecodeError)` — never an unwind. A failure prints the exact
//! seed + mutation so the crash is reproducible.
//!
//! It is deterministic (fixed PRNG seed), dependency-free, and runs in CI. For
//! coverage-guided fuzzing the same entry point (`decode`) can be wrapped in a
//! `cargo-fuzz` target later; this catches the broad classes today.

use rusty_h264_decoder::Decoder;
use rusty_h264_encoder::{Encoder, EncoderConfig};
use rusty_h264_common::YuvFrame;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

static LAST_PANIC: Mutex<String> = Mutex::new(String::new());

/// SplitMix64 — a tiny deterministic PRNG (no external crates).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn upto(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// Builds a handful of valid seed streams exercising distinct decode paths:
/// all-intra at several sizes/QPs, and an I+P (inter) sequence.
fn seed_streams() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();

    // All-intra single frames, varied sizes (aligned + non-multiple-of-16) and QP.
    for &(w, h) in &[(16usize, 16usize), (48, 32), (80, 48), (34, 26)] {
        for &qp in &[10u8, 26, 45] {
            let mut cfg = EncoderConfig::new(w, h);
            cfg.qp = qp;
            cfg.gop_size = 1;
            if let Ok(mut enc) = Encoder::new(cfg) {
                seeds.push(enc.encode(&textured_frame(w, h, qp as u64)));
            }
        }
    }

    // An I+P sequence (exercises P-slice / motion-compensation decode paths).
    let (w, h) = (48, 48);
    let mut cfg = EncoderConfig::new(w, h);
    cfg.qp = 26;
    cfg.gop_size = 30;
    if let Ok(mut enc) = Encoder::new(cfg) {
        let mut stream = Vec::new();
        for f in 0..6u64 {
            stream.extend_from_slice(&enc.encode(&moving_frame(w, h, f)));
        }
        seeds.push(stream);
    }

    // CABAC seeds. Our encoder emits CAVLC only, so the entire CABAC decode path
    // (arithmetic engine, I_4x4 / I_16x16 intra, P/B motion + residual, direct) would
    // otherwise be unfuzzed. These are tiny libx264 Main-profile clips committed as
    // fixtures; mutating them hunts panics in the CABAC branch specifically.
    seeds.push(include_bytes!("../../../tests/fuzz_seeds/cabac_i4x4.264").to_vec());
    seeds.push(include_bytes!("../../../tests/fuzz_seeds/cabac_i16.264").to_vec());
    seeds.push(include_bytes!("../../../tests/fuzz_seeds/cabac_p.264").to_vec());
    seeds.push(include_bytes!("../../../tests/fuzz_seeds/cabac_b.264").to_vec());

    seeds
}

/// A deterministic textured frame (gradients + a block) so residuals are non-trivial.
fn textured_frame(w: usize, h: usize, salt: u64) -> YuvFrame {
    let mut f = YuvFrame::black(w, h);
    for y in 0..h {
        for x in 0..w {
            f.y[y * w + x] = ((x * 3 + y * 5 + salt as usize * 7) & 0xff) as u8;
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    for y in 0..ch {
        for x in 0..cw {
            f.u[y * cw + x] = ((x * 2 + salt as usize) & 0xff) as u8;
            f.v[y * cw + x] = ((y * 2 + salt as usize) & 0xff) as u8;
        }
    }
    f
}

/// A frame with a moving bright box, for inter prediction.
fn moving_frame(w: usize, h: usize, t: u64) -> YuvFrame {
    let mut f = textured_frame(w, h, 0);
    let (bx, by) = ((t as usize * 3) % w.max(1), (t as usize * 2) % h.max(1));
    for y in by..(by + 8).min(h) {
        for x in bx..(bx + 8).min(w) {
            f.y[y * w + x] = 240;
        }
    }
    f
}

/// Applies a few random byte-level mutations (flip / set / truncate / extend).
fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut out = base.to_vec();
    let edits = 1 + rng.upto(8);
    for _ in 0..edits {
        if out.is_empty() {
            break;
        }
        match rng.upto(5) {
            0 => {
                let i = rng.upto(out.len());
                out[i] ^= 1 << rng.upto(8);
            }
            1 => {
                let i = rng.upto(out.len());
                out[i] = rng.next() as u8;
            }
            2 => {
                // Truncate at a random point (length-driven OOB hunting).
                let i = rng.upto(out.len());
                out.truncate(i);
            }
            3 => {
                // Inject a start code at a random offset.
                let i = rng.upto(out.len());
                for (k, b) in [0u8, 0, 0, 1].into_iter().enumerate() {
                    if i + k < out.len() {
                        out[i + k] = b;
                    }
                }
            }
            _ => {
                let i = rng.upto(out.len());
                out.insert(i, rng.next() as u8);
            }
        }
    }
    out
}

fn decodes_without_panic(bytes: &[u8]) -> Result<(), String> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut dec = Decoder::new();
        let _ = dec.decode(bytes); // Ok or Err both fine; only a panic is a bug.
    }));
    r.map_err(|_| LAST_PANIC.lock().unwrap().clone())
}

#[test]
fn decoder_never_panics_on_mutated_streams() {
    let seeds = seed_streams();
    assert!(!seeds.is_empty(), "encoder produced no seed streams");

    // Capture each panic's site instead of spamming the default hook; the
    // assertion below reports the reproducing case + location.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        *LAST_PANIC.lock().unwrap() = format!("{loc}  {msg}");
    }));

    let mut rng = Rng(0xC0FF_EE12_3456_789A);
    let iters_per_seed = 4000;
    // Distinct panic site -> (count, one reproducing input as hex).
    let mut sites: std::collections::BTreeMap<String, (usize, String)> = std::collections::BTreeMap::new();
    let mut record = |site: String, bytes: &[u8]| {
        let e = sites.entry(site).or_insert_with(|| (0, hex(bytes)));
        e.0 += 1;
    };

    for seed in seeds.iter() {
        for _ in 0..iters_per_seed {
            let m = mutate(&mut rng, seed);
            if let Err(site) = decodes_without_panic(&m) {
                record(site, &m);
            }
        }
    }

    // Pure-random buffers of assorted lengths (shallow NAL-parser coverage).
    for len in [0usize, 1, 4, 5, 8, 16, 64, 256, 1024] {
        for _ in 0..500 {
            let buf: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if let Err(site) = decodes_without_panic(&buf) {
                record(site, &buf);
            }
        }
    }

    std::panic::set_hook(prev);
    let report: Vec<String> = sites
        .iter()
        .map(|(site, (n, ex))| format!("[{n}x] {site}\n   repro: {ex}"))
        .collect();
    assert!(
        sites.is_empty(),
        "decoder panicked at {} distinct site(s):\n{}",
        sites.len(),
        report.join("\n")
    );
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}
