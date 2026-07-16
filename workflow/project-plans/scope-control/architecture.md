# Issue 142 scope-control architecture

Plan ID: `PLAN-20260715-SCOPE-CONTROL`

## Delivery boundary

The machine-readable delivery charter is `task-charter.json` in this directory. It is the authority for this implementation's allowed paths, numeric change budget, non-goals, mandatory gates, and review limits. Contextual reads do not authorize edits. Crossing a listed path or numeric boundary stops implementation until the charter is explicitly amended.

The implementation adds one narrow `scope_control` domain. It reuses Luther's existing `StepOutcome::Wait`, durable wait-state/checkpoint lifecycle, command manifest, agent executors, and post-PR iteration history. It does not redesign the generic PR-follow-up artifact store.

## Runtime contract

A canonical task charter is created before a mutating implementation step. The charter binds:

- acceptance-criterion IDs;
- normalized repository-relative path prefixes grouped by subsystem;
- explicit non-goals;
- file, added-line, new-module, dependency, and public-API ceilings;
- mandatory command-manifest and required-PR gates;
- initial, delta, final-acceptance, and remediation limits;
- an immutable merge-base and measurement policy.

Scope-control artifacts live below the run's artifact root in `scope-control/<run-id>/`. Fixed artifact families and atomic temp-file/rename writes are used instead of extending PR-follow-up artifact identities.

## Deterministic patch measurement

Measurement is always relative to the charter's frozen merge-base. The production probe uses NUL-delimited Git output, disables rename inference, enumerates untracked files explicitly, sorts repository-relative paths, and defines binary and unterminated-line handling. It records:

- changed files and textual additions/deletions;
- added source modules;
- touched configured subsystems;
- additions to configured dependency sections;
- added public Rust API lines under the frozen policy;
- growth from the prior distinct snapshot;
- stable divergence reason codes.

A replay of an identical snapshot is idempotent and does not consume a round.

## Scope decision state machine

A scope guard runs after every mutation and immediately before every push. Under-budget snapshots proceed. Divergent snapshots atomically write a request bound to run, charter, snapshot, and sorted reasons, then return `StepOutcome::Wait` without a transition.

The wait kind is `scope_decision`. A resolution is immutable and request-bound:

1. `approve_expanded_scope` approves only that exact snapshot and its reasons;
2. `split_follow_up_issue` persists follow-up work and permits only scope reduction;
3. `return_to_minimal_implementation` permits only scope reduction.

Mutation preflight exists at both broad implementation entry points. Pending or frozen state blocks broad implementation and remediation even after restart or forced continuation. Only a matching reduction decision can enable a scope-reduction mutation. A new growth snapshot requires a new decision.

## Two-axis finding disposition

Every new evaluated finding has independent fields:

- correctness: `blocker`, `high`, `medium`, `low`, or `invalid`;
- delivery scope: `required_acceptance_criterion`, `regression_from_current_patch`, `small_adjacent_fix`, `follow_up_issue`, or `user_decision`.

The planning matrix is deterministic. Invalid findings are never remediated. Required criteria and current-patch regressions are current-delivery work. A small adjacent fix is current work only when scope preflight proves it fits the remaining charter. Follow-up findings are durably recorded and do not fail the delivery. User decisions block. Correctness severity alone cannot silently enlarge delivery scope.

Historical single-axis artifacts remain readable through a labeled compatibility projection, while new evaluator responses require both axes.

## Bounded review phases

The existing post-PR iteration guard becomes the authoritative review-round state:

1. exactly one initial full review over merge-base to head;
2. after a head-changing remediation, a delta review over the prior reviewed head to the current head;
3. exactly one read-only final acceptance review over merge-base to head against the charter.

Same-head replay consumes no round. Contract-validation retries remain separate from code-changing remediation rounds. Cap exhaustion writes a durable summary and terminates without launching another unrestricted agent. Final acceptance may block completion but cannot reopen a broad remediation loop.

Each review scope records review kind, merge-base, from/to SHAs, changed files, changed tests, contextual files, and charter digest. Contextual files may explain an invariant but are not silently treated as changed scope.

## Timeout recovery

Before mutation, Luther records a patch snapshot. When timeout or idle-timeout leaves a different snapshot, Luther:

1. freezes mutation;
2. records pre/post snapshot and process evidence;
3. runs only the configured targeted compile/check gate;
4. maps every partial path to a selected subsystem and acceptance criterion;
5. creates a scope-decision request.

No second broad agent starts until the partial snapshot is approved exactly or reduced to the charter.

## Status projection

A bounded `status.json` read model exposes current totals and growth, touched subsystems, dependency/API additions, divergence, freeze/pending decision, review phase, and remaining rounds. `status --json`, `runs show --json`, and human output read this artifact. Missing scope status for historical runs remains compatible; corrupt status is reported rather than interpreted as compliant.

## Implementation slices

1. Charter/config schema, validation, canonical persistence, and tests.
2. Deterministic patch probe, growth calculation, scope guard, and tests.
3. Durable decision wait, mutation barrier, resolution command, and recovery tests.
4. Two-axis finding schema, compatibility projection, disposition matrix, and tests.
5. Review phase/cap state, explicit review ranges, final acceptance, and tests.
6. Timeout freeze/recovery and status projection.
7. Workflow topology, OCR integration, fixture parity, and end-to-end tests.

Each slice must remain within `task-charter.json`, preserve mandatory gates, and pass focused tests before the next slice.
