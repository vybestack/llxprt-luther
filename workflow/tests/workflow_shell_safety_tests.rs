/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// Workflow-level shell-safety coverage for production and fixture PR follow-up workflows.
use std::path::{Path, PathBuf};

use luther_workflow::workflow::{parse_workflow_type_json, parse_workflow_type_toml, WorkflowType};
use serde_json::Value;

#[derive(Debug)]
struct WorkflowCommand {
    path: PathBuf,
    step_id: String,
    command: String,
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workflow_paths() -> Vec<PathBuf> {
    let root = project_root();
    let mut paths = vec![
        root.join("config/workflows/llxprt-issue-fix-v1.toml"),
        root.join("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml"),
        root.join("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json"),
    ];

    let invalid_dir = root.join("tests/fixtures/workflows/invalid");
    for entry in std::fs::read_dir(&invalid_dir).expect("read invalid workflow fixtures") {
        let path = entry.expect("read invalid fixture entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn load_workflow(path: &Path) -> WorkflowType {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read workflow {}: {err}", path.display()));
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("toml") => parse_workflow_type_toml(&content)
            .unwrap_or_else(|err| panic!("parse TOML workflow {}: {err}", path.display())),
        Some("json") => parse_workflow_type_json(&content)
            .unwrap_or_else(|err| panic!("parse JSON workflow {}: {err}", path.display())),
        extension => panic!(
            "unsupported workflow extension {extension:?} for {}",
            path.display()
        ),
    }
}

fn shell_commands() -> Vec<WorkflowCommand> {
    let mut commands = Vec::new();
    for path in workflow_paths() {
        let workflow = load_workflow(&path);
        for step in workflow.steps {
            if step.step_type != "shell" {
                continue;
            }
            let Some(parameters) = step.parameters else {
                continue;
            };
            if let Some(command) = parameters.get("command").and_then(Value::as_str) {
                commands.push(WorkflowCommand {
                    path: path.clone(),
                    step_id: step.step_id,
                    command: command.to_string(),
                });
            }
        }
    }
    commands
}

fn has_unquoted_body_argument(command: &str) -> bool {
    command.contains("gh issue comment")
        && (command.contains(" --body \"")
            || command.contains(" --body '")
            || command.contains(" --body "))
        && !command.contains("--body-file")
}

fn assert_no_dangerous_shell_expansion(command: &WorkflowCommand, needle: &str) {
    assert!(
        !command.command.contains(needle),
        "workflow {} step {} contains dangerous shell metacharacter fixture text {needle:?}:\n{}",
        command.path.display(),
        command.step_id,
        command.command
    );
}

