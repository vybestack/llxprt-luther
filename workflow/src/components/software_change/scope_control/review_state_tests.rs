//! Tests for [`super::review_state`].

use super::super::model::CanonicalReviewCaps;
use super::*;
use tempfile::TempDir;

fn caps() -> CanonicalReviewCaps {
    CanonicalReviewCaps {
        initial_full_reviews: 1,
        max_delta_reviews: 2,
        final_acceptance_reviews: 1,
        max_mutating_remediation_rounds: 2,
    }
}

struct PreLaunchArgs<'a> {
    run_id: &'a str,
    head_sha: &'a str,
    merge_base: &'a str,
    changed_files: &'a [String],
    changed_tests: &'a [String],
    charter_digest: &'a str,
    caps: &'a CanonicalReviewCaps,
    now: &'a str,
}

fn pre_launch(tmp: &tempfile::TempDir, args: &PreLaunchArgs<'_>) -> ReviewCheckOutcome {
    let request = PreLaunchReviewRequest {
        run_id: args.run_id,
        head_sha: args.head_sha,
        merge_base: args.merge_base,
        changed_files: args.changed_files,
        changed_tests: args.changed_tests,
        charter_digest: args.charter_digest,
        caps: args.caps,
        now_rfc3339: args.now,
    };
    pre_launch_review_gate(tmp.path(), &request).unwrap()
}

fn initial_scope(head: &str) -> ReviewScope {
    ReviewScope {
        review_kind: ReviewKind::InitialFull,
        merge_base: "base".into(),
        from_sha: "base".into(),
        to_sha: head.into(),
        changed_files: vec!["src/a.rs".into()],
        changed_tests: vec!["tests/a.rs".into()],
        contextual_files: vec![],
        charter_digest: "digest".into(),
    }
}

fn delta_scope(head: &str) -> ReviewScope {
    ReviewScope {
        review_kind: ReviewKind::Delta,
        merge_base: "base".into(),
        from_sha: "prev".into(),
        to_sha: head.into(),
        changed_files: vec!["src/b.rs".into()],
        changed_tests: vec!["tests/b.rs".into()],
        contextual_files: vec![],
        charter_digest: "digest".into(),
    }
}

fn final_scope(head: &str) -> ReviewScope {
    ReviewScope {
        review_kind: ReviewKind::FinalAcceptance,
        merge_base: "base".into(),
        from_sha: "base".into(),
        to_sha: head.into(),
        changed_files: vec!["src/a.rs".into()],
        changed_tests: vec![],
        contextual_files: vec![],
        charter_digest: "digest".into(),
    }
}

#[test]
fn initial_review_allowed_when_empty() {
    let history = ReviewHistory::default();
    assert_eq!(
        check_initial(&history, "head1"),
        ReviewCheckOutcome::Allowed
    );
}

#[test]
fn initial_same_head_is_replay() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1")],
        ..Default::default()
    };
    assert_eq!(
        check_initial(&history, "head1"),
        ReviewCheckOutcome::SameHeadReplay
    );
}

#[test]
fn second_initial_for_different_head_blocked() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1")],
        ..Default::default()
    };
    assert_eq!(
        check_initial(&history, "head2"),
        ReviewCheckOutcome::InitialAlreadyRecorded
    );
}

#[test]
fn delta_requires_initial_first() {
    let history = ReviewHistory::default();
    assert_eq!(
        check_delta(&history, "head1", &caps()),
        ReviewCheckOutcome::InitialRequiredFirst
    );
}

#[test]
fn delta_same_head_replay() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1"), delta_scope("head2")],
        ..Default::default()
    };
    assert_eq!(
        check_delta(&history, "head2", &caps()),
        ReviewCheckOutcome::SameHeadReplay
    );
}

#[test]
fn delta_cap_exhausted() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![
            initial_scope("head1"),
            delta_scope("head2"),
            delta_scope("head3"),
        ],
        ..Default::default()
    };
    assert!(matches!(
        check_delta(&history, "head4", &caps()),
        ReviewCheckOutcome::DeltaCapExhausted { used: 2, cap: 2 }
    ));
}

#[test]
fn delta_blocked_after_final() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1"), final_scope("head1")],
        ..Default::default()
    };
    assert_eq!(
        check_delta(&history, "head2", &caps()),
        ReviewCheckOutcome::BlockedAfterFinal
    );
}

#[test]
fn final_requires_initial_first() {
    let history = ReviewHistory::default();
    assert_eq!(
        check_final(&history, &caps()),
        ReviewCheckOutcome::InitialRequiredFirst
    );
}

