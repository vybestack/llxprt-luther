use super::status::install_interrupt_handlers;
use super::wait_state::persist_external_wait_state;
use luther_workflow::adapters::github::{run_preflight, GithubError, SystemGithubCommandRunner};
use luther_workflow::adapters::llxprt::{
    run_preflight as run_llxprt_preflight, LlxprtError, SystemLlxprtCommandRunner,
};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::persistence::{get_run_with_conn, init_database};
use luther_workflow::workflow::config_loader::{
    resolve_workflow, resolve_workflow_config, resolve_workflow_type,
    validate_artifact_dependencies, validate_workflow_tokens,
};
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
    TargetProfileOverrides,
};
use std::process;

#[path = "daemon_run.rs"]
mod daemon_run;

// Re-export the daemon launcher so `daemon::mod` can reach it via
// `super::run::DaemonWorkflowLauncher`, preserving the public API surface
// after the source-size decomposition.
pub use daemon_run::DaemonWorkflowLauncher;

/// Report dry-run semantic validation: unresolved interpolation tokens and
/// missing artifact producers. Returns `true` if any error was reported.
///
/// Output uses stable, greppable prefixes (`unresolved token:` /
/// `missing artifact producer:`) so callers and tests can assert on them.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
pub fn report_dry_run_validation(workflow_type: &WorkflowType, config: &WorkflowConfig) -> bool {
    let unresolved = validate_workflow_tokens(workflow_type, config);
    let missing = validate_artifact_dependencies(workflow_type);

    if !unresolved.is_empty() {
        println!("\nUnresolved interpolation tokens:");
        for token in &unresolved {
            println!(
                "  unresolved token: step '{}' {} references '{{{}}}'",
                token.step_id, token.parameter_path, token.token_name
            );
        }
    }

    if !missing.is_empty() {
        println!("\nMissing artifact producers:");
        for producer in &missing {
            println!(
                "  missing artifact producer: step '{}' consumes '{}' which no step produces",
                producer.consumer_step_id, producer.artifact_name
            );
        }
    }

    !unresolved.is_empty() || !missing.is_empty()
}

/// Determine whether the selected workflow actually depends on the GitHub CLI.
///
/// Returns `true` when any step is a registered `github_*` step type, or any
/// shell step's `command` parameter contains a `gh ` token. Pure
/// `shell`/`noop` workflows that never call `gh` return `false` so offline runs
/// are unaffected by the preflight gate.
pub fn workflow_requires_github(workflow_type: &WorkflowType) -> bool {
    workflow_type.steps.iter().any(|step| {
        if step.step_type.starts_with("github_") {
            return true;
        }
        step.parameters
            .as_ref()
            .and_then(|params| params.get("command"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|command| command.contains("gh "))
    })
}

/// Print actionable diagnostics for a failed GitHub preflight using a stable,
/// greppable prefix, then exit the process without creating any state.
pub fn fail_preflight(err: &GithubError) -> ! {
    eprintln!("gh preflight failed: {err}");
    let diagnostics = err.get_diagnostics();
    let mut keys: Vec<&String> = diagnostics.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = diagnostics.get(key) {
            eprintln!("  {key}: {value}");
        }
    }
    process::exit(1);
}

/// Returns `true` when any step is a `llxprt` step that actually spawns the
/// binary (i.e. is not a pure `static_content` / `static_stdout` step). Pure
/// static workflows never invoke `llxprt`, so the preflight gate is skipped.
pub fn workflow_requires_llxprt(workflow_type: &WorkflowType) -> bool {
    workflow_type.steps.iter().any(|step| {
        if step.step_type != "llxprt" {
            return false;
        }
        step.parameters.as_ref().is_none_or(|params| {
            let has_static = params
                .get("static_content")
                .and_then(serde_json::Value::as_str)
                .is_some()
                || params
                    .get("static_stdout")
                    .and_then(serde_json::Value::as_str)
                    .is_some();
            !has_static
        })
    })
}

