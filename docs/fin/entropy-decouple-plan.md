# Entropy decoupling — taking the CABAC serial chain off the wall clock

*2026-08-05. Follows the engine-survey close (WHYS Part 24): the per-bin chain
is at its irreducible core, so the remaining entropy win is OVERLAP — parse
and reconstruction proceed concurrently, hiding the ~25% entropy+syntax mass
behind the ~75% pixel mass.*

## The enabling facts (each verified in this codebase)

1. **Parsing needs no pixels.** Bins → coefficients/MVs/modes depend on
   syntax-neighbor state only (nzc caches, mvd caches, skip flags, mb types).
   MV prediction, P_Skip inference, and B spatial direct read neighbor MVs —
   syntax-derived. B temporal direct reads the CO-LOCATED frame's motion,
   which is a product of that frame's PARSE, not its reconstruction.
2. **Reconstruction of an inter MB needs no parse state** beyond a compact
   job: block MVs/refs, coefficient blocks, nnz counts, qp, transform flags.
   Reference pixels come from the DPB (immutable during the picture).
3. **Intra is the one coupling**: an intra MB's reconstruction reads neighbor
   PIXELS, so every deferred job at lower addresses must complete first.
   Intra MBs are ~3.6% of this corpus's P/B pictures — rare flush points.
4. Row-interleaved deblocking (Part 20) already made reconstruction proceed
   row-by-row behind a watermark — the same watermark a decoupled parser
   publishes.

## Stages

- **E1 (defer-and-flush seam, single-threaded)** — the P-path inter/skip
  reconstruction is extracted into `recon_*_job` methods taking a compact
  job struct; the parse loop enqueues jobs and FLUSHES (replays in order, on
  the same thread) before any intra MB, at each row boundary (ahead of the
  row deblock hook), and at slice end. Byte-identical by construction: the
  replay order equals today's inline order at every point where pixels are
  observable (intra reads, row filtering). Knob `RS_H264_EDC=1` opt-IN until
  E2 pays; expected cost of the seam alone: job copy traffic (~2.6 KB/MB).
- **E2 (the thread)** — the flush boundary becomes a `sync_channel` to a
  scoped worker owning the pixel side (rec planes, bak rows, refs, its own
  qp/bs copies fed by the jobs); flush = send marker + wait. Parse of row
  r+1 then overlaps reconstruction of row r. Prize: up to
  min(parse, recon) ≈ 25% of decode.
- **E3** — B path jobs (the b_mc region list is recorded rather than
  executed; direct derivation stays parse-side, it is syntax-only).
- **E4** — intra jobs (modes + coeffs; recon-side availability), turning
  flush points into ordinary jobs and removing the stalls.

## Gates

Byte-identical 9/9 both knob arms at every stage; conformance 160/160;
workspace suites; ABBA per stage. E2 additionally: thread-clean shutdown on
error paths (a poisoned picture must fall back to inline), and the
`RFF_ABL_*` knobs keep their meanings.