fn is_shell_assignment(line: &str) -> bool {
    let Some((name, _value)) = line.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[test]
fn production_and_fixture_workflows_use_safe_body_handling() {
    let commands = shell_commands();
    assert!(
        !commands.is_empty(),
        "workflow shell command audit must inspect production and fixture commands"
    );

    let body_file_commands: Vec<_> = commands
        .iter()
        .filter(|command| command.command.contains("--body-file"))
        .collect();
    assert!(
        body_file_commands.len() >= 2,
        "production and fixture workflows must include safe body-file gh usage: {body_file_commands:#?}"
    );

    for command in &commands {
        assert!(
            !has_unquoted_body_argument(&command.command),
            "workflow {} step {} must not use gh issue comment --body in a shell command; use --body-file/API input instead:\n{}",
            command.path.display(),
            command.step_id,
            command.command
        );
    }

    let abandon_commands: Vec<_> = commands
        .iter()
        .filter(|command| command.step_id == "abandon_and_log")
        .collect();
    assert!(
        !abandon_commands.is_empty(),
        "abandon_and_log commands must be present in production and fixture workflows"
    );
    for command in abandon_commands {
        assert!(
            command.command.contains("mktemp")
                && command.command.contains("printf '%s")
                && command.command.contains("gh issue comment")
                && command.command.contains("--body-file"),
            "workflow {} abandon_and_log must write the comment body to a temporary file and pass --body-file:\n{}",
            command.path.display(),
            command.command
        );
    }
}

#[test]
fn shared_shell_commands_do_not_embed_target_bootstrap_or_guidance() {
    for command in shell_commands() {
        assert!(
            !command.command.contains("npm ci") && !command.command.contains("node_modules"),
            "target bootstrap belongs in command_manifest argv, not shared shell command {}:{}\n{}",
            command.path.display(),
            command.step_id,
            command.command
        );
        assert!(
            !command.command.contains("target_guidance_")
                && !command.command.contains("target_bootstrap_command_group"),
            "target guidance/bootstrap variables must not be interpolated into shell commands {}:{}\n{}",
            command.path.display(),
            command.step_id,
            command.command
        );
    }
}

#[test]
fn shared_workflow_prompts_keep_forbidden_actions_in_target_guidance() {
    let root = project_root();
    let workflow_text =
        std::fs::read_to_string(root.join("config/workflows/llxprt-issue-fix-v1.toml"))
            .expect("read production workflow");

    for forbidden_text in ["package lockfiles", "generated notice files", "NOTICES.txt"] {
        assert!(
            !workflow_text.contains(forbidden_text),
            "repository-specific forbidden-action text belongs in target config guidance, not shared workflow TOML: {forbidden_text}"
        );
    }
    assert!(
        workflow_text.contains("Forbidden actions: {target_guidance_forbidden_actions}"),
        "shared workflow should inject target-specific forbidden-action guidance"
    );
}

#[test]
fn coderabbit_text_metacharacters_cannot_execute() {
    let commands = shell_commands();
    assert!(
        commands
            .iter()
            .any(|command| command.command.contains("--body-file")),
        "safe body-file command paths must be present before adversarial text audit"
    );

    for command in &commands {
        for needle in [
            "coderabbit-pwned",
            "$(touch",
            "`touch",
            "<<LUTHER",
            "LUTHER_EOF",
            "; touch",
            "&& touch",
            "| touch",
        ] {
            assert_no_dangerous_shell_expansion(command, needle);
        }
    }

    let marker_commands: Vec<_> = commands
        .iter()
        .filter(|command| command.command.contains("gh issue comment"))
        .collect();
    assert!(
        !marker_commands.is_empty(),
        "the audit must exercise gh issue comment commands"
    );
    for command in marker_commands {
        assert!(
            command.command.contains("--body-file") && !command.command.contains(" --body "),
            "gh issue comment commands must carry dynamic text by file, not shell-interpolated body text:\n{}",
            command.command
        );
    }
}

#[test]
fn static_command_allowlist_is_machine_checked() {
    let commands = shell_commands();
    assert!(
        !commands.is_empty(),
        "static command allowlist must inspect workflow shell commands"
    );

    let allowed_prefixes = [
        "set ",
        "#",
        "ISSUE_NUM=",
        "case ",
        "\"{\"*)",
        "\"\")",
        "esac",
        "ABANDON_BODY_FILE=",
        "trap ",
        "printf ",
        "printf continuation",
        "gh issue comment ",
        "gh issue edit ",
        "gh pr create ",
        "gh issue view ",
        "gh issue list ",
        "gh pr list ",
        "gh api ",
        "jq ",
        "git clone ",
        "git fetch ",
        "git checkout ",
        "git reset ",
        "git restore ",
        "git add ",
        "git diff ",
        "git commit ",
        "git push ",
        "mkdir ",
        "rm ",
        "cd ",
        "cat ",
        "echo ",
        "exit ",
        "if ",
        "then",
        "fi",
        "else",
    ];

    for command in &commands {
        for raw_line in command.command.lines() {
            let mut line = raw_line.trim();
            if line.contains("Luther abandoning this issue: workflow failed at step")
                && line.contains("ABANDON_BODY_FILE")
            {
                line = "printf continuation";
            }
            if line.is_empty()
                || line == "do"
                || line == "done"
                || line == "fi"
                || line == "then"
                || line == "else"
                || line == "continue"
                || line == ";;"
                || line.ends_with("|\"\")")
                || line.starts_with("' ")
                || line.starts_with("break ")
                || line.starts_with("for ")
                || line.starts_with("while ")
                || line.starts_with("elif ")
                || line.starts_with("[ ")
                || line.starts_with(']')
                || line.contains("$(gh ")
                || line.contains("$(echo ")
                || line.contains("$(seq ")
                || line.contains("| jq ")
                || line.contains("| sort ")
                || is_shell_assignment(line)
            {
                continue;
            }
            assert!(
                allowed_prefixes.iter().any(|prefix| line.starts_with(prefix)),
                "workflow {} step {} contains command line outside static allowlist: {line:?}\nfull command:\n{}",
                command.path.display(),
                command.step_id,
                command.command
            );
        }
    }
}
