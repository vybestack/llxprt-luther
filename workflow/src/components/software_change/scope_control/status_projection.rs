//! Read-only scope-control status projection for status/runs-show output.
//!
//! This module provides a **bounded** read model that projects the scope-control
//! state for a single run from well-known artifact files under
//! `{artifact_root}/scope-control/{run_id}/`. It reads a fixed set of named
//! files — never performs unbounded directory scans — and surfaces three
//! distinct outcomes:
//!
//! - [`ScopeControlStatus::Unavailable`] — the run predates scope-control
//!   (no `scope-control/{run_id}/` directory or no `status.json`). Historical
//!   runs must remain compatible and clearly report this.
//! - [`ScopeControlStatus::Available`] — the status read model was read
//!   successfully; the projection carries patch totals/growth, divergence,
//!   pending/frozen scope decision, review phase, and remaining rounds.
//! - [`ScopeControlStatus::Error`] — the status file exists but is corrupt or
//!   unreadable. Callers must surface this explicitly rather than silently
//!   looking normal.
//!
//! @plan:PLAN-20260715-SCOPE-CONTROL issue 142

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::decision::{read_expansion_request, read_expansion_resolution};
use super::persistence::{read_json, scope_control_dir, status_path, ScopeStatus, STATUS_FILENAME};
use super::review_state::{read_exhaustion_summary, read_review_history, ReviewKind};
use super::timeout_recovery::read_timeout_snapshot;

/// Bounded outcome of projecting scope-control state for a run.
///
/// Callers should match on this to decide how to present (or surface an error
/// for) scope-control data in status/runs-show output.
#[derive(Debug, Clone, PartialEq)]
pub enum ScopeControlStatus {
    /// The run has no scope-control artifacts (historical/legacy run, or scope
    /// control not enabled). Output must clearly report this rather than
    /// implying scope control is active and healthy.
    Unavailable {
        /// Human-readable reason (e.g., "no scope-control directory").
        reason: String,
    },
    /// The status read model was read successfully.
    Available(Box<ScopeControlProjection>),
    /// The status file exists but could not be read or parsed. This must be
    /// surfaced as an explicit error, not collapsed into "unavailable" or
    /// "within budget".
    Error {
        /// Human/machine-readable error message.
        message: String,
    },
}

/// The projected scope-control read model for a run.
///
/// This is a *view* over the persisted artifacts: it does not mutate state and
/// collects only the fields needed by status output (patch totals/growth,
/// divergence, pending/frozen scope decision, review phase, and remaining
/// rounds).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopeControlProjection {
    /// Charter identifier.
    pub charter_id: String,
    /// Deterministic charter digest.
    pub charter_digest: String,
    /// Frozen merge base SHA.
    pub merge_base: String,
    /// Whether a measurement has been recorded yet.
    pub measured: bool,
    /// Patch totals and growth (present only after the first measurement).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchProjection>,
    /// Scope-decision state: within budget, pending, frozen (denied), or
    /// approved.
    pub decision: ScopeDecisionProjection,
    /// Review phase and remaining rounds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewProjection>,
    /// Timeout recovery state, if a frozen snapshot exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_recovery: Option<TimeoutRecoveryProjection>,
    /// Timestamp of the last measurement update (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<String>,
}

/// Patch totals and divergence growth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchProjection {
    /// Current HEAD SHA at measurement time.
    pub head_sha: String,
    /// Number of commits between merge base and HEAD.
    pub divergence: u32,
    /// Total changed files (tracked + untracked).
    pub files_changed: u32,
    /// Total added lines across non-binary files.
    pub added_lines: u32,
    /// Number of binary files in the patch.
    pub binary_files: u32,
    /// New source modules.
    pub new_modules: u32,
    /// Dependencies added to configured manifests.
    pub dependencies_added: u32,
    /// Public API additions matching configured regexes.
    pub public_apis_added: u32,
    /// Growth since the prior distinct measurement round (issue 142). Present
    /// only after a second distinct measurement snapshot has been recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub growth: Option<PatchGrowthProjection>,
}

