//! Integration tests for the durable recovery epoch CAS, operations ledger,
//! and append-only attempt store.
//!
//! These tests exercise the **real durable store (SQLite)** directly, asserting
//! durable invariants against the store itself — no in-memory facade, no
//! protocol dependency. They are the **RED phase** for P04: they compile and
//! assert real invariants, but fail because the designated P03 stubs
//! (`todo!()`) are reached. P05 will implement the stubs to turn these green.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use luther_workflow::persistence::attempts::{
    append_attempt_outcome, init_attempts_table, load_unfinalized_for_operation,
    record_attempt_start, verify_snapshot_digest, AttemptStart,
};
use luther_workflow::persistence::checkpoint::StateSnapshot;
use luther_workflow::persistence::effect_intents::init_effect_intents_table;
use luther_workflow::persistence::recovery_epoch::CasOutcome;
use luther_workflow::persistence::recovery_epoch::{
    cas_advance_epoch, init_epoch_table, read_epoch,
};
use luther_workflow::persistence::recovery_operations::{
    finalize_completed, find_adoptable_pending, init_operations_table, insert_pending,
    lookup_logical_operation, try_adopt_pending, AdoptOutcome, OperationStatus,
    PendingOperationInsert,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create an in-memory SQLite connection with all recovery tables initialized.
fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_epoch_table(&conn).expect("init epoch table");
    init_operations_table(&conn).expect("init operations table");
    init_attempts_table(&conn).expect("init attempts table");
    init_effect_intents_table(&conn).expect("init effect intents table");
    conn
}

/// Begin an IMMEDIATE transaction (mirrors the protocol's reserve/finalize tx).
fn begin_tx(conn: &Connection) -> Transaction<'_> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate).expect("begin IMMEDIATE tx")
}

/// Independently compute a lowercase-hex SHA-256 digest of a byte slice.
///
/// This mirrors `hex_digest` in `launch_provenance.rs` so the test can verify
/// stored digests **without** relying on the implementation under test.
fn independent_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// A minimal `StateSnapshot` with scalar fields set and empty maps.
///
/// Independently canonicalize a snapshot through a JSON value so object keys
/// are sorted deterministically across processes.
fn independent_canonical_snapshot(snapshot: &StateSnapshot) -> Vec<u8> {
    let value = serde_json::to_value(snapshot).expect("convert StateSnapshot to JSON value");
    serde_json::to_vec(&value).expect("serialize canonical StateSnapshot")
}

/// Empty maps keep `serde_json::to_vec` output deterministic (sorted-key
/// canonical form is vacuously correct for empty objects).
fn test_snapshot(retry_count: u32, loop_count: u32, status: &str) -> StateSnapshot {
    StateSnapshot {
        retry_count,
        loop_count,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: status.to_string(),
    }
}

/// A `StateSnapshot` with populated context, used for digest-divergence tests.
fn snapshot_with_context() -> StateSnapshot {
    let mut context = HashMap::new();
    context.insert("branch".to_string(), serde_json::json!("feature-x"));
    context.insert("commit".to_string(), serde_json::json!("abc123"));
    StateSnapshot {
        retry_count: 2,
        loop_count: 1,
        edge_loop_counts: HashMap::new(),
        context,
        status: "running".to_string(),
    }
}

/// Values for a raw-SQL insert into `recovery_operations`.
#[derive(Debug, Clone)]
struct RawOperation {
    operation_id: String,
    run_id: String,
    epoch: u64,
    step_id: String,
    capsule_envelope_digest: String,
    source_attempt_id: Option<i64>,
    logical_request_key: String,
    intent_digest: String,
    status: String,
    owner_pid: Option<u32>,
    lease_expires_at: Option<DateTime<Utc>>,
    execution_attempt_id: Option<i64>,
}

impl RawOperation {
    /// A pending operation with a live (future-expiry) lease.
    fn pending_live_lease(operation_id: &str, logical_key: &str) -> Self {
        Self {
            operation_id: operation_id.to_string(),
            run_id: "run-1".to_string(),
            epoch: 0,
            step_id: "step-1".to_string(),
            capsule_envelope_digest: "env-digest-1".to_string(),
            source_attempt_id: None,
            logical_request_key: logical_key.to_string(),
            intent_digest: "intent-digest-1".to_string(),
            status: OperationStatus::Pending.as_str().to_string(),
            owner_pid: Some(1000),
            lease_expires_at: Some(Utc::now() + Duration::minutes(10)),
            execution_attempt_id: Some(1),
        }
    }

