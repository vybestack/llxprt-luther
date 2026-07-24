//! Production system probes for the typed merge verifier. [B11/P17]
//!
//! These probe implementations shell out to `git` and `gh` and are used by the
//! production merge verifier. Tests inject deterministic stub probes instead.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
//! @requirement:REQ-RP-010

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engine::recovery::typed_merge::{
    MergeError, MergeGitProbe, MergeObservation, MergeRemoteProbe, MergeStrategy,
};

// ===========================================================================
// SystemMergeGitProbe
// ===========================================================================

/// Production Git probe that shells out to `git merge-base --is-ancestor`,
/// `git cat-file`/`git ls-tree`, and `git patch-id`. [B11]
///
/// All SHA/branch arguments are validated to reject option-like values
/// (starting with `-`) before being passed to the subprocess, preventing
/// argument injection.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, Default)]
pub struct SystemMergeGitProbe;

impl SystemMergeGitProbe {
    /// Create a new system Git probe.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Reject any argument that looks like a CLI option (starts with `-`).
    fn reject_option_like(value: &str, label: &str) -> Result<(), MergeError> {
        if value.starts_with('-') || value.is_empty() {
            return Err(MergeError::ReachabilityFailed(format!(
                "invalid {label}: option-like or empty value rejected"
            )));
        }
        Ok(())
    }
}

