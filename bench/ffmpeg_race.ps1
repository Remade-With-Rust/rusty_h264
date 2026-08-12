# DECODE RACE vs ffmpeg native h264 -- the standing number.
#
# ASCII-ONLY BY RULE: PS 5.1 reads a BOM-less .ps1 as ANSI; a single em-dash in a
# comment has previously broken string literals many lines later.
#
# ASSUME THE PREVIOUS SCRIPT WAS WRONG. The 2026-08-09 baseline claimed NUL sink
# and ffmpeg -threads 1; the committed script still wrote TEMP YUVs via the CLI
# (decode_stream accumulates every frame then writes) and launched ffmpeg with
# default thread count. Those four defects all flattered us. This rewrite is the
# measurement-clean shape from codec-measurement + pinvs.ps1.
#
# DESIRED STATE (enforced, not aspirational)
#   1. Correctness gate BEFORE any timed pair: ours and ffmpeg YUV byte-identical
#      on a short probe (PROBE_FRAMES), then frame-count parity on the FULL stream.
#   2. Timed ours arm = decode_bench (AU decode, drop pictures -- same work as
#      ffmpeg -f null). NEVER the CLI: cmd_decode builds Vec<YuvFrame> + write.
#   3. Timed ffmpeg arm = -threads 1 -f null -  (discard, single core).
#   4. Pin both arms to one core at High priority; measure CPU time (not wall).
#   5. Print cores-busy = cpu/wall every pair -- two arms at different parallelism
#      are not comparable, and no other column says so.
#   6. ABBA inside every pair; N pairs; paired win-rate + z-score.
#   7. NULL arm (ours vs ours) first -- the harness floor. If |null-1| is large,
#      stop believing ratios.
#   8. Refuse to quote a ratio when either median arm is < ~15 s (timer quantisation
#      / startup fraction). Absolute Mpx/s is NOT comparable across sessions;
#      the within-run ratio is.
#
# Usage:
#   powershell -NoProfile -File bench/ffmpeg_race.ps1 [-Pairs 9] [-Dir _dprof] [-Stem shields]
#   powershell -NoProfile -File bench/ffmpeg_race.ps1 -SkipGate   # only if gate already ran
#
# Env:
#   FFMPEG_BIN   path to ffmpeg (default: ffmpeg on PATH)
#   RACE_CORE    processor affinity mask (default: 4 = core 2; avoid core 0)