    /// A pending operation with an expired lease.
    fn pending_expired_lease(operation_id: &str, logical_key: &str) -> Self {
        let mut op = Self::pending_live_lease(operation_id, logical_key);
        op.lease_expires_at = Some(Utc::now() - Duration::minutes(10));
        op
    }
}

/// Insert a row into `recovery_operations` via raw SQL (bypasses the stub API).
fn raw_insert_operation(conn: &Connection, op: &RawOperation) {
    conn.execute(
        "INSERT INTO recovery_operations
             (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
              source_attempt_id, logical_request_key, intent_digest, status,
              owner_pid, lease_expires_at, execution_attempt_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            op.operation_id,
            op.run_id,
            op.epoch as i64,
            op.step_id,
            op.capsule_envelope_digest,
            op.source_attempt_id,
            op.logical_request_key,
            op.intent_digest,
            op.status,
            op.owner_pid.map(|p| p as i64),
            op.lease_expires_at.map(|dt| dt.to_rfc3339()),
            op.execution_attempt_id,
            Utc::now().to_rfc3339(),
        ],
    )
    .expect("raw insert into recovery_operations");
}

/// Query the `status` column of a recovery operation by id.
fn raw_query_operation_status(conn: &Connection, operation_id: &str) -> String {
    conn.query_row(
        "SELECT status FROM recovery_operations WHERE operation_id = ?1",
        params![operation_id],
        |row| row.get(0),
    )
    .expect("query operation status")
}

/// Count rows in `recovery_attempts` for a given run+step.
fn raw_count_attempts(conn: &Connection, run_id: &str, step_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM recovery_attempts WHERE run_id = ?1 AND step_id = ?2",
        params![run_id, step_id],
        |row| row.get(0),
    )
    .expect("count attempts")
}

/// Values for a raw-SQL insert into `recovery_attempts`.
#[derive(Debug, Clone)]
struct RawAttempt {
    run_id: String,
    epoch: u64,
    source_attempt_id: Option<i64>,
    operation_id: String,
    step_id: String,
    step_status: String,
    capsule_schema_version: u32,
    capsule_envelope_digest: String,
    state_snapshot: StateSnapshot,
    finalized_at: Option<DateTime<Utc>>,
}

/// Insert a row into `recovery_attempts` via raw SQL. Returns the attempt_id.
fn raw_insert_attempt(conn: &Connection, attempt: &RawAttempt) -> i64 {
    let snapshot_json =
        serde_json::to_vec(&attempt.state_snapshot).expect("serialize StateSnapshot");
    let snapshot_digest = independent_sha256_hex(&snapshot_json);
    conn.execute(
        "INSERT INTO recovery_attempts
             (run_id, epoch, source_attempt_id, operation_id, step_id, step_status,
              capsule_schema_version, capsule_envelope_digest,
              state_snapshot_json, snapshot_digest, checkpoint_digest,
              runner_result_json, started_at, finalized_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?12)",
        params![
            attempt.run_id,
            attempt.epoch as i64,
            attempt.source_attempt_id,
            attempt.operation_id,
            attempt.step_id,
            attempt.step_status,
            attempt.capsule_schema_version as i64,
            attempt.capsule_envelope_digest,
            String::from_utf8(snapshot_json).expect("snapshot json is utf8"),
            snapshot_digest,
            Utc::now().to_rfc3339(),
            attempt.finalized_at.map(|dt| dt.to_rfc3339()),
        ],
    )
    .expect("raw insert into recovery_attempts");
    conn.last_insert_rowid()
}

// ===========================================================================
// Epoch CAS tests  [C1/B2]
// ===========================================================================

