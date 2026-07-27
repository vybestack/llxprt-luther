# Convergence retrospective: is the hypothesis supported?

**Verdict: UNSUPPORTED as proposed.** 30% of cases were `Sufficient` under both
raters, against a pre-registered threshold of 50% for "unsupported" and 70% for
"supported."

The proposed protocol should not be built in its current form. The evidence
points somewhere specific instead, and the redirection is the useful output of
this study.

Method: rubric pre-registered at `68ae8b4`, rater A labels at `e60760d`, rater
B labels at `a80d5de`, in that order. Rater B is a different model profile
(`opusthinking` vs `gpt56solhigh`), given the rubric and corpus without rater
A's labels, without the convergence framing, and without any indication of an
expected distribution.

## Result against pre-registered thresholds

| Criterion | Threshold | Observed | Outcome |
|---|---|---|---|
| Unclassifiable | > 20% → stop | **0%** | not triggered |
| Field-4 rater disagreement | > 30% → undetermined | **30%** | not triggered (at the boundary) |
| `Sufficient` under both raters | < 50% → unsupported | **30%** | **UNSUPPORTED** |

Both raters independently found every case classifiable from retained
evidence. **Evidence capture is not the bottleneck**, which contradicts the
hypothesis this study was prepared to confirm.

## Inter-rater agreement

| Field | Agreement |
|---|---|
| Observation class | 80% |
| Evidence deficiency | 50% |
| **Correct action** | **40%** |
| Sufficiency | 70% |

### The 40% action agreement is the central finding

Two capable raters, each with the **full issue text** — root-cause analysis,
controlled experiments, verified fixes — agreed on the correct action in only 4
of 10 cases.

A policy engine would hold strictly less information than either rater had. If
the correct action is not reliably derivable from a complete written
post-mortem, it is not derivable from a typed field set that is a lossy summary
of the same situation.

This is not a labelling problem to be tightened with a better rubric. The
disagreements are substantive:

- **#169** — rater A: `RequestDecision` (a config defect misreported as a
  project failure needs a human); rater B: `RepairMechanical` (interpolate
  argv). Both defensible. The action depends on whether you privilege *who
  should decide* or *what the code change is*.
- **#195** — rater A: `RecollectEvidence`; rater B: `RepairSemantic`. Same
  disagreement in different clothing: re-query with correct selection, or
  change the gate's selection rule.
- **#176, #179, #183** — rater A chose `RecollectEvidence`, rater B chose
  `RepairMechanical`. Retrieving evidence correctly *is* the repair in these
  cases, so the two actions are not cleanly separable.

That last pattern matters most: for path-keyed and format-mismatch failures,
`RecollectEvidence` and `RepairMechanical` are **not distinct actions**. A
protocol whose action set cannot separate them will route inconsistently
regardless of implementation quality.

## What both raters found instead

Rater B, without prompting and without seeing rater A's notes, named the same
gap rater A did, and stated it more sharply:

> The held field set describes **what the caller sent, never what the callee
> honored**.

Seven of rater B's ten cases were `InsufficientNeedsField`, and the named
fields collapse into one concept: **the external tool's effective contract per
subcommand** — which input keys its state, which flags it honors, what format
it emits, which artifact is authoritative.

The corpus supports this directly:

| Case | What was assumed | What was true |
|---|---|---|
| #174 | prints `Excluded (2):` | prints `Excluded from review (2):` |
| #176 | keys on the passed path | keys on the canonical realpath |
| #179 | keys on the build workspace root | keys on the git root |
| #182 | honors `--repo` | ignores it; uses process cwd |
| #183, #186 | honors `--json` | accepts it, prints a human table |
| #195 | one session per run | auxiliary invocations create empty sessions |

Six of ten failures are the same defect wearing different clothes. None is a
routing failure. In every one, the review **succeeded** and the gate reported
failure because it could not retrieve or interpret the result.

#186 is the sharpest instance. That exact `--json` behavior was already known
and written down — **as a prompt rule** — and did not survive a rewrite. The
project's documented anti-pattern reproduced itself inside the corpus being
studied.

## Consequences for the parent plan

**Do not build the typed convergence protocol as specified.** It is a routing
mechanism, and these are not routing failures. Building it would add a policy
layer above evidence that is already wrong, making incorrect decisions faster
and with more ceremony.

**Build the tool-contract layer instead.** A `ToolContract` pinned to a tool
version, carrying a captured real output fixture, the state-keying convention,
and the authoritative artifact source, with a digest that invalidates on
version change. That addresses six of ten cases directly, and it is what both
raters independently asked for.

**Revisit convergence afterward, on new evidence.** Three cases (#169, #177,
#193) were `Sufficient` under both raters — routing genuinely would have helped
there. That is a real but narrow benefit, and 3 of 10 does not justify
replacing the verify/repair cycle. If the contract layer lands and failures
persist that are genuinely about *which repair to attempt*, the question
deserves re-asking against that evidence rather than this.

**Do not raise the action-set granularity to force agreement.** The 40% figure
is a measurement of how underdetermined these decisions are, not a defect in
the rubric. A finer action set would move the disagreement rather than resolve
it.

## Threats to validity

- **n = 10**, all from one campaign against one repository. The contract-defect
  concentration may reflect that this project's verification leans heavily on
  one external tool rather than a general property of verify/repair.
- **Rater A is the author of the parent plan**, and had incentive to find the
  hypothesis supported. Rater A's labels were the *more* favourable of the two
  (6/10 vs 3/10 `Sufficient`) — consistent with that bias, and a reason to
  weight the conjunction rather than either rater alone. The verdict uses the
  conjunction.
- **Field-4 disagreement landed exactly at 30%**, the boundary of the
  "undetermined" rule. Read as not triggered per the pre-registered wording
  ("> 30%"). Had one more case diverged, the verdict would have been
  Undetermined rather than Unsupported. This is disclosed rather than smoothed:
  the result is near a threshold, and a larger corpus could move it.
- **Both raters are language models.** Their agreement about a missing concept
  is evidence, not proof.

## Answer to the question in the issue

> Would the proposed fields have been sufficient to route correctly?

**No, for 7 of 10 cases.** Not because the fields are poorly chosen, but
because they describe the caller's intent and these failures live in the gap
between intent and what the tool actually did.
