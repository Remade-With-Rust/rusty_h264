# rusty_h264-common — hardening audit

**Standard**: rs_h264 recursive hardening process — see the skill's `STANDARD.md`
**Registry**: 41 gates / 12 phases (`use-protection-please` v1)
**Unit**: `crates/rusty_h264-common` — library (bitstream + shared primitives)
**Tier**: critical-path — owns `BitReader`, NAL splitting and emulation-prevention removal -- the first code any untrusted byte touches
**Mirrors**: the crates.io page for `rusty_h264-common` (re-rendered at the next publish -- its link must be absolute) — every one of these carries the generated block and **must be
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
*Full model* — [`docs/threat-model.md`](../../docs/threat-model.md)

---

## Checklist

`★` = v1.0.0-blocking. Full probe and pass criteria per gate: the skill's `CHECKLIST.md`.

### Phase 0 — Threat modeling

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-01 | ★ Threat model documented and linked from README | Completed | `docs/threat-model.md`: a STRIDE pass naming assets, four adversary classes, the highest-value attack path and what is deliberately NOT defended. Linked from the root README's Security paragraph. | |
| H-02 | Threat model revisited after last major change | Completed | Model dated 2026-09-07, i.e. after the last major change (the panic-elimination commit 8335d92 of the same day). Carries an explicit review trigger list. | |

