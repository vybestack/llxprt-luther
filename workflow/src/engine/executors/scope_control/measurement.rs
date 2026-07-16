//! Deterministic patch measurement against a charter's frozen merge base.
//!
//! This module collects Git diff data using NUL-terminated (`-z`) output with
//! rename inference disabled, covering all tracked staged/unstaged/HEAD changes
//! plus explicitly enumerated untracked files. It then computes the budget
//! metrics (changed files, added lines with documented binary semantics, new
//! source modules, dependencies added, added public APIs) deterministically.
//!
//! # Git collection design
//!
//! All Git commands are invoked with explicit argument vectors — never
//! shell-string construction — so filenames containing spaces, newlines, or
//! shell metacharacters are handled safely. The `-z` (NUL-terminated) output
//! format ensures that paths containing newlines are parsed correctly, which
//! line-oriented parsing cannot do.
//!
//! `git diff HEAD --numstat --no-renames -z` covers both staged and unstaged
//! tracked changes relative to HEAD. Untracked files are collected separately
//! via `git ls-files --others --exclude-standard -z`.
//!
//! # Binary file semantics
//!
//! Binary files produce `-\t-\t<path>` in numstat output. They are counted as
//! changed files but do not contribute to `added_lines` (the count is
//! indeterminate in numstat). This is documented explicitly in the measurement
//! result via [`FileChange::is_binary`].

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::engine::executors::scope_control::model::CanonicalTaskCharter;
use crate::workflow::schema::ScopeMeasurementConfig;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced during Git data collection or measurement computation.
#[derive(Debug)]
pub enum MeasurementError {
    /// A Git command failed to spawn or returned a non-zero exit status.
    Git { command: String, message: String },
    /// A Git SHA (HEAD) could not be resolved.
    HeadResolution(String),
    /// Numstat output could not be parsed.
    Parse(String),
}

impl std::fmt::Display for MeasurementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git { command, message } => {
                write!(f, "git command '{command}' failed: {message}")
            }
            Self::HeadResolution(msg) => write!(f, "failed to resolve HEAD: {msg}"),
            Self::Parse(msg) => write!(f, "failed to parse git output: {msg}"),
        }
    }
}

impl std::error::Error for MeasurementError {}

// ---------------------------------------------------------------------------
// Raw Git data
// ---------------------------------------------------------------------------

/// The status letter for a changed file, as reported by `git diff
/// --name-status`. For untracked files, the synthetic letter `'?'` is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ChangeStatus {
    /// Newly added file.
    Added,
    /// Modified existing file.
    Modified,
    /// Deleted file.
    Deleted,
    /// Untracked file (not in the index or HEAD).
    Untracked,
}

impl ChangeStatus {
    /// Parse a `git diff --name-status` status letter.
    ///
    /// With `--no-renames`, status letters are `A`, `M`, `D` (and `T` for type
    /// changes). `T` is treated as `Modified` since it represents a content
    /// change that doesn't add or remove a file.
    fn from_letter(letter: &str) -> Result<Self, MeasurementError> {
        match letter {
            "A" => Ok(Self::Added),
            "M" => Ok(Self::Modified),
            "D" => Ok(Self::Deleted),
            "T" => Ok(Self::Modified),
            other => Err(MeasurementError::Parse(format!(
                "unknown status letter '{other}'"
            ))),
        }
    }

    /// Whether this status represents a file being introduced (Added or
    /// Untracked).
    fn is_new_file(self) -> bool {
        matches!(self, Self::Added | Self::Untracked)
    }
}

/// A single file change with numstat data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Repository-relative path (forward-slash, as reported by Git).
    pub path: String,
    /// Status letter classification.
    pub status: ChangeStatus,
    /// Lines added, or `None` if the file is binary.
    pub added_lines: Option<u32>,
    /// Lines deleted, or `None` if the file is binary.
    pub deleted_lines: Option<u32>,
    /// Whether the file is binary (numstat reports `-` for added/deleted).
    pub is_binary: bool,
}

/// Raw Git data collected from the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPatchData {
    /// The current HEAD sha.
    pub head_sha: String,
    /// Number of commits between merge base and HEAD.
    pub divergence: u32,
    /// All tracked file changes (committed + staged + unstaged) relative to
    /// the frozen merge base, with numstat data.
    pub tracked_changes: Vec<FileChange>,
    /// Untracked files explicitly enumerated via `git ls-files --others`.
    pub untracked_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Git data collection
