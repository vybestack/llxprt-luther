//! The schema no longer requires a repository section.
//!
//! Issue #204 (B3) names this the falsifiable core: while
//! `WorkflowConfig.repo` was mandatory, a workflow with nothing to do with
//! version control could not be *expressed*, so "the engine is generic" was
//! false at the point of parsing — before any executor ran, before any
//! registry was composed. No amount of decoupling further down could recover
//! genericity the schema had already denied.

use luther_workflow::workflow::config_loader::parse_workflow_config_toml;

/// A config that declares no repository at all.
const NO_REPOSITORY: &str = r#"
config_id = "core-only"
workflow_type_id = "core-only-type"

[runtime]
timeout_seconds = 7200
max_retries = 3
parallel_steps = 1
log_level = "info"

[guards]
max_iterations = 10
max_file_changes = 50
max_tokens = 100000
max_cost = 10.00
"#;

/// The same config with a repository section, to hold the comparison fixed.
const WITH_REPOSITORY: &str = r#"
config_id = "repo-driven"
workflow_type_id = "repo-driven-type"

[runtime]
timeout_seconds = 7200
max_retries = 3
parallel_steps = 1
log_level = "info"

[repository]
workspace_strategy = "temp_clone"
branch_template = "luther-fix-{issue_number}"
base_branch = "main"

[guards]
max_iterations = 10
max_file_changes = 50
max_tokens = 100000
max_cost = 10.00
"#;

#[test]
fn a_config_with_no_repository_section_parses() {
    let config = parse_workflow_config_toml(NO_REPOSITORY)
        .expect("a workflow that drives no repository must be expressible");
    assert!(
        config.repo.is_none(),
        "the absent section must stay absent rather than being filled with defaults; \
         a fabricated repository would let a repository-driven step run against \
         invented data instead of failing"
    );
    assert_eq!(config.config_id, "core-only");
}

#[test]
fn a_config_with_a_repository_section_still_parses_it() {
    let config = parse_workflow_config_toml(WITH_REPOSITORY)
        .expect("existing repository-driven configs must be unaffected");
    let repo = config
        .repo
        .as_ref()
        .expect("a declared repository section must survive parsing");
    assert_eq!(repo.workspace_strategy, "temp_clone");
    assert_eq!(repo.branch_template, "luther-fix-{issue_number}");
}

/// An empty required field is still rejected when the section is present.
///
/// Making the section optional must not weaken the rules that apply once it
/// is declared. Without this, "no repository section" and "a malformed
/// repository section" would become the same permissive case, and the change
/// would have relaxed validation rather than relocated it.
#[test]
fn a_declared_repository_is_still_validated() {
    let malformed = WITH_REPOSITORY.replace(
        r#"workspace_strategy = "temp_clone""#,
        r#"workspace_strategy = """#,
    );
    // `parse_workflow_config_toml` validates as part of loading, so the
    // rejection surfaces here rather than from a separate validation call.
    let error = parse_workflow_config_toml(&malformed)
        .expect_err("an empty workspace_strategy must still be rejected");
    assert!(
        error.message.contains("workspace_strategy"),
        "the diagnostic must name the offending field, got: {}",
        error.message
    );
}

/// A repo-less config validates rather than merely parsing.
///
/// Parsing alone would be a hollow result: validation ran the repository
/// rules unconditionally, so a config that parsed would still have been
/// rejected — or panicked — a moment later.
#[test]
fn a_config_with_no_repository_section_also_validates() {
    // Parsing performs validation, so a successful parse of a repo-less
    // config is the acceptance criterion: the repository rules did not fire.
    let config = parse_workflow_config_toml(NO_REPOSITORY)
        .expect("a config that declares no repository must pass validation, not just parsing");
    assert!(config.repo.is_none());
}
