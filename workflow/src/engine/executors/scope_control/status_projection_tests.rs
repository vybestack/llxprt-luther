//! Tests for [`super::status_projection`].

use super::*;
use crate::engine::executors::scope_control::decision::{
    ScopeExpansionDecision, ScopeExpansionRequest, ScopeExpansionResolution,
};
use crate::engine::executors::scope_control::evaluation::{
    ScopeEvaluation, Violation, ViolationCode,
};
use crate::engine::executors::scope_control::measurement::PatchMeasurement;
use crate::engine::executors::scope_control::model::CanonicalReviewCaps;
use crate::engine::executors::scope_control::persistence::ScopeStatus;
use crate::engine::executors::scope_control::review_state::{
    ReviewExhaustionRouting, ReviewExhaustionSummary, ReviewHistory, ReviewKind, ReviewScope,
};
use tempfile::TempDir;

fn base_status() -> ScopeStatus {
    ScopeStatus {
        charter_id: "CHARTER-001".into(),
        run_id: "run-1".into(),
        digest: "abc123def456".into(),
        merge_base: "mergebase123".into(),
        created_at: chrono::Utc::now(),
        measurement: None,
        evaluation: None,
        measured_at: None,
        prior_measurement: None,
        prior_measurement_digest: None,
        prior_measured_at: None,
    }
}

fn sample_measurement() -> PatchMeasurement {
    PatchMeasurement {
        merge_base: "mergebase123".into(),
        head_sha: "head789".into(),
        divergence: 2,
        files_changed: 3,
        added_lines: 150,
        binary_files: 1,
        new_modules: 1,
        dependencies_added: 0,
        content_digest: String::new(),
        public_apis_added: 2,
        changed_paths: vec!["src/core/a.rs".into()],
        changed_subsystems: vec!["core".into()],
        file_details: vec![],
    }
}

fn within_budget_eval() -> ScopeEvaluation {
    ScopeEvaluation {
        within_budget: true,
        within_subsystems: true,
        at_merge_base: false,
        violations: vec![],
    }
}

fn over_budget_eval() -> ScopeEvaluation {
    ScopeEvaluation {
        within_budget: false,
        within_subsystems: true,
        at_merge_base: false,
        violations: vec![Violation {
            code: ViolationCode::BudgetAddedLines,
            message: "added_lines (150) exceeds ceiling (100)".into(),
        }],
    }
}

// --- Unavailable / legacy compatibility ---

#[test]
fn unavailable_when_no_artifact_root() {
    let status = project_scope_status(None, "run-1");
    assert!(matches!(status, ScopeControlStatus::Unavailable { .. }));
}

#[test]
fn unavailable_when_no_scope_dir() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_str().unwrap();
    let status = project_scope_status(Some(root), "run-1");
    assert!(matches!(status, ScopeControlStatus::Unavailable { .. }));
}

#[test]
fn unavailable_when_status_missing() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let root = tmp.path().to_str().unwrap();
    let status = project_scope_status(Some(root), "run-1");
    assert!(matches!(status, ScopeControlStatus::Unavailable { .. }));
}

#[test]
fn json_unavailable_is_null_for_compatibility() {
    let status = ScopeControlStatus::Unavailable {
        reason: "legacy".into(),
    };
    let value = scope_status_to_json(&status);
    assert!(value.is_null());
}

#[test]
fn human_unavailable_explains_reason() {
    let status = ScopeControlStatus::Unavailable {
        reason: "legacy run".into(),
    };
    let text = scope_status_to_human(&status);
    assert!(text.contains("unavailable"));
    assert!(text.contains("legacy run"));
}

// --- Corruption surfacing ---

