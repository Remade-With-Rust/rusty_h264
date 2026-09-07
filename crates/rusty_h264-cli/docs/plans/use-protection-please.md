# rusty_h264-cli — hardening audit

**Standard**: rs_h264 recursive hardening process — see the skill's `STANDARD.md`
**Registry**: 41 gates / 12 phases (`use-protection-please` v1)
**Unit**: `crates/rusty_h264-cli` — binary (`rusty_h264`)
**Tier**: critical-path — the shipped binary; it takes a file path from argv and feeds arbitrary bytes to the decoder
**Mirrors**: the crates.io page for `rusty_h264-cli` (re-rendered at the next publish -- its link must be absolute) — every one of these carries the generated block and **must be
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
| H-02 | Threat model revisited after last major change | Incomplete | No model exists to revisit (H-01). | |

### Phase 1 — Toolchain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-03 | Toolchain pinned (`rust-toolchain.toml`) | Incomplete | No `rust-toolchain.toml` at the repo root; builds take the developer default (stable 1.98 here, with 1.73/1.85/nightly also installed -- so the build is not reproducible across machines). | |
| H-04 | Committed `.cargo/config.toml` hardening defaults | Incomplete | `.cargo/config.toml` IS present but contains only `target-cpu=x86-64-v3` for x86_64 -- no frame pointers, no linker hardening. Read in full, not merely detected. | |
| H-05 | ★ Release profile hardened (overflow-checks, LTO, panic policy) | Incomplete | `[profile.release]` sets `opt-level=3`, `lto=thin`, `codegen-units=1`. **`overflow-checks` is absent** and no `panic` policy is declared. | |
| H-06 | Security toolchain available to CI and developers | Incomplete | audit/deny/geiger/vet/outdated/cyclonedx/auditable/mutants and miri are installed LOCALLY, but no CI job installs or pins any of them. | |

### Phase 2 — Supply chain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-07 | ★ `Cargo.lock` committed | Incomplete | `git ls-files Cargo.lock` -> not tracked. The 105 resolved dependencies are therefore unpinned for consumers and for H-09/H-10. | |
| H-08 | ★ `deny.toml` policy present and enforced | Incomplete | No `deny.toml`. `cargo deny check` (defaults) run 2026-09-07: `advisories FAILED, bans ok, licenses FAILED, sources ok`. | |
| H-09 | ★ Vulnerability scan clean (`cargo audit`) | Incomplete | `cargo audit --deny warnings` run 2026-09-07: 1 finding, RUSTSEC-2024-0384 (`instant` 0.1.13 unmaintained). Traced to `minifb` 0.28 -> **dev-dependency** of rusty_h264-decoder (the side-by-side viewer); `cargo tree --edges normal` shows 0 hits, so it never ships. No dated ignore is recorded, so the gate is not met. | |
| H-10 | ★ `cargo vet` coverage complete | Incomplete | No `supply-chain/` directory; `cargo vet` has no certifications. | |
| H-11 | Unsafe inventory measured and trending down (geiger) | Incomplete | `cargo geiger` run 2026-09-07 on the only crate carrying unsafe, rusty_h264-accel: 160/160 unsafe fns, 5609/5611 unsafe expressions. Measured, but no archived baseline to trend against. | |
| H-12 | ★ SBOM generated and published with releases | Incomplete | `cargo-cyclonedx` and `cargo-auditable` are installed; no SBOM is attached to v0.16.0 or any prior release. | |
| H-13 | Git deps pinned; no unknown registries or sources | Incomplete | No `git =` deps and no alternate registries in any manifest (grep, 2026-09-07), so the first half holds vacuously. The second half needs `[sources]` in a `deny.toml`, which does not exist (H-08). | |
| H-14 | Dependency freshness reviewed, human-in-the-loop updates | Incomplete | No Renovate or Dependabot config; no triaged `cargo outdated` report. | |

### Phase 3 — Code level

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-15 | ★ Workspace lint policy set and clean | Incomplete | No `[workspace.lints]`. `cargo clippy --workspace --all-targets --features asm` run 2026-09-07 exits 0 with 3 distinct warnings (120 including duplicates), so `-D warnings` would fail and no pedantic/nursery policy is configured. | |
| H-16 | ★ `unsafe` isolated, SAFETY-commented, inventoried | Completed | The crate is `#![forbid(unsafe_code)]`, so the gate is met by construction: there is no `unsafe` block to comment or inventory. Verified in `src/lib.rs` and by a clean build. | |
| H-17 | Arithmetic safety explicit | Incomplete | Deliberate discipline is visible -- 82 `wrapping_*`, 18 `saturating_*`, and the codec's arithmetic is spec-defined modular -- but the 447 `as usize` / 288 `as u8` casts have not been reviewed against untrusted lengths. | |
| H-18 | ★ No `unwrap`/`expect`/panic on untrusted paths; typed errors | Incomplete | See per-unit evidence. | |
| H-19 | Input validation — external bytes treated as hostile | Completed | `Decoder::decode(&[u8]) -> Result<Option<YuvFrame>, DecodeError>` is the single untrusted entry point; the crate is `#![forbid(unsafe_code)]` so every index is bounds-checked by the compiler, and `tests/fuzz_no_panic.rs` asserts that corrupted and pure-random buffers yield only `Ok`/`Err` under `catch_unwind`. No serde, no untrusted deserialization. | |
| H-20 | ★ Secrets zeroized; never logged | N/A | The unit handles no secrets: zero crypto dependencies, zero key material, zero network sockets. A video codec's inputs and outputs are pixels and bitstreams. | |
| H-21 | Concurrency discipline | Completed | 0 `static mut`, 0 `unsafe impl Send`, 0 `unsafe impl Sync` across the workspace (grep 2026-09-07). Shared state is `Arc<RwLock<..>>` and channels, and as of 8335d92 every lock is taken poison-tolerantly rather than with `.unwrap()`. | |

