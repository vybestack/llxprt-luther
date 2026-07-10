//! Private helpers extracted from the runner: log previews, checkpoint loading,
//! step-context construction, and outcome translation that previously lived in a
//! textually-included tail fragment.
use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use crate::engine::executor::StepContext;
use crate::engine::instance::WorkflowInstance;
use crate::engine::transition::StepOutcome;
use crate::persistence::load_checkpoint_with_conn;

use super::target_path_context::seed_target_paths;
use super::{EngineError, RunContext, RunOutcome};

pub(super) fn preview_for_log(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

pub(super) fn open_initialized_connection(db_path: &Path) -> Result<Connection, EngineError> {
    let conn = Connection::open(db_path)
        .map_err(|e| EngineError::PersistenceError(format!("Failed to open database: {}", e)))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| {
            EngineError::PersistenceError(format!("Failed to set database busy timeout: {e}"))
        })?;

    crate::persistence::checkpoint::init_checkpoint_table(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize checkpoint schema: {e}"))
    })?;
    crate::persistence::run_metadata::init_runs_table(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize runs schema: {e}"))
    })?;

    Ok(conn)
}

pub(super) fn load_checkpoint_state(
    conn: &Connection,
    run_id: &str,
) -> (u32, HashMap<String, u32>) {
    if let Ok(Some(checkpoint)) = load_checkpoint_with_conn(conn, run_id) {
        (
            checkpoint.state_snapshot.retry_count,
            checkpoint.state_snapshot.edge_loop_counts.clone(),
        )
    } else {
        (0, HashMap::new())
    }
}

pub(super) fn build_step_context(
    instance: &WorkflowInstance,
    run_context: Option<&RunContext>,
) -> Result<StepContext, EngineError> {
    let work_dir = std::env::temp_dir().join(&instance.run_id);
    let mut context = StepContext::new(work_dir, instance.run_id.clone());

    for (key, value) in &instance.config.variables {
        context.set(key, value);
    }
    seed_parent_orchestration_config(&mut context, instance);
    if let Some(run_context) = run_context {
        seed_run_context(&mut context, run_context);
    }

    if let Some(work_dir_str) = context.get("work_dir").cloned() {
        let path = std::path::PathBuf::from(work_dir_str);
        std::fs::create_dir_all(&path).map_err(|e| {
            EngineError::InvalidState(format!(
                "Failed to create work_dir '{}': {}",
                path.display(),
                e
            ))
        })?;
        context.set_work_dir(path);
    }

    seed_target_paths(&mut context, instance);

    Ok(context)
}

fn seed_run_context(context: &mut StepContext, run_context: &RunContext) {
    if let Some(workspace_path) = run_context.workspace_path.as_deref() {
        context.set("work_dir", workspace_path);
        context.set("workspace_path", workspace_path);
    }
    if let Some(artifact_root) = run_context.artifact_root.as_deref() {
        context.set("artifact_root", artifact_root);
        context.set("artifact_dir", artifact_root);
    }
    if let Some(repository) = run_context.repository.as_deref() {
        context.set("target_repo", repository);
    }
    if let Some(issue_number) = run_context.issue_number {
        let issue_number = issue_number.to_string();
        context.set("issue_number", &issue_number);
        context.set("primary_issue_number", &issue_number);
    }
}

fn seed_parent_orchestration_config(context: &mut StepContext, instance: &WorkflowInstance) {
    let config = &instance.config.parent_orchestration;
    context.set(
        "parent_orchestration.auto_merge_children",
        if config.auto_merge_children {
            "true"
        } else {
            "false"
        },
    );
    context.set(
        "parent_orchestration.wait_for_human_merge",
        if config.wait_for_human_merge {
            "true"
        } else {
            "false"
        },
    );
    context.set(
        "parent_orchestration.merge_poll_interval_seconds",
        &config.merge_poll_interval_seconds.to_string(),
    );
    if let Some(max_wait) = config.max_child_merge_wait_seconds {
        context.set(
            "parent_orchestration.max_child_merge_wait_seconds",
            &max_wait.to_string(),
        );
    }
    context.set(
        "parent_orchestration.child_workflow_type_id",
        &config.child_workflow_type_id,
    );
    context.set(
        "parent_orchestration.child_config_id",
        &config.child_config_id,
    );
}

pub(super) fn run_outcome_without_transition(step_id: &str, outcome: &StepOutcome) -> RunOutcome {
    match outcome {
        StepOutcome::Success => RunOutcome::Success,
        StepOutcome::Fatal => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Fatal error occurred".to_string(),
        },
        StepOutcome::Fixable => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Fixable error with no recovery transition".to_string(),
        },
        StepOutcome::Retryable => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Retryable error with no recovery transition".to_string(),
        },
        _ => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Unexpected outcome".to_string(),
        },
    }
}
