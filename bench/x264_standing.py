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
def ssim_db(s): return -10.0*math.log10(max(1.0-s,1e-9))
def polyfit3(xs,ys):
    a=[[0.0]*4 for _ in range(4)]; b=[0.0]*4
    for x,y in zip(xs,ys):
        xp=[1.0]
        for p in range(1,7): xp.append(xp[p-1]*x)
        for j in range(4):
            for k in range(4): a[j][k]+=xp[j+k]
            b[j]+=y*xp[j]
    for c in range(4):
        piv=max(range(c,4),key=lambda r:abs(a[r][c])); a[c],a[piv]=a[piv],a[c]; b[c],b[piv]=b[piv],b[c]
        for r in range(4):
            if r!=c and a[c][c]:
                f=a[r][c]/a[c][c]
                for k in range(c,4): a[r][k]-=f*a[c][k]
                b[r]-=f*b[c]
    return [b[i]/a[i][i] if a[i][i] else 0.0 for i in range(4)]
def bd(anchor,test):
    def prep(p):
        v=sorted((d,math.log10(r)) for r,d in p); return [x[0] for x in v],[x[1] for x in v]
    if len(anchor)<4 or len(test)<4: return None
    da,la=prep(anchor); dt,lt=prep(test)
    ca,ct=polyfit3(da,la),polyfit3(dt,lt)
    lo,hi=max(da[0],dt[0]),min(da[-1],dt[-1])
    if hi<=lo: return None
    I=lambda c,x:c[0]*x+c[1]*x*x/2+c[2]*x**3/3+c[3]*x**4/4
    return (10.0**(((I(ct,hi)-I(ct,lo))-(I(ca,hi)-I(ca,lo)))/(hi-lo))-1)*100
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
