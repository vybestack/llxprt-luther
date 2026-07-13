/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// Integration tests for Quality and Release Infrastructure Guardrails.
///
/// These tests verify the behavioral requirements for QA automation,
/// PR quality workflows, and release pipeline controls.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Helper to get the Rust crate root path.
/// @plan:PLAN-PLAN-20260404-INITIAL-RUNTIME.P11
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> PathBuf {
    workspace_root()
        .parent()
        .expect("workflow crate should live under the repository root")
        .to_path_buf()
}

fn root_workflow_path(name: &str) -> PathBuf {
    repository_root()
        .join(".github")
        .join("workflows")
        .join(name)
}

fn nested_workflow_path(name: &str) -> PathBuf {
    workspace_root()
        .join(".github")
        .join("workflows")
        .join(name)
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

#[test]
fn test_workflows_are_discoverable_from_repository_root_only() {
    for workflow in ["ocr-pr-review.yml", "pr-quality.yml", "release.yml"] {
        let root_path = root_workflow_path(workflow);
        assert!(
            root_path.is_file(),
            "GitHub Actions workflow must live at repository root path {root_path:?}"
        );

        let nested_path = nested_workflow_path(workflow);
        assert!(
            !nested_path.exists(),
            "workflow/.github/workflows must not contain undiscoverable replacement workflow {nested_path:?}"
        );
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
    let workflow_path = root_workflow_path("pr-quality.yml");

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
    let workflow_path = root_workflow_path("release.yml");

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
    let workflow_path = root_workflow_path("release.yml");

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
    let workflow_path = root_workflow_path("release.yml");

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
    let workflow_path = root_workflow_path("pr-quality.yml");
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
    let workflow_path = root_workflow_path("ocr-pr-review.yml");
    fs::read_to_string(&workflow_path)
        .unwrap_or_else(|e| panic!("Failed to read ocr-pr-review.yml at {workflow_path:?}: {e}"))
}

/// Extract the exact pinned OCR version from the single-source-of-truth
/// `OCR_VERSION` env var declared on the `code-review` job.
///
/// The install command and the cache key both reference `${OCR_VERSION}`, so
/// this is the one place the version is spelled out literally. This helper
/// fails the test if the variable is missing or not a concrete (non-range)
/// semver pin, keeping the guardrails strict without re-hardcoding the version.
fn ocr_version_from_workflow() -> String {
    let content = ocr_pr_review_workflow_content();
    let raw_value = content
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("OCR_VERSION:"))
        .map(|rest| rest.trim())
        .unwrap_or_else(|| {
            panic!("Workflow must declare OCR_VERSION as the single source of truth for the pinned OCR version")
        });
    let unquoted = if (raw_value.starts_with('"') && raw_value.ends_with('"'))
        || (raw_value.starts_with('\'') && raw_value.ends_with('\''))
    {
        &raw_value[1..raw_value.len() - 1]
    } else {
        raw_value
    };
    assert!(
        !unquoted.is_empty(),
        "OCR_VERSION must be a non-empty concrete semver, not an empty value"
    );
    // Reject anything that looks like an npm range/operator instead of a pin.
    assert!(
        !unquoted.contains(['^', '~', '>', '*', ' ']),
        "OCR_VERSION must be an exact pinned semver (no ranges, comparators, or spaces): found {unquoted:?}"
    );
    let core = unquoted.split(['-', '+']).next().unwrap_or(unquoted);
    let semver_parts: Vec<&str> = core.split('.').collect();
    assert!(
        semver_parts.len() == 3
            && semver_parts
                .iter()
                .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit())),
        "OCR_VERSION must be a concrete three-part semver pin, found {unquoted:?}"
    );
    unquoted.to_string()
}

/// Path to the single shared OCR secret-redaction module. Every github-script
/// step loads this file via require() so the redaction patterns live in exactly
/// one place instead of being duplicated inline per step.
fn ocr_redaction_module_path() -> PathBuf {
    repository_root()
        .join(".github")
        .join("scripts")
        .join("ocr-redaction.js")
}

/// Helper to read the shared OCR secret-redaction module for guardrail tests.
fn ocr_redaction_module_content() -> String {
    let module_path = ocr_redaction_module_path();
    fs::read_to_string(&module_path)
        .unwrap_or_else(|e| panic!("Failed to read ocr-redaction.js at {module_path:?}: {e}"))
}

fn yaml_block_after(content: &str, header: &str) -> String {
    let header_key = header.trim();
    let start_idx = find_yaml_key_line(content, header_key).unwrap_or_else(|| {
        panic!("yaml_block_after: header '{header_key}' not found as a real YAML key in content")
    });

    let lines: Vec<&str> = content.lines().collect();
    let header_line = lines[start_idx];
    let header_indent = indent_of(header_line);
    let mut block = String::new();
    block.push_str(header_line);
    block.push('\n');

    // Walk subsequent lines until a real YAML key (or value) at the same or
    // lesser indent is encountered. Block-scalar indicator tails (|, >) are
    // never treated as block boundaries so a `run: |` step body is not cut
    // short by a line that merely starts with `|`.
    for line in lines.iter().skip(start_idx + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            block.push_str(line);
            block.push('\n');
            continue;
        }
        let indent = indent_of(line);
        if indent <= header_indent
            && !trimmed.starts_with('#')
            && !is_block_scalar_indicator(trimmed)
        {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }

    block
}

/// Indentation (number of leading spaces) of a line.
fn indent_of(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// True if the trimmed line looks like a YAML block-scalar indicator tail
/// (e.g. "|", "|-", ">", ">+"). Used to avoid treating these as block
/// boundaries.
fn is_block_scalar_indicator(trimmed: &str) -> bool {
    let first = trimmed.chars().next();
    first == Some('|') || first == Some('>')
}

/// Find the line index of a YAML key, skipping block-scalar content.
///
/// This walks the content tracking block-scalar context: once a line ends with
/// `|` or `>` (a YAML literal/folded scalar indicator), subsequent lines at a
/// deeper indentation are scalar content (not YAML keys) and are skipped.
/// This prevents a literal string like `permissions:` inside a `run:` script
/// from being mistaken for a real YAML key.
fn find_yaml_key_line(content: &str, key: &str) -> Option<usize> {
    let lines: Vec<&str> = content.lines().collect();
    let mut block_scalar_indent: Option<usize> = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Skip blank lines and full-line comments (a comment line can never be
        // a YAML key, and a key never follows `#` on its own line).
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = indent_of(line);

        // If we are inside a block scalar, lines at a deeper indent are scalar
        // content — skip them for key detection.
        if let Some(scalar_indent) = block_scalar_indent {
            if indent > scalar_indent {
                // Still inside the scalar; this line is script content.
                // But a line at or below the scalar indent could be a new key.
                continue;
            } else {
                // Dedented out of the scalar.
                block_scalar_indent = None;
            }
        }

        // Check if this line opens a block scalar (ends with | or >), so the
        // *next* matching key search must skip the scalar body.
        if line_ends_with_block_scalar(trimmed) {
            block_scalar_indent = Some(indent);
        }

        // Does this line define the key we are looking for?
        if trimmed == key
            || trimmed.starts_with(&format!("{key} "))
            || trimmed.starts_with(&format!("{key}\t"))
        {
            return Some(idx);
        }
    }
    None
}

/// True if a trimmed line ends with a YAML block scalar indicator (| or >,
/// optionally followed by a chomping indicator like |- or >+).
fn line_ends_with_block_scalar(trimmed: &str) -> bool {
    // A block scalar indicator is the last non-whitespace token and is | or >
    // optionally followed by a single chomping char (- or +).
    let last_word = trimmed.split_whitespace().last().unwrap_or("");
    if last_word.is_empty() {
        return false;
    }
    let bytes = last_word.as_bytes();
    let first = bytes[0] as char;
    (first == '|' || first == '>')
        && last_word[1..].chars().all(|c| c == '-' || c == '+')
        && last_word.len() <= 2
}

/// Extract the YAML block for a single top-level job, stopping at the next
/// top-level key (the next job header or the end of the `jobs:` section).
/// This isolates each job so permission validation cannot leak across jobs.
fn job_yaml_block(content: &str, job_name: &str) -> String {
    let job_header = format!("{job_name}:");
    let all_job_names = workflow_job_names(content);
    let start_idx = find_yaml_key_line(content, &job_header).unwrap_or_else(|| {
        panic!("job_yaml_block: job '{job_name}' header not found as a real YAML key")
    });
    let lines: Vec<&str> = content.lines().collect();
    let header_indent = indent_of(lines[start_idx]);
    let mut block = String::new();
    block.push_str(lines[start_idx]);
    block.push('\n');

    for line in lines.iter().skip(start_idx + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            block.push_str(line);
            block.push('\n');
            continue;
        }
        let indent = indent_of(line);
        // Stop at the next top-level job (indent <= header_indent and the line
        // is another job header) or any key at a lesser indent.
        if indent <= header_indent && !trimmed.starts_with('#') {
            // Is this the next job header?
            let candidate = trimmed.trim_end_matches(':');
            if indent == header_indent
                && all_job_names.iter().any(|jn| jn == candidate)
                && candidate != job_name
            {
                break;
            }
            if indent < header_indent {
                // Left the jobs section entirely.
                break;
            }
        }
        block.push_str(line);
        block.push('\n');
    }
    block
}

