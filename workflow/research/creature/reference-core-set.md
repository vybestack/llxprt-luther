# Creature/Luther Core Reference Set

This document defines the minimal archive subset worth keeping as an ongoing reference for Creature/Luther.

The purpose of this reduced set is to preserve:

- project intent
- architectural boundaries
- MVP shape
- failure history and lessons learned
- verification philosophy
- long-term evolution direction

Everything outside this set is lower-value supporting material and can be discarded if the goal is to keep only the most decision-relevant reference docs.

---

## Core reference documents

### 1. `main/notes/harness/overview.md`
**Why keep it**
- Best high-level explanation of the harness-first philosophy
- Explains why structured workflows, verification, context engineering, and tools matter more than open-ended agent behavior

**What decisions it should influence**
- workflow design
- verification model
- evaluation philosophy
- how much autonomy to allow in the system

---

### 2. `main/notes/evolve/overview.md`
**Why keep it**
- Best synthesis of the long-term self-improving / self-evolving direction
- Captures the destination without forcing premature implementation choices

**What decisions it should influence**
- extensibility boundaries
- what should be versioned from day one
- what should evolve first versus remain stable

---

### 3. `main/notes/luther-mvp-attempt-1/overview.md`
**Why keep it**
- Concise postmortem of the first MVP attempt
- Gives the clearest high-level summary of what was built and what failed

**What decisions it should influence**
- MVP scope
- reset strategy
- what to avoid rebuilding in the same form

---

### 4. `main/notes/luther-mvp-attempt-1/boundary-violations.md`
**Why keep it**
- Highest-value warning document in the archive
- Explains how the engine/workflow boundary collapsed and why that mattered

**What decisions it should influence**
- engine versus workflow separation
- runtime versus policy separation
- service/supervision boundaries
- Rust architecture constraints

---

### 5. `main/notes/luther-mvp-attempt-1/what-to-keep.md`
**Why keep it**
- Captures which ideas and pieces were still worth preserving after the failed attempt
- Prevents throwing away the useful parts along with the mistakes

**What decisions it should influence**
- migration strategy
- what concepts to preserve in a Rust rewrite
- what abstractions are worth carrying forward

---

### 6. `main/llxprt-luther/REQUIREMENTS.md`
**Why keep it**
- Canonical behavior and product requirements reference
- Anchors implementation against stated expectations rather than vague memory

**What decisions it should influence**
- capability scope
- acceptance criteria
- user-facing behavior
- workflow semantics

---

### 7. `main/llxprt-luther/plans/architecture.md`
**Why keep it**
- Main intended architecture document
- Useful as the “target shape” to compare against both past failures and future designs

**What decisions it should influence**
- subsystem boundaries
- responsibilities of core components
- long-lived architectural structure

---

### 8. `main/llxprt-luther/project-plans/mvp/plan/00-overview.md`
**Why keep it**
- Best single overview of the intended MVP build sequence
- Converts broad goals into a concrete implementation path

**What decisions it should influence**
- MVP sequencing
- delivery order
- first milestones

---

### 9. `main/llxprt-luther/project-plans/mvp/plan/08-engine-machine.md`
**Why keep it**
- Most important MVP planning doc for the workflow engine / machine shape
- Highly relevant to a Rust rewrite

**What decisions it should influence**
- engine state model
- transitions
- persistence and resumability assumptions
- runner behavior

---

### 10. `main/llxprt-luther/project-plans/mvp/tests/design-constraints.md`
**Why keep it**
- Captures critical non-behavioral constraints and architecture guardrails
- Helps preserve the intended quality and system shape

**What decisions it should influence**
- implementation constraints
- testability requirements
- observability and structure expectations

---

### 11. `main/llxprt-luther/project-plans/mvp/tests/plan.md`
**Why keep it**
- Best testing and verification overview in the archive
- Shows how the system was supposed to prove correctness

**What decisions it should influence**
- QA strategy
- workflow-level testing
- engine verification
- regression prevention

---

## What this core set preserves

This reduced set preserves the minimum needed to remember:

- what Creature/Luther was trying to accomplish
- what architecture was intended
- what went wrong in the first attempt
- what should be carried forward
- what the MVP should look like
- what constraints and tests should shape implementation
- what long-term evolution path should remain open

---

## What is intentionally not preserved in the core set

The following can be useful, but are not essential for the smallest durable reference archive:

- broader supporting research collections
- detailed phase-by-phase implementation plans beyond the key MVP docs
- lower-priority test breakdowns
- supplemental rule/process documents already subsumed by the retained architecture, requirements, and postmortem docs

Those materials are valuable for deep dives, but they are not required for maintaining continuity of goals and lessons learned.
