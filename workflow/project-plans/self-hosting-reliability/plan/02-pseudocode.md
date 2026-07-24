# Phase 02: Pseudocode — Protocol, Capsule, Policy, Adapters, Merge, Salvage

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P02`

## Prerequisites

- Required: P01A completed with PASS.

## Purpose

Produce numbered-line pseudocode for every new surface introduced by this plan.
Implementation phases (P05, P08, P11, P14, P15, P17) MUST cite these line
numbers. No implementation phase may begin until its pseudocode block is locked.

> **This document is the SOLE pseudocode source.** No implementation phase may
> carry alternate, conflicting pseudocode. Phase docs that need pseudocode MUST
> reference these numbered lines by component and line number. (See B13.)

### Design Corrections Applied

#### Remediation cycle 1 (13 corrections `[C1]`–`[C13]`)

Each is referenced by number `[C##]` inline where it affects the pseudocode:

- **[C1]** Distinct durable recovery epoch/state, not `MAX(attempt generation)`,
  with short IMMEDIATE CAS claim. No synthetic attempts to bump generation.
- **[C2]** `recovery_operations` ledger with stable `operation_id`/idempotency
  key, exact request/capsule/source-attempt binding, Pending/Completed/Refused/
  Conflict state and serialized prior outcome.
- **[C3]** Append-only attempts include complete `StateSnapshot`, immutable
  attempt/source-parent IDs, step ID/status, capsule schema + envelope digest,
  snapshot/checkpoint digest.
- **[C4]** Public `RecoveryRequest` has no `trusted_internal` bool; sealed
  `RecoveryAuthority`/`PreparedRecovery` derived from exact durable state and
  descriptor-bound `WorkspaceAuthorization`, revalidated in CAS transaction.
- **[C5]** Prepare outside writer tx; reserve in short IMMEDIATE tx; execute
  external work with no tx; finalize in short IMMEDIATE tx.
- **[C6]** Recovery policy consumes canonical `StepDef`/explicit declaration plus
  exact current `SAFE_RERUN_STEPS` classifications; generic shell/write_file
  default `NonRecoverable` unless a specific canonical step explicitly opts into
  `ContinueWorkspace`.
- **[C7]** Complete effect-intent state machine: stable unique effect key,
  operation/attempt/sequence binding, canonical payload and digest/version,
  expected target/predecessor, observed result, Prepared/Completed/Conflict,
  committed before effect, effect-specific external reconciliation, guarded
  finalize; wired commit/push/open PR/merge call-site phases.
- **[C8]** Capsule has explicit canonicalization/schema/domain version and ONE
  envelope digest over all replay authority fields; component digests are
  metadata. Object-safe adapter via `fn version()`.
- **[C9]** Legacy salvage: every run without a valid pre-execution V1 capsule is
  salvage-only regardless of provenance/migration source.
- **[C10]** Strategy-specific merge proof enum: merge commit has two ancestry
  checks; squash and rebase include ancestry plus computed expected/observed
  content or patch evidence; `result_sha` strategy-neutral.
- **[C11]** Typed merge: external verification then short IMMEDIATE atomic
  artifact+status transaction; explicit allowed predecessor, affected-row check,
  exact idempotent retry; normal merge-required flow must not first write
  Completed.
- **[C12]** Protocol owns prepare/reserve/execute/finalize and cannot return
  `Recovered` before runner outcome is finalized.
- **[C13]** P16 failpoint/concurrency expectations updated (see P16).

#### Remediation cycle 2 (13 refinements `[B1]`–`[B13]`)

- **[B1]** `PreparedRecovery` preserves exact run/status/current-step/live-PID/
  checkpoint/wait/lease authority and reselect/revalidate inside reserve before
  mutation.
- **[B2]** One caller `expected_epoch` in `RecoveryRequest`; ONE single CAS at
  reserve only; no finalize CAS.
- **[B3]** Separate normalized logical request uniqueness/conflict binding from
  exact operation ID; `Pending` has guarded owner/lease claim so one process
  executes/reconciles.
- **[B4]** Allocate durable `execution_attempt_id` at reserve before effects,
  with attempt-start record; append immutable outcome snapshot at finalize or
  durable runner-result record recoverable after execute-before-finalize crash.
- **[B5]** Effect prepare is insert-or-load exact-binding comparison; conflict
  state on mismatch; merge intent completed/conflicted in atomic merge
  transaction.
- **[B6]** `PreparedRecovery` owns a real retained `VerifiedWorkspace`/
  `OwnedFd`-compatible anchor via existing `adjudicate_workspace_ownership` APIs;
  reserve does descriptor-relative marker re-snapshot plus exact identity
  comparison.
- **[B7]** `StepDef.recovery_policy` schema/canonicalizer/validation changes in
  capsule milestone (P06–P08) before protocol; policy persisted in canonical
  workflow bytes.
- **[B8]** Map actual launch/resume surfaces `app/run.rs`, `app/daemon_run.rs`,
  `parent_orchestration/child_workflow.rs`/`child_run.rs`,
  `app/runs/continuation_execution.rs`; capsule-before-run-row atomic ordering.
- **[B9]** Framed canonical envelope byte format with fixed supported schema/
  canonicalization/domain/provenance versions and fail-closed dispatch.
- **[B10]** Prohibit historical capsule backfill; every run without a capsule
  written by fresh launch before execution is salvage-only.
- **[B11]** Merge verifier takes authoritative injected Git/remote interfaces and
  bound repo/PR/base/head; computes evidence itself.
- **[B12]** Merge artifact DDL/init, exact artifact equality, capsule lookup/join,
  fixed allowed predecessor `ReviewReady`, modify runner completion semantics for
  merge-required workflows.
- **[B13]** This document is the sole pseudocode source; phase docs reference
  exact component-local line numbers only.

---

## Component: Recovery Epoch (`src/persistence/recovery_epoch.rs`) — [C1]

Distinct durable per-run epoch. NOT derived from `MAX(attempt generation)`.
Advanced via CAS inside a short IMMEDIATE transaction. No synthetic attempt rows
are ever appended to bump the epoch.

```
01 // recovery_epoch — single durable row per run holding the current epoch.
02 // CREATE TABLE IF NOT EXISTS recovery_epoch (
03 //   run_id TEXT PRIMARY KEY,
04 //   epoch INTEGER NOT NULL DEFAULT 0,
05 //   updated_at TEXT NOT NULL
06 // )
07
08 // Read the current epoch (read-only). Returns 0 for a new run with no row.
09 pub fn read_epoch(conn, run_id: &str) -> Result<u64, EpochError> {
10     SELECT COALESCE(epoch, 0) FROM recovery_epoch WHERE run_id = ?1   // line 10
11 }
12
13 // CAS claim: advance the epoch only if expected_epoch matches the persisted
14 // value. Called inside the protocol's short IMMEDIATE reserve transaction.
15 // This is the ONLY CAS in the recovery protocol. [B2]
16 // Checks affected rows so a concurrent advance is detected.
17 pub fn cas_advance_epoch(
18     tx,
19     run_id: &str,
20     expected_epoch: u64,
21 ) -> Result<CasOutcome, EpochError> {
22     INSERT INTO recovery_epoch (run_id, epoch, updated_at)             // line 22
23       VALUES (?1, ?2 + 1, ?3)
24       ON CONFLICT(run_id) DO UPDATE SET
25         epoch = recovery_epoch.epoch + 1,
26         updated_at = excluded.updated_at
27       WHERE recovery_epoch.epoch = ?2                                  // line 27 — CAS guard
28     let affected = count_rows_affected()                               // line 28
29     if affected == 0 {                                                 // line 29
30         // A concurrent claim advanced the epoch from expected_epoch.
31         // The insert path also advances a previously absent epoch 0 to 1.
32         let persisted = read_epoch(tx, run_id)?                        // line 32
33         return Ok(CasOutcome::Stale { persisted, expected: expected_epoch })  // line 33
34     }
35     Ok(CasOutcome::Advanced { from: expected_epoch, to: expected_epoch + 1 })  // line 35
36 }
37
38 pub enum CasOutcome {
39     Advanced { from: u64, to: u64 },
40     Stale { persisted: u64, expected: u64 },
41 }
```

---

## Component: Recovery Operations Ledger (`src/persistence/recovery_operations.rs`) — [C2/B3/B4]

Idempotent operation ledger. Each recovery is a durable operation. **[B3]**
separates the normalized *logical request key* (uniqueness/conflict binding)
from the exact `operation_id` (durable row identity). A `Pending` operation
carries a guarded owner/lease claim so exactly one process executes or
reconciles.

**[B4]** A durable `execution_attempt_id` is allocated at reserve (before
effects) and recorded in an attempt-start row. The outcome snapshot is appended
at finalize, or a durable runner-result record is recoverable after an
execute-before-finalize crash.

```
01 // recovery_operations — idempotent ledger of recovery operations. [C2/B3]
02 // CREATE TABLE IF NOT EXISTS recovery_operations (
03 //   operation_id TEXT PRIMARY KEY,             -- durable row identity [B3]
04 //   run_id TEXT NOT NULL,
05 //   epoch INTEGER NOT NULL,                    -- epoch at which this op was reserved
06 //   step_id TEXT NOT NULL,
07 //   capsule_envelope_digest TEXT NOT NULL,     -- exact capsule binding [C3/C8]
08 //   source_attempt_id INTEGER,                 -- exact source attempt (nullable for fresh)
09 //   logical_request_key TEXT NOT NULL UNIQUE,  -- one operation per logical request [B3]
10 //   intent_digest TEXT NOT NULL,                -- normalized operator intent binding
10A//   status TEXT NOT NULL DEFAULT 'pending',    -- 'pending'|'completed'|'refused'|'conflict'
11 //   owner_pid INTEGER,                         -- guarded owner claim for Pending [B3]
12 //   lease_expires_at TEXT,                     -- guarded lease claim for Pending [B3]
13 //   execution_attempt_id INTEGER,              -- allocated at reserve [B4]
14 //   serialized_outcome TEXT,                   -- JSON of prior outcome (set at finalization)
15 //   created_at TEXT NOT NULL,
16 //   finalized_at TEXT
17 // )
18
19 // Compute the exact operation_id (durable row identity). [B3]
20 // Binds: normalized intent plus run, step, capsule, and source attempt.
21 // This is the row PRIMARY KEY and prevents different operator verbs from aliasing.
22 pub fn compute_operation_id(
23     run_id: &str,
24     step_id: &str,
25     capsule_envelope_digest: &str,
25A    source_attempt_id: Option<i64>,
25B    normalized_intent: &RecoveryIntent,
26 ) -> String {
27     sha256_hex(canonical_concat(run_id, step_id, capsule_envelope_digest, source_attempt_id, normalized_intent))  // line 27
28 }
29
30 // Compute the normalized logical request key (uniqueness/conflict binding). [B3]
31 // This is SEPARATE from operation_id. It binds the normalized operator intent
32 // and logical target (run + source attempt), independent of capsule/step details.
33 // A second request for that target with different exact bindings is a conflict.
34 // The UNIQUE constraint on logical_request_key makes the check race-safe.
35 pub fn compute_logical_request_key(
36     run_id: &str,
37     source_attempt_id: Option<i64>,
38     normalized_intent: &RecoveryIntent,
39 ) -> String {
40     sha256_hex(canonical_concat(run_id, source_attempt_id, normalized_intent))  // line 40
41 }
42
43 // Look up the single operation for the logical request. Exact operation_id,
44 // intent_digest, capsule, step, and source bindings are compared by reserve.
45 pub fn lookup_logical_operation(tx, logical_request_key: &str) -> Result<Option<RecoveryOperation>> {
46     SELECT * FROM recovery_operations WHERE logical_request_key = ?1    // line 46
47 }
48
48A// Find a Pending operation for the same logical_request_key with an expired
49 // or missing lease, so a reconciler can adopt it. [B3]
50 pub fn find_adoptable_pending(tx, logical_request_key: &str, now: DateTime<Utc>) -> Result<Option<RecoveryOperation>> {
51     SELECT * FROM recovery_operations                                    // line 51
52       WHERE logical_request_key = ?1 AND status = 'pending'
53         AND (lease_expires_at IS NULL OR lease_expires_at < ?2)
54     ORDER BY created_at ASC LIMIT 1
55 }
56
57 // Insert a new pending operation with a guarded owner/lease claim. [B3/B4]
58 // Called inside the reserve tx AFTER the epoch CAS and AFTER allocating
59 // execution_attempt_id. The owner_pid + lease_expires_at form a guarded
60 // claim so exactly one process may execute/reconcile.
61 pub fn insert_pending(
62     tx,
63     operation_id: &str,
64     run_id: &str,
65     epoch: u64,
66     step_id: &str,
67     capsule_envelope_digest: &str,
68     source_attempt_id: Option<i64>,
69     logical_request_key: &str,
69A    intent_digest: &str,
70     owner_pid: u32,
71     lease_expires_at: DateTime<Utc>,
72     execution_attempt_id: i64,
73 ) -> Result<()> {
74     INSERT INTO recovery_operations
75       (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
76        source_attempt_id, logical_request_key, intent_digest, status, owner_pid,
77        lease_expires_at, execution_attempt_id, created_at)
78     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10, ?11, ?12)  // line 78
79 }
80
81 // Attempt to adopt an existing Pending operation whose lease has expired. [B3]
82 // Guarded: only transitions if the lease is still expired inside this tx.
83 pub fn try_adopt_pending(
84     tx,
85     operation_id: &str,
86     new_owner_pid: u32,
87     new_lease_expires_at: DateTime<Utc>,
88     now: DateTime<Utc>,
89 ) -> Result<AdoptOutcome> {
90     UPDATE recovery_operations                                          // line 90
91       SET owner_pid = ?3, lease_expires_at = ?4
92       WHERE operation_id = ?1 AND status = 'pending'
93         AND (lease_expires_at IS NULL OR lease_expires_at < ?5)
94     let affected = count_rows_affected()                                // line 94
95     if affected == 1 { Ok(AdoptOutcome::Adopted) }                      // line 95
96     else { Ok(AdoptOutcome::StillOwned) }                               // line 96
97 }
98
99 // Finalize an operation as completed with a serialized outcome. [C2]
100 // Guarded: only transitions from 'pending'.
101 pub fn finalize_completed(
102     tx,
103     operation_id: &str,
104     serialized_outcome: &str,
105 ) -> Result<i64> {
106     UPDATE recovery_operations                                         // line 106
107       SET status = 'completed', serialized_outcome = ?2, finalized_at = ?3
108       WHERE operation_id = ?1 AND status = 'pending'                   // line 108 — guard
109     let affected = count_rows_affected()                               // line 109
110     if affected != 1 { return Err(RecoveryOpsError::GuardFailed { operation_id, affected }) }  // line 110
111     SELECT attempt_id_from_outcome(serialized_outcome)                  // line 111
112 }
113
114 // Finalize an operation as refused. [C2]
115 pub fn finalize_refused(tx, operation_id: &str, reason: &str) -> Result<()> {
116     UPDATE recovery_operations
117       SET status = 'refused', serialized_outcome = ?2, finalized_at = ?3
118       WHERE operation_id = ?1 AND status = 'pending'
119     let affected = count_rows_affected()
120     if affected != 1 { return Err(RecoveryOpsError::GuardFailed { operation_id, affected }) }
121 }
122
123 // Finalize an operation as conflict. [C2]
124 pub fn finalize_conflict(tx, operation_id: &str, detail: &str) -> Result<()> {
125     UPDATE recovery_operations
126       SET status = 'conflict', serialized_outcome = ?2, finalized_at = ?3
127       WHERE operation_id = ?1 AND status = 'pending'
128     let affected = count_rows_affected()
129     if affected != 1 { return Err(RecoveryOpsError::GuardFailed { operation_id, affected }) }
130 }
131
132 pub enum AdoptOutcome { Adopted, StillOwned }
133
134 pub struct RecoveryOperation {
135     operation_id: String,
136     run_id: String,
137     epoch: u64,
138     step_id: String,
139     capsule_envelope_digest: String,
140     source_attempt_id: Option<i64>,
141     logical_request_key: String,       // [B3]
141A    intent_digest: String,              // normalized RecoveryIntent binding
142     status: OperationStatus,
143     owner_pid: Option<u32>,            // [B3]
144     lease_expires_at: Option<DateTime<Utc>>,  // [B3]
145     execution_attempt_id: Option<i64>, // [B4]
146     serialized_outcome: Option<String>,
147 }
148
149 pub enum OperationStatus { Pending, Completed, Refused, Conflict }
```

---

## Component: Append-Only Attempts (`src/persistence/attempts.rs`) — [C3/B4]

Append-only attempt rows with complete `StateSnapshot`, immutable IDs, and
capsule binding. No row is ever updated; history is preserved.

**[B4]** A durable `execution_attempt_id` is allocated at reserve (before any
effect) and recorded via `record_attempt_start`. The outcome snapshot is
appended at finalize via `append_attempt_outcome`. If the process crashes
between execute and finalize, the durable runner-result record (in the attempt
row's `runner_result_json`) is recoverable so a reconciler can detect that
execution completed without a finalized outcome.

```
01 // recovery_attempts — append-only, complete per-attempt record. [C3/B4]
02 // CREATE TABLE IF NOT EXISTS recovery_attempts (
03 //   attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
04 //   run_id TEXT NOT NULL,
05 //   epoch INTEGER NOT NULL,
06 //   source_attempt_id INTEGER,               -- parent attempt this recovers from [C3]
07 //   operation_id TEXT NOT NULL,              -- bound to recovery operation [B4]
08 //   step_id TEXT NOT NULL,
08A//   step_status TEXT NOT NULL,               -- 'started'|'resumed'|'interrupted'|'completed'|'failed' [B4]
09 //   capsule_schema_version INTEGER NOT NULL,  -- [C3/C8]
10 //   capsule_envelope_digest TEXT NOT NULL,    -- exact capsule binding [C3/C8]
11 //   state_snapshot_json TEXT NOT NULL,        -- complete StateSnapshot [C3]
12 //   snapshot_digest TEXT NOT NULL,            -- SHA-256 of canonical state_snapshot_json [C3]
13 //   checkpoint_digest TEXT,                   -- digest of checkpoint referenced (nullable) [C3]
14 //   runner_result_json TEXT,                  -- durable runner result (recoverable after crash) [B4]
15 //   started_at TEXT NOT NULL,                 -- attempt-start timestamp [B4]
16 //   finalized_at TEXT                         -- NULL until outcome appended [B4]
17 // )
18
19 // Record the attempt START at reserve, before any effect. [B4]
20 // Allocates the durable execution_attempt_id and writes a 'started' row with
21 // finalized_at = NULL. This row proves the attempt was reserved even if the
22 // process crashes before finalize.
23 pub fn record_attempt_start(
24     tx,
25     run_id: &str,
26     epoch: u64,
27     source_attempt_id: Option<i64>,
28     operation_id: &str,
29     step_id: &str,
30     capsule_schema_version: u32,
31     capsule_envelope_digest: &str,
32     state_snapshot: &StateSnapshot,
33 ) -> Result<i64, AttemptError> {
34     let snapshot_json = canonical_serialize(state_snapshot)            // line 34
35     let snapshot_digest = sha256_hex(&snapshot_json)                   // line 35
36     INSERT INTO recovery_attempts
37       (run_id, epoch, source_attempt_id, operation_id, step_id, step_status,
38        capsule_schema_version, capsule_envelope_digest,
39        state_snapshot_json, snapshot_digest, started_at)
40     VALUES (?1, ?2, ?3, ?4, ?5, 'started', ?6, ?7, ?8, ?9, ?10)       // line 40
41     RETURNING attempt_id                                               // line 41
42 }
43
44 // Append the immutable outcome snapshot at finalize. [B4]
45 // Updates the existing attempt row's step_status + runner_result + finalized_at.
46 // This is the ONLY non-append mutation on recovery_attempts: it completes a row
47 // that was already inserted at reserve. It is guarded by finalized_at IS NULL.
48 pub fn append_attempt_outcome(
49     tx,
50     attempt_id: i64,
51     step_status: &str,
52     state_snapshot: &StateSnapshot,
53     runner_result: Option<&serde_json::Value>,
54     checkpoint_digest: Option<&str>,
55 ) -> Result<(), AttemptError> {
56     let snapshot_json = canonical_serialize(state_snapshot)            // line 56
57     let snapshot_digest = sha256_hex(&snapshot_json)                   // line 57
58     UPDATE recovery_attempts                                           // line 58
59       SET step_status = ?2, state_snapshot_json = ?3, snapshot_digest = ?4,
60           runner_result_json = ?5, checkpoint_digest = ?6, finalized_at = ?7
61       WHERE attempt_id = ?1 AND finalized_at IS NULL                   // line 61 — guard
62     let affected = count_rows_affected()                               // line 62
63     if affected != 1 { return Err(AttemptError::OutcomeAlreadyAppended) }  // line 63
64     Ok(())
65 }
66
66 // Load the latest attempt for a run+step (by monotonic attempt_id).
67 pub fn latest_for_step(conn, run_id: &str, step_id: &str) -> Result<Option<AttemptRow>> {
68     SELECT * FROM recovery_attempts
69     WHERE run_id = ?1 AND step_id = ?2
70     ORDER BY attempt_id DESC LIMIT 1                                  // line 70
71 }
72
73 // Load a specific attempt by ID.
74 pub fn load_attempt(conn, attempt_id: i64) -> Result<AttemptRow> {
75     SELECT * FROM recovery_attempts WHERE attempt_id = ?1              // line 75
76 }
77
78 // Load an attempt by operation_id that was started but never finalized. [B4]
79 // Recoverable after an execute-before-finalize crash.
80 pub fn load_unfinalized_for_operation(conn, operation_id: &str) -> Result<Option<AttemptRow>> {
81     SELECT * FROM recovery_attempts                                    // line 81
82       WHERE operation_id = ?1 AND finalized_at IS NULL
83     ORDER BY attempt_id DESC LIMIT 1
84 }
85
86 // Verify the snapshot digest of a loaded attempt row.
87 pub fn verify_snapshot_digest(row: &AttemptRow) -> Result<(), AttemptError> {
88     let recomputed = sha256_hex(&canonical_serialize(&row.state_snapshot))  // line 88
89     if recomputed != row.snapshot_digest {                             // line 89
90         return Err(AttemptError::SnapshotDigestMismatch)               // line 90
91     }
92     Ok(())                                                            // line 92
93 }
94
95 pub struct AttemptRow {
96     attempt_id: i64,
97     run_id: String,
98     epoch: u64,
99     source_attempt_id: Option<i64>,
100     operation_id: String,            // [B4]
101     step_id: String,
102     step_status: String,
103     capsule_schema_version: u32,
104     capsule_envelope_digest: String,
105     state_snapshot: StateSnapshot,
106     snapshot_digest: String,
107     checkpoint_digest: Option<String>,
108     runner_result_json: Option<serde_json::Value>,  // [B4]
109     started_at: DateTime<Utc>,                      // [B4]
110     finalized_at: Option<DateTime<Utc>>,            // [B4]
111 }
```

---

## Component: ExecutionCapsuleV1 (`src/engine/recovery/capsule.rs`) — [C8/B9]

Immutable canonical launch record with ONE envelope digest over all replay
authority fields. Component digests are metadata, not authority.

**[B9]** The envelope digest is computed over a **framed canonical envelope
byte format** with fixed supported schema/canonicalization/domain/provenance
versions. The adapter dispatch is fail-closed: an unsupported version is
rejected before any step executes.

```
01 // ExecutionCapsuleV1 — immutable canonical launch record. [C8/B9]
02 pub struct ExecutionCapsuleV1 {
03     // --- Versioning ---
04     schema_version: u32,                // capsule format version (always 1)
05     canonicalization_version: u32,      // canonicalization algorithm version
06     domain_version: u32,                // workflow/config domain schema version
07     provenance_version: u32,            // LaunchProvenance canonical digest version [B9]
08     // --- Replay authority fields (all covered by envelope_digest) ---
09     run_id: String,
10     config_root_encoding: String,       // canonical config root, encoded
11     resolved_workflow_bytes: Vec<u8>,   // canonical serialization of resolved WorkflowType
12     resolved_config_bytes: Vec<u8>,     // canonical serialization of resolved WorkflowConfig
13     launch_provenance_digest: String,   // canonical digest of actual LaunchProvenance
14     base_ref: String,
15     // --- Envelope digest (THE authority) ---
16     envelope_digest: String,            // SHA-256 over framed canonical envelope [B9]
17     // --- Component digests (metadata only, NOT authority) ---
18     workflow_digest: String,            // SHA-256 of resolved_workflow_bytes
19     config_digest: String,              // SHA-256 of resolved_config_bytes
20     // ---
21     created_at: DateTime<Utc>,
22 }
23
24 // Fixed supported version constants. [B9]
25 pub const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1];
26 pub const SUPPORTED_CANONICALIZATION_VERSIONS: &[u32] = &[1];
27 pub const SUPPORTED_DOMAIN_VERSIONS: &[u32] = &[1];
28 pub const SUPPORTED_PROVENANCE_VERSIONS: &[u32] = &[1];
29
30 // Build the framed canonical envelope byte format. [B9]
31 // The frame is a deterministic byte sequence with fixed-width version fields
32 // followed by length-prefixed authority fields, so the digest is stable and
33 // unambiguous regardless of serialization library key ordering.
34 //
35 // Frame layout (all integers big-endian u32):
36 //   [schema_version][canonicalization_version][domain_version][provenance_version]
37 //   [len(run_id)][run_id bytes]
38 //   [len(config_root_encoding)][config_root_encoding bytes]
39 //   [len(resolved_workflow_bytes)][resolved_workflow_bytes]
40 //   [len(resolved_config_bytes)][resolved_config_bytes]
41 //   [len(launch_provenance_digest)][launch_provenance_digest bytes]
42 //   [len(base_ref)][base_ref bytes]
43 pub fn build_envelope_frame(capsule_fields: &CapsuleAuthorityFields) -> Vec<u8> {
44     let mut buf = Vec::new();                                           // line 44
45     buf.extend(schema_version.to_be_bytes());                          // line 45
46     buf.extend(canonicalization_version.to_be_bytes());               // line 46
47     buf.extend(domain_version.to_be_bytes());                         // line 47
48     buf.extend(provenance_version.to_be_bytes());                     // line 48
49     write_len_prefixed(&mut buf, run_id);                            // line 49
50     write_len_prefixed(&mut buf, config_root_encoding);              // line 50
51     write_len_prefixed(&mut buf, resolved_workflow_bytes);           // line 51
52     write_len_prefixed(&mut buf, resolved_config_bytes);             // line 52
53     write_len_prefixed(&mut buf, launch_provenance_digest);          // line 53
54     write_len_prefixed(&mut buf, base_ref);                         // line 54
55     buf                                                                // line 55
56 }
57
58 // Build from a freshly resolved type+config+provenance (launch surfaces only).
59 pub fn build_capsule_v1(
60     run_id: String,
61     workflow_type: &WorkflowType,
62     config: &WorkflowConfig,
63     config_root: &Path,
64     launch_provenance: &LaunchProvenance,
65     base_ref: String,
66 ) -> Result<ExecutionCapsuleV1, CapsuleError> {
67     // Fail-closed version check before building. [B9]
68     if !SUPPORTED_SCHEMA_VERSIONS.contains(&1) { return Err(CapsuleError::UnsupportedSchema(1)) }  // line 68
69     canonicalize config_root -> PathBuf                                 // line 69
70     encode config root -> config_root_encoding                         // line 70
71     compute resolved_workflow_bytes = canonicalize_workflow_type(workflow_type)  // line 71
72     compute resolved_config_bytes = canonicalize_workflow_config(config)  // line 72
73     compute launch_provenance_digest = compute_provenance_digest(launch_provenance)  // line 73
74     compute workflow_digest = sha256_hex(&resolved_workflow_bytes)     // line 75
75     compute config_digest = sha256_hex(&resolved_config_bytes)         // line 76
76     let frame = build_envelope_frame(&authority_fields)                // line 77
77     compute envelope_digest = sha256_hex(&frame)                       // line 78
78     return ExecutionCapsuleV1 { schema_version: 1, canonicalization_version: 1,
79         domain_version: 1, provenance_version: 1, envelope_digest, ... }  // line 79
80 }
81
82 // Verify the ONE envelope digest over the framed canonical envelope. [C8/B9]
83 pub fn verify_envelope_digest(capsule: &ExecutionCapsuleV1) -> Result<(), CapsuleError> {
84     // Fail-closed version dispatch. [B9]
85     if !SUPPORTED_SCHEMA_VERSIONS.contains(&capsule.schema_version) {   // line 85
86         return Err(CapsuleError::UnsupportedSchema(capsule.schema_version))  // line 86
87     }
88     if !SUPPORTED_CANONICALIZATION_VERSIONS.contains(&capsule.canonicalization_version) {  // line 88
89         return Err(CapsuleError::UnsupportedCanonicalization(capsule.canonicalization_version))  // line 89
90     }
91     if !SUPPORTED_DOMAIN_VERSIONS.contains(&capsule.domain_version) {   // line 91
92         return Err(CapsuleError::UnsupportedDomain(capsule.domain_version))  // line 92
93     }
94     if !SUPPORTED_PROVENANCE_VERSIONS.contains(&capsule.provenance_version) {  // line 94
95         return Err(CapsuleError::UnsupportedProvenance(capsule.provenance_version))  // line 95
96     }
97     let frame = build_envelope_frame(&capsule.authority_fields())       // line 97
98     let recomputed = sha256_hex(&frame)                                // line 98
99     if recomputed != capsule.envelope_digest {                         // line 99
100         return Err(CapsuleError::EnvelopeDigestMismatch)              // line 100
101     }
102     Ok(())                                                           // line 102
103 }
104
105 pub enum CapsuleError {
106     EnvelopeDigestMismatch,
107     UnsupportedSchema(u32),            // [B9]
108     UnsupportedCanonicalization(u32),  // [B9]
109     UnsupportedDomain(u32),            // [B9]
110     UnsupportedProvenance(u32),        // [B9]
111     Canonicalize { config_root: PathBuf, io_error: String },
112     InvalidEncoding { encoded: String, reason: String },
113 }
```

---

## Component: CapsuleAdapter (`src/engine/recovery/adapters/mod.rs` + `v1.rs`) — [C8/B9]

Object-safe adapter trait using `fn version(&self) -> u32` instead of `const
VERSION`. This allows `dyn CapsuleAdapter` dispatch. **[B9]** dispatch is
fail-closed: unsupported versions error before any step executes.

```
01 // CapsuleAdapter — object-safe, versioned capsule execution. [C8/B9]
02 pub trait CapsuleAdapter {
03     fn version(&self) -> u32;                                          // line 03 — object-safe
04     fn step_def_for(&self, capsule: &ExecutionCapsuleV1, step_id: &str) -> Result<StepDef, AdapterError>;
05     fn build_instance(&self, capsule: &ExecutionCapsuleV1) -> Result<WorkflowInstance, AdapterError>;
06     fn envelope_digest<'a>(&'a self, capsule: &'a ExecutionCapsuleV1) -> &'a str;
07 }
08
09 // Registry: schema_version -> adapter. Fail-closed dispatch. [B9]
10 pub fn adapter_for(capsule: &ExecutionCapsuleV1) -> Result<Box<dyn CapsuleAdapter>, AdapterError> {
11     match capsule.schema_version {                                     // line 11
12         1 => Ok(Box::new(V1Adapter)),                                 // line 12
13         v => Err(AdapterError::UnsupportedCapsuleVersion(v)),         // line 13 — fail-closed
14     }
15 }
16
17 pub enum AdapterError {
18     UnsupportedCapsuleVersion(u32),
19     StepNotFound { step_id: String },
20 }
```

---

## Component: StepRecoveryPolicy (`src/engine/recovery/policy.rs`) — [C6/B7]

Consumes canonical `StepDef`/explicit declaration plus exact current
`SAFE_RERUN_STEPS` classifications. Generic shell/write_file defaults
`NonRecoverable` unless a specific canonical step explicitly opts into
`ContinueWorkspace`.

**[B7]** The `recovery_policy` field is added to `StepDef` (schema +
canonicalizer + validation) in the capsule milestone (P06–P08) BEFORE the
protocol (P09–P11). The policy is persisted in the canonical workflow bytes so
the capsule carries it.

```
01 // StepRecoveryPolicy — selects the recovery strategy for a canonical step.
02 pub enum StepRecoveryPolicy {
03     PureReenter,            // safe to re-run from scratch (no side effects)
04     Idempotent,             // re-running yields identical effects
05     ReconcileThenReenter,   // reconcile observed state, then re-enter
06     ContinueWorkspace,      // resume in-place after exact verification
07     CompensateThenRetry,    // undo prior partial effect, then retry
08     NonRecoverable,         // fail closed; no recovery possible
09 }
10
11 // StepDef.recovery_policy field (added in P06–P08 capsule milestone). [B7]
12 // In src/workflow/schema.rs StepDef gains:
13 //   /// Explicit recovery policy declared on the canonical step definition.
14 //   /// When present, takes precedence over SAFE_RERUN_STEPS classification.
15 //   /// Persisted in canonical workflow bytes (canonicalize_workflow_type). [B7]
16 //   pub recovery_policy: Option<StepRecoveryPolicy>,
17 // The canonicalizer (canonicalize_workflow_type) includes this field so the
18 // capsule envelope digest covers it. Validation rejects unknown variants.

19 // Resolve the policy for a canonical step. [C6/B7]
20 // Consumes the StepDef (canonical step definition, including the optional
21 // recovery_policy field added in B7) plus exact current SAFE_RERUN_STEPS
22 // classifications. Generic shell/write_file defaults to NonRecoverable unless
22A// the step explicitly opts into ContinueWorkspace.
23 pub fn policy_for_step(step_def: &StepDef) -> StepRecoveryPolicy {
24     // 1. Explicit declaration on the StepDef takes precedence. [C6/B7]
25     if let Some(declared) = &step_def.recovery_policy {               // line 25
26         return declared.clone()                                        // line 26
27     }
28     // 2. SAFE_RERUN_STEPS classification (by step_id, not step_type). [C6]
29     if is_safe_rerun_step(&step_def.step_id) {                        // line 29
30         return StepRecoveryPolicy::Idempotent                          // line 30
31     }
32     // 3. Generic shell/write_file defaults to NonRecoverable. [C6]
33     match step_def.step_type.as_str() {                                // line 33
34         _ => StepRecoveryPolicy::NonRecoverable,                       // line 34
35     }
36 }
37
38 // SAFE_RERUN_STEPS — exact current classifications from continuation.rs.
39 // These are step_id values, not step_type values:
40 //   "watch_pr_checks", "collect_ci_failures", "collect_coderabbit_feedback",
41 //   "capture_pr_identity", "post_pr_iteration_guard"
42 // is_safe_rerun_step(step_id) delegates to the existing constant.

43 // Select the concrete strategy from a policy. [C4/C6]
44 // No trusted_internal bool: authorization is handled by the sealed
45 // RecoveryAuthority (descriptor-bound WorkspaceAuthorization, verified
46 // during prepare and revalidated during reserve).
47 pub fn select_strategy(policy: StepRecoveryPolicy) -> RecoveryStrategy {
48     match policy {                                                     // line 47
49         PureReenter => RecoveryStrategy::Reenter,                      // line 49
50         Idempotent => RecoveryStrategy::Reenter,                       // line 50
51         ReconcileThenReenter => RecoveryStrategy::ReconcileThenReenter,  // line 51
52         ContinueWorkspace => RecoveryStrategy::ContinueWorkspace,     // line 52
53         CompensateThenRetry => RecoveryStrategy::CompensateThenRetry,  // line 53
54         NonRecoverable => RecoveryStrategy::Refused(RefusalReason::NonRecoverable),  // line 54
55     }
56 }
```

---

## Component: RecoveryProtocolV1 (`src/engine/recovery/protocol.rs`) — [C1/C2/C4/C5/C12/B1/B2/B4/B6]

Single typed recovery entry point with phased execution:
prepare (no tx) → reserve (short IMMEDIATE tx) → execute (no tx) → finalize
(short IMMEDIATE tx). Cannot return `Recovered` before runner outcome is
finalized.

**[B1]** `PreparedRecovery` preserves exact run/status/current-step/live-PID/
checkpoint/wait/lease authority and reselect/revalidate inside reserve before
mutation.

**[B2]** One caller `expected_epoch` in `RecoveryRequest`; ONE single CAS at
reserve only; no finalize CAS.

**[B4]** Durable `execution_attempt_id` allocated at reserve before effects;
attempt-start record written; outcome appended at finalize.

**[B6]** `PreparedRecovery` owns a real retained `VerifiedWorkspace` anchor via
`adjudicate_workspace_ownership`; reserve does descriptor-relative marker
re-snapshot plus exact identity comparison.

```
01 // RecoveryRequest — single typed recovery entry. [C4/B2]
02 // NO trusted_internal bool. [B2] carries the caller's expected_epoch.
03 pub struct RecoveryRequest {
04     run_id: String,
05     step_id: String,
06     expected_epoch: u64,           // caller's view of current epoch [B2]
07     operator_verb: OperatorVerb,   // Resume | Retry | Rewind (CLI-facing only)
08 }
09
10 // Sealed RecoveryAuthority — derived from exact durable state +
11 // descriptor-bound WorkspaceAuthorization. [C4/B6]
12 // Cannot be constructed outside this module.
13 pub struct RecoveryAuthority {
14     _sealed: Sealed,               // prevents external construction [C4]
15     workspace_authorization: WorkspaceAuthorization,  // descriptor-bound
16     capsule: ExecutionCapsuleV1,
17     source_attempt: Option<AttemptRow>,
18     policy: StepRecoveryPolicy,
19     strategy: RecoveryStrategy,
20 }
21
22 // PreparedRecovery — output of the prepare phase (no transaction). [C5/B1/B6]
23 // Preserves exact run/status/current-step/live-PID/checkpoint/wait/lease
24 // authority so reserve can reselect/revalidate before mutation. [B1]
25 // Owns a real retained VerifiedWorkspace anchor (not a borrowed fd). [B6]
26 pub struct PreparedRecovery {
27     authority: RecoveryAuthority,
28     expected_epoch: u64,           // caller's expected epoch [B2]
29     operation_id: String,          // exact operation id [B3]
30     logical_request_key: String,   // normalized logical request key [B3]
31     // [B1] exact authority snapshot captured during prepare:
32     run_status: RunStatus,          // exact run status at prepare time
33     current_step: Option<String>,   // exact current step at prepare time
34     live_pid: Option<u32>,          // exact live PID at prepare time
35     checkpoint_identity: Option<CheckpointIdentity>,  // exact checkpoint
36     wait_state: Option<WaitState>,  // exact wait state
37     lease: Option<LeaseState>,      // exact lease state
38     // [B6] retained anchor (OwnedFd-compatible) for descriptor-relative revalidation:
39     verified_workspace: VerifiedWorkspace,  // retained anchor from adjudicate
40 }
41
42 // RecoveryOutcome — result of recovery dispatch.
43 pub enum RecoveryOutcome {
44     Recovered { resumed_at_step: String, attempt_id: i64, operation_id: String },
45     AlreadyApplied { prior_outcome: SerializedOutcome, attempt_id: i64 },  // [C2]
46     Refused { reason: RefusalReason },
47     StaleEpoch { persisted: u64, expected: u64 },  // [C1/B2]
48     Conflict { detail: String },                   // [C2/B3]
49 }
50
51 pub enum OperatorVerb { Resume, Retry, Rewind }
52
53 pub enum RefusalReason {
54     NonRecoverable,
55     VerificationFailed(String),
56     NotAuthorized,         // [C4/B6]
57     SalvageOnly,
58 }
59
60 pub enum RecoveryStrategy {
61     ContinueWorkspace,
62     Reenter,
63     ReconcileThenReenter,
64     CompensateThenRetry,
65     Refused(RefusalReason),
66 }
67
68 // ──────────────────────────────────────────────────────────────────────
69 // The single typed recovery entry point. [C5/C12]
70 // Protocol owns prepare → reserve → execute → finalize.
71 // Cannot return Recovered before runner outcome is finalized.
72 pub fn recover(
73     conn: &Connection,
74     workspace: &Path,
75     request: &RecoveryRequest,
76 ) -> Result<RecoveryOutcome, RecoveryError> {
77     // --- Phase 1: PREPARE (no transaction) --- [C5]
78     let prepared = prepare_recovery(conn, workspace, request)?         // line 78
79
80     // --- Phase 2: RESERVE (short IMMEDIATE tx) --- [C5/B2]
81     // The ONLY CAS in the protocol is the epoch CAS inside reserve. [B2]
82     let reservation = reserve_recovery(conn, &prepared)?               // line 82
83     match &reservation.status {
84         ReserveStatus::CompletedDuplicate { prior_outcome, attempt_id } => {
85             // Exact completed duplicate: return prior outcome. [C2]
86             return Ok(RecoveryOutcome::AlreadyApplied {               // line 86
87                 prior_outcome: prior_outcome.clone(),
88                 attempt_id: *attempt_id,
89             })
90         }
91         ReserveStatus::ConflictDuplicate { detail } => {
92             // Conflicting duplicate: refuse. [C2/B3]
93             return Ok(RecoveryOutcome::Conflict { detail: detail.clone() })  // line 93
94         }
95         ReserveStatus::StaleEpoch { persisted, expected } => {
96             return Ok(RecoveryOutcome::StaleEpoch {                   // line 96
97                 persisted: *persisted, expected: *expected,
98             })
99         }
100         ReserveStatus::PendingOwned { .. } => {
101             return Ok(RecoveryOutcome::InProgress)
102         }
102A        ReserveStatus::PendingReconcile { .. }
102B        | ReserveStatus::NewlyReserved { .. } => {}
102C    }
103
104     // --- Phase 3: EXECUTE (no transaction; external work) --- [C5]
105     let execution = execute_recovery(&prepared, &reservation)?         // line 105
106
107     // --- Phase 4: FINALIZE (short IMMEDIATE tx) --- [C5]
108     // Cannot return Recovered before finalize commits. [C12]
109     // NO epoch CAS here — the single CAS is at reserve. [B2]
110     let finalized = finalize_recovery(conn, &prepared, &reservation, execution)?  // line 110
111
112     match finalized {
113         FinalizeResult::Finalized { attempt_id, .. } => {
114             Ok(RecoveryOutcome::Recovered {                           // line 114
115                resumed_at_step: request.step_id.clone(),
116                attempt_id,
117                operation_id: prepared.operation_id.clone(),
118             })
119         }
120         FinalizeResult::Refused { reason } => {
121             Ok(RecoveryOutcome::Refused { reason })                   // line 121
122         }
123     }
124 }
125
126 // ──────────────────────────────────────────────────────────────────────
127 // Phase 1: PREPARE — load durable state, verify capsule, resolve policy,
128 // derive sealed RecoveryAuthority, capture exact authority snapshot. [B1/B6]
129 // NO transaction. [C4/C5]
130 fn prepare_recovery(
131     conn: &Connection,
132     workspace: &Path,
133     request: &RecoveryRequest,
134 ) -> Result<PreparedRecovery, RecoveryError> {
135     let capsule = capsule_store::load_capsule_v1(conn, &request.run_id)?  // line 135
136     verify_envelope_digest(&capsule)?                               // line 136
137     let adapter = adapter_for(&capsule)?                           // line 137
138     let step_def = adapter.step_def_for(&capsule, &request.step_id)?  // line 138
139     let policy = policy_for_step(&step_def)?                       // line 139
140     let source_attempt = attempts::latest_for_step(conn, &request.run_id, &request.step_id)?  // line 140
141     // [B6] adjudicate workspace ownership via the existing consolidated kernel.
142     // This retains the VerifiedWorkspace anchor (OwnedFd-compatible) so the
143     // descriptor is held through reserve without reopening the path.
144     let verdict = adjudicate_workspace_ownership(workspace, &request.run_id)?  // line 144
145     let verified_workspace = match verdict {                         // line 145
146         OwnershipVerdict::Owned(v) => v,                            // line 146
147         OwnershipVerdict::NoEvidence | OwnershipVerdict::Rejected(_) => {  // line 147
148             return Err(RecoveryError::WorkspaceNotOwned)           // line 148
149         }
150     }
151     let workspace_auth = verified_workspace.authorization()         // line 151
152     let strategy = select_strategy(policy)?                        // line 152
153     let intent = RecoveryIntent::normalize(&request.operator_verb)?     // line 153
154     let source_id = source_attempt.as_ref().map(|a| a.attempt_id)
155     let operation_id = compute_operation_id(&request.run_id, &request.step_id,
155A        &capsule.envelope_digest, source_id, &intent)                    // [B3]
156     let logical_request_key = compute_logical_request_key(              // line 156
157         &request.run_id, source_id, &intent)                             // [B3]
158     // [B1] capture exact authority snapshot from durable state:
159     let run_md = run_metadata::load(conn, &request.run_id)?         // line 159
160     let run_status = run_md.status                                  // line 160
161     let current_step = run_md.current_step.clone()                  // line 161
162     let live_pid = run_md.live_pid                                  // line 162
163     let checkpoint_identity = select_checkpoint(conn, &request.run_id, &request.step_id)?  // line 163
164     let wait_state = wait_state::load(conn, &request.run_id)?       // line 164
165     let lease = leases::load_for_run(conn, &request.run_id)?       // line 165
166     let authority = RecoveryAuthority::new(                        // line 166 — sealed [C4]
167         workspace_auth, capsule, source_attempt, policy, strategy)
168     Ok(PreparedRecovery {                                           // line 168
169         authority, expected_epoch: request.expected_epoch,          // [B2]
170         operation_id, logical_request_key, intent_digest: intent.digest(), // [B3]
171         run_status, current_step, live_pid,                         // [B1]
172         checkpoint_identity, wait_state, lease,                     // [B1]
173         verified_workspace,                                         // [B6]
174     })
175 }
176
177 // ──────────────────────────────────────────────────────────────────────
178 // Phase 2: RESERVE — single epoch CAS + operation reservation. [C1/C2/B1/B2/B4]
179 // Short IMMEDIATE transaction. The ONLY CAS in the protocol. [B2]
180 fn reserve_recovery(
181     conn: &Connection,
182     prepared: &PreparedRecovery,
183 ) -> Result<Reservation, RecoveryError> {
184     let tx = Transaction::new(conn, Immediate)?                     // line 184
185     // Find the single operation for this logical request. [C2/B3]
186     match recovery_operations::lookup_logical_operation(tx, &prepared.logical_request_key)? {
187         None => {
188             // New operation. [B1] reselect/revalidate inside the tx before mutation:
189             let actual_status = run_metadata::read_status(tx, &run_id)?  // line 189
190             if actual_status != prepared.run_status {               // line 190 — [B1] revalidate
191                 tx.rollback()?                                      // line 191
192                 return Err(RecoveryError::AuthorityChanged)        // line 192
193             }
194             let actual_checkpoint = select_checkpoint(tx, &run_id, &step_id)?  // line 194 — [B1] reselect
195             if actual_checkpoint != prepared.checkpoint_identity {  // line 195 — [B1] revalidate
196                 tx.rollback()?                                      // line 196
197                 return Err(RecoveryError::AuthorityChanged)        // line 197
198             }
199             // [B6] descriptor-relative marker re-snapshot + exact identity comparison.
200             // Re-snapshot the durable marker via the retained anchor's fd.
201             let marker_verdict = snapshot_durable_marker_via_anchor(  // line 201
202                 &prepared.verified_workspace, &run_id)             // line 202
203             if marker_verdict != AnchoredMarkerVerdict::Trusted {  // line 203 — [B6] exact identity
204                 tx.rollback()?                                      // line 204
205                 return Err(RecoveryError::WorkspaceAuthorizationRevoked)  // line 205
206             }
207             // [B6] exact descriptor identity comparison (TOCTOU guard).
208             if !descriptor_matches_authorization(                   // line 208
209                 prepared.verified_workspace.as_fd(),                // line 209
210                 &prepared.authority.workspace_authorization)? {     // line 210
211                 tx.rollback()?                                      // line 211
212                 return Err(RecoveryError::WorkspaceAuthorizationRevoked)  // line 212
213             }
214             // SINGLE epoch CAS. [B2] No other CAS exists in the protocol.
215             let cas = recovery_epoch::cas_advance_epoch(tx, &run_id, prepared.expected_epoch)?  // line 215
216             match cas {
217                 CasOutcome::Advanced { from, to } => {
218                     // [B4] allocate durable execution_attempt_id at reserve.
219                     let attempt_id = attempts::record_attempt_start(  // line 219
220                         tx, &run_id, from, source_attempt_id,
221                         &prepared.operation_id, &step_id,
222                         capsule.schema_version, &capsule.envelope_digest,
223                         &snapshot)?                                  // [B4]
224                     recovery_operations::insert_pending(            // line 224
225                         tx, &prepared.operation_id, &run_id, from, &step_id,
226                         &envelope_digest, source_attempt_id,
227                         &prepared.logical_request_key,               // [B3]
228                         owner_pid, lease_expires_at,                 // [B3] guarded claim
229                         attempt_id)?                                // [B4]
230                     tx.commit()?                                    // line 230
231                     Ok(Reservation::newly_reserved(prepared.operation_id.clone(), from, attempt_id))  // line 231
232                 }
233                 CasOutcome::Stale { persisted, expected } => {
234                     tx.rollback()?                                  // line 234
235                     Ok(Reservation::stale_epoch(persisted, expected))  // line 235
236                 }
237             }
238         }
239         Some(op) if !op.exact_bindings_match(prepared) => {
240             // Same logical request but different operation/intent/capsule/source binding.
241             tx.rollback()?                                          // line 241
242             Ok(Reservation::conflict_duplicate(op))                 // line 242
243         }
243A        Some(op) if op.status == Completed => {
243B            tx.rollback()?
243C            Ok(Reservation::completed_duplicate(op))
243D        }
244         Some(op) if op.status == Pending => {
245             // Pending duplicate: reconcile via guarded owner/lease claim. [B3]
246             // Try to adopt if the lease has expired; otherwise reconcile.
247             let now = Utc::now()                                    // line 247
248             if let Some(adoptable) = recovery_operations::find_adoptable_pending(  // line 248
249                 tx, &prepared.logical_request_key, now)? {
250                 let adopt = recovery_operations::try_adopt_pending(  // line 250
251                     tx, &adoptable.operation_id, owner_pid, lease_expires_at, now)?  // [B3]
252                 if adopt == AdoptOutcome::Adopted {                 // line 252
253                     tx.commit()?                                    // line 253
254                     Ok(Reservation::pending_reconcile(adoptable))   // line 254
255                 } else {
256                     // Still owned: this caller must not execute or reconcile effects.
257                     tx.rollback()?                                  // line 257
258                     Ok(Reservation::pending_owned(adoptable))       // line 258
259                 }
260             } else {
261                 tx.rollback()?                                      // line 261
262                 Ok(Reservation::pending_owned(op))                  // line 262
263             }
264         }
265         Some(op) if op.status == Refused || op.status == Conflict => {
266             // Conflicting duplicate: refuse. [C2/B3]
267             tx.rollback()?                                          // line 267
268             Ok(Reservation::conflict_duplicate(op))                 // line 268
269         }
270     }
271 }
272
273 // ──────────────────────────────────────────────────────────────────────
274 // Phase 3: EXECUTE — run the recovery strategy. NO transaction. [C5/C12]
275 // Every RecoveryStrategy execution is completed here.
276 fn execute_recovery(
277     prepared: &PreparedRecovery,
278     reservation: &Reservation,
279 ) -> Result<ExecutionResult, RecoveryError> {
280     // For PendingReconcile: check if external work was already done via
281     // effect intent reconciliation. If done, return the prior result. [C2/B4]
282     if let Some(prior) = check_pending_reconcile(prepared, reservation)? {  // line 282
283         return Ok(prior)                                            // line 283
284     }
285     match &prepared.authority.strategy {
286         RecoveryStrategy::ContinueWorkspace => {
287             // --- exact verification (ContinueWorkspace) ---
288             verify_worktree_path(&prepared.authority)?             // line 288
289             verify_ownership_marker(&prepared.authority)?          // line 289
290             verify_base_ref(&prepared.authority)?                  // line 290
291             verify_diagnostic_state(&prepared.authority)?          // line 291
292             let runner_outcome = dispatch_runner(&prepared.authority)?  // line 292
293             Ok(ExecutionResult::from_runner(runner_outcome))        // line 293
294         }
295         RecoveryStrategy::Reenter => {
296             let runner_outcome = dispatch_runner_reenter(&prepared.authority)?  // line 296
297             Ok(ExecutionResult::from_runner(runner_outcome))        // line 297
298         }
299         RecoveryStrategy::ReconcileThenReenter => {
300             reconcile_observed_state(&prepared.authority)?          // line 300
301             let runner_outcome = dispatch_runner_reenter(&prepared.authority)?  // line 301
302             Ok(ExecutionResult::from_runner(runner_outcome))        // line 302
303         }
304         RecoveryStrategy::CompensateThenRetry => {
305             compensate_prior_effect(&prepared.authority)?           // line 305
306             let runner_outcome = dispatch_runner_reenter(&prepared.authority)?  // line 306
307             Ok(ExecutionResult::from_runner(runner_outcome))        // line 307
308         }
309         RecoveryStrategy::Refused(reason) => {
310             Ok(ExecutionResult::Refused { reason: reason.clone() })  // line 310
311         }
312     }
313 }
314
315 // ──────────────────────────────────────────────────────────────────────
316 // Phase 4: FINALIZE — append outcome + finalize operation. [C3/C5/C12/B4]
317 // Short IMMEDIATE transaction. Runner outcome is recorded BEFORE
318 // Recovered is returned. NO epoch CAS here. [B2]
319 fn finalize_recovery(
320     conn: &Connection,
321     prepared: &PreparedRecovery,
322     reservation: &Reservation,
323     execution: ExecutionResult,
324 ) -> Result<FinalizeResult, RecoveryError> {
325     let tx = Transaction::new(conn, Immediate)?                     // line 325
326     match execution {
327         ExecutionResult::Completed { snapshot } => {
328             // [B4] append outcome to the attempt row allocated at reserve.
329             attempts::append_attempt_outcome(                      // line 329
330                 tx, reservation.attempt_id, "completed",
331                 &snapshot, Some(&runner_result_json), checkpoint_digest)?  // [B4]
332             recovery_operations::finalize_completed(tx, &prepared.operation_id, &serialize_outcome(reservation.attempt_id, &snapshot))?  // line 332
333             tx.commit()?                                            // line 333
334             Ok(FinalizeResult::Finalized { attempt_id: reservation.attempt_id, status: "completed" })  // line 334
335         }
336         ExecutionResult::Interrupted { snapshot } => {
337             attempts::append_attempt_outcome(                      // line 337
338                 tx, reservation.attempt_id, "interrupted",
339                 &snapshot, Some(&runner_result_json), checkpoint_digest)?  // [B4]
340             recovery_operations::finalize_completed(tx, &prepared.operation_id, &serialize_outcome(reservation.attempt_id, &snapshot))?  // line 340
341             tx.commit()?                                            // line 341
342             Ok(FinalizeResult::Finalized { attempt_id: reservation.attempt_id, status: "interrupted" })  // line 342
343         }
344         ExecutionResult::Failed { snapshot, error } => {
345             attempts::append_attempt_outcome(                      // line 345
346                 tx, reservation.attempt_id, "failed",
347                 &snapshot, Some(&runner_result_json), checkpoint_digest)?  // [B4]
348             recoveryOperations::finalize_completed(tx, &prepared.operation_id, &serialize_outcome(reservation.attempt_id, &snapshot))?  // line 348
349             tx.commit()?                                            // line 349
350             Err(RecoveryError::ExecutionFailed(error))              // line 350
351         }
352         ExecutionResult::Refused { reason } => {
353             recovery_operations::finalize_refused(tx, &prepared.operation_id, &reason.to_string())?  // line 353
354             tx.commit()?                                            // line 354
355             Ok(FinalizeResult::Refused { reason })                  // line 355
356         }
356     }
357 }
358
359 pub enum ReserveStatus {
360     NewlyReserved { attempt_id: i64 },
361     PendingReconcile { operation: RecoveryOperation },
362     CompletedDuplicate { prior_outcome: SerializedOutcome, attempt_id: i64 },
363     ConflictDuplicate { detail: String },
364     StaleEpoch { persisted: u64, expected: u64 },
365 }
366
367 pub struct Reservation {
368     status: ReserveStatus,
369     epoch: u64,
370     attempt_id: Option<i64>,  // [B4] allocated at reserve
371 }
372
373 pub enum FinalizeResult {
374     Finalized { attempt_id: i64, status: &'static str },
375     Refused { reason: RefusalReason },
376 }
377
378 pub enum ExecutionResult {
379     Completed { snapshot: StateSnapshot },
380     Interrupted { snapshot: StateSnapshot },
381     Failed { snapshot: StateSnapshot, error: String },
382     Refused { reason: RefusalReason },
383 }
```

---

## Component: Effect Intents + Reconcile (`src/persistence/effect_intents.rs`) — [C7/B5]

Complete effect-intent state machine: stable unique effect key, operation/
attempt/sequence binding, canonical payload and digest/version, expected
target/predecessor, observed result, Prepared/Completed/Conflict, committed
before effect, effect-specific external reconciliation, guarded finalize.

**[B5]** Effect prepare is insert-or-load exact-binding comparison; conflict
state on mismatch; merge intent completed/conflicted in atomic merge transaction.

```
01 // effect_intents — durable effect-intent state machine. [C7/B5]
02 // CREATE TABLE IF NOT EXISTS effect_intents (
03 //   effect_key TEXT PRIMARY KEY,             -- stable unique key [C7]
04 //   operation_id TEXT NOT NULL,              -- bound to recovery operation [C7]
05 //   attempt_id INTEGER NOT NULL,             -- bound to attempt [C7/B4]
06 //   sequence INTEGER NOT NULL,               -- ordinal within the attempt [C7]
07 //   effect_kind TEXT NOT NULL,               -- 'commit'|'push'|'open_pr'|'merge'
08 //   canonical_payload BLOB NOT NULL,         -- canonical serialization
09 //   payload_digest TEXT NOT NULL,            -- SHA-256 of canonical_payload
10 //   payload_version INTEGER NOT NULL,        -- canonicalization version [C7]
11 //   expected_target TEXT,                    -- e.g., branch name, PR number [C7]
12 //   expected_predecessor TEXT,               -- e.g., expected parent commit SHA [C7]
13 //   observed_result TEXT,                    -- observed result after effect [C7]
14 //   status TEXT NOT NULL DEFAULT 'prepared', -- 'prepared'|'completed'|'conflict' [C7/B5]
15 //   created_at TEXT NOT NULL,
16 //   finalized_at TEXT
17 // )
18
19 // Compute the stable unique effect key. [C7]
20 pub fn compute_effect_key(
21     operation_id: &str,
22     attempt_id: i64,
23     sequence: i64,
24     effect_kind: &str,
25 ) -> String {
26     sha256_hex(canonical_concat(operation_id, attempt_id, sequence, effect_kind))  // line 26
27 }
28
29 // Prepare an effect intent BEFORE the external effect is issued. [C7/B5]
30 // [B5] insert-or-load exact-binding comparison: if a row with the same
31 // effect_key already exists, load it and compare the exact binding
32 // (canonical_payload, payload_digest, expected_target, expected_predecessor).
33 // On mismatch, transition to 'conflict'. On match, return the existing intent.
34 pub fn prepare_effect(
35     conn,
36     operation_id: &str,
37     attempt_id: i64,
38     sequence: i64,
39     kind: EffectKind,
40     payload: &[u8],
41     expected_target: Option<&str>,
42     expected_predecessor: Option<&str>,
43 ) -> Result<EffectIntent, EffectError> {
44     let canonical = canonicalize_payload(kind, payload)              // line 44
45     let digest = sha256_hex(&canonical)                              // line 45
46     let key = compute_effect_key(operation_id, attempt_id, sequence, kind.as_str())  // line 46
47     // [B5] insert-or-load exact-binding comparison.
48     match load_effect(conn, &key)? {                                 // line 48
49         None => {
50             // No existing intent: insert new Prepared intent. [C7]
51             INSERT INTO effect_intents                               // line 51
52               (effect_key, operation_id, attempt_id, sequence, effect_kind,
53                canonical_payload, payload_digest, payload_version,
54                expected_target, expected_predecessor, status, created_at)
55             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'prepared', ?11)  // line 55
56             load_effect(conn, &key)                                  // line 56
57         }
58         Some(existing) => {
59             // [B5] exact-binding comparison against the existing intent.
60             if existing.payload_digest != digest                     // line 60
61                 || existing.expected_target != expected_target       // line 61
62                 || existing.expected_predecessor != expected_predecessor {  // line 62
63                 // Mismatch: transition to conflict state. [B5]
64                 finalize_effect(conn, &key, "conflict", None)?       // line 64
65                 return Err(EffectError::BindingConflict { key })     // line 65
66             }
67             // Exact match: return the existing intent (idempotent). [B5]
68             Ok(existing)                                             // line 68
69         }
70     }
71 }
72
73 // Reconcile an effect against observed external state. [C7]
74 // Effect-specific: each kind has its own reconciliation logic.
75 pub fn reconcile_effect(conn, key: &str, observed: &ObservedState) -> Result<ReconcileVerdict, EffectError> {
76     let intent = load_effect(conn, key)?                             // line 76
77     match intent.status.as_str() {
78         "completed" => {
79             // Already completed: return the prior observed result. [C2/C7]
80             Ok(ReconcileVerdict::Completed { result: intent.observed_result.clone() })  // line 80
81         }
82         "conflict" => {
83             Ok(ReconcileVerdict::Conflict { detail: "prior conflict".to_string() })  // line 83
84         }
85         "prepared" => {
86             match intent.effect_kind {
87                 Commit => reconcile_commit(conn, &intent, observed),   // line 87
88                 Push => reconcile_push(conn, &intent, observed),        // line 88
89                 OpenPr => reconcile_open_pr(conn, &intent, observed),   // line 89
90                 Merge => reconcile_merge(conn, &intent, observed),      // line 90
91             }
92         }
93     }
94 }
95
96 // Commit reconciliation. [C7]
97 fn reconcile_commit(conn, intent: &EffectIntent, observed: &ObservedState) -> Result<ReconcileVerdict> {
98     if observed.head_sha == intent.expected_target {                 // line 98
99         finalize_effect(conn, &intent.effect_key, "completed", Some(&observed.head_sha))?  // line 99
100         Ok(ReconcileVerdict::Completed { result: Some(observed.head_sha.clone()) })  // line 100
101     } else if observed.head_sha == intent.expected_predecessor {     // line 102
102         // HEAD hasn't moved: needs reissue. [C7]
103         Ok(ReconcileVerdict::NeedsReissue)                           // line 103
104     } else {
105         finalize_effect(conn, &intent.effect_key, "conflict", None)?  // line 105
106         Ok(ReconcileVerdict::Conflict { detail: "unexpected HEAD".to_string() })  // line 106
107     }
108 }
109
110 // Push reconciliation. [C7]
111 fn reconcile_push(conn, intent: &EffectIntent, observed: &ObservedState) -> Result<ReconcileVerdict> {
112     if observed.remote_ref_sha == intent.expected_target {           // line 112
113         finalize_effect(conn, &intent.effect_key, "completed", Some(&observed.remote_ref_sha))?  // line 113
114         Ok(ReconcileVerdict::Completed { result: Some(observed.remote_ref_sha.clone()) })  // line 114
115     } else if observed.remote_ref_sha == intent.expected_predecessor {  // line 115
116         Ok(ReconcileVerdict::NeedsReissue)                           // line 116
117     } else {
118         finalize_effect(conn, &intent.effect_key, "conflict", None)?  // line 118
119         Ok(ReconcileVerdict::Conflict { detail: "remote ref diverged".to_string() })  // line 119
120     }
121 }
122
123 // Open PR reconciliation. [C7]
124 fn reconcile_open_pr(conn, intent: &EffectIntent, observed: &ObservedState) -> Result<ReconcileVerdict> {
125     if let Some(pr_number) = observed.matching_pr_for_head(&intent.expected_target) {  // line 125
126         finalize_effect(conn, &intent.effect_key, "completed", Some(&pr_number.to_string()))?  // line 126
127         Ok(ReconcileVerdict::Completed { result: Some(pr_number.to_string()) })  // line 127
128     } else {
129         Ok(ReconcileVerdict::NeedsReissue)                           // line 129
130     }
131 }
132
133 // Merge reconciliation delegates to the typed merge component (P17). [C7/B5]
134 // [B5] merge intent completed/conflicted in the atomic merge transaction
135 // (typed_merge::complete_typed_merge), NOT via effect finalize alone.
136 fn reconcile_merge(conn, intent: &EffectIntent, observed: &ObservedState) -> Result<ReconcileVerdict> {
137     typed_merge::verify_and_complete(conn, intent, observed)         // line 137
138 }
139
140 // Guarded finalize: transition the effect to a terminal state. [C7]
141 // Only transitions from 'prepared'. Checks affected rows.
142 fn finalize_effect(conn, key: &str, status: &str, observed_result: Option<&str>) -> Result<()> {
143     UPDATE effect_intents                                            // line 143
144       SET status = ?2, observed_result = ?3, finalized_at = ?4
145       WHERE effect_key = ?1 AND status = 'prepared'                  // line 145 — guard
146     let affected = count_rows_affected()                             // line 146
147     if affected != 1 {                                               // line 147
148         return Err(EffectError::GuardFailed { key: key.to_string(), affected })  // line 148
149     }
150     Ok(())                                                          // line 150
151 }
152
153 pub enum EffectKind { Commit, Push, OpenPr, Merge }
154
155 pub enum ReconcileVerdict {
156     Completed { result: Option<String> },
157     NeedsReissue,
158     Conflict { detail: String },
159 }
160
161 pub struct EffectIntent {
162     effect_key: String,
163     operation_id: String,
164     attempt_id: i64,
165     sequence: i64,
166     effect_kind: EffectKind,
167     canonical_payload: Vec<u8>,
168     payload_digest: String,
169     payload_version: u32,
170     expected_target: Option<String>,
171     expected_predecessor: Option<String>,
172     observed_result: Option<String>,
173     status: String,
174 }
175
176 // ── Call-site phase wiring: commit / push / open_pr / merge ──────── [C7/B5]
177 // Each call-site follows: prepare_effect → issue external work →
178 // reconcile_effect → (NeedsReissue: re-issue) → finalize.
179 //
180 //   commit:  prepare_effect(kind=Commit, payload=tree+message,
181 //              expected_target=computed_commit_sha,
182 //              expected_predecessor=current_head)
183 //            → git commit → reconcile_effect(observed HEAD)
184 //
185 //   push:    prepare_effect(kind=Push, payload=local_sha+ref,
186 //              expected_target=local_sha, expected_predecessor=remote_sha)
187 //            → git push → reconcile_effect(observed remote ref)
188 //
189 //   open_pr: prepare_effect(kind=OpenPr, payload=head+title+body,
190 //              expected_target=head_sha, expected_predecessor=None)
191 //            → gh pr create → reconcile_effect(observed PRs for head)
192 //
193 //   merge:   prepare_effect(kind=Merge, payload=merge_request,
194 //              expected_target=expected_merge_sha,
195 //              expected_predecessor=current_pr_head)
196 //            → gh pr merge → reconcile_merge → typed_merge::verify_and_complete
197 //            → (P17 artifact+status transaction; merge intent finalized inside that tx) [B5]
```

---

## Component: Legacy Salvage (`src/engine/recovery/salvage.rs`) — [C9/B10]

Every run WITHOUT a valid pre-execution V1 capsule is salvage-only, regardless
of provenance or migration source. Immutable idempotent lineage. Exact recovery
refuses.

**[B10]** Prohibit historical capsule backfill. Every run without a capsule
written by fresh launch (P14 wiring) before execution is salvage-only. A capsule
cannot be retroactively inserted for a run that already executed without one.

```
01 // salvage.rs — legacy salvage lineage. [C9/B10]
02 // Every run without a valid pre-execution V1 capsule is salvage-only,
03 // regardless of provenance or migration source.
04 // [B10] Historical capsule backfill is PROHIBITED: a capsule may only be
05 // written by the fresh-launch path BEFORE any step executes.

06 // Classify a run: capsule-backed (exact recovery possible) or salvage-only.
07 pub fn classify_run(conn: &Connection, run_id: &str) -> Result<RunClassification, SalvageError> {
08     match capsule_store::load_capsule_v1(conn, run_id) {              // line 08
09         Ok(capsule) => {
10             // Valid pre-execution V1 capsule exists and envelope verifies.
11             match verify_envelope_digest(&capsule) {                  // line 11
12                 Ok(()) => Ok(RunClassification::CapsuleBacked { capsule }),  // line 12
13                 Err(_) => Ok(RunClassification::SalvageOnly { run_id: run_id.to_string() }),  // line 13
14             }
15         }
16         Err(_) => {
17             // No valid pre-execution V1 capsule: salvage-only. [C9/B10]
18             // This applies regardless of whether the run has a LaunchProvenance
19             // (including migrated provenance with sentinel digests) or none.
20             // [B10] Backfill is prohibited; the run remains salvage-only forever.
21             Ok(RunClassification::SalvageOnly { run_id: run_id.to_string() })  // line 21
22         }
23     }
24 }
25
26 // A salvage-only run produces an immutable salvage lineage record
27 // (audit-only) and REFUSES exact recovery. [C9/B10]
28 pub fn salvage_recover(conn: &Connection, run_id: &str) -> Result<RecoveryOutcome, SalvageError> {
29     let classification = classify_run(conn, run_id)?                  // line 29
30     match classification {
31         CapsuleBacked { .. } => {
32             // Should have been routed to the protocol, not here. [C9]
33             Err(SalvageError::UnexpectedCapsuleBackedRun)             // line 33
34         }
35         SalvageOnly { run_id } => {
36             // Record immutable salvage lineage (audit-only, append-only). [C9]
37             append_salvage_record(conn, &run_id)?                     // line 37
38             // Exact recovery is REFUSED. [C9/B10]
39             Ok(RecoveryOutcome::Refused { reason: RefusalReason::SalvageOnly })  // line 39
40         }
41     }
42 }
43
44 // Append an immutable salvage lineage record (never updates existing).
45 // CREATE TABLE IF NOT EXISTS salvage_lineage (
46 //   salvage_id INTEGER PRIMARY KEY AUTOINCREMENT,
47 //   run_id TEXT NOT NULL,
48 //   recorded_at TEXT NOT NULL,
49 //   detail TEXT
50 // )
51 pub fn append_salvage_record(conn: &Connection, run_id: &str) -> Result<i64, SalvageError> {
52     INSERT INTO salvage_lineage (run_id, recorded_at, detail)         // line 52
53       VALUES (?1, ?2, ?3)
54     RETURNING salvage_id                                             // line 54
55 }
56
57 pub enum RunClassification {
58     CapsuleBacked { capsule: ExecutionCapsuleV1 },
59     SalvageOnly { run_id: String },
60 }
```

---

## Component: Typed Verified Merge (`src/engine/recovery/typed_merge.rs`) — [C10/C11/B11/B12]

Strategy-specific merge proof enum. Merge commit has two ancestry checks; squash
and rebase include ancestry plus computed expected/observed content or patch
evidence. `result_sha` is strategy-neutral. Typed merge: external verification
then short IMMEDIATE atomic artifact+status transaction with explicit allowed
predecessor and affected-row check.

**[B11]** The merge verifier takes authoritative injected Git/remote interfaces
and bound repo/PR/base/head; it computes all evidence itself (no ambient shell).

**[B12]** Merge artifact DDL/init, exact artifact equality, capsule lookup/join,
fixed allowed predecessor `ReviewReady`, modify runner completion semantics for
merge-required workflows.

```
01 // TypedMergeArtifact — proof that a merge happened and is reachable. [C10/C11/B12]
02 pub struct TypedMergeArtifact {
03     run_id: String,
04     pr_number: i64,
05     result_sha: String,                 // strategy-neutral observed commit [C10]
06     repo: String,                       // bound to repo [C11]
07     head_sha: String,                   // bound to head [C11]
08     base_sha: String,                   // bound to base [C11]
09     capsule_envelope_digest: String,    // bound to capsule [C11]
10     reachability_proof: MergeReachabilityProof,  // strategy-specific [C10]
11     recorded_at: DateTime<Utc>,
12 }
13
14 // merge_artifacts DDL. [B12]
15 // CREATE TABLE IF NOT EXISTS merge_artifacts (
16 //   run_id TEXT PRIMARY KEY,                 -- one artifact per run [B12]
17 //   pr_number INTEGER NOT NULL,
18 //   result_sha TEXT NOT NULL,
19 //   repo TEXT NOT NULL,
20 //   head_sha TEXT NOT NULL,
21 //   base_sha TEXT NOT NULL,
22 //   capsule_envelope_digest TEXT NOT NULL,   -- join key to execution_capsules [B12]
23 //   proof_kind TEXT NOT NULL,                -- 'merge_commit'|'squash'|'rebase'
24 //   proof_json TEXT NOT NULL,                -- serialized MergeReachabilityProof
25 //   recorded_at TEXT NOT NULL
26 // )
27
28 // Strategy-specific merge proof enum. [C10]
29 // NEVER assumes the merged SHA is an ancestor of the final head.
30 pub enum MergeReachabilityProof {
31     // Merge commit: TWO ancestry checks. [C10]
32     MergeCommit {
33         head_sha: String,
34         base_sha: String,
35         merge_commit_sha: String,
36         // Verified: --is-ancestor <head_sha> <merge_commit_sha>
37         //      AND: --is-ancestor <base_sha> <merge_commit_sha>
38     },
39     // Squash: ancestry PLUS computed expected/observed content evidence. [C10]
40     Squash {
41         base_sha: String,
42         squash_commit_sha: String,
43         expected_content_digest: String,  // computed from PR diff
44         observed_content_digest: String,  // computed from squash commit tree
45         // Verified: --is-ancestor <base_sha> <squash_commit_sha>
46         //      AND: expected_content_digest == observed_content_digest
47     },
48     // Rebase: ancestry PLUS computed expected/observed patch evidence. [C10]
49     Rebase {
50         base_sha: String,
51         final_head_sha: String,
52         expected_patch_id: String,        // computed from PR commits
53         observed_patch_id: String,        // computed from rebased commits
54         // Verified: --is-ancestor <base_sha> <final_head_sha>
55         //      AND: expected_patch_id == observed_patch_id
56     },
57 }
58
59 // [B11] Authoritative injected Git interface for reachability checks.
60 // Production: SystemMergeVerifier (shells out to git). Tests: inject a
61 // deterministic probe. Mirrors the existing MergeBaseProbe pattern
62 // (src/engine/executors/scope_control/task_charter.rs).
63 pub trait MergeGitProbe: Send + Sync {
64     /// Returns Ok(()) if <ancestor> is an ancestor of <descendant>.
65     fn is_ancestor(&self, work_dir: &Path, ancestor: &str, descendant: &str) -> Result<(), MergeError>;
66     /// Compute the tree content digest of a commit (squash evidence). [C10]
67     fn compute_tree_content_digest(&self, work_dir: &Path, commit: &str) -> Result<String, MergeError>;
68     /// Compute the patch-id of a commit range (rebase evidence). [C10]
69     fn compute_patch_id(&self, work_dir: &Path, base: &str, head: &str) -> Result<String, MergeError>;
70 }
71
72 // [B11] Authoritative injected remote interface for PR/merge observation.
73 // Production: SystemMergeRemote (shells out to gh). Tests: inject a stub.
74 pub trait MergeRemoteProbe: Send + Sync {
75     /// Observe whether the PR is merged and return the merge strategy + result sha.
76     fn observe_merge(&self, repo: &str, pr_number: i64) -> Result<MergeObservation, MergeError>;
77 }
78
79 pub struct MergeObservation {
80     merged: bool,
81     strategy: MergeStrategy,      // MergeCommit | Squash | Rebase
82     result_sha: String,
83 }
84
85 // [B11] The verifier context: injected probes + bound identity. The verifier
86 // computes ALL evidence itself from these authoritative interfaces.
87 pub struct MergeVerifier {
88     git_probe: Box<dyn MergeGitProbe>,
89     remote_probe: Box<dyn MergeRemoteProbe>,
90     work_dir: PathBuf,
91     repo: String,
92     pr_number: i64,
93     base_sha: String,
94     head_sha: String,
95 }
96
97 // [B11] Build the strategy-specific proof by computing evidence via probes.
98 pub fn build_reachability_proof(verifier: &MergeVerifier) -> Result<MergeReachabilityProof, MergeError> {
99     let obs = verifier.remote_probe.observe_merge(&verifier.repo, verifier.pr_number)?  // line 99
100     if !obs.merged { return Err(MergeError::NotMerged) }              // line 100
101     match obs.strategy {
102         MergeStrategy::MergeCommit => {
103             // TWO ancestry checks, computed by the verifier. [C10/B11]
104             verifier.git_probe.is_ancestor(&verifier.work_dir, &verifier.head_sha, &obs.result_sha)?  // line 104
105             verifier.git_probe.is_ancestor(&verifier.work_dir, &verifier.base_sha, &obs.result_sha)?  // line 105
106             Ok(MergeReachabilityProof::MergeCommit {                  // line 106
107                 head_sha: verifier.head_sha.clone(), base_sha: verifier.base_sha.clone(),
108                 merge_commit_sha: obs.result_sha.clone(),
109             })
110         }
111         MergeStrategy::Squash => {
112             // Ancestry PLUS computed content evidence. [C10/B11]
113             verifier.git_probe.is_ancestor(&verifier.work_dir, &verifier.base_sha, &obs.result_sha)?  // line 113
114             let expected = compute_pr_diff_content_digest(&verifier)?  // line 114
115             let observed = verifier.git_probe.compute_tree_content_digest(&verifier.work_dir, &obs.result_sha)?  // line 115
116             if expected != observed { return Err(MergeError::ContentMismatch) }  // line 116
117             Ok(MergeReachabilityProof::Squash {                       // line 117
118                 base_sha: verifier.base_sha.clone(), squash_commit_sha: obs.result_sha.clone(),
119                 expected_content_digest: expected, observed_content_digest: observed,
120             })
121         }
122         MergeStrategy::Rebase => {
123             // Ancestry PLUS computed patch evidence. [C10/B11]
124             verifier.git_probe.is_ancestor(&verifier.work_dir, &verifier.base_sha, &obs.result_sha)?  // line 124
125             let expected = verifier.git_probe.compute_patch_id(&verifier.work_dir, &verifier.base_sha, &verifier.head_sha)?  // line 125
126             let observed = verifier.git_probe.compute_patch_id(&verifier.work_dir, &verifier.base_sha, &obs.result_sha)?  // line 126
127             if expected != observed { return Err(MergeError::PatchMismatch) }  // line 127
128             Ok(MergeReachabilityProof::Rebase {                      // line 128
129                 base_sha: verifier.base_sha.clone(), final_head_sha: obs.result_sha.clone(),
130                 expected_patch_id: expected, observed_patch_id: observed,
131             })
132         }
133     }
134 }
135
136 // [B12] Fixed allowed predecessor for the merge status transition.
137 // The only status from which a run may transition to Merged.
138 pub const ALLOWED_MERGE_PREDECESSOR: RunStatus = RunStatus::ReviewReady;
139A// (ReviewReady is the non-terminal status indicating CI/review passed,
139B// awaiting merge.)
140
141 // [B12] Complete a typed merge: external verification THEN short IMMEDIATE
142 // atomic artifact+status transaction. Bound to repo/PR/head/capsule.
143 // Explicit allowed predecessor (ReviewReady). Affected-row check.
144 // Exact idempotent retry. Normal merge-required flow must NOT first write Completed.
145 pub fn complete_typed_merge(conn: &Connection, artifact: &TypedMergeArtifact, verifier: &MergeVerifier) -> Result<(), MergeError> {
146     // Step 1: build the reachability proof via injected probes (no tx). [B11]
147     let proof = build_reachability_proof(verifier)?                  // line 147
148
149     // Step 2: short IMMEDIATE atomic artifact+status transaction. [C11/B12]
150     // Artifact INSERT and status UPDATE happen in ONE transaction.
151     // Normal merge-required flow must NOT first write Completed.
152     let tx = Transaction::new(conn, Immediate)?                      // line 152
153
154     // [B12] Insert artifact (immutable). Idempotent: ON CONFLICT DO NOTHING.
155     // [B12] exact artifact equality: if a row exists, compare all fields.
156     let artifact_inserted = INSERT INTO merge_artifacts               // line 156
157       (run_id, pr_number, result_sha, repo, head_sha, base_sha,
158        capsule_envelope_digest, proof_kind, proof_json, recorded_at)
159       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
160       ON CONFLICT(run_id) DO NOTHING                                 // line 160 — idempotent
161     let artifact_affected = count_rows_affected()                     // line 161
162
163     // [B12] if artifact already existed, verify exact equality.
164     if artifact_affected == 0 {                                       // line 164
165         let existing = SELECT * FROM merge_artifacts WHERE run_id = ?1  // line 165
166         if !artifact_exact_equal(existing, artifact) {                // line 166 — [B12]
167             tx.rollback()?                                           // line 167
168             return Err(MergeError::ArtifactConflict)                 // line 168
169         }
170     }
171
172     // [B12] Conditional status transition with FIXED allowed predecessor ReviewReady.
173     // Only transitions from ReviewReady to Merged.
174     // Must NOT first write Completed.
175     UPDATE runs                                                       // line 175
176       SET status = 'merged'
177       WHERE run_id = ?1
178         AND status = ?2                  -- ALLOWED_MERGE_PREDECESSOR (ReviewReady) [B12]
179         AND head_sha = ?3                 -- bound to head [C11]
180         AND capsule_envelope_digest = ?4  -- bound to capsule [C11/B12]
181     let status_affected = count_rows_affected()                      // line 181
182
183     // Affected-row check. [C11/B12]
184     if status_affected != 1 {                                        // line 184
185         // Either already merged (idempotent retry) or wrong predecessor.
186         let current_status = SELECT status FROM runs WHERE run_id = ?1  // line 186
187         if current_status == "merged" && artifact_affected == 0 {    // line 187
188             // Idempotent retry: artifact already existed and status already merged.
189             tx.commit()?                                              // line 189
190             return Ok(())                                            // line 190
191         }
192         tx.rollback()?                                               // line 192
193         return Err(MergeError::PreconditionFailed {                   // line 193
194             current_status, expected_predecessor: ALLOWED_MERGE_PREDECESSOR,
195         })
196     }
197     tx.commit()?                                                     // line 197
198     Ok(())                                                           // line 198
199 }
200
201 // [B12] Completion requires BOTH a typed artifact row AND RunStatus::Merged.
202 // A status field alone NEVER satisfies completion.
203 pub fn completion_satisfied(conn: &Connection, run_id: &str) -> bool {
204     let has_artifact = SELECT COUNT(*) FROM merge_artifacts WHERE run_id = ?1  // line 204
205     let status = SELECT status FROM runs WHERE run_id = ?1            // line 205
206     has_artifact > 0 && status == "merged"                           // line 206
207 }
208
209 // [B12] Capsule lookup/join: verify the run's capsule envelope digest
210 // matches the artifact's capsule_envelope_digest.
211 pub fn verify_capsule_binding(conn: &Connection, run_id: &str, artifact: &TypedMergeArtifact) -> Result<(), MergeError> {
212     let capsule = capsule_store::load_capsule_v1(conn, run_id)?       // line 212
213     if capsule.envelope_digest != artifact.capsule_envelope_digest {  // line 213
214         return Err(MergeError::CapsuleBindingMismatch)               // line 214
215     }
216     Ok(())                                                           // line 216
217 }
218
219 // [B12] Modify runner completion semantics for merge-required workflows:
220 // a merge-required run does NOT reach Completed; it reaches ReviewReady,
2 // then Merged only via complete_typed_merge.
221 pub fn runner_completion_for_merge_required(conn: &Connection, run_id: &str) -> RunStatus {
222     // After all steps complete, a merge-required run transitions to ReviewReady
223     // (NOT Completed). complete_typed_merge then transitions ReviewReady → Merged.
224     RunStatus::ReviewReady                                            // line 224
225 }
226
227 pub enum MergeError {
228     NotMerged,
229     ReachabilityFailed(String),
230     ContentMismatch,
231     PatchMismatch,
232     PreconditionFailed { current_status: String, expected_predecessor: RunStatus },
233     AlreadyTerminal,
234     ArtifactConflict,            // [B12]
235     CapsuleBindingMismatch,      // [B12]
236 }
```

---

## Verification Gate

- [x] Every component has numbered lines.
- [ ] Implementation phases (P05, P08, P11, P14, P15, P17) cite specific lines.
- [x] Recovery epoch is a distinct durable state with CAS claim (epoch lines
      09–41). [C1]
- [x] `recovery_operations` ledger has stable operation_id, logical_request_key,
      guarded owner/lease claim, Pending/Completed/Refused/Conflict, serialized
      prior outcome (operations lines 01–149). [C2/B3]
- [x] Append-only attempts include complete StateSnapshot, immutable IDs,
      capsule envelope digest, snapshot/checkpoint digest, attempt-start +
      outcome-append (attempts lines 01–111). [C3/B4]
- [x] `RecoveryRequest` has no `trusted_internal` bool and carries
      `expected_epoch`; sealed `RecoveryAuthority` carries descriptor-bound
      `WorkspaceAuthorization` revalidated in CAS tx (protocol lines 03–08,
      208–213). [C4/B2]
- [x] Prepare outside tx, reserve in short IMMEDIATE tx, execute with no tx,
      finalize in short IMMEDIATE tx (protocol lines 78, 82, 105, 110). [C5]
- [x] SINGLE epoch CAS at reserve only; no finalize CAS (protocol lines 215,
      325). [B2]
- [x] PreparedRecovery preserves exact run/status/current-step/live-PID/
      checkpoint/wait/lease authority; reselect/revalidate inside reserve
      (protocol lines 31–37, 189–198). [B1]
- [x] PreparedRecovery owns retained VerifiedWorkspace anchor; reserve does
      descriptor-relative marker re-snapshot + exact identity comparison
      (protocol lines 39, 144–150, 201–213). [B6]
- [x] Durable execution_attempt_id allocated at reserve with attempt-start
      record; outcome appended at finalize (attempts lines 23–65; protocol
      lines 219–223, 329–348). [B4]
- [x] Policy consumes canonical StepDef + SAFE_RERUN_STEPS; generic
      shell/write_file defaults NonRecoverable; recovery_policy field added in
      capsule milestone (policy lines 11–36). [C6/B7]
- [x] Effect intent has stable key, operation/attempt/sequence binding,
      canonical payload/digest/version, expected target/predecessor, observed
      result, Prepared/Completed/Conflict, guarded finalize, insert-or-load
      exact-binding comparison with conflict on mismatch (intents lines 01–197).
      [C7/B5]
- [x] Capsule has framed canonical envelope byte format with fixed supported
      schema/canonicalization/domain/provenance versions; ONE envelope digest;
      component digests are metadata; fail-closed version dispatch (capsule
      lines 01–113). [C8/B9]
- [x] Adapter is object-safe via `fn version()` with fail-closed dispatch
      (adapters lines 01–20). [C8/B9]
- [x] Legacy salvage: every run without valid V1 capsule is salvage-only;
      historical capsule backfill prohibited (salvage lines 01–60). [C9/B10]
- [x] Merge proof is strategy-specific enum with two ancestry checks for
      merge-commit, content/patch evidence for squash/rebase, strategy-neutral
      result_sha (typed_merge lines 30–57). [C10]
- [x] Typed merge: injected Git/remote probes compute all evidence; atomic
      artifact+status tx with fixed allowed predecessor ReviewReady, affected-row
      check, capsule binding, artifact DDL, completion semantics (typed_merge
      lines 59–236). [C11/B11/B12]
- [x] Protocol owns prepare/reserve/execute/finalize; cannot return Recovered
      before finalize (protocol lines 72–124, 315–357). [C12]

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P02.md`
