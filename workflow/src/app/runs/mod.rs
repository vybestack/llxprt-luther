use super::monitor::monitor_state_token;
use super::status::{next_step_label, pid_liveness_label, run_metadata_to_json};
#[cfg(test)]
use luther_workflow::engine::runner::RunOutcome;
use luther_workflow::monitor::heartbeat::{read_all_heartbeats, MonitorState};
use luther_workflow::persistence::{
    list_artifacts, load_events, RunMetadata, RunStatus, SqliteStore,
};
use std::process;

/// Open the persistent run registry store at the shared checkpoints.db.
///
/// Returns `Ok(None)` when the database file does not exist yet (treated as an
/// empty registry), `Ok(Some(store))` when opened, and `Err` when the file is
/// present but cannot be opened (surfaced distinctly from "no runs").
/// @plan:issue-51
pub fn open_runs_store() -> Result<Option<SqliteStore>, String> {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if !db_path.exists() {
        return Ok(None);
    }
    SqliteStore::open(&db_path)
        .map(Some)
        .map_err(|e| format!("failed to open run registry at {}: {e}", db_path.display()))
}

/// Dispatch the `runs` command family (issue #51).
/// @plan:issue-51
pub async fn handle_runs_command(args: &luther_workflow::cli::RunsArgs) {
    use luther_workflow::cli::RunsCommand;
    match &args.command {
        RunsCommand::List(list_args) => handle_runs_list(list_args),
        RunsCommand::Show(show_args) => handle_runs_show(show_args),
        RunsCommand::Tail(tail_args) => handle_runs_tail(tail_args).await,
        RunsCommand::Ps(ps_args) => handle_runs_ps(ps_args).await,
        RunsCommand::Checkpoints(cp_args) => handle_runs_checkpoints(cp_args),
        RunsCommand::Resume(resume_args) => handle_runs_resume(resume_args),
        RunsCommand::Retry(retry_args) => handle_runs_retry(retry_args),
        RunsCommand::Rewind(rewind_args) => handle_runs_rewind(rewind_args),
        RunsCommand::MigrateLegacyOwnership(migrate_args) => {
            handle_runs_migrate_legacy_ownership(migrate_args)
        }
    }
}

/// `runs checkpoints RUN_ID` — list every per-step checkpoint for a run.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn handle_runs_checkpoints(args: &luther_workflow::cli::RunsCheckpointsArgs) {
    let store = require_runs_store(&args.run_id);
    let checkpoints =
        match luther_workflow::persistence::list_checkpoints(store.conn(), &args.run_id) {
            Ok(cps) => cps,
            Err(e) => {
                eprintln!(
                    "Error: failed to list checkpoints for '{}': {e}",
                    args.run_id
                );
                process::exit(1);
            }
        };
    if args.json {
        print_checkpoints_json(&args.run_id, &checkpoints);
    } else {
        print_checkpoints_human(&args.run_id, &checkpoints);
    }
}

/// Render checkpoints as JSON.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn print_checkpoints_json(
    run_id: &str,
    checkpoints: &[luther_workflow::persistence::Checkpoint],
) {
    let rows: Vec<serde_json::Value> = checkpoints
        .iter()
        .map(|c| {
            serde_json::json!({
                "step_id": c.step_id,
                "checkpoint_id": luther_workflow::engine::continuation::checkpoint_identity(c),
                "status": c.state_snapshot.status,
                "timestamp": c.timestamp.to_rfc3339(),
                "loop_count": c.state_snapshot.loop_count,
                "retry_count": c.state_snapshot.retry_count,
                "context_keys": c.state_snapshot.context.len(),
            })
        })
        .collect();
    let doc = serde_json::json!({ "run_id": run_id, "checkpoints": rows });
    println!(
        "{}",
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    );
}

/// Render checkpoints as a human-readable table.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn print_checkpoints_human(
    run_id: &str,
    checkpoints: &[luther_workflow::persistence::Checkpoint],
) {
    if checkpoints.is_empty() {
        println!("No checkpoints recorded for run '{run_id}'.");
        return;
    }
    println!("Checkpoints for run '{run_id}':");
    println!(
        "  {:<26} {:<16} {:<6} {:<6} TIMESTAMP",
        "STEP", "STATUS", "LOOP", "RETRY"
    );
    for c in checkpoints {
        println!(
            "  {:<26} {:<16} {:<6} {:<6} {}",
            c.step_id,
            c.state_snapshot.status,
            c.state_snapshot.loop_count,
            c.state_snapshot.retry_count,
            c.timestamp.to_rfc3339(),
        );
    }
}

/// `runs resume RUN_ID` — resume from the latest resumable checkpoint via
/// `RecoveryProtocolV1`.
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
pub fn handle_runs_resume(args: &luther_workflow::cli::RunsResumeArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: args.force,
    };
    execute_operator_recovery(&store, &md, &request);
}

/// `runs retry RUN_ID [--from-failed-step]` — retry an external-wait step via
/// `RecoveryProtocolV1`.
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
pub fn handle_runs_retry(args: &luther_workflow::cli::RunsRetryArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: args.from_failed_step,
        },
        force: args.force,
    };
    execute_operator_recovery(&store, &md, &request);
}

/// `runs rewind RUN_ID (--to-step S | --to-checkpoint ID)` — rewind the resume
/// point to an earlier checkpoint via `RecoveryProtocolV1`.
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
pub fn handle_runs_rewind(args: &luther_workflow::cli::RunsRewindArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let target = if let Some(step) = &args.to_step {
        luther_workflow::engine::RewindTarget::ToStep(step.clone())
    } else if let Some(id) = &args.to_checkpoint {
        luther_workflow::engine::RewindTarget::ToCheckpoint(id.clone())
    } else {
        eprintln!("Error: rewind requires --to-step or --to-checkpoint");
        process::exit(1);
    };
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Rewind { target },
        force: args.force,
    };
    execute_operator_recovery(&store, &md, &request);
}

/// Bounded entrypoint for operator recovery: dispatch through the
/// `RecoveryProtocolV1` wiring and exit with the outcome code.
fn execute_operator_recovery(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &luther_workflow::engine::ContinuationRequest,
) {
    let step = request_kind_step_hint(&request.kind, md);
    match recovery_wiring::recover_operator_run(store, md, request) {
        Ok(result) => {
            let code = recovery_wiring::report_recovery_outcome(&request.run_id, &step, &result);
            process::exit(if result.maintenance_failed && code == 0 {
                1
            } else {
                code
            });
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

/// Derive a step hint for reporting from the continuation kind and metadata.
fn request_kind_step_hint(
    kind: &luther_workflow::engine::ContinuationKind,
    md: &RunMetadata,
) -> String {
    match kind {
        luther_workflow::engine::ContinuationKind::Rewind {
            target: luther_workflow::engine::RewindTarget::ToStep(step),
        } => step.clone(),
        _ => md.current_step.clone().unwrap_or_default(),
    }
}

mod continuation_execution;
pub use continuation_execution::*;

mod diagnostics;
mod inspect;
pub use inspect::*;

mod legacy_migration;
pub use legacy_migration::*;

mod recovery_wiring;

#[cfg(test)]
#[path = "runs_tests.rs"]
mod runs_tests;
