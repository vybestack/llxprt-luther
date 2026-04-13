# Rust Workflow Systems Overview for Luther / Creature

## Executive summary

After reviewing current Rust workflow engines and the archived Creature/Luther planning material, the best fit is:

> **Use `dagrs` as the runtime, but define a small declarative workflow layer on top of it in YAML or TOML.**

That recommendation is driven less by generic "workflow engine" marketing and more by the specific shape of the problem the archived docs describe:

- local, embeddable execution
- orchestration of external tools and CLIs
- branches based on command results
- loops and retry/remediation cycles
- explicit separation between a generic engine and workflow-specific logic
- workflow definition separate from implementation code
- persistence / resume desirable
- deterministic skeleton with agentic steps inserted where judgment is needed

`dagrs` is the best current Rust match because it explicitly supports **conditional nodes**, **loop DAGs**, **dynamic routing**, **checkpointing**, and **custom configuration parsers**. That makes it the strongest substrate for a Luther-style system.

If the top priority were simply "workflow definitions in JSON out of the box," then `actflow` would be the runner-up.

---

## What locally runnable / embeddable Rust workflow systems exist?

The most relevant options found were:

### 1. `dagrs`

**Shape:** async DAG / task-graph execution framework.

**Relevant capabilities:**
- local execution
- embeddable in a Rust process
- conditional nodes
- loop DAG support
- dynamic router support
- checkpointing / resume
- custom configuration parser support

**Why it matters here:**
This is the only option I found that so directly lines up with:
- steps
- branches
- loops
- workflow defined outside code
- command/tool orchestration

**Best use:** build a declarative workflow DSL for CLI/git/tool-driven automation.

---

### 2. `actflow`

**Shape:** lightweight event-driven embedded workflow engine.

**Relevant capabilities:**
- embeddable
- local runtime
- pluggable storage
- workflow definitions via JSON
- node/edge workflow model

**Why it matters here:**
This has the clearest explicit support for **workflow definitions separate from code** via JSON workflow models.

**Tradeoff:**
The docs I reviewed make JSON-defined workflows clear, but looping/routing control for Luther-style remediation cycles was less explicitly documented than in `dagrs`.

**Best use:** when the top priority is a workflow file format today, especially JSON.

---

### 3. `treadle`

**Shape:** persistent, resumable, human-in-the-loop local DAG workflow engine.

**Relevant capabilities:**
- local single-process workflows
- persistent state
- resumability
- human review pauses
- fan-out subtasks
- SQLite state store

**Why it matters here:**
Good if the system is mostly a durable local DAG with review gates.

**Tradeoff:**
Workflow definitions are primarily in code, and it is less obviously shaped for external declarative workflow definitions plus loop/router behavior.

---

### 4. `apalis-workflow`

**Shape:** workflow engine layered on `apalis`.

**Relevant capabilities:**
- sequential workflows
- DAG workflows
- durable / resumable execution
- integration with multiple backends

**Tradeoff:**
Best if adopting `apalis` broadly. Less attractive if the core requirement is "separate declarative workflow definition for local automation."

---

### 5. `floxide`

**Shape:** directed graph workflow framework.

**Relevant capabilities:**
- distributed and parallel graph execution
- retries
- checkpointing
- typed nodes
- split / merge patterns

**Tradeoff:**
Powerful, but more framework-oriented and code/macro-driven than declarative by default.

---

### 6. `ironflow`

**Shape:** event-sourced durable workflow/state machine engine.

**Relevant capabilities:**
- deterministic workflow core
- event sourcing
- timers
- outbox/effects architecture

**Tradeoff:**
Architecturally strong, but shaped more like an event-sourced domain workflow engine than a flexible CLI/git orchestration runner.

---

### 7. `Temporal` via Rust SDK

**Shape:** Rust SDK for a durable workflow platform.

**Relevant capabilities:**
- powerful workflow semantics
- retries, timers, durable execution
- self-hosted possible

**Tradeoff:**
Operationally much heavier and not really an "embedded library workflow engine" in the same sense as the above options.

---

## Which is best for steps, branches, loops based on CLI and git results?

## Best match: `dagrs`

This is the strongest fit for the actual workflow shape implied by Luther:

- run a command
- inspect exit code / stdout / stderr
- branch accordingly
- loop through planning / review / implement / test / remediate cycles
- persist state between steps
- possibly resume after interruption
- define workflow separately from execution code

That is more naturally a **task-graph orchestration** problem than a pure BPM or event-sourced business-process problem.

### Why `dagrs` wins

The documentation explicitly calls out:
- **Conditional Node**
- **Loop DAG**
- **Dynamic Router**
- **Customized Configuration Parser**

That is almost a one-to-one match with the needs of a Luther-like engine.

### Why not the others?