#[test]
fn final_allowed_after_initial() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1")],
        ..Default::default()
    };
    assert_eq!(check_final(&history, &caps()), ReviewCheckOutcome::Allowed);
}

#[test]
fn final_already_recorded() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1"), final_scope("head1")],
        ..Default::default()
    };
    assert_eq!(
        check_final(&history, &caps()),
        ReviewCheckOutcome::FinalAlreadyRecorded
    );
}

#[test]
fn record_initial_persists() {
    let tmp = TempDir::new().unwrap();
    let scope = initial_scope("head1");
    let outcome = record_review(tmp.path(), "r", &scope, &caps()).unwrap();
    assert_eq!(outcome, ReviewCheckOutcome::Allowed);
    let history = read_review_history(tmp.path(), "r").unwrap();
    assert_eq!(history.reviews.len(), 1);
    assert_eq!(history.reviews[0], scope);
}

#[test]
fn record_same_head_initial_does_not_duplicate() {
    let tmp = TempDir::new().unwrap();
    let scope = initial_scope("head1");
    record_review(tmp.path(), "r", &scope, &caps()).unwrap();
    let outcome = record_review(tmp.path(), "r", &scope, &caps()).unwrap();
    assert_eq!(outcome, ReviewCheckOutcome::SameHeadReplay);
    let history = read_review_history(tmp.path(), "r").unwrap();
    assert_eq!(history.reviews.len(), 1);
}

#[test]
fn record_delta_then_final_then_blocked() {
    let tmp = TempDir::new().unwrap();
    record_review(tmp.path(), "r", &initial_scope("head1"), &caps()).unwrap();
    record_review(tmp.path(), "r", &delta_scope("head2"), &caps()).unwrap();
    record_review(tmp.path(), "r", &final_scope("head2"), &caps()).unwrap();

    // After final, delta is blocked.
    let outcome = record_review(tmp.path(), "r", &delta_scope("head3"), &caps()).unwrap();
    assert_eq!(outcome, ReviewCheckOutcome::BlockedAfterFinal);
}

#[test]
fn read_history_returns_empty_when_missing() {
    let tmp = TempDir::new().unwrap();
    let history = read_review_history(tmp.path(), "r").unwrap();
    assert!(history.reviews.is_empty());
}

// -----------------------------------------------------------------------
// Production integration API tests
// -----------------------------------------------------------------------

#[test]
fn pre_launch_initial_allowed_when_empty() {
    let tmp = TempDir::new().unwrap();
    let outcome = pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &["src/a.rs".into()],
            changed_tests: &["tests/a.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );
    assert_eq!(outcome, ReviewCheckOutcome::Allowed);

    let history = read_review_history(tmp.path(), "run1").unwrap();
    assert_eq!(history.reviews.len(), 1);
    assert_eq!(history.reviews[0].review_kind, ReviewKind::InitialFull);
    assert_eq!(history.reviews[0].merge_base, "base");
    assert_eq!(history.reviews[0].from_sha, "base");
    assert_eq!(history.reviews[0].to_sha, "head1");
    assert_eq!(history.reviews[0].changed_files, vec!["src/a.rs"]);
    assert_eq!(history.reviews[0].changed_tests, vec!["tests/a.rs"]);
}

#[test]
fn pre_launch_same_head_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let args = PreLaunchArgs {
        run_id: "run1",
        head_sha: "head1",
        merge_base: "base",
        changed_files: &["src/a.rs".into()],
        changed_tests: &["tests/a.rs".into()],
        charter_digest: "digest",
        caps: &caps(),
        now: "2026-07-15T00:00:00Z",
    };
    pre_launch(&tmp, &args);

    // Same head: idempotent replay.
    let outcome = pre_launch(&tmp, &args);
    assert_eq!(outcome, ReviewCheckOutcome::SameHeadReplay);

    let history = read_review_history(tmp.path(), "run1").unwrap();
    assert_eq!(history.reviews.len(), 1);
    assert_eq!(history.mutating_remediation_rounds, 0);
}

#[test]
fn pre_launch_delta_increments_mutating_counter() {
    let tmp = TempDir::new().unwrap();
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &["src/a.rs".into()],
            changed_tests: &["tests/a.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );

    let outcome = pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head2",
            merge_base: "base",
            changed_files: &["src/b.rs".into()],
            changed_tests: &["tests/b.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:01:00Z",
        },
    );
    assert_eq!(outcome, ReviewCheckOutcome::Allowed);

    let history = read_review_history(tmp.path(), "run1").unwrap();
    assert_eq!(history.reviews.len(), 2);
    assert_eq!(history.reviews[1].review_kind, ReviewKind::Delta);
    assert_eq!(history.reviews[1].from_sha, "head1");
    assert_eq!(history.reviews[1].to_sha, "head2");
    assert_eq!(history.mutating_remediation_rounds, 1);
}

