//! Scope evaluation: comparing a patch measurement against charter ceilings.
//!
//! This module produces stable divergence/violation codes by comparing every
//! metric plus changed paths/subsystems to the charter's budget and declared
//! subsystems. The evaluation result is deterministic: the same measurement
//! against the same charter always produces the same codes.

use serde::{Deserialize, Serialize};

use crate::engine::executors::scope_control::measurement::PatchMeasurement;
use crate::engine::executors::scope_control::model::CanonicalTaskCharter;

/// A single budget ceiling violation with a stable code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// Stable machine-readable code for this violation (e.g.,
    /// `BUDGET_FILES_CHANGED`).
    pub code: ViolationCode,
    /// Human-readable detail message.
    pub message: String,
}

/// Stable violation codes. These are part of the public API and must not
/// change once shipped — they appear in status artifacts and are used for
/// automated decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationCode {
    /// `files_changed` exceeds `max_files_changed`.
    BudgetFilesChanged,
    /// `added_lines` exceeds `max_added_lines`.
    BudgetAddedLines,
    /// `new_modules` exceeds `max_new_modules`.
    BudgetNewModules,
    /// `dependencies_added` exceeds `max_dependencies_added`.
    BudgetDependenciesAdded,
    /// `public_apis_added` exceeds `max_public_apis_added`.
    BudgetPublicApisAdded,
    /// A changed path is not within any declared subsystem.
    PathOutsideCharter,
    /// A changed subsystem is not declared in the charter.
    SubsystemOutsideCharter,
    /// The HEAD has diverged beyond the merge base (patch grown beyond the
    /// frozen point).
    DivergenceFromMergeBase,
}

impl ViolationCode {
    /// Convert to the stable string representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BudgetFilesChanged => "BUDGET_FILES_CHANGED",
            Self::BudgetAddedLines => "BUDGET_ADDED_LINES",
            Self::BudgetNewModules => "BUDGET_NEW_MODULES",
            Self::BudgetDependenciesAdded => "BUDGET_DEPENDENCIES_ADDED",
            Self::BudgetPublicApisAdded => "BUDGET_PUBLIC_APIS_ADDED",
            Self::PathOutsideCharter => "PATH_OUTSIDE_CHARTER",
            Self::SubsystemOutsideCharter => "SUBSYSTEM_OUTSIDE_CHARTER",
            Self::DivergenceFromMergeBase => "DIVERGENCE_FROM_MERGE_BASE",
        }
    }
}

impl std::fmt::Display for ViolationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// The result of evaluating a measurement against a charter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEvaluation {
    /// Whether the patch is within all charter ceilings.
    pub within_budget: bool,
    /// Whether all changed paths fall within declared subsystems.
    pub within_subsystems: bool,
    /// Whether the HEAD is still at the frozen merge base (no divergence).
    pub at_merge_base: bool,
    /// All violations found, sorted by code for deterministic ordering.
    pub violations: Vec<Violation>,
}

/// Evaluate a patch measurement against the charter's ceilings and subsystems.
///
/// Every metric is compared: files changed, added lines, new modules,
/// dependencies added, public APIs added. Changed paths are checked against
/// subsystem prefixes. Divergence from the merge base is flagged.
///
/// The result is deterministic: identical measurements produce identical
/// evaluations.
#[must_use]
pub fn evaluate(measurement: &PatchMeasurement, charter: &CanonicalTaskCharter) -> ScopeEvaluation {
    let mut violations = Vec::new();

    collect_budget_violations(&mut violations, measurement, charter);
    collect_path_violations(&mut violations, measurement, charter);
    collect_subsystem_violations(&mut violations, measurement, charter);

    // Divergence is observable via `at_merge_base` but is not itself a blocking
    // scope violation. Commit divergence after the frozen merge base is normal
    // (e.g. the implementation step advances HEAD) and must not block mutation
    // on its own. Budget and path/subsystem violations remain blocking.

    // Sort violations by code for deterministic ordering.
    violations.sort_by_key(|v| format!("{}", v.code));

    let within_budget = violations.iter().all(|v| !is_budget_violation(v.code));
    let within_subsystems = violations.iter().all(|v| {
        !matches!(
            v.code,
            ViolationCode::PathOutsideCharter | ViolationCode::SubsystemOutsideCharter
        )
    });
    let at_merge_base = measurement.divergence == 0;

    ScopeEvaluation {
        within_budget,
        within_subsystems,
        at_merge_base,
        violations,
    }
}

