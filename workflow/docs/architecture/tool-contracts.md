# Tool contracts

A tool contract records **what an external tool actually does**, pinned to the
exact version it was verified against, evidenced by digested output captured
from the real binary.

## Why this exists

The convergence retrospective (#201, `convergence-retrospective.md`) analysed
ten campaign failures under a pre-registered rubric. Six share one shape:

| Case | Caller assumed | Tool actually did |
|---|---|---|
| #174 | prints `Excluded (2):` | prints `Excluded from review (2):` |
| #176 | keys on the canonical realpath | keys on the **logical** working directory |
| #179 | keys on the build workspace root | keys on the Git root |
| #182 | `--repo` behaves uniformly | honoured by one subcommand, ignored by another |
| #183, #186 | honours `--json` | accepts it, prints a human table |
| #195 | one session per run | auxiliary invocations create empty sessions |

In **every one**, the underlying operation succeeded and the gate reported
failure because it could not retrieve or interpret the result. None was a
routing failure, which is why a routing layer would not have helped.

Two rows correct the record rather than restate it. The retrospective and issue
#176 both say the tool keys on the *canonical* path; measured against the
binary it keys on the **logical** path, which is the opposite error and needs
the opposite fix. Issue #182 says `--repo` is ignored; it is honoured by
`session list` and ignored by `session show`. Those sources are wrong, and the
corrections belong here rather than silently applied upstream, so the audit
trail survives.

Both independent raters named the same gap. Stated by the rater who had not
seen the other's labels:

> The held field set describes **what the caller sent, never what the callee
> honoured**.

## The four rules

**1. Capture from the real binary.** Fixtures are recorded by running the tool
and saving its output verbatim. A hand-written fixture encodes the same
assumption as the parser it validates, so the two agree with each other and
disagree only with reality. That is #174 exactly.

**2. Digest the captures.** "Captured from the real binary" is otherwise an
unfalsifiable claim. A digest makes editing a fixture to match a mistaken
parser detectable.

**3. Pin the exact version.** Substring matching accepts `v1.7.1` against
`v1.7.16`; a pin that accepts a version it was never verified against is not a
pin. Matching is on whitespace-delimited tokens, and the pin is checked against
the version CI installs, so upgrading the tool forces re-verification.

**4. Record per subcommand, and prove it with a discriminating capture.**
`session list` honours `--repo`; `session show` accepts it and ignores it. Same
tool, same flag, opposite behaviour.

## Designing the capture

A capture that cannot distinguish two hypotheses is not evidence for either.

Varying only the working directory shows that `ocr session list` returns
nothing from an unrelated directory. That is equally consistent with "keys on
the working directory" and with "defaults to the working directory when no path
argument is given". The captures must vary the thing in question:

```text
cd /tmp && ocr session list --json                  -> null
cd /tmp && ocr session list --json --repo $REPO     -> [ ... ]
```

Now the hypotheses separate: `--repo` is honoured, and the working directory is
its default. The same pair against `session show` gives the opposite answer,
which is the finding that matters.

## What a contract records

- **State key** — what actually selects the data: the logical working
  directory, the canonical working directory, a path argument, or the Git root.
- **Flag behaviour** — `Honoured`, `Rejected`, or `AcceptedAndIgnored`. The
  third is the dangerous one: exit zero plus wrong output. It must name a
  non-empty alternative, because that remediation is the whole value of
  recording it.
- **Result source** — `Stdout`, or a `DurableArtifact` when stdout is
  human-oriented and something else is authoritative.
- **Required fields** — the fields consumers read, so that a rename in the tool
  is caught rather than surfacing later as a missing value.
- **Captures** — filename plus digest.
- **Capture provenance** — how the captures were taken.

## Logical versus canonical working directory

`ocr` keys the session store off the **logical** working directory, not the
resolved one. From a symlinked path the lookup fails; unsetting `PWD` so the
process reports the resolved path makes it succeed.

`.github/scripts/ocr-session-evidence.js` calls `fs.realpathSync()` and states
the opposite. Under a symlinked workspace those disagree, and the reader would
look somewhere the tool never wrote. The contract records the observed
behaviour; reconciling the reader is tracked separately, since changing
evidence retrieval is outside this change.

This distinction is why `StateKey` separates `LogicalWorkingDirectory` from
`CanonicalWorkingDirectory`. An enum that collapsed them could not express the
failure #176 actually was.

## Failing at the right layer

`require_honoured` rejects a caller that depends on an ignored flag **at
validation**, naming the alternative.

Without it, the failure surfaces as a decode error at byte one of unexpected
output. Issue #183 records where that leads: the investigation went hunting
through path derivation and session ids, when the requested format was simply
never produced.

## Adding or updating a contract

1. Run the tool and capture output verbatim. Do not hand-write it.
2. Include a **discriminating** capture for any behaviour in question — one
   that varies the thing being tested, not merely the surrounding conditions.
3. Record the contract pinned to the exact version, with a digest per capture.
4. Assert each recorded behaviour against its capture, reading captures
   **through** the contract so declared filenames are load-bearing.
5. **Mutate every claim and confirm the suite fails.** A contract nothing can
   contradict is decoration.

## Verified mutations

`tests/tool_contract_mutation/mutate.py` applies each of these to a clean tree,
runs the suite, restores, and verifies the restored tree is green before
reporting. All 34 fail the suite; the table lists them by the assumption each
one falsifies:

| Mutation | Detects |
|---|---|
| Truncate the version pin to a prefix | a pin that is not a pin |
| Accept an empty version string | failing open where it matters most |
| Swap two declared capture filenames | captures not read through the contract |
| Falsify a fixture, leaving the digest stale | casual editing |
| **Falsify a fixture and refresh its digest** | **#174** |
| Replace a capture with a hand-written invention | fabricated evidence |
| Reduce the version capture to a bare token | evidence hollowed out |
| Pad the negative control with whitespace | a control that proves nothing |
| Empty an ignored flag's alternative | a vacuous remediation |
| Point a remediation at a flag the tool ignores | a remediation that is false |
| Declare a flag that does not exist | a guess recorded as fact |
| Add a subcommand with no capture | claims without evidence |
| Claim the contract describes another tool | unanchored identity |
| Deny in provenance that the tool was run | provenance as decoration |
| Empty either subcommand's required fields | content checks silently disabled |
| Empty a field's justification | a record without a reason |
| Bump the version CI installs | a pin that does not track the gate |
| Truncate the digest above 1 MB | integrity that stops at a threshold |
| Change a recorded state key | #176, #182 |
| Claim an ignored flag is honoured | #183, #186 |
| Drop a required field | a contract weakened silently |
| Record a flag that only prefixes a real one | a check passing by substring |
| Point a remediation at an uncaptured flag | a remedy with no evidence |
| Rewrite a capture with CRLF endings | line endings breaking the digest |
| Emptied remediation string | a vacuous alternative |
| Factually false remediation | a remedy contradicting the tool |
| Version check accepts empty input | failing open on an empty probe |
| Claim `--limit` is ignored despite its capture | a behaviour contradicted by evidence |
| Drift the documented count from the battery | a coverage claim nothing checks |
| Record one flag twice | a duplicate silently shadowing |
| Name an unhonoured flag after a second subcommand | a flag checked against the wrong command |
| Claim review keys on the working directory | #179 |
| Shorten the preview's exclusion header | #174 |
| Describe review's durable artifact as stdout | #195 |

The fifth is the one that matters. Digesting a capture only catches a *stale*
digest; a maintainer who fabricates output will refresh the digest, because the
re-capture procedure tells them to. What defeats fabrication is that the
captures must agree with each other the way the tool's own output does — the
store path derives from the repository path, the jsonl is named for its
session, and both subcommands describe the same session. Forging one file now
requires forging a coherent set.

Two earlier revisions of this document claimed that mutation was caught when it
was not. That is the same pattern the contract exists to prevent, arrived at
twice, which is why the battery is an executable script that classifies a
mutation as caught only when the suite genuinely fails — a mutation that does
not compile, or whose anchor has drifted, is reported as invalid rather than
counted as a pass.

## Scope

Contracts cover `ocr session list` and `ocr session show`. `ocr review`, `git`
and `gh` do not yet have contracts.

What this does **not** yet do, stated plainly because a partial mechanism
described as a complete one is the failure this work exists to correct:

- **No production code consumes the contract.** It is enforced by its own tests
  and by the version binding to CI. Routing every flag through
  `require_honoured` before spawning, and driving retrieval from
  `ResultSource`, is the integration that would make it load-bearing.
- **Of the failures cited above, two are modelled** — `--repo` per subcommand
  (#182) and `--json` on `session show` (#183, #186) — and `session show`'s
  path derivation (#176). #174 is a format-drift defect in printed output that
  `RequiredField` cannot express, since it holds field names rather than output
  literals; #179 and #195 have no representation.
- **Captures are machine-specific.** They embed absolute paths from the
  capturing machine and cannot be reproduced in CI, where no session store
  exists. Nothing detects the captures drifting from the binary — only from
  each other and from the pinned version.

These are the reasons the contract is not yet the thing issue #239 asks for.
