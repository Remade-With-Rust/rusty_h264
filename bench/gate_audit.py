#!/usr/bin/env python3
"""GREAT GATE audit — does each veto still EARN ITS KEEP on the content it acts on?

`bench/gate_refit.py` asks "is this threshold in the right place". This asks the
prior question: "does this gate still help at all". They are different, and the
second one is what caught `shape_rd_tex_max` — a guard added to prevent a +1.99%
regression that had silently become a -0.90% win blocker, because the encoder it was
fitted against no longer exists.

For each gate: BD-SSIM of GATE-OFF against GATE-ON (shipped), measured ONLY on the
clips the gate actually acts on, plus a control that must come back byte-identical.

    NEGATIVE  = the non-shipped arm WINS = the shipped setting is COSTING us.
    POSITIVE  = the gate is still earning its keep. Leave it.
    byte-ident= the gate did not act here; the cell is not a test.

CONFIGURATION IS PART OF THE MEASUREMENT. Each gate declares the CLI it must be
measured under, because a gate can be config-dependent and a shared default silently
measures it where it cannot run. The first version of this file used one `--refs 3`
BASE for every gate and reported `sub8_grain` as dead weight and `mbtree_grain` as
inert. Both were false. `sub8_grain` guards a split arm behind `num_refs == 1`, so at
refs 3 that arm is alive only on the first P frame of each GOP. Re-run at the shipped
default (refs 1), both gates earn their keep: +0.08/+0.16% and +4.18%.

Run: python bench/gate_audit.py [gate_name ...]
"""
import math
import os
import re
import subprocess
import sys

OURS = os.path.join("target", "release", "rusty_h264.exe")
GC = "_gc"
QPS = (22, 27, 32, 37)
DIM = {"grain_akiyo": (352, 288), "grain_flat": (352, 288), "akiyo_cif": (352, 288),
       "foreman_cif": (352, 288), "mobile_cif": (352, 288), "harbour_4cif": (704, 576),
       "FourPeople_1280x720_60": (1280, 720), "screen_text": (352, 288)}

# NO `--refs` HERE, DELIBERATELY. Every gate declares its own reference count in
# `extra`, because a gate can be CONFIGURATION-DEPENDENT and a single shared BASE
# silently measures it where it cannot run. `sub8_grain` sits behind
# `want_split = ... && num_refs == 1`: at `--refs 3` the split arm is alive only on
# the first P frame of each GOP, so an audit at refs 3 reports a gate that is
# switched off. The census used `EncoderConfig::new()` (num_ref_frames = 1) and saw
# it fire on 11484 MBs = 29 of 30 frames. Matching the configuration to the claim is
# the same rule as R5, applied to the tool instead of the operator.
BASE = ["--preset", "quality", "--cabac", "1", "--profile", "high",
        "--gop", "30", "--bframes", "0"]
REFS1 = ["--refs", "1"]   # the shipped default (num_ref_frames = 1)

# (knob, off-value, extra CLI, clips the gate ACTS on, control clip it must not touch)
#
# "acts on" comes from the measured census + the signal harvest, NOT from a guess.
# A gate with an empty acts-on list is REFUSED rather than reported: asking a grain
# veto about non-grain content measures the fallback and nothing else.
GATES = {
    "aq_grain_veto":  ("RFF_AQ_GRAIN",      "0", REFS1, ["grain_akiyo", "grain_flat"], "foreman_cif"),
    # REFS1 IS LOAD-BEARING here, not boilerplate: the split arm this gate vetoes
    # requires num_refs == 1.
    "sub8_grain":     ("RFF_SUB8_GRAIN",    "0", REFS1 + ["--sub8x8", "1"], ["grain_akiyo", "grain_flat"], "foreman_cif"),
    "mbtree_grain":   ("RFF_MBTREE_GRAIN",  "0", REFS1 + ["--mbtree", "1"], ["grain_akiyo", "grain_flat"], "foreman_cif"),
    # residual_frac never dropped below 0.03 anywhere in the census (0.0% on 6/6
    # clips), so there is no content here to measure. Recorded, not reported.
    "mbtree_backoff": ("RFF_MBTREE_RESMIN", "0", REFS1 + ["--mbtree", "1"], [], "foreman_cif"),
    "mbtree_spread":  ("RFF_MBTREE_SDMIN",  "1.111", REFS1 + ["--mbtree", "1"],
                       ["harbour_4cif", "foreman_cif", "mobile_cif"], "akiyo_cif"),
}


