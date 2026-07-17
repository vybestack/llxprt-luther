use super::*;

use crate::persistence::leases::{
    update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome,
};

pub fn bool_context(context: &StepContext, primary: &str, fallback: &str) -> bool {
    bool_context_default(context, primary, fallback, false)
}

pub fn bool_context_default(
    context: &StepContext,
    primary: &str,
    fallback: &str,
    default: bool,
) -> bool {
    context
        .get(primary)
        .or_else(|| context.get(fallback))
        .map_or(default, |value| value == "true")
}

pub fn daemon_connection() -> Result<rusqlite::Connection, EngineError> {
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    ensure_daemon_database_initialized(&db_path)?;
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|err| parent_error(format!("open daemon database: {err}")))?;
    configure_parent_orchestration_connection(&conn)?;
    Ok(conn)
}

#[derive(Default)]
struct DaemonDatabaseInitState {
    initialized: std::collections::BTreeSet<PathBuf>,
    in_flight: std::collections::BTreeMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>,
}

fn ensure_daemon_database_initialized(db_path: &Path) -> Result<(), EngineError> {
    static INITIALIZED_DATABASES: std::sync::OnceLock<std::sync::Mutex<DaemonDatabaseInitState>> =
        std::sync::OnceLock::new();
    let init_state = INITIALIZED_DATABASES.get_or_init(Default::default);
    let path_lock = {
        let mut state = lock_daemon_database_init_state(init_state);
        if state.initialized.contains(db_path) {
            return Ok(());
        }
        state
            .in_flight
            .entry(db_path.to_path_buf())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    };
    let _path_guard = path_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let state = lock_daemon_database_init_state(init_state);
        if state.initialized.contains(db_path) {
            return Ok(());
        }
    }
    let result = crate::persistence::init_database(db_path)
        .map_err(|err| parent_error(format!("initialize daemon database: {err}")));
    let mut state = lock_daemon_database_init_state(init_state);
    state.in_flight.remove(db_path);
    match result {
        Ok(()) => {
            state.initialized.insert(db_path.to_path_buf());
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn lock_daemon_database_init_state(
    init_state: &std::sync::Mutex<DaemonDatabaseInitState>,
) -> std::sync::MutexGuard<'_, DaemonDatabaseInitState> {
    init_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn configure_parent_orchestration_connection(
    conn: &rusqlite::Connection,
) -> Result<(), EngineError> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| parent_error(format!("set daemon database busy timeout: {err}")))?;
    Ok(())
}

