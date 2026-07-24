use super::*;

pub(super) fn load_parent_issue(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, state.parent_issue_number, &state.luther_label)
        .map_err(github_error)?;
    let issue = query
        .get_issue(&state.repo, state.parent_issue_number)
        .map_err(github_error)?
        .ok_or_else(|| parent_error("parent issue could not be loaded".to_string()))?;
    write_json(&state.artifact_root, "parent-issue.json", &issue)?;
    context.set("parent_issue_number", &issue.number.to_string());
    Ok(StepOutcome::Success)
}

pub fn discover_subissues(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let children = query
        .list_sub_issues(&state.repo, state.parent_issue_number)
        .map_err(github_error)?;
    let numbers: Vec<u64> = children.iter().map(|child| child.issue.number).collect();
    write_json(&state.artifact_root, "parent-subissues.json", &children)?;
    write_json(
        &state.artifact_root,
        "parent-refresh-snapshot.json",
        &json!({"parent_issue_number": state.parent_issue_number, "children": numbers}),
    )?;
    context.set("child_issue_numbers", &json!(numbers).to_string());
    if state.current_step == "refresh_parent_and_children" {
        clear_selected_child(&state.artifact_root)?;
    }
    Ok(StepOutcome::Success)
}

pub fn classify_subissues(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let children = read_children(&state.artifact_root)?;
    let conn = daemon_connection()?;
    let states = children
        .iter()
        .map(|child| classify_child_with_run_state(state, query, &conn, &child.issue))
        .collect::<Result<Vec<_>, _>>()?;
    write_json(
        &state.artifact_root,
        "subissue-state-snapshot.json",
        &states,
    )?;
    context.set("subissue_state_snapshot", &json!(states).to_string());
    Ok(StepOutcome::Success)
}

pub(super) fn classify_child_with_run_state(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    conn: &rusqlite::Connection,
    issue: &GithubIssue,
) -> Result<ChildIssueState, EngineError> {
    let pr = query
        .pr_state_for_issue(&state.repo, issue.number)
        .map_err(github_error)?;
    let mut child = classify_child(issue, pr.as_ref());
    apply_child_run_state(state, conn, issue, &mut child)?;
    Ok(child)
}

pub fn apply_child_run_state(
    state: &OrchestrationState,
    conn: &rusqlite::Connection,
    issue: &GithubIssue,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    if let Some(lease) =
        get_lease_for_issue(conn, &state.repo, child.issue_number).map_err(sql_error)?
    {
        match lease.status {
            LeaseStatus::Failed
            | LeaseStatus::Abandoned
            | LeaseStatus::CleanupAbandoned
            | LeaseStatus::Stale
                if issue.state.eq_ignore_ascii_case("open") =>
            {
                child.terminal_state = ChildIssueStatus::FailedRun;
            }
            LeaseStatus::Claimed
            | LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Running => {
                if stale_child_run(&lease, state.merge_poll_interval_seconds)
                    && issue.state.eq_ignore_ascii_case("open")
                {
                    child.terminal_state = ChildIssueStatus::StaleRun;
                } else if !child_workflow_completed(conn, &lease)? {
                    child.terminal_state = ChildIssueStatus::ActiveRun;
                }
            }
            LeaseStatus::Failed
            | LeaseStatus::Abandoned
            | LeaseStatus::CleanupAbandoned
            | LeaseStatus::Stale => {}
            LeaseStatus::Pending | LeaseStatus::Completed => {}
        }
    }
    apply_child_rollup_state(state, child)
}

pub(super) fn apply_child_rollup_state(
    state: &OrchestrationState,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    let rollup = read_rollup(&state.artifact_root)?;
    if rollup.children.iter().any(|entry| {
        entry.child_issue_number == child.issue_number
            && unresolved_rollup_outcome_requires_pr(entry)
    }) {
        child.terminal_state = ChildIssueStatus::Blocked;
    }
    Ok(())
}

pub(super) fn stale_child_run(
    lease: &crate::persistence::leases::IssueLease,
    poll_interval_seconds: u64,
) -> bool {
    let grace_seconds = poll_interval_seconds.saturating_mul(3).max(900);
    let stale_after = match i64::try_from(grace_seconds) {
        Ok(seconds) => Duration::seconds(seconds),
        Err(_) => return false,
    };
    Utc::now().signed_duration_since(lease.heartbeat_at) > stale_after
}

