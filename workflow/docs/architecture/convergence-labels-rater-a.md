# Convergence classification — Rater A

Recorded against `convergence-rubric.md` (`68ae8b4`), committed before Rater B
was engaged and before Rater B's labels were visible.

| Case | Observation | Deficiency | Correct action | Sufficiency |
|---|---|---|---|---|
| #169 | InfrastructureFailure | MisidentifiedSubject | RequestDecision | Sufficient |
| #174 | ContractFailure | NotCaptured | RecollectEvidence | InsufficientNeedsField |
| #176 | EvidenceUnavailable | MisidentifiedSubject | RecollectEvidence | Sufficient |
| #177 | EvidenceUnavailable | ErrorErasure | RetryTransient | Sufficient |
| #179 | EvidenceUnavailable | MisidentifiedSubject | RecollectEvidence | InsufficientNeedsField |
| #182 | ContractFailure | NotCaptured | RepairMechanical | Sufficient |
| #183 | ContractFailure | NotCaptured | RepairSemantic | InsufficientNeedsField |
| #186 | ContractFailure | NotCaptured | RepairSemantic | InsufficientNeedsField |
| #193 | ProductFailure | CapturedNotRouted | RepairMechanical | Sufficient |
| #195 | InfrastructureFailure | MisidentifiedSubject | RecollectEvidence | Sufficient |

## Citations

- **#169** — `command_manifest.rs:473` copies argv verbatim; the group id is
  interpolated at `:442`. The literal `{task_charter_merge_base}` reached
  `git rev-parse`. Implementation had already passed clippy, tests, build,
  format, check.
- **#174** — Parser expected `Excluded (2):`; OCR 1.7.13 prints
  `Excluded from review (2):`. Fixtures encoded the same guess as the parser.
- **#176** — Session existed (7.3 MB, matching id) under the canonical slug;
  adapter queried the unresolved `/tmp` form. `/tmp` → `/private/tmp` on macOS.
- **#177** — Retained artifact was literally the four bytes `null`; three
  queries substituted empty on error. Artifact and session file share an
  mtime to the second: queried while the tool was still writing.
- **#179** — Tool keys by git root; wrapper derived the cargo workspace root.
  Both canonicalize correctly, so both look right on inspection.
- **#182** — Controlled experiment A/B/C: C fails with the flag set but a
  different cwd, so cwd controls the subcommand and `--repo` is inert.
- **#183** — `--json` accepted, exits 0, prints a human table. Decode fails at
  byte 1 and misdirects toward path/session hunting.
- **#186** — Same `--json` defect recurring after a rewrite. The prior
  knowledge existed **as a prompt rule** and did not survive.
- **#193** — One check of eight failed (`format`). `cargo fmt` appears zero
  times in the entire remediation transcript. Report 185,322 bytes vs
  evaluation 6,836 bytes.
- **#195** — Three sessions: two empty (0 bytes, 0 items), one real
  (8,705,787 bytes, 74 items). Gate failed on the count.

## Named missing field

Four cases (#174, #183, #186, and #179 in its tool-keying aspect) are
`InsufficientNeedsField` for the **same** reason. The proposed `VerificationSet`
carries argv, cwd, repo identity, range, digest, and freshness, but nothing
pinning **what the external tool actually emits and how it keys its state**.

Proposed field: `tool_contract` — captured real output fixture plus the keying
convention, pinned to the tool version, with a digest that invalidates on
version change.

Without it, a policy engine can route these failures but cannot detect them:
every local signal is green and disagreement appears only against reality.
