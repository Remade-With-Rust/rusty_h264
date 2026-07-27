//! Merge `speed.tsv` + `stages.tsv` into the side-by-side report.
//!
//! The hard part is not tabulating — it is comparing two DIFFERENT stage
//! taxonomies honestly. Each encoder's profiler partitions its own encode loop,
//! and the partitions do not line up one-to-one (x264 codes entropy as a
//! top-level stage; ours nests it inside MB coding). So the report gives:
//!   1. each side's own top-level partition (structure, % of its own time), and
//!   2. a FUNCTIONAL comparison — only for work both sides genuinely measure —
//!      normalised to ms per megapixel so absolute speed is comparable.
//! Anything that exists on one side only (x264's hpel prefilter and lookahead,
//! our source copy) is listed but never silently folded into a ratio.

use crate::{Row, StageRow};
use std::collections::BTreeMap;
use std::path::Path;

fn read_rows(p: &Path) -> Vec<Row> {
    let Ok(txt) = std::fs::read_to_string(p) else { return Vec::new() };
    txt.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 16 {
                return None;
            }
            Some(Row {
                clip: f[0].into(), class: f[1].into(),
                width: f[2].parse().ok()?, height: f[3].parse().ok()?, frames: f[4].parse().ok()?,
                codec: f[5].into(), preset: f[6].into(), arm: f[7].into(), kind: f[8].into(),
                wall_ms: f[9].parse().ok()?, fps: f[10].parse().ok()?, mpx_s: f[11].parse().ok()?,
                bytes: f[12].parse().ok()?, kbps: f[13].parse().ok()?,
                psnr: f[14].parse().unwrap_or(f64::NAN), ssim: f[15].parse().unwrap_or(f64::NAN),
            })
        })
        .collect()
}

fn read_stages(p: &Path) -> Vec<StageRow> {
    let Ok(txt) = std::fs::read_to_string(p) else { return Vec::new() };
    txt.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 11 {
                return None;
            }
            Some(StageRow {
                clip: f[0].into(), codec: f[1].into(), preset: f[2].into(), arm: f[3].into(),
                kind: f[4].into(), stage: f[5].into(), nested_in: f[6].into(),
                ms: f[7].parse().ok()?, calls: f[8].parse().ok()?, pct: f[10].parse().ok()?,
            })
        })
        .collect()
}

/// Functional groups both encoders genuinely measure. `(label, our stages, x264 stages)`.
/// Kept deliberately narrow — a group only earns a ratio when both sides time the
/// same work.
const GROUPS: &[(&str, &[&str], &[&str])] = &[
    ("mode decision total", &["enc-me(best_part)", "enc-intra-cost", "enc-skip-check"], &["mb-analyse(decision)"]),
    ("  ↳ motion search",   &["enc-me(best_part)"],                                     &["me-search"]),
    ("  ↳ intra cost",      &["enc-intra-cost"],                                        &["intra-cost"]),
    ("MB coding (T/Q+recon)", &["enc-inter-code", "enc-intra-code"],                    &["mb-encode(T/Q+recon)"]),
    ("  ↳ motion comp",     &["inter-mc"],                                              &["inter-mc"]),
    ("entropy coding",      &["enc-cavlc-emit"],                   &["entropy-cabac", "entropy-cavlc"]),
    // x264's boundary-strength derivation lives in its MB-encode loop, not in
    // deblock_row, so it must be added back here or its deblocking looks cheaper
    // than it is — ours derives bS inside the filter pass and would be compared
    // against x264's filtering alone.
    ("deblocking",          &["deblock"],                          &["deblock", "deblock-strength"]),
];

/// Work that exists on one side only — reported, never ratioed.
const ASYMMETRIC: &[(&str, &str)] = &[
    ("lookahead/slicetype", "x264 only — frame-type/mb-tree lookahead (we default it off)"),
    ("hpel-filter",         "x264 only — precomputes half-pel planes once per frame; we interpolate on demand"),
    ("enc-source-copy",     "ours only — clamped copy of source planes to the MB-aligned grid"),
    ("enc-finalize",        "ours only — per-frame deblock-info build + reference handoff"),
];

fn mpx_of(rows: &[Row], clip: &str) -> f64 {
    rows.iter()
        .find(|r| r.clip == clip)
        .map(|r| (r.width * r.height * r.frames) as f64 / 1e6)
        .unwrap_or(0.0)
}

