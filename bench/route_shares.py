#!/usr/bin/env python3
"""Per-ROUTE stage shares of our own decode time, MAIN-tier streams.

Refreshes the "FUNCTIONS BY % OF OUR PIPELINE, PER ROUTE" table in
docs/big-oppy-decoder.md.

METHOD (matches bench/decode_anatomy_report.py's partition rules):
  * 1-in-64 SAMPLED profiler. The scope guard is an rdtsc pair and the per-MB
    buckets are entered tens of millions of times, so exact timing taxes the run
    1.3-1.4x and inflates precisely the high-call-count stages whose share we are
    reading. Sampling scales survivors back up, so shares stay unbiased.
  * THREE passes per stream, per-stage MEDIAN. One instrumented pass inherits this
    box's drift.
  * Only TOP-LEVEL scopes are summed. Anything tagged `(nested)` is an INFO scope
    inside another and would double-count.
  * `per-MB glue (othr)` is the RESIDUE: 100 - sum(named stages). That is the
    per-MB glue no scope names, and it keeps each column summing to 100 by
    construction (the invariant the doc table states).
"""
import subprocess, sys, re, statistics, collections, os

EXE = sys.argv[1]
TT = "_xbench/tt"
PASSES = 3

# doc table row order -> profiler stage name
ROWS = [
    ("inter-mc", "inter-mc"),
    ("entropy decode", "entropy/cavlc"),
    ("deblock", "deblock"),
    ("syntax-parse", "syntax-parse"),
    ("dpb-clone", "dpb-clone"),
    ("reconstruct", "reconstruct"),
    ("dequant", "dequant"),
    ("skip-recon", "skip-recon"),
    ("scatter(store)", "scatter(store)"),
    ("intra-pred", "intra-pred"),
    ("pred-buf copy", "pred-buf copy"),
    ("finalize", "finalize"),
    ("neighbors", "neighbors"),
    ("mv+grid", "mv+grid"),
]

# class -> route, straight off the doc's `route (truth)` column
ROUTE = {
    "static": "LIGHT", "screen": "LIGHT",
    "medium": "MID", "smooth": "MID",
    "detail": "DENSE", "pan": "DENSE", "complex": "DENSE", "fastmotion": "DENSE",
    "grain": "ENTROPY",
}
CLIPS = [
    ("static", "FourPeople_1280x720_60"), ("static", "akiyo_cif"),
    ("screen", "screen_text"), ("screen", "screen_ui"),
    ("medium", "foreman_cif"), ("medium", "in_to_tree_420_720p50"),
    ("smooth", "blue_sky_1080p25"),
    ("detail", "720p50_shields_ter"), ("detail", "mobile_cif"),
    ("pan", "720p5994_stockholm_ter"), ("pan", "bus_cif"),
    ("complex", "crew_4cif"), ("complex", "tempete_cif"),
    ("fastmotion", "crowd_run_1080p50"), ("fastmotion", "football_cif"),
    ("grain", "grain_akiyo"), ("grain", "grain_flat"),
]

# The profiler prints to STDERR, two-space indented, as
#   "  <name padded to 15> <ms:>8.1> ms  <pct:>5.1>%   (<n> calls)"
# with the total row carrying no `(n calls)` suffix and `mgmt/other` carrying a
# trailing note. This regex was previously written for a `prof `-prefixed STDOUT
# format the binary no longer emits, so every clip reported "NO TOTAL" and the
# table it refreshes would have been silently stale (the stale-instrument law).
ROW_RE = re.compile(r"^\s{2}(\S.*?)\s{2,}([\d.]+)\s+ms\s+([\d.]+)%")


def one_pass(stream):
    env = dict(os.environ, DP_REPS="1", RS_H264_PROF_SAMPLE="64")
    r = subprocess.run([EXE, stream], capture_output=True, text=True, env=env)
    out = r.stdout + r.stderr
    ms = {}
    for line in out.splitlines():
        m = ROW_RE.match(line)
        if not m:
            continue
        name, val = m.group(1).strip(), float(m.group(2))
        if "(nested)" in name:      # never sum a nested scope
            continue
        ms[name] = val
    return ms


per_route = collections.defaultdict(lambda: collections.defaultdict(list))
print("%-11s %-26s %8s  %s" % ("class", "clip", "TOTAL ms", "othr%"))
for cls, clip in CLIPS:
    stream = f"{TT}/{clip}__main.264"
    if not os.path.exists(stream):
        print("MISSING", stream); continue
    passes = [one_pass(stream) for _ in range(PASSES)]
    keys = set().union(*[set(p) for p in passes])
    med = {k: statistics.median([p.get(k, 0.0) for p in passes]) for k in keys}
    total = med.get("TOTAL", 0.0)
    if total <= 0:
        print("NO TOTAL", clip); continue
    named = 0.0
    for label, stage in ROWS:
        share = 100.0 * med.get(stage, 0.0) / total
        per_route[ROUTE[cls]][label].append(share)
        named += share
    othr = 100.0 - named
    per_route[ROUTE[cls]]["per-MB glue (othr)"].append(othr)
    print("%-11s %-26s %8.1f  %5.1f" % (cls, clip, total, othr))

print()
order = ["per-MB glue (othr)"] + [r[0] for r in ROWS]
routes = ["LIGHT", "MID", "DENSE", "ENTROPY"]
means = {rt: {lab: statistics.mean(per_route[rt][lab]) for lab in order} for rt in routes}
order.sort(key=lambda lab: -means["LIGHT"][lab])   # doc orders rows by LIGHT share
print("| function (stage)   | LIGHT | MID  | DENSE | ENTROPY |")
print("| ------------------ | ----- | ---- | ----- | ------- |")
for lab in order:
    print("| %-18s | %5.1f | %4.1f | %5.1f | %7.1f |" % (
        lab, means["LIGHT"][lab], means["MID"][lab], means["DENSE"][lab], means["ENTROPY"][lab]))
print()
for rt in routes:
    print("%s column sum = %.1f" % (rt, sum(means[rt].values())))
