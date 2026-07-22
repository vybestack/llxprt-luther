use super::*;
use crate::persistence::leases::{
    count_active_leases_for_config, get_lease_for_issue, init_leases_table, try_claim,
};
use std::sync::Mutex;

fn cfg(max: u32) -> DiscoveryConfig {
    DiscoveryConfig {
        enabled: true,
        repo: Some("o/r".to_string()),
        include_labels: vec![],
        exclude_labels: vec![],
        active_parent_label: None,
        issue_states: vec!["open".to_string()],
        approval_label: None,
        approval_actor: None,
        claim_assignee: None,
        claim_label: None,
        milestone_order: Some("semver".to_string()),
        max_concurrent_runs: Some(max),
        poll_interval_secs: Some(300),
        max_concurrent_active_runs: None,
        max_concurrent_runs_per_repository: None,
        max_concurrent_runs_per_config: None,
        route_parent_issues: false,
        parent_workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        parent_config_id: None,
        skip_children_of_active_parents: false,
    }
}

fn issue(number: u64) -> GithubIssue {
    GithubIssue {
        number,
        title: format!("Issue {number}"),
        state: "open".to_string(),
        labels: vec![],
        assignees: vec![],
        milestone: None,
        body: None,
    }
}

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    init_leases_table(&c).unwrap();
    crate::persistence::sqlite::init_runs_schema(&c).unwrap();
    crate::persistence::claim_metadata::init_claim_metadata_table(&c).unwrap();
    crate::persistence::wait_state::init_wait_states_table(&c).unwrap();
    crate::persistence::checkpoint::init_checkpoint_table(&c).unwrap();
    c
}

/// Seed a complete, pollable external wait using the production-path
/// `persist_external_wait` function, establishing the full invariant:
/// run status, checkpoint, wait_states row, and waiting lease.
fn seed_complete_external_wait(
    c: &Connection,
    issue_number: u64,
    run_id: &str,
    resume_step: &str,
) -> String {
    let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    // Seed run metadata + checkpoint (required by persist_external_wait).
    let metadata = crate::persistence::RunMetadata::new(run_id, "wf", "cfg");
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        c,
        &crate::persistence::checkpoint::Checkpoint::new(run_id, resume_step),
    )
    .unwrap();
    let mut record = crate::persistence::wait_state::WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease.lease_id.clone());
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.resume_step = resume_step.to_string();
    crate::persistence::persist_external_wait(c, &record).unwrap();
    lease.lease_id
}

/// Records launch requests and returns a preset success flag.
struct MockLauncher {
    result: WorkflowLaunchResult,
    requests: Mutex<Vec<LaunchRequest>>,
}

impl MockLauncher {
    fn new(result: WorkflowLaunchResult) -> Self {
        Self {
            result,
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl WorkflowLauncher for MockLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.result.clone())
    }
}

#[path = "tests/launch_cases.rs"]
mod launch_cases;
#[path = "tests/resume_cases.rs"]
mod resume_cases;
