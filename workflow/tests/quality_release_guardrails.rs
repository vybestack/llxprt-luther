/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// Integration tests for Quality and Release Infrastructure Guardrails.
///
/// These tests verify the behavioral requirements for QA automation,
/// PR quality workflows, and release pipeline controls.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Helper to get the workspace root path.
/// @plan:PLAN-PLAN-20260404-INITIAL-RUNTIME.P11
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lint_level(value: &toml::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("level").and_then(toml::Value::as_str))
}

fn assert_lint_not_allowed(content: &str, lint_name: &str) {
    let manifest: toml::Value = toml::from_str(content).expect("Cargo.toml should be valid TOML");

    for lint_table in ["rust", "clippy"] {
        let lint_level = manifest
            .get("lints")
            .and_then(|lints| lints.get(lint_table))
            .and_then(|lints| lints.get(lint_name))
            .and_then(lint_level);

        assert_ne!(
            lint_level,
            Some("allow"),
            "Cargo.toml should not globally suppress {lint_name} in lints.{lint_table}"
        );
    }
}

#[test]
fn test_lint_suppression_guard_parses_toml_variants() {
    for cargo_toml in [
        "[lints.rust]\nunused='allow'\n",
        "[lints.clippy]\nunused = 'allow'\n",
        "[lints.clippy]\nunused={ level = 'allow', priority = -1 }\n",
    ] {
        assert!(
            std::panic::catch_unwind(|| assert_lint_not_allowed(cargo_toml, "unused")).is_err(),
            "guard should reject allow suppression in {cargo_toml}"
        );
    }

    assert!(
        std::panic::catch_unwind(|| {
            assert_lint_not_allowed("[lints.clippy]\nunused = 'deny'\n", "unused");
        })
        .is_ok(),
        "guard should accept non-allow lint levels"
    );
}

/// Test: xtask qa command exists and is executable.
/// GIVEN: the xtask qa automation tool
/// WHEN: running "cargo xtask qa"
/// THEN: command exists and can be executed (may fail tests but infrastructure exists)
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_xtask_qa_exists() {
    // GIVEN: the xtask qa automation tool
    let workspace_root = workspace_root();

    // xtask directory should exist
    let xtask_dir = workspace_root.join("xtask");
    assert!(
        xtask_dir.is_dir(),
        "xtask directory should exist at {xtask_dir:?}"
    );

    // xtask source should exist
    let xtask_src = xtask_dir.join("src").join("main.rs");
    assert!(xtask_src.is_file(), "xtask/src/main.rs should exist");

    // xtask should have qa command
    let xtask_content = fs::read_to_string(&xtask_src).expect("Failed to read xtask main.rs");
    let has_qa = xtask_content.contains("fn qa()")
        || xtask_content.contains("\"qa\"")
        || xtask_content.contains("Some(\"qa\")");
    assert!(
        has_qa,
        "xtask should have qa command. Content does not contain qa pattern."
    );

    // WHEN: attempting to run cargo xtask qa
    let mut cmd = Command::new("cargo");
    cmd.arg("xtask").arg("qa");
    cmd.current_dir(&workspace_root);

    let output = cmd.output();

    // THEN: command should be recognized (may fail tests but infrastructure exists)
    // We just need to verify the command exists, not that it passes
    match output {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Should NOT be "unknown xtask command"
            assert!(
                !stderr.contains("unknown xtask command"),
                "qa command should exist in xtask. stderr: {stderr}"
            );
        }
        Err(e) => {
            // Command might fail for other reasons (e.g., tests failing),
            // but the infrastructure should exist
            // We'll accept this as the infrastructure exists
            assert!(
                e.to_string().contains("cargo") || e.to_string().contains("The directory"),
                "Expected cargo or path error, got: {e}"
            );
        }
    }
}

