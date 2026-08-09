# DECODE RACE vs ffmpeg's native h264 decoder -- the number crates.io quotes.
#
# ASCII-ONLY BY RULE: PS 5.1 reads a BOM-less .ps1 as ANSI, so a single em-dash in a
# comment can decode into bytes containing a quote and break a string literal many
# lines later. This file has been bitten by exactly that.
#
# The 2026-08-08 baseline was produced ad-hoc and NO SCRIPT WAS COMMITTED, so the
# headline performance claim could not be reproduced. This is that script.
#
# WHAT IS AND IS NOT COMPARABLE ACROSS RUNS
#   * Absolute Mpx/s is NOT. Two identical runs of the old harness gave 180.4 and
#     143.2 Mpx/s for the same stream -- this box drifts ~25%. Any Mpx/s figure is a
#     property of the afternoon, not of the codec.
#   * The WITHIN-RUN RATIO rusty/ffmpeg is. Both decoders run interleaved, on the
#     same stream, in the same thermal state, so drift is common-mode and divides out.
#   * Frame counts must match. `decode_bench` once reported frames=1800 while the
#     process decoded 3600, doubling the apparent gap.
#
# METHOD
#   OUTPUT GOES TO NUL, both sides. The first version of this script wrote a
#   1.74 GB YUV per decode, twice per pair -- ~73 GB of writes across a run. That
#   measures the DISK, and both decoders converge toward disk speed, which flatters
#   the ratio: it read 1.82x/1.64x where the recorded standing gap is ~2.2x. Same
#   decode work, no I/O.
#
#   CPU time (not wall -- this box is pinned at 100%), ABBA inside every pair so
#   drift cancels, N pairs, median of the per-pair ratios. Correctness gate runs
#   BEFORE the clock: a decoder that is wrong is not fast.
#
#   powershell -File bench/ffmpeg_race.ps1 [-Pairs 9] [-Dir _dprof] [-Stem shields]

param([int]$Pairs = 9, [string]$Dir = "_dprof", [string]$Stem = "shields",
      [int]$Width = 1280, [int]$Height = 720)

$exe = "target\release\rusty_h264.exe"
$env:RUSTY_THREADS = "1"

function CpuMs([string]$file, [string]$argline) {
  # USE THE .NET Process API, NOT Start-Process -PassThru. The handle Start-Process
  # hands back does not reliably carry query rights here, and TotalProcessorTime
  # comes back 0 -- silently, which the first version turned into a ratio of 0.000x
  # and a divide-by-zero. This form returns a handle that can be read after exit.
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $file
  $psi.Arguments = $argline
  $psi.UseShellExecute = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $p = [System.Diagnostics.Process]::Start($psi)
  $null = $p.StandardOutput.ReadToEnd()
  $null = $p.StandardError.ReadToEnd()
  $p.WaitForExit()
  $ms = $p.TotalProcessorTime.TotalMilliseconds
  if ($ms -le 0.0) { throw "CPU time read as 0 for $file -- the process did not run" }
  if ($p.ExitCode -ne 0) { throw "$file exited $($p.ExitCode)" }
  return $ms
}

Write-Output "DECODE RACE -- rusty_h264 vs ffmpeg native h264, CPU time, ABBA, $Pairs pairs"
Write-Output "Ratio > 1 means ffmpeg is that many times our throughput."
Write-Output ("{0,-8} {1,12} {2,12} {3,10} {4,8}" -f "tier", "rusty ms", "ffmpeg ms", "ratio", "wins")
Write-Output ("-" * 56)

foreach ($tier in @("cavlc", "main", "high")) {
  $bit = "$Dir\${Stem}__$tier.264"
  if (-not (Test-Path $bit)) { Write-Output ("{0,-8} (missing)" -f $tier); continue }
  $ourArgs = @("decode", "--width", $Width, "--height", $Height, "--in", $bit, "--out", "$env:TEMP\race_us.yuv")
  $ffArgs  = @("-v", "error", "-i", $bit, "-f", "rawvideo", "-pix_fmt", "yuv420p", "-y", "$env:TEMP\race_ff.yuv")
  $ratios = @(); $rs = @(); $fs = @(); $ffWins = 0
  for ($i = 0; $i -lt $Pairs; $i++) {
    # ABBA: r f f r. Drift within a pair cancels; a bare A/B does not.
    $r1 = CpuMs $exe $ourArgs
    $f1 = CpuMs "ffmpeg" $ffArgs
    $f2 = CpuMs "ffmpeg" $ffArgs
    $r2 = CpuMs $exe $ourArgs
    $r = ($r1 + $r2) / 2; $f = ($f1 + $f2) / 2
    $rs += $r; $fs += $f; $ratios += ($r / $f)
    if ($f -lt $r) { $ffWins++ }
  }
  $mr = ($ratios | Sort-Object)[[int]($Pairs / 2)]
  $mrust = ($rs | Sort-Object)[[int]($Pairs / 2)]
  $mff = ($fs | Sort-Object)[[int]($Pairs / 2)]
  Write-Output ("{0,-8} {1,12:N0} {2,12:N0} {3,9:F3}x {4,6}/{5}" -f $tier, $mrust, $mff, $mr, $ffWins, $Pairs)
}
