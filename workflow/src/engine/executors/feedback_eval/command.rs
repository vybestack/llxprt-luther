//! Process-execution seam for the feedback evaluator command.

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::*;

/// Argv-safe command runner seam for the production feedback evaluator adapter.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
pub trait FeedbackEvaluatorCommandRunner: Send + Sync {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, EngineError>;
}

/// Production command runner that passes structured request JSON on stdin and never invokes a shell.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
#[derive(Clone, Debug, Default)]
pub struct ProcessFeedbackEvaluatorCommandRunner {
    timeout: Option<Duration>,
}

impl ProcessFeedbackEvaluatorCommandRunner {
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
        }
    }

    fn timeout(&self) -> Duration {
        self.timeout
            .unwrap_or_else(super::super::feedback_eval_timeout::default_evaluator_timeout)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl FeedbackEvaluatorCommandRunner for ProcessFeedbackEvaluatorCommandRunner {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, EngineError> {
        let (program, args) = argv.split_first().ok_or_else(|| {
            feedback_eval_error("feedback evaluator command argv must not be empty")
        })?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Run the evaluator in its own process group so that if it spawns
        // descendants (which can inherit our stdout/stderr pipes) we can
        // terminate the entire tree on the error and timeout paths instead of
        // leaking processes that keep the pipes—and thus our reader threads—
        // alive. This mirrors the process_group(0) convention used by the
        // llxprt and command-manifest executors.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|err| {
            feedback_eval_error(format!("spawn feedback evaluator command: {err}"))
        })?;

        // Drain stdout and stderr on dedicated reader threads BEFORE writing
        // stdin and waiting for the child. Reading only after the process
        // exits can deadlock if either pipe fills while the child is still
        // running: the child blocks on a full pipe, and we block waiting for
        // it to exit. This mirrors the concurrent-draining pattern used by the
        // llxprt executor.
        let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
        let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

        run_evaluator_child(
            &mut child,
            stdin_json,
            self.timeout(),
            stdout_reader,
            stderr_reader,
        )
    }
}

type ReaderHandle = thread::JoinHandle<std::io::Result<Vec<u8>>>;

/// Spawn a reader thread that drains a child pipe to end-of-file, returning the
/// accumulated bytes (or the read error) when the pipe closes.
fn spawn_pipe_reader(mut pipe: impl Read + Send + 'static) -> ReaderHandle {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.read_to_end(&mut buffer).map(|_| buffer)
    })
}

/// Drive an already-spawned evaluator child to completion, writing the request
/// on stdin, waiting under the timeout, and collecting reader output.
///
/// Every error path—stdin write failure, wait-poll failure, and timeout—is
/// funneled through [`cleanup_after_failure`] so the whole process group is
/// terminated, the immediate child is reaped, and both reader threads are
/// joined before returning. This prevents leaked descendants and dangling
/// reader threads regardless of where the failure occurs.
fn run_evaluator_child(
    child: &mut std::process::Child,
    stdin_json: &str,
    timeout: Duration,
    stdout_reader: Option<ReaderHandle>,
    stderr_reader: Option<ReaderHandle>,
) -> Result<String, EngineError> {
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            return Err(cleanup_after_failure(
                child,
                stdout_reader,
                stderr_reader,
                feedback_eval_error("feedback evaluator command stdin unavailable"),
            ));
        }
    };

    if let Err(err) = write_and_close_stdin(stdin, stdin_json) {
        return Err(cleanup_after_failure(
            child,
            stdout_reader,
            stderr_reader,
            feedback_eval_error(format!("write feedback evaluator stdin: {err}")),
        ));
    }

    let status = match wait_for_feedback_evaluator(child, timeout) {
        Ok(status) => status,
        Err(err) => {
            return Err(cleanup_after_failure(
                child,
                stdout_reader,
                stderr_reader,
                err,
            ));
        }
    };

    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;

    if !status.success() {
        return Err(feedback_eval_error(format!(
            "feedback evaluator command exited with status {}: {}",
            status,
            String::from_utf8_lossy(&stderr)
        )));
    }
    String::from_utf8(stdout).map_err(|err| {
        feedback_eval_error(format!("feedback evaluator stdout was not utf-8: {err}"))
    })
}