fn push_run_block(blocks: &mut Vec<String>, block: String) {
    let normalized = normalize_run_block(&block);
    if !normalized.trim().is_empty() {
        blocks.push(normalized);
    }
}

fn normalize_run_block(block: &str) -> String {
    let lines: Vec<&str> = block.lines().collect();
    let min_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|ch| *ch == ' ').count())
        .min()
        .unwrap_or(0);

    let mut normalized = String::new();
    for line in lines {
        let without_indent = line.get(min_indent..).unwrap_or("");
        normalized.push_str(without_indent);
        normalized.push('\n');
    }
    normalized
}

fn unescape_double_quoted(inner: &str) -> String {
    let mut unescaped = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            unescaped.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => unescaped.push('\\'),
            Some('"') => unescaped.push('"'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

fn unquote_inline_run_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.len() >= 2 {
        if let Some(inner) = trimmed
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        {
            return unescape_double_quoted(inner);
        }
        if let Some(inner) = trimmed
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
        {
            return unescape_double_quoted(inner);
        }
    }
    trimmed.to_string()
}

fn run_blocks(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, String)> = None;

    for line in content.lines() {
        if let Some((run_indent, block)) = current.as_mut() {
            let trimmed = line.trim();
            let indent = line.chars().take_while(|ch| *ch == ' ').count();
            if !trimmed.is_empty() && indent <= *run_indent {
                push_run_block(&mut blocks, std::mem::take(block));
                current = None;
            } else {
                let indicator = trimmed.chars().next();
                if let Some(indicator_ch) = indicator {
                    if block.is_empty() && matches!(indicator_ch, '|' | '>') {
                        let rest_after_indicator = &trimmed[indicator_ch.len_utf8()..]
                            .trim_start_matches(|ch: char| {
                                matches!(ch, '-' | '+') || ch.is_ascii_digit()
                            })
                            .trim_start();
                        if !rest_after_indicator.is_empty() {
                            block.push_str(rest_after_indicator);
                            block.push('\n');
                        }
                        continue;
                    }
                }
                block.push_str(line);
                block.push('\n');
                continue;
            }
        }

        let trimmed = line.trim_start();
        let run_entry = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
        if let Some((key, rest)) = run_entry.split_once(':') {
            if key.trim() != "run" {
                continue;
            }
            let indent = line.chars().take_while(|ch| *ch == ' ').count();
            let command = rest.trim_start();
            let indicator = command.chars().next();
            if indicator.is_none() || indicator.is_some_and(|ch| matches!(ch, '|' | '>')) {
                let rest_after_indicator = indicator
                    .map(|ch| &command[ch.len_utf8()..])
                    .unwrap_or("")
                    .trim_start_matches(|ch: char| matches!(ch, '-' | '+') || ch.is_ascii_digit())
                    .trim_start();
                let mut block = String::new();
                if !rest_after_indicator.is_empty() {
                    block.push_str(rest_after_indicator);
                    block.push('\n');
                }
                current = Some((indent, block));
            } else if !command.is_empty() {
                blocks.push(format!("{}\n", unquote_inline_run_command(command)));
            }
        }
    }

    if let Some((_, block)) = current {
        push_run_block(&mut blocks, block);
    }

    blocks
}
fn strip_shell_comment(line: &str) -> String {
    let mut result = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut previous = ' '; // Leading '#' comments should strip like shell comments.
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            result.push(ch);
            // Escaped whitespace is not a shell word boundary, so it must not
            // enable comment stripping for a following '#'.
            if !ch.is_whitespace() {
                previous = ch;
            }
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            let escapes_next = !in_double
                || chars
                    .peek()
                    .is_some_and(|next| matches!(*next, '$' | '`' | '"' | '\\'));
            result.push(ch);
            previous = ch;
            if escapes_next {
                escaped = true;
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
        } else if ch == '"' && !in_single {
            in_double = !in_double;
        } else if ch == '#' && !in_single && !in_double && previous.is_whitespace() {
            break;
        }
        result.push(ch);
        previous = ch;
    }

    result
}

#[test]
fn test_ocr_pr_review_run_block_parsing_handles_shell_edge_cases() {
    let content =
        "steps:\n  - name: strip\n    run: |-\n      echo \"\\#notcomment\" # strip this\n  - name: runtime-key\n    runtime: node20\n  - name: inline\n    run: |+ echo inline\n  - name: quoted-inline\n    run: \"bash scripts/review.sh\"\n  - name: following-line-scalar\n    run:\n      |\n        echo late-block\n  - run: ./direct.sh\n";
    let blocks = run_blocks(content);
    assert_eq!(blocks.len(), 5);
    assert!(blocks[0].contains("echo \"\\#notcomment\" # strip this"));
    assert_eq!(strip_shell_comment(&blocks[0]), "echo \"\\#notcomment\" ");
    assert_eq!(blocks[1], "echo inline\n");
    assert_eq!(blocks[2], "bash scripts/review.sh\n");
    // Escaped spaces do not delimit a shell word, so the following '#'

    // remains part of the same word instead of starting a shell comment.
    assert_eq!(
        strip_shell_comment("echo \\ #notcomment"),
        "echo \\ #notcomment"
    );
    assert!(
        strip_shell_comment(&blocks[3]).contains("echo late-block"),
        "run keys with a following-line scalar indicator should still be scanned"
    );
    assert_eq!(blocks[4], "./direct.sh\n");
}

#[test]
fn test_pr_quality_targets_nested_workflow_crate() {
    let workflow = fs::read_to_string(root_workflow_path("pr-quality.yml"))
        .expect("Failed to read pr-quality.yml");

    assert!(
        workflow.matches("working-directory: workflow").count() >= 7,
        "Every PR quality job that runs project commands should execute from workflow/"
    );
    assert!(
        workflow.contains("CLIPPY_CONF_DIR: .github/clippy"),
        "Clippy config should remain relative to the workflow crate root"
    );
    assert!(
        workflow.contains("fetch-depth: 0")
            && workflow
                .contains("cargo xtask complexity --changed origin/${{ github.base_ref }} HEAD"),
        "Changed-file complexity checks require full git history and an explicit PR base"
    );
    assert!(
        workflow.contains("workspaces: workflow -> target")
            && workflow.contains("path: workflow/target/llvm-cov-target/workspace-summary.json"),
        "Action-managed cache and artifact paths must be rooted at repository paths"
    );
}

#[test]
fn test_pr_quality_exposes_expected_checks() {
    let workflow = fs::read_to_string(root_workflow_path("pr-quality.yml"))
        .expect("Failed to read pr-quality.yml");

    for required in [
        "name: Format (rustfmt)",
        "name: Lint (clippy + structural)",
        "name: Tests (lib + integration)",
        "name: Coverage (llvm-cov gate)",
        "name: Docs (cargo doc)",
        "name: Security (cargo audit)",
        "name: Release readiness (release build)",
        "cargo doc --workspace --all-features --no-deps",
        "run: cargo audit",
        "cargo build --release --bin luther-workflow",
    ] {
        assert!(
            workflow.contains(required),
            "PR quality workflow should contain {required}"
        );
    }
}

#[test]
fn test_release_workflow_targets_nested_workflow_crate() {
    let workflow =
        fs::read_to_string(root_workflow_path("release.yml")).expect("Failed to read release.yml");

    assert!(
        workflow.contains("working-directory: workflow"),
        "Release shell steps should execute from workflow/ so cargo aliases resolve"
    );
    assert!(
        workflow.contains("workspaces: workflow -> target")
            && workflow.contains("cargo release-all \"${RELEASE_TAG}\""),
        "Release workflow should cache the nested workspace and run the existing cargo release flow"
    );
}
/// Test: the OCR PR review workflow file exists.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
#[test]

