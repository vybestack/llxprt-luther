//! Bounded review phase tracking (issue 142, slice 5).
//!
//! Extends the existing post-PR iteration guard history so each head/run
//! allows:
//! 1. exactly one initial full changed-file review;
//! 2. bounded delta reviews only for changed heads/ranges after remediation;
//! 3. exactly one final charter-acceptance review.
//!
//! Same-head replay is idempotent (consumes no round). After the final
//! acceptance review, no fresh broad review loop is launched. Caps come from
//! the charter's `review_caps`. The merge-base..head range and changed-tests
//! inclusion are persisted per review.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::CanonicalReviewCaps;
use super::persistence::{read_json, scope_control_dir, write_updatable_json, PersistenceError};

/// Filename for the review-round history artifact.
pub const REVIEW_HISTORY_FILENAME: &str = "review-history.json";

/// The kind of review phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    /// Initial full review over merge-base to head.
    InitialFull,
    /// Delta review over prior reviewed head to current head.
    Delta,
    /// Read-only final acceptance review against the charter.
    FinalAcceptance,
}

/// A single recorded review scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewScope {
    pub review_kind: ReviewKind,
    pub merge_base: String,
    pub from_sha: String,
    pub to_sha: String,
    pub changed_files: Vec<String>,
    /// Changed test file paths included in the review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_tests: Vec<String>,
    /// Contextual files that explain invariants but are not treated as
    /// changed scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contextual_files: Vec<String>,
    /// Digest of the charter this review was evaluated against.
    pub charter_digest: String,
}

/// Persisted review-round history for a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewHistory {
    pub run_id: String,
    pub reviews: Vec<ReviewScope>,
    /// Number of mutating remediation rounds completed (head changed after
    /// remediation). Bounded by `max_mutating_remediation_rounds`.
    #[serde(default)]
    pub mutating_remediation_rounds: u32,
}

/// Outcome of checking whether a review phase is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewCheckOutcome {
    /// The review is allowed and has been recorded (if not already).
    Allowed,
    /// Same-head replay; no new round consumed.
    SameHeadReplay,
    /// An initial full review has already been recorded for this head.
    InitialAlreadyRecorded,
    /// The delta review cap is exhausted.
    DeltaCapExhausted { used: u32, cap: u32 },
    /// The final acceptance review has already been recorded.
    FinalAlreadyRecorded,
    /// Cannot start a delta or final review before the initial full review.
    InitialRequiredFirst,
    /// A broad review loop is blocked after the final acceptance review.
    BlockedAfterFinal,
    /// The `max_mutating_remediation_rounds` cap is exhausted.
    MutatingRemediationExhausted { used: u32, cap: u32 },
}

impl ReviewCheckOutcome {
    /// Whether this outcome permits the review to proceed.
    #[must_use]
    pub fn permits_review(&self) -> bool {
        matches!(self, Self::Allowed | Self::SameHeadReplay)
    }
}

/// Count how many reviews of each kind are in the history.
#[must_use]
pub fn count_by_kind(history: &ReviewHistory, kind: ReviewKind) -> u32 {
    history
        .reviews
        .iter()
        .filter(|r| r.review_kind == kind)
        .count() as u32
}

/// Check whether an initial full review is allowed for the given head.
///
/// Returns `Allowed` if no initial review exists yet for this head, or
/// `SameHeadReplay` if one already exists with the same `to_sha`.
#[must_use]
pub fn check_initial(history: &ReviewHistory, head_sha: &str) -> ReviewCheckOutcome {
    let existing = history
        .reviews
        .iter()
        .find(|r| r.review_kind == ReviewKind::InitialFull);
    match existing {
        Some(r) if r.to_sha == head_sha => ReviewCheckOutcome::SameHeadReplay,
        Some(_) => ReviewCheckOutcome::InitialAlreadyRecorded,
        None => ReviewCheckOutcome::Allowed,
    }
}

