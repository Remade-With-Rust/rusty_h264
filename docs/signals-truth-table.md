# P1 signal truth table — per-clip signal vector vs content class

The Great Gate P1 validation (docs/great-gate.md §6 P1): every signal in the
per-frame vector (`encoder/src/signals.rs`), measured per clip across the full
corpus PLUS the two synthesized corpus-gap classes, against hand-labelled
content classes from the §2 taxonomy.

**Method line.** 60 frames/clip, qp 28, gop 30, CLI defaults (CABAC, fast
preset, AQ 1.0, mb-tree on), `RUSTY_THREADS=1`, tap `RFF_SIGNALS_CSV`. Cell =
**median over the 58 P-frame rows** (2 IDR + 58 P per clip — count verified).
Signals are deterministic counters, not clocks: one run is the verdict
(`codec-measurement` §15). Regenerate with `video-tests/synth_clips.sh` +
the harvest loop in the P1 session record; aggregation = median per column.

| class | clip | mgain | dcfrac | headroom | gmc | med_var | lv_spread | flat_run | hist_top16 | grain_floor |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| GRAIN | grain_akiyo | 0.014 | 0.074 | 0.41 | 8.36 | 121 | 1.60 | 1.04 | 0.208 | **7.96** |
| GRAIN | grain_flat | 0.081 | 0.074 | 5.20 | 7.51 | 57 | **0.13** | 1.04 | 0.707 | **7.58** |
| SCREEN | screen_text | 0.000 | 0.024 | 0.00 | 0.70 | **0** | **7.55** | **22.73** | **0.976** | 0.00 |
| SCREEN | screen_ui | 0.297 | 0.373 | 39.31 | 2.32 | **0** | **8.34** | **13.68** | **0.934** | 0.00 |
| busy-motion | crowd_run_1080p50 | 0.074 | 0.207 | 3.53 | 10.94 | 300 | 2.66 | 1.07 | 0.142 | 3.81 |
| busy-motion | ducks_take_off_1080p50 | 0.041 | 0.287 | 0.33 | 8.07 | 426 | 1.70 | 1.08 | 0.186 | 6.01 |
| busy-motion | park_joy_1080p50 | 0.257 | 0.370 | 19.35 | 11.49 | 158 | 3.22 | 1.10 | 0.309 | 4.38 |
| busy-tex | harbour_4cif | 0.028 | 0.199 | 0.05 | 6.83 | 663 | 1.66 | 1.06 | 0.157 | 6.59 |
| busy-tex | mobile_cif | 0.011 | 0.111 | 0.42 | 12.83 | 1494 | 2.08 | 1.11 | 0.202 | 9.09 |
| busy-tex | tempete_cif | 0.010 | 0.176 | 0.89 | 7.99 | 785 | 2.03 | 1.12 | 0.320 | 4.01 |
| flash | crew_4cif | 0.070 | 0.362 | 3.39 | 4.05 | 52 | 2.27 | 1.22 | 0.235 | 1.98 |
| motion-local | football_cif | 0.347 | 0.358 | 27.33 | 20.35 | 449 | 2.07 | 1.12 | 0.339 | 9.22 |
| motion-local | foreman_cif | 0.136 | 0.363 | 2.14 | 4.94 | 281 | 3.03 | 1.19 | 0.268 | 2.61 |
| motion-local | foreman_qcif | 0.040 | 0.341 | 0.55 | 5.33 | 793 | 2.35 | 1.16 | 0.270 | 3.62 |
| motion-local | soccer_4cif | 0.241 | 0.284 | 11.17 | 6.57 | 100 | 2.34 | 1.17 | 0.239 | 4.43 |
| pan-fast | bus_cif | 0.324 | 0.216 | 24.41 | 15.38 | 694 | 2.31 | 1.09 | 0.281 | 10.67 |
| pan-natural | 720p50_shields_ter | 0.531 | 0.264 | 35.82 | 6.11 | 331 | 2.28 | 1.08 | 0.244 | 6.99 |
| pan-natural | blue_sky_1080p25 | 0.363 | 0.666 | 37.90 | 6.87 | 2 | 4.34 | 1.36 | 0.281 | 1.53 |
| pan-natural | in_to_tree_420_720p50 | 0.029 | 0.134 | 0.84 | 5.02 | 40 | 2.06 | 1.09 | 0.233 | 3.27 |
| pan-struct | 720p5994_stockholm_ter | 0.152 | 0.152 | 0.44 | 4.85 | 163 | 2.36 | 1.08 | 0.203 | 4.45 |
| pan-struct | city_4cif | 0.072 | 0.182 | 0.00 | 7.60 | 295 | 1.76 | 1.09 | 0.233 | 7.90 |
| static | FourPeople_1280x720_60 | 0.013 | 0.228 | 0.82 | 2.15 | 61 | 3.59 | 1.24 | 0.211 | 1.21 |
| static | akiyo_cif | 0.001 | 0.178 | 0.00 | 1.95 | 61 | 3.19 | 1.27 | 0.272 | 1.00 |
| static | akiyo_qcif | 0.000 | 0.191 | 0.00 | 2.35 | 208 | 2.58 | 1.32 | 0.274 | 1.16 |

