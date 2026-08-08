# ENCODER head-to-head: rusty_h264 vs x264 — rate, quality AND speed on one ladder.
#
# x264 is encode-only, so this is the ONLY axis on which the two are directly
# comparable. (The decode side races ffmpeg's decoder — bench/decode_x264_speedtest.sh.)
#
# A single operating point cannot settle an encoder comparison: "we are Nx faster"
# is meaningless without naming the reference's preset, and "we are N% larger" is
# meaningless without naming the speed it was measured at. So this sweeps a PRESET
# LADDER on both sides and reports the speed/quality Pareto — every arm gets bytes,
# SSIM and pinned CPU time at four QPs.
#
# TWO PASSES, deliberately separated (2026-08-01):
#   * QUALITY ladder  — bytes + SSIM are DETERMINISTIC, so one run per point is enough
#     and no timing is claimed from it.
#   * SPEED pass      — arms ABBA-INTERLEAVED at one QP, N reps, pinned CPU time.
# They were fused, which meant every CPU-time number came from a SINGLE un-interleaved
# run. On a box that is always CPU-limited that is not a weak number, it is an invalid
# one: block-vs-block puts machine drift between the arms. Deterministic quantities and
# timed quantities have different validity requirements and must not share a loop.
#
# Method matches the decode bench: pinned to one core at High priority, CPU time
# (this box runs at 100% from unrelated processes and elapsed wall counts time spent
# descheduled), and the `$p.Handle` cache without which TotalProcessorTime reads
# empty after exit.
#
# WORK PARITY: both sides get the SAME keyint (= the frame count, so exactly one
# IDR each). Our `--gop 12` against x264's default keyint 250 put 5 IDRs against 1
# and charged us the difference -- a harness error worth tens of percent of BD-rate.
#
#   powershell -ExecutionPolicy Bypass -File bench/x264_headtohead.ps1 [-Frames 60]
param([int]$Frames = 60, [string]$X264 = "..\_ref_x264\x264.exe")

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$tmp = "$root\_h2h"; New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$ours = "$root\target\release\rusty_h264.exe"

# Untimed encode — used by the QUALITY ladder, where only bytes and SSIM are read.
function RunEnc([string]$exe, [string[]]$a) {
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\so.txt" -RedirectStandardError "$tmp\se.txt"
  $null = $p.Handle; $p.WaitForExit()
}

function PinRun([string]$exe, [string[]]$a) {
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\so.txt" -RedirectStandardError "$tmp\se.txt"
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High' } catch {}
  $p.WaitForExit()
  return $p.TotalProcessorTime.TotalMilliseconds
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

$clips = @("720p50_shields_ter","in_to_tree_420_720p50","720p5994_stockholm_ter")
$qps   = @(22,27,32,37)

# Both ladders carry the SAME coding tools (CABAC, B-frames, 3 refs) so the arms
# differ in EFFORT, not in capability — otherwise the comparison prices a feature
# gap as a speed gap.
# `--tune ssim` disables x264's PSY-RD. Its presets enable psy by default, which
# deliberately trades measured SSIM/PSNR for perceived quality -- with it on,
# `slower` scored WORSE than `medium` on BD-SSIM, which is the giveaway. Comparing
# coding efficiency by SSIM against a psy-tuned reference prices its perceptual
# tuning as a loss. Defaults are configuration: read the reference's help first.
$arms = @(
  @{side="x264"; name="veryfast"; args=@("--preset","veryfast","--tune","ssim","--profile","main")},
  @{side="x264"; name="medium";   args=@("--preset","medium","--tune","ssim","--profile","main")},
  @{side="x264"; name="slower";   args=@("--preset","slower","--tune","ssim","--profile","main")},
  @{side="ours"; name="fast";     args=@("--preset","fast","--cabac","1","--bframes","2","--refs","3")},
  @{side="ours"; name="balanced"; args=@("--cabac","1","--bframes","2","--refs","3")},
  @{side="ours"; name="quality";  args=@("--preset","quality","--cabac","1","--bframes","2","--refs","3")}
)

"clip,side,arm,qp,bytes,ssim"
foreach ($clip in $clips) {
  $y4m = "$root\video-tests\clips\$clip.y4m"
  $src = "$tmp\$clip.yuv"
  if (-not (Test-Path $src)) {
    $p = Start-Process -FilePath "ffmpeg" -ArgumentList @("-v","error","-i",$y4m,"-frames:v",$Frames,
         "-f","rawvideo","-pix_fmt","yuv420p","-y",$src) -PassThru -WindowStyle Hidden
    $null = $p.Handle; $p.WaitForExit()
  }
  foreach ($arm in $arms) {
    foreach ($qp in $qps) {
      $bit = "$tmp\o.264"; $dec = "$tmp\o.yuv"
      Remove-Item -Force -ErrorAction SilentlyContinue $bit,$dec
      if ($arm.side -eq "x264") {
        $a = $arm.args + @("--keyint","$Frames","--qp","$qp","--frames","$Frames","-o",$bit,$y4m)
        RunEnc $X264 $a
      } else {
        $a = @("encode","--width","1280","--height","720","--qp","$qp","--gop","$Frames") + $arm.args + @("--in",$src,"--out",$bit)
        RunEnc $ours $a
      }
      if (-not (Test-Path $bit)) { "$clip,$($arm.side),$($arm.name),$qp,ENCFAIL,"; continue }
      $bytes = (Get-Item $bit).Length
      # Decode with ffmpeg on BOTH sides so the quality number never depends on
      # whose decoder is under test.
      $p = Start-Process -FilePath "ffmpeg" -ArgumentList @("-v","error","-i",$bit,"-f","rawvideo",
           "-pix_fmt","yuv420p","-y",$dec) -PassThru -WindowStyle Hidden
      $null = $p.Handle; $p.WaitForExit()
      $ss = Ssim $dec $src 1280 720
      "{0},{1},{2},{3},{4},{5:F6}" -f $clip,$arm.side,$arm.name,$qp,$bytes,$ss
    }
  }
}

# ---------------------------------------------------------------------------
# SPEED PASS — one QP, arms ABBA-INTERLEAVED, pinned CPU time, N reps.
# Emitted separately from the quality ladder above so a timing number is never
# taken from a single un-interleaved run.
# ---------------------------------------------------------------------------
$speedQp = 27
$reps    = 5
Write-Output ""
Write-Output "clip,arm,rep,cpu_ms   # SPEED PASS qp=$speedQp, ABBA-interleaved, pinned, High priority"
foreach ($clip in $clips) {
  $y4m = "$root\video-tests\clips\$clip.y4m"
  $src = "$tmp\$clip.yuv"
  for ($r = 1; $r -le $reps; $r++) {
    # reverse the arm order on alternate reps so "the one that runs first" cancels
    $order = if ($r % 2 -eq 0) { $arms } else { $arms[($arms.Count-1)..0] }
    foreach ($arm in $order) {
      $bit = "$tmp\s.264"
      Remove-Item -Force -ErrorAction SilentlyContinue $bit
      if ($arm.side -eq "x264") {
        $a = $arm.args + @("--keyint","$Frames","--qp","$speedQp","--frames","$Frames","-o",$bit,$y4m)
        $cpu = PinRun $X264 $a
      } else {
        $a = @("encode","--width","1280","--height","720","--qp","$speedQp","--gop","$Frames") + $arm.args + @("--in",$src,"--out",$bit)
        $cpu = PinRun $ours $a
      }
      "{0},{1}:{2},{3},{4:F0}" -f $clip,$arm.side,$arm.name,$r,$cpu
    }
  }
}
