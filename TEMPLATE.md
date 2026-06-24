<!--
  ───────────────────────────────────────────────────────────────────────────
  REMADE WITH RUST — README TEMPLATE
  ───────────────────────────────────────────────────────────────────────────
  Copy this file to readme.md in a new project and fill in every {{PLACEHOLDER}}.
  Delete sections that don't apply, but keep the shared blocks marked
  "ORG BOILERPLATE — keep identical across repos" verbatim so every Remade With
  Rust project reads as one family.

  Replace:
    {{PROJECT}}            e.g. Starfire
    {{TAGLINE}}            one sentence, what it is + what it replaces
    {{ORIGINAL}}          the C/C++ tool being rebuilt, e.g. Moonlight
    {{ORIGINAL_URL}}      link to the original project
    {{ORIGINAL_LICENSE}}  e.g. GPLv3
    {{LICENSE}}           this project's license, e.g. Apache-2.0
    {{LANG_BADGE}}        platforms/targets, e.g. Windows · macOS
    {{METRIC_*}}          headline performance numbers (see Performance section)
  ───────────────────────────────────────────────────────────────────────────
-->

# {{PROJECT}}

[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)
[![License: {{LICENSE}}](https://img.shields.io/badge/license-{{LICENSE}}-blue)](LICENSE)
![Platforms: {{LANG_BADGE}}](https://img.shields.io/badge/platforms-{{LANG_BADGE}}-informational)

> **{{PROJECT}}** is {{TAGLINE}} — a ground-up **Rust** rebuild of
> [{{ORIGINAL}}]({{ORIGINAL_URL}}) ({{ORIGINAL_LICENSE}}/C++), under a permissive
> license, built for speed, safety, and zero copyleft strings.

---

## ⚡ The headline

<!-- Lead with the number. This is why someone clicks the repo. -->

| | {{ORIGINAL}} (C/C++) | **{{PROJECT}} (Rust)** | Change |
|---|---:|---:|:---:|
| {{METRIC_1_NAME}} | {{METRIC_1_OLD}} | **{{METRIC_1_NEW}}** | **{{METRIC_1_DELTA}}** |
| {{METRIC_2_NAME}} | {{METRIC_2_OLD}} | **{{METRIC_2_NEW}}** | **{{METRIC_2_DELTA}}** |
| {{METRIC_3_NAME}} | {{METRIC_3_OLD}} | **{{METRIC_3_NEW}}** | **{{METRIC_3_DELTA}}** |

<sub>Measured {{MEASUREMENT_CONDITIONS}}. Methodology + raw captures: [{{BENCH_DOC}}]({{BENCH_DOC}}).</sub>

---

## What is this?

{{PARAGRAPH: 2–4 sentences. What the tool does, who it's for, and what changed by
moving to Rust — memory safety, zero-copy, no GPL, a clean library API others can
embed. Keep it concrete.}}

## Remade With Rust

<!-- ORG BOILERPLATE — keep identical across repos -->

**Remade With Rust** is an initiative by [Mata Network](https://www.mata.network)
to rebuild essential C and C++ tools in Rust — for the memory safety, the
predictable performance, and the freedom of a permissive license. Each project is a reimplementation, not a fork: same wire protocols and file formats,
new code you can actually depend on.

We build the core to production grade and open-source it so the community can
extend it. No copyleft. No surprises. Just the tools we rely on, made faster and
safer.

→ More projects: **[github.com/remade-with-rust](https://github.com/remade-with-rust)**

<!-- /ORG BOILERPLATE -->

## Features

- {{FEATURE_1}}
- {{FEATURE_2}}
- {{FEATURE_3}}
- **Permissive license** ({{LICENSE}}) — embed it in closed-source software freely.
- **100% safe Rust** on the core path; every `unsafe` FFI boundary documented and isolated.

## Install

```sh
{{INSTALL_COMMAND}}
```

Or grab a prebuilt binary from [Releases]({{RELEASES_URL}}).

## Quick start

```sh
{{QUICKSTART_COMMAND}}
```

## Architecture

{{SHORT_ARCHITECTURE_PARAGRAPH_OR_DIAGRAM}}

```
{{ASCII_DIAGRAM_OPTIONAL}}
```

<!-- OPTIONAL — keep only if the tool has auth/onboarding worth highlighting.
     Useful when there's a "works with stock hosts" path AND a "zero-touch for
     fleets" path (e.g. a MATA mID identity for programmatic deployments). -->
## Authentication & deployment

- **{{ZERO_TOUCH_METHOD}} (default for MATA deployments).** {{ZERO_TOUCH_DESCRIPTION —
  e.g. authenticate with a MATA mID, a locally-verified cryptographic identity; no
  interactive step, built for programmatic/headless/fleet deployments}}
- **{{COMPAT_METHOD}} (universal compatibility).** {{COMPAT_DESCRIPTION — the
  standard mechanism stock hosts expect, retained so {{PROJECT}} is drop-in
  compatible}}

## Building from source

```sh
git clone {{REPO_URL}}
cd {{PROJECT_DIR}}
{{BUILD_COMMAND}}
```

**Requirements:** {{BUILD_REQUIREMENTS}}

## Platform support

| Platform | Status |
|---|---|
| {{PLATFORM_1}} | {{STATUS_1}} |
| {{PLATFORM_2}} | {{STATUS_2}} |

Adding a platform backend is a first-class extension point — implement the
{{TRAIT_NAMES}} traits, no protocol-core changes required.

## Roadmap

- [ ] {{ROADMAP_1}}
- [ ] {{ROADMAP_2}}
- [ ] {{ROADMAP_3}}

## License

{{LICENSE}} — see [LICENSE](LICENSE). No GPL/LGPL anywhere in the dependency tree
(CI-enforced via `cargo-deny`).

## About Mata Network

<!-- ORG BOILERPLATE — keep identical across repos -->

[Mata Network](https://www.mata.network) builds sovereign, self-hostable
infrastructure. **Remade With Rust** is our open-source home for the
permissively-licensed building blocks that work depends on.

<!-- /ORG BOILERPLATE -->