// ---------------------------------------------------------------------------

/// Trait for collecting Git patch data. Production uses real Git; tests inject
/// a deterministic collector or use real temporary Git repositories.
pub trait GitPatchCollector: Send + Sync {
    /// Collect Git patch data from `work_dir` relative to the charter's frozen
    /// merge base.
    fn collect(
        &self,
        work_dir: &Path,
        merge_base: &str,
        config: &ScopeMeasurementConfig,
    ) -> Result<GitPatchData, MeasurementError>;
}

/// Production Git patch collector that shells out to `git`.
pub struct SystemGitPatchCollector;

impl GitPatchCollector for SystemGitPatchCollector {
    fn collect(
        &self,
        work_dir: &Path,
        merge_base: &str,
        config: &ScopeMeasurementConfig,
    ) -> Result<GitPatchData, MeasurementError> {
        let head_sha = resolve_head_sha(work_dir)?;
        let divergence = resolve_divergence(work_dir, merge_base, &head_sha)?;

        // Collect the full frozen merge-base-to-current patch. This covers
        // committed changes after the merge base, staged changes, and unstaged
        // tracked changes — all in a single deterministic diff invocation.
        let tracked_changes = collect_tracked_changes(work_dir, merge_base, &head_sha, config)?;

        let untracked_files = if config.enumerate_untracked {
            collect_untracked_files(work_dir)?
        } else {
            Vec::new()
        };

        Ok(GitPatchData {
            head_sha,
            divergence,
            tracked_changes,
            untracked_files,
        })
    }
}

