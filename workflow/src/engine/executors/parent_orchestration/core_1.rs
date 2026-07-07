fn load_parent_issue(
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

fn discover_subissues(
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

fn classify_subissues(
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

fn classify_child_with_run_state(
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

fn apply_child_run_state(
    state: &OrchestrationState,
    conn: &rusqlite::Connection,
    issue: &GithubIssue,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    if let Some(lease) =
        get_lease_for_issue(conn, &state.repo, child.issue_number).map_err(sql_error)?
    {
        match lease.status {
            LeaseStatus::Failed | LeaseStatus::Abandoned | LeaseStatus::Stale
                if issue.state.eq_ignore_ascii_case("open") =>
            {
                child.terminal_state = ChildTerminalState::FailedRun;
            }
            LeaseStatus::Claimed
            | LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Running => {
                if stale_child_run(&lease, state.merge_poll_interval_seconds)
                    && issue.state.eq_ignore_ascii_case("open")
                {
                    child.terminal_state = ChildTerminalState::StaleRun;
                } else if !child_workflow_completed(conn, &lease)? {
                    child.terminal_state = ChildTerminalState::ActiveRun;
                }
            }
            _ => {}
        }
    }
    apply_child_rollup_state(state, child)
}

fn apply_child_rollup_state(
    state: &OrchestrationState,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    let rollup = read_rollup(&state.artifact_root)?;
    if rollup.children.iter().any(|entry| {
        entry.child_issue_number == child.issue_number
            && unresolved_rollup_outcome_requires_pr(entry)
    }) {
        child.terminal_state = ChildTerminalState::Blocked;
    }
    Ok(())
}

fn stale_child_run(
    lease: &crate::persistence::leases::IssueLease,
    poll_interval_seconds: u64,
) -> bool {
    let grace_seconds = poll_interval_seconds.saturating_mul(3).max(900);
    let grace_i64 = i64::try_from(grace_seconds).unwrap_or(i64::MAX);
    let stale_after = Duration::seconds(grace_i64);
    Utc::now().signed_duration_since(lease.heartbeat_at) > stale_after
}

fn child_workflow_completed(
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
        RunStatus::Completed | RunStatus::Merged
    ))
}

fn determine_subissue_order(
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
        let source = state.artifact_root.join(artifact_name);
        let destination = state.artifact_root.join("subissue-order-plan.json");
        fs::copy(&source, &destination).map_err(|err| {
            parent_error(format!(
                "copy refreshed subissue order artifact from {} to {}: {err}",
                source.display(),
                destination.display()
            ))
        })?;
    }
    context.set("subissue_order", &json!(order).to_string());
    Ok(StepOutcome::Success)
}

fn select_next_child(
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

fn launch_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
) -> Result<StepOutcome, EngineError> {
    let conn = daemon_connection()?;
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
    match prepare_child_lease(state, child, &conn)? {
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

fn has_active_or_recoverable_child_lease(
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

fn is_observable_existing_pr(pr: &GithubIssuePrState) -> bool {
    !pr.merged
        && !pr.state.eq_ignore_ascii_case("closed")
        && !pr.state.eq_ignore_ascii_case("superseded")
}

fn observe_existing_child_pr(
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

fn wait_for_existing_child(
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

fn write_child_workflow_wait_artifact(
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

fn start_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    conn: &rusqlite::Connection,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    let request = child_launch_request(state, child);
    update_lease_status(
        conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some(&request.run_id),
    )
    .map_err(sql_error)?;
    let result = runner.launch_child(&request).map_err(|err| {
        if let Err(restore_err) =
            restore_child_lease_after_runner_error(lease, lease.status, lease.run_id.as_deref())
        {
            return parent_error(format!(
                "{err}; failed to restore child lease {}: {restore_err}",
                lease.lease_id
            ));
        }
        parent_error(err.to_string())
    })?;
    let (run_status, pr) = post_launch_metadata(state, query, runner, child, lease, &request)?;
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

fn resume_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    conn: &rusqlite::Connection,
) -> Result<StepOutcome, EngineError> {
    let Some(run_id) = lease.run_id.clone() else {
        update_lease_status(
            conn,
            &lease.lease_id,
            LeaseStatus::Failed,
            None,
        )
        .map_err(sql_error)?;
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
        return Ok(StepOutcome::Fixable);
    };
    let request = child_resume_request(state, child, run_id.clone());
    update_lease_status(
        conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some(&request.run_id),
    )
    .map_err(sql_error)?;
    let result = runner.resume_child(&request).map_err(|err| {
        if let Err(restore_err) = restore_child_lease_after_runner_error(lease, lease.status, Some(&run_id)) {
            return parent_error(format!(
                "{err}; failed to restore child lease {}: {restore_err}",
                lease.lease_id
            ));
        }
        parent_error(err.to_string())
    })?;
    let (run_status, pr) = post_launch_metadata(state, query, runner, child, lease, &request)?;
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

fn restore_child_lease_after_runner_error(
    lease: &crate::persistence::leases::IssueLease,
    status: LeaseStatus,
    run_id: Option<&str>,
) -> Result<(), EngineError> {
    update_lease_status(&daemon_connection()?, &lease.lease_id, status, run_id).map_err(sql_error)
}

fn post_launch_metadata(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    _lease: &crate::persistence::leases::IssueLease,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(Option<RunStatus>, Option<GithubIssuePrState>), EngineError> {
    let run_status = runner
        .run_status(&request.run_id)
        .map_err(|err| post_launch_metadata_error(err, "read child run status"))?;
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(|err| post_launch_metadata_error(github_error(err), "read child PR state"))?;
    Ok((run_status, pr))
}

fn post_launch_metadata_error(err: EngineError, action: &str) -> EngineError {
    parent_error(format!(
        "{action} after child launch failed; child lease remains Running for the launched run: {err}"
    ))
}

fn wait_for_child_merge(
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

fn record_observed_child_pr_merge_wait(
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
