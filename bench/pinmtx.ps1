# Multi-clip interleaved paired A/B for a THREADING change.
#
# WHY THIS EXISTS. `pinmt.ps1` interleaves ABBA *within* one clip, which makes
# the A-vs-B comparison sound. It does not interleave ACROSS clips: run it in a
# loop and you get all of clip 1, then all of clip 2, so every BETWEEN-CLIP
# difference is confounded with time — thermal ramp, a background task starting,
# the machine's mood. That is fatal precisely when the question is "is this a
# CONTENT effect", because clip-vs-clip is then the only comparison in the whole
# design that was never interleaved.
#
# It cost a real result: the same two clips at identical settings read 0.768x /
# 1.300x in one block-ordered run and 1.340x / 0.891x in the next. They swapped
# places. Block ordering could not tell content from drift.
#
# This harness runs ROUND-ROBIN: round 1 touches every clip, then round 2, and
# so on, with the A/B order flipping each round. Every clip therefore samples
# the same conditions, and a clip-to-clip difference that survives is a
# difference between CLIPS rather than between MINUTES.
#
# It also reports each clip's own spread across rounds, so you can compare
# BETWEEN-clip spread against WITHIN-clip spread directly — the only way to say
# a content effect exists at all.
#
#   pinmtx.ps1 -Exe bench.exe -Clips @('a.264','b.264') `
#              -AExtra @('mt=1','bound=65536') -BExtra @('mt=0') -Rounds 9
param(
  [string]$Exe,
  [string[]]$Clips,
  [string[]]$Pre = @(),          # args before the clip-specific ones
  [string[]]$AExtra = @(),       # args that define arm A
  [string[]]$BExtra = @(),       # args that define arm B
  [int]$Rounds = 9,
  [int]$Mask = 340,
  [string]$ALabel = 'A',
  [string]$BLabel = 'B'
)

function Run($argv) {
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList $argv -PassThru -WindowStyle Hidden
  $null = $p.Handle
  $p.ProcessorAffinity = [IntPtr]$Mask; $p.PriorityClass = 'High'; $p.WaitForExit()
  $sw.Stop()
  [pscustomobject]@{ Cpu = $p.TotalProcessorTime.TotalMilliseconds; Wall = $sw.Elapsed.TotalMilliseconds }
}
function Med($xs) { ($xs | Sort-Object)[[int]($xs.Count/2)] }

$wall = @{}; $cpu = @{}; $wins = @{}; $awall = @{}; $bwall = @{}
foreach ($c in $Clips) { $wall[$c] = @(); $cpu[$c] = @(); $wins[$c] = 0; $awall[$c] = @(); $bwall[$c] = @() }

1..$Rounds | ForEach-Object {
  $round = $_
  foreach ($c in $Clips) {
    $aArgs = @($c) + $Pre + $AExtra
    $bArgs = @($c) + $Pre + $BExtra
    # Flip arm order every round so a within-round ramp cannot favour one arm.
    if ($round % 2 -eq 0) { $a = Run $aArgs; $b = Run $bArgs }
    else                  { $b = Run $bArgs; $a = Run $aArgs }
    if ($a.Cpu -gt 0 -and $b.Cpu -gt 0 -and $a.Wall -gt 0 -and $b.Wall -gt 0) {
      $wall[$c] += $a.Wall / $b.Wall
      $cpu[$c]  += $a.Cpu  / $b.Cpu
      $awall[$c] += $a.Wall; $bwall[$c] += $b.Wall
      if ($a.Wall -lt $b.Wall) { $wins[$c]++ }
    }
  }
  "round {0}/{1} done" -f $round, $Rounds
}

"---"
"{0,-38} {1,8} {2,8} {3,8} {4,8} {5,10}" -f 'clip','wallMed','wallMin','wallMax','cpuMed','wallWins'
foreach ($c in $Clips) {
  $w = $wall[$c]; $n = $w.Count
  if ($n -eq 0) { "{0,-38}  ALL PAIRS FAILED" -f $c; continue }
  $sorted = $w | Sort-Object
  "{0,-38} {1,8:N3} {2,8:N3} {3,8:N3} {4,8:N3} {5,7}/{6}" -f `
    (Split-Path $c -Leaf), (Med $w), $sorted[0], $sorted[-1], (Med $cpu[$c]), $wins[$c], $n
}
"---"
# The decisive comparison: is the spread BETWEEN clips bigger than the spread
# WITHIN a clip? If not, there is no content effect to find — only noise.
$meds = @(); $within = @()
foreach ($c in $Clips) {
  if ($wall[$c].Count -gt 0) {
    $s = $wall[$c] | Sort-Object
    $meds += (Med $wall[$c])
    $within += ($s[-1] - $s[0])
  }
}
# PER-ARM dispersion. A RATIO's spread cannot say WHERE the instability lives.
# If arm B (single-threaded, pinned) is tight and arm A (threaded) is wild, the
# variance belongs to the THREADING and is a defect in its own right, separate
# from the mean. If both are wild, it is the machine and every number here is
# weaker than it looks.
"---"
"{0,-38} {1,10} {2,10} {3,10} {4,10}" -f 'clip','A_cv%','B_cv%','A_max/min','B_max/min'
function CV($xs) {
  if ($xs.Count -lt 2) { return 0 }
  $m = ($xs | Measure-Object -Average).Average
  if ($m -le 0) { return 0 }
  $v = ($xs | ForEach-Object { ($_ - $m) * ($_ - $m) } | Measure-Object -Average).Average
  100.0 * [Math]::Sqrt($v) / $m
}
foreach ($c in $Clips) {
  if ($awall[$c].Count -lt 2) { continue }
  $as = $awall[$c] | Sort-Object; $bs = $bwall[$c] | Sort-Object
  "{0,-38} {1,10:N1} {2,10:N1} {3,10:N2} {4,10:N2}" -f `
    (Split-Path $c -Leaf), (CV $awall[$c]), (CV $bwall[$c]), ($as[-1]/$as[0]), ($bs[-1]/$bs[0])
}

$betweenSpread = ($meds | Measure-Object -Maximum).Maximum - ($meds | Measure-Object -Minimum).Minimum
$worstWithin = ($within | Measure-Object -Maximum).Maximum
"BETWEEN-clip spread of medians : {0:N3}" -f $betweenSpread
"WORST WITHIN-clip spread       : {0:N3}" -f $worstWithin
if ($betweenSpread -le $worstWithin) {
  "=> NOT a content effect on this evidence: one clip's own run-to-run range"
  "   covers the entire gap between clips. Do not fit a dispatch to this."
} else {
  "=> Between-clip spread EXCEEDS within-clip range: a content effect is"
  "   admissible. Still needs a mechanism before it is a gate."
}
