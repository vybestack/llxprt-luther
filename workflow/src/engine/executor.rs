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
    /// If current_step_id is set, also stores in namespaced_vars[step_id][key].
    /// Always stores bare key in variables for backward compatibility.
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
/// shell-style `${VAR}` references do not match (the leading `$` is outside the
/// brace and the brace content alone is still matched only if it is a strict
/// identifier — `${VAR}` yields `VAR`, but callers pass interpolation templates
/// where `$`-prefixed forms are not used as Luther tokens).
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
    re.captures_iter(template)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
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

    /// Create a registry with default executors pre-registered.
    ///
    /// Registers: `shell`, `write_file`, `verify`, and `noop` executors.
    /// @plan:PLAN-20260408-STEP-EXEC.P05
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-LF-SEP-001,REQ-PRFU-020
    /// @pseudocode lines 1-53
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register("shell", Box::new(crate::engine::executors::ShellExecutor));
        registry.register(
            "write_file",
            Box::new(crate::engine::executors::WriteFileExecutor),
        );
        registry.register("verify", Box::new(crate::engine::executors::VerifyExecutor));
        registry.register("llxprt", Box::new(crate::engine::executors::LlxprtExecutor));

        registry.register("noop", Box::new(crate::engine::executors::NoOpExecutor));
        registry.register(
            "github_pr_identity",
            Box::new(
                crate::engine::executors::GithubPrIdentityExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        registry.register(
            "post_pr_iteration_guard",
            Box::new(crate::engine::executors::PostPrIterationGuardExecutor),
        );
        registry.register(
            "github_pr_checks",
            Box::new(
                crate::engine::executors::GithubPrChecksExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        registry.register(
            "github_check_failures",
            Box::new(
                crate::engine::executors::GithubCheckFailuresExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        registry.register(
            "github_coderabbit_feedback",
            Box::new(
                crate::engine::executors::GithubCodeRabbitFeedbackExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemFeedbackClock,
                ),
            ),
        );
        registry.register(
            "feedback_evaluator",
            Box::new(crate::engine::executors::FeedbackEvaluatorExecutor::new(
                crate::engine::executors::CommandFeedbackEvaluationAdapter::new(
                    crate::engine::executors::default_feedback_evaluator_argv(),
                    crate::engine::executors::ProcessFeedbackEvaluatorCommandRunner::default(),
                ),
                crate::engine::executors::SystemClockSleeper,
            )),
        );
        registry.register(
            "pr_remediation_plan",
            Box::new(crate::engine::executors::PrRemediationPlanExecutor),
        );
        registry.register(
            "pr_followup_remediation",
            Box::new(
                crate::engine::executors::PrFollowupRemediationExecutorWithRunner::new(
                    crate::engine::executors::SystemPrFollowupLlxprtCommandRunner,
                    crate::engine::executors::SystemClockSleeper,
                ),
            ),
        );
        registry.register(
            "pr_remediation_result",
            Box::new(crate::engine::executors::PrRemediationResultExecutor),
        );
        registry.register(
            "run_post_pr_tests",
            Box::new(crate::engine::executors::RunPostPrTestsExecutor),
        );
        registry.register(
            "push_remediation_changes",
            Box::new(crate::engine::executors::PushRemediationChangesExecutor),
        );
        registry.register(
            "github_feedback_marker",
            Box::new(
                crate::engine::executors::GithubFeedbackMarkerExecutorWithRunner::new(
                    crate::engine::executors::SystemGithubPrCommandRunner,
                    crate::engine::executors::SystemFeedbackClock,
                ),
            ),
        );
        registry.register(
            "post_pr_failure_terminal",
            Box::new(crate::engine::executors::PostPrFailureTerminalExecutor),
        );
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
}
