//! Task-charter domain models: draft input, canonical (normalized) charter,
//! deterministic digest computation, and charter validation against configured
//! ceilings and subsystems.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workflow::schema::{ScopeControlConfig, ScopeSubsystemConfig};

/// Draft task-charter input assembled by the executor from run context and
/// config. This is the *pre-normalization* form: fields are validated and
/// normalized into a [`CanonicalTaskCharter`] before persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCharterDraft {
    pub charter_id: String,
    pub issue_number: u64,
    pub run_id: String,
    pub merge_base: String,
    pub acceptance_criteria: Vec<String>,
    pub non_goals: Vec<String>,
    pub subsystems: Vec<DraftSubsystem>,
    pub budget: DraftBudget,
    pub review_caps: DraftReviewCaps,
    pub mandatory_gates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSubsystem {
    pub id: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftBudget {
    pub max_files_changed: u32,
    pub max_added_lines: u32,
    pub max_new_modules: u32,
    pub max_dependencies_added: u32,
    pub max_public_apis_added: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReviewCaps {
    pub initial_full_reviews: u32,
    pub max_delta_reviews: u32,
    pub final_acceptance_reviews: u32,
    pub max_mutating_remediation_rounds: u32,
}

/// Canonical (normalized, immutable) task charter persisted to disk.
///
/// All vectors are sorted, path prefixes are normalized to forward-slash
/// repo-relative form without trailing slashes, and the digest is computed
/// deterministically from the normalized field set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalTaskCharter {
    pub schema_version: u32,
    pub charter_id: String,
    pub issue_number: u64,
    pub run_id: String,
    pub merge_base: String,
    pub acceptance_criteria: Vec<String>,
    pub non_goals: Vec<String>,
    pub subsystems: BTreeMap<String, Vec<String>>,
    pub budget: CanonicalBudget,
    pub review_caps: CanonicalReviewCaps,
    pub mandatory_gates: Vec<String>,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalBudget {
    pub max_files_changed: u32,
    pub max_added_lines: u32,
    pub max_new_modules: u32,
    pub max_dependencies_added: u32,
    pub max_public_apis_added: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalReviewCaps {
    pub initial_full_reviews: u32,
    pub max_delta_reviews: u32,
    pub final_acceptance_reviews: u32,
    pub max_mutating_remediation_rounds: u32,
}

/// Normalize a draft into a canonical charter.
///
/// Normalization is deterministic: vectors are sorted and de-duplicated, path
/// prefixes use forward slashes with no trailing slash, and the digest is
/// derived from the normalized fields.
#[must_use]
pub fn normalize_charter(draft: &TaskCharterDraft) -> CanonicalTaskCharter {
    let subsystems: BTreeMap<String, Vec<String>> = draft
        .subsystems
        .iter()
        .map(|sub| {
            let mut paths: Vec<String> = sub
                .paths
                .iter()
                .map(|p| normalize_path_prefix(p))
                .collect::<std::collections::BTreeSet<String>>()
                .into_iter()
                .collect();
            paths.sort();
            (sub.id.clone(), paths)
        })
        .collect();

    let mut acceptance_criteria: Vec<String> = draft
        .acceptance_criteria
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect();
    acceptance_criteria.sort();

    let mut non_goals: Vec<String> = draft
        .non_goals
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect();
    non_goals.sort();

    let mut mandatory_gates: Vec<String> = draft
        .mandatory_gates
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect();
    mandatory_gates.sort();

    let canonical = CanonicalTaskCharter {
        schema_version: CHARTER_SCHEMA_VERSION,
        charter_id: draft.charter_id.clone(),
        issue_number: draft.issue_number,
        run_id: draft.run_id.clone(),
        merge_base: draft.merge_base.clone(),
        acceptance_criteria,
        non_goals,
        subsystems,
        budget: CanonicalBudget {
            max_files_changed: draft.budget.max_files_changed,
            max_added_lines: draft.budget.max_added_lines,
            max_new_modules: draft.budget.max_new_modules,
            max_dependencies_added: draft.budget.max_dependencies_added,
            max_public_apis_added: draft.budget.max_public_apis_added,
        },
        review_caps: CanonicalReviewCaps {
            initial_full_reviews: draft.review_caps.initial_full_reviews,
            max_delta_reviews: draft.review_caps.max_delta_reviews,
            final_acceptance_reviews: draft.review_caps.final_acceptance_reviews,
            max_mutating_remediation_rounds: draft.review_caps.max_mutating_remediation_rounds,
        },
        mandatory_gates,
        digest: String::new(),
    };

    let digest = compute_digest(&canonical);
    CanonicalTaskCharter {
        digest,
        ..canonical
    }
}

/// Current canonical task-charter schema version.
pub const CHARTER_SCHEMA_VERSION: u32 = 1;

/// Compute a deterministic SHA-256 digest for a canonical charter.
///
/// The digest is derived from canonical JSON serialization of the charter
/// with the digest field zeroed, guaranteeing structural identity produces
/// identical digests while avoiding ambiguous byte concatenation.
fn compute_digest(charter: &CanonicalTaskCharter) -> String {
    let mut for_digest = charter.clone();
    for_digest.digest.clear();
    let serialized = serde_json::to_vec(&for_digest).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    let result = hasher.finalize();
    result.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Normalize a repository-relative path prefix.
///
/// Converts backslashes to forward slashes, collapses repeated slashes, strips
/// leading `./`, and removes trailing slashes.
fn normalize_path_prefix(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let stripped = forward.strip_prefix("./").unwrap_or(&forward);
    let collapsed = strip_repeated_slashes(stripped);
    let trimmed = collapsed.trim_end_matches('/');
    trimmed.to_string()
}

fn strip_repeated_slashes(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut prev_was_slash = false;
    for ch in value.chars() {
        if ch == '/' {
            if !prev_was_slash {
                result.push(ch);
            }
            prev_was_slash = true;
        } else {
            result.push(ch);
            prev_was_slash = false;
        }
    }
    result
}

/// Validation error for a draft charter against configured scope-control ceilings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharterValidationError {
    pub message: String,
}

impl std::fmt::Display for CharterValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CharterValidationError {}

/// Validate a draft charter against the configured scope-control ceilings.
///
/// Checks that:
/// - required charter fields are present (nonempty IDs, run ID, merge base,
///   issue number, acceptance criteria, non-goals, subsystems, gates);
/// - applicable budget caps and review budgets are positive;
/// - budget fields do not exceed the configured ceilings;
/// - subsystems in the draft are a subset of configured subsystems;
/// - subsystem paths are within configured path prefixes.
pub fn validate_draft_against_config(
    draft: &TaskCharterDraft,
    config: &ScopeControlConfig,
) -> std::result::Result<(), CharterValidationError> {
    validate_draft_required_fields(draft)?;
    validate_draft_budgets_positive(draft)?;
    validate_budget_ceiling(&draft.budget, &config.budget)?;
    validate_subsystems(&draft.subsystems, &config.subsystems)?;
    Ok(())
}

fn validate_draft_required_fields(
    draft: &TaskCharterDraft,
) -> std::result::Result<(), CharterValidationError> {
    if draft.charter_id.is_empty() {
        return Err(CharterValidationError {
            message: "charter_id must not be empty".into(),
        });
    }
    if draft.run_id.is_empty() {
        return Err(CharterValidationError {
            message: "run_id must not be empty".into(),
        });
    }
    if draft.merge_base.is_empty() {
        return Err(CharterValidationError {
            message: "merge_base must not be empty".into(),
        });
    }
    if draft.issue_number == 0 {
        return Err(CharterValidationError {
            message: "issue_number must be positive".into(),
        });
    }
    if draft.acceptance_criteria.is_empty() {
        return Err(CharterValidationError {
            message: "acceptance_criteria must not be empty".into(),
        });
    }
    if draft.non_goals.is_empty() {
        return Err(CharterValidationError {
            message: "non_goals must not be empty".into(),
        });
    }
    if draft.subsystems.is_empty() {
        return Err(CharterValidationError {
            message: "subsystems must not be empty".into(),
        });
    }
    if draft.mandatory_gates.is_empty() {
        return Err(CharterValidationError {
            message: "mandatory_gates must not be empty".into(),
        });
    }
    Ok(())
}

fn validate_draft_budgets_positive(
    draft: &TaskCharterDraft,
) -> std::result::Result<(), CharterValidationError> {
    if draft.budget.max_files_changed == 0 {
        return Err(CharterValidationError {
            message: "charter budget max_files_changed must be positive".into(),
        });
    }
    if draft.budget.max_added_lines == 0 {
        return Err(CharterValidationError {
            message: "charter budget max_added_lines must be positive".into(),
        });
    }
    if draft.budget.max_new_modules == 0 {
        return Err(CharterValidationError {
            message: "charter budget max_new_modules must be positive".into(),
        });
    }
    if draft.budget.max_public_apis_added == 0 {
        return Err(CharterValidationError {
            message: "charter budget max_public_apis_added must be positive".into(),
        });
    }
    if draft.review_caps.initial_full_reviews == 0 {
        return Err(CharterValidationError {
            message: "charter review_caps initial_full_reviews must be positive".into(),
        });
    }
    if draft.review_caps.max_delta_reviews == 0 {
        return Err(CharterValidationError {
            message: "charter review_caps max_delta_reviews must be positive".into(),
        });
    }
    if draft.review_caps.final_acceptance_reviews == 0 {
        return Err(CharterValidationError {
            message: "charter review_caps final_acceptance_reviews must be positive".into(),
        });
    }
    if draft.review_caps.max_mutating_remediation_rounds == 0 {
        return Err(CharterValidationError {
            message: "charter review_caps max_mutating_remediation_rounds must be positive".into(),
        });
    }
    Ok(())
}

fn validate_budget_ceiling(
    draft: &DraftBudget,
    ceiling: &crate::workflow::schema::ScopeBudgetConfig,
) -> std::result::Result<(), CharterValidationError> {
    if draft.max_files_changed > ceiling.max_files_changed {
        return Err(CharterValidationError {
            message: format!(
                "charter budget max_files_changed ({}) exceeds configured ceiling ({})",
                draft.max_files_changed, ceiling.max_files_changed
            ),
        });
    }
    if draft.max_added_lines > ceiling.max_added_lines {
        return Err(CharterValidationError {
            message: format!(
                "charter budget max_added_lines ({}) exceeds configured ceiling ({})",
                draft.max_added_lines, ceiling.max_added_lines
            ),
        });
    }
    if draft.max_new_modules > ceiling.max_new_modules {
        return Err(CharterValidationError {
            message: format!(
                "charter budget max_new_modules ({}) exceeds configured ceiling ({})",
                draft.max_new_modules, ceiling.max_new_modules
            ),
        });
    }
    if draft.max_dependencies_added > ceiling.max_dependencies_added {
        return Err(CharterValidationError {
            message: format!(
                "charter budget max_dependencies_added ({}) exceeds configured ceiling ({})",
                draft.max_dependencies_added, ceiling.max_dependencies_added
            ),
        });
    }
    if draft.max_public_apis_added > ceiling.max_public_apis_added {
        return Err(CharterValidationError {
            message: format!(
                "charter budget max_public_apis_added ({}) exceeds configured ceiling ({})",
                draft.max_public_apis_added, ceiling.max_public_apis_added
            ),
        });
    }
    Ok(())
}

fn validate_subsystems(
    draft_subs: &[DraftSubsystem],
    config_subs: &[ScopeSubsystemConfig],
) -> std::result::Result<(), CharterValidationError> {
    let config_map: BTreeMap<&str, &ScopeSubsystemConfig> = config_subs
        .iter()
        .map(|sub| (sub.id.as_str(), sub))
        .collect();
    let mut seen_ids = std::collections::BTreeSet::new();
    for draft_sub in draft_subs {
        if !seen_ids.insert(draft_sub.id.as_str()) {
            return Err(CharterValidationError {
                message: format!("duplicate charter subsystem id '{}'", draft_sub.id),
            });
        }
        if draft_sub.paths.iter().any(|path| {
            std::path::Path::new(path).components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        }) {
            return Err(CharterValidationError {
                message: format!("charter subsystem '{}' has unsafe path", draft_sub.id),
            });
        }
        let Some(config_sub) = config_map.get(draft_sub.id.as_str()) else {
            return Err(CharterValidationError {
                message: format!(
                    "charter subsystem '{}' is not in configured subsystems",
                    draft_sub.id
                ),
            });
        };
        let config_prefixes: Vec<String> = config_sub
            .paths
            .iter()
            .map(|p| normalize_path_prefix(p))
            .collect();
        for path in &draft_sub.paths {
            let normalized = normalize_path_prefix(path);
            if !config_prefixes
                .iter()
                .any(|prefix| is_path_within(&normalized, prefix))
            {
                return Err(CharterValidationError {
                    message: format!(
                        "charter subsystem '{}' path '{}' is not within configured prefixes",
                        draft_sub.id, path
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Whether `path` is equal to or a descendant of `prefix`, using
/// component-aware comparison so `src/core` covers `src/core/foo.rs` but not
/// `src/coreutils`.
fn is_path_within(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    let path = std::path::Path::new(path);
    let prefix = std::path::Path::new(prefix);
    path.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{ScopeBudgetConfig, ScopeControlConfig, ScopeSubsystemConfig};

    fn sample_draft() -> TaskCharterDraft {
        TaskCharterDraft {
            charter_id: "TEST-001".into(),
            issue_number: 42,
            run_id: "run-abc".into(),
            merge_base: "abc123".into(),
            acceptance_criteria: vec!["AC-1".into(), "AC-2".into()],
            non_goals: vec!["no redesign".into()],
            subsystems: vec![DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core/".into(), "src/core".into()],
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

    #[test]
    fn normalize_is_deterministic() {
        let draft = sample_draft();
        let c1 = normalize_charter(&draft);
        let c2 = normalize_charter(&draft);
        assert_eq!(c1, c2);
    }

    #[test]
    fn digest_is_stable_across_reordered_inputs() {
        let mut draft_a = sample_draft();
        let mut draft_b = sample_draft();
        // Reorder acceptance criteria and subsystem paths.
        draft_a.acceptance_criteria = vec!["AC-2".into(), "AC-1".into()];
        draft_b.acceptance_criteria = vec!["AC-1".into(), "AC-2".into()];
        let c1 = normalize_charter(&draft_a);
        let c2 = normalize_charter(&draft_b);
        assert_eq!(c1.digest, c2.digest);
    }

    #[test]
    fn normalize_strips_trailing_slash_and_dedupes() {
        let draft = sample_draft();
        let canonical = normalize_charter(&draft);
        let paths = canonical.subsystems.get("core").expect("subsystem exists");
        assert_eq!(paths, &vec!["src/core".to_string()]);
    }

    #[test]
    fn normalize_sorts_acceptance_criteria() {
        let draft = sample_draft();
        let canonical = normalize_charter(&draft);
        assert_eq!(
            canonical.acceptance_criteria,
            vec!["AC-1".to_string(), "AC-2".to_string()]
        );
    }

    #[test]
    fn digest_changes_when_budget_changes() {
        let draft = sample_draft();
        let c1 = normalize_charter(&draft);
        let mut draft2 = draft;
        draft2.budget.max_files_changed = 6;
        let c2 = normalize_charter(&draft2);
        assert_ne!(c1.digest, c2.digest);
    }

    #[test]
    fn digest_is_hex_sha256() {
        let draft = sample_draft();
        let canonical = normalize_charter(&draft);
        assert_eq!(canonical.digest.len(), 64);
        assert!(canonical.digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn budget_exceeding_ceiling_rejected() {
        let draft = sample_draft();
        let config = ScopeControlConfig {
            budget: ScopeBudgetConfig {
                max_files_changed: 3, // draft has 5
                ..Default::default()
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
            budget: large_budget(),
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
            budget: large_budget(),
            subsystems: vec![ScopeSubsystemConfig {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            ..Default::default()
        };
        let err = validate_draft_against_config(&draft, &config).unwrap_err();
        assert!(err.message.contains("not within configured prefixes"));
    }

    #[test]
    fn within_budget_and_subsystems_passes() {
        let draft = sample_draft();
        let config = ScopeControlConfig {
            budget: large_budget(),
            subsystems: vec![ScopeSubsystemConfig {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            ..Default::default()
        };
        assert!(validate_draft_against_config(&draft, &config).is_ok());
    }

    fn large_budget() -> ScopeBudgetConfig {
        ScopeBudgetConfig {
            max_files_changed: 100,
            max_added_lines: 1000,
            max_new_modules: 10,
            max_dependencies_added: 10,
            max_public_apis_added: 20,
        }
    }
}
