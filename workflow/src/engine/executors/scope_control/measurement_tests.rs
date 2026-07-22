//! Tests for [`super::measurement`].

use super::*;
use crate::engine::workspace_ownership::WORKSPACE_OWNER_MARKER;

#[test]
fn split_z_handles_empty() {
    assert!(split_z(&[]).is_empty());
}

#[test]
fn split_z_single_nul() {
    assert!(split_z(&[0]).is_empty());
}

#[test]
fn split_z_multiple_segments() {
    let data = b"foo\0bar\0baz\0";
    let result = split_z(data);
    assert_eq!(result, vec![b"foo".as_ref(), b"bar".as_ref(), b"baz"]);
}

#[test]
fn split_z_preserves_spaces_in_paths() {
    let data = b"file with spaces.rs\0other\0";
    let result = split_z(data);
    assert_eq!(result, vec![b"file with spaces.rs".as_ref(), b"other"]);
}

#[test]
fn parse_z_paths_basic() {
    let data = b"src/a.rs\0src/b.rs\0";
    let paths = parse_z_paths(data).expect("parse paths");
    assert_eq!(paths, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
}

#[test]
fn parse_z_paths_with_spaces() {
    let data = b"src/my file.rs\0other.txt\0";
    let paths = parse_z_paths(data).expect("parse paths");
    assert_eq!(
        paths,
        vec!["src/my file.rs".to_string(), "other.txt".to_string()]
    );
}

#[test]
fn parse_z_paths_rejects_non_utf8() {
    let error = parse_z_paths(b"valid\0invalid\xff\0").expect_err("must fail closed");
    assert!(error.to_string().contains("non-UTF-8 path"));
}

#[test]
fn parse_name_status_z_basic() {
    let data = b"M\0src/a.rs\0A\0src/b.rs\0";
    let result = parse_name_status_z(data).expect("parse");
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].0, "M");
    assert_eq!(result[0].1, "src/a.rs");
    assert_eq!(result[1].0, "A");
    assert_eq!(result[1].1, "src/b.rs");
}

#[test]
fn parse_name_status_z_rejects_odd_segments() {
    let data = b"M\0src/a.rs\0A\0";
    let result = parse_name_status_z(data);
    assert!(result.is_err());
}

#[test]
fn parse_numstat_z_basic() {
    let data = b"10\t0\tsrc/a.rs\0-\t-\tbinary.dat\0";
    let result = parse_numstat_z(data).expect("parse");
    assert_eq!(result.len(), 2);
    assert!(matches!(result[0].added, NumstatCount::Lines(10)));
    assert!(matches!(result[0].deleted, NumstatCount::Lines(0)));
    assert_eq!(result[0].path, "src/a.rs");
    assert!(matches!(result[1].added, NumstatCount::Binary));
    assert!(matches!(result[1].deleted, NumstatCount::Binary));
    assert_eq!(result[1].path, "binary.dat");
}

#[test]
fn parse_numstat_with_spaces_in_path() {
    let data = b"5\t0\tmy file.rs\0";
    let result = parse_numstat_z(data).expect("parse");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].path, "my file.rs");
}

#[test]
fn merge_status_and_numstat_matches() {
    let statuses = vec![
        ("M".to_string(), "src/a.rs".to_string()),
        ("A".to_string(), "src/b.rs".to_string()),
    ];
    let numstats = vec![
        NumstatEntry {
            added: NumstatCount::Lines(10),
            deleted: NumstatCount::Lines(0),
            path: "src/a.rs".into(),
        },
        NumstatEntry {
            added: NumstatCount::Lines(5),
            deleted: NumstatCount::Lines(0),
            path: "src/b.rs".into(),
        },
    ];
    let result = merge_status_and_numstat(&statuses, &numstats).expect("merge");
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].path, "src/a.rs");
    assert_eq!(result[0].status, ChangeStatus::Modified);
    assert_eq!(result[0].added_lines, Some(10));
}

#[test]
fn merge_status_and_numstat_binary_file() {
    let statuses = vec![("A".to_string(), "binary.dat".to_string())];
    let numstats = vec![NumstatEntry {
        added: NumstatCount::Binary,
        deleted: NumstatCount::Binary,
        path: "binary.dat".into(),
    }];
    let result = merge_status_and_numstat(&statuses, &numstats).expect("merge");
    assert_eq!(result.len(), 1);
    assert!(result[0].is_binary);
    assert_eq!(result[0].added_lines, None);
}

#[test]
fn merge_status_and_numstat_mismatch_count_errors() {
    let statuses = vec![("M".to_string(), "a.rs".to_string())];
    let numstats = vec![];
    assert!(merge_status_and_numstat(&statuses, &numstats).is_err());
}

