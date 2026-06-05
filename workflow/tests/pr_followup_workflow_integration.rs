/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A
/// @pseudocode lines 1-53
/// Fake PR follow-through workflow integration tests written before P17 TOML wiring.
use std::collections::BTreeSet;

use luther_workflow::engine::runner::RunOutcome;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeRunOutcome {
    Success,
    Failure,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[derive(Debug, Default)]
struct FakePostPrTrace {
    steps: Vec<&'static str>,
    artifacts: BTreeSet<&'static str>,
    heads_seen: Vec<&'static str>,
    comments_posted: Vec<&'static str>,
    comments_resolved: Vec<&'static str>,
    marker_actions: Vec<&'static str>,
    outcome: Option<FakeRunOutcome>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
impl FakePostPrTrace {
    fn step(&mut self, step: &'static str) {
        self.steps.push(step);
    }

    fn artifact(&mut self, artifact: &'static str) {
        self.artifacts.insert(artifact);
    }

    fn head(&mut self, head: &'static str) {
        self.heads_seen.push(head);
    }

    fn comment(&mut self, item: &'static str) {
        self.comments_posted.push(item);
    }

    fn resolve(&mut self, item: &'static str) {
        self.comments_resolved.push(item);
    }

    fn marker(&mut self, action: &'static str) {
        self.marker_actions.push(action);
    }

    const fn finish_success(&mut self) {
        self.outcome = Some(FakeRunOutcome::Success);
    }

    const fn finish_failure(&mut self) {
        self.outcome = Some(FakeRunOutcome::Failure);
    }

    fn assert_path(&self, expected: &[&str]) {
        assert_eq!(self.steps, expected, "fake E2E path mismatch");
    }

    fn assert_artifacts(&self, expected: &[&str]) {
        let expected: BTreeSet<_> = expected.iter().copied().collect();
        assert_eq!(self.artifacts, expected, "fake E2E artifacts mismatch");
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
fn clean_success() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.artifact("pr-identity.json");
    trace.step("post_pr_iteration_guard");
    trace.step("watch_pr_checks");
    trace.artifact("pr-checks.json");
    trace.step("collect_ci_failures");
    trace.artifact("ci-failures.json");
    trace.step("collect_coderabbit_feedback");
    trace.artifact("coderabbit-feedback.json");
    trace.step("evaluate_coderabbit_feedback");
    trace.artifact("feedback-evaluations.json");
    trace.step("build_remediation_plan");
    trace.artifact("remediation-plan.json");
    trace.step("mark_coderabbit_feedback");
    trace.marker("noop-clean");
    trace.artifact("marker-result.json");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-6,12-40,50-53
fn remediation_loop_from_ci_failure() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    for head in ["head-a", "head-b"] {
        trace.step("capture_pr_identity");
        trace.head(head);
        trace.artifact("pr-identity.json");
        trace.step("post_pr_iteration_guard");
        trace.step("watch_pr_checks");
        trace.artifact("pr-checks.json");
        trace.step("collect_ci_failures");
        trace.artifact("ci-failures.json");
        trace.step("collect_coderabbit_feedback");
        trace.artifact("coderabbit-feedback.json");
        trace.step("evaluate_coderabbit_feedback");
        trace.artifact("feedback-evaluations.json");
        trace.step("build_remediation_plan");
        trace.artifact("remediation-plan.json");
        if head == "head-a" {
            trace.step("remediate_pr_followup");
            trace.artifact("remediation-result.json");
            trace.step("validate_remediation_result");
            trace.step("run_post_pr_tests");
            trace.artifact("post-pr-test-result.json");
            trace.step("push_remediation_changes");
            trace.artifact("push-result.json");
        } else {
            trace.step("mark_coderabbit_feedback");
            trace.step("log_completion");
            trace.finish_success();
        }
    }
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-5,50-53
fn terminal_collection(status: &'static str) -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.step("post_pr_iteration_guard");
    trace.step("watch_pr_checks");
    trace.artifact("pr-checks.json");
    trace.step("collect_ci_failures");
    trace.artifact("ci-failures.json");
    trace.marker(status);
    trace.step("post_pr_failure_terminal");
    trace.finish_failure();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 6-11,12-40,41-49
fn coderabbit_valid_remediation() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.step("watch_pr_checks");
    trace.step("collect_ci_failures");
    trace.step("collect_coderabbit_feedback");
    trace.artifact("coderabbit-feedback.json");
    trace.step("evaluate_coderabbit_feedback");
    trace.artifact("feedback-evaluations.json");
    trace.step("build_remediation_plan");
    trace.artifact("remediation-plan.json");
    trace.step("remediate_pr_followup");
    trace.artifact("remediation-result.json");
    trace.step("validate_remediation_result");
    trace.resolve("cr-valid-1");
    trace.step("run_post_pr_tests");
    trace.step("push_remediation_changes");
    trace.step("capture_pr_identity");
    trace.head("head-b");
    trace.step("mark_coderabbit_feedback");
    trace.marker("resolved-valid-feedback");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 18-28,50-53
fn malformed_remediation_result() -> FakePostPrTrace {
    let mut trace = remediation_loop_from_ci_failure();
    trace.steps.truncate(8);
    trace.step("remediate_pr_followup");
    trace.artifact("remediation-result.json");
    trace.marker("malformed-non-empty-result-rejected");
    trace.step("validate_remediation_result");
    trace.step("post_pr_failure_terminal");
    trace.outcome = Some(FakeRunOutcome::Failure);
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 29-33,50-53
fn local_verification_failure_loop() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.step("watch_pr_checks");
    trace.step("collect_ci_failures");
    trace.step("collect_coderabbit_feedback");
    trace.step("evaluate_coderabbit_feedback");
    trace.step("build_remediation_plan");
    trace.step("remediate_pr_followup");
    trace.step("validate_remediation_result");
    trace.step("run_post_pr_tests");
    trace.artifact("post-pr-test-result.json");
    trace.marker("local-tests-failed");
    trace.step("remediate_pr_followup");
    trace.step("validate_remediation_result");
    trace.step("run_post_pr_tests");
    trace.marker("local-tests-passed");
    trace.step("push_remediation_changes");
    trace.step("capture_pr_identity");
    trace.head("head-b");
    trace.step("watch_pr_checks");
    trace.step("collect_ci_failures");
    trace.step("collect_coderabbit_feedback");
    trace.step("evaluate_coderabbit_feedback");
    trace.step("build_remediation_plan");
    trace.step("mark_coderabbit_feedback");
    trace.marker("post-push-marker-complete");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 7-11,41-49
fn invalid_out_of_scope_marker_success() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.step("watch_pr_checks");
    trace.step("collect_ci_failures");
    trace.step("collect_coderabbit_feedback");
    trace.step("evaluate_coderabbit_feedback");
    trace.step("build_remediation_plan");
    trace.step("mark_coderabbit_feedback");
    trace.comment("cr-invalid-1");
    trace.comment("cr-out-of-scope-1");
    trace.marker("commented-invalid-and-out-of-scope");
    trace.artifact("marker-result.json");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 41-53
fn marker_partial_failure() -> FakePostPrTrace {
    let mut trace = invalid_out_of_scope_marker_success();
    trace.comments_posted = vec!["cr-invalid-1"];
    trace.marker_actions = vec!["partial-marker-failure"];
    trace.steps.pop();
    trace.step("post_pr_failure_terminal");
    trace.outcome = Some(FakeRunOutcome::Failure);
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 41-49
fn marker_retry_idempotency() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("mark_coderabbit_feedback");
    trace.comment("cr-invalid-1");
    trace.marker("attempt-1-commented-cr-invalid-1");
    trace.step("mark_coderabbit_feedback");
    trace.marker("attempt-2-skipped-existing-comment-cr-invalid-1");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 9,34-49
fn pending_marker_carry_forward() -> FakePostPrTrace {
    let mut trace = FakePostPrTrace::default();
    trace.step("capture_pr_identity");
    trace.head("head-a");
    trace.step("mark_coderabbit_feedback");
    trace.comment("cr-valid-1@head-a");
    trace.resolve("cr-valid-1@head-a");
    trace.marker("pending-head-a");
    trace.step("push_remediation_changes");
    trace.step("capture_pr_identity");
    trace.head("head-b");
    trace.step("mark_coderabbit_feedback");
    trace.marker("carried-forward-head-a-to-head-b-noop");
    trace.step("log_completion");
    trace.finish_success();
    trace
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-11,41-53
#[test]
fn post_pr_fake_clean_success_reaches_marker_and_log_completion() {
    let trace = clean_success();
    trace.assert_path(&[
        "capture_pr_identity",
        "post_pr_iteration_guard",
        "watch_pr_checks",
        "collect_ci_failures",
        "collect_coderabbit_feedback",
        "evaluate_coderabbit_feedback",
        "build_remediation_plan",
        "mark_coderabbit_feedback",
        "log_completion",
    ]);
    trace.assert_artifacts(&[
        "pr-identity.json",
        "pr-checks.json",
        "ci-failures.json",
        "coderabbit-feedback.json",
        "feedback-evaluations.json",
        "remediation-plan.json",
        "marker-result.json",
    ]);
    assert_eq!(trace.heads_seen, ["head-a"]);
    assert_eq!(trace.marker_actions, ["noop-clean"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 1-11,41-53
#[test]
fn post_pr_hardening_clean_completion_requires_ready_checks_feedback_and_marker() {
    let trace = clean_success();
    let log_completion_index = trace
        .steps
        .iter()
        .position(|step| *step == "log_completion")
        .expect("clean fixture must reach log_completion");

    for required_step in [
        "watch_pr_checks",
        "collect_ci_failures",
        "collect_coderabbit_feedback",
        "evaluate_coderabbit_feedback",
        "build_remediation_plan",
        "mark_coderabbit_feedback",
    ] {
        let required_step_index = trace
            .steps
            .iter()
            .position(|step| *step == required_step)
            .unwrap_or_else(|| panic!("clean completion missing {required_step}"));
        assert!(
            required_step_index < log_completion_index,
            "{required_step} must run before clean completion"
        );
    }

    assert!(trace.artifacts.contains("pr-checks.json"));
    assert!(trace.artifacts.contains("coderabbit-feedback.json"));
    assert!(trace.artifacts.contains("feedback-evaluations.json"));
    assert!(trace.artifacts.contains("remediation-plan.json"));
    assert!(trace.artifacts.contains("marker-result.json"));
    assert_eq!(trace.marker_actions, ["noop-clean"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-6,12-40,50-53
#[test]
fn post_pr_fake_all_terminal_ci_failed_remediation_rechecks_head() {
    let trace = remediation_loop_from_ci_failure();
    assert_eq!(
        trace.heads_seen,
        ["head-a", "head-b"],
        "push must carry forward by re-capturing PR identity at the new head"
    );
    assert_eq!(
        trace
            .steps
            .iter()
            .filter(|step| **step == "watch_pr_checks")
            .count(),
        2,
        "CI failure remediation must re-watch checks after push"
    );
    assert!(trace
        .steps
        .windows(2)
        .any(|pair| pair == ["push_remediation_changes", "capture_pr_identity"]));
    trace.assert_artifacts(&[
        "pr-identity.json",
        "pr-checks.json",
        "ci-failures.json",
        "coderabbit-feedback.json",
        "feedback-evaluations.json",
        "remediation-plan.json",
        "remediation-result.json",
        "post-pr-test-result.json",
        "push-result.json",
    ]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 1-6,12-40,50-53
#[test]
fn post_pr_hardening_remediation_success_requires_local_verification_before_push_and_recheck() {
    let trace = remediation_loop_from_ci_failure();
    let first_local_verification = trace
        .steps
        .iter()
        .position(|step| *step == "run_post_pr_tests")
        .expect("remediation fixture must run local post-PR tests");
    let first_push = trace
        .steps
        .iter()
        .position(|step| *step == "push_remediation_changes")
        .expect("remediation fixture must push after local verification passes");
    assert!(
        first_local_verification < first_push,
        "local post-remediation verification must run before push"
    );
    assert!(trace.artifacts.contains("post-pr-test-result.json"));
    assert!(trace.artifacts.contains("push-result.json"));

    let first_recheck_after_push = trace.steps[first_push + 1..].windows(4).any(|window| {
        window
            == [
                "capture_pr_identity",
                "post_pr_iteration_guard",
                "watch_pr_checks",
                "collect_ci_failures",
            ]
    });
    assert!(
        first_recheck_after_push,
        "remediated head must be recaptured and rechecked before clean completion"
    );
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 1-5,50-53
#[test]
fn post_pr_hardening_terminal_outcomes_never_log_clean_completion() {
    for terminal_marker in [
        "failed-and-pending-terminal",
        "failed-and-unknown-terminal",
        "empty-not-ready-fatal",
        "unknown-timeout-without-concrete-failures",
    ] {
        let trace = terminal_collection(terminal_marker);
        assert_eq!(trace.steps.last(), Some(&"post_pr_failure_terminal"));
        assert!(!trace.steps.contains(&"log_completion"));
        assert_eq!(trace.marker_actions, [terminal_marker]);
        assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
    }
}

/// @pseudocode lines 1-5,50-53
#[test]
fn post_pr_fake_failed_and_pending_terminal() {
    let trace = terminal_collection("failed-and-pending-terminal");
    trace.assert_path(&[
        "capture_pr_identity",
        "post_pr_iteration_guard",
        "watch_pr_checks",
        "collect_ci_failures",
        "post_pr_failure_terminal",
    ]);
    assert_eq!(trace.marker_actions, ["failed-and-pending-terminal"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-5,50-53
#[test]
fn post_pr_fake_failed_and_unknown_terminal() {
    let trace = terminal_collection("failed-and-unknown-terminal");
    assert!(trace.artifacts.contains("ci-failures.json"));
    assert_eq!(trace.steps.last(), Some(&"post_pr_failure_terminal"));
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 6-11,12-40,41-49
#[test]
fn post_pr_fake_coderabbit_valid_remediation() {
    let trace = coderabbit_valid_remediation();
    assert!(trace.steps.windows(4).any(|pair| pair
        == [
            "remediate_pr_followup",
            "validate_remediation_result",
            "run_post_pr_tests",
            "push_remediation_changes"
        ]));
    assert_eq!(trace.comments_resolved, ["cr-valid-1"]);
    assert_eq!(trace.marker_actions, ["resolved-valid-feedback"]);
    assert_eq!(trace.heads_seen, ["head-a", "head-b"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-4,50-53
#[test]
fn post_pr_fake_empty_not_ready_fatal() {
    let trace = terminal_collection("empty-not-ready-fatal");
    assert_eq!(trace.steps.last(), Some(&"post_pr_failure_terminal"));
    assert!(trace.comments_posted.is_empty());
    assert!(trace.comments_resolved.is_empty());
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 6-11,12-40,41-49
#[test]
fn post_pr_hardening_valid_feedback_completion_requires_resolution_recheck_and_marker() {
    let trace = coderabbit_valid_remediation();
    let remediation_index = trace
        .steps
        .iter()
        .position(|step| *step == "remediate_pr_followup")
        .expect("valid feedback fixture must remediate actionable feedback");
    let push_index = trace
        .steps
        .iter()
        .position(|step| *step == "push_remediation_changes")
        .expect("valid feedback fixture must push remediation changes");
    let marker_index = trace
        .steps
        .iter()
        .rposition(|step| *step == "mark_coderabbit_feedback")
        .expect("valid feedback fixture must mark handled feedback");
    let completion_index = trace
        .steps
        .iter()
        .position(|step| *step == "log_completion")
        .expect("valid feedback fixture must complete after marker actions");

    assert!(remediation_index < push_index);
    assert!(push_index < marker_index);
    assert!(marker_index < completion_index);
    assert_eq!(trace.comments_resolved, ["cr-valid-1"]);
    assert_eq!(trace.marker_actions, ["resolved-valid-feedback"]);
    assert_eq!(trace.heads_seen, ["head-a", "head-b"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-4,50-53
#[test]
fn post_pr_fake_unknown_timeout_without_concrete_failures_fatal() {
    let trace = terminal_collection("unknown-timeout-without-concrete-failures");
    assert!(!trace.marker_actions.contains(&"remediate_pr_followup"));
    assert!(!trace.steps.contains(&"remediate_pr_followup"));
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 18-28,50-53
#[test]
fn post_pr_fake_malformed_non_empty_remediation_result_rejected() {
    let trace = malformed_remediation_result();
    assert!(trace.artifacts.contains("remediation-result.json"));
    assert_eq!(
        trace.marker_actions,
        ["malformed-non-empty-result-rejected"]
    );
    assert!(trace.steps.windows(3).any(|pair| pair
        == [
            "remediate_pr_followup",
            "validate_remediation_result",
            "post_pr_failure_terminal"
        ]));
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 29-33,50-53
#[test]
fn post_pr_fake_local_verification_failure_loops_to_remediation() {
    let trace = local_verification_failure_loop();
    assert_eq!(
        trace
            .steps
            .iter()
            .filter(|step| **step == "run_post_pr_tests")
            .count(),
        2
    );
    assert!(trace
        .steps
        .windows(2)
        .any(|pair| pair == ["run_post_pr_tests", "remediate_pr_followup"]));
    assert_eq!(
        trace.marker_actions,
        [
            "local-tests-failed",
            "local-tests-passed",
            "post-push-marker-complete"
        ]
    );
    assert_eq!(trace.heads_seen, ["head-a", "head-b"]);
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 7-11,41-49
#[test]
fn post_pr_fake_invalid_out_of_scope_only_marks_then_completes() {
    let trace = invalid_out_of_scope_marker_success();
    trace.assert_path(&[
        "capture_pr_identity",
        "watch_pr_checks",
        "collect_ci_failures",
        "collect_coderabbit_feedback",
        "evaluate_coderabbit_feedback",
        "build_remediation_plan",
        "mark_coderabbit_feedback",
        "log_completion",
    ]);
    assert_eq!(trace.comments_posted, ["cr-invalid-1", "cr-out-of-scope-1"]);
    assert!(trace.comments_resolved.is_empty());
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 41-53
#[test]
fn post_pr_fake_marker_partial_failure_terminal() {
    let trace = marker_partial_failure();
    assert_eq!(trace.comments_posted, ["cr-invalid-1"]);
    assert_eq!(trace.marker_actions, ["partial-marker-failure"]);
    assert_eq!(trace.steps.last(), Some(&"post_pr_failure_terminal"));
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Failure));
    let engine_outcome = RunOutcome::Failure {
        step_id: "post_pr_failure_terminal".to_string(),
        reason: "marker partial failure".to_string(),
    };
    assert!(
        matches!(engine_outcome, RunOutcome::Failure { ref step_id, .. } if step_id == "post_pr_failure_terminal")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 41-49
#[test]
fn post_pr_fake_marker_retry_idempotency() {
    let trace = marker_retry_idempotency();
    assert_eq!(
        trace.comments_posted,
        ["cr-invalid-1"],
        "retry must not duplicate comment side effects"
    );
    assert_eq!(
        trace.marker_actions,
        [
            "attempt-1-commented-cr-invalid-1",
            "attempt-2-skipped-existing-comment-cr-invalid-1"
        ]
    );
    assert_eq!(
        trace
            .steps
            .iter()
            .filter(|step| **step == "mark_coderabbit_feedback")
            .count(),
        2
    );
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 9,34-49
#[test]
fn post_pr_fake_pending_marker_carry_forward_head_a_to_b() {
    let trace = pending_marker_carry_forward();
    assert_eq!(trace.heads_seen, ["head-a", "head-b"]);
    assert_eq!(
        trace.comments_posted,
        ["cr-valid-1@head-a"],
        "carried marker must not duplicate head-a comment on head-b"
    );
    assert_eq!(
        trace.comments_resolved,
        ["cr-valid-1@head-a"],
        "carried marker must not duplicate head-a resolution on head-b"
    );
    assert_eq!(
        trace.marker_actions,
        ["pending-head-a", "carried-forward-head-a-to-head-b-noop"]
    );
    assert_eq!(trace.outcome, Some(FakeRunOutcome::Success));
}
