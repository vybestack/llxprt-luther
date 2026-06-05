/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Workflow instance management - binds workflow type and config into an executable runtime object.
use uuid::Uuid;

use crate::workflow::schema::{WorkflowConfig, WorkflowType};

/// A concrete workflow instance ready for execution.
/// Contains the merged type and config, a unique run_id, and the current state.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-ENG-001
#[derive(Debug, Clone)]
pub struct WorkflowInstance {
    /// The workflow type defining the topology and steps.
    pub workflow_type: WorkflowType,
    /// The runtime configuration for this instance.
    pub config: WorkflowConfig,
    /// Unique identifier for this run (UUID v4).
    pub run_id: String,
    /// Current state - the step_id of the active step.
    pub current_state: String,
}

impl WorkflowInstance {
    /// Create a new workflow instance from a workflow type and config.
    /// Generates a new UUID for the run_id and sets initial state to the first step.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-ENG-001,REQ-EARS-ARCH-004
    pub fn create(workflow_type: WorkflowType, config: WorkflowConfig) -> Self {
        // Generate a new UUID v4 for the run_id
        let run_id = Uuid::new_v4().to_string();

        // Set initial state to the first step defined in the workflow type
        let current_state = workflow_type
            .steps
            .first()
            .map(|step| step.step_id.clone())
            .unwrap_or_default();

        Self {
            workflow_type,
            config,
            run_id,
            current_state,
        }
    }

    /// Create a new workflow instance with a specific run_id (for testing or replay).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn create_with_run_id(
        workflow_type: WorkflowType,
        config: WorkflowConfig,
        run_id: impl Into<String>,
    ) -> Self {
        let current_state = workflow_type
            .steps
            .first()
            .map(|step| step.step_id.clone())
            .unwrap_or_default();

        Self {
            workflow_type,
            config,
            run_id: run_id.into(),
            current_state,
        }
    }

    /// Get the workflow type ID.
    pub fn workflow_type_id(&self) -> &str {
        &self.workflow_type.workflow_type_id
    }

    /// Get the config ID.
    pub fn config_id(&self) -> &str {
        &self.config.config_id
    }

    /// Transition to a new state (step).
    pub fn transition_to(&mut self, new_state: impl Into<String>) {
        self.current_state = new_state.into();
    }
}
