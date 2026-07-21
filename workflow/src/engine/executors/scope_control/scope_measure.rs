//! Scope-measurement step executor (issue 142, slice 2).
//!
//! This executor is registered under the step type `"scope_measure"`. It:
//!
//! 1. Loads the charter (from context or artifact path).
//! 2. Collects Git patch data against the charter's frozen merge base.
//! 3. Computes the patch measurement (changed files, added lines, new modules,
//!    dependencies added, public APIs).
//! 4. Evaluates the measurement against the charter's ceilings.
//! 5. Persists/updates the scope-control status read model crash-safely.
//! 6. Exposes patch growth, current HEAD, and divergence through context
//!    outputs.
//!
//! Charter immutability is preserved: the charter file is never modified. The
//! status file is updated atomically (temp-file + rename + fsync). Repeated
//! measurements are idempotent in the sense that the same worktree state
//! produces the same measurement result.

use std::path::Path;

use serde_json::Value;

use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::scope_control::config_validation::active_scope_control;
use crate::engine::executors::scope_control::decision::{
    build_expansion_request, check_scope_gate, write_expansion_request, ScopeGateOutcome,
};
use crate::engine::executors::scope_control::evaluation::evaluate;
use crate::engine::executors::scope_control::measurement::{
    collect_dependency_diffs, compute_measurement, GitPatchCollector, SystemGitPatchCollector,
};
use crate::engine::executors::scope_control::model::CanonicalTaskCharter;
use crate::engine::executors::scope_control::persistence::{
    charter_path, read_json, scope_control_dir, update_status_measurement,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::workflow::schema::{ScopeControlConfig, TargetProfileConfig};

/// Scope-measurement step executor.
///
/// Constructed with an injectable [`GitPatchCollector`] so tests can avoid
/// real Git or use temporary Git repositories. Registered under the step type
/// `"scope_measure"`.
pub struct ScopeMeasureExecutor {
    collector: Box<dyn GitPatchCollector>,
}

impl ScopeMeasureExecutor {
    /// Create with the system (production) Git patch collector.
    #[must_use]
    pub fn with_system_collector() -> Self {
        Self {
            collector: Box::new(SystemGitPatchCollector),
        }
    }

    /// Create with a custom collector (for tests).
    #[must_use]
    pub fn with_collector(collector: Box<dyn GitPatchCollector>) -> Self {
        Self { collector }
    }
}

impl StepExecutor for ScopeMeasureExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        let Some(scope_control) = resolve_scope_control(context, params)? else {
            // Scope control is absent or disabled: no-op successfully so shared
            // workflows for other profiles remain compatible.
            return Ok(StepOutcome::Success);
        };
        let work_dir = context.work_dir().clone();
        let run_id = context.run_id().to_string();
        let artifact_dir = resolve_artifact_dir(context)?;

        // Load the canonical charter from the persisted artifact.
        let charter = load_charter(&artifact_dir, &run_id)?;

        // Collect Git patch data against the frozen merge base.
        let git_data = self
            .collector
            .collect(&work_dir, &charter.merge_base, &scope_control.measurement)
            .map_err(|err| measure_error(err.to_string()))?;

        // Collect dependency manifest diffs.
        let dependency_diffs = collect_dependency_diffs(
            &work_dir,
            &scope_control.dependency_manifests,
            &charter.merge_base,
        )
        .map_err(|err| measure_error(err.to_string()))?;

        // Compute the patch measurement.
        let measurement = compute_measurement(
            &git_data,
            &charter,
            &run_id,
            context.daemon_managed_claim(),
            &scope_control.measurement,
            &work_dir,
            &dependency_diffs,
        )
        .map_err(|err| measure_error(err.to_string()))?;

        // Evaluate against the charter.
        let evaluation = evaluate(&measurement, &charter);

        // Persist/update the status read model crash-safely.
        update_status_measurement(&artifact_dir, &run_id, &measurement, &evaluation)
            .map_err(|err| measure_error(err.to_string()))?;

        // Drive violating snapshots through the durable gate so a matching
        // approval resumes successfully and stale approvals remain blocked.
        if let Some(outcome) = enforce_gate(
            context,
            &artifact_dir,
            &run_id,
            &charter,
            &measurement,
            &evaluation,
        )? {
            return Ok(outcome);
        }

        // Expose context outputs for downstream steps.
        expose_measurement_outputs(context, &measurement, &evaluation);

        Ok(StepOutcome::Success)
    }
}

