# Generic paired A/B for two DIFFERENT command lines (e.g. ours vs ffmpeg),
# pinned, High priority, measured by CPU time (H-46: affinity restricts us but
# does not reserve the core, so elapsed wall counts descheduled time; CPU time
# does not accrue off-core and is ~5x tighter under a foreign load).
#
#   pinvs.ps1 -AExe ours.exe -AArgs @('decode','--in','x.264','--out','NUL') `
#             -BExe ffmpeg   -BArgs @('-i','x.264','-f','null','-') -Pairs 15
#
# Reports the median CPU-time ratio A/B and the paired win count. Because the
# two arms are DIFFERENT PROGRAMS, the ratio is a throughput comparison, not a
# regression check -- state the work-identity check (frame counts) separately.
param([string]$AExe, [string[]]$AArgs, [string]$BExe, [string[]]$BArgs,
      [int]$Pairs = 15, [string]$ALabel = 'A', [string]$BLabel = 'B')

function Run($exe, $argv) {
  $p = Start-Process -FilePath $exe -ArgumentList $argv -PassThru -WindowStyle Hidden
  $null = $p.Handle   # cache the handle or TotalProcessorTime reads empty after exit
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'; $p.WaitForExit()
  $p.TotalProcessorTime.TotalMilliseconds
}
$wins = 0; $ratios = @(); $ta_all = @(); $tb_all = @()
1..$Pairs | ForEach-Object {
  if ($_ % 2 -eq 0) { $ta = Run $AExe $AArgs; $tb = Run $BExe $BArgs }
  else              { $tb = Run $BExe $BArgs; $ta = Run $AExe $AArgs }
  if ($ta -gt 0 -and $tb -gt 0) {
    $r = $ta / $tb; $ratios += $r; $ta_all += $ta; $tb_all += $tb
    if ($tb -lt $ta) { $wins++ }
    "pair {0,2}: {1} {2,8:N0} ms   {3} {4,8:N0} ms   ratio {5:N3}" -f $_, $ALabel, $ta, $BLabel, $tb, $r
  } else { "pair {0,2}: INSTRUMENT FAILED - dropped" -f $_ }
}
$n = $ratios.Count
if ($n -eq 0) { "ALL PAIRS FAILED - no usable samples"; exit 1 }
$med = ($ratios | Sort-Object)[[int]($n/2)]
$z = ($wins - $n/2.0) / (0.5 * [Math]::Sqrt($n))
"---"
"{0} median CPU {1:N0} ms   {2} median CPU {3:N0} ms" -f `
  $ALabel, ($ta_all|Sort-Object)[[int]($n/2)], $BLabel, ($tb_all|Sort-Object)[[int]($n/2)]
# CPU time is accounted in ~15.6 ms scheduler ticks on Windows. A workload of a few
# ticks cannot express a ratio, however many pairs you run -- and the harness will
# happily print a confident 0.667x from 2 ticks vs 3. Refuse to be believed there.
$ma = ($ta_all|Sort-Object)[[int]($n/2)]; $mb = ($tb_all|Sort-Object)[[int]($n/2)]
$minMed = [Math]::Min($ma,$mb)
if ($minMed -lt 500) {
  "!! WORKLOAD TOO SHORT: median arm {0:N0} ms is ~{1:N0} scheduler ticks (15.6 ms each)." -f $minMed, ($minMed/15.6)
  "!! The ratio below is timer QUANTISATION, not a measurement. Lengthen the workload"
  "!! until BOTH arms run >= ~15 s (codec-measurement 5)."
}
if ($n -lt $Pairs) {
  "!! {0} of {1} pairs were DROPPED (instrument returned 0/non-finite). A sample the" -f ($Pairs-$n), $Pairs
  "!! instrument failed to take is not a tie -- treat this run as suspect."
}
"median ratio {0}/{1} = {2:N3}x   ({3} is {4:N2}x the throughput)   {1} faster in {5}/{6}, z={7:N2}" -f `
  $ALabel, $BLabel, $med, $BLabel, $med, $wins, $n, $z
