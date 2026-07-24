//! Validation for [`ScopeControlConfig`] against a workflow config's command
//! manifest and target profile.
//!
//! When scope control is enabled, every numeric ceiling, subsystem,
//! dependency manifest, measurement field, mandatory command group, partial
//! compile command/group, and mandatory gate is checked for structural
//! correctness before a run begins.
use std::collections::HashSet;
use std::path::{Component, Path};

use crate::workflow::config_loader::{ConfigError, ConfigErrorKind, Result};
use crate::workflow::schema::{ScopeControlConfig, TargetProfileConfig, WorkflowConfig};

/// Validate the scope-control section of a resolved workflow config.
///
/// This is a no-op when `target_profile.scope_control.enabled` is `false`.
/// When enabled, it aggregates every structural error and returns the first
/// as a [`ConfigError`].
pub fn validate_scope_control(config: &WorkflowConfig) -> Result<()> {
    let Some(profile) = &config.target_profile else {
        return Ok(());
    };
    if !profile.scope_control.enabled {
        return Ok(());
    }
    let mut errors = Vec::new();
    validate_budget(&profile.scope_control, &mut errors);
    validate_review_caps(&profile.scope_control, &mut errors);
    validate_subsystems(&profile.scope_control, &mut errors);
    validate_dependency_manifests(&profile.scope_control, &mut errors);
    validate_measurement(&profile.scope_control, &mut errors);
    validate_command_manifest_references(config, &profile.scope_control, &mut errors);
    validate_mandatory_gates(&profile.scope_control, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError {
            message: format!("invalid scope_control config: {}", errors.join("; ")),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })
    }
}

fn validate_budget(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    let b = &sc.budget;
    if b.max_files_changed == 0 {
        errors.push("budget.max_files_changed must be positive".into());
    }
    if b.max_added_lines == 0 {
        errors.push("budget.max_added_lines must be positive".into());
    }
    if b.max_new_modules == 0 {
        errors.push("budget.max_new_modules must be positive".into());
    }
    // max_dependencies_added is zero-or-positive, so no check needed.
    if b.max_public_apis_added == 0 {
        errors.push("budget.max_public_apis_added must be positive".into());
    }
}

fn validate_review_caps(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    let r = &sc.review_caps;
    if r.initial_full_reviews != 1 {
        errors.push("review_caps.initial_full_reviews must be exactly 1".into());
    }
    if r.max_delta_reviews == 0 {
        errors.push("review_caps.max_delta_reviews must be positive".into());
    }
    if r.final_acceptance_reviews != 1 {
        errors.push("review_caps.final_acceptance_reviews must be exactly 1".into());
    }
    if r.max_mutating_remediation_rounds == 0 {
        errors.push("review_caps.max_mutating_remediation_rounds must be positive".into());
    }
}