/// Persist and enforce the scope gate, returning a `Wait` outcome when the gate
/// blocks mutation. Returns `None` to allow the caller to continue to success.
fn enforce_gate(
    context: &mut StepContext,
    artifact_dir: &Path,
    run_id: &str,
    charter: &CanonicalTaskCharter,
    measurement: &crate::engine::executors::scope_control::measurement::PatchMeasurement,
    evaluation: &crate::engine::executors::scope_control::evaluation::ScopeEvaluation,
) -> Result<Option<StepOutcome>, EngineError> {
    if evaluation.within_budget && evaluation.violations.is_empty() {
        return Ok(None);
    }
    let gate = check_scope_gate(artifact_dir, run_id, charter, measurement, evaluation)
        .map_err(|err| measure_error(err.to_string()))?;
    if gate.allows_mutation() {
        return Ok(None);
    }
    let request = build_expansion_request(run_id, charter, measurement, evaluation);
    if matches!(gate, ScopeGateOutcome::OverBudgetNeedsRequest) {
        write_expansion_request(artifact_dir, &request)
            .map_err(|err| measure_error(err.to_string()))?;
    }
    context.set(
        "scope_measure_expansion_request_digest",
        &request.measurement_digest,
    );
    context.set("scope_measure_gate_outcome", gate_outcome_label(gate));
    context.set("artifact_root", &artifact_dir.to_string_lossy());
    Ok(Some(StepOutcome::Wait))
}

/// Map a scope-gate outcome to its stable context-output label.
fn gate_outcome_label(gate: ScopeGateOutcome) -> &'static str {
    match gate {
        ScopeGateOutcome::OverBudgetNeedsRequest => "needs_request",
        ScopeGateOutcome::PendingResolution => "pending_resolution",
        ScopeGateOutcome::Denied(_) => "denied",
        _ => "blocked",
    }
}

/// Expose measurement and evaluation totals as context outputs for downstream
/// steps.
fn expose_measurement_outputs(
    context: &mut StepContext,
    measurement: &crate::engine::executors::scope_control::measurement::PatchMeasurement,
    evaluation: &crate::engine::executors::scope_control::evaluation::ScopeEvaluation,
) {
    context.set("scope_measure_head_sha", &measurement.head_sha);
    context.set("scope_measure_merge_base", &measurement.merge_base);
    context.set(
        "scope_measure_divergence",
        &measurement.divergence.to_string(),
    );
    context.set(
        "scope_measure_files_changed",
        &measurement.files_changed.to_string(),
    );
    context.set(
        "scope_measure_added_lines",
        &measurement.added_lines.to_string(),
    );
    context.set(
        "scope_measure_new_modules",
        &measurement.new_modules.to_string(),
    );
    context.set(
        "scope_measure_dependencies_added",
        &measurement.dependencies_added.to_string(),
    );
    context.set(
        "scope_measure_public_apis_added",
        &measurement.public_apis_added.to_string(),
    );
    context.set(
        "scope_measure_within_budget",
        &evaluation.within_budget.to_string(),
    );
    context.set(
        "scope_measure_violation_count",
        &evaluation.violations.len().to_string(),
    );

    // Serialize the full measurement and evaluation as JSON context outputs.
    let measurement_json = serde_json::to_string(measurement)
        .expect("PatchMeasurement serialization is infallible for valid measurements");
    context.set("scope_measure_result", &measurement_json);

    let evaluation_json = serde_json::to_string(evaluation)
        .expect("ScopeEvaluation serialization is infallible for valid evaluations");
    context.set("scope_measure_evaluation", &evaluation_json);
}