#[test]
fn merge_status_and_numstat_path_mismatch_errors() {
    let statuses = vec![("M".to_string(), "a.rs".to_string())];
    let numstats = vec![NumstatEntry {
        added: NumstatCount::Lines(1),
        deleted: NumstatCount::Lines(0),
        path: "b.rs".into(),
    }];
    assert!(merge_status_and_numstat(&statuses, &numstats).is_err());
}

#[test]
fn change_status_from_letter() {
    assert_eq!(ChangeStatus::from_letter("A").unwrap(), ChangeStatus::Added);
    assert_eq!(
        ChangeStatus::from_letter("M").unwrap(),
        ChangeStatus::Modified
    );
    assert_eq!(
        ChangeStatus::from_letter("D").unwrap(),
        ChangeStatus::Deleted
    );
    assert_eq!(
        ChangeStatus::from_letter("T").unwrap(),
        ChangeStatus::Modified
    );
    assert!(ChangeStatus::from_letter("X").is_err());
}

#[test]
fn change_status_is_new_file() {
    assert!(ChangeStatus::Added.is_new_file());
    assert!(ChangeStatus::Untracked.is_new_file());
    assert!(!ChangeStatus::Modified.is_new_file());
    assert!(!ChangeStatus::Deleted.is_new_file());
}

#[test]
fn has_source_extension_matches() {
    let exts = vec!["rs".to_string(), "toml".to_string()];
    assert!(has_source_extension("src/main.rs", &exts));
    assert!(has_source_extension("Cargo.toml", &exts));
    assert!(!has_source_extension("README.md", &exts));
}

#[test]
fn count_new_modules_filters_by_extension_and_status() {
    let changes = vec![
        FileChange {
            path: "src/new.rs".into(),
            status: ChangeStatus::Added,
            added_lines: Some(10),
            deleted_lines: Some(0),
            is_binary: false,
        },
        FileChange {
            path: "src/modified.rs".into(),
            status: ChangeStatus::Modified,
            added_lines: Some(5),
            deleted_lines: Some(0),
            is_binary: false,
        },
        FileChange {
            path: "src/untracked.rs".into(),
            status: ChangeStatus::Untracked,
            added_lines: Some(3),
            deleted_lines: Some(0),
            is_binary: false,
        },
        FileChange {
            path: "src/new.txt".into(),
            status: ChangeStatus::Added,
            added_lines: Some(1),
            deleted_lines: Some(0),
            is_binary: false,
        },
    ];
    let config = test_measurement_config(&["rs"], &[]);
    assert_eq!(count_new_modules(&changes, &config), 2); // new.rs + untracked.rs
}

#[test]
fn total_added_lines_sums_correctly() {
    let changes = vec![
        FileChange {
            path: "a.rs".into(),
            status: ChangeStatus::Added,
            added_lines: Some(10),
            deleted_lines: Some(0),
            is_binary: false,
        },
        FileChange {
            path: "b.dat".into(),
            status: ChangeStatus::Added,
            added_lines: None,
            deleted_lines: None,
            is_binary: true,
        },
        FileChange {
            path: "c.rs".into(),
            status: ChangeStatus::Modified,
            added_lines: Some(5),
            deleted_lines: Some(2),
            is_binary: false,
        },
    ];
    assert_eq!(total_added_lines(&changes), 15);
}

#[test]
fn is_path_within_checks_prefix() {
    assert!(is_path_within("src/core/foo.rs", "src/core"));
    assert!(is_path_within("src/core", "src/core"));
    assert!(!is_path_within("src/coreutils/foo.rs", "src/core"));
    assert!(!is_path_within("src/other/bar.rs", "src/core"));
}

#[test]
fn compute_changed_subsystems_matches_paths() {
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    let draft = TaskCharterDraft {
        charter_id: "T".into(),
        issue_number: 1,
        run_id: "r".into(),
        merge_base: "abc".into(),
        acceptance_criteria: vec!["AC".into()],
        non_goals: vec!["NG".into()],
        subsystems: vec![
            DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core".into()],
            },
            DraftSubsystem {
                id: "cli".into(),
                paths: vec!["src/cli".into()],
            },
        ],
        budget: DraftBudget {
            max_files_changed: 10,
            max_added_lines: 100,
            max_new_modules: 5,
            max_dependencies_added: 0,
            max_public_apis_added: 5,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        },
        mandatory_gates: vec!["cargo test".into()],
    };
    let charter = normalize_charter(&draft);
    let paths = vec!["src/core/foo.rs".to_string(), "README.md".to_string()];
    let subs = compute_changed_subsystems(&paths, &charter);
    assert_eq!(subs, vec!["core".to_string()]);
}

