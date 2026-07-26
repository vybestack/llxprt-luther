# Product gates

Status: binding. Established by issue #198 under parent #197.

Luther had no definition of "works" as a product, so no gate could fail for the
right reason. Component suites were green while 28 consecutive autonomous runs
produced zero pull requests. This document defines what must be true, what
evidence counts, and what evidence is disqualifying.

## Why this document exists

The project has four recorded instances of declaring success against a proxy that
omitted the product boundary:

1. The archived TypeScript attempt: 414/414 tests green, scrapped because only
   128 of 1,668 engine lines were generic.
2. The llxprt-first acceptance smoke: marked PASS with
   `0 passed; 0 failed; 2 ignored`.
3. The self-hosting canary: marked QUALIFIED while never invoking `EngineRunner`
   — it builds synthetic configs in memory but never executes one.
4. PR 196: five review cycles against a gate that `pull_request_target` guaranteed
   never executed.

The common shape is a measurement that cannot fail for the reason that matters.
Every rule below exists to make a specific one of those failures impossible.

## The three gates

### Gate A-R — runner reachability (diagnostic)

    explicit work item  ->  workflow run  ->  new draft PR

Isolates workflow reachability from discovery. **Diagnostic only.** Passing
Gate A-R does not mean the product works; it means the workflow can reach a PR
when handed a task directly.

### Gate A-D — approved issue to draft PR (official product gate)

    authorized approval event
      -> daemon discovery
      -> eligibility decision
      -> durable claim
      -> workflow routing
      -> workflow run
      -> new draft PR

This is the gate that matters. Only Gate A-D earns the claim "Luther picks up
approved issues and produces pull requests."

Note the scope limit: Gate A-D is reachable without the non-GitHub composability
proof (#212). Passing it proves the **GitHub software-change application** works
end to end. It does not prove the engine is composable.

### Gate B — draft PR to verified merge

    existing draft PR -> ready -> checks -> verified merge

## Required properties

Each gate must state all five, or it is not a gate:

| Property | Meaning |
|---|---|
| Causal start | The event that begins the measurement |
| Terminal evidence | What observation counts as success |
| Forbidden substitutions | Mechanisms that disqualify a run |
| Sampling protocol | Corpus, N, threshold, isolation, pinned versions |
| False positive | What a wrongly-counted success looks like |

## Forbidden substitutions

Each entry cites the mechanism in the existing canary that made it necessary.

| Substitution | Precedent |
|---|---|
| Writing the postcondition the gate claims to reach | Stage 2 writes the change directly, `canary_harness_tests.rs:635-653` |
| Supplying an already-successful observation | Stage 6 supplies head/remote SHAs, `:741-829` |
| Fabricating external state in local storage | Stage 7 inserts PR metadata into SQLite, `:842-870` |
| Returning a success observation from a probe | Stage 9 returns `merged: true`, `:900-918` |
| Asserting the harness called its own helpers | `run_canary` appends stage labels, `:986-1030` |
| An executor that cannot fail | `CanaryExecutor` always succeeds, `:238-289` |
| Bypassing the shipping binary | Library APIs are not the product |
| Reusing a prior successful diff, branch, or PR | Removes the work being measured |
| A fake external interface that answers plausibly | Must be built from captured contracts and fail closed |

A simulator is permitted. A simulator that **returns success without a modeled
state transition** is not — that is the same defect one layer out.

## Sampling

LLM steps are probabilistic, so a single run is not evidence.

- **Gate A: minimum 8 of 10** independent clean runs.
- **Gate B: 5 of 5** against a deterministic external simulator.
- **Zero false positives, absolute, at every gate.**

These are **normative floors**. They may be raised, never lowered. Once a
protocol is pre-registered its threshold is immutable; changing it requires a new
pre-registration visible in history.

Each run requires a clean workspace, no pre-existing branch or PR, no reused
workspace, and pinned tool and model versions. Discarded runs must be disclosed
with reasons. Undisclosed reruns invalidate the sample.

### Corpus admissibility

An independent per-task oracle does not prevent biased task **selection**. Ten
tasks chosen because the workflow already handles them yield a precise but
invalid result.

The corpus must be frozen before implementation, drawn from pre-existing real
issues, and selected by stated inclusion and exclusion rules independent of the
implementation. If the corpus must be narrow, the product claim is narrowed to
that task class rather than stated generally.

## False positives

A success is false if any of these hold:

- the PR head does not contain a fix produced by that run;
- the change was injected, pre-existing, or carried over;
- the terminal observation was supplied rather than obtained;
- the task's expected-behavior oracle does not pass;
- the run was retried without disclosure.

Every counted success needs an independent, pre-registered, task-specific oracle
that fails for a trivial or wrong change. "The commit is non-empty" is not an
oracle.

## Worked examples

**Legitimate pass.** Shipping binary, production config, clean workspace,
authorized approval event. Workflow produces a change; verification runs as a
typed step; commit and push reach a local bare remote; a draft PR is created
through the contract simulator. Terminal evidence: the draft PR exists, its head
commit contains the run-produced change, and the task oracle passes. Nothing was
supplied.

**Construct-invalid pass — must be judged invalid.** Same run, except the harness
pre-populates the workspace with the expected diff before launch. Every stage
reports success and the PR exists. This **fails** the gate: the implementation
postcondition was supplied. This is precisely the shape of canary stage 2.

**Ambiguous — adjudication.** A run reaches a draft PR, but one verification check
errored rather than failing, and the workflow proceeded. Adjudication: an errored
check is `EvidenceUnavailable`, not `Pass`. The run does not count as a success,
and it is recorded as an evidence-availability failure rather than a product
failure. Conflating the two produced issue #177.

## Standing rule

**A gate that injects its own postcondition proves nothing about reachability.**

If a gate cannot fail because the product is broken, it is not measuring the
product. Green results from such a gate are not evidence, regardless of how many
assertions they contain.
