# Luther Roadmap After Self-Hosting Qualification

Date: 2026-07-24
Basis: `PLAN-20260723-SELFHOST-RELIABILITY` is QUALIFIED.

## Immediate Delivery Gate

The qualified reliability surface is still an uncommitted working-tree stack on
`main`. It must be landed and verified by repository CI before another issue is
started through the loop. This is a process gate, not additional feature scope.

## Verified Plan Status

| Plan | Status | Disposition |
|---|---|---|
| Initial runtime | Complete | Historical foundation |
| Step execution (`next`) | Complete | Superseded by llxprt-first |
| llxprt-first | Complete | Historical foundation |
| CodeRabbit PR follow-up | Complete | Active production capability |
| Scope control, issue 142 | Complete and closed | Active production capability |
| Self-hosting reliability | Qualified, not yet landed | Immediate delivery gate |

## Ordered Remaining Work

1. Land the qualified self-hosting reliability stack and confirm CI.
2. Issue 138 — immutable reviewed-range manifest and fail-closed incomplete OCR.
3. Issue 115 — suppress resolved duplicate OCR findings across heads.
4. Issue 122 — gate PR completion on unresolved OCR threads.
5. Issue 119 — auditable recovery of eligible abandoned terminal runs.
6. Issue 108 — eliminate stale `success_file` dead ends.
7. Issue 107 — detect broken llxprt installations and harden process failure handling.

Issue 138 is the next bounded product delivery because it is assigned, gives OCR
coverage an auditable completeness contract, and provides the foundation needed
by issues 115 and 122.

### Issue 138 dependency verification

The pinned OCR 1.7.9 already contains the session/checkpoint substrate from
upstream PR 306:

- immutable session IDs and parent `resumed_from` identity;
- per-file `review_item_done`, `review_item_reused`, and
  `review_item_failed` checkpoint records;
- `ocr review --resume <session-id>`;
- `ocr session list/show` inspection;
- completed, reused, failed, aborted, and LLM-failure aggregates.

It does **not** provide the complete manifest contract requested by upstream
issue 367: selected-file identity, exact requested/resolved commit range,
versioned completeness state, explicit waivers, typed failure classes,
provider/config hashes, or parity between persisted session and review JSON.
Upstream issue 367 remains open and assigned. Therefore issue 138 should adapt
and validate OCR's existing session data, not build a second checkpoint store;
any Luther-owned wrapper manifest must be a redacted immutable projection that
can be replaced by the upstream contract once available.

## Issue 138 Scope Boundary

In scope:

- one versioned reviewed-range manifest schema shared by PR and local OCR paths;
- exact repository/head/base/range identity;
- selected, completed, reused, failed, and explicitly waived file sets;
- terminal completeness (`complete`, `partial`, `failed`, `skipped`);
- clean/dirty local state with a non-secret diff hash;
- immutable redacted artifacts and hashes;
- fail-closed summaries when coverage is incomplete;
- tests for full success, zero findings, partial output, resume, waiver,
  cancellation, dirty local state, and redaction.

Out of scope:

- CodeRabbit behavior;
- RecoveryProtocolV1 redesign;
- workflow topology changes unrelated to OCR completeness;
- making every OCR finding merge-blocking.

## Autonomy Scorecard

| Metric | Target | Collection point |
|---|---:|---|
| Human interventions per delivery | 0 | recovery/continuation events and supervisor restarts |
| Duplicate logical effects | 0 | effect intents, PR action identities, OCR finding identities |
| Terminal diagnostic completeness | 100% | redacted run/attempt/phase artifacts |
| Exact-head completion gates | 100% | typed merge proof and artifact |
| Verified merges | 100% | `ReviewReady → Merged` typed completion |
| Scope budget crossings without durable decision | 0 | scope-control request/resolution artifacts |

Every subsequent bounded issue should report these metrics. A manual SQL edit,
manual Git/GitHub mutation, duplicate effect, unbound head, or unverified merge
invalidates that delivery's autonomy claim.

## Explicitly Deferred

- Distributed persistence.
- Async engine redesign.
- Arbitrary exact recovery of legacy runs.
- Generic PR artifact-store redesign.
- Language-independent source-analysis plugin framework.
