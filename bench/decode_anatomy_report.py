#!/usr/bin/env python3
"""Decoder anatomy: per-function CALL COUNTS (exact) beside ms/share (estimated).

Reads bench/decode_anatomy.sh's raw output.

WHAT IS TRUSTWORTHY HERE, AND WHY THEY ARE PRINTED SEPARATELY:

  * CALL COUNTS are exact and deterministic. Counting is a relaxed add, not an rdtsc
    pair, so it is unaffected by the sampling period, by machine load, or by drift.
    Run it once and it is true. These are the durable numbers.

  * HONEST WALL is best-of-N from a build with the profiler compiled OUT. best-of is
    used rather than mean because the minimum is the sample least polluted by the
    load spikes this box shows continuously.

  * STAGE ms/SHARES are estimates from a single instrumented pass and inherit both the
    probe's tax and the machine's drift. They are reported from TWO instruments --
    exact timing (every call) and 1-in-64 sampling -- and the report FLAGS any stage
    where the two disagree by more than 3 points. Two instruments agreeing is the
    standard of evidence; one instrument is a hypothesis.

The flat partition is stages 0..13 plus `dec-setup`. Everything tagged `(nested)` is
an INFO scope nested inside another and MUST NOT be summed into the partition -- doing
so double-counts. `mgmt/other` is TOTAL minus the flat partition: the per-MB glue that
no scope names.

  python bench/decode_anatomy_report.py bench/_map/decode_anatomy.raw
"""
import sys, re
from collections import defaultdict

FLAT = ["entropy/cavlc", "intra-pred", "inter-mc", "reconstruct", "deblock",
        "dequant", "scatter(store)", "pred-buf copy", "mv+grid", "neighbors",
        "skip-recon", "finalize", "syntax-parse", "dpb-clone", "dec-setup",
        # added 2026-08-07 while hunting the unnamed residue: these are top-level and
        # disjoint, so they belong in the partition. `dec-mb-loop` and `dec-row-hook`
        # do NOT -- they are nested wrappers and summing them would double-count.
        "dec-nal-split", "dec-rbsp-unescape", "dec-slice-alloc"]

CLASS = {"720p50_shields_ter": "720p-detail", "in_to_tree_420_720p50": "720p-foliage",
         "720p5994_stockholm_ter": "720p-pan", "mobile_cif": "cif-texture",
         "bus_cif": "cif-motion", "crowd_run_1080p50": "1080p-motion"}

# "    prof pred-buf copy         13.37 ms       621900 calls       21.5 ns/call"
ROW = re.compile(r"^\s*prof\s+(.+?)\s+([\d.]+)\s+ms\s+(\d+)\s+calls")
HDR = re.compile(r"best-of-\d+\s+([\d.]+)\s+ms\s+=\s+([\d.]+)\s+Mpx/s")


def parse(path):
    streams, cur, sec = {}, None, None
    for line in open(path, encoding="utf-8", errors="replace"):
        line = line.rstrip("\n")
        if line.startswith("===STREAM "):
            cur = line[len("===STREAM "):].strip()
            streams[cur] = {"honest": None, "mpx": None,
                            "exact": {}, "samp": [], "wall_exact": None, "wall_samp": []}
            sec = None
        elif line.startswith("---"):
            sec = line[3:].strip()
            # each SAMPLED64 marker opens a fresh pass; passes are medianed per stream
            if sec == "SAMPLED64":
                streams[cur]["samp"].append({})
        elif cur:
            m = HDR.search(line)
            if m and "prof " not in line:
                ms, mpx = float(m.group(1)), float(m.group(2))
                if sec == "HONEST":
                    streams[cur]["honest"], streams[cur]["mpx"] = ms, mpx
                elif sec == "PROFILED":
                    streams[cur]["wall_exact"] = ms
                elif sec == "SAMPLED64":
                    streams[cur]["wall_samp"].append(ms)
                continue
            m = ROW.match(line)
            if m:
                name, ms, calls = m.group(1).strip(), float(m.group(2)), int(m.group(3))
                if sec == "PROFILED":
                    streams[cur]["exact"][name] = (ms, calls)
                elif streams[cur]["samp"]:
                    streams[cur]["samp"][-1][name] = (ms, calls)
    return streams


