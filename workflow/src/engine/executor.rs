/// @plan:PLAN-20260408-STEP-EXEC.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P09
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-002,REQ-LF-CTX-003,REQ-LF-CTX-004
/// Executor module - step execution trait, registry, and context.
use std::collections::HashMap;
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
        variables.insert("work_dir".to_string(), work_dir.to_string_lossy().to_string());
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
        self.variables.get(key)
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
                .or_insert_with(HashMap::new)
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
        self.variables.insert("current_step_id".to_string(), step_id.to_string());
    }

    /// Set the working directory for step execution.
    /// Only changes the work_dir field without affecting variables or namespaced storage.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-LF-PROF-003
    pub fn set_work_dir(&mut self, work_dir: PathBuf) {
        self.work_dir = work_dir;
        // Update the flat variable for backward compatibility
        self.variables.insert("work_dir".to_string(), self.work_dir.to_string_lossy().to_string());
    }
}

/// Interpolate `{key}` and `{step_id.key}` placeholders in a template string.
///
/// Replaces all occurrences of `{key}` with the corresponding value from context.
/// Handles both namespaced (`{step_id.variable}`) and bare (`{variable}`) placeholders.
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

    // Sort keys by length descending to prevent partial replacements
    // (e.g., "{foo}" vs "{foobar}" or "{step.var}" vs "{step.variable}")
    all_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));

    // Iterate over all keys and replace
    for key in all_keys {
        let placeholder = format!("{{{key}}}");
        if let Some(value) = context.get(&key) {
            result = result.replace(&placeholder, value);
        }
    }

    result
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
    /// @requirement:REQ-LF-SEP-001
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register("shell", Box::new(crate::engine::executors::ShellExecutor));
        registry.register("write_file", Box::new(crate::engine::executors::WriteFileExecutor));
        registry.register("verify", Box::new(crate::engine::executors::VerifyExecutor));
        registry.register("llxprt", Box::new(crate::engine::executors::LlxprtExecutor));

        registry.register("noop", Box::new(crate::engine::executors::NoOpExecutor));
        registry
    }
}
