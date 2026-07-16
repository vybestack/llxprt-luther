//! Integration tests for issue 142, slice 1: task-charter config/model/
//! validation/persistence/executor registration.
//!
//! These prove:
//! - invalid config rejection (scope-control config validation);
//! - stable digest (canonical normalization determinism);
//! - budget/subsystem rejection (draft validation against configured ceilings);
//! - atomic immutable persistence (temp-file + rename, refuse overwrite);
//! - executor observable artifacts (paths, digest, merge base in context);
//! - executor registration (step type `"task_charter"` in default registry).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::executors::scope_control::{
    normalize_charter, validate_draft_against_config, DraftBudget, DraftReviewCaps, DraftSubsystem,
    MergeBaseError, MergeBaseProbe, ScopePersistenceError, ScopeStatus, TaskCharterDraft,
    TaskCharterExecutor,
};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::workflow::config_loader::validate_workflow_config;
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardLimits, RepoConfig, RuntimeConfig, ScopeBudgetConfig,
    ScopeControlConfig, ScopeReviewCapsConfig, ScopeSubsystemConfig, TargetProfileConfig,
    WorkflowConfig,
};
use serde_json::json;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct FixedMergeBaseProbe {
    sha: String,
}

impl MergeBaseProbe for FixedMergeBaseProbe {
    fn resolve_merge_base(
        &self,
        _work_dir: &Path,
        _base_branch: &str,
    ) -> Result<String, MergeBaseError> {
        Ok(self.sha.clone())
    }
}