fn validate_subsystems(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    if sc.subsystems.is_empty() {
        errors.push("subsystems must not be empty".into());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for sub in &sc.subsystems {
        if sub.id.is_empty() {
            errors.push("subsystem id must not be empty".into());
            continue;
        }
        if !seen.insert(sub.id.as_str()) {
            errors.push(format!("duplicate subsystem id '{}'", sub.id));
        }
        if sub.paths.is_empty() {
            errors.push(format!(
                "subsystem '{}' must declare at least one path",
                sub.id
            ));
        }
        let mut path_seen: HashSet<&str> = HashSet::new();
        for path in &sub.paths {
            if let Err(msg) = validate_repo_relative_path(path) {
                errors.push(format!("subsystem '{}' path '{}': {msg}", sub.id, path));
            }
            if !path_seen.insert(path.as_str()) {
                errors.push(format!(
                    "subsystem '{}' has duplicate path '{}'",
                    sub.id, path
                ));
            }
        }
    }
}

fn validate_dependency_manifests(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    let mut path_seen: HashSet<&str> = HashSet::new();
    for manifest in &sc.dependency_manifests {
        if let Err(msg) = validate_repo_relative_path(&manifest.path) {
            errors.push(format!(
                "dependency manifest path '{}': {msg}",
                manifest.path
            ));
        }
        if !path_seen.insert(manifest.path.as_str()) {
            errors.push(format!(
                "duplicate dependency manifest path '{}'",
                manifest.path
            ));
        }
        let mut section_seen: HashSet<&str> = HashSet::new();
        for section in &manifest.sections {
            if section.is_empty() {
                errors.push(format!(
                    "dependency manifest '{}' sections must not be empty",
                    manifest.path
                ));
            }
            if !section_seen.insert(section.as_str()) {
                errors.push(format!(
                    "dependency manifest '{}' has duplicate section '{}'",
                    manifest.path, section
                ));
            }
        }
    }
}

fn validate_measurement(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    let m = &sc.measurement;
    if m.source_extensions.is_empty() {
        errors.push("measurement.source_extensions must not be empty".into());
    } else if m.source_extensions.iter().any(|ext| ext.is_empty()) {
        errors.push("measurement.source_extensions must not contain empty entries".into());
    }
    for pattern in &m.public_api_regexes {
        if regex::Regex::new(pattern).is_err() {
            errors.push(format!(
                "measurement.public_api_regexes contains invalid regex '{}'",
                pattern
            ));
        }
    }
    if !m.disable_rename_inference {
        errors.push(
            "measurement.disable_rename_inference must be true for deterministic measurement"
                .into(),
        );
    }
    if !m.enumerate_untracked {
        errors.push(
            "measurement.enumerate_untracked must be true for deterministic measurement".into(),
        );
    }
}

fn validate_command_manifest_references(
    config: &WorkflowConfig,
    sc: &ScopeControlConfig,
    errors: &mut Vec<String>,
) {
    if sc.mandatory_command_groups.is_empty()
        && sc.partial_compile_command.is_none()
        && sc.partial_compile_group.is_none()
    {
        return;
    }
    let Some(manifest) = &config.command_manifest else {
        if !sc.mandatory_command_groups.is_empty() {
            errors.push("scope_control mandatory_command_groups require a command_manifest".into());
        }
        if sc.partial_compile_group.is_some() {
            errors.push("scope_control partial_compile_group requires a command_manifest".into());
        }
        if sc.partial_compile_command.is_some() {
            errors.push("scope_control partial_compile_command requires a command_manifest".into());
        }
        return;
    };
    for group in &sc.mandatory_command_groups {
        if !manifest.groups.contains_key(group) {
            errors.push(format!(
                "scope_control mandatory_command_group '{}' is not a known manifest group",
                group
            ));
        }
    }
    if let Some(cmd) = &sc.partial_compile_command {
        if !manifest.commands.iter().any(|entry| &entry.id == cmd) {
            errors.push(format!(
                "scope_control partial_compile_command '{}' is not a known manifest command",
                cmd
            ));
        }
    }
    if let Some(group) = &sc.partial_compile_group {
        if !manifest.groups.contains_key(group) {
            errors.push(format!(
                "scope_control partial_compile_group '{}' is not a known manifest group",
                group
            ));
        }
    }
}

fn validate_mandatory_gates(sc: &ScopeControlConfig, errors: &mut Vec<String>) {
    if sc.mandatory_gates.is_empty() {
        errors.push(
            "scope_control mandatory_gates must not be empty when scope control is enabled".into(),
        );
    }
    let mut gate_seen: HashSet<&str> = HashSet::new();
    for gate in &sc.mandatory_gates {
        if gate.is_empty() {
            errors.push("scope_control mandatory_gates must not contain empty entries".into());
        }
        if !gate_seen.insert(gate.as_str()) {
            errors.push(format!(
                "scope_control mandatory_gates must not contain duplicate '{gate}'",
            ));
        }
    }
}

fn validate_repo_relative_path(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("must not be empty".into());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("must be a relative path under the repository root".into());
    }
    Ok(())
}