pub fn child_workflow_completed(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<bool, EngineError> {
    let Some(run_id) = lease.run_id.as_deref() else {
        return Ok(false);
    };
    let Some(metadata) = get_run_with_conn(conn, run_id).map_err(sql_error)? else {
        return Ok(false);
    };
    Ok(matches!(
        metadata.status,
        RunStatus::Completed | RunStatus::ReviewReady | RunStatus::Merged
    ))
}

pub fn determine_subissue_order(
    context: &mut StepContext,
    state: &OrchestrationState,
) -> Result<StepOutcome, EngineError> {
    let children = read_children(&state.artifact_root)?;
    let order = order_subissues(&children);
    let artifact_name = if state.current_step == "determine_refreshed_subissue_order" {
        "subissue-order-plan-refreshed.json"
    } else {
        "subissue-order-plan.json"
    };
    write_json(
        &state.artifact_root,
        artifact_name,
        &json!({"order": order, "strategy": "native_position_then_issue_number"}),
    )?;
    if state.current_step == "determine_refreshed_subissue_order" {
        write_json(
            &state.artifact_root,
            "subissue-order-plan.json",
            &json!({"order": order, "strategy": "native_position_then_issue_number"}),
        )?;
    }
    context.set("subissue_order", &json!(order).to_string());
    Ok(StepOutcome::Success)
}

pub(super) fn select_next_child(
    context: &mut StepContext,
    state: &OrchestrationState,
) -> Result<StepOutcome, EngineError> {
    let states: Vec<ChildIssueState> =
        read_json(&state.artifact_root.join("subissue-state-snapshot.json"))?;
    let order_plan: Value = read_json(&state.artifact_root.join("subissue-order-plan.json"))?;
    let order: Vec<u64> = serde_json::from_value(order_plan["order"].clone())
        .map_err(|err| parent_error(format!("parse subissue order artifact: {err}")))?;
    let missing_states = model::missing_ordered_child_states(&states, &order);
    if !missing_states.is_empty() {
        write_json(
            &state.artifact_root,
            "selected-child.json",
            &json!({
                "issue_number": null,
                "blocked": true,
                "reason": "order_state_snapshot_mismatch",
                "missing_state_issue_numbers": missing_states
            }),
        )?;
        return Ok(StepOutcome::Fixable);
    }
    let next = next_actionable_child(&states, &order);
    write_json(
        &state.artifact_root,
        "selected-child.json",
        &json!({"issue_number": next}),
    )?;
    if let Some(number) = next {
        context.set("selected_child_issue_number", &number.to_string());
        Ok(StepOutcome::Success)
    } else {
        Ok(StepOutcome::Fixable)
    }
}

pub fn launch_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
) -> Result<StepOutcome, EngineError> {
    let mut conn = daemon_connection()?;
    let Some(child) = selected_child(&state.artifact_root)? else {
        write_launch_artifact(
            state,
            json!({"launched": false, "reason": "no_actionable_child"}),
        )?;
        return Ok(StepOutcome::Success);
    };
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    if let Some(pr) = pr.filter(is_observable_existing_pr) {
        if !has_active_or_recoverable_child_lease(state, child, &conn)? {
            return observe_existing_child_pr(context, state, query, child, &pr);
        }
    }
    match prepare_child_lease(state, child, &mut conn)? {
        ChildLeaseAction::Wait { lease, reason } => {
            wait_for_existing_child(state, child, lease.as_ref(), &reason)
        }
        ChildLeaseAction::Resume(lease) => {
            resume_child_workflow(context, state, query, runner, child, &lease, &conn)
        }
        ChildLeaseAction::Launch(lease) => {
            start_child_workflow(context, state, query, runner, child, &lease, &conn)
        }
    }
}

pub fn has_active_or_recoverable_child_lease(
    state: &OrchestrationState,
    child: u64,
    conn: &rusqlite::Connection,
) -> Result<bool, EngineError> {
    Ok(get_lease_for_issue(conn, &state.repo, child)
        .map_err(sql_error)?
        .is_some_and(|lease| {
            matches!(
                lease.status,
                LeaseStatus::ReadyToResume
                    | LeaseStatus::Failed
                    | LeaseStatus::Abandoned
                    | LeaseStatus::Stale
                    | LeaseStatus::WaitingExternal
                    | LeaseStatus::Claimed
                    | LeaseStatus::Running
            )
        }))
}

