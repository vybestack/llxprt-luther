# Local OpenCodeReview wrapper

Luther owns a local OCR entrypoint so contributors and automation do not need to remember project-specific review flags.

## Install OCR

Install the OpenCodeReview CLI that the PR workflow uses and make it available as `ocr`, or pass an explicit path:

```bash
npm install -g @alibaba-group/open-code-review
cargo xtask ocr-review --ocr-path /path/to/ocr --preview
```

The wrapper also honors `OCR_BIN=/path/to/ocr`. Local runs use your local OCR LLM configuration, keyring, and provider credentials. GitHub Actions uses repository secrets and the separate repository-root `.github/workflows/ocr-pr-review.yml` publication workflow.

## Usage

```bash
# Review staged, unstaged, and untracked local changes
cargo xtask ocr-review --current
make ocr-review ARGS="--current"

# Preview the files OCR will review, without running the full review
cargo xtask ocr-review --preview

# Review an explicit range
cargo xtask ocr-review --from main --to HEAD

# Review a pull request by computing merge-base to PR head
cargo xtask ocr-review --pr 123

# Preserve and validate machine-readable output
cargo xtask ocr-review --from main --to HEAD --format json
```

When no range mode is supplied, `--current` is the default. The command rejects conflicting modes and incomplete `--from` / `--to` pairs.

## Enforced project contract

Every full run performs a preview first and invokes OCR as:

```text
ocr review --audience agent --timeout 20
```

The `--timeout 20` floor and `--audience agent` output are mandatory defaults. `--format json` is added only when requested, and the wrapper fails if OCR emits empty or invalid JSON.

The preview protects test inclusion. If changed files include review-relevant tests or specs, OCR preview must list them under “Will review” and must not list them under “Excluded”. Review-relevant paths include `*test*`, `*spec*`, `tests/`, `__tests__/`, and Rust integration tests such as `tests/foo.rs`. Use `--allow-excluded-tests` only for a deliberate, documented local exception.

## Artifacts

Raw OCR output is preserved under `artifacts/ocr/`:

- `ocr-version.txt`
- `ocr-preview.txt`
- `ocr-preview-stderr.log`
- `ocr-stdout.raw`
- `ocr-stderr.log`
- `ocr-exit-code.txt`
- `ocr-result.json` when `--format json` is requested

Inspect these files when OCR fails to run, exits unsuccessfully, or produces invalid machine-readable output.

## Relationship to CI

This local wrapper is for deterministic local/project invocation. The GitHub Actions OCR workflow remains responsible for PR review publication and CI secret handling. Both paths enforce the same intent: agent-oriented output, the 20-minute timeout rule, raw artifact preservation, and review of in-scope test/spec files.

Do not weaken quality gates, add lint suppressions, or lower coverage/complexity expectations in response to OCR findings. Fix the underlying issue or document why a finding is outside the change scope.
