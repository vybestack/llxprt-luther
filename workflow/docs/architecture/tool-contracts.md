# Tool contracts

A tool contract records **what an external tool actually does**, pinned to the
version it was verified against, checked against output captured from the real
binary.

## Why this exists

The convergence retrospective (#201, `convergence-retrospective.md`) analysed
ten campaign failures under a pre-registered rubric. Six of them share one
shape:

| Case | Caller assumed | Tool actually did |
|---|---|---|
| #174 | prints `Excluded (2):` | prints `Excluded from review (2):` |
| #176 | keys on the passed path | keys on the canonical realpath |
| #179 | keys on the build workspace root | keys on the Git root |
| #182 | honours `--repo` | ignores it, uses the process cwd |
| #183, #186 | honours `--json` | accepts it, prints a human table |
| #195 | one session per run | auxiliary invocations create empty sessions |

In **every one**, the underlying operation succeeded and the gate reported
failure because it could not retrieve or interpret the result. None was a
routing failure, which is why a routing layer would not have helped.

Both independent raters named the same gap. Stated by the rater who had not
seen the other's labels:

> The held field set describes **what the caller sent, never what the callee
> honoured**.

## The three rules

**1. Capture from the real binary.** Fixtures are recorded by running the tool
and saving its output verbatim. A hand-written fixture encodes the same
assumption as the parser it validates, so the two agree with each other and
disagree only with reality. That is #174 exactly, and unit tests confirmed the
invention.

**2. Pin the version.** A contract is a claim about one build. If the installed
tool reports a different version, validation fails closed and names both
versions. The alternative is silently reinstating the assumption the contract
exists to remove.

**3. Record per subcommand.** `session list` honours `--json`; `session show`
accepts it and prints a table. Same tool, same flag, opposite behaviour. A
per-tool record cannot express this.

## What a contract records

- **State key** — what actually selects the data. The process working
  directory, a canonicalised path argument, or the Git root. Guessing wrong
  produced #176, #179 and #182.
- **Flag behaviour** — `Honoured`, `Rejected`, or `AcceptedAndIgnored`. The
  third is the dangerous one: exit zero plus wrong output. It names what to use
  instead.
- **Result source** — `Stdout`, or a `DurableArtifact` when stdout is
  human-oriented and something else is authoritative.
- **Captured output** — the fixture the contract is checked against.
- **Capture provenance** — how the captures were taken, so anyone can repeat
  them.

## Failing at the right layer

`require_honoured` rejects a caller that depends on an ignored flag **at
validation**, naming the alternative.

Without it, the failure surfaces as a decode error at byte one of unexpected
output. Issue #183 records where that leads: the investigation went hunting
through path derivation and session ids, when the requested format was simply
never produced. A diagnostic that names the real cause is the difference
between a short fix and a long one.

## Adding or updating a contract

1. Run the tool and capture output verbatim into
   `tests/fixtures/tool-contracts/<tool>/`. Do not hand-write it.
2. Include a **control capture** where a behaviour is in question — the same
   command under a different condition. The working-directory keying is
   demonstrated by capturing `session list --json` from inside and outside the
   repository; the second returns `null`.
3. Record the contract in `src/tool_contract/<tool>.rs`, pinned to the version.
4. Add a test asserting each recorded behaviour against its capture.
5. **Mutate every claim and confirm the suite fails.** A contract nothing can
   contradict is decoration.

## Verified mutations

Each of these was applied and confirmed to fail the suite:

| Mutation | Detects |
|---|---|
| Claim `--json` is honoured on `session show` | #183, #186 |
| Change the state key to a path argument | #176, #179, #182 |
| Pin a stale version | version drift |
| Falsify a fixture to match a wrong assumption | **#174** |
| Falsify the foreign-cwd control capture | a control that proves nothing |
| Drift the shipping reader's version claim | prose and types disagreeing |

The fourth matters most: it is the failure where the parser and its fixture
agree with each other and only reality dissents.

## Scope

Contracts currently cover `open-code-review`, the tool that produced six of the
ten analysed failures. `git` and `gh` are also invoked; neither has yet
produced a contract defect in the campaign record, and contracts should be
added when evidence calls for them rather than pre-emptively.