/// GIVEN: a new run with `read_epoch == 0`
/// WHEN: `cas_advance_epoch(tx, R, 0)` is called
/// THEN: epoch advances to 1; `CasOutcome::Advanced { from: 0, to: 1 }`;
///       `read_epoch` returns 1 afterwards.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn epoch_cas_advances_from_zero_to_one() {
    let conn = test_conn();

    // GIVEN: new run starts at epoch 0.
    let initial = read_epoch(&conn, "run-cas-1").expect("read_epoch");
    assert_eq!(initial, 0, "a new run must start at epoch 0");

    // WHEN: CAS advance with expected=0.
    let outcome = {
        let tx = begin_tx(&conn);
        let result = cas_advance_epoch(&tx, "run-cas-1", 0).expect("CAS advance");
        tx.commit().expect("commit tx");
        result
    };

    // THEN: epoch advanced from 0 to 1.
    assert_eq!(
        outcome,
        CasOutcome::Advanced { from: 0, to: 1 },
        "CAS must advance epoch from 0 to 1"
    );

    // AND: the persisted epoch is now 1.
    let after = read_epoch(&conn, "run-cas-1").expect("read_epoch after CAS");
    assert_eq!(after, 1, "epoch must persist as 1 after CAS advance");
}

/// GIVEN: epoch already advanced to 1
/// WHEN: `cas_advance_epoch(tx, R, 0)` is called AGAIN (stale expected)
/// THEN: `CasOutcome::Stale { persisted: 1, expected: 0 }` — the **persisted**
///       value is reported, not just a generic failure.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn epoch_cas_stale_returns_persisted_value() {
    let conn = test_conn();

    // Advance epoch from 0 to 1.
    {
        let tx = begin_tx(&conn);
        let first = cas_advance_epoch(&tx, "run-stale", 0).expect("first CAS");
        assert_eq!(first, CasOutcome::Advanced { from: 0, to: 1 });
        tx.commit().expect("commit first CAS");
    }

    // Second CAS with the now-stale expected=0.
    let outcome = {
        let tx = begin_tx(&conn);
        let result = cas_advance_epoch(&tx, "run-stale", 0).expect("stale CAS");
        tx.commit().expect("commit stale CAS");
        result
    };

    // Stale must carry the **persisted** value (1), not just a failure.
    assert_eq!(
        outcome,
        CasOutcome::Stale {
            persisted: 1,
            expected: 0
        },
        "stale CAS must report the actual persisted epoch (1), not just a failure"
    );
}

/// GIVEN: a concurrent claim has advanced the epoch to 5
/// WHEN: `cas_advance_epoch(tx, R, 0)` is called
/// THEN: the affected-row check detects the mismatch and returns
///       `Stale { persisted: 5, expected: 0 }`, proving the CAS reads the
///       actual persisted epoch rather than assuming sequential progression.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn epoch_cas_detects_concurrent_advance() {
    let conn = test_conn();

    // Simulate a concurrent advance by seeding epoch=5 directly.
    conn.execute(
        "INSERT INTO recovery_epoch (run_id, epoch, updated_at) VALUES (?1, ?2, ?3)",
        params!["run-concurrent", 5_i64, Utc::now().to_rfc3339()],
    )
    .expect("seed concurrent epoch");

    // CAS with expected=0 must detect the mismatch (persisted=5, not 1).
    let outcome = {
        let tx = begin_tx(&conn);
        let result = cas_advance_epoch(&tx, "run-concurrent", 0).expect("concurrent CAS");
        tx.commit().expect("commit");
        result
    };

    assert_eq!(
        outcome,
        CasOutcome::Stale {
            persisted: 5,
            expected: 0
        },
        "CAS must report the actual persisted epoch (5), proving the affected-row check"
    );
}

// ===========================================================================
// Operations ledger tests  [C2/B3]
// ===========================================================================