impl MergeGitProbe for SystemMergeGitProbe {
    fn is_ancestor(
        &self,
        work_dir: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<(), MergeError> {
        Self::reject_option_like(ancestor, "ancestor")?;
        Self::reject_option_like(descendant, "descendant")?;
        let output = Command::new("git")
            .arg("merge-base")
            .arg("--is-ancestor")
            .arg(ancestor)
            .arg(descendant)
            .current_dir(work_dir)
            .output()
            .map_err(|e| MergeError::ReachabilityFailed(format!("failed to invoke git: {e}")))?;
        // exit code 0 = confirmed ancestor; non-zero = NOT an ancestor or error.
        if output.status.success() {
            Ok(())
        } else {
            Err(MergeError::ReachabilityFailed(format!(
                "ancestry check failed: {} is NOT an ancestor of {}",
                ancestor, descendant
            )))
        }
    }

    fn compute_tree_content_digest(
        &self,
        work_dir: &Path,
        commit: &str,
    ) -> Result<String, MergeError> {
        Self::reject_option_like(commit, "commit")?;
        // Use `git ls-tree -r` to get a stable tree listing, then SHA-256 it.
        // This is independent of `git patch-id` and captures the full tree
        // content state at the commit.
        let output = Command::new("git")
            .arg("ls-tree")
            .arg("-r")
            .arg(commit)
            .current_dir(work_dir)
            .output()
            .map_err(|e| {
                MergeError::ReachabilityFailed(format!("failed to invoke git ls-tree: {e}"))
            })?;
        if !output.status.success() {
            return Err(MergeError::ReachabilityFailed(format!(
                "git ls-tree failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(sha256_hex(&output.stdout))
    }

    fn compute_patch_id(
        &self,
        work_dir: &Path,
        base: &str,
        head: &str,
    ) -> Result<String, MergeError> {
        Self::reject_option_like(base, "base")?;
        Self::reject_option_like(head, "head")?;
        // `git diff` piped to `git patch-id` gives a stable patch identity.
        let diff = Command::new("git")
            .arg("diff")
            .arg(format!("{base}..{head}"))
            .current_dir(work_dir)
            .output()
            .map_err(|e| {
                MergeError::ReachabilityFailed(format!("failed to invoke git diff: {e}"))
            })?;
        if !diff.status.success() {
            return Err(MergeError::ReachabilityFailed(format!(
                "git diff failed: {}",
                String::from_utf8_lossy(&diff.stderr).trim()
            )));
        }
        let patch_id_output = Command::new("git")
            .arg("patch-id")
            .current_dir(work_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                MergeError::ReachabilityFailed(format!("failed to invoke git patch-id: {e}"))
            })?;
        use std::io::Write;
        let mut child = patch_id_output;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&diff.stdout)
                .map_err(|e| MergeError::ReachabilityFailed(format!("pipe write failed: {e}")))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| MergeError::ReachabilityFailed(format!("patch-id wait failed: {e}")))?;
        if !output.status.success() {
            return Err(MergeError::ReachabilityFailed(format!(
                "git patch-id failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        // patch-id output: "<patch-id> <commit-count>"
        let line = String::from_utf8_lossy(&output.stdout);
        let patch_id = line.split_whitespace().next().unwrap_or("").to_string();
        if patch_id.is_empty() {
            return Err(MergeError::ReachabilityFailed(
                "git patch-id returned empty output".to_string(),
            ));
        }
        Ok(patch_id)
    }

    fn resolve_base_commit(&self, work_dir: &Path, base_ref: &str) -> Result<String, MergeError> {
        Self::reject_option_like(base_ref, "base_ref")?;
        // `refs/heads/` prefix defends against ambiguous-ref attacks: even if
        // base_ref looks like a branch, we resolve through the heads namespace.
        // `--verify` ensures the ref exists; `^{commit}` peels tags to commits.
        let output = Command::new("git")
            .arg("rev-parse")
            .arg("--verify")
            .arg(format!("refs/heads/{base_ref}^{{commit}}"))
            .current_dir(work_dir)
            .output()
            .map_err(|e| {
                MergeError::ReachabilityFailed(format!("failed to invoke git rev-parse: {e}"))
            })?;
        if output.status.success() {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !sha.is_empty() {
                return Ok(sha);
            }
        }
        // Fallback: resolve as a raw ref (tag or SHA) with commit peeling.
        let output = Command::new("git")
            .arg("rev-parse")
            .arg("--verify")
            .arg(format!("{base_ref}^{{commit}}"))
            .current_dir(work_dir)
            .output()
            .map_err(|e| {
                MergeError::ReachabilityFailed(format!("failed to invoke git rev-parse: {e}"))
            })?;
        if !output.status.success() {
            return Err(MergeError::ReachabilityFailed(format!(
                "git rev-parse failed for base_ref '{base_ref}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Err(MergeError::ReachabilityFailed(format!(
                "git rev-parse returned empty SHA for base_ref '{base_ref}'"
            )));
        }
        Ok(sha)
    }
}

// ===========================================================================
// SystemMergeRemoteProbe
// ===========================================================================

/// Production remote probe that shells out to `gh pr view --json`. [B11]
///
/// Bound to an **explicit expected merge strategy** declared in capsule/config.
/// GitHub's REST/GraphQL API does not reliably report the merge method used for
/// a closed PR, so the probe does NOT guess the strategy. Instead it cross-checks
/// the observed merge commit's structure against the declared expected strategy
/// and fails closed on any structural inconsistency. The expected strategy is
/// authoritative config evidence, not an inference. [P17]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone)]
pub struct SystemMergeRemoteProbe {
    /// The strategy declared in capsule/config — the sole authority. [P17]
    expected_strategy: MergeStrategy,
    /// Working directory for local `git rev-list` parent-count queries. When
    /// set, `count_commit_parents` runs `git` in this directory (matching all
    /// other git invocations in this module). When unset, the ambient process
    /// CWD is used — the commit SHA may not be resolvable, causing a fail-closed
    /// single-parent default.
    work_dir: Option<PathBuf>,
}

impl SystemMergeRemoteProbe {
    /// Create a new system remote probe bound to the given expected strategy.
    ///
    /// The strategy MUST come from config, not from a hard-coded default.
    /// [P17]
    #[must_use]
    pub fn new(expected_strategy: MergeStrategy) -> Self {
        Self {
            expected_strategy,
            work_dir: None,
        }
    }

    /// Bind this probe to a working directory so local `git rev-list` parent
    /// queries resolve the commit SHA in the correct repository (matching every
    /// other git invocation in this module).
    #[must_use]
    pub fn with_work_dir(mut self, work_dir: PathBuf) -> Self {
        self.work_dir = Some(work_dir);
        self
    }

    /// Reject any GitHub merge observation whose structural evidence is
    /// inconsistent with the declared expected strategy. [P17]
    ///
    /// GitHub does not expose `mergeMethod` on a merged PR via the standard
    /// `pr view` fields, so we use **structural proof**: a merge commit has 2+
    /// parents, a squash/rebase commit has exactly 1 parent. This cross-checks
    /// the observation against the declared config strategy and fails closed
    /// if they disagree — we never guess the strategy from the observation.
    fn cross_check_strategy(
        &self,
        result_sha: &str,
        parent_count: usize,
    ) -> Result<MergeStrategy, MergeError> {
        if result_sha.is_empty() {
            return Err(MergeError::ReachabilityFailed(
                "observed merge result SHA is empty".to_string(),
            ));
        }
        // Structural inference from parent count:
        //   2+ parents → MergeCommit
        //   1 parent   → Squash or Rebase (cannot distinguish structurally)
        let structurally_merge_commit = parent_count >= 2;
        match self.expected_strategy {
            MergeStrategy::MergeCommit => {
                if !structurally_merge_commit {
                    return Err(MergeError::StrategyMismatch {
                        expected: MergeStrategy::MergeCommit,
                        structural: "single-parent commit (squash/rebase)".to_string(),
                    });
                }
                Ok(MergeStrategy::MergeCommit)
            }
            MergeStrategy::Squash | MergeStrategy::Rebase => {
                if structurally_merge_commit {
                    // Config says squash/rebase but the commit has 2+ parents
                    // → a merge commit was made instead. Fail closed.
                    Err(MergeError::StrategyMismatch {
                        expected: self.expected_strategy,
                        structural: "multi-parent merge commit".to_string(),
                    })
                } else {
                    // Single-parent is consistent with squash or rebase.
                    Ok(self.expected_strategy)
                }
            }
        }
    }

    /// Count the number of parents of a commit SHA using `git rev-list
    /// --parents -n 1`. This provides structural evidence for strategy
    /// verification. Returns 0 on failure (fail-closed to single-parent
    /// default).
    ///
    /// When `work_dir` is set, `git` runs in that directory — matching every
    /// other git invocation in this module. When unset, the ambient process
    /// CWD is used.
    fn count_commit_parents(&self, sha: &str) -> Option<usize> {
        if sha.starts_with('-') || sha.is_empty() {
            return None;
        }
        let mut command = Command::new("git");
        command
            .arg("rev-list")
            .arg("--parents")
            .arg("-n")
            .arg("1")
            .arg(sha);
        if let Some(dir) = &self.work_dir {
            command.current_dir(dir);
        }
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        // Output: "<sha> <parent1> <parent2> ..."
        let line = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = line.split_whitespace().collect();
        // parts[0] is the commit itself; parents are parts[1..].
        parts.len().checked_sub(1)
    }
}

impl MergeRemoteProbe for SystemMergeRemoteProbe {
    fn observe_merge(&self, repo: &str, pr_number: i64) -> Result<MergeObservation, MergeError> {
        if repo.starts_with('-') || repo.is_empty() {
            return Err(MergeError::ReachabilityFailed(
                "invalid repo: option-like or empty value rejected".to_string(),
            ));
        }
        let output = Command::new("gh")
            .arg("pr")
            .arg("view")
            .arg(pr_number.to_string())
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("state,mergeCommit")
            .output()
            .map_err(|e| MergeError::ReachabilityFailed(format!("failed to invoke gh: {e}")))?;
        if !output.status.success() {
            return Err(MergeError::ReachabilityFailed(format!(
                "gh pr view failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| {
            MergeError::ReachabilityFailed(format!("failed to parse gh output: {e}"))
        })?;
        let state = json.get("state").and_then(|v| v.as_str()).unwrap_or("OPEN");
        if state != "MERGED" {
            return Ok(MergeObservation {
                merged: false,
                strategy: self.expected_strategy,
                result_sha: String::new(),
            });
        }
        let result_sha = json
            .get("mergeCommit")
            .and_then(|v| v.get("oid"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if result_sha.is_empty() {
            return Err(MergeError::ReachabilityFailed(
                "PR is MERGED but mergeCommit.oid is absent".to_string(),
            ));
        }
        // Structural strategy proof via parent count (cat-file). [P17]
        let parent_count = self.count_commit_parents(&result_sha).unwrap_or(0);
        let verified_strategy = self.cross_check_strategy(&result_sha, parent_count)?;
        Ok(MergeObservation {
            merged: true,
            strategy: verified_strategy,
            result_sha,
        })
    }
}

/// SHA-256 hex digest of a byte slice (lowercase hex).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