pub fn is_observable_existing_pr(pr: &GithubIssuePrState) -> bool {
    !pr.merged
        && !pr.state.eq_ignore_ascii_case("closed")
        && !pr.state.eq_ignore_ascii_case("superseded")
}

pub fn observe_existing_child_pr(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: &GithubIssuePrState,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": "existing_child_pr",
            "observing_existing_pr": true,
            "pr": pr
        }),
    )?;
    context.set("child_pr_number", &pr.number.to_string());
    update_rollup(state, child, None, "observing_existing_child_pr", Some(pr))?;
    Ok(StepOutcome::Success)
}

pub fn wait_for_existing_child(
    state: &OrchestrationState,
    child: u64,
    lease: Option<&crate::persistence::leases::IssueLease>,
    reason: &str,
) -> Result<StepOutcome, EngineError> {
    let run_status = lease
        .and_then(|lease| lease.run_id.as_deref())
        .map(child_run_status_from_registry)
        .transpose()?
        .flatten();
    if run_status == Some(RunStatus::Merged) {
        write_launch_artifact(
            state,
            json!({
                "launched": false,
                "child_issue_number": child,
                "reason": "child_workflow_completed_waiting_for_pr_merge",
                "existing_run_id": lease.and_then(|lease| lease.run_id.as_deref()),
                "lease_status": lease.map(|lease| lease.status.to_string()),
                "run_status": run_status.map(|status| status.to_string())
            }),
        )?;
        update_rollup(
            state,
            child,
            lease.and_then(|lease| lease.run_id.as_deref()),
            "child_workflow_completed_waiting_for_pr_merge",
            None,
        )?;
        return Ok(StepOutcome::Success);
    }
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": reason,
            "existing_run_id": lease.and_then(|lease| lease.run_id.as_deref()),
            "lease_status": lease.map(|lease| lease.status.to_string()),
            "run_status": run_status.as_ref().map(ToString::to_string)
        }),
    )?;
    write_child_workflow_wait_artifact(
        state,
        child,
        lease,
        lease.and_then(|lease| lease.run_id.as_deref()),
        reason,
        run_status.as_ref(),
    )?;
    update_rollup(
        state,
        child,
        lease.and_then(|lease| lease.run_id.as_deref()),
        reason,
        None,
    )?;
    Ok(StepOutcome::Wait)
}

pub fn write_child_workflow_wait_artifact(
    state: &OrchestrationState,
    child: u64,
    lease: Option<&crate::persistence::leases::IssueLease>,
    child_run_id: Option<&str>,
    reason: &str,
    run_status: Option<&RunStatus>,
) -> Result<(), EngineError> {
    write_json(
        &state.artifact_root,
        "child-workflow-wait.json",
        &json!({
            "waiting": true,
            "state": "child_workflow_in_progress",
            "child_issue_number": child,
            "child_run_id": child_run_id,
            "child_lease_id": lease.map(|lease| lease.lease_id.as_str()),
            "lease_status": lease.map(|lease| lease.status.to_string()),
            "run_status": run_status.map(ToString::to_string),
            "reason": reason,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )
}

pub fn start_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    conn: &rusqlite::Connection,
) -> Result<StepOutcome, EngineError> {
    let request = child_launch_request(state, child);
    // Exact compare-and-swap: transition to Running only when the lease is
    // still in the fresh-claim state we observed. A rejected CAS means a
    // concurrent writer advanced the lease (relaunch, reclaim, or
    // finalization), so we must skip dispatch entirely rather than launch a
    // duplicate child.
    if !claim_running_lease_cas(
        conn,
        lease,
        &[LeaseStatus::Claimed],
        lease.run_id.as_deref(),
        &request.run_id,
    )? {
        return child_cas_rejected_outcome(
            state,
            child,
            lease,
            "start_child_workflow_cas_rejected",
        );
    }
    query
        .add_label(&state.repo, child, &state.luther_label)
        .map_err(|err| compensate_label_error(conn, lease, &request.run_id, err))?;
    let result = runner.launch_child(&request).map_err(|err| {
        if let Err(restore_err) = restore_child_lease_after_runner_error(
            conn,
            lease,
            lease.status,
            lease.run_id.as_deref(),
            &request.run_id,
        ) {
            return parent_error(format!(
                "{err}; failed to restore child lease {}: {restore_err}",
                lease.lease_id
            ));
        }
        parent_error(err.to_string())
    })?;
    let (run_status, pr) = post_launch_metadata(state, query, runner, child, lease, &request)
        .map_err(|err| {
            if let Err(restore_err) = restore_child_lease_after_runner_error(
                conn,
                lease,
                lease.status,
                lease.run_id.as_deref(),
                &request.run_id,
            ) {
                return parent_error(format!(
                    "{err}; failed to restore child lease {} after metadata error: {restore_err}",
                    lease.lease_id
                ));
            }
            err
        })?;
    finish_child_launch(
        state,
        context,
        query,
        conn,
        ChildLaunchCompletion {
            child,
            lease,
            request: &request,
            result,
            run_status,
            pr,
        },
    )
}

pub fn resume_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    conn: &rusqlite::Connection,
) -> Result<StepOutcome, EngineError> {
    let Some(run_id) = lease.run_id.clone() else {
        return missing_resume_run_id_outcome(conn, state, child, lease);
    };
    let request = child_resume_request(state, child, run_id.clone());
    // Exact compare-and-swap: transition to Running only when the lease still
    // holds the ReadyToResume status and the same run_id we are resuming. A
    // rejected CAS means a concurrent writer advanced the lease, so we skip the
    // dispatch rather than resume a stale or duplicate child workflow.
    if !claim_running_lease_cas(
        conn,
        lease,
        &[LeaseStatus::ReadyToResume],
        Some(&run_id),
        &request.run_id,
    )? {
        return child_cas_rejected_outcome(
            state,
            child,
            lease,
            "resume_child_workflow_cas_rejected",
        );
    }
    dispatch_and_finalize_child(
        state,
        context,
        query,
        runner,
        conn,
        ChildDispatchInput {
            child,
            lease,
            request: &request,
            compensate_run_id: Some(&run_id),
        },
        |req| runner.resume_child(req),
    )
}

