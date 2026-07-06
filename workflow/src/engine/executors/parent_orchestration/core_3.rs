fn validated_child_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("unsafe child workflow {label} '{value}'"));
    }
    Ok(value)
}

fn apply_child_overrides(
    config: &mut WorkflowConfig,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: request.work_dir.clone(),
        artifact_dir: request.artifact_dir.clone(),
    };
    apply_target_profile_overrides(config, &overrides)
        .map_err(|err| format!("apply child target overrides: {err}"))
}

fn prepare_child_resume(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let metadata = get_run_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing child run metadata for {}", request.run_id))?;
    let step = metadata
        .current_step
        .as_deref()
        .filter(|step| !step.is_empty())
        .ok_or_else(|| format!("missing current_step for child resume {}", request.run_id))?;
    crate::engine::commit_continuation(
        &conn,
        &crate::engine::ContinuationRequest {
            run_id: request.run_id.clone(),
            kind: crate::engine::ContinuationKind::Resume,
            force: true,
        },
        step,
    )
    .map(|_| ())
    .map_err(|err| format!("commit child resume: {err}"))
}

fn child_run_context(config: &WorkflowConfig, request: &ChildWorkflowLaunchRequest) -> RunContext {
    RunContext {
        log_path: None,
        artifact_root: request
            .artifact_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("artifact_dir").cloned()),
        workspace_path: request
            .work_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("work_dir").cloned()),
        repository: Some(request.repo.clone()),
        issue_number: i64::try_from(request.issue_number).ok(),
        pr_number: None,
        head_sha: None,
    }
}

fn child_result_from_run_outcome(
    outcome: RunOutcome,
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    match outcome {
        RunOutcome::Success => Ok(ChildWorkflowRunResult::CompletedSuccess),
        RunOutcome::WaitingExternal { step_id, reason } => {
            persist_child_external_wait_state(request, config, db_path, &step_id, &reason)?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Interrupted { step_id } => {
            persist_child_interrupted_state(request, config, db_path, &step_id)?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Failure { .. } | RunOutcome::Abandoned { .. } => {
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
    }
}

fn persist_child_interrupted_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            format!(
                "missing child interrupted checkpoint for {}",
                request.run_id
            )
        })?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id)
        .map_err(|err| err.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = child_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.wait_kind = child_wait_kind_for_step(step_id);
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": "child_workflow_interrupted",
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "interrupted",
        "step_id": step_id,
        "reason": "child_workflow_interrupted"
    });
    record.poll_interval_seconds = child_wait_poll_interval(config);
    record.max_wait_seconds = None;
    record.next_poll_at = crate::polling::next_poll_time(record.poll_interval_seconds);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = crate::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|err| err.to_string())?;
    write_wait_state_artifact(&request.run_id, &record)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn persist_child_external_wait_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
    reason: &str,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing child waiting checkpoint for {}", request.run_id))?;
    let metadata = get_run_with_conn(&conn, &request.run_id).map_err(|err| err.to_string())?;
    let wait_kind = child_wait_kind_for_step(step_id);
    let identity = child_wait_poll_identity(metadata.as_ref(), wait_kind)?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id)
        .map_err(|err| err.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = child_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.pr_number = identity.pr_number;
    record.head_sha = identity.head_sha;
    record.wait_kind = wait_kind;
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "suspended",
        "step_id": step_id,
        "reason": reason
    });
    record.poll_interval_seconds = child_wait_poll_interval(config);
    record.max_wait_seconds = None;
    record.next_poll_at = crate::polling::next_poll_time(record.poll_interval_seconds);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = crate::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|err| err.to_string())?;
    write_wait_state_artifact(&request.run_id, &record)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn child_wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        "launch_or_resume_child_workflow" | "dependency_child_workflow" => {
            WaitKind::DependencyChildWorkflow
        }
        "wait_for_child_merge" | "dependency_child_merge" => WaitKind::DependencyChildMerge,
        "rate_limit_backoff" | "github_rate_limit_backoff" => WaitKind::RateLimitBackoff,
        _ => WaitKind::HumanReview,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildWaitIdentity {
    pr_number: Option<u64>,
    head_sha: Option<String>,
}

fn child_wait_poll_identity(
    metadata: Option<&RunMetadata>,
    wait_kind: WaitKind,
) -> Result<ChildWaitIdentity, String> {
    let identity = ChildWaitIdentity {
        pr_number: metadata
            .and_then(|md| md.pr_number)
            .and_then(|number| u64::try_from(number).ok()),
        head_sha: metadata.and_then(|md| md.head_sha.clone()),
    };
    match wait_kind {
        WaitKind::PrChecks if identity.pr_number.is_none() || identity.head_sha.is_none() => {
            Err("missing child PR number or head SHA for PR checks wait state".to_string())
        }
        WaitKind::CoderabbitReview
        | WaitKind::HumanReview
        | WaitKind::PrMerge
        | WaitKind::DependencyChildMerge
            if identity.pr_number.is_none() =>
        {
            Err(format!(
                "missing child PR number for {wait_kind} wait state"
            ))
        }
        _ => Ok(identity),
    }
}

