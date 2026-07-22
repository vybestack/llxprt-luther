use rusqlite::Connection;

use crate::adapters::github::GithubError;
use crate::adapters::github_issues::{GithubIssue, GithubIssueQuery};
use crate::daemon::discovery::SkipReason;
use crate::persistence::claim_metadata::{
    upsert_claim_metadata, ClaimMetadataReceipt, PendingClaimCleanup,
};
use crate::persistence::leases::{
    count_active_leases_for_config, try_claim, update_lease_status, IssueLease, LeaseStatus,
};
use crate::workflow::schema::DiscoveryConfig;

use super::launcher::{DaemonPathBases, LaunchRequest};

pub struct ClaimedLaunch {
    pub lease_id: String,
    pub request: LaunchRequest,
}

pub fn claim_for_launch(
    issue: &GithubIssue,
    cfg: &DiscoveryConfig,
    conn: &Connection,
    config_id: &str,
    bases: &DaemonPathBases,
    config_root: &std::path::Path,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    claim_for_launch_with_state(issue, cfg, conn, config_id, bases, true, config_root)
}

pub(crate) fn claim_for_launch_pending(
    issue: &GithubIssue,
    cfg: &DiscoveryConfig,
    conn: &Connection,
    config_id: &str,
    bases: &DaemonPathBases,
    config_root: &std::path::Path,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    claim_for_launch_with_state(issue, cfg, conn, config_id, bases, false, config_root)
}

fn claim_for_launch_with_state(
    issue: &GithubIssue,
    cfg: &DiscoveryConfig,
    conn: &Connection,
    config_id: &str,
    bases: &DaemonPathBases,
    mark_running: bool,
    config_root: &std::path::Path,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    let repo = cfg.repo.clone().unwrap_or_default();
    let lease = match acquire_lease_with_receipt(conn, &repo, issue.number, config_id, cfg)? {
        Some(lease) => lease,
        None => return Ok(Err(SkipReason::HasActiveLease)),
    };
    let max = cfg
        .max_concurrent_runs_per_config
        .or(cfg.max_concurrent_runs)
        .unwrap_or(1) as usize;
    if count_active_leases_for_config(conn, config_id)? > max {
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Abandoned, None)?;
        return Ok(Err(SkipReason::ConcurrencyLimitReached));
    }
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let paths = match bases.per_run_paths(issue.number, &run_id) {
        Ok(paths) => paths,
        Err(error) => {
            update_lease_status(conn, &lease.lease_id, LeaseStatus::Abandoned, None)?;
            return Ok(Err(SkipReason::InvalidPath(error)));
        }
    };
    if mark_running {
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(&run_id))?;
    }
    Ok(Ok(ClaimedLaunch {
        lease_id: lease.lease_id,
        request: LaunchRequest {
            config_id: config_id.to_owned(),
            workflow_type_id: None,
            run_id,
            repo,
            issue_number: issue.number,
            daemon_managed_claim: false,
            claim_assignment_added: false,
            claim_label_added: false,
            work_dir: paths.work_dir,
            artifact_dir: paths.artifact_dir,
            // Issue 158 finding 5: flow the target's config root (which
            // originated from the supervisor's --config-dir) rather than
            // hardcoding "config".
            config_root: config_root.to_path_buf(),
        },
    }))
}