### Phase 4 — Static analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-22 | Static analysis beyond the default linter runs on every PR | Incomplete | No Semgrep / MIRAI / Rudra / module-graph job in `.github/workflows/`; grep for each returns 0. | |

### Phase 5 — Dynamic analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-23 | ★ Tests pass under Miri | Incomplete | `cargo +nightly miri test -p rusty_h264-common --lib` with `-Zmiri-strict-provenance` run 2026-09-07: the interpreter ABORTED on the host (STATUS_STACK_BUFFER_OVERRUN, 0xc0000409) before reporting a test count. That is a host/interpreter failure, not a finding in this code -- but a run that reports no test count cannot pass. Needs a Linux runner. | |
| H-24 | Critical paths pass the sanitizers (ASan/MSan/TSan) | Incomplete | No ASan/MSan/TSan run; `-Zsanitizer` is not wired into CI or any local script. | |
| H-25 | `cargo careful test` green | Incomplete | `cargo careful` is not installed -- the only tool in the registry absent from this machine. | |

### Phase 6 — Fuzzing and properties

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-26 | ★ Fuzz target per public parser, decoder, or message handler | Incomplete | No `fuzz/fuzz_targets/` and no seeded corpus, so the gate as written is not met. **The mitigation is unusually strong for an Incomplete row**: `tests/fuzz_no_panic.rs` is a dependency-free deterministic mutation fuzzer over the one public entry point (`Decoder::decode`) -- valid encoder-produced streams corrupted thousands of ways, plus pure-random buffers, all under `catch_unwind`, asserting only `Ok(_)`/`Err(DecodeError)` and printing seed+mutation on failure -- and CI runs it as a dedicated job. What is missing is coverage guidance and a corpus, which `cargo-fuzz` would add over the same entry point. | |
| H-27 | ★ Continuous fuzzing with no open crashes | Incomplete | No OSS-Fuzz integration and no >=30-day coverage-guided campaign. The CI fuzz job is a fixed-seed regression run, not continuous fuzzing. | |
| H-28 | Property tests cover the documented invariants | Incomplete | No `proptest`/`quickcheck` in the tree. Invariants are pinned instead by 31 test and gate files, several stronger than a property (see H-29), but the documented invariants have no property harness. | |
| H-29 | Mutation and/or differential testing on critical modules | Completed | A differential harness against the reference decoder runs as a standing gate: `bench/ident_gate.sh` decodes 68 x264-encoded streams and requires every one byte-identical to ffmpeg (68/68 on 2026-09-07), and `bench/verify_hint_sweep.sh` sweeps 258 streams for derivation disagreements (0 found). This is the re-implementation case the gate names. | |

### Phase 7 — Formal verification

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-30 | Proof of panic-freedom / UB-freedom per `unsafe` module | Incomplete | No `#[kani::proof]` harness anywhere; no Creusot/Verus/Prusti. | |

### Phase 8 — Build and binary

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-31 | ★ Binary hardening applied and verified | Incomplete | PE header of `target/release/rusty_h264.exe` read 2026-09-07: `DllCharacteristics = 0x8160` -- HIGH_ENTROPY_VA **set**, DYNAMICBASE (ASLR) **set**, NX_COMPAT (DEP) **set**, **GUARD_CF (Control Flow Guard) NOT set**. Three of four; CFG needs `-C control-flow-guard`. | |
| H-32 | Build is reproducible or fully auditable | Incomplete | `cargo-auditable` is installed but release builds do not use it, so shipped binaries embed no dependency list. No documented reproducible-build procedure and no `SOURCE_DATE_EPOCH` handling. | |

### Phase 9 — Runtime privilege

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-33 | Least privilege documented and tested | Incomplete | The CLI reads and writes files as the invoking user with no privilege drop, no seccomp/landlock, and no documented least-privilege deployment. Arguably proportionate for a local codec CLI, but neither documented nor tested. | |

### Phase 10 — Cryptography

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-34 | Vetted crypto only; no bespoke primitives | N/A | No cryptography: the unit implements no primitive and depends on no crypto library. | |
| H-35 | Side-channel discipline (constant-time, no secret branches) | N/A | No secret-dependent code path exists -- there are no secrets (H-20). | |
| H-36 | Post-quantum migration plan for long-lived keys | N/A | No long-lived keys, so there is no post-quantum horizon to plan for. | |

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
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 4 / 0 / 33 | 1 of 16 | First pass. Zero `Scheduled` by design: scheduling needs an owner and a date, which are the human's to assign, not the auditor's. |
|---|---|---|---|---|---|
| 2026-09-07 | deep (tool probes executed: cargo audit, cargo deny, clippy, geiger, miri, PE header read) | Claude (use-protection-please v1, depth=deep) | | | first pass |
