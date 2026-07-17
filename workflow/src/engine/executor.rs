/// @plan:PLAN-20260408-STEP-EXEC.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P09
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-002,REQ-LF-CTX-003,REQ-LF-CTX-004
/// Executor module - step execution trait, registry, and context.
use std::collections::{BTreeSet, HashMap};
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 1-23
use std::path::PathBuf;

use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

// Re-export executors for convenience
pub use crate::engine::executors::noop::NoOpExecutor;

/// Context for step execution.
/// Stores key-value pairs for variable interpolation across steps.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P09
#[derive(Debug)]
pub struct StepContext {
    /// Working directory for step execution
    work_dir: PathBuf,
    /// Unique identifier for this workflow run
    run_id: String,
    /// Storage for context values: key -> value
    variables: HashMap<String, String>,
    /// Current step ID being executed (for namespaced variable storage)
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P09
    current_step_id: Option<String>,
    /// Order of step execution for most-recent-writer-first resolution
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P09
    step_order: Vec<String>,
    /// Namespaced variables: step_id -> {variable_name -> value}
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P09
    pub namespaced_vars: HashMap<String, HashMap<String, String>>,
}

fn is_checkpointable_context_key(key: &str) -> bool {
    matches!(
        key,
        "issue_number"
            | "primary_issue_number"
            | "issue_title"
            | "pr_number"
            | "owner"
            | "repo"
            | "repository"
            | "current_branch"
            | "base_branch"
            | "existing_pr_number"
            | "head_ref"
            | "head_sha"
            | "base_ref"
            | "base_sha"
    )
}

impl StepContext {
    /// Create a new `StepContext` with the given `work_dir` and `run_id`.
    /// Stores work_dir and run_id in variables for bare key access (REQ-LF-CTX-004).
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P09
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    #[must_use]
    pub fn new(work_dir: PathBuf, run_id: String) -> Self {
        let mut variables = HashMap::new();
        // Store built-ins as bare keys for backward compatibility
        variables.insert(
            "work_dir".to_string(),
            work_dir.to_string_lossy().to_string(),
        );
        variables.insert("run_id".to_string(), run_id.clone());

        Self {
            work_dir,
            run_id,
            variables,
            current_step_id: None,
            step_order: Vec::new(),
            namespaced_vars: HashMap::new(),
        }
    }

