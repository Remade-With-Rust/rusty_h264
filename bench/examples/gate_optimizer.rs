//! Gate optimizer: evaluate content-gate rules against per-unit signed gain.
//!
//! The Great Gate P2 rule-search tool (docs/great-gate.md §1.5, §6 P2) — a
//! port of the FFai campaign's `suppress_optimizer.rs` (§8.106–§8.117) to the
//! codec contract. A GATE routes a unit (clip / segment / frame) to a feature
//! arm; the only score that matters is the summed SIGNED gain of the units it
//! routes, never classification accuracy — a 90%-precision rule still loses if
//! the 10% it misroutes is expensive (the unit-weighted-objective law).
//!
//! ## Input
//!
//! CSV with header: `unit,class,split,net_gain,<feature>...`
//!   - `unit`      — the routed unit's name (clip for per-clip gates).
//!   - `class`     — hand-labelled content class (great-gate.md §2). The plan's
//!                   finish line is per-CLASS: worst fired class ≥ 0, never an
//!                   average that one class pays for.
//!   - `split`     — `train` / `holdout`, assigned BY CLIP offline. A rule is
//!                   fitted on train and judged ONCE on holdout; sign
//!                   disagreement between splits refuses the rule whatever its
//!                   total (§8.114).
//!   - `net_gain`  — signed metric gain if THIS unit takes the feature arm
//!                   (for a BD-gated feature: −BD so positive = win). Summing
//!                   it over fired units IS the rule's exact effect.
//!   - features   — the P1 signal vector medians (docs/signals-truth-table.md).
//!
//! ## The traps carried over from the original (verbatim discipline)
//!
//! 1. **Check what the input was already filtered by.** A harvest taken with
//!    the candidate gate (or an upstream gate) active scores the gate's own
//!    output — a constant inert across its whole grid is the tell (§8.117).
//! 2. **Sum per UNIT before believing a total** — the `top3` column: a net
//!    carried by ~100% on 3 units is a unit list, not a rule (§8.114).
//! 3. **Enumerate the combination space** — with few units most random
//!    conjunctions separate perfectly; the `tried/passed` line calibrates how
//!    much a "clean" separation is worth (great-gate.md §1.5). More variables
//!    make this WORSE, so depth is capped at 3 and depth-1 rules are preferred
//!    at equal net.
//!
//! Usage: gate_optimizer <input.csv>

use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Row {
    unit: String,
    class: String,
    split: String,
    gain: f64,
    feats: Vec<f64>,
}

struct Table {
    feat_names: Vec<String>,
    rows: Vec<Row>,
}

fn load(path: &str) -> Table {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut lines = text.lines();
    let hdr: Vec<&str> = lines.next().expect("empty csv").split(',').collect();
    assert_eq!(&hdr[..4], &["unit", "class", "split", "net_gain"], "header contract");
    let feat_names: Vec<String> = hdr[4..].iter().map(|s| s.to_string()).collect();
    let rows = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            Row {
                unit: c[0].into(),
                class: c[1].into(),
                split: c[2].into(),
                gain: c[3].parse().unwrap_or_else(|_| panic!("bad net_gain in {l}")),
                feats: c[4..].iter().map(|v| v.parse().unwrap_or(0.0)).collect(),
            }
        })
        .collect();
    Table { feat_names, rows }
}

/// One threshold predicate: `feature <op> value`. A rule is an AND of these
/// over DISTINCT features (a conjunction buys precision — each clause removes
/// false fires faster than true ones).
#[derive(Clone)]
struct Pred {
    feat: usize,
    less: bool,
    t: f64,
}

impl Pred {
    fn fires(&self, r: &Row) -> bool {
        let v = r.feats[self.feat];
        if self.less { v < self.t } else { v > self.t }
    }
    fn label(&self, names: &[String]) -> String {
        format!("{}{}{}", names[self.feat], if self.less { "<" } else { ">" }, self.t)
    }
}

