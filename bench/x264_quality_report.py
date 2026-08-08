#!/usr/bin/env python3
"""BD-rate report for the rusty_h264-vs-x264 quality ladder, BY CONTENT CLASS.

Consumes bench/x264_quality.ps1's CSV (threads pinned on both sides).

Two things this reports that a plain BD table does not:

  * WORST CLASS, not mean. The campaign's finish line is "worst content class <= 0,
    verified per class, never on average." A mean BD-rate across clips is not a
    number -- it hides exactly the sign-flips the whole effort exists to find.

  * THE MATCHED-SPEED COLUMN. "We are N% larger" is meaningless without naming the
    speed it was measured at, and "we are Nx faster" is meaningless without naming
    the reference's preset. bench/x264_speed.ps1 measured ours:fast at 0.99-1.03x
    the CPU time of x264 --preset medium, so `medium` is the honest anchor for our
    `fast` arm -- that pairing is the actual standing of the encoder.

  python bench/x264_quality_report.py bench/_map/x264_quality_2026-08-07.csv
"""
import sys, math
from collections import defaultdict

# Measured by bench/x264_speed.ps1 (300 frames, threads=1, ABBA, pinned; null-arm
# floor 0.2-1.0%). Ratio of our CPU time to that x264 preset's, per our arm.
SPEED = {"fast": {"veryfast": 2.63, "medium": 1.01, "slower": 0.33},
         "quality": {"veryfast": 29.9, "medium": 11.6, "slower": 3.79}}


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
    """BD-rate (%) of test vs anchor. Negative = test needs fewer bits = test wins."""
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


def main():
    path = sys.argv[1]
    q = defaultdict(list)
    klass, warned = {}, []
    for line in open(path, encoding="utf-8-sig"):
        line = line.strip()
        if line.startswith("# MISMATCH") or line.startswith("# ENCFAIL") or line.startswith("# PROBEFAIL"):
            warned.append(line)
        if not line or line.startswith("#") or line.startswith("clip,"):
            continue
        f = line.split(",")
        if len(f) < 10 or f[8] == "ENCFAIL":
            if len(f) > 8 and f[8] == "ENCFAIL":
                warned.append("ENCFAIL " + ",".join(f[:5]))
            continue
        clip, cls, side, arm = f[0], f[1], f[2], f[3]
        try:
            q[(clip, side, arm)].append((float(f[8]), ssim_db(float(f[9]))))
        except ValueError:
            continue
        klass[clip] = cls

    if warned:
        print("!! HARNESS WARNINGS -- read before trusting anything below")
        for w in warned[:20]:
            print("   " + w)
        print()

    clips = sorted(klass, key=lambda c: (klass[c], c))
    xarms = ["veryfast", "medium", "slower"]
    oarms = ["fast", "quality"]

    print("=" * 100)
    print("BD-rate (%) of rusty_h264 vs x264, SSIM, per clip. threads=1 BOTH sides.")
    print("Negative = WE need fewer bits at equal quality. Positive = x264 wins by that much.")
    print("=" * 100)
    hdr = f"{'content class':<18}{'clip':<26}{'our arm':<9}" + "".join(f"{'vs '+a:>13}" for a in xarms)
    print(hdr); print("-" * len(hdr))
    bd = {}
    for clip in clips:
        for oa in oarms:
            row = f"{klass[clip]:<18}{clip[:25]:<26}{oa:<9}"
            for xa in xarms:
                v = bd_rate(q.get((clip, "x264", xa), []), q.get((clip, "ours", oa), []))
                bd[(clip, oa, xa)] = v
                row += f"{v:>12.1f}%" if v is not None else f"{'n/a':>13}"
            print(row)
        print()

    print("=" * 100)
    print("MATCHED-SPEED STANDING -- each of our arms against the x264 preset it")
    print("actually costs the same as. A quality gap quoted at an unmatched speed is not")
    print("a coding-efficiency result.")
    print("=" * 100)
    for oa in oarms:
        # the x264 preset whose measured CPU ratio is closest to 1.0
        anchor = min(SPEED[oa], key=lambda a: abs(math.log(SPEED[oa][a])))
        r = SPEED[oa][anchor]
        vals = [(bd[(c, oa, anchor)], c) for c in clips if bd.get((c, oa, anchor)) is not None]
        if not vals:
            continue
        vals.sort()
        print(f"\n  ours:{oa}  vs  x264:{anchor}   (we cost {r:.2f}x its CPU time)")
        print(f"    best  {vals[0][0]:>+7.1f}%  {vals[0][1]} [{klass[vals[0][1]]}]")
        print(f"    worst {vals[-1][0]:>+7.1f}%  {vals[-1][1]} [{klass[vals[-1][1]]}]")
        med = sorted(v[0] for v in vals)[len(vals) // 2]
        print(f"    median{med:>+7.1f}%   over {len(vals)} clips")
        wins = [v for v in vals if v[0] < 0]
        print(f"    clips where we WIN: {len(wins)}/{len(vals)}")

    print()
    print("=" * 100)
    print("WORST CLASS per x264 preset -- the campaign's stated finish line is that this")
    print("column, not an average, is what counts.")
    print("=" * 100)
    print(f"{'our arm':<10}" + "".join(f"{'vs '+a:>34}" for a in xarms))
    print("-" * 112)
    for oa in oarms:
        row = f"{oa:<10}"
        for xa in xarms:
            vals = [(bd[(c, oa, xa)], klass[c]) for c in clips if bd.get((c, oa, xa)) is not None]
            if vals:
                w = max(vals)
                row += f"{w[0]:>+18.1f}%  {w[1]:<13}"
            else:
                row += f"{'n/a':>34}"
        print(row)


if __name__ == "__main__":
    main()
