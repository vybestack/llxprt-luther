use luther_workflow::workflow::config_loader::{
    command_manifest_entry, parse_workflow_config_json, parse_workflow_config_toml,
};
use luther_workflow::workflow::DiffPathNormalization;

fn config_with_manifest(manifest: &str) -> String {
    format!(
        r#"
config_id = "manifest-test"
workflow_type_id = "llxprt-issue-fix-v1"

[runtime]
timeout_seconds = 3600
max_retries = 3
parallel_steps = 1
log_level = "info"

[repository]
workspace_strategy = "temp"
branch_template = "test-{{issue_number}}"
base_branch = "main"
workspace_root = "/tmp/luther"

[guards]
max_iterations = 3
max_file_changes = 50
max_tokens = 100000
max_cost = 100.0

[variables]
target_repo = "owner/repo"
repository_owner = "owner"
repository_name = "repo"
work_dir = "/tmp/luther"
artifact_dir = "/tmp/luther-artifacts"
primary_issue_number = "1"
target_ecosystem_name = "Rust"

{manifest}
"#
    )
}

#[test]
fn manifest_parses_and_supports_lookup() {
    let toml = config_with_manifest(
        r#"
[[command_manifest.commands]]
id = "lint"
argv = ["cargo", "fmt", "--check"]
working_directory = "workflow"
timeout_seconds = 60
acceptable_exit_codes = [0]
failure_outcome = "fixable"

[command_manifest.commands.env]
RUST_BACKTRACE = "1"

[command_manifest.commands.stdout]
required_patterns = ["Finished|Checking"]
forbidden_patterns = ["panic"]

[command_manifest.groups]
local = ["lint"]
"#,
    );
    let config = parse_workflow_config_toml(&toml).expect("valid manifest");
    let manifest = config.command_manifest.expect("manifest present");
    let entry = command_manifest_entry(&manifest, "lint").expect("lookup lint");
    assert_eq!(entry.argv, vec!["cargo", "fmt", "--check"]);
}

#[test]
fn manifest_rejects_shell_strings_empty_argv_and_duplicates() {
    let shell = config_with_manifest(
        r#"
[[command_manifest.commands]]
id = "lint"
command = "cargo fmt --check"
argv = ["cargo"]
"#,
    );
    assert!(parse_workflow_config_toml(&shell).is_err());

    let duplicate = config_with_manifest(
        r#"
[[command_manifest.commands]]
id = "lint"
argv = ["cargo"]

[[command_manifest.commands]]
id = "lint"
argv = ["cargo"]
"#,
    );
    let err = parse_workflow_config_toml(&duplicate).expect_err("duplicate rejected");
    assert!(err.message.contains("duplicate"));

    let empty = config_with_manifest(
        r#"
[[command_manifest.commands]]
id = "lint"
argv = []
"#,
    );
    let err = parse_workflow_config_toml(&empty).expect_err("empty argv rejected");
    assert!(err.message.contains("argv"));
}

#[test]
fn manifest_validates_env_regex_timeout_retry_and_groups() {
    for manifest in [
        r#"
[[command_manifest.commands]]
id = "bad-env"
argv = ["printenv"]
[command_manifest.commands.env]
Path = "bad"
"#,
        r#"
[[command_manifest.commands]]
id = "bad-regex"
argv = ["echo", "x"]
[command_manifest.commands.stdout]
required_patterns = ["["]
"#,
        r#"
[[command_manifest.commands]]
id = "bad-timeout"
argv = ["true"]
timeout_seconds = 0
"#,
        r#"
[[command_manifest.commands]]
id = "bad-retry"
argv = ["true"]
[command_manifest.commands.retry]
max_attempts = 2
"#,
        r#"
[[command_manifest.commands]]
id = "lint"
argv = ["true"]
[command_manifest.groups]
local = ["missing"]
"#,
    ] {
        assert!(parse_workflow_config_toml(&config_with_manifest(manifest)).is_err());
    }
}

#[test]
fn manifest_validates_conditional_and_removal_paths() {
    for field in [
        "run_if_missing_any",
        "run_if_present_all",
        "remove_before_run",
    ] {
        let manifest = format!(
            r#"
[[command_manifest.commands]]
id = "bad-path"
argv = ["true"]
{field} = ["../outside"]
"#
        );
        let err = parse_workflow_config_toml(&config_with_manifest(&manifest))
            .expect_err("escaping manifest path rejected");
        assert!(err.message.contains("must stay under work_dir"));
    }
}