#[test]
fn pre_launch_mutating_cap_exhausted() {
    let tmp = TempDir::new().unwrap();
    let empty = PreLaunchArgs {
        run_id: "run1",
        head_sha: "",
        merge_base: "base",
        changed_files: &[],
        changed_tests: &[],
        charter_digest: "digest",
        caps: &caps(),
        now: "",
    };
    // Initial at head1
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head1",
            now: "2026-07-15T00:00:00Z",
            ..empty
        },
    );
    // Delta head2 (mutating round 1)
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head2",
            now: "2026-07-15T00:01:00Z",
            ..empty
        },
    );
    // Delta head3 (mutating round 2 = cap)
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head3",
            now: "2026-07-15T00:02:00Z",
            ..empty
        },
    );

    // head4: mutating cap (2) exhausted
    let outcome = pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head4",
            now: "2026-07-15T00:03:00Z",
            ..empty
        },
    );
    assert!(matches!(
        outcome,
        ReviewCheckOutcome::MutatingRemediationExhausted { used: 2, cap: 2 }
    ));

    // Durable summary should exist.
    let summary = read_exhaustion_summary(tmp.path(), "run1").unwrap();
    assert!(summary.is_some());
    assert_eq!(
        summary.unwrap().routing,
        ReviewExhaustionRouting::MutatingRemediationExhausted
    );
}

#[test]
fn pre_launch_blocked_after_final() {
    let tmp = TempDir::new().unwrap();
    let empty = PreLaunchArgs {
        run_id: "run1",
        head_sha: "",
        merge_base: "base",
        changed_files: &[],
        changed_tests: &[],
        charter_digest: "digest",
        caps: &caps(),
        now: "",
    };
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head1",
            now: "2026-07-15T00:00:00Z",
            ..empty
        },
    );

    record_final_acceptance(tmp.path(), "run1", "head1", "base", "digest", &caps()).unwrap();

    // Attempting another broad review after final is blocked.
    let outcome = pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head2",
            now: "2026-07-15T00:01:00Z",
            ..empty
        },
    );
    assert_eq!(outcome, ReviewCheckOutcome::BlockedAfterFinal);

    let summary = read_exhaustion_summary(tmp.path(), "run1").unwrap();
    assert!(summary.is_some());
    assert_eq!(
        summary.unwrap().routing,
        ReviewExhaustionRouting::BlockedAfterFinal
    );
}

#[test]
fn pre_launch_delta_cap_exhausted_writes_summary() {
    let mut tight_caps = caps();
    tight_caps.max_delta_reviews = 1;
    tight_caps.max_mutating_remediation_rounds = 5;

    let tmp = TempDir::new().unwrap();
    let empty = PreLaunchArgs {
        run_id: "run1",
        head_sha: "",
        merge_base: "base",
        changed_files: &[],
        changed_tests: &[],
        charter_digest: "digest",
        caps: &tight_caps,
        now: "",
    };
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head1",
            now: "2026-07-15T00:00:00Z",
            ..empty
        },
    );
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head2",
            now: "2026-07-15T00:01:00Z",
            ..empty
        },
    );

    // head3: delta cap (1) exhausted
    let outcome = pre_launch(
        &tmp,
        &PreLaunchArgs {
            head_sha: "head3",
            now: "2026-07-15T00:02:00Z",
            ..empty
        },
    );
    assert!(matches!(
        outcome,
        ReviewCheckOutcome::DeltaCapExhausted { used: 1, cap: 1 }
    ));

    let summary = read_exhaustion_summary(tmp.path(), "run1").unwrap();
    assert!(summary.is_some());
    assert_eq!(
        summary.unwrap().routing,
        ReviewExhaustionRouting::DeltaCapExhausted
    );
}

#[test]
fn pre_launch_delta_scope_has_correct_from_sha() {
    let tmp = TempDir::new().unwrap();
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &["src/a.rs".into()],
            changed_tests: &["tests/a.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head2",
            merge_base: "base",
            changed_files: &["src/b.rs".into()],
            changed_tests: &["tests/b.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:01:00Z",
        },
    );

    let history = read_review_history(tmp.path(), "run1").unwrap();
    let delta = history
        .reviews
        .iter()
        .find(|r| r.review_kind == ReviewKind::Delta)
        .unwrap();
    assert_eq!(delta.from_sha, "head1");
    assert_eq!(delta.to_sha, "head2");
}