/// Fail a resume lease that has no durable run id and record a fixable launch
/// artifact, so the orchestrator re-evaluates the child rather than erroring.
///
/// The failure is conditional on the lease still matching the observed status
/// (ReadyToResume) and run_id-less state. A concurrent writer that advanced
/// the lease or assigned a run id is authoritative: the conditional update
/// becomes a no-op, leaving the durable state intact rather than overwriting
/// it with a terminal `Failed`.
fn missing_resume_run_id_outcome(
    conn: &rusqlite::Connection,
    state: &OrchestrationState,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<StepOutcome, EngineError> {
    let updated = crate::persistence::update_lease_status_conditional_exact_owner(
        conn,
        &lease.lease_id,
        LeaseStatus::Failed,
        &[lease.status],
        None,
        lease.run_id.as_deref(),
    )
    .map_err(sql_error)?;
    if !updated {
        let durable = crate::persistence::get_lease_for_issue(conn, &state.repo, child)
            .map_err(sql_error)?
            .ok_or_else(|| EngineError::PersistenceError("child lease disappeared".into()))?;
        return child_cas_rejected_outcome(
            state,
            child,
            &durable,
            "missing_child_run_id_repair_rejected",
        );
    }
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": "missing_child_run_id",
            "lease_id": lease.lease_id,
            "lease_status": lease.status.to_string()
        }),
    )?;
    Ok(StepOutcome::Fixable)
}

/// Per-child references needed to dispatch and finalize a child workflow.
struct ChildDispatchInput<'a> {
    child: u64,
    lease: &'a crate::persistence::leases::IssueLease,
    request: &'a ChildWorkflowLaunchRequest,
    /// The run id keyed on the lease's owned `Running` transition, used to
    /// restore the lease on a post-dispatch error.
    compensate_run_id: Option<&'a str>,
}

