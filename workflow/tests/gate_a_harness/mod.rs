//! Gate A-R harness: explicit work item -> workflow run -> new draft PR.
//!
//! The distinguishing property of this harness, versus the canary it replaces,
//! is that it can legitimately fail. It supplies no postconditions. It starts
//! the shipping binary and observes what the binary does, and every step that
//! matters -- producing a change, committing, pushing, opening a PR -- is
//! performed by the product rather than by the harness on the product's
//! behalf.
//!
//! See `docs/architecture/product-gates.md` for the gate definitions and the
//! forbidden-substitution list this harness is written against.

pub mod fake_gh;

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the harness observed. Serialized so a run is auditable after the fact
/// rather than reduced to a boolean.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResult {
    pub gate: String,
    pub outcome: GateOutcome,
    /// Last step the run reported entering, if any.
    pub terminal_step: Option<String>,
    /// Distinct steps the run reached, in first-entry order.
    ///
    /// The endpoint alone is too coarse to detect a regression that changes
    /// the path without changing where it stops. This records the trajectory
    /// so a lost step is visible even when the terminal step is unchanged.
    pub steps_reached: Vec<String>,
    /// Absolute path of the process the harness actually started.
    pub executed_binary: PathBuf,
    /// Digest of the workflow config as loaded from disk.
    pub config_digest: String,
    pub config_path: PathBuf,
    pub tool_versions: Vec<(String, String)>,
    /// Every `gh` invocation the run made, in order.
    pub gh_invocations: Vec<String>,
    /// Terminal evidence: the PR ref observed in the bare remote, if any.
    pub pushed_ref: Option<String>,
    pub exit_code: Option<i32>,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GateOutcome {
    /// A new draft PR was produced from a clean start.
    Pass,
    /// The run did not produce one. This is the expected outcome today.
    Fail,
}

/// Inputs the harness varies across its negative controls.
pub struct GateRun {
    pub issue_number: u64,
    pub github: fake_gh::FakeGitHub,
    /// Replaces the implementation step's agent with a script. The default
    /// makes a real edit; a control makes none.
    pub agent_script: String,
}

impl GateRun {
    #[must_use]
    pub fn new(issue_number: u64) -> Self {
        Self {
            issue_number,
            github: fake_gh::FakeGitHub::new(issue_number),
            agent_script: default_agent_script(),
        }
    }
}

/// An agent stand-in that makes a real, verifiable edit.
///
/// Standing in for the LLM keeps the harness deterministic. It does not stand
/// in for anything downstream: the workflow still has to detect the change,
/// stage it, commit it, push it, and open the PR. Those are the steps under
/// test.
#[must_use]
pub fn default_agent_script() -> String {
    agent_script(true)
}

/// Builds the agent stand-in.
///
/// The v1 workflow routes on magic strings the model is told to print, so the
/// stand-in has to answer each step with the token that step expects. It
/// selects the token by matching the prompt it was handed rather than by
/// counting invocations, so the workflow's own routing decides the path.
///
/// `makes_change` controls only whether a real file edit occurs; every other
/// behaviour is identical, which is what makes the no-change control a clean
/// single-variable comparison.
fn agent_script(makes_change: bool) -> String {
    // The implement step requires the diff to touch a path the workflow
    // recognizes as product surface (`required_changed_path_patterns`). An
    // edit outside those paths is correctly rejected, so the stand-in writes
    // where a real change would go.
    let edit = if makes_change {
        "mkdir -p workflow/docs && \
         printf 'harness edit %s\\n' \"$(date +%s)\" >> workflow/docs/harness-change.md"
    } else {
        ":"
    };
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

# The prompt arrives on stdin or as an argument depending on the step.
prompt="$*"
if [ -t 0 ]; then :; else prompt="$prompt$(cat || true)"; fi

# Steps that gate on an artifact name it in the prompt. Writing the file the
# prompt asks for keeps the stand-in honest: it satisfies the step's stated
# contract instead of a path this harness hardcoded.
write_named_artifact() {{
  local target
  target=$(printf '%s' "$prompt" | grep -oE '/[^ ]*/(plan|pr-description)\.md' | tail -1 || true)
  if [ -n "$target" ]; then
    mkdir -p "$(dirname "$target")"
    # plan_gate rejects a plan under 200 non-whitespace characters, so the
    # stand-in writes something that clears the shipping threshold rather than
    # a token file. Satisfying the real contract is the point.
    {{
      printf '# Harness plan for %s\n\n' "$(basename "$target")"
      printf 'This plan is produced by the Gate A-R harness in place of an agent.\n'
      printf 'It exists to satisfy the plan gate the shipping workflow enforces,\n'
      printf 'which requires a substantive artifact rather than a placeholder.\n'
      printf 'Step: append a line to HARNESS_CHANGE.md in the target workspace.\n'
    }} > "$target"
  fi
}}

# Dispatch on the token the step tells the agent to emit. Ordering matters:
# the implement prompt mentions both IMPLEMENTATION_COMPLETE and the plan gate
# banner, so the most specific instruction is matched first.
case "$prompt" in
  *"exactly IMPL_APPROVED"*|*IMPL_NEEDS_WORK*)
    echo 'IMPL_APPROVED'
    ;;
  *"exactly PLAN_APPROVED"*|*PLAN_NEEDS_REVISION*)
    write_named_artifact
    echo 'PLAN_APPROVED'
    ;;
  *REMEDIATION_COMPLETE*)
    {edit}
    echo 'REMEDIATION_COMPLETE'
    ;;
  *IMPLEMENTATION_COMPLETE*)
    {edit}
    echo 'IMPLEMENTATION_COMPLETE'
    ;;
  *)
    # Planning and PR-description steps route on their artifact, not a token.
    write_named_artifact
    echo 'done'
    ;;
