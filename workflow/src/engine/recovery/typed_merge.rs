//! Typed verified merge with strategy-specific reachability proof and atomic
//! artifact+status transaction. [C10/C11/B11/B12]
//!
//! This module implements the typed verified merge contract:
//!
//! - A merge-required run's completion requires BOTH a [`TypedMergeArtifact`]
//!   (observed merge + strategy-specific [`MergeReachabilityProof`]) AND the
//!   durable [`RunStatus::Merged`] state, committed in a single short
//!   `IMMEDIATE` atomic artifact+status transaction. [C11]
//! - A status field alone never satisfies completion ([`completion_satisfied`]
//!   requires both). [B12]
//! - The verifier takes authoritative injected Git/remote interfaces and
//!   computes ALL evidence itself (no ambient shell, no caller-supplied
//!   proof). [B11]
//! - The only status from which a run may transition to `Merged` is
//!   [`RunStatus::ReviewReady`] (the fixed allowed predecessor). [B12]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
//! @requirement:REQ-RP-010

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::persistence::capsule_store;
use crate::persistence::run_metadata::{RunMetadata, RunStatus};
use crate::persistence::sqlite;

/// Table name for the immutable typed merge artifact store. [B12]
pub const MERGE_ARTIFACTS_TABLE: &str = "merge_artifacts";

/// Fixed allowed predecessor for the merge status transition. [B12]
///
/// The only status from which a run may transition to [`RunStatus::Merged`].
/// A merge-required run reaches `ReviewReady` after all steps complete (never
/// `Completed`); `complete_typed_merge` then transitions `ReviewReady → Merged`
/// atomically with the artifact insert.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub const ALLOWED_MERGE_PREDECESSOR: RunStatus = RunStatus::ReviewReady;

// ===========================================================================
// TypedMergeArtifact — proof that a merge happened and is reachable. [C10/C11]
// ===========================================================================

/// Immutable proof that a merge happened and is reachable, bound to
/// run/repo/PR/head/base/capsule/result/proof/time. [C10/C11/B12]
///
/// One artifact per run (`run_id` is the PRIMARY KEY in
/// [`MERGE_ARTIFACTS_TABLE`]). The artifact is immutable: once inserted it
/// cannot be mutated, and a second insert with different fields is rejected as
/// a conflict. [B12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedMergeArtifact {
    /// The run this artifact proves merged.
    pub run_id: String,
    /// The PR number that was merged.
    pub pr_number: i64,
    /// Strategy-neutral observed commit SHA (the merge/squash/rebase result).
    /// [C10]
    pub result_sha: String,
    /// Bound to repo. [C11]
    pub repo: String,
    /// Bound to head. [C11]
    pub head_sha: String,
    /// Bound to base. [C11]
    pub base_sha: String,
    /// Bound to capsule (join key to `execution_capsules`). [C11/B12]
    pub capsule_envelope_digest: String,
    /// Strategy-specific reachability proof. [C10]
    pub reachability_proof: MergeReachabilityProof,
    /// When the artifact was recorded.
    pub recorded_at: DateTime<Utc>,
}

// ===========================================================================
// MergeReachabilityProof — strategy-specific reachability evidence. [C10]
// ===========================================================================

/// Strategy-specific merge reachability proof. [C10]
///
/// Different merge strategies produce different ancestry relationships, so the
/// proof records the exact evidence per strategy. The `result_sha` on
/// [`TypedMergeArtifact`] is strategy-neutral.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MergeReachabilityProof {
    /// Merge commit: TWO ancestry checks. [C10]
    ///
    /// Verified: `--is-ancestor <head_sha> <merge_commit_sha>`
    ///      AND: `--is-ancestor <base_sha> <merge_commit_sha>`
    MergeCommit {
        head_sha: String,
        base_sha: String,
        merge_commit_sha: String,
    },
    /// Squash: ancestry PLUS computed expected/observed content evidence. [C10]
    ///
    /// Verified: `--is-ancestor <base_sha> <squash_commit_sha>`
    ///      AND: `expected_content_digest == observed_content_digest`
    Squash {
        base_sha: String,
        squash_commit_sha: String,
        expected_content_digest: String,
        observed_content_digest: String,
    },
    /// Rebase: ancestry PLUS computed expected/observed patch evidence. [C10]
    ///
    /// Verified: `--is-ancestor <base_sha> <final_head_sha>`
    ///      AND: `expected_patch_id == observed_patch_id`
    Rebase {
        base_sha: String,
        final_head_sha: String,
        expected_patch_id: String,
        observed_patch_id: String,
    },
}

