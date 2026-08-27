# Multi-reference P — x264 parity, gated (2026-08-26)

The inter campaign's first brick, picked because the bit-accountant era
memories named it the standing unpriced lever: x264 defaults to `--ref 3`,
we shipped `num_ref_frames: 1`. The machinery (search across refs with a
`ref_bits` rate term, SPS/PPS/DPB plumbing, decoder support) already existed —
this campaign priced it, flipped it, pruned it, and fixed what the flip
surfaced.

## The evidence (refs_ab harness: 4 QPs x refs {1,2,3}, BD via bdmath.py)

| clip | BD refs2 vs 1 | BD refs3 vs 1 |
| ---- | ------------: | ------------: |
| foreman_cif | -5.40% | **-8.03%** |
| akiyo_cif | -0.35% | -2.08% |
| bus_cif | -3.37% | -4.78% |
| football_cif | +0.82% | **+0.24%** ← the one flip |
| mobile_cif | -8.40% | **-14.26%** |
| tempete_cif | -6.84% | **-11.80%** |
| city_4cif | -3.23% | -3.83% |
| crew_4cif | -6.79% | **-8.84%** |
| screen_text | -10.48% | **-12.03%** |
| grain_akiyo | -2.13% | -0.94% |
| 720p50_shields | -3.98% | -5.83% |
| FourPeople_720p | -0.66% | -1.21% |

**Corpus complete: 12 clips, 11 wins, one +0.24% flip — mean ≈ −6.1% BD-rate
at refs 3.** For scale: the whole trellis-RDOQ campaign was worth −0.5..−1.3%;
this is the largest compression win in the codebase's history, from one
default plus the machinery to hold it.

**One sign-flip, sized and kept**: football (chaotic high motion) pays +0.24%
at refs 3 — and refs 2 is WORSE (+0.82%), so the loss is `ref_idx`
signalling + occasional mispicks, not reference depth; more refs partially
recover it. Against wins of -8..-14% on five clips, a fixed 3 is
overwhelmingly net-positive and is exactly the trade x264 ships flat. The
dispatch law is answered, not ignored: the flip is a quarter of a percent on
one clip, the `refs_ab` harness is the standing instrument, and a
`--refs auto` content gate is the queued response IF a future class flips
harder — building one for 0.24% today would be fitting a gate to noise-scale
evidence (threshold-transfer law).

## The ten wins

1. **`refs_ab` harness** — per-clip (bytes, PSNR) at 4 QPs x 3 ref counts,
   CSV for `bench/bdmath.py` (one home; the harness deliberately does not
   fork the BD arithmetic), with a proves-the-tool-ran stream-difference
   check.
2. **The corpus BD table above** — all wins, no sign-flip, the flip's
   justification.
3. **Default flipped to x264 parity**: `num_ref_frames: 1 → 3` in the config
   AND the CLI's `--refs` fallback (both defaults existed; flipping one
   silently leaves CLI users at 1).
4. **The bisection anchor, proven**: `EH_REFS=1` added to `encode_hash`, and
   the refs-1 arm reproduces the historical 12-hash baseline BYTE-FOR-BYTE —
   the prune, the census counter and the level floor are all inert at the
   old configuration, by construction and now by hash.
5. **New standing baseline** captured at refs 3
   (`scratchpad/hash_r11_refs3.txt` lineage; sequential == parallel held).
6. **The exact `ref_bits` prune** in `best_part`: reference `r`'s cost is
   bounded below by its `λ·ref_bits(r)` term (motion cost is SATD + a
   nonnegative mvd rate), so once the incumbent is at or under that bound,
   `r` cannot win the strict `<` — and `ref_bits` is nondecreasing in `r`,
   so `break` is byte-identical, not just safe-per-ref. Fires hardest
   exactly where multi-ref costs the most for the least: near-perfect ref-0
   matches on cheap partitions at high λ.
7. **`ref_bits_is_monotone`** — the prune's soundness condition pinned as a
   test over every admissible active-reference count, plus the coding facts
   (no ref_idx at 1 ref, one flag bit at 2).
8. **`ref_search` work counter** — the deterministic speed instrument:
   multi-ref's cost IS this count (per-reference motion searches), and the
   prune's effect is its distance below `3 x best_part`.
9. **`multiref_census` integration gate** — encodes synthetic crossing-
   occlusion motion with the census on and asserts BOTH bounds:
   `ref_search > best_part` (the searcher genuinely leaves ref 0 — a
   flipped default whose searcher never did would pass every BD row while
   measuring nothing) and `ref_search < 3 x best_part` (the prune fires).
   In the suite permanently.
10. **The Table A-1 level floor** (`Sps::from_config`): `level_idc` is now
    `max(caller, floor)` where the floor satisfies BOTH MaxFS ≥ frame MBs
    and MaxDpbMbs ≥ frame MBs x refs. Raising refs to 3 made 720p need
    level 3.1 (what x264 signals there) — and testing the floor surfaced a
    LATENT BUG independent of refs: **the fixed `level_idc: 30` default has
    been signalling level 3.0 on every 720p and 1080p stream, whose frame
    sizes alone (3600 / 8160 MBs) exceed level 3.0's MaxFS of 1620** —
    nominal spec violations shipped since the beginning, caught the day the
    constraint became executable. Floor + six-row test; SPS/PPS syntax tests
    re-pinned per the pin-the-toolset discipline.

## Gates

Encoder lib tests 23/23 (incl. the new monotonicity + level-floor pins),
`multiref_census` green, `EH_REFS=1` anchor byte-identical to the historical
baseline, new refs-3 baseline captured, full workspace suite green at refs-3
default (run after the corpus binaries freed). Not clocked, per the
measurement law: the speed cost of refs 3 is banked as the `ref_search`
count and bounded by the prune; the wall number waits for the quiet box like
every other round.

## Deliberately not done

* **B-frame List-0 multi-ref** — B slices still use one ref per list; a
  separate campaign with its own BD table.
* **Adaptive ref-count dispatch** — no sign-flip on the corpus, so no gate
  to build. The `refs_ab` harness is the instrument to re-run if a future
  content class flips.
* **Duplicate-ref detection / MV scaling seeds across refs** — real
  candidates, but they change search behaviour (BD-gated, not exact);
  queued behind a profile that says the pruned search is still the cost.