#[test]
fn owned_workspace_marker_is_excluded_but_other_luther_files_are_measured() {
    let workspace = tempfile::tempdir().expect("workspace");
    crate::engine::continuation::write_workspace_owner_marker(workspace.path(), "run-owned")
        .expect("owner marker");
    let other = workspace.path().join(".luther/agent-note");
    std::fs::write(&other, "agent content").expect("agent file");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![WORKSPACE_OWNER_MARKER.into(), ".luther/agent-note".into()],
    };

    let files = patch_untracked_files(&data, workspace.path(), "run-owned", true).expect("filter");
    assert_eq!(files, vec![".luther/agent-note".to_string()]);
}

#[test]
fn malformed_or_foreign_workspace_marker_fails_closed() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".luther")).expect("luther dir");
    std::fs::write(
        workspace.path().join(WORKSPACE_OWNER_MARKER),
        "different-run",
    )
    .expect("foreign marker");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![WORKSPACE_OWNER_MARKER.into()],
    };

    let error = patch_untracked_files(&data, workspace.path(), "run-owned", true)
        .expect_err("foreign marker must fail closed");
    assert!(
        error.to_string().contains("different-run"),
        "foreign marker should identify the conflicting claim, got: {error}"
    );
}

#[test]
fn listed_but_missing_workspace_marker_fails_closed() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![WORKSPACE_OWNER_MARKER.into()],
    };

    let error = patch_untracked_files(&data, workspace.path(), "run-owned", true)
        .expect_err("missing marker must fail closed");
    assert!(
        error.to_string().contains("missing"),
        "missing marker should fail closed with a missing reason, got: {error}"
    );
}

#[test]
fn daemon_measurement_requires_marker_even_when_git_does_not_list_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![],
    };

    let error = patch_untracked_files(&data, workspace.path(), "run-owned", true)
        .expect_err("daemon marker must be verified independently of git output");
    assert!(
        error.to_string().contains("missing"),
        "daemon marker must be verified independently of git output, got: {error}"
    );
}

#[test]
fn non_daemon_measurement_does_not_require_workspace_marker() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![WORKSPACE_OWNER_MARKER.into(), "README.md".into()],
    };

    let files = patch_untracked_files(&data, workspace.path(), "run-owned", false)
        .expect("ordinary runs do not own daemon control metadata");
    assert_eq!(
        files,
        vec![WORKSPACE_OWNER_MARKER.to_string(), "README.md".to_string()]
    );
}

#[test]
fn durable_only_workspace_is_trusted_for_scope_exclusion() {
    // A workspace whose bootstrap marker was deleted by agent cleanup but
    // whose durable marker remains must still be trusted for scope exclusion.
    // The durable evidence lives under .git and is naturally invisible to
    // scope measurement, so only the bootstrap marker (if present) appears in
    // the untracked list and would be excluded.
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".git/luther")).expect("durable dir");
    std::fs::write(
        workspace.path().join(".git/luther/workspace-owner"),
        "run-owned",
    )
    .expect("durable marker");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        // Bootstrap marker absent from the untracked list because it was
        // deleted by agent cleanup; durable marker is invisible to git.
        untracked_files: vec!["README.md".into()],
    };
    let files = patch_untracked_files(&data, workspace.path(), "run-owned", true)
        .expect("durable-only workspace must be trusted");
    assert_eq!(files, vec!["README.md".to_string()]);
}

#[test]
fn durable_only_workspace_excludes_bootstrap_when_listed() {
    // Even with durable-only evidence, if the bootstrap marker somehow appears
    // in the untracked list, it is excluded because ownership is verified via
    // the durable record.
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".git/luther")).expect("durable dir");
    std::fs::write(
        workspace.path().join(".git/luther/workspace-owner"),
        "run-owned",
    )
    .expect("durable marker");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![WORKSPACE_OWNER_MARKER.into(), "src/main.rs".into()],
    };
    let files = patch_untracked_files(&data, workspace.path(), "run-owned", true)
        .expect("durable-only workspace must be trusted");
    assert_eq!(files, vec!["src/main.rs".to_string()]);
}
fn run_git(workspace: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .status()
        .expect("run git");
    assert!(status.success(), "git command failed: {args:?}");
}

fn initialized_measurement_repo() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("workspace");
    run_git(workspace.path(), &["init", "-q"]);
    run_git(
        workspace.path(),
        &["config", "user.email", "test@example.com"],
    );
    run_git(workspace.path(), &["config", "user.name", "Test"]);
    std::fs::write(workspace.path().join("tracked.txt"), "base\n").expect("tracked");
    run_git(workspace.path(), &["add", "tracked.txt"]);
    run_git(workspace.path(), &["commit", "-qm", "base"]);
    workspace
}

