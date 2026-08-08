#!/usr/bin/env python3
"""ALL-INTRA head-to-head vs x264 — the cleanest comparison axis we have.

All-intra removes every inter confound: no motion search, no reference management, no
B-pyramid, no lookahead. What is left is transform + quantization + intra prediction +
entropy coding. The project's record claims we WIN on all-intra and lose on inter; this
checks whether that is still true, per clip, on the same corpus the gate census uses.

Both sides forced to every-frame-keyframe:
    ours   --gop 1
    x264   --keyint 1  (plus --tune ssim --threads 1, matching x264_quality.ps1's
                        reasoning: psy-rd deliberately trades measured SSIM, and thread
                        count changes x264's compression)

BD-rate convention is bit-for-bit the one in bench/examples/bdrate.rs: cubic polyfit of
log10(rate) against -10*log10(1-SSIM), integrated over the overlapping quality range.
Negative = WE need fewer bits at equal quality.

PER CLIP ONLY. A mean across clips hides the sign flips that matter.

  python bench/intra_vs_x264.py
"""
import math
import os
import re
import subprocess

OURS = os.path.join("target", "release", "rusty_h264.exe")
X264 = os.path.join("..", "_ref_x264", "x264.exe")
CLIPS = [
    ("grain_akiyo", 352, 288),
    ("screen_text", 352, 288),
    ("akiyo_cif", 352, 288),
    ("mobile_cif", 352, 288),
    ("foreman_cif", 352, 288),
    ("harbour_4cif", 704, 576),
    ("FourPeople_1280x720_60", 1280, 720),
]
QPS = (22, 27, 32, 37)


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


def ssim_of(bit, src, w, h):
    dec = os.path.join("_gc", "iv_d.yuv")
    subprocess.run(["ffmpeg", "-v", "error", "-i", bit, "-f", "rawvideo",
                    "-pix_fmt", "yuv420p", "-y", dec], capture_output=True)
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "info",
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", dec,
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", src,
                        "-lavfi", "ssim", "-f", "null", "-"],
                       capture_output=True, text=True)
    m = re.findall(r"All:([0-9.]+)", r.stderr)
    return ssim_db(float(m[-1])) if m else None


def run_ours(clip, w, h, qp, t8=False):
    src = os.path.join("_gc", clip + ".yuv")
    bit = os.path.join("_gc", "iv_o.264")
    env = dict(os.environ)
    env["RUSTY_THREADS"] = "1"
    # PIN the arm, never rely on the absence of an override. The 8x8 transform
    # became DEFAULT-ON on 2026-08-08, so the old `if t8: args += [...]` form
    # silently made both arms identical -- the exact trap that once printed
    # "IDENTICAL, no effect" for a knob that was on in both arms.
    args = [OURS, "encode", "--width", str(w), "--height", str(h), "--qp", str(qp),
            "--gop", "1", "--preset", "quality", "--cabac", "1",
            "--transform-8x8", "1" if t8 else "0",
            "--in", src, "--out", bit]
    subprocess.run(args, capture_output=True, env=env)
    return os.path.getsize(bit), ssim_of(bit, src, w, h)


def run_x264(clip, w, h, qp, preset):
    y4m = os.path.join("video-tests", "clips", clip + ".y4m")
    src = os.path.join("_gc", clip + ".yuv")
    bit = os.path.join("_gc", "iv_x.264")
    subprocess.run([X264, "--preset", preset, "--tune", "ssim", "--threads", "1",
                    "--keyint", "1", "--qp", str(qp), "--frames", "60",
                    "-o", bit, y4m], capture_output=True)
    return os.path.getsize(bit), ssim_of(bit, src, w, h)


PRESETS = ["veryfast", "medium", "slower"]
print("ALL-INTRA BD-rate vs x264 (SSIM), per clip. threads=1 both sides, every frame a keyframe.")
print("Negative = WE need fewer bits at equal quality.")
print("=" * 92)
print("%-18s %-8s %13s %13s %13s" % ("clip", "arm", "vs veryfast", "vs medium", "vs slower"))
print("-" * 92)
for clip, w, h in CLIPS:
    if not os.path.exists(os.path.join("_gc", clip + ".yuv")):
        print("%-26s  (source missing)" % clip[:25])
        continue
    arms = {}
    for label, t8 in (("t8-off", False), ("t8-ON", True)):
        pts = []
        for qp in QPS:
            by, sd = run_ours(clip, w, h, qp, t8)
            if sd is not None:
                pts.append((by, sd))
        arms[label] = pts
    for label in ("t8-off", "t8-ON"):
        row = "%-18s %-8s" % (clip[:17], label)
        for pre in PRESETS:
            ref = []
            for qp in QPS:
                by, sd = run_x264(clip, w, h, qp, pre)
                if sd is not None:
                    ref.append((by, sd))
            v = bd(ref, arms[label])
            row += ("%12.1f%%" % v) if v is not None else "%13s" % "n/a"
        print(row)
