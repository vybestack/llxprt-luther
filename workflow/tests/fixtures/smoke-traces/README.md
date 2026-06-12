# Smoke-Trace Fixtures (deterministic engine-routing replay)

These JSON fixtures are normalized **smoke traces** (schema v1) that record an
engine run's ordered `(step_id, outcome)` sequence plus its terminal
`final_outcome`. Replaying these recorded per-step outcomes through the real
`EngineRunner` re-derives identical routing **offline** — no network, no `gh`,
no auth — which makes otherwise-irreproducible live smoke failures
deterministically replayable (Luther issue #19).

## Schema (v1)

A trace is a JSON object:

| Field              | Type     | Meaning                                                         |
| ------------------ | -------- | -------------------------------------------------------------- |
| `schema_version`   | u32      | Trace schema version. Loader rejects versions newer than 1.    |
| `run_id`           | string   | Engine run id the trace was captured from.                     |
| `workflow_type_id` | string   | Workflow type executed (e.g. `llxprt-issue-fix-v1`).           |
| `config_id`        | string   | Workflow config that parameterized the run (e.g. `llxprt-code`).|
| `captured_at`      | RFC3339  | When the trace was captured (UTC).                             |
| `final_outcome`    | object   | Terminal run outcome (`success`/`failure`/`abandoned`/`interrupted`). |
| `events`           | array    | Ordered per-step events that drove routing.                    |

Each entry in `events` is:

| Field     | Type   | Meaning                                                |
| --------- | ------ | ------------------------------------------------------ |
| `seq`     | u32    | Zero-based sequence index (recorded/timestamp order).  |
| `step_id` | string | The step that executed.                                |
| `outcome` | string | `success` / `retryable` / `fatal` / `fixable` / `abandon`. |

`final_outcome` is tagged by `kind`:

- `{ "kind": "success" }`
- `{ "kind": "failure", "step_id": "...", "reason": "..." }`
- `{ "kind": "abandoned", "step_id": "...", "reason": "..." }`
- `{ "kind": "interrupted", "step_id": "..." }`

## Fixtures

- `success-select-and-fetch.json` — the all-success happy path through the
  `llxprt-issue-fix-v1` workflow, terminating at `log_completion` with
  `RunOutcome::Success`.
- `failure-abandon.json` — the `create_plan -> fatal -> abandon_and_log` route
  that the live smoke executor forces, terminating in `RunOutcome::Failure` at
  `abandon_and_log`.

## How to regenerate from a live smoke run

```sh
LUTHER_SAVE_TRACES=1 cargo test --test smoke_test -- --ignored
```

The live smoke test prints a `SMOKE_TRACE saved=<path> replay="..."` line and
writes the captured trace JSON. Copy the relevant trace into this directory and
adjust `run_id`/`captured_at`/`reason` as needed (replay compares step sequence,
per-step outcomes, and the terminal outcome's variant + `step_id`; free-form
`reason` text is ignored).