#[test]
fn error_when_status_corrupt() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    std::fs::write(&status_p, "{ this is not valid json").unwrap();
    let root = tmp.path().to_str().unwrap();
    let status = project_scope_status(Some(root), "run-1");
    match status {
        ScopeControlStatus::Error { message } => {
            assert!(message.contains("corrupt"));
            assert!(message.contains("status.json"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn json_error_surfaces_message() {
    let status = ScopeControlStatus::Error {
        message: "corrupt status.json: ...".into(),
    };
    let value = scope_status_to_json(&status);
    assert_eq!(value["error"], "corrupt status.json: ...");
}

#[test]
fn human_error_surfaces_message() {
    let status = ScopeControlStatus::Error {
        message: "corrupt status.json".into(),
    };
    let text = scope_status_to_human(&status);
    assert!(text.contains("ERROR"));
    assert!(text.contains("corrupt status.json"));
}

// --- Available: within budget ---

#[test]
fn available_within_budget_no_measurement() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let status = base_status();
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            assert_eq!(p.charter_id, "CHARTER-001");
            assert!(!p.measured);
            assert!(p.patch.is_none());
            assert_eq!(p.decision.state, ScopeDecisionState::WithinBudget);
            assert!(p.decision.within_budget);
            assert!(p.review.is_none());
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

#[test]
fn available_within_budget_with_measurement() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(within_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            assert!(p.measured);
            let patch = p.patch.expect("patch");
            assert_eq!(patch.head_sha, "head789");
            assert_eq!(patch.divergence, 2);
            assert_eq!(patch.files_changed, 3);
            assert_eq!(patch.added_lines, 150);
            assert_eq!(patch.binary_files, 1);
            assert_eq!(p.decision.state, ScopeDecisionState::WithinBudget);
            assert!(p.decision.within_budget);
            assert!(p.decision.violations.is_empty());
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

// --- Available: over budget, pending resolution ---

#[test]
fn available_over_budget_pending() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(over_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            assert!(!p.decision.within_budget);
            assert_eq!(p.decision.state, ScopeDecisionState::PendingResolution);
            assert!(p
                .decision
                .violations
                .contains(&"BUDGET_ADDED_LINES".to_string()));
            assert!(p.decision.resolution.is_none());
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

// --- Available: over budget, approved ---

#[test]
fn available_over_budget_approved() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(over_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    // Write an expansion request + resolution.
    let request = ScopeExpansionRequest {
        run_id: "run-1".into(),
        charter_id: "CHARTER-001".into(),
        charter_digest: status.digest.clone(),
        measurement_digest: "digest123".into(),
        measurement: sample_measurement(),
        evaluation: over_budget_eval(),
        violations: over_budget_eval().violations,
        created_at: chrono::Utc::now(),
    };
    super::super::decision::write_expansion_request(tmp.path(), &request).unwrap();

    let resolution = ScopeExpansionResolution {
        run_id: "run-1".into(),
        measurement_digest: "digest123".into(),
        decision: ScopeExpansionDecision::ApproveExpandedScope,
        rationale: "approved by operator".into(),
        resolved_at: chrono::Utc::now(),
    };
    super::super::decision::write_expansion_resolution(tmp.path(), &resolution).unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            assert_eq!(p.decision.state, ScopeDecisionState::ApprovedExpandedScope);
            let res = p.decision.resolution.expect("resolution");
            assert!(res.authorizes_expansion);
            assert_eq!(res.decision, "approve_expanded_scope");
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

// --- Available: over budget, frozen (split) ---

#[test]
fn available_over_budget_frozen_split() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(over_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    let request = ScopeExpansionRequest {
        run_id: "run-1".into(),
        charter_id: "CHARTER-001".into(),
        charter_digest: status.digest.clone(),
        measurement_digest: "digest123".into(),
        measurement: sample_measurement(),
        evaluation: over_budget_eval(),
        violations: over_budget_eval().violations,
        created_at: chrono::Utc::now(),
    };
    super::super::decision::write_expansion_request(tmp.path(), &request).unwrap();

    let resolution = ScopeExpansionResolution {
        run_id: "run-1".into(),
        measurement_digest: "digest123".into(),
        decision: ScopeExpansionDecision::SplitFollowUpIssue,
        rationale: "split into follow-up".into(),
        resolved_at: chrono::Utc::now(),
    };
    super::super::decision::write_expansion_resolution(tmp.path(), &resolution).unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            assert_eq!(p.decision.state, ScopeDecisionState::Frozen);
            let res = p.decision.resolution.expect("resolution");
            assert!(!res.authorizes_expansion);
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

// --- Review phase projection ---

#[test]
fn review_phase_in_progress() {
    let history = ReviewHistory {
        run_id: "run-1".into(),
        reviews: vec![ReviewScope {
            review_kind: ReviewKind::InitialFull,
            merge_base: "base".into(),
            from_sha: "base".into(),
            to_sha: "head1".into(),
            changed_files: vec![],
            changed_tests: vec![],
            contextual_files: vec![],
            charter_digest: "d".into(),
        }],
        mutating_remediation_rounds: 0,
    };
    let review = build_review_projection(&history, None);
    let r = review.expect("review projection");
    assert_eq!(r.phase, ReviewPhase::InProgress);
    assert_eq!(r.initial_reviews, 1);
    assert_eq!(r.delta_reviews, 0);
}

#[test]
fn review_phase_completed() {
    let history = ReviewHistory {
        run_id: "run-1".into(),
        reviews: vec![
            ReviewScope {
                review_kind: ReviewKind::InitialFull,
                merge_base: "base".into(),
                from_sha: "base".into(),
                to_sha: "head1".into(),
                changed_files: vec![],
                changed_tests: vec![],
                contextual_files: vec![],
                charter_digest: "d".into(),
            },
            ReviewScope {
                review_kind: ReviewKind::FinalAcceptance,
                merge_base: "base".into(),
                from_sha: "base".into(),
                to_sha: "head1".into(),
                changed_files: vec![],
                changed_tests: vec![],
                contextual_files: vec![],
                charter_digest: "d".into(),
            },
        ],
        mutating_remediation_rounds: 0,
    };
    let review = build_review_projection(&history, None);
    let r = review.expect("review projection");
    assert_eq!(r.phase, ReviewPhase::Completed);
    assert_eq!(r.final_reviews, 1);
}

#[test]
fn review_phase_exhausted_with_remaining_from_caps() {
    let history = ReviewHistory {
        run_id: "run-1".into(),
        reviews: vec![],
        mutating_remediation_rounds: 2,
    };
    let summary = ReviewExhaustionSummary {
        routing: ReviewExhaustionRouting::MutatingRemediationExhausted,
        run_id: "run-1".into(),
        head_sha: "head1".into(),
        initial_reviews: 1,
        delta_reviews: 2,
        final_reviews: 0,
        mutating_remediation_rounds: 2,
        caps: CanonicalReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        },
        charter_digest: "d".into(),
        written_at: "2026-07-15T00:00:00Z".into(),
    };
    let review = build_review_projection(&history, Some(&summary));
    let r = review.expect("review projection");
    assert_eq!(r.phase, ReviewPhase::Exhausted);
    assert_eq!(r.remaining_delta_reviews, 0);
    assert_eq!(r.remaining_mutating_remediation_rounds, 0);
}

// --- Timeout recovery projection ---

#[test]
fn timeout_recovery_present_in_projection() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(within_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();

    // Write a timeout snapshot via the public API.
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    let draft = TaskCharterDraft {
        charter_id: "CHARTER-001".into(),
        issue_number: 42,
        run_id: "run-1".into(),
        merge_base: "mergebase123".into(),
        acceptance_criteria: vec!["AC-1".into()],
        non_goals: vec!["NG".into()],
        subsystems: vec![DraftSubsystem {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        budget: DraftBudget {
            max_files_changed: 10,
            max_added_lines: 500,
            max_new_modules: 3,
            max_dependencies_added: 0,
            max_public_apis_added: 5,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        },
        mandatory_gates: vec!["cargo test".into()],
    };
    let charter = normalize_charter(&draft);
    super::super::timeout_recovery::handle_timeout_recovery(
        tmp.path(),
        "run-1",
        &charter,
        &sample_measurement(),
        super::super::timeout_recovery::TimeoutKind::IdleTimeout,
        true,
        &super::super::timeout_recovery::ProcessEvidence::default(),
    )
    .unwrap();

    let root = tmp.path().to_str().unwrap();
    let projected = project_scope_status(Some(root), "run-1");
    match projected {
        ScopeControlStatus::Available(p) => {
            let tr = p.timeout_recovery.expect("timeout recovery");
            assert!(tr.recovery_required);
            assert_eq!(tr.timeout_kind, "IdleTimeout");
        }
        other => panic!("expected Available, got {other:?}"),
    }
}

// --- JSON serialization shape ---

#[test]
fn json_available_has_expected_fields() {
    let status = ScopeControlStatus::Available(Box::new(ScopeControlProjection {
        charter_id: "C".into(),
        charter_digest: "d".into(),
        merge_base: "m".into(),
        measured: true,
        patch: Some(PatchProjection {
            head_sha: "h".into(),
            divergence: 1,
            files_changed: 2,
            added_lines: 50,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            public_apis_added: 1,
            growth: None,
        }),
        decision: ScopeDecisionProjection {
            state: ScopeDecisionState::WithinBudget,
            within_budget: true,
            violations: vec![],
            resolution: None,
        },
        review: None,
        timeout_recovery: None,
        measured_at: Some("2026-07-15T00:00:00Z".into()),
    }));
    let value = scope_status_to_json(&status);
    assert_eq!(value["charter_id"], "C");
    assert_eq!(value["measured"], true);
    assert_eq!(value["patch"]["files_changed"], 2);
    assert_eq!(value["decision"]["state"], "within_budget");
}

#[test]
fn corrupt_review_history_surfaces_error() {
    let tmp = TempDir::new().unwrap();
    let dir = scope_control_dir(tmp.path(), "run-1");
    std::fs::create_dir_all(&dir).unwrap();
    let status_p = status_path(&dir);
    let status = base_status();
    std::fs::write(&status_p, serde_json::to_string_pretty(&status).unwrap()).unwrap();
    // Corrupt review history.
    std::fs::write(dir.join("review-history.json"), "NOT JSON").unwrap();
    let root = tmp.path().to_str().unwrap();
    let result = project_scope_status(Some(root), "run-1");
    assert!(matches!(result, ScopeControlStatus::Error { .. }));
}

#[test]
fn diagnostic_scope_dir_resolves_correctly() {
    let path = diagnostic_scope_dir("/artifacts", "run-1");
    assert!(path.ends_with("scope-control/run-1"));
}

#[test]
fn human_available_renders_block() {
    let status = ScopeControlStatus::Available(Box::new(ScopeControlProjection {
        charter_id: "CHARTER-001".into(),
        charter_digest: "abc123def456".into(),
        merge_base: "mergebase".into(),
        measured: true,
        patch: Some(PatchProjection {
            head_sha: "head789".into(),
            divergence: 2,
            files_changed: 3,
            added_lines: 150,
            binary_files: 1,
            new_modules: 1,
            dependencies_added: 0,
            public_apis_added: 2,
            growth: None,
        }),
        decision: ScopeDecisionProjection {
            state: ScopeDecisionState::PendingResolution,
            within_budget: false,
            violations: vec!["BUDGET_ADDED_LINES".into()],
            resolution: None,
        },
        review: Some(ReviewProjection {
            phase: ReviewPhase::InProgress,
            initial_reviews: 1,
            delta_reviews: 0,
            final_reviews: 0,
            mutating_remediation_rounds: 0,
            remaining_delta_reviews: 0,
            remaining_mutating_remediation_rounds: 0,
        }),
        timeout_recovery: None,
        measured_at: Some("2026-07-15T00:00:00Z".into()),
    }));
    let text = scope_status_to_human(&status);
    assert!(text.contains("Scope Control:"));
    assert!(text.contains("HEAD: head789"));
    assert!(text.contains("pending scope decision"));
    assert!(text.contains("Review: in progress"));
}

#[test]
fn human_no_measurement_shows_pending() {
    let status = ScopeControlStatus::Available(Box::new(ScopeControlProjection {
        charter_id: "C".into(),
        charter_digest: "d".into(),
        merge_base: "m".into(),
        measured: false,
        patch: None,
        decision: ScopeDecisionProjection {
            state: ScopeDecisionState::WithinBudget,
            within_budget: true,
            violations: vec![],
            resolution: None,
        },
        review: None,
        timeout_recovery: None,
        measured_at: None,
    }));
    let text = scope_status_to_human(&status);
    assert!(text.contains("pending"));
}

#[test]
fn build_projection_pure_function_no_io() {
    // Verify the pure builder works without any filesystem.
    let status = base_status();
    let projection = build_projection(&status, None, None, &ReviewHistory::default(), None, None);
    assert_eq!(projection.charter_id, "CHARTER-001");
    assert!(!projection.measured);
    assert!(projection.patch.is_none());
    assert!(projection.review.is_none());
}

// --- Issue 142: growth projection from persisted prior snapshot ---

fn measured_status_with_prior() -> ScopeStatus {
    let mut status = base_status();
    status.measurement = Some(PatchMeasurement {
        merge_base: "mergebase123".into(),
        head_sha: "head-current".into(),
        divergence: 4,
        files_changed: 6,
        added_lines: 200,
        binary_files: 0,
        new_modules: 2,
        dependencies_added: 1,
        content_digest: String::new(),
        public_apis_added: 3,
        changed_paths: vec!["src/core/a.rs".into()],
        changed_subsystems: vec!["core".into()],
        file_details: vec![],
    });
    status.evaluation = Some(within_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    status.prior_measurement = Some(PatchMeasurement {
        merge_base: "mergebase123".into(),
        head_sha: "head-prior".into(),
        divergence: 2,
        files_changed: 3,
        added_lines: 100,
        binary_files: 0,
        new_modules: 1,
        dependencies_added: 0,
        content_digest: String::new(),
        public_apis_added: 1,
        changed_paths: vec!["src/core/a.rs".into()],
        changed_subsystems: vec!["core".into()],
        file_details: vec![],
    });
    status.prior_measurement_digest = Some("prior-digest-abc".into());
    status.prior_measured_at = Some(
        chrono::DateTime::parse_from_rfc3339("2026-07-14T00:00:00Z")
            .expect("valid rfc3339")
            .with_timezone(&chrono::Utc),
    );
    status
}

#[test]
fn projection_exposes_growth_deltas() {
    let status = measured_status_with_prior();
    let projection = build_projection(&status, None, None, &ReviewHistory::default(), None, None);
    let patch = projection.patch.expect("patch");
    let growth = patch.growth.expect("growth present with prior");
    assert_eq!(growth.files_changed_delta, 3); // 6 - 3
    assert_eq!(growth.added_lines_delta, 100); // 200 - 100
    assert_eq!(growth.new_modules_delta, 1); // 2 - 1
    assert_eq!(growth.dependencies_added_delta, 1); // 1 - 0
    assert_eq!(growth.public_apis_added_delta, 2); // 3 - 1
    assert_eq!(growth.divergence_delta, 2); // 4 - 2
    assert_eq!(growth.prior_head_sha, "head-prior");
    assert_eq!(growth.prior_digest.as_deref(), Some("prior-digest-abc"));
}

#[test]
fn projection_no_growth_when_no_prior() {
    let mut status = base_status();
    status.measurement = Some(sample_measurement());
    status.evaluation = Some(within_budget_eval());
    status.measured_at = Some(chrono::Utc::now());
    let projection = build_projection(&status, None, None, &ReviewHistory::default(), None, None);
    let patch = projection.patch.expect("patch");
    assert!(patch.growth.is_none(), "no growth without a prior snapshot");
}

#[test]
fn json_projection_includes_growth_object() {
    let status = ScopeControlStatus::Available(Box::new(ScopeControlProjection {
        charter_id: "C".into(),
        charter_digest: "d".into(),
        merge_base: "m".into(),
        measured: true,
        patch: Some(PatchProjection {
            head_sha: "h".into(),
            divergence: 4,
            files_changed: 6,
            added_lines: 200,
            binary_files: 0,
            new_modules: 2,
            dependencies_added: 1,
            public_apis_added: 3,
            growth: Some(PatchGrowthProjection {
                files_changed_delta: 3,
                added_lines_delta: 100,
                new_modules_delta: 1,
                dependencies_added_delta: 1,
                public_apis_added_delta: 2,
                divergence_delta: 2,
                prior_head_sha: "h-old".into(),
                prior_digest: Some("dig".into()),
                prior_measured_at: Some("2026-07-14T00:00:00Z".into()),
            }),
        }),
        decision: ScopeDecisionProjection {
            state: ScopeDecisionState::WithinBudget,
            within_budget: true,
            violations: vec![],
            resolution: None,
        },
        review: None,
        timeout_recovery: None,
        measured_at: Some("2026-07-15T00:00:00Z".into()),
    }));
    let value = scope_status_to_json(&status);
    assert_eq!(value["patch"]["growth"]["files_changed_delta"], 3);
    assert_eq!(value["patch"]["growth"]["added_lines_delta"], 100);
    assert_eq!(value["patch"]["growth"]["divergence_delta"], 2);
    assert_eq!(value["patch"]["growth"]["prior_head_sha"], "h-old");
}

#[test]
fn human_projection_renders_growth_line() {
    let status = measured_status_with_prior();
    let projection = build_projection(&status, None, None, &ReviewHistory::default(), None, None);
    let rendered = ScopeControlStatus::Available(Box::new(projection));
    let text = scope_status_to_human(&rendered);
    assert!(text.contains("Growth since prior round"), "got: {text}");
    assert!(text.contains("+3 files"), "files delta present: {text}");
    assert!(text.contains("+100 lines"), "lines delta present: {text}");
}

#[test]
fn format_signed_delta_shows_explicit_sign() {
    assert_eq!(format_signed_delta(5), "+5");
    assert_eq!(format_signed_delta(0), "+0");
    assert_eq!(format_signed_delta(-3), "-3");
}