esac
"#
    )
}

/// An agent that reports success without changing anything.
///
/// The primary construct-validity control. A harness that passes with this
/// installed is measuring its own scaffolding.
#[must_use]
pub fn no_change_agent_script() -> String {
    agent_script(false)
}

/// Runs Gate A-R end to end and reports what happened.
pub fn run_gate_a(run: &GateRun) -> GateResult {
    let root = tempfile::tempdir().expect("harness scratch dir");
    let bin_dir = root.path().join("bin");
    let workspace = root.path().join("workspace");
    let artifacts = root.path().join("artifacts");
    let remote = root.path().join("remote.git");
    let gh_log = root.path().join("gh-invocations.log");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&artifacts).unwrap();

    seed_remote(&remote, &workspace);
    install_script(&bin_dir.join("gh"), &run.github.script(&gh_log));
    // The agent is resolved by name through PATH (workflow variable
    // `llxprt_binary_path` defaults to "llxprt"), so the stand-in is installed
    // under that name. Same interception the real binary would go through.
    install_script(&bin_dir.join("llxprt"), &run.agent_script);

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_luther-workflow"));
    let config_path = shipping_config_path();
    let config_digest = digest_of(&config_path);

    let mut command = Command::new(&binary);
    command
        .arg("run")
        .arg("--config-dir")
        .arg(config_root())
        .arg("--config")
        .arg(config_root().join("workflow-configs/llxprt-luther.toml"))
        .arg("--workflow-type")
        .arg("llxprt-luther-dogfood-v1")
        .arg("--repo")
        .arg("example/repo")
        .arg("--transport-url")
        .arg(&remote)
        .arg("--issue")
        .arg(run.issue_number.to_string())
        .arg("--work-dir")
        .arg(&workspace)
        .arg("--artifact-dir")
        .arg(&artifacts)
        .arg("--skip-preflight")
        .env("PATH", prepended_path(&bin_dir))
        .env("HOME", root.path())
        .current_dir(root.path());

    let output = command.output().expect("shipping binary must start");

    // Steps that redirect to files keep their diagnostics out of the parent's
    // stderr, so the harness reads them back. Without this a failure inside a
    // redirected step is invisible and easy to misattribute.
    let step_logs = collect_step_logs(&artifacts);
    let stderr = format!("{}{step_logs}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let pushed_ref = observed_pushed_ref(&remote, run.issue_number);

    GateResult {
        gate: "A-R".to_string(),
        // Terminal evidence is the ref in the bare remote plus an observed
        // `pr create`. Neither is supplied by the harness.
        outcome: if pushed_ref.is_some() && gh_log_contains(&gh_log, "pr create") {
            GateOutcome::Pass
        } else {
            GateOutcome::Fail
        },
        // Step transitions are logged to stderr by the engine, so both streams
        // are searched; reading only stdout silently yields None and makes the
        // report look emptier than the run was.
        // The step the run stopped on, which is what the engine names in its
        // terminal line. The last *executed* step is the cleanup handler, so
        // reporting that would understate where the product actually got.
        terminal_step: failed_step(&stderr)
            .or_else(|| last_step(&stderr))
            .or_else(|| last_step(&stdout)),
        steps_reached: steps_reached(&stderr),
        executed_binary: binary,
        config_digest,
        config_path,
        tool_versions: tool_versions(),
        gh_invocations: read_lines(&gh_log),
        pushed_ref,
        exit_code: output.status.code(),
        stderr_tail: tail(&stderr, 4000),
    }
}

