//! Tests for the changed-path detection seam used by the llxprt executor.
//!
//! Covers issue #16 acceptance criteria:
//! 1. Changed-path detection is behind a trait/helper with parser tests
//!    (parser tests live in the `change_detection` `#[cfg(test)]` module; these
//!    tests exercise the `GitChangedPathDetector` and injection seam).
//! 2. Workflows can choose tracked-only vs untracked-included detection.
//! 3. Errors from missing git or non-repo workdirs are explicit.

use std::path::Path;
use std::process::Command;

use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::change_detection::{
    ChangeDetectionMode, ChangedPathDetector, GitChangedPathDetector,
};
use luther_workflow::engine::executors::LlxprtExecutorWithDetector;
use luther_workflow::engine::runner::EngineError;
use luther_workflow::engine::transition::StepOutcome;
use serde_json::json;
use tempfile::tempdir;

fn git_init(dir: &Path) {
    let status = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init should run");
    assert!(status.status.success(), "git init failed");
}

#[test]
fn detector_returns_tracked_and_untracked_paths_by_mode() {
    let dir = tempdir().unwrap();
    git_init(dir.path());

    // One tracked-and-modified file (staged) and one untracked file.
    std::fs::write(dir.path().join("tracked.txt"), "v1").unwrap();
    let add = Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(add.status.success());
    std::fs::write(dir.path().join("untracked.txt"), "u").unwrap();

    let detector = GitChangedPathDetector;

    let with_untracked = detector
        .detect_changed_paths(dir.path(), ChangeDetectionMode::IncludeUntracked)
        .expect("include-untracked detection should succeed");
    assert!(with_untracked.contains(&"tracked.txt".to_string()));
    assert!(with_untracked.contains(&"untracked.txt".to_string()));

    let tracked_only = detector
        .detect_changed_paths(dir.path(), ChangeDetectionMode::TrackedOnly)
        .expect("tracked-only detection should succeed");
    assert!(tracked_only.contains(&"tracked.txt".to_string()));
    assert!(
        !tracked_only.contains(&"untracked.txt".to_string()),
        "tracked-only mode must exclude untracked files"
    );
}

#[test]
fn detector_clean_repo_returns_empty_not_error() {
    let dir = tempdir().unwrap();
    git_init(dir.path());

    let paths = GitChangedPathDetector
        .detect_changed_paths(dir.path(), ChangeDetectionMode::IncludeUntracked)
        .expect("clean repo must return Ok");
    assert!(
        paths.is_empty(),
        "clean repo should report no changed paths"
    );
}

#[test]
fn detector_non_repo_dir_is_explicit_error() {
    let dir = tempdir().unwrap();
    // No git init: this is not a repository / has no worktree.
    let err = GitChangedPathDetector
        .detect_changed_paths(dir.path(), ChangeDetectionMode::IncludeUntracked)
        .expect_err("non-repo dir must be an explicit error");

    match err {
        EngineError::StepExecutionError { step_id, message } => {
            assert_eq!(step_id, "llxprt");
            assert!(
                message.contains("not a git repository") || message.contains("128"),
                "message should identify the non-repo condition, got: {message}"
            );
        }
        other => panic!("expected StepExecutionError, got {other:?}"),
    }
}

/// Stub detector returning a fixed result so success/fixable can be driven
/// deterministically without git.
struct StubDetector {
    result: Result<Vec<String>, EngineError>,
}

impl ChangedPathDetector for StubDetector {
    fn detect_changed_paths(
        &self,
        _work_dir: &Path,
        _mode: ChangeDetectionMode,
    ) -> Result<Vec<String>, EngineError> {
        match &self.result {
            Ok(paths) => Ok(paths.clone()),
            Err(_) => Err(EngineError::StepExecutionError {
                step_id: "llxprt".to_string(),
                message: "stub detection failure".to_string(),
            }),
        }
    }
}

fn run_static_stdout_step(
    detector: impl ChangedPathDetector + 'static,
    params: serde_json::Value,
) -> (StepOutcome, StepContext) {
    let dir = tempdir().unwrap();
    let mut ctx = StepContext::new(dir.path().to_path_buf(), "run-test".to_string());
    let executor = LlxprtExecutorWithDetector::new(detector);
    let outcome = executor.execute(&mut ctx, &params).expect("execute ok");
    (outcome, ctx)
}

#[test]
fn injected_detector_drives_success_on_diff() {
    let detector = StubDetector {
        result: Ok(vec!["src/changed.rs".to_string()]),
    };
    let params = json!({
        "static_stdout": "done",
        "success_on_diff": true,
        // Treat the detected change as newly produced (empty initial snapshot).
        "success_on_existing_diff": true,
        "required_changed_paths": ["src/changed.rs"],
    });
    let (outcome, _ctx) = run_static_stdout_step(detector, params);
    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn injected_detector_missing_required_path_is_fixable() {
    let detector = StubDetector {
        result: Ok(vec!["src/other.rs".to_string()]),
    };
    let params = json!({
        "static_stdout": "done",
        "success_on_diff": true,
        "required_changed_paths": ["src/changed.rs"],
    });
    let (outcome, _ctx) = run_static_stdout_step(detector, params);
    assert_eq!(outcome, StepOutcome::Fixable);
}

#[test]
fn injected_detector_error_is_surfaced_not_silent_success() {
    let detector = StubDetector {
        result: Err(EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: "boom".to_string(),
        }),
    };
    let params = json!({
        "static_stdout": "done",
        "success_on_diff": true,
        "required_changed_paths": ["src/changed.rs"],
    });
    let (outcome, ctx) = run_static_stdout_step(detector, params);
    // Detection error must NOT be reported as success.
    assert_eq!(outcome, StepOutcome::Fixable);
    let diagnostic = ctx.get("diagnostic").cloned().unwrap_or_default();
    assert!(
        diagnostic.contains("change detection failed"),
        "diagnostic should record the detection failure, got: {diagnostic:?}"
    );
}
