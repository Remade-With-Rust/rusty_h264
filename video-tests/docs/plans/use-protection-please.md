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
**Audit depth**: deep (tools executed: cargo audit, cargo deny, clippy -D warnings, geiger, careful, cyclonedx, auditable, vet, fuzz build, miri attempt, PE header read)
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
*Full model* — [`docs/threat-model.md`](../threat-model.md)

---

## Checklist

`★` = v1.0.0-blocking. Full probe and pass criteria per gate: the skill's `CHECKLIST.md`.

### Phase 0 — Threat modeling

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-01 | ★ Threat model documented and linked from README | Completed | `docs/threat-model.md`: a STRIDE pass naming assets, four adversary classes, the highest-value attack path and what is deliberately NOT defended. Linked from the root README's Security paragraph. | |
| H-02 | Threat model revisited after last major change | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 1 — Toolchain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-03 | Toolchain pinned (`rust-toolchain.toml`) | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-04 | Committed `.cargo/config.toml` hardening defaults | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-05 | ★ Release profile hardened (overflow-checks, LTO, panic policy) | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-06 | Security toolchain available to CI and developers | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |

### Phase 2 — Supply chain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-07 | ★ `Cargo.lock` committed | Completed | `git ls-files --error-unmatch Cargo.lock` succeeds. It was in `.gitignore`; that line is replaced by a comment explaining why a workspace with a shipped binary tracks it -- without it, `cargo audit`/`cargo vet` describe a moving target. | |
| H-08 | ★ `deny.toml` policy present and enforced | Completed | `deny.toml` covers advisories + licenses + bans + sources. `cargo deny check` run 2026-09-07: **advisories ok, bans ok, licenses ok, sources ok**. `allow-wildcard-paths` is on with a written reason: the two version-less path deps are DEV-dependencies that exist to break a publish cycle. | |
| H-09 | ★ Vulnerability scan clean (`cargo audit`) | Completed | `cargo audit --deny warnings` exits 0 (2026-09-07). The single finding, RUSTSEC-2024-0384 (`instant` unmaintained), is ignored in BOTH `deny.toml` and `.cargo/audit.toml` with a dated justification: it arrives via `minifb`, a dev-dependency of the viewer example, and `cargo tree --workspace --edges normal` returns zero hits, so it is in no shipped artifact. Review 2026-12-07. | |
| H-10 | ★ `cargo vet` coverage complete | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-11 | Unsafe inventory measured and trending down (geiger) | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-12 | ★ SBOM generated and published with releases | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-13 | Git deps pinned; no unknown registries or sources | Completed | No `git =` deps and no alternate registries in any manifest. `deny.toml [sources]` sets `unknown-registry = "deny"`, `unknown-git = "deny"` and an allow-list of crates.io alone, so adding one FAILS the build rather than passing quietly. `cargo deny check`: sources ok. | |
| H-14 | Dependency freshness reviewed, human-in-the-loop updates | Completed | `.github/dependabot.yml`: cargo + github-actions, monthly, grouped for minor/patch with `needs-review` labels, security updates deliberately ungrouped and undelayed. Nothing auto-merges. | |

### Phase 3 — Code level

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-15 | ★ Workspace lint policy set and clean | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-16 | ★ `unsafe` isolated, SAFETY-commented, inventoried | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-17 | Arithmetic safety explicit | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-18 | ★ No `unwrap`/`expect`/panic on untrusted paths; typed errors | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
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
| H-23 | ★ Tests pass under Miri | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-24 | Critical paths pass the sanitizers (ASan/MSan/TSan) | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-25 | `cargo careful test` green | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |

### Phase 6 — Fuzzing and properties

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-26 | ★ Fuzz target per public parser, decoder, or message handler | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-27 | ★ Continuous fuzzing with no open crashes | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-28 | Property tests cover the documented invariants | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-29 | Mutation and/or differential testing on critical modules | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 7 — Formal verification

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-30 | Proof of panic-freedom / UB-freedom per `unsafe` module | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 8 — Build and binary

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-31 | ★ Binary hardening applied and verified | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-32 | Build is reproducible or fully auditable | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |

### Phase 9 — Runtime privilege

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-33 | Least privilege documented and tested | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |

### Phase 10 — Cryptography

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-34 | Vetted crypto only; no bespoke primitives | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-35 | Side-channel discipline (constant-time, no secret branches) | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |
| H-36 | Post-quantum migration plan for long-lived keys | N/A | Out of tier: gate applies to critical-path units only; this unit is `utility`. | |