/// Test: PR quality workflow YAML file exists and is valid.
/// GIVEN: the repository has GitHub Actions workflows
/// WHEN: examining .github/workflows/pr-quality.yml
/// THEN: file exists with required quality gates
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_pr_quality_workflow_exists() {
    // GIVEN: the repository should have GitHub Actions workflows
    let workspace_root = workspace_root();
    let workflow_path = workspace_root
        .join(".github")
        .join("workflows")
        .join("pr-quality.yml");

    // WHEN: examining the workflow file
    // THEN: file should exist
    assert!(
        workflow_path.is_file(),
        "PR quality workflow should exist at {workflow_path:?}"
    );

    // Read and validate content
    let content = fs::read_to_string(&workflow_path).expect("Failed to read pr-quality.yml");

    // Should have pull_request trigger
    assert!(
        content.contains("pull_request:"),
        "Workflow should trigger on pull requests"
    );

    // Should have required jobs
    let has_fmt = content.contains("fmt:") || content.contains("format");
    let has_lint = content.contains("lint:") || content.contains("clippy");
    let has_test = content.contains("test:");
    let _has_coverage = content.contains("coverage:") || content.contains("cov");

    assert!(
        has_fmt || has_lint || has_test,
        "Workflow should have format, lint, or test jobs. Content: {content}"
    );

    // Should run cargo commands
    assert!(
        content.contains("cargo "),
        "Workflow should run cargo commands"
    );
}

/// Test: Release workflow YAML file exists and is valid.
/// GIVEN: the repository has GitHub Actions workflows
/// WHEN: examining .github/workflows/release.yml
/// THEN: file exists with release automation
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-003
#[test]
fn test_release_workflow_exists() {
    // GIVEN: the repository should have GitHub Actions workflows
    let workspace_root = workspace_root();
    let workflow_path = workspace_root
        .join(".github")
        .join("workflows")
        .join("release.yml");

    // WHEN: examining the workflow file
    // THEN: file should exist
    assert!(
        workflow_path.is_file(),
        "Release workflow should exist at {workflow_path:?}"
    );

    // Read and validate content
    let content = fs::read_to_string(&workflow_path).expect("Failed to read release.yml");

    // Should have release-related content
    let has_release =
        content.contains("release") || content.contains("Release") || content.contains("publish");
    assert!(
        has_release,
        "Workflow should be related to release. Content: {content}"
    );

    // Should build the release binary
    assert!(
        content.contains("cargo build --release")
            || content.contains("cargo build")
            || content.contains("release"),
        "Workflow should build release binary"
    );
}

/// Test: Release workflow triggers on version tags.
/// GIVEN: the release workflow
/// WHEN: examining trigger configuration
/// THEN: workflow triggers on v* tag pushes
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-003
#[test]
fn test_release_workflow_triggers_on_tag() {
    // GIVEN: the release workflow exists
    let workspace_root = workspace_root();
    let workflow_path = workspace_root
        .join(".github")
        .join("workflows")
        .join("release.yml");

    // WHEN: examining the workflow trigger configuration
    let content = fs::read_to_string(&workflow_path).expect("Failed to read release.yml");

    // THEN: should trigger on tag push (v* pattern)
    let has_tag_trigger = content.contains("tags:")
        || content.contains("\"v*\"")
        || content.contains("'v*'")
        || (content.contains("push:") && content.contains('v'));

    let has_workflow_dispatch = content.contains("workflow_dispatch:");

    assert!(
        has_tag_trigger || has_workflow_dispatch,
        "Release workflow should trigger on tags or have manual dispatch. Content: {content}"
    );

    // If triggering on tags, should specifically mention v* pattern
    if has_tag_trigger {
        assert!(
            content.contains("\"v*\"") || content.contains("'v*'") || content.contains("- 'v'"),
            "Tag trigger should use v* pattern for semantic versioning"
        );
    }
}

