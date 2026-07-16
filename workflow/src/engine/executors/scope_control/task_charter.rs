//! Task-charter [`StepExecutor`] implementation.
//!
//! The executor resolves a merge base through an injectable probe, assembles a
//! draft charter from config and run context, validates it against the
//! configured scope-control ceilings, normalizes it to canonical form,
//! persistently writes the charter + status atomically, and stores observable
//! artifacts (paths, digest) in the step context for downstream steps.
use std::path::Path;

use serde_json::Value;

use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::scope_control::config_validation::active_scope_control;
use crate::engine::executors::scope_control::model::{
    normalize_charter, validate_draft_against_config, DraftBudget, DraftReviewCaps, DraftSubsystem,
    TaskCharterDraft,
};
use crate::engine::executors::scope_control::persistence::persist_charter_and_status;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::workflow::schema::{ScopeControlConfig, TargetProfileConfig};

/// Trait for resolving a Git merge base. Production uses `git merge-base`;
/// tests inject a deterministic probe.
pub trait MergeBaseProbe: Send + Sync {
    /// Resolve the merge-base SHA for the given repository working directory
    /// and base branch.
    fn resolve_merge_base(
        &self,
        work_dir: &Path,
        base_branch: &str,
    ) -> Result<String, MergeBaseError>;
}

/// Error returned by [`MergeBaseProbe`].
#[derive(Debug, Clone)]
pub struct MergeBaseError {
    pub message: String,
}

impl std::fmt::Display for MergeBaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "merge-base resolution failed: {}", self.message)
    }
}

impl std::error::Error for MergeBaseError {}

/// Production merge-base probe that shells out to `git merge-base`.
pub struct SystemMergeBaseProbe;

impl MergeBaseProbe for SystemMergeBaseProbe {
    fn resolve_merge_base(
        &self,
        work_dir: &Path,
        base_branch: &str,
    ) -> Result<String, MergeBaseError> {
        if base_branch.starts_with('-') {
            return Err(MergeBaseError {
                message: format!("invalid option-like base branch: {base_branch}"),
            });
        }
        let output = std::process::Command::new("git")
            .arg("merge-base")
            .arg(base_branch)
            .arg("HEAD")
            .current_dir(work_dir)
            .output()
            .map_err(|err| MergeBaseError {
                message: format!("failed to invoke git: {err}"),
            })?;
        if !output.status.success() {
            return Err(MergeBaseError {
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Err(MergeBaseError {
                message: "git merge-base returned empty output".into(),
            });
        }
        Ok(sha)
    }
}

/// Task-charter step executor.
///
/// Constructed with an injectable [`MergeBaseProbe`] so tests can avoid Git.
/// Registered under the step type `"task_charter"`.
pub struct TaskCharterExecutor {
    probe: Box<dyn MergeBaseProbe>,
}

impl TaskCharterExecutor {
    /// Create with a system (production) merge-base probe.
    #[must_use]
    pub fn with_system_probe() -> Self {
        Self {
            probe: Box::new(SystemMergeBaseProbe),
        }
    }

    /// Create with a custom probe (for tests).
    #[must_use]
    pub fn with_probe(probe: Box<dyn MergeBaseProbe>) -> Self {
        Self { probe }
    }
}

impl StepExecutor for TaskCharterExecutor {
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

        let run_id = context.run_id().to_string();
        let work_dir = context.work_dir().clone();
        let base_branch = resolve_base_branch(context);
        let merge_base = self
            .probe
            .resolve_merge_base(&work_dir, &base_branch)
            .map_err(|err| charter_error(err.to_string()))?;

        let draft = build_draft(&scope_control, &run_id, &merge_base, context, params);

        validate_draft_against_config(&draft, &scope_control)
            .map_err(|err| charter_error(err.to_string()))?;

        let canonical = normalize_charter(&draft);

        let artifact_dir = resolve_artifact_dir(context);
        std::fs::create_dir_all(&artifact_dir).map_err(|err| EngineError::StepExecutionError {
            step_id: "task_charter".into(),
            message: format!("failed to create artifact dir: {err}"),
        })?;

        let (charter_path, status_path) = persist_charter_and_status(&artifact_dir, &canonical)
            .map_err(|err| EngineError::StepExecutionError {
                step_id: "task_charter".into(),
                message: err.to_string(),
            })?;

        // Store observable artifacts for downstream steps.
        context.set("task_charter_digest", &canonical.digest);
        context.set("task_charter_merge_base", &canonical.merge_base);
        context.set("task_charter_path", charter_path.to_string_lossy().as_ref());
        context.set(
            "task_charter_status_path",
            status_path.to_string_lossy().as_ref(),
        );

        Ok(StepOutcome::Success)
    }
}

fn charter_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "task_charter".into(),
        message: message.into(),
    }
}

