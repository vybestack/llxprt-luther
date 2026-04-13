/// @plan:PLAN-20260408-STEP-EXEC.P03
/// Executor module - step execution trait, registry, and context.
use std::collections::HashMap;
use std::path::PathBuf;

use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

// Re-export executors for convenience
pub use crate::engine::executors::noop::NoOpExecutor;

/// Context for step execution.
/// Stores key-value pairs for variable interpolation across steps.
#[derive(Debug)]
pub struct StepContext {
    /// Working directory for step execution
    work_dir: PathBuf,
    /// Unique identifier for this workflow run
    run_id: String,
    /// Storage for context values: key -> value
    variables: HashMap<String, String>,
}

impl StepContext {
    /// Create a new `StepContext` with the given `work_dir` and `run_id`.
    #[must_use]
    pub fn new(work_dir: PathBuf, run_id: String) -> Self {
        Self {
            work_dir,
            run_id,
            variables: HashMap::new(),
        }
    }

    /// Get a context value by key.
    /// Returns `None` if the key is not found.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&String> {
        self.variables.get(key)
    }

    /// Set a context value.
    pub fn set(&mut self, key: &str, value: &str) {
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
}

/// Interpolate `{key}` placeholders in a template string.
///
/// Replaces all occurrences of `{key}` with the corresponding value from context.
/// Undefined keys are left as-is (no error, no replacement).
/// No nested/recursive resolution - only one pass.
/// @plan:PLAN-20260408-STEP-EXEC.P05
#[must_use]
pub fn interpolate_string(template: &str, context: &StepContext) -> String {
    let mut result = template.to_string();

    // Collect all keys from context variables and built-ins
    let mut all_keys: Vec<String> = Vec::new();

    // Add context variable keys
    for key in context.variables.keys() {
        all_keys.push(key.clone());
    }

    // Add built-in keys (work_dir and run_id)
    all_keys.push("work_dir".to_string());
    all_keys.push("run_id".to_string());

    // Sort keys by length descending to prevent partial replacements
    // (e.g., "{foo}" vs "{foobar}")
    all_keys.sort_by_key(|b| std::cmp::Reverse(b.len()));

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
    /// Registers: `shell`, `write_file`, and `noop` executors.
    /// @plan:PLAN-20260408-STEP-EXEC.P05
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register("shell", Box::new(crate::engine::executors::ShellExecutor));
        registry.register("write_file", Box::new(crate::engine::executors::WriteFileExecutor));
        registry.register("noop", Box::new(crate::engine::executors::NoOpExecutor));
        registry
    }
}