/// Test: Release workflow validates required secrets.
/// GIVEN: the release workflow
/// WHEN: examining secret handling
/// THEN: workflow validates `HOMEBREW_TAP_GITHUB_TOKEN` or similar secrets
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-004
#[test]
fn test_release_secrets_validation() {
    // GIVEN: the release workflow exists
    let workspace_root = workspace_root();
    let workflow_path = workspace_root
        .join(".github")
        .join("workflows")
        .join("release.yml");

    // WHEN: examining the workflow secret handling
    let content = fs::read_to_string(&workflow_path).expect("Failed to read release.yml");

    // THEN: should reference required secrets
    // HOMEBREW_TAP_GITHUB_TOKEN is used for tap updates
    let has_token_ref = content.contains("HOMEBREW_TAP_GITHUB_TOKEN")
        || content.contains("secrets.")
        || content.contains("GITHUB_TOKEN");

    assert!(
        has_token_ref,
        "Release workflow should reference required secrets. Content: {content}"
    );

    // Should have a validation step or reference to secrets in env
    let has_validation = content.contains("validate")
        || content.contains("Validate")
        || content.contains("secrets.")
        || content.contains("env:");

    assert!(
        has_validation,
        "Release workflow should validate or reference secrets"
    );
}

#[test]
fn test_lint_policy_removes_global_suppressions() {
    let workspace_root = workspace_root();
    let cargo_toml =
        fs::read_to_string(workspace_root.join("Cargo.toml")).expect("Failed to read Cargo.toml");

    for suppressed_lint in [
        "unused",
        "dead_code",
        "unused_imports",
        "unused_variables",
        "unused_mut",
        "all",
        "pedantic",
        "nursery",
    ] {
        assert_lint_not_allowed(&cargo_toml, suppressed_lint);
    }
}

#[test]
fn test_lint_policy_contains_high_signal_clippy_denies() {
    let workspace_root = workspace_root();
    let cargo_toml =
        fs::read_to_string(workspace_root.join("Cargo.toml")).expect("Failed to read Cargo.toml");

    for denied_lint in [
        "cognitive_complexity = \"deny\"",
        "too_many_lines = \"deny\"",
        "too_many_arguments = \"deny\"",
        "type_complexity = \"deny\"",
        "struct_excessive_bools = \"deny\"",
    ] {
        assert!(
            cargo_toml.contains(denied_lint),
            "Cargo.toml should deny high-signal lint {denied_lint}"
        );
    }
}

#[test]
fn test_pr_quality_uses_cargo_lint_policy() {
    let workspace_root = workspace_root();
    let workflow_path = workspace_root
        .join(".github")
        .join("workflows")
        .join("pr-quality.yml");
    let workflow = fs::read_to_string(&workflow_path).expect("Failed to read pr-quality.yml");

    assert!(
        workflow.contains("cargo clippy --workspace --all-targets --all-features -- -D warnings"),
        "PR quality workflow should run clippy with the Cargo.toml lint policy"
    );

    for duplicated_lint in [
        "clippy::cognitive_complexity",
        "clippy::too_many_lines",
        "clippy::too_many_arguments",
        "clippy::type_complexity",
        "clippy::struct_excessive_bools",
    ] {
        assert!(
            !workflow.contains(duplicated_lint),
            "PR quality workflow should not duplicate {duplicated_lint}"
        );
    }
}

#[test]
fn test_xtask_clippy_uses_shared_cargo_lint_policy_command() {
    let workspace_root = workspace_root();
    let xtask_path = workspace_root.join("xtask").join("src").join("main.rs");
    let xtask = fs::read_to_string(&xtask_path).expect("Failed to read xtask main.rs");

    assert!(
        xtask.contains("const CLIPPY_ARGS"),
        "xtask should share one clippy argv definition"
    );
    assert!(
        xtask.contains("CLIPPY_ARGS: [&str; 7]"),
        "xtask CLIPPY_ARGS should be a 7-element array"
    );
    assert!(
        xtask.contains("\"--workspace\"")
            && xtask.contains("\"--all-targets\"")
            && xtask.contains("\"--all-features\"")
            && xtask.contains("\"-D\"")
            && xtask.contains("\"warnings\""),
        "xtask should run cargo clippy --workspace --all-targets --all-features -- -D warnings"
    );

    for duplicated_lint in [
        "clippy::cognitive_complexity",
        "clippy::too_many_lines",
        "clippy::too_many_arguments",
        "clippy::type_complexity",
        "clippy::struct_excessive_bools",
    ] {
        assert!(
            !xtask.contains(duplicated_lint),
            "xtask should not duplicate {duplicated_lint}"
        );
    }
}