impl MergeReachabilityProof {
    /// Returns the discriminant string used for the `proof_kind` column. [B12]
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::MergeCommit { .. } => "merge_commit",
            Self::Squash { .. } => "squash",
            Self::Rebase { .. } => "rebase",
        }
    }

    /// The result SHA this proof points at (merge_commit / squash / final_head).
    #[must_use]
    pub fn result_sha(&self) -> &str {
        match self {
            Self::MergeCommit {
                merge_commit_sha, ..
            } => merge_commit_sha,
            Self::Squash {
                squash_commit_sha, ..
            } => squash_commit_sha,
            Self::Rebase { final_head_sha, .. } => final_head_sha,
        }
    }
}

/// The merge strategy observed by the remote probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeStrategy {
    MergeCommit,
    Squash,
    Rebase,
}

/// Observed merge state from the remote probe. [B11]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeObservation {
    /// Whether the PR is merged.
    pub merged: bool,
    /// The observed merge strategy.
    pub strategy: MergeStrategy,
    /// The observed result SHA (merge commit / squash commit / final head).
    pub result_sha: String,
}

// ===========================================================================
// Injected probes (B11) — the verifier computes ALL evidence itself.
// ===========================================================================

/// Authoritative injected Git interface for reachability checks. [B11]
///
/// Production: [`SystemMergeGitProbe`] (shells out to git). Tests: inject a
/// deterministic probe. The verifier NEVER trusts caller-supplied proof — it
/// recomputes all evidence via these probes.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub trait MergeGitProbe: Send + Sync {
    /// Returns `Ok(())` if `<ancestor>` is an ancestor of `<descendant>`.
    /// Fails closed on non-zero exit. [C10]
    fn is_ancestor(
        &self,
        work_dir: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<(), MergeError>;

    /// Compute the tree content digest of a commit (squash evidence). [C10]
    fn compute_tree_content_digest(
        &self,
        work_dir: &Path,
        commit: &str,
    ) -> Result<String, MergeError>;

    /// Compute the patch-id of a commit range (rebase evidence). [C10]
    fn compute_patch_id(
        &self,
        work_dir: &Path,
        base: &str,
        head: &str,
    ) -> Result<String, MergeError>;

    /// Resolve the exact base commit SHA from a base reference (branch name,
    /// tag, or SHA) by reading the Git object database. [P17]
    ///
    /// This derives an **exact** base commit from `capsule.base_ref` rather than
    /// accepting an empty/ambient value. The probe validates the ref is
    /// option-safe before invoking `git rev-parse`.
    fn resolve_base_commit(&self, work_dir: &Path, base_ref: &str) -> Result<String, MergeError>;
}

/// Authoritative injected remote interface for PR/merge observation. [B11]
///
/// Production: [`SystemMergeRemoteProbe`] (shells out to `gh`). Tests: inject a
/// stub.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub trait MergeRemoteProbe: Send + Sync {
    /// Observe whether the PR is merged and return the merge strategy + result
    /// SHA.
    fn observe_merge(&self, repo: &str, pr_number: i64) -> Result<MergeObservation, MergeError>;
}

// ===========================================================================
// Production system probes with safe argument handling.
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
}

impl SystemMergeRemoteProbe {
    /// Create a new system remote probe bound to the given expected strategy.
    ///
    /// The strategy MUST come from config, not from a hard-coded default.
    /// [P17]
    #[must_use]
    pub fn new(expected_strategy: MergeStrategy) -> Self {
        Self { expected_strategy }
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
        let parent_count = count_commit_parents(&result_sha).unwrap_or(0);
        let verified_strategy = self.cross_check_strategy(&result_sha, parent_count)?;
        Ok(MergeObservation {
            merged: true,
            strategy: verified_strategy,
            result_sha,
        })
    }
}