pub fn open_parent_orchestration_connection(path: &Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|err| format!("open parent orchestration database: {err}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| format!("set parent orchestration database busy timeout: {err}"))?;
    Ok(conn)
}

pub fn child_run_status_from_registry(run_id: &str) -> Result<Option<RunStatus>, EngineError> {
    let conn = daemon_connection()?;
    get_run_with_conn(&conn, run_id)
        .map(|metadata| metadata.map(|run| run.status))
        .map_err(sql_error)
}

pub enum ChildLeaseAction {
    Launch(crate::persistence::leases::IssueLease),
    Resume(crate::persistence::leases::IssueLease),
    Wait {
        lease: Option<crate::persistence::leases::IssueLease>,
        reason: String,
    },
}

pub fn prepare_child_lease(
    state: &OrchestrationState,
    child: u64,
    conn: &mut rusqlite::Connection,
) -> Result<ChildLeaseAction, EngineError> {
    prepare_child_lease_with_conn(state, child, conn)
}

pub fn prepare_child_lease_with_conn(
    state: &OrchestrationState,
    child: u64,
    conn: &mut rusqlite::Connection,
) -> Result<ChildLeaseAction, EngineError> {
    if let Some(lease) = get_lease_for_issue(conn, &state.repo, child).map_err(sql_error)? {
        return Ok(match lease.status {
            LeaseStatus::ReadyToResume => {
                if child_workflow_completed(conn, &lease)? {
                    ChildLeaseAction::Wait {
                        lease: Some(lease),
                        reason: "child_workflow_completed_waiting_for_pr_merge".to_string(),
                    }
                } else {
                    ChildLeaseAction::Resume(lease)
                }
            }
            LeaseStatus::Failed | LeaseStatus::Abandoned | LeaseStatus::Stale => {
                prepare_relaunchable_child(conn, &lease)?
            }
            LeaseStatus::WaitingExternal | LeaseStatus::Claimed | LeaseStatus::Running => {
                ChildLeaseAction::Wait {
                    lease: Some(lease),
                    reason: "active_child_lease".to_string(),
                }
            }
            LeaseStatus::CleanupAbandoned => ChildLeaseAction::Wait {
                lease: Some(lease),
                reason: "cleanup_abandoned_requires_continuation".to_string(),
            },
            LeaseStatus::Pending | LeaseStatus::Completed => ChildLeaseAction::Wait {
                lease: Some(lease),
                reason: "non_actionable_child_lease".to_string(),
            },
        });
    }
    claim_child_lease(state, child, conn)
}

pub fn claim_child_lease(
    state: &OrchestrationState,
    child: u64,
    conn: &rusqlite::Connection,
) -> Result<ChildLeaseAction, EngineError> {
    let Some(lease) =
        try_claim(conn, &state.repo, child, &state.child_config_id).map_err(sql_error)?
    else {
        return Ok(ChildLeaseAction::Wait {
            lease: None,
            reason: "child_lease_claim_contended".to_string(),
        });
    };
    Ok(ChildLeaseAction::Launch(lease))
}

pub fn prepare_relaunchable_child(
    conn: &mut rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<ChildLeaseAction, EngineError> {
    let tx = conn.transaction().map_err(sql_error)?;
    if !clear_child_lease_for_relaunch(&tx, lease)? {
        // Another connection won the compare-and-swap and already flipped this
        // terminal lease into a fresh Claimed state, so we must not relaunch a
        // duplicate child workflow. Roll back and let the caller wait.
        tx.rollback().map_err(sql_error)?;
        return Ok(ChildLeaseAction::Wait {
            lease: None,
            reason: "child_lease_relaunch_contended".to_string(),
        });
    }
    let relaunchable = get_lease_for_issue(&tx, &lease.issue_repo, lease.issue_number)
        .map_err(sql_error)?
        .ok_or_else(|| {
            parent_error("child lease disappeared while preparing relaunch".to_string())
        })?;
    tx.commit().map_err(sql_error)?;
    Ok(ChildLeaseAction::Launch(relaunchable))
}

/// Atomically claim a terminal child lease for relaunch.
///
/// This is a compare-and-swap keyed on the *observed* terminal `status` and
/// `run_id`: it only flips the lease to `Claimed` (clearing `run_id`) when the
/// row still matches the terminal identity the caller read. If another
/// connection already claimed the same terminal lease, the row no longer
/// matches, zero rows are affected, and this returns `Ok(false)` so the caller
/// can wait instead of launching a duplicate child workflow.
pub fn clear_child_lease_for_relaunch(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<bool, EngineError> {
    let updated = conn
        .execute(
            "UPDATE issue_leases
             SET status = ?1, run_id = NULL, updated_at = ?2
             WHERE lease_id = ?3 AND status = ?4 AND run_id IS ?5",
            rusqlite::params![
                LeaseStatus::Claimed.to_string(),
                Utc::now().to_rfc3339(),
                lease.lease_id,
                lease.status.to_string(),
                lease.run_id,
            ],
        )
        .map_err(sql_error)?;
    Ok(updated == 1)
}

pub enum ChildPrWait {
    Merged,
    ReadyForHumanMerge,
    MissingPr,
    ClosedUnmerged,
    Superseded,
}

pub fn classify_child_pr_wait(pr: Option<&GithubIssuePrState>) -> ChildPrWait {
    let Some(pr) = pr else {
        return ChildPrWait::MissingPr;
    };
    if pr.merged {
        return ChildPrWait::Merged;
    }
    if pr.state.eq_ignore_ascii_case("superseded") {
        return ChildPrWait::Superseded;
    }
    if pr.state.eq_ignore_ascii_case("closed") {
        return ChildPrWait::ClosedUnmerged;
    }
    ChildPrWait::ReadyForHumanMerge
}

pub fn finish_merged_child(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let run_id = persisted_child_run_id(context, state, child)?;
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": false,
            "child_issue_number": child,
            "state": "merged",
            "child_run_id": run_id,
            "pr": pr
        }),
    )?;

    query
        .remove_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    if let Some(run_id) = run_id.as_deref() {
        mark_child_lease_completed(state, child, run_id)?;
    }
    update_rollup(state, child, run_id.as_deref(), "merged", pr)?;
    Ok(StepOutcome::Success)
}