fn measure_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "scope_measure".into(),
        message: message.into(),
    }
}

/// Resolve the active scope-control policy (mirrors the task_charter executor).
///
/// Returns `Ok(None)` when scope control is absent or disabled so the executor
/// can no-op successfully. A malformed active policy fails closed.
fn resolve_scope_control(
    context: &StepContext,
    params: &Value,
) -> Result<Option<ScopeControlConfig>, EngineError> {
    if let Some(policy_json) = context.get("scope_control_policy") {
        let config: ScopeControlConfig = serde_json::from_str(policy_json)
            .map_err(|err| measure_error(format!("invalid scope_control_policy context: {err}")))?;
        return Ok(config.enabled.then_some(config));
    }
    if let Some(profile_value) = params.get("target_profile") {
        let profile: TargetProfileConfig = serde_json::from_value(profile_value.clone())
            .map_err(|err| measure_error(format!("invalid target_profile parameter: {err}")))?;
        return Ok(active_scope_control(&profile).cloned());
    }
    if let Some(sc_value) = params.get("scope_control") {
        let config: ScopeControlConfig = serde_json::from_value(sc_value.clone())
            .map_err(|err| measure_error(format!("invalid scope_control parameter: {err}")))?;
        return Ok(config.enabled.then_some(config));
    }
    Ok(None)
}

/// Resolve the persistent artifact directory from trusted context.
fn resolve_artifact_dir(context: &StepContext) -> Result<std::path::PathBuf, EngineError> {
    context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| measure_error("scope-control requires artifact_dir or artifact_root"))
}