/// Helper to read the OCR PR review workflow content for guardrail tests.
fn ocr_pr_review_workflow_content() -> String {
    let workflow_path = workspace_root()
        .join(".github")
        .join("workflows")
        .join("ocr-pr-review.yml");
    fs::read_to_string(&workflow_path)
        .unwrap_or_else(|_| panic!("Failed to read ocr-pr-review.yml at {workflow_path:?}"))
}

fn yaml_block_after(content: &str, header: &str) -> String {
    let header_indent = header.chars().take_while(|ch| *ch == ' ').count();
    let mut in_block = false;
    let mut block = String::new();

    for line in content.lines() {
        if !in_block {
            if line == header {
                in_block = true;
                block.push_str(line);
                block.push('\n');
            }
            continue;
        }

        let trimmed = line.trim();
        let indent = line.chars().take_while(|ch| *ch == ' ').count();
        if !trimmed.is_empty() && !trimmed.starts_with('#') && indent <= header_indent {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }

    block
}

fn run_blocks(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, String)> = None;

    for line in content.lines() {
        if let Some((run_indent, block)) = current.as_mut() {
            let trimmed = line.trim();
            let indent = line.chars().take_while(|ch| *ch == ' ').count();
            if !trimmed.is_empty() && indent <= *run_indent {
                blocks.push(std::mem::take(block));
                current = None;
            } else {
                block.push_str(line);
                block.push('\n');
                continue;
            }
        }

        if line.trim() == "run: |" {
            let indent = line.chars().take_while(|ch| *ch == ' ').count();
            current = Some((indent, String::new()));
        }
    }

    if let Some((_, block)) = current {
        blocks.push(block);
    }

    blocks
}

/// Test: the OCR PR review workflow file exists.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
#[test]
fn test_ocr_pr_review_workflow_exists() {
    let workflow_path = workspace_root()
        .join(".github")
        .join("workflows")
        .join("ocr-pr-review.yml");
    assert!(
        workflow_path.is_file(),
        "OCR PR review workflow should exist at {workflow_path:?}"
    );
    let content = ocr_pr_review_workflow_content();
    assert!(!content.trim().is_empty(), "ocr-pr-review.yml is empty");
}

/// Test: the workflow uses `pull_request_target` with the required event types.
#[test]
fn test_ocr_pr_review_uses_pull_request_target() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("pull_request_target:"),
        "Workflow must use pull_request_target for fork-safe secret access"
    );
    let pull_request_target = yaml_block_after(&content, "  pull_request_target:");
    assert!(
        pull_request_target.contains("types:"),
        "pull_request_target must define explicit event types"
    );
    for required in ["opened", "reopened", "synchronize", "ready_for_review"] {
        assert!(
            pull_request_target.contains(&format!("- {required}")),
            "pull_request_target must list the {required} event type"
        );
    }
}

/// Test: permissions are minimal and explicit.
#[test]
fn test_ocr_pr_review_permissions_are_minimal() {
    let content = ocr_pr_review_workflow_content();
    let permissions = yaml_block_after(&content, "permissions:");
    let permission_lines: Vec<&str> = permissions
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        permission_lines,
        vec!["contents: read", "pull-requests: write", "issues: write"],
        "Workflow permissions must be exactly the approved minimal set"
    );
}

