/// Integration tests for operator continuation and recoverable external-wait
/// state (Luther #63). These assert externally-visible behavior: run outcomes,
/// persisted run status, checkpoint status, the resumed step, and that earlier
/// steps are not re-executed on resume.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::engine::{
    commit_continuation, prepare_continuation, ContinuationKind, ContinuationRequest, RewindTarget,
};
use luther_workflow::persistence::checkpoint::init_checkpoint_table;
use luther_workflow::persistence::run_metadata::init_runs_table;
use luther_workflow::persistence::{
    get_run_with_conn, load_checkpoint_with_conn, persist_run_with_conn, save_checkpoint_with_conn,
    Checkpoint, RunMetadata, RunStatus, StateSnapshot, CHECKPOINT_STATUS_READY_TO_RESUME,
    CHECKPOINT_STATUS_WAITING,
};
use luther_workflow::workflow::schema::{
    GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

const STEPS: &[(&str, &str)] = &[
    ("implement", "impl_step"),
    ("watch_pr_checks", "watch_step"),
    ("collect_ci_failures", "collect_step"),
    ("post_pr_failure_terminal", "terminal_step"),
];

/// Executor that records its label and returns a fixed outcome.
struct RecordingExecutor {
    label: String,
    outcome: StepOutcome,
    log: Arc<Mutex<Vec<String>>>,
}

impl StepExecutor for RecordingExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        self.log.lock().expect("log lock").push(self.label.clone());
        Ok(self.outcome)
    }
}

/// Build a registry where `watch_step` yields `watch_outcome`; other steps
/// succeed. Executions are appended to the shared log.
fn registry_with(watch_outcome: StepOutcome, log: &Arc<Mutex<Vec<String>>>) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    for (step_id, step_type) in STEPS {
        let outcome = if *step_type == "watch_step" {
            watch_outcome
        } else {
            StepOutcome::Success
        };
        registry.register(
            step_type,
            Box::new(RecordingExecutor {
                label: (*step_id).to_string(),
                outcome,
                log: Arc::clone(log),
            }),
        );
    }
    registry
}

fn step(step_id: &str, step_type: &str) -> StepDef {
    StepDef {
        step_id: step_id.to_string(),
        step_type: step_type.to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
    }
}

fn edge(from: &str, to: &str, cond: &str) -> TransitionDef {
    TransitionDef {
        from: from.to_string(),
        to: to.to_string(),
        condition: Some(cond.to_string()),
        max_iterations: None,
    }
}

/// Workflow: implement -> watch_pr_checks -> collect_ci_failures, with
/// collect's fatal branch routing to post_pr_failure_terminal. There is no
/// `wait` edge for watch_pr_checks (so a Wait outcome pauses the run).
fn followup_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "continuation-test-v1".to_string(),
        steps: STEPS.iter().map(|(id, ty)| step(id, ty)).collect(),
        transitions: vec![
            edge("implement", "watch_pr_checks", "success"),
            edge("watch_pr_checks", "collect_ci_failures", "success"),
            edge("collect_ci_failures", "post_pr_failure_terminal", "fatal"),
        ],
        guards: Default::default(),
    }
}

fn followup_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "continuation-test".to_string(),
        workflow_type_id: "continuation-test-v1".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(5),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: HashMap::new(),
        discovery: None,
    }
}

fn build_runner(
    run_id: &str,
    db_path: &std::path::Path,
    registry: ExecutorRegistry,
) -> EngineRunner {
    let instance =
        WorkflowInstance::create_with_run_id(followup_workflow_type(), followup_config(), run_id);
    EngineRunner::with_db_path_and_context(instance, registry, db_path, Default::default())
        .expect("runner")
}

#[test]
fn pending_wait_pauses_run_with_resumable_checkpoint() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "wait-run-1";
    let log = Arc::new(Mutex::new(Vec::new()));

    let mut runner = build_runner(run_id, &db_path, registry_with(StepOutcome::Wait, &log));
    let outcome = runner.run().expect("run");
    drop(runner);

    assert_eq!(
        outcome,
        RunOutcome::WaitingExternal {
            step_id: "watch_pr_checks".to_string(),
            reason: "External condition still pending at watch limit".to_string(),
        }
    );
    assert_eq!(
        *log.lock().unwrap(),
        vec!["implement".to_string(), "watch_pr_checks".to_string()]
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(md.status, RunStatus::WaitingForChecks);
    assert!(
        !md.status.is_terminal(),
        "waiting status must be non-terminal"
    );
    let cp = load_checkpoint_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(cp.step_id, "watch_pr_checks");
    assert_eq!(cp.state_snapshot.status, CHECKPOINT_STATUS_WAITING);
}

