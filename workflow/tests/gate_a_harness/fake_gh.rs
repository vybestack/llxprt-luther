//! A `gh` replacement built from captured real command contracts.
//!
//! Every invocation the shipping workflow makes is enumerated here. Anything
//! else exits non-zero with a diagnostic naming the unrecognized invocation,
//! because a fake that answers plausibly to an unknown command is how a
//! harness ends up proving that its own stub works.
//!
//! Each invocation is appended to a log so the harness can assert on what the
//! run actually asked GitHub for, rather than on what the harness expected it
//! to ask.

use std::fmt::Write as _;

/// State the fake exposes to the running workflow.
pub struct FakeGitHub {
    /// Issue the run is allowed to select.
    pub issue_number: u64,
    pub issue_title: String,
    pub issue_body: String,
    /// When set, `pr view`/`pr list` report this pre-existing PR. Used by the
    /// negative control that injects an open PR for the target issue.
    pub existing_pr: Option<ExistingPr>,
    /// Whether `pr create` is permitted to succeed.
    pub allow_pr_create: bool,
}

#[derive(Clone)]
pub struct ExistingPr {
    pub number: u64,
    pub is_draft: bool,
    pub state: String,
}

impl FakeGitHub {
    #[must_use]
    pub fn new(issue_number: u64) -> Self {
        Self {
            issue_number,
            issue_title: "Harness target issue".to_string(),
            issue_body: "Body supplied by the Gate A-R harness.".to_string(),
            existing_pr: None,
            allow_pr_create: true,
        }
    }

    /// Renders the fake as a shell script installed ahead of the real `gh`.
    ///
    /// A script rather than a Rust binary because the workflow invokes `gh`
    /// through the shell by name; intercepting on `PATH` is the same mechanism
    /// a real misconfiguration would use, so the interception itself is
    /// faithful.
    #[must_use]
    pub fn script(&self, log_path: &std::path::Path) -> String {
        let mut script = String::new();
        let _ = writeln!(script, "#!/usr/bin/env bash");
        let _ = writeln!(script, "set -euo pipefail");
        // Record every invocation before dispatching, so an unrecognized
        // command is still visible in the log that explains the failure.
        let _ = writeln!(
            script,
            "printf '%s\\n' \"$*\" >> {}",
            shell_quote(&log_path.to_string_lossy())
        );
        let _ = writeln!(script, "case \"$*\" in");

        self.write_issue_view(&mut script);
        self.write_issue_list(&mut script);
        self.write_issue_edit(&mut script);
        self.write_issue_comment(&mut script);
        self.write_pr_view(&mut script);
        self.write_pr_list(&mut script);
        self.write_pr_create(&mut script);
        self.write_api(&mut script);

        // Fail closed. An unknown invocation is a contract gap, not a default.
        let _ = writeln!(script, "  *)");
        let _ = writeln!(
            script,
            "    echo \"fake gh: unrecognized invocation: gh $*\" >&2"
        );
        let _ = writeln!(script, "    exit 97");
        let _ = writeln!(script, "    ;;");
        let _ = writeln!(script, "esac");
        script
    }

    fn write_issue_view(&self, script: &mut String) {
        // `gh issue view --json` returns exactly the requested fields. The
        // workflow asks for several different field sets and pipes some of
        // them through `jq`, so the fake returns the union: omitting a field
        // the caller selected makes jq iterate over null, which is a fake
        // defect masquerading as a product failure.
        let payload = serde_json::json!({
            "number": self.issue_number,
            "title": self.issue_title,
            "body": self.issue_body,
            "state": "OPEN",
            "url": format!("https://github.com/example/repo/issues/{}", self.issue_number),
            "comments": [],
            "labels": [],
            "assignees": [],
        });
        let _ = writeln!(script, "  \"issue view\"*)");
        let _ = writeln!(
            script,
            "    printf '%s' {}",
            shell_quote(&payload.to_string())
        );
        let _ = writeln!(script, "    ;;");
    }

    fn write_issue_list(&self, script: &mut String) {
        let payload = serde_json::json!([{
            "number": self.issue_number,
            "title": self.issue_title,
            "labels": [],
            "assignees": [],
        }]);
        let _ = writeln!(script, "  \"issue list\"*)");
        let _ = writeln!(
            script,
            "    printf '%s' {}",
            shell_quote(&payload.to_string())
        );
        let _ = writeln!(script, "    ;;");
    }

    fn write_issue_edit(&self, script: &mut String) {
        let _ = writeln!(script, "  \"issue edit\"*)");
        let _ = writeln!(script, "    exit 0");
        let _ = writeln!(script, "    ;;");
    }

    fn write_issue_comment(&self, script: &mut String) {
        let _ = writeln!(script, "  \"issue comment\"*)");
        let _ = writeln!(script, "    exit 0");
        let _ = writeln!(script, "    ;;");
    }

    fn write_pr_view(&self, script: &mut String) {
        let _ = writeln!(script, "  \"pr view\"*)");
        match &self.existing_pr {
            Some(pr) => {
                let payload = serde_json::json!({
                    "number": pr.number,
                    "url": format!("https://github.com/example/repo/pull/{}", pr.number),
                    "title": "Pre-existing pull request",
                    "state": pr.state,
                    "isDraft": pr.is_draft,
                });
                let _ = writeln!(
                    script,
                    "    printf '%s' {}",
                    shell_quote(&payload.to_string())
                );
            }
            // `gh pr view` exits non-zero when no PR exists; the workflow
            // relies on that, so the fake reproduces it rather than printing
            // an empty object.
            None => {
                let _ = writeln!(script, "    echo 'no pull requests found' >&2");
                let _ = writeln!(script, "    exit 1");
            }
        }
        let _ = writeln!(script, "    ;;");
    }

    fn write_pr_list(&self, script: &mut String) {
        let payload = match &self.existing_pr {
            Some(pr) => serde_json::json!([{
                "number": pr.number,
                "state": pr.state,
                "isDraft": pr.is_draft,
            }]),
            None => serde_json::json!([]),
        };
        let _ = writeln!(script, "  \"pr list\"*)");
        let _ = writeln!(
            script,
            "    printf '%s' {}",
            shell_quote(&payload.to_string())
        );
        let _ = writeln!(script, "    ;;");
    }

    fn write_pr_create(&self, script: &mut String) {
        let _ = writeln!(script, "  \"pr create\"*)");
        if self.allow_pr_create {
            // Record the created PR as modeled state only on success. Scoring
            // reads this record rather than the fact that the command was
            // invoked, so a `pr create` that fails cannot be counted as a
            // created pull request.
            let _ = writeln!(
                script,
                "    printf 'created draft=%s\\n' \"$(case \"$*\" in *--draft*) echo true;; *) echo false;; esac)\" >> \"$LUTHER_FAKE_GH_STATE\""
            );
            let _ = writeln!(
                script,
                "    printf '%s' 'https://github.com/example/repo/pull/4242'"
            );
        } else {
            let _ = writeln!(script, "    echo 'pr create refused by harness' >&2");
            let _ = writeln!(script, "    exit 1");
        }
        let _ = writeln!(script, "    ;;");
    }

    fn write_api(&self, script: &mut String) {
        // Only the specific repos lookup the workflow performs.
        let payload = serde_json::json!({ "default_branch": "main" });
        let _ = writeln!(script, "  \"api repos\"*)");
        let _ = writeln!(
            script,
            "    printf '%s' {}",
            shell_quote(&payload.to_string())
        );
        let _ = writeln!(script, "    ;;");
    }
}

/// Single-quotes a value for POSIX shell.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}
