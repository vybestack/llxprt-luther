//! Git transport separated from logical repository identity.
//!
//! These tests drive the *shipping* workflow steps -- the `git_config_publish`
//! executor and the workflow's own `push_changes` command -- against a real
//! bare repository on disk. Nothing about Git is faked.
//!
//! The distinction matters: a test that ran `git push` itself would pass even
//! if the production publisher ignored the transport entirely, or if the
//! production push step were deleted. These invoke the real code and read the
//! result out of the remote repository.

use std::path::Path;
use std::process::Command;

use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::git_config_publish::GitConfigPublishExecutor;
use luther_workflow::engine::executors::shell::ShellExecutor;
use luther_workflow::engine::executors::WorkspaceOwnershipVerifyExecutor;
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::workflow::config_loader::{
    parse_workflow_config_toml, parse_workflow_type_toml,
};
use luther_workflow::workflow::schema::WorkflowConfig;
use luther_workflow::workflow::schema::WorkflowType;
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, default_transport_url, GIT_TRANSPORT_URL_VAR,
};
use luther_workflow::workflow::TargetProfileOverrides;

/// Plain `git`, with no hardening supplied by the test.
///
/// Test setup deliberately does NOT neutralize global configuration, so the
/// hardening assertions below observe what production code does rather than
/// what the harness did on its behalf.
/// Serializes tests that depend on process-global Git environment.
///
/// The environment is shared by every test in this binary and they run in
/// parallel, so a hostile `GIT_CONFIG_GLOBAL` set by one test is inherited by
/// any Git another test happens to launch inside that window.
fn git_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let _guard = git_env_lock();
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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

/// The shipping dogfood workflow, parsed from the file production uses.
fn shipped_workflow() -> WorkflowType {
    let text = std::fs::read_to_string("config/workflows/llxprt-luther-dogfood-v1.toml")
        .expect("shipping workflow readable");
    parse_workflow_type_toml(&text).expect("shipping workflow parses")
}

/// Parameters of a step in the shipping workflow, by id.
fn shipped_step_params(workflow: &WorkflowType, step_id: &str) -> serde_json::Value {
    workflow
        .steps
        .iter()
        .find(|step| step.step_id == step_id)
        .unwrap_or_else(|| panic!("shipping workflow must define {step_id}"))
        .parameters
        .clone()
        .unwrap_or_else(|| panic!("{step_id} must carry parameters"))
}

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

