//! An unknown outcome name must be rejected when a workflow is loaded.
//!
//! The unit tests call `validate_workflow_graph` directly, which proves the
//! check works but not that anything runs it. This drives the real loader so a
//! future load path that skips validation is caught here rather than by an
//! executor silently defaulting at runtime.

use std::path::Path;

use luther_workflow::workflow::resolve_workflow_type;

fn write_workflow(dir: &Path, outcome_name: &str) {
    let workflows = dir.join("workflows");
    std::fs::create_dir_all(&workflows).expect("fixture directory is creatable");
    std::fs::write(
        workflows.join("probe.toml"),
        format!(
            r#"workflow_type_id = "probe"

[[steps]]
step_id = "run"
step_type = "shell"
[steps.parameters.exit_code_map]
"2" = "{outcome_name}"

[[steps]]
step_id = "done"
step_type = "shell"
terminal = true

[[transitions]]
from = "run"
to = "done"
"#
        ),
    )
    .expect("fixture is writable");
}

#[test]
fn loading_a_workflow_with_a_misspelled_outcome_name_fails() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_workflow(dir.path(), "fixxable");

    let error = resolve_workflow_type("probe", dir.path())
        .expect_err("the loader must reject a workflow naming an outcome that does not exist");

    assert!(
        error.message.contains("fixxable"),
        "the loader error must carry the offending value so the author can find it: {}",
        error.message
    );
}

/// The control for the test above: the same workflow with a real outcome name
/// must load. Without this, the rejection test would still pass if the loader
/// rejected every workflow for an unrelated reason.
#[test]
fn loading_the_same_workflow_with_a_real_outcome_name_succeeds() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_workflow(dir.path(), "fixable");

    resolve_workflow_type("probe", dir.path()).expect("a workflow naming a real outcome must load");
}
