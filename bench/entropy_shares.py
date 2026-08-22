#!/usr/bin/env python3
"""KEY entropy-decode functions, as a CONTAINMENT TREE. DENSE-route MAIN streams.

Refreshes "#### KEY entropy decode functions" in docs/big-oppy-decoder.md, and is
the entropy-side twin of bench/glue_shares.py.

WHY DENSE. Entropy decode is 20.5% of LIGHT but 40.9% of DENSE and 78.0% of
ENTROPY (route_shares.py). LIGHT is where the per-MB glue lives; DENSE is where
the entropy stage lives, so that is the route to crack it open on.

WHAT THE TREE MEANS. `Stage::Entropy` in the decoder wraps exactly
`parse_residual_cabac` -- the CABAC residual parse. `ent:cbf`, `ent:sigmap` and
`ent:levels` are INFO scopes INSIDE it, so they do NOT add to it: they are
contained by it. The RESIDUE row is the part of the parse no child names, which
is the only part with no owner and therefore the only part worth attacking
blind. (The CAVLC scopes `cav:token`/`cav:levels`/`cav:runs` live under the same
parent on CAVLC streams and are reported too, so a DENSE CAVLC clip does not
silently read as pure residue.)

METHOD, matching glue_shares.py:
  * 1-in-64 SAMPLED profiler, 3 passes, per-stage MEDIAN. Exact timing taxes the
    run 1.3-1.4x and inflates precisely the high-call-count stages whose share we
    are reading; sampling scales survivors back up, so shares stay unbiased.
  * CALLS ARE EXACT and load-immune; ms is an estimate carrying the probe's tax.
    Where a count and a time disagree, THE COUNT WINS -- so calls/MB and ns/call
    sit beside every share.
  * Macroblocks come from decode_bench's own `px=` counter (px/256) and are
    EXACT, not derived from a throughput figure.

TWO BUGS THIS HARNESS DOES NOT HAVE, because its siblings did: the profiler
prints to STDERR (not stdout) and its rows are `  name  N.N ms  P.P%  (C calls)`
(no `prof ` prefix). route_shares.py and glue_shares.py both grepped stdout for
the old shape and returned EMPTY tables rather than an error.

  cargo build --release -p rusty_h264-decoder --features asm,profile --example decode_bench
  python bench/entropy_shares.py <path-to-that-decode_bench>
"""
import collections
import os
import re
import statistics
import subprocess
import sys

EXE = sys.argv[1]
PASSES = int(os.environ.get("PASSES", "7"))
TIER = sys.argv[2] if len(sys.argv) > 2 else "main"

# The DENSE route, straight off the routing table in the doc.
CLIPS = [
    ("detail", "720p50_shields_ter"),
    ("detail", "mobile_cif"),
    ("pan", "720p5994_stockholm_ter"),
    ("pan", "bus_cif"),
    ("complex", "crew_4cif"),
    ("complex", "tempete_cif"),
    ("fastmotion", "crowd_run_1080p50"),
    ("fastmotion", "football_cif"),
]

# (scope, doc label, where, depth)
NODES = [
    ("entropy/cavlc", "entropy decode (parse_residual_cabac)", "mb16.rs + cavlc.rs", 0),
    ("ent:cbf(nested)", "ent:cbf", "mb16.rs coded_block_flag", 1),
    ("ent:sigmap(nested)", "ent:sigmap", "mb16.rs significance map", 1),
    ("ent:levels(nested)", "ent:levels", "mb16.rs level decode", 1),
    ("cav:token(nested)", "cav:token", "cavlc.rs coeff_token", 1),
    ("cav:levels(nested)", "cav:levels", "cavlc.rs levels", 1),
    ("cav:runs(nested)", "cav:runs", "cavlc.rs total_zeros + runs", 1),
]

RESIDUE = [
    (
        "entropy/cavlc",
        "RESIDUE = unnamed parse glue",
        [
            "ent:cbf(nested)",
            "ent:sigmap(nested)",
            "ent:levels(nested)",
            "cav:token(nested)",
            "cav:levels(nested)",
            "cav:runs(nested)",
        ],
    ),
]

# `  <name>   12.3 ms   4.5%   (67890 calls)` -- total row has no calls suffix.
ROW_RE = re.compile(r"^\s{2}(\S.*?)\s{2,}([\d.]+)\s+ms\s+([\d.]+)%(?:.*?\((\d+) calls\))?")
PX_RE = re.compile(r"px=(\d+)")