/// Resolve the active scope-control policy from the trusted context binding
/// (`scope_control_policy`) seeded by the runner. Falls back to a
/// `target_profile` param for integration-test scenarios where the runner has
/// not seeded the context.
///
/// Returns `Ok(None)` when scope control is absent or disabled so the executor
/// can no-op successfully (shared workflows remain compatible). A **malformed**
/// active policy fails closed: deserialization errors propagate as `Err` so a
/// corrupt enabled policy never silently degrades into a no-op.
fn resolve_scope_control(
    context: &StepContext,
    params: &Value,
) -> Result<Option<ScopeControlConfig>, EngineError> {
    if let Some(policy_json) = context.get("scope_control_policy") {
        let config: ScopeControlConfig = serde_json::from_str(policy_json)
            .map_err(|err| charter_error(format!("invalid scope_control_policy context: {err}")))?;
        return Ok(config.enabled.then_some(config));
    }
    if let Some(profile_value) = params.get("target_profile") {
        let profile: TargetProfileConfig = serde_json::from_value(profile_value.clone())
            .map_err(|err| charter_error(format!("invalid target_profile parameter: {err}")))?;
        return Ok(active_scope_control(&profile).cloned());
    }
    if let Some(sc_value) = params.get("scope_control") {
        let config: ScopeControlConfig = serde_json::from_value(sc_value.clone())
            .map_err(|err| charter_error(format!("invalid scope_control parameter: {err}")))?;
        return Ok(config.enabled.then_some(config));
    }
    Ok(None)
}

fn resolve_base_branch(context: &StepContext) -> String {
    context
        .get("base_branch")
        .map(|branch| branch.trim())
        .filter(|branch| !branch.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "main".to_string())
}

fn resolve_artifact_dir(context: &StepContext) -> std::path::PathBuf {
    context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.work_dir().clone())
}

