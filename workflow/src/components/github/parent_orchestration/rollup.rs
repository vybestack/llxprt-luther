use super::*;

/// Persisted rollup of every child issue tracked by the parent orchestrator.
///
/// The rollup is written to `parent-orchestration-rollup.json` in the parent
/// artifact root and records the latest known outcome for each child so the
/// orchestrator and downstream consumers can reason about overall progress
/// without re-querying GitHub.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct ParentOrchestrationRollup {
    pub(super) parent_issue_number: u64,
    pub(super) children: Vec<ChildRollupEntry>,
}

/// A single child issue's latest outcome within a [`ParentOrchestrationRollup`].
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct ChildRollupEntry {
    pub(super) child_issue_number: u64,
    pub(super) child_run_id: Option<String>,
    pub(super) child_artifact_dir: Option<String>,
    pub(super) pr_number: Option<u64>,
    pub(super) pr_state: Option<String>,
    pub(super) merge_sha: Option<String>,
    pub(super) outcome: Option<String>,
    pub(super) non_actionable_reason: Option<String>,
}

/// Record (or replace) a child's outcome in the persisted rollup.
///
/// Each call rewrites `parent-orchestration-rollup.json`, replacing any prior
/// entry for `child` with the supplied outcome and keeping children sorted by
/// issue number for deterministic output.
pub fn update_rollup(
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

/// Human-readable explanation for known non-actionable child outcomes.
///
/// Non-actionable outcomes (a child explicitly marked as non-actionable, or a
/// terminal lease outside the orchestrator) are annotated in the rollup so the
/// completion evaluator can present why a child requires no further action.
pub fn non_actionable_reason_for_outcome(outcome: &str) -> Option<String> {
    match outcome {
        "non_actionable_child" => Some("child issue is explicitly non-actionable".to_string()),
        "non_actionable_child_lease" => {
            Some("child lease is already terminal outside the parent orchestrator".to_string())
        }
        _ => None,
    }
}

/// Return whether the rollup already records `outcome` for `child`.
///
/// Used to guard idempotency-sensitive side effects (such as posting a
/// "ready for human merge" comment) so they are not duplicated across
/// orchestration passes.
pub fn rollup_has_outcome(
    state: &OrchestrationState,
    child: u64,
    outcome: &str,
) -> Result<bool, EngineError> {
    let rollup = read_rollup(&state.artifact_root)?;
    Ok(rollup.children.iter().any(|entry| {
        entry.child_issue_number == child && entry.outcome.as_deref() == Some(outcome)
    }))
}

/// Read the persisted rollup, returning an empty default when absent.
pub fn read_rollup(artifact_root: &Path) -> Result<ParentOrchestrationRollup, EngineError> {
    let path = artifact_root.join("parent-orchestration-rollup.json");

    if path.exists() {
        read_json(&path)
    } else {
        Ok(ParentOrchestrationRollup::default())
    }
}