/// Load the canonical charter from the persisted artifact.
fn load_charter(artifact_dir: &Path, run_id: &str) -> Result<CanonicalTaskCharter, EngineError> {
    let dir = scope_control_dir(artifact_dir, run_id);
    let charter_p = charter_path(&dir);
    read_json::<CanonicalTaskCharter>(&charter_p).map_err(|err| {
        measure_error(format!(
            "failed to read charter at {}: {err}",
            charter_p.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executors::scope_control::measurement::GitPatchData;
    use crate::engine::executors::scope_control::MeasurementError;
    use crate::workflow::schema::{
        ScopeBudgetConfig, ScopeControlConfig, ScopeReviewCapsConfig, ScopeSubsystemConfig,
        TargetProfileConfig,
    };
    use serde_json::json;
    use tempfile::TempDir;

    /// A deterministic collector that returns fixed data.
    struct FixedCollector {
        data: GitPatchData,
    }

    impl GitPatchCollector for FixedCollector {
        fn collect(
            &self,
            _work_dir: &Path,
            _merge_base: &str,
            _config: &crate::workflow::schema::ScopeMeasurementConfig,
        ) -> Result<GitPatchData, MeasurementError> {
            Ok(self.data.clone())
        }
    }

    fn valid_scope_control() -> ScopeControlConfig {
        ScopeControlConfig {
            enabled: true,
            budget: ScopeBudgetConfig {
                max_files_changed: 10,
                max_added_lines: 500,
                max_new_modules: 3,
                max_dependencies_added: 0,
                max_public_apis_added: 5,
            },
            review_caps: ScopeReviewCapsConfig {
                initial_full_reviews: 1,
                max_delta_reviews: 2,
                final_acceptance_reviews: 1,
                max_mutating_remediation_rounds: 2,
            },
            subsystems: vec![ScopeSubsystemConfig {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            dependency_manifests: vec![],
            mandatory_command_groups: vec![],
            partial_compile_command: None,
            partial_compile_group: None,
            measurement: Default::default(),
            mandatory_gates: vec!["cargo test".into()],
        }
    }

    fn make_context_with_charter(tmp: &TempDir) -> StepContext {
        use crate::engine::executors::scope_control::model::{
            normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
        };
        use crate::engine::executors::scope_control::persistence::persist_charter_and_status;

        let work_dir = tmp.path().join("work");
        let artifact_dir = tmp.path().join("artifacts");
        std::fs::create_dir_all(&work_dir).expect("create work dir");

        let draft = TaskCharterDraft {
            charter_id: "TEST-001".into(),
            issue_number: 42,
            run_id: "run-test".into(),
            merge_base: "abc123".into(),
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
        persist_charter_and_status(&artifact_dir, &charter).expect("persist charter");

        // Create the work_dir file for measurement
        std::fs::create_dir_all(work_dir.join("src/core")).expect("create src/core");
        std::fs::write(work_dir.join("src/core/mod.rs"), "pub fn hello() {}\n")
            .expect("write file");

        let mut ctx = StepContext::new(work_dir, "run-test".into());
        ctx.set("artifact_dir", artifact_dir.to_str().expect("utf8"));
        ctx
    }

    #[test]
    fn executor_succeeds_and_sets_context_outputs() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context_with_charter(&tmp);
        let executor = ScopeMeasureExecutor::with_collector(Box::new(FixedCollector {
            data: GitPatchData {
                head_sha: "abc123".into(),
                divergence: 0,
                tracked_changes: vec![],
                untracked_files: vec![],
            },
        }));
        let params = json!({
            "scope_control": valid_scope_control(),
        });

        let outcome = executor.execute(&mut context, &params).expect("execute");
        assert_eq!(outcome, StepOutcome::Success);

        assert_eq!(
            context.get("scope_measure_head_sha").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(
            context.get("scope_measure_divergence").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            context
                .get("scope_measure_within_budget")
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn executor_fails_when_scope_control_disabled() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context_with_charter(&tmp);
        let executor = ScopeMeasureExecutor::with_collector(Box::new(FixedCollector {
            data: GitPatchData {
                head_sha: "abc123".into(),
                divergence: 0,
                tracked_changes: vec![],
                untracked_files: vec![],
            },
        }));
        let params = json!({
            "target_profile": TargetProfileConfig {
                scope_control: ScopeControlConfig {
                    enabled: false,
                    ..valid_scope_control()
                },
                ..Default::default()
            }
        });

        let outcome = executor.execute(&mut context, &params).unwrap();
        assert_eq!(outcome, StepOutcome::Success);
        assert!(context.get("scope_measure_head_sha").is_none());
    }

    #[test]
    fn executor_fails_without_charter() {
        let tmp = TempDir::new().expect("tempdir");
        let work_dir = tmp.path().join("work");
        let artifact_dir = tmp.path().join("artifacts");
        std::fs::create_dir_all(&work_dir).expect("create work dir");
        let mut context = StepContext::new(work_dir, "no-charter-run".into());
        context.set("artifact_dir", artifact_dir.to_str().expect("utf8"));

        let executor = ScopeMeasureExecutor::with_collector(Box::new(FixedCollector {
            data: GitPatchData {
                head_sha: "abc".into(),
                divergence: 0,
                tracked_changes: vec![],
                untracked_files: vec![],
            },
        }));
        let params = json!({
            "scope_control": valid_scope_control(),
        });

        let err = executor.execute(&mut context, &params).unwrap_err();
        assert!(err.to_string().contains("charter"));
    }

    #[test]
    fn executor_updates_status_with_measurement() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context_with_charter(&tmp);
        let artifact_dir = context.get("artifact_dir").expect("artifact_dir").clone();

        let executor = ScopeMeasureExecutor::with_collector(Box::new(FixedCollector {
            data: GitPatchData {
                head_sha: "abc123".into(),
                divergence: 0,
                tracked_changes: vec![],
                untracked_files: vec![],
            },
        }));
        let params = json!({
            "scope_control": valid_scope_control(),
        });

        executor.execute(&mut context, &params).expect("execute");

        // Read the status and verify measurement was persisted.
        let status_p = std::path::PathBuf::from(&artifact_dir)
            .join("scope-control")
            .join("run-test")
            .join("status.json");
        let status: crate::engine::executors::scope_control::persistence::ScopeStatus =
            read_json(&status_p).expect("read status");
        assert!(status.measurement.is_some());
        assert!(status.evaluation.is_some());
        assert!(status.measured_at.is_some());
    }
}