param(
  [int]$Pairs = 9,
  [string]$Dir = "_dprof",
  [string]$Stem = "shields",
  [int]$Width = 1280,
  [int]$Height = 720,
  [int]$ProbeFrames = 60,
  [switch]$SkipGate,
  [switch]$SkipNull
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$FFmpeg = if ($env:FFMPEG_BIN) { $env:FFMPEG_BIN } else { "ffmpeg" }
$Bench  = Join-Path $Root "target\release\examples\decode_bench.exe"
$Affinity = if ($env:RACE_CORE) { [IntPtr][int]$env:RACE_CORE } else { [IntPtr]4 }

if (-not (Test-Path $Bench)) {
  throw "missing $Bench -- build:`n  cargo build --release -p rusty_h264-decoder --features asm --example decode_bench"
}

# --- pinned invoke -----------------------------------------------------------
# USE THE .NET Process API, NOT Start-Process -PassThru. Start-Process handles
# have previously returned TotalProcessorTime=0 here (silent 0.000x ratio).
# Cache .Handle BEFORE WaitForExit. Never Refresh() an exited process (zeros counters).
#
# CAPTURE DEADLOCK (D6-H6, 2026-08-10): never ReadToEnd stdout then stderr.
# ffmpeg -loglevel info writes a progress line per frame to stderr; on a long
# stream the OS pipe buffer fills, ffmpeg blocks on write, and the harness sits
# forever on stdout.ReadToEnd. Fix: drain BOTH pipes concurrently, or do not
# redirect. Timed arms use Capture=$false (no pipes). Frame-count uses
# -progress file instead of scraping info-level stderr.
function Invoke-Pinned {
  param(
    [string]$Exe,
    [string[]]$ArgList,
    [switch]$Capture
  )
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $Exe
  $psi.Arguments = (
    $ArgList | ForEach-Object {
      $a = "$_"
      if ($a -match '[\s"]') { '"' + ($a -replace '\\', '\\' -replace '"', '\"') + '"' } else { $a }
    }
  ) -join ' '
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = [bool]$Capture
  $psi.RedirectStandardError  = [bool]$Capture
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = [System.Diagnostics.Process]::Start($psi)
  $null = $p.Handle
  try {
    $p.ProcessorAffinity = $Affinity
    $p.PriorityClass = 'High'
  } catch {
    # Affinity can fail under some job objects; still measure CPU.
  }
  $stdout = ''; $stderr = ''
  if ($Capture) {
    # Concurrent drain -- sequential ReadToEnd deadlocks when the other pipe fills.
    $outTask = $p.StandardOutput.ReadToEndAsync()
    $errTask = $p.StandardError.ReadToEndAsync()
    $p.WaitForExit()
    $stdout = $outTask.GetAwaiter().GetResult()
    $stderr = $errTask.GetAwaiter().GetResult()
  } else {
    $p.WaitForExit()
  }
  $sw.Stop()
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  if ($cpu -le 0.0) { throw "CPU time read as 0 for $Exe -- instrument failed (exit $($p.ExitCode))" }
  if ($p.ExitCode -ne 0) { throw "$Exe exited $($p.ExitCode)`n$stderr" }
  [pscustomobject]@{
    CpuMs  = $cpu
    WallMs = $sw.Elapsed.TotalMilliseconds
    Cores  = $cpu / [Math]::Max($sw.Elapsed.TotalMilliseconds, 1.0)
    StdOut = $stdout
    StdErr = $stderr
    Exit   = $p.ExitCode
  }
}

function Get-Median([double[]]$xs) {
  $s = $xs | Sort-Object
  $s[[int]($s.Count / 2)]
}

function Get-OurFrames([string]$Bit) {
  Write-Host "  ... ours frame count: $Bit"
  $r = Invoke-Pinned -Exe $Bench -ArgList @($Bit, '1') -Capture
  if ($r.StdOut -notmatch 'frames=(\d+)') { throw "decode_bench did not print frames=: $($r.StdOut)" }
  return [int]$Matches[1]
}

function Get-FfFrames([string]$Bit) {
  # Do NOT scrape -loglevel info stderr (pipe deadlock on long streams, D6-H6).
  # -progress writes machine-readable keys to a file; -nostats keeps stderr quiet.
  # Invoke WITHOUT Capture -- no redirected pipes at all.
  $prog = Join-Path $env:TEMP ("race_ff_progress_{0}.txt" -f [guid]::NewGuid().ToString('N'))
  Write-Host "  ... ffmpeg frame count via -progress: $Bit"
  try {
    $null = Invoke-Pinned -Exe $FFmpeg -ArgList @(
      '-hide_banner', '-loglevel', 'error', '-nostats', '-threads', '1',
      '-progress', $prog, '-i', $Bit, '-f', 'null', '-'
    )
    if (-not (Test-Path $prog)) { throw "ffmpeg wrote no progress file $prog" }
    $text = Get-Content -Raw $prog
    $m = [regex]::Matches($text, '(?m)^frame=(\d+)\s*$')
    if ($m.Count -eq 0) { throw "ffmpeg progress lacked frame=: $($text.Substring(0, [Math]::Min(200, $text.Length)))" }
    return [int]$m[$m.Count - 1].Groups[1].Value
  } finally {
    Remove-Item $prog -Force -ErrorAction SilentlyContinue
  }
}

function Assert-ByteIdentical([string]$Bit, [string]$Tag) {
  $probeDir = Join-Path $Dir '_race_probe'
  New-Item -ItemType Directory -Force -Path $probeDir | Out-Null
  $ourYuv = Join-Path $probeDir "${Tag}_ours.yuv"
  $ffYuv  = Join-Path $probeDir "${Tag}_ff.yuv"

  # Short probe only -- full-stream YUV at 720p x 1800 is ~1.7 GB and measures disk.
  # Both arms decode the SAME first N display pictures from the FULL bitstream
  # (decode_bench maxf=; ffmpeg -frames:v). No remux: remux can drop parameter sets.
  & $FFmpeg -y -hide_banner -loglevel error -threads 1 -i $Bit `
    -frames:v $ProbeFrames -f rawvideo -pix_fmt yuv420p $ffYuv
  if ($LASTEXITCODE -ne 0) { throw "ffmpeg probe decode failed for $Tag" }

  $r = Invoke-Pinned -Exe $Bench -ArgList @(
    $Bit, '1', "maxf=$ProbeFrames", "out=$ourYuv"
  ) -Capture
  if ($r.StdOut -notmatch 'frames=(\d+)') { throw "ours probe missing frames=: $($r.StdOut)" }
  $got = [int]$Matches[1]
  if ($got -ne $ProbeFrames) {
    throw "PROBE FRAME COUNT $Tag ours=$got expected=$ProbeFrames"
  }

  $bOurs = (Get-Item $ourYuv).Length
  $bFf   = (Get-Item $ffYuv).Length
  if ($bOurs -ne $bFf) {
    throw "PROBE SIZE MISMATCH $Tag ours=$bOurs ff=$bFf -- comparison VOID"
  }
  $sha = [System.Security.Cryptography.SHA256]::Create()
  $fs1 = [IO.File]::OpenRead($ourYuv)
  $fs2 = [IO.File]::OpenRead($ffYuv)
  try {
    $h1 = [BitConverter]::ToString($sha.ComputeHash($fs1))
    $h2 = [BitConverter]::ToString($sha.ComputeHash($fs2))
  } finally {
    $fs1.Dispose(); $fs2.Dispose(); $sha.Dispose()
  }
  Remove-Item $ourYuv, $ffYuv -Force -ErrorAction SilentlyContinue
  if ($h1 -ne $h2) {
    throw "PROBE PIXEL MISMATCH $Tag -- decode is WRONG; timing would be meaningless"
  }
}

# --- banner ------------------------------------------------------------------
Write-Output "DECODE RACE -- rusty_h264 vs ffmpeg native h264"
Write-Output "method: pinned(affinity=$Affinity) High; CPU time; ABBA; decode_bench vs ffmpeg -threads 1 -f null -; NUL sink (no YUV on clock)"
Write-Output "pairs=$Pairs  dir=$Dir  stem=$Stem  probe_frames=$ProbeFrames"
Write-Output "bench=$Bench"
Write-Output "ffmpeg=$FFmpeg"
Write-Output ("-" * 72)

$tiers = @('cavlc', 'main', 'high')
$bits = @{}
foreach ($t in $tiers) {
  $p = Join-Path $Dir "${Stem}__$t.264"
  if (Test-Path $p) { $bits[$t] = (Resolve-Path $p).Path }
}

if ($bits.Count -eq 0) { throw "no streams matched $Dir\${Stem}__*.264" }

# --- D6a correctness ---------------------------------------------------------
if (-not $SkipGate) {
  Write-Output "GATE: byte-identical probe ($ProbeFrames frames) per tier..."
  foreach ($t in $tiers) {
    if (-not $bits.ContainsKey($t)) { continue }
    Assert-ByteIdentical -Bit $bits[$t] -Tag "${Stem}_$t"
    Write-Output "  $t  OK"
  }
} else {
  Write-Output "GATE: SKIPPED (-SkipGate) -- results are not a standing claim"
}

# --- D6b work counts on full streams ----------------------------------------
Write-Output "WORK: frame counts on full streams (mismatch voids the tier)..."
$frames = @{}
foreach ($t in $tiers) {
  if (-not $bits.ContainsKey($t)) { Write-Output "  $t  (missing)"; continue }
  $o = Get-OurFrames $bits[$t]
  $f = Get-FfFrames  $bits[$t]
  if ($o -ne $f) {
    Write-Output "  $t  VOID ours=$o ff=$f"
    $bits.Remove($t)
  } else {
    $frames[$t] = $o
    Write-Output "  $t  frames=$o"
  }
}

# --- D6c null arm -----------------------------------------------------------
$nullMed = $null
if (-not $SkipNull -and $bits.ContainsKey('main')) {
  Write-Output "NULL: decode_bench vs decode_bench on main ($Pairs pairs)..."
  $nr = @()
  for ($i = 0; $i -lt $Pairs; $i++) {
    if ($i % 2 -eq 0) {
      $a = Invoke-Pinned -Exe $Bench -ArgList @($bits['main'], '1')
      $b = Invoke-Pinned -Exe $Bench -ArgList @($bits['main'], '1')
    } else {
      $b = Invoke-Pinned -Exe $Bench -ArgList @($bits['main'], '1')
      $a = Invoke-Pinned -Exe $Bench -ArgList @($bits['main'], '1')
    }
    $nr += ($a.CpuMs / $b.CpuMs)
    Write-Output ("  pair {0,2}: {1,8:N0}/{2,8:N0} ms  ratio {3:N4}  cores {4:N2}/{5:N2}" -f `
      ($i+1), $a.CpuMs, $b.CpuMs, ($a.CpuMs/$b.CpuMs), $a.Cores, $b.Cores)
  }
  $nullMed = Get-Median $nr
  Write-Output ("NULL median ratio = {0:N4}  (harness floor; |r-1| should be << claimed effects)" -f $nullMed)
  if ([Math]::Abs($nullMed - 1.0) -gt 0.05) {
    Write-Output "!! NULL ARM DRIFT >5% -- box or harness is not stable enough for small effects"
  }
} elseif (-not $SkipNull) {
  Write-Output "NULL: skipped (no main stream)"
}

