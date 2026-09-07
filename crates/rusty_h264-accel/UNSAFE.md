# `unsafe` inventory — rusty_h264-accel

**Last audited**: 2026-09-07. **Next review**: 2026-12-07, or whenever a kernel is
added, which is the only way this file goes stale.

Generated from the tree, not written from memory: the counts below are what the
source contains today. Regenerate rather than edit.

## Why all of it is in one crate

`rusty_h264-accel` is the workspace's **only** crate that may use `unsafe`, and
that is enforced rather than agreed: `[workspace.lints.rust] unsafe_code =
"forbid"` is the default, and exactly two manifests opt out — this one and
`rusty_h264-memprobe` (a test-only allocator probe). So "which code may use
`unsafe`?" is a two-line grep, and the crate that eats attacker-controlled
bytes — `rusty_h264-decoder` — cannot contain any.

That isolation is the single most valuable structural control in the codebase
(see [`docs/threat-model.md`](docs/threat-model.md) §5), and it only holds while
nothing else opts out.

## What the `unsafe` is FOR

One thing only: **calling SIMD intrinsics behind a runtime feature check.**
Every `unsafe fn` here is a `#[target_feature]` kernel; every `unsafe` block is
either a call to one of those or an intrinsic call inside one. There is no raw
pointer arithmetic on heap data, no `transmute` of a lifetime, no manual
`Send`/`Sync` (0 across the whole workspace), and no `static mut` (also 0).

The safety obligation is therefore always the same two-part one:

1. **The target feature is present.** Established by `is_x86_feature_detected!`
   in the safe dispatcher immediately above the call, never assumed.
2. **Every load and store is in bounds.** The kernels take fixed-size arrays
   (`&[i16; 16]`, `&[u8; 16]`) or slices whose extents the dispatcher asserts
   before the call. The caller proves it in safe code; the kernel relies on it.

Each block states which of these it is relying on. As of this audit **every**
`unsafe` block carries a `// SAFETY:` comment, and
`clippy::undocumented_unsafe_blocks` is wired into the workspace lint policy so
a new one without a comment fails the build.

## Inventory

| file | `unsafe fn` | `unsafe {}` | `// SAFETY:` | `#[target_feature]` |
|---|--:|--:|--:|--:|
| `chroma_mc.rs` | 4 | 4 | 4 | 2 |
| `deblock_simd.rs` | 46 | 2 | 2 | 4 |
| `hpel.rs` | 2 | 2 | 2 | 2 |
| `idct4x4.rs` | 16 | 7 | 7 | 2 |
| `luma_mc.rs` | 42 | 26 | 26 | 26 |
| `mectx.rs` | 1 | 1 | 1 | 0 |
| `satd_avg.rs` | 12 | 5 | 6 | 12 |
| `satd_sad.rs` | 13 | 4 | 4 | 8 |
| `transform_quant.rs` | 19 | 12 | 9 | 0 |
| `x86_asm.rs` | 6 | 12 | 12 | 6 |
| **total** | **161** | **75** | **73** | **62** |

`cargo geiger` (2026-09-07): **160/160** unsafe functions and **5,609/5,611**
unsafe expressions, i.e. essentially the whole crate — which is the intended
shape. A geiger report that shrank would mean kernels had been deleted, not that
the codebase got safer.

## How each kernel is gated

Every kernel has a **scalar twin that is the oracle**, and a test asserting they
agree over randomised input — e.g. `mb_uniform_sse2_matches_avx2` runs 50,000
rounds, including inputs that differ in exactly one lane so an off-by-one in a
mask cannot hide. Above that sits the decoder's own differential gate: 68
x264-encoded streams required byte-identical to ffmpeg on every run.

A memory-safety bug in a kernel would have to survive both the scalar oracle and
byte-identity on 68 real streams. That is not a proof — see the open item below.

## Open items

* **`unsafe_op_in_unsafe_fn` is `allow` in this crate** (`deny` everywhere
  else). Every one of the 161 `unsafe fn` bodies is a sequence of intrinsic
  calls, so denying it asks for ~5,600 blocks that each restate the signature.
  Promoting it means wrapping the bodies first. Tracked in the crate's H-16 row.
* **No formal proof (H-30).** No Kani harness exists for any kernel. The
  argument today is the scalar oracle plus byte-identity, which is evidence,
  not verification.
