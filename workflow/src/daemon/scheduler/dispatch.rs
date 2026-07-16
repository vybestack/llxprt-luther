use std::thread;

use rusqlite::Connection;

use super::{LeaseStatePreservedDetail, RunSummary, SchedulerError};
use crate::adapters::github_issues::GithubIssueQuery;
use crate::daemon::claim::cleanup_remote_claim;
use crate::daemon::launcher::{
    finish_lease_after_result, LaunchOutcome, LaunchRequest, WorkflowLauncher,
};
use crate::persistence::claim_metadata::{get_claim_metadata, upsert_claim_metadata};

#[derive(Debug, Clone)]
pub(super) struct DispatchUnit {
    pub(super) lease_id: String,
    pub(super) request: LaunchRequest,
    pub(super) resume: bool,
    pub(super) query_index: Option<usize>,
}

pub(super) fn dispatch_units(
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    queries: &[&dyn GithubIssueQuery],
    units: Vec<DispatchUnit>,
    max_parallel: usize,
    summary: &mut RunSummary,
) -> Result<(), SchedulerError> {
    let max_parallel = max_parallel.max(1);
    for chunk in units.chunks(max_parallel) {
        dispatch_unit_chunk(conn, launcher, queries, chunk, summary)?;
    }
    Ok(())
}