/// Collect all budget-ceiling violations for the measurement.
fn collect_budget_violations(
    violations: &mut Vec<Violation>,
    measurement: &PatchMeasurement,
    charter: &CanonicalTaskCharter,
) {
    check_budget(
        violations,
        ViolationCode::BudgetFilesChanged,
        "files_changed",
        measurement.files_changed,
        charter.budget.max_files_changed,
    );
    check_budget(
        violations,
        ViolationCode::BudgetAddedLines,
        "added_lines",
        measurement.added_lines,
        charter.budget.max_added_lines,
    );
    check_budget(
        violations,
        ViolationCode::BudgetNewModules,
        "new_modules",
        measurement.new_modules,
        charter.budget.max_new_modules,
    );
    check_budget(
        violations,
        ViolationCode::BudgetDependenciesAdded,
        "dependencies_added",
        measurement.dependencies_added,
        charter.budget.max_dependencies_added,
    );
    check_budget(
        violations,
        ViolationCode::BudgetPublicApisAdded,
        "public_apis_added",
        measurement.public_apis_added,
        charter.budget.max_public_apis_added,
    );
}

/// Collect path-outside-charter violations for changed paths outside any
/// declared subsystem.
fn collect_path_violations(
    violations: &mut Vec<Violation>,
    measurement: &PatchMeasurement,
    charter: &CanonicalTaskCharter,
) {
    let all_charter_prefixes: Vec<&Vec<String>> = charter.subsystems.values().collect();
    let path_outside: Vec<String> = measurement
        .changed_paths
        .iter()
        .filter(|path| !path_is_within_any_subsystem(path, &all_charter_prefixes))
        .cloned()
        .collect();
    if !path_outside.is_empty() {
        violations.push(Violation {
            code: ViolationCode::PathOutsideCharter,
            message: format!(
                "changed paths outside charter subsystems: {}",
                path_outside.join(", ")
            ),
        });
    }
}

/// Collect subsystem-outside-charter violations for changed subsystems not
/// declared in the charter.
fn collect_subsystem_violations(
    violations: &mut Vec<Violation>,
    measurement: &PatchMeasurement,
    charter: &CanonicalTaskCharter,
) {
    for sub in &measurement.changed_subsystems {
        if !charter.subsystems.contains_key(sub) {
            violations.push(Violation {
                code: ViolationCode::SubsystemOutsideCharter,
                message: format!("changed subsystem '{sub}' is not declared in the charter"),
            });
        }
    }
}

/// Whether `path` falls within any of the declared subsystem prefix groups.
fn path_is_within_any_subsystem(path: &str, prefix_groups: &[&Vec<String>]) -> bool {
    prefix_groups
        .iter()
        .any(|prefixes| prefixes.iter().any(|prefix| is_path_within(path, prefix)))
}

/// Check a single budget dimension and push a violation if exceeded.
fn check_budget(
    violations: &mut Vec<Violation>,
    code: ViolationCode,
    metric_name: &str,
    actual: u32,
    ceiling: u32,
) {
    if actual > ceiling {
        violations.push(Violation {
            code,
            message: format!("{metric_name} ({actual}) exceeds ceiling ({ceiling})"),
        });
    }
}

/// Whether a violation code represents a budget ceiling breach.
fn is_budget_violation(code: ViolationCode) -> bool {
    matches!(
        code,
        ViolationCode::BudgetFilesChanged
            | ViolationCode::BudgetAddedLines
            | ViolationCode::BudgetNewModules
            | ViolationCode::BudgetDependenciesAdded
            | ViolationCode::BudgetPublicApisAdded
    )
}

