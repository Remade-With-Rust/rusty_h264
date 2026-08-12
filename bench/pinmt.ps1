# Paired A/B for a THREADING change, on TWO SEPARATE PHYSICAL CORES.
#
# WHY THIS EXISTS, AND WHY pinvs.ps1 CANNOT DO IT. pinvs pins every arm to
# `ProcessorAffinity = 4` -- logical CPU 2, a SINGLE core. That is correct for
# two single-threaded arms and catastrophically wrong for a multi-threaded one:
# the E2 decoder worker measured 3.47-5.49x the CPU of the inline path, 11/11,
# z=3.32, which is not the seam's cost but two threads thrashing one core.
# Measuring a threading change needs the threads to have somewhere to go.
#
# BOTH arms get the SAME two-core mask, so the comparison is at equal resources
# and the single-threaded arm is not handicapped. Default mask 20 = logical
# CPUs 2 and 4. On this i7-14650HX (16 physical / 24 logical, hybrid) the
# P-cores are logical 0-15 in HT sibling pairs, so 2 and 4 are DISTINCT PHYSICAL
# P-cores -- not HT siblings (2,3 would be), and not E-cores (16-23).
# Pass -Mask explicitly on any other machine; siblings or E-cores invalidate it.
#
# Reports BOTH metrics, because a threading change moves them in opposite
# directions and either one alone is a lie:
#   * WALL   -- the point of threading. Lower is better.
#   * CPU    -- total across threads, i.e. the EFFICIENCY price. ~2x CPU for a
#               ~2x wall win is the deal working; 4x CPU for 1.1x wall is a
#               spin-wait, not a speedup.
#
#   pinmt.ps1 -AExe bench.exe -AArgs @('s.264','1','fthreads=2') `
#             -BExe bench.exe -BArgs @('s.264','1','fthreads=1') -Pairs 11
#   -FloorMs 7000  # abort after pair 1 if min wall > 2.5x the standing floor
param([string]$AExe, [string[]]$AArgs, [string]$BExe, [string[]]$BArgs,
      [int]$Pairs = 11, [string]$ALabel = 'A', [string]$BLabel = 'B',
      [int]$Mask = 20, [int]$FloorMs = 0)

function Run($exe, $argv) {
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList $argv -PassThru -WindowStyle Hidden
  $null = $p.Handle   # cache the handle or TotalProcessorTime reads empty after exit
  $p.ProcessorAffinity = [IntPtr]$Mask; $p.PriorityClass = 'High'; $p.WaitForExit()
  $sw.Stop()
  [pscustomobject]@{ Cpu = $p.TotalProcessorTime.TotalMilliseconds; Wall = $sw.Elapsed.TotalMilliseconds }
}
$wallWins = 0; $wr = @(); $cr = @(); $aw = @(); $bw = @(); $ac = @(); $bc = @()
1..$Pairs | ForEach-Object {
  # ABBA: alternate which arm runs first so a monotonic drift (thermal, a
  # background task ramping) cannot land entirely on one arm.
  if ($_ % 2 -eq 0) { $a = Run $AExe $AArgs; $b = Run $BExe $BArgs }
  else              { $b = Run $BExe $BArgs; $a = Run $AExe $AArgs }
  if ($a.Cpu -gt 0 -and $b.Cpu -gt 0 -and $a.Wall -gt 0 -and $b.Wall -gt 0) {
    $wr += $a.Wall / $b.Wall; $cr += $a.Cpu / $b.Cpu
    $aw += $a.Wall; $bw += $b.Wall; $ac += $a.Cpu; $bc += $b.Cpu
    if ($a.Wall -lt $b.Wall) { $wallWins++ }
    $ba = $a.Cpu / $a.Wall; $bb = $b.Cpu / $b.Wall
    "pair {0,2}: wall {1,7:N0} / {2,7:N0} ms   cpu {3,7:N0} / {4,7:N0} ms   busy {5:N2}/{6:N2}" -f `
      $_, $a.Wall, $b.Wall, $a.Cpu, $b.Cpu, $ba, $bb
    if ($_ -eq 1 -and $FloorMs -gt 0) {
      $wmin = [Math]::Min($a.Wall, $b.Wall)
      if ($wmin -gt 2.5 * $FloorMs) {
        "!! LOADED: pair-1 wall {0:N0} ms is {1:N1}x the floor ({2} ms). Abort -- do not quote." -f `
          $wmin, ($wmin / $FloorMs), $FloorMs
        exit 2
      }
    }
  } else { "pair {0,2}: INSTRUMENT FAILED - dropped" -f $_ }
}
$n = $wr.Count
if ($n -eq 0) { "ALL PAIRS FAILED - no usable samples"; exit 1 }
function Med($xs) { ($xs | Sort-Object)[[int]($xs.Count/2)] }
$mwr = Med $wr; $mcr = Med $cr
$z = ($wallWins - $n/2.0) / (0.5 * [Math]::Sqrt($n))
"---"
"{0}: wall {1,7:N0} ms  cpu {2,7:N0} ms   (medians)" -f $ALabel, (Med $aw), (Med $ac)
"{0}: wall {1,7:N0} ms  cpu {2,7:N0} ms   (medians)" -f $BLabel, (Med $bw), (Med $bc)
$minMed = [Math]::Min((Med $aw), (Med $bw))
if ($minMed -lt 500) {
  "!! WORKLOAD TOO SHORT: median arm {0:N0} ms. Lengthen it (codec-measurement 5)." -f $minMed
}
if ($n -lt $Pairs) {
  "!! {0} of {1} pairs DROPPED -- a sample the instrument failed to take is not a tie." -f ($Pairs-$n), $Pairs
}
"WALL ratio {0}/{1} = {2:N3}x   ({0} faster in {3}/{4}, z={5:N2})" -f $ALabel, $BLabel, $mwr, $wallWins, $n, $z
"CPU  ratio {0}/{1} = {2:N3}x   <- the efficiency price of the threading" -f $ALabel, $BLabel, $mcr
if ($mwr -lt 0.95 -and $mcr -gt 2.5) {
  "!! WALL win bought with {0:N1}x CPU: suspect spin-waiting, not parallelism." -f $mcr
}