/// Resolve the current HEAD SHA via `git rev-parse HEAD`.
fn resolve_head_sha(work_dir: &Path) -> Result<String, MeasurementError> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: "rev-parse".into(),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !output.status.success() {
        return Err(MeasurementError::HeadResolution(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(MeasurementError::HeadResolution(
            "git rev-parse returned empty output".into(),
        ));
    }
    Ok(sha)
}

/// Resolve the number of commits from `merge_base` to `HEAD` via
/// `git rev-list --count <merge_base>..HEAD`.
fn resolve_divergence(
    work_dir: &Path,
    merge_base: &str,
    head_sha: &str,
) -> Result<u32, MeasurementError> {
    if merge_base == head_sha {
        return Ok(0);
    }
    let output = Command::new("git")
        .args(["rev-list", "--count", &format!("{merge_base}..{head_sha}")])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: "rev-list --count".into(),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !output.status.success() {
        return Err(MeasurementError::Git {
            command: "rev-list --count".into(),
            message: format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    let count_str = String::from_utf8(output.stdout)
        .map_err(|err| MeasurementError::Parse(format!("non-UTF-8 rev-list output: {err}")))?
        .trim()
        .to_string();
    let count: u64 = count_str.parse().map_err(|err| {
        MeasurementError::Parse(format!("invalid rev-list count '{count_str}': {err}"))
    })?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Collect tracked changes (committed + staged + unstaged) relative to the
/// frozen merge base.
///
/// Uses `git diff <merge_base> --numstat --no-renames -z` which captures the
/// full frozen patch: committed changes after the merge base, staged changes
/// in the index, and unstaged worktree changes — all in one invocation.
///
/// `--no-renames` is mandatory for deterministic output: rename inference is
/// heuristic and non-deterministic across Git versions and similarity
/// thresholds. With renames disabled, a renamed file appears as a delete of
/// the old path plus an add of the new path, which is stable and predictable.
fn collect_tracked_changes(
    work_dir: &Path,
    merge_base: &str,
    head_sha: &str,
    _config: &ScopeMeasurementConfig,
) -> Result<Vec<FileChange>, MeasurementError> {
    // When merge base equals HEAD, there are no committed tracked changes.
    // Staged/unstaged changes are still captured by the diff. We pass the
    // merge-base as the diff spec which correctly includes the worktree.
    let diff_spec = if merge_base == head_sha {
        "HEAD".to_string()
    } else {
        merge_base.to_string()
    };

    let numstat_output = Command::new("git")
        .args(["diff", &diff_spec, "--numstat", "--no-renames", "-z"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: format!("diff {diff_spec} --numstat"),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !numstat_output.status.success() {
        return Err(MeasurementError::Git {
            command: format!("diff {diff_spec} --numstat"),
            message: format!(
                "exit {}: {}",
                numstat_output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&numstat_output.stderr).trim()
            ),
        });
    }

    let name_status_output = Command::new("git")
        .args(["diff", &diff_spec, "--name-status", "--no-renames", "-z"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: format!("diff {diff_spec} --name-status"),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !name_status_output.status.success() {
        return Err(MeasurementError::Git {
            command: format!("diff {diff_spec} --name-status"),
            message: format!(
                "exit {}: {}",
                name_status_output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&name_status_output.stderr).trim()
            ),
        });
    }

    let statuses = parse_name_status_z(&name_status_output.stdout)?;
    let numstats = parse_numstat_z(&numstat_output.stdout)?;

    merge_status_and_numstat(&statuses, &numstats)
}

/// Collect untracked files via `git ls-files --others --exclude-standard -z`.
fn collect_untracked_files(work_dir: &Path) -> Result<Vec<String>, MeasurementError> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: "ls-files --others".into(),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !output.status.success() {
        return Err(MeasurementError::Git {
            command: "ls-files --others".into(),
            message: format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    Ok(parse_z_paths(&output.stdout))
}

// ---------------------------------------------------------------------------
// NUL-safe parsing
// ---------------------------------------------------------------------------

/// Parse NUL-terminated (`-z`) output into a list of byte segments.
fn split_z(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return Vec::new();
    }
    // The final segment may or may not be NUL-terminated. If the data ends
    // with NUL, the last split would be empty; we strip a trailing empty
    // segment to avoid phantom entries.
    data.split(|&b| b == 0)
        .filter(|seg| !seg.is_empty())
        .collect()
}

/// Parse NUL-terminated paths into owned strings.
fn parse_z_paths(data: &[u8]) -> Vec<String> {
    split_z(data)
        .into_iter()
        .filter_map(|seg| String::from_utf8(seg.to_vec()).ok())
        .collect()
}

/// Parse `git diff --name-status -z` output into `(status_letter, path)`
/// pairs.
///
/// The format is: `STATUS\0PATH\0STATUS\0PATH\0...` where STATUS is a
/// single-letter code (A/M/D/T, since `--no-renames` prevents R/C codes).
fn parse_name_status_z(data: &[u8]) -> Result<Vec<(String, String)>, MeasurementError> {
    let segments = split_z(data);
    if !segments.len().is_multiple_of(2) {
        return Err(MeasurementError::Parse(format!(
            "name-status -z output has odd number of NUL segments ({}); expected status+path pairs",
            segments.len()
        )));
    }

    let mut result = Vec::with_capacity(segments.len() / 2);
    for chunk in segments.chunks_exact(2) {
        let status = String::from_utf8(chunk[0].to_vec()).map_err(|err| {
            MeasurementError::Parse(format!("non-UTF-8 status letter in name-status: {err}"))
        })?;
        let path = String::from_utf8(chunk[1].to_vec()).map_err(|err| {
            MeasurementError::Parse(format!("non-UTF-8 path in name-status: {err}"))
        })?;
        result.push((status, path));
    }
    Ok(result)
}

/// Parse `git diff --numstat -z` output into `(added, deleted, path)` tuples.
///
/// The format is: `ADDED\tDELETED\tPATH\0ADDED\tDELETED\tPATH\0...` where
/// ADDED and DELETED are either decimal numbers or `-` for binary files.
fn parse_numstat_z(data: &[u8]) -> Result<Vec<NumstatEntry>, MeasurementError> {
    let segments = split_z(data);
    let mut result = Vec::with_capacity(segments.len());

    for seg in segments {
        let entry_str = String::from_utf8(seg.to_vec())
            .map_err(|err| MeasurementError::Parse(format!("non-UTF-8 numstat entry: {err}")))?;
        let entry = parse_single_numstat(&entry_str)?;
        result.push(entry);
    }
    Ok(result)
}

/// A parsed numstat entry for a single file.
struct NumstatEntry {
    added: NumstatCount,
    deleted: NumstatCount,
    path: String,
}

/// The count value in a numstat entry: either a number or `-` for binary.
#[derive(Debug, Clone, Copy)]
enum NumstatCount {
    Lines(u32),
    Binary,
}

/// Parse a single numstat line: `ADDED\tDELETED\tPATH`.
fn parse_single_numstat(line: &str) -> Result<NumstatEntry, MeasurementError> {
    // The numstat format is: added\tdenleted\tpath
    // With -z, each entry is NUL-terminated (no trailing newline).
    // We need to split on the first two tabs to get added, deleted, and path.
    let mut parts = line.splitn(3, '\t');
    let added_str = parts.next().ok_or_else(|| {
        MeasurementError::Parse(format!("numstat line missing added field: '{line}'"))
    })?;
    let deleted_str = parts.next().ok_or_else(|| {
        MeasurementError::Parse(format!("numstat line missing deleted field: '{line}'"))
    })?;
    let path = parts.next().ok_or_else(|| {
        MeasurementError::Parse(format!("numstat line missing path field: '{line}'"))
    })?;

    let added = parse_numstat_count(added_str)?;
    let deleted = parse_numstat_count(deleted_str)?;

    Ok(NumstatEntry {
        added,
        deleted,
        path: path.to_string(),
    })
}

/// Parse a numstat count field: either a decimal number or `-` for binary.
fn parse_numstat_count(value: &str) -> Result<NumstatCount, MeasurementError> {
    if value == "-" {
        return Ok(NumstatCount::Binary);
    }
    let n = value.parse::<u32>().map_err(|err| {
        MeasurementError::Parse(format!("invalid numstat count '{value}': {err}"))
    })?;
    Ok(NumstatCount::Lines(n))
}

/// Merge name-status and numstat data into a unified list of file changes.
///
/// Both commands report the same set of paths in the same order, but name-status
/// provides the status letter while numstat provides line counts. We join them
/// by index since Git guarantees the same ordering for the same diff range.
fn merge_status_and_numstat(
    statuses: &[(String, String)],
    numstats: &[NumstatEntry],
) -> Result<Vec<FileChange>, MeasurementError> {
    if statuses.len() != numstats.len() {
        return Err(MeasurementError::Parse(format!(
            "name-status has {} entries but numstat has {}; Git should report the same paths in the same order",
            statuses.len(),
            numstats.len()
        )));
    }

    let mut changes = Vec::with_capacity(statuses.len());
    for (i, (status_letter, status_path)) in statuses.iter().enumerate() {
        let numstat = &numstats[i];
        // Sanity: paths should match between name-status and numstat.
        if &numstat.path != status_path {
            return Err(MeasurementError::Parse(format!(
                "path mismatch at index {i}: name-status='{status_path}' vs numstat='{}'",
                numstat.path
            )));
        }

        let status = ChangeStatus::from_letter(status_letter)?;
        let is_binary = matches!(numstat.added, NumstatCount::Binary)
            || matches!(numstat.deleted, NumstatCount::Binary);
        let (added_lines, deleted_lines) = if is_binary {
            (None, None)
        } else {
            let added = match numstat.added {
                NumstatCount::Lines(n) => n,
                NumstatCount::Binary => 0,
            };
            let deleted = match numstat.deleted {
                NumstatCount::Lines(n) => n,
                NumstatCount::Binary => 0,
            };
            (Some(added), Some(deleted))
        };

        changes.push(FileChange {
            path: status_path.clone(),
            status,
            added_lines,
            deleted_lines,
            is_binary,
        });
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

// ---------------------------------------------------------------------------
// Measurement computation
// ---------------------------------------------------------------------------

/// The computed patch measurement against a charter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchMeasurement {
    /// The charter's frozen merge base.
    pub merge_base: String,
    /// The current HEAD sha at measurement time.
    pub head_sha: String,
    /// Number of commits between merge base and HEAD (divergence).
    pub divergence: u32,
    /// Total changed files (tracked + untracked).
    pub files_changed: u32,
    /// Total added lines across all non-binary files.
    pub added_lines: u32,
    /// Number of binary files in the patch.
    pub binary_files: u32,
    /// New source modules (added/untracked files matching source extensions).
    pub new_modules: u32,
    /// Dependencies added to configured manifests.
    pub dependencies_added: u32,
    /// Public API additions matching configured regexes.
    pub public_apis_added: u32,
    /// Content-bound identity for the exact measured worktree snapshot.
    #[serde(default)]
    pub content_digest: String,
    /// All changed file paths (sorted, deduplicated).
    pub changed_paths: Vec<String>,
    /// Changed subsystems (IDs from the charter whose paths overlap).
    pub changed_subsystems: Vec<String>,
    /// Per-file detail.
    pub file_details: Vec<FileChange>,
}

/// Compute a patch measurement from Git data against a charter.
///
/// # Arguments
/// * `data` - Raw Git data collected from the worktree.
/// * `charter` - The canonical task charter providing the merge base, budget
///   ceilings, subsystems, and measurement config.
/// * `measurement_config` - Measurement policy (source extensions, API
///   regexes, rename/untracked flags).
/// * `work_dir` - The worktree root for reading file contents.
/// * `dependency_diffs` - Pre-collected dependency manifest diffs (from
///   [`collect_dependency_diffs`]).
///
/// # Errors
/// Returns [`MeasurementError`] if public API diff lines cannot be read from
/// the worktree.
#[allow(clippy::too_many_arguments)]
pub fn compute_measurement(
    data: &GitPatchData,
    charter: &CanonicalTaskCharter,
    measurement_config: &ScopeMeasurementConfig,
    work_dir: &Path,
    dependency_diffs: &[(String, Vec<String>)],
) -> Result<PatchMeasurement, MeasurementError> {
    // Merge tracked changes and untracked files into a unified list.
    let mut all_changes: Vec<FileChange> = data.tracked_changes.clone();
    for untracked in &data.untracked_files {
        all_changes.push(FileChange {
            path: untracked.clone(),
            status: ChangeStatus::Untracked,
            added_lines: count_lines_in_file(work_dir, untracked),
            deleted_lines: Some(0),
            is_binary: is_file_binary(work_dir, untracked),
        });
    }

    all_changes.sort_by(|a, b| a.path.cmp(&b.path));
    all_changes.dedup_by(|a, b| a.path == b.path);

    let files_changed = u32::try_from(all_changes.len()).unwrap_or(u32::MAX);
    let added_lines = all_changes
        .iter()
        .filter_map(|c| c.added_lines)
        .fold(0u32, |acc, n| acc.saturating_add(n));
    let binary_files =
        u32::try_from(all_changes.iter().filter(|c| c.is_binary).count()).unwrap_or(u32::MAX);

    let new_modules = count_new_modules(&all_changes, measurement_config);
    let dependencies_added = count_dependencies_added(dependency_diffs);
    let public_apis_added = count_public_apis(
        &all_changes,
        measurement_config,
        work_dir,
        &charter.merge_base,
    )?;

    let mut changed_paths: Vec<String> = all_changes.iter().map(|c| c.path.clone()).collect();
    changed_paths.sort();
    changed_paths.dedup();
    let content_digest = compute_content_digest(work_dir, &all_changes)?;

    let changed_subsystems = compute_changed_subsystems(&changed_paths, charter);

    Ok(PatchMeasurement {
        merge_base: charter.merge_base.clone(),
        head_sha: data.head_sha.clone(),
        divergence: data.divergence,
        files_changed,
        added_lines,
        binary_files,
        new_modules,
        dependencies_added,
        public_apis_added,
        content_digest,
        changed_paths,
        changed_subsystems,
        file_details: all_changes,
    })
}
fn compute_content_digest(
    work_dir: &Path,
    changes: &[FileChange],
) -> Result<String, MeasurementError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for change in changes {
        hasher.update(change.path.as_bytes());
        hasher.update([0]);
        hasher.update(format!("{:?}", change.status).as_bytes());
        hasher.update([0]);
        let path = work_dir.join(&change.path);
        match std::fs::read(&path) {
            Ok(bytes) => hasher.update(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => hasher.update(b"<deleted>"),
            Err(err) => {
                return Err(MeasurementError::Git {
                    command: "read measured file".into(),
                    message: format!("{}: {err}", path.display()),
                });
            }
        }
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Count lines in a file, returning `None` for binary files.
///
/// Errors during file read or UTF-8 decoding propagate to the caller so
/// measurement fails closed rather than silently reporting a zero/incorrect
/// count.
fn count_lines_in_file(work_dir: &Path, repo_relative: &str) -> Option<u32> {
    let path = work_dir.join(repo_relative);
    let content = std::fs::read(&path).ok()?;
    if content.contains(&0) {
        return None;
    }
    let text = String::from_utf8(content).ok()?;
    Some(u32::try_from(text.lines().count()).unwrap_or(u32::MAX))
}

/// Check whether a file is binary by scanning for NUL bytes.
fn is_file_binary(work_dir: &Path, repo_relative: &str) -> bool {
    let path = work_dir.join(repo_relative);
    match std::fs::read(&path) {
        Ok(content) => content.contains(&0),
        Err(_) => false,
    }
}

/// Count new source modules: added or untracked files whose extensions match
/// configured source extensions.
fn count_new_modules(changes: &[FileChange], config: &ScopeMeasurementConfig) -> u32 {
    let count = changes
        .iter()
        .filter(|c| c.status.is_new_file())
        .filter(|c| has_source_extension(&c.path, &config.source_extensions))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Check whether a path has one of the configured source extensions.
fn has_source_extension(path: &str, extensions: &[String]) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    extensions.iter().any(|configured| configured == ext)
}

/// Count dependencies added across all configured manifests.
fn count_dependencies_added(manifest_diffs: &[(String, Vec<String>)]) -> u32 {
    manifest_diffs
        .iter()
        .map(|(_, added)| added.len())
        .fold(0u32, |acc, len| {
            acc.saturating_add(u32::try_from(len).unwrap_or(u32::MAX))
        })
}

/// Count public API additions by scanning only added diff lines for lines
/// matching configured regexes.
///
/// For tracked files, the added diff lines are extracted via
/// `git diff <merge_base> -- <path>` (only `+`-prefixed lines). For untracked
/// files, the entire file content is scanned since every line is an addition.
/// Binary files are skipped entirely.
fn count_public_apis(
    changes: &[FileChange],
    config: &ScopeMeasurementConfig,
    work_dir: &Path,
    merge_base: &str,
) -> Result<u32, MeasurementError> {
    if config.public_api_regexes.is_empty() {
        return Ok(0);
    }

    let regexes: Vec<regex::Regex> = config
        .public_api_regexes
        .iter()
        .map(|p| regex::Regex::new(p))
        .collect::<Result<_, _>>()
        .map_err(|err| MeasurementError::Parse(format!("invalid public API regex: {err}")))?;

    let mut count = 0u32;
    for change in changes {
        if change.is_binary {
            continue;
        }
        if !has_source_extension(&change.path, &config.source_extensions) {
            continue;
        }
        let added_lines = match change.status {
            ChangeStatus::Untracked => {
                // Entire file content is new — scan every line.
                let path = work_dir.join(&change.path);
                std::fs::read_to_string(&path)
                    .map_err(|err| {
                        MeasurementError::Parse(format!(
                            "failed to read untracked source '{}': {err}",
                            change.path
                        ))
                    })?
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }
            ChangeStatus::Added | ChangeStatus::Modified | ChangeStatus::Deleted => {
                // Only count added diff lines from the merge-base diff.
                collect_added_diff_lines(work_dir, merge_base, &change.path)?
            }
        };
        for line in &added_lines {
            if regexes.iter().any(|re| re.is_match(line)) {
                count = count.saturating_add(1);
            }
        }
    }
    Ok(count)
}

/// Extract only the added (`+`-prefixed) lines from a `git diff` of a single
/// file path relative to the merge base.
///
/// Fails closed on command, IO, or UTF-8 errors.
fn collect_added_diff_lines(
    work_dir: &Path,
    merge_base: &str,
    path: &str,
) -> Result<Vec<String>, MeasurementError> {
    let output = Command::new("git")
        .args(["diff", merge_base, "--no-renames", "--no-color", "--", path])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: format!("diff {merge_base} -- {path}"),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !output.status.success() {
        return Err(MeasurementError::Git {
            command: format!("diff {merge_base} -- {path}"),
            message: format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    let diff_text = String::from_utf8(output.stdout).map_err(|err| {
        MeasurementError::Parse(format!("non-UTF-8 diff output for '{path}': {err}"))
    })?;

    let added = diff_text
        .lines()
        // Skip diff metadata lines: hunk headers (@@), file headers (+++/---),
        // diff header (diff --git), index lines, and deletion markers.
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .map(|line| line[1..].to_string())
        .collect();
    Ok(added)
}

/// Determine which subsystems overlap with the changed paths.
fn compute_changed_subsystems(
    changed_paths: &[String],
    charter: &CanonicalTaskCharter,
) -> Vec<String> {
    let mut subs: BTreeSet<String> = BTreeSet::new();
    for path in changed_paths {
        for (sub_id, prefixes) in &charter.subsystems {
            if prefixes.iter().any(|prefix| is_path_within(path, prefix)) {
                subs.insert(sub_id.clone());
            }
        }
    }
    subs.into_iter().collect()
}

/// Whether `path` is equal to or a descendant of `prefix`.
///
/// A prefix ending in `/**` matches all descendant files under the directory.
/// The `/**` suffix is stripped before comparison so `src/**` behaves
/// identically to `src`.
fn is_path_within(path: &str, prefix: &str) -> bool {
    let normalized_prefix = prefix
        .strip_suffix("/**")
        .or_else(|| prefix.strip_suffix("/"))
        .unwrap_or(prefix);
    if path == normalized_prefix {
        return true;
    }
    let path = Path::new(path);
    let prefix = Path::new(normalized_prefix);
    path.starts_with(prefix)
}

#[path = "measurement_dependency_diffs.rs"]
mod dependency_diffs;

/// Collect dependency manifest diffs: for each configured manifest, return the
/// path and the list of added dependency lines.
///
/// Delegates to the private dependency-diff implementation while preserving the
/// public `measurement::collect_dependency_diffs` path.
pub fn collect_dependency_diffs(
    work_dir: &Path,
    manifests: &[crate::workflow::schema::ScopeDependencyManifestConfig],
    merge_base: &str,
) -> Result<Vec<(String, Vec<String>)>, MeasurementError> {
    dependency_diffs::collect_dependency_diffs(work_dir, manifests, merge_base)
}

/// Extract dependency keys from manifest content for the given sections.
///
/// Delegated re-export so existing tests referencing `extract_dependency_keys`
/// via `super::` continue to resolve.
pub fn extract_dependency_keys(
    content: &str,
    sections: &[String],
) -> Result<Vec<String>, MeasurementError> {
    dependency_diffs::extract_dependency_keys(content, sections)
}

// ---------------------------------------------------------------------------
// Test helpers (public for integration tests)
// ---------------------------------------------------------------------------

/// Count added lines from a list of file changes (sum of `added_lines` where
/// present, with saturating addition). Exposed for testing.
#[must_use]
pub fn total_added_lines(changes: &[FileChange]) -> u32 {
    changes
        .iter()
        .filter_map(|c| c.added_lines)
        .fold(0u32, |acc, n| acc.saturating_add(n))
}

/// Build a `ScopeMeasurementConfig` with the given source extensions and
/// public API regexes, with deterministic-measurement flags enabled. Exposed
/// for testing.
#[must_use]
pub fn test_measurement_config(
    source_extensions: &[&str],
    public_api_regexes: &[&str],
) -> ScopeMeasurementConfig {
    ScopeMeasurementConfig {
        source_extensions: source_extensions.iter().map(|s| (*s).to_string()).collect(),
        public_api_regexes: public_api_regexes
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        disable_rename_inference: true,
        enumerate_untracked: true,
    }
}

/// Read the diff of a file's content between HEAD and the worktree to count
/// added lines for untracked files. This is used internally but exposed for
/// testing.
#[must_use]
pub fn file_change_from_path(path: &str, status: ChangeStatus, work_dir: &Path) -> FileChange {
    let is_binary = is_file_binary(work_dir, path);
    let added_lines = if is_binary {
        None
    } else {
        count_lines_in_file(work_dir, path)
    };
    FileChange {
        path: path.to_string(),
        status,
        added_lines,
        deleted_lines: if is_binary { None } else { Some(0) },
        is_binary,
    }
}

#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;
