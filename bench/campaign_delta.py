#!/usr/bin/env python3
"""What the Great Gate campaign actually changed, measured two ways.

  BEFORE = 3bf242d (release 0.8.0), the parent of a6e45f7 "the Great Gate campaign".
  AFTER  = HEAD.

Both ladders are produced by bench/x264_quality.ps1 with threads pinned, so the only
thing that differs is our encoder.

TWO MEASURES, deliberately separate:

  1. DIRECT (after vs before) -- BD-rate of our new encoder against our old one, with
     x264 nowhere in the loop. This is the campaign's coding-efficiency delta and it
     cannot be contaminated by anything the reference does. It is the honest headline.

  2. STANDING SHIFT (vs x264) -- how far the gap to each x264 preset moved. Only valid
     because x264 is deterministic for a fixed arg set, so today's anchors serve both
     ladders. This is what a reader actually wants to know, but it inherits the
     reference's own quirks and is reported second.

A byte-identical arm is reported as EXACTLY 0.00 rather than a tiny float, because
"the campaign did not touch this configuration" and "the campaign moved it 0.003%" are
different claims.

  python bench/campaign_delta.py <after.csv> <before.csv>
"""
import sys, math
from collections import defaultdict


def ssim_db(s):
    return -10.0 * math.log10(max(1.0 - s, 1e-9))


def polyfit3(xs, ys):
    a = [[0.0] * 4 for _ in range(4)]
    b = [0.0] * 4
    for x, y in zip(xs, ys):
        xp = [1.0]
        for p in range(1, 7):
            xp.append(xp[p - 1] * x)
        for j in range(4):
            for k in range(4):
                a[j][k] += xp[j + k]
            b[j] += y * xp[j]
    for c in range(4):
        piv = max(range(c, 4), key=lambda r: abs(a[r][c]))
        a[c], a[piv] = a[piv], a[c]
        b[c], b[piv] = b[piv], b[c]
        for r in range(4):
            if r != c and a[c][c] != 0.0:
                f = a[r][c] / a[c][c]
                for k in range(c, 4):
                    a[r][k] -= f * a[c][k]
                b[r] -= f * b[c]
    return [b[i] / a[i][i] if a[i][i] else 0.0 for i in range(4)]


def bd_rate(anchor, test):
    def prep(pts):
        v = sorted(((d, math.log10(r)) for r, d in pts))
        return [p[0] for p in v], [p[1] for p in v]
    if len(anchor) < 4 or len(test) < 4:
        return None
    da, la = prep(anchor)
    dt, lt = prep(test)
    ca, ct = polyfit3(da, la), polyfit3(dt, lt)
    lo, hi = max(da[0], dt[0]), min(da[-1], dt[-1])
    if hi <= lo:
        return None
    def integ(c, x):
        return c[0]*x + c[1]*x*x/2.0 + c[2]*x**3/3.0 + c[3]*x**4/4.0
    avg = ((integ(ct, hi) - integ(ct, lo)) - (integ(ca, hi) - integ(ca, lo))) / (hi - lo)
    return (10.0 ** avg - 1.0) * 100.0


def load(path):
    q, klass, raw = defaultdict(list), {}, {}
    for line in open(path, encoding="utf-8-sig"):
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("clip,"):
            continue
        f = line.split(",")
        if len(f) < 10 or f[8] == "ENCFAIL":
            continue
        try:
            q[(f[0], f[2], f[3])].append((float(f[8]), ssim_db(float(f[9]))))
            raw[(f[0], f[2], f[3], f[4])] = (int(f[8]), float(f[9]))
        except ValueError:
            continue
        klass[f[0]] = f[1]
    return q, klass, raw


def main():
    after, before = sys.argv[1], sys.argv[2]
    qa, klass, ra = load(after)
    qb, _, rb = load(before)
    clips = sorted(klass, key=lambda c: (klass[c], c))
    oarms = ["fast", "quality"]

    # Which configurations did the campaign touch at all? A byte-identical arm is a
    # fact worth stating -- it means the shipped gates abstained on that content.
    print("=" * 96)
    print("DID THE CAMPAIGN CHANGE THE BITSTREAM AT ALL? (identical bytes at every QP")
    print("means every shipped gate ABSTAINED on that clip+arm -- the abstention")
    print("property the gates were built to have.)")
    print("=" * 96)
    touched = {}
    for clip in clips:
        for oa in oarms:
            pts = [(ra.get((clip, "ours", oa, str(q))), rb.get((clip, "ours", oa, str(q))))
                   for q in (22, 27, 32, 37)]
            pts = [(x, y) for x, y in pts if x and y]
            if not pts:
                touched[(clip, oa)] = None
                continue
            same = all(x[0] == y[0] for x, y in pts)
            touched[(clip, oa)] = not same
            n_diff = sum(1 for x, y in pts if x[0] != y[0])
            tag = "IDENTICAL (gate abstained)" if same else f"changed at {n_diff}/{len(pts)} QPs"
            print(f"  {klass[clip]:<18}{clip[:26]:<28}{oa:<9} {tag}")
    print()

    print("=" * 96)
    print("1. DIRECT -- BD-rate of AFTER vs BEFORE, our encoder against itself.")
    print("   Negative = the campaign made us need FEWER bits at equal quality.")
    print("=" * 96)
    print(f"{'content class':<18}{'clip':<28}{'fast':>12}{'quality':>12}")
    print("-" * 70)
    direct = defaultdict(list)
    for clip in clips:
        row = f"{klass[clip]:<18}{clip[:26]:<28}"
        for oa in oarms:
            if touched.get((clip, oa)) is False:
                row += f"{'0.00':>12}"
                direct[oa].append(0.0)
                continue
            v = bd_rate(qb.get((clip, "ours", oa), []), qa.get((clip, "ours", oa), []))
            if v is None:
                row += f"{'n/a':>12}"
            else:
                row += f"{v:>+11.2f}%"
                direct[oa].append(v)
        print(row)
    print("-" * 70)
    for oa in oarms:
        v = sorted(direct[oa])
        if not v:
            continue
        med = v[len(v) // 2]
        print(f"  ours:{oa:<9} best {v[0]:>+7.2f}%   worst {v[-1]:>+7.2f}%   median {med:>+7.2f}%"
              f"   improved on {sum(1 for x in v if x < 0)}/{len(v)}, REGRESSED on {sum(1 for x in v if x > 0)}/{len(v)}")

    print()
    print("=" * 96)
    print("2. STANDING SHIFT -- BD-rate vs x264 then and now (same x264 anchors).")
    print("   Negative delta = the campaign closed that much of the gap.")
    print("=" * 96)
    for xa in ("veryfast", "medium", "slower"):
        if not any((c, "x264", xa) in qa for c in clips):
            continue
        print(f"\n  vs x264 --preset {xa}")
        print(f"    {'clip':<28}{'arm':<9}{'before':>10}{'after':>10}{'delta':>10}")
        print("    " + "-" * 67)
        for clip in clips:
            anchor = qa.get((clip, "x264", xa), [])
            for oa in oarms:
                b = bd_rate(anchor, qb.get((clip, "ours", oa), []))
                a = bd_rate(anchor, qa.get((clip, "ours", oa), []))
                if b is None or a is None:
                    continue
                d = a - b
                mark = "" if abs(d) >= 0.005 else "  (untouched)"
                print(f"    {clip[:26]:<28}{oa:<9}{b:>9.1f}%{a:>9.1f}%{d:>+9.2f}%{mark}")


if __name__ == "__main__":
    main()