/// Growth deltas computed by subtracting the prior distinct measurement from
/// the current measurement. Positive deltas indicate growth; negative deltas
/// indicate reduction. This is a pure projection derived from the persisted
/// prior snapshot — never an in-memory or display-only computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchGrowthProjection {
    /// Change in changed-files count since the prior snapshot.
    pub files_changed_delta: i64,
    /// Change in added-lines count since the prior snapshot.
    pub added_lines_delta: i64,
    /// Change in new-module count since the prior snapshot.
    pub new_modules_delta: i64,
    /// Change in dependencies-added count since the prior snapshot.
    pub dependencies_added_delta: i64,
    /// Change in public-API-additions count since the prior snapshot.
    pub public_apis_added_delta: i64,
    /// Change in divergence (commits since merge base) since the prior
    /// snapshot.
    pub divergence_delta: i64,
    /// HEAD sha captured by the prior snapshot.
    pub prior_head_sha: String,
    /// Digest of the prior measurement snapshot.
    pub prior_digest: Option<String>,
    /// RFC 3339 timestamp when the prior snapshot was promoted.
    pub prior_measured_at: Option<String>,
}

/// Scope-decision projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeDecisionProjection {
    /// Stable machine-readable decision state token.
    pub state: ScopeDecisionState,
    /// Whether the patch is within all charter ceilings.
    pub within_budget: bool,
    /// Violation codes (stable strings), if any.
    pub violations: Vec<String>,
    /// Operator decision if a resolution exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ScopeResolutionProjection>,
}

/// Machine-readable scope-decision state token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDecisionState {
    /// Patch is within budget; no expansion needed.
    WithinBudget,
    /// Over-budget and a request is pending operator resolution.
    PendingResolution,
    /// Over-budget and explicitly approved for the current snapshot.
    ApprovedExpandedScope,
    /// Over-budget and frozen: operator declined (split or minimal).
    Frozen,
}

/// Projection of a scope-expansion resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeResolutionProjection {
    /// The decision as a stable string.
    pub decision: String,
    /// Whether this decision authorizes the current over-budget snapshot.
    pub authorizes_expansion: bool,
    /// Operator rationale.
    pub rationale: String,
}

/// Review phase and remaining rounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewProjection {
    /// Current review phase token.
    pub phase: ReviewPhase,
    /// Number of initial full reviews recorded.
    pub initial_reviews: u32,
    /// Number of delta reviews recorded.
    pub delta_reviews: u32,
    /// Number of final acceptance reviews recorded.
    pub final_reviews: u32,
    /// Mutating remediation rounds used.
    pub mutating_remediation_rounds: u32,
    /// Remaining delta reviews (cap minus used, floored at zero).
    pub remaining_delta_reviews: u32,
    /// Remaining mutating remediation rounds (cap minus used, floored at zero).
    pub remaining_mutating_remediation_rounds: u32,
}

/// Current review phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPhase {
    /// No reviews yet.
    NotStarted,
    /// Initial full review recorded; deltas may follow.
    InProgress,
    /// Final acceptance review recorded; no further broad review.
    Completed,
    /// Review caps exhausted; routed to terminal failure.
    Exhausted,
}

/// Timeout-recovery projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeoutRecoveryProjection {
    /// Recovery is required before broad continuation.
    pub recovery_required: bool,
    /// The kind of timeout as a stable string.
    pub timeout_kind: String,
    /// Number of changed paths mapped to a subsystem.
    pub mapped_changes_count: u32,
    /// Number of changed paths not mapped to any subsystem.
    pub unmapped_path_count: u32,
}

/// The maximum number of artifact files this projection will read for a single
/// run. This is a safety guard documenting the bounded nature of the read.
pub const MAX_ARTIFACT_READS: usize = 5;

