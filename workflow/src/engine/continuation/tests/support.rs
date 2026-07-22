//! Shared test helpers for the continuation test suite.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use crate::engine::continuation::{
    checkpoint_identity, write_workspace_owner_marker, ContinuationKind, ContinuationRequest,
    RewindTarget, TERMINAL_STEP,
};
use crate::persistence::checkpoint::init_checkpoint_table;
use crate::persistence::run_metadata::init_runs_table;
use crate::persistence::{
    get_checkpoint_for_step, get_run_with_conn, persist_run_with_conn, save_checkpoint_with_conn,
    Checkpoint, RunMetadata, RunStatus, StateSnapshot, CHECKPOINT_STATUS_WAITING,
};
use chrono::Utc;

pub(super) fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    init_checkpoint_table(&conn).expect("checkpoint table");
    init_runs_table(&conn).expect("runs table");
    crate::persistence::leases::init_leases_table(&conn).expect("leases table");
    conn
}

pub(super) fn seed_run(conn: &Connection, run_id: &str, status: RunStatus, current_step: &str) {
    let mut md = RunMetadata::new(run_id, "llxprt-issue-fix", "llxprt-issue-fix-v1");
    md.status = status.clone();
    md.current_step = Some(current_step.to_string());
    md.repository = Some("vybestack/llxprt-code".to_string());
    md.issue_number = Some(2133);
    md.pr_number = Some(2138);
    let workspace = std::env::temp_dir().join(format!("luther-continuation-{run_id}"));
    std::fs::create_dir_all(&workspace).expect("create continuation test workspace");
    md.workspace_path = Some(workspace.to_string_lossy().into_owned());
    persist_run_with_conn(conn, &md).expect("persist run");
    // Seed the issue lease that backs this run's claim. `acquire_continuation_lease`
    // rejects missing leases when repository+issue identity is present, so every
    // test run that expects a successful commit must have a backing lease owned
    // by the same run_id.
    seed_lease_for_run(conn, run_id, status);
}

/// Create an issue lease owned by `run_id`, mapping the run's terminal status
/// to an appropriate starting lease status so `acquire_continuation_lease`'s
/// expected-status set matches.
pub(super) fn seed_lease_for_run(conn: &Connection, run_id: &str, status: RunStatus) {
    use crate::persistence::{IssueLease, LeaseStatus};
    let lease_status = match status {
        RunStatus::Failed | RunStatus::Abandoned => LeaseStatus::Failed,
        RunStatus::WaitingExternal => LeaseStatus::WaitingExternal,
        RunStatus::ReadyToResume | RunStatus::Paused => LeaseStatus::ReadyToResume,
        _ => LeaseStatus::Running,
    };
    let now = Utc::now();
    let lease = IssueLease {
        lease_id: format!("lease-{run_id}"),
        issue_repo: "vybestack/llxprt-code".to_string(),
        issue_number: 2133,
        config_id: "llxprt-issue-fix-v1".to_string(),
        run_id: Some(run_id.to_string()),
        status: lease_status,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    };
    crate::persistence::leases::create_lease(conn, &lease).expect("create lease");
}

pub(super) fn seed_checkpoint(conn: &Connection, run_id: &str, step: &str, status: &str) {
    let snapshot = StateSnapshot {
        retry_count: 0,
        loop_count: 0,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: status.to_string(),
    };
    let cp = Checkpoint::with_snapshot(run_id, step, snapshot);
    save_checkpoint_with_conn(conn, &cp).expect("save checkpoint");
    // Ensure distinct, increasing timestamps for ordering assertions.
    std::thread::sleep(std::time::Duration::from_millis(2));
}

pub(super) fn seed_terminal_failed_run(conn: &Connection, run_id: &str) {
    seed_run(conn, run_id, RunStatus::Failed, TERMINAL_STEP);
    seed_checkpoint(conn, run_id, "capture_pr_identity", "completed");
    seed_checkpoint(conn, run_id, "post_pr_iteration_guard", "completed");
    seed_checkpoint(conn, run_id, "watch_pr_checks", "completed");
    seed_checkpoint(conn, run_id, "collect_ci_failures", "completed");
    seed_checkpoint(conn, run_id, TERMINAL_STEP, "completed");
}

pub(super) fn request(run_id: &str, kind: ContinuationKind, force: bool) -> ContinuationRequest {
    ContinuationRequest {
        run_id: run_id.to_string(),
        kind,
        force,
        trusted_internal: false,
    }
}

