# big-oppy-encoder

## 1. Benchmark vs x264 (= ffmpeg's H.264 encoder)

| axis                   | record                                 |
| ---------------------- | -------------------------------------- |
| BD-rate, x264 defaults | ~30% behind on natural content         |
| BD-rate, matched tools | ~2% behind                             |
| BD-rate, all-intra     | WE WIN (tsrc-class content)            |
| speed gap owner        | ME: 81% of encode (fast preset)        |
| speed gap mechanism    | per-CALL 10.3× vs x264, not call count |
| quality preset         | enc-me(best_part) = 91.6% of total     |

Gate for any change: 4-QP per-clip BD (distribution, not mean) + round-trip
+ ffmpeg decode byte-exact.

## 2. All content types

Corpus classes (`video-tests/manifest.tsv` + synthetic clips). BD = class has
per-clip entries in the standing BD gates.

| class          | clips                                 | known per-class facts                                         | BD  |
| -------------- | ------------------------------------- | ------------------------------------------------------------- | --- |
| static         | akiyo, FourPeople                     | B-frames win; skip dominates; akiyo 46.5% null-subpel outlier | ✓   |
| medium         | foreman, in_to_tree                   | the default path                                              | ✓   |
| detail         | mobile, city, harbour, shields, ducks | AQ loses on synthetic-like texture                            | ✓   |
| pan            | bus, stockholm                        | mbtree wins (tsrc −1.8%)                                      | ✓   |
| complex        | tempete, crew                         | crew flash frames broke B2                                    | ✓   |
| fastmotion     | football, soccer, park_joy, crowd_run | B-frames lose (+3.6%); mbtree backs off                       | ✓   |
| smooth         | blue_sky                              | B-frames win (−19.6%); DC-heavy                               | ✓   |
| grain          | grain_akiyo, grain_flat               | grain-floor signal exists, unconsumed                         | —   |
| screen content | screen_text, screen_ui                | nothing exists for it                                         | —   |

Cross-cutting event types inside any class:

| event type           | what it is                    |
| -------------------- | ----------------------------- |
| scene cut            | GOP-internal full change      |
| fade / lighting      | global luma ramp              |
| flash frame          | 1-frame exposure spike (crew) |
| duplicate / near-dup | static repeat frames          |
| letterbox border     | dead flat bands               |
| noise vs true detail | the AQ self-limit axis        |

## 3. Main gate per content — how each type is routed today

| content type / event  | gate/signal today                   | channel it routes into                    |
| --------------------- | ----------------------------------- | ----------------------------------------- |
| busy vs smooth GOP    | B2 translational-gain (per frame)   | bframes auto on/off                       |
| predictable GOP       | predictability signal (per GOP)     | mbtree strength back-off; I/B QP cascades |
| flat vs busy MB       | 256·variance (lme clip table)       | AQ per-MB QP                              |
| high-variance MB      | variance top-fraction (tune_satd_q) | SAD → SATD cost routing                   |
| texture/motion frame  | lme_hi / tex / motion thresholds    | lambda scale                              |
| coherent motion       | me_wide coherence signal            | wide-search on/off                        |
| heavy 16x16 residual  | split_gate (qstep formula)          | 16x8/8x16 + sub-8x8 search                |
| free-skip-rich frame  | online free-skip %                  | greedy_skip / rd_skip enable              |
| direct-wins B content | bskip busy% + dirwin%               | RD B_Skip                                 |
| MB shape population   | structural shape signals            | best_part centre-2 / shape RD             |
| dense CABAC frame     | bits/MB (EDC dispatch)              | defer-and-flush recon seam                |
| synthetic vs natural  | signal EXISTS — no consumer         | (should gate AQ)                          |
| grain floor           | signal EXISTS — no consumer         | (should gate deadzone/AQ)                 |
| screen content        | NONE                                | NONE                                      |
| scene cut             | NONE (B-intra escape exists unused) | NONE                                      |
| fade / lighting       | NONE (weighted-P is manual)         | NONE                                      |
| duplicate frames      | NONE beyond ordinary skip           | NONE                                      |

## 4. Pipeline anatomy

Fast preset, of record (docs/WHYS-speed-gap.md speed map). Shares of encode.

| stage                      | ms    | % encode | status                    |
| -------------------------- | ----- | -------- | ------------------------- |
| enc-me total               | 261.0 | 81%      | the gap owner             |
| ├ me-subpel                | 141.4 | 44%      | U1 — dominant cost        |
| ├ me-diamond               | 90.7  | 28%      | U2                        |
| ├ me-rescue                | ~0    | ~0%      | R6 gate works             |
| └ residue (seed/pred/glue) | ~29   | 9%       | named enough              |
| enc-inter-code             | 18.9  | 6%       | U5                        |
| enc-cavlc-emit             | 11.4  | 4%       | at parity with x264       |
| enc-prep + hpel build      | 15.3  | 5%       | fine (278 µs/frame build) |
| deblock                    | 3.7   | 1%       | closed (R4)               |

Nested primitives: me-hpel-read 31.9 ns ×2.16M, me-cost/satd 17.3 ns ×2.04M,
inter-mc 106.5 ns ×219k. Quality preset: enc-me(best_part) 91.6% of 266.6 s
corpus total.