from bdmath import bd, polyfit3, ssim_db  # one home (plan A6)






def run(clip, qp, knob, val, extra):
    w, h = DIM[clip]
    bit, dec = os.path.join(GC, "ga.264"), os.path.join(GC, "ga.yuv")
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    if val is not None:
        env[knob] = val
    subprocess.run([OURS, "encode", "--width", str(w), "--height", str(h), "--qp", str(qp)]
                   + BASE + extra + ["--in", os.path.join(GC, clip + ".yuv"), "--out", bit],
                   capture_output=True, env=env)
    if not os.path.exists(bit) or os.path.getsize(bit) == 0:
        return None
    raw = open(bit, "rb").read()
    subprocess.run(["ffmpeg", "-v", "error", "-i", bit, "-f", "rawvideo",
                    "-pix_fmt", "yuv420p", "-y", dec], capture_output=True)
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "info",
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", dec,
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i",
                        os.path.join(GC, clip + ".yuv"),
                        "-lavfi", "ssim", "-f", "null", "-"], capture_output=True, text=True)
    m = re.findall(r"All:([0-9.]+)", r.stderr)
    return (len(raw), ssim_db(float(m[-1])), raw) if m else None


def audit(name):
    knob, off, extra, acts, control = GATES[name]
    print("=" * 84)
    print("GATE %s   (knob %s, off=%s)" % (name, knob, off))
    print("=" * 84)
    if not acts:
        print("  REFUSED: no clip in the corpus is on the side of the line this gate acts")
        print("  on (census: 0.0%% fired on 6/6 clips). Measuring it here would time the")
        print("  fallback and report it as the gate. Synthesize content that trips it first.")
        print()
        return
    for clip in acts:
        if not os.path.exists(os.path.join(GC, clip + ".yuv")):
            print("  %-24s (source missing)" % clip)
            continue
        on = [run(clip, q, knob, None, extra) for q in QPS]
        of = [run(clip, q, knob, off, extra) for q in QPS]
        on = [x for x in on if x]
        of = [x for x in of if x]
        if len(on) < 4 or len(of) < 4:
            print("  %-24s encode failed" % clip)
            continue
        if all(a[2] == b[2] for a, b in zip(on, of)):
            print("  %-24s byte-identical — the gate did NOT act here; not a test" % clip)
            continue
        v = bd([(x[0], x[1]) for x in on], [(x[0], x[1]) for x in of])
        # State the sign in terms of ARMS, never "the gate". For a gate whose shipped
        # state has been flipped, the `off` arm is the NON-shipped value, and a verdict
        # phrased as "the gate earns its keep" reads exactly backwards.
        if v < -0.05:
            verdict = "%s=%s WINS by %.2f%% -- shipped setting is costing us" % (knob, off, -v)
        elif v > 0.05:
            verdict = "shipped setting holds (%s=%s costs %+.2f%%)" % (knob, off, v)
        else:
            verdict = "neutral"
        print("  %-24s BD(%s=%s vs shipped) %+7.2f%%  %s" % (clip, knob, off, v, verdict))
        sys.stdout.flush()
    # CONTROL: a clip the gate must not touch. If it moves, the gate is not gated by
    # what we think it is and every number above is suspect.
    if os.path.exists(os.path.join(GC, control + ".yuv")):
        a = run(control, 27, knob, None, extra)
        b = run(control, 27, knob, off, extra)
        same = a and b and a[2] == b[2]
        print("  control %-16s %s" % (control, "byte-identical (good)" if same
                                      else "** MOVED — the gate touches content it should not **"))
    print()


if __name__ == "__main__":
    want = sys.argv[1:] or list(GATES)
    print("GREAT GATE AUDIT — BD-SSIM of GATE-OFF vs GATE-ON, on the acting content only.")
    print("Negative = removing the gate WINS = the gate is costing us.")
    print()
    for g in want:
        audit(g)
