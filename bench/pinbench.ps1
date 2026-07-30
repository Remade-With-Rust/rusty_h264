# Pinned, high-priority A/B bench (rs_h264 H-41/H-46). Two binaries, N alternating
# pairs, reports each ratio plus the win count.
#
# H-46: AFFINITY RESTRICTS US, IT DOES NOT RESERVE THE CORE. Pinning stops OUR
# thread migrating; it does nothing to stop an unpinned foreign load (a 9.5-core
# `ffai`, a 21-core rustc storm) from being scheduled onto the same logical CPU,
# or onto its SMT sibling where priority buys nothing at all. This box is an
# i7-14650HX: 16 physical / 24 logical, so logical CPU 2's sibling is CPU 3.
#
# The fix is the METRIC, not more pinning. Elapsed wall counts time we spent
# descheduled; TotalProcessorTime does not accrue while we are off-core, so it
# removes the preemption term entirely and leaves only real slowdown (cache and
# execution-unit contention). -Metric cpu is the default for that reason; use
# -Metric wall only on a box verified quiet.
param([string]$A, [string]$B, [string]$Clip, [int]$Pairs = 9, [string]$Reps = "3",
      [ValidateSet('cpu','wall')][string]$Metric = 'cpu')
$env:DP_REPS = $Reps
function Run($exe, $tag) {
  $out = [IO.Path]::GetTempFileName()
  $p = Start-Process -FilePath $exe -ArgumentList $Clip -PassThru -NoNewWindow -RedirectStandardOutput $out
  # Touch .Handle BEFORE waiting: .NET only caches the process handle once it has
  # been accessed, and without it TotalProcessorTime reads empty after exit
  # (intermittently — which silently produced 0.000 and Inf ratios).
  $null = $p.Handle
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'; $p.WaitForExit()
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  $line = Get-Content $out | Select-Object -First 1; Remove-Item $out
  if ($Metric -eq 'cpu') { return $cpu }
  if ($line -match 'best-of-\d+ ([0-9.]+) ms') { [double]$matches[1] } else { [double]::NaN }
}
$wins = 0; $ratios = @()
1..$Pairs | ForEach-Object {
  if ($_ % 2 -eq 0) { $ta = Run $A 'A'; $tb = Run $B 'B' } else { $tb = Run $B 'B'; $ta = Run $A 'A' }
  # A non-finite sample means the instrument failed, not that the arms tied —
  # drop the pair loudly rather than letting a 0 or Inf into the median.
  if ($ta -gt 0 -and $tb -gt 0) {
    $r = $ta / $tb; $ratios += $r; if ($tb -lt $ta) { $wins++ }
    "pair {0}: A {1:N0} ms  B {2:N0} ms  ratio {3:N3}" -f $_, $ta, $tb, $r
  } else {
    "pair {0}: INSTRUMENT FAILED (A={1} B={2}) - pair DROPPED" -f $_, $ta, $tb
  }
}
$n = $ratios.Count
if ($n -lt $Pairs) { "WARNING: {0} of {1} pairs dropped by instrument failure" -f ($Pairs - $n), $Pairs }
$med = ($ratios | Sort-Object)[[int]($n/2)]
$z = ($wins - $n/2.0) / (0.5 * [Math]::Sqrt($n))
"B wins {0}/{1}   median ratio {2:N3}   z={3:N2}  {4}   [metric: {5}]" -f `
  $wins, $n, $med, $z, $(if ([Math]::Abs($z) -gt 2) {"VERDICT"} else {"not resolved"}), $Metric