fn valid_scope_control() -> ScopeControlConfig {
    ScopeControlConfig {
        enabled: true,
        budget: ScopeBudgetConfig {
            max_files_changed: 10,
            max_added_lines: 500,
            max_new_modules: 3,
            max_dependencies_added: 0,
            max_public_apis_added: 5,
        },
        review_caps: ScopeReviewCapsConfig {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        },
        subsystems: vec![ScopeSubsystemConfig {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        dependency_manifests: vec![],
        mandatory_command_groups: vec![],
        partial_compile_command: None,
        partial_compile_group: None,
        measurement: Default::default(),
        mandatory_gates: vec!["cargo test".into()],
    }
}

fn workflow_config(scope_control: ScopeControlConfig) -> WorkflowConfig {
    WorkflowConfig {
        config_id: "test".into(),
        workflow_type_id: "test-type".into(),
        runtime: RuntimeConfig {
            timeout_seconds: 60,
            max_retries: 1,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".into(),
            branch_template: "issue{n}".into(),
            base_branch: Some("main".into()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(1),
            max_file_changes: None,
            max_tokens: None,
            max_cost: None,
        },
        variables: BTreeMap::new()
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: Some(TargetProfileConfig {
            scope_control,
            ..Default::default()
        }),
    }
}

fn sample_draft() -> TaskCharterDraft {
    TaskCharterDraft {
        charter_id: "ISSUE-142".into(),
        issue_number: 142,
        run_id: "run-abc".into(),
        merge_base: "abc123".into(),
        acceptance_criteria: vec!["AC-142-01".into(), "AC-142-02".into()],
        non_goals: vec!["no redesign".into()],
        subsystems: vec![DraftSubsystem {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        budget: DraftBudget {
            max_files_changed: 5,
            max_added_lines: 200,
            max_new_modules: 2,
            max_dependencies_added: 0,
            max_public_apis_added: 3,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        },
        mandatory_gates: vec!["cargo test".into()],
    }
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

#[test]
fn invalid_config_zero_file_budget_rejected() {
    let sc = ScopeControlConfig {
        budget: ScopeBudgetConfig {
            max_files_changed: 0,
            ..valid_scope_control().budget
        },
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    let err = validate_workflow_config(&config).unwrap_err();
    assert!(err.message.contains("max_files_changed"));
}

#[test]
fn invalid_config_zero_review_cap_rejected() {
    let sc = ScopeControlConfig {
        review_caps: ScopeReviewCapsConfig {
            max_delta_reviews: 0,
            ..valid_scope_control().review_caps
        },
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    let err = validate_workflow_config(&config).unwrap_err();
    assert!(err.message.contains("max_delta_reviews"));
}

#[test]
fn invalid_config_duplicate_subsystem_rejected() {
    let sc = ScopeControlConfig {
        subsystems: vec![
            ScopeSubsystemConfig {
                id: "dup".into(),
                paths: vec!["src/a".into()],
            },
            ScopeSubsystemConfig {
                id: "dup".into(),
                paths: vec!["src/b".into()],
            },
        ],
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    let err = validate_workflow_config(&config).unwrap_err();
    assert!(err.message.contains("duplicate subsystem"));
}

#[test]
fn invalid_config_unsafe_subsystem_path_rejected() {
    let sc = ScopeControlConfig {
        subsystems: vec![ScopeSubsystemConfig {
            id: "core".into(),
            paths: vec!["../escape".into()],
        }],
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    let err = validate_workflow_config(&config).unwrap_err();
    assert!(err.message.contains("must be a relative path"));
}

#[test]
fn invalid_config_empty_mandatory_gates_rejected() {
    let sc = ScopeControlConfig {
        mandatory_gates: vec![],
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    let err = validate_workflow_config(&config).unwrap_err();
    assert!(err.message.contains("mandatory_gates"));
}

#[test]
fn valid_config_passes_validation() {
    let config = workflow_config(valid_scope_control());
    assert!(validate_workflow_config(&config).is_ok());
}

#[test]
fn disabled_scope_control_config_passes_validation() {
    let sc = ScopeControlConfig {
        enabled: false,
        ..valid_scope_control()
    };
    let config = workflow_config(sc);
    assert!(validate_workflow_config(&config).is_ok());
}

// ---------------------------------------------------------------------------
// Stable digest
// ---------------------------------------------------------------------------

#[test]
fn stable_digest_across_reordered_inputs() {
    let mut draft_a = sample_draft();
    let mut draft_b = sample_draft();
    // Reorder acceptance criteria.
    draft_a.acceptance_criteria = vec!["AC-142-02".into(), "AC-142-01".into()];
    draft_b.acceptance_criteria = vec!["AC-142-01".into(), "AC-142-02".into()];
    // Reorder subsystem paths.
    draft_a.subsystems[0].paths = vec!["src/core/".into(), "src/core".into()];
    draft_b.subsystems[0].paths = vec!["src/core".into(), "src/core/".into()];

    let c1 = normalize_charter(&draft_a);
    let c2 = normalize_charter(&draft_b);
    assert_eq!(c1.digest, c2.digest);
}

#[test]
fn digest_is_sha256_hex_64_chars() {
    let draft = sample_draft();
    let canonical = normalize_charter(&draft);
    assert_eq!(canonical.digest.len(), 64);
    assert!(canonical.digest.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn digest_changes_on_different_budget() {
    let draft = sample_draft();
    let c1 = normalize_charter(&draft);
    let mut draft2 = draft;
    draft2.budget.max_added_lines = 999;
    let c2 = normalize_charter(&draft2);
    assert_ne!(c1.digest, c2.digest);
}

#[test]
fn digest_changes_on_different_merge_base() {
    let draft = sample_draft();
    let c1 = normalize_charter(&draft);
    let mut draft2 = draft;
    draft2.merge_base = "different".into();
    let c2 = normalize_charter(&draft2);
    assert_ne!(c1.digest, c2.digest);
}

// ---------------------------------------------------------------------------
// Budget / subsystem rejection
// ---------------------------------------------------------------------------

#[test]
fn budget_exceeding_ceiling_rejected() {
    let draft = sample_draft();
    let config = ScopeControlConfig {
        budget: ScopeBudgetConfig {
            max_files_changed: 3,
            ..valid_scope_control().budget
        },
        subsystems: vec![ScopeSubsystemConfig {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        ..Default::default()
    };
    let err = validate_draft_against_config(&draft, &config).unwrap_err();
    assert!(err.message.contains("max_files_changed"));
}

#[test]
fn unknown_subsystem_rejected() {
    let mut draft = sample_draft();
    draft.subsystems = vec![DraftSubsystem {
        id: "unknown".into(),
        paths: vec!["src/x".into()],
    }];
    let config = ScopeControlConfig {
        budget: ScopeBudgetConfig {
            max_files_changed: 100,
            max_added_lines: 1000,
            max_new_modules: 10,
            max_dependencies_added: 10,
            max_public_apis_added: 20,
        },
        subsystems: vec![ScopeSubsystemConfig {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        ..Default::default()
    };
    let err = validate_draft_against_config(&draft, &config).unwrap_err();
    assert!(err.message.contains("unknown"));
}

#[test]
fn subsystem_path_outside_prefix_rejected() {
    let mut draft = sample_draft();
    draft.subsystems = vec![DraftSubsystem {
        id: "core".into(),
        paths: vec!["src/other".into()],
    }];
    let config = ScopeControlConfig {
        budget: ScopeBudgetConfig {
            max_files_changed: 100,
            max_added_lines: 1000,
            max_new_modules: 10,
            max_dependencies_added: 10,
            max_public_apis_added: 20,
        },
        subsystems: vec![ScopeSubsystemConfig {
            id: "core".into(),
            paths: vec!["src/core".into()],
        }],
        ..Default::default()
    };
    let err = validate_draft_against_config(&draft, &config).unwrap_err();
    assert!(err.message.contains("not within configured prefixes"));
}

// ---------------------------------------------------------------------------
// Atomic immutable persistence
// ---------------------------------------------------------------------------

#[test]
fn persist_writes_immutably_and_refuses_overwrite() {
    let tmp = TempDir::new().expect("tempdir");
    let draft = sample_draft();
    let canonical = normalize_charter(&draft);

    let dir = tmp.path().join("scope-control").join("run-abc");
    std::fs::create_dir_all(&dir).expect("create dir");

    let charter_path = dir.join("task-charter.json");
    let status_path = dir.join("status.json");

    luther_workflow::engine::executors::scope_control::persistence::write_immutable_json(
        &charter_path,
        &canonical,
    )
    .expect("write charter");

    let status = ScopeStatus {
        charter_id: canonical.charter_id.clone(),
        run_id: canonical.run_id.clone(),
        digest: canonical.digest.clone(),
        merge_base: canonical.merge_base.clone(),
        created_at: chrono::Utc::now(),
        measurement: None,
        evaluation: None,
        measured_at: None,
        prior_measurement: None,
        prior_measurement_digest: None,
        prior_measured_at: None,
    };
    luther_workflow::engine::executors::scope_control::persistence::write_immutable_json(
        &status_path,
        &status,
    )
    .expect("write status");

    // Second write must be refused (immutable).
    let result =
        luther_workflow::engine::executors::scope_control::persistence::write_immutable_json(
            &charter_path,
            &canonical,
        );
    assert!(matches!(
        result,
        Err(ScopePersistenceError::AlreadyExists(_))
    ));

    // No leftover temp files.
    let temps: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(".task-charter.json.tmp"))
        })
        .collect();
    assert!(temps.is_empty());
}

// ---------------------------------------------------------------------------
// Executor observable artifacts
// ---------------------------------------------------------------------------

fn make_context(tmp: &TempDir) -> StepContext {
    let work_dir = tmp.path().join("work");
    let artifact_dir = tmp.path().join("artifacts");
    std::fs::create_dir_all(&work_dir).expect("create work dir");
    let mut ctx = StepContext::new(work_dir, "run-int".into());
    ctx.set("artifact_dir", artifact_dir.to_str().expect("utf8"));
    ctx.set("primary_issue_number", "142");
    ctx
}

#[test]
fn executor_produces_observable_artifacts() {
    let tmp = TempDir::new().expect("tempdir");
    let mut context = make_context(&tmp);
    let executor = TaskCharterExecutor::with_probe(Box::new(FixedMergeBaseProbe {
        sha: "deadbeefcafe".into(),
    }));
    let params = json!({
        "charter_id": "ISSUE-142",
        "acceptance_criteria": ["AC-142-01"],
        "non_goals": ["no unrelated redesign"],
        "target_profile": TargetProfileConfig {
            scope_control: valid_scope_control(),
            identity: luther_workflow::workflow::schema::TargetIdentityConfig {
                base_branch: Some("main".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    });

    let outcome = executor.execute(&mut context, &params).expect("execute");
    assert_eq!(outcome, StepOutcome::Success);

    // Observable: digest
    let digest = context.get("task_charter_digest").expect("digest set");
    assert_eq!(digest.len(), 64);

    // Observable: merge base
    let merge_base = context
        .get("task_charter_merge_base")
        .expect("merge_base set");
    assert_eq!(merge_base, "deadbeefcafe");

    // Observable: charter file exists and contains charter data
    let charter_path = context.get("task_charter_path").expect("path set");
    let path = PathBuf::from(charter_path);
    assert!(path.exists());
    let charter_text = std::fs::read_to_string(&path).expect("read charter");
    assert!(charter_text.contains("ISSUE-142"));

    // Observable: status file exists
    let status_path = context
        .get("task_charter_status_path")
        .expect("status path set");
    assert!(PathBuf::from(status_path).exists());
}

#[test]
fn executor_replay_to_same_run_id_is_idempotent() {
    let tmp = TempDir::new().expect("tempdir");
    let mut context = make_context(&tmp);
    let executor =
        TaskCharterExecutor::with_probe(Box::new(FixedMergeBaseProbe { sha: "aaa".into() }));
    let params = json!({
        "charter_id": "ISSUE-142",
        "acceptance_criteria": ["AC-142-01"],
        "non_goals": ["no unrelated redesign"],
        "target_profile": TargetProfileConfig {
            scope_control: valid_scope_control(),
            ..Default::default()
        }
    });

    executor.execute(&mut context, &params).expect("first");
    let outcome = executor.execute(&mut context, &params).expect("replay");
    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn executor_noops_when_scope_control_disabled() {
    let tmp = TempDir::new().expect("tempdir");
    let mut context = make_context(&tmp);
    let executor =
        TaskCharterExecutor::with_probe(Box::new(FixedMergeBaseProbe { sha: "bbb".into() }));
    let params = json!({
        "target_profile": TargetProfileConfig {
            scope_control: ScopeControlConfig {
                enabled: false,
                ..valid_scope_control()
            },
            ..Default::default()
        }
    });

    let outcome = executor.execute(&mut context, &params).unwrap();
    assert_eq!(outcome, StepOutcome::Success);
    assert!(context.get("task_charter_digest").is_none());
}

// ---------------------------------------------------------------------------
// Executor registration
// ---------------------------------------------------------------------------

#[test]
fn task_charter_executor_registered_in_defaults() {
    let registry = ExecutorRegistry::with_defaults();
    assert!(registry.contains_step_type("task_charter"));
}