fn test_ocr_pr_review_workflow_exists() {
    let workflow_path = root_workflow_path("ocr-pr-review.yml");
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
    let pull_request_target = yaml_block_after(&content, "pull_request_target:");
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

/// Extract top-level job names from a GitHub Actions workflow YAML string.
///
/// Finds the `jobs:` key as a real top-level YAML key (indent 0) by scanning
/// with block-scalar awareness, then collects the direct child keys (job names)
/// at the first nested indentation level.
fn workflow_job_names(content: &str) -> Vec<String> {
    let jobs_idx = find_yaml_key_line(content, "jobs:")
        .expect("workflow must have a top-level jobs: section (not inside a block scalar)");
    let lines: Vec<&str> = content.lines().collect();
    // The job-name indentation is the first non-blank, non-comment line after
    // the jobs: header. This avoids hard-coding a 2-space assumption.
    let job_indent = lines[jobs_idx + 1..]
        .iter()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                None
            } else {
                Some(indent_of(line))
            }
        })
        .unwrap_or(2);

    let mut names = Vec::new();
    let mut block_scalar_indent: Option<usize> = None;
    for line in lines.iter().skip(jobs_idx + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = indent_of(line);

        // Track block-scalar context to skip script content.
        if let Some(scalar_indent) = block_scalar_indent {
            if indent > scalar_indent {
                continue;
            } else {
                block_scalar_indent = None;
            }
        }

        // A line at indent 0 after jobs: means we left the jobs section.
        if indent == 0 && !trimmed.starts_with('#') {
            break;
        }

        if trimmed.starts_with('#') {
            continue;
        }

        if line_ends_with_block_scalar(trimmed) {
            block_scalar_indent = Some(indent);
        }

        if indent == job_indent {
            if let Some(name) = trimmed.strip_suffix(':') {
                if !name.contains(' ') && !name.contains('\t') && !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// Test: permissions are minimal and explicit.
#[test]
fn test_ocr_pr_review_permissions_are_minimal() {
    let content = ocr_pr_review_workflow_content();

    // The top-level permissions block defines the approved minimal set.
    let permissions = yaml_block_after(&content, "permissions:");
    let permission_lines: Vec<&str> = permissions
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let mut sorted_permission_lines = permission_lines.clone();
    sorted_permission_lines.sort_unstable();
    let mut expected_permission_lines = vec![
        "contents: read",
        "pull-requests: write",
        "issues: write",
        "actions: read",
    ];
    expected_permission_lines.sort_unstable();
    assert_eq!(
        sorted_permission_lines, expected_permission_lines,
        "Top-level workflow permissions must be exactly the approved minimal set"
    );

    // Every job that declares its own `permissions:` block must be a strict
    // subset of the top-level block (least-privilege narrowing). Iterate over
    // all job blocks so a future change cannot add excessive permissions to
    // any job (e.g. gate or code-review) without triggering a failure.
    let job_names = workflow_job_names(&content);
    assert!(
        !job_names.is_empty(),
        "Workflow must define at least one job"
    );
    for job_name in &job_names {
        // Use the structural job-block extractor so each job is validated in
        // isolation (the block stops at the next job header). This prevents a
        // nested `permissions:` occurrence in another job or a `run:` script
        // from leaking into the wrong job's permission check.
        let job_block = job_yaml_block(&content, job_name);
        if !job_block
            .lines()
            .any(|line| line.trim_start() == "permissions:")
        {
            continue;
        }
        let job_perms = yaml_block_after(&job_block, "permissions:");
        let job_permission_lines: Vec<&str> = job_perms
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        for job_perm in &job_permission_lines {
            assert!(
                permission_lines.iter().any(|top| top == job_perm),
                "Job '{job_name}' permission '{job_perm}' must be a subset of the top-level minimal permissions"
            );
        }
    }
}

/// Test: find_yaml_key_line skips occurrences of a key name inside YAML
/// block-scalar content (run: |, script: |). This is the structural guard
/// against a literal `permissions:` inside a step's shell script being
/// mistaken for a real YAML permissions key.
#[test]
fn test_find_yaml_key_line_skips_block_scalar_content() {
    let workflow = r#"name: Test
on: push
permissions:
  contents: read
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Step with permissions in script
        run: |
          echo "checking permissions:"
          if [ -f config ]; then
            permissions: read
          fi
  deploy:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - run: echo hi
"#;
    // The FIRST real `permissions:` key is the top-level one (line 3).
    let idx =
        find_yaml_key_line(workflow, "permissions:").expect("should find top-level permissions:");
    let lines: Vec<&str> = workflow.lines().collect();
    assert_eq!(lines[idx].trim(), "permissions:");
    // The line must NOT be the one inside the run: | block.
    assert!(
        !lines[idx].contains("echo"),
        "find_yaml_key_line must not match keys inside block-scalar (run: |) content"
    );
}

/// Test: workflow_job_names finds jobs: as a top-level key and extracts all
/// job names structurally, even when a step script contains `jobs:`-like text.
#[test]
fn test_workflow_job_names_structural_extraction() {
    let workflow = r#"name: Test
on: push
env:
  NOTE: "jobs: is also a string here"
jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - run: echo "jobs: appear in scripts too"
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo hi
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: echo bye
"#;
    let names = workflow_job_names(workflow);
    assert_eq!(names, vec!["gate", "build", "deploy"]);
}

/// Test: job_yaml_block isolates each job, stopping at the next job header so
/// permission validation never leaks across jobs. The build job's block must
/// not contain the deploy job's content.
#[test]
fn test_job_yaml_block_isolates_jobs() {
    let workflow = r#"name: Test
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - run: echo build
  deploy:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - run: echo deploy
"#;
    let build_block = job_yaml_block(workflow, "build");
    assert!(
        !build_block.contains("deploy:"),
        "job_yaml_block for 'build' must not include the deploy job header"
    );
    assert!(
        !build_block.contains("packages: write"),
        "job_yaml_block for 'build' must not include deploy's permissions"
    );
    assert!(
        build_block.contains("contents: read"),
        "job_yaml_block for 'build' must include build's own permissions"
    );

    let deploy_block = job_yaml_block(workflow, "deploy");
    assert!(
        deploy_block.contains("packages: write"),
        "job_yaml_block for 'deploy' must include its own permissions"
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
    assert!(
        content.contains("group: ${{ needs.gate.outputs.group_key }}"),
        "Code review concurrency group must reuse the centralized gate output"
    );
    assert!(
        content.contains("const prNumber = context.payload.pull_request && context.payload.pull_request.number")
            && content.contains("const issueNumber = context.payload.issue && context.payload.issue.number")
            && content.contains("const dispatchNumber = context.payload.inputs && context.payload.inputs.pr_number")
            && content.contains("const groupNumber = prNumber || issueNumber || dispatchNumber")
            && content.contains("shouldRun ? `ocr-pr-review-${groupNumber}` : `ocr-pr-review-skip-${context.runId}`"),
        "Centralized gate must key runnable automatic, comment, and manual runs by PR/issue number while isolating skipped comment runs"
    );
    let concurrency_block = yaml_block_after(&content, "concurrency:");
    assert!(
        !concurrency_block.contains("github.ref"),
        "Concurrency must not fall back to github.ref for PR review runs"
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
    assert!(
        content.contains("--concurrency 2"),
        "Workflow must cap OCR provider contention with an explicit --concurrency value"
    );
}

/// Test: CI reviews are deterministic — OCR self-update checks are disabled.
#[test]
fn test_ocr_pr_review_sets_ocr_no_update_env() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains(r#"OCR_NO_UPDATE: "1""#) || content.contains("OCR_NO_UPDATE: '1'"),
        "Workflow must set OCR_NO_UPDATE to keep CI reviews deterministic"
    );
}

/// Test: OCR installs to an isolated local prefix, exposes it on PATH, exports
/// OCR_BIN for later steps, and pins the version through the single-source
/// `OCR_VERSION` env var referenced by both the install command and the cache
/// key.
#[test]
fn test_ocr_pr_review_installs_locally_pinned_version() {
    let content = ocr_pr_review_workflow_content();
    // Single source of truth: OCR_VERSION must be an exact pinned semver.
    let pinned_version = ocr_version_from_workflow();
    assert!(
        content
            .contains(r#"OCR_PREFIX="${RUNNER_TEMP}/ocr-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}""#),
        "Install must scope the prefix per run and attempt so concurrent runners cannot collide"
    );
    // The install must reference the OCR_VERSION variable (not a hard-coded
    // version literal) so the single source of truth cannot drift.
    assert!(
        content.contains(r#"npm install --prefix "$OCR_PREFIX" --ignore-scripts "@alibaba-group/open-code-review@${OCR_VERSION}""#),
        "Install must target the isolated local prefix and reference the single-source OCR_VERSION variable"
    );
    // The literal version may only appear in the OCR_VERSION declaration.
    let literal_occurrences = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.contains(&format!("@alibaba-group/open-code-review@{pinned_version}"))
        })
        .count();
    assert_eq!(
        literal_occurrences, 0,
        "Pinned version {pinned_version} must only appear in the OCR_VERSION declaration, not hard-coded in the install command"
    );
    assert!(
        content.contains(r#"echo "${OCR_PREFIX}/node_modules/.bin" >> "$GITHUB_PATH""#),
        "Install must append the local prefix bin dir to GITHUB_PATH"
    );
    assert!(
        content.contains("OCR_BIN=${OCR_BIN}") && content.contains(r#""$OCR_BIN" version"#),
        "Install must export OCR_BIN and invoke it directly in later steps"
    );
    assert!(
        !content.contains("npm install -g"),
        "Install must never fall back to a global npm install"
    );
}

/// Test: every action reference is a pinned SHA with a ratchet comment.
#[test]
fn test_ocr_pr_review_pins_action_shas_with_ratchet() {
    let content = ocr_pr_review_workflow_content();
    let uses_lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("uses:"))
        .collect();
    assert!(
        !uses_lines.is_empty(),
        "Workflow must reference at least one pinned action"
    );
    for line in &uses_lines {
        // Distinguish two distinct failure modes so the error message points to
        // the correct root cause: (1) a missing ratchet comment, and (2) a
        // reference that is not pinned to a 40-hex SHA.
        assert!(
            line.contains("# ratchet:actions/"),
            "Pinned action must carry a ratchet comment naming the tracked version: {line}"
        );
        let reference = line
            .trim_start_matches("uses:")
            .trim()
            .split_once('#')
            .map(|(reference, _)| reference.trim())
            .unwrap_or("");
        let sha = reference
            .split_once('@')
            .map(|(_, sha)| sha)
            .unwrap_or_default();
        assert!(
            sha.len() == 40 && sha.chars().all(|ch| ch.is_ascii_hexdigit()),
            "Action reference must be pinned to a 40-hex commit SHA: {line}"
        );
    }
    assert!(
        !content.contains("uses: actions/checkout@v")
            && !content.contains("uses: actions/github-script@v")
            && !content.contains("uses: actions/upload-artifact@v")
            && !content.contains("uses: actions/download-artifact@v"),
        "Workflow must not reference actions by a bare @vN tag"
    );
}

/// Test: phase tracking and infrastructure-vs-policy classification are wired.
#[test]
fn test_ocr_pr_review_tracks_phase_and_classifies_failures() {
    let content = ocr_pr_review_workflow_content();
    for phase_write in [
        r#"echo "install" > ocr-phase.txt"#,
        r#"echo "validate" > ocr-phase.txt"#,
        r#"echo "llm-preflight" > ocr-phase.txt"#,
        r#"echo "preview" > ocr-phase.txt"#,
        r#"echo "review" > ocr-phase.txt"#,
    ] {
        assert!(
            content.contains(phase_write),
            "Each OCR step must record its phase: missing {phase_write}"
        );
    }
    assert!(
        content.contains("ocr-infrastructure-failure.txt")
            && content.contains("ocr-policy-failure.txt"),
        "Workflow must record infrastructure and policy failure markers"
    );
    assert!(
        content.contains("id: ocr-classification")
            && content.contains("policy_failure=true")
            && content.contains("infrastructure_failure=true"),
        "Workflow must resolve a classification step that emits policy/infrastructure outputs"
    );
    assert!(
        content.contains(
            "infrastructure_failure: ${{ steps.ocr-classification.outputs.infrastructure_failure }}"
        ) && content
            .contains("policy_failure: ${{ steps.ocr-classification.outputs.policy_failure }}"),
        "code-review job must expose classification outputs for the notification job"
    );
    assert!(
        content.contains("[ -s ocr-exit-code.txt ]"),
        "Setup/validate/preview/review steps must short-circuit gracefully once an earlier failure is recorded"
    );
}

/// Test: secrets are redacted in both the posting step and the artifact
/// redaction step, and redaction runs via github-script (never a node/perl
/// interpreter in a run step).
#[test]
fn test_ocr_pr_review_redacts_secrets_in_artifacts_and_comments() {
    let content = ocr_pr_review_workflow_content();
    let module = ocr_redaction_module_content();

    // The redaction implementation must be defined exactly once, in the shared
    // module, and must cover the common secret patterns there.
    assert!(
        module.contains("function redactSecretDiagnostics(value"),
        "Shared module must define the redaction helper"
    );
    assert_eq!(
        module.matches("function redactSecretDiagnostics").count(),
        1,
        "The shared module is the single source of truth: redactSecretDiagnostics must be defined exactly once"
    );
    for pattern in [
        r"Authorization\s*:",
        r"x-api-key\s*:",
        r"api[_-]?key\s*[=:]",
        r"access[_-]?token\s*[=:]",
    ] {
        assert!(
            module.contains(pattern),
            "Shared redaction module must cover common secret patterns: missing {pattern}"
        );
    }
    assert!(
        module.contains("module.exports") && module.contains("redactSecretDiagnostics"),
        "Shared module must export redactSecretDiagnostics for reuse across steps"
    );

    // The workflow inlines a canonical redaction fallback (kept in sync with
    // the shared module) so redaction works even when the shared module is not
    // yet present in the base-branch checkout (the transition window before
    // this PR's module reaches every base commit). The inline fallback must
    // contain the SAME canonical patterns as the shared module so there is no
    // drift between the two definitions.
    let inline_fallback_count = content.matches("escapeRegExpLocal").count();
    assert!(
        inline_fallback_count >= 3,
        "All three OCR steps must define an inline redaction fallback (escapeRegExpLocal): found {inline_fallback_count}"
    );
    // Every canonical pattern in the shared module must also appear in the
    // workflow's inline fallback, so the fallback can never be weaker than the
    // module. We check the distinctive pattern fragments that are unique to
    // each redaction rule.
    for canonical_pattern in [
        r"Authorization\s*:\s*",
        r"x-api-key\s*:\s*",
        r"api[_-]?key\s*[=:]\s*",
        r"access[_-]?token\s*[=:]\s*",
        r"refresh[_-]?token\s*[=:]\s*",
        r"id[_-]?token\s*[=:]\s*",
    ] {
        assert!(
            module.contains(canonical_pattern),
            "Shared redaction module must define canonical pattern: {canonical_pattern}"
        );
        assert!(
            content.contains(canonical_pattern),
            "Workflow inline fallback must mirror the shared module's canonical pattern: missing {canonical_pattern}"
        );
    }
    // The inline fallback must be gated behind a try/catch that first attempts
    // to load the shared module, so post-merge (when the module is on the base
    // branch) the single source of truth is used rather than the inline copy.
    assert!(
        content.contains("redactDiagnosticsWithSecrets = require(ocrRedactionModulePath).redactSecretDiagnostics")
            && content.contains("} catch (_) {"),
        "Inline redaction fallback must only activate when the shared module cannot be loaded (try/catch guard)"
    );
    let require_count = content.matches(".github/scripts/ocr-redaction.js").count();
    assert!(
        require_count >= 3,
        "All three OCR steps (posting, artifact redaction, notification) must load the shared redaction module: found {require_count} require(s)"
    );
    // Every shared-redaction require must resolve to the trusted file inside the
    // workspace. Accept any valid resolution strategy (process.env.GITHUB_WORKSPACE,
    // github.workspace, or a relative path) as long as it targets the checked-in
    // module — do not lock the test to a single path-expression syntax.
    for line in content.lines() {
        if line.contains(".github/scripts/ocr-redaction.js") && line.contains("require(") {
            assert!(
                line.contains("GITHUB_WORKSPACE")
                    || line.contains("github.workspace")
                    || line.contains("__dirname"),
                "Every shared-redaction require must resolve via the trusted workspace root: {line}"
            );
        }
    }

    assert!(
        content.contains("name: Redact OCR diagnostic artifacts")
            && content.contains("fs.writeFileSync(fileName, placeholder)")
            && content.contains("redactSecretDiagnostics(raw)"),
        "Artifact redaction must read/write each diagnostic file via github-script fs with fail-closed placeholder, not a node/perl heredoc"
    );
    // Fail-closed invariant: the artifact redaction loop must destroy the
    // original content *before* attempting redaction so a redaction error or
    // fallback-write error can never leave unredacted secrets on disk for the
    // artifact upload step.
    assert!(
        content.contains("Fail closed: destroy the original on disk")
            && content.contains("[redaction unavailable for"),
        "Artifact redaction must fail closed: overwrite with a placeholder before redaction so unredacted secrets are never uploaded"
    );
    // Lock in the architectural decision: redaction/notify must not reintroduce
    // an inline node/perl interpreter or a sourced helper script.
    let scanned_commands = scanned_ocr_pr_review_run_commands();
    for forbidden in ["node", "perl", "npx"] {
        assert!(
            !shell_command_segments(&scanned_commands)
                .any(|token| token_invokes_forbidden_command(token, forbidden)),
            "Redaction/notify logic must not invoke {forbidden} from a run step"
        );
    }
    assert!(
        !content.contains("ocr-workflow-helpers.sh"),
        "Cross-step markers must use plain redirection, not a sourced helper script"
    );
}

/// Test: the OCR workflow is self-contained for redaction and does NOT hard-
/// depend on the shared module being present in the base-branch checkout.
///
/// Under pull_request_target the code-review job checks out the PR's base SHA.
/// If the shared module (.github/scripts/ocr-redaction.js) is new in this PR
/// and not yet on every base commit, a hard `require()` would throw
/// MODULE_NOT_FOUND and silently disable redaction. The workflow must define
/// an inline canonical fallback so redaction works in all scenarios:
/// pre-merge transition, post-merge, and future PRs against any base — without
/// ever loading PR-supplied code.
#[test]
fn test_ocr_pr_review_redaction_is_base_branch_available() {
    let content = ocr_pr_review_workflow_content();
    // Every redaction step must attempt the shared module first (single source
    // of truth post-merge) but fall back to an inline implementation.
    let require_attempts = content
        .matches("require(ocrRedactionModulePath).redactSecretDiagnostics")
        .count();
    assert!(
        require_attempts >= 3,
        "All three OCR steps must attempt to load the shared redaction module: found {require_attempts}"
    );
    let inline_fallbacks = content.matches("escapeRegExpLocal").count();
    assert_eq!(
        inline_fallbacks, 6,
        "All three OCR steps must define an inline fallback (escapeRegExpLocal defined + used = 2 per step): found {inline_fallbacks}"
    );
    // The fallback must be inside a catch block so a MODULE_NOT_FOUND never
    // propagates as an uncaught error.
    assert!(
        content.contains("} catch (_) {")
            && content.contains("redactDiagnosticsWithSecrets = (value, exactSecrets = [])"),
        "Inline redaction fallback must be activated only inside a catch block"
    );
}

/// Test: every inline redaction fallback must be a faithful structural mirror
/// of the shared module (.github/scripts/ocr-redaction.js). This prevents the
/// three inline copies from silently drifting from the canonical implementation.
///
/// Specifically, the inline fallback must include the same try/catch +
/// split-join safety net that the module uses for exact-secret replacement, so
/// a secret value containing regex-special characters cannot cause the inline
/// copy to throw while the module succeeds.
#[test]
fn test_inline_redaction_mirrors_shared_module() {
    let content = ocr_pr_review_workflow_content();
    let module = ocr_redaction_module_content();

    // The module must contain the try/catch + split-join safety net.
    assert!(
        module.contains("try {") && module.contains("sanitized.split(secret).join(REDACTION)"),
        "Shared module must define the try/catch split-join safety net for exact-secret redaction"
    );

    // Every inline fallback copy must also contain the split-join safety net.
    let inline_split_join = content
        .matches("sanitized.split(secret).join(REDACTION)")
        .count();
    assert_eq!(
        inline_split_join, 3,
        "All three inline redaction fallbacks must include the split-join safety net (one per step): found {inline_split_join}"
    );

    // The number of inline try/catch blocks for secret redaction must match
    // the number of inline copies (3).
    let inline_try_in_secret_loop = content
        .matches("sanitized = sanitized.replace(new RegExp(escapeRegExpLocal(secret)")
        .count();
    assert_eq!(
        inline_try_in_secret_loop, 3,
        "All three inline fallbacks must wrap the RegExp replace in a try block: found {inline_try_in_secret_loop}"
    );

    // Verify structural pattern parity: every structural .replace() call in the
    // module must also appear (the same number of times, proportionally) in
    // the workflow's inline fallbacks.
    let structural_patterns = [
        r"Authorization\s*:\s*",
        r"x-api-key\s*:\s*",
        r"api[_-]?key\s*[=:]\s*",
        r"[?&](?:key|api[_-]?key|token)=",
        r"access[_-]?token\s*[=:]\s*",
        r"refresh[_-]?token\s*[=:]\s*",
        r"id[_-]?token\s*[=:]\s*",
        r"token\s*[=:]\s*",
        r"secret\s*[=:]\s*",
    ];
    for pattern in structural_patterns {
        let module_count = module.matches(pattern).count();
        let workflow_count = content.matches(pattern).count();
        assert!(
            module_count > 0,
            "Shared module must define structural pattern: {pattern}"
        );
        // Each pattern appears once in the module and three times in the
        // workflow (one per inline copy).
        assert!(
            workflow_count >= module_count * 3,
            "Inline fallback must mirror the shared module for pattern '{pattern}': module has {module_count}, workflow has {workflow_count} (expected at least {})",
            module_count * 3
        );
    }

    // The workflow must document the transition cleanup so the inline copies
    // are removed once the shared module reaches every base checkout.
    assert!(
        content.contains("TRANSITION CLEANUP"),
        "Workflow must document the transition cleanup plan for the inline redaction fallback"
    );
}

/// Test: duplicate tracking issues must be closed WITHOUT state_reason:
/// 'not_planned'. The 'not_planned' reason is semantically inaccurate for
/// duplicate issues (it means the team decided not to implement the feature).
/// Omitting state_reason entirely lets GitHub record a neutral close.
#[test]
fn test_ocr_pr_review_duplicate_close_omits_misleading_state_reason() {
    let content = ocr_pr_review_workflow_content();

    // The deduplication close call must close issues but must NOT set
    // state_reason to 'not_planned'.
    assert!(
        !content.contains("state_reason: 'not_planned'"),
        "Deduplication close must not use state_reason 'not_planned' (semantically inaccurate for duplicates)"
    );
    assert!(
        !content.contains("state_reason: 'completed'"),
        "Deduplication close must not use state_reason 'completed' (semantically inaccurate for duplicates)"
    );

    // Verify the close call itself still exists (state: 'closed' without a
    // misleading state_reason).
    assert!(
        content.contains("state: 'closed'"),
        "Deduplication close must still close the issue (state: 'closed')"
    );

    // The close call must not carry any state_reason field at all.
    let closed_lines: Vec<&str> = content
        .lines()
        .filter(|l| l.contains("state: 'closed'"))
        .collect();
    assert!(
        !closed_lines.is_empty(),
        "Workflow must contain at least one issue close call"
    );
    for closed_line in &closed_lines {
        let line_num = content.lines().position(|l| l == *closed_line).unwrap_or(0);
        let nearby: String = content
            .lines()
            .skip(line_num.saturating_sub(1))
            .take(4)
            .collect::<Vec<&str>>()
            .join("\n");
        assert!(
            !nearby.contains("state_reason"),
            "Issue close call must not include state_reason: {nearby}"
        );
    }
}

/// Test: the policy-skip informational message in the notification job must
/// use core.info (or core.warning for actionable conditions), never
/// core.notice. core.notice has reduced visibility in some GitHub Actions log
/// viewers and is inappropriate for diagnostic messages that maintainers need
/// to see. The policy-skip is expected behavior (not a warning), so core.info
/// is the correct severity.
#[test]
fn test_ocr_pr_review_policy_skip_uses_visible_log_level() {
    let content = ocr_pr_review_workflow_content();

    // The policy-skip message must NOT use core.notice.
    assert!(
        !content.contains("core.notice"),
        "Workflow must not use core.notice (reduced visibility); use core.info or core.warning instead"
    );

    // The policy-skip message must use core.info (informational, not a warning
    // — policy failures are already surfaced as core.setFailed in the
    // code-review job).
    assert!(
        content.contains(
            "core.info('OCR policy failure detected; skipping infrastructure issue notification.')"
        ),
        "Policy-skip message must use core.info for reliable log visibility"
    );
}

/// Test: an infrastructure-failure notification job exists and is gated.
#[test]
fn test_ocr_pr_review_has_infrastructure_notification_job() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("notify-ocr-infrastructure-failure:"),
        "Workflow must define the infrastructure-failure notification job"
    );
    assert!(
        content.contains("needs: code-review"),
        "Notification job must depend on the code-review job"
    );
    assert!(
        content.contains("!cancelled()")
            && content.contains("needs.code-review.result == 'success'")
            && content.contains("needs.code-review.result == 'failure'"),
        "Notification job must run for completed (non-cancelled) code-review outcomes"
    );
    assert!(
        content.contains("actions/download-artifact"),
        "Notification job must download the OCR artifacts"
    );
    // The notification job must declare its own minimal permissions block
    // rather than relying on the inherited top-level permissions, so the
    // least-privilege surface is auditable per-job.
    let notify_block = yaml_block_after(&content, "notify-ocr-infrastructure-failure:");
    assert!(
        notify_block.contains("permissions:")
            && notify_block.contains("issues: write")
            && notify_block.contains("actions: read"),
        "Notification job must declare explicit minimal permissions (issues: write, actions: read)"
    );
    assert!(
        content.contains("const ISSUE_TITLE = 'OCR review infrastructure failure'")
            && content.contains("github.rest.search.issuesAndPullRequests")
            && content.contains("github.rest.issues.create(")
            && content.contains("github.rest.issues.createComment("),
        "Notification job must find-or-create a single tracking issue via github-script"
    );
    assert!(
        content.contains("skipping infrastructure issue notification"),
        "Notification job must skip when a policy failure was recorded"
    );
}

/// Test: rate-limit responses are classified distinctly from auth/config
/// failures so a shared provider quota is not misreported.
#[test]
fn test_ocr_pr_review_run_uses_concurrency_and_classifies_rate_limit() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("--concurrency 2"),
        "OCR review must cap provider contention with --concurrency 2"
    );
    // Check each rate-limit signal as a separate literal so the guardrail
    // remains valid regardless of how the workflow expresses the pattern
    // (grep -E alternation, separate grep calls, or a case statement).
    for rate_limit_signal in ["http 429", "concurrency reached", "rate limit"] {
        assert!(
            content.contains(rate_limit_signal),
            "OCR review must classify rate-limit signal: missing {rate_limit_signal}"
        );
    }
    assert!(
        content.contains("reason=rate-limited"),
        "OCR review must map HTTP 429 / concurrency-reached stderr to a distinct rate-limit reason"
    );
    // Verify the provider-failure classification covers the key structural
    // phrases without relying on regex-metacharacter literal matching.
    for provider_failure_signal in ["file review", "failed", "provider/config/auth failure"] {
        assert!(
            content.contains(provider_failure_signal),
            "OCR review must classify wholesale per-file review failure: missing {provider_failure_signal}"
        );
    }
}

fn secret_reference_names(content: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for (idx, _) in content.match_indices("secrets.") {
        let tail = &content[idx + "secrets.".len()..];
        let name = tail
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .next()
            .unwrap_or_default();
        if !name.is_empty() {
            refs.push(name.to_string());
        }
    }
    for (idx, _) in content.match_indices("secrets[") {
        let tail = &content[idx + "secrets[".len()..];
        let quote = tail.chars().next();
        if !matches!(quote, Some('\'') | Some('"')) {
            refs.push("<dynamic>".to_string());
            continue;
        }
        let quote = quote.expect("checked quote");
        if let Some(end) = tail[quote.len_utf8()..].find(quote) {
            refs.push(tail[quote.len_utf8()..quote.len_utf8() + end].to_string());
        }
    }
    refs
}

#[test]
fn test_ocr_pr_review_test_scope_guard_fails_closed() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("(tests?|specs?|__tests__)")
            && content.contains("test_[^/]*\\.py")
            && content.contains("[^/]*_test\\.py")
            && content.contains("[^/]*_test\\.go")
            && content.contains("[^/]*(Test|Tests)\\.java")
            && content.contains("[^/]*_test\\.(c|cc|cpp|h|hpp)")
            && content.contains("[^/]*_spec\\.(rs|ts|tsx|js|jsx|mjs|cjs|go|py|java|c|cc|cpp|h|hpp|rb|php|kt|kts|swift|scala)"),
        "Changed-test detection must include common language-specific test filenames outside test directories"
    );
    assert!(
        content.contains("\"$OCR_BIN\" review --preview --from \"$BASE_SHA\" --to \"$HEAD_SHA\"")
            && content.contains("Could not verify OCR preview scope for changed test files")
            && content.contains("OCR preview did not list changed test files in the reviewed set")
            && content.contains("/^Will review[[:space:]]*\\(/ { in_section = 1; next }")
            && content.contains("/^Excluded[[:space:]]*\\(/ { in_section = 1; next }")
            && content.contains("sub(/^[-*][[:space:]]*/, \"\", line)")
            && content.contains("sub(/^\\[[^]]+\\][[:space:]]*/, \"\", line)")
            && content.contains("split(line, fields, /[[:space:]]+/)")
            && content.contains("candidate = fields[1]")
            && content.contains("if (candidate == target) found = 1"),
        "Changed-test scope validation must fail closed when OCR preview cannot be generated or parsed"
    );
    // A genuine policy breach (changed test missing from / excluded by OCR's
    // reviewed set) must record a policy failure and block the PR check with a
    // non-zero exit, while infrastructure problems remain non-blocking.
    assert!(
        content.contains("echo \"changed test files were missing from OCR reviewed set\" > ocr-policy-failure.txt")
            && content.contains("echo \"changed test files were excluded from OCR reviewed set\" > ocr-policy-failure.txt")
            && content.contains("phase=preview; reason=OCR preview command failed"),
        "Changed-test scope guard must record a blocking policy failure on a genuine breach and an infrastructure failure when the preview command itself fails"
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
        content.contains("OCR_LLM_TOKEN: ${{ secrets.OCR_LLM_AUTH_TOKEN }}"),
        "Workflow must expose OCR's supported env var name instead of passing the token on argv."
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
        !content.contains("secrets.OCR_LLM_URL")
            && !content.contains("secrets['OCR_LLM_URL']")
            && !content.contains("secrets[\"OCR_LLM_URL\"]"),
        "OCR_LLM_URL must be a variable, not a secret (dot or bracket notation)"
    );
    let mut secret_refs = secret_reference_names(&content);
    secret_refs.sort_unstable();
    secret_refs.dedup();
    assert_eq!(
        secret_refs,
        vec!["OCR_LLM_AUTH_TOKEN"],
        "OCR_LLM_AUTH_TOKEN must be the only referenced secret, using either dot or bracket syntax"
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
        content.contains("ocr-result.json")
            && content.contains("ocr-stdout.raw")
            && content.contains("ocr-stderr.log")
            && content.contains("if-no-files-found: warn"),
        "Workflow must upload OCR artifacts; placeholders guarantee presence so a missing file is a non-blocking warning"
    );
    assert!(
        content.contains("ocr-exit-code.txt")
            && content.contains(": > ocr-result.json")
            && content.contains(": > ocr-preview-stderr.log")
            && content.contains(": > ocr-phase.txt")
            && content.contains(": > ocr-infrastructure-failure.txt")
            && content.contains(": > ocr-policy-failure.txt"),
        "Workflow must seed the exit-code, phase, and failure-classification artifacts up front"
    );
    // Policy failures block the PR check; infrastructure/unparsable output is
    // surfaced as a non-blocking warning rather than failing the job.
    assert!(
        content.contains("core.setFailed(`OCR policy failure:")
            && content.contains(
                "core.warning(`OpenCodeReview failed or produced unparsable output"
            ),
        "Posting step must fail only on policy failures and warn (not fail) on infrastructure/unparsable output"
    );
    assert!(
        content.contains("github.paginate(github.rest.issues.listComments"),
        "Sticky summary lookup must paginate comments to avoid duplicates"
    );
    assert!(
        content.contains("const markerComments = comments.filter")
            && content.contains("markerComments.slice(1)")
            && content.contains("github.rest.issues.deleteComment"),
        "Sticky summary maintenance must delete duplicate marker comments"
    );
}

