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
$med = ($ratios | Sort-Object)[[int]($n/2)]
$z = ($wins - $n/2.0) / (0.5 * [Math]::Sqrt($n))
"---"
"{0} median CPU {1:N0} ms   {2} median CPU {3:N0} ms" -f `
  $ALabel, ($ta_all|Sort-Object)[[int]($n/2)], $BLabel, ($tb_all|Sort-Object)[[int]($n/2)]
"median ratio {0}/{1} = {2:N3}x   ({3} is {4:N2}x the throughput)   {1} faster in {5}/{6}, z={7:N2}" -f `
  $ALabel, $BLabel, $med, $BLabel, $med, $wins, $n, $z
