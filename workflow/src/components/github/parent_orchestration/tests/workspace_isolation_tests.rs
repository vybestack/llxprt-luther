//! Child workspace isolation tests (issue 137).
//!
//! Each child issue and each relaunch must receive an isolated workspace
//! directory under the parent `work_dir` so concurrent children and relaunches
//! do not stomp on a shared parent workspace.

use super::super::*;
use super::support::*;

/// Build an `OrchestrationState` with a work_dir for child workspace isolation
/// tests.
fn workspace_isolation_state() -> OrchestrationState {
    let temp = tempfile::tempdir().unwrap();
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.work_dir = Some(temp.path().join("parent-work"));
    std::fs::create_dir_all(state.work_dir.as_ref().unwrap()).unwrap();
    std::mem::forget(temp);
    state
}

#[test]
fn child_request_derives_isolated_work_dir_not_parent() {
    // The child request must derive an isolated workspace under
    // children/issue-N/run-id, not clone the parent's work_dir.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child = unique_child_issue_number();
    let request = child_request_with_run_id(&state, child, "child-run-1".to_string());

    let child_work = request.work_dir.expect("child work_dir must be set");
    assert_ne!(
        child_work, parent_work_dir,
        "child workspace must not be the parent work_dir"
    );
    assert_eq!(
        child_work,
        child_work_dir(&parent_work_dir, child, "child-run-1"),
        "child work_dir must follow the isolated children/issue-N/run-id layout"
    );
}

#[test]
fn child_relaunches_get_distinct_isolated_work_dirs() {
    // A relaunched child (same issue, new run id) must get a distinct workspace
    // so the prior run's worktree is not overwritten.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child = unique_child_issue_number();

    let first = child_request_with_run_id(&state, child, "child-run-1".to_string());
    let second = child_request_with_run_id(&state, child, "child-run-2".to_string());

    assert_ne!(
        first.work_dir, second.work_dir,
        "relaunched children must get distinct workspaces"
    );
    // Both are under the parent work_dir but isolated per run.
    assert!(first
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
    assert!(second
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
}

#[test]
fn sibling_children_get_distinct_isolated_work_dirs() {
    // Two different child issues must get distinct workspaces even with the
    // same run id, proving sibling isolation.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child_a = unique_child_issue_number();
    let child_b = unique_child_issue_number();

    let req_a = child_request_with_run_id(&state, child_a, "shared-run".to_string());
    let req_b = child_request_with_run_id(&state, child_b, "shared-run".to_string());

    assert_ne!(
        req_a.work_dir, req_b.work_dir,
        "sibling children must get distinct workspaces"
    );
    assert!(req_a
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
    assert!(req_b
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
}

#[test]
fn child_work_dir_layout_is_deterministic() {
    let base = Path::new("/tmp/luther-parent");
    let path = child_work_dir(base, 42, "run-xyz");
    assert_eq!(path, base.join("children").join("issue-42").join("run-xyz"));
}
