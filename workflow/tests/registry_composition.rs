//! The catalog is composed in one place and every path agrees on it.
//!
//! `src/engine/recovery/policy.rs` resolves recovery policy from the canonical
//! `StepDef`/`step_id`. If the production path and the recovery path ever
//! composed different catalogs, an in-flight run could be recovered by a
//! process that cannot resolve the step it stopped on, and the run would
//! strand. These tests assert the catalogs are equal rather than trusting that
//! three call sites stay in step.

use luther_workflow::engine::executor::ExecutorRegistry;

/// Every step type the product shipped with before composition moved.
///
/// Written out in full rather than derived from the registry: deriving the
/// expected set from the thing under test would make this pass no matter what
/// the registry contained. This list is the specification, and it is what a
/// reviewer can check against the workflow TOMLs.
const EXPECTED_CATALOG: &[&str] = &[
    // core
    "command_manifest_group",
    "failure_cleanup",
    "noop",
    "shell",
    "verify",
    "write_file",
    // software change
    "git_config_publish",
    "llxprt",
    "parent_orchestration",
    "scope_measure",
    "task_charter",
    "workflow_auth_preflight",
    "workspace_ownership",
    "workspace_ownership_verify",
    // github follow-up
    "github_check_failures",
    "github_coderabbit_feedback",
    "github_pr_checks",
    "github_pr_identity",
    "post_pr_iteration_guard",
    // feedback and remediation
    "feedback_evaluator",
    "github_feedback_marker",
    "post_pr_failure_terminal",
    "pr_followup_remediation",
    "pr_remediation_plan",
    "pr_remediation_result",
    "push_remediation_changes",
    "run_post_pr_tests",
];

/// The product catalog resolves exactly the step types it did before.
///
/// This is the regression guard for the move: composition changed hands, and
/// the set of things that resolve at runtime must not have.
#[test]
fn the_product_catalog_matches_the_shipped_step_types() {
    let registry = ExecutorRegistry::with_defaults();
    let actual = registry.registered_step_types();
    let expected: std::collections::BTreeSet<String> =
        EXPECTED_CATALOG.iter().map(|s| (*s).to_string()).collect();

    let missing: Vec<_> = expected.difference(&actual).collect();
    let unexpected: Vec<_> = actual.difference(&expected).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "the composed catalog no longer matches the shipped one.\nmissing: {missing:?}\n\
         unexpected: {unexpected:?}"
    );
}

/// Every step type in the catalog actually resolves to an executor.
///
/// Membership in the name set and the ability to dispatch are different
/// claims; a registry could list a type whose entry was never installed.
#[test]
fn every_catalogued_step_type_resolves() {
    let registry = ExecutorRegistry::with_defaults();
    for step_type in EXPECTED_CATALOG {
        assert!(
            registry.contains_step_type(step_type),
            "`{step_type}` is in the catalog but does not resolve to an executor"
        );
    }
}

/// The bundles partition the catalog: none overlap, and together they are the
/// whole.
///
/// Overlap would mean a later bundle silently overwrote an earlier one's
/// executor, which is invisible at runtime until the wrong one runs.
#[test]
fn the_bundles_partition_the_catalog_without_overlapping() {
    let mut core = ExecutorRegistry::new();
    core.register_core_bundle();
    let mut software = ExecutorRegistry::new();
    software.register_software_change_bundle();
    let mut github = ExecutorRegistry::new();
    github.register_github_followup_executors();
    let mut feedback = ExecutorRegistry::new();
    feedback.register_feedback_and_remediation_executors();

    let sets = [
        ("core", core.registered_step_types()),
        ("software_change", software.registered_step_types()),
        ("github", github.registered_step_types()),
        ("feedback", feedback.registered_step_types()),
    ];

    for (i, (name_a, a)) in sets.iter().enumerate() {
        for (name_b, b) in sets.iter().skip(i + 1) {
            let shared: Vec<_> = a.intersection(b).collect();
            assert!(
                shared.is_empty(),
                "bundles `{name_a}` and `{name_b}` both register {shared:?}; whichever is \
                 composed last silently wins"
            );
        }
    }

    let union: std::collections::BTreeSet<String> =
        sets.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
    assert_eq!(
        union,
        ExecutorRegistry::with_defaults().registered_step_types(),
        "the bundles together must compose exactly the product catalog"
    );
}

/// The core bundle contains no domain vocabulary.
///
/// The previous `register_core_executors` was named "core" while registering
/// `llxprt`, `parent_orchestration`, `scope_measure`, and
/// `git_config_publish`. The name claimed a boundary the body did not keep,
/// and nothing failed. This is that check.
#[test]
fn the_core_bundle_registers_no_domain_step_types() {
    let mut core = ExecutorRegistry::new();
    core.register_core_bundle();

    const FORBIDDEN: &[&str] = &[
        "github",
        "coderabbit",
        "llxprt",
        "pr_",
        "remediation",
        "scope",
        "parent_orchestration",
        "git_config",
        "workspace_ownership",
    ];

    for step_type in core.registered_step_types() {
        for forbidden in FORBIDDEN {
            assert!(
                !step_type.contains(forbidden),
                "core bundle registers `{step_type}`, which carries the domain term \
                 `{forbidden}`; core must be usable by a product that is not this one"
            );
        }
    }
}

/// A core-only catalog is genuinely smaller than the product catalog.
///
/// Without this, the previous test passes for a core bundle that registers
/// nothing at all, and "the engine can be had without the domain" would be
/// true only in the sense that nothing can be had.
#[test]
fn a_core_only_catalog_is_usable_and_strictly_smaller() {
    let mut core = ExecutorRegistry::new();
    core.register_core_bundle();
    let core_types = core.registered_step_types();
    let product_types = ExecutorRegistry::with_defaults().registered_step_types();

    assert!(
        !core_types.is_empty(),
        "a core catalog that registers nothing proves nothing"
    );
    assert!(
        core_types.len() < product_types.len(),
        "the core catalog must be a strict subset of the product catalog"
    );
    assert!(
        core_types.is_subset(&product_types),
        "core registers a step type the product does not, so they have diverged"
    );
    assert!(
        core.contains_step_type("shell") && core.contains_step_type("verify"),
        "a core catalog without shell or verify cannot run a workflow at all"
    );
}
