use super::*;
use luther_workflow::monitor::heartbeat::Heartbeat;
use luther_workflow::persistence::{RunMetadata, RunStatus, WaitKind};
use std::collections::HashMap;

fn run(run_id: &str) -> RunMetadata {
    RunMetadata::new(run_id, "wf", "cfg")
}

#[test]
fn wait_kind_for_step_maps_known_steps() {
    assert_eq!(wait_kind_for_step("watch_pr_checks"), WaitKind::PrChecks);
    assert_eq!(
        wait_kind_for_step("collect_coderabbit_feedback"),
        WaitKind::CoderabbitReview
    );
    assert_eq!(wait_kind_for_step("merge_pr"), WaitKind::PrMerge);
    assert_eq!(wait_kind_for_step("wait_for_merge"), WaitKind::PrMerge);
    assert_eq!(
        wait_kind_for_step("launch_or_resume_child_workflow"),
        WaitKind::DependencyChildWorkflow
    );
    assert_eq!(
        wait_kind_for_step("dependency_child_workflow"),
        WaitKind::DependencyChildWorkflow
    );
    assert_eq!(
        wait_kind_for_step("wait_for_child_merge"),
        WaitKind::DependencyChildMerge
    );
    assert_eq!(
        wait_kind_for_step("github_rate_limit_backoff"),
        WaitKind::RateLimitBackoff
    );
}

#[test]
fn wait_kind_for_step_unknown_defaults_to_human_review() {
    assert_eq!(wait_kind_for_step("totally_unknown"), WaitKind::HumanReview);
}

#[test]
fn monitor_state_label_covers_all_variants() {
    assert_eq!(monitor_state_label(MonitorState::Starting), "starting");
    assert_eq!(monitor_state_label(MonitorState::Running), "running");
    assert_eq!(monitor_state_label(MonitorState::Degraded), "degraded");
    assert_eq!(monitor_state_label(MonitorState::Stopping), "stopping");
    assert_eq!(monitor_state_label(MonitorState::Stopped), "stopped");
    assert_eq!(monitor_state_label(MonitorState::Error), "error");
}

#[test]
fn pid_liveness_label_reports_unknown_without_pid() {
    let md = run("r1");
    assert_eq!(pid_liveness_label(&md), "unknown");
}

#[test]
fn pid_liveness_label_reports_alive_for_current_process() {
    let mut md = run("r1");
    md.process_pid = Some(std::process::id());
    let label = pid_liveness_label(&md);
    assert!(label.contains("alive"), "expected alive, got {label}");
}

#[test]
fn next_step_label_terminal_run_has_no_next_step() {
    let mut md = run("r1");
    md.status = RunStatus::Completed;
    assert_eq!(next_step_label(&md), "none (run is terminal)");
}

#[test]
fn next_step_label_non_terminal_without_candidates() {
    let mut md = run("r1");
    md.status = RunStatus::Running;
    assert_eq!(next_step_label(&md), "unknown until current step completes");
}

#[test]
fn next_step_label_joins_candidates() {
    let mut md = run("r1");
    md.next_step_candidates = vec!["a".to_string(), "b".to_string()];
    assert_eq!(next_step_label(&md), "a, b");
}

#[test]
fn run_metadata_to_json_serializes_expected_fields() {
    let mut md = run("run-123");
    md.status = RunStatus::Running;
    md.current_step = Some("step-1".to_string());
    md.repository = Some("owner/repo".to_string());
    md.issue_number = Some(7);
    md.pr_number = Some(42);
    md.head_sha = Some("deadbeef".to_string());
    let value = run_metadata_to_json(&md);
    assert_eq!(value["run_id"], "run-123");
    assert_eq!(value["status"], "running");
    assert_eq!(value["current_step"], "step-1");
    assert_eq!(value["repository"], "owner/repo");
    assert_eq!(value["issue_number"], 7);
    assert_eq!(value["pr_number"], 42);
    assert_eq!(value["head_sha"], "deadbeef");
    assert_eq!(value["process_stale"], false);
}

#[test]
fn print_status_json_smoke_with_runs_and_error() {
    // Exercise the JSON rendering path for both Ok and Err registry results.
    let mut heartbeats = HashMap::new();
    heartbeats.insert("run-1".to_string(), Heartbeat::new("inst-1"));
    let ok_runs: Result<Vec<RunMetadata>, String> = Ok(vec![run("run-1")]);
    print_status_json(&heartbeats, &ok_runs);
    let err_runs: Result<Vec<RunMetadata>, String> = Err("registry down".to_string());
    print_status_json(&heartbeats, &err_runs);
}

#[test]
fn print_status_human_smoke_paths() {
    let mut heartbeats = HashMap::new();
    heartbeats.insert("run-1".to_string(), Heartbeat::new("inst-1"));
    print_heartbeat_status(&heartbeats);
    print_heartbeat_status(&HashMap::new());
    print_requested_heartbeat_details(Some("run-1"), &heartbeats);
    print_requested_heartbeat_details(Some("missing"), &heartbeats);
    print_requested_heartbeat_details(None, &heartbeats);
    print_run_registry(&[run("run-1")], Some("run-1"));
    print_run_registry(&[], Some("missing"));
    print_run_registry(&[], None);
    print_run_registry_error("boom");
}
