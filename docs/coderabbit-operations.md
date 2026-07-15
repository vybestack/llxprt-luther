# CodeRabbit configuration and review operations

The repository-root [`.coderabbit.yaml`](../.coderabbit.yaml) is Luther's sole
repository configuration for CodeRabbit. CodeRabbit's YAML configuration guide
states that this file **must** be in the repository root and that the feature
branch version is used while reviewing that branch. The previous
`workflow/.coderabbit.yaml` location was therefore not a documented lookup
location; this does not prove how CodeRabbit treated historical reviews.

Vendor references:

- [Configuration via YAML file](https://docs.coderabbit.ai/getting-started/yaml-configuration)
- [Configuration reference](https://docs.coderabbit.ai/reference/configuration)
- [Automatic review controls](https://docs.coderabbit.ai/configuration/auto-review)
- [Review commands](https://docs.coderabbit.ai/reference/review-commands)

## Incremental-review baseline

The cap is `5` reviewed commits, matching the current vendor default. The choice
uses demand and observed coverage, rather than treating throttle notices as the
only signal.

The baseline queried merged PRs 141, 130, 134, 133, 128, 126, 120, 124, 116,
110, 114, 112, 111, 98, 106, 100, 105, 104, 103, and 102 at
`2026-07-15T11:47:12Z`. These were the 20 most recently updated merged PRs when
the sample was selected. The append-only
[baseline snapshot](coderabbit-baseline-2026-07-15.json) retains PR and record
IDs, event kinds, source URLs, timestamps, commit and reviewed-head SHAs, and
SHA-256 hashes of mutable comment/review payloads at observation time.

The query selected `number`, `commits(first: 100) { totalCount, commit.oid }`,
`reviews(first: 100) { databaseId, url, author.login, state, submittedAt,
commit.oid, body }`, and `comments(first: 100) { databaseId, url, author.login,
createdAt, updatedAt, body }`, using `pullRequests(first: 20, states: MERGED,
orderBy: {field: UPDATED_AT, direction: DESC})`. Bot records were classified
case-insensitively when the author login contained `coderabbit`; reviewed heads
required a non-null review commit OID. Payload hashes cover the UTF-8 comment or
review body, so the standard SHA-256 empty-input value is valid for reviews with
an empty body. A reviewed head may be absent from the PR's current commit list
after a force-push or rebase; the review's immutable commit association remains
the authoritative completion record. PR commit count is the available demand
proxy. A CodeRabbit review submission attached to a commit records a completed
reviewed head. The mutable CodeRabbit walkthrough comment records the currently
visible throttle marker, not a count of discrete throttle events.

| Event or measure | Observed value |
| --- | ---: |
| PR status observations (walkthrough comments) | 20 |
| Total commits across sampled PRs | 131 |
| Median commits per PR | 3 |
| PRs with at most 3 commits | 11/20 |
| PRs with at most 5 commits | 12/20 |
| PRs with more than 5 commits | 8/20 |
| PRs with at least one completed reviewed head | 13/20 |
| Distinct completed reviewed heads | 26 |
| PRs whose final head had a recorded completed review | 1/20 |
| Walkthrough comments containing a throttle marker | 20/20 |

The former cap of 3 covered the median demand but the sample had weak head and
final-head coverage. Raising the cap to 5 allows two more automatic reviewed
commits before pausing and increases commit-count-proxy coverage by one sampled
PR without removing the safety valve for the eight long-tail PRs. Historical
review timing is not available, so this proxy does not prove those PRs would
have been reviewed end-to-end under either cap. For a paused PR, request another
incremental review with `@coderabbitai review`, preferably after the branch is
ready for another pass. Use `@coderabbitai full review` only when a fresh review
of the complete change is needed.

These figures are a reproducible baseline, not a causal claim: rate limits,
manual commands, draft state, review timing, and the formerly nested file may
all affect historical observations.

## Ingestion verification

File placement and schema validation are necessary but not sufficient. PR 143
provides observable vendor evidence:

- The [resolved-configuration response](https://github.com/vybestack/llxprt-luther/pull/143#issuecomment-4980198709)
  reported `Path: .coderabbit.yaml` and identified repository YAML as the source
  of the assertive profile, cap of 5, automatic review controls, enabled Clippy,
  issue planning, and disabled issue-label application.
- The [completed review](https://github.com/vybestack/llxprt-luther/pull/143#pullrequestreview-4703812969)
  was submitted for head `297386ae54ca87a78b93630b04321b04813b61a8`.
- The append-only [ingestion snapshot](coderabbit-ingestion-pr143.json) retains
  those record IDs, timestamps, URLs, resolved values, head SHA, and payload
  hashes.

The resolved output did not contain the former unsupported
`reviews.instructions` key. The configuration now expresses that preserved
Rust/workflow guidance through the documented `reviews.path_instructions`
field for `workflow/**`. After the updated head is reviewed, the PR evidence
must likewise show that the path instruction resolves from repository YAML and
that the submitted review is attached to that exact head.

For subsequent changes, comment `@coderabbitai configuration`, retain the actual
response and source annotations, and verify the completed review's commit ID
against the then-current PR head. A review request alone is not ingestion
evidence.

## Event ledger

No pre-existing durable CodeRabbit measurement ledger was present when this
baseline was created. The committed snapshots retain this study's observable
status, completion, throttle, and reviewed-head/coverage evidence. The sample
does not claim request events because bot-authored walkthroughs do not identify
the request source, mode, or requested head. For future observations, append one
immutable record per event to a durable ledger or commit a content-hashed
snapshot with:

- repository and PR number;
- event kind: `request`, `status_snapshot`, `completion`, `throttle`, or `coverage`;
- observation timestamp and source URL;
- requested head SHA and reviewed head SHA when known;
- request mode (`automatic`, `incremental`, or `full`) when known;
- completion result or throttle reason; and
- whether the reviewed head equals the then-current PR head.

Do not derive event counts by repeatedly scraping the latest walkthrough body:
CodeRabbit edits that comment, so it is a status snapshot rather than an
append-only event stream.

## Rollback

If root-level ingestion changes review scope unexpectedly, revert the PR 143
merge or squash commit as one rollback unit. If the PR is merged without
squashing, revert its complete commit range together; reverting only the first
configuration-move commit does not restore the exact prior state. Do not keep
root and nested copies in parallel: duplicate files create ambiguous human
ownership even though only the root path is documented as vendor authority.
After rollback, use `@coderabbitai configuration` to verify the effective source
and values before reapplying a root file.
