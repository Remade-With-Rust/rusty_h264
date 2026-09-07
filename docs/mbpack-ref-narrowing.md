# Narrowing `MbPack`'s two reference planes from `[i32; 16]` to `[u8; 16]`

**Status: BUILT, MEASURED, REFUTED on correctness. Reverted.** 2026-09-07.

`MbPack` is the packed per-macroblock record the deblock derivation reads. Two of
its fields are reference-identity planes:

```rust
pub ref1:   [i32; 16],   // 64 B
pub ref_id: [i32; 16],   // 64 B
```

128 of the record's 288 bytes, built 6.48 M times per tier. Every use of those
values is an **equality test or a `NO_REF` test** — never an ordering, never an
arithmetic — so in principle any labelling that is injective across a picture
serves, and a byte holds one. This note records why that is nonetheless not a
free change.

## What was built

The whole thing, end to end, and it compiled and passed the unit suites:

* `mb_uniform`'s AVX2 and SSE2 twins narrowed to byte reference planes — two
  256-bit loads, two compares, a `packs` and a lane-fixup permute per plane
  collapse to one `pcmpeqb` plus a sign-extend (AVX2) or nothing at all (SSE2).
  There is no NEON twin for this kernel, so x86 was the whole surface. The
  50,000-round `mb_uniform_sse2_matches_avx2` oracle was extended with a byte
  generator covering both sentinels and a one-differing-lane case, and passed.
* The two `bs_motion_masks` kernels kept their `i32` planes and take a widened
  copy at the call site. That path is the RARE one — 27,295 calls against
  720,000 macroblocks on FourPeople — so expanding sixteen bytes there is far
  cheaper than storing sixteen `i32` in every record.
* `BlockInfo` gained `rid0`/`rid1` (a `ref_idx -> byte id` table per list) and
  the decoder gained a per-picture POC intern table feeding them.

## The ceiling — measure this before trying again

The splat **does** already vectorise (44 `vmovups`, 3 `vbroadcastss` in the
inlined `derive_bs_row`), so the win is not "scalar stores become vector ones".
It is:

* ~5 store instructions per macroblock (4 `ymm` -> 2 `xmm` on the two planes,
  plus 3 fewer `ymm` on the `MbPack::default()` write), and
* ~96 fewer bytes stored per record, with the two-row rolling window's resident
  footprint dropping by a third.

**Modest, and the memory half is not measurable on this box.** Price it against
that before spending the correctness budget again.

> A first ceiling probe replaced `rec.ref_id = [r0; 16]` with `fill(..4)` and read
> −48 instructions. That probe was INVALID: `fill` de-vectorised, so it measured a
> codegen SHAPE change, not a width change. An ablation has to keep the shape it
> is not testing.

## Why it is blocked

The blocker is not the record and not the kernels. It is what the `i32` values
**mean**, which is not one thing:

| slice state | old `ref_id` value |
| --- | --- |
| reference list present | the picture order count, `NO_REF` (`i32::MIN`) if unused |
| reference list EMPTY (an I slice) | the raw `ref_idx`, and `-1` if unused |

A multi-slice picture can carry both, and neighbouring macroblocks from the two
are compared **directly** — a raw `ref_idx` against a POC, two different number
spaces. `-1` is also not `NO_REF`, so an unused block on the empty-list path
counted as carrying a real reference and let its motion term through.

Any byte encoding therefore has to reproduce a relation that includes those
cross-space collisions, exactly. Two attempts, each refuted by
`tests/mslice_idc2.rs` (which passes on HEAD):

1. **Intern the POCs only.** A block with raw `ref_idx` 0 met a block referencing
   POC 4 whose interned id was also 0. A pair the old code called *different*
   became *equal* and the strength on that edge collapsed.
2. **Intern the index values into the same table** (so raw `r` and POC `p`
   collide iff `r == p`, as the `i32` form did), with a supplier-specified id for
   negative indices. This got further and then diverged again, with two
   macroblocks resolving the same POC through what must be two different tables —
   not yet explained, and not explicable by the model above.

**The real fix is upstream of the encoding: give each reference PICTURE a stable
small identity** (a DPB slot that lives as long as the picture) and store that,
instead of reconstructing an identity per slice from POCs. Then the byte plane is
a consequence rather than a re-derivation, and the empty-list path stops being a
second number space. That is a decoder-structure change, not a record change.

## The oracle to use next time

Do not reason about injectivity — assert it on real streams. Carry the old value
beside the new id on the record and check both relations at the one place they
matter:

```rust
// on MbPack, temporarily
pub dbg0: [i32; 16],   // the old map_ref result for list 0
pub dbg1: [i32; 16],

// at the top of pk_differs
assert_eq!(p.ref_id[pk] == q.ref_id[qk], p.dbg0[pk] == q.dbg0[qk]);
assert_eq!(p.ref_id[pk] == NO_REF_ID,    p.dbg0[pk] == NO_REF);
assert_eq!(p.ref1[pk]   == NO_REF_ID,    p.dbg1[pk] == NO_REF);
```

That harness found both divergences in one run each, and printing the whole
`ref_id`/`dbg0` arrays either side is what turned the second one from "wrong
answer" into "these two records used different tables". It is worth building
FIRST, before the kernels.
