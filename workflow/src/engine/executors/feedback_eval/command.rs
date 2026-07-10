//! Process-execution seam for the feedback evaluator command.

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
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                feedback_eval_error(format!("spawn feedback evaluator command: {err}"))
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| feedback_eval_error("feedback evaluator command stdin unavailable"))?;

        // Drain stdout and stderr on dedicated reader threads BEFORE writing
        // stdin and waiting for the child. Reading only after the process
        // exits can deadlock if either pipe fills while the child is still
        // running: the child blocks on a full pipe, and we block waiting for
        // it to exit. This mirrors the concurrent-draining pattern used by the
        // llxprt executor.
        let stdout_reader = child.stdout.take().map(|mut pipe| {
            thread::spawn(move || {
                let mut buffer = Vec::new();
                pipe.read_to_end(&mut buffer).map(|_| buffer)
            })
        });
        let stderr_reader = child.stderr.take().map(|mut pipe| {
            thread::spawn(move || {
                let mut buffer = Vec::new();
                pipe.read_to_end(&mut buffer).map(|_| buffer)
            })
        });

        stdin
            .write_all(stdin_json.as_bytes())
            .map_err(|err| feedback_eval_error(format!("write feedback evaluator stdin: {err}")))?;
        drop(stdin);

        let status = wait_for_feedback_evaluator(&mut child, self.timeout())?;

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
}

type ReaderHandle = thread::JoinHandle<std::io::Result<Vec<u8>>>;

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

fn wait_for_feedback_evaluator(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, EngineError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| feedback_eval_error(format!("poll feedback evaluator command: {err}")))?
        {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
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
