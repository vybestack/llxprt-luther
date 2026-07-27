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

## Terminology

Two distinct failure kinds, conflated at cost:

- **Scoring false positive** — a run reported as passing that did not meet the
  gate's terminal evidence.
- **Harmful effect** — any wrong, trivial, unauthorized, duplicate, or
  task-mismatched effect produced during **any attempt**, including attempts
  scored as failures. This covers externally visible effects (PR, comment, push,
  merge) **and** local damage: writes outside the authorized workspace, corruption
  of product state, or destruction of unrelated files. A system that produces
  correct PRs while corrupting state it does not own has not passed.

Both are counted. A sample with eight correct PRs and two wrong ones is **not** a
pass, even when the two wrong runs are correctly scored as failures. Emitting a
harmful PR is a product defect regardless of how the harness scored it.

## Invariants binding on all gates

These are normative, not illustrative:

1. **Shipping binary.** The compiled release artifact, invoked as a subprocess.
   Library-level API calls are not the product.
2. **Production configuration.** The exact config shipped to users, by content
   hash. A test-specific workflow disqualifies the run.
3. **Pinned and recorded invocation manifest.** Binary hash, config hash, tool
   versions, model identity and version, simulator fixture version, **plus the
   full invocation**: argv, allowlisted environment variables, feature flags,
   config and runtime directory roots, credential and endpoint class by
   non-secret identity, and process topology. Recorded per run and required to
   match a documented supported production deployment. A gate-only mode — a flag,
   environment variable, or subcommand not used in production — disqualifies the
   run, because the same binary and config hashes can otherwise hide a path that
   works only under test.
4. **Per-trial state reset.** Every counted trial starts from fresh product
   SQLite state, artifact roots, Git and GitHub simulator state, model session,
   daemon process state, repository namespace, and workspace. Gate B may retain
   only its explicitly declared starting PR. Without this, an ordered sample can
   warm state in trial 1 and pass the rest from a condition production never
   reaches.
5. **Bounded observation for negative cases.** "Produces no PR" and "does not
   merge" are not finitely checkable without a deadline. Every negative case
   pre-registers: a minimum number of complete discovery/poll cycles, a durable
   typed skip or refusal outcome with a recorded reason, no pending or runnable
   work remaining for that input, and no prohibited transition in the simulator's
   immutable log through the deadline. Checking immediately after delivery is
   insufficient — it lets delayed unsafe behavior escape observation.
6. **Real model.** Work generation uses the production model endpoint. A fake or
   replayed model disqualifies the run — it can encode the answers.
7. **Simulators are permitted only at the named Git and GitHub boundaries.**
   Built from captured real contracts, fail closed on unknown calls, expose
   inspectable state, and log immutable transitions. A simulator that returns
   success without a modeled state transition is forbidden.
8. **Causal provenance.** Launch, generated commit, PR, and terminal observation
   must be bound to one run by recorded identifiers.
9. **Retry accounting.** Infrastructure retries are permitted only for failures
   provably outside the product, must be disclosed with cause, and are capped.
   Any undisclosed rerun invalidates the sample.
10. **Zero harmful effects**, absolute, across every attempt in the sample.

Where a rule below says **"Gate A"** without a suffix, it applies to **both A-R and
A-D**. Rules specific to one name it exactly.

## Gate A-R — runner reachability (diagnostic)

| Property | Definition |
|---|---|
| Causal start | An explicit work item handed directly to the shipping binary |
| Terminal evidence | A new draft PR exists whose head contains the run-produced change, and the task oracle passes |
| Forbidden | Discovery bypass is intentional here; all shared forbidden substitutions apply |
| Sample | **8 of 10**, corpus per admissibility rules below |
| Scoring false positive | Head lacks a run-produced change; oracle fails; observation supplied |

**Diagnostic only.** Passing Gate A-R isolates workflow reachability from
discovery. It does not license any product claim.

## Gate A-D — approved issue to draft PR (official product gate)

| Property | Definition |
|---|---|
| Causal start | An authorized approval event delivered to the daemon through the production path |
| Terminal evidence | Discovery, eligibility, durable claim, routing, and run all observed in order, terminating in a new draft PR whose head contains the run-produced change, with the task oracle passing |
| Forbidden | Injecting the work item past discovery; pre-seeding the claim; all shared forbidden substitutions |
| Sample | **8 of 10 positive tasks**, independent of the A-R sample, plus the safety set below at **zero tolerance** |
| Scoring false positive | Any A-R false positive, plus a PR produced without an authorized event, or a claim not durably held |

**Required safety set** (separate from the 10 positive tasks, all must hold):

- an unauthorized event produces no PR;
- an ineligible issue produces no PR;
- duplicate delivery of one event produces exactly one PR;
- concurrent claim attempts yield exactly one winner;
- a stale claim is not honored;
- an issue with an existing open PR does not produce a second.

**Claim scope.** Passing Gate A-D with a GitHub simulator establishes that the
GitHub software-change application works end to end **against captured GitHub
contracts**. It does not establish behavior against live GitHub — authentication,
permissions, rate limits, and API drift are unproven until at least one live
sandbox-repository smoke run passes. State the qualified form, not the general one.

Gate A-D does not prove the engine is composable; that is #212.

## Gate B — draft PR to verified merge

| Property | Definition |
|---|---|
| Causal start | An existing draft PR, **produced by a prior Gate A-D run** and identified by run ID |
| Terminal evidence | **Scenario-specific** — see below. Not uniform across scenarios |
| Forbidden | A probe returning `merged: true` without a modeled transition; fabricating merge state in local storage; supplying the final head |
| Sample | **4 of 4 merge-positive scenarios**, plus a separately scored zero-tolerance refusal scenario |
| Scoring false positive | Merged state supplied rather than observed; strategy or head mismatch; proof not validated; a refusal scenario that merged |