/// Creates the bare remote and a workspace clone with an initial commit.
fn seed_remote(remote: &Path, workspace: &Path) {
    git(
        remote.parent().unwrap(),
        &["init", "--bare", "-b", "main", &remote.to_string_lossy()],
    );
    let seed = remote.parent().unwrap().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "seed\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-m", "seed"]);
    git(
        &seed,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);
    // The workspace is deliberately NOT created here. Luther provisions and
    // takes ownership of it (issue #158), and pre-creating it trips the
    // adoption guard. Letting the product own that step is the point.
    let _ = workspace;
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Gate Harness")
        .env("GIT_AUTHOR_EMAIL", "harness@example.invalid")
        .env("GIT_COMMITTER_NAME", "Gate Harness")
        .env("GIT_COMMITTER_EMAIL", "harness@example.invalid")
        .output()
        .expect("git available");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn install_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn prepended_path(dir: &Path) -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", dir.display())
}

/// Reads the branch the run pushed, from the bare remote itself.
fn observed_pushed_ref(remote: &Path, issue_number: u64) -> Option<String> {
    let output = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/issue{issue_number}"),
        ])
        .current_dir(remote)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn gh_log_contains(log: &Path, needle: &str) -> bool {
    read_lines(log).iter().any(|line| line.contains(needle))
}

/// Reads back per-step stdout/stderr artifacts the workflow redirected.
fn collect_step_logs(artifacts: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(artifacts) else {
        return String::new();
    };
    let mut collected = String::new();
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("stdout") || name.contains("stderr"))
        })
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if !body.trim().is_empty() {
                collected.push_str(&format!(
                    "\n--- {} ---\n{}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    tail(&body, 1500)
                ));
            }
        }
    }
    collected
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Distinct steps the engine entered, in first-entry order.
fn steps_reached(text: &str) -> Vec<String> {
    const MARKER: &str = "Executing step: ";
    let mut seen = Vec::new();
    for line in text.lines() {
        if let Some(at) = line.find(MARKER) {
            let step = line[at + MARKER.len()..].trim().to_string();
            if !seen.contains(&step) {
                seen.push(step);
            }
        }
    }
    seen
}

/// The step named in the engine's terminal line, e.g.
/// `Workflow abandoned at step 'implement'`.
fn failed_step(text: &str) -> Option<String> {
    text.lines()
        .filter(|line| {
            line.contains("Workflow failed at step") || line.contains("Workflow abandoned at step")
        })
        .filter_map(|line| {
            let start = line.find('\'')? + 1;
            let rest = &line[start..];
            let end = rest.find('\'')?;
            Some(rest[..end].to_string())
        })
        .next_back()
}

/// Last step the engine reported entering.
///
/// Engine lines carry an `[engine] ` prefix, so the marker is located within
/// the line rather than anchored at its start.
fn last_step(text: &str) -> Option<String> {
    const MARKER: &str = "Executing step: ";
    text.lines()
        .filter_map(|line| line.find(MARKER).map(|at| &line[at + MARKER.len()..]))
        .map(|step| step.trim().to_string())
        .next_back()
}

/// Directory holding the shipping `workflows/` tree, resolved the way the
/// product resolves it rather than by copying files into the scratch dir.
fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config")
}

fn shipping_config_path() -> PathBuf {
    config_root().join("workflows/llxprt-luther-dogfood-v1.toml")
}

fn digest_of(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}

fn tool_versions() -> Vec<(String, String)> {
    ["git", "bash"]
        .into_iter()
        .map(|tool| {
            let version = Command::new(tool)
                .arg("--version")
                .output()
                .ok()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_else(|| "unavailable".to_string());
            (tool.to_string(), version)
        })
        .collect()
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    text[text.len() - limit..].to_string()
}
