# B8: Component-owned typed output ports — migration design

> **Status: design only, not an accepted plan.** The split this proposes
> (B8a/B8b/B8c) is raised on #226 and not yet decided. Committed so the
> measurements are reviewable rather than living in a chat log.
>
> Two corrections to what was reported alongside it. `generic/shell.rs` does
> not import `Fixable` from `software_change`: it defines its own
> `parse_outcome_name` (shell.rs:560, llxprt.rs:969). That is duplication, not
> a package-boundary violation, and the two copies disagree — unknown names
> become `Success` in one and `Fatal` in the other, and one lowercases while
> the other does not. Filed as #276. The design's claim that outcome strings
> are read back is correct: `tests/smoke_replay_tests.rs:43` parses them to
> replay routing, so they are not write-only telemetry.


Design for issue #226. Every claim below was checked against the source at
`e74e623` on `main`. Claims I could not verify are marked **unverified**.

---

## 0. Verification of the stated premises

Correcting the brief before designing to it. Two of its premises are wrong in
ways that change the design.

| Claim in the brief | Result | Evidence |
| --- | --- | --- |
| 294 `StepOutcome::` in src/ | **Confirmed** | `grep -rn "StepOutcome::" src/ \| wc -l` = 294 |
| across 53 files | **Corrected: 47** | 47 files contain `StepOutcome::`; 53 contain the bare token `StepOutcome` |
| 476 more in tests/ | **Confirmed** | 476, across 22 of 73 test files |
| 43 `StepOutcome::Fixable` in src/ | **Confirmed** | exactly 43 |
| 34 files under config/ + tests/fixtures/ contain "fixable" | **Confirmed** | `grep -rl fixable config/ tests/fixtures/ \| wc -l` = 34 |
| `StepOutcome` is persisted | **Confirmed** | `append_event` (`src/persistence/checkpoint.rs:562-577`) passes `&outcome.to_string()` into the `events.outcome` TEXT column |
| **`StepOutcome` is a core type** | **FALSE — this is the most important correction** | see §0.1 |

### 0.1 `StepOutcome` is not in core, and core does not exist as the issue assumes

`luther-engine-core` is a real Cargo package, but its entire contents are
`sha256_hex` and `recovery_epoch` — **291 lines across two files**
(`crates/luther-engine-core/src/lib.rs` 136, `recovery_epoch.rs` 155).

`StepOutcome` lives in `src/engine/transition.rs`, which is part of the
**domain** package `luther-workflow`. `docs/architecture/package-boundaries.md`
says so explicitly, under a heading titled *Known accepted violation*:

> `StepOutcome::Fixable` is documented as "the issue is fixable by remediation"
> — domain semantics on a core type. It is deliberately left alone: correcting
> it requires the output-port design from B8 [...] **`StepOutcome` has not moved
> into core, so the forbidden-vocabulary test does not yet apply to it.**

This matters concretely:

- Acceptance criterion 1 ("grep for remediation/repair/issue/fix semantics in
  core outcome types returns nothing") is **satisfied vacuously today**, because
  there are no outcome types in core at all. Extending
  `core_source_contains_no_domain_vocabulary` changes nothing, because it scans
  `crates/luther-engine-core/src` and `transition.rs` is not there.
- Therefore B8 is not only a refactor of an existing core type. It must **create
  the core disposition type inside `luther-engine-core`** and then make the
  domain depend on it. Otherwise every gate in this issue passes while
  `transition.rs` is untouched — precisely the "cosmetic boundary" failure the
  issue exists to prevent.

`FORBIDDEN_IN_CORE` in `tests/package_boundary.rs:18-28` already contains
`"remediation"`, `"issue"`, and `"fix"`-adjacent vocabulary is *not* present —
`"fix"` is absent from the list. See §4.

### 0.2 The persisted string is display-only in production — but the smoke-replay harness parses it back

I traced every read of the `events.outcome` column.

**Production paths — display only:**

- `EventRecord.outcome` is `String` (`checkpoint.rs:783`).
- Readers: `load_events` → `persistence/trace.rs:152-159` → rendered by
  `app/runs/inspect.rs:195,339` and `monitor/snapshot.rs:328`.
- `FromStr for StepOutcome` exists (`transition.rs:64-78`) but its **only**
  caller in the repository is its own unit test (`transition.rs:248`).
- No SQL anywhere filters or joins on the `outcome` column value (`WHERE
  outcome`, `outcome =`, `outcome IN` return no hits against the events table).
- `RunMetadata.previous_outcome` is likewise `Option<String>` read only for
  display (`monitor/snapshot.rs:303`, `app/status.rs:272,335`,
  `app/runs/inspect.rs:260`).
- Nothing outside the Rust tree reads it: `grep -rn "events" .github/scripts/`
  returns nothing. **Resolved** (was an open question in an earlier draft).

**The exception, which I initially got wrong and am correcting here:**

`EngineRunner::export_trace` (`runner.rs:665`) serialises the events table —
including the outcome strings — to a `SmokeTrace` JSON file
(`persistence/trace.rs:144-170`). `tests/smoke_replay_tests.rs:43-54` then reads
those fixtures back and **parses the strings into `StepOutcome` to replay
routing**, with its own third `parse_outcome` that handles
`success|retryable|fatal|fixable|abandon` and returns `EngineError::InvalidState`
on anything else.

So there is a **round trip**: persisted string → JSON fixture → parsed outcome →
routing. It is confined to the smoke-replay test harness, but it is real, and it
is a third divergent parser alongside the two in §0.3c.

**What this changes:**

- The claim "no data migration is required" **still holds**, because the port
  strings do not change (§2.1). Old rows, old fixtures, and new writes all carry
  identical bytes.
- The claim "nothing parses it back" is **false** and the design must account for
  it. `tests/fixtures/smoke-traces/*.json` currently contain only `success` (38)
  and `fatal` (2) — no `fixable`, no `abandon` — so no fixture regenerates. But
  `smoke_replay_tests.rs::parse_outcome` must be migrated to `PortName::new`
  along with the two production parsers (PR 4), and it is the one parser that
  already fails fast, so it is the model the other two should follow.

This is the single most load-bearing fact in the migration and it is why §2.3
recommends deleting `FromStr` rather than merely noting it is unused.

### 0.3 Facts the brief did not mention that change the design

**(a) `Retryable` does not retry. The engine has no retry loop.**

`EngineRunner.max_retries` is carried in the struct at `runner.rs:114` under an
explicit `#[allow(dead_code)]` with the comment "Retained for the configured
retry policy while retry transitions are expanded." It is written by
`construction.rs:82,147,216` and **never read**. `EngineError::RetryLimitExceeded`
(`engine/error.rs:33-34`) is **never constructed anywhere in the repository**.

So the doc comment on `StepOutcome::Retryable` — "The engine should retry the
step up to max_retries" — describes behaviour that does not exist. `Retryable`
is currently just a routing label that, absent a transition, produces
`RunOutcome::Failure` (`failure_cleanup.rs:655-662`, `support.rs:258`).

**(b) `Fixable` is the default outcome of the *generic* shell component.**

`src/components/generic/shell.rs:355-366`: `mapped_nonzero_outcome` returns
`StepOutcome::Fixable` for any non-zero exit with no `exit_code_map` entry. The
remediation concept has already leaked out of software-change into the generic
component bundle. Any design that only moves `Fixable` out of
`software_change` misses this.

**(c) The two `parse_outcome_name` implementations disagree, and both swallow
errors.**

| | unknown name → | lowercases input |
| --- | --- | --- |
| `software_change/llxprt.rs:969-977` | `Fatal` | no |
| `generic/shell.rs:560-569` | `Success` | yes |

A typo in a workflow TOML — `"fixble"` — silently becomes `Fatal` in one
component and `Success` in the other. Neither handles `"wait"` at all, so
`condition = "wait"` in an `exit_code_map` is unreachable through either parser.
This is exactly the defensive-fallback pattern the constraints forbid, and the
port design removes it structurally (§1.4).

**(d) Config already contains four condition strings that are not
`StepOutcome` variants, and they are silently dead.**

`config/workflows/issue-fix-v1.toml` uses `condition = "approved" | "rejected" |
"passed" | "failed"` (lines 72, 77, 86, 91), mirrored in three fixture copies.
`resolve_transition` compares the condition against `as_condition_str()`, so none
of these four can ever match. They are dead edges that no validator rejects.
`grep` shows `issue-fix-v1` (the non-`llxprt-` one) is referenced by no Rust
source; the live workflows are `llxprt-issue-fix-v1`,
`llxprt-luther-dogfood-v1`, and `parent-issue-orchestrator-v1`.

This is direct evidence for acceptance criterion 6 (A1 must validate that every
transition targets a declared port): **the validation gap is already causing
real dead configuration today.** It also means adding that validation is a
breaking change for `issue-fix-v1.toml` — see §5, PR 6.

Condition-string census across `config/` and `tests/fixtures/`:

| condition | TOML | JSON |
| --- | --- | --- |
| `fatal` | 215 | 76 |
| `success` | 150 | 66 |
| `fixable` | 77 | 30 |
| `retryable` | 4 | 2 |
| `approved`/`rejected`/`passed`/`failed` | 3 each | 1 each |
| `abandon` | **0** | **0** |
| `wait` | **0** | **0** |

**No workflow file anywhere routes on `abandon` or `wait`.** Both are
engine-internal dispositions, not routing labels. That is decisive for §3.

---

## 1. Target design

### 1.1 The shape of the problem

Today one type does three unrelated jobs:

1. **Control disposition** — does the engine advance, pause, or stop? Read by
   `runner.rs:274,299` (`Abandon` ⇒ do not resolve a transition),
   `failure_cleanup.rs:652` (`Wait` ⇒ pause), `runner.rs:450,537` and
   `failure_cleanup.rs:63,402,634` (`Success` ⇒ completion/status).
2. **Routing selector** — the string matched against `TransitionDef.condition`
   in `resolve_transition{,_schema}` (`transition.rs:132-188`).
3. **Domain classification** — `Fixable` means "a software change can repair
   this"; that meaning exists only in the software-change domain.

Job 3 is the violation. Jobs 1 and 2 are legitimately universal, but they are
*different* and today they are welded together, which is why a domain concept
had to be added to a control enum to get routing.

**Split them.** Core owns job 1 and performs job 2 opaquely. Components own
job 3 and express it as a port name that core never interprets.

### 1.2 The core type (in `luther-engine-core`)

New file `crates/luther-engine-core/src/step_signal.rs`. Naming is deliberate:
`StepOutcome` stays in the domain crate as the compatibility surface during
migration (§2), so the core type needs a distinct name, and "signal" carries no
success/failure connotation.

```rust
// crates/luther-engine-core/src/step_signal.rs

/// What the orchestrator does next with the step that just ran.
///
/// These three cases are exhaustive over the orchestrator's own behaviour:
/// it either continues the graph, suspends the run so it can be resumed
/// later, or stops the run. Nothing here describes why a step produced a
/// given case; that is the port, which this crate does not interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// Continue the graph, selecting the edge whose label equals the port.
    Proceed,
    /// Suspend the run at this step with a resumable checkpoint. No edge is
    /// selected.
    Suspend,
    /// Stop the run at this step. No edge is selected.
    Halt,
}

/// The label a step emits so the graph can select an edge.
///
/// Opaque here by construction: this crate compares ports for equality and
/// renders them, and has no constructor that inspects the contents. There is
/// no `is_*` accessor, no `match` on the string, and no constant naming a
/// particular value. Adding one is what the enforcement in
/// `tests/core_port_opacity.rs` fails on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PortName(String);

/// The port name is not a free-form string: it is an identifier, checked once
/// at construction so every later comparison can be a plain equality test.
/// This is validation of external input (config files, component
/// declarations), not a defensive fallback: it returns an error, it does not
/// substitute a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPortName {
    pub offending: String,
}

impl PortName {
    /// # Errors
    /// Returns `InvalidPortName` when `raw` is empty, or contains anything
    /// other than ASCII lowercase letters, digits, and `_`.
    pub fn new(raw: impl Into<String>) -> Result<Self, InvalidPortName> {
        let raw = raw.into();
        let shaped = !raw.is_empty()
            && raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if shaped {
            Ok(Self(raw))
        } else {
            Err(InvalidPortName { offending: raw })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A step result: one disposition, and the port that selects the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepSignal {
    disposition: Disposition,
    port: PortName,
}
```

`StepSignal` carries a port in all three dispositions, not only `Proceed`.
`Suspend` and `Halt` select no edge, but the port is still the reason string
that reaches telemetry, and keeping the field unconditional means core has no
`match` on disposition to decide whether a port exists.

Accessors: `disposition() -> Disposition`, `port() -> &PortName`. Constructors:
`proceed(PortName)`, `suspend(PortName)`, `halt(PortName)`.

**Vocabulary compliance:** this file contains none of the nine forbidden words.
`"issue"` is the risk — it is in `FORBIDDEN_IN_CORE` and `contains_word` matches
it as a whole word. The comments above are written to avoid it (note "a step
produced a given case", not "the issue"). This must be checked when the file
lands; `core_source_contains_no_domain_vocabulary` will catch it.

### 1.3 The routing function (core)

```rust
/// Select the edge label-matched to `signal` from `edges`.
///
/// `Suspend` and `Halt` select nothing: those runs do not advance, so asking
/// for an edge is meaningless rather than merely unproductive.
///
/// The port is compared for equality and never inspected. This function is
/// the whole of core's routing, and it is why core can route two components
/// with disjoint port sets without knowing either.
pub fn select_edge<'a, E: Edge>(signal: &StepSignal, from: &str, edges: &'a [E])
    -> Option<&'a E>
{
    if signal.disposition() != Disposition::Proceed {
        return None;
    }
    edges.iter().find(|e| e.from() == from && e.label() == signal.port().as_str())
}
```

`Edge` is a small trait (`fn from(&self) -> &str; fn to(&self) -> &str; fn
label(&self) -> &str`) implemented in the domain by both
`engine::transition::TransitionDef` and `workflow::schema::TransitionDef`. That
trait is what lets the two near-duplicate functions `resolve_transition` and
`resolve_transition_schema` (`transition.rs:132-188`, currently 56 lines of
copy-paste differing only in the transition type) collapse into one. That
de-duplication is a genuine DRY win — same decision, two spellings — and it
shrinks `transition.rs`.

Note `label()`: the "no condition means success" default at
`transition.rs:148,179` moves to the `Edge` impl in the domain
(`self.condition.as_deref().unwrap_or("success")`), matching the existing
`effective_condition` helper at `workflow/validation.rs:76-78`. Core never sees
the word "success" — it sees a label string it does not interpret. **This is
load-bearing for acceptance criterion 4** and is the one place a reviewer should
look hardest, because putting the `unwrap_or("success")` in core would be a
branch on a port name.

### 1.4 Port declaration (domain side)

Extend the executor contract in `src/engine/executor.rs`:

```rust
pub trait StepExecutor: Send + Sync {
    /// The complete set of ports this executor can emit.
    ///
    /// Declared, not inferred. A1 validates workflow edges against this set,
    /// so an executor that emits a port it did not declare is a defect the
    /// validator is entitled to miss — which is why `execute` returning an
    /// undeclared port is a panic-worthy invariant break, not a routing
    /// fallback.
    fn declared_ports(&self) -> &'static [&'static str];

    fn execute(&self, context: &mut StepContext, params: &serde_json::Value)
        -> Result<StepSignal, EngineError>;
}
```

Each component declares its own set as a module constant, e.g. in
`src/components/software_change/`:

```rust
pub const PORT_FIXABLE: &str = "fixable";
pub const SOFTWARE_CHANGE_PORTS: &[&str] =
    &[PORT_SUCCESS, PORT_FIXABLE, PORT_FATAL, PORT_RETRYABLE];
```

`ExecutorRegistry` gains `ports_for(step_type: &str) -> Option<&'static [&'static str]>`,
which A1 consumes. The registry already exposes `registered_step_types()` and
`contains_step_type()` (`executor.rs:527-536`), so this is the same shape as
what is there.

`parse_outcome_name` in both components (§0.3c) is **deleted**, not fixed. Its
replacement is `PortName::new(name)` returning `Result`, with the error
propagated as `EngineError`. A workflow that names a port the component does not
declare fails at validation time (A1, before the run starts), and a config
string that is not a legal identifier fails at parse time. Neither has a
default-to-`Fatal` or default-to-`Success` path. That is the fail-fast structure
replacing the two divergent fallbacks.

### 1.5 Alternatives considered

**Alternative A — associated type per component (`trait Component { type Port:
PortEnum; }`).**

Each component defines its own `enum Port { Success, Fixable, ... }` with a
trait bound giving `as_str`/`all()`.

*Rejected.* Three reasons, none of them taste:

1. **It does not survive the registry.** `ExecutorRegistry` stores `Box<dyn
   StepExecutor>` (`executor.rs:502`) keyed by a runtime `step_type` string read
   from TOML. An associated type is not object-safe in the position needed —
   `execute` would have to return `Self::Port`, which cannot appear in a trait
   object's method signature. Recovering dispatch requires erasing the port back
   to a string at the registry boundary anyway, so the associated type buys
   compile-time checking that is discarded exactly where the routing happens.
   The type-erasure would be pure ceremony.
2. **The routing input is a string from a file.** `TransitionDef.condition` is
   `Option<String>` deserialized from TOML/JSON. No amount of type machinery on
   the emit side makes the *config* side typed; the check against declared ports
   is a runtime set-membership test either way. A1 is where that check belongs
   and it is equally strong against a `&'static [&'static str]`.
3. **Cost against the 1000-line file limit.** Per-component port enums plus
   their trait impls plus conversions add mechanical LoC to 47 files under a
   hard complexity gate, for checking that §1.4 already gets from a declared
   slice validated once at load.

Where the associated type *would* win — catching a typo in Rust source at
compile time — is covered: component code refers to `PORT_FIXABLE`, a `const`,
so a typo there is already a compile error.

**Alternative B — keep a flat enum, just rename `Fixable` to something
neutral** (e.g. `DomainA`, or a generic `Classified(u8)`).

*Rejected.* This is the cosmetic fix the issue explicitly targets. A core enum
with a fixed arity still forces every component into the same port cardinality,
and criterion 4 ("two components declare **disjoint** port sets") is
unsatisfiable: a fixed enum means the sets are necessarily equal. It also fails
the mutation test in §4 for the wrong reason — the check would pass because the
word left, while the structure that admits the word remained.

**Chosen: string-newtype `PortName` + declared `&'static [&'static str]` + a
three-case core `Disposition`.** It is the only option that (i) keeps
`Box<dyn StepExecutor>` dispatch working, (ii) makes disjoint port sets
expressible, (iii) puts the validation where the untyped input actually enters
(config load, A1), and (iv) adds no per-component type machinery to 47 files.

The cost, stated plainly: a port typo in a *TOML file* is caught at workflow
validation, not at `cargo build`. Given the input is a file, no design catches
it at `cargo build`; A1 is the earliest possible point, and it runs before any
step executes.

---

## 2. Persistence and backward compatibility

Requirement: *"existing workflows continue to route identically; transition
behavior fixtures unchanged."*

### 2.1 Routing identity is achieved by keeping the port names

The port strings stay exactly as they are: `"success"`, `"fixable"`, `"fatal"`,
`"retryable"`. `resolve_transition` today compares
`TransitionDef.condition == outcome.as_condition_str()`. After the change,
`select_edge` compares `edge.label() == signal.port().as_str()`. For every
existing workflow, both sides of that comparison hold the identical string.

Therefore **zero files under `config/` change**, and the 34 files containing
"fixable" are untouched. The 77+30 `condition = "fixable"` edges keep working
because `fixable` becomes a port declared by the software-change component
rather than a variant of a global enum. That is the whole point: same wire
string, different owner.

This is not a compatibility shim. It is the observation that the *string* was
never the problem — the problem is which module is entitled to *define* it.

Verification: the existing transition tests
(`transition.rs:252-276`, `workflow_graph_validation_tests.rs`, and the
`llxprt-*` workflow load tests) are not edited in the routing PRs. If a routing
PR needs to edit a behaviour fixture, that PR has changed behaviour and must be
rejected.

### 2.2 Database compatibility: nothing to migrate

After the change, the value written is `signal.port().as_str()` — for existing
components, **byte-identical** to what `outcome.to_string()` writes today. Old
rows and new rows carry the same strings.

This holds *including* the smoke-replay round trip of §0.2: the trace fixtures
under `tests/fixtures/smoke-traces/` contain only `success` and `fatal`, both of
which remain valid port names, so no fixture is regenerated and the replay
harness routes identically.

**No schema migration. No dual-read. No version column.** Proposing one would be
defence-in-depth against a hazard that does not exist — the bytes do not change.

### 2.3 The structural fix that replaces a guard

Because §0.2 established that a round trip *does* exist (via smoke-replay),
"nobody parses this" cannot be assumed; it must be made true where it should be
and explicit where it should not.

**Delete `impl FromStr for StepOutcome` and its test** (`transition.rs:64-78`,
`245-250`) in PR 1. It has no caller but its own test, and it is the generic
"turn any string back into a control decision" affordance. With it gone, an
author who wants to re-derive control flow from a persisted string must
consciously write that parser, and the reviewer sees it. This is a structural
fail-fast change, not a guard.

The one legitimate parse — smoke-replay — then goes through `PortName::new`,
which returns `Result` and has no default arm (PR 4).

I considered and **rejected** adding a `tests/persistence_outcome_is_telemetry.rs`
source-scan asserting "nothing parses the events column". §0.2 proves such a scan
would have to allowlist the replay harness, and §4's critique of grep-based
checks applies to it in full: it would test spelling, not structure. Deleting
`FromStr` achieves the same end structurally and cannot be bypassed by a synonym.

### 2.4 Stronger form (recommended, and cheap)

Make the telemetry column structurally unparseable back into a decision by
widening what is written. Persist the disposition and the port as **two
columns**, and give the port column no `FromStr` in either direction:

- `events.outcome` keeps receiving the port string (unchanged bytes → old rows
  remain comparable).
- add `events.disposition TEXT` via the existing idempotent `migrate_events_table`
  (`checkpoint.rs:544-555`), which already does `ALTER TABLE ... ADD COLUMN` and
  ignores duplicate-column errors.

`migrate_events_table` is the established mechanism in this file for exactly
this; using it adds no new pattern. New information is recorded; nothing that
exists changes meaning. **Unverified:** I did not check whether any external
tooling (dashboards, the OCR scripts under `.github/scripts/`) reads
`events.outcome`. Measurement that resolves it: `grep -rn "events" .github/scripts/`
plus a check of any operator SQL in `docs/`. If something external does read it,
§2.2 is unaffected (the column's bytes do not change) but that consumer should
be listed in the PR.

### 2.5 What about the four dead condition strings?

`approved`/`rejected`/`passed`/`failed` (§0.3d) do not route today and will not
route after. Under A1's new validation they become **errors** rather than silent
dead edges. `config/workflows/issue-fix-v1.toml` would fail to load.

Options, and the recommendation:

- **Recommended:** delete `config/workflows/issue-fix-v1.toml` and its three
  fixture copies in PR 6, as a separate commit with the census in the message.
  It is referenced by no Rust source and is superseded by
  `llxprt-issue-fix-v1.toml`. **Unverified:** whether any operator or script
  loads it by path outside the Rust tree; `grep -rn "issue-fix-v1"` over
  `.github/` and `docs/` resolves it.
- Rejected: make A1 warn instead of error on unknown ports. That reintroduces
  the "boundary that does not enforce" failure at the validation layer.
- Rejected: declare `approved`/`rejected`/`passed`/`failed` as ports of some
  component to keep the file loading. They correspond to no executor behaviour;
  declaring them would be inventing a contract to satisfy a test.

---

## 3. Which variants are genuinely universal — reviewer checklist

The test for universality used below: **can the meaning of this case be stated
using only the orchestrator's own vocabulary (steps, edges, runs, checkpoints),
without reference to what a step does?** Anything failing that is a port, not a
disposition.

### Core keeps three dispositions

| Case | One-line justification of universality | Code that proves core needs it |
| --- | --- | --- |
| `Proceed` | The run continues; an edge is selected by label. Meaningful for any graph of any steps. | `runner.rs:308+` advances via `resolve_next_step`; `transition.rs:132-188` selects the edge |
| `Suspend` | The run stops advancing but stays resumable; a checkpoint is written and no edge is selected. Refers only to run lifecycle. | `failure_cleanup.rs:652-654` → `pause_for_external_wait`; distinct persisted `waiting` status |
| `Halt` | The run stops and is not resumed; no edge is selected. Refers only to run lifecycle. | `runner.rs:274` (`Abandon` ⇒ `next_step = None`), `runner.rs:299-305` |

Three cases, and they are exhaustive over what `EngineRunner`'s loop can do with
a step result: continue, pause, stop. That exhaustiveness is the argument for
the arity — not a judgement that six felt like too many.

### The six current variants, each adjudicated

| Variant | Verdict | Justification from the code |
| --- | --- | --- |
| `Success` | **Port** (`"success"`), disposition `Proceed` | Its *routing* role is a label like any other. Its apparent universality is an artifact of one line: `transition.rs:148,179` treats a `None` condition as `success`. That default is a property of the **config format**, not of orchestration, and it moves to the domain's `Edge::label()` impl (§1.3). Core comparing a label to the literal `"success"` would be a branch on a port name and would fail criterion 4. |
| `Fatal` | **Port** (`"fatal"`), disposition `Proceed` when an edge exists, else `Halt` | 215 TOML edges route on `fatal`, mostly into `post_pr_failure_terminal`. It is used as a *routing label*, and the engine's own behaviour when it cannot route is already covered by `Halt`. Keeping a distinct core `Fatal` would give core two ways to say "stop". |
| `Fixable` | **Port** (`"fixable"`), owned by software-change | The originally identified violation. Documented as "the issue is fixable by remediation" (`transition.rs:25-29`); `"remediation"` and `"issue"` are both in `FORBIDDEN_IN_CORE`. **Also emitted by the generic shell component as the default for any non-zero exit** (`shell.rs:355-366`), so PR 4 must relocate that too, not just the software-change uses. **Shell must keep `fixable` as a declared port — see the edge inventory below; it cannot be defaulted to `fatal`.** |
| `Retryable` | **Port** (`"retryable"`) — *not* a core disposition. I argue against the brief's suspicion. | The brief suspects `Retryable` may be universal. **The code says it is not, on stronger grounds than domain-ladenness: it is not a disposition at all, because the engine does not retry.** `max_retries` is `#[allow(dead_code)]` at `runner.rs:112-116`; `EngineError::RetryLimitExceeded` is never constructed. `Retryable` is used exactly like `fatal`: 4 TOML edges route on it, and with no edge it becomes `RunOutcome::Failure` (`failure_cleanup.rs:655-662`). A core disposition must correspond to orchestrator behaviour; this one corresponds to a doc comment. *If* a retry loop is later built, `Retryable` still would not become a core disposition — a retry is "re-run this step", which is a fourth disposition (`Repeat`) that any component could request, and the *policy* of how many times is config. That is a separate issue, not this one. |
| `Abandon` | **Removed as a routing port; folded into the `Halt` disposition.** The brief's suspicion is correct. | Domain-laden as documented ("Used when loop limits are reached"), but the decisive fact is that **no shipped workflow file routes on `abandon`: 0 occurrences of `condition = "abandon"` in `config/` or `tests/fixtures/*.toml|json`.** Its only production role is engine-internal: `runner.rs:274,299` uses it to mean "do not resolve a transition, stop" — exactly `Halt`. The actual loop-limit enforcement at `runner.rs:505-515` does **not** produce `StepOutcome::Abandon`; it produces `RunOutcome::Abandoned` directly. So the variant's documented reason for existing is not how it is used. `validation.rs:301-307` explicitly *forbids* post-PR routes with `condition = "abandon"`. **Caveat, verified:** `tests/engine_execution_integration.rs:388-415` constructs an in-test `condition: Some("abandon")` transition and asserts `resolve_transition` routes on it. That test encodes the *hypothesis* the code never adopted (its own comment reads "At loop limit, Fixable should convert to Abandon outcome **OR** the transition table should route to abandon"). PR 4 must rewrite it against `Halt`, and that rewrite is a substantive review item, not a mechanical edit. Full inventory of affected tests in §5.2 PR 4. |
| `Wait` | **Core disposition `Suspend`** (with port `"wait"` for telemetry) | The one variant whose meaning is purely about run lifecycle: pause with a resumable checkpoint rather than terminate (`failure_cleanup.rs:652`, `pause_for_external_wait`). Its doc mentions "PR checks" as an *example*, which is prose to fix, not semantics to move. Like `abandon`, it has 0 `condition = "wait"` edges in config — consistent with it being a disposition rather than a routing label. |

#### Declared ports are unenforced at runtime

A1 validates workflow *edge labels* against `declared_ports()`, which catches a
graph routing on a port no executor declares. It does not catch the converse: a
component constructing any valid `PortName` and emitting a port it never
declared. Nothing checks the emitted value against the declaration, so
`declared_ports()` is documentation the runtime does not consult.

That gap makes the whole ports model advisory. **Required:** the runner or
registry must reject an undeclared emitted port before routing, as an error
rather than a fallback — consistent with the fail-fast choice made for unknown
outcome names, where the fix was to make the bad value unrepresentable at the
boundary rather than to pick a default.

#### Durable-state and failure paths are absent from the rollout gate

The verification plan covers routing and parsing and stops there. `Suspend`
changes run lifecycle, so the gate must also cover: an interrupted checkpoint
write during suspend; the resume boundary; stale ownership on resume; malformed
or truncated persisted records; and concurrent runners against one run.

This is the same weakness as the `abandon` decode gap — the plan reasoned about
config and in-memory routing and treated persisted state as though it followed
automatically. It does not, and B8 changes lifecycle semantics, which is
precisely where durable state breaks.

#### The renaming property must rename *effective* labels, not written ones

An omitted `condition` is not an absent label: `validation.rs:78` resolves it
with `condition.unwrap_or("success")`, so a transition with no condition
carries the effective label `"success"`.

The rename-invariance test as sketched above renames the labels *written in the
graph*. Applied to a workflow with implicit edges it would rename the explicit
`"success"` labels and leave the implicit ones resolving to the original
string, so routing would legitimately change and the test would fail on a
correct implementation — a false positive that would most likely be "fixed" by
weakening the test.

**Required:** the property must rename the *effective* label of every edge
(resolving `None` first), or the fixture must use explicit conditions
throughout. The former is the real property; the latter is a narrower test that
does not exercise the implicit path at all.

#### Persisted `abandon` must still decode

`append_event` writes `outcome.to_string()` into the events database, and the
replay parser at `tests/smoke_replay_tests.rs:43-51` has an explicit
`"abandon" => Ok(StepOutcome::Abandon)` arm. Removing the variant without
replacing that arm makes every persisted trace containing `abandon`
undecodable, which contradicts this document's "no data migration" claim.

The claim is defensible only for *config*: 0 shipped workflows route on
`abandon`, so no TOML changes. It is **not** defensible for *persisted state*,
which is a separate surface this document previously conflated with config.

**Required:** decode legacy `"abandon"` to the `Halt` disposition rather than
erroring, and add a test that a trace recorded before the migration still
replays. This is a decode-compatibility shim, not a schema migration, and it
cannot be dropped until the retention window for old traces has passed.

#### Edge inventory: `fixable` on generic shell steps

The claim that shell could default to `fatal` without routing impact is
**false**, and the counterexamples are in a shipped workflow. Inventorying
every `condition = "fixable"` edge against the `step_type` of its `from` step:

| Workflow | From step | `step_type` |
| --- | --- | --- |
| `llxprt-luther-dogfood-v1.toml` | `route_pr_path` | **`shell`** |
| `llxprt-luther-dogfood-v1.toml` | `plan_gate` | **`shell`** |
| `llxprt-issue-fix-v1.toml` | 12 edges | `llxprt`, `verify`, `github_pr_checks`, `pr_remediation_*`, `command_manifest_group` |
| `llxprt-luther-dogfood-v1.toml` | 11 further edges | as above |
| `parent-issue-orchestrator-v1.toml` | 4 edges | `parent_orchestration` |

Two live edges route `fixable` **from generic shell steps**. Defaulting shell's
non-zero exit to `fatal` would silently reroute both — `route_pr_path` and
`plan_gate` are decision points, so the change would not surface as an error,
it would take a different branch.

**Consequence for the design:** `fixable` must remain a port declared by the
generic shell component, not one relocated wholly into software-change. This
weakens the "clean ownership" story — `fixable` is declared by two components —
and that is the honest outcome: the port is genuinely shared, and pretending
otherwise would change shipped behaviour. Any future attempt to remove it from
shell must first migrate these two edges.

**Summary of the reduction:** 6 flat variants → 3 core dispositions + 4 ports
owned by components (`success`, `fatal`, `fixable`, `retryable`), with `abandon`
deleted as an unused synonym for `Halt`.

The uncomfortable result worth stating: **`success` and `fatal` are ports, not
core concepts.** A reviewer will resist this. The defence is criterion 4 — if
core may branch on `"success"`, it may branch on `"fixable"`, and the boundary
is again a matter of taste. The `None ⇒ "success"` config default is where that
knowledge legitimately lives, in the domain's config layer.

---

## 4. Making the semantic mutation test real

### 4.1 The obvious check, and why it is weak

Proposed: extend `FORBIDDEN_IN_CORE` with `remediation|repair|fix|issue` and
scan core's source (`core_source_contains_no_domain_vocabulary`,
`package_boundary.rs:303-327`).

Attacks:

1. **`"fix"` cannot go in the list as-is.** `contains_word` matches whole words
   with non-alphanumeric boundaries, and `_` counts as a boundary — so `"fix"`
   matches `fix_up`, and would also need to not match "prefix"/"suffix" (it
   would not, those have no boundary). But "fixed" is ordinary English —
   "the interval is fixed" — and `contains_word("fixed", "fix")` is **false**
   (`e` is alphanumeric), so `"fix"` is actually safe on that axis. `"fixable"`
   likewise does not match `"fix"`. To catch `Fixable` the list needs
   `"fixable"` itself. This is the fragility: the check catches the exact word
   someone already thought of.
2. **Trivially bypassed by synonym.** `Amendable`, `Correctable`,
   `Recoverable`, `Actionable`, `NeedsWork`, or `DomainRetry` all encode the same
   policy and match no wordlist. The strongest bypass is a *neutral-sounding*
   name: a variant `Classified` documented as "the component determined further
   work is possible" is domain policy in core with zero forbidden words.
3. **False positives push toward weakening the list.** The existing file already
   documents this failure mode at `package_boundary.rs:56-63` ("the response to
   a test that fails on innocent prose is to weaken the list"). Adding `"issue"`
   is the live risk: it is *already* in the list, and core is currently only 291
   lines. A new 120-line `step_signal.rs` must avoid the word "issue" entirely
   in its prose, which is a real authoring constraint (I wrote §1.2's comments to
   respect it).
4. **Bypassed by placement.** Put the domain variant in the *domain* crate and
   have core accept it generically — which is, ironically, the correct design.
   The wordlist cannot tell that apart from an evasion; only the structural check
   below can.

Conclusion: a vocabulary scan is necessary (it catches the careless case and it
already exists) but **is not the mutation test**. It tests spelling.

### 4.2 The strong form: constrain the *shape*, not the words

The property that actually matters is **arity and opacity**, and both are
checkable structurally.

**Check A — core's disposition type has exactly three variants, enumerated.**

```rust
#[test]
fn core_disposition_has_exactly_the_three_universal_cases() {
    let src = read_to_string(core_src().join("step_signal.rs"));
    let variants = enum_variants(&src, "pub enum Disposition {"); // brace-matched
    assert_eq!(variants, ["Proceed", "Suspend", "Halt"]);
}
```

This is the mutation test, and it is strong precisely because it is
**allowlist-shaped rather than denylist-shaped**. Adding *any* variant — named
`Fixable`, `Amendable`, `Classified`, or `Xyzzy` — fails it. There is no synonym
bypass, because the test does not read meaning. The only way to pass while
adding a case is to edit the assertion, which is a three-word diff in a file
called `core_port_opacity.rs` that a reviewer cannot miss and whose failure
message says why the list is fixed. That converts a silent architectural
regression into an explicit, reviewable decision — which is the realistic
maximum for any in-repo check.

The brace-matched enum-variant parser already exists in this repo
(`package_boundary.rs:390-435`, written for `EngineError`) and should be lifted
into a shared test helper rather than copied — it is the same decision in two
places.

Guard against vacuity, in the style the file already uses
(`the_metadata_scan_actually_finds_dependencies`): assert the parser returns a
non-empty list, so a renamed type fails rather than passing on an empty parse.

**Check B — the demonstrated failing run.** The acceptance criterion asks for a
demonstration in the PR. Concretely: add a fourth variant `Amendable`, push,
screenshot/link the red CI, revert. Recording a *synonym* rather than
`Fixable` is the stronger demonstration, because it proves the check does not
depend on the wordlist.

**Check C — vocabulary scan, extended, kept as the cheap outer net.** Add
`"fixable"` and `"remediable"` to `FORBIDDEN_IN_CORE`. Extending it to "type
documentation, not only identifiers" is **already satisfied**: the existing scan
reads whole files including comments (`package_boundary.rs:313-320`) and the doc
at `package-boundaries.md:41-46` states comments are deliberately included. The
issue's criterion 1 is met by the scan's existing behaviour once
`step_signal.rs` is in core; what is new is that there is finally a core outcome
type for it to apply to.

### 4.3 "Core has no branch on port name" — proving a negative

You cannot prove absence of a branch by testing behaviour, and a source grep for
`match` is defeated by `if port.as_str() == "fixable"`. Three mechanisms, in
increasing strength:

**(1) Behavioural — disjoint port sets, identical routing (criterion 4).**

```rust
#[test]
fn core_routes_two_components_with_disjoint_port_sets() {
    // Component A declares: ["alpha", "beta"]. Component B: ["gamma", "delta"].
    // No overlap with each other or with any shipped port name.
    // Build a graph with edges labelled alpha/beta/gamma/delta and assert
    // select_edge picks the label-matched edge in all four cases.
}
```

This proves core routes *unknown* names correctly. It does not prove core lacks
a special case for `"fixable"` — a hidden branch would simply not fire. Its real
value is as the criterion-4 artifact and as a regression test.

**(2) Property-based — routing is invariant under port renaming.** The strong
behavioural form. If core has no branch on a port name, then consistently
renaming every port in a graph must not change which edge is selected:

```rust
#[test]
fn routing_is_invariant_under_consistent_port_renaming() {
    // For each shipped port name p, and a bijection r that maps every port
    // to a fresh opaque name (e.g. "fixable" -> "p7f3a"):
    //   select_edge(signal(p),  from, edges)
    //     yields the same *target* as
    //   select_edge(signal(r(p)), from, rename_labels(edges, r)).
    // Cover the real shipped set: success, fatal, fixable, retryable.
}
```

A branch such as `if port == "fixable" { ... }` changes behaviour under renaming
and **fails this test**, without the test naming the branch or knowing it exists.
This is the strongest available evidence and it is cheap — a loop over four
names and one bijection. Combined with Check A's arity assertion, a hidden
special case has to survive both a shape check and a semantic invariance check.

Limits, stated honestly: it detects branches that alter *edge selection*. A
branch that alters only, say, a log line would pass. I judge that acceptable —
the criterion is about routing — but a reviewer should know the boundary of the
claim.

**(3) Structural — no string literals in core's routing module.**

```rust
#[test]
fn core_routing_contains_no_string_literal_port_names() {
    // Scan crates/luther-engine-core/src/step_signal.rs for `"` outside of
    // comments and doc comments. Assert none appear in executable code.
}
```

Blunt but decisive for a ~120-line file with no legitimate need for a string
literal. It catches the `if port.as_str() == "fixable"` form that (1) misses and
that (2) catches only after the fact. Requires a comment/string-aware scanner —
`xtask/src/main.rs:1341-1470` already contains one for the complexity gate
(**unverified**: I read the function boundaries but did not confirm it is
reusable as-is; measurement is to read `xtask/src/main.rs:1330-1480` in full).

**Recommendation: ship (1) and (2) always; ship (3) if the existing scanner is
reusable, otherwise rely on (1)+(2)+Check A.** Do not write a new
comment-aware Rust lexer for this — that is over-engineering, and (2) already
catches what (3) catches, one commit later.

---

## 5. Sequencing

### 5.1 Should this be one issue?

**No. Split it.** The reasons are structural, not effort-based:

- The work spans two Cargo packages, a trait signature implemented by 28
  registered step types, ~770 call sites, and a config-validation change that
  breaks a shipped file.
- §0.1 means B8 must *create* core's outcome type, so the "prove the boundary"
  work and the "migrate the call sites" work are genuinely different reviews.
- The `Abandon` deletion (§3) and the `issue-fix-v1.toml` deletion (§2.5) are
  behaviour changes that deserve their own arguments and must not ride inside a
  700-line rename.

Proposed split into three sub-issues under #226:

- **B8a — core disposition + port type, and the enforcement that keeps them
  opaque.** PRs 1–3. Delivers criteria 1, 3, and the routing half of 4.
- **B8b — components declare ports; the domain stops owning a global outcome
  algebra.** PRs 4–5. Delivers criteria 2, 3 (the move), and the disjoint-set
  half of 4.
- **B8c — A1 validates declared ports; retire the compatibility surface.**
  PRs 6–7. Delivers criteria 5 and 6.

Risk is front-loaded: every contested decision (arity, opacity enforcement,
`Abandon`'s deletion) is settled in B8a/B8b before any large mechanical diff.

### 5.2 The PR sequence

Every PR leaves `main` green under the full local gate, which per the project's
own CI note is all three of:

```bash
CLIPPY_CONF_DIR=.github/clippy cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask guard
cargo xtask complexity --changed origin/main HEAD
```

plus `cargo test --workspace --all-features`.

---

**PR 1 — Core gains a disposition and an opaque port name. Nothing uses it.**
*Sub-issue B8a. Magnitude: ~150 LoC added in core, ~20 removed from the domain.*

- Add `crates/luther-engine-core/src/step_signal.rs` (§1.2) and export it.
- Delete `impl FromStr for StepOutcome` and its unit test (§2.3) — dead code
  with no caller but its own test.
- Unit tests for `PortName::new` rejecting empty/uppercase/punctuated input and
  accepting `success`/`fixable`/`a_b_9`.

*Verification:* `core_source_contains_no_domain_vocabulary` passes with the new
file present (this is the real check — the file must be written without the word
"issue"). Whole suite green. No behaviour change: nothing imports `StepSignal`
yet.

*Why first:* it is the smallest change that makes criterion 1 non-vacuous, and
it forces the vocabulary question before any call site moves.

---

**PR 2 — The opacity enforcement, against the type from PR 1.**
*Sub-issue B8a. Magnitude: ~200 LoC of tests.*

- New `tests/core_port_opacity.rs`: Check A (three variants, allowlisted),
  vacuity guard, and the string-literal scan (§4.3(3)) if the existing
  scanner is reusable.
- Lift the brace-matched enum-variant parser out of `package_boundary.rs` into a
  shared test helper and have `the_executor_error_type_names_no_domain_concept`
  use it. *Note:* `package_boundary.rs` is already 770 lines against a 750-line
  recommendation and a 1000-line hard limit, so extracting the helper into a
  module is also what keeps that file from growing.
- Add `"fixable"`, `"remediable"` to `FORBIDDEN_IN_CORE`.
- **The mutation demonstration:** a commit adding `Disposition::Amendable`,
  red CI linked in the PR body, then reverted (§4.2 Check B).

*Verification:* the linked failing run. Green after revert.

*Why second:* the enforcement exists before there is anything tempting to
violate it. Doing this after the migration would let the migration itself
establish the wrong shape.

---

**PR 3 — Core routes by label; the domain's two routing functions collapse into
one.**
*Sub-issue B8a. Magnitude: ~120 LoC net; deletes ~56 duplicated lines in
`transition.rs`.*

- Add `Edge` trait + `select_edge` to core (§1.3).
- Implement `Edge` for both `TransitionDef`s in the domain, carrying the
  `None ⇒ "success"` default into the domain impls.
- Reimplement `resolve_transition` and `resolve_transition_schema` as thin
  wrappers over `select_edge`. **Public signatures unchanged** — they still take
  `&StepOutcome`, converting internally. No call site outside `transition.rs`
  changes.
- Add the renaming-invariance property test (§4.3(2)) and the disjoint-port-set
  routing test (§4.3(1)).

*Verification:* all existing transition tests unedited and passing. This is the
"routes identically" proof, and the fact that the fixtures did not need editing
*is* the evidence.

*Why third:* it proves core can route before any component is asked to declare
anything, so PR 4's blast radius is limited to declaration.

---

**PR 4 — `fixable` becomes a declared port of the components that own it;
`abandon` is deleted.**
*Sub-issue B8b. Magnitude: ~250 LoC across ~10 files. The contested PR.*

Two behaviour changes, argued in the PR body from §3:

- Delete `StepOutcome::Abandon`. Replace its two engine uses
  (`runner.rs:274,299`) with the `Halt` disposition. Remove `"abandon"` from
  both `parse_outcome_name`s. Evidence: 0 config edges route on it; the actual
  loop-limit path (`runner.rs:505-515`) never produces it.
- Move `Fixable`'s ownership: define `PORT_FIXABLE` in the software-change
  component, and **fix `generic/shell.rs:355-366`**, which currently defaults
  any non-zero exit to `Fixable`. The generic shell must default to its own
  neutral port (`"fatal"`, preserving today's routing for workflows that have a
  `fatal` edge) — or require an explicit `exit_code_map` entry. Recommend the
  former to keep routing identical; the latter is a second behaviour change and
  belongs in its own PR if wanted.
- Delete both `parse_outcome_name` fallbacks (§0.3c) **and unify with the third
  parser** in `tests/smoke_replay_tests.rs:43-54` (§0.2), replacing all three
  with `PortName::new(...)?` propagating `EngineError`. The smoke-replay parser
  is the one that already fails fast; it is the model, not the outlier.

**Exhaustive inventory of `Abandon` test sites** (verified; PR 4 must address
each, and this list is the review checklist):

| Site | Nature of change |
| --- | --- |
| `tests/engine_execution_integration.rs:395-415` | **Substantive.** Asserts routing on `condition = "abandon"`; rewrite against `Halt`. See §3. |
| `tests/executor_behaviour_snapshot.rs:32` | Mechanical: drop the arm from `outcome_name`'s match. |
| `tests/e2e_workflow_integration.rs:1802-1808` | **Delete.** It loops over a hardcoded literal list (`("post_pr_failure_terminal", StepOutcome::Fatal)`, …) asserting none equals `Abandon` — it tests the literal list the test itself wrote, not the executors. Vacuous today; deleting it removes no coverage. Worth calling out in the PR body as a pre-existing defect found, not as collateral. |
| `tests/smoke_replay_tests.rs:49` | Remove the `"abandon"` arm; no fixture contains it (fixtures hold only `success`×38 and `fatal`×2). |
| `src/components/software_change/llxprt_tests.rs:24,55` | Mechanical: the `parse_outcome_name("abandon")` cases go with the parser. |
| `src/workflow/validation.rs:301-307` + test at `:725,734` | The post-PR `abandon` prohibition becomes redundant once no port is named `abandon`; **delete the rule and its test together**, or the rule silently guards nothing. |

*Verification:* the 34 config/fixture files are untouched; the workflow-loading
tests for all three live workflows pass unedited; smoke-replay fixtures
regenerate to byte-identical content. A test asserting an unknown outcome name in
an `exit_code_map` now *errors* rather than silently becoming `Fatal` or
`Success` — a new, intended behaviour with its own test.

*Risk note:* this is where routing could actually change. The mitigation is that
PR 3 already proved label-equality routing, so any diff here that alters a
*label string* is visible in review.

---

**PR 5 — `StepExecutor` declares ports; the registry exposes them.**
*Sub-issue B8b. Magnitude: ~300 LoC across ~30 files, but mechanical and
uniform.*

- Add `declared_ports()` to `StepExecutor` with **no default implementation** —
  a default would let a new component silently declare nothing, and A1 would then
  reject all of its edges, which is a confusing failure. Requiring it means
  adding a component is a compile error until its ports are stated.
- Each component declares its constant set.
- `ExecutorRegistry::ports_for(step_type)`.
- Extend `tests/registry_composition.rs` with the declared-port sets, written out
  in full in the same style as `EXPECTED_CATALOG` — the file's own comment
  explains why the expectation is spelled out rather than derived.

*Verification:* `the_product_catalog_matches_the_shipped_step_types` still
passes; the new port-set assertion is the reviewable specification. No routing
change — `declared_ports` has no consumer until PR 6.

*Why this is reviewable despite touching ~30 files:* every hunk is the same
four-line shape, and the semantic content is concentrated in one table in the
test file, which is what a reviewer actually reads.

---

**PR 6 — A1 validates every transition against declared ports.**
*Sub-issue B8c. Magnitude: ~150 LoC + deletion of 4 config/fixture files.*

- `validate_workflow_graph` gains a rule: for each transition, the effective
  condition must be in `ports_for(step.step_type)`. New
  `GraphErrorCategory::UndeclaredPort`.
- Delete `config/workflows/issue-fix-v1.toml` and its three fixture copies
  (§2.5), with the dead-edge census in the commit message.

*Verification:* the three live workflows validate clean; a new test asserts a
workflow with `condition = "nonsense"` is rejected with `UndeclaredPort`; a test
asserts `condition = "approved"` on a `review` step is rejected — the previously
silent failure now speaks. **This closes criterion 6 and is the first time the
dead edges found in §0.3d are caught.**

---

**PR 7 — Retire `StepOutcome`; `execute` returns `StepSignal`.**
*Sub-issue B8c. Magnitude: ~700 call sites across 47 src files and 22 test
files. Mechanical by construction, because PRs 1–6 removed every decision.*

- `StepExecutor::execute` returns `Result<StepSignal, EngineError>`.
- `StepOutcome` is deleted; `engine/mod.rs:29` stops re-exporting it.
- Call sites become `StepSignal::proceed(PORT_FIXABLE)` etc.

*Verification:* no config file changes; no behaviour fixture changes; the
renaming-invariance and disjoint-port tests from PR 3 still pass.

**This PR is deliberately last and is deliberately mechanical.** The brief asks
that no PR be "a mechanical 700-line rename that cannot be reviewed" — the
resolution is not to avoid the mechanical change but to ensure that by the time
it happens, *every semantic decision has already been reviewed elsewhere*, so
the reviewer's job is to confirm it is mechanical. If PR 7 contains any hunk
that is not a mechanical substitution, that hunk belongs in an earlier PR.

Consider splitting PR 7 by directory (`src/components/github/`,
`src/components/software_change/`, `src/engine/`, `tests/`) if the trait
signature can be changed with a transitional blanket impl. **Unverified** whether
that is workable without a `#[deprecated]`-style shim; the measurement is to
attempt the trait change locally and count the resulting error sites. If it
cannot be split cleanly, keep it whole — a single atomic mechanical PR is safer
than a shim that lets two representations coexist.

### 5.3 Complexity-gate watch items

- `tests/package_boundary.rs` is **770 lines**, over the 750 recommendation. PR 2
  must extract the shared parser into a module rather than append to it.
- `src/persistence/checkpoint.rs` is **1055 lines — already over the 1000 hard
  limit.** `cargo xtask complexity --changed` only inspects changed files, so it
  passes today only because the file is untouched. **Any PR that edits
  `checkpoint.rs` will trip the hard limit and must split the file first.** This
  directly affects §2.4's second column. Recommendation: if §2.4 is adopted, its
  first commit is a pure split of `checkpoint.rs` (the events-table functions,
  lines ~480-560 and ~580-700, are a natural `checkpoint/events.rs`), verified by
  the complexity gate before any behaviour change lands.
- `src/components/software_change/llxprt.rs` is 982 lines — under the limit but
  close. PR 4 removes ~10 lines from it, so it moves the right way.

---

## 6. Open questions and the measurements that resolve them

**Resolved during this review:**

| Question | Answer |
| --- | --- |
| Does anything outside the Rust tree read `events.outcome`? | **No.** `grep -rn "events" .github/scripts/` returns nothing. |
| Do any test-side `Abandon` uses encode real behaviour? | **Yes, one.** `tests/engine_execution_integration.rs:395-415` asserts routing on `condition = "abandon"`. Full inventory in §5.2 PR 4. |
| Is the persisted outcome string ever parsed back? | **Yes, in the smoke-replay harness.** §0.2 — this corrected an earlier draft of this document. |

**Still open:**

| Question | Measurement |
| --- | --- |
| Is `config/workflows/issue-fix-v1.toml` live? | Referenced by **no** Rust source and by **no** `.github/` file. It *is* described as "the MVP workflow" in `docs/project/mvp-workflow-design.md:23,70,96,583`, which also lists it as a P0 item to replace ("Replace placeholder step_types with real ones"). That reads as aspirational design doc, not an operational reference — but PR 6 should confirm with the doc's owner before deleting, or update the doc in the same PR. |
| Is the comment-aware scanner in `xtask/src/main.rs:1330-1480` reusable for §4.3(3)? | Read that range in full; check it is exposed rather than a private closure |
| Can PR 7 be split by directory behind a transitional impl? | Change the trait signature locally, `cargo check`, count and cluster the error sites |

---

## 7. Summary of the design in one paragraph

`StepOutcome` conflates three jobs. Core keeps only the first — a three-case
`Disposition` (`Proceed`/`Suspend`/`Halt`) that is exhaustive over what the
runner can do with a step result — and performs the second opaquely, selecting
an edge by equality on a `PortName` it never inspects. Components own the third,
declaring their port sets as constants that A1 validates workflow edges against.
The wire strings do not change, so all 34 config files and every routing fixture
are untouched and no database migration exists to get wrong. `Abandon` is deleted
as an unused synonym for `Halt`; `Retryable` becomes an ordinary port because the
engine has no retry loop; `Fixable` moves to software-change *and* out of the
generic shell's non-zero-exit default. Enforcement is an allowlisted three-variant
assertion plus a routing-invariance-under-port-renaming property test, which
together catch synonym evasion and hidden special-cases that a vocabulary grep
cannot.
