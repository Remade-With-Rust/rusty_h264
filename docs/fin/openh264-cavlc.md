# openh264 CAVLC — every function, mapped to ours, with the optimizations

Goal: make our CAVLC match openh264's efficiency. Source: `openh264/codec/encoder/
core/src/set_mb_syn_cavlc.cpp` + `svc_set_mb_syn_cavlc.cpp` + `golomb_common.h`.
We've already killed the per-block `Vec` allocations (commit `915201a`, +13–33%).
This is the function-by-function pass for the rest.

## The full function inventory (openh264)

| # | openh264 function | file:line | our equivalent | status |
|---|---|---|---|---|
| 1 | `CavlcParamCal_c` (+ `_sse2`/`_sse42`) | set_mb_syn_cavlc.cpp:84 | inline in `encode_residual_block` | **3 passes → make 1** |
| 2 | `WriteBlockResidualCavlc` | set_mb_syn_cavlc.cpp:108 | `encode_residual_block` (cavlc.rs:337) | **packed writes** |
| 3 | `BsWriteBits` | golomb_common.h:79 | `BitWriter::write_bits` | 32-bit cache, 4-byte flush |
| 4 | `WelsSpatialWriteMbSyn` / `WelsWriteMbResidual` | svc_set_mb_syn_cavlc.cpp | the emit loop in `encode_inter_mb`/`encode_mb` | per-block nc + scan |
| 5 | `StashMBStatus*`, `GetBsPos*`, `WelsWriteSliceEndSyn`, `InitCoeffFunc` | set_mb_syn_cavlc.cpp:234+ | n/a (slice bookkeeping / dispatch) | not hot |

---

## (1) `CavlcParamCal_c` — parameter extraction, ONE pass

```c
int32_t CavlcParamCal_c(int16_t* pCoffLevel, uint8_t* pRun, int16_t* pLevel,
                        int32_t* pTotalCoeff, int32_t iLastIndex) {
  int32_t iTotalZeros = 0, iTotalCoeffs = 0;
  while (iLastIndex >= 0 && pCoffLevel[iLastIndex] == 0) --iLastIndex;   // skip trailing zeros
  while (iLastIndex >= 0) {
    int32_t iCountZero = 0;
    pLevel[iTotalCoeffs] = pCoffLevel[iLastIndex--];                     // next non-zero level (high→low)
    while (iLastIndex >= 0 && pCoffLevel[iLastIndex] == 0) { ++iCountZero; --iLastIndex; }
    iTotalZeros += iCountZero;
    pRun[iTotalCoeffs++] = iCountZero;                                   // run of zeros below it
  }
  *pTotalCoeff = iTotalCoeffs;
  return iTotalZeros;                                                    // also returns total_zeros
}
```
**Key:** one descending pass produces `level[]` (high→low), `run[]` (high→low), `total_coeff`,
and `total_zeros` — all at once. **It even has SSE2/SSE42 asm versions** (`coeff.asm`).

**Ours** (cavlc.rs) does **three** passes: build `positions[]` (ascending), reverse-map
`levels[]`, then a third loop for `run_val[]` + separately compute `total_zeros` from
`positions.last()`. → **Collapse to one descending pass like openh264.**

## (2) `WriteBlockResidualCavlc` — the wins beyond allocation

```c
/* coeff_token: ONE table lookup gives (value,len); trailing-one signs PACKED in */
upCoeffToken = &g_kuiVlcCoeffToken[ncmap][iTotalCoeffs][iTrailingOnes][0];
iValue = upCoeffToken[0]; n = upCoeffToken[1];
... n += iTrailingOnes; iValue = (iValue << iTrailingOnes) + uiSign;   // coeff_token + T1 signs
CAVLC_BS_WRITE(n, iValue);                                              // ← ONE write

/* level: branchless code, prefix+suffix PACKED into one write */
iLevelCode = (iVal - 1) * 2;
uiSign = iLevelCode >> 31; iLevelCode = (iLevelCode ^ uiSign) + (uiSign << 1);  // |code| branchless
iLevelCode -= ((i==iTrailingOnes) && (iTrailingOnes<3)) << 1;
... n = iLevelPrefix + 1 + iLevelSuffixSize;
iValue = ((1 << iLevelSuffixSize) | iLevelSuffix);
CAVLC_BS_WRITE(n, iValue);                                              // ← ONE write per level

/* run_before: single table lookup + single write */
n = g_kuiVlcRunBefore[iZeroLeft][uirun][1]; iValue = g_kuiVlcRunBefore[iZeroLeft][uirun][0];
CAVLC_BS_WRITE(n, iValue);
```

**Ours:** writes the coeff_token, then **loops writing each trailing-one sign as a separate
`write_bit`**; and `write_level` may split prefix/suffix into separate writes. → **Pack
coeff_token + T1 signs into one `write_bits`; pack each level (prefix+suffix) into one.**
Fewer `write_bits` calls = fewer cache-flush loops.

## (3) `BsWriteBits` — 32-bit cache, 4-byte flush

```c
if (iLen < iLeftBits) { uiCurBits = (uiCurBits << iLen) | kuiValue; iLeftBits -= iLen; }
else { iLen -= iLeftBits; uiCurBits = (uiCurBits<<iLeftBits)|(kuiValue>>iLen);
       WRITE_BE_32(pCurBuf, uiCurBits); pCurBuf += 4;                  // ← flush 4 bytes at once
       uiCurBits = kuiValue & ((1<<iLen)-1); iLeftBits = 32 - iLen; }
```
Flushes a whole **u32** to a **pre-allocated raw buffer** (no per-byte `Vec::push`, no
bounds/capacity check). Ours now caches (commit `915201a`) but flushes **one byte at a time**
to a `Vec`. → **Flush u32 words; pre-size the buffer.** (Lower priority — the cache rewrite
alone measured ~0, so emission isn't the bottleneck; revisit after 1+2.)

---

## Execution order (highest leverage first)
1. **One-pass param extraction** (#1) — replace our 3 passes with openh264's single descending loop. Removes 2 passes over every block's coeffs.
2. **Packed writes** (#2) — coeff_token+T1 signs in one write; level prefix+suffix in one write.
3. **u32-flush bit writer** (#3) — only if 1+2 don't close it.

Each step stays **byte-identical** (same bits, computed cheaper) — verify with the
roundtrip tests + `cmp` vs HEAD after each.
