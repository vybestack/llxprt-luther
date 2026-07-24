//! Integration tests for the durable effect-intent state machine.
//!
//! These tests exercise the **real durable store (SQLite)** directly, asserting
//! durable invariants for `prepare_effect`, `reconcile_effect`, and
//! `finalize_effect`. They are the **RED phase** for P04: they compile and
//! assert real invariants, but fail because the designated P03 stubs
//! (`todo!()`) are reached. P05 will implement the stubs to turn these green.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use luther_workflow::persistence::effect_intents::{
    compute_effect_key, finalize_effect, init_effect_intents_table, prepare_effect,
    reconcile_effect, EffectKind, EffectPreparation, ReconcileVerdict,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create an in-memory SQLite connection with the effect_intents table
/// initialized.
fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_effect_intents_table(&conn).expect("init effect intents table");
    conn
}

/// Begin an IMMEDIATE transaction.
fn begin_tx(conn: &Connection) -> Transaction<'_> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate).expect("begin IMMEDIATE tx")
}

/// Independently compute a lowercase-hex SHA-256 digest of a byte slice.
fn independent_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Query the persisted `status` of an effect intent by key.
fn raw_query_effect_status(conn: &Connection, key: &str) -> String {
    conn.query_row(
        "SELECT status FROM effect_intents WHERE effect_key = ?1",
        params![key],
        |row| row.get(0),
    )
    .expect("query effect status")
}

/// Read the persisted `payload_digest` of an effect intent by key.
fn raw_query_payload_digest(conn: &Connection, key: &str) -> String {
    conn.query_row(
        "SELECT payload_digest FROM effect_intents WHERE effect_key = ?1",
        params![key],
        |row| row.get(0),
    )
    .expect("query payload_digest")
}

/// Read the persisted `observed_result` of an effect intent by key.
fn raw_query_observed_result(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT observed_result FROM effect_intents WHERE effect_key = ?1",
        params![key],
        |row| row.get(0),
    )
    .expect("query observed_result")
}

// ===========================================================================
// compute_effect_key tests  [C7]
// ===========================================================================

/// GIVEN: a binding (operation_id, attempt_id, sequence, kind)
/// WHEN: `compute_effect_key` is called twice with the same binding
/// THEN: the key is deterministic (identical both times) and is a lowercase
///       hex SHA-256 digest.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn compute_effect_key_is_deterministic_for_same_binding() {
    let key1 = compute_effect_key("op-1", 10, 0, EffectKind::Commit.as_str());
    let key2 = compute_effect_key("op-1", 10, 0, EffectKind::Commit.as_str());

    assert_eq!(
        key1, key2,
        "effect key must be deterministic for the same binding [C7]"
    );
    assert_eq!(key1.len(), 64, "effect key must be a SHA-256 hex digest");
    assert!(
        key1.chars().all(|c| c.is_ascii_hexdigit()),
        "effect key must be lowercase hex"
    );
}

/// GIVEN: two different bindings
/// WHEN: `compute_effect_key` is called for each
/// THEN: the keys differ.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn compute_effect_key_differs_for_different_binding() {
    let key_commit = compute_effect_key("op-1", 10, 0, EffectKind::Commit.as_str());
    let key_push = compute_effect_key("op-1", 10, 0, EffectKind::Push.as_str());

    assert_ne!(
        key_commit, key_push,
        "effect keys must differ when the binding differs (kind) [C7]"
    );
}

// ===========================================================================
// prepare_effect tests  [C7/B5]
// ===========================================================================

/// GIVEN: an empty effect-intents store
/// WHEN: `prepare_effect` is called with a Commit payload
/// THEN: a row exists with `status = 'prepared'`,
///       `payload_digest = sha256(canonical_payload)`, and a stable `effect_key`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn prepare_effect_stores_digest_and_prepared_status() {
    let conn = test_conn();
    let payload = br#"{"tree":"abc","message":"commit message"}"#;
    let expected_digest = independent_sha256_hex(payload);

    let key = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-prep-1",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some("computed-sha"),
                expected_predecessor: Some("parent-sha"),
            },
        )
        .expect("prepare_effect");
        tx.commit().expect("commit");
        intent.effect_key.clone()
    };

    // Verify the row exists with status='prepared'.
    let status = raw_query_effect_status(&conn, &key);
    assert_eq!(
        status, "prepared",
        "prepare_effect must persist status='prepared' [C7]"
    );

    // Verify the payload_digest equals the independently computed SHA-256.
    let digest = raw_query_payload_digest(&conn, &key);
    assert_eq!(
        digest, expected_digest,
        "payload_digest must equal the independently computed SHA-256 of the payload [C7]"
    );

    // Verify the stable key is deterministic.
    let expected_key = compute_effect_key("op-prep-1", 1, 0, EffectKind::Commit.as_str());
    assert_eq!(
        key, expected_key,
        "effect_key must be the deterministic compute_effect_key value [C7]"
    );
}