/// Check whether a delta review is allowed after the initial full review.
///
/// Delta reviews are bounded by `max_delta_reviews`. A head change that
/// matches an existing delta review's `to_sha` is a same-head replay.
#[must_use]
pub fn check_delta(
    history: &ReviewHistory,
    head_sha: &str,
    caps: &CanonicalReviewCaps,
) -> ReviewCheckOutcome {
    if count_by_kind(history, ReviewKind::InitialFull) == 0 {
        return ReviewCheckOutcome::InitialRequiredFirst;
    }
    if count_by_kind(history, ReviewKind::FinalAcceptance) > 0 {
        return ReviewCheckOutcome::BlockedAfterFinal;
    }
    // Same-head replay: an existing delta already reviewed this head.
    if history
        .reviews
        .iter()
        .any(|r| r.review_kind == ReviewKind::Delta && r.to_sha == head_sha)
    {
        return ReviewCheckOutcome::SameHeadReplay;
    }
    let used = count_by_kind(history, ReviewKind::Delta);
    if used >= caps.max_delta_reviews {
        return ReviewCheckOutcome::DeltaCapExhausted {
            used,
            cap: caps.max_delta_reviews,
        };
    }
    ReviewCheckOutcome::Allowed
}

/// Check whether the final acceptance review is allowed.
#[must_use]
pub fn check_final(history: &ReviewHistory, caps: &CanonicalReviewCaps) -> ReviewCheckOutcome {
    if count_by_kind(history, ReviewKind::InitialFull) == 0 {
        return ReviewCheckOutcome::InitialRequiredFirst;
    }
    let used = count_by_kind(history, ReviewKind::FinalAcceptance);
    if used >= caps.final_acceptance_reviews {
        return ReviewCheckOutcome::FinalAlreadyRecorded;
    }
    ReviewCheckOutcome::Allowed
}

/// Read the review history for a run. Returns an empty history if the file
/// does not exist yet (first review).
pub fn read_review_history(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<ReviewHistory, PersistenceError> {
    let path = review_history_path(artifact_dir, run_id);
    if !path.exists() {
        return Ok(ReviewHistory {
            run_id: run_id.to_string(),
            reviews: Vec::new(),
            mutating_remediation_rounds: 0,
        });
    }
    let history: ReviewHistory = read_json(&path)?;
    Ok(history)
}

/// Persist the review history atomically.
pub fn write_review_history(
    artifact_dir: &Path,
    history: &ReviewHistory,
) -> Result<(), PersistenceError> {
    let path = review_history_path(artifact_dir, &history.run_id);
    write_updatable_json(&path, history)
}

/// Record a review scope in the history if it passes the phase check.
///
/// Returns the outcome. When `Allowed`, the scope is appended and the
/// updated history is persisted.
pub fn record_review(
    artifact_dir: &Path,
    run_id: &str,
    scope: &ReviewScope,
    caps: &CanonicalReviewCaps,
) -> Result<ReviewCheckOutcome, PersistenceError> {
    let mut history = read_review_history(artifact_dir, run_id)?;
    let outcome = match scope.review_kind {
        ReviewKind::InitialFull => check_initial(&history, &scope.to_sha),
        ReviewKind::Delta => check_delta(&history, &scope.to_sha, caps),
        ReviewKind::FinalAcceptance => check_final(&history, caps),
    };
    if outcome == ReviewCheckOutcome::Allowed {
        history.run_id = run_id.to_string();
        history.reviews.push(scope.clone());
        write_review_history(artifact_dir, &history)?;
    }
    Ok(outcome)
}

/// Resolve the review history artifact path for a run.
fn review_history_path(artifact_dir: &Path, run_id: &str) -> PathBuf {
    scope_control_dir(artifact_dir, run_id).join(REVIEW_HISTORY_FILENAME)
}

// ---------------------------------------------------------------------------
// Production integration API
// ---------------------------------------------------------------------------

/// Durable summary written when the review-state machine is exhausted. It
/// captures the routing decision and all provenance needed by the terminal
/// failure executor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewExhaustionSummary {
    pub routing: ReviewExhaustionRouting,
    pub run_id: String,
    pub head_sha: String,
    pub initial_reviews: u32,
    pub delta_reviews: u32,
    pub final_reviews: u32,
    pub mutating_remediation_rounds: u32,
    pub caps: CanonicalReviewCaps,
    pub charter_digest: String,
    pub written_at: String,
}