def _med(v):
    v = sorted(v)
    return v[len(v) // 2] if v else float("nan")


def samp_share(d, name):
    """Median share of `name` across this stream's sampled passes."""
    out = []
    for p in d["samp"]:
        tot = p.get("TOTAL", (0, 0))[0]
        if tot and name in p:
            out.append(100 * p[name][0] / tot)
    return _med(out) if out else None


def samp_residue(d, flat):
    out = []
    for p in d["samp"]:
        tot = p.get("TOTAL", (0, 0))[0]
        if tot:
            out.append(100 * (tot - sum(p[n][0] for n in flat if n in p)) / tot)
    return _med(out) if out else None


def main():
    S = parse(sys.argv[1])
    tiers = ["cavlc", "main", "high"]

    print("=" * 104)
    print("1. HONEST DECODE SPEED  (profiler compiled OUT, best-of-5, 300 frames)")
    print("   Every stream verified byte-identical against ffmpeg before timing.")
    print("=" * 104)
    print(f"{'content class':<16}{'clip':<24}" + "".join(f"{t:>16}" for t in tiers))
    print("-" * 104)
    clips = sorted({k.split("__")[0] for k in S}, key=lambda c: CLASS.get(c, "z"))
    for c in clips:
        row = f"{CLASS.get(c,'?'):<16}{c[:23]:<24}"
        for t in tiers:
            d = S.get(f"{c}__{t}")
            row += f"{d['mpx']:>11.1f}Mpx/s" if d and d["mpx"] else f"{'-':>16}"
        print(row)

    print()
    print("=" * 104)
    print("2. PROFILER TAX  (instrumented wall / honest wall). Read no share without it.")
    print("=" * 104)
    print(f"{'stream':<40}{'honest ms':>12}{'exact x':>10}{'sampled x':>12}")
    print("-" * 104)
    for k in sorted(S):
        d = S[k]
        if not d["honest"]:
            continue
        e = d["wall_exact"] / d["honest"] if d["wall_exact"] else float("nan")
        s = _med(d["wall_samp"]) / d["honest"] if d["wall_samp"] else float("nan")
        flag = "  <-- sampling did NOT reduce tax; single-pass drift" if s > e else ""
        print(f"{k:<40}{d['honest']:>12.1f}{e:>9.2f}x{s:>11.2f}x{flag}")

    print()
    print("=" * 104)
    print("3. DECODER ANATOMY per tool tier -- CALLS ARE EXACT, shares are estimates.")
    print("   calls/frame is deterministic and content-addressable; share% from two")
    print("   independent instruments, flagged (!) where they disagree by >3 points.")
    print("=" * 104)
    for t in tiers:
        ks = [k for k in S if k.endswith("__" + t) and S[k]["exact"]]
        if not ks:
            continue
        print(f"\n  --- tier: {t} ({len(ks)} clips, 300 frames each) ---")
        print(f"    {'function':<18}{'calls/frame':>14}{'ms(est)':>10}{'ns/call':>11}"
              f"{'share%exact':>12}{'share%samp':>11}")
        print(f"    {'':<18}{'EXACT':>14}{'de-taxed':>10}{'de-taxed':>11}{'instr A':>12}{'instr B':>11}")
        print("    " + "-" * 84)
        agg = []
        for name in FLAT:
            cf, se, ss, est_ms, est_ns = [], [], [], [], []
            for k in ks:
                ex = S[k]["exact"]
                tot_e = ex.get("TOTAL", (0, 0))[0]
                calls = ex[name][1] if name in ex else None
                if name in ex and tot_e:
                    cf.append(calls / 300.0)
                    se.append(100 * ex[name][0] / tot_e)
                v = samp_share(S[k], name)
                if v is not None:
                    ss.append(v)
                # DE-TAXED ESTIMATE: the exact build's ns/call carries an rdtsc pair
                # per entry, which inflates precisely the high-count stages. Combining
                # the EXACT call count with the SAMPLED share and the HONEST wall gives
                # the best available per-call cost, with no probe in the denominator.
                if v is not None and calls and S[k]["honest"]:
                    t_ms = v / 100.0 * S[k]["honest"]
                    est_ms.append(t_ms)
                    est_ns.append(t_ms * 1e6 / calls)
            if not se:
                continue
            e, s = _med(se), _med(ss)
            agg.append((s if s == s else e, name, _med(cf), _med(est_ns), e, s,
                        abs(e - s), _med(est_ms)))
        agg.sort(reverse=True)
        for _, name, cf, nsc, e, s, dis, tms in agg:
            mark = " (!)" if dis > 3 else ""
            print(f"    {name:<18}{cf:>14,.0f}{tms:>10.1f}{nsc:>11.1f}{e:>12.1f}%{s:>11.1f}%{mark}")
        # residue
        for label, key in (("exact", "exact"), ("samp", "samp")):
            pass
        res_e, res_s = [], []
        for k in ks:
            ex = S[k]["exact"]
            te = ex.get("TOTAL", (0, 0))[0]
            if te:
                res_e.append(100 * (te - sum(ex[n][0] for n in FLAT if n in ex)) / te)
            v = samp_residue(S[k], FLAT)
            if v is not None:
                res_s.append(v)
        med = lambda v: sorted(v)[len(v) // 2] if v else float("nan")
        print("    " + "-" * 84)
        print(f"    {'mgmt/other (unnamed per-MB glue)':<42}{med(res_e):>13.1f}%{med(res_s):>12.1f}%")


if __name__ == "__main__":
    main()