fn child_wait_poll_interval(config: &WorkflowConfig) -> u64 {
    config
        .discovery
        .as_ref()
        .and_then(|discovery| discovery.poll_interval_secs)
        .unwrap_or(300)
}

fn child_lease_id(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
) -> Result<Option<String>, String> {
    get_lease_for_issue(conn, &request.repo, request.issue_number)
        .map(|lease| lease.map(|lease| lease.lease_id))
        .map_err(|err| err.to_string())
}

fn sql_error(err: rusqlite::Error) -> EngineError {
    parent_error(format!("lease database error: {err}"))
}

fn evaluate_parent_completion(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    refresh_parent_completion_evidence(state, query)?;
    let states: Vec<ChildIssueState> =
        read_json(&state.artifact_root.join("subissue-state-snapshot.json"))?;
    let rollup = read_rollup(&state.artifact_root)?;
    let parent: GithubIssue = read_json(&state.artifact_root.join("parent-issue.json"))?;
    let active_children = incomplete_child_numbers(&states, &rollup);
    let blocked_children = blocked_child_numbers(&states);
    let active_runs = active_child_leases(state, &states)?;
    let merged_pr_children: Vec<_> = rollup
        .children
        .iter()
        .filter(|child| child.outcome.as_deref() == Some("merged"))
        .cloned()
        .collect();
    let closed_without_completion_evidence: Vec<u64> = states
        .iter()
        .filter(|child| child.terminal_state == ChildTerminalState::Closed)
        .map(|child| child.issue_number)
        .collect();
    let acceptance = evaluate_acceptance_criteria(parent.body.as_deref(), &states, &rollup);
    let child_completion_evidence = child_completion_evidence(&states, &rollup);
    let native_subissues_closed_or_non_actionable = active_children.is_empty();
    let required_prs_merged_or_superseded = required_prs_satisfied(&states, &rollup);
    let no_active_child_runs = active_runs.is_empty();
    let complete = native_subissues_closed_or_non_actionable
        && required_prs_merged_or_superseded
        && no_active_child_runs
        && acceptance.satisfied
        && blocked_children.is_empty();
    write_json(
        &state.artifact_root,
        "parent-completion-evaluation.json",
        &json!({
            "complete": complete,
            "native_subissues_closed_or_non_actionable": native_subissues_closed_or_non_actionable,
            "required_child_prs_merged_or_superseded": required_prs_merged_or_superseded,
            "no_active_child_workflow_runs": no_active_child_runs,
            "parent_acceptance_criteria_satisfied": acceptance.satisfied,
            "no_parent_followup_remaining": acceptance.remaining_work.is_empty(),
            "acceptance_criteria_satisfied": acceptance.satisfied,
            "acceptance_criteria_evidence": acceptance.evidence,
            "active_child_issues": active_children,
            "blocked_child_issues": blocked_children,
            "active_child_runs": active_runs,
            "merged_child_prs": merged_pr_children,
            "closed_without_completion_evidence": closed_without_completion_evidence,
            "child_completion_evidence": child_completion_evidence,
            "children": states,
            "rollup": rollup,
            "remaining_work": acceptance.remaining_work
        }),
    )?;
    context.set("parent_complete", if complete { "true" } else { "false" });
    Ok(if complete {
        StepOutcome::Success
    } else {
        StepOutcome::Fixable
    })
}

fn incomplete_child_numbers(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<u64> {
    states
        .iter()
        .filter(|child| !child_completion_satisfied(child, rollup))
        .map(|child| child.issue_number)
        .collect()
}

fn blocked_child_numbers(states: &[ChildIssueState]) -> Vec<u64> {
    states
        .iter()
        .filter(|child| child_is_blocked(child))
        .map(|child| child.issue_number)
        .collect()
}

fn required_prs_satisfied(states: &[ChildIssueState], rollup: &ParentOrchestrationRollup) -> bool {
    !states.is_empty()
        && states.iter().all(|child| match child.terminal_state {
            ChildTerminalState::Merged => true,
            ChildTerminalState::Closed => child_has_explicit_non_actionable_reason(child, rollup),
            _ => false,
        })
        && !rollup
            .children
            .iter()
            .any(unresolved_rollup_outcome_requires_pr)
}

fn child_completion_satisfied(child: &ChildIssueState, rollup: &ParentOrchestrationRollup) -> bool {
    child_is_complete(child) || child_has_explicit_non_actionable_reason(child, rollup)
}

fn child_has_explicit_non_actionable_reason(
    child: &ChildIssueState,
    rollup: &ParentOrchestrationRollup,
) -> bool {
    child.terminal_state == ChildTerminalState::Closed
        && rollup.children.iter().any(|entry| {
            entry.child_issue_number == child.issue_number
                && matches!(
                    entry.outcome.as_deref(),
                    Some("non_actionable_child" | "non_actionable_child_lease")
                )
                && entry
                    .non_actionable_reason
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty())
        })
}

