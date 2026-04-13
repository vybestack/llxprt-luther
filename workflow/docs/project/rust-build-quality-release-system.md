# Rust Build, Quality, Release, and Distribution System

## Purpose

This document defines the project-level Rust engineering system to adopt for Luther.

It is based on successful patterns copied from:

- `personal-agent/ui`
- `homebrew-tap`

The goal is not to copy application code or application-specific workflows.
The goal is to copy the **project discipline**:

- strict local quality gates
- a Rust-native task runner
- strong CI layering
- deterministic release packaging
- automated Homebrew publishing
- service-aware distribution for macOS and Linux

---

## Source patterns we are copying

## From `personal-agent/ui`

Primary patterns worth copying:

- central lint policy in `Cargo.toml`
- cargo aliases via `.cargo/config.toml`
- Rust-native `xtask` task runner
- `cargo qa` as the standard project gate
- forbidden-marker scanning in `xtask`
- LLVM coverage gate in `xtask`
- layered GitHub Actions for fmt/lint/test/coverage
- tag-driven release workflow
- packaging script that emits artifact metadata
- script-driven Homebrew tap update

## From `homebrew-tap`

Primary patterns worth copying:

- Homebrew formula with:
  - `on_macos`
  - `on_linux`
  - CPU-aware asset selection
- formula caveats
- simple formula smoke test
- tap README with clean install/upgrade guidance

---

## Guiding principles

## 1. One canonical local quality command

The project should have one standard engineering gate:

- `cargo qa`

That command should be the local equivalent of CI quality checks.

It should be easy for contributors and agents to discover and run.

## 2. Use `xtask`, not shell-sprawl

Quality, guard, coverage, and packaging orchestration should be implemented in a Rust `xtask` crate rather than scattered through bash scripts.

Use shell only where it is the natural boundary:
- release packaging
- artifact assembly
- Homebrew tap update

Everything else should prefer Rust-native task code.

## 3. CI should be layered

CI should separate:
- formatting
- linting
- tests
- coverage
- service-system checks
- release-only work

This makes failures easier to diagnose and keeps the project maintainable.

## 4. Releases should be deterministic

Release artifacts should:
- be built from tags
- have stable naming
- emit checksums
- carry explicit target triples
- be consumable by Homebrew formulas cleanly

## 5. Distribution must match the service model

Because Luther will run as a foreground service supervised by launchd/systemd, packaging should support:
- the main binary
- service install commands
- service-related user guidance in formula caveats

---

## Recommended project file layout

## Cargo and local tooling

- `Cargo.toml`
- `.cargo/config.toml`
- `xtask/Cargo.toml`
- `xtask/src/main.rs`

## CI

- `.github/workflows/pr-quality.yml`
- `.github/workflows/release.yml`

## Release scripts

- `scripts/release/package_target.sh`
- `scripts/release/update_homebrew_tap.sh`

## Optional distribution helpers

- `scripts/release/build_matrix_targets.sh`
- `scripts/release/verify_release_artifacts.sh`

## Docs

- `docs/architecture/service-system-and-foreground-manager.md`
- `docs/project/rust-build-quality-release-system.md`

---

## Cargo.toml policy

The root `Cargo.toml` should include a central lint posture similar to the successful project reference.

## Recommended lint section

```toml
[lints.rust]
unsafe_code = "warn"

[lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = "warn"
nursery = "warn"
cognitive_complexity = "warn"
```

This should be copied conceptually, with project-specific exceptions only when justified.

## Dev/test profile guidance

Copy the pattern of disabling incremental compilation in dev/test if it proves beneficial for deterministic builds and artifact size.

```toml
[profile.dev]
incremental = false

[profile.test]
incremental = false
```

This should be adopted if the project sees the same issues with large or noisy incremental state.

## Binary declaration guidance

If the project exposes distinct roles, define explicit binaries in `Cargo.toml` rather than relying on accidental defaults.

Potential future pattern:

```toml
[[bin]]
name = "luther"
path = "src/main.rs"
```

If later the project splits into multiple binaries, declare them explicitly.

---

## .cargo/config.toml policy

Copy the cargo alias pattern.

## Recommended aliases

```toml
[alias]
xtask = "run --quiet --manifest-path xtask/Cargo.toml --"
qa = "xtask qa"
coverage = "xtask coverage"
```

Recommended additional aliases:

