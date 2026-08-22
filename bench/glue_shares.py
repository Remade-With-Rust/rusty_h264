#!/usr/bin/env python3
"""KEY per-mb-glue functions, as a CONTAINMENT TREE. LIGHT MAIN-tier streams.

Refreshes "#### KEY per-mb-glue functions" in docs/big-oppy-decoder.md.

WHY A TREE. These are INFO scopes and they OVERLAP: `dec-mb-loop` wraps the whole
macroblock loop, so it contains the B/I/P bodies and the row hook. Reporting them
as a flat list invites the reading that they add up, which they do not. Each parent
below is followed by its children and then by the RESIDUE — the part of that scope
no child scope names, which is the actual "glue" and the only thing worth attacking.

  * 1-in-64 sampled profiler, 3 passes, per-stage median.
  * CALLS ARE EXACT and load-immune; ms is an estimate carrying the probe's tax.
    Where a count and a time disagree the count wins, so calls/MB sits beside every
    share. calls/MB uses REAL macroblocks (px/256), not slice invocations.
  * LIGHT = the two static + two screen clips, per the routing table.
"""
import subprocess, sys, re, statistics, collections, os

EXE = sys.argv[1]
PASSES = int(os.environ.get("PASSES", "7"))  # residue = parent-minus-children;
# at 3 passes that differential swung 5.7%% -> 16.5%% on the entropy twin.
CLIPS = ["FourPeople_1280x720_60", "akiyo_cif", "screen_text", "screen_ui"]

# (scope, doc label, where, depth) — depth drives the indent in the emitted table
NODES = [
    ("dec-mb-loop(nested)", "per-MB loop (ALL MB work)", "both slice loops", 0),
    ("dec-mb-B(nested)", "dec-mb-B bodies", "mb16.rs CABAC B arm", 1),
    ("b-mc(nested)", "b-mc", "mb16.rs b_mc", 2),
    ("b:luma-mc(nested)", "b:luma-mc", "in b-mc", 3),
    ("b:chroma-mc(nested)", "b:chroma-mc", "in b-mc", 3),
    ("b:blend(nested)", "b:blend", "in b-mc", 3),
    ("b:weights(nested)", "b:weights", "in b-mc", 3),
    ("b-direct(nested)", "b-direct", "mb16.rs b_direct*", 2),
    ("b-deriv(nested)", "b-deriv", "in b-direct", 3),
    ("b-setmotion(nested)", "b-setmotion", "mb16.rs b_set_motion", 2),
    ("dec-mb-P(nested)", "dec-mb-P bodies", "P path incl. decode_p_skip", 1),
    ("dec-mb-I(nested)", "dec-mb-I bodies", "intra path", 1),
    ("dec-row-hook(nested)", "row-hook", "mb16.rs row_hook", 1),
    ("deb:derive(nested)", "deb:derive", "deblock.rs bS derivation", 2),
    ("deb:pack(nested)", "deb:pack", "deblock.rs pack_frame_into", 2),
    ("mc-stage(nested)", "mc-stage", "recon helpers", 1),
    ("resid-add(nested)", "resid-add", "recon helpers", 1),
    ("state-cache(nested)", "state-cache", "mb16.rs nzc/mn export", 1),
    ("ent:sigmap(nested)", "ent:sigmap", "cabac.rs significance map", 1),
    ("ent:levels(nested)", "ent:levels", "cabac.rs level decode", 1),
    ("ent:cbf(nested)", "ent:cbf", "cabac.rs coded_block_flag", 1),
    ("dec-setup", "dec-setup", "grid refill (per picture)", 0),
    ("dec-slice-alloc", "dec-slice-alloc", "per-slice scratch", 0),
    ("dec-rbsp-unescape", "dec-rbsp-unescape", "nal.rs", 0),
    ("dec-nal-split", "dec-nal-split", "nal.rs", 0),
]

# parent -> children it fully contains, for the residue rows
RESIDUE = [
    ("dec-mb-loop(nested)", "per-MB loop RESIDUE (true glue)",
     ["dec-mb-B(nested)", "dec-mb-P(nested)", "dec-mb-I(nested)", "dec-row-hook(nested)"]),
    ("dec-mb-B(nested)", "dec-mb-B RESIDUE",
     ["b-mc(nested)", "b-direct(nested)", "b-setmotion(nested)"]),
    ("b-mc(nested)", "b-mc RESIDUE",
     ["b:luma-mc(nested)", "b:chroma-mc(nested)", "b:blend(nested)", "b:weights(nested)"]),
    ("dec-row-hook(nested)", "row-hook RESIDUE",
     ["deb:derive(nested)", "deb:pack(nested)"]),
]

# THIS HARNESS WAS BROKEN AND RETURNED NOTHING (fixed 2026-08-22, same defect as
# route_shares.py). Three things were wrong at once:
#   * it read STDOUT; the profiler prints to STDERR.
#   * ROW_RE wanted a `prof `-prefixed row; the shipped format is
#     `  <name>   12.3 ms   4.5%   (67890 calls)`.
#   * HDR_RE wanted `best-of-N ... = ... Mpx/s`; decode_bench prints
#     `frames=N px=N best=N.Nms ...`, so MBs came out 0 and every calls/MB was 0.
# It failed SILENTLY - an empty table, not an error - which is the expensive
# shape of a stale instrument. MBs now come from the exact `px=` counter.
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
        mm = ROW_RE.match(line)
        if not mm:
            continue
        name = mm.group(1).strip()
        ms[name] = float(mm.group(2))
        calls[name] = int(mm.group(4)) if mm.group(4) else 0
    return ms, calls, mbs


share = collections.defaultdict(list)
cpmb = collections.defaultdict(list)
for clip in CLIPS:
    stream = f"_xbench/tt/{clip}__main.264"
    if not os.path.exists(stream):
        print("MISSING", stream); continue
    runs = [one_pass(stream) for _ in range(PASSES)]
    keys = set().union(*[set(r[0]) for r in runs])
    med = {k: statistics.median([r[0].get(k, 0.0) for r in runs]) for k in keys}
    cal = {k: statistics.median([r[1].get(k, 0) for r in runs]) for k in keys}
    total = med.get("TOTAL", 0.0)
    mbs = statistics.median([r[2] for r in runs]) or 1.0
    print(f"-- {clip}: TOTAL {total:.1f} ms over {mbs:,.0f} MBs")
    for scope, _, _, _ in NODES:
        share[scope].append(100.0 * med.get(scope, 0.0) / total if total else 0.0)
        cpmb[scope].append(cal.get(scope, 0) / mbs)
    for parent, label, kids in RESIDUE:
        r = med.get(parent, 0.0) - sum(med.get(k, 0.0) for k in kids)
        share[label].append(100.0 * r / total if total else 0.0)

M = lambda k: statistics.mean(share[k]) if share[k] else 0.0
C = lambda k: statistics.mean(cpmb[k]) if cpmb[k] else 0.0
resid_after = {p: (lab, kids) for p, lab, kids in RESIDUE}

print()
print("| function | file | LIGHT share | calls/MB |")
print("| --- | --- | --- | --- |")
for scope, label, where, depth in NODES:
    pad = "&nbsp;" * (4 * depth)
    print("| %s%s | %s | %.1f%% | %.2f |" % (pad, label, where, M(scope), C(scope)))
    if scope in resid_after:
        lab, _ = resid_after[scope]
        print("| %s**%s** | (unnamed) | **%.1f%%** | - |" % ("&nbsp;" * (4 * (depth + 1)), lab, M(lab)))