/// Project the scope-control status for a run from its artifacts.
///
/// This performs a **bounded** read: it checks for the `scope-control/{run_id}/`
/// directory and reads at most [`MAX_ARTIFACT_READS`] well-known files by name
/// (`status.json`, the latest expansion request, the expansion resolution,
/// `review-history.json`, `timeout-snapshot.json`). It never scans directories
/// or reads arbitrary files.
///
/// - If the per-run scope-control directory does not exist, returns
///   [`ScopeControlStatus::Unavailable`].
/// - If `status.json` is missing (directory exists but no status), returns
///   `Unavailable` with a descriptive reason.
/// - If `status.json` exists but cannot be read or parsed, returns
///   [`ScopeControlStatus::Error`] with the underlying message.
/// - Otherwise returns [`ScopeControlStatus::Available`] with the full
///   projection.
///
/// Companion artifacts (expansion request/resolution, review history, timeout
/// snapshot) are best-effort reads: if they are missing, their projections are
/// simply absent. If they exist but are corrupt, the error is surfaced in the
/// overall `Error` variant so the operator is not misled.
pub fn project_scope_status(artifact_root: Option<&str>, run_id: &str) -> ScopeControlStatus {
    let Some(root) = artifact_root.map(Path::new) else {
        return ScopeControlStatus::Unavailable {
            reason: "no artifact root recorded for run".to_string(),
        };
    };
    let dir = scope_control_dir(root, run_id);
    if !dir.exists() {
        return ScopeControlStatus::Unavailable {
            reason: "no scope-control directory for run".to_string(),
        };
    }
    let status_p = status_path(&dir);
    if !status_p.exists() {
        return ScopeControlStatus::Unavailable {
            reason: format!("scope-control directory exists but {STATUS_FILENAME} is absent"),
        };
    }

    let status = match read_json(&status_p) {
        Ok(s) => s,
        Err(err) => {
            return ScopeControlStatus::Error {
                message: format!("corrupt {STATUS_FILENAME} at {}: {err}", status_p.display()),
            };
        }
    };

    let (expansion_request, expansion_resolution) = match read_decision_artifacts(root, run_id) {
        Ok(pair) => pair,
        Err(message) => return ScopeControlStatus::Error { message },
    };
    let (review_history, exhaustion) = match read_review_artifacts(root, run_id) {
        Ok(pair) => pair,
        Err(message) => return ScopeControlStatus::Error { message },
    };
    let timeout_snapshot = match read_timeout_snapshot(root, run_id) {
        Ok(s) => s,
        Err(err) => {
            return ScopeControlStatus::Error {
                message: format!("corrupt timeout-snapshot: {err}"),
            };
        }
    };

    let projection = build_projection(
        &status,
        expansion_request.as_ref(),
        expansion_resolution.as_ref(),
        &review_history,
        exhaustion.as_ref(),
        timeout_snapshot.as_ref(),
    );
    ScopeControlStatus::Available(Box::new(projection))
}

/// Read the expansion request and resolution companion artifacts.
///
/// Returns `(request, resolution)` on success or a corrupt-error message.
fn read_decision_artifacts(
    root: &Path,
    run_id: &str,
) -> Result<
    (
        Option<super::decision::ScopeExpansionRequest>,
        Option<super::decision::ScopeExpansionResolution>,
    ),
    String,
> {
    let expansion_request = read_expansion_request(root, run_id)
        .map_err(|err| format!("corrupt scope-expansion-request: {err}"))?;
    let expansion_resolution = read_expansion_resolution(root, run_id)
        .map_err(|err| format!("corrupt scope-expansion-resolution: {err}"))?;
    Ok((expansion_request, expansion_resolution))
}

/// Read the review history and exhaustion summary companion artifacts.
///
/// Returns `(history, exhaustion)` on success or a corrupt-error message.
fn read_review_artifacts(
    root: &Path,
    run_id: &str,
) -> Result<
    (
        super::review_state::ReviewHistory,
        Option<super::review_state::ReviewExhaustionSummary>,
    ),
    String,
> {
    let review_history = read_review_history(root, run_id)
        .map_err(|err| format!("corrupt review-history: {err}"))?;
    let exhaustion = read_exhaustion_summary(root, run_id)
        .map_err(|err| format!("corrupt review-exhaustion: {err}"))?;
    Ok((review_history, exhaustion))
}

/// Build the projection from the read artifacts.
///
/// This is a pure function over already-read data — no I/O — so it is trivially
/// testable and deterministic.
fn build_projection(
    status: &ScopeStatus,
    expansion_request: Option<&super::decision::ScopeExpansionRequest>,
    expansion_resolution: Option<&super::decision::ScopeExpansionResolution>,
    review_history: &super::review_state::ReviewHistory,
    exhaustion: Option<&super::review_state::ReviewExhaustionSummary>,
    timeout_snapshot: Option<&super::timeout_recovery::TimeoutSnapshot>,
) -> ScopeControlProjection {
    let patch = status.measurement.as_ref().map(|m| {
        let growth = status.prior_measurement.as_ref().map(|prior| {
            compute_growth(
                m,
                prior,
                &status.prior_measurement_digest,
                status.prior_measured_at,
            )
        });
        PatchProjection {
            head_sha: m.head_sha.clone(),
            divergence: m.divergence,
            files_changed: m.files_changed,
            added_lines: m.added_lines,
            binary_files: m.binary_files,
            new_modules: m.new_modules,
            dependencies_added: m.dependencies_added,
            public_apis_added: m.public_apis_added,
            growth,
        }
    });

    let decision = build_decision_projection(status, expansion_request, expansion_resolution);

    let review = build_review_projection(review_history, exhaustion);

    let timeout_recovery = timeout_snapshot.map(|snap| {
        let recovery = super::timeout_recovery::TimeoutRecoveryStatus::from_snapshot(snap);
        TimeoutRecoveryProjection {
            recovery_required: recovery.recovery_required,
            timeout_kind: format!("{:?}", recovery.timeout_kind),
            mapped_changes_count: recovery.mapped_changes_count,
            unmapped_path_count: recovery.unmapped_path_count,
        }
    });

    ScopeControlProjection {
        charter_id: status.charter_id.clone(),
        charter_digest: status.digest.clone(),
        merge_base: status.merge_base.clone(),
        measured: status.measurement.is_some(),
        patch,
        decision,
        review,
        timeout_recovery,
        measured_at: status.measured_at.map(|dt| dt.to_rfc3339()),
    }
}

