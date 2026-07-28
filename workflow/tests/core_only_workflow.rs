//! A workflow that runs with only core and generic components registered.
//!
//! Every other boundary test in this repository is negative: it asserts that
//! some domain name does *not* appear somewhere. A suite of negative tests can
//! be fully satisfied by a core that cannot actually do anything - remove
//! enough and every "does not import" assertion passes trivially.
//!
//! This is the positive half. It registers core and generic components only -
//! no software-change package, no GitHub package - and runs a workflow that
//! produces a real artifact on disk. If the engine has become unable to
//! execute without a domain package, this fails, and no amount of import
//! hygiene elsewhere will hide it.

use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

/// Registers the generic executors this proof uses, and nothing else.
///
/// Deliberately not `register_core_bundle()`: that registers whatever the
/// bundle happens to contain, so it would keep passing if a domain executor
/// were added to it. Naming each executor means this test states exactly which
/// components the claim covers.
fn core_and_generic_only() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register(
        "write_file",
        Box::new(luther_workflow::components::generic::write_file::WriteFileExecutor),
    );
    registry.register(
        "noop",
        Box::new(luther_workflow::components::generic::noop::NoOpExecutor),
    );
    registry
}

/// A workflow executes end to end through the runner and leaves its artifact.
///
/// Driven through `EngineRunner::run`, not by calling `dispatch` directly.
/// Direct dispatch would only prove the registry resolves two step types; it
/// would say nothing about workflow parsing, sequencing, or transitions, so a
/// domain dependency introduced in the runner would not disturb it.
///
/// The assertion is on the artifact on disk rather than the run outcome, since
/// a run can report success without a step having done anything.
#[test]
fn a_workflow_runs_end_to_end_with_no_domain_package() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("core-only-v1", &fixture_root)
        .expect("the core-only workflow type loads");
    let mut config =
        resolve_workflow_config("core-only", &fixture_root).expect("the core-only config loads");

    let workspace = tempfile::tempdir().expect("a temp dir is available");
    config.variables.insert(
        "work_dir".to_string(),
        workspace.path().display().to_string(),
    );

    let registry = core_and_generic_only();
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("the runner constructs");

    let outcome = runner
        .run()
        .expect("the run completes without an engine error");
    assert!(
        matches!(outcome, RunOutcome::Success),
        "a workflow of generic steps must complete with only core and generic \
         components registered, got {outcome:?}"
    );

    let artifact = workspace.path().join("artifact.txt");
    let contents = std::fs::read_to_string(&artifact).unwrap_or_else(|error| {
        panic!(
            "the run must leave its artifact at {}: {error}",
            artifact.display()
        )
    });
    assert_eq!(
        contents, "produced without a domain package",
        "the artifact must carry the interpolated config variable, which proves \
         the step ran and the context resolved rather than the run merely \
         reporting Success"
    );
}

/// The registry used by the proof holds no domain step types.
///
/// Without this, the test above could be satisfied by a registry that happened
/// to include a domain executor, and it would still pass while proving nothing
/// about the boundary.
#[test]
fn the_core_only_registry_contains_no_domain_step_types() {
    let registry = core_and_generic_only();
    let registered = registry.registered_step_types();

    // Exact set, not a substring scan. Substrings are not an enforced naming
    // contract: a domain executor registered as "sync_upstream" carries none
    // of the words a blocklist would anticipate, and would pass. Naming the
    // set means any addition to this registry has to be stated here.
    let expected: std::collections::BTreeSet<String> = ["noop", "write_file"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        registered, expected,
        "the core-only registry must hold exactly the generic executors this \
         proof registers; anything else means the run above was not proved \
         against core and generic components alone"
    );
}

/// The core crate has no dependency edge back to the workspace crate.
///
/// The tests above reason about module paths and registries inside one crate.
/// This asserts the claim at the package level, where cargo enforces it: if
/// `luther-engine-core` ever gained a dependency on `luther-workflow`, the two
/// would be mutually dependent and the layering would be a fiction regardless
/// of how the modules are arranged.
///
/// Reads the resolved dependency graph rather than parsing Cargo.toml text, so
/// a dependency introduced through a feature or a target-specific table is
/// still seen.
///
/// Note on what this test is worth, since overstating it would be worse than
/// not having it. Both mutations that would make it fail were tried, and
/// neither reached the test: cargo rejects them at resolution with "cyclic
/// package dependency", because `luther-workflow` already depends on core, so
/// any path back - direct or through another workspace member - closes a
/// cycle. Inside this workspace the property is enforced by cargo, not here.
///
/// What remains is the case cargo cannot see: core taking a dependency on a
/// crate outside this workspace that itself depends on a published
/// `luther-workflow`. That resolves to a real acyclic graph. Reading the full
/// resolved tree rather than the direct dependency list is what would catch
/// it. That is a narrow guarantee, and it is the honest one.
#[test]
fn the_core_crate_does_not_depend_on_the_workspace_crate() {
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "tree",
            "--package",
            "luther-engine-core",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        tree.contains("luther-engine-core"),
        "the tree must name the package it was asked about, or the assertion \
         below would pass against empty output"
    );
    assert!(
        !tree.contains("luther-workflow"),
        "luther-engine-core depends on luther-workflow, which makes the \
         layering circular:\n{tree}"
    );
}