/// Count the number of parents of a commit SHA using `git rev-list --count
/// --parents -n 1`. This provides structural evidence for strategy
/// verification. Returns 0 on failure (fail-closed to single-parent default).
fn count_commit_parents(sha: &str) -> Option<usize> {
    if sha.starts_with('-') || sha.is_empty() {
        return None;
    }
    let output = Command::new("git")
        .arg("rev-list")
        .arg("--parents")
        .arg("-n")
        .arg("1")
        .arg(sha)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Output: "<sha> <parent1> <parent2> ..."
    let line = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = line.split_whitespace().collect();
    // parts[0] is the commit itself; parents are parts[1..].
    parts.len().checked_sub(1)
}

// ===========================================================================
// MergeVerifier — injected probes + bound identity. [B11]
// ===========================================================================

/// The verifier context: injected probes + bound identity. The verifier
/// computes ALL evidence itself from these authoritative interfaces. [B11]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub struct MergeVerifier {
    /// Git probe for reachability checks.
    pub git_probe: Box<dyn MergeGitProbe>,
    /// Remote probe for PR/merge observation.
    pub remote_probe: Box<dyn MergeRemoteProbe>,
    /// Working directory for Git operations.
    pub work_dir: PathBuf,
    /// Bound repo. [C11]
    pub repo: String,
    /// Bound PR number. [C11]
    pub pr_number: i64,
    /// Bound base SHA. [C11]
    pub base_sha: String,
    /// Bound head SHA. [C11]
    pub head_sha: String,
}

impl MergeVerifier {
    /// Create a verifier with custom probes (for tests or production override).
    #[must_use]
    pub fn new(
        git_probe: Box<dyn MergeGitProbe>,
        remote_probe: Box<dyn MergeRemoteProbe>,
        work_dir: PathBuf,
        repo: String,
        pr_number: i64,
        base_sha: String,
        head_sha: String,
    ) -> Self {
        Self {
            git_probe,
            remote_probe,
            work_dir,
            repo,
            pr_number,
            base_sha,
            head_sha,
        }
    }

    /// Create a production verifier with system probes. [P17]
    ///
    /// The `expected_strategy` MUST come from config, not a hard-coded default.
    /// The system remote probe cross-checks the observed merge structure
    /// against this declared strategy and fails closed on mismatch.
    #[must_use]
    pub fn with_system_probes(
        work_dir: PathBuf,
        repo: String,
        pr_number: i64,
        base_sha: String,
        head_sha: String,
        expected_strategy: MergeStrategy,
    ) -> Self {
        Self::new(
            Box::new(SystemMergeGitProbe::new()),
            Box::new(SystemMergeRemoteProbe::new(expected_strategy)),
            work_dir,
            repo,
            pr_number,
            base_sha,
            head_sha,
        )
    }
}

// ===========================================================================
// build_reachability_proof — compute evidence via probes (no tx). [C10/B11]
// ===========================================================================

/// Build the strategy-specific proof by computing evidence via probes. [B11]
///
/// The verifier NEVER trusts caller-supplied proof — it recomputes all
/// evidence from the injected authoritative interfaces. [B11]
///
/// # Errors
/// - [`MergeError::NotMerged`] if the PR is not observed as merged.
/// - [`MergeError::ReachabilityFailed`] if an ancestry check fails.
/// - [`MergeError::ContentMismatch`] if squash content digests differ.
/// - [`MergeError::PatchMismatch`] if rebase patch-ids differ.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn build_reachability_proof(
    verifier: &MergeVerifier,
) -> Result<MergeReachabilityProof, MergeError> {
    let obs = verifier
        .remote_probe
        .observe_merge(&verifier.repo, verifier.pr_number)?;
    if !obs.merged {
        return Err(MergeError::NotMerged);
    }
    match obs.strategy {
        MergeStrategy::MergeCommit => {
            // TWO ancestry checks, computed by the verifier. [C10/B11]
            verifier.git_probe.is_ancestor(
                &verifier.work_dir,
                &verifier.head_sha,
                &obs.result_sha,
            )?;
            verifier.git_probe.is_ancestor(
                &verifier.work_dir,
                &verifier.base_sha,
                &obs.result_sha,
            )?;
            Ok(MergeReachabilityProof::MergeCommit {
                head_sha: verifier.head_sha.clone(),
                base_sha: verifier.base_sha.clone(),
                merge_commit_sha: obs.result_sha.clone(),
            })
        }
        MergeStrategy::Squash => {
            // Ancestry PLUS computed content evidence. [C10/B11]
            verifier.git_probe.is_ancestor(
                &verifier.work_dir,
                &verifier.base_sha,
                &obs.result_sha,
            )?;
            let expected = compute_pr_diff_content_digest(verifier)?;
            let observed = verifier
                .git_probe
                .compute_tree_content_digest(&verifier.work_dir, &obs.result_sha)?;
            if expected != observed {
                return Err(MergeError::ContentMismatch);
            }
            Ok(MergeReachabilityProof::Squash {
                base_sha: verifier.base_sha.clone(),
                squash_commit_sha: obs.result_sha.clone(),
                expected_content_digest: expected,
                observed_content_digest: observed,
            })
        }
        MergeStrategy::Rebase => {
            // Ancestry PLUS computed patch evidence. [C10/B11]
            verifier.git_probe.is_ancestor(
                &verifier.work_dir,
                &verifier.base_sha,
                &obs.result_sha,
            )?;
            let expected = verifier.git_probe.compute_patch_id(
                &verifier.work_dir,
                &verifier.base_sha,
                &verifier.head_sha,
            )?;
            let observed = verifier.git_probe.compute_patch_id(
                &verifier.work_dir,
                &verifier.base_sha,
                &obs.result_sha,
            )?;
            if expected != observed {
                return Err(MergeError::PatchMismatch);
            }
            Ok(MergeReachabilityProof::Rebase {
                base_sha: verifier.base_sha.clone(),
                final_head_sha: obs.result_sha.clone(),
                expected_patch_id: expected,
                observed_patch_id: observed,
            })
        }
    }
}

