# Security operations — rs_h264

Companion to [`threat-model.md`](threat-model.md). That file says what could go
wrong; this one says what we do about it on an ongoing basis. It carries the
hardening gates that are *practices* rather than files: arithmetic discipline
(H-17), runtime privilege (H-33), and advisory monitoring (H-40).

**Last reviewed**: 2026-09-07. **Next review**: 2026-12-07.

---

## 1. Arithmetic discipline (H-17)

A codec is almost entirely integer arithmetic on values derived from
attacker-controlled input, so "what happens on overflow" is a security question
rather than a style one.

**The release profile now has `overflow-checks = true`** (H-05). That is the
headline control and it is not free: it means an unintended overflow *aborts*
rather than silently wrapping into a wrong offset. It was turned on and the full
suite plus the 68-stream byte-identity gate passed unchanged, which is the
evidence that nothing in the codec was relying on accidental wraparound.

Three kinds of arithmetic live here, and they are kept visibly distinct:

| kind | how it is written | why |
|---|---|---|
| **Deliberately modular** | `wrapping_add`, `wrapping_sub`, … (82 sites) | H.264 defines several quantities modulo a power of two — `frame_num`, POC LSBs, CABAC state. Wrapping is the SPEC, so it is spelled out. |
| **Deliberately clamped** | `saturating_*`, `.clamp()`, `.min()/.max()` (18 sites) | Pixel and coefficient ranges are bounded by the spec; saturation is the defined behaviour. |
| **Everything else** | plain `+ - *` | Must not overflow. `overflow-checks` is what turns that from an assumption into an assertion. |

Because the first two are explicit, anything that trips the checked arithmetic in
the third group is by definition unintended — which is what makes the check
meaningful rather than noisy.

### The open item: narrowing casts

`as usize` (447 sites) and `as u8` (288 sites) are **not** covered by
`overflow-checks` — a narrowing `as` cast truncates silently, by design, and no
compiler flag changes that. Most are provably safe (a 4×4 block index into a
fixed array), but they have not been reviewed one by one against
*untrusted-length* inputs. That review is the open half of H-17 and it is
recorded as such rather than waved through.

Mitigating factors, none of which is a substitute for the review: the decoder is
`#![forbid(unsafe_code)]`, so a truncated index produces a bounds-check panic
rather than an out-of-bounds access; and since 2026-09-07 it carries no
production panic path, so such a case returns `Err(DecodeError)`.

---

## 2. Runtime privilege (H-33)

`rusty_h264` the CLI is a local, offline tool. It:

* reads one input file named on the command line,
* writes one output file named on the command line,
* opens **no sockets**, spawns no subprocesses, and reads no configuration,
* holds no credentials and needs no elevated privilege.

**It should be run as an ordinary unprivileged user, and never as root or an
Administrator.** There is nothing it does that requires more.

### Deploying it against untrusted input

If you are running this over files you did not produce — a transcoding service,
an upload pipeline — treat the decoder as the untrusted-input boundary it is and
give it as little as it needs:

* **Containerised**: non-root user, `--read-only` root filesystem, a single
  writable output mount, `--cap-drop=ALL`, `--security-opt=no-new-privileges`,
  and a memory limit. Decoded frames are large and the stream declares its own
  dimensions, so a memory cap is the practical backstop against a hostile
  resolution.
* **Systemd**: `DynamicUser=yes`, `ProtectSystem=strict`, `PrivateNetwork=yes`
  (nothing here needs a network), `MemoryMax=`, `NoNewPrivileges=yes`.
* **Library embedding**: the decoder returns `Result` on every path and does not
  panic, so a bad stream is an error value rather than a process-level event.
  That is only true of the *decoder*; the encoder is `standard` tier and its
  input is assumed to be your own frames.

No `seccomp` or `landlock` profile ships with the crate. Writing one is tracked
against H-33; the sandboxing above is applied by the deployment, not by us, which
is why this section is guidance rather than a shipped artifact.

---

## 3. Advisory monitoring and re-audit (H-40)

**Automated, weekly, on a schedule rather than on a push:**
`.github/workflows/hardening.yml` runs `cargo deny check` and
`cargo audit --deny warnings` every Monday at 06:27 UTC. That cadence exists
because the advisory database moves even when this repository does not — a tree
nobody has touched can become vulnerable overnight, and a push-triggered job
would never notice.

**Automated, monthly:** Dependabot opens grouped minor/patch PRs
(`.github/dependabot.yml`). Security updates are deliberately *not* grouped and
not delayed. Every update requires human review; nothing auto-merges.

**Manual, quarterly:** the full 41-gate audit re-runs in place against
`docs/plans/use-protection-please.md` in each unit — statuses updated, `Audited`
bumped, a dated line appended to the audit log. Re-audited **in place**, never
regenerated, because the value is in the trend and the trend lives in the file's
history.

**Accepted advisories are dated and expire.** The one currently accepted
(RUSTSEC-2024-0384, `instant` unmaintained, dev-only via the viewer's `minifb`)
carries a review date of 2026-12-07 in both `deny.toml` and `.cargo/audit.toml`.
An accepted risk with no review date is just an ignored one.

### Where to look when something fires

| signal | what it means | first move |
|---|---|---|
| `cargo audit` fails on the weekly run | a new advisory touches the lockfile | `cargo tree --edges normal` — if it has no hits, it is dev-only and not an exposure |
| `cargo deny` licenses fail | a dependency changed license | check it is still redistributable under BSD-2-Clause |
| clippy fails on `unwrap_used` | someone reintroduced a panic path in a crate that eats untrusted bytes | that is the H-18 regression the lint exists to catch |
| the fuzz job finds a crasher | a panic on attacker-controlled input | it is a **security** bug; the artifact in `fuzz/artifacts/` is the reproducer, and it becomes a regression test |
