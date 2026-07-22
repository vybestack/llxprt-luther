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
    crate::persistence::sqlite::init_runs_schema_serialized(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize runs schema: {e}"))
    })?;
    crate::persistence::leases::init_leases_table(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize lease schema: {e}"))
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
    // Issue 158 slice 4 (freeze workspace authority): resolve the final,
    // immutable `work_dir` BEFORE `StepContext` construction, from the
    // authoritative RunContext.workspace_path, then the config `work_dir`
    // variable, then the per-run temp default. The runner construction path
    // must not create directories (callers like the CLI/daemon create the
    // workspace explicitly before runner construction), and the resulting
    // `StepContext::work_dir` is immutable for the lifetime of the run so a
    // shell step cannot redirect workspace-mutating cleanup verification.
    let work_dir = resolve_final_work_dir(instance, run_context);
    let mut context = match run_context {
        Some(run_context) => {
            StepContext::from_run_context(work_dir, instance.run_id.clone(), run_context)
        }
        None => StepContext::new(work_dir, instance.run_id.clone()),
    };
    // Issue 158 slice 6: reconstruct the ephemeral WorkspaceAuthorization from
    // the verified RunContext into the StepContext BEFORE any resumed step
    // executes. The authorization is dev/inode (ephemeral, never persisted),
    // reconstructed by resume surfaces from a freshly-verified workspace
    // descriptor. On a fresh launch this is None until the
    // `workspace_ownership_verify` graph step captures it; on resume it is
    // already populated here so a resumed shell step retains
    // descriptor-anchored authorization.
    if let Some(auth) = run_context.and_then(|ctx| ctx.workspace_authorization) {
        context.set_workspace_authorization(auth);
    }

    for (key, value) in &instance.config.variables {
        context.set(key, value);
    }
    seed_parent_orchestration_config(&mut context, instance);
    if let Some(run_context) = run_context {
        seed_run_context(&mut context, run_context);
    }

    // Issue 158 slice 4: keep the `work_dir` context variable in sync with the
    // immutable typed field for legacy interpolation consumers, but do NOT
    // allow a shell step to mutate the typed field via this variable. The
    // variable is updated here only from the already-resolved authoritative
    // `work_dir`, never from a later mutable set.
    let work_dir_str = context.work_dir().to_string_lossy().to_string();
    context.set("work_dir", &work_dir_str);

    seed_target_paths(&mut context, instance);
    seed_scope_control_policy(&mut context, instance);

    Ok(context)
}

/// Resolve the final, immutable `work_dir` for a run from the authoritative
/// sources in priority order: immutable `RunContext.workspace_path`, then the
/// trusted config `work_dir` variable, then the per-run temp default.
///
/// The runner construction path never creates the directory: callers (the CLI
/// `ensure_non_daemon_workspace`, the daemon `ensure_daemon_workspace`) create
/// the workspace explicitly before runner construction, preserving the
/// first-mutation ordering invariant. The returned path is the sole source of
/// truth for `StepContext::work_dir`, set once at construction and immutable
/// thereafter.
fn resolve_final_work_dir(
    instance: &WorkflowInstance,
    run_context: Option<&RunContext>,
) -> std::path::PathBuf {
    if let Some(run_context) = run_context {
        if let Some(workspace_path) = run_context.workspace_path.as_deref() {
            return std::path::PathBuf::from(workspace_path);
        }
    }
    if let Some(work_dir) = instance.config.variables.get("work_dir") {
        return std::path::PathBuf::from(work_dir);
    }
    std::env::temp_dir().join(&instance.run_id)
}

/// Seed the active serialized scope-control policy from the target profile into
/// the step context so executors consume the trusted config binding rather than
/// requiring the workflow topology to duplicate it.
fn seed_scope_control_policy(context: &mut StepContext, instance: &WorkflowInstance) {
    if let Some(profile) = &instance.config.target_profile {
        if profile.scope_control.enabled {
            if let Ok(json) = serde_json::to_string(&profile.scope_control) {
                context.set("scope_control_policy", &json);
            }
            // Seed derived charter context so the production task_charter
            // executor can resolve canonical acceptance criteria and non-goals
            // from trusted resolved config rather than test-only injected params.
            let sub_ids: Vec<&str> = profile
                .scope_control
                .subsystems
                .iter()
                .map(|s| s.id.as_str())
                .collect();
            context.set("task_charter_subsystem_ids", &sub_ids.join(","));
            let gates: Vec<&str> = profile
                .scope_control
                .mandatory_gates
                .iter()
                .map(String::as_str)
                .collect();
            context.set("task_charter_mandatory_gates", &gates.join(", "));
        }
    }
}

fn seed_run_context(context: &mut StepContext, run_context: &RunContext) {
    context.set(
        "daemon_managed_claim",
        if run_context.daemon_managed {
            "true"
        } else {
            "false"
        },
    );
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
