//! Timeout-recovery helpers extracted from [`super`] (issue 142).
//!
//! These functions implement the partial-timeout-recovery path that persists a
//! scope-control snapshot when the llxprt child process is killed for exceeding
//! its wall-clock or idle budget and scope control is active.

use crate::engine::executor::StepContext;
use crate::engine::executors::change_detection::new_changed_paths;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

use super::{DiffDetection, ProcessResult};

/// Build the human-readable diagnostic for a timeout result.
pub(super) fn timeout_diagnostic(result: &ProcessResult) -> String {
    if result.idle_timed_out {
        result.idle_timeout.map_or_else(
            || "llxprt timed out after stalled output".to_string(),
            |timeout| {
                format!(
                    "llxprt produced no new output for {} seconds",
                    timeout.as_secs()
                )
            },
        )
    } else {
        format!(
            "llxprt timed out after {} seconds",
            result.timeout.as_secs()
        )
    }
}

/// Attempt partial timeout recovery when scope control is active.
///
/// Detects changed paths since the initial snapshot; when there are new changes
/// and a valid scope-control policy, persists a timeout-recovery snapshot and
/// routes the run to `Wait` for operator continuation.
pub(super) fn recover_partial_timeout(
    context: &mut StepContext,
    initial_changed_paths: &[String],
    detection: DiffDetection<'_>,
    timeout_kind: crate::engine::executors::scope_control::timeout_recovery::TimeoutKind,
) -> Result<Option<StepOutcome>, EngineError> {
    use crate::engine::executors::scope_control::timeout_recovery::ProcessEvidence;
    use crate::engine::executors::scope_control::{
        charter_path, handle_timeout_recovery, read_json, scope_control_dir, CanonicalTaskCharter,
        SystemGitPatchCollector,
    };

    let Some(current_paths) = detection.detect(context, "timeout change detection failed") else {
        return Ok(Some(StepOutcome::Fatal));
    };
    if new_changed_paths(&current_paths, initial_changed_paths).is_empty() {
        return Ok(None);
    }
    let Some(policy) = resolve_recovery_policy(context)? else {
        return Ok(None);
    };
    let artifact_dir = resolve_recovery_artifact_dir(context);
    let scope_dir = scope_control_dir(&artifact_dir, context.run_id());
    let charter: CanonicalTaskCharter = read_json(&charter_path(&scope_dir))
        .map_err(|err| recovery_error("charter unavailable", err))?;
    let measurement =
        compute_recovery_measurement(context, &charter, &policy, SystemGitPatchCollector)?;
    let process_evidence = ProcessEvidence {
        exit_code: Some(124),
        wall_clock_timeout: matches!(
            timeout_kind,
            crate::engine::executors::scope_control::timeout_recovery::TimeoutKind::Timeout
        ),
        process_killed: true,
    };
    handle_timeout_recovery(
        &artifact_dir,
        context.run_id(),
        &charter,
        &measurement,
        timeout_kind,
        policy.partial_compile_command.is_some() || policy.partial_compile_group.is_some(),
        &process_evidence,
    )
    .map_err(|err| recovery_error("persist timeout recovery snapshot", err))?;
    context.set("artifact_root", &artifact_dir.to_string_lossy());
    context.set("scope_timeout_recovery_required", "true");
    Ok(Some(StepOutcome::Wait))
}

/// Build a step-execution error for a timeout-recovery failure.
fn recovery_error(label: &str, err: impl std::fmt::Display) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "llxprt".into(),
        message: format!("timeout recovery {label}: {err}"),
    }
}

/// Resolve and validate the scope-control policy for timeout recovery.
///
/// Returns `None` when scope control is disabled or no policy is configured.
fn resolve_recovery_policy(
    context: &mut StepContext,
) -> Result<Option<crate::workflow::schema::ScopeControlConfig>, EngineError> {
    let Some(policy_json) = context.get("scope_control_policy") else {
        return Ok(None);
    };
    let policy: crate::workflow::schema::ScopeControlConfig = serde_json::from_str(policy_json)
        .map_err(|err| EngineError::StepExecutionError {
            step_id: "llxprt".into(),
            message: format!("invalid scope-control policy during timeout recovery: {err}"),
        })?;
    if !policy.enabled {
        return Ok(None);
    }
    Ok(Some(policy))
}

/// Resolve the artifact directory used to persist timeout-recovery artifacts.
fn resolve_recovery_artifact_dir(context: &StepContext) -> std::path::PathBuf {
    context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.work_dir().clone())
}

/// Collect git patch data, dependency diffs, and compute the patch measurement
/// for timeout recovery.
fn compute_recovery_measurement<C>(
    context: &mut StepContext,
    charter: &crate::engine::executors::scope_control::model::CanonicalTaskCharter,
    policy: &crate::workflow::schema::ScopeControlConfig,
    collector: C,
) -> Result<crate::engine::executors::scope_control::measurement::PatchMeasurement, EngineError>
where
    C: crate::engine::executors::scope_control::GitPatchCollector,
{
    use crate::engine::executors::scope_control::{collect_dependency_diffs, compute_measurement};

    let git_data = collector
        .collect(context.work_dir(), &charter.merge_base, &policy.measurement)
        .map_err(|err| recovery_error("measurement failed", err))?;
    let dependency_diffs = collect_dependency_diffs(
        context.work_dir(),
        &policy.dependency_manifests,
        &charter.merge_base,
    )
    .map_err(|err| recovery_error("dependency measurement failed", err))?;
    compute_measurement(
        &git_data,
        charter,
        context.run_id(),
        context
            .get("daemon_managed_claim")
            .is_some_and(|value| value == "true"),
        &policy.measurement,
        context.work_dir(),
        &dependency_diffs,
    )
    .map_err(|err| recovery_error("measurement failed", err))
}