#[test]
fn manifest_parses_json_schema() {
    let json = r#"{
        "config_id": "manifest-json",
        "workflow_type_id": "llxprt-issue-fix-v1",
        "runtime": { "timeout_seconds": 3600, "max_retries": 3, "parallel_steps": 1, "log_level": "info" },
        "repository": { "workspace_strategy": "temp", "branch_template": "test-{issue_number}", "base_branch": "main", "workspace_root": "/tmp/luther" },
        "guard_limits": { "max_iterations": 3, "max_file_changes": 50, "max_tokens": 100000, "max_cost": 100.0 },
        "variables": { "target_repo": "owner/repo", "repository_owner": "owner", "repository_name": "repo", "work_dir": "/tmp/luther", "artifact_dir": "/tmp/luther-artifacts", "primary_issue_number": "1", "target_ecosystem_name": "Rust" },
        "command_manifest": {
            "commands": [{ "id": "test", "argv": ["cargo", "test"], "acceptable_exit_codes": [0] }],
            "groups": { "local": ["test"] }
        }
    }"#;
    let config = parse_workflow_config_json(json).expect("json manifest parses");
    assert_eq!(config.command_manifest.unwrap().commands[0].id, "test");
}

#[test]
fn repository_path_fields_parse_and_validate() {
    let toml = config_with_manifest("").replace(
        "workspace_root = \"/tmp/luther\"",
        "workspace_root = \"/tmp/luther\"\nproject_subdir = \"workflow\"\nartifact_path_base = \".\"\ndiff_path_base = \"workflow\"\ndiff_path_normalization = \"base_relative\"",
    );
    let config = parse_workflow_config_toml(&toml).expect("path fields parse");
    assert_eq!(config.repo.project_subdir.as_deref(), Some("workflow"));
    assert_eq!(config.repo.artifact_path_base.as_deref(), Some("."));
    assert_eq!(config.repo.diff_path_base.as_deref(), Some("workflow"));
    assert_eq!(
        config.repo.diff_path_normalization,
        DiffPathNormalization::BaseRelative
    );

    let invalid = config_with_manifest("").replace(
        "workspace_root = \"/tmp/luther\"",
        "workspace_root = \"/tmp/luther\"\nproject_subdir = \"../outside\"",
    );
    assert!(parse_workflow_config_toml(&invalid).is_err());

    let invalid_base_relative = config_with_manifest("").replace(
        "workspace_root = \"/tmp/luther\"",
        "workspace_root = \"/tmp/luther\"\ndiff_path_normalization = \"base_relative\"",
    );
    assert!(parse_workflow_config_toml(&invalid_base_relative).is_err());
}

fn config_with_target_profile(manifest: &str, groups: &str) -> String {
    config_with_manifest(&format!(
        r#"
[target_profile.identity]
repo = "owner/repo"
base_branch = "main"

[target_profile.paths]
work_dir = "/tmp/luther"
artifact_dir = "/tmp/luther-artifacts"

[target_profile.issue_conventions]
assignee = "bot"
ok_label = "OK"
luther_label = "Working"

[target_profile.command_groups]
{groups}

[target_profile.prompt_guidance]
ecosystem_name = "Rust"

{manifest}
"#
    ))
}

#[test]
fn target_profile_command_groups_resolve_to_manifest_groups() {
    let config = parse_workflow_config_toml(&config_with_target_profile(
        r#"
[[command_manifest.commands]]
id = "lint"
argv = ["cargo", "fmt", "--check"]

[command_manifest.groups]
local = ["lint"]
post_pr = ["lint"]
"#,
        "local = \"local\"\npost_pr = \"post_pr\"",
    ))
    .expect("target profile command groups resolve");

    let profile = config.target_profile.expect("target profile present");
    assert_eq!(
        profile.command_groups.get("local").map(String::as_str),
        Some("local")
    );
    assert_eq!(
        config.variables.get("command_manifest_group_local"),
        Some(&"local".to_string())
    );
}

#[test]
fn target_profile_rejects_unknown_manifest_group_reference() {
    let err = parse_workflow_config_toml(&config_with_target_profile(
        r#"
[[command_manifest.commands]]
id = "lint"
argv = ["cargo", "fmt", "--check"]

[command_manifest.groups]
local = ["lint"]
"#,
        "local = \"missing\"",
    ))
    .expect_err("unknown target profile command group rejected");

    assert!(err.message.contains("unknown manifest group"));
}
