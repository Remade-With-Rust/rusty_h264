# Pinned mb-tree cost-column harness (rs_h264 H-41 recipe applied to the encoder).
#
# mbtree_bench already alternates its OFF/ON arms inside one process, so the
# statistic we want is not a win-rate but the SPREAD of the overhead figure
# across repeated invocations: that spread is what swung +-40 points unpinned
# and made the cost column unusable.
#
#   -Affinity 0  -> leave the process on all cores (shipped GOP-parallel config)
#   -Affinity N  -> pin to the core mask N (4 = core 2, the H-41 recipe)
param([string]$Clip, [int]$Runs = 7, [int]$Affinity = 4, [int]$Frames = 48,
      [int]$Reps = 2, [string]$Exe = "target\release\examples\mbtree_bench.exe")
$env:MB_FRAMES = $Frames; $env:MB_REPS = $Reps
$ovh = @(); $unp = @(); $sz = @(); $evals = @()
1..$Runs | ForEach-Object {
  $out = [IO.Path]::GetTempFileName()
  $p = Start-Process -FilePath $Exe -ArgumentList $Clip -PassThru -NoNewWindow -RedirectStandardOutput $out
  if ($Affinity -ne 0) { $p.ProcessorAffinity = [IntPtr]$Affinity }
  $p.PriorityClass = 'High'; $p.WaitForExit()
  $txt = Get-Content $out; Remove-Item $out
  $o = ($txt | Select-String 'PAIRED median\): ([-+0-9.]+)%').Matches.Groups[1].Value
  $u = ($txt | Select-String 'historical\): ([-+0-9.]+)%').Matches.Groups[1].Value
  $s = ($txt | Select-String 'size ([-+0-9.]+)%').Matches.Groups[1].Value
  $e = ($txt | Select-String 'work: (\d+) candidate').Matches.Groups[1].Value
  $ovh += [double]$o; $unp += [double]$u; $sz += [double]$s; $evals += [long]$e
  "run {0}: PAIRED {1,7:N1}%   unpaired {2,7:N1}%   size {3,6:N2}%   evals {4}" -f `
    $_, [double]$o, [double]$u, [double]$s, [long]$e
}
$so = $ovh | Sort-Object; $su = $unp | Sort-Object
"---"
"PAIRED    median {0:N1}%   min {1:N1}%   max {2:N1}%   SPREAD {3:N1} points" -f `
  $so[[int]($so.Count/2)], $so[0], $so[-1], ($so[-1] - $so[0])
"unpaired  median {0:N1}%   min {1:N1}%   max {2:N1}%   SPREAD {3:N1} points  <- the old column" -f `
  $su[[int]($su.Count/2)], $su[0], $su[-1], ($su[-1] - $su[0])
"size      median {0:N2}%   (bitstream effect, deterministic)" -f ($sz | Sort-Object)[[int]($sz.Count/2)]
"evals     {0}   distinct={1}  {2}" -f $evals[0], ($evals | Select-Object -Unique).Count, `
  $(if (($evals | Select-Object -Unique).Count -eq 1) {"DETERMINISTIC - work column is exact"} else {"NON-DETERMINISTIC - investigate"})