/// GIVEN: an empty operations ledger
/// WHEN: `insert_pending` then `lookup_logical_operation` is called
/// THEN: the pending operation is returned with `owner_pid` and
///       `lease_expires_at` populated, and status is `Pending`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn insert_pending_then_lookup_returns_pending_with_owner_and_lease() {
    let conn = test_conn();
    let lease = Utc::now() + Duration::minutes(5);
    let insert = PendingOperationInsert {
        operation_id: "op-lookup-1".to_string(),
        run_id: "run-lookup".to_string(),
        epoch: 1,
        step_id: "step-1".to_string(),
        capsule_envelope_digest: "env-digest".to_string(),
        source_attempt_id: None,
        logical_request_key: "logical-key-1".to_string(),
        intent_digest: "intent-digest".to_string(),
        owner_pid: 4242,
        lease_expires_at: lease,
        execution_attempt_id: 7,
    };

    {
        let tx = begin_tx(&conn);
        insert_pending(&tx, &insert).expect("insert_pending");
        tx.commit().expect("commit insert");
    }

    let found = {
        let tx = begin_tx(&conn);
        let result = lookup_logical_operation(&tx, "logical-key-1").expect("lookup");
        tx.commit().expect("commit lookup");
        result
    };

    let op = found.expect("operation must be found");
    assert_eq!(op.operation_id, "op-lookup-1");
    assert_eq!(op.status, OperationStatus::Pending);
    assert_eq!(op.owner_pid, Some(4242), "owner_pid must be persisted [B3]");
    assert_eq!(
        op.lease_expires_at,
        Some(lease),
        "lease_expires_at must be persisted [B3]"
    );
    assert_eq!(
        op.execution_attempt_id,
        Some(7),
        "execution_attempt_id [B4]"
    );
}

/// GIVEN: a Pending operation
/// WHEN: `finalize_completed` is called
/// THEN: status transitions to `Completed` with the serialized outcome,
///       and the function returns the attempt id encoded in the outcome.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn finalize_completed_transitions_to_completed_with_outcome() {
    let conn = test_conn();

    // Seed a pending operation via raw SQL.
    raw_insert_operation(
        &conn,
        &RawOperation::pending_live_lease("op-finalize-1", "logical-key-fc"),
    );

    let outcome_json = r#"{"attempt_id":42,"status":"completed"}"#;

    let returned_attempt_id = {
        let tx = begin_tx(&conn);
        let id =
            finalize_completed(&tx, "op-finalize-1", outcome_json).expect("finalize_completed");
        tx.commit().expect("commit finalize");
        id
    };

    assert_eq!(
        returned_attempt_id, 42,
        "finalize_completed must return the attempt id from the outcome"
    );

    let status = raw_query_operation_status(&conn, "op-finalize-1");
    assert_eq!(
        status,
        OperationStatus::Completed.as_str(),
        "status must transition to Completed"
    );

    // Verify the serialized outcome was persisted.
    let stored_outcome: String = conn
        .query_row(
            "SELECT serialized_outcome FROM recovery_operations WHERE operation_id = ?1",
            params!["op-finalize-1"],
            |row| row.get(0),
        )
        .expect("query outcome");
    assert_eq!(stored_outcome, outcome_json);
}

/// GIVEN: an already-Completed operation
/// WHEN: `finalize_completed` is called AGAIN
/// THEN: returns `Err` (GuardFailed) — the guarded transition refuses to
///       re-finalize.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn finalize_on_already_completed_returns_error() {
    let conn = test_conn();

    // Seed a Completed operation via raw SQL.
    let mut op = RawOperation::pending_live_lease("op-double-fin", "logical-key-df");
    op.status = OperationStatus::Completed.as_str().to_string();
    raw_insert_operation(&conn, &op);

    let result = {
        let tx = begin_tx(&conn);
        let r = finalize_completed(&tx, "op-double-fin", r#"{"attempt_id":1}"#);
        // Roll back regardless; the assertion is on the Err.
        let _ = tx.rollback();
        r
    };

    assert!(
        result.is_err(),
        "finalize_completed on an already-Completed operation must return Err (GuardFailed)"
    );
}

