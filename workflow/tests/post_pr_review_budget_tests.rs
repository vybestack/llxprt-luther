//! Cause-classified post-PR remediation budget coverage.
//!
//! Objective failures (failing tests, CI, lint, format, merge conflicts) are
//! binary and must be remediated until green, so they consume only the larger
//! iteration budget. Reviewer opinion is advisory and additionally consumes a
//! small review budget, so review churn cannot expand scope indefinitely.

use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::{
    ArtifactWriteContext, ClockSleeper, JsonArtifactWriteRequest, PostPrIterationGuardExecutor,
    PrFollowupArtifactStore, PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION,
};
use luther_workflow::engine::transition::StepOutcome;

const HEAD_SHAS: [&str; 8] = [
    "1111111111111111111111111111111111111111",
    "2222222222222222222222222222222222222222",
    "3333333333333333333333333333333333333333",
    "4444444444444444444444444444444444444444",
    "5555555555555555555555555555555555555555",
    "6666666666666666666666666666666666666666",
    "7777777777777777777777777777777777777777",
    "8888888888888888888888888888888888888888",
];

struct FixedClock;

impl ClockSleeper for FixedClock {
    fn now_rfc3339(&self) -> String {
        "2026-07-24T00:00:00Z".to_string()
    }

    fn sleep(&self, _duration: std::time::Duration) {}
}

fn binding_for(head_sha: &str) -> PrFollowupBinding {
    PrFollowupBinding {
        run_id: "run-budget".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 1910,
        head_ref: "feature".to_string(),
        head_sha: head_sha.to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
    }
}

fn context_for(temp: &tempfile::TempDir) -> StepContext {
    let mut context = StepContext::new(temp.path().to_path_buf(), "run-budget".to_string());
    context.set("repository_owner", "example");
    context.set("repository_name", "workflow");
    context.set("pr_number", "1910");
    context.set("head_ref", "feature");
    context.set("base_ref", "main");
    context.set("base_sha", "base-a");
    context
}

fn params_for(temp: &tempfile::TempDir, head_sha: &str) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": head_sha,
        "base_ref": "main",
        "base_sha": "base-a",
        "step_order_index": 2,
        "max_post_pr_remediation_iterations": 6,
        "max_review_remediation_iterations": 2
    })
}

/// Seeds the identity artifact the guard binds against, then records whether
/// the completed round observed concrete CI failures.
fn seed_round(temp: &tempfile::TempDir, head_sha: &str, ci_failures: Option<usize>) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = binding_for(head_sha);
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(&binding, "pr", "capture_pr_identity", 1, &FixedClock),
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "capture_state": "captured",
                "captured_at": "2026-07-24T00:00:00Z",
                "source": "fixture",
                "source_pr_node_id": "PR_kwDOExample",
                "source_head_repository_owner": null,
                "source_head_repository_name": null
            }),
            None,
        ))
        .expect("write pr identity");
    let Some(failure_count) = ci_failures else {
        return;
    };
    let failures: Vec<serde_json::Value> = (0..failure_count)
        .map(|index| {
            serde_json::json!({
                "name": format!("check-{index}"),
                "conclusion": "failure",
                "details_url": "https://github.com/example/workflow/runs/1"
            })
        })
        .collect();
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &binding,
                "ci-failures",
                "collect_ci_failures",
                4,
                &FixedClock,
            ),
            &serde_json::json!({
                "collection_state": "collected",
                "failures": failures,
                "fatal_source": null,
                "watcher_fatal_source": null
            }),
            None,
        ))
        .expect("write ci failures");
}

fn run_guard(temp: &tempfile::TempDir, head_sha: &str) -> StepOutcome {
    let mut context = context_for(temp);
    PostPrIterationGuardExecutor
        .execute(&mut context, &params_for(temp, head_sha))
        .expect("guard executes")
}

