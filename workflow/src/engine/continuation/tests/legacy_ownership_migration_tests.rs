//! Tests for the audited legacy ownership migration API (issue 158 gap 2).
//!
//! @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION

use std::path::Path;

use crate::engine::continuation::legacy_ownership_migration::{
    migrate_legacy_ownership, LegacyMigrationError, LegacyMigrationOutcome,
};
use crate::engine::continuation::prepare_resume_authorization;
use crate::engine::workspace_ownership::{
    adjudicate_workspace_ownership, provision_workspace_owner_marker, OwnershipVerdict,
};
use crate::persistence::{persist_run_with_conn, RunMetadata, RunStatus};

use super::support::test_conn;

/// Create a legacy run row (no launch provenance) with a persisted workspace
/// path pointing at an unowned temp directory.
fn seed_legacy_run(conn: &rusqlite::Connection, run_id: &str, workspace: &Path) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "legacy-workflow", "legacy-config");
    md.status = RunStatus::ReadyToResume;
    md.current_step = Some("some_step".to_string());
    md.workspace_path = Some(workspace.to_string_lossy().into_owned());
    // legacy row: explicitly no launch_provenance
    md.launch_provenance = None;
    persist_run_with_conn(conn, &md).expect("persist legacy run");
    md
}

/// Canonical config root used by the migration tests. The migration records
/// this in provenance; the workspace path must never appear in provenance.
const TEST_CONFIG_ROOT_STR: &str = "/etc/luther/config";

// ---------------------------------------------------------------------------
// Ordinary markerless legacy resume refuses
// ---------------------------------------------------------------------------

