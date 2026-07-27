# Convergence classification rubric

**Status: pre-registered.** Committed before any case was examined, so the
categories cannot be shaped to fit the answer. Changing it after
classification begins requires a separate commit stating what changed and why.

## What is being tested

The parent plan proposes replacing the flat verify → general-remediate cycle
with a typed convergence protocol. The claim under test:

> Given a typed `VerificationSet` and a typed action set, the engine would have
> routed these failures correctly, where the flat cycle did not.

This is an inference. This study attempts to falsify it against real failures.

**A negative result is a valid outcome.** If the proposed fields would not have
routed these cases, the protocol is not justified by this evidence and the
parent plan must change.

## Corpus

Ten documented failures from the issue-138 campaign: #169, #174, #176, #177,
#179, #182, #183, #186, #193, #195. Selected before classification, by the
criterion "filed as a defect during the campaign and describes a verify/repair
failure." No case may be added or dropped after classification begins.

## Rater protocol

1. Two raters classify independently. Rater A is the session agent. Rater B is
   a separate reviewer given the rubric and corpus **without** rater A's labels
   and without this study's framing.
2. Neither rater sees the other's labels before both are recorded.
3. Labels are committed before adjudication.
4. Disagreements are enumerated case by case, never silently reconciled.
5. Agreement is reported as a number: exact-match percentage per field.
6. Adjudication: a disagreement is resolved only by citing the issue text or
   retained artifact. If neither settles it, the case is recorded as
   **Unclassifiable**, not split or averaged.

## Field 1 — Observation class

What the verification step actually observed. Exactly one:

| Class | Definition |
|---|---|
| `ProductFailure` | The product under test is genuinely wrong. |
| `InfrastructureFailure` | Environment, network, or tooling failed. Product unknown. |
| `ContractFailure` | An external tool behaved differently than assumed. |
| `EvidenceUnavailable` | The check could not produce a usable observation. |
| `Ambiguous` | The observation cannot be assigned from retained evidence. |

## Field 2 — Evidence deficiency

What was missing at the decision point. Exactly one:

| Deficiency | Definition |
|---|---|
| `None` | Sufficient evidence was present and correctly interpreted. |
| `NotCaptured` | Required evidence was never collected. |
| `CapturedNotRouted` | Evidence existed but did not reach the deciding step. |
| `MisidentifiedSubject` | Evidence was real but described the wrong subject. |
| `ErrorErasure` | A failure was converted into a success-shaped value. |

## Field 3 — Correct typed action

Which action the protocol should have selected:

`RecollectEvidence`, `RetryTransient`, `RepairMechanical`, `RepairSemantic`,
`ReplanScope`, `WaitExternal`, `RequestDecision`, `Abandon`.

## Field 4 — Sufficiency (the actual test)

Would the proposed typed fields have been **sufficient** to select the correct
action automatically, without a human reading the issue?

- `Sufficient` — the fields determine the action.
- `InsufficientNeedsField` — determinable, but requires a field the proposal
  does not include. **The missing field must be named.**
- `InsufficientNeedsJudgment` — no field set determines it; requires judgment.
- `Unclassifiable` — retained evidence does not support any answer.

**A case is "classifiable" if and only if fields 1–3 can each be assigned by
citing specific issue text or a retained artifact.** Plausible inference from
general knowledge is not sufficient. This definition is fixed here so it cannot
be adjusted case by case to satisfy the stop condition.

## Stop condition

Pre-registered, from issue #201:

> If more than **20%** of cases (3 or more of 10) are `Unclassifiable`, the
> recommendation is to build evidence capture first and defer policy design.

This is evaluated mechanically from the recorded labels.

## Verdict criteria

Fixed in advance:

- **Supported** — ≥ 70% `Sufficient`, and no systematic gap.
- **Undetermined** — stop condition triggers, or raters disagree on > 30% of
  field-4 labels.
- **Unsupported** — < 50% `Sufficient`, or a systematic pattern shows the
  proposed fields miss the actual cause.

Between 50% and 70% with no systematic gap is reported as **Weakly supported**,
with the qualification stated plainly.
