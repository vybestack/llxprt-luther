//! llxprt agent step executor.
//!
//! Spawns the `llxprt` agent CLI to perform a step's work. The binary is
//! configurable so deployments can point at a non-`PATH` install:
//!
//! - Step parameter `binary_path` (highest precedence). Supports `{...}`
//!   interpolation, e.g. `"{work_dir}/bin/llxprt"`.
//! - Workflow variable `llxprt_binary_path` (fallback for all steps).
//! - Default `"llxprt"` (resolved from `PATH`).
//!
//! The same resolution order is shared with the preflight gate in
//! [`crate::adapters::llxprt`] so they never diverge. Spawn failures map to the
//! typed [`EngineError::LlxprtBinaryNotFound`] (missing binary) and runtime
//! failure modes set a `llxprt_failure_reason` context variable
//! (`timeout` / `idle_timeout` / `agent_failure` / `no_diff` / `process_error`)
//! so callers can discriminate the cause.
//!
//! Success-by-diff detection is delegated to a [`ChangedPathDetector`] so the
//! brittle `git status` polling can be swapped, mode-selected
//! (tracked-only vs untracked-included), and unit-tested. The production
//! [`LlxprtExecutor`] uses [`GitChangedPathDetector`]; tests can inject a stub
//! via [`LlxprtExecutorWithDetector`].

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::change_detection::{
    ChangeDetectionMode, ChangedPathDetector, GitChangedPathDetector,
};
use crate::engine::executors::llxprt_diff::{
    detect_initial_changed_paths, string_array_param, success_condition_met, DiffDetection,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

mod artifact_paths;
mod artifacts;
mod outcome_scan;
mod process_control;

#[path = "llxprt_timeout.rs"]
mod llxprt_timeout;

/// Production llxprt executor (uses [`GitChangedPathDetector`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct LlxprtExecutor;

impl StepExecutor for LlxprtExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        execute_llxprt(context, params, &GitChangedPathDetector)
    }
}

/// Injectable llxprt executor for tests and alternate change detectors.
///
/// Mirrors the `*WithRunner` dependency-injection idiom used elsewhere in this
/// crate (e.g. `GithubPrChecksExecutorWithRunner`). The production
/// [`LlxprtExecutor`] is a thin wrapper that delegates to the same
/// change-detection execution path with a [`GitChangedPathDetector`].
pub struct LlxprtExecutorWithDetector<D> {
    detector: D,
}

impl<D> LlxprtExecutorWithDetector<D> {
    /// Construct an executor backed by the supplied change detector.
    pub fn new(detector: D) -> Self {
        Self { detector }
    }
}

impl<D: ChangedPathDetector> StepExecutor for LlxprtExecutorWithDetector<D> {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        execute_llxprt(context, params, &self.detector)
    }
}

fn llxprt_step_requires_scope_barrier(context: &StepContext) -> bool {
    context.get("current_step_id").is_some_and(|step_id| {
        matches!(
            step_id.as_str(),
            "implement" | "remediate" | "remediate_tests" | "remediate_pr_followup"
        )
    })
}

/// llxprt process orchestration entry point.
///
/// Parses the step configuration once, then delegates to focused phase helpers
/// (static-content handling, process polling, outcome classification) so each
/// stays within the complexity budget. The orchestration order mirrors the
/// historical behavior exactly.
fn execute_llxprt(
    context: &mut StepContext,
    params: &serde_json::Value,
    detector: &dyn ChangedPathDetector,
) -> Result<StepOutcome, EngineError> {
    std::fs::create_dir_all(context.work_dir()).map_err(|e| EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: format!("Failed to create work_dir: {e}"),
    })?;
    let artifacts = artifacts::DiagnosticArtifacts::initialize(context, params)?;

    if llxprt_step_requires_scope_barrier(context) {
        if let Some(outcome) = crate::engine::executors::scope_control_barrier(context) {
            return Ok(outcome);
        }
    }

    let config = LlxprtStepConfig::build(params, context, detector);

    if let Some(outcome) = handle_static_content(context, params, &config)? {
        return Ok(outcome);
    }

    if let Some(outcome) = handle_static_stdout(context, params, &config, &artifacts)? {
        return Ok(outcome);
    }

    let result = run_llxprt_process(context, params, &config, &artifacts)?;

    classify_llxprt_outcome(context, params, &config, &result)
}