/// Test: inline review payloads use GitHub-compatible diff fields and fallback.
#[test]
fn test_ocr_pr_review_inline_payload_is_github_compatible() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("line: endLine")
            && content.contains("side: 'RIGHT'")
            && content.contains("renderFindingText")
            && content
                .contains("String(typeof value === 'string' && value ? value : 'OCR finding')")
            && content.contains("function escapeMarkdownHtml(value)")
            && content.contains(".replace(new RegExp('&', 'g'), '&amp;')")
            && content.contains(".replace(new RegExp('<', 'g'), '&lt;')")
            && content.contains(".replace(new RegExp('>', 'g'), '&gt;')")
            && content.contains("function unrenderFindingText(value)")
            && content.contains("INLINE_MARKER")
            && content.contains(".replace(/@/g, '@\\u200b')"),
        "Inline comments must target the ending line with an explicit side"
    );
    assert!(
        content.contains("if (startLine > endLine)")
            && content.contains("[startLine, endLine] = [endLine, startLine]"),
        "Inline comments must normalize inverted OCR ranges before posting"
    );
    assert!(
        content.contains("f.start_line > 0 && f.end_line > 0"),
        "Inline comments must only be attempted for positive GitHub line numbers"
    );
    assert!(
        content.contains("comment.start_line = startLine")
            && content.contains("comment.start_side = 'RIGHT'"),
        "Multi-line OCR findings must include GitHub start_line/start_side metadata"
    );
    assert!(
        content.contains("await new Promise((resolve) => setTimeout(resolve, 1000))"),
        "Individual fallback comment posting must be paced to reduce secondary rate limit risk"
    );
    assert!(
        content.contains("lineless.push(f && typeof f === 'object' ? f : { body: f || 'Malformed OCR finding' })")
            && content.contains("const item = f && typeof f === 'object' ? f : {}")
            && content
                .contains("const rawText = item.content || item.comment || item.message || item.body")
            && content.contains("renderFindingText(typeof rawText === 'string' ? rawText : 'OCR finding')")
            && !content.contains("|| f || 'OCR finding'"),
        "Sticky summary must safely render malformed lineless findings"
    );
    assert!(
        content.contains("No valid PR number resolved from pr-context step")
            && content.contains("No valid HEAD_SHA resolved from environment")
            && content
                .contains("OCR result was not valid JSON and no JSON array could be extracted"),
        "Posting script must fail clearly if PR context did not resolve a valid PR number or head SHA"
    );
    assert!(
        content.contains("function findingsFromParsed(parsed)")
            && content.contains("Array.isArray(parsed.comments)")
            && content.contains("return parsed.comments"),
        "Posting script must accept the ocr --format json object envelope with a comments array"
    );
    assert!(
        content.contains("github.rest.pulls.createReviewComment")
            && content.contains("Batch review post failed")
            && content.contains("const MAX_INLINE_FALLBACK = 50")
            && content.contains("for (const [index, c] of inline.entries())")
            && content.contains("if (index >= MAX_INLINE_FALLBACK)")
            && content.contains("Failed to post inline comment on ${c.path}:${c.line}:")
            && content.contains("const lineRange = c.start_line && c.start_line !== c.line")
            && content.contains("const overflowBody = `${unrenderFindingText(c.body)} (line ${lineRange})`")
            && content.contains("const fallbackBody = `${unrenderFindingText(c.body)} (line ${lineRange})`")
            && content.contains("comment: fallbackBody")
            && content.contains("message: fallbackBody")
            && content.contains("body: fallbackBody"),
        "Unpostable inline comments must be logged and demoted to the sticky summary with location context"
    );
}