/// Print actionable diagnostics for a failed llxprt preflight using a stable,
/// greppable prefix, then exit the process without creating any state.
pub fn fail_llxprt_preflight(err: &LlxprtError) -> ! {
    eprintln!("llxprt preflight failed: {err}");
    let diagnostics = err.get_diagnostics();
    let mut keys: Vec<&String> = diagnostics.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = diagnostics.get(key) {
            eprintln!("  {key}: {value}");
        }
    }
    process::exit(1);
}

/// Handle the run command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12, PLAN-20260408-LLXPRT-FIRST.P20
pub async fn handle_run_command(args: &luther_workflow::cli::RunArgs) {
    let config_root = run_config_root(args);
    let (workflow_type, mut config, run_ref) = resolve_run_inputs(args, &config_root);
    apply_run_target_overrides(args, &workflow_type, &mut config);

    let run_id = run_ref.run_id;
    validate_cli_run_id(&run_id);
    println!("Starting workflow run: {run_id}");
    println!("  Workflow type: {}", workflow_type.workflow_type_id);
    println!("  Config: {}", config.config_id);

    run_start_preflights(args, &workflow_type, &config);
    // Issue 158 slice 4: a dry run must exit BEFORE any state mutation. The
    // checkpoint database and workspace directory are side effects of a real
    // run; a dry run only reports the planned step sequence and validation
    // results, so it must not create the DB or the workspace. Exiting here
    // (after preflights, before `init_database` and workspace creation) keeps
    // the dry run side-effect-free: no DB file, no workspace directory.
    if args.dry_run {
        finish_dry_run(&workflow_type, &config);
    }
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Err(e) = init_database(&db_path) {
        eprintln!("Warning: Failed to initialize checkpoint database: {e}");
    }

    let launch_provenance = match luther_workflow::persistence::LaunchProvenance::from_resolved(
        &workflow_type,
        &config,
        &config_root,
    ) {
        Ok(provenance) => provenance,
        Err(error) => {
            eprintln!("Error: failed to record launch provenance: {error}");
            process::exit(1);
        }
    };
    let workspace_config = config.clone();
    // Reserve the fresh run id and persist provenance before publishing any
    // workspace ownership evidence. A collision or DB failure leaves no marker.
    let mut runner = create_durable_runner_with_provenance(
        workflow_type,
        config,
        &run_id,
        &db_path,
        false,
        launch_provenance,
        &config_root,
    )
    .unwrap_or_else(|error| {
        eprintln!("Error: Failed to create durable engine runner: {error}");
        process::exit(1);
    });
    if let Err(error) = daemon_run::ensure_non_daemon_workspace(&workspace_config, &run_id) {
        eprintln!("Workspace initialization error: {error}");
        process::exit(1);
    }
    install_interrupt_handlers(runner.interrupt_handle());
    println!("Executing workflow...");
    match runner.run() {
        Ok(outcome) => exit_run_outcome(outcome, &run_id),
        Err(e) => {
            eprintln!("\nWorkflow execution error: {e}");
            process::exit(1);
        }
    }
}

fn validate_cli_run_id(run_id: &str) {
    if !is_valid_run_id(run_id) {
        eprintln!("Error: run id must contain only ASCII letters, digits, '-' or '_'");
        process::exit(1);
    }
}

/// Whether a run id is a safe identifier.
fn is_valid_run_id(run_id: &str) -> bool {
    if run_id.is_empty() {
        return false;
    }
    for ch in run_id.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' {
            return false;
        }
    }
    true
}

pub fn run_config_root(args: &luther_workflow::cli::RunArgs) -> std::path::PathBuf {
    args.config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"))
}

pub fn resolve_run_inputs(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    match &args.config {
        Some(config_path) => resolve_explicit_run_config(args, config_root, config_path),
        None => resolve_default_run_config(args, config_root),
    }
}

pub fn resolve_explicit_run_config(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
    config_path: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    let config_id = config_path.file_stem().map_or_else(
        || "default".to_string(),
        |s| s.to_string_lossy().to_string(),
    );
    let workflow_type_id = args
        .workflow_type
        .clone()
        .unwrap_or_else(|| "test-workflow".to_string());
    let workflow_type = resolve_workflow_type(&workflow_type_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve workflow type '{workflow_type_id}': {e}");
        process::exit(1);
    });
    let config = resolve_workflow_config(&config_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve config '{config_id}': {e}");
        process::exit(1);
    });
    let run_ref = luther_workflow::workflow::schema::WorkflowRunRef::new(
        &workflow_type_id,
        &config_id,
        args.run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    );
    (workflow_type, config, run_ref)
}