/// Resolved configuration and success-condition inputs for an llxprt step.
///
/// Parsing once keeps the orchestration helpers within the argument-count lint
/// budget and documents the parameter contract in a single place.
struct LlxprtStepConfig<'a> {
    detection: DiffDetection<'a>,
    success_file: Option<String>,
    diff_gate: DiffGateConfig,
    required_changed_paths: Vec<String>,
    required_changed_path_patterns: Vec<String>,
    initial_changed_paths: Vec<String>,
    initial_success_condition_met: bool,
}

/// Boolean diff-gating parameters grouped to avoid an excessive-bool struct.
struct DiffGateConfig {
    success_on_diff: bool,
    early_success_on_diff: bool,
    continue_on_empty_diff: bool,
}

impl<'a> LlxprtStepConfig<'a> {
    /// Resolve all step parameters and the pre-run success-condition snapshot.
    fn build(
        params: &serde_json::Value,
        context: &mut StepContext,
        detector: &'a dyn ChangedPathDetector,
    ) -> Self {
        let detection = DiffDetection {
            detector,
            mode: ChangeDetectionMode::from_param(
                params
                    .get("change_detection_mode")
                    .and_then(serde_json::Value::as_str),
            ),
        };
        let success_file = interpolated_optional_str(params, "success_file", context);
        let success_on_diff = bool_param(params, "success_on_diff", false);
        let required_changed_paths = string_array_param(params, "required_changed_paths", context);
        let required_changed_path_patterns =
            string_array_param(params, "required_changed_path_patterns", context);
        let success_on_existing_diff = bool_param(params, "success_on_existing_diff", false);

        let initial_changed_paths = if success_on_existing_diff {
            Vec::new()
        } else {
            detect_initial_changed_paths(context, detection)
        };

        let initial_success_condition_met = !success_on_existing_diff
            && success_condition_met(
                context,
                detection,
                success_file.as_deref(),
                success_on_diff,
                &required_changed_paths,
                &required_changed_path_patterns,
                &[],
            );

        Self {
            diff_gate: DiffGateConfig {
                early_success_on_diff: bool_param(params, "early_success_on_diff", success_on_diff),
                continue_on_empty_diff: bool_param(params, "continue_on_empty_diff", false),
                success_on_diff,
            },
            detection,
            success_file,
            required_changed_paths,
            required_changed_path_patterns,
            initial_changed_paths,
            initial_success_condition_met,
        }
    }

    /// Whether this step gates success on a diff or success file at all.
    fn gates_on_diff(&self) -> bool {
        self.success_file.is_some() || self.diff_gate.success_on_diff
    }
}

/// Handle `static_content`: if present and non-empty, write it to the success
/// file (required) and return `Success`.
fn handle_static_content(
    context: &mut StepContext,
    params: &serde_json::Value,
    config: &LlxprtStepConfig<'_>,
) -> Result<Option<StepOutcome>, EngineError> {
    let Some(static_content) = str_param(params, "static_content") else {
        return Ok(None);
    };
    let content = interpolate_string(static_content, context);
    if content.trim().is_empty() {
        return Ok(None);
    }
    let Some(path_template) = config.success_file.as_deref() else {
        return Err(EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: "static_content requires success_file".to_string(),
        });
    };
    artifact_paths::write_file(context, path_template, &content)?;
    Ok(Some(StepOutcome::Success))
}

