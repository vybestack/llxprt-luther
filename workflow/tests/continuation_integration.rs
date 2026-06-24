/// Integration tests for operator continuation and recoverable external-wait
/// state (Luther #63). These assert externally-visible behavior: run outcomes,
/// persisted run status, checkpoint status, the resumed step, and that earlier
/// steps are not re-executed on resume.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::engine::{
    commit_continuation, continuation_overrides, prepare_continuation, ContinuationKind,
    ContinuationRequest, RewindTarget,
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
use luther_workflow::workflow::target_profile::apply_target_profile_overrides;

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

/// Build a runner from an explicit config (used to exercise continuation
/// override re-application on reconstruction).
fn build_runner_with_config(
    run_id: &str,
    db_path: &std::path::Path,
    config: WorkflowConfig,
    registry: ExecutorRegistry,
) -> EngineRunner {
    let instance = WorkflowInstance::create_with_run_id(followup_workflow_type(), config, run_id);
    EngineRunner::with_db_path_and_context(instance, registry, db_path, Default::default())
        .expect("runner")
}

/// Executor that records the effective interpolation values it observed.
struct CapturingExecutor {
    captured: Arc<Mutex<HashMap<String, String>>>,
}

impl StepExecutor for CapturingExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let mut map = self.captured.lock().expect("captured lock");
        for key in ["target_repo", "issue_number", "work_dir", "artifact_dir"] {
            if let Some(value) = context.get(key) {
                map.insert(key.to_string(), value.clone());
            }
        }
        Ok(StepOutcome::Success)
    }
}

/// Registry whose steps all capture interpolation values into `captured`.
fn capturing_registry(captured: &Arc<Mutex<HashMap<String, String>>>) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    for (_step_id, step_type) in STEPS {
        registry.register(
            step_type,
            Box::new(CapturingExecutor {
                captured: Arc::clone(captured),
            }),
        );
    }
    registry
}

/// A `followup_config` whose `[variables]` hold the given target-profile values.
fn config_with_variables(vars: &[(&str, &str)]) -> WorkflowConfig {
    let mut config = followup_config();
    config.variables = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    config
}

/// Seed a terminal `Failed` run with a known `created_at` and populated history
/// (previous step/outcome + next-step candidates), plus a checkpoint per step.
fn seed_failed_with_history(db_path: &std::path::Path, run_id: &str, created_at: DateTime<Utc>) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    init_checkpoint_table(&conn).expect("checkpoint table");
    init_runs_table(&conn).expect("runs table");
    let mut md = RunMetadata::new(run_id, "continuation-test-v1", "continuation-test");
    md.status = RunStatus::Failed;
    md.created_at = created_at;
    md.current_step = Some("post_pr_failure_terminal".to_string());
    md.previous_step = Some("collect_ci_failures".to_string());
    md.previous_outcome = Some("fatal".to_string());
    md.next_step_candidates = vec!["post_pr_failure_terminal".to_string()];
    md.repository = Some("vybestack/llxprt-code".to_string());
    md.issue_number = Some(63);
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