/// Resolve canonical acceptance criteria for the charter.
///
/// Precedence (trusted context-first):
/// 1. Runner-seeded `task_charter_acceptance_criteria` context variable
///    (trusted, derived from resolved run/config context by the runner).
/// 2. `acceptance_criteria` step parameter (test-only injection).
/// 3. Derived default from the issue and subsystem context (safety net so a
///    production run never produces an empty canonical charter).
fn resolve_acceptance_criteria(context: &StepContext, params: &Value) -> Vec<String> {
    if let Some(criteria_json) = context.get("task_charter_acceptance_criteria") {
        let parsed = parse_string_list_json(criteria_json);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Some(arr) = params.get("acceptance_criteria").and_then(Value::as_array) {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    derive_default_acceptance_criteria(context)
}

/// Resolve canonical non-goals for the charter.
///
/// Same trusted-context-first precedence as acceptance criteria.
fn resolve_non_goals(context: &StepContext, params: &Value) -> Vec<String> {
    if let Some(goals_json) = context.get("task_charter_non_goals") {
        let parsed = parse_string_list_json(goals_json);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Some(arr) = params.get("non_goals").and_then(Value::as_array) {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    derive_default_non_goals(context)
}

/// Parse a JSON array-of-strings from a context string value.
fn parse_string_list_json(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Derive nonempty acceptance criteria from the issue number and subsystems
/// so a production charter is always actionable.
fn derive_default_acceptance_criteria(context: &StepContext) -> Vec<String> {
    let issue = context
        .get("primary_issue_number")
        .or_else(|| context.get("issue_number"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let subsystems = context
        .get("task_charter_subsystem_ids")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut criteria = vec![format!(
        "Implementation addresses the requirements of issue #{issue}"
    )];
    for sub in &subsystems {
        if !sub.is_empty() {
            criteria.push(format!("Changes scoped to declared subsystem '{sub}'"));
        }
    }
    criteria
}

/// Derive nonempty non-goals from the charter context so a production
/// charter always carries explicit scope boundaries.
fn derive_default_non_goals(context: &StepContext) -> Vec<String> {
    let mut goals = vec![
        "No refactoring unrelated to the target issue".to_string(),
        "No new dependencies unless required by the fix".to_string(),
    ];
    if let Some(gates) = context.get("task_charter_mandatory_gates") {
        if !gates.trim().is_empty() {
            goals.push(format!("No weakening of mandatory gates: {gates}"));
        }
    }
    goals
}

fn build_draft(
    scope_control: &ScopeControlConfig,
    run_id: &str,
    merge_base: &str,
    context: &StepContext,
    params: &Value,
) -> TaskCharterDraft {
    let charter_id = params
        .get("charter_id")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| format!("SCOPE-{run_id}"));
    let issue_number = params
        .get("issue_number")
        .and_then(Value::as_u64)
        .or_else(|| {
            context
                .get("primary_issue_number")
                .or_else(|| context.get("issue_number"))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0);
    let acceptance_criteria = resolve_acceptance_criteria(context, params);
    let non_goals = resolve_non_goals(context, params);

    let subsystems = scope_control
        .subsystems
        .iter()
        .map(|sub| DraftSubsystem {
            id: sub.id.clone(),
            paths: sub.paths.clone(),
        })
        .collect::<Vec<_>>();

    let budget = DraftBudget {
        max_files_changed: scope_control.budget.max_files_changed,
        max_added_lines: scope_control.budget.max_added_lines,
        max_new_modules: scope_control.budget.max_new_modules,
        max_dependencies_added: scope_control.budget.max_dependencies_added,
        max_public_apis_added: scope_control.budget.max_public_apis_added,
    };
    let review_caps = DraftReviewCaps {
        initial_full_reviews: scope_control.review_caps.initial_full_reviews,
        max_delta_reviews: scope_control.review_caps.max_delta_reviews,
        final_acceptance_reviews: scope_control.review_caps.final_acceptance_reviews,
        max_mutating_remediation_rounds: scope_control.review_caps.max_mutating_remediation_rounds,
    };

    TaskCharterDraft {
        charter_id,
        issue_number,
        run_id: run_id.to_string(),
        merge_base: merge_base.to_string(),
        acceptance_criteria,
        non_goals,
        subsystems,
        budget,
        review_caps,
        mandatory_gates: scope_control.mandatory_gates.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{
        ScopeBudgetConfig, ScopeControlConfig, ScopeReviewCapsConfig, ScopeSubsystemConfig,
        TargetProfileConfig,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A deterministic merge-base probe that returns a fixed SHA.
    struct FixedProbe {
        sha: String,
    }

    impl MergeBaseProbe for FixedProbe {
        fn resolve_merge_base(
            &self,
            _work_dir: &Path,
            _base_branch: &str,
        ) -> Result<String, MergeBaseError> {
            Ok(self.sha.clone())
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

    fn make_context(tmp: &TempDir) -> StepContext {
        let work_dir = tmp.path().join("work");
        let artifact_dir = tmp.path().join("artifacts");
        std::fs::create_dir_all(&work_dir).expect("create work dir");
        let mut ctx = StepContext::new(work_dir, "run-test".into());
        ctx.set("artifact_dir", artifact_dir.to_str().expect("utf8"));
        ctx.set("primary_issue_number", "42");
        ctx
    }

    fn executor_with_fixed_probe(sha: &str) -> TaskCharterExecutor {
        TaskCharterExecutor::with_probe(Box::new(FixedProbe { sha: sha.into() }))
    }

    #[test]
    fn executor_succeeds_and_writes_artifacts() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let executor = executor_with_fixed_probe("deadbeef");
        let params = json!({
            "acceptance_criteria": ["AC-1"],
            "non_goals": ["no redesign"],
            "target_profile": TargetProfileConfig {
                scope_control: valid_scope_control(),
                identity: crate::workflow::schema::TargetIdentityConfig {
                    base_branch: Some("main".into()),
                    ..Default::default()
                },
                ..Default::default()
            }
        });

        let outcome = executor.execute(&mut context, &params).expect("execute");
        assert_eq!(outcome, StepOutcome::Success);

        // Observable artifacts in context.
        let digest = context.get("task_charter_digest").expect("digest set");
        assert!(!digest.is_empty());
        let merge_base = context
            .get("task_charter_merge_base")
            .expect("merge_base set");
        assert_eq!(merge_base, "deadbeef");
        let charter_path = context.get("task_charter_path").expect("path set");
        assert!(PathBuf::from(charter_path).exists());
    }

    #[test]
    fn executor_noops_when_scope_control_disabled() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let executor = executor_with_fixed_probe("deadbeef");
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
        assert!(context.get("task_charter_digest").is_none());
    }

    #[test]
    fn executor_fails_on_merge_base_error() {
        struct ErrorProbe;
        impl MergeBaseProbe for ErrorProbe {
            fn resolve_merge_base(
                &self,
                _work_dir: &Path,
                _base_branch: &str,
            ) -> Result<String, MergeBaseError> {
                Err(MergeBaseError {
                    message: "no git repo".into(),
                })
            }
        }
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let executor = TaskCharterExecutor::with_probe(Box::new(ErrorProbe));
        let params = json!({
            "target_profile": TargetProfileConfig {
                scope_control: valid_scope_control(),
                ..Default::default()
            }
        });

        let err = executor.execute(&mut context, &params).unwrap_err();
        assert!(err.to_string().contains("merge-base"));
    }

    #[test]
    fn executor_uses_scope_control_from_params_when_no_profile() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let executor = executor_with_fixed_probe("cafe");
        let params = json!({
            "scope_control": valid_scope_control(),
            "charter_id": "CUSTOM-001",
            "acceptance_criteria": ["AC-1"],
            "non_goals": ["no redesign"],
        });

        let outcome = executor.execute(&mut context, &params).expect("execute");
        assert_eq!(outcome, StepOutcome::Success);

        let charter_path = context.get("task_charter_path").expect("path");
        let charter_json = std::fs::read_to_string(charter_path).expect("read");
        assert!(charter_json.contains("CUSTOM-001"));
    }

    #[test]
    fn executor_sets_digest_observable() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let executor = executor_with_fixed_probe("feedface");
        let params = json!({
            "acceptance_criteria": ["AC-1"],
            "non_goals": ["no redesign"],
            "target_profile": TargetProfileConfig {
                scope_control: valid_scope_control(),
                ..Default::default()
            }
        });

        executor.execute(&mut context, &params).expect("execute");
        let digest = context.get("task_charter_digest").expect("digest");
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn executor_resolves_acceptance_criteria_from_context_over_params() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let trusted = serde_json::to_string(&["CONTEXT-AC-1", "CONTEXT-AC-2"]).unwrap();
        context.set("task_charter_acceptance_criteria", &trusted);
        let executor = executor_with_fixed_probe("abc");
        let params = json!({
            "acceptance_criteria": ["PARAM-AC"],
            "non_goals": ["no redesign"],
            "scope_control": valid_scope_control(),
        });

        executor.execute(&mut context, &params).expect("execute");
        let charter_path = context.get("task_charter_path").expect("path");
        let charter_json = std::fs::read_to_string(charter_path).expect("read");
        assert!(charter_json.contains("CONTEXT-AC-1"));
        assert!(charter_json.contains("CONTEXT-AC-2"));
        assert!(!charter_json.contains("PARAM-AC"));
    }

    #[test]
    fn executor_resolves_non_goals_from_context_over_params() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        let trusted = serde_json::to_string(&["CONTEXT-NG"]).unwrap();
        context.set("task_charter_non_goals", &trusted);
        let executor = executor_with_fixed_probe("abc");
        let params = json!({
            "acceptance_criteria": ["AC-1"],
            "non_goals": ["PARAM-NG"],
            "scope_control": valid_scope_control(),
        });

        executor.execute(&mut context, &params).expect("execute");
        let charter_path = context.get("task_charter_path").expect("path");
        let charter_json = std::fs::read_to_string(charter_path).expect("read");
        assert!(charter_json.contains("CONTEXT-NG"));
        assert!(!charter_json.contains("PARAM-NG"));
    }

    #[test]
    fn executor_derives_nonempty_criteria_when_neither_context_nor_params() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        // No acceptance_criteria or non_goals in context or params.
        let executor = executor_with_fixed_probe("abc");
        let params = json!({
            "scope_control": valid_scope_control(),
        });

        executor.execute(&mut context, &params).expect("execute");
        let charter_path = context.get("task_charter_path").expect("path");
        let charter_json = std::fs::read_to_string(charter_path).expect("read");
        // Derived default should mention the issue.
        assert!(charter_json.contains("issue #42"));
        // Non-goals should be nonempty.
        assert!(charter_json.contains("No refactoring"));
    }

    #[test]
    fn executor_derives_criteria_from_subsystem_context() {
        let tmp = TempDir::new().expect("tempdir");
        let mut context = make_context(&tmp);
        context.set("task_charter_subsystem_ids", "core,api");
        let executor = executor_with_fixed_probe("abc");
        let params = json!({
            "scope_control": valid_scope_control(),
        });

        executor.execute(&mut context, &params).expect("execute");
        let charter_path = context.get("task_charter_path").expect("path");
        let charter_json = std::fs::read_to_string(charter_path).expect("read");
        assert!(charter_json.contains("subsystem 'core'"));
        assert!(charter_json.contains("subsystem 'api'"));
    }
}