fn child_completion_evidence(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<Value> {
    states
        .iter()
        .map(|child| {
            let rollup_entry = rollup
                .children
                .iter()
                .find(|entry| entry.child_issue_number == child.issue_number);
            json!({
                "child_issue_number": child.issue_number,
                "terminal_state": child.terminal_state,
                "pr_number": child.pr_number,
                "completion_satisfied": child_completion_satisfied(child, rollup),
                "non_actionable_reason": rollup_entry.and_then(|entry| entry.non_actionable_reason.clone()),
                "merge_sha": rollup_entry.and_then(|entry| entry.merge_sha.clone()),
                "child_artifact_dir": rollup_entry.and_then(|entry| entry.child_artifact_dir.clone())
            })
        })
        .collect()
}

fn unresolved_rollup_outcome_requires_pr(child: &ChildRollupEntry) -> bool {
    matches!(
        child.outcome.as_deref(),
        Some(
            "missing_child_pr"
                | "superseded_child_pr"
                | "closed_unmerged_pr"
                | "stale_child_run"
                | "failed_child_run"
                | "active_child_lease"
                | "completed_failure",
        )
    )
}

struct AcceptanceEvaluation {
    satisfied: bool,
    evidence: Vec<String>,
    remaining_work: Vec<String>,
}

fn evaluate_acceptance_criteria(
    parent_body: Option<&str>,
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> AcceptanceEvaluation {
    let mut evidence = Vec::new();
    let mut remaining_work = Vec::new();
    evidence.push(format!("{} child issue(s) classified", states.len()));
    evidence.push(format!(
        "{} child rollup entry(s) recorded",
        rollup.children.len()
    ));
    let criteria = parent_body.map_or(0, count_acceptance_criteria);
    if criteria == 0 {
        remaining_work.push(
            "parent acceptance criteria require deterministic verification; no explicit checked acceptance checklist was found"
                .to_string(),
        );
    } else {
        let unchecked = parent_body.map_or(0, count_unchecked_acceptance_criteria);
        evidence.push(format!(
            "{criteria} parent acceptance checklist item(s) found; {unchecked} unchecked"
        ));
        if unchecked > 0 {
            remaining_work.push(format!(
                "{unchecked} parent acceptance checklist item(s) remain unchecked"
            ));
        }
    }
    if states
        .iter()
        .any(|child| !child_completion_satisfied(child, rollup))
    {
        remaining_work.push("one or more child issues are not complete".to_string());
    }
    for child in states {
        match child.terminal_state {
            ChildTerminalState::Closed if child_has_explicit_non_actionable_reason(child, rollup) => {
                evidence.push(format!(
                    "child issue #{} is closed with explicit non-actionable evidence",
                    child.issue_number
                ));
            }
            ChildTerminalState::Closed => remaining_work.push(format!(
                "child issue #{} is closed without merged PR evidence or an explicit non-actionable reason",
                child.issue_number
            )),
            ChildTerminalState::Merged => evidence.push(format!(
                "child issue #{} is closed with merged PR evidence",
                child.issue_number
            )),
            ChildTerminalState::MergedIssueOpen => remaining_work.push(format!(
                "child issue #{} has merged PR evidence but is still open",
                child.issue_number
            )),
            _ => remaining_work.push(format!(
                "child issue #{} lacks terminal completion evidence",
                child.issue_number
            )),
        }
    }
    if rollup
        .children
        .iter()
        .any(unresolved_rollup_outcome_requires_pr)
    {
        remaining_work.push("one or more child runs lack merged PR evidence".to_string());
    }
    AcceptanceEvaluation {
        satisfied: remaining_work.is_empty(),
        evidence,
        remaining_work,
    }
}

fn count_unchecked_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]")
        })
        .count()
}

fn refresh_parent_completion_evidence(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<(), EngineError> {
    let issue = query
        .get_issue(&state.repo, state.parent_issue_number)
        .map_err(github_error)?
        .ok_or_else(|| parent_error("parent issue could not be loaded".to_string()))?;
    write_json(&state.artifact_root, "parent-issue.json", &issue)
}

fn count_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- [x]")
                || trimmed.starts_with("- [X]")
                || trimmed.starts_with("- [ ]")
                || trimmed.starts_with("* [x]")
                || trimmed.starts_with("* [X]")
                || trimmed.starts_with("* [ ]")
        })
        .count()
}