    /// Capture non-secret execution context needed to safely reconstruct a failed step.
    #[must_use]
    pub fn checkpoint_values(&self) -> HashMap<String, serde_json::Value> {
        let mut values = HashMap::new();
        for (key, value) in &self.variables {
            if is_checkpointable_context_key(key) {
                values.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
        let namespaced_vars: HashMap<_, _> = self
            .namespaced_vars
            .iter()
            .map(|(step, variables)| {
                let variables: HashMap<String, String> = variables
                    .iter()
                    .filter(|(key, _)| is_checkpointable_context_key(key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                (step.clone(), variables)
            })
            .collect();
        values.insert(
            "__namespaced_vars".to_string(),
            serde_json::to_value(namespaced_vars).unwrap_or_default(),
        );
        values.insert(
            "__step_order".to_string(),
            serde_json::to_value(&self.step_order).unwrap_or_default(),
        );
        values
    }

    /// Restore non-secret values captured by [`Self::checkpoint_values`].
    ///
    /// When restoring `__namespaced_vars`, the inner maps are filtered through
    /// the checkpointable-key allowlist so a malicious or corrupted persisted
    /// payload cannot inject disallowed keys (tokens, shell output, etc.) into
    /// the live context. Malformed JSON values fail closed with the underlying
    /// serde error rather than silently substituting an empty context.
    pub fn restore_checkpoint_values(
        &mut self,
        mut values: HashMap<String, serde_json::Value>,
    ) -> Result<(), serde_json::Error> {
        if let Some(value) = values.remove("__namespaced_vars") {
            let raw: HashMap<String, HashMap<String, String>> = serde_json::from_value(value)?;
            self.namespaced_vars = raw
                .into_iter()
                .map(|(step, variables)| {
                    let variables = variables
                        .into_iter()
                        .filter(|(key, _)| is_checkpointable_context_key(key))
                        .collect();
                    (step, variables)
                })
                .collect();
        }
        if let Some(value) = values.remove("__step_order") {
            self.step_order = serde_json::from_value(value)?;
        }
        for (key, value) in values {
            if is_checkpointable_context_key(&key) {
                if let Some(value) = value.as_str() {
                    self.variables.insert(key, value.to_string());
                }
            }
        }
        Ok(())
    }

    /// Get a context value by key.
    /// Handles both namespaced keys ("step_id.var") and bare keys.
    /// For bare keys, searches step_order in reverse (most-recent-writer-first).
    /// Falls back to "config" namespace, then flat variables, then built-ins.
    /// Returns `None` if the key is not found.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&String> {
        // Case 1: Explicit namespace "step_id.variable"
        if let Some(dot_pos) = key.find('.') {
            let step_id = &key[..dot_pos];
            let var_name = &key[dot_pos + 1..];
            return self.namespaced_vars.get(step_id)?.get(var_name);
        }

        // Case 2: Unnamespaced - most-recent-first search across step namespaces
        // Iterate step_order in reverse to find the most recent writer
        for step_id in self.step_order.iter().rev() {
            if let Some(vars) = self.namespaced_vars.get(step_id) {
                if let Some(value) = vars.get(key) {
                    return Some(value);
                }
            }
        }

        // Case 3: Fall back to "config" namespace (config-seeded vars)
        if let Some(config_vars) = self.namespaced_vars.get("config") {
            if let Some(value) = config_vars.get(key) {
                return Some(value);
            }
        }

        // Case 4: Fall back to flat variables (built-ins like work_dir/run_id, pre-namespace bare keys)
        self.variables.get(key).or_else(|| {
            if key == "issue_number" {
                self.get("primary_issue_number")
            } else {
                None
            }
        })
    }

    /// Set a context value.
    /// If `current_step_id` is set, also stores the value in the step's namespaced map.
    /// Always stores the bare key in `variables` for backward compatibility.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    pub fn set(&mut self, key: &str, value: &str) {
        // If current_step_id is Some, store in namespaced storage
        if let Some(ref step_id) = self.current_step_id {
            self.namespaced_vars
                .entry(step_id.clone())
                .or_default()
                .insert(key.to_string(), value.to_string());
        }
        // Always store bare key in variables (backward compat + pre-namespace-era bare keys)
        self.variables.insert(key.to_string(), value.to_string());
    }

    /// Get the working directory.
    #[must_use]
    pub const fn work_dir(&self) -> &PathBuf {
        &self.work_dir
    }

    /// Get the run ID.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Set the current step ID for namespaced variable storage.
    /// Appends to step_order if not already the last entry.
    /// Also stores the step_id as a context variable for executors to access.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P09
    pub fn set_current_step_id(&mut self, step_id: &str) {
        self.current_step_id = Some(step_id.to_string());
        if self.step_order.last() != Some(&step_id.to_string()) {
            self.step_order.push(step_id.to_string());
        }
        // Store as context variable so executors can know which step they're executing
        self.variables
            .insert("current_step_id".to_string(), step_id.to_string());
    }

    /// Set the working directory for step execution.
    /// Only changes the work_dir field without affecting variables or namespaced storage.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-LF-PROF-003
    pub fn set_work_dir(&mut self, work_dir: PathBuf) {
        self.work_dir = work_dir;
        // Update the flat variable for backward compatibility
        self.variables.insert(
            "work_dir".to_string(),
            self.work_dir.to_string_lossy().to_string(),
        );
    }
}

/// Interpolate `{key}` and `{step_id.key}` template tokens in a template string.
///
/// Replaces all occurrences of `{key}` with the corresponding value from context.
/// Handles both namespaced (`{step_id.variable}`) and bare (`{variable}`) template tokens.
/// Undefined keys are left as-is (no error, no replacement).
/// No nested/recursive resolution - only one pass.
/// @plan:PLAN-20260408-STEP-EXEC.P05
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn interpolate_string(template: &str, context: &StepContext) -> String {
    let mut result = template.to_string();

    // Collect all keys that can be resolved:
    // 1. All keys from namespaced_vars (both step_id.var and bare var patterns)
    // 2. All keys from flat variables
    // 3. Built-in keys (work_dir and run_id)
    let mut all_keys: Vec<String> = Vec::new();

    // Add namespaced keys: "step_id.var" from namespaced_vars
    for (step_id, vars) in &context.namespaced_vars {
        for var_name in vars.keys() {
            let namespaced_key = format!("{}.{}", step_id, var_name);
            all_keys.push(namespaced_key);
        }
    }

    // Add bare variable names from namespaced vars (for unqualified lookups)
    for vars in context.namespaced_vars.values() {
        for key in vars.keys() {
            if !all_keys.iter().any(|k| k == key) {
                all_keys.push(key.clone());
            }
        }
    }

    // Add context variable keys from flat variables
    for key in context.variables.keys() {
        if !all_keys.iter().any(|k| k == key) {
            all_keys.push(key.clone());
        }
    }

    // Add built-in keys (work_dir and run_id)
    if !all_keys.iter().any(|k| k == "work_dir") {
        all_keys.push("work_dir".to_string());
    }
    if !all_keys.iter().any(|k| k == "run_id") {
        all_keys.push("run_id".to_string());
    }
    if !all_keys.iter().any(|k| k == "issue_number") && context.get("issue_number").is_some() {
        all_keys.push("issue_number".to_string());
    }

    // Sort keys by length descending to prevent partial replacements
    // (e.g., "{foo}" vs "{foobar}" or "{step.var}" vs "{step.variable}")
    all_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));

