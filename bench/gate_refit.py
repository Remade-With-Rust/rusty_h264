#!/usr/bin/env python3
"""GREAT GATE refit harness — the repeatable version of the shape_rd_tex_max refit.

Every gate in this encoder is a THRESHOLD ON A SIGNAL, fitted against the encoder as
it was on the day it was fitted. Encoders change. This harness re-asks the question
and, more importantly, REFUSES to answer it when the corpus cannot support an answer.

The four failures this exists to make impossible — each one has actually happened here:

  1. NO ABOVE-THE-LINE CLIP. A census run on six clips with no grain and no screen
     clip concluded "four of five veto gates never fire". Asking a grain veto about
     non-grain content measures nothing. => refuses to report if no clip is on the
     side of the line the gate acts on.
  2. AN ARM THAT DOES NOT MOVE. An "off" arm that clears an env var instead of
     pinning a value falls through to a default and compares a setting against
     itself. Here the unveto arm must EXCEED each clip's own signal, per clip, or the
     cell is reported as NOT-AN-ARM rather than as a result. (`maxtex_plaid` sits at
     2962; a 2000 "unveto" arm silently still vetoes it and reads 0.00%.)
  3. FITTING ON ONE POINT. One above-the-line clip said "delete the guard"; a
     synthesized second one refuted it at +3.50%. => the verdict names how many
     above-the-line points it rests on, and says so when that number is small.
  4. UNSEEN COLLATERAL. Below-the-line clips must be BYTE-IDENTICAL across arms. If
     one moves, the signal is not what gates the behaviour and the whole table is
     void.

Usage:
    python bench/gate_refit.py --signal median_var --knob RFF_SHAPE_RD_TEXMAX \\
        --current 2000 --candidates 1000,2000,4000

    --signal      column in the RFF_SIGNALS_CSV harvest that the gate thresholds on
    --knob        env var that sets the threshold
    --current     the shipped value (reported as the incumbent)
    --candidates  comma-separated thresholds to compare

Reports BD-SSIM of each candidate against `--current`, PER CLIP. Negative = the
candidate needs fewer bits at equal quality = the candidate is better.
"""
import argparse
import math
import os
import re
import statistics
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gate_liveness as live  # noqa: E402

# clip -> {gate: (fired, seen)}, filled by harvest_signal from the SAME encode.
LIVENESS = {}

OURS = os.path.join("target", "release", "rusty_h264.exe")
GC = "_gc"
# One per content class, plus the synthesized above-the-line clips that exist
# precisely because the natural corpus has no content up there.
CLIPS = [
    ("akiyo_cif", 352, 288),
    ("foreman_cif", 352, 288),
    ("mobile_cif", 352, 288),
    ("harbour_4cif", 704, 576),
    ("FourPeople_1280x720_60", 1280, 720),
    ("grain_akiyo", 352, 288),
    ("screen_text", 352, 288),
    ("screen_ui", 352, 288),
    ("maxtex_plaid", 352, 288),
]
QPS = (22, 27, 32, 37)
BASE = ["--preset", "quality", "--cabac", "1", "--profile", "high",
        "--transform-8x8", "1", "--refs", "3", "--gop", "30"]


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
            if r != c and a[c][c]:
                f = a[r][c] / a[c][c]
                for k in range(c, 4):
                    a[r][k] -= f * a[c][k]
                b[r] -= f * b[c]
    return [b[i] / a[i][i] if a[i][i] else 0.0 for i in range(4)]


def bd(anchor, test):
    def prep(p):
        v = sorted((d, math.log10(r)) for r, d in p)
        return [x[0] for x in v], [x[1] for x in v]
    if len(anchor) < 4 or len(test) < 4:
        return None
    da, la = prep(anchor)
    dt, lt = prep(test)
    ca, ct = polyfit3(da, la), polyfit3(dt, lt)
    lo, hi = max(da[0], dt[0]), min(da[-1], dt[-1])
    if hi <= lo:
        return None
    I = lambda c, x: c[0]*x + c[1]*x*x/2 + c[2]*x**3/3 + c[3]*x**4/4
    return (10.0 ** (((I(ct, hi) - I(ct, lo)) - (I(ca, hi) - I(ca, lo))) / (hi - lo)) - 1) * 100


def fmt(v):
    """Threshold -> env string. INTEGER-CLEAN: `str(2000.0)` is "2000.0", which Rust's
    `parse::<i64>()` rejects, so the knob silently falls back to its DEFAULT and both
    arms become the same setting. That is failure mode #2 in this file's header, and
    it happened on this harness's very first run — the byte-identity check is what
    surfaced it."""
    return "%d" % int(v) if float(v).is_integer() else "%g" % v


