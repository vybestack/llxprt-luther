# Convergence classification — Rater B

Independent rater: `architect` subagent, model profile `opusthinking` —
a different model from rater A. Given the rubric fields and the corpus with no
access to rater A's labels (committed at `e60760d`), no reference to the
convergence proposal, and no indication of an expected distribution.

| Case | Observation | Deficiency | Action | Sufficiency |
|---|---|---|---|---|
| #169 | EvidenceUnavailable | MisidentifiedSubject | RepairMechanical | Sufficient |
| #174 | ContractFailure | NotCaptured | RepairMechanical | InsufficientNeedsField |
| #176 | EvidenceUnavailable | CapturedNotRouted | RepairMechanical | InsufficientNeedsField |
| #177 | EvidenceUnavailable | ErrorErasure | RetryTransient | Sufficient |
| #179 | EvidenceUnavailable | CapturedNotRouted | RepairMechanical | InsufficientNeedsField |
| #182 | ContractFailure | MisidentifiedSubject | RepairMechanical | InsufficientNeedsField |
| #183 | ContractFailure | CapturedNotRouted | RepairMechanical | InsufficientNeedsField |
| #186 | ContractFailure | CapturedNotRouted | RepairSemantic | InsufficientNeedsField |
| #193 | ProductFailure | CapturedNotRouted | RepairMechanical | Sufficient |
| #195 | ContractFailure | MisidentifiedSubject | RepairSemantic | InsufficientNeedsField |

Unclassifiable: **0 of 10**.

## Missing fields named

1. **#174** — installed tool version identity plus a verbatim captured output
   sample for that version, so a fixture can be proven to match reality.
2. **#176** — the path form under which the tool keys stored state (canonical
   realpath vs the as-passed argument).
3. **#179** — the root the tool keys its state directory on (git repository
   root), recorded as distinct from the build workspace root.
4. **#182** — per-subcommand record of which input actually controls the
   lookup (flag honored vs process working directory).
5. **#183** — the actual observed output format, as opposed to the format
   requested by flag.
6. **#186** — the authoritative durable evidence source, declared per
   subcommand.
7. **#195** — the artifact identifier reported by the invocation itself,
   linking the artifact to the check subject.

Rater B's own synthesis, unprompted:

> Fields 2-6 are variants of one absent typed concept — the external tool's
> effective input/output contract per subcommand. The held field set describes
> **what the caller sent, never what the callee honored**.