    // Iterate over all keys and replace
    for key in all_keys {
        let template_token = format!("{{{key}}}");
        if let Some(value) = context.get(&key) {
            result = result.replace(&template_token, value);
        }
    }

    result
}

/// Extract interpolation token names from a template string.
///
/// Matches only strict identifier tokens of the form `{name}` or
/// `{namespace.name}`, mirroring the grammar resolved by
/// [`interpolate_string`]. Identifiers must start with a letter or underscore
/// and contain only `[A-Za-z0-9_]`, optionally with a single dotted segment.
///
/// `jq` object-construction braces such as `{number, title}` or
/// `{title: .title}` contain spaces/commas/colons and therefore do **not**
/// match, so they are never mistaken for interpolation tokens. Likewise
/// shell-style `${VAR}` references are skipped: the brace is preceded by a `$`,
/// so it is treated as shell/env interpolation rather than a Luther token (a
/// bare `{VAR}` is still extracted normally).
///
/// Returned in first-seen order without de-duplication of distinct tokens; the
/// same token appearing twice is reported twice (callers de-duplicate as
/// needed).
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn extract_tokens(template: &str) -> Vec<String> {
    use std::sync::OnceLock;
    static TOKEN_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = TOKEN_RE.get_or_init(|| {
        // Strict identifier, optionally one dotted namespace segment.
        regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?)\}")
            .expect("static token regex is valid")
    });
    let bytes = template.as_bytes();
    re.captures_iter(template)
        .filter_map(|c| {
            let full = c.get(0)?;
            // Skip shell-style `${VAR}`: a `$` immediately before the `{` marks
            // this as shell/env interpolation, not a Luther interpolation token.
            if full.start() > 0 && bytes[full.start() - 1] == b'$' {
                return None;
            }
            c.get(1).map(|m| m.as_str().to_string())
        })
        .collect()
}

/// Trait for step executors. Each step type has a concrete implementation.
pub trait StepExecutor: Send + Sync {
    /// Execute the step and return an outcome.
    ///
    /// # Arguments
    /// * `context` - Mutable context for value storage and interpolation
    /// * `params` - JSON parameters for this step
    ///
    /// # Errors
    /// Returns `EngineError` on fatal execution failure.
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError>;
}

/// Registry mapping `step_type` strings to executor implementations.
#[derive(Default)]
pub struct ExecutorRegistry {
    /// Map of `step_type` -> boxed executor trait object
    executors: HashMap<String, Box<dyn StepExecutor>>,
}