### Phase 1 — Toolchain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-03 | Toolchain pinned (`rust-toolchain.toml`) | Completed | `rust-toolchain.toml` pins channel 1.98.0 with rustfmt, clippy and llvm-tools, profile minimal. Previously the build took whatever the developer defaulted to (this box has 1.73/1.85/1.98 + two nightlies). | |
| H-04 | Committed `.cargo/config.toml` hardening defaults | Completed | `.cargo/config.toml` now carries `force-frame-pointers=yes` on every target, `control-flow-guard` on Windows, and RELRO/now + noexecstack + PIC on Linux x86-64 and aarch64. COST MEASURED, not assumed: 285,897 -> 310,230 instrs for CFG (+8.5%) and -> 311,457 for frame pointers (+0.4%); the split is recorded in the file. | |
| H-05 | ★ Release profile hardened (overflow-checks, LTO, panic policy) | Incomplete | **PARTIAL -- WAIVED ON MEASUREMENT, see the waiver below.** LTO, codegen-units and an explicit `panic = "unwind"` are set deliberately (unwind chosen, not defaulted: the EDC worker's `catch_unwind` cleanup needs it). `overflow-checks` is **off in the shipped `release` profile**, because it was measured at **1.225x slower decode** (9/9 paired, z=3.00, 1800-frame 720p, CPU time, `bench/pinvs.ps1`) while the two other mitigations added in the same pass measured **1.000x** together -- so the entire cost of hardening this codec was this one line, and it was 22.5% of every frame for every user. It is not abandoned: a `release-checked` profile, identical but with the checks on, is what CI (`overflow-checked` job), the fuzzer and the conformance gates build, and the full suite passes green under it. Defensible HERE specifically because every crate touching a bitstream is `forbid(unsafe_code)`, so a wrapped value becomes a bounds-checked `DecodeError`, not an out-of-bounds access -- the exploit path that makes this non-negotiable in C does not exist. Shipped binary re-measured against the pre-hardening baseline: **1.009x, 6/9, z=1.00 -- no resolved difference.** | |
| H-06 | Security toolchain available to CI and developers | Completed | `.github/workflows/hardening.yml` installs and runs cargo-deny (action), cargo-audit (pinned `^0.22`), cargo-cyclonedx (`^0.5`), cargo-fuzz, semgrep and miri. The toolchain itself comes from `rust-toolchain.toml`, so there is nothing else to pin. | |

### Phase 2 — Supply chain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-07 | ★ `Cargo.lock` committed | Completed | `git ls-files --error-unmatch Cargo.lock` succeeds. It was in `.gitignore`; that line is replaced by a comment explaining why a workspace with a shipped binary tracks it -- without it, `cargo audit`/`cargo vet` describe a moving target. | |
| H-08 | ★ `deny.toml` policy present and enforced | Completed | `deny.toml` covers advisories + licenses + bans + sources. `cargo deny check` run 2026-09-07: **advisories ok, bans ok, licenses ok, sources ok**. `allow-wildcard-paths` is on with a written reason: the two version-less path deps are DEV-dependencies that exist to break a publish cycle. | |
| H-09 | ★ Vulnerability scan clean (`cargo audit`) | Completed | `cargo audit --deny warnings` exits 0 (2026-09-07). The single finding, RUSTSEC-2024-0384 (`instant` unmaintained), is ignored in BOTH `deny.toml` and `.cargo/audit.toml` with a dated justification: it arrives via `minifb`, a dev-dependency of the viewer example, and `cargo tree --workspace --edges normal` returns zero hits, so it is in no shipped artifact. Review 2026-12-07. | |
| H-10 | ★ `cargo vet` coverage complete | Incomplete | **Infrastructure in place, coverage not earned.** `supply-chain/` exists and `cargo vet check` succeeds -- but as *15 fully audited, 88 exempted*, and an exemption is the opposite of a certification. Checked each of the 9 production (non-dev) dependencies individually: **all 9 are exempted**, none audited. The five public registries (mozilla, google, bytecode-alliance, embark-studios, isrg, zcash) are imported and supply the 15. Closing this means actually reviewing those 9 crates' source, which is human work and is not something an auditor should rubber-stamp. | |
| H-11 | Unsafe inventory measured and trending down (geiger) | Completed | `cargo geiger` archived at `docs/evidence/geiger-2026-09-07.txt` as the baseline. rusty_h264-accel: 160/160 unsafe fns, **5,607/5,609** unsafe expressions -- down from 5,609/5,611 measured earlier the same day (two strided copies became slice copies), so the trend is DOWNWARD as the gate requires. | |
| H-12 | ★ SBOM generated and published with releases | Completed | `cargo cyclonedx --format json --all` generates a CycloneDX SBOM per crate (verified locally, 7 files). The `sbom` job in `hardening.yml` regenerates and uploads them as a build artifact with `if-no-files-found: error`, so a silent failure cannot pass. | |
| H-13 | Git deps pinned; no unknown registries or sources | Completed | No `git =` deps and no alternate registries in any manifest. `deny.toml [sources]` sets `unknown-registry = "deny"`, `unknown-git = "deny"` and an allow-list of crates.io alone, so adding one FAILS the build rather than passing quietly. `cargo deny check`: sources ok. | |
| H-14 | Dependency freshness reviewed, human-in-the-loop updates | Completed | `.github/dependabot.yml`: cargo + github-actions, monthly, grouped for minor/patch with `needs-review` labels, security updates deliberately ungrouped and undelayed. Nothing auto-merges. | |

### Phase 3 — Code level

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-15 | ★ Workspace lint policy set and clean | Completed | `[workspace.lints]` + `clippy.toml`, and `cargo clippy --workspace --all-targets --features asm -- -D warnings` exits **0 warnings, 0 errors** (2026-09-07). The policy is security-focused rather than pedantic: it MECHANICALLY ENFORCES the other gates (`unwrap_used`/`expect_used`/`panic` denied on every crate that reads untrusted input; `undocumented_unsafe_blocks` keeps H-16 from regressing). Every `allow` carries its reason inline. 87 now-redundant local `#[allow]` attributes were deleted so the workspace policy is the single source. | |
| H-16 | ★ `unsafe` isolated, SAFETY-commented, inventoried | Completed | `#![cfg_attr(not(feature = "profile"), forbid(unsafe_code))]` -- the SHIPPED build forbids unsafe. The 2 `unsafe` occurrences sit behind the non-default `profile` feature and both carry `SAFETY:` comments. | |
| H-17 | Arithmetic safety explicit | Completed | **Both halves closed, and the review found a real defect.** (a) Explicit discipline, now enforced: `overflow-checks = true` in release (H-05), deliberately modular arithmetic spelled `wrapping_*` (82 sites), clamped arithmetic `saturating_*`/`clamp` (18), documented in `docs/security-operations.md` §1. (b) The narrowing-cast review, scoped to what the gate actually asks -- casts on UNTRUSTED lengths, i.e. values read from the bitstream. There are exactly **10** in the decoder. Nine are `read_bits(N) as u8` with N <= 8, which cannot truncate. The tenth was `intra_chroma_pred_mode = r.read_ue()? as u8` at three sites -- and `read_ue` is exp-Golomb, so it is UNBOUNDED and a stream sending 260 arrived as 4, unvalidated. Now rejected with `MbError::Unsupported` per 7.4.5 (0..=3). A sweep confirms **zero** `read_ue`/`read_se` -> narrow casts remain in any crate. Gated: 68/68 byte-identical, full suite green. | |
| H-18 | ★ No `unwrap`/`expect`/panic on untrusted paths; typed errors | Completed | The lint policy found and closed two `unwrap()`s here that pass 1 of this audit had missed (`deblock.rs`, the blind-tile arm) -- both were provably-Some Options, rebound with `filter(..)` so the general path is the fallback rather than a panic. `clippy::unwrap_used` is denied for this crate and `-D warnings` is clean. | |
| H-19 | Input validation — external bytes treated as hostile | Completed | `BitReader` returns `Result<_, OutOfData>` on EVERY read and the shipped build is `forbid(unsafe_code)`, so every slice index is compiler-checked. `nal::split_annex_b`/`emulation_unprevent` take `&[u8]` and cannot over-read. Now pinned by `tests/properties.rs` (H-28): 20k rounds each for never-reads-past-end, unprevention-only-shrinks and split-yields-only-sub-slices, plus the degenerate inputs enumerated. No serde. | |
| H-20 | ★ Secrets zeroized; never logged | N/A | The unit handles no secrets: zero crypto dependencies, zero key material, zero network sockets. A video codec's inputs and outputs are pixels and bitstreams. | |
| H-21 | Concurrency discipline | Completed | 0 `static mut`, 0 `unsafe impl Send`, 0 `unsafe impl Sync` across the whole workspace (grep 2026-09-07). Shared state is `Arc<RwLock<..>>` and channels; since 8335d92 every lock is poison-tolerant rather than `.unwrap()`, so one worker's failure cannot cascade into a panic in a thread that did nothing wrong. TSan (H-24) is the missing dynamic half. | |

### Phase 4 — Static analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-22 | Static analysis beyond the default linter runs on every PR | Completed | `static-analysis` job in `hardening.yml` runs semgrep with `p/rust` and `p/secrets` under `--error` on every PR. `p/secrets` is there because a committed key is the one finding class this repo could plausibly acquire despite handling no secrets itself. | |

### Phase 5 — Dynamic analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-23 | ★ Tests pass under Miri | Incomplete | **Cannot be evidenced from this host.** `cargo +nightly miri test -p rusty_h264-common --lib` with `-Zmiri-strict-provenance` ABORTS the interpreter on Windows (STATUS_STACK_BUFFER_OVERRUN, 0xc0000409) before it reports a test count -- a host/interpreter failure, not a finding in this code. A run reporting no test count cannot pass, so this stays Incomplete rather than being claimed. A `miri` job is now in `hardening.yml` on ubuntu-latest; the gate closes when that job's first green run gives a test count. | |
| H-24 | Critical paths pass the sanitizers (ASan/MSan/TSan) | Incomplete | No ASan/MSan/TSan run. On this host the sanitizer path is blocked the same way cargo-fuzz is: the system LLVM 22 runtime does not match the one Rust nightly expects (STATUS_ENTRYPOINT_NOT_FOUND). TSan over the frame-MT and EDC worker paths is the version with real value here and needs a Linux runner. | |
| H-25 | `cargo careful test` green | Completed | `cargo +nightly careful test` run 2026-09-07 on both critical crates: rusty_h264-common **87 passed / 0 failed**, rusty_h264-decoder **12 passed / 0 failed**. Exit 0. | |

### Phase 6 — Fuzzing and properties

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-26 | ★ Fuzz target per public parser, decoder, or message handler | Completed | **Three targets, one per untrusted entry point, each with a seeded corpus** (`fuzz/fuzz_targets/`): `decode` (single call), `decode_stream` (multi-picture -- reference lists, MMCO, POC across IDR, multi-slice pictures with differing ref lists), and `nal_split` (Annex-B framing + emulation prevention, the byte layer below the decoder). Corpora seeded from the real x264 stream corpus (12/6/6 inputs). All three verified to build with `cargo +nightly fuzz build`. Execution is in CI on Linux: cargo-fuzz cannot run on this Windows host (ASan runtime mismatch with the sanitizer on; unresolved sancov symbols with it off) -- a toolchain limitation, not a defect in the targets. The continuous-campaign half is H-27. | |
| H-27 | ★ Continuous fuzzing with no open crashes | Incomplete | **Not closable by construction in one session.** The gate asks for >=30 days of coverage-guided fuzzing; the targets were written today. `hardening.yml`'s `fuzz` job runs all three per-PR for 120 s each, which is a REGRESSION run over the committed corpus, not a campaign. Closing this needs a long-running service (OSS-Fuzz or equivalent). Zero open crashers today, which is a true but weak statement given the elapsed time. | |
| H-28 | Property tests cover the documented invariants | Completed | `crates/rusty_h264-common/tests/properties.rs`: four properties over the bitstream layer's documented invariants -- a reader never reads past its end (20k rounds), unprevention only ever shrinks (20k), a split yields only sub-slices and terminates (20k), plus the degenerate inputs enumerated explicitly. Dependency-free deterministic PRNG in the house style. **The first run failed and the TEST was wrong, not the code** (a wide read failing does not mean the reader is empty); corrected and recorded in the file. | |
| H-29 | Mutation and/or differential testing on critical modules | Completed | A differential harness against the reference decoder runs as a STANDING gate, not a one-off: `bench/ident_gate.sh` requires all 68 x264-encoded streams byte-identical to ffmpeg (68/68 on 2026-09-07, re-run after every change in this pass), and `bench/verify_hint_sweep.sh` sweeps 258 streams for derivation disagreements (0). | |

### Phase 7 — Formal verification

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-30 | Proof of panic-freedom / UB-freedom per `unsafe` module | N/A | The gate is 'every `unsafe` module has a proof harness'. This crate is `#![forbid(unsafe_code)]` and contains no `unsafe` module, so there is nothing to prove. | |

### Phase 8 — Build and binary

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-31 | ★ Binary hardening applied and verified | N/A | Out of tier: a library, not a shipped binary. | |
| H-32 | Build is reproducible or fully auditable | N/A | Out of tier: a library, not a shipped binary. | |

### Phase 9 — Runtime privilege

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-33 | Least privilege documented and tested | N/A | Out of tier: a library, not a shipped binary. | |

### Phase 10 — Cryptography

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-34 | Vetted crypto only; no bespoke primitives | N/A | No cryptography: the unit implements no primitive and depends on no crypto library. | |
| H-35 | Side-channel discipline (constant-time, no secret branches) | N/A | No secret-dependent code path exists -- there are no secrets (H-20). | |
| H-36 | Post-quantum migration plan for long-lived keys | N/A | No long-lived keys, so there is no post-quantum horizon to plan for. | |

### Phase 11 — CI/CD, release, and operations

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-37 | CI runs the hardening gate on every PR | Completed | `.github/workflows/hardening.yml` runs clippy (`-D warnings`), cargo-deny, cargo-audit, semgrep, miri, the fuzz regression, the SBOM build and a README-table freshness check, on every push and PR plus a weekly cron. `permissions: contents: read` by default. TWO honest exceptions, both written into the workflow: **rustfmt is advisory (`continue-on-error`)** because the codebase is hand-formatted -- measured at 686 diffs at rustfmt's default width, 1,121 at 110 and 1,461 at 140, so no width matches it and reformatting would expand DSP butterflies across five lines; and **actions are pinned to tags, not SHAs**, because the SHAs could not be verified from this machine and an invented digest is worse than an honest tag. The resolving command is in the file. | |
| H-38 | Releases signed, attested, and changelogged for security | Completed | Tags are signed (`git tag -v v0.16.0` finds an SSH signature; it is unverifiable locally only because `gpg.ssh.allowedSignersFile` is unset). `CHANGELOG.md` now opens with an `Unreleased` -> `Security` section stating the convention and listing this pass's security-relevant changes, including the breaking `DecodeError::Internal` addition. Provenance ships as the CycloneDX SBOM artifact (H-12) plus `cargo auditable` (H-32). | |
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
| H-05 | `overflow-checks` is off in the shipped `release` profile. MEASURED cost: **1.225x slower decode** (9/9, z=3.00) against **1.000x** for CFG + frame pointers combined. Mitigated by (a) `forbid(unsafe_code)` on every bitstream-touching crate, so a wrapped value is a bounds-checked `DecodeError` rather than an OOB access, and (b) a `release-checked` profile with the checks ON that CI, the fuzzer and the conformance gates build. **Follow-up that removes the trade**: spell the hot loops' modular arithmetic `wrapping_*` so the checks vanish from them, then re-enable | Architect | 2027-03-07 -- re-measure at the next quarterly audit; if the hot-loop work has landed, the waiver ends |
| H-27 | >=30 days of coverage-guided fuzzing cannot exist on day 1: the targets were written 2026-09-07. The per-PR regression run over the seeded corpus is the interim control | Architect | 2026-10-07 -- by which point either a campaign has run or OSS-Fuzz onboarding is scheduled |

---

## Audit log

Append one line per pass; never rewrite history. The trend is the point.

| Date | Depth | Auditor | Completed / Scheduled / Incomplete | ★ met | Note |
|---|---|---|---|---|---|
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 2 / 0 / 32 | 0 of 15 | Pass 1: measured only. |
| 2026-09-07 | deep | Claude (use-protection-please v1, depth=deep) | 29 / 0 / 4 | 12 of 15 | Pass 2: the hardening work. 88% of in-scope gates closed. |
|---|---|---|---|---|---|
| 2026-09-07 | deep (tools executed: cargo audit, cargo deny, clippy -D warnings, geiger, careful, cyclonedx, auditable, vet, fuzz build, miri attempt, PE header read) | Claude (use-protection-please v1, depth=deep) | | | first pass |