#[test]
fn the_shipping_publisher_and_push_step_deliver_a_commit_to_a_bare_repository() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "-b", "main", "remote.git"]);
    let bare = root.path().join("remote.git");

    // Seed the remote with a base branch so --force-with-lease has an upstream.
    let seed = root.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("base.txt"), "base\n").unwrap();
    git(&seed, &["add", "base.txt"]);
    git(&seed, &["commit", "-m", "base"]);
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "-u", "origin", "main"]);

    // Logical identity is a GitHub repository; transport is the bare repo.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            transport_url: Some(bare.to_string_lossy().into_owned()),
            ..TargetProfileOverrides::default()
        },
    )
    .expect("transport override applies");
    let transport = config.variables[GIT_TRANSPORT_URL_VAR].clone();

    // Clone through the resolved transport, then let the SHIPPING publisher
    // write the repository's Git configuration.
    let workspace = root.path().join("workspace");
    // Ownership is established before the workspace is populated, matching the
    // production ordering the verify step enforces.
    luther_workflow::engine::continuation::provision_workspace_owner_marker(
        &workspace,
        "run-transport",
    )
    .expect("workspace ownership marker");
    // Production initializes an owned workspace and fetches, rather than
    // cloning over it; the publisher then configures origin.
    git(&workspace, &["init", "-b", "main"]);

    let mut context = StepContext::new(workspace.clone(), "run-transport".to_string());
    context.set("work_dir", &workspace.to_string_lossy());
    context.set_current_step_id("workspace_ownership_verify");
    assert_eq!(
        WorkspaceOwnershipVerifyExecutor
            .execute(&mut context, &serde_json::Value::Null)
            .unwrap(),
        StepOutcome::Success,
        "the shipping ownership step must authorize the workspace"
    );
    context.set("target_repo", "vybestack/llxprt-luther");
    context.set(GIT_TRANSPORT_URL_VAR, &transport);
    context.set("base_branch", "main");
    context.set("issue_number", "42");
    context.set("setup_workspace.existing_pr_number", "0");
    context.set_current_step_id("git_config_publish");

    let workflow = shipped_workflow();
    assert_eq!(
        GitConfigPublishExecutor
            .execute(
                &mut context,
                &shipped_step_params(&workflow, "git_config_publish")
            )
            .unwrap(),
        StepOutcome::Success,
        "the shipping publisher must accept the resolved transport"
    );

    // The publisher, not the test, decided origin.
    let configured = stdout(&git(&workspace, &["remote", "get-url", "origin"]));
    assert_eq!(
        configured, transport,
        "the shipping publisher must point origin at the resolved transport"
    );

    // Fetch through the origin the publisher configured, then branch.
    git(&workspace, &["fetch", "origin"]);
    git(&workspace, &["checkout", "-B", "main", "origin/main"]);
    git(&workspace, &["checkout", "-b", "issue42"]);
    std::fs::write(workspace.join("file.txt"), "content\n").unwrap();
    git(&workspace, &["add", "file.txt"]);
    git(&workspace, &["commit", "-m", "add file"]);

    // The SHIPPING push step performs the push, under the same lock the
    // hostile-config test uses so neither sees the other's environment.
    context.set_current_step_id("push_changes");
    let pushed = {
        let _guard = git_env_lock();
        ShellExecutor.execute(
            &mut context,
            &shipped_step_params(&workflow, "push_changes"),
        )
    };
    assert_eq!(
        pushed.unwrap(),
        StepOutcome::Success,
        "the shipping push step must succeed over the resolved transport; stderr: {}",
        context
            .get("stderr")
            .map_or("<none captured>", String::as_str)
    );

    // Verified by reading the bare repository itself.
    let local = stdout(&git(&workspace, &["rev-parse", "HEAD"]));
    let remote = stdout(&git(&bare, &["rev-parse", "refs/heads/issue42"]));
    assert_eq!(local, remote, "the commit must be present in the bare repo");
    assert_eq!(
        stdout(&git(&bare, &["show", "refs/heads/issue42:file.txt"])),
        "content"
    );
}

