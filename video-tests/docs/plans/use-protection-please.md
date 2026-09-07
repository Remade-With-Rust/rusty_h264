# video-tests — hardening audit

**Standard**: rs_h264 recursive hardening process — see the skill's `STANDARD.md`
**Registry**: 41 gates / 12 phases (`use-protection-please` v1)
**Unit**: `video-tests` — test assets and analysis scripts (not shipped)
**Tier**: utility — corpus clips and analysis helpers; no code ships to users and it consumes no untrusted input
**Mirrors**: none discovered: no standalone landing repo, and `cargo search` shows only the workspace crates — every one of these carries the generated block and **must be
re-rendered in the same pass as this file**; a stale mirror reports a posture the unit no
longer has (SKILL.md §3.1)
**Compliance**: none -- an offline video codec: no personal data, no network egress, no persistence of user data — in-scope framework ids, or `none` with the reason
(scope triage: the skill's `COMPLIANCE.md` §1)
**Architect**: [Tim Almond](https://github.com/Remade-With-Rust) — accountable for this unit's security design; rendered
at the foot of the block in every README and mirror
**Audit depth**: deep (tool probes executed: cargo audit, cargo deny, clippy, geiger, miri, PE header read)
**Audited**: 2026-09-07 by Claude (use-protection-please v1, depth=deep) · **Next review**: 2026-12-07

> Source of truth for this unit's hardening status. The README's status table is
> **generated from this file** — edit here, then run:
> `python <skills>/use-protection-please/scripts/render_readme_table.py --plan docs/plans/use-protection-please.md --readme README.md`

**Status tokens**: `Completed` (evidenced pass) · `Scheduled` (owner + date in Target) ·
`Incomplete` (not done, or not evidenced) · `N/A` (out of tier — reason required in
Evidence; excluded from the totals).

---

## Threat sketch

*Assets* — the decoding process itself (its availability and memory safety), and the integrity of the pixels it returns
*Adversaries* — anyone who can hand the decoder a byte string -- a media file, a network stream, a web upload
*Highest-value attack path* — a malformed H.264 bitstream reaching `Decoder::decode` and causing a panic (denial of service), an unbounded allocation, or -- in `rusty_h264-accel`, the only crate carrying `unsafe` -- an out-of-bounds kernel access
*Full model* — **not yet written (H-01)** -- this sketch is the placeholder that gate will replace

---

## Checklist

`★` = v1.0.0-blocking. Full probe and pass criteria per gate: the skill's `CHECKLIST.md`.

### Phase 0 — Threat modeling

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-01 | ★ Threat model documented and linked from README | Incomplete | No `docs/threat-model.md` and no `SECURITY.md` threat section; README carries no link. Probed 2026-09-07. | |
| H-02 | Threat model revisited after last major change | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 1 — Toolchain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-03 | Toolchain pinned (`rust-toolchain.toml`) | N/A | Not a Cargo package; it builds no Rust artifact. | |
| H-04 | Committed `.cargo/config.toml` hardening defaults | N/A | Not a Cargo package. | |
| H-05 | ★ Release profile hardened (overflow-checks, LTO, panic policy) | N/A | Not a Cargo package; it builds no artifact. | |
| H-06 | Security toolchain available to CI and developers | Incomplete | audit/deny/geiger/vet/outdated/cyclonedx/auditable/mutants and miri are installed LOCALLY, but no CI job installs or pins any of them. | |

### Phase 2 — Supply chain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-07 | ★ `Cargo.lock` committed | Incomplete | `git ls-files Cargo.lock` -> not tracked. The 105 resolved dependencies are therefore unpinned for consumers and for H-09/H-10. | |
| H-08 | ★ `deny.toml` policy present and enforced | Incomplete | No `deny.toml`. `cargo deny check` (defaults) run 2026-09-07: `advisories FAILED, bans ok, licenses FAILED, sources ok`. | |
| H-09 | ★ Vulnerability scan clean (`cargo audit`) | Incomplete | `cargo audit --deny warnings` run 2026-09-07: 1 finding, RUSTSEC-2024-0384 (`instant` 0.1.13 unmaintained). Traced to `minifb` 0.28 -> **dev-dependency** of rusty_h264-decoder (the side-by-side viewer); `cargo tree --edges normal` shows 0 hits, so it never ships. No dated ignore is recorded, so the gate is not met. | |
| H-10 | ★ `cargo vet` coverage complete | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-11 | Unsafe inventory measured and trending down (geiger) | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-12 | ★ SBOM generated and published with releases | Incomplete | `cargo-cyclonedx` and `cargo-auditable` are installed; no SBOM is attached to v0.16.0 or any prior release. | |
| H-13 | Git deps pinned; no unknown registries or sources | Incomplete | No `git =` deps and no alternate registries in any manifest (grep, 2026-09-07), so the first half holds vacuously. The second half needs `[sources]` in a `deny.toml`, which does not exist (H-08). | |
| H-14 | Dependency freshness reviewed, human-in-the-loop updates | Incomplete | No Renovate or Dependabot config; no triaged `cargo outdated` report. | |

### Phase 3 — Code level

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-15 | ★ Workspace lint policy set and clean | N/A | Not a Cargo package; no Rust crate to lint. | |
| H-16 | ★ `unsafe` isolated, SAFETY-commented, inventoried | N/A | Not a Cargo package; contains no Rust source. | |
| H-17 | Arithmetic safety explicit | N/A | Not a Cargo package; contains no Rust source. | |
| H-18 | ★ No `unwrap`/`expect`/panic on untrusted paths; typed errors | N/A | Not a Cargo package; contains no Rust source. | |
| H-19 | Input validation — external bytes treated as hostile | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-20 | ★ Secrets zeroized; never logged | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-21 | Concurrency discipline | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 4 — Static analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-22 | Static analysis beyond the default linter runs on every PR | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 5 — Dynamic analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-23 | ★ Tests pass under Miri | N/A | Not a Cargo package; no tests to run under Miri. | |
| H-24 | Critical paths pass the sanitizers (ASan/MSan/TSan) | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-25 | `cargo careful test` green | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 6 — Fuzzing and properties

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-26 | ★ Fuzz target per public parser, decoder, or message handler | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-27 | ★ Continuous fuzzing with no open crashes | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-28 | Property tests cover the documented invariants | N/A | Not a Cargo package; it holds the corpus the other units' tests consume. | |
| H-29 | Mutation and/or differential testing on critical modules | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 7 — Formal verification

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-30 | Proof of panic-freedom / UB-freedom per `unsafe` module | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 8 — Build and binary

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-31 | ★ Binary hardening applied and verified | N/A | Out of tier: gate applies to units producing a shipped binary; this one does not. | |
| H-32 | Build is reproducible or fully auditable | N/A | Out of tier: gate applies to units producing a shipped binary; this one does not. | |

### Phase 9 — Runtime privilege

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-33 | Least privilege documented and tested | N/A | Out of tier: gate applies to units producing a shipped binary; this one does not. | |

### Phase 10 — Cryptography

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-34 | Vetted crypto only; no bespoke primitives | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-35 | Side-channel discipline (constant-time, no secret branches) | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-36 | Post-quantum migration plan for long-lived keys | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 11 — CI/CD, release, and operations

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-37 | CI runs the hardening gate on every PR | Incomplete | `.github/workflows/ci.yml` is substantial -- tests on 3 OSes, an ARM64 runner, a no_std job, a feature-matrix job and a dedicated fuzz job -- but grep shows **zero** occurrences of clippy, fmt, deny, vet, miri, geiger or semgrep. The correctness gate is strong; the hardening gate is absent. | |
| H-38 | Releases signed, attested, and changelogged for security | Incomplete | Tags ARE signed: `git tag -v v0.16.0` finds an SSH signature (unverifiable locally only because `gpg.ssh.allowedSignersFile` is unset). `CHANGELOG.md` is 998 lines but carries no security section, and no provenance or auditable binary is attached to a release. | |
| H-39 | ★ `SECURITY.md` with a coordinated disclosure process | Incomplete | No `SECURITY.md` at the repo root. No disclosure contact, response window, or policy is published anywhere. | |
| H-40 | Advisory monitoring and scheduled re-audit | Incomplete | No documented advisory-feed subscription and no scheduled re-audit; this file's `Next review` is the first such commitment. | |
| H-41 | ★ Residual risks listed and accepted; waivers time-bounded | Incomplete | The register below is populated for the first time in this pass, but every row still needs a named owner, an acceptance and a review date -- which are the human's to assign, not the auditor's. | |

### Phase 12 — Compliance controls

Only in play when a framework is declared in scope above. With none in scope, every row is
`N/A` — reason: "no compliance framework in scope". Mapping: the skill's `COMPLIANCE.md`.

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| C-01 | Data inventory — personal/health/card data touched | N/A | No compliance framework in scope for this unit. | |
| C-02 | Data-flow map including third-party egress | N/A | No compliance framework in scope for this unit. | |
| C-03 | Encryption in transit for all egress | N/A | No compliance framework in scope for this unit. | |
| C-04 | Encryption at rest for stored sensitive data | N/A | No compliance framework in scope for this unit. | |
| C-05 | Key management — generation, storage, rotation, destruction | N/A | No compliance framework in scope for this unit. | |
| C-06 | Retention limits and honoured deletion | N/A | No compliance framework in scope for this unit. | |
| C-07 | Audit logging of security-relevant events | N/A | No compliance framework in scope for this unit. | |
| C-08 | Log hygiene — no PII, secrets, or card data in logs | N/A | No compliance framework in scope for this unit. | |
| C-09 | Least-privilege access to sensitive data | N/A | No compliance framework in scope for this unit. | |
| C-10 | Subprocessor and third-party inventory | N/A | No compliance framework in scope for this unit. | |
| C-11 | Incident response and breach notification path | N/A | No compliance framework in scope for this unit. | |
| C-12 | Change management — reviewed, approved, traceable | N/A | No compliance framework in scope for this unit. | |
| C-13 | Availability commitments and their evidence | N/A | No compliance framework in scope for this unit. | |
| C-14 | Machine-readable SBOM + provenance for regulators | N/A | No compliance framework in scope for this unit. | |

---

## Scheduled work

In execution order. Cheapest-first is usually correct: configuration gates clear in
minutes and unblock the outcome gates behind them.

| # | Gates | Work | Owner | Target | Notes |
|---|---|---|---|---|---|
| 1 | | | | | |

---

## Residual risk register

Every open risk carries an owner, an acceptance, and a review date (H-41).

| ID | Risk | Likelihood | Impact | Mitigation status | Accepted by | Review date |
|---|---|---|---|---|---|---|
| R-001 | | | | | | |

---

## Waivers

Time-bounded only. An expired waiver is an `Incomplete` gate, not a `Completed` one.

| Gate | Reason | Granted by | Expires |
|---|---|---|---|
| | | | |

---

## Audit log

Append one line per pass; never rewrite history. The trend is the point.

| Date | Depth | Auditor | Completed / Scheduled / Incomplete | ★ met | Note |
|---|---|---|---|---|---|
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 0 / 0 / 13 | 0 of 7 | First pass. Zero `Scheduled` by design: scheduling needs an owner and a date, which are the human's to assign, not the auditor's. |
|---|---|---|---|---|---|
| 2026-09-07 | deep (tool probes executed: cargo audit, cargo deny, clippy, geiger, miri, PE header read) | Claude (use-protection-please v1, depth=deep) | | | first pass |
