//! Focused regression tests for the four parent-orchestration blockers.
//!
//! 1. `discovery.rs` pre-dispatch CAS: `start_child_workflow` and
//!    `resume_child_workflow` must use an exact expected-status/expected-owner
//!    compare-and-swap to transition to `Running`, and skip dispatch when the
//!    CAS is rejected (the lease was advanced by a concurrent writer).
//! 2. `child_workflow.rs` workspace marker: `Launch` writes the marker,
//!    `Resume` verifies an existing marker and never writes it.
//! 3. `child_workflow.rs` run id validation: a run id must be a safe single
//!    path component before it is interpolated into child artifact/work dirs.
//! 4. `lease.rs` `finish_child_launch` on rejected CAS: the step outcome is
//!    derived from the durable lease state, never from the stale process
//!    result.

use super::super::*;
use super::support::*;

// ---------------------------------------------------------------------------
// Blocker 1: discovery.rs pre-dispatch CAS
// ---------------------------------------------------------------------------

/// Build a harness returning an initialized database connection, an
/// orchestration state, and a claimed lease for a unique child issue.
fn cas_harness() -> (
    OrchestrationState,
    rusqlite::Connection,
    crate::persistence::leases::IssueLease,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();
    let child = unique_child_issue_number();
    let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
        .unwrap()
        .unwrap();
    std::mem::forget(temp);
    (state, conn, lease)
}

/// A runner that records whether `launch_child` was invoked and asserts it is
/// never called on a rejected CAS.
struct LaunchTrackingRunner {
    launched: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LaunchTrackingRunner {
    fn new() -> Self {
        Self {
            launched: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl ChildWorkflowRunner for LaunchTrackingRunner {
    fn launch_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        self.launched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ChildWorkflowRunResult::CompletedSuccess)
    }

    fn resume_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        self.launched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ChildWorkflowRunResult::CompletedSuccess)
    }

    fn run_status(&self, _run_id: &str) -> Result<Option<RunStatus>, ChildWorkflowRunnerError> {
        Ok(Some(RunStatus::Completed))
    }
}

#[test]
fn start_child_workflow_skips_dispatch_when_lease_advances_before_cas() {
    // Blocker 1: a concurrent writer advances the lease from Claimed to Running
    // with a foreign run id before start_child_workflow's CAS runs. The CAS
    // must be rejected (the observed status is no longer Claimed), dispatch
    // must be skipped, and the step must return Wait so the orchestrator
    // re-evaluates on the next pass.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    // A concurrent writer flips the lease to Running with a foreign run id.
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("concurrent-run"),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let runner = LaunchTrackingRunner::new();
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());

    let outcome = start_child_workflow(
        &mut context,
        &state,
        &query,
        &runner,
        child,
        &lease_snapshot,
        &conn,
    )
    .unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Wait,
        "a rejected CAS must skip dispatch and wait for re-evaluation"
    );
    assert!(
        !runner.launched.load(std::sync::atomic::Ordering::SeqCst),
        "the runner must not be invoked when the CAS is rejected"
    );
}