fn acquire_lease_with_receipt(
    conn: &Connection,
    repo: &str,
    issue_number: u64,
    config_id: &str,
    config: &DiscoveryConfig,
) -> Result<Option<IssueLease>, rusqlite::Error> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let lease = try_claim(conn, repo, issue_number, config_id)?;
        if let Some(lease) = lease.as_ref() {
            // Persist a receipt for every claimed lease, even when the config
            // has zero or one optional claim fields. The resume path
            // (prepare_resume_lease) requires a receipt to reconstruct claim
            // ownership, so omitting it makes a lease permanently un-resumable.
            upsert_claim_metadata(
                conn,
                &ClaimMetadataReceipt {
                    lease_id: lease.lease_id.clone(),
                    assignee: config.claim_assignee.clone().unwrap_or_default(),
                    label: config.claim_label.clone().unwrap_or_default(),
                    assignment_added: false,
                    label_added: false,
                    cleanup_pending: false,
                },
            )?;
        }
        Ok(lease)
    })();
    match result {
        Ok(lease) => {
            conn.execute_batch("COMMIT")?;
            Ok(lease)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaimOwnership {
    pub assignment_added: bool,
    pub label_added: bool,
}

pub(crate) fn inspect_remote_claim(
    query: &dyn GithubIssueQuery,
    config: &DiscoveryConfig,
    issue: &GithubIssue,
) -> Result<ClaimOwnership, GithubError> {
    let (Some(repo), Some(assignee), Some(label)) = (
        config.repo.as_deref(),
        config.claim_assignee.as_deref(),
        config.claim_label.as_deref(),
    ) else {
        return Ok(ClaimOwnership::default());
    };
    let current = query
        .get_issue(repo, issue.number)?
        .unwrap_or_else(|| issue.clone());
    Ok(ClaimOwnership {
        assignment_added: !current
            .assignees
            .iter()
            .any(|login| login.eq_ignore_ascii_case(assignee)),
        label_added: !current
            .labels
            .iter()
            .any(|value| value.eq_ignore_ascii_case(label)),
    })
}

pub(crate) fn apply_remote_claim(
    query: &dyn GithubIssueQuery,
    config: &DiscoveryConfig,
    issue: &GithubIssue,
    ownership: ClaimOwnership,
) -> Result<(), GithubError> {
    let (Some(repo), Some(assignee), Some(label)) = (
        config.repo.as_deref(),
        config.claim_assignee.as_deref(),
        config.claim_label.as_deref(),
    ) else {
        return Ok(());
    };
    if ownership.assignment_added {
        query.add_assignee(repo, issue.number, assignee)?;
    }
    if ownership.label_added {
        query.add_label(repo, issue.number, label)?;
    }
    Ok(())
}

pub(crate) fn verify_remote_claim(
    query: &dyn GithubIssueQuery,
    config: &DiscoveryConfig,
    issue_number: u64,
) -> Result<(), GithubError> {
    let (Some(repo), Some(assignee), Some(label)) = (
        config.repo.as_deref(),
        config.claim_assignee.as_deref(),
        config.claim_label.as_deref(),
    ) else {
        return Ok(());
    };
    let issue = query
        .get_issue(repo, issue_number)?
        .ok_or_else(|| GithubError::NotFound {
            resource: format!("claim metadata for {repo}#{issue_number}"),
        })?;
    let assignment_present = issue
        .assignees
        .iter()
        .any(|login| login.eq_ignore_ascii_case(assignee));
    let label_present = issue
        .labels
        .iter()
        .any(|value| value.eq_ignore_ascii_case(label));
    if assignment_present && label_present {
        Ok(())
    } else {
        Err(GithubError::NotFound {
            resource: format!("verified claim metadata for {repo}#{issue_number}"),
        })
    }
}

pub(crate) fn cleanup_remote_claim(
    query: &dyn GithubIssueQuery,
    repo: &str,
    issue_number: u64,
    receipt: &ClaimMetadataReceipt,
) -> Result<(), GithubError> {
    let Some(issue) = query.get_issue(repo, issue_number)? else {
        return Ok(());
    };
    let mut first_error = None;
    if receipt.label_added
        && issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(&receipt.label))
    {
        if let Err(error) = query.remove_label(repo, issue_number, &receipt.label) {
            first_error = Some(error);
        }
    }
    if receipt.assignment_added
        && issue
            .assignees
            .iter()
            .any(|login| login.eq_ignore_ascii_case(&receipt.assignee))
    {
        if let Err(error) = query.remove_assignee(repo, issue_number, &receipt.assignee) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn reconcile_pending_cleanup(
    query: &dyn GithubIssueQuery,
    cleanup: &PendingClaimCleanup,
) -> Result<(), GithubError> {
    cleanup_remote_claim(
        query,
        &cleanup.issue_repo,
        cleanup.issue_number,
        &cleanup.receipt,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{
        acquire_lease_with_receipt, apply_remote_claim, cleanup_remote_claim, inspect_remote_claim,
        verify_remote_claim,
    };
    use crate::adapters::github::GithubError;
    use crate::adapters::github_issues::{GithubIssue, GithubIssueQuery};
    use crate::persistence::claim_metadata::{get_claim_metadata, ClaimMetadataReceipt};
    use crate::persistence::leases::init_leases_table;
    use crate::workflow::schema::DiscoveryConfig;

    #[derive(Default)]
    struct RecordingQuery {
        issue: Option<GithubIssue>,
        mutations: Mutex<Vec<String>>,
    }

    impl GithubIssueQuery for RecordingQuery {
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
            Ok(self.issue.clone())
        }

        fn add_label(&self, _repo: &str, _number: u64, label: &str) -> Result<(), GithubError> {
            self.mutations
                .lock()
                .unwrap()
                .push(format!("label:{label}"));
            Ok(())
        }

        fn add_assignee(&self, _repo: &str, _number: u64, login: &str) -> Result<(), GithubError> {
            self.mutations
                .lock()
                .unwrap()
                .push(format!("assign:{login}"));
            Ok(())
        }
    }

    fn issue(assignee: Option<&str>, labels: &[&str]) -> GithubIssue {
        GithubIssue {
            number: 42,
            title: "approved".to_owned(),
            state: "open".to_owned(),
            labels: labels.iter().map(|value| (*value).to_owned()).collect(),
            assignees: assignee.into_iter().map(str::to_owned).collect(),
            milestone: None,
            body: None,
        }
    }

    fn config() -> DiscoveryConfig {
        DiscoveryConfig {
            repo: Some("owner/repo".to_owned()),
            claim_assignee: Some("acoliver".to_owned()),
            claim_label: Some("Luther working".to_owned()),
            ..DiscoveryConfig::default()
        }
    }

    #[test]
    fn applies_missing_claim_metadata() {
        let query = RecordingQuery {
            issue: Some(issue(None, &[])),
            ..RecordingQuery::default()
        };
        let candidate = issue(None, &[]);
        let ownership = inspect_remote_claim(&query, &config(), &candidate).unwrap();
        apply_remote_claim(&query, &config(), &candidate, ownership).unwrap();
        assert!(ownership.assignment_added);
        assert!(ownership.label_added);
        assert_eq!(
            *query.mutations.lock().unwrap(),
            ["assign:acoliver", "label:Luther working"]
        );
    }

    #[test]
    fn preserves_preexisting_claim_metadata() {
        let existing = issue(Some("acoliver"), &["Luther working"]);
        let query = RecordingQuery {
            issue: Some(existing.clone()),
            ..RecordingQuery::default()
        };
        let ownership = inspect_remote_claim(&query, &config(), &existing).unwrap();
        apply_remote_claim(&query, &config(), &existing, ownership).unwrap();
        assert_eq!(ownership, Default::default());
        assert!(query.mutations.lock().unwrap().is_empty());
    }

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

        fn add_label(&self, _repo: &str, _number: u64, label: &str) -> Result<(), GithubError> {
            self.issue.lock().unwrap().labels.push(label.to_owned());
            Ok(())
        }

        fn remove_label(&self, _repo: &str, _number: u64, label: &str) -> Result<(), GithubError> {
            self.issue
                .lock()
                .unwrap()
                .labels
                .retain(|value| !value.eq_ignore_ascii_case(label));
            Ok(())
        }

        fn add_assignee(&self, _repo: &str, _number: u64, login: &str) -> Result<(), GithubError> {
            self.issue.lock().unwrap().assignees.push(login.to_owned());
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

    #[test]
    fn verifies_and_cleans_only_metadata_introduced_by_claim() {
        let candidate = issue(Some("reviewer"), &["approved"]);
        let query = MutableQuery {
            issue: Mutex::new(candidate.clone()),
        };
        let ownership = inspect_remote_claim(&query, &config(), &candidate).unwrap();
        apply_remote_claim(&query, &config(), &candidate, ownership).unwrap();
        verify_remote_claim(&query, &config(), candidate.number).unwrap();
        cleanup_remote_claim(
            &query,
            "owner/repo",
            candidate.number,
            &ClaimMetadataReceipt {
                lease_id: "lease".to_owned(),
                assignee: "acoliver".to_owned(),
                label: "Luther working".to_owned(),
                assignment_added: ownership.assignment_added,
                label_added: ownership.label_added,
                cleanup_pending: true,
            },
        )
        .unwrap();
        assert_eq!(*query.issue.lock().unwrap(), candidate);
    }

    #[test]
    fn concurrent_lease_claim_creates_exactly_one_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("claims.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        init_leases_table(&conn).unwrap();
        drop(conn);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let conn = rusqlite::Connection::open(database).unwrap();
                    conn.busy_timeout(Duration::from_secs(5)).unwrap();
                    barrier.wait();
                    acquire_lease_with_receipt(&conn, "owner/repo", 42, "cfg", &config()).unwrap()
                })
            })
            .collect();
        let winners: Vec<_> = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(winners.len(), 1);
        let conn = rusqlite::Connection::open(database).unwrap();
        assert!(get_claim_metadata(&conn, &winners[0].lease_id)
            .unwrap()
            .is_some());
        let receipt_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM claim_metadata", [], |row| row.get(0))
            .unwrap();
        assert_eq!(receipt_count, 1);
    }

    #[test]
    fn claim_with_zero_claim_fields_persists_receipt() {
        // Issue-137: a lease claimed with no claim_assignee and no claim_label
        // must still persist a ClaimMetadataReceipt so the resume path can
        // reconstruct ownership. Without a receipt, prepare_resume_lease skips
        // the resume, stranding the lease permanently.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_leases_table(&conn).unwrap();
        let cfg = DiscoveryConfig {
            repo: Some("owner/repo".to_owned()),
            claim_assignee: None,
            claim_label: None,
            ..DiscoveryConfig::default()
        };
        let lease = acquire_lease_with_receipt(&conn, "owner/repo", 50, "cfg", &cfg)
            .unwrap()
            .expect("claim should succeed");
        let receipt = get_claim_metadata(&conn, &lease.lease_id)
            .unwrap()
            .expect("a receipt must be persisted even with zero claim fields");
        assert_eq!(receipt.lease_id, lease.lease_id);
        assert!(receipt.assignee.is_empty());
        assert!(receipt.label.is_empty());
        assert!(!receipt.assignment_added);
        assert!(!receipt.label_added);
        assert!(!receipt.cleanup_pending);
    }

    #[test]
    fn claim_with_one_claim_field_persists_receipt() {
        // Issue-137: a lease claimed with exactly one optional claim field
        // (assignee but no label) must still persist a ClaimMetadataReceipt.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_leases_table(&conn).unwrap();
        let cfg = DiscoveryConfig {
            repo: Some("owner/repo".to_owned()),
            claim_assignee: Some("acoliver".to_owned()),
            claim_label: None,
            ..DiscoveryConfig::default()
        };
        let lease = acquire_lease_with_receipt(&conn, "owner/repo", 51, "cfg", &cfg)
            .unwrap()
            .expect("claim should succeed");
        let receipt = get_claim_metadata(&conn, &lease.lease_id)
            .unwrap()
            .expect("a receipt must be persisted with one claim field");
        assert_eq!(receipt.lease_id, lease.lease_id);
        assert_eq!(receipt.assignee, "acoliver");
        assert!(receipt.label.is_empty());
    }
}
