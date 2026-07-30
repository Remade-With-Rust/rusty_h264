# Pinned, high-priority A/B bench (rs_h264 H-41). Two binaries, N alternating
# pairs, reports each ratio plus the win count — the only wall harness whose
# spread (~1.06x) is smaller than the effects we routinely decide on.
param([string]$A, [string]$B, [string]$Clip, [int]$Pairs = 9, [string]$Reps = "3")
$env:DP_REPS = $Reps
function Run($exe, $tag) {
  $out = [IO.Path]::GetTempFileName()
  $p = Start-Process -FilePath $exe -ArgumentList $Clip -PassThru -NoNewWindow -RedirectStandardOutput $out
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'; $p.WaitForExit()
  $line = Get-Content $out | Select-Object -First 1; Remove-Item $out
  if ($line -match 'best-of-\d+ ([0-9.]+) ms') { [double]$matches[1] } else { [double]::NaN }
}
$wins = 0; $ratios = @()
1..$Pairs | ForEach-Object {
  if ($_ % 2 -eq 0) { $ta = Run $A 'A'; $tb = Run $B 'B' } else { $tb = Run $B 'B'; $ta = Run $A 'A' }
  $r = $ta / $tb; $ratios += $r; if ($tb -lt $ta) { $wins++ }
  "pair {0}: A {1:N0} ms  B {2:N0} ms  ratio {3:N3}" -f $_, $ta, $tb, $r
}
$med = ($ratios | Sort-Object)[[int]($ratios.Count/2)]
$z = ($wins - $Pairs/2.0) / (0.5 * [Math]::Sqrt($Pairs))
"B wins {0}/{1}   median ratio {2:N3}   z={3:N2}  {4}" -f $wins, $Pairs, $med, $z, $(if ([Math]::Abs($z) -gt 2) {"VERDICT"} else {"not resolved"})
