//! Gate-regression harness — Great Gate P4 (docs/great-gate.md, docs/gate-ledger.md).
//!
//! Every shipped gate is a CLAIM: "on this content, routing this way is better,
//! and here is what it costs." A claim nothing re-checks decays silently, and
//! it decays fastest when something UPSTREAM of the gate changes — which is not
//! hypothetical here: fixing AQ's grain handling flipped mb-tree's grain verdict
//! from -0.63% to +4.41% BD-SSIM, and only a re-run found it.
//!
//! Tiered so the cheap tiers can run constantly and the expensive one runs when
//! they say something moved. Counter-before-clock throughout: every tier below
//! Tier 3 is DETERMINISTIC — one run is the verdict, no pinning, no z-score.
//!
//!   Tier 0  escape hatches      seconds   every gate's neutral setting still
//!                                         reproduces the un-gated bytes
//!   Tier 1  fire-rate census    minutes   (fired, seen) per gate per clip vs
//!                                         the recorded baseline — the CANARY
//!   Tier 2  work counts         minutes   best_part / mb_plan per coded MB —
//!                                         the deterministic COST axis
//!   Tier 3  BD + pinned ms      hours     the quality/cost verdict itself;
//!                                         run when Tier 1 or 2 moves
//!                                         (bdrate.rs + bench/pinvs.ps1)
//!
//! Tier 3 deliberately lives OUTSIDE this binary: deterministic quantities and
//! timed ones must never share a loop (codec-measurement §13), and pinvs.ps1 is
//! the one compliant timing harness.
//!
//! Usage:
//!   gatecheck --baseline gates.json     # record the current state
//!   gatecheck --check    gates.json     # compare; non-zero exit on drift
//!
//! Clips come from RUSTY_GATECHECK_CLIPS (`name:WxH,...`), resolved under
//! RUSTY_GATECHECK_DIR. Keep the set SMALL and DISCRIMINATING — one clip per
//! gate-relevant content class is worth more than the whole corpus here,
//! because this tier is a canary, not a verdict.

use rusty_h264::{Encoder, EncoderConfig, Preset, YuvFrame};
use std::collections::BTreeMap;

fn load_clip(path: &str, w: usize, h: usize, max: usize) -> Vec<YuvFrame> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let fsz = ys + 2 * cs;
    (0..(raw.len() / fsz).min(max))
        .map(|i| {
            let b = &raw[i * fsz..];
            YuvFrame {
                width: w,
                height: h,
                y: b[..ys].to_vec(),
                u: b[ys..ys + cs].to_vec(),
                v: b[ys + cs..ys + 2 * cs].to_vec(),
            }
        })
        .collect()
}