pub fn resolve_default_run_config(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    let workflow_type_id = args
        .workflow_type
        .clone()
        .unwrap_or_else(|| "test-workflow".to_string());
    let config_id = "test-config".to_string();
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    resolve_workflow(&workflow_type_id, &config_id, &run_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve workflow: {e}");
        process::exit(1);
    })
}

pub fn apply_run_target_overrides(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &mut WorkflowConfig,
) {
    let overrides = TargetProfileOverrides {
        repo: args.repo.clone(),
        issue: args.issue.clone(),
        work_dir: args.work_dir.clone(),
        artifact_dir: args.artifact_dir.clone(),
    };
    if let Err(e) = apply_target_profile_overrides(config, &overrides) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
    if target_profile_validation_required(&workflow_type.workflow_type_id, config, &overrides) {
        if let Err(e) = validate_target_profile(config) {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

pub fn run_start_preflights(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
) {
    if args.dry_run || args.skip_preflight {
        return;
    }
    run_github_preflight_if_needed(args, workflow_type, config);
    run_llxprt_preflight_if_needed(workflow_type, config);
}

pub fn run_github_preflight_if_needed(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
) {
    if !workflow_requires_github(workflow_type) {
        return;
    }
    let repo = config
        .variables
        .get("target_repo")
        .cloned()
        .or_else(|| args.repo.clone());
    let Some(repo) = repo else {
        return;
    };
    let runner = SystemGithubCommandRunner;
    match run_preflight(&runner, &repo, &["repo"]) {
        Ok(report) => println!(
            "  GitHub preflight OK: repo {} (scopes: {})",
            report.repo,
            report.scopes.join(", ")
        ),
        Err(e) => fail_preflight(&e),
    }
}

pub fn run_llxprt_preflight_if_needed(workflow_type: &WorkflowType, config: &WorkflowConfig) {
    if workflow_requires_llxprt(workflow_type) {
        let runner = SystemLlxprtCommandRunner;
        match run_llxprt_preflight(&runner, workflow_type, &config.variables) {
            Ok(paths) => println!("  llxprt preflight OK: validated {}", paths.join(", ")),
            Err(e) => fail_llxprt_preflight(&e),
        }
    }
}

pub fn finish_dry_run(workflow_type: &WorkflowType, config: &WorkflowConfig) -> ! {
    println!("Dry run mode - workflow would execute the following steps:");
    for step in &workflow_type.steps {
        println!(
            "  - {} ({}): {:?}",
            step.step_id,
            step.step_type,
            step.description.as_deref().unwrap_or("No description")
        );
    }
    if report_dry_run_validation(workflow_type, config) {
        eprintln!("\nDry run found validation errors. No changes made.");
        process::exit(1);
    }
    println!("\nDry run complete. No changes made.");
    process::exit(0);
}

pub fn exit_run_outcome(outcome: RunOutcome, run_id: &str) -> ! {
    match outcome {
        RunOutcome::Success => {
            println!("\nWorkflow completed successfully!");
            println!("Run ID: {run_id}");
            process::exit(0);
        }
        RunOutcome::Failure { step_id, reason } => {
            eprintln!("\nWorkflow failed at step '{step_id}'");
            eprintln!("Reason: {reason}");
            process::exit(1);
        }
        RunOutcome::Abandoned { step_id, reason } => {
            eprintln!("\nWorkflow abandoned at step '{step_id}'");
            eprintln!("Reason: {reason}");
            process::exit(1);
        }
        RunOutcome::Interrupted { step_id } => {
            println!("\nWorkflow interrupted at step '{step_id}'");
            println!("Run ID: {run_id} (can be resumed)");
            process::exit(130);
        }
        RunOutcome::WaitingExternal { step_id, reason } => {
            println!("\nWorkflow paused at step '{step_id}' awaiting external state");
            println!("Reason: {reason}");
            println!("Run ID: {run_id}");
            println!("Resume with: luther-workflow runs resume {run_id}");
            process::exit(0);
        }
    }
}

/// Create a durable runner for a **fresh launch**, atomically inserting the
/// initial `Starting` `RunMetadata` row with the recorded launch provenance
/// and the immutable `ExecutionCapsuleV1` in one SQLite `IMMEDIATE`
/// transaction.
///
/// Uses [`EngineRunner::with_db_path_for_launch`] so a `run_id` collision,
/// capsule collision, or DB error fails closed (propagates an error) rather
/// than silently overwriting an existing run record or leaving an orphan
/// capsule. The capsule is built from the exact resolved post-override
/// workflow/config/config-root/provenance/base-ref before the runner is
/// constructed.
///
/// **Issue 158 finding 1:** this function returns `Result` and never calls
/// `process::exit`. The CLI top-level maps the error to a non-zero exit; the
/// daemon and child paths propagate the error so it can be surfaced through the
/// launcher seam / parent orchestration result.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
pub fn create_durable_runner_with_provenance(
    workflow_type: luther_workflow::workflow::schema::WorkflowType,
    config: luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
    db_path: &std::path::Path,
    daemon_managed: bool,
    launch_provenance: luther_workflow::persistence::LaunchProvenance,
    config_root: &std::path::Path,
) -> Result<EngineRunner, String> {
    // Build the immutable ExecutionCapsuleV1 from the exact resolved
    // post-override workflow/config/config-root/provenance/base-ref BEFORE
    // any are moved into the instance. The capsule is the launch authority
    // and must exist before any workflow execution/effects.
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
    let base_ref = config
        .repo
        .base_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    let capsule = luther_workflow::engine::recovery::capsule::build_capsule_v1(
        run_id.to_string(),
        &workflow_type,
        &config,
        config_root,
        &launch_provenance,
        base_ref,
    )
    .map_err(|error| format!("build execution capsule: {error}"))?;

    let mut run_context = build_run_context(&config, run_id);
    run_context.daemon_managed = daemon_managed;
    run_context.launch_provenance = Some(launch_provenance);
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    // Attach the run context up front so the initial persisted `Starting` row
    // includes path and GitHub metadata, instead of chaining
    // `with_run_context` after the initial record has already been written.
    // Fail closed on collision/persistence error rather than best-effort
    // overwriting. The capsule is atomically persisted in the same
    // transaction.
    EngineRunner::with_db_path_for_launch(instance, registry, db_path, run_context, capsule)
        .map_err(|error| error.to_string())
}

/// Build a [`RunContext`] from a workflow config and run id, populating run
/// paths (log/artifact/workspace) and GitHub references when available.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn build_run_context(
    config: &luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
) -> luther_workflow::engine::RunContext {
    let vars = &config.variables;
    let repository = vars.get("target_repo").cloned();
    let issue_number = vars
        .get("primary_issue_number")
        .or_else(|| vars.get("issue_number"))
        .and_then(|s| s.parse::<i64>().ok());
    let workspace_path = Some(vars.get("work_dir").cloned().unwrap_or_else(|| {
        luther_workflow::runtime_paths::get_run_dir(run_id)
            .to_string_lossy()
            .to_string()
    }));
    let log_path = Some(
        luther_workflow::runtime_paths::get_log_dir()
            .join(format!("{run_id}.log"))
            .to_string_lossy()
            .to_string(),
    );
    let artifact_root = Some(vars.get("artifact_dir").cloned().unwrap_or_else(|| {
        luther_workflow::runtime_paths::get_artifacts_root()
            .to_string_lossy()
            .to_string()
    }));
    luther_workflow::engine::RunContext {
        daemon_managed: false,
        log_path,
        artifact_root,
        workspace_path,
        repository,
        issue_number,
        pr_number: None,
        head_sha: None,
        workspace_authorization: None,
        // build_run_context is used by create_durable_runner*; the launch
        // provenance is injected by the caller via the run_context field.
        launch_provenance: None,
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