struct Verdict {
    n: usize,
    net: f64,
    train: f64,
    hold: f64,
    /// Mean gain of fired units in the WORST fired class — the finish line
    /// (worst class ≥ 0, per class, never on average).
    worst_class: f64,
    worst_name: String,
    top3: f64,
    precision: f64,
    /// Fraction of the corpus's total positive gain this rule captures.
    recall_gain: f64,
}

fn evaluate(rows: &[Row], fires: impl Fn(&Row) -> bool, perfect: f64) -> Verdict {
    let (mut n, mut net, mut tr, mut ho, mut hits, mut pos) = (0usize, 0f64, 0f64, 0f64, 0usize, 0f64);
    let mut per_unit: Vec<f64> = Vec::new();
    let mut per_class: HashMap<&str, (f64, usize)> = HashMap::new();
    for r in rows {
        if fires(r) {
            n += 1;
            net += r.gain;
            if r.split == "train" { tr += r.gain } else { ho += r.gain }
            if r.gain > 0.0 {
                hits += 1;
                pos += r.gain;
            }
            per_unit.push(r.gain);
            let e = per_class.entry(r.class.as_str()).or_insert((0.0, 0));
            e.0 += r.gain;
            e.1 += 1;
        }
    }
    let (mut worst, mut worst_name) = (f64::INFINITY, String::new());
    for (c, (g, k)) in &per_class {
        let m = g / *k as f64;
        if m < worst {
            worst = m;
            worst_name = c.to_string();
        }
    }
    // top3 share, with the near-cancellation guard: a net that is a
    // near-cancellation of large opposite movements makes the ratio explode —
    // that is a division by almost zero, not a concentration reading (§8.114).
    let gross: f64 = per_unit.iter().map(|v| v.abs()).sum();
    let top3 = if gross < 1e-12 || net.abs() < 0.02 * gross {
        f64::NAN
    } else {
        let mut v = per_unit.clone();
        v.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap());
        100.0 * v.iter().take(3).sum::<f64>() / net
    };
    Verdict {
        n,
        net,
        train: tr,
        hold: ho,
        worst_class: if worst.is_finite() { worst } else { 0.0 },
        worst_name,
        top3,
        precision: if n > 0 { hits as f64 / n as f64 } else { 0.0 },
        recall_gain: if perfect > 0.0 { pos / perfect } else { 0.0 },
    }
}

fn fmt_top3(v: f64) -> String {
    if v.is_nan() { "  n/a".into() } else { format!("{v:4.0}%") }
}

fn print_header() {
    println!(
        "{:<52} {:>3} {:>8} {:>8} {:>8} {:>9} {:<12} {:>6} {:>5} {:>6}",
        "Rule (fire = route unit to the feature arm)", "n", "net", "train", "holdout",
        "worstcls", "(which)", "top3", "prec", "recall"
    );
    println!("{}", "-".repeat(130));
}

fn print_verdict(name: &str, v: &Verdict) {
    println!(
        "{:<52} {:>3} {:>+8.3} {:>+8.3} {:>+8.3} {:>+9.3} {:<12} {:>6} {:>5.2} {:>6.2}",
        name, v.n, v.net, v.train, v.hold, v.worst_class, v.worst_name,
        fmt_top3(v.top3), v.precision, v.recall_gain
    );
}