fn guard_artifact(temp: &tempfile::TempDir, head_sha: &str) -> serde_json::Value {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    store
        .read_optional_current_json_for_head(&binding_for(head_sha), "post-pr-iteration-guard")
        .expect("read guard artifact")
        .unwrap_or_else(|| panic!("guard artifact missing for {head_sha}"))
}

/// Objective CI failures must not consume the advisory review budget: a run
/// that keeps fixing genuinely failing checks must be allowed to reach green.
#[test]
fn objective_ci_failure_rounds_do_not_consume_the_review_budget() {
    let temp = tempfile::tempdir().expect("tempdir");
    for head_sha in HEAD_SHAS.iter().take(4) {
        seed_round(&temp, head_sha, Some(2));
        let outcome = run_guard(&temp, head_sha);
        assert_eq!(
            outcome,
            StepOutcome::Success,
            "objective failure rounds must keep proceeding while under the objective budget"
        );
        let artifact = guard_artifact(&temp, head_sha);
        assert_eq!(
            artifact
                .get("review_iteration_index")
                .and_then(serde_json::Value::as_u64),
            Some(0),
            "rounds driven by concrete CI failures must never consume the review budget"
        );
    }
}

/// Review-driven rounds are advisory and must stop after the configured cap.
#[test]
fn review_driven_rounds_exhaust_the_review_budget_and_route_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut outcomes = Vec::new();
    for head_sha in HEAD_SHAS.iter().take(4) {
        seed_round(&temp, head_sha, Some(0));
        outcomes.push(run_guard(&temp, head_sha));
    }
    assert_eq!(
        outcomes[0],
        StepOutcome::Success,
        "the initial entry must proceed"
    );
    assert_eq!(
        outcomes[3],
        StepOutcome::Fatal,
        "a third review-driven remediation round must exhaust the review budget"
    );
    let artifact = guard_artifact(&temp, HEAD_SHAS[3]);
    assert_eq!(
        artifact.get("reason").and_then(serde_json::Value::as_str),
        Some("max_review_iterations_exceeded"),
        "exhaustion must be attributed to the review budget, not the objective budget"
    );
    assert!(
        artifact
            .get("iteration_index")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|index| index <= 6),
        "the objective budget must not be the cause of this terminal"
    );
}

/// A missing `ci-failures` artifact must be treated as objective. Failing open
/// would let unreadable state silently drain the review budget and strand a
/// pull request that still has failing checks.
#[test]
fn missing_ci_failures_artifact_is_classified_as_objective() {
    let temp = tempfile::tempdir().expect("tempdir");
    for head_sha in HEAD_SHAS.iter().take(4) {
        seed_round(&temp, head_sha, None);
        run_guard(&temp, head_sha);
    }
    let artifact = guard_artifact(&temp, HEAD_SHAS[3]);
    assert_eq!(
        artifact
            .get("review_iteration_index")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "unreadable failure state must never be charged to the review budget"
    );
}

/// Re-entering the guard on an unchanged head is a retry, not a new
/// remediation round, so it must not consume either budget.
#[test]
fn same_head_reentry_does_not_consume_the_review_budget() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed_round(&temp, HEAD_SHAS[0], Some(0));
    run_guard(&temp, HEAD_SHAS[0]);
    seed_round(&temp, HEAD_SHAS[1], Some(0));
    run_guard(&temp, HEAD_SHAS[1]);
    let after_first = guard_artifact(&temp, HEAD_SHAS[1])
        .get("review_iteration_index")
        .and_then(serde_json::Value::as_u64);

    for _ in 0..3 {
        run_guard(&temp, HEAD_SHAS[1]);
    }
    let after_reentry = guard_artifact(&temp, HEAD_SHAS[1])
        .get("review_iteration_index")
        .and_then(serde_json::Value::as_u64);

    assert_eq!(
        after_first, after_reentry,
        "repeated same-head activations must not consume review budget"
    );
}