pub fn record_ready_for_human_merge(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let run_id = child_run_id_for_wait(state, child)?;
    let auto_merge = attempt_auto_merge_if_enabled(state, query, pr);
    let already_recorded = rollup_has_outcome(state, child, "ready_for_human_merge")?;
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "ready_for_human_merge",
            "child_run_id": run_id,
            "pr": pr,
            "auto_merge_children": state.auto_merge_children,
            "auto_merge": auto_merge,
            "wait_for_human_merge": state.wait_for_human_merge,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )?;
    if !already_recorded {
        query
            .comment_issue(
                &state.repo,
                state.parent_issue_number,
                &format!(
                    "Child issue #{child} has a PR ready for human merge. Parent orchestration will continue after the PR is merged."
                ),
            )
            .map_err(github_error)?;
    }
    update_rollup(state, child, run_id.as_deref(), "ready_for_human_merge", pr)?;
    Ok(if state.wait_for_human_merge {
        StepOutcome::Wait
    } else {
        StepOutcome::Success
    })
}

pub fn persisted_child_run_id(
    context: &StepContext,
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    if let Some(run_id) = context.get("child_run_id") {
        return Ok(Some(run_id.clone()));
    }
    child_run_id_for_wait(state, child)
}

pub fn child_run_id_for_wait(
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    let conn = daemon_connection()?;
    Ok(get_lease_for_issue(&conn, &state.repo, child)
        .map_err(sql_error)?
        .and_then(|lease| lease.run_id))
}

pub fn child_workflow_ready_for_merge(run_id: &Option<String>) -> Result<bool, EngineError> {
    let Some(run_id) = run_id.as_deref() else {
        return Ok(false);
    };
    let conn = daemon_connection()?;
    let Some(metadata) = get_run_with_conn(&conn, run_id).map_err(sql_error)? else {
        return Ok(false);
    };
    Ok(matches!(
        metadata.status,
        RunStatus::Completed | RunStatus::Merged
    ))
}

pub fn record_child_pr_still_in_progress(
    state: &OrchestrationState,
    child: u64,
    pr: Option<&GithubIssuePrState>,
    run_id: Option<&str>,
) -> Result<StepOutcome, EngineError> {
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "child_workflow_in_progress",
            "child_run_id": run_id,
            "pr": pr,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )?;
    update_rollup(state, child, run_id, "child_workflow_in_progress", pr)?;
    Ok(StepOutcome::Wait)
}

pub fn reevaluate_closed_unmerged_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let Some(issue) = query.get_issue(&state.repo, child).map_err(github_error)? else {
        return record_blocked_child(state, query, child, pr, "closed_unmerged_pr");
    };
    if issue.state.eq_ignore_ascii_case("open") {
        mark_child_lease_relaunchable(state, child)?;
        update_rollup(state, child, None, "closed_unmerged_pr_relaunchable", pr)?;
        write_json(
            &state.artifact_root,
            "child-merge-wait.json",
            &json!({
                "waiting": false,
                "child_issue_number": child,
                "state": "closed_unmerged_relaunchable",
                "pr": pr
            }),
        )?;
        return Ok(StepOutcome::Success);
    }
    record_blocked_child(state, query, child, pr, "closed_unmerged_pr")
}

pub fn mark_child_lease_relaunchable(
    state: &OrchestrationState,
    child: u64,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        let Some(run_id) = lease.run_id.as_deref() else {
            return Ok(());
        };
        crate::persistence::update_lease_status_conditional(
            &conn,
            &lease.lease_id,
            LeaseStatus::Failed,
            &[LeaseStatus::Completed, LeaseStatus::Failed],
            Some(run_id),
            Some(run_id),
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

pub fn attempt_auto_merge_if_enabled(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    pr: Option<&GithubIssuePrState>,
) -> Value {
    if !state.auto_merge_children {
        return json!({"attempted": false, "reason": "disabled"});
    }
    let Some(pr) = pr else {
        return json!({"attempted": false, "reason": "missing_pr"});
    };
    if let Some(reason) = auto_merge_block_reason(pr) {
        return json!({
            "attempted": false,
            "enabled": false,
            "pr_number": pr.number,
            "fallback": "wait_for_human_merge",
            "reason": reason
        });
    }
    match query.enable_pr_auto_merge(&state.repo, pr.number) {
        Ok(()) => json!({"attempted": true, "enabled": true, "pr_number": pr.number}),
        Err(err) => json!({
            "attempted": true,
            "enabled": false,
            "pr_number": pr.number,
            "fallback": "wait_for_human_merge",
            "error": err.to_string()
        }),
    }
}

pub fn auto_merge_block_reason(pr: &GithubIssuePrState) -> Option<&'static str> {
    if pr.status_check_rollup.as_deref() != Some("passed") {
        return Some("checks_not_passed");
    }
    match pr.review_decision.as_deref() {
        Some("changes_requested") => Some("changes_requested"),
        Some("review_required") => Some("review_required"),
        _ => None,
    }
}

pub fn record_superseded_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Parent orchestration paused on superseded child issue #{child}; a replacement PR needs human confirmation."
            ),
        )
        .map_err(github_error)?;
    record_blocked_child(state, query, child, pr, "superseded_child_pr")
}