/// Candidate thresholds for a feature: midpoints between adjacent sorted
/// distinct values — every separation the data supports, nothing invented.
fn thresholds(rows: &[Row], f: usize) -> Vec<f64> {
    let mut v: Vec<f64> = rows.iter().map(|r| r.feats[f]).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.dedup();
    v.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: gate_optimizer <input.csv>");
    let t = load(&path);
    let rows = &t.rows;
    let n_units = rows.len();
    let winners = rows.iter().filter(|r| r.gain > 0.0).count();
    let perfect: f64 = rows.iter().filter(|r| r.gain > 0.0).map(|r| r.gain).sum();
    let (n_tr, n_ho) = (
        rows.iter().filter(|r| r.split == "train").count(),
        rows.iter().filter(|r| r.split == "holdout").count(),
    );
    println!(
        "Loaded {n_units} units ({n_tr} train / {n_ho} holdout) | winners: {winners} | \
         perfect gain (fire exactly the winners): {perfect:+.3}\n"
    );
    if n_tr == 0 || n_ho == 0 {
        println!("A single-split input cannot pass the both-splits gate — assign splits by clip.");
        return;
    }

    // Per-class truth table first: the sign-flip table IS the dispatch trigger.
    let mut classes: Vec<&str> = rows.iter().map(|r| r.class.as_str()).collect();
    classes.sort();
    classes.dedup();
    println!("Per-class truth table (mean gain, n; a sign-flip across classes = dispatch):");
    for c in &classes {
        let g: Vec<f64> = rows.iter().filter(|r| &r.class == c).map(|r| r.gain).collect();
        let mean = g.iter().sum::<f64>() / g.len() as f64;
        let all: Vec<String> = rows
            .iter()
            .filter(|r| &r.class == c)
            .map(|r| format!("{}:{:+.2}", r.unit, r.gain))
            .collect();
        println!("  {:<12} {:+7.3} (n={})  {}", c, mean, g.len(), all.join("  "));
    }
    println!();

    // The two boundary arms every gate must beat.
    print_header();
    print_verdict("ALWAYS-ON (no gate)", &evaluate(rows, |_| true, perfect));
    print_verdict("ALWAYS-OFF", &evaluate(rows, |_| false, perfect));
    println!();

    // Depth-1 sweep: every feature, every supported threshold, both directions.
    println!("=== Depth-1 threshold sweep (rules positive on BOTH splits) ===");
    print_header();
    let mut passed1 = 0usize;
    let mut tried1 = 0usize;
    let mut best: Vec<(String, Verdict, usize)> = Vec::new();
    for f in 0..t.feat_names.len() {
        for &th in &thresholds(rows, f) {
            for less in [true, false] {
                let p = Pred { feat: f, less, t: th };
                tried1 += 1;
                let v = evaluate(rows, |r| p.fires(r), perfect);
                if v.train > 0.0 && v.hold > 0.0 {
                    passed1 += 1;
                    best.push((p.label(&t.feat_names), v, 1));
                }
            }
        }
    }
    // Depth 2–3 conjunctions, distinct features, gates applied INSIDE the
    // search (both splits positive) — enumerate rather than guess.
    let mut tried23 = 0usize;
    let mut passed23 = 0usize;
    let feats = t.feat_names.len();
    let mut preds: Vec<Pred> = Vec::new();
    for f in 0..feats {
        for &th in &thresholds(rows, f) {
            for less in [true, false] {
                preds.push(Pred { feat: f, less, t: th });
            }
        }
    }
    for i in 0..preds.len() {
        for j in i + 1..preds.len() {
            if preds[i].feat == preds[j].feat {
                continue;
            }
            tried23 += 1;
            let (a, b) = (&preds[i], &preds[j]);
            let v = evaluate(rows, |r| a.fires(r) && b.fires(r), perfect);
            if v.train > 0.0 && v.hold > 0.0 {
                passed23 += 1;
                best.push((
                    format!("{} & {}", a.label(&t.feat_names), b.label(&t.feat_names)),
                    v,
                    2,
                ));
            }
        }
    }

    // Rank: finish line first (worst fired class ≥ 0), then captured net,
    // then SHALLOWNESS (a depth-1 rule beats a depth-2 at equal net — the
    // combination-space law says deeper separations carry less information).
    best.sort_by(|a, b| {
        let fa = (a.1.worst_class >= 0.0) as u8;
        let fb = (b.1.worst_class >= 0.0) as u8;
        fb.cmp(&fa)
            .then(b.1.net.partial_cmp(&a.1.net).unwrap())
            .then(a.2.cmp(&b.2))
    });
    for (name, v, _) in best.iter().take(14) {
        print_verdict(name, v);
    }
    println!();
    println!(
        "Combination-space calibration: depth-1 {passed1}/{tried1} passed, \
         depth-2 {passed23}/{tried23} passed."
    );
    println!(
        "If a large share passes, a clean separation is the DEFAULT outcome and \
         carries no information (great-gate.md §1.5) — demand physical justification \
         per clause, prefer the shallow rule, and prefer the rule that abstains."
    );
}
