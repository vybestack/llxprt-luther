//! Changed-path discovery for the diff gate.
//!
//! The gate asks whether the run produced qualifying changes. Committing does
//! not undo that, so an uncommitted-only view would report no changes as soon
//! as the work was committed. Both the working tree and the committed range
//! are therefore consulted and merged.
use super::diff_gate_error;
use crate::engine::runner::EngineError;
use std::collections::HashSet;
use std::process::Command;

/// Expose the changed-path computation so its committed-range behavior can be
/// exercised directly against a real repository.
///
/// Gated behind `debug_assertions` so it is not part of the release API
/// surface; integration tests build with it enabled.
#[cfg(debug_assertions)]
pub fn changed_paths_for_test(
    work_dir: &std::path::Path,
    base_ref: Option<&str>,
) -> Result<Vec<String>, EngineError> {
    git_changed_paths(work_dir, base_ref)
}

/// Paths the run has changed, whether or not they are still uncommitted.
pub(super) fn git_changed_paths(
    work_dir: &std::path::Path,
    base_ref: Option<&str>,
) -> Result<Vec<String>, EngineError> {
    let mut paths = git_worktree_changed_paths(work_dir)?;
    let Some(base_ref) = base_ref else {
        return Ok(paths);
    };
    // Order is meaningful to the caller, so membership is tracked separately
    // rather than scanning the accumulating vector for each candidate.
    let mut seen: HashSet<String> = paths.iter().cloned().collect();
    for path in git_committed_changed_paths(work_dir, base_ref)? {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Paths changed in commits the run has made on top of `base_ref`.
///
/// An unresolvable base, which happens when the remote ref is not present in
/// the workspace, is not an error: it means there is no committed range to
/// consult, and the working tree view stands alone.
fn git_committed_changed_paths(
    work_dir: &std::path::Path,
    base_ref: &str,
) -> Result<Vec<String>, EngineError> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("{base_ref}...HEAD")])
        .current_dir(work_dir)
        .output()
        .map_err(|err| diff_gate_error(format!("failed to run git diff: {err}")))?;
    if !output.status.success() {
        // Treated as "no committed range" rather than an error, because the
        // base ref is legitimately absent in some workspaces. It is reported
        // so a genuine git failure is not mistaken for an absent range.
        eprintln!(
            "verify: no committed range for base '{base_ref}': {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_worktree_changed_paths(work_dir: &std::path::Path) -> Result<Vec<String>, EngineError> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| diff_gate_error(format!("failed to run git status: {err}")))?;
    if !output.status.success() {
        return Err(diff_gate_error(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_git_status_path)
        .collect())
}

fn parse_git_status_path(line: &str) -> Option<String> {
    line.get(3..)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.split(" -> ").last().unwrap_or(path).to_string())
}
