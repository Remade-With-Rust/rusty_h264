# Threat model — rs_h264

**Last reviewed**: 2026-09-07 (against the tree at v0.16.0 + the panic-elimination
commit 8335d92). **Next review**: 2026-12-07, or at the next major feature or
dependency change, whichever is sooner.

This is a STRIDE pass over an offline video codec. The scope is deliberately
narrow because the system is: it converts bytes to pixels and back, opens no
sockets, and stores nothing.

---

## 1. What this system is

Six shipped crates. The one that matters for security is the decoder, because
it is the only one that consumes input an attacker chooses.

| Crate | Consumes | Trust |
|---|---|---|
| `rusty_h264-decoder` | **H.264 bitstreams** | **fully untrusted** |
| `rusty_h264-common` | the same bytes, via `BitReader` / NAL splitting | **fully untrusted** |
| `rusty_h264-accel` | buffers + lengths from the two above | trusted caller, `unsafe` inside |
| `rusty_h264-encoder` | caller-supplied YUV frames | trusted (the caller's own pixels) |
| `rusty_h264` | -- (facade) | n/a |
| `rusty_h264-cli` | a path from `argv`, then bytes from that file | **fully untrusted** |

## 2. Assets

1. **Availability of the decoding process.** The dominant asset. A codec is
   usually embedded in something bigger -- a media player, a transcoding farm, a
   browser upload path -- where a panic or a hang takes down more than the decode.
2. **Memory safety of the host process.** Concentrated entirely in
   `rusty_h264-accel`; everything else is `#![forbid(unsafe_code)]`.
3. **Integrity of decoded pixels.** A decoder that silently produces wrong
   pixels can mislead anything downstream that makes decisions on them.

There is no confidentiality asset: no secrets, no keys, no personal data, no
network egress.

## 3. Adversaries

| # | Adversary | Capability | Motivation |
|---|---|---|---|
| A1 | **Anyone who can supply a file or stream** | arbitrary bytes, unlimited attempts, offline crafting | crash the host, execute code, exhaust memory |
| A2 | A malicious *upstream* of a transcoding pipeline | as A1, at scale, automated | denial of service against the fleet |
| A3 | A supply-chain attacker | publish a malicious version of a dependency | code execution in anything that builds this |
| A4 | A local user of the CLI | chooses input and output paths | escape the process's own privileges |

A1 is the adversary this codebase is actually designed against.

## 4. STRIDE

| | Threat | Applies? | Control |
|---|---|---|---|
| **S** | Spoofing | **No** | No identity, no authentication, no network peer to impersonate. |
| **T** | Tampering | **Yes** -- with the *input*, which is the whole point | Input is assumed hostile by construction. The decoder is `forbid(unsafe_code)`, so every index is compiler-checked; `BitReader` returns `Result` on every read. Tampering with the *output* is out of scope: the caller owns the frames. |
| **R** | Repudiation | **No** | No audit-relevant actions; the codec makes no claims to log. |
| **I** | Information disclosure | **Minimal** | Nothing secret is in the process. The residual case is uninitialised memory reaching output pixels, which `forbid(unsafe_code)` prevents in every crate except `-accel`. |
| **D** | **Denial of service** | **YES -- the primary threat** | See §5. |
| **E** | Elevation of privilege | **Only via memory corruption** | Confined to `-accel`'s `unsafe` kernels. |

## 5. The highest-value attack path

> **A crafted H.264 bitstream reaches `Decoder::decode` and causes a panic, an
> unbounded allocation, a hang, or an out-of-bounds access in a SIMD kernel.**

Controls, in the order an attacker meets them:

1. **`#![forbid(unsafe_code)]` on the decoder and on `common`'s shipped build.**
   Every slice index on the parse path is bounds-checked by the compiler. This
   is the single most valuable control here and it is structural, not a
   convention someone must remember.
2. **No panics on the parse path.** All 47 production `.unwrap()`/`.expect()`
   sites were removed (commit 8335d92); the API returns `Result<_, DecodeError>`
   and the typed `DecodeError::Internal` carries conditions that used to abort.
3. **A mutation fuzzer in CI** (`tests/fuzz_no_panic.rs`): encoder-produced
   streams corrupted thousands of ways, plus pure-random buffers, all under
   `catch_unwind`, asserting only `Ok`/`Err`.
4. **A differential gate against ffmpeg**: 68 x264-encoded streams required
   byte-identical on every run, plus a 258-stream derivation sweep. This is the
   control for pixel *integrity* -- the one a fuzzer cannot provide.
5. **`unsafe` isolated to one crate**, whose kernels are each pinned to a scalar
   twin by a `*_matches_scalar` oracle.

### Known residual exposure

* **`-accel` takes its lengths on trust.** The kernels assume the decoder passed
  valid buffer extents. That contract is real but was, until this audit, not
  written down; ~85% of its `unsafe` blocks carry a `SAFETY:` comment.
  Tracked as the `-accel` unit's H-16.
* **Fuzzing is mutation-based, not coverage-guided.** Broad classes are covered;
  deep state machines (multi-slice, B-pyramid, MMCO) are reached by chance
  rather than by coverage feedback. Tracked as H-26/H-27.
* **Memory growth is bounded by the stream's declared dimensions**, which are
  themselves attacker-controlled within spec limits. A level-conformant stream
  cannot demand unbounded memory; a non-conformant one is rejected.

## 6. What is deliberately NOT defended

* **The encoder against its own caller.** Its input is your frames.
* **Timing side channels.** There are no secrets to leak.
* **The build host.** Covered by the supply-chain gates (H-07 to H-14), not here.

## 7. Review trigger

Revisit this document when any of these change: a new untrusted input surface;
a new `unsafe` block outside `-accel`; a new runtime dependency; or the addition
of any network, filesystem-beyond-argv, or persistence behaviour.