/// GIVEN: an operation exists with logical_request_key K
/// WHEN: a second `insert_pending` is attempted with the SAME logical_request_key
///       but a DIFFERENT operation_id (different exact binding)
/// THEN: the insert fails (UNIQUE constraint) — a logical-request conflict.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn logical_request_key_uniqueness_rejects_conflicting_binding() {
    let conn = test_conn();

    // Seed the first operation with logical_request_key "shared-key".
    raw_insert_operation(
        &conn,
        &RawOperation::pending_live_lease("op-original", "shared-key"),
    );

    // Attempt to insert a second operation with the same logical_request_key
    // but a different operation_id (different exact binding).
    let conflicting = PendingOperationInsert {
        operation_id: "op-conflicting".to_string(),
        run_id: "run-1".to_string(),
        epoch: 1,
        step_id: "step-2".to_string(),
        capsule_envelope_digest: "env-digest-2".to_string(),
        source_attempt_id: None,
        logical_request_key: "shared-key".to_string(),
        intent_digest: "intent-digest-2".to_string(),
        owner_pid: 9999,
        lease_expires_at: Utc::now() + Duration::minutes(5),
        execution_attempt_id: 2,
    };

    let result = {
        let tx = begin_tx(&conn);
        let r = insert_pending(&tx, &conflicting);
        let _ = tx.rollback();
        r
    };

    assert!(
        result.is_err(),
        "a second operation with the same logical_request_key but different exact binding \
         must be rejected (UNIQUE constraint / logical-request conflict)"
    );

    // The original operation must be unchanged.
    let status = raw_query_operation_status(&conn, "op-original");
    assert_eq!(
        status,
        OperationStatus::Pending.as_str(),
        "original operation must be unchanged after rejected conflict"
    );
}

// ===========================================================================
// Append-only attempts tests  [C3/B4]
// ===========================================================================

/// GIVEN: an empty attempt store
/// WHEN: `record_attempt_start` is called
/// THEN: a row exists with `step_status = 'started'` and `finalized_at = NULL`
///       BEFORE the outcome is appended.
/// WHEN: `append_attempt_outcome` is called
/// THEN: `finalized_at` is set.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn record_attempt_start_creates_started_row_with_null_finalized_at() {
    let conn = test_conn();
    let snapshot = test_snapshot(0, 0, "running");

    let attempt_id = {
        let tx = begin_tx(&conn);
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-attempt-1",
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-attempt-1",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot,
            },
        )
        .expect("record_attempt_start");
        tx.commit().expect("commit");
        id
    };

    // Verify the started row has finalized_at = NULL.
    let finalized_at: Option<String> = conn
        .query_row(
            "SELECT finalized_at FROM recovery_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .expect("query finalized_at");
    assert!(
        finalized_at.is_none(),
        "started row must have finalized_at = NULL before outcome is appended [B4]"
    );

    // Verify step_status is 'started'.
    let step_status: String = conn
        .query_row(
            "SELECT step_status FROM recovery_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .expect("query step_status");
    assert_eq!(step_status, "started", "step_status must be 'started' [B4]");

    // Append the outcome.
    let outcome_snapshot = test_snapshot(1, 0, "completed");
    {
        let tx = begin_tx(&conn);
        append_attempt_outcome(
            &tx,
            attempt_id,
            "completed",
            &outcome_snapshot,
            Some(&serde_json::json!({"result": "ok"})),
            Some("checkpoint-digest"),
        )
        .expect("append_attempt_outcome");
        tx.commit().expect("commit outcome");
    }

    // finalized_at must now be set.
    let finalized_after: Option<String> = conn
        .query_row(
            "SELECT finalized_at FROM recovery_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .expect("query finalized_at after");
    assert!(
        finalized_after.is_some(),
        "finalized_at must be set after outcome is appended [B4]"
    );
}

/// GIVEN: an empty attempt store
/// WHEN: two attempts are recorded for the same run+step
/// THEN: attempt_ids are strictly monotonically increasing.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn attempt_ids_strictly_monotonic() {
    let conn = test_conn();
    let snapshot = test_snapshot(0, 0, "running");

    let (id1, id2) = {
        let tx = begin_tx(&conn);
        let first = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-mono",
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-mono",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot,
            },
        )
        .expect("first record_attempt_start");
        let second = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-mono",
                epoch: 2,
                source_attempt_id: Some(first),
                operation_id: "op-mono",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot,
            },
        )
        .expect("second record_attempt_start");
        tx.commit().expect("commit");
        (first, second)
    };

    assert!(
        id2 > id1,
        "attempt_ids must be strictly monotonically increasing (got {id1} then {id2}) [C3]"
    );
    assert_eq!(
        raw_count_attempts(&conn, "run-mono", "step-1"),
        2,
        "exactly two rows must exist (append-only, no reuse) [C3]"
    );
}

