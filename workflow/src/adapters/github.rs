//! GitHub CLI (`gh`) preflight adapter.
//!
//! Provides a structured readiness gate for the GitHub CLI before any workflow
//! state is created. The workflow runtime shells out to `gh` from both shell
//! steps and PR follow-up executors; this adapter validates that `gh` exists, is
//! authenticated, can reach the target repository, and holds the required token
//! scopes, surfacing actionable diagnostics on failure.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::collections::HashMap;
use std::process::Command;

use thiserror::Error;

// Re-export the existing executor command-runner seam so this adapter module is
// the single documented home for the "GitHub adapter trait" surface.
pub use crate::engine::executors::github_pr::{GithubPrCommandRunner, SystemGithubPrCommandRunner};

/// Structured error for GitHub CLI preflight failures.
#[derive(Debug, Error)]
pub enum GithubError {
    /// `gh` was not found on PATH.
    #[error(
        "GitHub CLI not found on PATH; install gh (https://cli.github.com) and run `gh auth login`"
    )]
    CliNotFound,
    /// `gh` is installed but the user is not authenticated.
    #[error("GitHub CLI is not authenticated; run `gh auth login`")]
    AuthenticationRequired,
    /// The authenticated token is missing one or more required scopes.
    #[error(
        "GitHub token is missing required scope(s): {}; run `gh auth refresh -s {}`",
        missing.join(", "),
        missing.join(",")
    )]
    InsufficientScopes {
        /// Scopes that are required but not granted.
        missing: Vec<String>,
    },
    /// The target repository is not accessible to the authenticated user.
    #[error("GitHub repository `{repo}` is not accessible; verify the name and your permissions")]
    RepositoryNotAccessible {
        /// The repository in `owner/name` form.
        repo: String,
    },
    /// A `gh` command exited non-zero for an otherwise-unclassified reason.
    #[error("GitHub command {argv:?} failed with exit code {exit_code:?}: {stderr}")]
    CommandFailed {
        /// The argv that was executed.
        argv: Vec<String>,
        /// The process exit code, if available.
        exit_code: Option<i32>,
        /// Captured stderr.
        stderr: String,
    },
}

impl GithubError {
    /// Get structured diagnostics for this error.
    ///
    /// Mirrors the shape produced by `RepoPrepError::get_diagnostics`: every
    /// variant carries `error_type`, `message`, and `timestamp`, plus
    /// variant-specific fields (`missing_scopes`, `required_action`,
    /// `exit_code`, `repo`).
    pub fn get_diagnostics(&self) -> HashMap<String, String> {
        let mut diag = HashMap::new();
        diag.insert("error_type".to_string(), "GithubError".to_string());
        diag.insert("message".to_string(), self.to_string());
        diag.insert("timestamp".to_string(), chrono::Utc::now().to_rfc3339());

        match self {
            GithubError::CliNotFound => {
                diag.insert(
                    "required_action".to_string(),
                    "install gh and run `gh auth login`".to_string(),
                );
            }
            GithubError::AuthenticationRequired => {
                diag.insert(
                    "required_action".to_string(),
                    "run `gh auth login`".to_string(),
                );
            }
            GithubError::InsufficientScopes { missing } => {
                diag.insert("missing_scopes".to_string(), missing.join(","));
                diag.insert(
                    "required_action".to_string(),
                    format!("run `gh auth refresh -s {}`", missing.join(",")),
                );
            }
            GithubError::RepositoryNotAccessible { repo } => {
                diag.insert("repo".to_string(), repo.clone());
                diag.insert(
                    "required_action".to_string(),
                    "verify repository name and permissions".to_string(),
                );
            }
            GithubError::CommandFailed {
                exit_code, argv, ..
            } => {
                if let Some(code) = exit_code {
                    diag.insert("exit_code".to_string(), code.to_string());
                }
                diag.insert("argv".to_string(), argv.join(" "));
            }
        }

        diag
    }
}

/// Successful preflight summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubPreflightReport {
    /// The repository that was validated.
    pub repo: String,
    /// The token scopes that were observed.
    pub scopes: Vec<String>,
}

/// Command-runner seam for `gh` preflight calls.
///
/// Returns captured stdout on success. Implementations are responsible for
/// mapping a missing `gh` binary to [`GithubError::CliNotFound`] and any other
/// non-zero exit to [`GithubError::CommandFailed`].
pub trait GithubCommandRunner {
    /// Execute `gh` with the given argv and return captured stdout.
    fn run(&self, argv: &[String]) -> Result<String, GithubError>;
}

/// Production runner that spawns `gh` via `std::process::Command`.
#[derive(Debug, Default)]
pub struct SystemGithubCommandRunner;

