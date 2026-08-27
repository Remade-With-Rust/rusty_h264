#!/usr/bin/env python3
"""Should the 8x8 transform be ON by default? PER CLIP, per slice-type mix.

x264 has it on by default and it covers ~45% of its intra macroblocks, which is the
motivation — but "x264 does it" is not a measurement of OUR encoder. R6 made the tool
available under both entropy coders; this decides whether it earns the default.

The rule this reports against (adaptive-is-the-default): a feature that wins on some
content and loses on other content is an unfinished DISPATCH, not a default. A single
mean across clips hides exactly the sign flip that decides the question, so nothing is
averaged here.

Two arms, everything else pinned:
    t8-off   --transform-8x8 0
    t8-ON    --transform-8x8 1
both --profile high (the flag needs High to be signalled at all), threads pinned.

BD-rate convention is the one in bench/examples/bdrate.rs: cubic polyfit of log10(rate)
against -10*log10(1-SSIM), integrated over the overlapping quality range.
NEGATIVE = 8x8 needs FEWER bits at equal quality = 8x8 wins.

  python bench/t8_default.py
"""
import math
import os
import re
import subprocess
import sys

OURS = os.path.join("target", "release", "rusty_h264.exe")
CLIPS = [
    ("akiyo_cif", 352, 288),
    ("foreman_cif", 352, 288),
    ("mobile_cif", 352, 288),
    ("harbour_4cif", 704, 576),
    ("FourPeople_1280x720_60", 1280, 720),
    ("grain_akiyo", 352, 288),
    ("screen_text", 352, 288),
]
QPS = (22, 27, 32, 37)
# Three coding structures, because the flag has a DIFFERENT syntax position and a
# different presence rule in each, and the RD trial that selects it is per-MB.
MIXES = [
    ("all-intra", ["--gop", "1", "--bframes", "0"]),
    ("I+P", ["--gop", "30", "--bframes", "0"]),
    ("I+P+B", ["--gop", "30", "--bframes", "2"]),
]


from bdmath import bd, polyfit3, ssim_db  # one home (plan A6)






# Optional arm: RFF_INTER8 value applied to BOTH arms, so the comparison stays
# t8-ON vs t8-OFF at a fixed inter-8x8 policy (0 = intra-only 8x8, 1 = always-RD,
# 2 = content-adaptive). The first run showed the loss is concentrated in I+P,
# which points at the INTER half of the tool rather than the transform itself.
INTER8 = sys.argv[1] if len(sys.argv) > 1 else None


def point(clip, w, h, qp, t8, mix_args):
    src = os.path.join("_gc", clip + ".yuv")
    bit = os.path.join("_gc", "t8_%d.264" % t8)
    dec = os.path.join("_gc", "t8_%d.yuv" % t8)
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    if INTER8 is not None:
        env["RFF_INTER8"] = INTER8
    args = ([OURS, "encode", "--width", str(w), "--height", str(h), "--qp", str(qp),
             "--preset", "quality", "--cabac", "1", "--profile", "high",
             "--transform-8x8", str(t8), "--refs", "3"] + mix_args
            + ["--in", src, "--out", bit])
    subprocess.run(args, capture_output=True, env=env)
    if not os.path.exists(bit) or os.path.getsize(bit) == 0:
        return None
    subprocess.run(["ffmpeg", "-v", "error", "-i", bit, "-f", "rawvideo",
                    "-pix_fmt", "yuv420p", "-y", dec], capture_output=True)
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "info",
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", dec,
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", src,
                        "-lavfi", "ssim", "-f", "null", "-"],
                       capture_output=True, text=True)
    m = re.findall(r"All:([0-9.]+)", r.stderr)
    if not m:
        return None
    return os.path.getsize(bit), ssim_db(float(m[-1]))


print("8x8 transform ON vs OFF — BD-rate (SSIM), PER CLIP. CABAC, High, quality preset.")
if INTER8 is not None:
    print("ARM: RFF_INTER8=%s on both sides (0 = intra-only 8x8)." % INTER8)
print("Negative = 8x8 needs FEWER bits at equal quality = 8x8 WINS.")
print("=" * 78)
print("%-24s %13s %13s %13s" % ("clip", "all-intra", "I+P", "I+P+B"))
print("-" * 78)
rows, losers = [], []
for clip, w, h in CLIPS:
    if not os.path.exists(os.path.join("_gc", clip + ".yuv")):
        print("%-24s  (source missing)" % clip[:23])
        continue
    row = "%-24s" % clip[:23]
    for name, mix in MIXES:
        off = [p for p in (point(clip, w, h, q, 0, mix) for q in QPS) if p]
        on = [p for p in (point(clip, w, h, q, 1, mix) for q in QPS) if p]
        v = bd(off, on)
        if v is None:
            row += "%14s" % "n/a"
            continue
        row += "%13.2f%%" % v
        rows.append((clip, name, v))
        if v > 0.05:
            losers.append((clip, name, v))
    print(row)
    sys.stdout.flush()

print("-" * 78)
if losers:
    print("SIGN FLIP — 8x8 LOSES on %d of %d cells:" % (len(losers), len(rows)))
    for c, m, v in losers:
        print("   %-24s %-10s %+.2f%%" % (c, m, v))
    print("Per adaptive-is-the-default: this is an unfinished DISPATCH, not a default.")
else:
    print("8x8 wins or is neutral on ALL %d cells — it earns the default." % len(rows))
