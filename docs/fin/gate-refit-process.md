# The Great Gate refit process

Every gate in this encoder is a **threshold on a signal**, fitted against the encoder
as it was on the day it was fitted. The encoder changes underneath them. This file is
the repeatable procedure for re-asking whether a gate's threshold is still right, and
`bench/gate_refit.py` is the harness that enforces it.

It exists because a gate went stale in exactly this way and nobody noticed for months:
`shape_rd_tex_max` was set to 1000 because `mobile_cif` (median_var 1494) **lost
+1.99% BD-SSIM** with the shape-RD pass on. Re-measured on 2026-08-08, the same clip
with the same knob **wins -0.90%** — a ~2.9-point sign flip. The guard had stopped
preventing a regression and started blocking a win.

## The procedure

**0. Find a gate worth re-asking.** Do not sweep them all. Use the labelled census:
`gatecheck` emits `gate4.<name>` / `gate8.<name>` keys, so one run says whether a gate
behaves differently across an axis. A gate whose fire rate is the same on both sides
has nothing to refit. That is how `shape_rd_flip` was picked — it fires 34.9-51.8% on
4x4 macroblocks and 58.0-65.4% on 8x8 ones, on 4 of 4 clips.

**1. Locate every clip on the gate's axis.** Harvest the signal the gate thresholds
on and take its median per clip. A gate only ever acts on **one side** of its line.

**2. Refuse to proceed without content on the acting side.** This is the step people
skip. A census run on six clips containing no grain and no screen clip once concluded
"four of five veto gates never fire" — it had asked a grain veto about non-grain
content. If nothing in the corpus is past the threshold, the run measures the fallback
and nothing else. Synthesize the content (`video-tests/synth_clips.sh`) and start over.

**3. Pin every arm, per clip.** An arm that clears an env var instead of setting a
value falls through to the default and compares a setting against itself. Worse, a
threshold arm must clear **each clip's own signal**: a "2000 = unvetoed" arm still
vetoes `maxtex_plaid` at 2962, and reads a confident 0.00%. The harness marks those
cells `not-an-arm` rather than reporting them.

**4. Hold out BOTH sides of the line.** One above-the-line clip said "delete this
guard"; a synthesized second one refuted it at +3.50%. A threshold validated only from
below is not validated. The shipped value should ideally be bracketed: moving it down
costs something measurable, moving it up costs something measurable.

**5. Check the collateral.** Every clip on the non-acting side must be
**byte-identical** across arms. If one moves, the signal is not what gates that
behaviour and the whole table is void.

**6. Record what the fit rests on.** Name the number of above-the-line points. Two is
thin; one is provisional. Content landing between the old and new thresholds is
untested by definition — say so.

## Worked example (the one that produced this file)

```
python bench/gate_refit.py --signal median_var --knob RFF_SHAPE_RD_TEXMAX \
    --current 2000 --candidates 1000,4000
```

| candidate | clip that moves | BD-SSIM | reading |
|---|---|---:|---|
| T=1000 (the old value) | mobile_cif | +0.91% | reverting loses |
| T=4000 (remove the guard) | maxtex_plaid | +3.50% | removing loses badly |

Every other clip: `not-an-arm` (below the line under both settings). So 2000 is
bracketed by measured holdouts on both sides — the strongest form the evidence can
take with this corpus.

**The harness caught a real defect on its first run.** `--current 2000` parses as a
float, so the env var was set to `"2000.0"`, Rust's `parse::<i64>()` rejected it, and
BOTH arms silently fell back to the default. The byte-identity check surfaced it as
`byte-ident` where a result was expected. That is steps 3 and 5 doing their job — and
the reason they are mechanical rather than a checklist.

## What this process is NOT

It re-fits a threshold on an existing signal. It does not find new signals, and it
does not tell you a gate's threshold is *correctly calibrated* — only whether the
value is better or worse than its neighbours on this corpus. A gate can fire at the
right rate and still fire on the wrong macroblocks; that needs per-clip BD, which is
what this harness measures, on the content the gate actually acts on.

## Adding an axis to the census (step 0's other half)

`signals::census` counts `(fired, seen)` per gate. `commit_mb` additionally buckets
those counts by the macroblock's final transform size, exposed as
`gate_census_by_t8()` and emitted by `gatecheck` as `gate4.*` / `gate8.*`.

Two things to keep in mind if you add another axis:

* **Gates fire DURING mode decision**, before the macroblock's transform size (or any
  other outcome property) exists. A bucket argument at `bump` time would label every
  gate with the PREVIOUS macroblock's answer. Consultations are held in a per-MB
  pending buffer and committed once the property is known.
* **Label, do not split.** Adding a labelled dimension costs no new gate and no new
  threshold, and one census run says whether a split would even be worth fitting. Nine
  fitted thresholds cost nine corpora and nine chances to fit an axis the corpus never
  varied. The transform-size label showed exactly two gates worth splitting
  (`shape_rd_flip`, `sub8_rd_revert`) and seven not.