#[test]
fn the_shipping_push_step_neutralizes_hostile_global_git_configuration() {
    // A negative control for the hardening: a global config that rewrites the
    // transport to a decoy is installed, and the SHIPPING push step must
    // neutralize it. If the production hardening were removed, the commit would
    // land in the decoy and this test would fail.
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "-b", "main", "remote.git"]);
    git(root.path(), &["init", "--bare", "-b", "main", "decoy.git"]);
    let bare = root.path().join("remote.git");
    let decoy = root.path().join("decoy.git");

    let seed = root.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("base.txt"), "base\n").unwrap();
    git(&seed, &["add", "base.txt"]);
    git(&seed, &["commit", "-m", "base"]);
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "-u", "origin", "main"]);

    let hostile = root.path().join("hostile-gitconfig");
    std::fs::write(
        &hostile,
        format!(
            "[url \"{}\"]\n\tinsteadOf = {}\n",
            decoy.display(),
            bare.display()
        ),
    )
    .unwrap();

    let workspace = root.path().join("workspace");
    git(
        root.path(),
        &[
            "clone",
            &bare.to_string_lossy(),
            &workspace.to_string_lossy(),
        ],
    );
    git(&workspace, &["checkout", "-b", "issue42"]);
    std::fs::write(workspace.join("file.txt"), "content\n").unwrap();
    git(&workspace, &["add", "file.txt"]);
    git(&workspace, &["commit", "-m", "add file"]);

    let workflow = shipped_workflow();
    let mut context = StepContext::new(workspace.clone(), "run-hostile".to_string());
    context.set("base_branch", "main");
    context.set("issue_number", "42");
    context.set("setup_workspace.existing_pr_number", "0");
    context.set_current_step_id("push_changes");

    // The hostile configuration is live in the environment the step inherits.
    // Process environment is global to the test binary, so this window is
    // serialized against every other test here that shells out to Git --
    // otherwise the rewrite leaks into their pushes and fails them.
    let outcome = {
        let _guard = git_env_lock();
        let restore = std::env::var("GIT_CONFIG_GLOBAL").ok();
        std::env::set_var("GIT_CONFIG_GLOBAL", &hostile);
        let outcome = ShellExecutor.execute(
            &mut context,
            &shipped_step_params(&workflow, "push_changes"),
        );
        match restore {
            Some(previous) => std::env::set_var("GIT_CONFIG_GLOBAL", previous),
            None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
        }
        outcome
    };
    assert_eq!(
        outcome.unwrap(),
        StepOutcome::Success,
        "stderr: {}",
        context
            .get("stderr")
            .map_or("<none captured>", String::as_str)
    );

    // The real remote received it; the decoy did not.
    let local = stdout(&git(&workspace, &["rev-parse", "HEAD"]));
    assert_eq!(
        stdout(&git(&bare, &["rev-parse", "refs/heads/issue42"])),
        local,
        "the push must reach the intended remote despite hostile global config"
    );
    let decoy_refs = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "refs/heads/issue42"])
        .current_dir(&decoy)
        .output()
        .expect("git available");
    assert!(
        !decoy_refs.status.success(),
        "the decoy remote must never receive the push"
    );
}

#[test]
fn the_default_transport_tracks_logical_identity_and_is_unchanged() {
    // Guards the production path: with no override the derived URL must equal
    // the string the workflow hardcoded before this seam existed.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(&mut config, &TargetProfileOverrides::default()).unwrap();
    assert_eq!(
        config.variables[GIT_TRANSPORT_URL_VAR],
        "https://github.com/vybestack/llxprt-luther.git"
    );
    assert_eq!(
        default_transport_url("vybestack/llxprt-luther"),
        "https://github.com/vybestack/llxprt-luther.git"
    );

    // A later repository override must move the derived transport with it,
    // otherwise the API would address one repository while Git addressed
    // another.
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            repo: Some("example/other".to_string()),
            ..TargetProfileOverrides::default()
        },
    )
    .unwrap();
    assert_eq!(config.variables["target_repo"], "example/other");
    assert_eq!(
        config.variables[GIT_TRANSPORT_URL_VAR], "https://github.com/example/other.git",
        "a derived transport must follow the effective logical identity"
    );
}

#[test]
fn an_explicit_transport_survives_a_later_repository_override() {
    // The mirror image: an explicit choice is authoritative and must not be
    // silently recomputed when identity changes.
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "-b", "main", "remote.git"]);
    let bare = root.path().join("remote.git");

    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            transport_url: Some(bare.to_string_lossy().into_owned()),
            ..TargetProfileOverrides::default()
        },
    )
    .unwrap();
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            repo: Some("example/other".to_string()),
            ..TargetProfileOverrides::default()
        },
    )
    .unwrap();

    assert_eq!(config.variables["target_repo"], "example/other");
    assert_eq!(
        config.variables[GIT_TRANSPORT_URL_VAR],
        bare.to_string_lossy()
    );
}

#[test]
fn a_rejected_override_leaves_the_configuration_untouched() {
    // Overrides are all-or-nothing: pairing a valid repository with an invalid
    // transport must not leave the repository changed.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(&mut config, &TargetProfileOverrides::default()).unwrap();
    let before = config.variables.clone();

    let result = apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            repo: Some("example/other".to_string()),
            issue: Some("999".to_string()),
            transport_url: Some("ftp://example.com/x.git".to_string()),
            ..TargetProfileOverrides::default()
        },
    );

    assert!(result.is_err(), "an invalid transport must be rejected");
    assert_eq!(
        config.variables, before,
        "no field may be applied when any field is invalid"
    );
}

