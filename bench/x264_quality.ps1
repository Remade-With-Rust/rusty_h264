# ENCODE QUALITY: rusty_h264 vs x264 -- BD-rate on a per-clip, per-content-class ladder.
#
# ASCII ONLY (PowerShell 5.1 reads .ps1 as ANSI without a BOM; a UTF-8 dash inside a
# string literal decodes to bytes containing a quote and breaks parsing).
#
# WHAT THIS FIXES vs x264_headtohead.ps1's quality ladder (2026-08-07):
#
# 1. THREADS ARE PINNED ON BOTH SIDES. x264's `threads=auto` reads the number of
#    AVAILABLE cores, and frame threading restricts vertical MV range -- so thread
#    count changes x264's COMPRESSION. Measured on shields/slower/qp27:
#        threads=1    650672 bytes
#        threads=auto 672845 bytes   (+3.4%)
#    The old fused harness pinned every encode to one core, which silently forced
#    x264 to threads=1; the newer split harness left the quality ladder unpinned, so
#    x264 ran multi-threaded. That difference alone -- not any codec change -- moved
#    the reference between the 2026-07-31 baseline and 2026-08-07. A benchmark that
#    lets affinity choose the reference's coding efficiency is not reproducible.
#    Both sides are therefore fixed at ONE thread, stated in the output header.
#
# 2. PER-CLIP RESOLUTION. The old script hardcoded 1280x720 for our encoder AND for
#    the SSIM call, so it could only ever run three 720p clips. Resolution is now read
#    per clip, which is what lets the corpus span content CLASSES.
#
# 3. WIDER CORPUS. "Worst content class <= 0, verified per class" cannot be checked on
#    three clips of one resolution. Classes here: 720p detail/foliage/pan, 1080p high
#    motion, and CIF (the classic sequences the campaign's gates were fitted on).
#
# 4. NO `balanced` ARM. The CLI accepts only fast|quality and DEFAULTS to fast, so the
#    old `balanced` arm (which passed no --preset) was byte-identical to `fast` -- a
#    duplicate masquerading as a third operating point, confirmed by identical BD on
#    every clip. Our side has exactly TWO rungs; report two.
#
# WORK PARITY: keyint = frame count on both sides, so exactly one IDR each. Both sides
# read the SAME decoded frame count, and quality is measured by ffmpeg on BOTH sides so
# the number never depends on whose decoder is under test.
#
# Bytes and SSIM are DETERMINISTIC, so one run per point is enough and NO timing is
# claimed here -- speed lives in bench/x264_speed.ps1.
#
#   powershell -ExecutionPolicy Bypass -File bench/x264_quality.ps1
# -OursExe / -OnlyOurs support a BEFORE/AFTER of our own encoder against the SAME
# x264 anchors. Legitimate only because x264 is deterministic for a fixed arg set and
# our encoder is thread-count invariant (verified byte-identical md5 across
# RUSTY_THREADS=1/2/4/8/default), so the anchor points do not need re-running.
param([string]$X264 = "..\_ref_x264\x264.exe", [int]$MaxFrames = 120,
      [string]$OursExe = "", [switch]$OnlyOurs,
      [string[]]$X264Presets = @("veryfast","medium","slower"), [switch]$SkipOurs)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$tmp = "$root\_h2hq"; New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$ours = if ($OursExe) { $OursExe } else { "$root\target\release\rusty_h264.exe" }
$env:RUSTY_THREADS = "1"

function Run([string]$exe, [string[]]$a) {
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\so.txt" -RedirectStandardError "$tmp\se.txt"
  $null = $p.Handle; $p.WaitForExit(); return $p.ExitCode
}

function Probe([string]$f) {
  $a = @("-v","error","-count_frames","-select_streams","v:0","-show_entries",
         "stream=width,height,nb_read_frames","-of","csv=p=0",$f)
  $p = Start-Process -FilePath "ffprobe" -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\pr.txt" -RedirectStandardError "$tmp\pe.txt"
  $null = $p.Handle; $p.WaitForExit()
  $t = (Get-Content "$tmp\pr.txt" -Raw)
  if ($null -eq $t) { return $null }
  return $t.Trim().Split(",")
}

function Ssim([string]$dec, [string]$src, [int]$w, [int]$h) {
  $a = @("-hide_banner","-loglevel","info","-s","${w}x${h}","-pix_fmt","yuv420p","-i",$dec,
         "-s","${w}x${h}","-pix_fmt","yuv420p","-i",$src,"-lavfi","ssim","-f","null","-")
  $p = Start-Process -FilePath "ffmpeg" -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\x.txt" -RedirectStandardError "$tmp\ss.txt"
  $null = $p.Handle; $p.WaitForExit()
  $m = Select-String -Path "$tmp\ss.txt" -Pattern "All:([0-9.]+)" | Select-Object -Last 1
  if ($m) { return [double]$m.Matches[0].Groups[1].Value }
  return [double]::NaN
}