/// Whether `path` is equal to or a descendant of `prefix`, using component-
/// aware comparison.
///
/// A prefix ending in `/**` matches all descendant files under the directory.
/// The `/**` suffix is stripped before comparison so `src/**` behaves
/// identically to `src`.
fn is_path_within(path: &str, prefix: &str) -> bool {
    let normalized_prefix = prefix
        .strip_suffix("/**")
        .or_else(|| prefix.strip_suffix("/"))
        .unwrap_or(prefix);
    if path == normalized_prefix {
        return true;
    }
    let path = std::path::Path::new(path);
    let prefix = std::path::Path::new(normalized_prefix);
    path.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executors::scope_control::measurement::{ChangeStatus, FileChange};
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };

    fn sample_charter() -> CanonicalTaskCharter {
        let draft = TaskCharterDraft {
            charter_id: "T".into(),
            issue_number: 1,
            run_id: "r".into(),
            merge_base: "abc".into(),
            acceptance_criteria: vec!["AC".into()],
            non_goals: vec!["NG".into()],
            subsystems: vec![DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            budget: DraftBudget {
                max_files_changed: 5,
                max_added_lines: 100,
                max_new_modules: 3,
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
        normalize_charter(&draft)
    }

    fn sample_measurement() -> PatchMeasurement {
        PatchMeasurement {
            merge_base: "abc".into(),
            head_sha: "abc".into(),
            divergence: 0,
            files_changed: 2,
            added_lines: 50,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            public_apis_added: 2,
            changed_paths: vec!["src/core/a.rs".into(), "src/core/b.rs".into()],
            changed_subsystems: vec!["core".into()],
            file_details: vec![
                FileChange {
                    path: "src/core/a.rs".into(),
                    status: ChangeStatus::Added,
                    added_lines: Some(30),
                    deleted_lines: Some(0),
                    is_binary: false,
                },
                FileChange {
                    path: "src/core/b.rs".into(),
                    status: ChangeStatus::Modified,
                    added_lines: Some(20),
                    deleted_lines: Some(5),
                    is_binary: false,
                },
            ],
        }
    }

    #[test]
    fn clean_patch_passes() {
        let charter = sample_charter();
        let measurement = sample_measurement();
        let eval = evaluate(&measurement, &charter);
        assert!(eval.within_budget);
        assert!(eval.within_subsystems);
        assert!(eval.at_merge_base);
        assert!(eval.violations.is_empty());
    }

    #[test]
    fn files_changed_exceeds_budget() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.files_changed = 10;
        let eval = evaluate(&measurement, &charter);
        assert!(!eval.within_budget);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::BudgetFilesChanged));
    }

    #[test]
    fn added_lines_exceeds_budget() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.added_lines = 200;
        let eval = evaluate(&measurement, &charter);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::BudgetAddedLines));
    }

    #[test]
    fn new_modules_exceeds_budget() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.new_modules = 10;
        let eval = evaluate(&measurement, &charter);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::BudgetNewModules));
    }

    #[test]
    fn dependencies_added_exceeds_budget() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.dependencies_added = 1;
        let eval = evaluate(&measurement, &charter);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::BudgetDependenciesAdded));
    }

    #[test]
    fn public_apis_exceeds_budget() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.public_apis_added = 10;
        let eval = evaluate(&measurement, &charter);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::BudgetPublicApisAdded));
    }

    #[test]
    fn path_outside_charter_detected() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.changed_paths.push("src/other/c.rs".into());
        let eval = evaluate(&measurement, &charter);
        assert!(!eval.within_subsystems);
        assert!(eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::PathOutsideCharter));
    }

    #[test]
    fn divergence_is_observable_without_blocking_scope() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.divergence = 1;
        measurement.head_sha = "def".into();
        let eval = evaluate(&measurement, &charter);
        assert!(!eval.at_merge_base);
        assert!(eval.within_budget);
        assert!(!eval
            .violations
            .iter()
            .any(|v| v.code == ViolationCode::DivergenceFromMergeBase));
    }

    #[test]
    fn violations_are_sorted_deterministically() {
        let charter = sample_charter();
        let mut measurement = sample_measurement();
        measurement.files_changed = 100;
        measurement.added_lines = 999;
        measurement.divergence = 1;
        let eval = evaluate(&measurement, &charter);
        let codes: Vec<String> = eval
            .violations
            .iter()
            .map(|v| format!("{}", v.code))
            .collect();
        let mut sorted = codes.clone();
        sorted.sort();
        assert_eq!(codes, sorted);
    }

    #[test]
    fn violation_code_as_str_is_stable() {
        assert_eq!(
            ViolationCode::BudgetFilesChanged.as_str(),
            "BUDGET_FILES_CHANGED"
        );
        assert_eq!(
            ViolationCode::BudgetAddedLines.as_str(),
            "BUDGET_ADDED_LINES"
        );
        assert_eq!(
            ViolationCode::BudgetNewModules.as_str(),
            "BUDGET_NEW_MODULES"
        );
        assert_eq!(
            ViolationCode::BudgetDependenciesAdded.as_str(),
            "BUDGET_DEPENDENCIES_ADDED"
        );
        assert_eq!(
            ViolationCode::BudgetPublicApisAdded.as_str(),
            "BUDGET_PUBLIC_APIS_ADDED"
        );
        assert_eq!(
            ViolationCode::PathOutsideCharter.as_str(),
            "PATH_OUTSIDE_CHARTER"
        );
        assert_eq!(
            ViolationCode::SubsystemOutsideCharter.as_str(),
            "SUBSYSTEM_OUTSIDE_CHARTER"
        );
        assert_eq!(
            ViolationCode::DivergenceFromMergeBase.as_str(),
            "DIVERGENCE_FROM_MERGE_BASE"
        );
    }

    #[test]
    fn evaluation_is_deterministic() {
        let charter = sample_charter();
        let measurement = sample_measurement();
        let eval1 = evaluate(&measurement, &charter);
        let eval2 = evaluate(&measurement, &charter);
        assert_eq!(eval1, eval2);
    }
}