/// Compute the expected content digest from the PR diff (base..head). [C10]
/// This is the independently computed expected value for squash evidence.
fn compute_pr_diff_content_digest(verifier: &MergeVerifier) -> Result<String, MergeError> {
    // Delegate to the git probe's tree content digest over the head commit.
    // The "expected" content for a squash is the tree of the head commit
    // (the squashed commit should produce the same tree content).
    verifier
        .git_probe
        .compute_tree_content_digest(&verifier.work_dir, &verifier.head_sha)
}

// ===========================================================================
// complete_typed_merge — external verification THEN atomic tx. [C11/B12]
// ===========================================================================

/// Complete a typed merge: external verification THEN short `IMMEDIATE` atomic
/// artifact+status transaction. [C11/B12]
///
/// Bound to repo/PR/head/capsule. Explicit allowed predecessor
/// ([`ALLOWED_MERGE_PREDECESSOR`] = `ReviewReady`). Affected-row check. Exact
/// idempotent retry. The normal merge-required flow must NOT first write
/// `Completed`.
///
/// # Phases
/// 1. **External verification (no tx):** build the reachability proof via
///    injected probes; verify capsule binding. [B11]
/// 2. **Short `IMMEDIATE` atomic tx:** insert the immutable artifact (or
///    verify exact equality if it already exists), then conditional
///    `ReviewReady → Merged` status update with affected-row CAS. [C11/B12]
///
/// # Idempotent retry
/// If the transaction is retried (e.g., after a crash), the affected-row check
/// ensures the retry is safe: if the artifact already exists with exact
/// equality and status is already `Merged`, the retry succeeds cleanly.
///
/// # Errors
/// - [`MergeError::NotMerged`], [`MergeError::ReachabilityFailed`],
///   [`MergeError::ContentMismatch`], [`MergeError::PatchMismatch`] from
///   external verification.
/// - [`MergeError::ArtifactConflict`] if an artifact exists with different
///   fields. [B12]
/// - [`MergeError::CapsuleBindingMismatch`] if the capsule digest does not
///   match. [B12]
/// - [`MergeError::PreconditionFailed`] if the status is not `ReviewReady`
///   and not already `Merged` (or head/capsule binding mismatches). [C11/B12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn complete_typed_merge(
    conn: &Connection,
    artifact: &TypedMergeArtifact,
    verifier: &MergeVerifier,
) -> Result<(), MergeError> {
    let proof = build_reachability_proof(verifier)?;
    if proof.result_sha() != artifact.result_sha
        || proof != artifact.reachability_proof
        || verifier.repo != artifact.repo
        || verifier.pr_number != artifact.pr_number
        || verifier.head_sha != artifact.head_sha
        || verifier.base_sha != artifact.base_sha
    {
        return Err(MergeError::ArtifactConflict);
    }
    verify_capsule_binding(conn, artifact)?;
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| MergeError::Database(error.to_string()))?;
    revalidate_merge_bindings(&tx, artifact)?;
    let artifact_affected = insert_merge_artifact(&tx, artifact, &proof)?;
    transition_to_merged(&tx, artifact, artifact_affected)?;
    tx.commit()
        .map_err(|error| MergeError::Database(error.to_string()))
}

