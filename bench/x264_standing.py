#!/usr/bin/env python3
"""WHERE WE STAND vs x264 veryfast -- BD-rate, per clip, DEFAULTS on both sides.

This is the PRODUCT number: each encoder run the way it ships, so it includes every
default (QP cascade, AQ, B-frame count, ref count). That is deliberately NOT the
matched-configuration comparison in docs/WHYS-p-frames.md, which strips those to
isolate coding efficiency. Both are legitimate; they answer different questions, and
conflating them is how the first pass at this got a decomposition backwards.

NEGATIVE = we need FEWER bits than x264 at equal quality = we win.
"""
import math, os, re, subprocess, sys
X=os.path.join("..","_ref_x264","x264.exe"); OURS=os.path.join("target","release","rusty_h264.exe")
GC="_gc"; QPS=(22,27,32,37)
CLIPS=[("akiyo_cif",352,288),("foreman_cif",352,288),("mobile_cif",352,288),
       ("harbour_4cif",704,576),("FourPeople_1280x720_60",1280,720),
       ("grain_akiyo",352,288),("screen_text",352,288)]
from bdmath import bd, polyfit3, ssim_db  # one home (plan A6)
def ssim_of(bit,clip,w,h):
    dec=os.path.join(GC,"st.yuv")
    subprocess.run(["ffmpeg","-v","error","-i",bit,"-f","rawvideo","-pix_fmt","yuv420p","-y",dec],capture_output=True)
    r=subprocess.run(["ffmpeg","-hide_banner","-loglevel","info","-s","%dx%d"%(w,h),"-pix_fmt","yuv420p","-i",dec,
        "-s","%dx%d"%(w,h),"-pix_fmt","yuv420p","-i",os.path.join(GC,clip+".yuv"),
        "-lavfi","ssim","-f","null","-"],capture_output=True,text=True)
    m=re.findall(r"All:([0-9.]+)",r.stderr)
    return ssim_db(float(m[-1])) if m else None
print("STANDING vs x264 veryfast -- BD-rate (SSIM), per clip, DEFAULTS both sides.")
print("Negative = WE need fewer bits at equal quality.")
print("%-24s %13s %13s"%("clip","ours fast","ours balanced"))
print("-"*54)
tf=[];tb=[]
for clip,w,h in CLIPS:
    src=os.path.join(GC,clip+".yuv")
    if not os.path.exists(src): continue
    n=os.path.getsize(src)//(w*h*3//2)
    ref=[]
    for q in QPS:
        b=os.path.join(GC,"st_x.264")
        subprocess.run([X,"--preset","veryfast","--tune","ssim","--threads","1","--scenecut","0",
            "--keyint",str(n),"--qp",str(q),"-o",b,src,"--input-res","%dx%d"%(w,h)],capture_output=True)
        s=ssim_of(b,clip,w,h)
        if s is not None: ref.append((os.path.getsize(b),s))
    row="%-24s"%clip[:23]
    for pre,acc in (("fast",tf),("balanced",tb)):
        pts=[]
        for q in QPS:
            b=os.path.join(GC,"st_o.264"); env=dict(os.environ); env["RUSTY_THREADS"]="1"
            subprocess.run([OURS,"encode","--width",str(w),"--height",str(h),"--qp",str(q),
                "--gop",str(n),"--preset",pre,"--in",src,"--out",b],capture_output=True,env=env)
            if os.path.exists(b) and os.path.getsize(b)>0:
                s=ssim_of(b,clip,w,h)
                if s is not None: pts.append((os.path.getsize(b),s))
        v=bd(ref,pts)
        row+=("%12.1f%%"%v) if v is not None else "%13s"%"n/a"
        if v is not None: acc.append(v)
    print(row); sys.stdout.flush()
print("-"*54)
for nm,v in (("fast",tf),("balanced",tb)):
    if v: print("  ours %-9s wins %d/%d   worst %+.1f%%   best %+.1f%%"%(nm,sum(1 for z in v if z<0),len(v),max(v),min(v)))