def one_pass(stream):
    env = dict(os.environ, DP_REPS="1", RS_H264_PROF_SAMPLE="64")
    r = subprocess.run([EXE, stream], capture_output=True, text=True, env=env)
    out = r.stdout + r.stderr
    ms, calls = {}, {}
    px = PX_RE.search(out)
    mbs = (int(px.group(1)) / 256.0) if px else 0.0
    for line in out.splitlines():
        m = ROW_RE.match(line)
        if not m:
            continue
        name = m.group(1).strip()
        ms[name] = float(m.group(2))
        calls[name] = int(m.group(4)) if m.group(4) else 0
    return ms, calls, mbs


share = collections.defaultdict(list)
cpmb = collections.defaultdict(list)
nspc = collections.defaultdict(list)
absms = collections.defaultdict(list)
ncall = collections.defaultdict(list)

for cls, clip in CLIPS:
    stream = f"_xbench/tt/{clip}__{TIER}.264"
    if not os.path.exists(stream):
        print("MISSING", stream)
        continue
    runs = [one_pass(stream) for _ in range(PASSES)]
    keys = set().union(*[set(r[0]) for r in runs])
    med = {k: statistics.median([r[0].get(k, 0.0) for r in runs]) for k in keys}
    cal = {k: statistics.median([r[1].get(k, 0) for r in runs]) for k in keys}
    total = med.get("TOTAL", 0.0)
    mbs = statistics.median([r[2] for r in runs]) or 1.0
    if total <= 0:
        print("NO TOTAL", clip)
        continue
    ent = med.get("entropy/cavlc", 0.0)
    print(
        "-- %-24s TOTAL %7.1f ms  entropy %6.1f ms (%4.1f%%)  over %10.0f MBs"
        % (clip, total, ent, 100.0 * ent / total, mbs)
    )
    for scope, _, _, _ in NODES:
        share[scope].append(100.0 * med.get(scope, 0.0) / total)
        cpmb[scope].append(cal.get(scope, 0) / mbs)
        absms[scope].append(med.get(scope, 0.0))
        ncall[scope].append(cal.get(scope, 0))
        c = cal.get(scope, 0)
        nspc[scope].append((med.get(scope, 0.0) * 1e6 / c) if c else 0.0)
    for parent, label, kids in RESIDUE:
        r = med.get(parent, 0.0) - sum(med.get(k, 0.0) for k in kids)
        share[label].append(100.0 * r / total)
        absms[label].append(r)

M = lambda k: statistics.mean(share[k]) if share[k] else 0.0
SPREAD = lambda k: (min(share[k]), max(share[k])) if share[k] else (0.0, 0.0)
C = lambda k: statistics.mean(cpmb[k]) if cpmb[k] else 0.0
NS = lambda k: statistics.mean(nspc[k]) if nspc[k] else 0.0
A = lambda k: statistics.mean(absms[k]) if absms[k] else 0.0
NC = lambda k: statistics.mean(ncall[k]) if ncall[k] else 0.0
resid_after = {p: (lab, kids) for p, lab, kids in RESIDUE}

print()
print("| function                              | file                        | DENSE | calls/MB |")
print("| ------------------------------------- | --------------------------- | ----- | -------- |")
for scope, label, where, depth in NODES:
    if M(scope) == 0.0 and C(scope) == 0.0:
        continue
    pad = "|  " * depth
    print("| %-37s | %-27s | %4.1f%% | %8.2f |" % (pad + label, where, M(scope), C(scope)))
    if scope in resid_after:
        lab, _ = resid_after[scope]
        print("| %-37s | %-27s | %4.1f%% | %8s |" % ("|  " + lab, "(unnamed)", M(lab), "-"))

print()
print("| component of entropy decode | ms | %decode | ns/call | calls |")
print("| --- | --- | --- | --- | --- |")
rows = [(lab, A(sc), M(sc), NS(sc), NC(sc)) for sc, lab, _, d in NODES if d == 1 and A(sc) > 0]
rows.sort(key=lambda r: -r[1])
for lab, a, m, ns, nc in rows:
    print("| %s | %.2f | %.1f%% | %.0f | %,.0f |".replace("%,", "%") % (lab, a, m, ns, nc))
lab0, _ = resid_after["entropy/cavlc"]
print("| %s | %.2f | %.1f%% | - | - |" % (lab0, A(lab0), M(lab0)))
print("| = entropy decode | %.2f | %.1f%% | %.0f | %.0f |"
      % (A("entropy/cavlc"), M("entropy/cavlc"), NS("entropy/cavlc"), NC("entropy/cavlc")))