/// GIVEN: an existing prepared effect intent
/// WHEN: `prepare_effect` is called AGAIN with the SAME exact binding (same
///       payload_digest, expected_target, expected_predecessor)
/// THEN: returns the EXISTING intent (insert-or-load, idempotent) — NOT a new
///       row and NOT an error.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn prepare_effect_same_exact_binding_returns_existing_intent() {
    let conn = test_conn();
    let payload = br#"{"tree":"abc","message":"idempotent"}"#;

    // First prepare.
    let first = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-idem",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some("target-1"),
                expected_predecessor: Some("pred-1"),
            },
        )
        .expect("first prepare_effect");
        tx.commit().expect("commit");
        intent
    };

    // Second prepare with the SAME exact binding.
    let second = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-idem",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some("target-1"),
                expected_predecessor: Some("pred-1"),
            },
        )
        .expect("second prepare_effect (idempotent load)");
        tx.commit().expect("commit");
        intent
    };

    // The two intents must be the same (insert-or-load idempotency).
    assert_eq!(
        first.effect_key, second.effect_key,
        "same exact binding must return the EXISTING intent [B5]"
    );
    assert_eq!(
        first.payload_digest, second.payload_digest,
        "idempotent prepare must preserve the original digest [B5]"
    );

    // Exactly one row must exist.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM effect_intents WHERE effect_key = ?1",
            params![&first.effect_key],
            |row| row.get(0),
        )
        .expect("count rows");
    assert_eq!(
        count, 1,
        "insert-or-load must NOT create a duplicate row [B5]"
    );
}

/// GIVEN: an existing prepared effect intent with key K
/// WHEN: `prepare_effect` is called with the SAME key K but a DIFFERENT exact
///       binding (different payload_digest)
/// THEN: the intent transitions to `conflict` and `prepare_effect` returns
///       `Err` (BindingConflict).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn prepare_effect_different_binding_with_same_key_returns_binding_conflict() {
    let conn = test_conn();
    let payload_a = br#"{"tree":"aaa"}"#;
    let payload_b = br#"{"tree":"bbb"}"#;

    // First prepare with payload_a.
    let first_key = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-conflict",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload: payload_a,
                expected_target: Some("target-1"),
                expected_predecessor: Some("pred-1"),
            },
        )
        .expect("first prepare_effect");
        tx.commit().expect("commit");
        intent.effect_key
    };

    // Second prepare with payload_b (same key, different binding).
    let result = {
        let tx = begin_tx(&conn);
        let r = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-conflict",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload: payload_b,
                expected_target: Some("target-1"),
                expected_predecessor: Some("pred-1"),
            },
        );
        tx.commit().expect("commit conflict transition");
        r
    };

    assert!(
        result.is_err(),
        "prepare_effect with a different binding under the same key must return Err (BindingConflict) [B5]"
    );

    // The intent must be in 'conflict' state.
    let status = raw_query_effect_status(&conn, &first_key);
    assert_eq!(
        status, "conflict",
        "a binding conflict must transition the intent to 'conflict' [B5]"
    );
}

// ===========================================================================
// reconcile_effect tests  [C7]
// ===========================================================================