fn dispatch_unit_chunk(
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    queries: &[&dyn GithubIssueQuery],
    units: &[DispatchUnit],
    summary: &mut RunSummary,
) -> Result<(), SchedulerError> {
    thread::scope(|scope| {
        let handles: Vec<_> = units
            .iter()
            .map(|unit| {
                let handle = scope.spawn(move || {
                    if unit.resume {
                        launcher.resume(&unit.request)
                    } else {
                        launcher.launch(&unit.request)
                    }
                });
                (unit, handle)
            })
            .collect();
        let mut first_error = None;
        for (unit, handle) in handles {
            let result = match handle.join() {
                Ok(result) => result,
                Err(payload) => Err(format!(
                    "launcher thread panicked for lease={} run={}: {}",
                    unit.lease_id,
                    unit.request.run_id,
                    panic_message(payload.as_ref())
                )),
            };
            match finish_lease_after_result(conn, &unit.lease_id, &unit.request.run_id, result) {
                Ok(outcome) => {
                    if let Err(error) = finalize_claim_after_outcome(conn, queries, unit, &outcome)
                    {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                    record_outcome(outcome, unit.resume, summary);
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), |error| Err(error.into()))
    })
}

fn finalize_claim_after_outcome(
    conn: &Connection,
    queries: &[&dyn GithubIssueQuery],
    unit: &DispatchUnit,
    outcome: &LaunchOutcome,
) -> Result<(), rusqlite::Error> {
    if unit.resume || !matches!(outcome, LaunchOutcome::Launched { success: false, .. }) {
        return Ok(());
    }
    let (Some(query_index), Some(mut receipt)) =
        (unit.query_index, get_claim_metadata(conn, &unit.lease_id)?)
    else {
        return Ok(());
    };
    receipt.cleanup_pending = true;
    upsert_claim_metadata(conn, &receipt)?;
    if cleanup_remote_claim(
        queries[query_index],
        &unit.request.repo,
        unit.request.issue_number,
        &receipt,
    )
    .is_ok()
    {
        receipt.cleanup_pending = false;
        upsert_claim_metadata(conn, &receipt)?;
    }
    Ok(())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic payload")
}

pub(super) fn record_outcome(outcome: LaunchOutcome, was_resume: bool, summary: &mut RunSummary) {
    match outcome {
        LaunchOutcome::Launched { success: true, .. } if was_resume => summary.resumed += 1,
        LaunchOutcome::Launched { success: true, .. } => summary.launched += 1,
        LaunchOutcome::Launched { success: false, .. } => summary.failed += 1,
        LaunchOutcome::WaitingExternal { .. } => summary.suspended += 1,
        LaunchOutcome::LeaseStatePreserved {
            run_id,
            current_status,
            current_run_id,
        } => summary.record_lease_state_preserved(LeaseStatePreservedDetail {
            run_id,
            current_status,
            current_run_id,
        }),
        LaunchOutcome::Skipped(_) => summary.skipped += 1,
    }
}
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::adapters::github::GithubError;
    use crate::adapters::github_issues::GithubIssue;
    use crate::daemon::launcher::WorkflowLaunchResult;
    use crate::persistence::claim_metadata::{upsert_claim_metadata, ClaimMetadataReceipt};
    use crate::persistence::leases::{
        get_lease_for_issue, init_leases_table, try_claim, update_lease_status, LeaseStatus,
    };

    struct MutableQuery {
        issue: Mutex<GithubIssue>,
    }

    impl GithubIssueQuery for MutableQuery {
        fn list_issues(
            &self,
            _repo: &str,
            _labels: &[String],
            _states: &[String],
        ) -> Result<Vec<GithubIssue>, GithubError> {
            Ok(Vec::new())
        }

        fn has_open_pr_for_issue(&self, _repo: &str, _number: u64) -> Result<bool, GithubError> {
            Ok(false)
        }

        fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
            Ok(Vec::new())
        }

        fn get_issue(&self, _repo: &str, _number: u64) -> Result<Option<GithubIssue>, GithubError> {
            Ok(Some(self.issue.lock().unwrap().clone()))
        }

        fn remove_label(&self, _repo: &str, _number: u64, label: &str) -> Result<(), GithubError> {
            self.issue
                .lock()
                .unwrap()
                .labels
                .retain(|value| !value.eq_ignore_ascii_case(label));
            Ok(())
        }

        fn remove_assignee(
            &self,
            _repo: &str,
            _number: u64,
            login: &str,
        ) -> Result<(), GithubError> {
            self.issue
                .lock()
                .unwrap()
                .assignees
                .retain(|value| !value.eq_ignore_ascii_case(login));
            Ok(())
        }
    }

    enum LauncherFailure {
        Error,
        Panic,
    }

    impl WorkflowLauncher for LauncherFailure {
        fn launch(&self, _request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
            match self {
                Self::Error => Err("pre-launch failure".to_owned()),
                Self::Panic => panic!("pre-launch panic"),
            }
        }
    }

    fn issue(assignees: &[&str]) -> GithubIssue {
        GithubIssue {
            number: 42,
            title: "claimed".to_owned(),
            state: "open".to_owned(),
            labels: vec!["Luther working".to_owned()],
            assignees: assignees.iter().map(|value| (*value).to_owned()).collect(),
            milestone: None,
            body: None,
        }
    }

    fn run_failed_dispatch(
        launcher: &dyn WorkflowLauncher,
        assignment_added: bool,
        assignees: &[&str],
    ) -> (GithubIssue, ClaimMetadataReceipt, LeaseStatus) {
        let conn = Connection::open_in_memory().unwrap();
        init_leases_table(&conn).unwrap();
        let lease = try_claim(&conn, "owner/repo", 42, "cfg").unwrap().unwrap();
        update_lease_status(&conn, &lease.lease_id, LeaseStatus::Running, Some("run-42")).unwrap();
        let receipt = ClaimMetadataReceipt {
            lease_id: lease.lease_id.clone(),
            assignee: "acoliver".to_owned(),
            label: "Luther working".to_owned(),
            assignment_added,
            label_added: true,
            cleanup_pending: false,
        };
        upsert_claim_metadata(&conn, &receipt).unwrap();
        let query = MutableQuery {
            issue: Mutex::new(issue(assignees)),
        };
        let unit = DispatchUnit {
            lease_id: lease.lease_id,
            request: LaunchRequest {
                config_id: "cfg".to_owned(),
                workflow_type_id: None,
                run_id: "run-42".to_owned(),
                repo: "owner/repo".to_owned(),
                issue_number: 42,
                daemon_managed_claim: true,
                claim_assignment_added: assignment_added,
                claim_label_added: true,
                work_dir: None,
                artifact_dir: None,
            },
            resume: false,
            query_index: Some(0),
        };
        dispatch_units(
            &conn,
            launcher,
            &[&query],
            vec![unit],
            1,
            &mut RunSummary::default(),
        )
        .unwrap();
        let current = query.issue.lock().unwrap().clone();
        let receipt = get_claim_metadata(&conn, &receipt.lease_id)
            .unwrap()
            .unwrap();
        let status = get_lease_for_issue(&conn, "owner/repo", 42)
            .unwrap()
            .unwrap()
            .status;
        (current, receipt, status)
    }

    #[test]
    fn launcher_error_cleans_owned_claim_metadata() {
        let (current, receipt, status) =
            run_failed_dispatch(&LauncherFailure::Error, true, &["acoliver"]);
        assert!(current.labels.is_empty());
        assert!(current.assignees.is_empty());
        assert!(!receipt.cleanup_pending);
        assert_eq!(status, LeaseStatus::Failed);
    }

    #[test]
    fn launcher_panic_preserves_preexisting_assignment() {
        let (current, receipt, status) =
            run_failed_dispatch(&LauncherFailure::Panic, false, &["reviewer", "acoliver"]);
        assert!(current.labels.is_empty());
        assert_eq!(current.assignees, ["reviewer", "acoliver"]);
        assert!(!receipt.cleanup_pending);
        assert_eq!(status, LeaseStatus::Failed);
    }
}