/// Sum ms for a set of stage names over the whole corpus, and the megapixels covered.
fn sum_ms(st: &[StageRow], speed: &[Row], pred: impl Fn(&StageRow) -> bool, names: &[&str]) -> (f64, f64) {
    let mut ms = 0.0;
    let mut clips: BTreeMap<&str, ()> = BTreeMap::new();
    for r in st.iter().filter(|r| pred(r)) {
        if names.contains(&r.stage.as_str()) {
            ms += r.ms;
            clips.insert(r.clip.as_str(), ());
        }
    }
    let mpx: f64 = clips.keys().map(|c| mpx_of(speed, c)).sum();
    (ms, mpx)
}

pub fn generate(dir: &Path) {
    let speed = read_rows(&dir.join("speed.tsv"));
    let stages = read_stages(&dir.join("stages.tsv"));
    let mut o = String::new();

    o.push_str("# rusty_h264 vs x264 — function-level speed analysis\n\n");
    if speed.is_empty() && stages.is_empty() {
        o.push_str("_No results yet. Run `bash video-tests/run_analysis.sh`._\n");
        std::fs::write(dir.join("REPORT.md"), o).ok();
        return;
    }

    // ---- method ------------------------------------------------------------
    o.push_str("## Method\n\n");
    o.push_str("* Corpus: `video-tests/clips` — real source video, fixed frame counts, byte-identical every run (`manifest.tsv` pins sizes + hashes).\n");
    o.push_str("* Both encoders: **single-threaded**, matched **QP 26**, matched **keyint 60**, best-of-N wall clock.\n");
    o.push_str("* x264 is built from source in `_ref_x264` (8-bit, 4:2:0, asm on, `checkasm` green). Two binaries: a **stock** one for throughput and an **rdtsc-instrumented** twin for the breakdown, so neither side pays for the other's instrumentation.\n");
    o.push_str("* x264 arms: `high` = stock x264 defaults (CABAC + B-frames + 8×8 + weighted pred); `baseline` = clamped to the toolset we implement by default. The `baseline` arm is the implementation-vs-implementation comparison; `high` is the real-world bar.\n");
    o.push_str("* Quality (PSNR/SSIM) is measured by the **same external ffmpeg** for both, so neither is scored by its own reconstruction.\n\n");

    // ---- encoder throughput ------------------------------------------------
    let enc: Vec<&Row> = speed.iter().filter(|r| r.kind == "encode").collect();
    if !enc.is_empty() {
        o.push_str("## 1. Encoder throughput — where our presets sit on x264's ladder\n\n");
        o.push_str("Mpx/s, higher is better. `ratio` is x264's throughput ÷ ours at the same row.\n\n");
        o.push_str("| clip | class | ours/fast | ours/quality | x264 ultrafast | x264 veryfast | x264 medium | x264 veryslow |\n");
        o.push_str("|---|---|---:|---:|---:|---:|---:|---:|\n");
        let clips: Vec<&str> = {
            let mut v: Vec<&str> = enc.iter().map(|r| r.clip.as_str()).collect();
            v.dedup();
            let mut seen = BTreeMap::new();
            v.retain(|c| seen.insert(*c, ()).is_none());
            v
        };
        let get = |clip: &str, codec: &str, preset: &str, arm: &str| -> Option<f64> {
            enc.iter()
                .find(|r| r.clip == clip && r.codec == codec && r.preset == preset && (arm.is_empty() || r.arm == arm))
                .map(|r| r.mpx_s)
        };
        for c in &clips {
            let class = enc.iter().find(|r| r.clip == *c).map(|r| r.class.clone()).unwrap_or_default();
            let f = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into());
            o.push_str(&format!(
                "| {c} | {class} | {} | {} | {} | {} | {} | {} |\n",
                f(get(c, "ours", "fast", "")),
                f(get(c, "ours", "quality", "")),
                f(get(c, "x264", "ultrafast", "high")),
                f(get(c, "x264", "veryfast", "high")),
                f(get(c, "x264", "medium", "high")),
                f(get(c, "x264", "veryslow", "high")),
            ));
        }
        o.push('\n');

        // preset-equivalence: which x264 preset is closest in speed to ours/fast
        o.push_str("### Speed-equivalent x264 preset\n\n");
        o.push_str("The x264 preset whose throughput is nearest ours, per clip — i.e. what our encoder *costs* in x264 terms.\n\n");
        o.push_str("| clip | ours/fast Mpx/s | nearest x264 preset (high) | its Mpx/s | our size vs its size |\n|---|---:|---|---:|---:|\n");
        for c in &clips {
            let Some(ours) = enc.iter().find(|r| r.clip == *c && r.codec == "ours" && r.preset == "fast") else { continue };
            let mut best: Option<(&Row, f64)> = None;
            for r in enc.iter().filter(|r| r.clip == *c && r.codec == "x264" && r.arm == "high") {
                let d = (r.mpx_s - ours.mpx_s).abs();
                if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((r, d));
                }
            }
            if let Some((r, _)) = best {
                o.push_str(&format!(
                    "| {c} | {:.2} | {} | {:.2} | {:.2}× |\n",
                    ours.mpx_s, r.preset, r.mpx_s,
                    if r.bytes > 0 { ours.bytes as f64 / r.bytes as f64 } else { f64::NAN }
                ));
            }
        }
        o.push('\n');
    }

    // ---- top-level partitions ---------------------------------------------
    if !stages.is_empty() {
        o.push_str("## 2. Top-level stage partition (each side's own encode loop)\n\n");
        for (codec, preset, arm, title) in [
            ("ours", "fast", "-", "rusty_h264 — preset fast (default)"),
            ("ours", "quality", "-", "rusty_h264 — preset quality"),
            ("x264", "medium", "high", "x264 — preset medium (default), High profile"),
            ("x264", "medium", "baseline", "x264 — preset medium, Baseline profile"),
        ] {
            let sel: Vec<&StageRow> = stages
                .iter()
                .filter(|r| r.kind == "encode" && r.codec == codec && r.preset == preset && (arm == "-" || r.arm == arm) && r.nested_in == "-")
                .collect();
            if sel.is_empty() {
                continue;
            }
            let mut agg: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
            for r in &sel {
                let e = agg.entry(r.stage.as_str()).or_insert((0.0, 0));
                e.0 += r.ms;
                e.1 += r.calls;
            }
            let total = agg.get("TOTAL").map(|(m, _)| *m).unwrap_or(1.0).max(1e-9);
            o.push_str(&format!("**{title}** (corpus total {total:.0} ms)\n\n"));
            o.push_str("| stage | ms | % | calls | ns/call |\n|---|---:|---:|---:|---:|\n");
            let mut v: Vec<_> = agg.iter().filter(|(k, _)| **k != "TOTAL").collect();
            v.sort_by(|a, b| b.1 .0.partial_cmp(&a.1 .0).unwrap());
            for (k, (ms, calls)) in v {
                let nspc = if *calls > 0 { ms * 1e6 / *calls as f64 } else { 0.0 };
                o.push_str(&format!("| {k} | {ms:.0} | {:.1}% | {calls} | {nspc:.0} |\n", 100.0 * ms / total));
            }
            o.push('\n');
        }

        // ---- the headline: function-level comparison ------------------------
        o.push_str("## 3. Function-level comparison — where we are slower\n\n");
        o.push_str("Normalised to **ms per megapixel of source** so the numbers are directly comparable across clips and resolutions. `ratio` = ours ÷ x264: **>1 means we are slower** at that function.\n\n");
        o.push_str("Each of our presets is compared across a span of the x264 ladder — a fast rung (roughly speed-matched), x264's default, and a slow rung — because a function that looks competitive against `placebo` may not be against `ultrafast`.\n\n");
        for (our_preset, x_presets) in [
            ("fast", ["ultrafast", "veryfast", "medium"]),
            ("quality", ["veryfast", "medium", "slow"]),
        ] {
        for (arm, label) in [("baseline", "vs x264 Baseline (matched toolset)"), ("high", "vs x264 default High profile")] {
            for xp in x_presets {
                o.push_str(&format!("### ours/{our_preset} {label}, x264 preset {xp}\n\n"));
                o.push_str("| function | ours ms/Mpx | x264 ms/Mpx | ratio | verdict |\n|---|---:|---:|---:|---|\n");
                let mut worst: Vec<(String, f64, f64, f64)> = Vec::new();
                for (name, on, xn) in GROUPS {
                    let (oms, ompx) = sum_ms(&stages, &speed, |r| r.kind == "encode" && r.codec == "ours" && r.preset == our_preset, on);
                    let (xms, xmpx) = sum_ms(&stages, &speed, |r| r.kind == "encode" && r.codec == "x264" && r.preset == xp && r.arm == arm, xn);
                    if ompx <= 0.0 || xmpx <= 0.0 {
                        continue;
                    }
                    let (o1, x1) = (oms / ompx, xms / xmpx);
                    let ratio = if x1 > 0.0 { o1 / x1 } else { f64::NAN };
                    let verdict = if !ratio.is_finite() { "—" }
                        else if ratio > 3.0 { "**much slower**" }
                        else if ratio > 1.3 { "slower" }
                        else if ratio < 0.77 { "faster" }
                        else { "≈ parity" };
                    o.push_str(&format!("| {name} | {o1:.2} | {x1:.2} | {ratio:.2}× | {verdict} |\n"));
                    if !name.starts_with("  ") {
                        worst.push((name.to_string(), o1, x1, ratio));
                    }
                }
                o.push('\n');
                worst.retain(|(_, _, _, r)| r.is_finite());
                worst.sort_by(|a, b| (b.1 - b.2).partial_cmp(&(a.1 - a.2)).unwrap());
                if let Some((n, o1, x1, r)) = worst.first() {
                    o.push_str(&format!(
                        "**Biggest absolute loss:** `{n}` — {o1:.2} vs {x1:.2} ms/Mpx ({r:.2}×), i.e. {:.2} ms/Mpx of the gap comes from this one function.\n\n",
                        o1 - x1
                    ));
                }
            }
        }
        }

        o.push_str("### Work only one side does\n\n| stage | side | note |\n|---|---|---|\n");
        for (stage, note) in ASYMMETRIC {
            let ms: f64 = stages.iter().filter(|r| r.kind == "encode" && &r.stage == stage).map(|r| r.ms).sum();
            if ms > 0.0 {
                o.push_str(&format!("| {stage} | {} | {note} |\n", if note.starts_with("x264") { "x264" } else { "ours" }));
            }
        }
        o.push('\n');
    }

    // ---- decoder -----------------------------------------------------------
    let dec: Vec<&Row> = speed.iter().filter(|r| r.kind == "decode").collect();
    if !dec.is_empty() {
        o.push_str("## 4. Decoder\n\n");
        o.push_str("Our decoder vs ffmpeg's native h264 decoder, both single-threaded, decoding the **same bitstreams**.\n\n");
        o.push_str("| clip | stream | ours Mpx/s | ffmpeg Mpx/s | ratio (ffmpeg ÷ ours) |\n|---|---|---:|---:|---:|\n");
        let mut keys: Vec<(String, String)> = dec.iter().map(|r| (r.clip.clone(), r.preset.clone())).collect();
        keys.dedup();
        let mut seen = BTreeMap::new();
        keys.retain(|k| seen.insert(k.clone(), ()).is_none());
        for (clip, stream) in keys {
            let ours = dec.iter().find(|r| r.clip == clip && r.preset == stream && r.codec == "ours").map(|r| r.mpx_s);
            let ff = dec.iter().find(|r| r.clip == clip && r.preset == stream && r.codec == "ffmpeg").map(|r| r.mpx_s);
            if let (Some(a), Some(b)) = (ours, ff) {
                o.push_str(&format!("| {clip} | {stream} | {a:.2} | {b:.2} | {:.2}× |\n", b / a));
            }
        }
        o.push_str("\n> ffmpeg ships stripped (0 symbols), so its decoder cannot be attributed per function. Our own decode breakdown is in `stages.tsv` (`kind=decode`); a two-sided decoder comparison needs ffmpeg rebuilt from source with symbols.\n\n");

        // Coverage: which x264 streams our decoder could handle at all. A row
        // present for ffmpeg but absent for ours means our decoder rejected it —
        // a conformance-surface fact worth stating rather than leaving as a gap.
        let ff_streams: Vec<&String> = dec.iter().filter(|r| r.codec == "ffmpeg").map(|r| &r.preset).collect();
        let our_streams: Vec<&String> = dec.iter().filter(|r| r.codec == "ours").map(|r| &r.preset).collect();
        let missing: Vec<&&String> = ff_streams.iter().filter(|s| !our_streams.contains(s)).collect();
        let mut uniq: Vec<String> = missing.iter().map(|s| (**s).clone()).collect();
        uniq.sort();
        uniq.dedup();
        o.push_str("### Decoder coverage\n\n");
        if uniq.is_empty() {
            o.push_str("Our decoder handled **every** stream in the run, including x264's High-profile output.\n\n");
        } else {
            o.push_str(&format!(
                "Our decoder could not decode {} of the stream configurations ffmpeg decoded — these exercise tools outside our implemented subset:\n\n",
                uniq.len()
            ));
            for s in uniq.iter().take(24) {
                o.push_str(&format!("* `{s}`\n"));
            }
            o.push('\n');
        }
    }

    let p = dir.join("REPORT.md");
    std::fs::write(&p, o).expect("write report");
    eprintln!("wrote {}", p.display());
}