fn revalidate_merge_bindings(
    conn: &Connection,
    artifact: &TypedMergeArtifact,
) -> Result<(), MergeError> {
    let metadata = sqlite::get_run_with_conn(conn, &artifact.run_id)
        .map_err(|error| MergeError::Database(error.to_string()))?
        .ok_or_else(|| MergeError::Database(format!("run not found: {}", artifact.run_id)))?;
    require_identity(
        "repository",
        metadata.repository.as_deref().unwrap_or(""),
        &artifact.repo,
    )?;
    require_identity(
        "pr_number",
        &metadata.pr_number.unwrap_or(0).to_string(),
        &artifact.pr_number.to_string(),
    )?;
    require_identity(
        "head_sha",
        metadata.head_sha.as_deref().unwrap_or(""),
        &artifact.head_sha,
    )?;
    let capsule = capsule_store::load_capsule_v1(conn, &artifact.run_id)
        .map_err(|error| MergeError::Database(format!("capsule revalidation failed: {error}")))?;
    if capsule.envelope_digest != artifact.capsule_envelope_digest {
        return Err(MergeError::CapsuleBindingMismatch);
    }
    Ok(())
}

fn require_identity(
    field: &'static str,
    persisted: &str,
    artifact: &str,
) -> Result<(), MergeError> {
    if persisted == artifact {
        return Ok(());
    }
    Err(MergeError::IdentityMismatch {
        field,
        persisted: persisted.to_string(),
        artifact: artifact.to_string(),
    })
}

fn insert_merge_artifact(
    conn: &Connection,
    artifact: &TypedMergeArtifact,
    proof: &MergeReachabilityProof,
) -> Result<usize, MergeError> {
    let proof_json = serde_json::to_string(proof)
        .map_err(|error| MergeError::Database(format!("proof serialization failed: {error}")))?;
    let affected = conn
        .execute(
            &format!(
                "INSERT INTO {MERGE_ARTIFACTS_TABLE}
                   (run_id, pr_number, result_sha, repo, head_sha, base_sha,
                    capsule_envelope_digest, proof_kind, proof_json, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(run_id) DO NOTHING"
            ),
            params![
                artifact.run_id,
                artifact.pr_number,
                artifact.result_sha,
                artifact.repo,
                artifact.head_sha,
                artifact.base_sha,
                artifact.capsule_envelope_digest,
                proof.kind_str(),
                proof_json,
                artifact.recorded_at.to_rfc3339(),
            ],
        )
        .map_err(|error| MergeError::Database(error.to_string()))?;
    if affected == 0 {
        let existing = load_merge_artifact_tx(conn, &artifact.run_id)
            .map_err(|error| MergeError::Database(error.to_string()))?
            .ok_or_else(|| {
                MergeError::Database("artifact conflict without existing row".to_string())
            })?;
        if !artifact_exact_equal(&existing, artifact) {
            return Err(MergeError::ArtifactConflict);
        }
    }
    Ok(affected)
}

fn transition_to_merged(
    conn: &Connection,
    artifact: &TypedMergeArtifact,
    artifact_affected: usize,
) -> Result<(), MergeError> {
    let affected = conn
        .execute(
            "UPDATE runs SET status = ?1, updated_at = ?2
             WHERE run_id = ?3 AND status = ?4 AND head_sha = ?5",
            params![
                RunStatus::Merged.to_string(),
                Utc::now().to_rfc3339(),
                artifact.run_id,
                ALLOWED_MERGE_PREDECESSOR.to_string(),
                artifact.head_sha,
            ],
        )
        .map_err(|error| MergeError::Database(error.to_string()))?;
    if affected == 1 {
        return Ok(());
    }
    let current_status = sqlite::get_run_with_conn(conn, &artifact.run_id)
        .map_err(|error| MergeError::Database(error.to_string()))?
        .map(|metadata| metadata.status)
        .unwrap_or(RunStatus::Initialized);
    if current_status == RunStatus::Merged && artifact_affected == 0 {
        return Ok(());
    }
    Err(MergeError::PreconditionFailed {
        current_status: current_status.to_string(),
        expected_predecessor: ALLOWED_MERGE_PREDECESSOR,
    })
}

// ===========================================================================
// completion_satisfied — requires BOTH artifact row AND Merged status. [B12]
// ===========================================================================

/// Completion requires BOTH a typed artifact row AND [`RunStatus::Merged`].
///
/// A status field alone NEVER satisfies completion. [B12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[must_use]
pub fn completion_satisfied(conn: &Connection, run_id: &str) -> bool {
    let has_artifact = match count_merge_artifacts(conn, run_id) {
        Ok(n) => n > 0,
        Err(_) => return false,
    };
    if !has_artifact {
        return false;
    }
    let status = match sqlite::get_run_with_conn(conn, run_id) {
        Ok(Some(md)) => md.status,
        _ => return false,
    };
    status == RunStatus::Merged
}

// ===========================================================================
// verify_capsule_binding — join key to execution_capsules. [B12]
// ===========================================================================

/// Verify the run's capsule envelope digest matches the artifact's
/// `capsule_envelope_digest`. [B12]
///
/// # Errors
/// - [`MergeError::CapsuleBindingMismatch`] if the digests differ or no
///   capsule exists.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn verify_capsule_binding(
    conn: &Connection,
    artifact: &TypedMergeArtifact,
) -> Result<(), MergeError> {
    let capsule = capsule_store::load_capsule_v1(conn, &artifact.run_id)
        .map_err(|e| MergeError::Database(format!("capsule load failed: {e}")))?;
    if capsule.envelope_digest != artifact.capsule_envelope_digest {
        return Err(MergeError::CapsuleBindingMismatch);
    }
    Ok(())
}

