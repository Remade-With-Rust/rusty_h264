#!/usr/bin/env python3
"""WHERE DOES THE INTER-FRAME RATE GAP LIVE, per content class and per frame type?

docs/WHYS-p-frames.md closed D6/D2/D3 on ONE high-motion 720p clip and flagged the
B-vs-P split as probably content-dependent. This is that check.

MATCHED CONFIGURATION IS THE WHOLE POINT. The first pass at this comparison had three
asymmetries (bframes 2 vs 3, our QP cascade vs x264's ipratio/pbratio, AQ on one side)
and they did not cancel: they moved the answer from "P owns 86% of the excess" to
"B owns 61%". Both sides here run: one IDR, no scenecut, no frame-type QP cascade,
no AQ, 3 refs, 3 B-frames, single thread.

Reports, per clip: total ratio, per-frame-type bytes, and WHICH FRAME TYPE OWNS THE
EXCESS. Negative excess = we are ahead on that frame type.
"""
import math, os, re, subprocess, sys

X = os.path.join("..", "_ref_x264", "x264.exe")
OURS = os.path.join("target", "release", "rusty_h264.exe")
GC = "_gc"
CLIPS = [
    ("akiyo_cif", 352, 288, "smooth/static"),
    ("FourPeople_1280x720_60", 1280, 720, "smooth 720p"),
    ("foreman_cif", 352, 288, "medium motion"),
    ("mobile_cif", 352, 288, "detail + pan"),
    ("harbour_4cif", 704, 576, "detail + motion"),
    ("grain_akiyo", 352, 288, "grain"),
    ("screen_text", 352, 288, "screen"),
]
QP = 26


def frames_of(clip, w, h):
    return os.path.getsize(os.path.join(GC, clip + ".yuv")) // (w * h * 3 // 2)


def ftype_bytes(path):
    r = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0",
                        "-show_entries", "frame=pict_type,pkt_size", "-of", "csv=p=0", path],
                       capture_output=True, text=True)
    agg = {}
    for line in r.stdout.splitlines():
        p = line.strip().split(",")
        if len(p) < 2:
            continue
        sz, t = (p[0], p[1]) if p[0].isdigit() else (p[1], p[0])
        if not sz.isdigit():
            continue
        a = agg.setdefault(t, [0, 0])
        a[0] += int(sz); a[1] += 1
    return agg


def ssim_of(bit, clip, w, h):
    dec = os.path.join(GC, "isp.yuv")
    subprocess.run(["ffmpeg", "-v", "error", "-i", bit, "-f", "rawvideo",
                    "-pix_fmt", "yuv420p", "-y", dec], capture_output=True)
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "info",
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i", dec,
                        "-s", "%dx%d" % (w, h), "-pix_fmt", "yuv420p", "-i",
                        os.path.join(GC, clip + ".yuv"),
                        "-lavfi", "ssim", "-f", "null", "-"], capture_output=True, text=True)
    m = re.findall(r"All:([0-9.]+)", r.stderr)
    nf = os.path.getsize(dec) // (w * h * 3 // 2)
    return (float(m[-1]) if m else None), nf


print("INTER RATE GAP BY CONTENT AND FRAME TYPE -- matched configuration both sides.")
print("x264: --ipratio 1.0 --pbratio 1.0 --aq-mode 0 --ref 3 --bframes 3 --scenecut 0")
print("ours: --aq 0 --iqp-offset 0 --bqp-offset 0 --refs 3 --bframes 3")
print("=" * 104)
print("%-22s %-15s %9s %9s %7s   %s" % ("clip", "class", "x264 B", "ours B", "ratio", "excess owned by"))
print("-" * 104)
for clip, w, h, cls in CLIPS:
    src = os.path.join(GC, clip + ".yuv")
    if not os.path.exists(src):
        print("%-22s (missing)" % clip)
        continue
    n = frames_of(clip, w, h)
    xb = os.path.join(GC, "isp_x.264"); ob = os.path.join(GC, "isp_o.264")
    subprocess.run([X, "--preset", "veryfast", "--tune", "ssim", "--profile", "main",
                    "--scenecut", "0", "--threads", "1", "--keyint", str(n), "--qp", str(QP),
                    "--ipratio", "1.0", "--pbratio", "1.0", "--aq-mode", "0",
                    "--ref", "3", "--bframes", "3", "-o", xb, src,
                    "--input-res", "%dx%d" % (w, h)], capture_output=True)
    env = dict(os.environ); env["RUSTY_THREADS"] = "1"
    subprocess.run([OURS, "encode", "--width", str(w), "--height", str(h), "--qp", str(QP),
                    "--gop", str(n), "--preset", "fast", "--cabac", "1", "--bframes", "3",
                    "--refs", "3", "--aq", "0", "--iqp-offset", "0", "--bqp-offset", "0",
                    "--in", src, "--out", ob], capture_output=True, env=env)
    if not (os.path.exists(xb) and os.path.exists(ob)):
        print("%-22s ENCODE FAILED" % clip); continue
    ax, ao = ftype_bytes(xb), ftype_bytes(ob)
    sx, nfx = ssim_of(xb, clip, w, h)
    so, nfo = ssim_of(ob, clip, w, h)
    tx, to = os.path.getsize(xb), os.path.getsize(ob)
    if nfx != nfo or nfx != n:
        print("%-22s WORK PARITY FAIL frames x264=%s ours=%s src=%s" % (clip, nfx, nfo, n)); continue
    exc = to - tx
    parts = []
    for t in ("I", "P", "B"):
        dx = ao.get(t, [0, 0])[0] - ax.get(t, [0, 0])[0]
        parts.append((t, dx, 100.0 * dx / exc if exc else 0.0))
    owner = "  ".join("%s %+5.1f%%" % (t, s) for t, dx, s in parts)
    print("%-22s %-15s %9d %9d %6.2fx   %s   [SSIM %.4f vs %.4f]"
          % (clip[:21], cls, tx, to, to / tx, owner, sx, so))
    # per-frame-type detail
    for t in ("I", "P", "B"):
        if t in ax or t in ao:
            bx, cx = ax.get(t, [0, 0]); bo, co = ao.get(t, [0, 0])
            if cx or co:
                print("      %s  x264 %3d f %10d B (%7d/f)   ours %3d f %10d B (%7d/f)  %5.2fx"
                      % (t, cx, bx, bx // max(cx, 1), co, bo, bo // max(co, 1),
                         (bo / max(cx, 1)) / max(bx / max(cx, 1), 1e-9) if bx else 0))
    sys.stdout.flush()