/// Compute growth deltas between the current measurement and the prior distinct
/// snapshot (issue 142).
///
/// Deltas are computed as signed `i64` values (`current - prior`) so reductions
/// are represented as negative numbers. Saturation is applied at the `u32` →
/// `i64` boundary so valid measurements never overflow.
#[must_use]
fn compute_growth(
    current: &super::measurement::PatchMeasurement,
    prior: &super::measurement::PatchMeasurement,
    prior_digest: &Option<String>,
    prior_measured_at: Option<chrono::DateTime<chrono::Utc>>,
) -> PatchGrowthProjection {
    PatchGrowthProjection {
        files_changed_delta: i64::from(current.files_changed) - i64::from(prior.files_changed),
        added_lines_delta: i64::from(current.added_lines) - i64::from(prior.added_lines),
        new_modules_delta: i64::from(current.new_modules) - i64::from(prior.new_modules),
        dependencies_added_delta: i64::from(current.dependencies_added)
            - i64::from(prior.dependencies_added),
        public_apis_added_delta: i64::from(current.public_apis_added)
            - i64::from(prior.public_apis_added),
        divergence_delta: i64::from(current.divergence) - i64::from(prior.divergence),
        prior_head_sha: prior.head_sha.clone(),
        prior_digest: prior_digest.clone(),
        prior_measured_at: prior_measured_at.map(|dt| dt.to_rfc3339()),
    }
}

/// Determine the scope-decision projection from the status evaluation and any
/// expansion request/resolution.
fn build_decision_projection(
    status: &ScopeStatus,
    expansion_request: Option<&super::decision::ScopeExpansionRequest>,
    expansion_resolution: Option<&super::decision::ScopeExpansionResolution>,
) -> ScopeDecisionProjection {
    let within_budget = status
        .evaluation
        .as_ref()
        .is_some_and(|e| e.within_budget && e.violations.is_empty());

    let violations: Vec<String> = status
        .evaluation
        .as_ref()
        .map(|e| {
            e.violations
                .iter()
                .map(|v| v.code.as_str().to_string())
                .collect()
        })
        .unwrap_or_default();

    // No measurement yet → no decision applicable.
    if status.measurement.is_none() {
        return ScopeDecisionProjection {
            state: ScopeDecisionState::WithinBudget,
            within_budget: true,
            violations,
            resolution: None,
        };
    }

    if within_budget {
        return ScopeDecisionProjection {
            state: ScopeDecisionState::WithinBudget,
            within_budget: true,
            violations,
            resolution: expansion_resolution.map(resolution_projection),
        };
    }

    // Over-budget: determine pending vs resolved.
    let state = match (expansion_request.is_some(), expansion_resolution) {
        (false, _) => ScopeDecisionState::PendingResolution,
        (true, None) => ScopeDecisionState::PendingResolution,
        (true, Some(r)) => {
            if r.decision.authorizes_expansion() {
                ScopeDecisionState::ApprovedExpandedScope
            } else {
                ScopeDecisionState::Frozen
            }
        }
    };

    ScopeDecisionProjection {
        state,
        within_budget: false,
        violations,
        resolution: expansion_resolution.map(resolution_projection),
    }
}

/// Map a resolution to its projection.
fn resolution_projection(
    r: &super::decision::ScopeExpansionResolution,
) -> ScopeResolutionProjection {
    ScopeResolutionProjection {
        decision: format!("{}", r.decision),
        authorizes_expansion: r.decision.authorizes_expansion(),
        rationale: r.rationale.clone(),
    }
}