fn owned_measurement_charter(head: String) -> CanonicalTaskCharter {
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    normalize_charter(&TaskCharterDraft {
        charter_id: "owned".into(),
        issue_number: 154,
        run_id: "run-owned".into(),
        merge_base: head,
        acceptance_criteria: vec!["measure".into()],
        non_goals: vec!["none".into()],
        subsystems: vec![DraftSubsystem {
            id: "luther".into(),
            paths: vec![".luther".into()],
        }],
        budget: DraftBudget {
            max_files_changed: 5,
            max_added_lines: 20,
            max_new_modules: 1,
            max_dependencies_added: 0,
            max_public_apis_added: 0,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 1,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 1,
        },
        mandatory_gates: vec!["test".into()],
    })
}

#[test]
fn collector_measures_ignored_luther_sibling_and_owned_marker_is_digest_neutral() {
    let workspace = initialized_measurement_repo();
    std::fs::write(workspace.path().join(".gitignore"), ".luther/\n").expect("ignore");
    crate::engine::continuation::write_workspace_owner_marker(workspace.path(), "run-owned")
        .expect("owner marker");
    std::fs::write(workspace.path().join(".luther/agent-note"), "agent content")
        .expect("agent note");
    let head = resolve_head_sha(workspace.path()).expect("head");
    let config = test_measurement_config(&["rs"], &[]);
    let data = SystemGitPatchCollector
        .collect(workspace.path(), &head, &config)
        .expect("collect");
    assert!(data
        .untracked_files
        .contains(&WORKSPACE_OWNER_MARKER.to_string()));
    assert!(data
        .untracked_files
        .contains(&".luther/agent-note".to_string()));

    let charter = owned_measurement_charter(head);
    let first = compute_measurement(
        &data,
        &charter,
        "run-owned",
        true,
        &config,
        workspace.path(),
        &[],
    )
    .expect("measure");
    let mut reversed = data.clone();
    reversed.untracked_files.reverse();
    let second = compute_measurement(
        &reversed,
        &charter,
        "run-owned",
        true,
        &config,
        workspace.path(),
        &[],
    )
    .expect("measure reversed");
    assert_eq!(first, second);
    assert_eq!(
        first.changed_paths,
        vec![".gitignore", ".luther/agent-note"]
    );
    assert!(!first
        .file_details
        .iter()
        .any(|change| change.path == WORKSPACE_OWNER_MARKER));
}

#[test]
fn measurement_rejects_charter_for_different_active_run() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = GitPatchData {
        head_sha: "abc".into(),
        divergence: 0,
        tracked_changes: vec![],
        untracked_files: vec![],
    };
    let mut charter = test_charter_for_run("charter-run");
    charter.merge_base = "abc".into();
    let error = compute_measurement(
        &data,
        &charter,
        "active-run",
        false,
        &test_measurement_config(&[], &[]),
        workspace.path(),
        &[],
    )
    .expect_err("run mismatch must fail");
    assert!(error.to_string().contains("does not match active run"));
}

fn test_charter_for_run(
    run_id: &str,
) -> crate::engine::executors::scope_control::model::CanonicalTaskCharter {
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, TaskCharterDraft,
    };
    normalize_charter(&TaskCharterDraft {
        charter_id: "test".into(),
        issue_number: 154,
        run_id: run_id.into(),
        merge_base: "abc".into(),
        acceptance_criteria: vec!["test".into()],
        non_goals: vec!["none".into()],
        subsystems: vec![],
        budget: DraftBudget {
            max_files_changed: 5,
            max_added_lines: 20,
            max_new_modules: 1,
            max_dependencies_added: 0,
            max_public_apis_added: 0,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 1,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 1,
        },
        mandatory_gates: vec!["test".into()],
    })
}

#[test]
fn extract_dependency_keys_toml() {
    let content = r#"
[dependencies]
serde = "1"
regex = "1.11"

[dev-dependencies]
tempfile = "3"

[other]
not_a_dep = true
"#;
    let deps = extract_dependency_keys(content, &["dependencies".to_string()])
        .expect("parse dependencies");
    assert!(deps.contains(&"serde".to_string()));
    assert!(deps.contains(&"regex".to_string()));
    assert!(!deps.contains(&"tempfile".to_string()));
}

#[test]
fn extract_dependency_keys_dotted_path() {
    let content = r#"
[target."cfg(unix)".dependencies]
libc = "0.2"
"#;
    let section = r#"target."cfg(unix)".dependencies"#;
    let deps = extract_dependency_keys(content, &[section.to_string()])
        .expect("parse target dependencies");
    assert!(deps.contains(&"libc".to_string()));
}