pub fn record_blocked_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
    reason: &str,
) -> Result<StepOutcome, EngineError> {
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": false,
            "child_issue_number": child,
            "state": "blocked",
            "reason": reason,
            "pr": pr
        }),
    )?;
    query
        .remove_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!("Parent orchestration is blocked on child issue #{child}: {reason}."),
        )
        .map_err(github_error)?;
    update_rollup(state, child, None, reason, pr)?;
    Ok(StepOutcome::Fixable)
}

pub fn mark_child_lease_completed(
    state: &OrchestrationState,
    child: u64,
    run_id: &str,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        crate::persistence::update_lease_status_conditional(
            &conn,
            &lease.lease_id,
            LeaseStatus::Completed,
            &[
                LeaseStatus::Completed,
                LeaseStatus::Failed,
                LeaseStatus::ReadyToResume,
            ],
            Some(run_id),
            Some(run_id),
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

pub struct ChildLaunchCompletion<'a> {
    pub child: u64,
    pub lease: &'a crate::persistence::leases::IssueLease,
    pub request: &'a ChildWorkflowLaunchRequest,
    pub result: ChildWorkflowRunResult,
    pub run_status: Option<RunStatus>,
    pub pr: Option<GithubIssuePrState>,
}

pub fn finish_child_launch(
    state: &OrchestrationState,
    context: &mut StepContext,
    query: &dyn GithubIssueQuery,
    conn: &rusqlite::Connection,
    completion: ChildLaunchCompletion<'_>,
) -> Result<StepOutcome, EngineError> {
    let effective_result =
        classify_child_run_result(&completion.result, completion.run_status.as_ref());
    let outcome = match effective_result {
        ChildWorkflowRunResult::CompletedSuccess => "completed_success",
        ChildWorkflowRunResult::CompletedFailure => "completed_failure",
        ChildWorkflowRunResult::WaitingExternal => "waiting_external",
    };
    let lease_outcome = advance_child_lease_for_result(conn, &completion, &effective_result)?;
    if !should_apply_child_finalization_side_effects(&lease_outcome, &effective_result, &completion)
    {
        // Stale child finalization: the lease was advanced by a concurrent
        // writer to a foreign owner, is in an incompatible status (e.g.
        // Completed or CleanupAbandoned), or the lease row is missing. Stop
        // before any side effects — artifacts, context mutation, rollup, label
        // removal, and comments — to avoid duplicating work on a lease we no
        // longer own or that has already reached a durable terminal state.
        return Ok(step_outcome_for_child_result(&effective_result));
    }
    write_launch_artifact(
        state,
        json!({
            "launched": true,
            "child_issue_number": completion.child,
            "child_workflow_type_id": completion.request.workflow_type_id,
            "child_config_id": completion.request.config_id,
            "run_id": completion.request.run_id,
            "lease_id": completion.lease.lease_id,
            "resumed": completion.lease.run_id.is_some(),
            "outcome": outcome,
            "run_status": completion.run_status.as_ref().map(ToString::to_string),
            "pr": completion.pr
        }),
    )?;
    context.set("child_run_id", &completion.request.run_id);
    if let Some(pr_state) = completion.pr.as_ref() {
        context.set("child_pr_number", &pr_state.number.to_string());
    }
    update_rollup(
        state,
        completion.child,
        Some(&completion.request.run_id),
        outcome,
        completion.pr.as_ref(),
    )?;
    if effective_result == ChildWorkflowRunResult::WaitingExternal {
        write_child_workflow_wait_artifact(
            state,
            completion.child,
            Some(completion.lease),
            Some(&completion.request.run_id),
            "child_workflow_waiting_external",
            completion.run_status.as_ref(),
        )?;
    }
    if effective_result == ChildWorkflowRunResult::CompletedFailure {
        record_terminal_child_failure(state, query, &completion)?;
    }
    Ok(step_outcome_for_child_result(&effective_result))
}

/// Conditionally advance a child lease to the outcome-appropriate status,
/// guarding against overwriting a durable `CleanupAbandoned` protection, and
/// return the classified conditional outcome so the caller can decide whether
/// to apply finalization side effects.
///
/// When the engine runner detects a failure-cleanup path it protects the lease
/// by transitioning it to [`LeaseStatus::CleanupAbandoned`] (conditional on the
/// owned run id). That protection must survive this finalization step: a
/// reclaimable `Failed` would allow the parent orchestrator (or a competing
/// daemon) to relaunch a duplicate workflow while the original run's cleanup
/// artifacts are still owned.
///
/// Each effective result maps to a conditional update keyed on the durable run
/// provenance (`expected_run_id`) and the set of statuses from which that
/// transition is valid — mirroring the daemon finalization pattern in
/// [`crate::daemon::launcher::finish_lease_after_result`]. `CleanupAbandoned`
/// is deliberately excluded from the failure transition's expected set so the
/// conditional update is a no-op when protection is in place, leaving the
/// durable lease state intact rather than overwriting it with reclaimable
/// `Failed`.
///
/// The returned [`ConditionalLeaseStatusOutcome`] distinguishes `Applied` from
/// the rejection variants so the caller can avoid emitting finalization side
/// effects (artifacts, context mutation, rollup, label removal, comments) for
/// a lease it no longer owns or that has already reached a foreign terminal
/// state.
fn advance_child_lease_for_result(
    conn: &rusqlite::Connection,
    completion: &ChildLaunchCompletion<'_>,
    effective_result: &ChildWorkflowRunResult,
) -> Result<ConditionalLeaseStatusOutcome, EngineError> {
    let (status, expected_statuses) = match effective_result {
        // Failed must not overwrite CleanupAbandoned: the expected set excludes
        // CleanupAbandoned so a protected lease is left untouched.
        ChildWorkflowRunResult::CompletedFailure => (
            LeaseStatus::Failed,
            &[LeaseStatus::Running, LeaseStatus::Failed][..],
        ),
        ChildWorkflowRunResult::CompletedSuccess => (
            LeaseStatus::ReadyToResume,
            &[LeaseStatus::Running, LeaseStatus::WaitingExternal][..],
        ),
        ChildWorkflowRunResult::WaitingExternal => (
            LeaseStatus::WaitingExternal,
            &[LeaseStatus::Running, LeaseStatus::WaitingExternal][..],
        ),
    };
    update_lease_status_conditional_outcome(
        conn,
        &completion.lease.lease_id,
        status,
        expected_statuses,
        None,
        Some(&completion.request.run_id),
    )
    .map_err(sql_error)
}

/// Decide whether finalization side effects (artifacts, context mutation,
/// rollup, label removal, comments) may be applied after a child lease
/// conditional transition.
///
/// Side effects are only applied when the conditional transition either
/// succeeded (`Applied`) *or* was idempotently rejected because the lease
/// already holds the exact same terminal result owned by this same run id.
/// In every other rejection case — a foreign owner, an incompatible terminal
/// status (e.g. `Completed`, `CleanupAbandoned`), a missing lease row, or a
/// stale `CleanupAbandoned` lease — the durable state is authoritative and the
/// caller must not emit any side effects that would duplicate work on a lease
/// it no longer owns.
///
/// `CleanupAbandoned` is never treated as reclaimable here, even on an
/// exact-same-owner match: it represents a deliberate, durable protection that
/// requires explicit continuation and must not be reclaimed via finalization.
fn should_apply_child_finalization_side_effects(
    outcome: &ConditionalLeaseStatusOutcome,
    effective_result: &ChildWorkflowRunResult,
    completion: &ChildLaunchCompletion<'_>,
) -> bool {
    match outcome {
        ConditionalLeaseStatusOutcome::Applied => true,
        ConditionalLeaseStatusOutcome::Missing => false,
        ConditionalLeaseStatusOutcome::Rejected {
            current_status,
            current_run_id,
        } => {
            // Allow only an exact same-owner idempotent match: the lease
            // already holds the terminal result this finalization would have
            // produced, owned by this very run id. Any other owner or any
            // non-matching terminal status (including CleanupAbandoned) must
            // suppress side effects.
            let expected_terminal = match effective_result {
                ChildWorkflowRunResult::CompletedFailure => LeaseStatus::Failed,
                ChildWorkflowRunResult::CompletedSuccess => LeaseStatus::ReadyToResume,
                ChildWorkflowRunResult::WaitingExternal => LeaseStatus::WaitingExternal,
            };
            *current_status == expected_terminal
                && current_run_id.as_deref() == Some(completion.request.run_id.as_str())
        }
    }
}

/// Map a child workflow result to its workflow step outcome.
fn step_outcome_for_child_result(effective_result: &ChildWorkflowRunResult) -> StepOutcome {
    match effective_result {
        ChildWorkflowRunResult::CompletedFailure => StepOutcome::Fixable,
        ChildWorkflowRunResult::CompletedSuccess => StepOutcome::Success,
        ChildWorkflowRunResult::WaitingExternal => StepOutcome::Wait,
    }
}

pub fn record_terminal_child_failure(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    completion: &ChildLaunchCompletion<'_>,
) -> Result<(), EngineError> {
    write_json(
        &state.artifact_root,
        "child-terminal-state.json",
        &json!({
            "child_issue_number": completion.child,
            "state": "failed_child_run",
            "child_run_id": completion.request.run_id,
            "lease_id": completion.lease.lease_id,
            "run_status": completion.run_status.as_ref().map(ToString::to_string),
            "pr": completion.pr
        }),
    )?;
    query
        .remove_label(&state.repo, completion.child, &state.luther_label)
        .map_err(github_error)?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Parent orchestration is paused because child issue #{} reached a terminal failed workflow state.",
                completion.child
            ),
        )
        .map_err(github_error)?;
    update_rollup(
        state,
        completion.child,
        Some(&completion.request.run_id),
        "failed_child_run",
        completion.pr.as_ref(),
    )
}