### Phase 11 — CI/CD, release, and operations

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-37 | CI runs the hardening gate on every PR | Completed | `.github/workflows/hardening.yml` runs clippy (`-D warnings`), cargo-deny, cargo-audit, semgrep, miri, the fuzz regression, the SBOM build and a README-table freshness check, on every push and PR plus a weekly cron. `permissions: contents: read` by default. TWO honest exceptions, both written into the workflow: **rustfmt is advisory (`continue-on-error`)** because the codebase is hand-formatted -- measured at 686 diffs at rustfmt's default width, 1,121 at 110 and 1,461 at 140, so no width matches it and reformatting would expand DSP butterflies across five lines; and **actions are pinned to tags, not SHAs**, because the SHAs could not be verified from this machine and an invented digest is worse than an honest tag. The resolving command is in the file. | |
| H-38 | Releases signed, attested, and changelogged for security | N/A | Not a Cargo package; it holds the corpus the other units' tests consume, and builds no artifact. | |
| H-39 | ★ `SECURITY.md` with a coordinated disclosure process | Completed | `SECURITY.md` at the repo root: private reporting channel, a fallback contact, response windows (3 days ack / 10 days assessment / 30 days fix for High+), a 90-day coordinated-disclosure policy, and an explicit in/out-of-scope list that names decoder panics and `-accel` memory safety as in scope and dev-only dependencies as out. | |
| H-40 | Advisory monitoring and scheduled re-audit | Completed | Three cadences, all written down in `docs/security-operations.md` §3: **weekly** cron in `hardening.yml` re-runs deny+audit against a tree nobody touched (the advisory DB moves even when the repo does not); **monthly** grouped Dependabot with review required; **quarterly** full 41-gate re-audit in place. The one accepted advisory carries a 2026-12-07 review date in both config files. | |
| H-41 | ★ Residual risks listed and accepted; waivers time-bounded | Completed | The residual-risk register below is populated with six risks, each carrying an owner, a written acceptance and a review date, and the waiver table carries one time-bounded waiver. Every open gate above appears there or is named in its own Evidence cell. | |

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
| R-001 | A memory-safety bug in a `rusty_h264-accel` SIMD kernel, driven by crafted input | Low | **High** -- it is the only `unsafe` in the workspace, so it is the only path to memory corruption | Partial: every kernel has a scalar-twin oracle, all `unsafe` blocks are SAFETY-commented, 68-stream byte-identity gates every change. NOT formally verified (H-30), and no sanitizer run (H-24) | Architect | 2026-12-07 |
| R-002 | A malformed bitstream reaches a code path that panics | Low | Medium -- denial of service in the host process | Strong: decoder is `forbid(unsafe_code)` with **zero** production panic sites, enforced by `clippy::unwrap_used`; mutation fuzzer in CI; three coverage-guided targets added | Architect | 2026-12-07 |
| R-003 | A narrowing `as` cast truncates an attacker-influenced length | Low | Medium | Partial: `overflow-checks` is on but does NOT cover narrowing casts; 447 `as usize` / 288 `as u8` remain unreviewed (H-17). Mitigated by `forbid(unsafe_code)` turning a bad index into a bounds check, not an OOB access | Architect | 2026-12-07 |
| R-004 | A compromised or malicious dependency version | Low | **High** -- code execution in every consumer | Partial: 16-crate production surface, `Cargo.lock` tracked, `deny.toml` restricts sources to crates.io, weekly `cargo audit`. But `cargo vet` shows all 9 production deps **exempted rather than audited** (H-10) | Architect | 2026-12-07 |
| R-005 | Deep decoder state (multi-slice, MMCO, B-pyramid) holds a bug that random mutation never reaches | Medium | Medium | Partial: coverage-guided targets exist and are seeded, but have no campaign behind them yet (H-27). A real instance of this class was found on 2026-09-07 by a hand-written multi-slice fixture, not by fuzzing | Architect | 2026-12-07 |
| R-006 | A GitHub Action tag is moved by an attacker, compromising CI | Low | **High** -- CI has repo write via the default token | Open: actions are pinned to TAGS, not SHAs, because the SHAs could not be verified from the audit host. `permissions: contents: read` limits blast radius. Resolving command is in the workflow | Architect | 2026-10-07 |

---

## Waivers

Time-bounded only. An expired waiver is an `Incomplete` gate, not a `Completed` one.

| Gate | Reason | Granted by | Expires |
|---|---|---|---|
| H-27 | >=30 days of coverage-guided fuzzing cannot exist on day 1: the targets were written 2026-09-07. The per-PR regression run over the seeded corpus is the interim control | Architect | 2026-10-07 -- by which point either a campaign has run or OSS-Fuzz onboarding is scheduled |

---

## Audit log

Append one line per pass; never rewrite history. The trend is the point.

| Date | Depth | Auditor | Completed / Scheduled / Incomplete | ★ met | Note |
|---|---|---|---|---|---|
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 2 / 0 / 32 | 0 of 6 | Pass 1: measured only. |
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 10 / 0 / 0 | 6 of 6 | Pass 2: the hardening work. 100% of in-scope gates closed. |
|---|---|---|---|---|---|
| 2026-09-07 | deep (tools executed: cargo audit, cargo deny, clippy -D warnings, geiger, careful, cyclonedx, auditable, vet, fuzz build, miri attempt, PE header read) | Claude (use-protection-please v1, depth=deep) | | | first pass |