```toml
fmt = "xtask fmt"
lint = "xtask clippy"
test-all = "xtask test"
```

These make the project’s quality gates easy to discover and use.

---

## `xtask` policy

The `xtask` crate should be the canonical quality/build orchestration layer.

## Required commands

At minimum:

- `cargo xtask qa`
- `cargo xtask fmt`
- `cargo xtask clippy`
- `cargo xtask test`
- `cargo xtask coverage`
- `cargo xtask guard`

Recommended later additions:

- `cargo xtask dist`
- `cargo xtask service-verify`
- `cargo xtask release-dry-run`

---

## `xtask qa`

This should be the standard local gate.

Recommended sequence:

1. forbidden source guard checks
2. rustfmt check
3. clippy
4. tests
5. coverage

This mirrors the successful reference pattern.

---

## Guard checks to copy

The reference `xtask` contains a valuable pattern: ban unfinished runtime markers in source.

Luther should copy this pattern.

## Recommended forbidden patterns in runtime source

At minimum, scan production Rust source for:

- `TODO`
- `FIXME`
- `todo!(`
- `unimplemented!(`
- `panic!("not yet implemented` style patterns if relevant

This is especially useful in a project that may involve agent-generated code, because it blocks placeholder sludge from entering the codebase unnoticed.

## Recommended architecture guard additions

Because the archive showed boundary collapse as a major failure mode, `xtask guard` should eventually also include architecture checks such as:

- service layer must not import workflow domain layer directly in the wrong direction
- runtime kernel must not import service adapters
- workflow/domain code must not leak into generic engine/kernel modules

These can start as simple source-pattern or dependency-graph checks.

---

## Coverage policy

Copy the LLVM coverage gate pattern from the successful reference.

## Recommendation

Use:
- `cargo-llvm-cov`
- a Rust-native `xtask coverage` wrapper
- a workspace line-coverage threshold enforced in code

This is a strong pattern because:
- it works locally and in CI
- it keeps the threshold versioned in Rust code
- it avoids shell-driven drift

## Suggested initial gate

Start with a realistic threshold rather than a vanity number.

If the codebase is brand new, it is reasonable to begin with a lower gate and ratchet upward over time.

The exact threshold should be set pragmatically once the first real code lands.

---

## PR CI design

Copy the layered CI idea from `personal-agent/ui`, but adapt it for a cross-platform service-oriented Rust tool.

## Workflow file

Create:
- `.github/workflows/pr-quality.yml`

## Platform matrix

The project should test at least:

- `ubuntu-latest`
- `macos-14`

Windows can be added later when actual support work begins.

## Core jobs

### 1. fmt
- install stable Rust
- run rustfmt check

### 2. lint
- install stable Rust
- run clippy with warnings denied
- run `cargo xtask guard`
- run structural maintainability checks if desired

### 3. test
- run unit/integration tests on both macOS and Linux

### 4. coverage
- run on one canonical platform initially
- install `llvm-tools-preview`
- install `cargo-llvm-cov`
- run `cargo coverage`

### 5. service-file verification
This is specific to Luther.

It should verify generated service definitions.

Examples:
- launchd plist syntax on macOS via `plutil -lint`
- systemd unit rendering sanity on Linux
- optional `systemd-analyze verify` if environment permits

This job is important because the service system is a first-class deliverable.

---

## Structural quality checks

The reference project used additional structural checks beyond clippy:
- complexity budget
- file length budget

This is worth copying.

## Recommendation

Keep these as optional but encouraged checks in CI:

- complexity threshold via `lizard` or equivalent
- file-length warnings/errors for very large files

This is particularly useful in long-lived agent-heavy projects where files can otherwise quietly bloat.

---

## Release workflow design

Copy the tag-driven release model, but make it multi-target.

## Workflow file

Create:
- `.github/workflows/release.yml`

## Triggers

- push tags matching `v*`
- manual dispatch with explicit release tag

## Required release steps

1. resolve and validate release tag
2. validate required secrets early
3. build release artifacts for all supported targets
4. package tarballs deterministically
5. generate SHA256 checksums
6. create or update GitHub Release
7. update Homebrew tap automatically

---

## Release targets

Unlike the source reference, Luther should not be macOS-arm64-only.

## Recommended initial release targets

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

Windows targets should be added only when Windows runtime/service support is real.

---