**Gate B starts from an existing PR by construction.** The no-pre-existing-PR rule
applies to Gate A only. The Gate B integrity requirement is different: the starting
PR must be attributable to a prior Gate A-D run, and the ready → checks → merge
transition must be produced by the run under test. All other shared invariants,
including per-trial state reset, apply unchanged.

**Four merge-positive scenarios**, each with terminal evidence "externally observed
merged state, with strategy, head, and base matching what the run intended, and
merge proof validating". Repeating one deterministic case four times is not
evidence:

1. merge commit; 2. squash; 3. rebase; 4. delayed or transitioning checks.

**One refusal scenario**, scored separately at zero tolerance: a fail-closed
contract mismatch. Its terminal evidence is the **opposite** — a durable typed
refusal outcome, no merge transition in the simulator's immutable log through the
observation deadline, and a recorded refusal reason. A merge here is a gate
failure, not a success.

Applying the merge-positive terminal evidence to the refusal scenario would make
Gate B unsatisfiable by a correct fail-closed system, which is why the terminal
evidence is scenario-specific rather than uniform.

## Forbidden substitutions

Each entry cites the mechanism in the existing canary that made it necessary.

| Substitution | Precedent |
|---|---|
| Writing the postcondition the gate claims to reach | Stage 2 writes the change directly, `canary_harness_tests.rs:635-653` |
| Supplying an already-successful observation | Stage 6 supplies head/remote SHAs, `:748-765`, `:805-822` |
| Fabricating external state in local storage | Stage 7 persists PR identity into SQLite, `:842-870` |
| A probe that returns success without a modeled transition | `DeterministicRemoteProbe::observe_merge` returns its injected observation, `:208-211`; constructed at `:875-895` and `:900-918` |
| Asserting the harness recorded its own labels | `run_canary` records labels and asserts the vector, `:990-1030` |
| An executor with no failure path | `CanaryExecutor` always returns success, `:272-289` |
| Bypassing the shipping binary or production config | Library APIs and test configs are not the product |
| A fake model replaying task-specific answers | Encodes the result being measured |
| Reusing a prior successful diff, branch, or PR | Removes the work being measured |

## Sampling

LLM steps are probabilistic, so a single run is not evidence.

- **Gate A-R: 8 of 10.** **Gate A-D: 8 of 10**, sampled independently — A-R
  results are never reused for A-D.
- **Gate B: 5 of 5** across the five distinct scenarios.
- **Zero harmful effects and zero scoring false positives**, absolute.

**These thresholds are release floors, not reliability estimates.** For Gate A,
8/10 has an exact 95% Clopper–Pearson interval of 0.444–0.975; a system with a true
50% success rate still passes about 5.5% of the time, and a true 80% system fails
the threshold about 32% of the time. Gate A results must report the observed
proportion **and its exact 95% interval**, and must not describe the floor as a
demonstrated reliability level.

**The interval applies to Gate A only.** Gate A tasks are independent draws from a
declared population — the target issue population, its random draw protocol, and a
requirement of distinct task identities must all be pre-registered. Gate B's
scenarios are fixed and deliberately heterogeneous, not independent draws from
anything, so a binomial interval over them is meaningless. **Gate B reports
scenario coverage**, not a confidence interval. Reliability inference for a Gate B
scenario requires independently seeded repetitions within that scenario, reported
per scenario.

Floors may be raised, never lowered. Once pre-registered, a threshold is immutable;
changing it requires a new pre-registration visible in history.

Each run requires a clean workspace, no pre-existing branch or PR (Gate A only),
no reused workspace, and pinned versions per the invariants.

### Corpus admissibility

An independent per-task oracle does not prevent biased task **selection**, and a
corpus visible during implementation invites overfitting to specific issue IDs.

- Inclusion and exclusion rules are pre-registered **before** implementation.
- The **actual tasks are drawn after the binary and config are frozen**, from a
  holdout not visible during implementation.
- No product, workflow, prompt, fixture, or simulator behavior may be keyed to
  corpus task identities.
- Tasks are pre-existing real issues, not written for the gate.
- If the corpus must be narrow, the claim narrows to that task class.

## Scoring false positives

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

**Legitimate pass.** Shipping binary at a recorded hash, production config by
content hash, real model, clean workspace, authorized approval event through the
production discovery path. Workflow produces a change; verification runs as a
typed step; commit and push reach a local bare remote; a draft PR is created
through the GitHub contract simulator, whose modeled state shows the transition.
Terminal evidence: the draft PR exists, its head commit contains the run-produced
change, provenance binds it to the run, and the task oracle passes.

**Construct-invalid pass — must be judged invalid.** Same run, except the harness
pre-populates the workspace with the expected diff before launch. Every stage
reports success and the PR exists. This **fails**: the implementation
postcondition was supplied. This is the shape of canary stage 2.

**Ambiguous — adjudication.** A run reaches a draft PR, but one verification check
errored rather than failing, and the workflow proceeded. Adjudication: an errored
check is `EvidenceUnavailable`, not `Pass`. The run does not count as a success,
and is recorded as an evidence-availability failure rather than a product failure.
Conflating the two produced issue #177.

## Standing rule

**A gate that injects its own postcondition proves nothing about reachability.**

If a gate cannot fail because the product is broken, it is not measuring the
product. Green results from such a gate are not evidence, regardless of how many
assertions they contain.
