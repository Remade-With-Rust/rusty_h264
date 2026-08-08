# CAMPAIGN BEFORE/AFTER: encoder SPEED, our own binary against itself.
#
# ASCII ONLY (PS 5.1 reads .ps1 as ANSI without a BOM).
#
# WHY: bench/campaign_delta.py measured the Great Gate campaign's BD-rate effect and
# found it near-neutral (ours:quality median -0.53%). That is only half the claim. The
# campaign's encoder work was a SPEED play -- making sub-8x8 and intra-RD affordable
# (best_part 5.59x -> ~4x) -- so the quantity it must be judged on is COST AT EQUAL
# QUALITY. A BD-neutral change that bought a large speedup is a win; a BD-neutral change
# that bought nothing is not. Neither can be read from the BD table alone.
#
# BEFORE = 3bf242d (release 0.8.0), parent of a6e45f7 "the Great Gate campaign".
# AFTER  = the working-tree build.
#
# Method as bench/x264_speed.ps1: inputs LOOPED to $Frames so per-invocation overhead
# is a small fraction of every arm (at 60 frames the fastest arm ran 781 ms and the
# measured spread hit 74%); pinned to one core at High priority; CPU time not wall;
# arms ABBA-interleaved. RUSTY_THREADS=1 on both -- our encoder is thread-count
# invariant (byte-identical md5 across 1/2/4/8) so this costs no generality.
#
# NULL ARM: `new:fastdup` runs the SAME binary and args as `new:fast`. Its gap to
# `new:fast` is the harness floor; no before/after difference smaller than that is real.
#
#   powershell -ExecutionPolicy Bypass -File bench/campaign_speed.ps1 [-Frames 150] [-Reps 3]
param([int]$Frames = 150, [int]$Reps = 3,
      [string]$PreExe = "C:\Users\talmo\coding\rs_h264_pre\target\release\rusty_h264.exe",
      [string]$OldDefExe = "C:\Users\talmo\coding\rs_h264_olddef\target\release\rusty_h264.exe")

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$tmp = "$root\_cspeed"; New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$new = "$root\target\release\rusty_h264.exe"
$env:RUSTY_THREADS = "1"

function PinRun([string]$exe, [string[]]$a) {
  $p = Start-Process -FilePath $exe -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$tmp\so.txt" -RedirectStandardError "$tmp\se.txt"
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High' } catch {}
  $p.WaitForExit()
  return $p.TotalProcessorTime.TotalMilliseconds
}

$clips = @("720p50_shields_ter","in_to_tree_420_720p50","720p5994_stockholm_ter")
$qp = 27
$arms = @(
  @{tag="pre:fast";     exe=$PreExe; args=@("--preset","fast")},
  @{tag="new:fast";     exe=$new;    args=@("--preset","fast")},
  @{tag="new:fastdup";  exe=$new;    args=@("--preset","fast")},
  @{tag="pre:quality";  exe=$PreExe;    args=@("--preset","quality")},
  @{tag="odef:quality"; exe=$OldDefExe; args=@("--preset","quality")},
  @{tag="new:quality";  exe=$new;       args=@("--preset","quality")},
  @{tag="odef:fast";    exe=$OldDefExe; args=@("--preset","fast")}
)
$common = @("--cabac","1","--bframes","2","--refs","3")

foreach ($clip in $clips) {
  $y4m = "$root\video-tests\clips\$clip.y4m"
  $lyuv = "$tmp\${clip}_$Frames.yuv"
  if (-not (Test-Path $lyuv)) {
    $loops = [Math]::Ceiling($Frames / 60.0)
    $p = Start-Process -FilePath "ffmpeg" -ArgumentList @("-v","error","-stream_loop","$loops","-i",$y4m,
         "-frames:v","$Frames","-f","rawvideo","-pix_fmt","yuv420p","-y",$lyuv) -PassThru -WindowStyle Hidden
    $null = $p.Handle; $p.WaitForExit()
  }
}

Write-Output "clip,arm,rep,cpu_ms,bytes   # qp=$qp frames=$Frames looped, threads=1, ABBA, pinned, High"
for ($r = 1; $r -le $Reps; $r++) {
  $order = if ($r % 2 -eq 0) { $arms } else { $arms[($arms.Count-1)..0] }
  foreach ($clip in $clips) {
    $lyuv = "$tmp\${clip}_$Frames.yuv"
    foreach ($arm in $order) {
      $bit = "$tmp\s.264"
      Remove-Item -Force -ErrorAction SilentlyContinue $bit
      $a = @("encode","--width","1280","--height","720","--qp","$qp","--gop","$Frames") +
           $arm.args + $common + @("--in",$lyuv,"--out",$bit)
      $cpu = PinRun $arm.exe $a
      # bytes are recorded so a speed change can never be confused with a work change:
      # if the two binaries emit different byte counts the comparison is speed AT A
      # DIFFERENT OPERATING POINT, which the report must say out loud.
      $sz = if (Test-Path $bit) { (Get-Item $bit).Length } else { 0 }
      "{0},{1},{2},{3:F0},{4}" -f $clip,$arm.tag,$r,$cpu,$sz
    }
  }
}
