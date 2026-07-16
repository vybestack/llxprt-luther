//! Durable scope-decision gate (issue 142, slice 3).
//!
//! When a patch measurement exceeds the charter's ceilings the executor yields
//! [`StepOutcome::Wait`](crate::engine::transition::StepOutcome::Wait) and a durable,
//! immutable *scope expansion request*
//! is written to disk. An operator resolves the request by writing a
//! *resolution* artifact. Only an `approve_expanded_scope` decision that
//! matches the exact measurement digest authorizes the over-budget mutation.
//! Any patch change invalidates the approval because the digest is derived
//! from the measurement snapshot.
//!
//! Requests and resolutions are persisted through the existing atomic
//! immutable/updatable scope-control paths — no new persistence machinery.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evaluation::{ScopeEvaluation, Violation};
use super::measurement::{
    collect_dependency_diffs, compute_measurement, GitPatchCollector, PatchMeasurement,
};
use super::model::CanonicalTaskCharter;
use super::persistence::{
    charter_path, read_json, scope_control_dir, write_immutable_json, write_updatable_json,
    PersistenceError,
};

/// Filename for the scope expansion request artifact.
pub const EXPANSION_REQUEST_FILENAME: &str = "scope-expansion-request.json";

/// Filename for the scope expansion resolution artifact.
pub const EXPANSION_RESOLUTION_FILENAME: &str = "scope-expansion-resolution.json";

/// Operator decision for an over-budget scope expansion request.
///
/// Only `ApproveExpandedScope` authorizes a mutation that exceeds the
/// charter's ceilings — and only against the exact measurement digest captured
/// in the request. The other variants are explicit non-approvals that keep the
/// mutation blocked until the patch is reduced to within-budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeExpansionDecision {
    /// Authorize the exact over-budget snapshot captured in the request.
    ApproveExpandedScope,
    /// Decline expansion and require the work to be split into a follow-up
    /// issue. The current patch must be reduced before mutation is allowed.
    SplitFollowUpIssue,
    /// Decline expansion and require a return to minimal within-budget
    /// implementation. The current patch must be reduced before mutation is
    /// allowed.
    ReturnToMinimalImplementation,
}

impl ScopeExpansionDecision {
    /// Whether this decision authorizes an over-budget mutation against the
    /// matching snapshot.
    #[must_use]
    pub const fn authorizes_expansion(self) -> bool {
        matches!(self, Self::ApproveExpandedScope)
    }
}

impl std::fmt::Display for ScopeExpansionDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::ApproveExpandedScope => "approve_expanded_scope",
            Self::SplitFollowUpIssue => "split_follow_up_issue",
            Self::ReturnToMinimalImplementation => "return_to_minimal_implementation",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for ScopeExpansionDecision {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "approve_expanded_scope" => Ok(Self::ApproveExpandedScope),
            "split_follow_up_issue" => Ok(Self::SplitFollowUpIssue),
            "return_to_minimal_implementation" => Ok(Self::ReturnToMinimalImplementation),
            other => Err(format!("unknown scope expansion decision: {other}")),
        }
    }
}

/// Durable, immutable scope expansion request capturing the exact over-budget
/// measurement snapshot that must be resolved before mutation proceeds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeExpansionRequest {
    pub run_id: String,
    pub charter_id: String,
    pub charter_digest: String,
    /// Deterministic digest of the measurement snapshot. A resolution applies
    /// only when the current measurement digest matches this value.
    pub measurement_digest: String,
    pub measurement: PatchMeasurement,
    pub evaluation: ScopeEvaluation,
    pub violations: Vec<Violation>,
    pub created_at: DateTime<Utc>,
}

/// Durable resolution for a scope expansion request.
///
/// Persisted as an updatable artifact so a changed decision overwrites the
/// prior resolution atomically. The `measurement_digest` binds the resolution
/// to the exact measurement snapshot it resolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeExpansionResolution {
    pub run_id: String,
    pub measurement_digest: String,
    pub decision: ScopeExpansionDecision,
    pub rationale: String,
    pub resolved_at: DateTime<Utc>,
}