/// Read-only access to the scope-control config of a profile for use by the
/// executor. Returns `None` when there is no profile or scope control is
/// disabled.
#[must_use]
pub fn active_scope_control(profile: &TargetProfileConfig) -> Option<&ScopeControlConfig> {
    if profile.scope_control.enabled {
        Some(&profile.scope_control)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::command_manifest::CommandManifest;
    use crate::workflow::schema::{
        ScopeBudgetConfig, ScopeControlConfig, ScopeReviewCapsConfig, ScopeSubsystemConfig,
        TargetProfileConfig, WorkflowConfig,
    };

    fn workflow_config(scope_control: ScopeControlConfig) -> WorkflowConfig {
        WorkflowConfig {
            config_id: "test".into(),
            workflow_type_id: "test-type".into(),
            runtime: crate::workflow::schema::RuntimeConfig {
                timeout_seconds: 60,
                max_retries: 1,
                parallel_steps: None,
                log_level: None,
            },
            repo: crate::workflow::schema::RepoConfig {
                workspace_strategy: "temp_clone".into(),
                branch_template: "issue{n}".into(),
                base_branch: Some("main".into()),
                workspace_root: None,
                project_subdir: None,
                artifact_path_base: None,
                diff_path_base: None,
                diff_path_normalization:
                    crate::workflow::schema::DiffPathNormalization::RepoRelative,
            },
            guard_limits: crate::workflow::schema::GuardLimits {
                max_iterations: Some(1),
                max_file_changes: None,
                max_tokens: None,
                max_cost: None,
            },
            variables: std::collections::HashMap::new(),
            discovery: None,
            parent_orchestration: Default::default(),
            merge_required: false,
            merge_strategy: None,
            command_manifest: None,
            target_profile: Some(TargetProfileConfig {
                scope_control,
                ..Default::default()
            }),
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
            measurement: crate::workflow::schema::ScopeMeasurementConfig {
                disable_rename_inference: true,
                enumerate_untracked: true,
                ..Default::default()
            },
            mandatory_gates: vec!["cargo test".into()],
        }
    }

    #[test]
    fn disabled_scope_control_is_ok() {
        let sc = ScopeControlConfig {
            enabled: false,
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        assert!(validate_scope_control(&config).is_ok());
    }

    #[test]
    fn missing_profile_is_ok() {
        let mut config = workflow_config(valid_scope_control());
        config.target_profile = None;
        assert!(validate_scope_control(&config).is_ok());
    }

    #[test]
    fn zero_file_budget_is_rejected() {
        let sc = ScopeControlConfig {
            budget: ScopeBudgetConfig {
                max_files_changed: 0,
                ..valid_scope_control().budget
            },
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("max_files_changed"));
    }

    #[test]
    fn zero_review_cap_is_rejected() {
        let sc = ScopeControlConfig {
            review_caps: ScopeReviewCapsConfig {
                max_delta_reviews: 0,
                ..valid_scope_control().review_caps
            },
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("max_delta_reviews"));
    }

    #[test]
    fn duplicate_subsystem_id_is_rejected() {
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
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("duplicate subsystem"));
    }

    #[test]
    fn empty_subsystem_id_is_rejected() {
        let sc = ScopeControlConfig {
            subsystems: vec![ScopeSubsystemConfig {
                id: "".into(),
                paths: vec!["src/a".into()],
            }],
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("subsystem id must not be empty"));
    }

    #[test]
    fn unsafe_subsystem_path_is_rejected() {
        let sc = ScopeControlConfig {
            subsystems: vec![ScopeSubsystemConfig {
                id: "core".into(),
                paths: vec!["../escape".into()],
            }],
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("must be a relative path"));
    }

    #[test]
    fn invalid_regex_is_rejected() {
        let sc = ScopeControlConfig {
            measurement: crate::workflow::schema::ScopeMeasurementConfig {
                public_api_regexes: vec!["[invalid".into()],
                ..Default::default()
            },
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("invalid regex"));
    }

    #[test]
    fn mandatory_groups_without_manifest_rejected() {
        let sc = ScopeControlConfig {
            mandatory_command_groups: vec!["local".into()],
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("require a command_manifest"));
    }

    #[test]
    fn unknown_mandatory_group_rejected() {
        let mut sc = valid_scope_control();
        sc.mandatory_command_groups = vec!["nonexistent".into()];
        let mut config = workflow_config(sc);
        config.command_manifest = Some(CommandManifest::default());
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("nonexistent"));
    }

    #[test]
    fn empty_mandatory_gates_rejected() {
        let sc = ScopeControlConfig {
            mandatory_gates: vec![],
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("mandatory_gates"));
    }

    #[test]
    fn partial_compile_unknown_command_rejected() {
        let mut sc = valid_scope_control();
        sc.partial_compile_command = Some("ghost".into());
        let mut config = workflow_config(sc);
        config.command_manifest = Some(CommandManifest::default());
        let err = validate_scope_control(&config).unwrap_err();
        assert!(err.message.contains("partial_compile_command"));
    }

    #[test]
    fn zero_dependency_ceiling_is_allowed() {
        let sc = ScopeControlConfig {
            budget: ScopeBudgetConfig {
                max_dependencies_added: 0,
                ..valid_scope_control().budget
            },
            ..valid_scope_control()
        };
        let config = workflow_config(sc);
        assert!(validate_scope_control(&config).is_ok());
    }

    #[test]
    fn active_scope_control_returns_some_when_enabled() {
        let sc = valid_scope_control();
        let profile = TargetProfileConfig {
            scope_control: sc,
            ..Default::default()
        };
        assert!(active_scope_control(&profile).is_some());
    }

    #[test]
    fn active_scope_control_returns_none_when_disabled() {
        let profile = TargetProfileConfig::default();
        assert!(active_scope_control(&profile).is_none());
    }
}