## Verdicts on the two new axes (the P1 ask)

**Synthetic axis — BOTH signals separate outright, no conjunction needed:**

- `flat_run`: screen min **13.68** vs non-screen max **1.36** — a 10× gap.
  Natural content (grain included) never exceeds 1.4; even blue_sky's smooth
  gradients read 1.36.
- `hist_top16`: screen min **0.934** vs non-screen max 0.707 — and that 0.707
  is `grain_flat` (a synthetic gray card); the *natural* max is 0.339.
- Free third tell: `median_var = 0` on both screen clips (flat background
  dominates the subsampled median) and `lv_spread 7.5–8.3` vs natural 1.6–3.6
  — confirming the AQ back-off doc's "synthetic ~6+, natural ~1–3" claim on
  real measurements.

**Grain axis — separates its target class; the motion confound is real and
resolvable, exactly as the signal's doc predicted:**

- `grain_floor`: grain **7.6–8.0** vs clean static **1.0–1.2** (6.5×) — noise
  never predicts, clean static predicts almost perfectly.
- Confound: high-motion/high-texture clips also read high (bus 10.7, football
  9.2, mobile 9.1). The joint read separates them: grain has the high floor at
  **mgain ≈ 0.01–0.08** (searching does NOT reduce the residual — it is noise)
  and **low median_var** (57–121); bus/football carry mgain 0.32–0.35 (motion
  explains their floor), mobile carries median_var 1494 (texture explains
  its). ⚠ Only 2 grain clips — by the enumerate-the-combination-space law this
  is a hypothesis for the P2 optimizer's conjunction search, NOT a fitted
  gate. Do not hand-ship a 3-term grain gate off n=2.

**Grain kills the AQ premise from both ends** (the gate fit should see this):
`grain_flat` reads lv_spread **0.13** — uniform noise = flat variance
everywhere = AQ's spread back-off does NOT fire, while "busy = maskable" is
false on grain. The AQ default-on fit (P2 priority 1) must include the grain
class.

## Cross-checks vs the documented clip tables

Ordering is preserved against the lme/me_wide calibration tables (akiyo lowest
gmc 1.9, bus/football highest 15.4/20.4; mobile's median_var 1494 ≈ the
documented 1554, tempete 785 ≈ 746, akiyo 61 = 61). Absolute gmc values sit
BELOW the documented ones on some clips (foreman 4.9 vs 9.6, bus 15.4 vs 27.5)
— different measurement window (60 frames, qp 28, current defaults incl.
mb-tree/AQ) than the original calibration runs. This is the
recalibrate-on-the-deployed-estimator law in action: any threshold fitted
against THIS table must be validated on the config that ships it, and the
existing lme/me_wide thresholds should be re-swept in P2 on the current
defaults before new gates stack on them.

## P2 handoff

The per-clip CSVs (one row per slice, signals + gate decisions) are the
`gate_optimizer` input contract. Assign train/holdout splits BY CLIP offline;
fit depth-≤4 trees; judge once. Priority per the plan: AQ default-on (now with
the grain class present), mb-tree default-on, RD-skip preset gate, RDOQ inter.