/// GIVEN: a prepared Commit effect intent with expected_target=T
/// WHEN: `reconcile_effect` is called with an observed state that MATCHES
///       expected_target
/// THEN: returns `ReconcileVerdict::Completed { result: Some(T) }`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn reconcile_effect_completed_when_observed_matches_expected_target() {
    let conn = test_conn();
    let payload = br#"{"tree":"abc"}"#;
    let target = "computed-target-sha";

    let key = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-reconcile-ok",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some(target),
                expected_predecessor: Some("parent-sha"),
            },
        )
        .expect("prepare_effect");
        tx.commit().expect("commit");
        intent.effect_key
    };

    // Reconcile with an observed HEAD that matches expected_target.
    let verdict = {
        let tx = begin_tx(&conn);
        let result = reconcile_effect(
            &tx,
            &key,
            &luther_workflow::persistence::effect_intents::ObservedState {
                head_sha: Some(target.to_string()),
                remote_ref_sha: None,
                matching_pr_number: None,
            },
        )
        .expect("reconcile_effect");
        tx.commit().expect("commit");
        result
    };

    match verdict {
        ReconcileVerdict::Completed { result } => {
            assert_eq!(
                result,
                Some(target.to_string()),
                "Completed verdict must carry the observed target sha [C7]"
            );
        }
        other => panic!(
            "expected ReconcileVerdict::Completed, got {other:?} when observed matches expected_target"
        ),
    }
}

/// GIVEN: a prepared Commit effect intent with expected_target=T and
///        expected_predecessor=P
/// WHEN: `reconcile_effect` is called with an observed HEAD that is NEITHER T
///       NOR P (unexpected)
/// THEN: returns `ReconcileVerdict::Conflict`.
///
/// This test would FAIL if `reconcile_effect` always returned `Completed`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn reconcile_effect_conflict_when_observed_is_unexpected() {
    let conn = test_conn();
    let payload = br#"{"tree":"abc"}"#;

    let key = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-reconcile-conflict",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some("expected-target"),
                expected_predecessor: Some("expected-predecessor"),
            },
        )
        .expect("prepare_effect");
        tx.commit().expect("commit");
        intent.effect_key
    };

    // Reconcile with an observed HEAD that matches NEITHER expected_target NOR
    // expected_predecessor.
    let verdict = {
        let tx = begin_tx(&conn);
        let result = reconcile_effect(
            &tx,
            &key,
            &luther_workflow::persistence::effect_intents::ObservedState {
                head_sha: Some("totally-unexpected-sha".to_string()),
                remote_ref_sha: None,
                matching_pr_number: None,
            },
        )
        .expect("reconcile_effect");
        tx.commit().expect("commit");
        result
    };

    assert!(
        matches!(verdict, ReconcileVerdict::Conflict { .. }),
        "an unexpected observed HEAD must yield Conflict, not Completed — \
         this would fail if reconcile_effect always returned Completed [C7]"
    );
}

// ===========================================================================
// finalize_effect guard tests  [C7]
// ===========================================================================

/// GIVEN: an effect intent that is already `completed`
/// WHEN: `finalize_effect` is called on it
/// THEN: returns `Err` (GuardFailed) — the guarded transition refuses to
///       re-finalize.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement:REQ-RP-008
#[test]
fn finalize_effect_guard_double_finalize_returns_error() {
    let conn = test_conn();
    let payload = br#"{"tree":"abc"}"#;

    let key = {
        let tx = begin_tx(&conn);
        let intent = prepare_effect(
            &tx,
            &EffectPreparation {
                operation_id: "op-finalize-double",
                attempt_id: 1,
                sequence: 0,
                kind: EffectKind::Commit,
                payload,
                expected_target: Some("target"),
                expected_predecessor: Some("pred"),
            },
        )
        .expect("prepare_effect");
        tx.commit().expect("commit");
        intent.effect_key
    };

    // First finalize: prepared → completed.
    {
        let tx = begin_tx(&conn);
        finalize_effect(&tx, &key, "completed", Some("observed-result"))
            .expect("first finalize_effect");
        tx.commit().expect("commit");
    }
    assert_eq!(
        raw_query_effect_status(&conn, &key),
        "completed",
        "first finalize must transition to completed"
    );
    assert_eq!(
        raw_query_observed_result(&conn, &key),
        Some("observed-result".to_string()),
        "first finalize must persist observed_result"
    );

    // Second finalize: completed → completed must fail (GuardFailed).
    let result = {
        let tx = begin_tx(&conn);
        let r = finalize_effect(&tx, &key, "completed", Some("observed-result"));
        let _ = tx.rollback();
        r
    };

    assert!(
        result.is_err(),
        "finalize_effect on an already-completed intent must return Err (GuardFailed) [C7]"
    );
}
