# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report privately through GitHub's [private vulnerability
reporting](https://github.com/Remade-With-Rust/rs_h264/security/advisories/new)
on this repository. If that is unavailable to you, email
**security@thehouseinc.xyz** with `rs_h264` in the subject.

Include, as far as you have it: the affected crate and version, the input that
triggers the problem (a bitstream is ideal -- attach it, do not paste it), what
you observed, and what you expected.

## What to expect

| Stage | Target |
|---|---|
| Acknowledgement of your report | **3 working days** |
| Initial assessment, with a severity and a plan | **10 working days** |
| Fix released for a confirmed High/Critical issue | **30 days** from acknowledgement |
| Public advisory | at release, or by mutual agreement earlier |

We will keep you updated if any of these slip, and we will credit you in the
advisory unless you ask us not to.

## Coordinated disclosure

We ask for **90 days** before public disclosure, or until a fix ships --
whichever comes first. If we cannot fix a confirmed issue inside 90 days we
will say so, publish the mitigation, and agree a date with you rather than let
the clock run silently.

## Scope

This repository is an offline H.264 codec: it converts bytes to pixels and back.
It opens no sockets, stores no credentials, and processes no personal data.

**In scope** -- the things that can actually go wrong here:

* Any input that makes the **decoder** panic, hang, abort, or allocate without
  bound. The decoder eats attacker-controlled bitstreams, so an unwind is a
  denial-of-service primitive, and we treat it as a security bug rather than a
  robustness one. It is `#![forbid(unsafe_code)]` and carries no production
  `.unwrap()`, so a panic is a finding.
* Memory-safety problems in `rusty_h264-accel`, the one crate that uses
  `unsafe` (SIMD kernels). Out-of-bounds reads or writes driven by crafted
  input are the highest-severity class in this repository.
* Any divergence where we decode a stream differently from the reference
  decoder in a way that is exploitable downstream.

**Out of scope**: build-time-only dependencies (the side-by-side viewer's
`minifb` and its tree are `dev-dependencies` and never ship), performance
regressions, and bitstreams that are simply invalid and are correctly rejected
with a `DecodeError`.

## Supported versions

The latest released minor version receives security fixes. Older lines do not.

## Hardening

Every crate carries a generated hardening-status table in its README, produced
from `docs/plans/use-protection-please.md`. The threat model is
[`docs/threat-model.md`](docs/threat-model.md).