/// Where an exhausted review state machine routes the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewExhaustionRouting {
    /// Delta review cap exhausted; route to terminal failure.
    DeltaCapExhausted,
    /// `max_mutating_remediation_rounds` exhausted; route to terminal failure.
    MutatingRemediationExhausted,
    /// Blocked after final acceptance; no further broad review.
    BlockedAfterFinal,
}

/// Filename for the exhaustion summary artifact.
pub const EXHAUSTION_SUMMARY_FILENAME: &str = "review-exhaustion.json";

/// Write the durable exhaustion summary atomically. Idempotent: overwriting
/// with the same routing is safe (the terminal executor selects by durable
/// sequence).
pub fn write_exhaustion_summary(
    artifact_dir: &Path,
    summary: &ReviewExhaustionSummary,
) -> Result<PathBuf, PersistenceError> {
    let dir = scope_control_dir(artifact_dir, &summary.run_id);
    let path = dir.join(EXHAUSTION_SUMMARY_FILENAME);
    write_updatable_json(&path, summary)?;
    Ok(path)
}

/// Read the exhaustion summary if it exists.
pub fn read_exhaustion_summary(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<Option<ReviewExhaustionSummary>, PersistenceError> {
    let path = scope_control_dir(artifact_dir, run_id).join(EXHAUSTION_SUMMARY_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let summary: ReviewExhaustionSummary = read_json(&path)?;
    Ok(Some(summary))
}

/// Cohesive input for [`pre_launch_review_gate`].
///
/// Bundles the run context and charter provenance the gate needs to decide
/// whether a new review round is allowed.
#[derive(Debug, Clone)]
pub struct PreLaunchReviewRequest<'a> {
    /// Unique run identifier used to locate the review history artifact.
    pub run_id: &'a str,
    /// Current PR head SHA under review.
    pub head_sha: &'a str,
    /// Merge-base SHA used as the review range anchor.
    pub merge_base: &'a str,
    /// Changed file paths to include in the review scope.
    pub changed_files: &'a [String],
    /// Changed test file paths to include in the review scope.
    pub changed_tests: &'a [String],
    /// Deterministic digest of the charter this review is evaluated against.
    pub charter_digest: &'a str,
    /// Review caps from the canonical charter.
    pub caps: &'a CanonicalReviewCaps,
    /// RFC 3339 timestamp for durable summary provenance.
    pub now_rfc3339: &'a str,
}

/// The pre-launch gate checked by production before invoking remediation.
///
/// This is the production entry point that binds the review state machine into
/// the PR-follow-up remediation flow. It:
///
/// 1. Loads the durable review history for the run.
/// 2. Determines whether the current head requires a new review round.
/// 3. Enforces `max_mutating_remediation_rounds` against prior rounds.
/// 4. Records the review scope (delta or initial) if allowed.
/// 5. Writes a durable exhaustion summary when caps are reached.
///
/// Returns the outcome. Callers must check `permits_review()` and route to
/// terminal failure when `false`.
/// Build a durable exhaustion summary from current history state.
fn build_exhaustion_summary(
    history: &ReviewHistory,
    request: &PreLaunchReviewRequest<'_>,
    routing: ReviewExhaustionRouting,
    delta_reviews: u32,
) -> ReviewExhaustionSummary {
    ReviewExhaustionSummary {
        routing,
        run_id: request.run_id.to_string(),
        head_sha: request.head_sha.to_string(),
        initial_reviews: count_by_kind(history, ReviewKind::InitialFull),
        delta_reviews,
        final_reviews: count_by_kind(history, ReviewKind::FinalAcceptance),
        mutating_remediation_rounds: history.mutating_remediation_rounds,
        caps: request.caps.clone(),
        charter_digest: request.charter_digest.to_string(),
        written_at: request.now_rfc3339.to_string(),
    }
}

pub fn pre_launch_review_gate(
    artifact_dir: &Path,
    request: &PreLaunchReviewRequest<'_>,
) -> Result<ReviewCheckOutcome, PersistenceError> {
    let mut history = read_review_history(artifact_dir, request.run_id)?;

    // If final acceptance is already recorded, no broad review is allowed.
    if count_by_kind(&history, ReviewKind::FinalAcceptance) > 0 {
        let summary = build_exhaustion_summary(
            &history,
            request,
            ReviewExhaustionRouting::BlockedAfterFinal,
            count_by_kind(&history, ReviewKind::Delta),
        );
        write_exhaustion_summary(artifact_dir, &summary)?;
        return Ok(ReviewCheckOutcome::BlockedAfterFinal);
    }

    let has_initial = count_by_kind(&history, ReviewKind::InitialFull) > 0;

    // Determine the review kind for this head.
    let (outcome, scope) = if !has_initial {
        // First review is the initial full review.
        let scope = build_review_scope(
            ReviewKind::InitialFull,
            request,
            request.merge_base.to_string(),
        );
        let outcome = check_initial(&history, request.head_sha);
        (outcome, scope)
    } else {
        let delta_result = evaluate_delta(artifact_dir, &mut history, request)?;
        return Ok(delta_result);
    };

    if outcome == ReviewCheckOutcome::Allowed {
        history.run_id = request.run_id.to_string();
        history.reviews.push(scope);
        write_review_history(artifact_dir, &history)?;
    }

    Ok(outcome)
}

/// Build a `ReviewScope` from the request fields.
fn build_review_scope(
    review_kind: ReviewKind,
    request: &PreLaunchReviewRequest<'_>,
    from_sha: String,
) -> ReviewScope {
    ReviewScope {
        review_kind,
        merge_base: request.merge_base.to_string(),
        from_sha,
        to_sha: request.head_sha.to_string(),
        changed_files: request.changed_files.to_vec(),
        changed_tests: request.changed_tests.to_vec(),
        contextual_files: Vec::new(),
        charter_digest: request.charter_digest.to_string(),
    }
}

/// Evaluate a delta review for a head change, enforcing caps and returning the
/// outcome. Handles mutating-remediation and delta-cap exhaustion by writing
/// the durable summary and returning the appropriate blocked outcome. When
/// allowed, increments the mutating-remediation counter and persists the
/// history.
fn evaluate_delta(
    artifact_dir: &Path,
    history: &mut ReviewHistory,
    request: &PreLaunchReviewRequest<'_>,
) -> Result<ReviewCheckOutcome, PersistenceError> {
    let prior_head = last_reviewed_head(history);

    if prior_head.as_deref() == Some(request.head_sha) {
        return Ok(ReviewCheckOutcome::SameHeadReplay);
    }

    // Head changed — this is a mutating remediation round.
    if history.mutating_remediation_rounds >= request.caps.max_mutating_remediation_rounds {
        let summary = build_exhaustion_summary(
            history,
            request,
            ReviewExhaustionRouting::MutatingRemediationExhausted,
            count_by_kind(history, ReviewKind::Delta),
        );
        write_exhaustion_summary(artifact_dir, &summary)?;
        return Ok(ReviewCheckOutcome::MutatingRemediationExhausted {
            used: history.mutating_remediation_rounds,
            cap: request.caps.max_mutating_remediation_rounds,
        });
    }

    // Check delta cap before allowing. A prior delta for this head is an
    // idempotent replay and must not consume another remediation round.
    let delta_outcome = check_delta(history, request.head_sha, request.caps);
    match delta_outcome {
        ReviewCheckOutcome::Allowed => {}
        ReviewCheckOutcome::SameHeadReplay => return Ok(ReviewCheckOutcome::SameHeadReplay),
        other => return handle_delta_exhaustion(artifact_dir, history, request, other),
    }
    // Delta is allowed: increment mutating counter and record.
    history.mutating_remediation_rounds += 1;
    let scope = build_review_scope(
        ReviewKind::Delta,
        request,
        prior_head.unwrap_or_else(|| request.merge_base.to_string()),
    );
    history.run_id = request.run_id.to_string();
    history.reviews.push(scope);
    write_review_history(artifact_dir, history)?;
    Ok(ReviewCheckOutcome::Allowed)
}

/// Handle a non-permitting delta outcome, writing the exhaustion summary when
/// the delta cap is reached.
fn handle_delta_exhaustion(
    artifact_dir: &Path,
    history: &ReviewHistory,
    request: &PreLaunchReviewRequest<'_>,
    delta_outcome: ReviewCheckOutcome,
) -> Result<ReviewCheckOutcome, PersistenceError> {
    match delta_outcome {
        ReviewCheckOutcome::DeltaCapExhausted { used, cap } => {
            let summary = build_exhaustion_summary(
                history,
                request,
                ReviewExhaustionRouting::DeltaCapExhausted,
                used,
            );
            write_exhaustion_summary(artifact_dir, &summary)?;
            Ok(ReviewCheckOutcome::DeltaCapExhausted { used, cap })
        }
        other => Ok(other),
    }
}

/// Record the final charter-acceptance review. After this, no broad review
/// loop is allowed.
pub fn record_final_acceptance(
    artifact_dir: &Path,
    run_id: &str,
    head_sha: &str,
    merge_base: &str,
    charter_digest: &str,
    caps: &CanonicalReviewCaps,
) -> Result<ReviewCheckOutcome, PersistenceError> {
    let scope = ReviewScope {
        review_kind: ReviewKind::FinalAcceptance,
        merge_base: merge_base.to_string(),
        from_sha: merge_base.to_string(),
        to_sha: head_sha.to_string(),
        changed_files: Vec::new(),
        changed_tests: Vec::new(),
        contextual_files: Vec::new(),
        charter_digest: charter_digest.to_string(),
    };
    record_review(artifact_dir, run_id, &scope, caps)
}

/// Return the last reviewed `to_sha` (from initial or delta reviews), or
/// `None` when no reviews exist.
#[must_use]
pub fn last_reviewed_head(history: &ReviewHistory) -> Option<String> {
    history
        .reviews
        .iter()
        .rev()
        .find(|r| matches!(r.review_kind, ReviewKind::InitialFull | ReviewKind::Delta))
        .map(|r| r.to_sha.clone())
}

/// Extract changed test paths from a list of changed file paths. A file is
/// considered a test file when its filename (not directory) follows a
/// conventional test-naming pattern: contains `test_` or `tests_` prefix,
/// `_test` suffix, or `.test.` infix in the extension.
#[must_use]
pub fn filter_changed_tests(changed_files: &[String]) -> Vec<String> {
    changed_files
        .iter()
        .filter(|path| is_test_file(path))
        .cloned()
        .collect()
}

/// Whether a file path's basename follows a conventional test-naming pattern.
fn is_test_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
    normalized.starts_with("tests/")
        || normalized.contains("/tests/")
        || basename.starts_with("test_")
        || basename.starts_with("tests_")
        || basename.ends_with("_test.rs")
        || basename.ends_with("_test.go")
        || basename.ends_with("_test.py")
        || basename == "test.rs"
        || basename.ends_with(".test.ts")
        || basename.ends_with(".test.tsx")
        || basename.ends_with(".test.js")
}

#[cfg(test)]
#[path = "review_state_tests.rs"]
mod tests;