/// Handle `static_stdout`: when provided, capture it as the step's stdout
/// artifact and classify the resulting outcome through the diff gate.
fn handle_static_stdout(
    context: &mut StepContext,
    params: &serde_json::Value,
    config: &LlxprtStepConfig<'_>,
    artifacts: &artifacts::DiagnosticArtifacts,
) -> Result<Option<StepOutcome>, EngineError> {
    let Some(static_stdout) = str_param(params, "static_stdout") else {
        return Ok(None);
    };
    let stdout = interpolate_string(static_stdout, context);
    artifacts::append(&artifacts.stdout, stdout.as_bytes())?;
    artifacts.publish_manifest()?;
    context.set("stdout", &stdout);

    if let Some(outcome) = match_static_stdout_outcome(params, &stdout) {
        if outcome == StepOutcome::Success && config.gates_on_diff() {
            if let Some(fixable) = diff_gate_fixable(context, config)? {
                return Ok(Some(fixable));
            }
        }
        return Ok(Some(outcome));
    }
    if config.gates_on_diff() {
        if let Some(fixable) = diff_gate_fixable(context, config)? {
            return Ok(Some(fixable));
        }
    }
    Ok(Some(StepOutcome::Success))
}

/// Determine whether the diff/success-file gate makes the current outcome a
/// `Fixable` rather than `Success`. Returns `None` when the gate is satisfied
/// (so `Success` should stand).
fn diff_gate_fixable(
    context: &mut StepContext,
    config: &LlxprtStepConfig<'_>,
) -> Result<Option<StepOutcome>, EngineError> {
    if config.initial_success_condition_met {
        return Ok(Some(StepOutcome::Fixable));
    }
    let met = success_condition_met(
        context,
        config.detection,
        config.success_file.as_deref(),
        config.diff_gate.success_on_diff,
        &config.required_changed_paths,
        &config.required_changed_path_patterns,
        &config.initial_changed_paths,
    );
    if !met {
        return Ok(Some(StepOutcome::Fixable));
    }
    Ok(None)
}

/// Outcome of running the llxprt child process, carrying the captured streams
/// and exit status for the classifier.
struct ProcessResult {
    stdout: String,
    stderr: String,
    exit_status: std::process::ExitStatus,
    outcome_seen: Option<StepOutcome>,
    success_seen: bool,
    timed_out: bool,
    idle_timed_out: bool,
    timeout: Duration,
    idle_timeout: Option<Duration>,
}

type StreamReader = thread::JoinHandle<Result<(), EngineError>>;

struct ProcessStreams {
    stdout: artifacts::SharedCapture,
    stderr: artifacts::SharedCapture,
    outcome_scanner: outcome_scan::SharedScanner,
    stdout_reader: Option<StreamReader>,
    stderr_reader: Option<StreamReader>,
}

impl ProcessStreams {
    fn start(
        child: &mut std::process::Child,
        params: &serde_json::Value,
        artifacts: &artifacts::DiagnosticArtifacts,
    ) -> Self {
        let stdout = Arc::clone(&artifacts.stdout);
        let stderr = Arc::clone(&artifacts.stderr);
        let outcome_scanner = Arc::new(std::sync::Mutex::new(
            outcome_scan::OutcomeScanner::from_params(params),
        ));
        let stdout_reader = child.stdout.take().map(|mut pipe| {
            let capture = Arc::clone(&stdout);
            let scanner = Arc::clone(&outcome_scanner);
            thread::spawn(move || read_stream_into_buffer(&mut pipe, &capture, Some(&scanner)))
        });
        let stderr_reader = child.stderr.take().map(|mut pipe| {
            let capture = Arc::clone(&stderr);
            thread::spawn(move || read_stream_into_buffer(&mut pipe, &capture, None))
        });
        Self {
            stdout,
            stderr,
            outcome_scanner,
            stdout_reader,
            stderr_reader,
        }
    }

