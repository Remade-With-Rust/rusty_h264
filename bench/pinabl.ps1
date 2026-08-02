# Pinned, paired ABLATION A/B: the SAME binary, run twice, differing only in one
# environment knob (`RFF_ABL_*`). Prices a stage with ZERO instrument tax — no
# rdtsc scope is added, so a per-MB stage cannot inflate its own share the way it
# does under the scope profiler (measured tax there: 1.32-1.43x of whole decode).
#
# Measurement shape is the one codec-measurement mandates and pinvs.ps1 implements:
# pinned to one core, High priority, CPU time (not wall — this box runs at 100%
# from unrelated processes and wall counts time spent descheduled), arms ABBA
# alternated so drift and warm-up bias cancel, paired win-rate with a z-score.
#
#   .\bench\pinabl.ps1 -Exe target\release\examples\decode_bench.exe `
#                      -ExeArgs @('_xbench\long_cavlc.264','1') `
#                      -Knob RFF_ABL_DEBLOCK -Pairs 7
#
# Reports the ABLATED share of total: 1 - (ablated cpu / full cpu). Note the arms
# do DIFFERENT work by construction (that is the point), so the usual work-count
# parity rule is replaced by: the ablated arm must still decode the same FRAME
# COUNT, which the caller checks. Output pixels are wrong while a knob is set.
param([string]$Exe, [string[]]$ExeArgs, [string]$Knob, [int]$Pairs = 7,
      [string]$KnobValue = '1')

function Run([bool]$ablate) {
  if ($ablate) { Set-Item -Path "env:$Knob" -Value $KnobValue }
  else { if (Test-Path "env:$Knob") { Remove-Item "env:$Knob" } }
  $p = Start-Process -FilePath $Exe -ArgumentList $ExeArgs -PassThru -WindowStyle Hidden
  # Cache the handle BEFORE waiting or TotalProcessorTime reads empty after exit.
  $null = $p.Handle
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'; $p.WaitForExit()
  $p.TotalProcessorTime.TotalMilliseconds
}

$shares = @(); $full_all = @(); $abl_all = @(); $wins = 0
1..$Pairs | ForEach-Object {
  if ($_ % 2 -eq 0) { $full = Run $false; $abl = Run $true }
  else              { $abl = Run $true;   $full = Run $false }
  if ($full -gt 0 -and $abl -gt 0) {
    $share = 1.0 - ($abl / $full)
    $shares += $share; $full_all += $full; $abl_all += $abl
    if ($abl -lt $full) { $wins++ }
    "pair {0,2}: full {1,8:N0} ms   ablated {2,8:N0} ms   stage = {3,6:P1}" -f $_, $full, $abl, $share
  } else { "pair {0,2}: INSTRUMENT FAILED - dropped" -f $_ }
}
if (Test-Path "env:$Knob") { Remove-Item "env:$Knob" }

$n = $shares.Count
if ($n -eq 0) { "ALL PAIRS FAILED - no usable samples"; exit 1 }
$sorted = $shares | Sort-Object
$med = $sorted[[int]($n/2)]
$z = ($wins - $n/2.0) / (0.5 * [Math]::Sqrt($n))
$mf = ($full_all|Sort-Object)[[int]($n/2)]; $ma = ($abl_all|Sort-Object)[[int]($n/2)]
"---"
"method: pinned core 2, High priority, CPU time, ABBA-alternated, $n pairs, knob $Knob=$KnobValue"
"full median {0:N0} ms   ablated median {1:N0} ms   delta {2:N0} ms" -f $mf, $ma, ($mf-$ma)
if ($mf -lt 500 -or $ma -lt 500) {
  "!! WORKLOAD TOO SHORT: an arm is under ~32 scheduler ticks (15.6 ms each);"
  "!! the share below is timer QUANTISATION. Lengthen the stream."
}
"{0} = {1:P1} of decode   (range {2:P1} .. {3:P1})   ablated faster in {4}/{5}, z={6:N2}" -f `
  $Knob, $med, $sorted[0], $sorted[$n-1], $wins, $n, $z
