#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct ParentOrchestrationRollup {
    parent_issue_number: u64,
    children: Vec<ChildRollupEntry>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct ChildRollupEntry {
    child_issue_number: u64,
    child_run_id: Option<String>,
    child_artifact_dir: Option<String>,
    pr_number: Option<u64>,
    pr_state: Option<String>,
    merge_sha: Option<String>,
    outcome: Option<String>,
    non_actionable_reason: Option<String>,
}

fn bool_context(context: &StepContext, primary: &str, fallback: &str) -> bool {
    bool_context_default(context, primary, fallback, false)
}

fn bool_context_default(
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

fn daemon_connection() -> Result<rusqlite::Connection, EngineError> {
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    ensure_daemon_database_initialized(&db_path)?;
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|err| parent_error(format!("open daemon database: {err}")))?;
    configure_parent_orchestration_connection(&conn)?;
    Ok(conn)
}

fn ensure_daemon_database_initialized(db_path: &Path) -> Result<(), EngineError> {
    static INITIALIZED_DATABASES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
    > = std::sync::OnceLock::new();
    let initialized = INITIALIZED_DATABASES.get_or_init(Default::default);
    if initialized
        .lock()
        .map_err(|err| parent_error(format!("lock daemon database init guard: {err}")))?
        .contains(db_path)
    {
        return Ok(());
    }
    crate::persistence::init_database(db_path)
        .map_err(|err| parent_error(format!("initialize daemon database: {err}")))?;
    initialized
        .lock()
        .map_err(|err| parent_error(format!("lock daemon database init guard: {err}")))?
        .insert(db_path.to_path_buf());
    Ok(())
}

fn configure_parent_orchestration_connection(
    conn: &rusqlite::Connection,
) -> Result<(), EngineError> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| parent_error(format!("set daemon database busy timeout: {err}")))?;
    Ok(())
}

fn open_parent_orchestration_connection(path: &Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|err| format!("open parent orchestration database: {err}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| format!("set parent orchestration database busy timeout: {err}"))?;
    Ok(conn)
}

fn child_run_status_from_registry(run_id: &str) -> Result<Option<RunStatus>, EngineError> {
    let conn = daemon_connection()?;
    get_run_with_conn(&conn, run_id)
        .map(|metadata| metadata.map(|run| run.status))
        .map_err(sql_error)
}

enum ChildLeaseAction {
    Launch(crate::persistence::leases::IssueLease),
    Resume(crate::persistence::leases::IssueLease),
    Wait {
        lease: Option<crate::persistence::leases::IssueLease>,
        reason: String,
    },
}

fn prepare_child_lease(
    state: &OrchestrationState,
    child: u64,
    conn: &mut rusqlite::Connection,
) -> Result<ChildLeaseAction, EngineError> {
    prepare_child_lease_with_conn(state, child, conn)
}

fn prepare_child_lease_with_conn(
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
            LeaseStatus::Pending | LeaseStatus::Completed => ChildLeaseAction::Wait {
                lease: Some(lease),
                reason: "non_actionable_child_lease".to_string(),
            },
        });
    }
    claim_child_lease(state, child, conn)
}

fn claim_child_lease(
    state: &OrchestrationState,
    child: u64,
    conn: &rusqlite::Connection,
) -> Result<ChildLeaseAction, EngineError> {
    let Some(lease) = try_claim(conn, &state.repo, child, &state.child_config_id).map_err(sql_error)?
    else {
        return Ok(ChildLeaseAction::Wait {
            lease: None,
            reason: "child_lease_claim_contended".to_string(),
        });
    };
    Ok(ChildLeaseAction::Launch(lease))
}