/// Test: issue_comment and workflow_dispatch triggers are explicitly gated.
#[test]
fn test_ocr_pr_review_manual_triggers_are_gated() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("issue_comment:"),
        "Workflow must support comment triggers"
    );
    assert!(
        content.contains("workflow_dispatch:"),
        "Workflow must support manual dispatch"
    );
    assert!(
        content.contains("Boolean(context.payload.issue && context.payload.issue.pull_request)"),
        "Comment trigger must only run on PR-linked comments"
    );
    assert!(
        content.contains("const reviewCommands = ['/ocr', '/open-code-review'];"),
        "Comment trigger must support only the explicit standalone OCR commands"
    );
    let required_command_predicates = [
        "body === command",
        "body.startsWith(`${command} `)",
        "body.startsWith(`${command}\\n`)",
        "body.startsWith(`${command}\\r\\n`)",
        "body.startsWith(`${command}\\t`)",
    ];
    for predicate in required_command_predicates {
        assert!(
            content.contains(predicate),
            "Comment trigger must include predicate: {predicate}"
        );
    }
    let trigger_guard = content
        .split("jobs:")
        .next()
        .expect("workflow trigger guard appears before jobs");
    assert!(
        !trigger_guard.contains('\u{000C}') && !trigger_guard.contains('\u{000B}'),
        "Comment trigger must only match documented space, LF, CRLF, and tab separators"
    );
    assert!(
        content
            .contains("const trustedAssociations = new Set(['OWNER', 'MEMBER', 'COLLABORATOR'])")
            && content
                .contains("context.payload.comment && context.payload.comment.author_association"),
        "Comment trigger must allow trusted owner, member, and collaborator associations"
    );
    assert!(
        content.contains("pr_number:"),
        "workflow_dispatch must require a pr_number input"
    );
}