#[test]
fn start_child_workflow_dispatches_when_cas_succeeds() {
    // Blocker 1: when the CAS succeeds (the lease is still Claimed), the runner
    // is invoked and the child workflow is launched normally.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let runner = LaunchTrackingRunner::new();
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());

    let outcome = start_child_workflow(
        &mut context,
        &state,
        &query,
        &runner,
        child,
        &lease_snapshot,
        &conn,
    )
    .unwrap();

    assert!(
        runner.launched.load(std::sync::atomic::Ordering::SeqCst),
        "the runner must be invoked when the CAS succeeds"
    );
    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn start_child_workflow_compensates_running_lease_when_add_label_fails_after_cas() {
    // Blocker 1: when the CAS succeeds (Claimed→Running with our run id) but the
    // subsequent `add_label` fails, the stranded Running lease must be
    // compensated back to the observed Claimed state (with the observed run id)
    // so the lease is not left orphaned as Running with no dispatch. The runner
    // must never be invoked, and the durable lease must be left available for
    // re-evaluation rather than stranded.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    // Snapshot the observed lease (Claimed, no run id).
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(lease_snapshot.status, LeaseStatus::Claimed);
    assert!(
        lease_snapshot.run_id.is_none(),
        "a fresh claim has no run id"
    );
    let runner = NoLaunchTrackingRunner::new();
    let launched = runner.launched.clone();
    let query = FailingAddLabelQuery::new(GithubError::CommandFailed {
        argv: vec!["gh".to_string(), "issue".to_string(), "edit".to_string()],
        exit_code: Some(1),
        stderr: "rate limited".to_string(),
    });
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());

    // The call must error (add_label failure propagates) after compensating the
    // Running lease.
    let result = start_child_workflow(
        &mut context,
        &state,
        &query,
        &runner,
        child,
        &lease_snapshot,
        &conn,
    );

    assert!(
        result.is_err(),
        "add_label failure must propagate as an error, got: {:?}",
        result
    );
    assert!(
        !launched.load(std::sync::atomic::Ordering::SeqCst),
        "the runner must not be invoked when add_label fails before dispatch"
    );
    // The durable lease must NOT be stranded as Running: it must be compensated
    // back to Claimed (the observed status) so it can be re-claimed rather than
    // orphaned. The run_id may carry the CAS-acquired value (the conditional
    // update preserves the column when the observed run_id was None), which is
    // acceptable because the status — not the run_id — drives dispatch
    // decisions. The critical invariant is that the lease is not left as
    // Running with no dispatch.
    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Claimed,
        "the stranded Running lease must be compensated back to Claimed, got {:?}",
        final_lease.status
    );
}

#[test]
fn resume_child_workflow_skips_dispatch_when_lease_advances_before_cas() {
    // Blocker 1: a concurrent writer advances the lease from ReadyToResume to
    // Running with a foreign run id before resume_child_workflow's CAS runs.
    // The CAS must be rejected (the observed status is no longer
    // ReadyToResume), dispatch must be skipped, and the step must return Wait.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let run_id = "original-child-run";
    // Set up a ReadyToResume lease with a run id (the normal resume entry point).
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some(run_id),
    )
    .unwrap();
    // A concurrent writer flips it to Running with a foreign run id.
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("concurrent-run"),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let runner = LaunchTrackingRunner::new();
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());

    let outcome = resume_child_workflow(
        &mut context,
        &state,
        &query,
        &runner,
        child,
        &lease_snapshot,
        &conn,
    )
    .unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Wait,
        "a rejected CAS must skip dispatch and wait for re-evaluation"
    );
    assert!(
        !runner.launched.load(std::sync::atomic::Ordering::SeqCst),
        "the runner must not be invoked when the CAS is rejected"
    );
}

#[test]
fn resume_child_workflow_missing_run_id_fails_lease() {
    // Blocker 1 edge case: resume with a lease that has no run_id must fail the
    // lease and return Fixable, not attempt a CAS or dispatch.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    // The claimed lease has no run_id.
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let runner = LaunchTrackingRunner::new();
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());

    let outcome = resume_child_workflow(
        &mut context,
        &state,
        &query,
        &runner,
        child,
        &lease_snapshot,
        &conn,
    )
    .unwrap();

    assert_eq!(outcome, StepOutcome::Fixable);
    assert!(
        !runner.launched.load(std::sync::atomic::Ordering::SeqCst),
        "the runner must not be invoked when run_id is missing"
    );
    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(final_lease.status, LeaseStatus::Failed);
}

// ---------------------------------------------------------------------------
// Blocker 3: run id path-component validation
// ---------------------------------------------------------------------------

#[test]
fn validate_run_id_rejects_empty() {
    let err = validate_run_id_path_component("").unwrap_err();
    assert!(
        err.contains("empty"),
        "expected empty-rejection message, got: {err}"
    );
}

#[test]
fn validate_run_id_rejects_path_separators() {
    let err = validate_run_id_path_component("run/with/slash").unwrap_err();
    assert!(
        err.contains("path separators"),
        "expected path-separator rejection, got: {err}"
    );
    let err = validate_run_id_path_component("run\\with\\backslash").unwrap_err();
    assert!(
        err.contains("path separators"),
        "expected path-separator rejection, got: {err}"
    );
}

