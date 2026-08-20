# Paired A/B of ONE binary under an env knob (e.g. RS_H264_NO_SKIPBAND).
# Arm A = knob SET (baseline/off path), arm B = knob UNSET (new path).
# Same discipline as pinvs.ps1: pinned core, High priority, CPU time primary
# (wall counts descheduled time on a loaded box; CPU time does not), ABBA order,
# busy=cpu/wall printed per pair. Env inherits into Start-Process children, so
# the knob is toggled on THIS process between runs.
#
#   pinenv.ps1 -Exe target\release\examples\decode_bench.exe `
#              -ExeArgs @('stream.264','3') -EnvName RS_H264_NO_SKIPBAND -Pairs 15
# NB: the param must NOT be named Args — that collides with PowerShell's
# automatic $args and arrives empty inside functions.
param([string]$Exe, [string[]]$ExeArgs, [string]$EnvName, [int]$Pairs = 15,
      [string]$ALabel = 'off', [string]$BLabel = 'on')

function Run($exe, $argv) {
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList $argv -PassThru -WindowStyle Hidden
  $null = $p.Handle
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'; $p.WaitForExit()
  $sw.Stop()
  [pscustomobject]@{ Cpu = $p.TotalProcessorTime.TotalMilliseconds; Wall = $sw.Elapsed.TotalMilliseconds }
}
function ArmA($exe, $argv) { Set-Item "env:$EnvName" '1'; $r = Run $exe $argv; Remove-Item "env:$EnvName" -ErrorAction SilentlyContinue; $r }
function ArmB($exe, $argv) { Remove-Item "env:$EnvName" -ErrorAction SilentlyContinue; Run $exe $argv }

$wins = 0; $ratios = @()
1..$Pairs | ForEach-Object {
  if ($_ % 2 -eq 0) { $a = ArmA $Exe $ExeArgs; $b = ArmB $Exe $ExeArgs }
  else              { $b = ArmB $Exe $ExeArgs; $a = ArmA $Exe $ExeArgs }
  $ta = $a.Cpu; $tb = $b.Cpu
  if ($ta -gt 0 -and $tb -gt 0) {
    $r = $tb / $ta; $ratios += $r
    if ($tb -lt $ta) { $wins++ }
    "pair {0,2}: {1} cpu {2,8:N0} busy {3:N2}   {4} cpu {5,8:N0} busy {6:N2}   B/A {7:N3}" -f `
      $_, $ALabel, $ta, ($ta / $a.Wall), $BLabel, $tb, ($tb / $b.Wall), $r
  } else { "pair {0,2}: zero cpu read, dropped" -f $_ }
}
$sorted = $ratios | Sort-Object
$med = $sorted[[int](($sorted.Count - 1) / 2)]
$n = $ratios.Count
# Two-sided sign test z: wins vs n/2.
$z = if ($n -gt 0) { (2 * $wins - $n) / [math]::Sqrt($n) } else { 0 }
"median B/A = {0:N4}  wins(B faster) = {1}/{2}  sign z = {3:N2}" -f $med, $wins, $n, $z
