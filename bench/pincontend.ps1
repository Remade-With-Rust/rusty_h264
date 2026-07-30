# Contention-sensitivity re-test under the H-41 pinning recipe.
#
# The original claim -- "our decoder degrades ~25% harder than ffmpeg's under
# load" -- was taken UNPINNED. Our arm runs 2-3x longer per invocation than
# ffmpeg's, so it absorbs proportionally more scheduler migration, which alone
# would manufacture exactly that asymmetry. This harness pins BOTH arms to one
# core at High priority and applies the load to the other cores, so the only
# contention left is for genuinely shared resources (LLC, memory bandwidth) --
# the thing the claim was supposed to be about.
#
# Reported: each arm's degradation (loaded/unloaded), and the ratio of those
# degradations. Ratio ~1.0 => the asymmetry was a harness artifact.
param([string]$Clip, [int]$Pairs = 7, [int]$Load = 16,
      [string]$Ours = "target\release\examples\decode_prof.exe",
      [string]$Ffmpeg = "ffmpeg")
$env:DP_REPS = "1"

function RunOurs {
  $out = [IO.Path]::GetTempFileName()
  $p = Start-Process -FilePath $Ours -ArgumentList $Clip -PassThru -NoNewWindow -RedirectStandardOutput $out
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'
  $sw = [Diagnostics.Stopwatch]::StartNew(); $p.WaitForExit(); $sw.Stop()
  $line = Get-Content $out | Select-Object -First 1; Remove-Item $out
  $inner = if ($line -match 'best-of-\d+ ([0-9.]+) ms') { [double]$matches[1] } else { [double]::NaN }
  [pscustomobject]@{ Wall = $sw.Elapsed.TotalMilliseconds; Inner = $inner }
}
function RunFf {
  $err = [IO.Path]::GetTempFileName()
  $p = Start-Process -FilePath $Ffmpeg -ArgumentList @('-v','quiet','-threads','1','-i',$Clip,'-f','null','-') `
       -PassThru -NoNewWindow -RedirectStandardError $err
  $p.ProcessorAffinity = [IntPtr]4; $p.PriorityClass = 'High'
  $sw = [Diagnostics.Stopwatch]::StartNew(); $p.WaitForExit(); $sw.Stop()
  Remove-Item $err
  [pscustomobject]@{ Wall = $sw.Elapsed.TotalMilliseconds; Inner = [double]::NaN }
}

function Sample($tag) {
  $o = @(); $f = @(); $oi = @()
  1..$Pairs | ForEach-Object {
    # Alternate which arm leads so any ordering effect cancels.
    if ($_ % 2 -eq 0) { $ro = RunOurs; $rf = RunFf } else { $rf = RunFf; $ro = RunOurs }
    $o += $ro.Wall; $oi += $ro.Inner; $f += $rf.Wall
  }
  $mo = ($o | Sort-Object)[[int]($o.Count/2)]
  $mf = ($f | Sort-Object)[[int]($f.Count/2)]
  $mi = ($oi | Sort-Object)[[int]($oi.Count/2)]
  # Write-Host, not the pipeline: a formatted string emitted here would be
  # captured into the caller's variable alongside the object.
  Write-Host ("{0,-12} ours wall {1,8:N0} ms   ours decode {2,8:N0} ms   ffmpeg wall {3,8:N0} ms" `
    -f $tag, $mo, $mi, $mf)
  [pscustomobject]@{ Ours = $mo; Inner = $mi; Ff = $mf }
}

$base = Sample 'UNLOADED'

# Load: real codec memory traffic on the OTHER cores, normal priority.
$jobs = 1..$Load | ForEach-Object {
  Start-Process -FilePath $Ffmpeg -PassThru -WindowStyle Hidden `
    -ArgumentList @('-v','quiet','-stream_loop','200','-threads','1','-i',$Clip,'-f','null','-')
}
Start-Sleep -Seconds 3
$hot = Sample "LOADED(x$Load)"
$jobs | ForEach-Object { try { $_.Kill() } catch {} }

"---"
$do = $hot.Ours / $base.Ours; $di = $hot.Inner / $base.Inner; $df = $hot.Ff / $base.Ff
"degradation under load:  ours wall {0:N3}x   ours decode {1:N3}x   ffmpeg {2:N3}x" -f $do, $di, $df
"asymmetry (ours/ffmpeg): wall {0:N3}x   decode-vs-wall {1:N3}x" -f ($do/$df), ($di/$df)
"  {0}" -f $(if ([Math]::Abs($di/$df - 1.0) -lt 0.10) {
    "WITHIN 10% - no differential contention sensitivity; the original claim does not survive pinning"
  } else { "differential sensitivity SURVIVES pinning - real shared-resource effect" })