/// Compute a deterministic SHA-256 digest for a patch measurement.
///
/// The digest is derived from the canonical JSON serialization of the
/// measurement. Two structurally identical measurements produce identical
/// digests; any patch change alters the measurement and therefore the digest,
/// invalidating prior approvals.
#[must_use]
pub fn measurement_digest(measurement: &PatchMeasurement) -> String {
    let serialized = serde_json::to_vec(measurement).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Directory holding per-generation immutable request artifacts.
const REQUESTS_DIRNAME: &str = "scope-expansion-requests";

fn requests_dir(dir: &Path) -> PathBuf {
    dir.join(REQUESTS_DIRNAME)
}

fn request_path(dir: &Path) -> PathBuf {
    dir.join(EXPANSION_REQUEST_FILENAME)
}

fn resolution_path(dir: &Path) -> PathBuf {
    dir.join(EXPANSION_RESOLUTION_FILENAME)
}

/// Write a scope expansion request as an immutable artifact using durable
/// request generations.
///
/// Instead of a single fixed filename that deadlocks when the patch changes
/// (producing a new measurement digest), each measurement digest gets its own
/// immutable generation file inside `scope-expansion-requests/`. This allows
/// a stale snapshot to be superseded by a new generation without conflict,
/// while each individual generation file remains immutable (create-new,
/// no-replace, idempotent).
///
/// The generation filename is derived from the measurement digest so the
/// write is idempotent: two writers racing with the same request both target
/// the same file and the loser's `create_new` fails harmlessly (the file
/// already exists with identical content). The containing directory is
/// fsynced where supported (via [`write_immutable_json`]).
pub fn write_expansion_request(
    artifact_dir: &Path,
    request: &ScopeExpansionRequest,
) -> Result<(), PersistenceError> {
    let dir = scope_control_dir(artifact_dir, &request.run_id);
    let gen_dir = requests_dir(&dir);
    std::fs::create_dir_all(&gen_dir)?;
    let gen_path = generation_request_path(&gen_dir, &request.measurement_digest);

    // Race-safe idempotent write: if the generation file already exists with
    // the same digest, it's a replay and succeeds. If it doesn't exist, we
    // create it with O_EXCL so concurrent writers race on the atomic create.
    if gen_path.exists() {
        let existing: ScopeExpansionRequest = read_json(&gen_path)?;
        if existing.measurement_digest == request.measurement_digest {
            return Ok(());
        }
        // Same filename hash collision with different content: reject. This
        // should never happen because the filename IS the digest, but we fail
        // closed for safety.
        return Err(PersistenceError::Conflict {
            path: gen_path,
            message: "generation file digest mismatch".into(),
        });
    }
    write_immutable_json(&gen_path, request)
}

/// Resolve the per-generation request path from a measurement digest.
fn generation_request_path(gen_dir: &Path, digest: &str) -> PathBuf {
    gen_dir.join(format!("{digest}.json"))
}

/// Read the **latest** scope expansion request for a run.
///
/// When durable generations are present (under
/// `scope-expansion-requests/`), the most recently written generation is
/// returned. For backward compatibility, the legacy single-file
/// `scope-expansion-request.json` is consulted if no generation directory
/// exists.
pub fn read_expansion_request(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<Option<ScopeExpansionRequest>, PersistenceError> {
    let dir = scope_control_dir(artifact_dir, run_id);
    let gen_dir = requests_dir(&dir);
    if gen_dir.is_dir() {
        return read_latest_generation(&gen_dir);
    }
    // Backward compatibility: legacy single-file request.
    let path = request_path(&dir);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

/// Read the most recent generation from the requests directory.
///
/// Generations are ordered by their file modification time (most recent
/// first). If two files share the same mtime (possible on fast filesystems),
/// the lexicographically larger digest wins as a tie-breaker, which is
/// deterministic.
fn read_latest_generation(
    gen_dir: &Path,
) -> Result<Option<ScopeExpansionRequest>, PersistenceError> {
    let mut entries: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in std::fs::read_dir(gen_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push((mtime, name));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    // Sort: most recent mtime first, then lexicographic name descending.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let latest_path = gen_dir.join(&entries[0].1);
    read_json(&latest_path).map(Some)
}

/// Write or overwrite a scope expansion resolution atomically.
pub fn write_expansion_resolution(
    artifact_dir: &Path,
    resolution: &ScopeExpansionResolution,
) -> Result<(), PersistenceError> {
    let dir = scope_control_dir(artifact_dir, &resolution.run_id);
    let path = resolution_path(&dir);
    write_updatable_json(&path, resolution)
}

/// Read the scope expansion resolution for a run, if one exists.
pub fn read_expansion_resolution(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<Option<ScopeExpansionResolution>, PersistenceError> {
    let dir = scope_control_dir(artifact_dir, run_id);
    let path = resolution_path(&dir);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

/// Outcome of checking the scope-decision gate for a mutation barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeGateOutcome {
    /// No scope-control artifacts exist; the barrier is a no-op.
    NoScopeControl,
    /// The patch is within budget; mutation is allowed.
    WithinBudget,
    /// The patch is within budget and a prior expansion request has been
    /// resolved (approved or declined). Mutation is allowed because the patch
    /// no longer exceeds the charter.
    WithinBudgetWithPriorResolution,
    /// The patch is over-budget and no request exists yet. The executor must
    /// yield `StepOutcome::Wait` so the request can be created and resolved.
    OverBudgetNeedsRequest,
    /// The patch is over-budget and the request is pending resolution.
    PendingResolution,
    /// The patch is over-budget and the resolution does not authorize
    /// expansion (split or minimal). Mutation remains blocked.
    Denied(ScopeExpansionDecision),
}

impl ScopeGateOutcome {
    /// Whether this outcome allows the mutation to proceed.
    #[must_use]
    pub const fn allows_mutation(&self) -> bool {
        matches!(
            self,
            Self::NoScopeControl | Self::WithinBudget | Self::WithinBudgetWithPriorResolution
        )
    }
}

/// Check the scope-decision gate using a freshly re-measured snapshot.
///
/// This is the shared barrier invoked at mutation entry points and
/// immediately before push. It re-measures the current worktree, evaluates
/// against the charter, and compares the result to any existing expansion
/// request/resolution.
///
/// No PR identity is required — the gate operates purely on the scope-control
/// artifacts and worktree measurement.
pub fn check_scope_gate(
    artifact_dir: &Path,
    run_id: &str,
    charter: &CanonicalTaskCharter,
    measurement: &PatchMeasurement,
    evaluation: &ScopeEvaluation,
) -> Result<ScopeGateOutcome, PersistenceError> {
    if evaluation.within_budget && evaluation.violations.is_empty() {
        return if read_expansion_request(artifact_dir, run_id)?.is_some() {
            Ok(ScopeGateOutcome::WithinBudgetWithPriorResolution)
        } else {
            Ok(ScopeGateOutcome::WithinBudget)
        };
    }

    // Over-budget: check request and resolution.
    let digest = measurement_digest(measurement);
    let Some(request) = read_expansion_request(artifact_dir, run_id)? else {
        return Ok(ScopeGateOutcome::OverBudgetNeedsRequest);
    };
    let Some(resolution) = read_expansion_resolution(artifact_dir, run_id)? else {
        return Ok(ScopeGateOutcome::PendingResolution);
    };

    // Approval applies only to the exact charter and measurement snapshot.
    if request.charter_digest != charter.digest
        || resolution.measurement_digest != digest
        || request.measurement_digest != digest
    {
        return Ok(ScopeGateOutcome::OverBudgetNeedsRequest);
    }

    if resolution.decision.authorizes_expansion() {
        // The approved snapshot matches; allow the over-budget mutation.
        Ok(ScopeGateOutcome::WithinBudgetWithPriorResolution)
    } else {
        Ok(ScopeGateOutcome::Denied(resolution.decision))
    }
}

/// Create a scope expansion request from a measurement and evaluation.
#[must_use]
pub fn build_expansion_request(
    run_id: &str,
    charter: &CanonicalTaskCharter,
    measurement: &PatchMeasurement,
    evaluation: &ScopeEvaluation,
) -> ScopeExpansionRequest {
    ScopeExpansionRequest {
        run_id: run_id.to_string(),
        charter_id: charter.charter_id.clone(),
        charter_digest: charter.digest.clone(),
        measurement_digest: measurement_digest(measurement),
        measurement: measurement.clone(),
        evaluation: evaluation.clone(),
        violations: evaluation.violations.clone(),
        created_at: Utc::now(),
    }
}

/// Result of the scope barrier check at a mutation entry point.
///
/// The barrier is compactly enforced through a single call that re-measures
/// the worktree, evaluates against the charter, and checks the gate. The
/// caller yields `StepOutcome::Wait` when `Blocked` is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeBarrierResult {
    /// Mutation may proceed: within budget or an exact-match approval exists.
    Allow,
    /// Mutation is blocked pending an operator decision. The caller must return
    /// `StepOutcome::Wait`. A request is persisted when needed.
    Blocked,
    /// The operator declined the expansion. This is terminal for the unchanged
    /// patch; waiting again cannot change the resolved decision.
    Denied(ScopeExpansionDecision),
}

/// Enforce the scope-decision barrier at a mutation entry point or pre-push.
///
/// This is the shared barrier invoked compactly at broad mutation/pre-push
/// executors rather than editing many leaf executors. It:
///
/// 1. Resolves the artifact directory from context.
/// 2. Loads the required charter, failing closed if it is unavailable.
/// 3. Re-measures the worktree against the charter using the supplied
///    collector.
/// 4. Evaluates the measurement.
/// 5. Checks the gate: if over-budget without a matching approval, writes a
///    durable request (idempotently) and returns `Blocked`.
///
/// No PR identity is required.
pub fn enforce_scope_barrier(
    context: &crate::engine::executor::StepContext,
    collector: &dyn GitPatchCollector,
    scope_control: &crate::workflow::schema::ScopeControlConfig,
) -> Result<ScopeBarrierResult, crate::engine::runner::EngineError> {
    use crate::engine::executors::scope_control::evaluation::evaluate;

    let artifact_dir = resolve_artifact_dir(context);
    let run_id = context.run_id();
    let charter = load_charter(&artifact_dir, run_id)
        .map_err(|err| barrier_error(format!("scope charter unavailable: {err}")))?;
    let git_data = collector
        .collect(
            context.work_dir(),
            &charter.merge_base,
            &scope_control.measurement,
        )
        .map_err(|err| barrier_error(err.to_string()))?;
    let dependency_diffs = collect_dependency_diffs(
        context.work_dir(),
        &scope_control.dependency_manifests,
        &charter.merge_base,
    )
    .map_err(|err| barrier_error(err.to_string()))?;
    let measurement = compute_measurement(
        &git_data,
        &charter,
        &scope_control.measurement,
        context.work_dir(),
        &dependency_diffs,
    )
    .map_err(|err| barrier_error(err.to_string()))?;
    let evaluation = evaluate(&measurement, &charter);
    let outcome = check_scope_gate(&artifact_dir, run_id, &charter, &measurement, &evaluation)
        .map_err(|err| barrier_error(err.to_string()))?;
    if outcome.allows_mutation() {
        Ok(ScopeBarrierResult::Allow)
    } else {
        match outcome {
            ScopeGateOutcome::OverBudgetNeedsRequest | ScopeGateOutcome::PendingResolution => {
                let request = build_expansion_request(run_id, &charter, &measurement, &evaluation);
                // Fail closed: if the request cannot be persisted, the barrier
                // must not yield Wait (which would create an unresolvable wait
                // state — the operator cannot resolve a request that was never
                // written). Propagate the error so the executor reports a
                // fatal failure instead.
                write_expansion_request(&artifact_dir, &request)
                    .map_err(|err| barrier_error(err.to_string()))?;
                Ok(ScopeBarrierResult::Blocked)
            }
            ScopeGateOutcome::Denied(decision) => Ok(ScopeBarrierResult::Denied(decision)),
            _ => Ok(ScopeBarrierResult::Allow),
        }
    }
}

fn resolve_artifact_dir(context: &crate::engine::executor::StepContext) -> std::path::PathBuf {
    context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.work_dir().clone())
}

fn load_charter(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<CanonicalTaskCharter, PersistenceError> {
    let dir = scope_control_dir(artifact_dir, run_id);
    read_json::<CanonicalTaskCharter>(&charter_path(&dir))
}

fn barrier_error(message: String) -> crate::engine::runner::EngineError {
    crate::engine::runner::EngineError::StepExecutionError {
        step_id: "scope_barrier".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executors::scope_control::evaluation::ViolationCode;
    use crate::engine::executors::scope_control::measurement::{ChangeStatus, FileChange};
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    use tempfile::TempDir;

    fn sample_charter() -> CanonicalTaskCharter {
        let draft = TaskCharterDraft {
            charter_id: "T".into(),
            issue_number: 1,
            run_id: "r".into(),
            merge_base: "abc".into(),
            acceptance_criteria: vec!["AC".into()],
            non_goals: vec!["NG".into()],
            subsystems: vec![DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            budget: DraftBudget {
                max_files_changed: 5,
                max_added_lines: 100,
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
        normalize_charter(&draft)
    }

    fn within_budget_measurement() -> PatchMeasurement {
        PatchMeasurement {
            merge_base: "abc".into(),
            head_sha: "abc".into(),
            divergence: 0,
            files_changed: 2,
            added_lines: 50,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            content_digest: String::new(),
            public_apis_added: 2,
            changed_paths: vec!["src/core/a.rs".into()],
            changed_subsystems: vec!["core".into()],
            file_details: vec![FileChange {
                path: "src/core/a.rs".into(),
                status: ChangeStatus::Added,
                added_lines: Some(50),
                deleted_lines: Some(0),
                is_binary: false,
            }],
        }
    }

    fn over_budget_measurement() -> PatchMeasurement {
        PatchMeasurement {
            merge_base: "abc".into(),
            head_sha: "abc".into(),
            divergence: 0,
            files_changed: 2,
            added_lines: 500,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            content_digest: String::new(),
            public_apis_added: 2,
            changed_paths: vec!["src/core/a.rs".into()],
            changed_subsystems: vec!["core".into()],
            file_details: vec![FileChange {
                path: "src/core/a.rs".into(),
                status: ChangeStatus::Added,
                added_lines: Some(500),
                deleted_lines: Some(0),
                is_binary: false,
            }],
        }
    }

    fn over_budget_evaluation() -> ScopeEvaluation {
        ScopeEvaluation {
            within_budget: false,
            within_subsystems: true,
            at_merge_base: true,
            violations: vec![Violation {
                code: ViolationCode::BudgetAddedLines,
                message: "added_lines (500) exceeds ceiling (100)".into(),
            }],
        }
    }

    #[test]
    fn measurement_digest_is_deterministic() {
        let m = within_budget_measurement();
        assert_eq!(measurement_digest(&m), measurement_digest(&m));
    }

    #[test]
    fn measurement_digest_changes_on_different_measurement() {
        let m1 = within_budget_measurement();
        let m2 = over_budget_measurement();
        assert_ne!(measurement_digest(&m1), measurement_digest(&m2));
    }

    #[test]
    fn write_and_read_request() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);

        write_expansion_request(tmp.path(), &request).unwrap();
        let read = read_expansion_request(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(read.measurement_digest, request.measurement_digest);
    }

    #[test]
    fn write_request_is_idempotent_for_same_digest() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);

        write_expansion_request(tmp.path(), &request).unwrap();
        write_expansion_request(tmp.path(), &request).unwrap();
    }

    #[test]
    fn write_request_accepts_new_generation_for_different_digest() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let mut different_measurement = measurement.clone();
        different_measurement.added_lines = 600;
        let new_request =
            build_expansion_request("r", &charter, &different_measurement, &evaluation);
        // A different measurement digest writes a new generation file without
        // conflict — generations are immutable and coexist.
        assert_ne!(request.measurement_digest, new_request.measurement_digest);
        write_expansion_request(tmp.path(), &new_request).unwrap();
    }

    #[test]
    fn write_and_read_resolution() {
        let tmp = TempDir::new().unwrap();
        let measurement = over_budget_measurement();
        let digest = measurement_digest(&measurement);
        let resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: digest,
            decision: ScopeExpansionDecision::ApproveExpandedScope,
            rationale: "approved".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &resolution).unwrap();
        let read = read_expansion_resolution(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(read.decision, ScopeExpansionDecision::ApproveExpandedScope);
    }

    #[test]
    fn check_gate_within_budget_allows_mutation() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = within_budget_measurement();
        let evaluation = ScopeEvaluation {
            within_budget: true,
            within_subsystems: true,
            at_merge_base: true,
            violations: vec![],
        };
        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert!(outcome.allows_mutation());
    }

    #[test]
    fn check_gate_over_budget_needs_request() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert_eq!(outcome, ScopeGateOutcome::OverBudgetNeedsRequest);
        assert!(!outcome.allows_mutation());
    }

    #[test]
    fn check_gate_over_budget_pending_resolution() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert_eq!(outcome, ScopeGateOutcome::PendingResolution);
    }

    #[test]
    fn check_gate_exact_approval_resumes() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: request.measurement_digest.clone(),
            decision: ScopeExpansionDecision::ApproveExpandedScope,
            rationale: "ok".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &resolution).unwrap();

        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert!(outcome.allows_mutation());
    }

    #[test]
    fn check_gate_stale_approval_rejected() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let old_measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &old_measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let stale_resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: "deadbeef".to_string(),
            decision: ScopeExpansionDecision::ApproveExpandedScope,
            rationale: "stale".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &stale_resolution).unwrap();

        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &old_measurement, &evaluation).unwrap();
        // Stale digest (doesn't match) → needs new request
        assert_eq!(outcome, ScopeGateOutcome::OverBudgetNeedsRequest);
    }

    #[test]
    fn check_gate_split_remains_blocked() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: request.measurement_digest.clone(),
            decision: ScopeExpansionDecision::SplitFollowUpIssue,
            rationale: "split".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &resolution).unwrap();

        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert_eq!(
            outcome,
            ScopeGateOutcome::Denied(ScopeExpansionDecision::SplitFollowUpIssue)
        );
        assert!(!outcome.allows_mutation());
    }

    #[test]
    fn check_gate_minimal_remains_blocked() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = over_budget_measurement();
        let evaluation = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &measurement, &evaluation);
        write_expansion_request(tmp.path(), &request).unwrap();

        let resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: request.measurement_digest.clone(),
            decision: ScopeExpansionDecision::ReturnToMinimalImplementation,
            rationale: "minimal".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &resolution).unwrap();

        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &measurement, &evaluation).unwrap();
        assert_eq!(
            outcome,
            ScopeGateOutcome::Denied(ScopeExpansionDecision::ReturnToMinimalImplementation)
        );
    }

    #[test]
    fn split_then_reduced_allows_mutation() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let over_measurement = over_budget_measurement();
        let evaluation_over = over_budget_evaluation();
        let request = build_expansion_request("r", &charter, &over_measurement, &evaluation_over);
        write_expansion_request(tmp.path(), &request).unwrap();
        let resolution = ScopeExpansionResolution {
            run_id: "r".into(),
            measurement_digest: request.measurement_digest.clone(),
            decision: ScopeExpansionDecision::SplitFollowUpIssue,
            rationale: "split".into(),
            resolved_at: Utc::now(),
        };
        write_expansion_resolution(tmp.path(), &resolution).unwrap();

        // Patch reduced — within budget now.
        let reduced = within_budget_measurement();
        let evaluation_ok = ScopeEvaluation {
            within_budget: true,
            within_subsystems: true,
            at_merge_base: true,
            violations: vec![],
        };
        let outcome =
            check_scope_gate(tmp.path(), "r", &charter, &reduced, &evaluation_ok).unwrap();
        // Within budget + prior resolution → allowed
        assert!(outcome.allows_mutation());
    }

    #[test]
    fn decision_display_and_fromstr_roundtrip() {
        for decision in [
            ScopeExpansionDecision::ApproveExpandedScope,
            ScopeExpansionDecision::SplitFollowUpIssue,
            ScopeExpansionDecision::ReturnToMinimalImplementation,
        ] {
            let s = decision.to_string();
            let parsed: ScopeExpansionDecision = s.parse().unwrap();
            assert_eq!(parsed, decision);
        }
    }

    #[test]
    fn only_approve_authorizes_expansion() {
        assert!(ScopeExpansionDecision::ApproveExpandedScope.authorizes_expansion());
        assert!(!ScopeExpansionDecision::SplitFollowUpIssue.authorizes_expansion());
        assert!(!ScopeExpansionDecision::ReturnToMinimalImplementation.authorizes_expansion());
    }
}