impl ExecutorRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an executor for a step type.
    ///
    /// # Arguments
    /// * `step_type` - The step type string (e.g., `"shell"`, `"write_file"`)
    /// * `executor` - Boxed trait object implementing `StepExecutor`
    pub fn register(&mut self, step_type: &str, executor: Box<dyn StepExecutor>) {
        self.executors.insert(step_type.to_string(), executor);
    }

    /// Return whether an executor is registered for `step_type` without dispatching it.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-PRFU-020
    /// @pseudocode lines 1-53
    #[must_use]
    pub fn contains_step_type(&self, step_type: &str) -> bool {
        self.executors.contains_key(step_type)
    }

    /// Return all registered step types without dispatching any executor.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-PRFU-020
    /// @pseudocode lines 1-53
    #[must_use]
    pub fn registered_step_types(&self) -> BTreeSet<String> {
        self.executors.keys().cloned().collect()
    }

    /// Dispatch execution to the appropriate executor.
    ///
    /// # Arguments
    /// * `step_type` - The step type to look up
    /// * `context` - Mutable execution context
    /// * `params` - JSON parameters for the step
    ///
    /// # Errors
    /// Returns `EngineError::StepExecutionError` if no executor is registered
    /// for the given `step_type`, or if the executor itself returns an error.
    ///
    /// @plan:PLAN-20260408-STEP-EXEC.P05
    pub fn dispatch(
        &self,
        step_type: &str,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        self.executors.get(step_type).map_or_else(
            || {
                Err(EngineError::StepExecutionError {
                    step_id: step_type.to_string(),
                    message: format!("No executor registered for step type '{step_type}'"),
                })
            },
            |executor| executor.execute(context, params),
        )
    }

    fn register_core_executors(&mut self) {
        self.register("shell", Box::new(crate::engine::executors::ShellExecutor));
        self.register(
            "failure_cleanup",
            Box::new(crate::engine::executors::ShellExecutor),
        );
        self.register(
            "write_file",
            Box::new(crate::engine::executors::WriteFileExecutor),
        );
        self.register("verify", Box::new(crate::engine::executors::VerifyExecutor));
        self.register(
            "command_manifest_group",
            Box::new(crate::engine::executors::command_manifest::CommandManifestGroupExecutor),
        );
        self.register("llxprt", Box::new(crate::engine::executors::LlxprtExecutor));
        self.register(
            "workflow_auth_preflight",
            Box::new(crate::engine::executors::WorkflowAuthPreflightExecutor),
        );
        self.register("noop", Box::new(crate::engine::executors::NoOpExecutor));
        self.register(
            "parent_orchestration",
            Box::new(crate::engine::executors::ParentOrchestrationExecutor),
        );
        self.register(
            "task_charter",
            Box::new(
                crate::engine::executors::scope_control::TaskCharterExecutor::with_system_probe(),
            ),
        );
        self.register(
            "scope_measure",
            Box::new(
                crate::engine::executors::scope_control::ScopeMeasureExecutor::with_system_collector(),
            ),
        );
    }

    fn register_github_followup_executors(&mut self) {
        self.register(
            "github_pr_identity",
            Box::new(
                crate::engine::executors::GithubPrIdentityExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        self.register(
            "post_pr_iteration_guard",
            Box::new(crate::engine::executors::PostPrIterationGuardExecutor),
        );
        self.register(
            "github_pr_checks",
            Box::new(
                crate::engine::executors::GithubPrChecksExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        self.register(
            "github_check_failures",
            Box::new(
                crate::engine::executors::GithubCheckFailuresExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        self.register(
            "github_coderabbit_feedback",
            Box::new(
                crate::engine::executors::GithubCodeRabbitFeedbackExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemFeedbackClock,
                ),
            ),
        );
    }

    fn register_feedback_and_remediation_executors(&mut self) {
        self.register(
            "feedback_evaluator",
            Box::new(crate::engine::executors::FeedbackEvaluatorExecutor::new(
                crate::engine::executors::CommandFeedbackEvaluationAdapter::new(
                    crate::engine::executors::default_feedback_evaluator_argv(),
                    crate::engine::executors::ProcessFeedbackEvaluatorCommandRunner::default(),
                ),
                crate::engine::executors::SystemClockSleeper,
            )),
        );
        self.register(
            "pr_remediation_plan",
            Box::new(crate::engine::executors::PrRemediationPlanExecutor),
        );
        self.register(
            "pr_followup_remediation",
            Box::new(
                crate::engine::executors::PrFollowupRemediationExecutorWithRunner::new(
                    crate::engine::executors::SystemPrFollowupLlxprtCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        self.register(
            "pr_remediation_result",
            Box::new(crate::engine::executors::PrRemediationResultExecutor),
        );
        self.register(
            "run_post_pr_tests",
            Box::new(crate::engine::executors::RunPostPrTestsExecutor),
        );
        self.register(
            "push_remediation_changes",
            Box::new(crate::engine::executors::PushRemediationChangesExecutor),
        );
        self.register(
            "github_feedback_marker",
            Box::new(
                crate::engine::executors::GithubFeedbackMarkerExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemFeedbackClock,
                ),
            ),
        );
        self.register(
            "post_pr_failure_terminal",
            Box::new(crate::engine::executors::PostPrFailureTerminalExecutor),
        );
    }

    /// Create a registry with default executors pre-registered.
    ///
    /// Registers core shell, write-file, verification, llxprt, workflow auth preflight,
    /// GitHub PR follow-up, feedback, and PR remediation executors.
    /// @plan:PLAN-20260408-STEP-EXEC.P05
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-LF-SEP-001,REQ-PRFU-020
    /// @pseudocode lines 1-53
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register_core_executors();
        registry.register_github_followup_executors();
        registry.register_feedback_and_remediation_executors();
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_number_falls_back_to_primary_issue_number() {
        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
        context.set("primary_issue_number", "3");

        assert_eq!(context.get("issue_number").map(String::as_str), Some("3"));
        assert_eq!(
            interpolate_string("issue{issue_number}", &context),
            "issue3"
        );
    }

    #[test]
    fn explicit_issue_number_takes_precedence_over_primary_issue_number() {
        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
        context.set("primary_issue_number", "3");
        context.set("issue_number", "4");

        assert_eq!(context.get("issue_number").map(String::as_str), Some("4"));
        assert_eq!(
            interpolate_string("issue{issue_number}", &context),
            "issue4"
        );
    }

    #[test]
    fn checkpoint_values_round_trip_safe_context_and_redact_outputs_and_secrets() {
        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
        context.set_current_step_id("shell");
        context.set("issue_number", "137");
        context.set("issue_title", "Preserve failed identity");
        context.set("head_sha", "abcdef");
        context.set("base_sha", "123456");
        context.set("github_token", "flat-secret");
        context.set("api_key", "namespaced-secret");
        context.set("stdout", "raw output secret");
        context.set("stderr", "raw error secret");

        let values = context.checkpoint_values();
        let serialized = serde_json::to_string(&values).expect("serialize checkpoint context");
        assert!(serialized.contains("137"));
        assert!(!serialized.contains("flat-secret"));
        assert!(!serialized.contains("namespaced-secret"));
        assert!(!serialized.contains("raw output secret"));
        assert!(!serialized.contains("raw error secret"));

        let mut restored = StepContext::new(PathBuf::from("/tmp/other"), "run-2".to_string());
        restored
            .restore_checkpoint_values(values)
            .expect("restore checkpoint context");
        assert_eq!(
            restored.get("issue_number").map(String::as_str),
            Some("137")
        );
        assert_eq!(
            restored.get("shell.issue_number").map(String::as_str),
            Some("137")
        );
        assert_eq!(
            restored.get("issue_title").map(String::as_str),
            Some("Preserve failed identity")
        );
        assert_eq!(restored.get("head_sha").map(String::as_str), Some("abcdef"));
        assert_eq!(restored.get("base_sha").map(String::as_str), Some("123456"));
        assert_eq!(restored.work_dir(), &PathBuf::from("/tmp/other"));
        assert_eq!(restored.run_id(), "run-2");
        assert!(restored.get("github_token").is_none());
        assert!(restored.get("shell.api_key").is_none());
        assert!(restored.get("shell.stdout").is_none());
        assert!(restored.get("shell.stderr").is_none());
    }

    #[test]
    fn extract_tokens_simple_and_namespaced() {
        assert_eq!(extract_tokens("{artifact_dir}"), vec!["artifact_dir"]);
        assert_eq!(
            extract_tokens("{setup_workspace.existing_pr_number}"),
            vec!["setup_workspace.existing_pr_number"]
        );
    }

    #[test]
    fn extract_tokens_multiple_and_adjacent_text() {
        assert_eq!(
            extract_tokens("path/{artifact_dir}/x.json"),
            vec!["artifact_dir"]
        );
        assert_eq!(
            extract_tokens("{owner}/{repo}#{issue_number}"),
            vec!["owner", "repo", "issue_number"]
        );
    }

    #[test]
    fn extract_tokens_none_when_no_tokens() {
        assert!(extract_tokens("no tokens here").is_empty());
        assert!(extract_tokens("").is_empty());
    }

    #[test]
    fn extract_tokens_ignores_jq_object_braces() {
        // jq object construction contains spaces/commas/colons -> not tokens.
        assert!(extract_tokens("{number, title}").is_empty());
        assert!(extract_tokens("{title: .title, url: .url}").is_empty());
    }

    #[test]
    fn extract_tokens_ignores_shell_style_dollar_brace() {
        // Shell-style `${VAR}` is env/shell interpolation, not a Luther token.
        assert!(extract_tokens("echo ${HOME}").is_empty());
        assert!(extract_tokens("${FOO}/${BAR}").is_empty());
    }

    #[test]
    fn extract_tokens_distinguishes_dollar_brace_from_bare_brace() {
        // Bare `{VAR}` is still extracted; the adjacent `${VAR}` is skipped.
        assert_eq!(
            extract_tokens("${HOME}/{artifact_dir}/${USER}"),
            vec!["artifact_dir"]
        );
    }

    #[test]
    fn restore_checkpoint_values_filters_disallowed_namespaced_keys() {
        // A malicious payload carrying secret-like keys in a namespaced map
        // must be filtered through the allowlist on restore, so secrets never
        // re-enter the live context even if they slipped into persisted state.
        let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
        let mut malicious = HashMap::new();
        let mut inner = HashMap::new();
        inner.insert("issue_number".to_string(), "42".to_string());
        inner.insert("github_token".to_string(), "bearer-secret".to_string());
        inner.insert("api_key".to_string(), "key-secret".to_string());
        inner.insert("stdout".to_string(), "raw-output-secret".to_string());
        malicious.insert("shell".to_string(), inner);
        payload.insert(
            "__namespaced_vars".to_string(),
            serde_json::to_value(&malicious).unwrap(),
        );
        payload.insert(
            "issue_number".to_string(),
            serde_json::Value::String("42".into()),
        );

        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-restore".to_string());
        context
            .restore_checkpoint_values(payload)
            .expect("restore must not error on a well-formed payload");

        // Allowed key survives.
        assert_eq!(
            context.get("shell.issue_number").map(String::as_str),
            Some("42")
        );
        // Disallowed keys are dropped.
        assert!(context.get("shell.github_token").is_none());
        assert!(context.get("shell.api_key").is_none());
        assert!(context.get("shell.stdout").is_none());
    }

    #[test]
    fn restore_checkpoint_values_rejects_malformed_namespaced_payload() {
        // A structurally invalid `__namespaced_vars` value (inner map values
        // not strings) must fail closed with the serde error rather than
        // silently substituting an empty/default context.
        let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
        let mut malformed = HashMap::new();
        let mut inner: HashMap<String, serde_json::Value> = HashMap::new();
        inner.insert(
            "issue_number".to_string(),
            serde_json::Value::Number(serde_json::Number::from(42u64)),
        );
        malformed.insert("shell".to_string(), serde_json::to_value(&inner).unwrap());
        payload.insert(
            "__namespaced_vars".to_string(),
            serde_json::to_value(&malformed).unwrap(),
        );

        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-malformed".to_string());
        let result = context.restore_checkpoint_values(payload);
        assert!(
            result.is_err(),
            "malformed __namespaced_vars must fail closed, got: {result:?}"
        );
    }
}