/// Build the review projection from history and any exhaustion summary.
///
/// The caps needed to compute "remaining" are read from the exhaustion summary
/// when present (it carries the canonical caps). If no exhaustion summary
/// exists, we can still report counts and phase but not remaining-round
/// deltas; remaining fields are set to zero in that case.
fn build_review_projection(
    history: &super::review_state::ReviewHistory,
    exhaustion: Option<&super::review_state::ReviewExhaustionSummary>,
) -> Option<ReviewProjection> {
    // If there is no review history file and no exhaustion summary, there is
    // nothing meaningful to report.
    if history.reviews.is_empty() && exhaustion.is_none() {
        return None;
    }

    let initial = super::review_state::count_by_kind(history, ReviewKind::InitialFull);
    let delta = super::review_state::count_by_kind(history, ReviewKind::Delta);
    let final_reviews = super::review_state::count_by_kind(history, ReviewKind::FinalAcceptance);
    let mutating = history.mutating_remediation_rounds;

    let phase = if exhaustion.is_some() {
        ReviewPhase::Exhausted
    } else if final_reviews > 0 {
        ReviewPhase::Completed
    } else if initial > 0 {
        ReviewPhase::InProgress
    } else {
        ReviewPhase::NotStarted
    };

    let (remaining_delta, remaining_mutating) = match exhaustion {
        Some(summary) => (
            summary
                .caps
                .max_delta_reviews
                .saturating_sub(summary.delta_reviews),
            summary
                .caps
                .max_mutating_remediation_rounds
                .saturating_sub(summary.mutating_remediation_rounds),
        ),
        None => (0, 0),
    };

    Some(ReviewProjection {
        phase,
        initial_reviews: initial,
        delta_reviews: delta,
        final_reviews,
        mutating_remediation_rounds: mutating,
        remaining_delta_reviews: remaining_delta,
        remaining_mutating_remediation_rounds: remaining_mutating,
    })
}

/// Convert a [`ScopeControlStatus`] to a JSON value for status/runs-show
/// output.
///
/// - `Unavailable` → `null` (no scope-control data; compatible with historical
///   runs).
/// - `Available` → serialized projection object.
/// - `Error` → an object with an `error` field so the corruption is surfaced.
#[must_use]
pub fn scope_status_to_json(status: &ScopeControlStatus) -> serde_json::Value {
    match status {
        ScopeControlStatus::Unavailable { .. } => serde_json::Value::Null,
        ScopeControlStatus::Available(projection) => serde_json::to_value(&**projection)
            .unwrap_or_else(
                |_| serde_json::json!({"error": "failed to serialize scope-control projection"}),
            ),
        ScopeControlStatus::Error { message } => {
            serde_json::json!({"error": message})
        }
    }
}

/// Convert a [`ScopeControlStatus`] to a human-readable summary string for
/// terminal output.
#[must_use]
pub fn scope_status_to_human(status: &ScopeControlStatus) -> String {
    match status {
        ScopeControlStatus::Unavailable { reason } => {
            format!("Scope Control: unavailable ({reason})")
        }
        ScopeControlStatus::Error { message } => {
            format!("Scope Control: ERROR — {message}")
        }
        ScopeControlStatus::Available(p) => format_projection_human(p),
    }
}

/// Format a full projection as a human-readable, indented block.
fn format_projection_human(p: &ScopeControlProjection) -> String {
    let mut lines = Vec::new();
    lines.push("Scope Control:".to_string());
    let digest_preview: String = p.charter_digest.chars().take(12).collect();
    lines.push(format!("  Charter: {} ({digest_preview})", p.charter_id));
    lines.push(format!("  Merge base: {}", p.merge_base));

    if !p.measured {
        lines.push("  Measurement: pending (no measurement yet)".to_string());
        lines.push(format!(
            "  Decision: {}",
            decision_state_label(&p.decision.state)
        ));
        return lines.join("\n");
    }

    push_patch_lines(&mut lines, &p.patch);
    push_decision_lines(&mut lines, &p.decision);
    push_review_lines(&mut lines, &p.review);
    push_timeout_recovery_lines(&mut lines, &p.timeout_recovery);

    if let Some(measured_at) = &p.measured_at {
        lines.push(format!("  Measured at: {measured_at}"));
    }

    lines.join("\n")
}

