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
the sample was selected. The immutable source records are each PR's GitHub
commits, reviews, and issue comments, available at
`https://github.com/vybestack/llxprt-luther/pull/NUMBER`.

The query selected `number`, `commits(first: 100) { totalCount, commit.oid }`,
`reviews(first: 100) { author.login, state, submittedAt, commit.oid }`, and
`comments(first: 100) { author.login, createdAt, body }`, using
`pullRequests(first: 20, states: MERGED, orderBy: {field: UPDATED_AT, direction: DESC})`.
Bot records were classified case-insensitively when the author login contained
`coderabbit`; reviewed heads required a non-null review commit OID. PR commit
count is the available demand proxy. A CodeRabbit review submission attached
to a commit records a completed reviewed head. The mutable CodeRabbit
walkthrough comment records the currently visible throttle marker, not a count
of discrete throttle events.

| Event or measure | Observed value |
| --- | ---: |
| PR request/status observations (walkthrough comments) | 20 |
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

File placement and schema validation are necessary but not sufficient. On the
PR that changes this configuration:

1. Comment `@coderabbitai configuration`.
2. Confirm the resolved configuration identifies repository YAML as the source
   of the assertive profile, cap of 5, Rust review instructions, enabled Clippy
   tool, issue planning, and disabled issue-label application.
3. Record the comment URL and reviewed head in the PR evidence.
4. After a review completes, confirm that its submitted review is attached to
   the expected PR head.

The configuration command is the vendor-supported observable ingestion check;
it reports both resolved values and their sources.

## Event ledger

No durable CodeRabbit measurement ledger was present when this baseline was
created. Until one is available, retain request/status, completion, throttle,
and reviewed-head evidence in the relevant PR. When a ledger becomes
available, append one immutable record per observed event with:

- repository and PR number;
- event kind: `request`, `completion`, `throttle`, or `coverage`;
- observation timestamp and source URL;
- requested head SHA and reviewed head SHA when known;
- request mode (`automatic`, `incremental`, or `full`) when known;
- completion result or throttle reason; and
- whether the reviewed head equals the then-current PR head.

Do not derive event counts by repeatedly scraping the latest walkthrough body:
CodeRabbit edits that comment, so it is a status snapshot rather than an
append-only event stream.

## Rollback

If root-level ingestion changes review scope unexpectedly, revert the
configuration-move commit. That restores the exact prior repository state while
the unexpected behavior is investigated. Do not keep root and nested copies in
parallel: duplicate files create ambiguous human ownership even though only the
root path is documented as vendor authority. Before reapplying the root file,
use `@coderabbitai configuration` to identify any repository, organization, or
workspace settings that override or extend it.