/// GIVEN: an attempt row recorded with snapshot1
/// WHEN: a second attempt is recorded with snapshot2, and the first row's
///       outcome is appended
/// THEN: the ORIGINAL row's `state_snapshot` is **unchanged** — no existing
///       row is mutated except the guarded outcome-append. The complete
///       `StateSnapshot` of the original row must match snapshot1.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn append_only_original_snapshot_unchanged_after_new_append() {
    let conn = test_conn();
    let snapshot1 = snapshot_with_context();
    let snapshot2 = test_snapshot(9, 9, "resumed");

    let (id1, _id2) = {
        let tx = begin_tx(&conn);
        let first = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-append-only",
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-ao",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot1,
            },
        )
        .expect("first record_attempt_start");
        let second = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-append-only",
                epoch: 2,
                source_attempt_id: Some(first),
                operation_id: "op-ao",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot2,
            },
        )
        .expect("second record_attempt_start");
        tx.commit().expect("commit");
        (first, second)
    };

    // Independently compute the canonical serialization of snapshot1.
    let expected_json = independent_canonical_snapshot(&snapshot1);

    // Load the original row's state_snapshot_json from the DB.
    let stored_json: String = conn
        .query_row(
            "SELECT state_snapshot_json FROM recovery_attempts WHERE attempt_id = ?1",
            params![id1],
            |row| row.get(0),
        )
        .expect("query state_snapshot_json");

    assert_eq!(
        stored_json.as_bytes(),
        expected_json.as_slice(),
        "the ORIGINAL row's StateSnapshot must be unchanged after a new append [C3]"
    );

    // Exactly two rows — no row reuse or mutation.
    assert_eq!(
        raw_count_attempts(&conn, "run-append-only", "step-1"),
        2,
        "append-only store must have exactly two rows [C3]"
    );
}

/// GIVEN: a loaded attempt row
/// WHEN: `verify_snapshot_digest` is called
/// THEN: it succeeds — the stored `snapshot_digest` equals an independently
///       computed SHA-256 of the canonical `StateSnapshot` serialization.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn verify_snapshot_digest_matches_independently_computed_sha256() {
    let conn = test_conn();
    let snapshot = snapshot_with_context();

    // Independently compute the expected digest.
    let canonical = independent_canonical_snapshot(&snapshot);
    let expected_digest = independent_sha256_hex(&canonical);

    let attempt_id = {
        let tx = begin_tx(&conn);
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-digest",
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-digest",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot,
            },
        )
        .expect("record_attempt_start");
        tx.commit().expect("commit");
        id
    };

    // Verify the stored digest equals the independently computed SHA-256.
    let stored_digest: String = conn
        .query_row(
            "SELECT snapshot_digest FROM recovery_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .expect("query snapshot_digest");
    assert_eq!(
        stored_digest, expected_digest,
        "stored snapshot_digest must equal the independently computed SHA-256 [C3]"
    );

    // verify_snapshot_digest must succeed on the loaded row.
    let row = {
        let tx = begin_tx(&conn);
        // Use load_attempt to get the full row.
        let r = luther_workflow::persistence::attempts::load_attempt(&tx, attempt_id)
            .expect("load_attempt");
        let _ = tx.rollback();
        r
    };
    verify_snapshot_digest(&row).expect("verify_snapshot_digest must succeed on a valid row");
}

/// GIVEN: an attempt-start was recorded but the outcome was NOT appended
/// WHEN: `load_unfinalized_for_operation` is called
/// THEN: it returns the row with `finalized_at = NULL` (crash recovery).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn load_unfinalized_for_operation_returns_crash_recovery_row() {
    let conn = test_conn();
    let snapshot = test_snapshot(0, 0, "running");

    // Record an attempt start (simulates a crash before outcome-append).
    let attempt_id = {
        let tx = begin_tx(&conn);
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id: "run-crash",
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-crash",
                step_id: "step-1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest",
                state_snapshot: &snapshot,
            },
        )
        .expect("record_attempt_start");
        tx.commit().expect("commit");
        id
    };

    let unfinalized =
        load_unfinalized_for_operation(&conn, "op-crash").expect("load_unfinalized_for_operation");

    let row = unfinalized.expect("an unfinalized row must exist for crash recovery [B4]");
    assert_eq!(row.attempt_id, attempt_id);
    assert!(
        row.finalized_at.is_none(),
        "crash-recovery row must have finalized_at = NULL [B4]"
    );
}