/// Write the request JSON to the child's stdin and close it so the child sees
/// end-of-input. The pipe is always dropped, even on write failure.
fn write_and_close_stdin(
    mut stdin: std::process::ChildStdin,
    stdin_json: &str,
) -> std::io::Result<()> {
    let result = stdin.write_all(stdin_json.as_bytes());
    drop(stdin);
    result
}

/// Terminate the child's whole process group, reap the immediate child, and
/// join both reader threads, then return the original diagnostic error.
///
/// Terminating the process group closes any stdout/stderr pipe descriptors held
/// by inherited descendants, which lets the reader threads observe end-of-file
/// and exit promptly instead of blocking forever on a pipe kept open by a
/// surviving grandchild. Reader-thread failures during cleanup are intentionally
/// discarded so the caller still receives the primary failure cause.
fn cleanup_after_failure(
    child: &mut std::process::Child,
    stdout_reader: Option<ReaderHandle>,
    stderr_reader: Option<ReaderHandle>,
    error: EngineError,
) -> EngineError {
    terminate_evaluator_process_group(child);
    let _ = join_reader(stdout_reader, "stdout");
    let _ = join_reader(stderr_reader, "stderr");
    error
}

/// Terminate the evaluator's process group with TERM then KILL (Unix) and reap
/// the immediate child so no zombie remains.
fn terminate_evaluator_process_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = run_process_group_kill("TERM", &process_group);
        thread::sleep(Duration::from_millis(250));
        let _ = run_process_group_kill("KILL", &process_group);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn run_process_group_kill(
    signal: &str,
    process_group: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let signal_arg = format!("-{signal}");
    Command::new("/bin/kill")
        .args([signal_arg.as_str(), "--", process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

fn join_reader(reader: Option<ReaderHandle>, stream: &str) -> Result<Vec<u8>, EngineError> {
    match reader {
        Some(handle) => match handle.join() {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(err)) => Err(feedback_eval_error(format!(
                "read feedback evaluator {stream}: {err}"
            ))),
            Err(_) => Err(feedback_eval_error(format!(
                "feedback evaluator {stream} reader thread panicked"
            ))),
        },
        None => Ok(Vec::new()),
    }
}

/// Poll the child until it exits or the timeout elapses. On a poll error or
/// timeout this returns the diagnostic error only; the caller is responsible
/// for process-group termination and reader-thread joins via
/// [`cleanup_after_failure`].
fn wait_for_feedback_evaluator(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, EngineError> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(err) => {
                return Err(feedback_eval_error(format!(
                    "poll feedback evaluator command: {err}"
                )));
            }
        }
        if started.elapsed() >= timeout {
            return Err(feedback_eval_error(format!(
                "feedback evaluator command timed out after {} seconds",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Production adapter that serializes one structured request and invokes a configured argv vector.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
#[derive(Clone, Debug)]
pub struct CommandFeedbackEvaluationAdapter<R> {
    argv: Vec<String>,
    runner: R,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl<R> CommandFeedbackEvaluationAdapter<R> {
    #[must_use]
    pub fn new(argv: Vec<String>, runner: R) -> Self {
        Self { argv, runner }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl<R: FeedbackEvaluatorCommandRunner> FeedbackEvaluationAdapter
    for CommandFeedbackEvaluationAdapter<R>
{
    fn evaluate(&self, request: &FeedbackEvaluationRequest) -> Result<String, EngineError> {
        let stdin_json = serde_json::to_string(request)
            .map_err(|err| feedback_eval_error(format!("serialize evaluator request: {err}")))?;
        self.runner
            .run_feedback_evaluator_command(&self.argv, &stdin_json)
    }
}