# class tag travels with the clip so the report can group by CONTENT CLASS, which is
# the unit the campaign's finish line is stated in.
$clips = @(
  @{name="720p50_shields_ter";      class="720p-detail"},
  @{name="in_to_tree_420_720p50";   class="720p-foliage"},
  @{name="720p5994_stockholm_ter";  class="720p-pan"},
  @{name="FourPeople_1280x720_60";  class="720p-lowmotion"},
  @{name="crowd_run_1080p50";       class="1080p-highmotion"},
  @{name="blue_sky_1080p25";        class="1080p-smooth"},
  @{name="bus_cif";                 class="cif-motion"},
  @{name="mobile_cif";              class="cif-texture"},
  @{name="foreman_cif";             class="cif-face"}
)
$qps = @(22,27,32,37)
$xcommon = @("--tune","ssim","--profile","main","--threads","1")
$arms = @()
foreach ($pp in $X264Presets) {
  # NOTE ultrafast/superfast force CAVLC and drop tools; that IS x264's operating
  # point at that speed, so it belongs on the Pareto curve. --profile main is dropped
  # for them because ultrafast is baseline-only and x264 errors on the combination.
  $xa = if ($pp -eq "ultrafast" -or $pp -eq "superfast") {
          @("--preset",$pp,"--tune","ssim","--threads","1")
        } else { @("--preset",$pp)+$xcommon }
  $arms += @{side="x264"; name=$pp; args=$xa}
}
$arms += @{side="ours"; name="fast";    args=@("--preset","fast","--cabac","1","--bframes","2","--refs","3")}
$arms += @{side="ours"; name="quality"; args=@("--preset","quality","--cabac","1","--bframes","2","--refs","3")}
if ($OnlyOurs) { $arms = $arms | Where-Object { $_.side -eq "ours" } }
if ($SkipOurs) { $arms = $arms | Where-Object { $_.side -eq "x264" } }

Write-Output "# threads=1 BOTH sides (x264 --threads 1, RUSTY_THREADS=1); keyint=frames => one IDR each"
Write-Output "clip,class,side,arm,qp,width,height,frames,bytes,ssim"
foreach ($c in $clips) {
  $clip = $c.name
  $y4m = "$root\video-tests\clips\$clip.y4m"
  if (-not (Test-Path $y4m)) { Write-Output "# MISSING $clip"; continue }
  $info = Probe $y4m
  if ($null -eq $info -or $info.Count -lt 3) { Write-Output "# PROBEFAIL $clip"; continue }
  $w = [int]$info[0]; $h = [int]$info[1]; $n = [Math]::Min([int]$info[2], $MaxFrames)
  $src = "$tmp\${clip}_$n.yuv"
  if (-not (Test-Path $src)) {
    $null = Run "ffmpeg" @("-v","error","-i",$y4m,"-frames:v","$n","-f","rawvideo","-pix_fmt","yuv420p","-y",$src)
  }
  foreach ($arm in $arms) {
    foreach ($qp in $qps) {
      $bit = "$tmp\o.264"; $dec = "$tmp\o.yuv"
      Remove-Item -Force -ErrorAction SilentlyContinue $bit,$dec
      if ($arm.side -eq "x264") {
        $a = $arm.args + @("--keyint","$n","--qp","$qp","--frames","$n","-o",$bit,$y4m)
        $null = Run $X264 $a
      } else {
        $a = @("encode","--width","$w","--height","$h","--qp","$qp","--gop","$n") + $arm.args + @("--in",$src,"--out",$bit)
        $null = Run $ours $a
      }
      if (-not (Test-Path $bit)) { Write-Output "$clip,$($c.class),$($arm.side),$($arm.name),$qp,$w,$h,$n,ENCFAIL,"; continue }
      $bytes = (Get-Item $bit).Length
      $null = Run "ffmpeg" @("-v","error","-i",$bit,"-f","rawvideo","-pix_fmt","yuv420p","-y",$dec)
      # WORK PARITY: the decoded stream must carry the frames we paid for. A short
      # decode would make bytes-per-quality look better than it is.
      $dn = [Math]::Floor((Get-Item $dec).Length / ($w * $h * 3 / 2))
      $ss = Ssim $dec $src $w $h
      if ($dn -ne $n) { Write-Output "# MISMATCH $clip $($arm.side):$($arm.name) qp$qp decoded $dn of $n" }
      "{0},{1},{2},{3},{4},{5},{6},{7},{8},{9:F6}" -f $clip,$c.class,$arm.side,$arm.name,$qp,$w,$h,$dn,$bytes,$ss
    }
  }
  Write-Output "# done $clip ($w x $h, $n frames)"
}