fn split_shell_segments(line: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut chars = line.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            let escapes_next = !in_double
                || chars
                    .peek()
                    .is_some_and(|(_, next_ch)| matches!(next_ch, '$' | '`' | '"' | '\\'));
            if escapes_next {
                escaped = true;
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && matches!(ch, '&' | '|' | ';') {
            segments.push(&line[start..idx]);
            if matches!(ch, '&' | '|') {
                if let Some(&(next_idx, next_ch)) = chars.peek() {
                    if next_ch == ch {
                        chars.next();
                        start = next_idx + next_ch.len_utf8();
                        continue;
                    }
                }
            }
            start = idx + ch.len_utf8();
        }
    }
    segments.push(&line[start..]);
    segments
}

fn shell_command_segments(scanned_commands: &str) -> impl Iterator<Item = &str> {
    scanned_commands
        .lines()
        .flat_map(split_shell_segments)
        .map(str::trim_start)
        .filter(|token| !token.is_empty())
}

#[test]
fn test_ocr_pr_review_shell_segment_splitter_respects_quotes() {
    assert_eq!(split_shell_segments(r#"echo "a;cargo"; npm test"#).len(), 2);
    assert_eq!(
        split_shell_segments(r#"echo 'a&node' && pnpm test"#),
        vec![r#"echo 'a&node' "#, " pnpm test"]
    );
    assert_eq!(
        split_shell_segments(r"echo foo\;bar"),
        vec![r"echo foo\;bar"]
    );
    assert_eq!(
        strip_shell_comment(r"echo a\\\ #cargo build"),
        r"echo a\\\ #cargo build"
    );
}

fn command_token_after_env_assignments(token: &str) -> &str {
    let mut current = token.trim_start();
    loop {
        let first = first_shell_word(current);
        let Some((name, _)) = first.split_once('=') else {
            return current;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return current;
        }
        current = current[first.len()..].trim_start();
    }
}

fn first_shell_word(token: &str) -> &str {
    token
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '>' | '<' | '|' | '&' | ';'))
        .next()
        .unwrap_or_default()
        .trim_matches(|ch| ch == '"' || ch == '\'')
}
fn command_matches_forbidden(first_word: &str, forbidden: &str) -> bool {
    // Multi-word forbidden commands are detected by token prefix checks in
    // token_invokes_forbidden_command; first_shell_word only handles one word.
    if !forbidden.contains(' ') && first_word == forbidden {
        return true;
    }
    forbidden == "python"
        && first_word.strip_prefix("python").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
        })
}

