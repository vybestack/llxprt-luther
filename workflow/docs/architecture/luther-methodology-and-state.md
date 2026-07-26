# Luther: whole-history analysis, methodology, and direction

Audience: the managing agent (human or LLM) directing work on Luther, until
Luther is self-directing. Read this **before** touching a workflow TOML, and
before believing any "QUALIFIED" or "phases complete" claim in this repository.

This revision replaces an earlier draft that was recency-biased, framed the
problem as a defect queue, and did not interrogate the project's own
qualification claim. Corrections from an independent whole-history audit are
incorporated and marked.

---

## 1. The one-sentence finding

> Luther repeatedly declares success against internal proxies that omit the
> product boundary. The components are good. The claims are not earned.

Everything below is a consequence of that sentence.

---

## 2. What Luther is supposed to be

A **static, typed, workflow-driven engine with LLMs embedded inside steps but
not driving control flow.** Evals later inspect completed workflows, find where
the workflow was wrong, and file issues against Luther.

**This is currently aspirational, not descriptive.** The LLM selects edges today
by printing magic strings that map directly to transitions:
`PLAN_APPROVED`/`PLAN_NEEDS_REVISION` (`dogfood-v1.toml:327-336`),
`IMPLEMENTATION_COMPLETE` (`:441-448`),
`IMPL_APPROVED`/`NEEDS_WORK`/`BLOCKED` (`:470-479`). The graph constrains the
menu; the model orders from it. That is LLM-mediated control flow and
self-certification.

---

## 3. The arc — four attempts, one recurring meta-error

**Attempt 1 (TypeScript, archived).** ~260 EARS requirements, 15 phases, ended
**414/414 tests green, all quality gates passing** — and was scrapped, because
only 128 of 1,668 engine lines were generic and the engine imported the Luther
workflow (`research/creature/archive/main/notes/luther-mvp-attempt-1/overview.md:14-45`).
The postmortem's own diagnosis: the boundary was broken in stubs and tests
before implementation, was never mechanically enforced, and phase gates
optimized behavior rather than architecture (`:59-77`).

**Rust reset (`8c7e22f`, 2026-04-13).** One commit, 202 files, +31,649 lines.
Re-promised the strict engine/domain boundary
(`project-plans/initial/overview.md:74-81,215-229`). All phases marked complete
with 61 tests — again, before a product slice existed.

**First real workflow (`1ebd4a8`–`438ec2d`).** Stated target: a real issue
through plan→implement→verify→PR, unattended, "not a demo"
(`project-plans/llxprt-first/overview.md:3-7`). Called engine ignorance of
GitHub/PR/LLxprt "inviolable" (`:11-36`). The final smoke verification phase was
marked **PASS with `0 passed; 0 failed; 2 ignored`**
(`plan/.completed/P21A.md` at `438ec2d`). The acceptance test never ran.

**Breadth before proof (June–July).** Daemon, continuation, parent
orchestration, OCR, scope control, recovery. Individually large:
`b50b808` +86,543; `d38c9f9` +13,379; `0c06995` +30,090. Sophisticated
infrastructure atop an unproven pre-PR path.

**The meta-error recurs in three forms:**
1. Domain code lives in the generic engine — GitHub PR, feedback, remediation,
   parent orchestration, scope control, merge wait, all declared in
   `src/engine/executors/mod.rs:18-40,56-110,113-175`. **This is Attempt 1's
   defining violation, repeated after being documented.**
2. Phase/test completion substitutes for a live product slice — ignored smoke,
   then synthetic qualification.
3. Requirements land in whatever layer is convenient — first file layout and
   tests, later TOML prompts — with no mechanical boundary.

---

## 4. Measured record (corrected)

The issue-138 campaign: **28 fresh runs (v3–v30) plus 3 refused continuations.**
My earlier "31 runs" conflated these; the continuations died at line 1–2 with
"step is not recoverable" and executed nothing.

Last substantive step reached, 28 fresh runs:

| Step | Runs |
|---|---|
| `run_tests` | **17 (61%)** |
| `remediate` | 5 |
| `create_plan` | 3 |
| `implement` | 2 |
| `scope_measure` | 1 |

**22 of 28 (79%) died in verify/repair.** No log contains
`Executing step: create_pr`. Zero PRs.

Canonical case — **v18**: the OCR review *completed successfully* (21 completed,
0 failed, 25 comments, correct range). The run was abandoned anyway because the
parser died on a human-formatted table. The workflow could not distinguish
*"no evidence"* from *"negative evidence."*

**Corrections to my earlier numbers:** prompt payload is 33,333 *characters*
(33,337 bytes), not bytes. The "37 lettered fixes A–AK" and peak sizes
(implement 23,244) are **operator diary, not reconstructable from Git** — the
pre-PR-196 parent has implement at 5,991 chars. Treat as unverified chronology.

