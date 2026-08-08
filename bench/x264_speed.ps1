# ENCODE SPEED: rusty_h264 vs x264 -- the speed pass, done so the number is admissible.
#
# ASCII ONLY. PowerShell 5.1 reads a .ps1 as ANSI unless it carries a BOM, so a UTF-8
# em-dash inside a STRING literal decodes to bytes that include a quote character and
# breaks parsing. (Cost one silent run: exit 1, zero rows, 2026-08-07.) Comments and
# strings here stay in the ASCII range.
#
# WHY THIS EXISTS SEPARATELY FROM x264_headtohead.ps1 (2026-08-07):
# That script's speed pass ran the 60-frame clips as-is, so `x264 --preset veryfast`
# finished in 781 ms. Measured spread across 5 reps was then 16-74%, MONOTONIC in run
# duration (slowest arm 15.6%, fastest arm 74.2%) -- the signature of a per-invocation
# overhead (process start, and the affinity/priority set that Start-Process can only
# apply AFTER launch) charged to a run too short to amortize it. That is a BIAS, not
# just noise: a fixed cost inflates the SHORTER arm by a larger fraction, which is how
# a harness manufactures a ratio. More reps cannot remove it; longer runs can.
#
# The corpus tops out at 60 frames, so inputs are LOOPED to $Frames. Legitimate for a
# speed measurement (same class of work per frame) and stated rather than hidden. NOT
# legitimate for quality -- BD-rate comes from the unlooped ladder elsewhere.
#
# THREADS ARE PINNED EXPLICITLY on both sides, and this is not cosmetic. x264's
# `threads=auto` reads the number of AVAILABLE cores, so pinning a process to one core
# silently makes x264 single-threaded -- and because frame threading restricts vertical
# MV range, that CHANGES ITS COMPRESSION by up to 3.4% (measured: slower/qp27/shields
# = 650672 bytes at threads=1 vs 672845 at auto). A harness that pins affinity is
# therefore also, invisibly, choosing the reference's coding efficiency. Set it out loud.
#
# WORK PARITY, both directions:
#   * keyint = $Frames on both sides => exactly one IDR each.
#   * `--scenecut 0` on x264: without it x264 reads each loop seam as a scene change
#     and inserts extra IDRs, encoding a different picture-type mix than we do.
#   * Frame counts of the produced streams are VERIFIED on rep 1; a mismatch VOIDS it.
#
# THE NULL ARM is the point of the `balanced` entry. The CLI accepts only fast|quality
# and defaults to fast, so `balanced` (passing no --preset) is BYTE-IDENTICAL work to
# `fast`. Their measured difference is a direct read of the harness floor: any ratio
# finer than that gap is not a result. Keep this arm.
#
#   powershell -ExecutionPolicy Bypass -File bench/x264_speed.ps1 [-Frames 300] [-Reps 4]
param([int]$Frames = 300, [int]$Reps = 4, [string]$X264 = "..\_ref_x264\x264.exe")

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$tmp = "$root\_h2hspeed"; New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$ours = "$root\target\release\rusty_h264.exe"
$env:RUSTY_THREADS = "1"    # inherited by Start-Process; pairs with x264 --threads 1

