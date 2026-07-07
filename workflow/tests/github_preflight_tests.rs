//! GitHub `gh` preflight adapter tests.
//!
//! Fixture-driven coverage for the readiness gate: CLI availability,
//! authentication, scope validation, repository access, and the orchestrating
//! `run_preflight`. No live `gh` invocation occurs.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::collections::HashMap;

use luther_workflow::adapters::github::{
    check_repo_access, run_preflight, GithubCommandRunner, GithubError,
};

/// Fixture runner: maps an argv-join key to a canned result.
#[derive(Default)]
struct FixtureGithubCommandRunner {
    responses: HashMap<String, Result<String, GithubError>>,
}

impl FixtureGithubCommandRunner {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
        }
    }

    fn key(argv: &[&str]) -> String {
        argv.join(" ")
    }

    fn with_ok(mut self, argv: &[&str], stdout: &str) -> Self {
        self.responses
            .insert(Self::key(argv), Ok(stdout.to_string()));
        self
    }

    fn with_err(mut self, argv: &[&str], err: GithubError) -> Self {
        self.responses.insert(Self::key(argv), Err(err));
        self
    }
}

impl GithubCommandRunner for FixtureGithubCommandRunner {
    fn run(&self, argv: &[String]) -> Result<String, GithubError> {
        let key = argv.join(" ");
        match self.responses.get(&key) {
            Some(Ok(out)) => Ok(out.clone()),
            Some(Err(GithubError::CliNotFound)) => Err(GithubError::CliNotFound),
            Some(Err(GithubError::AuthenticationRequired)) => {
                Err(GithubError::AuthenticationRequired)
            }
            Some(Err(GithubError::InsufficientScopes { missing })) => {
                Err(GithubError::InsufficientScopes {
                    missing: missing.clone(),
                })
            }
            Some(Err(GithubError::RepositoryNotAccessible { repo })) => {
                Err(GithubError::RepositoryNotAccessible { repo: repo.clone() })
            }
            Some(Err(GithubError::CommandFailed {
                argv: a,
                exit_code,
                stderr,
            })) => Err(GithubError::CommandFailed {
                argv: a.clone(),
                exit_code: *exit_code,
                stderr: stderr.clone(),
            }),
            Some(Err(GithubError::CacheLock { context, error })) => Err(GithubError::CacheLock {
                context: context.clone(),
                error: error.clone(),
            }),
            None => Err(GithubError::CommandFailed {
                argv: argv.to_vec(),
                exit_code: Some(1),
                stderr: format!("no fixture for {key}"),
            }),
        }
    }
}

const VERSION_ARGV: [&str; 2] = ["gh", "--version"];
const AUTH_ARGV: [&str; 3] = ["gh", "auth", "status"];

fn repo_argv(repo: &str) -> Vec<String> {
    vec![
        "gh".to_string(),
        "repo".to_string(),
        "view".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "nameWithOwner".to_string(),
    ]
}

#[test]
fn cli_not_found_reports_install_action() {
    let runner =
        FixtureGithubCommandRunner::new().with_err(&VERSION_ARGV, GithubError::CliNotFound);
    let err = run_preflight(&runner, "owner/name", &["repo"]).unwrap_err();
    assert!(matches!(err, GithubError::CliNotFound));
    let diag = err.get_diagnostics();
    assert!(diag
        .get("required_action")
        .unwrap()
        .contains("gh auth login"));
    assert_eq!(diag.get("error_type").unwrap(), "GithubError");
}

#[test]
fn unauthenticated_reports_login_action() {
    let runner = FixtureGithubCommandRunner::new()
        .with_ok(&VERSION_ARGV, "gh version 2.0.0")
        .with_ok(&AUTH_ARGV, "You are not logged into any GitHub hosts.");
    let err = run_preflight(&runner, "owner/name", &["repo"]).unwrap_err();
    assert!(matches!(err, GithubError::AuthenticationRequired));
    let diag = err.get_diagnostics();
    assert!(diag
        .get("required_action")
        .unwrap()
        .contains("gh auth login"));
}

#[test]
fn missing_scope_reports_insufficient() {
    let runner = FixtureGithubCommandRunner::new()
        .with_ok(&VERSION_ARGV, "gh version 2.0.0")
        .with_ok(
            &AUTH_ARGV,
            "Logged in to github.com as octocat\n  - Token scopes: 'gist'",
        );
    let err = run_preflight(&runner, "owner/name", &["repo"]).unwrap_err();
    match err {
        GithubError::InsufficientScopes { ref missing } => {
            assert_eq!(missing, &vec!["repo".to_string()]);
        }
        other => panic!("unexpected: {other:?}"),
    }
    let diag = err.get_diagnostics();
    assert_eq!(diag.get("missing_scopes").unwrap(), "repo");
}

#[test]
fn repo_not_accessible_reports_repo() {
    let runner = FixtureGithubCommandRunner::new().with_err(
        &[
            "gh",
            "repo",
            "view",
            "owner/name",
            "--json",
            "nameWithOwner",
        ],
        GithubError::CommandFailed {
            argv: repo_argv("owner/name"),
            exit_code: Some(1),
            stderr: "Could not resolve to a Repository".to_string(),
        },
    );
    let err = check_repo_access(&runner, "owner/name").unwrap_err();
    match err {
        GithubError::RepositoryNotAccessible { ref repo } => {
            assert_eq!(repo, "owner/name");
        }
        other => panic!("unexpected: {other:?}"),
    }
    let diag = err.get_diagnostics();
    assert_eq!(diag.get("repo").unwrap(), "owner/name");
}

#[test]
fn successful_preflight_returns_report() {
    let runner = FixtureGithubCommandRunner::new()
        .with_ok(&VERSION_ARGV, "gh version 2.0.0")
        .with_ok(
            &AUTH_ARGV,
            "Logged in to github.com as octocat\n  - Token scopes: 'repo', 'read:org'",
        )
        .with_ok(
            &[
                "gh",
                "repo",
                "view",
                "owner/name",
                "--json",
                "nameWithOwner",
            ],
            "{\"nameWithOwner\":\"owner/name\"}",
        );
    let report = run_preflight(&runner, "owner/name", &["repo"]).unwrap();
    assert_eq!(report.repo, "owner/name");
    assert!(report.scopes.contains(&"repo".to_string()));
}

#[test]
fn get_diagnostics_shape_matches_repo_prep() {
    let variants = vec![
        GithubError::CliNotFound,
        GithubError::AuthenticationRequired,
        GithubError::InsufficientScopes {
            missing: vec!["repo".to_string()],
        },
        GithubError::RepositoryNotAccessible {
            repo: "owner/name".to_string(),
        },
        GithubError::CommandFailed {
            argv: vec!["gh".to_string(), "api".to_string()],
            exit_code: Some(2),
            stderr: "boom".to_string(),
        },
    ];
    for err in variants {
        let diag = err.get_diagnostics();
        assert_eq!(diag.get("error_type").unwrap(), "GithubError");
        assert!(diag.contains_key("message"));
        assert!(diag.contains_key("timestamp"));
    }
}

#[test]
fn command_failed_diagnostics_include_exit_code() {
    let err = GithubError::CommandFailed {
        argv: vec!["gh".to_string(), "api".to_string()],
        exit_code: Some(7),
        stderr: "boom".to_string(),
    };
    let diag = err.get_diagnostics();
    assert_eq!(diag.get("exit_code").unwrap(), "7");
    assert_eq!(diag.get("argv").unwrap(), "gh api");
}