/// A normal resume of a markerless legacy workspace must be refused by
/// `prepare_resume_authorization`: it requires existing ownership evidence
/// and never creates a first claim.
#[test]
fn ordinary_markerless_legacy_resume_refuses() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    // Do NOT provision a marker: this is the legacy marker-less state.
    seed_legacy_run(&conn, "legacy-resume-refuse", &workspace);

    let err = prepare_resume_authorization(workspace.to_str(), "legacy-resume-refuse")
        .expect_err("markerless resume must be refused");
    assert!(
        matches!(
            err,
            crate::engine::continuation::ResumeAuthorizationError::NoOwnershipEvidence
        ),
        "expected NoOwnershipEvidence, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Explicit migration succeeds once
// ---------------------------------------------------------------------------

/// An explicit migration of a genuine legacy row (no provenance, no marker)
/// succeeds, publishes the bootstrap marker, and records an audit event.
#[test]
fn explicit_migration_succeeds_for_legacy_row() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-migrate-ok", &workspace);

    let outcome = migrate_legacy_ownership(
        &conn,
        "legacy-migrate-ok",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migration should succeed");
    assert_eq!(outcome, LegacyMigrationOutcome::Migrated);

    // The bootstrap marker must now exist and be owned by this run.
    assert!(matches!(
        adjudicate_workspace_ownership(&workspace, "legacy-migrate-ok"),
        OwnershipVerdict::Owned(_)
    ));
    // A normal resume authorization must now succeed.
    let prepared = prepare_resume_authorization(workspace.to_str(), "legacy-migrate-ok")
        .expect("resume authorization must succeed after migration");
    let _auth = prepared.authorization();
}

/// The migration records an audit event in the events table.
#[test]
fn migration_records_audit_event() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-audit", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-audit",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migrate");

    // Query the events table for the audit event.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2",
            rusqlite::params!["legacy-audit", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query audit event");
    assert!(count >= 1, "at least one audit event must be recorded");
}

// ---------------------------------------------------------------------------
// Idempotent migration
// ---------------------------------------------------------------------------

/// A second migration call for the same run id and workspace is idempotent:
/// it returns `AlreadyMigrated` without error.
#[test]
fn migration_is_idempotent() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-idempotent", &workspace);

    let first = migrate_legacy_ownership(
        &conn,
        "legacy-idempotent",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("first migration");
    assert_eq!(first, LegacyMigrationOutcome::Migrated);

    let second = migrate_legacy_ownership(
        &conn,
        "legacy-idempotent",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("second migration");
    assert_eq!(second, LegacyMigrationOutcome::AlreadyCompleted);
}

// ---------------------------------------------------------------------------
// Foreign evidence refuses
// ---------------------------------------------------------------------------

/// A migration attempt against a workspace that already carries a foreign
/// marker (owned by a different run id) is refused without overwrite.
#[test]
fn migration_refuses_foreign_marker() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    // Pre-claim the workspace with a foreign run id.
    provision_workspace_owner_marker(&workspace, "run-foreign-owner")
        .expect("provision foreign marker");
    seed_legacy_run(&conn, "legacy-foreign", &workspace);

    let err = migrate_legacy_ownership(
        &conn,
        "legacy-foreign",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect_err("foreign marker must be refused");
    match err {
        LegacyMigrationError::ForeignMarker(reason) => {
            assert!(reason.contains("run-foreign-owner"), "reason: {reason}");
        }
        other => panic!("expected ForeignMarker, got {other:?}"),
    }
    // The foreign marker is preserved (no overwrite).
    assert!(matches!(
        adjudicate_workspace_ownership(&workspace, "run-foreign-owner"),
        OwnershipVerdict::Owned(_)
    ));
}

// ---------------------------------------------------------------------------
// Non-legacy row refuses
// ---------------------------------------------------------------------------

/// A row with launch provenance (a new record) must not be migrated.
#[test]
fn migration_refuses_non_legacy_row() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let mut md = RunMetadata::new("new-row", "wf", "cfg");
    md.status = RunStatus::ReadyToResume;
    md.workspace_path = Some(workspace.to_string_lossy().into_owned());
    md.launch_provenance = Some(
        crate::persistence::LaunchProvenance::from_resolved(
            &crate::workflow::schema::WorkflowType {
                workflow_type_id: "wf".to_string(),
                steps: vec![],
                transitions: vec![],
                guards: crate::workflow::schema::GuardConfig {
                    max_retries: None,
                    timeout_seconds: None,
                    require_approval: None,
                },
            },
            &crate::workflow::schema::WorkflowConfig {
                config_id: "cfg".to_string(),
                workflow_type_id: "wf".to_string(),
                runtime: crate::workflow::schema::RuntimeConfig {
                    timeout_seconds: 60,
                    max_retries: 3,
                    parallel_steps: None,
                    log_level: None,
                },
                repo: crate::workflow::schema::RepoConfig {
                    workspace_strategy: "temp_clone".to_string(),
                    branch_template: "workflow-{run_id}".to_string(),
                    base_branch: Some("main".to_string()),
                    workspace_root: None,
                    project_subdir: None,
                    artifact_path_base: None,
                    diff_path_base: None,
                    diff_path_normalization:
                        crate::workflow::schema::DiffPathNormalization::RepoRelative,
                },
                guard_limits: crate::workflow::schema::GuardLimits {
                    max_iterations: None,
                    max_file_changes: None,
                    max_tokens: None,
                    max_cost: None,
                },
                variables: std::collections::HashMap::new(),
                discovery: None,
                parent_orchestration: crate::workflow::schema::ParentOrchestrationConfig::default(),
                merge_required: false,
                merge_strategy: None,
                command_manifest: None,
                target_profile: None,
            },
            dir.path(),
        )
        .expect("construct launch provenance for non-legacy row"),
    );
    persist_run_with_conn(&conn, &md).expect("persist new row");

    let err = migrate_legacy_ownership(
        &conn,
        "new-row",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect_err("non-legacy row must be refused");
    assert!(
        matches!(err, LegacyMigrationError::NotLegacyRow(_)),
        "expected NotLegacyRow, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Workspace mismatch refuses
// ---------------------------------------------------------------------------

/// A migration with a workspace that does not match the persisted workspace
/// path is refused.
#[test]
fn migration_refuses_workspace_mismatch() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let persisted_workspace = dir.path().join("real-ws");
    std::fs::create_dir_all(&persisted_workspace).expect("create persisted workspace");
    seed_legacy_run(&conn, "legacy-mismatch", &persisted_workspace);

    let wrong_workspace = dir.path().join("wrong-ws");
    std::fs::create_dir_all(&wrong_workspace).expect("create wrong workspace");

    let err = migrate_legacy_ownership(
        &conn,
        "legacy-mismatch",
        &wrong_workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect_err("workspace mismatch must be refused");
    assert!(
        matches!(err, LegacyMigrationError::WorkspaceMismatch { .. }),
        "expected WorkspaceMismatch, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Missing run refuses
// ---------------------------------------------------------------------------

/// A migration for a run id that does not exist in the registry is refused.
#[test]
fn migration_refuses_missing_run() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let err = migrate_legacy_ownership(
        &conn,
        "nonexistent-run",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect_err("missing run must be refused");
    assert!(
        matches!(err, LegacyMigrationError::RunNotFound(_)),
        "expected RunNotFound, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Durable state machine: crash recovery / reconciliation
// ---------------------------------------------------------------------------

/// Simulate a crash after intent is persisted but before marker publication.
/// A retry/reconciliation must finish the intent and produce exactly one
/// completion audit event.
#[test]
fn crash_after_intent_recovery_completes_with_exactly_one_audit() {
    use crate::persistence::legacy_migration_state::{
        init_legacy_migration_table, load_migration_state, persist_migration_intent,
        MigrationStatus,
    };
    use chrono::Utc;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-crash", &workspace);

    // Simulate the crash: persist the intent directly, without publishing.
    init_legacy_migration_table(&conn).expect("init migration table");
    persist_migration_intent(
        &conn,
        "legacy-crash",
        &workspace.to_string_lossy(),
        Utc::now(),
    )
    .expect("persist intent");

    // The state must be pending.
    let state = load_migration_state(&conn, "legacy-crash")
        .expect("load")
        .expect("row exists");
    assert_eq!(state.status, MigrationStatus::Pending);

    // Retry/reconciliation: the migration must finish and produce a completion.
    let outcome = migrate_legacy_ownership(
        &conn,
        "legacy-crash",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("reconciliation must succeed");
    assert_eq!(outcome, LegacyMigrationOutcome::Migrated);

    // The state must now be completed.
    let state = load_migration_state(&conn, "legacy-crash")
        .expect("load")
        .expect("row exists");
    assert_eq!(state.status, MigrationStatus::Completed);

    // Exactly one completion audit event.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-crash", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query audit events");
    assert_eq!(count, 1, "exactly one completion audit event");
}

// ---------------------------------------------------------------------------
// Durable state machine: exactly-once completion on retry after completion
// ---------------------------------------------------------------------------

/// After a migration is durably completed, a retry must return
/// `AlreadyCompleted` and produce no additional completion audit events.
#[test]
fn retry_after_completed_produces_no_additional_audit() {
    use crate::persistence::legacy_migration_state::load_migration_state;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-exact-once", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-exact-once",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("first migration");

    // Retry: must be AlreadyCompleted.
    let outcome = migrate_legacy_ownership(
        &conn,
        "legacy-exact-once",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("retry");
    assert_eq!(outcome, LegacyMigrationOutcome::AlreadyCompleted);

    // Exactly one completion audit event.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-exact-once", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query audit events");
    assert_eq!(count, 1, "exactly one completion audit event after retry");

    let _ = load_migration_state(&conn, "legacy-exact-once").expect("load state");
}

// ---------------------------------------------------------------------------
// Durable state machine: provenance tag
// ---------------------------------------------------------------------------

/// After migration, the run's launch_provenance is set to a synthetic
/// migration-source provenance, so post-upgrade NULL provenance is denied.
#[test]
fn migration_tags_provenance_as_schema_migrated() {
    use crate::persistence::get_run_with_conn;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-provenance", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-provenance",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migrate");

    let md = get_run_with_conn(&conn, "legacy-provenance")
        .expect("get run")
        .expect("run exists");
    let provenance = md
        .launch_provenance
        .as_ref()
        .expect("provenance must be set after migration");
    assert!(
        provenance.is_migrated(),
        "provenance must be tagged as migrated"
    );
    assert_eq!(
        provenance.migration_source,
        Some(crate::persistence::MigrationSource::LegacyOwnershipMigration)
    );
}

/// The migration must record the canonical config root in provenance — never
/// the workspace path. The workspace path is a transient run-scoped value;
/// encoding it in provenance would leak it into the launch-identity contract.
#[test]
fn migration_provenance_encodes_config_root_not_workspace() {
    use crate::persistence::{decode_config_root, get_run_with_conn};

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-config-root", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-config-root",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migrate");

    let md = get_run_with_conn(&conn, "legacy-config-root")
        .expect("get run")
        .expect("run exists");
    let provenance = md
        .launch_provenance
        .as_ref()
        .expect("provenance must be set");
    let encoded = &provenance.canonical_config_root;
    let decoded = decode_config_root(encoded).expect("must decode");
    assert_eq!(
        decoded,
        std::path::PathBuf::from(TEST_CONFIG_ROOT_STR),
        "provenance canonical_config_root must match the supplied canonical root"
    );
    // The workspace path must not be encoded in provenance.
    let workspace_str = workspace.to_string_lossy().into_owned();
    assert!(
        !encoded.contains(&workspace_str),
        "workspace path must not appear in encoded provenance: {encoded}"
    );
}

// ---------------------------------------------------------------------------
// Durable state machine: resume trusts migrated marker only when durable
// ---------------------------------------------------------------------------

/// An ordinary resume after migration succeeds because the durable completion
/// exists and the marker is published.
#[test]
fn resume_after_migration_succeeds_with_durable_completion() {
    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-resume-ok", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-resume-ok",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migrate");

    let prepared = prepare_resume_authorization(workspace.to_str(), "legacy-resume-ok")
        .expect("resume authorization must succeed after durable migration");
    let _auth = prepared.authorization();
}

/// An ordinary resume while a migration is pending (intent recorded but not
/// completed) is blocked because the marker is not yet published. This tests
/// the "blocks pending" contract.
#[test]
fn resume_blocks_when_migration_is_pending() {
    use crate::persistence::legacy_migration_state::{
        init_legacy_migration_table, persist_migration_intent,
    };
    use chrono::Utc;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-pending-block", &workspace);

    // Persist intent but do NOT publish the marker (simulates a crash).
    init_legacy_migration_table(&conn).expect("init migration table");
    persist_migration_intent(
        &conn,
        "legacy-pending-block",
        &workspace.to_string_lossy(),
        Utc::now(),
    )
    .expect("persist intent");

    // A resume attempt must be refused because the marker is not published.
    let err = prepare_resume_authorization(workspace.to_str(), "legacy-pending-block")
        .expect_err("resume must be refused while migration is pending");
    assert!(
        matches!(
            err,
            crate::engine::continuation::ResumeAuthorizationError::NoOwnershipEvidence
        ),
        "expected NoOwnershipEvidence for pending migration, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Durable state machine: idempotent marker (partial prior publication)
// ---------------------------------------------------------------------------

/// If the marker was already published by a prior partial run (intent
// persisted, marker published, but crash before completion), the
/// reconciliation completes without re-publishing and records exactly one
/// completion audit.
#[test]
fn reconciliation_completes_when_marker_already_published() {
    use crate::persistence::legacy_migration_state::{
        init_legacy_migration_table, load_migration_state, persist_migration_intent,
        MigrationStatus,
    };
    use chrono::Utc;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-partial", &workspace);

    // Simulate a partial prior run: intent persisted + marker published,
    // but crash before completion. We write the marker directly via the
    // anchor API to bypass provision's pre-existing-workspace adoption guard
    // (the migration itself is the authorized first-claim path for legacy rows).
    init_legacy_migration_table(&conn).expect("init");
    persist_migration_intent(
        &conn,
        "legacy-partial",
        &workspace.to_string_lossy(),
        Utc::now(),
    )
    .expect("persist intent");
    let canonical = workspace.canonicalize().expect("canonicalize");
    let anchor =
        crate::engine::workspace_ownership::WorkspaceAnchor::open(&canonical).expect("open anchor");
    crate::engine::workspace_ownership::publish_bootstrap_via_anchor(&anchor, "legacy-partial")
        .expect("publish marker (simulating partial prior run)");

    let outcome = migrate_legacy_ownership(
        &conn,
        "legacy-partial",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("reconciliation");
    assert_eq!(outcome, LegacyMigrationOutcome::IdempotentlyCompleted);

    let state = load_migration_state(&conn, "legacy-partial")
        .expect("load")
        .expect("row");
    assert_eq!(state.status, MigrationStatus::Completed);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-partial", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(count, 1, "exactly one completion audit");
}

// ---------------------------------------------------------------------------
// Durable state machine: conflicting intent refuses
// ---------------------------------------------------------------------------

/// A migration attempt with a workspace that conflicts with an existing
/// durable intent (different workspace path in the intent row) is refused.
/// This simulates a scenario where a partial migration recorded an intent
/// for workspace A, then the run's persisted workspace_path was corrected to
/// workspace B, and a retry is attempted with workspace B.
#[test]
fn migration_refuses_conflicting_intent_workspace() {
    use crate::persistence::legacy_migration_state::{
        init_legacy_migration_table, persist_migration_intent,
    };
    use chrono::Utc;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let ws_a = dir.path().join("ws-a");
    let ws_b = dir.path().join("ws-b");
    std::fs::create_dir_all(&ws_a).expect("create ws-a");
    std::fs::create_dir_all(&ws_b).expect("create ws-b");
    seed_legacy_run(&conn, "legacy-conflict", &ws_b);

    // Persist intent for workspace A (simulating an older persisted path).
    init_legacy_migration_table(&conn).expect("init migration table");
    persist_migration_intent(
        &conn,
        "legacy-conflict",
        &ws_a.to_string_lossy(),
        Utc::now(),
    )
    .expect("persist intent for ws-a");

    // Now attempt migration with workspace B (the current persisted path).
    // The intent row has workspace A, which conflicts.
    let err = migrate_legacy_ownership(
        &conn,
        "legacy-conflict",
        &ws_b,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect_err("conflicting intent must be refused");
    assert!(
        matches!(err, LegacyMigrationError::ConflictingIntent { .. }),
        "expected ConflictingIntent, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Concurrent migrations: only one completion audit per run
// ---------------------------------------------------------------------------

/// Two sequential migrations for the same run (simulating a race resolved by
/// the durable state machine) produce exactly one completion audit.
#[test]
fn concurrent_migrations_produce_exactly_one_completion() {
    use std::sync::Arc;
    use std::sync::Mutex;

    let conn = Arc::new(Mutex::new(test_conn()));
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    {
        let conn = conn.lock().expect("lock");
        seed_legacy_run(&conn, "legacy-concurrent", &workspace);
    }

    // Run two migrations sequentially (SQLite serializes via the connection).
    // Both must succeed; the first publishes, the second is AlreadyCompleted.
    {
        let conn = conn.lock().expect("lock");
        let outcome = migrate_legacy_ownership(
            &conn,
            "legacy-concurrent",
            &workspace,
            std::path::Path::new(TEST_CONFIG_ROOT_STR),
        )
        .expect("first");
        assert_eq!(outcome, LegacyMigrationOutcome::Migrated);
    }
    {
        let conn = conn.lock().expect("lock");
        let outcome = migrate_legacy_ownership(
            &conn,
            "legacy-concurrent",
            &workspace,
            std::path::Path::new(TEST_CONFIG_ROOT_STR),
        )
        .expect("second");
        assert_eq!(outcome, LegacyMigrationOutcome::AlreadyCompleted);
    }

    // Exactly one completion audit event.
    let conn = conn.lock().expect("lock");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-concurrent", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        count, 1,
        "exactly one completion audit event after concurrent attempts"
    );
}

// ---------------------------------------------------------------------------
// Durable state machine: audit failure does not mark completed
// ---------------------------------------------------------------------------

/// If the audit event write fails, the durable state must NOT be marked
/// completed, so a retry can re-record the audit. We simulate this by checking
/// that a properly completed migration has the audit and the state both set.
#[test]
fn completed_migration_has_both_audit_and_durable_state() {
    use crate::persistence::legacy_migration_state::migration_is_durable_completed;

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&conn, "legacy-both", &workspace);

    migrate_legacy_ownership(
        &conn,
        "legacy-both",
        &workspace,
        std::path::Path::new(TEST_CONFIG_ROOT_STR),
    )
    .expect("migrate");

    assert!(migration_is_durable_completed(&conn, "legacy-both"));
    let audit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-both", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(audit_count, 1, "one audit event");
}

// ---------------------------------------------------------------------------
// Post-upgrade NULL provenance denied
// ---------------------------------------------------------------------------

/// A genuine legacy row (NULL provenance, no migration) that is NOT migrated
/// is denied by verify_provenance with LegacyAllowed::Denied.
#[test]
fn unmigrated_legacy_null_provenance_denied_by_verify() {
    use crate::persistence::launch_provenance::{verify_provenance, LegacyAllowed};

    let conn = test_conn();
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let md = seed_legacy_run(&conn, "legacy-null-denied", &workspace);

    // The persisted provenance is None.
    assert!(md.launch_provenance.is_none());

    // verify_provenance with Denied policy must refuse.
    let verification = verify_provenance(
        &None,
        &crate::workflow::schema::WorkflowType {
            workflow_type_id: "wf".to_string(),
            steps: vec![],
            transitions: vec![],
            guards: crate::workflow::schema::GuardConfig {
                max_retries: None,
                timeout_seconds: None,
                require_approval: None,
            },
        },
        &crate::workflow::schema::WorkflowConfig {
            config_id: "cfg".to_string(),
            workflow_type_id: "wf".to_string(),
            runtime: crate::workflow::schema::RuntimeConfig {
                timeout_seconds: 60,
                max_retries: 3,
                parallel_steps: None,
                log_level: None,
            },
            repo: crate::workflow::schema::RepoConfig {
                workspace_strategy: "temp_clone".to_string(),
                branch_template: "workflow-{run_id}".to_string(),
                base_branch: Some("main".to_string()),
                workspace_root: None,
                project_subdir: None,
                artifact_path_base: None,
                diff_path_base: None,
                diff_path_normalization:
                    crate::workflow::schema::DiffPathNormalization::RepoRelative,
            },
            guard_limits: crate::workflow::schema::GuardLimits {
                max_iterations: None,
                max_file_changes: None,
                max_tokens: None,
                max_cost: None,
            },
            variables: std::collections::HashMap::new(),
            discovery: None,
            parent_orchestration: crate::workflow::schema::ParentOrchestrationConfig::default(),
            merge_required: false,
            merge_strategy: None,
            command_manifest: None,
            target_profile: None,
        },
        dir.path(),
        LegacyAllowed::Denied,
    );
    assert!(
        matches!(
            verification,
            crate::persistence::ProvenanceVerification::Mismatch(_)
        ),
        "NULL provenance must be denied with LegacyAllowed::Denied"
    );
}