# --- race -------------------------------------------------------------------
Write-Output ("-" * 72)
Write-Output ("{0,-8} {1,10} {2,10} {3,8} {4,8} {5,8} {6,10} {7,8}" -f `
  'tier', 'rusty ms', 'ffmpeg ms', 'ratio', 'wins', 'z', 'cores_r', 'cores_f')
Write-Output ("-" * 72)

$results = @()
foreach ($t in $tiers) {
  if (-not $bits.ContainsKey($t)) {
    Write-Output ("{0,-8} (missing or void)" -f $t)
    continue
  }
  $bit = $bits[$t]
  $ratios = New-Object System.Collections.Generic.List[double]
  $rs = New-Object System.Collections.Generic.List[double]
  $fs = New-Object System.Collections.Generic.List[double]
  $cr = New-Object System.Collections.Generic.List[double]
  $cf = New-Object System.Collections.Generic.List[double]
  $wins = 0

  for ($i = 0; $i -lt $Pairs; $i++) {
    # ABBA: odd pairs B-first so "second is warmer" cancels.
    if ($i % 2 -eq 0) {
      $r = Invoke-Pinned -Exe $Bench -ArgList @($bit, '1')
      $f = Invoke-Pinned -Exe $FFmpeg -ArgList @(
        '-hide_banner', '-loglevel', 'error', '-threads', '1',
        '-i', $bit, '-f', 'null', '-'
      )
    } else {
      $f = Invoke-Pinned -Exe $FFmpeg -ArgList @(
        '-hide_banner', '-loglevel', 'error', '-threads', '1',
        '-i', $bit, '-f', 'null', '-'
      )
      $r = Invoke-Pinned -Exe $Bench -ArgList @($bit, '1')
    }
    $ratio = $r.CpuMs / $f.CpuMs
    [void]$ratios.Add($ratio)
    [void]$rs.Add($r.CpuMs)
    [void]$fs.Add($f.CpuMs)
    [void]$cr.Add($r.Cores)
    [void]$cf.Add($f.Cores)
    if ($f.CpuMs -lt $r.CpuMs) { $wins++ }
  }

  $n = $ratios.Count
  $med = Get-Median $ratios.ToArray()
  $mr  = Get-Median $rs.ToArray()
  $mf  = Get-Median $fs.ToArray()
  $mcr = Get-Median $cr.ToArray()
  $mcf = Get-Median $cf.ToArray()
  $z = ($wins - $n/2.0) / (0.5 * [Math]::Sqrt($n))

  $flag = ''
  $minMed = [Math]::Min($mr, $mf)
  if ($minMed -lt 15000) {
    $flag = ' !!SHORT'
    Write-Output ("!! $t workload median {0:N0} ms < 15 s -- ratio is suspect (codec-measurement 5)" -f $minMed)
  }
  if ([Math]::Abs($mcr - 1.0) -gt 0.25 -or [Math]::Abs($mcf - 1.0) -gt 0.25) {
    $flag += ' !!THREADS'
    Write-Output ("!! $t cores-busy rusty={0:N2} ffmpeg={1:N2} -- not single-core comparable" -f $mcr, $mcf)
  }

  Write-Output ("{0,-8} {1,10:N0} {2,10:N0} {3,7:F3}x {4,3}/{5} {6,8:F2} {7,8:F2} {8,8:F2}{9}" -f `
    $t, $mr, $mf, $med, $wins, $n, $z, $mcr, $mcf, $flag)

  $results += [pscustomobject]@{
    Tier = $t; RustyMs = $mr; FfmpegMs = $mf; Ratio = $med
    Wins = $wins; N = $n; Z = $z; CoresR = $mcr; CoresF = $mcf
    Frames = $frames[$t]
  }
}

Write-Output ("-" * 72)
Write-Output "Ratio > 1 means ffmpeg is that many times our throughput (we are slower)."
if ($null -ne $nullMed) {
  Write-Output ("null-arm median={0:N4}  (subtract in quadrature before believing tiny deltas)" -f $nullMed)
}
Write-Output "Standing claim requires: gate OK, work parity OK, cores~1.0 both sides, median arm >=15s, null arm printed."
Write-Output ("date={0:yyyy-MM-dd HH:mm}  host={1}" -f (Get-Date), $env:COMPUTERNAME)

# Machine-readable footer for baselines/
Write-Output "JSON_BEGIN"
$results | ConvertTo-Json -Compress
Write-Output "JSON_END"