#[test]
fn an_unusable_local_transport_is_rejected_before_any_mutation() {
    // A path that is not a repository would otherwise be accepted here and
    // fail only during push, after the workspace had been mutated.
    let root = tempfile::tempdir().unwrap();
    let not_a_repo = root.path().join("empty");
    std::fs::create_dir_all(&not_a_repo).unwrap();

    for bad in [
        format!("{}/does-not-exist", root.path().display()),
        not_a_repo.to_string_lossy().into_owned(),
        "https://".to_string(),
        "git@".to_string(),
        "file://host/path".to_string(),
    ] {
        let mut config = config_for("vybestack/llxprt-luther");
        let before = config.variables.clone();
        let result = apply_target_profile_overrides(
            &mut config,
            &TargetProfileOverrides {
                transport_url: Some(bad.clone()),
                ..TargetProfileOverrides::default()
            },
        );
        assert!(result.is_err(), "expected {bad:?} to be rejected");
        assert_eq!(config.variables, before, "{bad:?} must not mutate config");
    }
}

/// A workflow whose single shell step records the repository a GitHub call
/// would address and the transport Git would use.
///
/// Both values come from the runner's own context, so this observes what the
/// production runner propagates rather than what a test inserted by hand.
fn probe_workflow_toml(marker: &Path) -> String {
    format!(
        r#"
workflow_type_id = "llxprt-issue-fix-v1"
name = "transport probe"
initial_step = "probe"

[[steps]]
step_id = "probe"
step_type = "shell"
description = "record the repo and transport the runner supplied"

[steps.parameters]
command = """
printf 'repo=%s\ntransport=%s\n' '{{target_repo}}' '{{git_transport_url}}' > {}
"""

[[transitions]]
from = "probe"
to = "COMPLETE"
"#,
        marker.display()
    )
}

#[test]
fn the_runner_propagates_transport_and_keeps_api_identity_logical() {
    // Enters through the production path: resolved config -> WorkflowInstance
    // -> EngineRunner -> StepContext. Nothing sets git_transport_url by hand,
    // so this fails if the runner stops propagating it.
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "-b", "main", "remote.git"]);
    let bare = root.path().join("remote.git");
    let marker = root.path().join("probe.txt");

    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            transport_url: Some(bare.to_string_lossy().into_owned()),
            ..TargetProfileOverrides::default()
        },
    )
    .expect("transport override applies");

    let workflow = parse_workflow_type_toml(&probe_workflow_toml(&marker)).expect("probe parses");
    let instance = luther_workflow::engine::instance::WorkflowInstance::create(workflow, config);
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let mut runner = luther_workflow::engine::runner::EngineRunner::new(instance, registry)
        .expect("runner builds");
    runner.execute_step("probe").expect("probe step runs");

    let recorded = std::fs::read_to_string(&marker).expect("probe wrote its observations");
    assert!(
        recorded.contains("repo=vybestack/llxprt-luther"),
        "GitHub identity must stay logical: {recorded}"
    );
    assert!(
        recorded.contains(&format!("transport={}", bare.display())),
        "the runner must propagate the resolved transport: {recorded}"
    );
}

#[test]
fn a_repeatedly_resolved_config_still_tracks_a_repository_override() {
    // Resolution must be idempotent: re-resolving an already-resolved config
    // must not promote its derived transport to an explicit one, which would
    // freeze it against a later override.
    let mut config = config_for("vybestack/llxprt-luther");
    apply_target_profile_overrides(&mut config, &TargetProfileOverrides::default()).unwrap();
    apply_target_profile_overrides(&mut config, &TargetProfileOverrides::default()).unwrap();
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            repo: Some("example/other".to_string()),
            ..TargetProfileOverrides::default()
        },
    )
    .unwrap();

    assert_eq!(
        config.variables[GIT_TRANSPORT_URL_VAR], "https://github.com/example/other.git",
        "a derived transport must follow identity however often it was resolved"
    );
}