---

## 5. The QUALIFIED contradiction — the most important finding

`project-plans/self-hosting-reliability/` reports **39 phases complete, Plan
Status QUALIFIED, three consecutive nine-stage canaries, zero violations.**
Simultaneously: 0 PRs in 28 real runs.

Both are true. **The canary does not run Luther.**

From `tests/canary_harness_tests.rs`:
- Never loads `llxprt-luther-dogfood-v1.toml` or runs `EngineRunner`;
  `run_canary` hand-calls nine helpers and appends each stage label
  (`:986-1030`). The final assertion proves the test called its own helpers in
  order.
- Stage 2 creates "the work" by **directly writing a file** (`:635-653`). No
  issue, plan, LLM, or implement step.
- `CanaryExecutor` unconditionally returns a completed snapshot and
  `{"status":"success"}` (`:238-289`).
- Stage 6 supplies already-successful `head_sha`/`remote_ref_sha` (`:741-829`).
  No Git commit or push.
- Stage 7 **writes fabricated PR metadata straight into SQLite** (`:842-870`).
  No PR is created.
- Stage 9's probe returns `MergeObservation { merged: true }` (`:900-918`).

**The general principle:** a test that writes a postcondition, or injects the
success observation, can prove safety and idempotency *conditional on that
postcondition*. It cannot prove **reachability** of it. Integration depth is not
counted by how many production APIs are called or stages are named — it is
whether the harness preserves the causal boundary of the claim.

The defensible claim is: *"RecoveryProtocolV1 and typed merge are
component-qualified under deterministic injected observations."* Not
self-hosting viable. **3/3 and 0/28 are both expected.**

---

## 6. Root-cause classes (not a defect queue)

1. **Construct-validity / proxy optimization.** The dominant meta-cause. A gate
   easier to satisfy than the claim, then treated as evidence for it.
2. **Impoverished domain state masquerading as "typed."** `StepOutcome` has six
   *unparameterized* labels (`src/engine/transition.rs:11-40`); routing compares
   strings. It cannot express *what* failed, evidence identity or freshness,
   product vs infrastructure vs contract vs absence vs ambiguity, admissible
   repair, or whether progress occurred. A typed transport envelope is not
   typed domain semantics.
3. **Verify/repair as homogeneous retry, not convergence.** All repairable
   failures route to one general LLM step (`:1211-1232`). Issue #193: one
   formatting failure buried in a 185,322-byte report; two rounds never ran
   `cargo fmt`. **The archived pre-Rust design had DIAGNOSING/TRIAGING and
   routed INFRA/FLAKY separately from PR_RELATED** (`REQUIREMENTS.md:117-129`).
   The current graph *regressed* that classification layer.
