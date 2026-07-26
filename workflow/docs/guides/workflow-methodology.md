# Luther workflow methodology

Rules derived from a 30-run dogfood sequence on issue #138 that produced zero
merged PRs. Each rule states the failure it prevents.

## 1. Check upstream before dispatching Luther

Much of Luther's OCR surface is a port of `vybestack/llxprt-code`. Before
pointing Luther at an issue, search that repository's issue tracker and recently
merged PRs for the same problem.

**What went wrong.** Luther issue #138 ("Reject incomplete OCR reviews and
persist an immutable reviewed-range manifest") is the same problem as
llxprt-code issue #2575. Upstream merged the fix as PR #2716 while we were on
run v30, having spent 30 runs autonomously reinventing it. The upstream solution
is roughly 4 files, three of them tests, using a positive-allowlist
`resolveCompleteness()` — deterministic, fail-closed, no agent prompt rules.

**Checklist before dispatch.**

- Search llxprt-code issues for the problem statement.
- Search recently merged llxprt-code PRs.
- Check whether pinned external tool versions are stale (see rule 3).
- Only then decide whether Luther should implement it or port it.

## 2. Never fix defects by growing agent prompts

Luther exists to provide a consistent typed workflow. If a behaviour is
deterministic, it belongs in a typed workflow step with tests in the Rust suite
— never as prose in a step prompt.

Deterministic work includes path resolution, null/empty handling, subprocess
working directory, artifact selection, and output parsing.

**Why prose fails.** Prompt rules bias a sampler; they do not enforce. Rule N
dilutes rule 1, and an agent will satisfy a rule in the code path it happens to
be editing while leaving the banned pattern alive elsewhere.

**Evidence.** A rule banned whitespace-width parsing of tool output. In run v30
the agent honoured it in newly written JavaScript and left the identical
whitespace-splitting parser in place in the workflow YAML. A typed step does not
miss a code path, and a test fails when the pattern returns.

**Measured drift.** The workflow has 41 steps: 35 typed, 6 prompt-bearing. Yet
the `implement` prompt reached 23,244 bytes with 18 all-caps rules — half of all
prompt bytes and most of the rules concentrated in one step. Fixes that stuck
permanently were Rust code plus tests; fixes that did not stick were prompt
prose.

**Rule of thumb.** If you can write a test that fails when the behaviour
regresses, it does not belong in a prompt.

## 3. Verify external tool pins before debugging behaviour

Confirm the pinned version is current before investigating a tool's behaviour.

**What went wrong.** Debugging proceeded against OCR 1.7.13 while 1.7.16 was
published. Release 1.7.15 contained "drain per-file comment work without racing
pool submissions", a concurrency race that could cause incomplete reviews or
deadlocks — plausibly contributing to the coverage symptoms under investigation.

## 4. Audit progress by depth, not by local detail

Track how far each run penetrates the pipeline and compare across blocks of
runs. A local improvement is not progress if the depth ceiling has not moved.

**What went wrong.** Run v30 was reported as "the first run to pass all checks."
Run v15 had done so fifteen runs earlier. Depth had been flat at `evaluate_impl`
across v15-v30, with zero of thirty runs reaching push or PR creation. Reporting
local detail as global progress hid a plateau.

## 5. Run a control before attributing failure

When repeated runs fail on one issue, run a different, unrelated issue before
concluding the engine is at fault. Thirty runs were spent on a single issue with
no baseline for comparison.
