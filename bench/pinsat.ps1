# Saturation-throughput contention test (rs_h264 H-44).
#
# The pinned single-core load test could not fire: 16 load processes on a
# 24-core box never contend with a pinned, High-priority arm. The question the
# "degrades harder under load" claim was really asking is a SATURATION
# question -- when every core is busy, does our decoder lose more throughput to
# shared-resource pressure (LLC, memory bandwidth) than ffmpeg's?
#
# Both arms run K concurrent instances with no affinity, so both are equally
# exposed to the scheduler and neither can absorb a migration asymmetry the
# other escapes. Efficiency = (aggregate throughput at K) / (K x solo
# throughput). Comparing the two efficiencies isolates contention sensitivity
# from the standing single-thread speed gap.
#
# DURATION IS LOAD-BEARING. Start-Process costs ~30 ms and the launcher
# serializes, so K=24 injects ~0.7 s of overhead into the saturated wall. That
# is a per-invocation cost, so it penalizes the SHORTER-running tool far
# harder -- the exact artifact this file exists to rule out. Each instance must
# therefore run long enough (~15 s) that launch overhead is <5% of both arms:
# -OursReps and -FfLoop set the per-instance work.
# -FfClip lets ffmpeg run a LONGER input than ours (-stream_loop does not work
# on raw Annex-B, so use a concatenated stream). Efficiency is a within-tool
# ratio, so the two arms need not share an input -- they only need per-instance
# durations in the same ballpark, which is what kills the launch-overhead bias.
#
# Residual asymmetry, stated because it is one-sided: our arm RETAINS every
# decoded frame (~182 MB for 1200 CIF frames) while ffmpeg's -f null discards
# them. That handicaps US on memory pressure, so a result showing ours scaling
# BETTER is conservative -- correcting it would only widen our margin.
param([string]$Clip, [int]$K = 24, [int]$Reps = 2,
      [int]$OursReps = 3, [string]$FfClip = "",
      [string]$Ours = "target\release\examples\decode_prof.exe",
      [string]$Ffmpeg = "ffmpeg")
$env:DP_REPS = "$OursReps"
if (-not $FfClip) { $FfClip = $Clip }
$ffArgs = @('-v','quiet','-threads','1','-i',$FfClip,'-f','null','-')

# Solo timing is taken PINNED (H-41) so the denominator is a clean number.
function Solo($exe, $argv) {
  $best = [double]::MaxValue
  1..$Reps | ForEach-Object {
    $p = Start-Process -FilePath $exe -ArgumentList $argv -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'
    $sw = [Diagnostics.Stopwatch]::StartNew(); $p.WaitForExit(); $sw.Stop()
    $best = [Math]::Min($best, $sw.Elapsed.TotalMilliseconds)
  }
  $best
}
# Saturated: K at once, no affinity, normal priority -- a real loaded box.
function Sat($exe, $argv) {
  $best = [double]::MaxValue
  1..$Reps | ForEach-Object {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $ps = 1..$K | ForEach-Object { Start-Process -FilePath $exe -ArgumentList $argv -PassThru -WindowStyle Hidden }
    $ps | ForEach-Object { $_.WaitForExit() }
    $sw.Stop()
    $best = [Math]::Min($best, $sw.Elapsed.TotalMilliseconds)
  }
  $best
}

$oSolo = Solo $Ours  @($Clip);  $oSat = Sat $Ours  @($Clip)
$fSolo = Solo $Ffmpeg $ffArgs;  $fSat = Sat $Ffmpeg $ffArgs
# Efficiency: perfect scaling would finish K instances in one solo time.
$oEff = $oSolo * $K / $oSat
$fEff = $fSolo * $K / $fSat
"ours    solo {0,7:N0} ms   {1}x concurrent {2,8:N0} ms   scaling {3,5:N1}/{4}  eff {5:P0}" -f $oSolo,$K,$oSat,$oEff,$K,($oEff/$K)
"ffmpeg  solo {0,7:N0} ms   {1}x concurrent {2,8:N0} ms   scaling {3,5:N1}/{4}  eff {5:P0}" -f $fSolo,$K,$fSat,$fEff,$K,($fEff/$K)
"launch overhead ~{0:N0} ms = {1:P1} of ours' saturated wall, {2:P1} of ffmpeg's  (must be small in BOTH)" -f `
  (30.0*$K), (30.0*$K/$oSat), (30.0*$K/$fSat)
"---"
"contention asymmetry (ours eff / ffmpeg eff): {0:N3}x" -f ($oEff/$fEff)
"  {0}" -f $(if ([Math]::Abs($oEff/$fEff - 1.0) -lt 0.10) {
    "WITHIN 10% - no differential contention sensitivity"
  } elseif ($oEff -lt $fEff) { "ours DOES lose more under saturation - real shared-resource effect" }
  else { "ours scales BETTER than ffmpeg under saturation" })