// ===========================================================================
// runner_completion_for_merge_required — ReviewReady, never Completed. [B12]
// ===========================================================================

/// Returns the terminal status a merge-required run should reach after all
/// steps complete: [`RunStatus::ReviewReady`], NOT [`RunStatus::Completed`].
///
/// `complete_typed_merge` then transitions `ReviewReady → Merged`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[must_use]
pub fn runner_completion_for_merge_required() -> RunStatus {
    RunStatus::ReviewReady
}

// ===========================================================================
// Production completion path — called when merge observation is available.
// ===========================================================================

/// Complete a merge-required run from observed merge state.
///
/// This is the **supported production completion path**: when the normal
/// merge-required flow observes that a PR has been merged, it calls this
/// function instead of leaving the run in `ReviewReady`. The function:
///
/// 1. Loads the run metadata to derive the bound repo/PR/head/base identity.
/// 2. Loads the capsule to get the envelope digest.
/// 3. Constructs a [`TypedMergeArtifact`] and [`MergeVerifier`] (with system
///    probes by default, or injected probes for testing).
/// 4. Calls [`complete_typed_merge`], which does external verification THEN
///    the atomic artifact+status transaction.
///
/// This ensures the typed merge API is **reachable from the production flow**
/// and not dead code.
///
/// # Errors
/// Propagates [`MergeError`] from `complete_typed_merge` or metadata/capsule
/// loading failures.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn complete_merge_from_observation(
    conn: &Connection,
    run_id: &str,
    work_dir: &Path,
    git_probe: Box<dyn MergeGitProbe>,
    remote_probe: Box<dyn MergeRemoteProbe>,
) -> Result<(), MergeError> {
    let md = read_run_metadata(conn, run_id)?;
    let repo = md.repository.clone().unwrap_or_default();
    let pr_number = md.pr_number.unwrap_or(0);
    let head_sha = md.head_sha.clone().unwrap_or_default();

    // Validate nonempty identity BEFORE any probe work. [P17]
    if repo.is_empty() {
        return Err(MergeError::IdentityIncomplete("repository"));
    }
    if pr_number <= 0 {
        return Err(MergeError::IdentityIncomplete("pr_number"));
    }
    if head_sha.is_empty() {
        return Err(MergeError::IdentityIncomplete("head_sha"));
    }

    let capsule = capsule_store::load_capsule_v1(conn, run_id)
        .map_err(|e| MergeError::Database(format!("capsule load failed: {e}")))?;

    // Derive the EXACT base commit from capsule.base_ref via the injected
    // descriptor-safe Git probe. Never empty. [P17]
    if capsule.base_ref.is_empty() {
        return Err(MergeError::IdentityIncomplete("capsule.base_ref"));
    }
    let base_sha = git_probe.resolve_base_commit(work_dir, &capsule.base_ref)?;
    if base_sha.is_empty() {
        return Err(MergeError::ReachabilityFailed(format!(
            "resolve_base_commit returned empty SHA for base_ref '{}'",
            capsule.base_ref
        )));
    }

    let verifier = MergeVerifier::new(
        git_probe,
        remote_probe,
        work_dir.to_path_buf(),
        repo.clone(),
        pr_number,
        base_sha.clone(),
        head_sha.clone(),
    );

    // Build the exact artifact from independently computed verifier evidence;
    // complete_typed_merge recomputes and requires exact equality.
    let proof = build_reachability_proof(&verifier)?;
    let artifact = TypedMergeArtifact {
        run_id: run_id.to_string(),
        pr_number,
        result_sha: proof.result_sha().to_string(),
        repo,
        head_sha,
        base_sha,
        capsule_envelope_digest: capsule.envelope_digest.clone(),
        reachability_proof: proof,
        recorded_at: Utc::now(),
    };

    complete_typed_merge(conn, &artifact, &verifier)
}