#[test]
fn pre_launch_delta_persists_changed_tests() {
    let tmp = TempDir::new().unwrap();
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &["src/a.rs".into()],
            changed_tests: &["tests/a.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head2",
            merge_base: "base",
            changed_files: &["src/b.rs".into()],
            changed_tests: &["tests/b.rs".into()],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:01:00Z",
        },
    );

    let history = read_review_history(tmp.path(), "run1").unwrap();
    let delta = history
        .reviews
        .iter()
        .find(|r| r.review_kind == ReviewKind::Delta)
        .unwrap();
    assert_eq!(delta.changed_tests, vec!["tests/b.rs"]);
}

#[test]
fn record_final_acceptance_persists() {
    let tmp = TempDir::new().unwrap();
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &[],
            changed_tests: &[],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );
    let outcome =
        record_final_acceptance(tmp.path(), "run1", "head1", "base", "digest", &caps()).unwrap();
    assert_eq!(outcome, ReviewCheckOutcome::Allowed);

    let history = read_review_history(tmp.path(), "run1").unwrap();
    assert_eq!(count_by_kind(&history, ReviewKind::FinalAcceptance), 1);
}

#[test]
fn record_final_acceptance_idempotent() {
    let tmp = TempDir::new().unwrap();
    pre_launch(
        &tmp,
        &PreLaunchArgs {
            run_id: "run1",
            head_sha: "head1",
            merge_base: "base",
            changed_files: &[],
            changed_tests: &[],
            charter_digest: "digest",
            caps: &caps(),
            now: "2026-07-15T00:00:00Z",
        },
    );
    record_final_acceptance(tmp.path(), "run1", "head1", "base", "digest", &caps()).unwrap();
    let outcome =
        record_final_acceptance(tmp.path(), "run1", "head1", "base", "digest", &caps()).unwrap();
    assert_eq!(outcome, ReviewCheckOutcome::FinalAlreadyRecorded);
}

#[test]
fn last_reviewed_head_returns_none_when_empty() {
    let history = ReviewHistory::default();
    assert_eq!(last_reviewed_head(&history), None);
}

#[test]
fn last_reviewed_head_returns_initial_then_delta() {
    let history = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1")],
        ..Default::default()
    };
    assert_eq!(last_reviewed_head(&history), Some("head1".into()));

    let history2 = ReviewHistory {
        run_id: "r".into(),
        reviews: vec![initial_scope("head1"), delta_scope("head2")],
        ..Default::default()
    };
    assert_eq!(last_reviewed_head(&history2), Some("head2".into()));
}

#[test]
fn filter_changed_tests_identifies_test_files() {
    let files = vec![
        "src/lib.rs".to_string(),
        "tests/foo_test.rs".to_string(),
        "tests/bar.rs".to_string(),
        "src/baz_test.rs".to_string(),
        "tests/baz_test.py".to_string(),
        "src/qux.go".to_string(),
        "frontend/src/App.test.tsx".to_string(),
    ];
    let tests = filter_changed_tests(&files);
    assert!(tests.contains(&"tests/foo_test.rs".to_string()));
    assert!(tests.contains(&"src/baz_test.rs".to_string()));
    assert!(tests.contains(&"tests/baz_test.py".to_string()));
    assert!(tests.contains(&"frontend/src/App.test.tsx".to_string()));
    assert!(!tests.contains(&"src/lib.rs".to_string()));
    assert!(tests.contains(&"tests/bar.rs".to_string()));
    assert!(!tests.contains(&"src/qux.go".to_string()));
}

#[test]
fn exhaustion_summary_round_trip() {
    let summary = ReviewExhaustionSummary {
        routing: ReviewExhaustionRouting::MutatingRemediationExhausted,
        run_id: "r1".into(),
        head_sha: "h".into(),
        initial_reviews: 1,
        delta_reviews: 2,
        final_reviews: 0,
        mutating_remediation_rounds: 2,
        caps: caps(),
        charter_digest: "d".into(),
        written_at: "2026-07-15T00:00:00Z".into(),
    };
    let tmp = TempDir::new().unwrap();
    let path = write_exhaustion_summary(tmp.path(), &summary).unwrap();
    assert!(path.exists());
    let read_back = read_exhaustion_summary(tmp.path(), "r1").unwrap().unwrap();
    assert_eq!(read_back, summary);
}

#[test]
fn read_exhaustion_returns_none_when_missing() {
    let tmp = TempDir::new().unwrap();
    assert!(read_exhaustion_summary(tmp.path(), "r1").unwrap().is_none());
}