#[test]
fn re_resolving_a_profile_does_not_freeze_a_derived_transport() {
    // resolve_target_profile runs whenever a config is loaded. Running it over
    // an already-resolved config must not reclassify its derived transport as
    // an operator choice, which would pin it against a later override.
    let mut config = config_for("vybestack/llxprt-luther");
    // A profile section is required for resolution to do anything at all.
    config.target_profile = Some(luther_workflow::workflow::schema::TargetProfileConfig {
        identity: luther_workflow::workflow::schema::TargetIdentityConfig {
            repo: Some("vybestack/llxprt-luther".to_string()),
            ..Default::default()
        },
        ..Default::default()
    });
    luther_workflow::workflow::target_profile::resolve_target_profile(&mut config).unwrap();
    luther_workflow::workflow::target_profile::resolve_target_profile(&mut config).unwrap();
    apply_target_profile_overrides(
        &mut config,
        &TargetProfileOverrides {
            repo: Some("example/other".to_string()),
            ..TargetProfileOverrides::default()
        },
    )
    .unwrap();

    assert_eq!(
        config.variables[GIT_TRANSPORT_URL_VAR], "https://github.com/example/other.git",
        "re-resolution must not promote a derived transport to explicit"
    );
}

/// The interpreter must support the dialect the shipping workflows are written in.
///
/// Regression for a defect that CI caught and every local run missed: the
/// executors ran `sh`, which is bash on macOS but dash on Ubuntu, and dash
/// rejects `set -o pipefail` at line 1. Fifteen shipping steps -- including
/// commit_changes, push_changes, and create_pr -- aborted before their first
/// real command on Linux.
///
/// This asserts the behaviour rather than the interpreter's name, so it fails
/// on any platform where the configured interpreter cannot run the dialect.
#[test]
fn the_workflow_interpreter_supports_the_dialect_shipping_steps_use() {
    let probe = Command::new(luther_workflow::engine::executors::WORKFLOW_SHELL)
        .arg("-c")
        .arg("set -euo pipefail; printf ok")
        .output()
        .expect("the workflow interpreter must be available on PATH");

    assert!(
        probe.status.success(),
        "the workflow interpreter must accept `set -euo pipefail`: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    assert_eq!(stdout(&probe), "ok");
}

/// Every shipping command that opts into `pipefail` must be run by an
/// interpreter that supports it. Guards against a future step being added with
/// the dialect while the interpreter is downgraded back to `sh`.
#[test]
fn shipping_workflows_that_use_pipefail_are_run_by_a_capable_interpreter() {
    let workflow = shipped_workflow();
    let uses_pipefail: Vec<&str> = workflow
        .steps
        .iter()
        .filter(|step| {
            step.parameters
                .as_ref()
                .and_then(|p| p.get("command"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|command| command.contains("pipefail"))
        })
        .map(|step| step.step_id.as_str())
        .collect();

    assert!(
        !uses_pipefail.is_empty(),
        "expected the shipping workflow to exercise the dialect"
    );

    for step_id in uses_pipefail {
        let command = workflow
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .and_then(|step| step.parameters.as_ref())
            .and_then(|p| p.get("command"))
            .and_then(serde_json::Value::as_str)
            .expect("command present");
        let first_line = command.lines().next().unwrap_or_default();
        let probe = Command::new(luther_workflow::engine::executors::WORKFLOW_SHELL)
            .arg("-c")
            .arg(first_line)
            .output()
            .expect("interpreter available");
        assert!(
            probe.status.success(),
            "step {step_id} cannot start under the configured interpreter: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
    }
}