function PinRun([string]$exe, [string[]]$a) {
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\so.txt" -RedirectStandardError "$tmp\se.txt"
  $null = $p.Handle          # without this cache, TotalProcessorTime reads empty after exit
  try { $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High' } catch {}
  $p.WaitForExit()
  return $p.TotalProcessorTime.TotalMilliseconds
}

function FrameCount([string]$f) {
  $a = @("-v","error","-count_frames","-select_streams","v:0","-show_entries",
         "stream=nb_read_frames","-of","csv=p=0",$f)
  $p = Start-Process -FilePath "ffprobe" -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\fc.txt" -RedirectStandardError "$tmp\fe.txt"
  $null = $p.Handle; $p.WaitForExit()
  $t = (Get-Content "$tmp\fc.txt" -Raw)
  if ($null -eq $t) { return -1 }
  return [int]($t.Trim())
}

$clips = @("720p50_shields_ter","in_to_tree_420_720p50","720p5994_stockholm_ter")
$qp    = 27

# `ours:quality` costs ~10x the other arms; give it fewer reps rather than let it set
# the runtime of the whole matrix. N is reported per arm in the CSV.
$xcommon = @("--tune","ssim","--profile","main","--scenecut","0","--threads","1")
$arms = @(
  @{side="x264"; name="veryfast"; reps=$Reps; args=@("--preset","veryfast")+$xcommon},
  @{side="x264"; name="medium";   reps=$Reps; args=@("--preset","medium")+$xcommon},
  @{side="x264"; name="slower";   reps=$Reps; args=@("--preset","slower")+$xcommon},
  @{side="ours"; name="fast";     reps=$Reps; args=@("--preset","fast","--cabac","1","--bframes","2","--refs","3")},
  @{side="ours"; name="balanced"; reps=$Reps; args=@("--cabac","1","--bframes","2","--refs","3")},
  @{side="ours"; name="quality";  reps=[Math]::Max(2,[int]($Reps/2)); args=@("--preset","quality","--cabac","1","--bframes","2","--refs","3")}
)

# ---- input prep: loop each clip up to $Frames on BOTH sides -----------------
foreach ($clip in $clips) {
  $y4m  = "$root\video-tests\clips\$clip.y4m"
  $ly4m = "$tmp\${clip}_$Frames.y4m"; $lyuv = "$tmp\${clip}_$Frames.yuv"
  $loops = [Math]::Ceiling($Frames / 60.0)
  if (-not (Test-Path $ly4m)) {
    $p = Start-Process -FilePath "ffmpeg" -ArgumentList @("-v","error","-stream_loop","$loops","-i",$y4m,
         "-frames:v","$Frames","-f","yuv4mpegpipe","-y",$ly4m) -PassThru -WindowStyle Hidden
    $null = $p.Handle; $p.WaitForExit()
  }
  if (-not (Test-Path $lyuv)) {
    $p = Start-Process -FilePath "ffmpeg" -ArgumentList @("-v","error","-stream_loop","$loops","-i",$y4m,
         "-frames:v","$Frames","-f","rawvideo","-pix_fmt","yuv420p","-y",$lyuv) -PassThru -WindowStyle Hidden
    $null = $p.Handle; $p.WaitForExit()
  }
}

Write-Output "clip,arm,rep,cpu_ms   # qp=$qp frames=$Frames looped, threads=1 both sides, ABBA, pinned, High"
for ($r = 1; $r -le $Reps; $r++) {
  $order = if ($r % 2 -eq 0) { $arms } else { $arms[($arms.Count-1)..0] }
  foreach ($clip in $clips) {
    $ly4m = "$tmp\${clip}_$Frames.y4m"; $lyuv = "$tmp\${clip}_$Frames.yuv"
    foreach ($arm in $order) {
      if ($r -gt $arm.reps) { continue }
      $bit = "$tmp\s.264"
      Remove-Item -Force -ErrorAction SilentlyContinue $bit
      if ($arm.side -eq "x264") {
        $a = $arm.args + @("--keyint","$Frames","--qp","$qp","--frames","$Frames","-o",$bit,$ly4m)
        $cpu = PinRun $X264 $a
      } else {
        $a = @("encode","--width","1280","--height","720","--qp","$qp","--gop","$Frames") + $arm.args + @("--in",$lyuv,"--out",$bit)
        $cpu = PinRun $ours $a
      }
      "{0},{1}:{2},{3},{4:F0}" -f $clip,$arm.side,$arm.name,$r,$cpu
      # WORK PARITY, checked once per arm (rep 1) -- after the timing is taken, so the
      # ffprobe never lands inside a measured interval.
      if ($r -eq 1) {
        if (-not (Test-Path $bit)) { Write-Output "# ENCFAIL $clip $($arm.side):$($arm.name)" }
        else {
          $n = FrameCount $bit
          if ($n -ne $Frames) { Write-Output "# MISMATCH $clip $($arm.side):$($arm.name) = $n frames, expected $Frames -- COMPARISON VOID" }
          else { Write-Output "# ok $clip $($arm.side):$($arm.name) frames=$n" }
        }
      }
    }
  }
}