/// GIVEN: an attempt row that already has `finalized_at` set
/// WHEN: `append_attempt_outcome` is called on it
/// THEN: returns `Err` (OutcomeAlreadyAppended) — the guarded append refuses
///       to re-finalize.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-003
#[test]
fn append_outcome_on_already_finalized_returns_error() {
    let conn = test_conn();
    let snapshot = test_snapshot(0, 0, "running");

    // Seed a finalized attempt row via raw SQL.
    let attempt_id = raw_insert_attempt(
        &conn,
        &RawAttempt {
            run_id: "run-already-fin".to_string(),
            epoch: 1,
            source_attempt_id: None,
            operation_id: "op-already-fin".to_string(),
            step_id: "step-1".to_string(),
            step_status: "completed".to_string(),
            capsule_schema_version: 1,
            capsule_envelope_digest: "env-digest".to_string(),
            state_snapshot: snapshot.clone(),
            finalized_at: Some(Utc::now()),
        },
    );

    // Attempt to append outcome to the already-finalized row.
    let result = {
        let tx = begin_tx(&conn);
        let r = append_attempt_outcome(&tx, attempt_id, "completed", &snapshot, None, None);
        let _ = tx.rollback();
        r
    };

    assert!(
        result.is_err(),
        "append_attempt_outcome on an already-finalized row must return Err (OutcomeAlreadyAppended) [B4]"
    );
}

// ===========================================================================
// Lease adoption tests  [B3]
// ===========================================================================

/// GIVEN: a Pending operation with an expired lease
/// WHEN: `find_adoptable_pending` is called
/// THEN: it returns the adoptable operation.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn find_adoptable_pending_finds_expired_lease() {
    let conn = test_conn();
    let now = Utc::now();

    // Seed a pending operation with an EXPIRED lease.
    raw_insert_operation(
        &conn,
        &RawOperation::pending_expired_lease("op-adopt-find", "logical-key-adopt"),
    );

    let adoptable = {
        let tx = begin_tx(&conn);
        let result =
            find_adoptable_pending(&tx, "logical-key-adopt", now).expect("find_adoptable_pending");
        let _ = tx.rollback();
        result
    };

    let op = adoptable.expect("an expired-lease pending op must be adoptable [B3]");
    assert_eq!(op.operation_id, "op-adopt-find");
    assert_eq!(op.status, OperationStatus::Pending);
}

/// GIVEN: a Pending operation with an expired lease
/// WHEN: `try_adopt_pending` is called with a new owner and lease
/// THEN: returns `AdoptOutcome::Adopted`, and the operation's `owner_pid` is
///       updated to the new owner.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-004
#[test]
fn try_adopt_pending_adopts_expired_lease() {
    let conn = test_conn();
    let now = Utc::now();
    let new_pid = 5555;
    let new_lease = now + Duration::minutes(5);

    // Seed a pending operation with an EXPIRED lease.
    raw_insert_operation(
        &conn,
        &RawOperation::pending_expired_lease("op-adopt-try", "logical-key-adopt2"),
    );

    let outcome = {
        let tx = begin_tx(&conn);
        let result = try_adopt_pending(&tx, "op-adopt-try", new_pid, new_lease, now)
            .expect("try_adopt_pending");
        tx.commit().expect("commit adopt");
        result
    };

    assert_eq!(
        outcome,
        AdoptOutcome::Adopted,
        "adopting an expired-lease op must return Adopted [B3]"
    );

    // Verify the owner_pid was updated.
    let owner_pid: Option<i64> = conn
        .query_row(
            "SELECT owner_pid FROM recovery_operations WHERE operation_id = ?1",
            params!["op-adopt-try"],
            |row| row.get(0),
        )
        .expect("query owner_pid");
    assert_eq!(
        owner_pid,
        Some(new_pid as i64),
        "owner_pid must be updated to the new owner after adoption [B3]"
    );
}