## Packaging policy

The reference project had a single-purpose packaging script. Copy that pattern, but generalize it.

## Recommended script strategy

Use one parameterized packaging script:

- `scripts/release/package_target.sh`

Inputs:
- release tag
- target triple
- binary name
- output asset name

Outputs:
- packaged tarball
- sha256
- metadata files consumable by CI

## Expected artifact naming

Use names like:

- `luther-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- `luther-vX.Y.Z-x86_64-apple-darwin.tar.gz`
- `luther-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `luther-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`

This aligns with the Homebrew formula style in the tap reference.

---

## Homebrew publishing design

Copy the release-to-tap update pattern.

## Tap update script

Create:
- `scripts/release/update_homebrew_tap.sh`

It should:

1. validate release tag
2. require tap token and repo information
3. clone the tap repo into a temp dir
4. rewrite the formula file from canonical release metadata
5. no-op if unchanged
6. commit and push with bot identity if changed

This is a proven pattern from the source reference and should be preserved.

---

## Homebrew formula design

Copy the `jefe.rb` pattern from the tap reference, not the single-asset personal-agent formula.

## Formula expectations

The formula should:
- use `on_macos` and `on_linux`
- branch by CPU architecture
- install the main binary cleanly
- include caveats for service installation
- include a simple smoke test

## Example caveat direction

The caveats should guide users toward the foreground-service-manager model.

Examples:

### macOS
- `luther service install --user`
- `luther service start`

### Linux
- `luther service install --user`
- `systemctl --user status luther.service`

This makes distribution consistent with the service architecture.

---

## Homebrew tap README

Copy the simple structure from the tap reference:

- what the tap is
- install command
- available formulae table if needed
- update/upgrade commands

This does not need to be fancy.

---

## Recommended GitHub Actions design summary

## `pr-quality.yml`

Jobs:
- `fmt`
- `lint`
- `test`
- `coverage`
- `service-verify`

Matrix:
- macOS
- Linux

## `release.yml`

Jobs:
- validate tag/secrets
- build/package matrix
- publish release
- update tap

This is the structure to copy.

---

## What not to copy literally

Some parts of the source systems should be adapted, not cloned verbatim.

## Do not copy literally

- macOS-arm64-only release assumptions
- application-specific E2E tests that depend on provider secrets and app UX
- formula details tied to a different binary/app name
- packaging steps specific to GUI/app signing assumptions unless they actually apply

## Do copy conceptually

- `xtask`
- cargo aliases
- strong local gate command
- forbidden marker checks
- coverage gate
- layered CI
- tag-driven release
- script-based tap updates
- multi-platform Homebrew formula pattern

---

## Recommended initial implementation checklist

## Local engineering system

- [ ] add root `Cargo.toml` lint policy
- [ ] add `.cargo/config.toml` aliases
- [ ] add `xtask/`
- [ ] implement `qa`, `fmt`, `clippy`, `test`, `coverage`, `guard`

## CI

- [ ] add `.github/workflows/pr-quality.yml`
- [ ] add macOS/Linux matrix for tests
- [ ] add service-file verification job

## Release

- [ ] add `.github/workflows/release.yml`
- [ ] add multi-target packaging script
- [ ] emit SHA256 checksums and metadata
- [ ] publish GitHub Release from tags

## Homebrew

- [ ] add tap-update script
- [ ] generate formula with `on_macos` / `on_linux`
- [ ] include service usage caveats

---

## Recommended next-step file set for this project

- `Cargo.toml`
- `.cargo/config.toml`
- `xtask/Cargo.toml`
- `xtask/src/main.rs`
- `.github/workflows/pr-quality.yml`
- `.github/workflows/release.yml`
- `scripts/release/package_target.sh`
- `scripts/release/update_homebrew_tap.sh`
- Homebrew formula in tap repo generated by release automation

---

## Final recommendation

The successful build/quality/release system to copy is:

- **Rust-native project automation via `xtask`**
- **cargo aliases for discoverable workflows**
- **layered CI with fmt/lint/test/coverage**
- **tag-driven releases with deterministic assets**
- **automated Homebrew publishing**
- **multi-platform Homebrew formula generation**

For Luther specifically, extend that copied system with one project-specific addition:

> **service-definition verification must be a first-class CI concern**, because the foreground service manager is part of the product architecture, not just deployment garnish.