impl GithubCommandRunner for SystemGithubCommandRunner {
    fn run(&self, argv: &[String]) -> Result<String, GithubError> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| GithubError::CommandFailed {
                argv: argv.to_vec(),
                exit_code: None,
                stderr: "github command argv must not be empty".to_string(),
            })?;
        let output = Command::new(program).args(args).output().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                GithubError::CliNotFound
            } else {
                GithubError::CommandFailed {
                    argv: argv.to_vec(),
                    exit_code: None,
                    stderr: format!("spawn gh command: {err}"),
                }
            }
        })?;
        if !output.status.success() {
            return Err(GithubError::CommandFailed {
                argv: argv.to_vec(),
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Build an argv vector from string slices.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// Verify that `gh` is available by running `gh --version`.
pub fn check_cli_available(runner: &dyn GithubCommandRunner) -> Result<(), GithubError> {
    runner.run(&argv(&["gh", "--version"]))?;
    Ok(())
}

/// Verify that `gh` reports an authenticated session via `gh auth status`.
pub fn check_auth_status(runner: &dyn GithubCommandRunner) -> Result<String, GithubError> {
    // `gh auth status` writes to stderr historically; system runner captures
    // stdout, while fixtures provide the combined text. A non-zero exit is
    // mapped to `CommandFailed`, which we re-classify as auth required.
    let output = match runner.run(&argv(&["gh", "auth", "status"])) {
        Ok(out) => out,
        Err(GithubError::CommandFailed { stderr, .. }) => {
            // gh returns non-zero when logged out; treat any such failure as
            // an authentication requirement, preserving the stderr text for
            // scope parsing fallback.
            if is_logged_out(&stderr) || stderr.is_empty() {
                return Err(GithubError::AuthenticationRequired);
            }
            stderr
        }
        Err(other) => return Err(other),
    };
    if is_logged_out(&output) {
        return Err(GithubError::AuthenticationRequired);
    }
    Ok(output)
}

/// Returns true if the given `gh auth status` text indicates a logged-out state.
fn is_logged_out(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("not logged in")
        || lower.contains("you are not logged into")
        || lower.contains("no accounts")
}

/// Parse the token scopes from `gh auth status` output.
///
/// Looks for a line containing `Token scopes:` followed by quoted, comma
/// separated scope names (e.g. `Token scopes: 'repo', 'read:org'`).
pub fn parse_token_scopes(text: &str) -> Vec<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("Token scopes:") {
            let rest = &trimmed[idx + "Token scopes:".len()..];
            return rest
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"' || c == ' '))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

/// Verify the authenticated token holds every required scope.
pub fn check_required_scopes(
    auth_status_text: &str,
    required: &[&str],
) -> Result<Vec<String>, GithubError> {
    let scopes = parse_token_scopes(auth_status_text);
    let missing: Vec<String> = required
        .iter()
        .filter(|req| !scopes.iter().any(|s| s == *req))
        .map(|req| (*req).to_string())
        .collect();
    if missing.is_empty() {
        Ok(scopes)
    } else {
        Err(GithubError::InsufficientScopes { missing })
    }
}

/// Verify the target repository is accessible via `gh repo view`.
pub fn check_repo_access(runner: &dyn GithubCommandRunner, repo: &str) -> Result<(), GithubError> {
    match runner.run(&argv(&[
        "gh",
        "repo",
        "view",
        repo,
        "--json",
        "nameWithOwner",
    ])) {
        Ok(_) => Ok(()),
        Err(GithubError::CommandFailed { .. }) => Err(GithubError::RepositoryNotAccessible {
            repo: repo.to_string(),
        }),
        Err(other) => Err(other),
    }
}

/// Run the full GitHub preflight gate.
///
/// Validates, in order: CLI availability, authentication, required scopes, and
/// repository access. Returns a [`GithubPreflightReport`] on success or a
/// structured [`GithubError`] with actionable diagnostics on the first failure.
pub fn run_preflight(
    runner: &dyn GithubCommandRunner,
    repo: &str,
    required_scopes: &[&str],
) -> Result<GithubPreflightReport, GithubError> {
    check_cli_available(runner)?;
    let auth_text = check_auth_status(runner)?;
    let scopes = check_required_scopes(&auth_text, required_scopes)?;
    check_repo_access(runner, repo)?;
    Ok(GithubPreflightReport {
        repo: repo.to_string(),
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_token_scopes_handles_quoted_list() {
        let text = "  - Token scopes: 'repo', 'read:org', 'gist'";
        let scopes = parse_token_scopes(text);
        assert_eq!(scopes, vec!["repo", "read:org", "gist"]);
    }

    #[test]
    fn parse_token_scopes_empty_when_absent() {
        let text = "Logged in to github.com as octocat";
        assert!(parse_token_scopes(text).is_empty());
    }

    #[test]
    fn is_logged_out_detects_not_logged_in_wording() {
        assert!(is_logged_out("You are not logged into any GitHub hosts."));
        assert!(!is_logged_out("Logged in to github.com as octocat"));
    }

    #[test]
    fn check_required_scopes_reports_missing() {
        let text = "Token scopes: 'gist'";
        let err = check_required_scopes(text, &["repo"]).unwrap_err();
        match err {
            GithubError::InsufficientScopes { missing } => {
                assert_eq!(missing, vec!["repo"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn check_required_scopes_ok_when_present() {
        let text = "Token scopes: 'repo', 'read:org'";
        let scopes = check_required_scopes(text, &["repo"]).unwrap();
        assert!(scopes.contains(&"repo".to_string()));
    }
}