/// FNV-1a of the whole coded stream — the identity the escape-hatch tier asserts.
fn hash_stream(aus: &[Vec<u8>]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for au in aus {
        for &b in au {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// The shipped configuration under test, plus the gate knobs a tier flips.
fn cfg_for(w: usize, h: usize) -> EncoderConfig {
    let mut c = EncoderConfig::new(w, h);
    c.qp = 27;
    c.gop_size = 30;
    c.preset = Preset::Quality;
    c
}

fn encode(cfg: EncoderConfig, frames: &[YuvFrame]) -> (u64, usize) {
    let enc = Encoder::new(cfg).expect("cfg");
    let aus = enc.encode_all(frames).expect("encode");
    (hash_stream(&aus), aus.iter().map(Vec::len).sum())
}

/// One clip's full record: stream identity, every gate's (fired, seen), and the
/// deterministic work counts.
fn measure(name: &str, path: &str, w: usize, h: usize) -> BTreeMap<String, String> {
    let frames = load_clip(path, w, h, 30);
    let mut rec = BTreeMap::new();

    // Tier 1 + 2 are captured PER ARM: a counter is only meaningful next to the
    // configuration that produced it, and the sub-8x8 gates are consulted only
    // in the arm that enables the split search (the first cut of this harness
    // snapshotted once, before that arm ran, and dutifully recorded 0/0 for
    // three gates — an instrument reporting silence as data).
    let mut snap = |rec: &mut BTreeMap<String, String>, tag: &str| {
        for (n, (f, s)) in rusty_h264::gate_census_names().iter().zip(rusty_h264::gate_census()) {
            rec.insert(format!("gate.{tag}{n}"), format!("{f}/{s}"));
        }
        for (n, v) in rusty_h264::gate_work_names().iter().zip(rusty_h264::gate_work()) {
            rec.insert(format!("work.{tag}{n}"), v.to_string());
        }
    };

    rusty_h264::gate_census_reset();
    let (hash, bytes) = encode(cfg_for(w, h), &frames);
    rec.insert("hash".into(), format!("{hash:016x}"));
    rec.insert("bytes".into(), bytes.to_string());
    snap(&mut rec, "");

    // Tier 0: every escape hatch still reproduces the bytes it promises. Each
    // gate's ledger entry claims a neutral setting that is BYTE-IDENTICAL to the
    // pre-gate encoder; that promise is what makes a gate bisectable, and it is
    // exactly the sort of thing a refactor breaks silently.
    let mut off = cfg_for(w, h);
    off.aq_strength = 0.0;
    off.mbtree = false;
    let (h_off, _) = encode(off, &frames);
    rec.insert("hash.aq0_mbtree0".into(), format!("{h_off:016x}"));

    let mut sub8 = cfg_for(w, h);
    sub8.tune_sub8x8_split = true;
    sub8.tune_sub8_rd = true;
    rusty_h264::gate_census_reset();
    let (h_s8, b_s8) = encode(sub8, &frames);
    rec.insert("hash.sub8_rd".into(), format!("{h_s8:016x}"));
    rec.insert("bytes.sub8_rd".into(), b_s8.to_string());
    // The sub-8x8 arm's own census + cost: this is where `sub8_split`,
    // `sub8_grain` and `sub8_rd_revert` actually fire, and where the feature's
    // deterministic COST (best_part / mb_plan per coded MB) is visible.
    snap(&mut rec, "sub8.");

    // The intra-vs-inter RD probe's own census + cost (P3 RD-pricing #2):
    // `intra_rd_flip` is how often RD overturns the SATD+penalty pick.
    let mut ird = cfg_for(w, h);
    ird.tune_intra_rd = true;
    rusty_h264::gate_census_reset();
    let (h_ir, b_ir) = encode(ird, &frames);
    rec.insert("hash.intra_rd".into(), format!("{h_ir:016x}"));
    rec.insert("bytes.intra_rd".into(), b_ir.to_string());
    snap(&mut rec, "intrard.");

    eprintln!("  {name}: {} keys", rec.len());
    rec
}

fn clips() -> Vec<(String, String, usize, usize)> {
    let dir = std::env::var("RUSTY_GATECHECK_DIR").unwrap_or_else(|_| ".".into());
    let spec = std::env::var("RUSTY_GATECHECK_CLIPS").unwrap_or_else(|_| {
        // One per gate-relevant class: grain (all three grain gates), screen
        // (the sub-8x8 win extreme), busy-tex (the old sub-8x8 loser), motion,
        // static (mb-tree's biggest win).
        "grain_akiyo:352x288,screen_text:352x288,harbour_4cif:704x576,\
         foreman_cif:352x288,akiyo_cif:352x288"
            .into()
    });
    spec.split(',')
        .map(|e| {
            let e = e.trim();
            let (name, wh) = e.split_once(':').expect("clip spec name:WxH");
            let (w, h) = wh.split_once('x').expect("WxH");
            (
                name.to_string(),
                format!("{dir}/{name}.yuv"),
                w.parse().unwrap(),
                h.parse().unwrap(),
            )
        })
        .collect()
}

fn flatten(all: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let mut out = String::new();
    for (clip, rec) in all {
        for (k, v) in rec {
            out.push_str(&format!("{clip}\t{k}\t{v}\n"));
        }
    }
    out
}

/// REFUSE TO RUN STALE. A regression harness that silently tests an old binary
/// is worse than no harness: it certifies the very change it should catch.
/// Measured, not hypothetical — the first `--check` of this tool reported
/// "PASS: 185 tracked quantities unchanged" immediately after a gate was added,
/// because the CLI and bdrate had been rebuilt and gatecheck had not.
///
/// codec-measurement §10 says verify the binary is fresh; this makes it
/// automatic. Compare our own exe's mtime against the newest source under
/// `crates/` — if any source is newer, the answer this run would give is void.
fn assert_fresh() {
    let exe = match std::env::current_exe().and_then(|p| p.metadata()).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return, // cannot tell — do not block on a platform quirk
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // `target/` holds build artifacts, not sources.
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                    if newest.as_ref().is_none_or(|(bt, _)| t > *bt) {
                        newest = Some((t, p));
                    }
                }
            }
        }
    }
    if let Some((t, path)) = newest {
        if t > exe {
            eprintln!("STALE HARNESS — refusing to report.");
            eprintln!("  {} is newer than this binary.", path.display());
            eprintln!("  Rebuild: cargo build --release --example gatecheck");
            eprintln!("  (A --check run now would certify code that is not the code under test.)");
            std::process::exit(3);
        }
    }
}

fn main() {
    assert_fresh();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, path) = match args.as_slice() {
        [m, p] if m == "--baseline" || m == "--check" => (m.clone(), p.clone()),
        _ => {
            eprintln!("usage: gatecheck --baseline|--check <file>");
            std::process::exit(2);
        }
    };

    let mut all = BTreeMap::new();
    for (name, path, w, h) in clips() {
        all.insert(name.clone(), measure(&name, &path, w, h));
    }
    let text = flatten(&all);

    if mode == "--baseline" {
        std::fs::write(&path, &text).expect("write baseline");
        println!("baseline written: {path} ({} lines)", text.lines().count());
        return;
    }

    let old = std::fs::read_to_string(&path).expect("read baseline");
    let parse = |t: &str| -> BTreeMap<(String, String), String> {
        t.lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some(((it.next()?.into(), it.next()?.into()), it.next()?.into()))
            })
            .collect()
    };
    let (o, n) = (parse(&old), parse(&text));
    let mut drift = 0usize;
    for (k, ov) in &o {
        match n.get(k) {
            Some(nv) if nv == ov => {}
            Some(nv) => {
                println!("DRIFT {} {}: {} -> {}", k.0, k.1, ov, nv);
                drift += 1;
            }
            None => {
                println!("MISSING {} {}", k.0, k.1);
                drift += 1;
            }
        }
    }
    for k in n.keys() {
        if !o.contains_key(k) {
            println!("NEW {} {}", k.0, k.1);
        }
    }
    println!("---");
    if drift == 0 {
        println!("PASS: {} tracked quantities unchanged", o.len());
    } else {
        // Deliberately not a verdict. A moved count is the canary: it says the
        // gate's world changed, so its BD/ms claim (Tier 3) must be re-earned.
        println!(
            "DRIFT: {drift} of {} quantities moved — re-run Tier 3 (bdrate + pinvs.ps1) \
             for the affected gates and update their ledger entries.",
            o.len()
        );
        std::process::exit(1);
    }
}
