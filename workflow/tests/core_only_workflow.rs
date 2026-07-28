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

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext};
use luther_workflow::engine::StepOutcome;
use std::path::PathBuf;

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

/// A workflow executes end to end and leaves its artifact behind.
///
/// The assertion is on the file's contents, not on the step outcome. A step
/// can report success without having done anything; the artifact can only
/// exist if the executor actually ran and the context actually interpolated.
#[test]
fn a_workflow_runs_and_writes_its_artifact_with_no_domain_package() {
    let workspace = tempfile::tempdir().expect("a temp dir is available");
    let work_dir: PathBuf = workspace.path().to_path_buf();

    let registry = core_and_generic_only();
    let mut context = StepContext::new(work_dir.clone(), "core-only-run".to_string());
    context.set("greeting", "produced without a domain package");

    // A no-op step first, to prove dispatch works for more than one step type
    // and that the run is a sequence rather than a single call.
    let noop_outcome = registry
        .dispatch("noop", &mut context, &serde_json::json!({}))
        .expect("the noop step dispatches");
    assert_eq!(
        noop_outcome,
        StepOutcome::Success,
        "a core-only run must be able to execute a step that does nothing"
    );

    let write_outcome = registry
        .dispatch(
            "write_file",
            &mut context,
            &serde_json::json!({
                "path": "artifact.txt",
                "content": "{greeting}",
            }),
        )
        .expect("the write_file step dispatches");
    assert_eq!(
        write_outcome,
        StepOutcome::Success,
        "the write_file step must succeed with only generic components registered"
    );

    let artifact = work_dir.join("artifact.txt");
    let contents = std::fs::read_to_string(&artifact).unwrap_or_else(|error| {
        panic!(
            "the workflow must leave its artifact at {}: {error}",
            artifact.display()
        )
    });
    assert_eq!(
        contents, "produced without a domain package",
        "the artifact must carry the interpolated context value, which proves \
         the executor ran and the context resolved - not merely that dispatch \
         returned Success"
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

    assert!(
        !registered.is_empty(),
        "an empty registry would satisfy every assertion below without proving \
         anything; the proof requires that something is actually registered"
    );

    // Substrings rather than exact names: a domain step type added later will
    // not be one this list anticipated, but it will carry one of these words.
    for forbidden in [
        "pr_",
        "issue",
        "github",
        "merge",
        "review",
        "remediation",
        "llxprt",
    ] {
        let offenders: Vec<&String> = registered
            .iter()
            .filter(|step_type| step_type.contains(forbidden))
            .collect();
        assert!(
            offenders.is_empty(),
            "the core-only registry contains domain step types matching \
             '{forbidden}': {offenders:?}"
        );
    }
}