pub fn classify_child_run_result(
    process_result: &ChildWorkflowRunResult,
    run_status: Option<&RunStatus>,
) -> ChildWorkflowRunResult {
    match run_status {
        Some(
            RunStatus::Initialized
            | RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::WaitingForChecks
            | RunStatus::WaitingExternal
            | RunStatus::ReadyToResume
            | RunStatus::Remediating
            | RunStatus::Blocked
            | RunStatus::Paused,
        ) => ChildWorkflowRunResult::WaitingExternal,
        Some(RunStatus::Completed | RunStatus::Merged) => ChildWorkflowRunResult::CompletedSuccess,
        Some(RunStatus::Failed | RunStatus::Abandoned | RunStatus::Cancelled) => {
            ChildWorkflowRunResult::CompletedFailure
        }
        None => process_result.clone(),
    }
}

pub fn write_launch_artifact(state: &OrchestrationState, value: Value) -> Result<(), EngineError> {
    write_json(&state.artifact_root, "child-run-launch.json", &value)
}

pub fn child_is_complete(child: &ChildIssueState) -> bool {
    matches!(child.terminal_state, ChildIssueStatus::Merged)
}

pub fn child_is_blocked(child: &ChildIssueState) -> bool {
    matches!(
        child.terminal_state,
        ChildIssueStatus::Blocked
            | ChildIssueStatus::MergedIssueOpen
            | ChildIssueStatus::Superseded
            | ChildIssueStatus::ClosedUnmerged
    )
}

pub fn parent_summary_comment(complete: bool, evaluation: &Value) -> String {
    let evaluation_json = parent_summary_evaluation_json(evaluation);
    if complete {
        format!("Parent orchestration complete. Evidence:\n{evaluation_json}")
    } else {
        format!("Parent orchestration is incomplete or blocked. Current state:\n{evaluation_json}")
    }
}

pub fn parent_summary_evaluation_json(evaluation: &Value) -> String {
    match serde_json::to_string_pretty(evaluation) {
        Ok(json) => json,
        Err(err) => format!(
            "Parent orchestration evaluation serialization failed; diagnostic context could not be encoded as JSON: {err}"
        ),
    }
}