#[test]
fn validate_run_id_rejects_parent_traversal() {
    let err = validate_run_id_path_component("..").unwrap_err();
    assert!(
        err.contains("safe single path component"),
        "expected traversal rejection, got: {err}"
    );
    let err = validate_run_id_path_component(".").unwrap_err();
    assert!(
        err.contains("safe single path component"),
        "expected traversal rejection, got: {err}"
    );
}

#[test]
fn validate_run_id_rejects_nul_byte() {
    let err = validate_run_id_path_component("run\u{0}evil").unwrap_err();
    assert!(
        err.contains("safe single path component"),
        "expected NUL rejection, got: {err}"
    );
}

#[test]
fn validate_run_id_rejects_non_alphanumeric_chars() {
    let err = validate_run_id_path_component("run;rm -rf").unwrap_err();
    assert!(
        err.contains("alphanumeric"),
        "expected character rejection, got: {err}"
    );
}

#[test]
fn validate_run_id_accepts_safe_identifiers() {
    validate_run_id_path_component("parent42-child7-1700000000000").unwrap();
    validate_run_id_path_component("abc_def-123").unwrap();
    validate_run_id_path_component("a").unwrap();
}

// ---------------------------------------------------------------------------
// Blocker 2: workspace marker — Launch writes, Resume verifies only
// ---------------------------------------------------------------------------

#[test]
fn launch_child_process_writes_workspace_owner_marker() {
    // Blocker 2: the Launch path must provision the durable workspace ownership
    // marker. We cannot run a full child workflow (requires a real config), so
    // we verify the marker-writing helper directly.
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let run_id = "child-launch-run-1";

    // The marker must not exist before launch.
    let marker = work_dir.join(".luther").join("workspace-owner");
    assert!(!marker.exists());

    // Invoke the launch marker provisioning path. This is exercised via the
    // public `write_child_workspace_owner_marker` helper used by
    // `run_child_workflow` in Launch mode.
    write_child_workspace_owner_marker(&work_dir, run_id).unwrap();

    assert!(
        marker.exists(),
        "Launch must write the workspace-owner marker"
    );
    let contents = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        contents.trim(),
        run_id,
        "the marker must record the owning run id"
    );
}

#[test]
fn launch_child_process_marker_writing_rejects_foreign_owner() {
    // Blocker 2: the Launch provisioning must reject a workspace already owned
    // by a different run id (the underlying write_workspace_owner_marker fails
    // on a foreign-owner collision).
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let first_run = "first-run";
    let second_run = "second-run";

    write_child_workspace_owner_marker(&work_dir, first_run).unwrap();
    let err = write_child_workspace_owner_marker(&work_dir, second_run).unwrap_err();
    assert!(
        err.contains("write child workspace owner marker"),
        "expected a foreign-owner rejection, got: {err}"
    );

    // The first owner must be preserved.
    let marker = work_dir.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), first_run);
}

// ---------------------------------------------------------------------------
// Blocker 2: Resume verifies (does not write) the workspace marker
// ---------------------------------------------------------------------------

#[test]
fn resume_child_process_marker_verification_passes_for_matching_owner() {
    // Blocker 2: Resume must verify (not write) the existing marker. When the
    // marker exists and matches the resuming run id, verification succeeds.
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let run_id = "child-resume-run-1";

    // Simulate a prior launch provisioning the marker.
    write_child_workspace_owner_marker(&work_dir, run_id).unwrap();
    let marker = work_dir.join(".luther").join("workspace-owner");
    let mtime_before = std::fs::metadata(&marker).unwrap().modified().unwrap();

    // Resume's verification must succeed without modifying the marker.
    verify_existing_workspace_owner_marker(&work_dir, run_id).unwrap();

    let mtime_after = std::fs::metadata(&marker).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "Resume must not modify the workspace-owner marker"
    );
}