    fn join(&mut self) -> Result<(), EngineError> {
        join_stream_reader(self.stdout_reader.take(), "stdout")?;
        join_stream_reader(self.stderr_reader.take(), "stderr")
    }
}

/// Run the llxprt child process to completion, polling for success conditions
/// and timeouts.
fn run_llxprt_process(
    context: &mut StepContext,
    params: &serde_json::Value,
    config: &LlxprtStepConfig<'_>,
    artifacts: &artifacts::DiagnosticArtifacts,
) -> Result<ProcessResult, EngineError> {
    let mut child = spawn_llxprt(context, params)?;
    let mut streams = ProcessStreams::start(&mut child, params, artifacts);
    let mut poll = ProcessPoll::new(ProcessTiming::from_params(params));
    let outcome_seen = poll.run(
        context,
        config,
        &streams.stdout,
        &streams.stderr,
        &streams.outcome_scanner,
        &mut child,
    );
    let outcome_seen = match outcome_seen {
        Ok(outcome) => outcome,
        Err(error) => {
            terminate_and_drain(&mut child, &mut streams, artifacts)?;
            return Err(error);
        }
    };
    if should_terminate(context, &poll, outcome_seen) {
        process_control::terminate_process_tree(&mut child);
    }
    finish_llxprt_process(context, child, streams, poll, outcome_seen, artifacts)
}

fn should_terminate(
    context: &StepContext,
    poll: &ProcessPoll,
    outcome_seen: Option<StepOutcome>,
) -> bool {
    context.is_interrupted()
        || poll.success_seen
        || poll.timed_out(outcome_seen)
        || poll.idle_timed_out(outcome_seen)
        || outcome_seen.is_some()
}

fn terminate_and_drain(
    child: &mut std::process::Child,
    streams: &mut ProcessStreams,
    artifacts: &artifacts::DiagnosticArtifacts,
) -> Result<(), EngineError> {
    process_control::terminate_process_tree(child);
    let _ = child.wait();
    let _ = streams.join();
    artifacts.publish_manifest()
}

fn finish_llxprt_process(
    context: &mut StepContext,
    mut child: std::process::Child,
    mut streams: ProcessStreams,
    poll: ProcessPoll,
    outcome_seen: Option<StepOutcome>,
    artifacts: &artifacts::DiagnosticArtifacts,
) -> Result<ProcessResult, EngineError> {
    let exit_status = child
        .wait()
        .map_err(|error| EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to wait for llxprt: {error}"),
        })?;
    streams.join()?;
    let outcome_seen = outcome_seen.or_else(|| outcome_scan::finish(&streams.outcome_scanner));
    artifacts.publish_manifest()?;
    let stdout = artifacts::capture_text(&streams.stdout);
    let stderr = artifacts::capture_text(&streams.stderr);
    if let Some(code) = exit_status.code() {
        context.set("exit_code", &code.to_string());
    }
    context.set("stdout", &stdout);
    context.set("stderr", &stderr);
    Ok(ProcessResult {
        stdout,
        stderr,
        exit_status,
        outcome_seen,
        success_seen: poll.success_seen,
        timed_out: poll.timed_out(outcome_seen),
        idle_timed_out: poll.idle_timed_out(outcome_seen),
        timeout: poll.timing.timeout,
        idle_timeout: poll.timing.idle_timeout,
    })
}

/// Spawn the llxprt child process with the configured binary, profile, and
/// prompt.
fn spawn_llxprt(
    context: &mut StepContext,
    params: &serde_json::Value,
) -> Result<std::process::Child, EngineError> {
    let prompt = params
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .map_or_else(String::new, |template| {
            interpolate_string(template, context)
        });
    let profile =
        str_param(params, "profile").map(|template| interpolate_string(template, context));

    let binary_template = params
        .get(crate::adapters::llxprt::BINARY_PATH_PARAM)
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            context
                .get(crate::adapters::llxprt::BINARY_PATH_VARIABLE)
                .cloned()
        })
        .unwrap_or_else(|| crate::adapters::llxprt::DEFAULT_LLXPRT_BINARY.to_string());
    let binary = interpolate_string(&binary_template, context);

    let mut cmd = Command::new(&binary);
    cmd.arg("--set").arg("reasoning.includeInResponse=false");
    if let Some(profile) = profile.as_deref() {
        cmd.arg("--profile-load").arg(profile);
    }
    cmd.arg("--yolo").arg("-p").arg(&prompt);
    cmd.current_dir(context.work_dir());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());
    process_control::configure_process_group(&mut cmd);

    cmd.spawn().map_err(|e| spawn_error(context, &binary, e))
}

