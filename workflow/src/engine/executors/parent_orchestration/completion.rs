use super::*;
use std::collections::HashMap;

/// Lookup of child rollup entries keyed by child issue number.
///
/// Built once per completion-evaluation pass so the per-child helpers below do
/// not each re-scan `rollup.children`. The previous `.iter().find(..)` /
/// `.any(..)` scans were invoked once per child from callers that also iterate
/// every child (for example `evaluate_acceptance_criteria` and
/// `required_prs_satisfied`), producing avoidable O(N*M) behavior.
struct ChildRollupIndex<'a> {
    by_child: HashMap<u64, &'a ChildRollupEntry>,
}

impl<'a> ChildRollupIndex<'a> {
    fn new(rollup: &'a ParentOrchestrationRollup) -> Self {
        let mut by_child = HashMap::with_capacity(rollup.children.len());
        for entry in &rollup.children {
            // Preserve the first-match semantics of the previous linear scans if
            // duplicate child issue numbers ever appear in a rollup.
            by_child.entry(entry.child_issue_number).or_insert(entry);
        }
        Self { by_child }
    }

    fn get(&self, child_issue_number: u64) -> Option<&'a ChildRollupEntry> {
        self.by_child.get(&child_issue_number).copied()
    }
}

pub fn evaluate_parent_completion(
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
        .filter(|child| child.terminal_state == ChildIssueStatus::Closed)
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

pub fn incomplete_child_numbers(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<u64> {
    let index = ChildRollupIndex::new(rollup);
    states
        .iter()
        .filter(|child| !child_completion_satisfied_indexed(child, &index))
        .map(|child| child.issue_number)
        .collect()
}

pub fn blocked_child_numbers(states: &[ChildIssueState]) -> Vec<u64> {
    states
        .iter()
        .filter(|child| child_is_blocked(child))
        .map(|child| child.issue_number)
        .collect()
}

pub fn required_prs_satisfied(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> bool {
    // An empty required-PR state set is vacuously satisfied: a parent with no
    // child issues has no required PRs to evaluate and must not be blocked by
    // this gate (mirrors the `active_children.is_empty()` completion check).
    let index = ChildRollupIndex::new(rollup);
    states.iter().all(|child| match child.terminal_state {
        ChildIssueStatus::Merged => true,
        ChildIssueStatus::Closed => child_has_explicit_non_actionable_reason_indexed(child, &index),
        _ => false,
    }) && !rollup
        .children
        .iter()
        .any(unresolved_rollup_outcome_requires_pr)
}

fn child_completion_satisfied_indexed(child: &ChildIssueState, index: &ChildRollupIndex) -> bool {
    child_is_complete(child) || child_has_explicit_non_actionable_reason_indexed(child, index)
}

fn child_has_explicit_non_actionable_reason_indexed(
    child: &ChildIssueState,
    index: &ChildRollupIndex,
) -> bool {
    child.terminal_state == ChildIssueStatus::Closed
        && index.get(child.issue_number).is_some_and(|entry| {
            matches!(
                entry.outcome.as_deref(),
                Some("non_actionable_child" | "non_actionable_child_lease")
            ) && entry
                .non_actionable_reason
                .as_deref()
                .is_some_and(|reason| !reason.trim().is_empty())
        })
}

pub fn child_completion_evidence(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<Value> {
    let index = ChildRollupIndex::new(rollup);
    states
        .iter()
        .map(|child| {
            let rollup_entry = index.get(child.issue_number);
            json!({
                "child_issue_number": child.issue_number,
                "terminal_state": child.terminal_state,
                "pr_number": child.pr_number,
                "completion_satisfied": child_completion_satisfied_indexed(child, &index),
                "non_actionable_reason": rollup_entry.and_then(|entry| entry.non_actionable_reason.clone()),
                "merge_sha": rollup_entry.and_then(|entry| entry.merge_sha.clone()),
                "child_artifact_dir": rollup_entry.and_then(|entry| entry.child_artifact_dir.clone())
            })
        })
        .collect()
}

pub fn unresolved_rollup_outcome_requires_pr(child: &ChildRollupEntry) -> bool {
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

pub struct AcceptanceEvaluation {
    pub satisfied: bool,
    pub evidence: Vec<String>,
    pub remaining_work: Vec<String>,
}

pub fn evaluate_acceptance_criteria(
    parent_body: Option<&str>,
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> AcceptanceEvaluation {
    let index = ChildRollupIndex::new(rollup);
    let mut evidence = Vec::new();
    let mut remaining_work = Vec::new();
    evidence.push(format!("{} child issue(s) classified", states.len()));
    evidence.push(format!(
        "{} child rollup entry(s) recorded",
        rollup.children.len()
    ));
    let criteria = parent_body.map_or(0, count_acceptance_criteria);
    if criteria == 0 {
        evidence.push(
            "no parent acceptance checklist found; relying on child completion evidence"
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
        .any(|child| !child_completion_satisfied_indexed(child, &index))
    {
        remaining_work.push("one or more child issues are not complete".to_string());
    }
    for child in states {
        match child.terminal_state {
            ChildIssueStatus::Closed
                if child_has_explicit_non_actionable_reason_indexed(child, &index) =>
            {
                evidence.push(format!(
                    "child issue #{} is closed with explicit non-actionable evidence",
                    child.issue_number
                ));
            }
            ChildIssueStatus::Closed => remaining_work.push(format!(
                "child issue #{} is closed without merged PR evidence or an explicit non-actionable reason",
                child.issue_number
            )),
            ChildIssueStatus::Merged => evidence.push(format!(
                "child issue #{} is closed with merged PR evidence",
                child.issue_number
            )),
            ChildIssueStatus::MergedIssueOpen => remaining_work.push(format!(
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

pub fn count_unchecked_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| checklist_marker(line.trim_start(), &["[ ]"]))
        .count()
}

pub fn refresh_parent_completion_evidence(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<(), EngineError> {
    let issue = query
        .get_issue(&state.repo, state.parent_issue_number)
        .map_err(github_error)?
        .ok_or_else(|| parent_error("parent issue could not be loaded".to_string()))?;
    write_json(&state.artifact_root, "parent-issue.json", &issue)
}

pub fn count_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| checklist_marker(line.trim_start(), &["[x]", "[X]", "[ ]"]))
        .count()
}

pub fn checklist_marker(trimmed: &str, markers: &[&str]) -> bool {
    // Match a `<bullet> <marker>` prefix without allocating a new String per
    // marker: strip the bullet and the single separating space, then test the
    // remaining slice against each marker directly.
    ['-', '*', '+'].into_iter().any(|bullet| {
        trimmed
            .strip_prefix(bullet)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|rest| markers.iter().any(|marker| rest.starts_with(marker)))
    })
}

pub fn active_child_leases(
    state: &OrchestrationState,
    children: &[ChildIssueState],
) -> Result<Vec<Value>, EngineError> {
    let conn = daemon_connection()?;
    let child_numbers: Vec<u64> = children.iter().map(|child| child.issue_number).collect();
    // Fetch every child lease in a single batched query rather than issuing one
    // database round trip per child.
    let leases = get_leases_for_issues(&conn, &state.repo, &child_numbers).map_err(sql_error)?;
    let mut active = Vec::new();
    for child in children {
        if let Some(lease) = leases
            .get(&child.issue_number)
            .filter(|lease| active_child_lease_blocks_parent(lease))
        {
            active.push(json!({
                "issue_number": child.issue_number,
                "run_id": lease.run_id,
                "status": lease.status.to_string()
            }));
        }
    }
    Ok(active)
}

pub fn active_child_lease_blocks_parent(lease: &crate::persistence::leases::IssueLease) -> bool {
    matches!(
        lease.status,
        LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Claimed
            | LeaseStatus::Running
    )
}

pub fn close_or_report_parent(
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

pub fn evaluation_reports_terminal_blocker(evaluation: &Value) -> bool {
    evaluation
        .get("blocked_child_issues")
        .and_then(Value::as_array)
        .is_some_and(|blocked| !blocked.is_empty())
}