/// Append patch (HEAD, divergence, totals, growth) summary lines.
fn push_patch_lines(lines: &mut Vec<String>, patch: &Option<PatchProjection>) {
    let Some(patch) = patch else { return };
    lines.push(format!("  HEAD: {}", patch.head_sha));
    lines.push(format!("  Divergence: {} commits", patch.divergence));
    lines.push(format!(
        "  Patch: {} files, +{} lines, {} new modules, {} deps, {} APIs ({} binary)",
        patch.files_changed,
        patch.added_lines,
        patch.new_modules,
        patch.dependencies_added,
        patch.public_apis_added,
        patch.binary_files,
    ));
    if let Some(growth) = &patch.growth {
        push_growth_lines(lines, growth);
    }
}

/// Append growth-since-prior-round summary lines.
fn push_growth_lines(lines: &mut Vec<String>, growth: &PatchGrowthProjection) {
    lines.push(format!(
        "  Growth since prior round: {} files, {} lines, {} modules, {} deps, {} APIs, {} commits",
        format_signed_delta(growth.files_changed_delta),
        format_signed_delta(growth.added_lines_delta),
        format_signed_delta(growth.new_modules_delta),
        format_signed_delta(growth.dependencies_added_delta),
        format_signed_delta(growth.public_apis_added_delta),
        format_signed_delta(growth.divergence_delta),
    ));
    if let Some(ts) = &growth.prior_measured_at {
        lines.push(format!(
            "    Prior snapshot: {} ({ts})",
            growth.prior_head_sha
        ));
    }
}

/// Append scope-decision summary lines.
fn push_decision_lines(lines: &mut Vec<String>, decision: &ScopeDecisionProjection) {
    lines.push(format!(
        "  Decision: {}{}",
        decision_state_label(&decision.state),
        if decision.within_budget {
            String::new()
        } else {
            format!(" — violations: {}", decision.violations.join(", "))
        },
    ));
    if let Some(res) = &decision.resolution {
        lines.push(format!(
            "    Resolution: {} ({})",
            res.decision, res.rationale
        ));
    }
}

/// Append review phase and round summary lines.
fn push_review_lines(lines: &mut Vec<String>, review: &Option<ReviewProjection>) {
    let Some(review) = review else { return };
    lines.push(format!("  Review: {}", review_phase_label(review.phase)));
    lines.push(format!(
        "    Rounds: {} initial, {} delta, {} final ({} mutating remediation)",
        review.initial_reviews,
        review.delta_reviews,
        review.final_reviews,
        review.mutating_remediation_rounds,
    ));
    lines.push(format!(
        "    Remaining: {} delta, {} mutating remediation",
        review.remaining_delta_reviews, review.remaining_mutating_remediation_rounds,
    ));
}

/// Append timeout-recovery summary lines.
fn push_timeout_recovery_lines(
    lines: &mut Vec<String>,
    timeout: &Option<TimeoutRecoveryProjection>,
) {
    let Some(timeout) = timeout else { return };
    lines.push(format!(
        "  Timeout Recovery: {} ({}, {} unmapped paths)",
        if timeout.recovery_required {
            "required"
        } else {
            "not required"
        },
        timeout.timeout_kind,
        timeout.unmapped_path_count,
    ));
}

/// Human-readable label for a scope-decision state.
fn decision_state_label(state: &ScopeDecisionState) -> &'static str {
    match state {
        ScopeDecisionState::WithinBudget => "within budget",
        ScopeDecisionState::PendingResolution => "pending scope decision (over budget)",
        ScopeDecisionState::ApprovedExpandedScope => "approved expanded scope",
        ScopeDecisionState::Frozen => "frozen (scope decision denied)",
    }
}

/// Human-readable label for a review phase.
fn review_phase_label(phase: ReviewPhase) -> &'static str {
    match phase {
        ReviewPhase::NotStarted => "not started",
        ReviewPhase::InProgress => "in progress",
        ReviewPhase::Completed => "completed (final acceptance recorded)",
        ReviewPhase::Exhausted => "exhausted (caps reached)",
    }
}

/// Format a signed delta for human output, always including an explicit sign
/// so zero deltas are unambiguous (`+0`).
#[must_use]
fn format_signed_delta(delta: i64) -> String {
    format!("{delta:+}")
}

/// Resolve the scope-control directory path for diagnostics. Exposed for
/// tests that need to verify the expected artifact location.
#[must_use]
pub fn diagnostic_scope_dir(artifact_root: &str, run_id: &str) -> PathBuf {
    scope_control_dir(Path::new(artifact_root), run_id)
}

#[cfg(test)]
#[path = "status_projection_tests.rs"]
mod tests;