4. **LLM self-certification drives control flow** — §2.
5. **External-contract / evidence-identity failures at seams.** Nearly all 33
   dogfood issues: argv interpolation (#169), invented tool contracts (#174),
   `/private/tmp` identity (#176/#179), swallowed errors (#177), `--repo`
   ignored (#182), `--json` ignored (#183/#186), session count vs identity
   (#195). Prompt rules cannot repair this class.
6. **Boundaries remain documentary** — §3.
7. **Duplicated/stale context across prompts** (#178, #187, #188). The config is
   a distributed program with no compiler enforcing shared invariants.
8. **Big-bang phase accounting hides missing vertical proof.**
9. **Recovery reliability optimized ahead of delivery reliability.** Perfect
   recovery of a state rarely reached.

**Causal chain:** weak product oracle → success on closed-world proxies →
seams escape tests → local patches, often in prompts → complexity and staleness
grow → more seam failures → more infrastructure that records failure safely
without improving completion.

---

## 7. Architecture verdict

**Not "correct architecture, buggy implementation."** A split verdict:

**Right:** static orchestration for the deterministic spine — claim, workspace
authorization, scope, commit, push, PR identity, waits, merge proof, budgets.
Capsules, effect intents, fail-closed defaults, typed merge are genuine assets.

**Wrong:** the flat verify→general-remediate cycle over six universal outcomes.
This is a structural error, not a bug.

**Correction to my earlier framing:** the spine did *not* "never fail" — #118
abandoned at `setup_workspace`, v10 stopped at `scope_measure`, #163–#171 are
spine defects. Do not use "the spine never failed" as evidence.

**Also wrong:** splitting `run_tests` into eight nodes. Eight typed nodes still
collapse into the same `fixable → remediate` edge. That was my D1, and it is
insufficient.

**Target:** keep the static outer graph; make CONVERGE a typed policy machine —
a `VerificationSet` of observations carrying check ID, argv/cwd, repo identity,
base/head/range, artifact digest, freshness, and classification
`Pass | ProductFailure | InfrastructureFailure | ContractFailure |
EvidenceUnavailable | Ambiguous`; plus `RepairState` with failure fingerprints
and progress detection; plus typed actions (`RecollectEvidence`,
`RetryTransient`, `RepairMechanical`, `RepairSemantic`, `ReplanScope`,
`WaitExternal`, `RequestDecision`, `Abandon`). The LLM is a callback emitting
schema-validated artifacts or patches — never the authoritative outcome.

**Do not** replace static policy with an LLM supervisor. The fix is richer typed
state and nested deterministic policy.

---

## 8. Direction

**Phase 0 — correct the claim and the oracle.** Rename the qualification to what
it measures. Define two product gates: **Gate A** (approved issue → draft PR)
and **Gate B** (draft PR → verified merge). Gate A is the blocker: 0/28 reached
PR. The harness must invoke the shipping binary and production config, from a
clean workspace, causing real subprocess and Git effects. A local bare remote
and contract-faithful fake GitHub are acceptable; **writing postconditions is
not.** Sample repeatedly — LLM steps are probabilistic.

**Phase 1 — restore the boundary mechanically.** Extract domain policy from
`engine/`. Add the forbidden-import lint Attempt 1 already specified
(`boundary-violations.md:189-206`) and a compile test proving the engine builds
without the domain package.

**Phase 2 — typed convergence protocol** (§7).

**Phase 3 — minimal v2 vertical slice.** Retire the 41-step v1 from the
qualification path. Small workflow, deterministic oracle, change *not* injected.

**Phase 4 — reintroduce capabilities only against product deltas.**

**Success criterion:** repeated unattended production-config runs from a real
issue to the declared terminal. Safe abandonment is a secondary metric, not a
substitute. **Evals must consume abandoned and paused traces** — a 0-completion
system starves an eval loop that only reads completions.

---

## 9. Stop / undo

**Stop:** calling the canary result self-hosting qualified; launching 41-step
dogfood runs as the design loop; adding deterministic rules to prompts; treating
LLM markers as authoritative outcomes; using phase/test/stage counts as product
evidence; expanding post-PR OCR/review/merge before Gate A (no run reached
`create_pr`, so **OCR suspension is not on the critical path** — my earlier D3
priority was wrong); burning down #163–#195 linearly; describing
`engine/executors` as generic.

**Undo:** reclassify the self-hosting plan as recovery/merge qualification;
remove v1 from the qualification path; replace flat six-outcome domain routing;
relocate domain executors out of `engine/`; delete prompt prose superseded by
typed code; retire "ignored smoke = PASS" semantics; make partial-trace evals
first-class.

**Do not undo:** fail-closed defaults, diagnostic persistence, capsules,
append-only attempts, effect intents, workspace authorization, typed merge
proof, contract fixtures, mutation testing.

> The minimum honest reset is not "rewrite Luther." It is: retract the
> overclaim, freeze breadth, extract domain policy from the engine, replace
> verify/repair's state algebra, and prove a small vertical slice before adding
> the preserved reliability machinery back to the qualification path.

---

## 10. Rules for agents working on Luther

**R1** Luther is **not** a port of `vybestack/llxprt-code` — it is an
independent composable workflow engine. The only directly ported artifact is the
OCR review script. But llxprt-code solves adjacent problems (CI review gating,
OCR integration), so check whether it has already solved *the specific
sub-problem* before building one. Cost of skipping: reinventing their merged
PR 2716 (reviewed-range completeness).
**R2** Never fix a defect by adding a prompt rule. Deterministic ⇒ typed step
with tests.
**R3** Pin external contracts with captured fixtures before parsing.
**R4** Run a control experiment before believing a fix. Fix X was wrong and I
validated it anyway from a directory that masked the bug.
**R5** Mutate, or the suite is decorative.
**R6** Audit artifact content; never trust a green step outcome.
**R7** Check whether a stale external pin already fixed the behavior.
**R8** The launch capsule is immutable — kill, release, rebuild, relaunch.
**R9** Two remediation cycles, then land and defer.
**R10** **A gate that injects its own postcondition proves nothing about
reachability.** Before believing any PASS, ask what was substituted for the hard
causal step.

---

## 11. Evidence gaps (do not overstate these)

- The A–AK fix chronology and peak prompt sizes are operator diary, not in Git.
- Issue #118's exact SQL/manual-Git mechanics are not in retained comments. The
  defensible claim: **all Luther attempts abandoned; PR #160 was administratively
  merged outside a successful run.**
- Only v30 artifacts survive; "OCR retrieval dominated all 17 `run_tests` stops"
  is supported for later runs, not recomputable for all.
- "Zero PRs" is verified for the 138 campaign, not all Luther history.
- That a convergence protocol will raise completion is an **inference**. It must
  be proven by Gate A, not by component tests — precisely the error this
  document exists to stop.