/// Map a process spawn error to the typed llxprt error, recording the failure
/// reason on the context.
fn spawn_error(context: &mut StepContext, binary: &str, e: std::io::Error) -> EngineError {
    context.set("llxprt_failure_reason", "process_error");
    if e.kind() == std::io::ErrorKind::NotFound {
        EngineError::LlxprtBinaryNotFound {
            path: binary.to_string(),
        }
    } else {
        EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to spawn llxprt at `{binary}`: {e}"),
        }
    }
}

/// Timing configuration parsed from step parameters.
struct ProcessTiming {
    timeout: Duration,
    min_runtime_before_success: Duration,
    max_runtime_after_required_diff: Option<Duration>,
    idle_timeout: Option<Duration>,
}

impl ProcessTiming {
    fn from_params(params: &serde_json::Value) -> Self {
        Self {
            timeout: duration_param(params, "timeout_seconds", 900),
            min_runtime_before_success: duration_param(
                params,
                "min_runtime_before_success_seconds",
                120,
            ),
            max_runtime_after_required_diff: params
                .get("max_runtime_after_required_diff_seconds")
                .and_then(serde_json::Value::as_u64)
                .map(Duration::from_secs),
            idle_timeout: params
                .get("idle_timeout_seconds")
                .and_then(serde_json::Value::as_u64)
                .map(Duration::from_secs),
        }
    }
}

/// Mutable poll-state for the llxprt process loop.
struct ProcessPoll {
    timing: ProcessTiming,
    start: Instant,
    last_progress: Instant,
    last_output_change: Instant,
    required_diff_seen_at: Option<Instant>,
    success_seen: bool,
}

impl ProcessPoll {
    fn new(timing: ProcessTiming) -> Self {
        let now = Instant::now();
        Self {
            timing,
            start: now,
            last_progress: now,
            last_output_change: now,
            required_diff_seen_at: None,
            success_seen: false,
        }
    }

    /// Whether the wall-clock timeout fired without success.
    fn timed_out(&self, outcome_seen: Option<StepOutcome>) -> bool {
        self.start.elapsed() >= self.timing.timeout && !self.success_seen && outcome_seen.is_none()
    }

    /// Whether the idle timeout fired without success.
    fn idle_timed_out(&self, outcome_seen: Option<StepOutcome>) -> bool {
        self.timing
            .idle_timeout
            .is_some_and(|timeout| self.last_output_change.elapsed() >= timeout)
            && !self.success_seen
            && outcome_seen.is_none()
    }