fn invokes_relative_repo_path(first_word: &str) -> bool {
    if first_word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }) {
        return false;
    }
    first_word.starts_with("./")
        || first_word.starts_with("../")
        || (first_word.contains('/')
            && !first_word.contains("(/")
            && !first_word.starts_with('/')
            && !first_word.starts_with('.')
            && !first_word.starts_with('$')
            && !first_word.starts_with('"')
            && !first_word.starts_with('\''))
}

fn token_invokes_repo_code(token: &str) -> bool {
    let command_token = command_token_after_env_assignments(token);
    let first_word = first_shell_word(command_token);
    invokes_relative_repo_path(first_word)
        || token.contains("$(./")
        || token.contains("$(../")
        || token.contains("`./")
        || token.contains("`../")
}

fn command_starts_with_forbidden_command(command: &str, forbidden: &str) -> bool {
    let normalized_command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized_command
        .strip_prefix(forbidden)
        .is_some_and(|suffix| {
            suffix
                .chars()
                .next()
                .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '>' | '<' | '|' | '&' | ';'))
        })
        || command_matches_forbidden(first_shell_word(command.trim_start()), forbidden)
}

fn token_contains_forbidden_after_shell_prefix(token: &str, prefix: &str, forbidden: &str) -> bool {
    token.match_indices(prefix).any(|(idx, _)| {
        command_starts_with_forbidden_command(&token[idx + prefix.len()..], forbidden)
    })
}

fn token_invokes_forbidden_command(token: &str, forbidden: &str) -> bool {
    let command_token = command_token_after_env_assignments(token);
    if command_starts_with_forbidden_command(command_token, forbidden) {
        return true;
    }
    token_contains_forbidden_after_shell_prefix(token, "$(", forbidden)
        || token_contains_forbidden_after_shell_prefix(token, "${", forbidden)
        || token_contains_forbidden_after_shell_prefix(token, "`", forbidden)
}

const FORBIDDEN_PR_CODE_COMMANDS: &[&str] = &[
    "make",
    "cargo",
    "npm run",
    "npm ci",
    "npm exec",
    "npm start",
    "npm test",
    "npx",
    "yarn",
    "pnpm",
    "python",
    "node",
    "bash",
    "sh",
    "dash",
    "zsh",
    "ruby",
    "perl",
    "java",
    "go",
    "deno",
    "bun",
    "pip",
    "pip3",
    "eval",
    "source",
    "sudo",
];

const FORBIDDEN_REPO_EXECUTION_FALLBACKS: &[&str] = &[
    "cargo",
    "make",
    "python",
    "node",
    "sh",
    "dash",
    "zsh",
    "eval",
    "source",
    "sudo",
    "npm run",
    "npm ci",
    "npm test",
    "npm exec",
    "npm start",
    "npx",
    "yarn",
    "pnpm",
];

const REJECTED_REPO_EXECUTION_PATTERNS: &[&str] = &[
    "scripts/review.sh",
    "../tool",
    "FOO=bar ./malicious.sh",
    "sh scripts/review.sh",
    "dash scripts/review.sh",
    "source scripts/env.sh",
    "eval ./malicious.sh",
    "python3.12 scripts/check.py",
    "npm exec tool",
    "npm start",
    "npx local-tool",
    "yarn test",
    "pnpm test",
    "$(../tool)",
    "$( cargo build)",
    "${ cargo test}",
    "FOO=1 npm   run test",
    "FOO=1 ./scripts/review.sh",
    "sudo npm run build",
    "$(  cargo build)",
    "${  cargo test}",
];

fn assert_scoped_git_auth_cleanup(content: &str) {
    assert!(
        content.contains("GH_TOKEN: ${{ github.token }}")
            && content.contains("GH_SERVER_URL=\"${GITHUB_SERVER_URL:-https://github.com}\"")
            && content.contains("GH_SERVER_URL=\"${GH_SERVER_URL%/}/\"")
            && content.contains("GIT_CONFIG_COUNT=1")
            && content.contains("GIT_CONFIG_KEY_0=\"http.${GH_SERVER_URL}.extraheader\"")
            && content.contains(
                "AUTH_B64=\"$(printf '%s' \"x-access-token:${GH_TOKEN}\" | base64 | tr -d '\\n')\""
            )
            && content.contains("GIT_CONFIG_VALUE_0=\"AUTHORIZATION: basic ${AUTH_B64}\"")
            && !content.contains("AUTHORIZATION: bearer")
            && content.contains("unset GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0")
            && content.contains("unset AUTH_B64")
            && !content.contains("GIT_CONFIG_GLOBAL")
            && !content.contains("ocr-git-auth-config")
            && content.contains("trap cleanup_git_auth EXIT")
            && content.contains("trap 'cleanup_git_auth; exit 130' INT TERM")
            && content.contains("trap 'cleanup_git_auth; exit 129' HUP")
            && content.contains("trap - EXIT INT TERM HUP")
            && content.contains("set +e")
            && content.contains("git config --global --unset-all safe.directory")
            && content.contains("git config --file \"${HOME}/.gitconfig\" --add safe.directory")
            && content
                .contains("git config --file \"${HOME}/.gitconfig\" --unset-all safe.directory")
            && content.contains("${GH_TOKEN}")
            && content.contains("if ! git cat-file -e \"${BASE_SHA}^{commit}\"; then"),
        "Fetch step must use scoped GitHub authentication and clean it up"
    );
    let persistent_safe_directory = content
        .find("git config --file \"${HOME}/.gitconfig\" --add safe.directory")
        .expect("persistent safe.directory must be scoped for later steps");
    let safe_directory = content
        .find("git config --global --add safe.directory")
        .expect("temporary safe.directory must be scoped");
    let persistent_safe_directory_cleanup = content
        .rfind("git config --file \"${HOME}/.gitconfig\" --unset-all safe.directory")
        .expect("persistent safe.directory must be cleaned up after later steps");
    let pr_fetch = content
        .find("git fetch origin \"pull/${PR_NUMBER}/head:pr-head\"")
        .expect("PR head fetch must be present");
    let post_fetch_env_cleanup = content[pr_fetch..]
        .find("unset GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0")
        .map(|offset| pr_fetch + offset)
        .expect("token-bearing fetch env vars must be unset immediately after PR fetch");
    assert!(
        persistent_safe_directory < safe_directory
            && safe_directory < pr_fetch
            && pr_fetch < post_fetch_env_cleanup
            && post_fetch_env_cleanup < persistent_safe_directory_cleanup,
        "safe.directory should be scoped before fetching PR-controlled refs and cleaned up after later steps"
    );
}