fn active_child_leases(
    state: &OrchestrationState,
    children: &[ChildIssueState],
) -> Result<Vec<Value>, EngineError> {
    let conn = daemon_connection()?;
    let mut active = Vec::new();
    for child in children {
        let lease =
            get_lease_for_issue(&conn, &state.repo, child.issue_number).map_err(sql_error)?;
        if let Some(lease) = lease.filter(active_child_lease_blocks_parent) {
            active.push(json!({
                "issue_number": child.issue_number,
                "run_id": lease.run_id,
                "status": lease.status.to_string()
            }));
        }
    }
    Ok(active)
}

fn active_child_lease_blocks_parent(lease: &crate::persistence::leases::IssueLease) -> bool {
    matches!(
        lease.status,
        LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Claimed
            | LeaseStatus::Running
    )
}

fn close_or_report_parent(
    _context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    refresh_parent_completion_evidence(state, query)?;
    let evaluation: Value = read_json(
        &state
            .artifact_root
            .join("parent-completion-evaluation.json"),
    )?;
    let complete = evaluation
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body = parent_summary_comment(complete, &evaluation);
    query
        .comment_issue(&state.repo, state.parent_issue_number, &body)
        .map_err(github_error)?;
    if complete {
        query
            .close_issue(&state.repo, state.parent_issue_number)
            .map_err(github_error)?;
    }
    if complete || evaluation_reports_terminal_blocker(&evaluation) {
        query
            .remove_label(&state.repo, state.parent_issue_number, &state.luther_label)
            .map_err(github_error)?;
    }
    write_json(
        &state.artifact_root,
        "parent-close-result.json",
        &json!({
            "closed": complete,
            "commented": true,
            "parent_issue_number": state.parent_issue_number
        }),
    )?;
    Ok(StepOutcome::Success)
}

fn evaluation_reports_terminal_blocker(evaluation: &Value) -> bool {
    evaluation
        .get("blocked_child_issues")
        .and_then(Value::as_array)
        .is_some_and(|blocked| !blocked.is_empty())
}

fn required_context(context: &StepContext, key: &str) -> Result<String, EngineError> {
    context
        .get(key)
        .cloned()
        .ok_or_else(|| parent_error(format!("missing context value '{key}'")))
}

fn parent_issue_number(context: &StepContext) -> Result<u64, EngineError> {
    context
        .get("primary_issue_number")
        .or_else(|| context.get("issue_number"))
        .ok_or_else(|| parent_error("missing context value 'primary_issue_number'".to_string()))?
        .parse::<u64>()
        .map_err(|err| parent_error(format!("invalid numeric parent issue context value: {err}")))
}

fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let template = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .or_else(|| context.get("artifact_root").map(String::as_str))
        .or_else(|| context.get("artifact_dir").map(String::as_str))
        .unwrap_or("{work_dir}/.luther-parent-orchestration");
    let interpolated = interpolate_string(template, context);
    if interpolated.contains('{') {
        return Err(parent_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    Ok(PathBuf::from(interpolated))
}

fn write_json<T: serde::Serialize>(
    artifact_root: &Path,
    name: &str,
    value: &T,
) -> Result<(), EngineError> {
    fs::create_dir_all(artifact_root)
        .map_err(|err| parent_error(format!("create artifact root: {err}")))?;
    let path = artifact_root.join(name);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| parent_error(format!("serialize {name}: {err}")))?;
    let write_id = ARTIFACT_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_extension(format!("{}.{}.tmp", std::process::id(), write_id));
    fs::write(&temp_path, bytes)
        .map_err(|err| parent_error(format!("write {}: {err}", temp_path.display())))?;
    fs::rename(&temp_path, &path).map_err(|err| {
        parent_error(format!(
            "rename {} to {}: {err}",
            temp_path.display(),
            path.display()
        ))
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EngineError> {
    let bytes =
        fs::read(path).map_err(|err| parent_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| parent_error(format!("parse {}: {err}", path.display())))
}

fn clear_selected_child(artifact_root: &Path) -> Result<(), EngineError> {
    let path = artifact_root.join("selected-child.json");
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|err| parent_error(format!("remove {}: {err}", path.display())))?;
    }
    Ok(())
}

fn read_children(
    artifact_root: &Path,
) -> Result<Vec<crate::adapters::github_issues::GithubSubIssue>, EngineError> {
    read_json(&artifact_root.join("parent-subissues.json"))
}

fn selected_child(artifact_root: &Path) -> Result<Option<u64>, EngineError> {
    let selected: Value = read_json(&artifact_root.join("selected-child.json"))?;
    Ok(selected.get("issue_number").and_then(Value::as_u64))
}


fn github_error(err: GithubError) -> EngineError {
    parent_error(err.to_string())
}

fn parent_error(message: String) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "parent_orchestration".to_string(),
        message,
    }
}