    /// Run the polling loop until the process exits or a terminal condition is
    /// reached. Returns any outcome marker seen before exit.
    fn run(
        &mut self,
        context: &mut StepContext,
        config: &LlxprtStepConfig<'_>,
        stdout_buffer: &artifacts::SharedCapture,
        stderr_buffer: &artifacts::SharedCapture,
        outcome_scanner: &outcome_scan::SharedScanner,
        child: &mut std::process::Child,
    ) -> Result<Option<StepOutcome>, EngineError> {
        let mut stdout_snapshot_len = 0usize;
        let mut stderr_snapshot_len = 0usize;
        let mut outcome_seen = None;
        while self.start.elapsed() < self.timing.timeout {
            if context.is_interrupted() {
                break;
            }
            if self.check_idle_timeout(
                stdout_buffer,
                stderr_buffer,
                &mut stdout_snapshot_len,
                &mut stderr_snapshot_len,
            ) {
                break;
            }

            if outcome_seen.is_none() {
                outcome_seen = outcome_scan::detected(outcome_scanner);
            }
            if outcome_seen.is_some() {
                break;
            }

            if child
                .try_wait()
                .map_err(|e| EngineError::StepExecutionError {
                    step_id: "llxprt".to_string(),
                    message: format!("Failed to poll llxprt: {e}"),
                })?
                .is_some()
            {
                break;
            }

            if self.check_success_condition(context, config)? {
                self.success_seen = true;
                break;
            }

            self.log_progress(
                stdout_buffer,
                stderr_buffer,
                &mut stdout_snapshot_len,
                &mut stderr_snapshot_len,
            );

            thread::sleep(Duration::from_secs(2));
        }
        Ok(outcome_seen)
    }

    /// Track idle-output changes. Returns `true` when the idle timeout has
    /// elapsed since the last output change.
    fn check_idle_timeout(
        &mut self,
        stdout_buffer: &artifacts::SharedCapture,
        stderr_buffer: &artifacts::SharedCapture,
        stdout_snapshot_len: &mut usize,
        stderr_snapshot_len: &mut usize,
    ) -> bool {
        let Some(idle_timeout) = self.timing.idle_timeout else {
            return false;
        };
        let stdout_len = artifacts::total_bytes(stdout_buffer);
        let stderr_len = artifacts::total_bytes(stderr_buffer);
        if stdout_len != *stdout_snapshot_len || stderr_len != *stderr_snapshot_len {
            self.last_output_change = Instant::now();
        }
        self.last_output_change.elapsed() >= idle_timeout
    }

    /// Evaluate the success condition and advance the required-diff-seen timer.
    /// Returns `true` when the step should terminate with success.
    fn check_success_condition(
        &mut self,
        context: &mut StepContext,
        config: &LlxprtStepConfig<'_>,
    ) -> Result<bool, EngineError> {
        if config.initial_success_condition_met {
            return Ok(false);
        }
        if !success_condition_met(
            context,
            config.detection,
            config.success_file.as_deref(),
            config.diff_gate.early_success_on_diff,
            &config.required_changed_paths,
            &config.required_changed_path_patterns,
            &config.initial_changed_paths,
        ) {
            self.required_diff_seen_at = None;
            return Ok(false);
        }
        Ok(self.advance_required_diff())
    }

    /// Advance the required-diff-seen state and decide whether the minimum
    /// runtime / max-runtime-after-diff thresholds are satisfied.
    fn advance_required_diff(&mut self) -> bool {
        match self.required_diff_seen_at {
            Some(seen_at) => {
                self.start.elapsed() >= self.timing.min_runtime_before_success
                    || self
                        .timing
                        .max_runtime_after_required_diff
                        .is_some_and(|max_runtime| seen_at.elapsed() >= max_runtime)
            }
            None => {
                self.required_diff_seen_at = Some(Instant::now());
                self.start.elapsed() >= self.timing.min_runtime_before_success
                    || self
                        .timing
                        .max_runtime_after_required_diff
                        .is_some_and(|d| d.is_zero())
            }
        }
    }

    /// Periodically log progress. Reader threads publish stream snapshots as
    /// bytes arrive, so this path only reports monotonic byte counters.
    fn log_progress(
        &mut self,
        stdout_buffer: &artifacts::SharedCapture,
        stderr_buffer: &artifacts::SharedCapture,
        stdout_snapshot_len: &mut usize,
        stderr_snapshot_len: &mut usize,
    ) {
        if self.last_progress.elapsed() < Duration::from_secs(30) {
            return;
        }
        let elapsed = self.start.elapsed().as_secs();
        let stdout_len = artifacts::total_bytes(stdout_buffer);
        let stderr_len = artifacts::total_bytes(stderr_buffer);
        println!(
            "[llxprt] running for {elapsed}s (stdout {stdout_len} bytes, stderr {stderr_len} bytes)"
        );
        *stdout_snapshot_len = stdout_len;
        *stderr_snapshot_len = stderr_len;
        self.last_progress = Instant::now();
    }
}

