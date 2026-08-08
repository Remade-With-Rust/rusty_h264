#!/usr/bin/env python3
"""R1 PRE-CHECK: is the SATD split proxy's error worth refitting?

docs/gate-repair-plan.md R1. The abstention census says RD overturns the SATD split
pick on 33.8-81.4% of the macroblocks where SATD chose to split. A RATE cannot justify
a refit: `prom_av1e004` was a 3x more accurate cost model that measured DEAD NEUTRAL
because its error was rank-invariant near the argmin. What decides R1 is the MAGNITUDE
of the disagreement, in the encoder's own currency.

`RFF_SUB8_REGRET` records one signed number per macroblock whose RD trial ran:

    dj = (j_split - j_flat) / lambda
    dj > 0  RD reverted -> following SATD would have cost `dj`.  REGRET.
    dj < 0  RD kept it  -> the split saved `-dj`.                GAIN.

VERDICT RULE, fixed before looking at the data so it cannot be rationalised after:
  * If the regret distribution is concentrated near zero (median regret < ~1 lambda-unit
    and a thin tail), SATD's false positives are NEAR-TIES. The RD pass is buying
    almost nothing, refitting the proxy cannot help, and R1 closes without touching it.
  * A fat regret tail (large p90/p99) is the only result that justifies a refit.

ALSO REPORTED, because it decides how much R1 can possibly be worth: how often the RD
trial runs at all in each configuration. The x264-comparable arm uses B-frames, and the
P_8x8 sub-split path is a P-slice path.

  python bench/r1_regret_harvest.py
"""
import os
import subprocess
import statistics as st

EXE = os.path.join("target", "release", "rusty_h264.exe")
CLIPS = [
    ("grain_akiyo", 352, 288),
    ("screen_text", 352, 288),
    ("harbour_4cif", 704, 576),
    ("akiyo_cif", 352, 288),
    ("FourPeople_1280x720_60", 1280, 720),
    ("mobile_cif", 352, 288),
]
QPS = (27, 32)
# Two configurations, because they exercise the path at wildly different rates:
#   x264cmp  what every competitive comparison uses (B-frames on)
#   noB      what gatecheck's census ran (bframes 0) -- the P_8x8 path's home
ARMS = {"x264cmp": ["--bframes", "2"], "noB": ["--bframes", "0"]}


def harvest(clip, w, h, qp, arm):
    src = os.path.join("_gc", clip + ".yuv")
    if not os.path.exists(src):
        return None
    out = os.path.join("_gc", "r1.264")
    csv = os.path.join("_gc", "r1_regret.csv")
    for p in (csv,):
        if os.path.exists(p):
            os.remove(p)
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    env["RFF_SUB8_REGRET"] = csv
    args = ([EXE, "encode", "--width", str(w), "--height", str(h), "--qp", str(qp),
             "--gop", "60", "--preset", "quality", "--cabac", "1", "--refs", "3"]
            + ARMS[arm] + ["--in", src, "--out", out])
    subprocess.run(args, capture_output=True, env=env)
    if not os.path.exists(csv):
        return []
    rows = []
    with open(csv, encoding="utf-8") as f:
        next(f, None)
        for line in f:
            p = line.strip().split(",")
            if len(p) == 6:
                rows.append((int(p[0]), float(p[4])))   # reverted, dj_lambda
    return rows


def q(v, p):
    if not v:
        return float("nan")
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))]


for arm in ("x264cmp", "noB"):
    print("=" * 100)
    print("ARM %s   (%s)" % (arm, " ".join(ARMS[arm])))
    print("=" * 100)
    print("%-24s %8s %8s %9s %9s %9s %9s %9s" %
          ("clip", "MBs", "revert%", "reg med", "reg p90", "reg p99", "reg max", "gain med"))
    print("-" * 100)
    tot = []
    for clip, w, h in CLIPS:
        rows = []
        for qp in QPS:
            r = harvest(clip, w, h, qp, arm)
            if r:
                rows += r
        if not rows:
            print("%-24s %8s" % (clip[:23], "PATH NEVER RUNS"))
            continue
        reg = [d for rv, d in rows if rv == 1]     # regret: RD reverted
        gain = [-d for rv, d in rows if rv == 0]   # gain:   RD kept
        tot += reg
        print("%-24s %8d %7.1f%% %9.3f %9.3f %9.3f %9.3f %9.3f" %
              (clip[:23], len(rows), 100.0 * len(reg) / len(rows),
               st.median(reg) if reg else float("nan"), q(reg, .90), q(reg, .99),
               max(reg) if reg else float("nan"),
               st.median(gain) if gain else float("nan")))
    if tot:
        print("-" * 100)
        print("%-24s %8d %8s %9.3f %9.3f %9.3f %9.3f" %
              ("ALL (regret pool)", len(tot), "", st.median(tot), q(tot, .90),
               q(tot, .99), max(tot)))
    print()