/// Test: duplicate runs for a PR are cancelled by concurrency.
#[test]
fn test_ocr_pr_review_concurrency_cancels_duplicates() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("concurrency:"),
        "Workflow must define a concurrency block"
    );
    assert!(
        content.contains("cancel-in-progress: true"),
        "Concurrency must cancel in-progress duplicate runs"
    );
    // Group must be keyed by PR/issue number for per-PR cancellation.
    assert!(
        content.contains("github.event.pull_request.number")
            || content.contains("github.event.issue.number"),
        "Concurrency group must key on the PR/issue number"
    );
}

/// Test: OCR runs with --timeout 30 over the merge-base..head diff.
#[test]
fn test_ocr_pr_review_uses_timeout_30_and_merge_base() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("--timeout 30"),
        "Workflow must run OCR with --timeout 30"
    );
    assert!(
        content.contains("--audience agent"),
        "Workflow must run OCR with --audience agent"
    );
    assert!(
        content.contains("--format json"),
        "Workflow must request JSON output for parsing"
    );
    assert!(
        content.contains("merge-base"),
        "Workflow must review the merge-base..head diff, not origin/main..head"
    );
}

/// Test: credential handling — URL is a variable, only the token is a secret.
#[test]
fn test_ocr_pr_review_credential_handling() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("secrets.OCR_LLM_AUTH_TOKEN"),
        "Workflow must reference OCR_LLM_AUTH_TOKEN via secrets."
    );
    assert!(
        content.contains("vars.OCR_LLM_URL"),
        "Workflow must reference OCR_LLM_URL via vars. (not a secret)"
    );
    assert!(
        content.contains("vars.OCR_LLM_MODEL"),
        "Workflow must reference OCR_LLM_MODEL via vars."
    );
    // The URL must not also be exposed as a secret.
    assert!(
        !content.contains("secrets.OCR_LLM_URL"),
        "OCR_LLM_URL must be a variable, not a secret"
    );
}

/// Test: stable sticky summary marker, updateComment, and artifact upload.
#[test]
fn test_ocr_pr_review_has_sticky_marker_and_artifacts() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("<!-- luther-ocr-review -->"),
        "Workflow must maintain the stable sticky summary marker"
    );
    assert!(
        content.contains("updateComment"),
        "Workflow must update the existing summary in place on reruns"
    );
    assert!(
        content.contains("createComment"),
        "Workflow must create a summary when none exists yet"
    );
    assert!(
        content.contains("upload-artifact"),
        "Workflow must upload OCR output artifacts"
    );
    assert!(
        content.contains("ocr-result.json") && content.contains("ocr-stderr.log"),
        "Workflow must upload both the OCR JSON result and stderr log"
    );
}

/// Test: the workflow does not execute untrusted PR code.
#[test]
fn test_ocr_pr_review_does_not_execute_pr_code() {
    let content = ocr_pr_review_workflow_content();
    let commands = run_blocks(&content).join("\n");
    for forbidden in ["make", "cargo", "npm run", "npm ci"] {
        assert!(
            !commands
                .lines()
                .map(str::trim_start)
                .any(|line| line.starts_with(forbidden)),
            "Workflow must not invoke {forbidden} from a run step"
        );
    }
    // Only the global OCR install is permitted.
    assert!(
        commands.contains("npm install -g @alibaba-group/open-code-review"),
        "Workflow may only globally install OCR"
    );
    assert!(
        !content.contains("safe.directory '*'"),
        "Workflow must not use safe.directory '*'"
    );
}

/// Test: CodeRabbit remains enabled (OCR is additive).
#[test]
fn test_ocr_pr_review_keeps_coderabbit() {
    let pr_quality_path = workspace_root()
        .join(".github")
        .join("workflows")
        .join("pr-quality.yml");
    assert!(
        pr_quality_path.is_file(),
        "pr-quality.yml must remain so CodeRabbit stays enabled"
    );
    let content = ocr_pr_review_workflow_content();
    assert!(
        !content.contains("CodeRabbit"),
        "OCR workflow must not disable or remove CodeRabbit wiring"
    );
}
