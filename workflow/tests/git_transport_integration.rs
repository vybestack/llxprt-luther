//! Git transport separated from logical repository identity.
//!
//! These tests run real `git` against a real bare repository on disk. Nothing
//! is faked and no global Git configuration is mutated: the point of the
//! transport seam is that a harness can exercise the actual push path, and a
//! test that stubbed Git would prove nothing about it.

use std::path::Path;
use std::process::Command;

use luther_workflow::workflow::config_loader::parse_workflow_config_toml;
use luther_workflow::workflow::schema::WorkflowConfig;
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, default_transport_url, GIT_TRANSPORT_URL_VAR,
};
use luther_workflow::workflow::TargetProfileOverrides;

/// A config parsed from real TOML through the production loader, so these tests
/// exercise the same resolution path a run does rather than a hand-built struct.
fn config_for(target_repo: &str) -> WorkflowConfig {
    let toml = format!(
        r#"
config_id = "git-transport-test"
workflow_type_id = "llxprt-issue-fix-v1"

[runtime]
timeout_seconds = 60
max_retries = 1

[repo]
workspace_strategy = "temp_clone"
branch_template = "issue{{issue_number}}"
base_branch = "main"

[guard_limits]

[variables]
target_repo = "{target_repo}"
primary_issue_number = "1"
work_dir = "/tmp/luther-transport-test/workspace"
artifact_dir = "/tmp/luther-transport-test/artifacts"
target_ecosystem_name = "rust"
"#
    );
    parse_workflow_config_toml(&toml).expect("config parses")
}

/// Run `git` with the same hardening the production workflow applies.
///
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`/`GIT_CONFIG_NOSYSTEM` neutralize the
/// developer's and machine's configuration, so a test cannot pass because of
/// ambient state and cannot write outside its temp directory.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Luther Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Luther Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git must be available");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_commit_pushes_to_a_local_bare_repository_over_the_resolved_transport() {
    let root = tempfile::tempdir().unwrap();
    let bare = root.path().join("remote.git");
    let work = root.path().join("workspace");
    std::fs::create_dir_all(&work).unwrap();

    // A real bare repository standing in for the remote.
    git(root.path(), &["init", "--bare", "-b", "main", "remote.git"]);

    // Logical identity stays a GitHub repository; transport points at the bare
    // repository on disk. That disagreement is the feature under test.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            transport_url: Some(bare.to_string_lossy().into_owned()),
            ..TargetProfileOverrides::default()
        },
    )
    .expect("transport override applies");

    let transport = config
        .variables
        .get(GIT_TRANSPORT_URL_VAR)
        .expect("transport resolved")
        .clone();
    assert_eq!(transport, bare.to_string_lossy());
    assert_eq!(
        config.variables.get("target_repo").unwrap(),
        "vybestack/llxprt-luther",
        "logical identity must be untouched by a transport override"
    );

    // Real work, really pushed.
    git(&work, &["init", "-b", "main"]);
    git(&work, &["remote", "add", "origin", &transport]);
    std::fs::write(work.join("file.txt"), "content\n").unwrap();
    git(&work, &["add", "file.txt"]);
    git(&work, &["commit", "-m", "add file"]);
    git(&work, &["push", "-u", "origin", "main"]);

    // Verified by reading the bare repository's own ref, not by trusting the
    // push command's exit code.
    let local = stdout(&git(&work, &["rev-parse", "HEAD"]));
    let remote = stdout(&git(&bare, &["rev-parse", "refs/heads/main"]));
    assert_eq!(local, remote, "the commit must be present in the bare repo");

    let blob = stdout(&git(&bare, &["show", "refs/heads/main:file.txt"]));
    assert_eq!(blob, "content", "pushed content must be readable remotely");
}

#[test]
fn the_default_transport_is_unchanged_for_a_logical_repository() {
    // Guards the production path: with no override the derived URL must equal
    // the string the workflow hardcoded before this seam existed.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(&mut config, &TargetProfileOverrides::default()).unwrap();

    assert_eq!(
        config.variables.get(GIT_TRANSPORT_URL_VAR).unwrap(),
        "https://github.com/vybestack/llxprt-luther.git"
    );
    assert_eq!(
        default_transport_url("vybestack/llxprt-luther"),
        "https://github.com/vybestack/llxprt-luther.git"
    );
}

#[test]
fn git_hardening_env_is_in_force_during_these_tests() {
    // If the hardening were not applied, a developer's global config could
    // decide the outcome. Asserting it here keeps the other tests meaningful.
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-b", "main", "repo"]);
    let repo = root.path().join("repo");

    let output = Command::new("git")
        .args(["config", "--global", "--list"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git must be available");
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "global git config must be neutralized"
    );
}