/// Classify the final outcome after the process has exited.
fn classify_llxprt_outcome(
    context: &mut StepContext,
    params: &serde_json::Value,
    config: &LlxprtStepConfig<'_>,
    result: &ProcessResult,
) -> Result<StepOutcome, EngineError> {
    if let Some(outcome) = result.outcome_seen {
        context.set(
            "diagnostic",
            "llxprt stdout outcome marker seen before process exit",
        );
        if outcome == StepOutcome::Success && config.gates_on_diff() {
            if let Some(fixable) = diff_gate_fixable(context, config)? {
                return Ok(fixable);
            }
        }
        return Ok(outcome);
    }

    if result.success_seen {
        context.set(
            "diagnostic",
            "llxprt success condition met before process exit",
        );
        return Ok(StepOutcome::Success);
    }

    if result.timed_out || result.idle_timed_out {
        return resolve_timeout_outcome(context, config, result);
    }

    if !result.exit_status.success() {
        context.set("llxprt_failure_reason", "agent_failure");
        let diagnostic = result.exit_status.code().map_or_else(
            || "llxprt exited without an exit code".to_string(),
            |code| format!("llxprt exited with status {code}"),
        );
        context.set("diagnostic", &diagnostic);
        return Ok(match_exit_code_outcome(params, result.exit_status.code())
            .unwrap_or(StepOutcome::Fatal));
    }

    if let Some(outcome) = match_static_stdout_outcome(params, &result.stdout) {
        if outcome == StepOutcome::Success && config.gates_on_diff() {
            if let Some(fixable) = diff_gate_fixable(context, config)? {
                return Ok(fixable);
            }
        }
        return Ok(outcome);
    }

    if !config.initial_success_condition_met
        && success_condition_met(
            context,
            config.detection,
            config.success_file.as_deref(),
            config.diff_gate.success_on_diff,
            &config.required_changed_paths,
            &config.required_changed_path_patterns,
            &config.initial_changed_paths,
        )
    {
        return Ok(StepOutcome::Success);
    }

    if config.gates_on_diff() {
        return Ok(resolve_no_diff_outcome(context, config, result));
    }

    Ok(StepOutcome::Success)
}

/// Resolve the outcome when the process exited but no required diff was
/// produced.
fn resolve_no_diff_outcome(
    context: &mut StepContext,
    config: &LlxprtStepConfig<'_>,
    result: &ProcessResult,
) -> StepOutcome {
    if config.diff_gate.continue_on_empty_diff
        && (!result.stdout.trim().is_empty() || !result.stderr.trim().is_empty())
    {
        context.set(
            "diagnostic",
            "llxprt process exited after making no additional required changes",
        );
        StepOutcome::Success
    } else {
        context.set("llxprt_failure_reason", "no_diff");
        StepOutcome::Fixable
    }
}

/// Resolve the outcome after a wall-clock or idle timeout, attempting partial
/// timeout recovery when scope control is active.
fn resolve_timeout_outcome(
    context: &mut StepContext,
    config: &LlxprtStepConfig<'_>,
    result: &ProcessResult,
) -> Result<StepOutcome, EngineError> {
    context.set("exit_code", "124");
    context.set(
        "llxprt_failure_reason",
        if result.idle_timed_out {
            "idle_timeout"
        } else {
            "timeout"
        },
    );
    let diagnostic = llxprt_timeout::timeout_diagnostic(result);
    context.set("diagnostic", &diagnostic);

    let timeout_kind = if result.idle_timed_out {
        crate::engine::executors::scope_control::timeout_recovery::TimeoutKind::IdleTimeout
    } else {
        crate::engine::executors::scope_control::timeout_recovery::TimeoutKind::Timeout
    };
    if let Some(outcome) = llxprt_timeout::recover_partial_timeout(
        context,
        &config.initial_changed_paths,
        config.detection,
        timeout_kind,
    )? {
        return Ok(outcome);
    }
    Ok(StepOutcome::Fatal)
}

