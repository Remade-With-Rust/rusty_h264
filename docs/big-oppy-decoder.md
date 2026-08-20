# big-oppy-decoder

## 1. Benchmark vs ffmpeg

1T, pinned CPU time, ABBA ×9, x264-encoded 720p corpus (1800 frames/tier),
byte-identical before timing. Record = 2026-08-12 morning-clean.

| tool tier                  | rusty/ffmpeg | ours Mpx/s | ffmpeg Mpx/s |
| -------------------------- | ------------ | ---------- | ------------ |
| CAVLC (baseline, veryfast) | 2.24×        | 213        | 412          |
| Main (medium)              | 2.04×        | 146        | 294          |
| High (slower)              | 2.04×        | 125        | 255          |

| fact            | value                                                         |
| --------------- | ------------------------------------------------------------- |
| cross-run band  | cavlc 1.98–2.49, main 2.04–2.35, high 2.04–2.25 (box-state)   |
| PGO build       | −3.1% high, −5.3% cavlc (`bench/pgo.sh`)                      |
| 2T vs ffmpeg-2T | 2.75–2.86× wall; our frame-MT busy 1.13–1.20 vs ffmpeg ~1.5   |
| pure-Rust rip   | 1.004× vs last asm build, z=−0.26 (null)                      |
| pure-Rust floor | ~1.4–1.5× (entropy asm gap); wall lever = frame-MT scheduling |

Re-baseline: sustained-quiet ≥10 min, name any hot process, fresh bin builds,
`bash bench/decode_x264_speedtest.sh 9`.

## 2. All content types

The decoder's content is the bitstream: pixel class × syntax tools ×
provenance. All three vary independently.

Pixel classes (bench = in the timed corpus today):

| class          | as the decoder feels it                    | bench   |
| -------------- | ------------------------------------------ | ------- |
| static         | skip-run dominated, few coeffs             | —       |
| medium         | balanced P coding                          | ✓       |
| detail         | dense residual, intra mix                  | ✓       |
| pan            | uniform MVs, qpel-rich                     | ✓       |
| complex        | dense residual + motion                    | —       |
| fastmotion     | partition-rich, sub-8x8, qpel              | anatomy |
| smooth         | DC-only residual, B-heavy                  | —       |
| grain          | max-density residual everywhere            | —       |
| screen content | sharp edges, long flat runs, big skip runs | —       |

Syntax/tool axes:

| axis            | values                          | cost consequence                              |
| --------------- | ------------------------------- | --------------------------------------------- |
| entropy coder   | CAVLC / CABAC                   | CABAC ~1.9× slower same content               |
| slice structure | 1 / N per picture               | availability gating; idc==2 bS suppression    |
| frame types     | I / IP / IPB(+pyramid)          | B = b-mc glue 22%; dep-density kills frame-MT |
| refs            | 1 / N / long-term / MMCO / gaps | reorder, marking, placeholder path            |
| partitions      | 16x16-heavy … sub-8x8-rich      | MC call count; coalescing ladder              |
| MC precision    | full-pel … qpel-rich            | interpolation share swings whole profile      |
| transform       | 4x4 / 8x8 mix                   | cat-5 residual path, scaling lists            |
| special MBs     | skip-heavy / I_PCM / lossless   | flips which fast paths matter                 |
| weighted pred   | none / explicit-P / implicit-B  | explicit-B refused (loud)                     |
| bits per MB     | sparse … dense                  | strongest single cost signal                  |

Provenance: own-encoder / x264 per-preset / other encoders / fuzzed. Own
streams are a narrow dialect (were 100% full-pel; hid half the gap).

## 3. Main gate per content — how each type is routed today

| content type          | gate/signal today               | channel it routes into                   |
| --------------------- | ------------------------------- | ---------------------------------------- |
| entropy coder         | stream flag                     | separate CAVLC / CABAC slice loops       |
| frame type            | slice header                    | I / P / B decode paths                   |
| skip MBs              | mb_skip_run / skip flag         | P_Skip / B_Skip prediction-copy recon    |
| AC-empty block        | cbf / nnz == 0                  | DC-only residual fast path               |
| sparse vs dense block | nnz ≤ 6                         | scatter path vs dense 16-mul path        |
| partition shape       | mb_type                         | mc_rect coalescing ladder                |
| MC precision          | mv fractional bits              | full-pel direct vs hpel/qpel kernels     |
| uniform-motion MB     | mb_uniform (AVX2)               | single bS kernel call                    |
| dense-residual frame  | bits/MB > 38.4 ∧ CABAC ∧ ≤5k MB | EDC defer-and-flush seam (auto)          |
| multi-slice           | first_mb_in_slice               | availability gating (ctx + pixels)       |
| pixel class itself    | NONE                            | arrives pre-encoded as the proxies above |

## 4. Pipeline anatomy

Sampled profiler, 2026-08-12, trustworthy. Shares of decode.

| stage                  | high (CABAC)         | cavlc    | SIMD state              |
| ---------------------- | -------------------- | -------- | ----------------------- |
| entropy                | 22.3% (+8.3% syntax) | 31.2%    | serial — engine CLOSED  |
| inter-MC (+b-mc glue)  | 19.5% (+22% INFO)    | 14.7%    | SSE2/AVX2/NEON done     |
| per-MB glue / row-hook | ~26% class           | ~similar | structural, not SIMD    |
| deblock                | 6.8%                 | 14.2%    | SSE2/NEON done          |
| dequant + reconstruct  | ~5%                  | ~14%     | hybrid; dense inherent  |
| dpb-clone              | 2.3%                 | 1.9%     | memcpy-bound            |
| intra-pred             | 0.3%                 | 0.9%     | not worth it (measured) |