/// Dispatch a child workflow (launch or resume), collect post-launch metadata,
/// and finalize the lease, applying identical error-compensation to both phases.
///
/// When the runner dispatch or the post-launch metadata read fails after the
/// CAS has transitioned the lease to `Running`, the lease is restored to its
/// observed status keyed on the owned run id. A failed restore augments the
/// original error with the restore failure; otherwise the original error
/// propagates unchanged.
fn dispatch_and_finalize_child(
    state: &OrchestrationState,
    context: &mut StepContext,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    conn: &rusqlite::Connection,
    input: ChildDispatchInput<'_>,
    dispatch: impl FnOnce(
        &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError>,
) -> Result<StepOutcome, EngineError> {
    let ChildDispatchInput {
        child,
        lease,
        request,
        compensate_run_id,
    } = input;
    let result = dispatch(request).map_err(|err| {
        compensate_dispatch_error(conn, lease, compensate_run_id, &request.run_id, err)
    })?;
    let (run_status, pr) = post_launch_metadata(state, query, runner, child, lease, request)
        .map_err(|err| {
            compensate_metadata_error(conn, lease, compensate_run_id, &request.run_id, err)
        })?;
    finish_child_launch(
        state,
        context,
        query,
        conn,
        ChildLaunchCompletion {
            child,
            lease,
            request,
            result,
            run_status,
            pr,
        },
    )
}

/// Compensate a label-add failure that occurs after the CAS has transitioned
/// the lease to `Running` with the new run id. Because no dispatch has happened
/// yet, the lease must be rolled back to its observed status and observed run
/// id so a future pass can reclaim it; otherwise the lease would be stranded
/// as `Running` with no running workflow.
///
/// The restore is keyed on the exact `Running` status owned by
/// `running_run_id` (the CAS-acquired owner). A concurrent writer that already
/// advanced the lease causes the restore to be a no-op, which is the correct
/// outcome: that writer is authoritative. On a successful restore the original
/// GitHub error is propagated; on a failed restore both errors are chained.
fn compensate_label_error(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    running_run_id: &str,
    err: GithubError,
) -> EngineError {
    restore_for_compensation(
        conn,
        lease,
        lease.run_id.as_deref(),
        running_run_id,
        &err.to_string(),
        " after label add failure",
    )
    .unwrap_or_else(|| github_error(err))
}

/// Restore a lease whose CAS-acquired `Running` transition must be rolled back
/// after a runner dispatch error, returning the error to propagate. A failed
/// restore augments the dispatch error with the restore failure; a successful
/// restore re-wraps the runner error as a parent orchestration error.
fn compensate_dispatch_error(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    run_id: Option<&str>,
    expected_running_run_id: &str,
    err: ChildWorkflowRunnerError,
) -> EngineError {
    restore_for_compensation(
        conn,
        lease,
        run_id,
        expected_running_run_id,
        &err.to_string(),
        "",
    )
    .unwrap_or_else(|| parent_error(err.to_string()))
}

/// Restore a lease whose CAS-acquired `Running` transition must be rolled back
/// after a post-launch metadata error, returning the error to propagate. A
/// failed restore augments the metadata error with the restore failure; a
/// successful restore returns the original metadata error unchanged.
fn compensate_metadata_error(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    run_id: Option<&str>,
    expected_running_run_id: &str,
    err: EngineError,
) -> EngineError {
    restore_for_compensation(
        conn,
        lease,
        run_id,
        expected_running_run_id,
        &err.to_string(),
        " after metadata error",
    )
    // On successful restore, the original metadata error propagates unchanged
    // (it is already an EngineError) to avoid double-wrapping its message.
    .unwrap_or(err)
}

/// Attempt the conditional lease restore used by error-compensation. Returns
/// `Some(augmented_error)` when the restore itself failed (the augmented error
/// must be propagated), or `None` when the restore succeeded and the caller
/// should propagate the original error instead.
fn restore_for_compensation(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    run_id: Option<&str>,
    expected_running_run_id: &str,
    err_message: &str,
    suffix: &str,
) -> Option<EngineError> {
    let Err(restore_err) = restore_child_lease_after_runner_error(
        conn,
        lease,
        lease.status,
        run_id,
        expected_running_run_id,
    ) else {
        return None;
    };
    Some(parent_error(format!(
        "{err_message}; failed to restore child lease {lease_id}{suffix}: {restore_err}",
        lease_id = lease.lease_id
    )))
}

/// Conditionally transition a child lease to `Running` via an exact
/// expected-status/expected-owner compare-and-swap.
///
/// The CAS is keyed on the lease's *observed* status (and, when provided, the
/// observed run_id) so a stale orchestrator returning from a slow pre-dispatch
/// step cannot overwrite a lease that a concurrent writer has already advanced.
/// Returns `true` when the transition applied and dispatch may proceed; `false`
/// when the CAS was rejected and dispatch must be skipped.
pub(super) fn claim_running_lease_cas(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    expected_statuses: &[LeaseStatus],
    expected_run_id: Option<&str>,
    new_run_id: &str,
) -> Result<bool, EngineError> {
    crate::persistence::update_lease_status_conditional_exact_owner(
        conn,
        &lease.lease_id,
        LeaseStatus::Running,
        expected_statuses,
        Some(new_run_id),
        expected_run_id,
    )
    .map_err(sql_error)
}

/// Build the step outcome for a rejected pre-dispatch CAS, recording a wait
/// artifact so the orchestrator re-evaluates the lease on the next pass rather
/// than erroring on a contention that a concurrent writer has already resolved.
/// Artifact write errors are propagated rather than silently swallowed so a
/// durable-record failure surfaces to the caller.
pub(super) fn child_cas_rejected_outcome(
    state: &OrchestrationState,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    reason: &str,
) -> Result<StepOutcome, EngineError> {
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": reason,
            "lease_id": lease.lease_id,
            "observed_lease_status": lease.status.to_string(),
            "observed_run_id": lease.run_id
        }),
    )?;
    Ok(StepOutcome::Wait)
}