def encode(clip, w, h, qp, bframes, knob, value):
    """One encode. Returns (bytes, ssim_db, raw_bitstream)."""
    src = os.path.join(GC, clip + ".yuv")
    bit = os.path.join(GC, "gr.264")
    dec = os.path.join(GC, "gr.yuv")
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    env[knob] = fmt(value)
    subprocess.run([OURS, "encode", "--width", str(w), "--height", str(h), "--qp", str(qp),
                    "--bframes", str(bframes)] + BASE + ["--in", src, "--out", bit],
                   capture_output=True, env=env)
    if not os.path.exists(bit) or os.path.getsize(bit) == 0:
        return None
    raw = open(bit, "rb").read()
    subprocess.run(["ffmpeg", "-v", "error", "-i", bit, "-f", "rawvideo",
                    "-pix_fmt", "yuv420p", "-y", dec], capture_output=True)
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "info",
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", dec,
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", src,
                        "-lavfi", "ssim", "-f", "null", "-"], capture_output=True, text=True)
    m = re.findall(r"All:([0-9.]+)", r.stderr)
    if not m:
        return None
    return len(raw), ssim_db(float(m[-1])), raw


def harvest_signal(clip, w, h, column):
    """Median of `column` over the clip's frames — where the clip sits on the axis."""
    csv_path = os.path.join(GC, "gr_sig.csv")
    census_path = os.path.join(GC, "gr_census.csv")
    for p in (csv_path, census_path):
        if os.path.exists(p):
            os.remove(p)
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    env["RFF_SIGNALS_CSV"] = csv_path
    # LIVENESS rides the SAME encode as the signal, deliberately: measuring it
    # in a separate run would be measuring a different configuration, which is
    # precisely how `gatecheck`'s counts (its own EncoderConfig, num_refs = 1)
    # came to describe a different encode than the audits that cited them.
    env[live.CENSUS_ENV] = census_path
    subprocess.run([OURS, "encode", "--width", str(w), "--height", str(h), "--qp", "27",
                    "--bframes", "0"] + BASE
                   + ["--in", os.path.join(GC, clip + ".yuv"), "--out", os.path.join(GC, "gr_s.264")],
                   capture_output=True, env=env)
    LIVENESS[clip] = live.read(census_path) if os.path.exists(census_path) else {}
    if not os.path.exists(csv_path):
        return None
    import csv as _csv
    rows = list(_csv.DictReader(open(csv_path)))
    vals = [float(r[column]) for r in rows if r.get(column) not in (None, "")]
    return statistics.median(vals) if vals else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--signal", required=True)
    ap.add_argument("--knob", required=True)
    ap.add_argument("--current", type=float, required=True)
    ap.add_argument("--candidates", required=True)
    ap.add_argument("--bframes", type=int, default=0)
    # LIVENESS. `--chain` is the precondition chain, LEAF LAST
    # (`--chain "sub8_grain > sub8_split"`). Declared rather than threaded
    # through the encoder's call frames, and self-checking: `seen` cannot GROW
    # as the chain narrows.
    ap.add_argument("--chain", default=None,
                    help="gate census chain, leaf last, e.g. 'sub8_grain > sub8_split'")
    # A FLOOR, not a null test. sub8_grain reads seen=19 at --refs 3 and 1213 at
    # --refs 1 — a zero-check calls that healthy, and 19 consultations cannot
    # carry a BD verdict.
    ap.add_argument("--min-seen", type=int, default=0, dest="min_seen",
                    help="consultations below which the gate is not considered under test")
    # DIRECTION IS NOT COSMETIC. A gate acts on ONE side of its line, and which side
    # decides both which clips are holdouts and whether a candidate is even an arm.
    # shape_rd_tex_max vetoes ABOVE (median_var > 1000); the grain conjunction vetoes
    # BELOW (median_var < 200). Assuming "above" would have called every grain clip a
    # non-holdout and reported a confident table about the wrong half of the corpus.
    ap.add_argument("--direction", choices=["above", "below"], default="above",
                    help="side of the threshold the gate ACTS on")
    args = ap.parse_args()
    cands = [float(x) for x in args.candidates.split(",")]

    clips = [(c, w, h) for c, w, h in CLIPS if os.path.exists(os.path.join(GC, c + ".yuv"))]
    print("GATE REFIT — signal `%s`, knob `%s`, incumbent %s"
          % (args.signal, args.knob, fmt(args.current)))
    print("=" * 96)

    # ---- STEP 1: where does each clip sit on the axis? -------------------------
    print("STEP 1 — locate every clip on the axis (a gate only acts on one side of it)")
    print("-" * 96)
    acts0 = (lambda v, t: v > t) if args.direction == "above" else (lambda v, t: v < t)
    where = {}
    for c, w, h in clips:
        v = harvest_signal(c, w, h, args.signal)
        where[c] = v
        on = v is not None and acts0(v, args.current)
        print("  %-24s %s = %-10s  %s" %
              (c, args.signal, ("%.4g" % v) if v is not None else "n/a",
               "GATE ACTS HERE" if on else "gate does not act"))
    acts = (lambda v, t: v > t) if args.direction == "above" else (lambda v, t: v < t)
    above = [c for c, _, _ in clips if where[c] is not None and acts(where[c], args.current)]
    print()

    # ---- STEP 1.5: LIVENESS — could the gate run here at all? -----------------
    #
    # BEFORE the corpus-gap check, and the order is load-bearing: a gate whose
    # decision site is never reached also acts on nothing, so it looks exactly
    # like a corpus gap — and "synthesize the content" is the wrong instruction
    # for a path that is switched off.
    if args.chain:
        chain = [g.strip() for g in args.chain.replace(">", ",").split(",") if g.strip()]
        print("STEP 1.5 — liveness: was `%s` actually consulted?" % chain[-1])
        print("-" * 96)
        statuses = {}
        for c, _, _ in clips:
            st, msg = live.verdict(LIVENESS.get(c, {}), chain, args.min_seen)
            statuses[c] = st
            print("  %-24s %-13s %s" % (c, st.upper(), msg))
        print()
        if any(s == "inconsistent" for s in statuses.values()):
            print("REFUSING TO REPORT: the declared --chain is not what the encoder does.")
            print("Every liveness number below would be describing the wrong structure.")
            return 2
        if all(s in ("dead", "thin", "absent") for s in statuses.values()):
            print("REFUSING TO REPORT: `%s` is not exercised on ANY clip in this" % chain[-1])
            print("configuration, so this run cannot say anything about it. This is NOT a")
            print("corpus gap — adding content changes nothing when the site is not reached.")
            print("Check these encode flags against the preconditions the gate sits behind;")
            print("`python bench/gate_liveness.py --diff A.csv B.csv` names the gates that")
            print("lose their decision site between two configurations.")
            return 2

    # ---- STEP 2: refuse to proceed without content on the acting side ---------
    if not above:
        print("REFUSING TO REPORT: no clip in the corpus is on the %s side of the line," % args.direction)
        print("so the gate never acts and this run would measure only the fallback.")
        print("Add or synthesize")
        print("content past the threshold first — that is the ONLY content that tests the")
        print("gate's actual claim. (video-tests/synth_clips.sh is where those live.)")
        return 2
    if len(above) < 2:
        print("WARNING: only %d clip is above the line. A threshold fitted on one point is"
              % len(above))
        print("the documented failure mode — one above-the-line clip once said 'delete this")
        print("guard' and a synthesized second one refuted it at +3.50%. Treat the verdict")
        print("below as provisional and synthesize a second above-the-line clip.")
        print()

    # ---- STEP 3: sweep, with per-clip arm validation --------------------------
    print("STEP 3 — BD-SSIM of each candidate vs the incumbent, PER CLIP.")
    print("Negative = candidate needs fewer bits at equal quality = candidate is better.")
    print("'not-an-arm' = the candidate lands on the SAME side of this clip's signal as")
    print("the incumbent, so the two arms are the same setting and the cell is not a test.")
    print("-" * 96)
    hdr = "%-24s %9s" % ("clip", args.signal)
    for cd in cands:
        hdr += "%12s" % ("T=%s" % fmt(cd))
    print(hdr)
    print("-" * 96)

    verdict = {cd: {"win": [], "loss": [], "moved": 0} for cd in cands}
    for c, w, h in clips:
        sig = where[c]
        base = [encode(c, w, h, q, args.bframes, args.knob, args.current) for q in QPS]
        base = [b for b in base if b]
        row = "%-24s %9s" % (c[:23], ("%.0f" % sig) if sig is not None else "n/a")
        for cd in cands:
            # ARM VALIDATION: does this candidate actually change this clip's fate?
            if sig is None or (acts(sig, args.current) == acts(sig, cd)):
                row += "%12s" % "not-an-arm"
                continue
            test = [encode(c, w, h, q, args.bframes, args.knob, cd) for q in QPS]
            test = [t for t in test if t]
            # COLLATERAL CHECK: identical bitstreams => the knob did nothing here.
            if all(a[2] == b[2] for a, b in zip(base, test)):
                row += "%12s" % "byte-ident"
                continue
            v = bd([(b[0], b[1]) for b in base], [(t[0], t[1]) for t in test])
            if v is None:
                row += "%12s" % "n/a"
                continue
            verdict[cd]["moved"] += 1
            if v > 0.05:
                verdict[cd]["loss"].append((c, v))
            elif v < -0.05:
                verdict[cd]["win"].append((c, v))
            row += "%11.2f%%" % v
        print(row)
        sys.stdout.flush()

    # ---- STEP 4: verdict, with its own limits stated --------------------------
    print("-" * 96)
    print("VERDICT")
    for cd in cands:
        losses = [(c, v) for c, v in verdict[cd]["loss"]]
        wins = verdict[cd]["win"]
        tag = "REGRESSES" if losses else ("wins" if wins else "no effect")
        print("  T=%-8s %-10s clips moved %d | wins %s | losses %s"
              % (fmt(cd), tag, verdict[cd]["moved"],
                 ", ".join("%s %.2f%%" % (c, v) for c, v in wins) or "-",
                 ", ".join("%s %+.2f%%" % (c, v) for c, v in losses) or "-"))
    print()
    print("Rests on %d clip(s) the gate ACTS on: %s" % (len(above), ", ".join(above)))
    print("A threshold is only valid on the axes its corpus VARIED. Content landing")
    print("between the incumbent and the chosen value is untested by this run.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