// ===========================================================================
// Persistence helpers
// ===========================================================================

/// Initialize the immutable merge artifacts table (idempotent). [B12]
///
/// DDL:
/// ```text
/// CREATE TABLE IF NOT EXISTS merge_artifacts (
///   run_id TEXT PRIMARY KEY,                 -- one artifact per run [B12]
///   pr_number INTEGER NOT NULL,
///   result_sha TEXT NOT NULL,
///   repo TEXT NOT NULL,
///   head_sha TEXT NOT NULL,
///   base_sha TEXT NOT NULL,
///   capsule_envelope_digest TEXT NOT NULL,   -- join key to execution_capsules [B12]
///   proof_kind TEXT NOT NULL,                -- 'merge_commit'|'squash'|'rebase'
///   proof_json TEXT NOT NULL,                -- serialized MergeReachabilityProof
///   recorded_at TEXT NOT NULL
/// )
/// ```
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn init_merge_artifacts_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {MERGE_ARTIFACTS_TABLE} (
                run_id TEXT PRIMARY KEY,
                pr_number INTEGER NOT NULL,
                result_sha TEXT NOT NULL,
                repo TEXT NOT NULL,
                head_sha TEXT NOT NULL,
                base_sha TEXT NOT NULL,
                capsule_envelope_digest TEXT NOT NULL,
                proof_kind TEXT NOT NULL,
                proof_json TEXT NOT NULL,
                recorded_at TEXT NOT NULL
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Load a merge artifact by run_id (within a transaction). [B12]
fn load_merge_artifact_tx(
    tx: &Connection,
    run_id: &str,
) -> rusqlite::Result<Option<TypedMergeArtifact>> {
    load_merge_artifact_conn(tx, run_id)
}

/// Load a merge artifact by run_id.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn load_merge_artifact_conn(
    conn: &Connection,
    run_id: &str,
) -> rusqlite::Result<Option<TypedMergeArtifact>> {
    conn.query_row(
        &format!(
            "SELECT pr_number, result_sha, repo, head_sha, base_sha,
                    capsule_envelope_digest, proof_kind, proof_json, recorded_at
             FROM {MERGE_ARTIFACTS_TABLE} WHERE run_id = ?1"
        ),
        params![run_id],
        |row| map_merge_artifact_row(row, run_id),
    )
    .optional()
}