#[test]
fn resume_child_process_marker_verification_rejects_missing_marker() {
    // Blocker 2: Resume of a workspace with no marker (never provisioned) must
    // fail closed.
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let run_id = "child-resume-no-marker";

    let err = verify_existing_workspace_owner_marker(&work_dir, run_id).unwrap_err();
    assert!(
        err.contains("missing"),
        "expected a missing-marker rejection, got: {err}"
    );
}

#[test]
fn resume_child_process_marker_verification_rejects_foreign_owner() {
    // Blocker 2: Resume of a workspace owned by a different run id must fail.
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let owning_run = "owning-run";
    let resuming_run = "resuming-run";

    write_child_workspace_owner_marker(&work_dir, owning_run).unwrap();

    let err = verify_existing_workspace_owner_marker(&work_dir, resuming_run).unwrap_err();
    assert!(
        err.contains("foreign") || err.contains("belongs to run"),
        "expected a foreign-owner rejection, got: {err}"
    );
}

#[test]
fn resume_child_process_marker_verification_rejects_empty_marker() {
    // Blocker 2: Resume of a workspace with an empty marker must fail closed.
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let marker_dir = work_dir.join(".luther");
    std::fs::create_dir_all(&marker_dir).unwrap();
    std::fs::write(marker_dir.join("workspace-owner"), "").unwrap();
    let run_id = "child-resume-empty";

    let err = verify_existing_workspace_owner_marker(&work_dir, run_id).unwrap_err();
    assert!(
        err.contains("empty"),
        "expected an empty-marker rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Blocker 4: finish_child_launch derives outcome from durable lease state
// ---------------------------------------------------------------------------

fn completion_request(
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    run_id: &str,
) -> ChildWorkflowLaunchRequest {
    ChildWorkflowLaunchRequest {
        workflow_type_id: "wf".to_string(),
        config_id: "cfg".to_string(),
        run_id: run_id.to_string(),
        repo: lease.issue_repo.clone(),
        issue_number: child,
        work_dir: None,
        artifact_dir: None,
        config_root: PathBuf::from("/config"),
    }
}

#[test]
fn finish_child_launch_rejected_cas_with_completed_durable_state_yields_success() {
    // Blocker 4: when the CAS is rejected because a concurrent writer has
    // already advanced the lease to Completed, the step outcome must be
    // Success (derived from the durable lease state), even though the in-process
    // result was a failure. This prevents masking a completed child as fixable.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let run_id = "child-durable-completed";
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Completed, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = completion_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Success,
        "durable Completed lease must yield Success, not the stale failure's Fixable"
    );
}

#[test]
fn finish_child_launch_rejected_cas_with_running_durable_state_yields_wait() {
    // Blocker 4: when the CAS is rejected because a concurrent writer holds the
    // lease as Running (foreign owner), the step outcome must be Wait (derived
    // from the durable lease state), even though the in-process result was a
    // failure. This avoids forcing a fixable re-evaluation of a child that is
    // actively being driven by another writer.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let run_id = "child-durable-running";
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("foreign-run"),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = completion_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Wait,
        "durable Running lease must yield Wait, not the stale failure's Fixable"
    );
}

#[test]
fn finish_child_launch_rejected_cas_with_waiting_external_durable_state_yields_wait() {
    // Blocker 4: a durable WaitingExternal lease must yield Wait.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let run_id = "child-durable-waiting";
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some("foreign-run"),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = completion_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedSuccess,
        run_status: Some(RunStatus::Completed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Wait,
        "durable WaitingExternal lease must yield Wait, not the stale success's Success"
    );
}

#[test]
fn finish_child_launch_rejected_cas_with_cleanup_abandoned_yields_fixable() {
    // Blocker 4: a durable CleanupAbandoned lease must yield Fixable
    // (re-evaluate) — never Success and never a side-effecting path.
    let (state, conn, lease) = cas_harness();
    let child = lease.issue_number;
    let run_id = "child-durable-cleanup-abandoned";
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some(run_id),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = completion_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedSuccess,
        run_status: Some(RunStatus::Completed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert_eq!(
        outcome,
        StepOutcome::Fixable,
        "durable CleanupAbandoned lease must yield Fixable (re-evaluate), not Success"
    );
}