fn str_param<'a>(params: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(serde_json::Value::as_str)
}

/// Read an interpolated optional string step parameter.
fn interpolated_optional_str(
    params: &serde_json::Value,
    name: &str,
    context: &StepContext,
) -> Option<String> {
    str_param(params, name).map(|template| interpolate_string(template, context))
}

/// Read a boolean step parameter with a default.
fn bool_param(params: &serde_json::Value, name: &str, default: bool) -> bool {
    params
        .get(name)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

/// Read a duration step parameter with a default (in seconds).
fn duration_param(params: &serde_json::Value, name: &str, default_secs: u64) -> Duration {
    params
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_secs))
}

fn join_stream_reader(
    reader: Option<thread::JoinHandle<Result<(), EngineError>>>,
    stream: &str,
) -> Result<(), EngineError> {
    let Some(reader) = reader else {
        return Ok(());
    };
    reader.join().map_err(|_| EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: format!("llxprt {stream} reader panicked"),
    })?
}

fn read_stream_into_buffer<R: Read>(
    reader: &mut R,
    buffer: &artifacts::SharedCapture,
    scanner: Option<&outcome_scan::SharedScanner>,
) -> Result<(), EngineError> {
    let mut bytes = [0_u8; 4096];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                artifacts::append(buffer, &bytes[..n])?;
                if let Some(shared) = scanner {
                    if let Ok(mut guard) = shared.lock() {
                        if let Some(scan) = guard.as_mut() {
                            scan.append(&bytes[..n]);
                        }
                    }
                }
            }
            Err(error) => {
                return Err(EngineError::StepExecutionError {
                    step_id: "llxprt".to_string(),
                    message: format!("Failed to read llxprt stream: {error}"),
                });
            }
        }
    }
}

fn match_exit_code_outcome(
    params: &serde_json::Value,
    exit_code: Option<i32>,
) -> Option<StepOutcome> {
    let code = exit_code?.to_string();
    let outcome_name = params
        .get("exit_code_map")?
        .as_object()?
        .get(&code)?
        .as_str()?;
    Some(parse_outcome_name(outcome_name))
}

fn match_static_stdout_outcome(params: &serde_json::Value, stdout: &str) -> Option<StepOutcome> {
    let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
    for (pattern, outcome_value) in pattern_map {
        if contains_outcome_marker_line(stdout, pattern) {
            return outcome_value.as_str().map(parse_outcome_name);
        }
    }
    None
}

#[cfg(test)]
fn match_stdout_outcome(
    params: &serde_json::Value,
    stdout_buffer: &artifacts::SharedCapture,
) -> Option<StepOutcome> {
    let stdout = artifacts::capture_text(stdout_buffer);
    let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
    for (pattern, outcome_value) in pattern_map {
        if contains_outcome_marker_line(&stdout, pattern) {
            return outcome_value.as_str().map(parse_outcome_name);
        }
    }
    None
}

fn contains_outcome_marker_line(stdout: &str, marker: &str) -> bool {
    stdout.lines().any(|line| line.trim() == marker)
}

fn parse_outcome_name(name: &str) -> StepOutcome {
    match name {
        "success" => StepOutcome::Success,
        "fixable" => StepOutcome::Fixable,
        "fatal" => StepOutcome::Fatal,
        "retryable" => StepOutcome::Retryable,
        "abandon" => StepOutcome::Abandon,
        _ => StepOutcome::Fatal,
    }
}

#[cfg(test)]
#[path = "llxprt_tests.rs"]
mod tests;