- **`actflow`**: strongest built-in JSON story, but less explicit evidence for rich loop/router behavior
- **`treadle`**: durable and resumable, but more DAG/pipeline oriented than workflow-DSL oriented
- **`apalis-workflow`**: useful if bought into the `apalis` ecosystem, but not the cleanest fit here
- **`ironflow`**: good durable architecture, wrong abstraction level for CLI/git orchestration
- **`Temporal`**: capable but too heavy for the stated need

---

## Best engine if workflow definition must be separate from code

This requirement is the main discriminator.

### Best explicit out-of-the-box support: `actflow`

If the primary requirement is:
- load a workflow model directly from a file
- use JSON nodes/edges
- keep the flow structure clearly outside application code

then `actflow` is the most direct answer.

### Best overall fit despite requiring a thin DSL layer: `dagrs`

If the requirement is broader:
- separate workflow definition from implementation code
- plus branches / loops / retries / router logic
- plus command-driven behavior

then `dagrs` is still the better answer.

The difference is:
- `actflow` gives you the file-defined workflow more directly
- `dagrs` gives you the control-flow substrate you actually need

For Luther, the second matters more.

---

## Understanding of what Creature / Luther were attempting to do

Based on the archived planning, requirements, and post-mortem docs, my understanding is that Creature/Luther were attempting to build:

> **a locally runnable autonomous software-engineering workflow system that watches for work, claims it, performs a structured fix workflow, uses agents only where judgment is needed, and drives the rest through a deterministic, inspectable, resumable engine.**

More concretely, the intent appears to have been:

### 1. Build an autonomous issue-to-PR workflow

The workflow described in the docs is extremely explicit:
- scan for eligible issues
- claim one
- plan a fix
- review the plan
- implement the fix
- run tests/lint/format/build
- commit and push
- open a PR
- watch CI and review comments
- diagnose failures
- triage review comments
- respond and/or remediate
- loop until success or abandonment
- log outcome and return to idle scanning

This is not a vague "agent that codes." It is a **structured operational workflow** for software maintenance.

### 2. Separate deterministic orchestration from agentic judgment

The archive repeatedly points toward a harness-first philosophy:
- workflow first
- evals and verification first
- mechanical steps should be deterministic
- agentic steps should be used selectively where judgment matters

That means the goal was not to let an LLM freely improvise the whole run.
The goal was to embed agent calls inside a strongly structured workflow.

In the archived state machine and step breakdowns, the agentic/judgment-heavy steps are things like:
- planning
- reviewing the plan
- implementation
- diagnosing CI failures
- triaging review comments
- remediation

The deterministic/mechanical steps are things like:
- git operations
- issue selection and filtering
- session/output persistence
- branch/transition logic
- CI polling
- PR bookkeeping
- cleanup and logging

### 3. Keep the engine generic and the workflow domain-specific

The post-mortem makes this especially clear.
The intended architecture was:
- a **generic reusable engine** that knows about states, transitions, persistence, retry, signals, logging, and execution machinery
- a **workflow layer** that knows about GitHub issues, PRs, CI, review comments, and LLM-assisted steps

The failure of the first attempt was described as a collapse of that boundary: the engine became Luther-specific instead of remaining reusable and generic.

That tells me the real design goal was not just "ship a bot."
It was:

> **build a reusable workflow engine capable of hosting a Luther-style software-engineering workflow without baking domain concepts into the engine itself.**

### 4. Make workflows inspectable, testable, and resumable

The archive places unusually high emphasis on:
- explicit requirements
- testable step contracts
- persisted state
- recovery after interruption
- loop counters and convergence boundaries
- artifact lifecycle
- outcome logging
- signal handling and shutdown behavior

That implies the desired system was not meant to be a black box.
It was meant to be:
- debuggable
- resume-capable
- bounded
- auditable
- mechanically verifiable

### 5. Support long-running, repeated operational use

The presence of `IDLE -> SCANNING` loops, lock handling, cleanup, persisted state, and outcome logs strongly suggests this was meant to be a daemon-like operational system, not just a one-shot script.

The harness notes reinforce the same idea: a daemon/orchestrator dispatches work, agents execute within a harness, humans steer the system through work selection and constraints rather than hand-driving every action.

### 6. Use external tools as first-class workflow actions

The archived workflow depends heavily on interactions with:
- git
- GitHub / gh-like functionality
- build/test/lint/format commands
- agent/LLM invocations
- logs and diffs

That implies the true execution model is not in-process function composition alone.
It is **workflow over tool invocations and their outputs**.

That is precisely why a router/loop/checkpoint/task-graph engine fits better than a purely business-process engine.

### 7. Preserve workflow definition as data or at least as a separable layer

The archived plans repeatedly discuss a JSON-defined workflow, generic loading, topology separate from executable logic, and strict boundary rules.

Even though the first implementation apparently failed at that separation, the intended architecture is clear:
- workflow structure should not be trapped inside engine code
- the engine should load/host workflows
- the domain logic should plug into the engine rather than being fused with it

So the fundamental aspiration was:

> **a reusable workflow runtime for structured software-engineering operations, where the workflow is a separately defined artifact and the engine remains domain-agnostic.**