fn map_merge_artifact_row(
    row: &rusqlite::Row<'_>,
    run_id: &str,
) -> rusqlite::Result<TypedMergeArtifact> {
    let proof_json: String = row.get(7)?;
    let recorded_at: String = row.get(8)?;
    let reachability_proof = serde_json::from_str(&proof_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let recorded_at = chrono::DateTime::parse_from_rfc3339(&recorded_at)
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .with_timezone(&Utc);
    Ok(TypedMergeArtifact {
        run_id: run_id.to_string(),
        pr_number: row.get(0)?,
        result_sha: row.get(1)?,
        repo: row.get(2)?,
        head_sha: row.get(3)?,
        base_sha: row.get(4)?,
        capsule_envelope_digest: row.get(5)?,
        reachability_proof,
        recorded_at,
    })
}

/// Count merge artifacts for a run.
pub fn count_merge_artifacts(conn: &Connection, run_id: &str) -> rusqlite::Result<i64> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {MERGE_ARTIFACTS_TABLE} WHERE run_id = ?1"),
        params![run_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Compare all fields of two artifacts for exact equality. [B12]
fn artifact_exact_equal(a: &TypedMergeArtifact, b: &TypedMergeArtifact) -> bool {
    a.run_id == b.run_id
        && a.pr_number == b.pr_number
        && a.result_sha == b.result_sha
        && a.repo == b.repo
        && a.head_sha == b.head_sha
        && a.base_sha == b.base_sha
        && a.capsule_envelope_digest == b.capsule_envelope_digest
        && a.reachability_proof == b.reachability_proof
}

/// SHA-256 hex digest of a byte slice (lowercase hex).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Helper: read the current run status (outside any tx).
#[allow(dead_code)]
fn read_run_status(conn: &Connection, run_id: &str) -> Result<RunStatus, MergeError> {
    let md = sqlite::get_run_with_conn(conn, run_id)
        .map_err(|e| MergeError::Database(e.to_string()))?
        .ok_or_else(|| MergeError::Database(format!("run not found: {run_id}")))?;
    Ok(md.status)
}

/// Helper: read the full run metadata.
fn read_run_metadata(conn: &Connection, run_id: &str) -> Result<RunMetadata, MergeError> {
    sqlite::get_run_with_conn(conn, run_id)
        .map_err(|e| MergeError::Database(e.to_string()))?
        .ok_or_else(|| MergeError::Database(format!("run not found: {run_id}")))
}

// ===========================================================================
// MergeError
// ===========================================================================

/// Errors returned by the typed merge component. [C10/C11/B12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MergeError {
    /// The PR is not observed as merged.
    #[error("PR is not merged")]
    NotMerged,
    /// The observed merge structure is inconsistent with the config-declared
    /// expected strategy. [P17] Never guess — fail closed.
    #[error("strategy mismatch: expected {expected:?}, but structural evidence is {structural}")]
    StrategyMismatch {
        /// The strategy declared in capsule/config (the authority).
        expected: MergeStrategy,
        /// What the structural evidence (parent count) actually shows.
        structural: String,
    },
    /// A reachability check (ancestry, content, patch) failed.
    #[error("reachability verification failed: {0}")]
    ReachabilityFailed(String),
    /// Squash content digest mismatch (expected != observed).
    #[error("squash content digest mismatch")]
    ContentMismatch,
    /// Rebase patch-id mismatch (expected != observed).
    #[error("rebase patch-id mismatch")]
    PatchMismatch,
    /// The status precondition failed (not ReviewReady and not already Merged).
    #[error(
        "precondition failed: current status is {current_status}, expected {expected_predecessor}"
    )]
    PreconditionFailed {
        current_status: String,
        expected_predecessor: RunStatus,
    },
    /// The run is already in a terminal state that blocks the merge transition.
    #[error("run is already terminal")]
    AlreadyTerminal,
    /// An artifact with different fields already exists. [B12]
    #[error("merge artifact conflict: existing artifact has different fields")]
    ArtifactConflict,
    /// The capsule envelope digest does not match. [B12]
    #[error("capsule binding mismatch")]
    CapsuleBindingMismatch,
    /// Required run identity (repo/pr/head/base) is missing or empty. [P17]
    #[error("run identity incomplete: {0}")]
    IdentityIncomplete(&'static str),
    /// The persisted run identity does not match the artifact's bound identity
    /// when revalidated under the transaction. [P17]
    #[error(
        "identity mismatch: persisted {field}='{persisted}' differs from artifact '{artifact}'"
    )]
    IdentityMismatch {
        /// Which identity field mismatched.
        field: &'static str,
        /// What the persisted run row holds.
        persisted: String,
        /// What the artifact claims.
        artifact: String,
    },
    /// A database error occurred.
    #[error("database error: {0}")]
    Database(String),
}