fn prepare_relaunchable_child(
    conn: &mut rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<ChildLeaseAction, EngineError> {
    let tx = conn.transaction().map_err(sql_error)?;
    clear_child_lease_for_relaunch(&tx, lease)?;
    let relaunchable = get_lease_for_issue(&tx, &lease.issue_repo, lease.issue_number)
        .map_err(sql_error)?
        .ok_or_else(|| {
            parent_error("child lease disappeared while preparing relaunch".to_string())
        })?;
    tx.commit().map_err(sql_error)?;
    Ok(ChildLeaseAction::Launch(relaunchable))
}

fn clear_child_lease_for_relaunch(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<(), EngineError> {
    let updated = conn
        .execute(
            "UPDATE issue_leases SET status = ?1, run_id = NULL, updated_at = ?2 WHERE lease_id = ?3",
            rusqlite::params![
                LeaseStatus::Claimed.to_string(),
                Utc::now().to_rfc3339(),
                lease.lease_id
            ],
        )
        .map_err(sql_error)?;
    if updated == 0 {
        return Err(parent_error(format!(
            "child lease {} was not updated while preparing relaunch",
            lease.lease_id
        )));
    }
    Ok(())
}

enum ChildPrWait {
    Merged,
    ReadyForHumanMerge,
    MissingPr,
    ClosedUnmerged,
    Superseded,
}

fn classify_child_pr_wait(pr: Option<&GithubIssuePrState>) -> ChildPrWait {
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

fn finish_merged_child(
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

fn record_ready_for_human_merge(
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

fn persisted_child_run_id(
    context: &StepContext,
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    if let Some(run_id) = context.get("child_run_id") {
        return Ok(Some(run_id.clone()));
    }
    child_run_id_for_wait(state, child)
}

fn child_run_id_for_wait(
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    let conn = daemon_connection()?;
    Ok(get_lease_for_issue(&conn, &state.repo, child)
        .map_err(sql_error)?
        .and_then(|lease| lease.run_id))
}

fn child_workflow_ready_for_merge(run_id: &Option<String>) -> Result<bool, EngineError> {
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

fn record_child_pr_still_in_progress(
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

fn reevaluate_closed_unmerged_child(
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

fn mark_child_lease_relaunchable(
    state: &OrchestrationState,
    child: u64,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        update_lease_status(
            &conn,
            &lease.lease_id,
            LeaseStatus::Failed,
            lease.run_id.as_deref(),
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

fn attempt_auto_merge_if_enabled(
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

fn auto_merge_block_reason(pr: &GithubIssuePrState) -> Option<&'static str> {
    if pr.status_check_rollup.as_deref() != Some("passed") {
        return Some("checks_not_passed");
    }
    match pr.review_decision.as_deref() {
        Some("changes_requested" | "review_required") => Some("review_not_approved"),
        _ => None,
    }
}

fn record_superseded_child(
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

fn record_blocked_child(
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

fn child_launch_request(state: &OrchestrationState, child: u64) -> ChildWorkflowLaunchRequest {
    let stamp = Utc::now().timestamp_millis();
    child_request_with_run_id(
        state,
        child,
        format!("parent{}-child{}-{stamp}", state.parent_issue_number, child),
    )
}

fn child_resume_request(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    child_request_with_run_id(state, child, run_id)
}

fn child_request_with_run_id(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    let artifact_dir = state
        .artifact_dir
        .as_ref()
        .map(|base| child_artifact_dir(base, child, &run_id));
    ChildWorkflowLaunchRequest {
        workflow_type_id: state.child_workflow_type_id.clone(),
        config_id: state.child_config_id.clone(),
        run_id,
        repo: state.repo.clone(),
        issue_number: child,
        work_dir: state.work_dir.clone(),
        artifact_dir,
        config_root: state.config_root.clone(),
    }
}

fn child_artifact_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join(format!("issue-{child}")).join(run_id)
}

fn mark_child_lease_completed(
    state: &OrchestrationState,
    child: u64,
    run_id: &str,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        update_lease_status(&conn, &lease.lease_id, LeaseStatus::Completed, Some(run_id))
            .map_err(sql_error)?;
    }
    Ok(())
}

struct ChildLaunchCompletion<'a> {
    child: u64,
    lease: &'a crate::persistence::leases::IssueLease,
    request: &'a ChildWorkflowLaunchRequest,
    result: ChildWorkflowRunResult,
    run_status: Option<RunStatus>,
    pr: Option<GithubIssuePrState>,
}

fn finish_child_launch(
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
    let status = match effective_result {
        ChildWorkflowRunResult::CompletedSuccess => LeaseStatus::ReadyToResume,
        ChildWorkflowRunResult::CompletedFailure => LeaseStatus::Failed,
        ChildWorkflowRunResult::WaitingExternal => LeaseStatus::WaitingExternal,
    };
    update_lease_status(
        conn,
        &completion.lease.lease_id,
        status,
        Some(&completion.request.run_id),
    )
    .map_err(sql_error)?;
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
    Ok(match effective_result {
        ChildWorkflowRunResult::CompletedFailure => StepOutcome::Fixable,
        ChildWorkflowRunResult::CompletedSuccess => StepOutcome::Success,
        ChildWorkflowRunResult::WaitingExternal => StepOutcome::Wait,
    })
}

fn record_terminal_child_failure(
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

fn classify_child_run_result(
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

fn write_launch_artifact(state: &OrchestrationState, value: Value) -> Result<(), EngineError> {
    write_json(&state.artifact_root, "child-run-launch.json", &value)
}

fn update_rollup(
    state: &OrchestrationState,
    child: u64,
    run_id: Option<&str>,
    outcome: &str,
    pr: Option<&GithubIssuePrState>,
) -> Result<(), EngineError> {
    let mut rollup = read_rollup(&state.artifact_root)?;
    rollup.parent_issue_number = state.parent_issue_number;
    rollup
        .children
        .retain(|entry| entry.child_issue_number != child);
    rollup.children.push(ChildRollupEntry {
        child_issue_number: child,
        child_run_id: run_id.map(str::to_string),
        child_artifact_dir: run_id.and_then(|run_id| {
            state.artifact_dir.as_ref().map(|base| {
                child_artifact_dir(base, child, run_id)
                    .to_string_lossy()
                    .to_string()
            })
        }),
        pr_number: pr.map(|state| state.number),
        pr_state: pr.map(|state| state.state.clone()),
        merge_sha: pr.and_then(|state| state.merge_commit_sha.clone()),
        outcome: Some(outcome.to_string()),
        non_actionable_reason: non_actionable_reason_for_outcome(outcome),
    });
    rollup
        .children
        .sort_by_key(|entry| entry.child_issue_number);
    write_json(
        &state.artifact_root,
        "parent-orchestration-rollup.json",
        &rollup,
    )
}

fn non_actionable_reason_for_outcome(outcome: &str) -> Option<String> {
    match outcome {
        "non_actionable_child" => Some("child issue is explicitly non-actionable".to_string()),
        "non_actionable_child_lease" => {
            Some("child lease is already terminal outside the parent orchestrator".to_string())
        }
        _ => None,
    }
}

fn rollup_has_outcome(
    state: &OrchestrationState,
    child: u64,
    outcome: &str,
) -> Result<bool, EngineError> {
    let rollup = read_rollup(&state.artifact_root)?;
    Ok(rollup.children.iter().any(|entry| {
        entry.child_issue_number == child && entry.outcome.as_deref() == Some(outcome)
    }))
}

fn read_rollup(artifact_root: &Path) -> Result<ParentOrchestrationRollup, EngineError> {
    let path = artifact_root.join("parent-orchestration-rollup.json");

    if path.exists() {
        read_json(&path)
    } else {
        Ok(ParentOrchestrationRollup::default())
    }
}

fn child_is_complete(child: &ChildIssueState) -> bool {
    matches!(child.terminal_state, ChildIssueStatus::Merged)
}

fn child_is_blocked(child: &ChildIssueState) -> bool {
    matches!(
        child.terminal_state,
        ChildIssueStatus::Blocked
            | ChildIssueStatus::MergedIssueOpen
            | ChildIssueStatus::Superseded
            | ChildIssueStatus::ClosedUnmerged
    )
}

fn parent_summary_comment(complete: bool, evaluation: &Value) -> String {
    let evaluation_json = parent_summary_evaluation_json(evaluation);
    if complete {
        format!("Parent orchestration complete. Evidence:\n{evaluation_json}")
    } else {
        format!("Parent orchestration is incomplete or blocked. Current state:\n{evaluation_json}")
    }
}

fn parent_summary_evaluation_json(evaluation: &Value) -> String {
    match serde_json::to_string_pretty(evaluation) {
        Ok(json) => json,
        Err(err) => format!(
            "Parent orchestration evaluation serialization failed; diagnostic context could not be encoded as JSON: {err}"
        ),
    }
}

fn resume_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Resume)
}

fn launch_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Launch)
}

enum ChildRunMode {
    Launch,
    Resume,
}

fn run_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    mode: ChildRunMode,
) -> Result<ChildWorkflowRunResult, String> {
    let config_root = &request.config_root;
    let config_id = validated_child_id(&request.config_id, "config id")?;
    let workflow_type_id = validated_child_id(&request.workflow_type_id, "type id")?;
    let mut config = resolve_workflow_config(config_id, config_root)
        .map_err(|err| format!("resolve child config '{config_id}': {err}"))?;
    let workflow_type = resolve_workflow_type(workflow_type_id, config_root)
        .map_err(|err| format!("resolve child workflow type: {err}"))?;
    apply_child_overrides(&mut config, request)?;
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    if matches!(mode, ChildRunMode::Resume) {
        prepare_child_resume(&db_path, request)?;
    }
    let run_context = child_run_context(&config, request)?;
    let instance =
        WorkflowInstance::create_with_run_id(workflow_type, config.clone(), &request.run_id);
    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        &db_path,
        run_context,
    )
    .map_err(|err| err.to_string())?;
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, &config, &db_path)
}
