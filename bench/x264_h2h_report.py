#!/usr/bin/env python3
"""Report the rusty_h264-vs-x264 head-to-head as BD-rate + a speed Pareto.

Consumes the CSV written by `bench/x264_headtohead.ps1` (two sections: a
deterministic QUALITY ladder, then an ABBA-interleaved SPEED pass).

BD-rate here is bit-for-bit the convention in `bench/examples/bdrate.rs` —
cubic polyfit of log10(rate) against -10*log10(1-SSIM), integrated over the
overlapping quality range -- so these numbers sit on the same scale as every
BD table the campaign produced. Reimplemented rather than shelled out to
because that example encodes its own clips; here the ladder already exists.

PER-CLIP ONLY. A mean BD-rate across clips is not a number: it hides the
sign-flips that the whole content-adaptive-dispatch campaign exists to find.

  python bench/x264_h2h_report.py bench/_map/x264_h2h_2026-08-07.csv [prior.csv]
"""
import sys, math
from collections import defaultdict


from bdmath import bd_rate, polyfit3, ssim_db  # one home (plan A6)






def load(path):
    """-> (quality{(clip,side,arm): [(bytes, ssim_db)]}, speed{(clip,arm): [ms]})"""
    quality, speed = defaultdict(list), defaultdict(list)
    section = None
    for line in open(path, encoding="utf-8-sig"):
        line = line.strip()
        if not line:
            continue
        if line.startswith("clip,side,arm"):
            section = "q"; continue
        if line.startswith("clip,arm,rep"):
            section = "s"; continue
        f = line.split(",")
        if section == "q" and len(f) >= 6:
            clip, side, arm, _qp, byts, ssim = f[0], f[1], f[2], f[3], f[4], f[5]
            if byts == "ENCFAIL":
                print(f"  !! ENCFAIL {clip} {side}:{arm} qp{_qp}", file=sys.stderr)
                continue
            try:
                quality[(clip, side, arm)].append((float(byts), ssim_db(float(ssim))))
            except ValueError:
                pass
        elif section == "s" and len(f) >= 4:
            # arm column is "side:name"; cpu_ms may still carry a thousands
            # separator if produced by a pre-fix harness -- rejoin the tail.
            clip, arm = f[0], f[1]
            try:
                speed[(clip, arm)].append(float("".join(f[3:]).replace(",", "")))
            except ValueError:
                pass
    return quality, speed


def median(v):
    v = sorted(v)
    n = len(v)
    return None if not n else (v[n // 2] if n % 2 else 0.5 * (v[n // 2 - 1] + v[n // 2]))


def main():
    cur = sys.argv[1]
    prior = sys.argv[2] if len(sys.argv) > 2 else None
    q, s = load(cur)
    clips = sorted({k[0] for k in q})
    xarms = [a for a in ("veryfast", "medium", "slower") if any(k[1] == "x264" and k[2] == a for k in q)]
    oarms = [a for a in ("fast", "balanced", "quality") if any(k[1] == "ours" and k[2] == a for k in q)]

    print("=" * 78)
    print("QUALITY — BD-rate (%) of rusty_h264 vs x264, per clip, SSIM-based.")
    print("Negative = WE need fewer bits at equal quality. Positive = x264 wins by that %.")
    print("=" * 78)
    hdr = f"{'clip':<26}{'arm':<10}" + "".join(f"{'vs '+a:>13}" for a in xarms)
    print(hdr)
    print("-" * len(hdr))
    cur_bd = {}
    for clip in clips:
        for oa in oarms:
            test = q.get((clip, "ours", oa))
            row = f"{clip:<26}{oa:<10}"
            for xa in xarms:
                bd = bd_rate(q.get((clip, "x264", xa), []), test or [])
                cur_bd[(clip, oa, xa)] = bd
                row += f"{bd:>12.2f}%" if bd is not None else f"{'n/a':>13}"
            print(row)
        print()

    if prior:
        pq, _ = load(prior)
        print("=" * 78)
        print("DELTA vs the 2026-07-31 baseline — same clips, same arms, same metric.")
        print("Negative delta = the campaign CLOSED that much BD-rate gap.")
        print("=" * 78)
        hdr = f"{'clip':<26}{'arm':<10}" + "".join(f"{'vs '+a:>13}" for a in xarms)
        print(hdr)
        print("-" * len(hdr))
        for clip in clips:
            for oa in oarms:
                row = f"{clip:<26}{oa:<10}"
                for xa in xarms:
                    b4 = bd_rate(pq.get((clip, "x264", xa), []), pq.get((clip, "ours", oa), []))
                    now = cur_bd.get((clip, oa, xa))
                    if b4 is None or now is None:
                        row += f"{'n/a':>13}"
                    else:
                        row += f"{now - b4:>+12.2f}%"
                print(row)
            print()

    if s:
        print("=" * 78)
        print("SPEED — median pinned CPU ms over the ABBA-interleaved reps (qp 27).")
        print("=" * 78)
        arms = sorted({k[1] for k in s})
        hdr = f"{'clip':<26}" + "".join(f"{a.split(':')[1][:9]:>11}" for a in arms)
        print(f"{'':<26}" + "".join(f"{a.split(':')[0]:>11}" for a in arms))
        print(hdr)
        print("-" * len(hdr))
        for clip in clips:
            row = f"{clip:<26}"
            for a in arms:
                m = median(s.get((clip, a), []))
                row += f"{m:>11.0f}" if m else f"{'-':>11}"
            print(row)
        print()
        print("  Speed ratio (ours / x264 CPU time) — >1 means we are that many times SLOWER.")
        hdr = f"{'clip':<26}{'our arm':<10}" + "".join(f"{'/'+a:>13}" for a in xarms)
        print(hdr)
        print("-" * len(hdr))
        for clip in clips:
            for oa in oarms:
                om = median(s.get((clip, f"ours:{oa}"), []))
                row = f"{clip:<26}{oa:<10}"
                for xa in xarms:
                    xm = median(s.get((clip, f"x264:{xa}"), []))
                    row += f"{om/xm:>12.2f}x" if (om and xm) else f"{'-':>13}"
                print(row)
            print()
        # Dispersion: a median is only worth reading next to its spread.
        print("  Per-arm spread across reps (max-min as % of median) — the harness noise floor.")
        worst = 0.0
        for (clip, a), v in sorted(s.items()):
            m = median(v)
            if m and len(v) > 1:
                sp = 100.0 * (max(v) - min(v)) / m
                worst = max(worst, sp)
                if sp > 10.0:
                    print(f"    ⚠ {clip} {a}: {sp:.1f}% spread over {len(v)} reps")
        print(f"    worst spread across all arms: {worst:.1f}%")
        if worst > 10.0:
            print("    ⚠ A ratio is only meaningful to a precision coarser than this spread.")


if __name__ == "__main__":
    main()