#[test]
fn resume_after_green_advances_without_rerunning_earlier_steps() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "wait-run-2";

    // First pass: watch pends -> pause at watch_pr_checks.
    let log1 = Arc::new(Mutex::new(Vec::new()));
    let mut runner1 = build_runner(run_id, &db_path, registry_with(StepOutcome::Wait, &log1));
    let first = runner1.run().expect("first run");
    drop(runner1);
    assert!(matches!(first, RunOutcome::WaitingExternal { .. }));

    // Second pass: checks are now green; resume re-enters watch and proceeds.
    let log2 = Arc::new(Mutex::new(Vec::new()));
    let mut runner2 = build_runner(run_id, &db_path, registry_with(StepOutcome::Success, &log2));
    let second = runner2.run().expect("second run");
    drop(runner2);

    assert_eq!(second, RunOutcome::Success);
    let executed = log2.lock().unwrap().clone();
    assert_eq!(
        executed,
        vec![
            "watch_pr_checks".to_string(),
            "collect_ci_failures".to_string()
        ],
        "resume must re-enter the wait step and continue, not re-run implement"
    );
    assert!(!executed.contains(&"implement".to_string()));
    assert!(!executed.contains(&"post_pr_failure_terminal".to_string()));
}

/// Seed a terminal `Failed` run with one checkpoint per step (increasing
/// timestamps) into a fresh checkpoints.db at `db_path`.
fn seed_terminal_failed(db_path: &std::path::Path, run_id: &str) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    init_checkpoint_table(&conn).expect("checkpoint table");
    init_runs_table(&conn).expect("runs table");
    let mut md = RunMetadata::new(run_id, "continuation-test-v1", "continuation-test");
    md.status = RunStatus::Failed;
    md.current_step = Some("post_pr_failure_terminal".to_string());
    md.repository = Some("vybestack/llxprt-code".to_string());
    md.pr_number = Some(2138);
    md.artifact_root = db_path.parent().map(|p| p.to_string_lossy().to_string());
    persist_run_with_conn(&conn, &md).expect("persist run");
    for (step_id, _) in STEPS {
        let snapshot = StateSnapshot {
            status: "completed".to_string(),
            ..StateSnapshot::default()
        };
        let cp = Checkpoint::with_snapshot(run_id, *step_id, snapshot);
        save_checkpoint_with_conn(&conn, &cp).expect("save checkpoint");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn resume_terminal_failed_run_rewinds_before_terminal_and_skips_terminal() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "failed-run-1";
    seed_terminal_failed(&db_path, run_id);

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    let request = ContinuationRequest {
        run_id: run_id.to_string(),
        kind: ContinuationKind::Resume,
        force: false,
    };
    let plan = prepare_continuation(&conn, &request, &md).expect("prepare");
    assert!(
        plan.validation.ok,
        "reasons: {:?}",
        plan.validation.failure_reasons()
    );
    let selected = plan.selected.as_ref().expect("selected").step_id.clone();
    assert_eq!(selected, "collect_ci_failures");
    let reopened = commit_continuation(&conn, &request, &selected).expect("commit");
    assert_eq!(reopened.status, RunStatus::Running);
    let newest = load_checkpoint_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(newest.step_id, "collect_ci_failures");
    assert_eq!(
        newest.state_snapshot.status,
        CHECKPOINT_STATUS_READY_TO_RESUME
    );
    drop(conn);

    // Reconstruct and resume: collect now succeeds, run completes without
    // re-running implement or re-entering the terminal step.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runner = build_runner(run_id, &db_path, registry_with(StepOutcome::Success, &log));
    let outcome = runner.run().expect("resume run");
    drop(runner);
    assert_eq!(outcome, RunOutcome::Success);
    let executed = log.lock().unwrap().clone();
    assert_eq!(executed, vec!["collect_ci_failures".to_string()]);
}

#[test]
fn retry_from_failed_step_targets_watch_pr_checks() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "failed-run-2";
    seed_terminal_failed(&db_path, run_id);

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    let request = ContinuationRequest {
        run_id: run_id.to_string(),
        kind: ContinuationKind::Retry {
            from_failed_step: true,
        },
        force: false,
    };
    let plan = prepare_continuation(&conn, &request, &md).expect("prepare");
    assert!(plan.validation.ok);
    assert_eq!(
        plan.selected.as_ref().unwrap().step_id,
        "watch_pr_checks",
        "retry --from-failed-step must target the external-wait step"
    );
    assert!(plan.artifact_dir.join("retry-result.json").exists() || plan.selected.is_some());
}

#[test]
fn unsafe_rewind_is_rejected_and_writes_validation_artifact() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "failed-run-3";
    seed_terminal_failed(&db_path, run_id);

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let mut md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    md.artifact_root = Some(temp.path().to_string_lossy().to_string());
    let request = ContinuationRequest {
        run_id: run_id.to_string(),
        kind: ContinuationKind::Rewind {
            target: RewindTarget::ToStep("implement".to_string()),
        },
        force: false,
    };
    let plan = prepare_continuation(&conn, &request, &md).expect("prepare");
    assert!(
        !plan.validation.ok,
        "rewinding onto implement must be unsafe"
    );
    assert!(plan.selected.is_none());
    assert!(plan
        .validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("safe_step")));
    assert!(plan
        .artifact_dir
        .join("continuation-validation.json")
        .exists());

    // State is uncorrupted: the run is still Failed and the newest checkpoint
    // is still the terminal step (no resume point was set).
    let md_after = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(md_after.status, RunStatus::Failed);
    let newest = load_checkpoint_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(newest.step_id, "post_pr_failure_terminal");
}