/// Seed a terminal `Completed` run with a whitelisted `watch_pr_checks`
/// checkpoint (used to prove non-resumable runs are refused).
fn seed_completed_run(db_path: &std::path::Path, run_id: &str) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    init_checkpoint_table(&conn).expect("checkpoint table");
    init_runs_table(&conn).expect("runs table");
    let mut md = RunMetadata::new(run_id, "continuation-test-v1", "continuation-test");
    md.status = RunStatus::Completed;
    md.current_step = Some("post_pr_failure_terminal".to_string());
    md.repository = Some("vybestack/llxprt-code".to_string());
    md.issue_number = Some(63);
    md.pr_number = Some(2138);
    md.artifact_root = db_path.parent().map(|p| p.to_string_lossy().to_string());
    persist_run_with_conn(&conn, &md).expect("persist run");
    for step_id in ["watch_pr_checks", "post_pr_failure_terminal"] {
        let snapshot = StateSnapshot {
            status: "completed".to_string(),
            ..StateSnapshot::default()
        };
        let cp = Checkpoint::with_snapshot(run_id, step_id, snapshot);
        save_checkpoint_with_conn(&conn, &cp).expect("save checkpoint");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Assert the persisted run row reflects a reopened continuation that preserved
/// history: status Running at the resume step, original `created_at`, and the
/// previous step/outcome/candidates intact.
fn assert_reopened_preserved(
    db_path: &std::path::Path,
    run_id: &str,
    created_at: DateTime<Utc>,
    resume_step: &str,
) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    let md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(md.created_at, created_at, "created_at must be preserved");
    assert_eq!(md.status, RunStatus::Running, "run must be reopened");
    assert_eq!(
        md.current_step.as_deref(),
        Some(resume_step),
        "current_step must remain the resume step"
    );
    assert_eq!(
        md.previous_step.as_deref(),
        Some("collect_ci_failures"),
        "previous_step history must be preserved"
    );
    assert_eq!(
        md.previous_outcome.as_deref(),
        Some("fatal"),
        "previous_outcome history must be preserved"
    );
    assert_eq!(
        md.next_step_candidates,
        vec!["post_pr_failure_terminal".to_string()],
        "next_step_candidates must be preserved"
    );
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

#[test]
fn reconstruction_applies_effective_overrides_to_interpolation_context() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    // Blocking fix 1: a continuation must resume against the original run's
    // effective target/workspace/artifacts, not the static config defaults.
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "override-run-1";
    // Distinct, real (tempdir-backed) work/artifact dirs so the runner's
    // work_dir creation has no effect outside the test sandbox.
    let override_work = temp.path().join("override-workspace");
    let override_artifacts = temp.path().join("override-artifacts");
    let default_work = temp.path().join("default-workspace");
    let default_artifacts = temp.path().join("default-artifacts");

    // Persist a run row carrying NON-default identity.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        init_checkpoint_table(&conn).expect("checkpoint table");
        init_runs_table(&conn).expect("runs table");
        let mut md = RunMetadata::new(run_id, "continuation-test-v1", "continuation-test");
        md.status = RunStatus::Failed;
        md.repository = Some("vybestack/llxprt-luther".to_string());
        md.issue_number = Some(65);
        md.workspace_path = Some(override_work.to_string_lossy().to_string());
        md.artifact_root = Some(override_artifacts.to_string_lossy().to_string());
        persist_run_with_conn(&conn, &md).expect("persist run");
    }

    // Static config holds DEFAULT identity values.
    let mut config = config_with_variables(&[
        ("target_repo", "vybestack/llxprt-code"),
        ("repository_owner", "vybestack"),
        ("repository_name", "llxprt-code"),
        ("primary_issue_number", "1803"),
        ("work_dir", default_work.to_string_lossy().as_ref()),
        ("artifact_dir", default_artifacts.to_string_lossy().as_ref()),
    ]);

    // Reconstruct effective overrides from the metadata row and re-apply them,
    // mirroring main.rs::reconstruct_runner.
    let md = {
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        get_run_with_conn(&conn, run_id).unwrap().unwrap()
    };
    let overrides = continuation_overrides(&md);
    apply_target_profile_overrides(&mut config, &overrides).expect("apply overrides");

    let captured = Arc::new(Mutex::new(HashMap::new()));
    let mut runner =
        build_runner_with_config(run_id, &db_path, config, capturing_registry(&captured));
    let outcome = runner.run().expect("resume run");
    drop(runner);
    assert_eq!(outcome, RunOutcome::Success);

    let map = captured.lock().unwrap().clone();
    assert_eq!(
        map.get("target_repo").map(String::as_str),
        Some("vybestack/llxprt-luther"),
        "resumed steps must target the original repo, not the static default"
    );
    assert_eq!(
        map.get("issue_number").map(String::as_str),
        Some("65"),
        "resumed steps must use the original issue number"
    );
    assert_eq!(
        map.get("work_dir").map(String::as_str),
        Some(override_work.to_string_lossy().as_ref()),
        "resumed steps must use the original work_dir"
    );
    assert_eq!(
        map.get("artifact_dir").map(String::as_str),
        Some(override_artifacts.to_string_lossy().as_ref()),
        "resumed steps must use the original artifact_dir"
    );
}

#[test]
fn reconstruction_preserves_reopened_metadata_and_history() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    // Blocking fix 2: reconstructing the runner must not overwrite the reopened
    // run row (created_at/history/current_step) with a fresh Starting record.
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "preserve-run-1";
    let created_at = "2023-01-02T03:04:05+00:00"
        .parse::<DateTime<Utc>>()
        .expect("parse created_at");
    seed_failed_with_history(&db_path, run_id, created_at);

    // Plan + commit the continuation (re-stamp checkpoint + reopen run).
    let resume_step = {
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
        let step = plan.selected.as_ref().expect("selected").step_id.clone();
        commit_continuation(&conn, &request, &step).expect("commit");
        step
    };
    assert_eq!(resume_step, "collect_ci_failures");

    // Reconstruct the runner. BEFORE run(), the reopened row must be intact.
    let log = Arc::new(Mutex::new(Vec::new()));
    let runner = build_runner(run_id, &db_path, registry_with(StepOutcome::Success, &log));
    assert_reopened_preserved(&db_path, run_id, created_at, &resume_step);

    // Failure-before-run-start: drop the runner without run(); the reopened row
    // must still be intact (construction alone must not corrupt state).
    drop(runner);
    assert_reopened_preserved(&db_path, run_id, created_at, &resume_step);
}

#[test]
fn non_resumable_completed_run_is_refused_without_corrupting_state() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    // Blocking fix 3: a Completed run with a whitelisted checkpoint must be
    // refused and left unchanged (no resume point set).
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let run_id = "completed-run-1";
    seed_completed_run(&db_path, run_id);

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    let request = ContinuationRequest {
        run_id: run_id.to_string(),
        kind: ContinuationKind::Resume,
        force: true,
    };
    let plan = prepare_continuation(&conn, &request, &md).expect("prepare");
    assert!(
        !plan.validation.ok,
        "a completed run must not be resumable, even with force"
    );
    assert!(plan.selected.is_none());
    assert!(plan
        .validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("resumable_status")));

    // The run row is unchanged: still Completed, and the newest checkpoint is
    // the terminal step (no resume point set).
    let md_after = get_run_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(md_after.status, RunStatus::Completed);
    let newest = load_checkpoint_with_conn(&conn, run_id).unwrap().unwrap();
    assert_eq!(newest.step_id, "post_pr_failure_terminal");
}