---

## Restating the best Rust workflow choice given that goal

Given the actual Creature/Luther idea, the best Rust choice is:

## **`dagrs` + a declarative Luther workflow DSL**

That means:
- use `dagrs` as the orchestration substrate
- define Luther workflows in YAML or TOML
- implement a set of reusable action types in Rust
- keep workflow topology separate from execution code
- keep domain adapters separate from the engine core

### Why this is the best fit for the idea

Because the idea needs all of these at once:

#### A. Generic engine vs domain workflow separation
`dagrs` can be used as a runtime substrate while keeping:
- generic orchestration in one layer
- Luther-specific actions/adapters in another

#### B. Branches and loops as first-class features
Luther is fundamentally a loop-heavy machine:
- revise-plan loops
- fix-tests loops
- remediate-and-retest loops
- watch/diagnose/triage/respond cycles

`dagrs` explicitly supports the control-flow features this shape needs.

#### C. Tool-driven execution
The actions are naturally expressed as steps that:
- run shell commands
- call git helpers
- call GitHub adapters
- invoke agent subprocesses
- inspect outputs
- emit structured results for routing

That is a comfortable fit for a task-graph engine.

#### D. External workflow definition
Because `dagrs` explicitly supports custom config parsing, you can define a Luther-specific DSL like:
- `scan_issues`
- `plan_fix`
- `review_plan`
- `implement_fix`
- `run_checks`
- `submit_pr`
- `watch_pr`
- `triage_comments`
- `remediate`
- `abandon`
- `log_outcome`

with edges, guards, and loop policies living in YAML/TOML rather than in core code.

#### E. Resume/checkpoint support
The archived design strongly values persisted state and recovery. `dagrs` checkpointing provides a foundation for that, even if Luther-specific persistence details need a wrapper layer.

---

## Recommended architecture in Rust

## Layer 1: generic runtime layer

This layer should know about:
- workflow loading
- step scheduling
- transitions
- checkpoints
- run context
- cancellation / shutdown
- generic retry helpers

It should **not** know about:
- GitHub issues
- PRs
- review comments
- agent profiles
- git branch naming
- CI diagnosis classes

This is the lesson the first attempt drove home.

## Layer 2: Luther domain layer

This layer should define:
- issue / PR / review / CI domain models
- Luther-specific step types
- branching semantics for workflow outcomes
- adapters to git / GitHub / agent runtime
- session and artifact policies

## Layer 3: declarative workflow definitions

Store these in YAML or TOML.

Example shape:

```yaml
workflow: fix-issue
initial: scan

steps:
  scan:
    action: scan_issues
    on:
      issue_found: plan
      no_issues: idle

  plan:
    action: plan_fix
    on:
      plan_ready: review

  review:
    action: review_plan
    on:
      approved: implement
      revise: plan
      max_loops_exceeded: abandon

  implement:
    action: implement_fix
    on:
      implemented: test

  test:
    action: run_checks
    on:
      pass: push
      fail_retryable: fix_tests
      fail_terminal: abandon
```

This preserves the design intent much better than hardcoding the full state machine into the runtime.

## Layer 4: harness / verification layer

This should implement the archived philosophy around:
- deterministic verification
- structured logs
- evals/tests
- architecture boundary checks
- import/dependency enforcement
- outcome logging

The harness notes are very clear that this layer is not optional.

---

## Final recommendation

### Best overall choice
**`dagrs`**

### How to use it
**Do not use it raw as the full product abstraction.**
Use it as the execution substrate beneath a declarative Luther workflow model.

### Runner-up
**`actflow`** if you want immediate JSON-defined workflows and are willing to accept a weaker fit on explicit loop/router semantics.

### Why not choose based on generic popularity
Because the archived Creature/Luther idea is not a generic BPM problem. It is a:
- local
- tool-driven
- branch-heavy
- loop-heavy
- resumable
- harnessed
- engine/domain-separated
software-engineering workflow system.

That pushes the choice strongly toward **`dagrs`**.

---

## Sources reviewed

### Rust workflow engines
- `dagrs` docs / crate overview
- `actflow` docs
- `treadle` docs
- `apalis-workflow` docs
- `floxide` docs
- `ironflow` docs
- `Temporal` Rust SDK references

### Archived Creature/Luther material
- `research/creature/archive/main/notes/luther-mvp-attempt-1/*.md`
- `research/creature/archive/main/notes/evolve/overview.md`
- `research/creature/archive/main/notes/harness/overview.md`
- `research/creature/archive/main/llxprt-luther/REQUIREMENTS.md`
- `research/creature/archive/main/llxprt-luther/RULES.md`
- `research/creature/archive/main/llxprt-luther/plans/architecture.md`
- `research/creature/archive/main/llxprt-luther/project-plans/mvp/requirements.md`
- `research/creature/archive/main/llxprt-luther/project-plans/mvp/review-findings.md`
- selected plan/test docs under `project-plans/mvp/`