pub(super) fn seed_cleanup_abandonment(
    conn: &Connection,
    run_id: &str,
    workspace: &Path,
) -> String {
    seed_run(conn, run_id, RunStatus::Abandoned, "abandon_and_log");
    seed_checkpoint(conn, run_id, "remediate", "completed");
    let checkpoint = get_checkpoint_for_step(conn, run_id, "remediate")
        .expect("checkpoint query")
        .expect("failed checkpoint");
    seed_checkpoint(conn, run_id, "abandon_and_log", "completed");
    let mut metadata = get_run_with_conn(conn, run_id)
        .expect("run query")
        .expect("run");
    metadata.workspace_path = Some(workspace.to_string_lossy().to_string());
    metadata.failure_cleanup = Some(crate::persistence::FailureCleanupState {
        schema_version: crate::persistence::FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "agent timed out".to_string(),
        failed_checkpoint_id: checkpoint_identity(&checkpoint),
        failed_state_snapshot: checkpoint.state_snapshot.clone(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: true,
        captured_at: Utc::now(),
        cleanup_completed_at: Some(Utc::now()),
        recovery_consumed_at: None,
        ownership_denied: false,
    });
    persist_run_with_conn(conn, &metadata).expect("persist cleanup provenance");
    // Write the durable `.luther/workspace-owner` marker so the workspace is
    // trusted for cleanup-failure-abandonment recovery.
    write_workspace_owner_marker(workspace, run_id).expect("write owner marker");
    checkpoint_identity(&checkpoint)
}

/// Seed a run whose failure-cleanup state marks it as an ownership-denied
/// terminal: `ownership_denied = true` and `cleanup_succeeded = false`. This
/// is the distinct non-resumable state that must never be selected for
/// cleanup continuation, because cleanup executes shell commands that must
/// only run in a trusted workspace.
pub(super) fn seed_ownership_denied_terminal(conn: &Connection, run_id: &str) {
    seed_run(conn, run_id, RunStatus::Failed, "abandon_and_log");
    seed_checkpoint(conn, run_id, "remediate", "completed");
    let checkpoint = get_checkpoint_for_step(conn, run_id, "remediate")
        .expect("checkpoint query")
        .expect("failed checkpoint");
    seed_checkpoint(conn, run_id, "abandon_and_log", "completed");
    let mut metadata = get_run_with_conn(conn, run_id)
        .expect("run query")
        .expect("run");
    metadata.failure_cleanup = Some(crate::persistence::FailureCleanupState {
        schema_version: crate::persistence::FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "workspace ownership denied".to_string(),
        failed_checkpoint_id: checkpoint_identity(&checkpoint),
        failed_state_snapshot: checkpoint.state_snapshot.clone(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: false,
        captured_at: Utc::now(),
        cleanup_completed_at: None,
        recovery_consumed_at: None,
        ownership_denied: true,
    });
    persist_run_with_conn(conn, &metadata).expect("persist ownership-denied terminal");
}

/// Continuation kinds that should be rejected uniformly when a run is in a
/// non-resumable terminal state, regardless of `--force`.
pub(super) fn resumable_kinds() -> Vec<ContinuationKind> {
    vec![
        ContinuationKind::Resume,
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        ContinuationKind::Retry {
            from_failed_step: true,
        },
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("watch_pr_checks".to_string()),
        },
    ]
}

/// Seed a run in `status` with a whitelisted, resumable `watch_pr_checks`
/// checkpoint, then assert every continuation kind is rejected with a
/// `resumable_status` failure, even with `force = true`.
pub(super) fn assert_non_resumable_rejected(status: RunStatus) {
    let conn = test_conn();
    seed_run(&conn, "term", status.clone(), "watch_pr_checks");
    seed_checkpoint(&conn, "term", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
    for kind in resumable_kinds() {
        for force in [false, true] {
            let req = request("term", kind.clone(), force);
            let validation =
                crate::engine::continuation::validate_continuation(&conn, &req).expect("validate");
            assert!(
                !validation.ok,
                "status {status:?} kind {kind:?} force={force} must be rejected"
            );
            assert!(
                validation
                    .failure_reasons()
                    .iter()
                    .any(|r| r.contains("resumable_status")),
                "expected resumable_status failure for {status:?} (got {:?})",
                validation.failure_reasons()
            );
        }
    }
}

#[cfg(unix)]
pub(super) fn short_lived_process() -> std::io::Result<std::process::Child> {
    std::process::Command::new("true").spawn()
}