pub fn restore_child_lease_after_runner_error(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
    status: LeaseStatus,
    run_id: Option<&str>,
    expected_running_run_id: &str,
) -> Result<(), EngineError> {
    let restored = crate::persistence::update_lease_status_conditional(
        conn,
        &lease.lease_id,
        status,
        &[LeaseStatus::Running],
        run_id,
        Some(expected_running_run_id),
    )
    .map_err(sql_error)?;
    if restored {
        Ok(())
    } else {
        Err(parent_error(format!(
            "child lease {} changed owner or status; stale error compensation was rejected",
            lease.lease_id
        )))
    }
}

pub fn post_launch_metadata(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(Option<RunStatus>, Option<GithubIssuePrState>), EngineError> {
    let run_status = runner.run_status(&request.run_id).map_err(|err| {
        post_launch_metadata_error(
            parent_error(err.to_string()),
            "read child run status",
            &lease.lease_id,
            &request.run_id,
        )
    })?;
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(|err| {
            post_launch_metadata_error(
                github_error(err),
                "read child PR state",
                &lease.lease_id,
                &request.run_id,
            )
        })?;
    Ok((run_status, pr))
}

pub(super) fn post_launch_metadata_error(
    err: EngineError,
    action: &str,
    lease_id: &str,
    run_id: &str,
) -> EngineError {
    parent_error(format!(
        "{action} after child launch failed for lease {lease_id} run {run_id}: {err}"
    ))
}

pub fn wait_for_child_merge(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let Some(child) = selected_child(&state.artifact_root)? else {
        write_json(
            &state.artifact_root,
            "child-merge-wait.json",
            &json!({"waiting": false, "reason": "no_actionable_child"}),
        )?;
        return Ok(StepOutcome::Success);
    };
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    match classify_child_pr_wait(pr.as_ref()) {
        ChildPrWait::Merged => finish_merged_child(context, state, query, child, pr.as_ref()),
        ChildPrWait::ReadyForHumanMerge => {
            let run_id = child_run_id_for_wait(state, child)?;
            if child_workflow_ready_for_merge(&run_id)? {
                record_ready_for_human_merge(state, query, child, pr.as_ref())
            } else if run_id.is_none() {
                record_observed_child_pr_merge_wait(state, query, child, pr.as_ref())
            } else {
                record_child_pr_still_in_progress(state, child, pr.as_ref(), run_id.as_deref())
            }
        }
        ChildPrWait::MissingPr => {
            record_blocked_child(state, query, child, pr.as_ref(), "missing_child_pr")
        }
        ChildPrWait::ClosedUnmerged => {
            reevaluate_closed_unmerged_child(state, query, child, pr.as_ref())
        }
        ChildPrWait::Superseded => record_superseded_child(state, query, child, pr.as_ref()),
    }
}

pub(super) fn record_observed_child_pr_merge_wait(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let already_recorded = rollup_has_outcome(state, child, "observing_existing_child_pr")?;
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "observing_existing_child_pr",
            "child_run_id": null,
            "pr": pr,
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
                    "Child issue #{child} already has an active PR. Parent orchestration will observe it and continue after the PR is merged."
                ),
            )
            .map_err(github_error)?;
    }
    update_rollup(state, child, None, "observing_existing_child_pr", pr)?;
    Ok(StepOutcome::Wait)
}