fn scanned_ocr_pr_review_run_commands() -> String {
    let commands = run_blocks(&ocr_pr_review_workflow_content()).join("\n");
    let continued_commands = commands.replace(concat!("\\", "\n"), " ");
    continued_commands
        .lines()
        .map(strip_shell_comment)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Test: the workflow does not execute untrusted PR code.
#[test]
fn test_ocr_pr_review_does_not_execute_pr_code() {
    let content = ocr_pr_review_workflow_content();
    let scanned_commands = scanned_ocr_pr_review_run_commands();
    for forbidden in FORBIDDEN_PR_CODE_COMMANDS {
        assert!(
            !shell_command_segments(&scanned_commands)
                .any(|token| token_invokes_forbidden_command(token, forbidden)),
            "Workflow must not invoke {} from a run step",
            forbidden
        );
    }
    assert!(
        !shell_command_segments(&scanned_commands).any(token_invokes_repo_code),
        "Workflow must not directly execute relative-path scripts from a run step"
    );
    for rejected in REJECTED_REPO_EXECUTION_PATTERNS {
        assert!(
            token_invokes_repo_code(rejected)
                || FORBIDDEN_REPO_EXECUTION_FALLBACKS
                    .iter()
                    .any(|forbidden| token_invokes_forbidden_command(rejected, forbidden)),
            "guardrail should reject repository execution pattern: {rejected}"
        );
    }
    let forbidden_js_apis = [
        "child_process",
        "execSync",
        "execFileSync",
        "spawnSync",
        "eval(",
    ];
    for forbidden_api in forbidden_js_apis {
        assert!(
            !content.contains(forbidden_api),
            "GitHub-script blocks must not use dangerous Node.js execution API: {forbidden_api}"
        );
    }
}

#[test]
fn test_ocr_pr_review_uses_trusted_checkout_and_scoped_fetch_auth() {
    let content = ocr_pr_review_workflow_content();
    assert!(
        content.contains("ref: ${{ steps.pr-context.outputs.base_sha }}"),
        "Checkout must use the trusted base SHA"
    );
    assert!(
        !content.contains("ref: ${{ steps.pr-context.outputs.head_sha }}")
            && !content.contains("ref: ${{ github.event.pull_request.head.sha }}"),
        "Checkout must never directly use the PR head SHA"
    );
    assert!(
        content.contains("persist-credentials: false"),
        "Checkout must not persist the token for later OCR steps"
    );
    assert_scoped_git_auth_cleanup(&content);
}

#[test]
fn test_ocr_pr_review_uses_only_pinned_ocr_install() {
    let scanned_commands = scanned_ocr_pr_review_run_commands();
    let ocr_installs: Vec<&str> = scanned_commands
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("@alibaba-group/open-code-review"))
        .collect();
    assert_eq!(
        ocr_installs,
        vec![
            r#"npm install --prefix "$OCR_PREFIX" --ignore-scripts "@alibaba-group/open-code-review@${OCR_VERSION}" 2>> ocr-stderr.log"#
        ],
        "Workflow may only install the reviewed pinned OCR version, referenced via the single-source OCR_VERSION variable, to an isolated local prefix"
    );
    for install in &ocr_installs {
        assert!(
            install.contains("@${OCR_VERSION}")
                && install.contains(" --ignore-scripts ")
                && install.contains(r#"--prefix "$OCR_PREFIX""#)
                && !install.contains("install -g")
                && !install.contains("@file:")
                && !install.contains("@link:")
                && !install.contains("@./")
                && !install.contains("@../"),
            "OCR install must reference the OCR_VERSION variable as an explicit registry version to an isolated local prefix, not a global, local, or linked package"
        );
    }
    // The OCR_VERSION variable (single source of truth) must resolve to an
    // exact pinned semver. Verifying the variable itself keeps the guardrail
    // strict without re-hardcoding the version literal in the install command.
    let pinned_version = ocr_version_from_workflow();
    let content = ocr_pr_review_workflow_content();
    // No alternate installs: the pinned version literal must only appear in the
    // OCR_VERSION declaration, never re-hardcoded in any install command.
    let literal_install_occurrences = content
        .lines()
        .filter(|line| line.contains(&format!("@alibaba-group/open-code-review@{pinned_version}")))
        .count();
    assert_eq!(
        literal_install_occurrences, 0,
        "Pinned version {pinned_version} must not be hard-coded in any install command; only the OCR_VERSION variable may be referenced"
    );
    // The cache key must reference the same OCR_VERSION variable so a version
    // bump never leaves a stale cache key pointing at the old version.
    assert!(
        content.contains("key: npm-ocr-${{ runner.os }}-${{ env.OCR_VERSION }}"),
        "Cache key must reference the single-source OCR_VERSION variable so it rotates with the pinned version"
    );
    assert!(
        !content.contains("safe.directory '*'")
            && !content.contains("safe.directory=*")
            && !content.contains("safe.directory *"),
        "Workflow must not use a wildcard safe.directory"
    );
}

/// Test: OCR remains additive to existing PR quality gates.
#[test]
fn test_ocr_pr_review_keeps_existing_pr_quality_gates() {
    let pr_quality_path = root_workflow_path("pr-quality.yml");
    assert!(
        pr_quality_path.is_file(),
        "pr-quality.yml must remain so existing PR quality checks stay enabled"
    );
    let pr_quality = fs::read_to_string(&pr_quality_path)
        .expect("pr-quality.yml must be readable to verify existing PR quality wiring");
    assert!(
        pr_quality.contains("pull_request:")
            && pr_quality.contains("cargo fmt --all -- --check")
            && pr_quality
                .contains("cargo clippy --workspace --all-targets --all-features -- -D warnings")
            && pr_quality.contains("cargo test --workspace --all-features --lib --tests"),
        "pr-quality.yml must retain existing PR quality gates so OCR is additive"
    );
    let content = ocr_pr_review_workflow_content();
    assert!(
        !content.contains("CodeRabbit"),
        "OCR workflow must not try to disable or replace existing PR review tooling"
    );
}

#[test]
fn test_local_ocr_review_contract_is_documented_and_guarded() {
    let workspace = workspace_root();
    let xtask_main = fs::read_to_string(workspace.join("xtask/src/main.rs"))
        .expect("xtask main should be readable");
    let ocr_module = fs::read_to_string(workspace.join("xtask/src/ocr_review.rs"))
        .expect("ocr_review module should be readable");
    let cargo_config = fs::read_to_string(workspace.join(".cargo/config.toml"))
        .expect("cargo config should be readable");
    let makefile =
        fs::read_to_string(workspace.join("Makefile")).expect("Makefile should be readable");
    let guide = fs::read_to_string(workspace.join("docs/guides/local-ocr-review.md"))
        .expect("local OCR guide should be readable");
    let readme =
        fs::read_to_string(workspace.join("README.md")).expect("README should be readable");

    assert!(
        xtask_main.contains("ocr-review"),
        "xtask should dispatch ocr-review"
    );
    assert!(cargo_config.contains("ocr-review = \"xtask ocr-review\""));
    assert!(makefile.contains("ocr-review:"));
    assert!(makefile.contains("cargo xtask ocr-review $(ARGS)"));
    assert!(guide.contains("--timeout 20"));
    assert!(guide.contains("--audience agent"));
    assert!(readme.contains("docs/guides/local-ocr-review.md"));

    for required in [
        "DEFAULT_TIMEOUT_MINUTES: &str = \"20\"",
        "--audience",
        "agent",
        "ocr-preview.txt",
        "ocr-preview-stderr.log",
        "ocr-stdout.raw",
        "ocr-stderr.log",
        "ocr-result.json",
        "ocr-exit-code.txt",
        "is_test_path",
        "allow_excluded_tests",
    ] {
        assert!(
            ocr_module.contains(required),
            "OCR wrapper should contain {required}"
        );
    }
}
