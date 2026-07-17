//! Daemon scheduler loop: discover -> claim+launch up to the concurrency limit.
//!
//! `run_once_with_bases` performs a single-target discovery/launch pass; `run_loop` recovers
//! stale leases at startup then repeats `run_multi_target_once` on the configured poll
//! interval until a shutdown flag is set.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-006,REQ-DAEMON-DISCOVERY-007

use std::borrow::Cow;
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssueQuery;
use crate::daemon::claim::{
    apply_remote_claim, cleanup_remote_claim, inspect_remote_claim, reconcile_pending_cleanup,
    verify_remote_claim, ClaimOwnership,
};
use crate::daemon::discovery::{discover, RoutedIssue};
use crate::daemon::launcher::{
    claim_for_launch_pending, prepare_resume_lease, ClaimedLaunch, DaemonPathBases, LaunchRequest,
    WorkflowLauncher,
};
use crate::daemon::poller::{
    apply_poll_decision, ArtifactPhase, ExternalWaitPoller, PollApplyError, PollApplyOutcome,
    SystemExternalWaitPoller,
};
use crate::persistence::claim_metadata::{
    get_claim_metadata, list_pending_claim_cleanups, upsert_claim_metadata, ClaimMetadataReceipt,
};
use crate::persistence::leases::{
    count_active_leases, count_active_leases_for_config, count_active_leases_for_repository,
    list_ready_to_resume_leases, mark_stale_leases, mark_stale_ready_to_resume_leases,
    update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome, IssueLease,
    LeaseStatus,
};
use crate::persistence::wait_state::list_pollable_wait_states;
use crate::workflow::schema::DiscoveryConfig;

mod dispatch;

#[cfg(test)]
use crate::daemon::launcher::LaunchOutcome;
#[cfg(test)]
use dispatch::record_outcome;
use dispatch::{dispatch_units, DispatchUnit};

/// Maximum number of per-run skipped-poll details retained in one pass.
pub const MAX_SKIPPED_POLL_DETAILS: usize = 100;
/// Maximum number of preserved lease-state details retained in one pass.
pub const MAX_LEASE_STATE_PRESERVED_DETAILS: usize = 100;
/// Maximum number of post-commit artifact warning details retained in one pass.
pub const MAX_ARTIFACT_WARNING_DETAILS: usize = 100;

/// Summary of a single scheduler pass.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub eligible: usize,
    pub launched: usize,
    pub resumed: usize,
    pub suspended: usize,
    pub failed: usize,
    /// Engine results that lost a guarded lease transition to newer durable state.
    pub lease_states_preserved: usize,
    /// Bounded details identifying preserved lease transitions.
    pub lease_state_preserved_details: Vec<LeaseStatePreservedDetail>,
    /// Number of preserved-state details omitted after the per-pass cap.
    pub lease_state_preserved_details_dropped: usize,
    pub skipped: usize,
    pub pollable_waits: usize,
    pub polls_applied: usize,
    /// Polls skipped because of concurrent transitions or row-level integrity
    /// violations. These are observable but not counted in `polls_applied` so
    /// the applied metric remains reliable.
    pub skipped_polls: usize,
    /// Human-readable details for skipped polls, preserving the run id and
    /// skip reason for operator inspection. Capped at
    /// [`MAX_SKIPPED_POLL_DETAILS`] per pass.
    pub skipped_poll_details: Vec<SkippedPollDetail>,
    /// Number of skipped-poll details omitted after the per-pass cap was
    /// reached. `skipped_polls` remains the authoritative total count.
    pub skipped_poll_details_dropped: usize,
    /// Post-commit artifact write warnings. The DB transaction committed but
    /// one or more observability artifacts failed to persist. The committed
    /// DB state is authoritative; these warnings are advisory and capped at
    /// [`MAX_ARTIFACT_WARNING_DETAILS`] per pass.
    pub artifact_warnings: Vec<ArtifactWarningDetail>,
    /// Number of artifact-warning details omitted after the per-pass cap.
    pub artifact_warnings_dropped: usize,
}

impl RunSummary {
    fn record_lease_state_preserved(&mut self, detail: LeaseStatePreservedDetail) {
        self.lease_states_preserved += 1;
        if self.lease_state_preserved_details.len() < MAX_LEASE_STATE_PRESERVED_DETAILS {
            self.lease_state_preserved_details.push(detail);
        } else {
            self.lease_state_preserved_details_dropped += 1;
        }
    }

    fn record_skipped_poll(&mut self, detail: SkippedPollDetail) {
        self.skipped_polls += 1;
        if self.skipped_poll_details.len() < MAX_SKIPPED_POLL_DETAILS {
            self.skipped_poll_details.push(detail);
        } else {
            self.skipped_poll_details_dropped += 1;
        }
    }

    fn record_artifact_warning(&mut self, detail: ArtifactWarningDetail) {
        if self.artifact_warnings.len() < MAX_ARTIFACT_WARNING_DETAILS {
            self.artifact_warnings.push(detail);
        } else {
            self.artifact_warnings_dropped += 1;
        }
    }

    /// Total artifact warnings observed, including details omitted by the cap.
    pub fn artifact_warning_count(&self) -> usize {
        self.artifact_warnings
            .len()
            .saturating_add(self.artifact_warnings_dropped)
    }
}

/// Structured detail for a guarded launcher result that preserved newer lease state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseStatePreservedDetail {
    /// Run whose stale engine result lost the guarded transition.
    pub run_id: String,
    /// Durable lease status observed after rejection, or `None` when missing.
    pub current_status: Option<crate::persistence::leases::LeaseStatus>,
    /// Durable owner observed after rejection, or `None` when absent/missing.
    pub current_run_id: Option<String>,
}

/// Structured detail for a post-commit artifact write warning.
///
/// The DB transaction already committed, so this is advisory only — the
/// committed run/lease/wait-state fact cannot be rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactWarningDetail {
    /// Run whose post-commit artifact write failed.
    pub run_id: String,
    /// Artifact phase that failed after the database commit.
    pub phase: ArtifactPhase,
    /// Error reported by the artifact writer.
    pub error: String,
}

/// Structured detail for a single poll skip.
///
/// Recorded in [`RunSummary::skipped_poll_details`] so operators can see
/// *which* run was skipped and *why* without grepping logs. Not counted in
/// `polls_applied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPollDetail {
    /// Run whose stale poll decision was skipped.
    pub run_id: String,
    /// Lease involved in the skip, when the wait record has one.
    pub lease_id: Option<String>,
    /// Resume step whose poll decision was skipped.
    pub step_id: String,
    /// Domain reason the poll was skipped.
    pub reason: SkippedPollReason,
    /// Precise lease guard rejection, when [`SkippedPollReason::LeaseTransitionRejected`].
    pub lease_transition_reason: Option<&'static str>,
}

/// Categorises why an individual poll was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedPollReason {
    /// The lease advanced past the expected status set (concurrent poller or
    /// launcher classification).
    LeaseTransitionRejected,
    /// Another path already processed this wait-state row.
    WaitStateConcurrentTransition,
    /// The run already advanced to a terminal status, so the stale poller's
    /// status update was rejected by the conditional guard.
    RunStatusConcurrentTransition,
    /// A pollable wait-state row has no backing run metadata. This integrity
    /// violation is skipped so it cannot block unrelated waits.
    RunMissing,
}

/// Domain error returned by the scheduler pass.
///
/// Carries both the poll-phase domain errors ([`PollApplyError`]) and the
/// SQLite errors from the discovery/launch/resume phases without converting
/// between them, so callers can pattern-match on the precise failure kind.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// External-wait poll application failed (integrity failure, persistence
    /// failure, or database error from the poll transaction).
    #[error(transparent)]
    PollApply(#[from] PollApplyError),
    /// Database error from discovery, launch, or resume phases.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Internal invariant violation: the caller passed mismatched targets and
    /// query slices. Surfaced as a structured error instead of a silent
    /// eprintln + Ok so the scheduler pass is not silently degraded.
    #[error("scheduler invariant violated: targets len ({targets}) != queries len ({queries})")]
    TargetsQueriesMismatch { targets: usize, queries: usize },
}

#[derive(Debug, Clone)]
pub struct SchedulerTarget {
    pub config_id: String,
    pub discovery: DiscoveryConfig,
    /// Resolved daemon base roots for this target's launch config, used to
    /// construct isolated per-run work/artifact directories. @plan:issue-117
    pub path_bases: DaemonPathBases,
    /// Parent-routed base roots keyed by config id, used when a routed parent
    /// issue launches under a parent config id. @plan:issue-117
    pub parent_path_bases: BTreeMap<String, DaemonPathBases>,
}

impl SchedulerTarget {
    #[must_use]
    pub fn new(
        config_id: String,
        discovery: DiscoveryConfig,
        path_bases: DaemonPathBases,
        parent_path_bases: BTreeMap<String, DaemonPathBases>,
    ) -> Self {
        Self {
            config_id,
            discovery,
            path_bases,
            parent_path_bases,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CapacityLimits {
    pub global: usize,
    pub per_config: usize,
    pub per_repository: usize,
}

/// Execute a single discovery + launch pass.
///
/// Discovers eligible issues (accounting for already-active leases), then for
/// each eligible issue attempts `claim_and_launch`, stopping when launches
/// reach the per-config concurrency budget.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-006
pub fn run_once_with_bases(
    cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    config_id: &str,
    path_bases: DaemonPathBases,
    parent_path_bases: BTreeMap<String, DaemonPathBases>,
) -> Result<RunSummary, SchedulerError> {
    let poller = SystemExternalWaitPoller::new();
    let target = SchedulerTarget::new(
        config_id.to_string(),
        cfg.clone(),
        path_bases,
        parent_path_bases,
    );
    run_multi_target_once_with_poller(&[target], &[q], conn, launcher, &poller)
}

pub fn run_multi_target_once(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
) -> Result<RunSummary, SchedulerError> {
    let poller = SystemExternalWaitPoller::new();
    run_multi_target_once_with_poller(targets, queries, conn, launcher, &poller)
}

/// Execute a single multi-target discovery + launch pass with an explicit poller.
///
/// # Invariants
///
/// `targets` and `queries` must have equal length: each target is zipped with
/// its corresponding query for discovery. A length mismatch is a programming
/// error (not a runtime condition) and returns
/// [`SchedulerError::TargetsQueriesMismatch`] rather than silently degrading.
///
/// All production callers satisfy this invariant by construction:
/// - [`run_once_with_bases`] passes single-element slices.
/// - [`run_multi_target_once`] forwards the caller's slices unchanged.
/// - `run_supervisor_scheduler_pass` builds `queries` via
///   `targets.iter().map(|_| query)` (1:1).
/// - `run_discovery_scheduler_pass` passes single-element slices.
/// - `run_loop` passes single-element slices via `slice::from_ref`.
///
/// The error is surfaced (not swallowed) so that a future caller violating the
/// invariant fails loudly instead of launching against the wrong queries.
pub fn run_multi_target_once_with_poller(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    poller: &dyn ExternalWaitPoller,
) -> Result<RunSummary, SchedulerError> {
    if targets.len() != queries.len() {
        return Err(SchedulerError::TargetsQueriesMismatch {
            targets: targets.len(),
            queries: queries.len(),
        });
    }
    reconcile_claim_cleanups(targets, queries, conn)?;
    let mut summary = poll_due_waits(conn, poller)?;
    let limits = capacity_limits(targets);
    let mut units = collect_resume_units(targets, conn, &limits)?;
    collect_launch_units(targets, queries, conn, &limits, &mut units, &mut summary)?;
    let max_parallel = dispatch_parallelism(&limits, units.len());
    dispatch_units(conn, launcher, queries, units, max_parallel, &mut summary)?;
    Ok(summary)
}

fn reconcile_claim_cleanups(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
) -> Result<(), SchedulerError> {
    for (target, query) in targets.iter().zip(queries.iter()) {
        let Some(repo) = target.discovery.repo.as_deref() else {
            continue;
        };
        for cleanup in list_pending_claim_cleanups(conn, repo)? {
            if cleanup.lease_status == LeaseStatus::Claimed {
                let transition = update_lease_status_conditional_outcome(
                    conn,
                    &cleanup.receipt.lease_id,
                    LeaseStatus::Pending,
                    &[LeaseStatus::Claimed],
                    None,
                    None,
                )?;
                if transition != ConditionalLeaseStatusOutcome::Applied {
                    continue;
                }
            }
            if let Err(error) = reconcile_pending_cleanup(*query, &cleanup) {
                eprintln!(
                    "claim cleanup reconciliation error for issue={}#{}: {error}",
                    cleanup.issue_repo, cleanup.issue_number
                );
                continue;
            }
            let mut receipt = cleanup.receipt;
            receipt.cleanup_pending = false;
            upsert_claim_metadata(conn, &receipt)?;
            if cleanup.lease_status == LeaseStatus::Claimed
                || cleanup.lease_status == LeaseStatus::Pending
            {
                update_lease_status_conditional_outcome(
                    conn,
                    &receipt.lease_id,
                    LeaseStatus::Abandoned,
                    &[LeaseStatus::Pending],
                    None,
                    None,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_resume_units(
    targets: &[SchedulerTarget],
    conn: &Connection,
    limits: &CapacityLimits,
) -> Result<Vec<DispatchUnit>, SchedulerError> {
    let mut units = Vec::new();
    for (resume_config_id, discovery) in resume_config_targets(targets, limits) {
        let ready_leases = match list_ready_to_resume_leases(conn, &resume_config_id) {
            Ok(leases) => leases,
            Err(e) => {
                eprintln!("resume discovery error for config={resume_config_id}: {e}");
                continue;
            }
        };
        for lease in ready_leases {
            if !has_capacity(
                conn,
                &discovery,
                &resume_config_id,
                &lease.issue_repo,
                limits,
            )? {
                continue;
            }
            match prepare_resume_unit(&lease, conn) {
                Ok(Some(unit)) => units.push(unit),
                Ok(None) => eprintln!(
                    "resume claim skipped for config={} issue={}#{}: invalid lease state",
                    resume_config_id, lease.issue_repo, lease.issue_number
                ),
                Err(e) => eprintln!(
                    "resume claim skipped for config={} issue={}#{}: {e}",
                    resume_config_id, lease.issue_repo, lease.issue_number
                ),
            }
        }
    }
    Ok(units)
}

fn resume_config_targets(
    targets: &[SchedulerTarget],
    limits: &CapacityLimits,
) -> Vec<(String, DiscoveryConfig)> {
    let mut config_targets: BTreeMap<String, (DiscoveryConfig, bool)> = BTreeMap::new();
    for target in targets {
        upsert_resume_config_target(
            &mut config_targets,
            target.config_id.clone(),
            target.discovery.clone(),
            true,
        );
        if let Some(parent_config_id) = target.discovery.parent_config_id.as_ref() {
            match parent_capacity_discovery(&target.discovery, limits) {
                Ok(discovery) => upsert_resume_config_target(
                    &mut config_targets,
                    parent_config_id.clone(),
                    discovery,
                    false,
                ),
                Err(err) => eprintln!(
                    "parent resume discovery capacity error for config={parent_config_id}: {err}"
                ),
            }
        }
    }
    config_targets
        .into_iter()
        .map(|(config_id, (discovery, _))| (config_id, discovery))
        .collect()
}

fn upsert_resume_config_target(
    config_targets: &mut BTreeMap<String, (DiscoveryConfig, bool)>,
    config_id: String,
    discovery: DiscoveryConfig,
    direct: bool,
) {
    match config_targets.entry(config_id) {
        Entry::Occupied(mut entry) => {
            let (existing_discovery, existing_direct) = entry.get_mut();
            if direct && !*existing_direct {
                *existing_discovery = discovery;
                *existing_direct = true;
            }
        }
        Entry::Vacant(entry) => {
            entry.insert((discovery, direct));
        }
    }
}

fn prepare_resume_unit(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Option<DispatchUnit>, SchedulerError> {
    let Ok(claimed) = prepare_resume_lease(lease, conn)? else {
        return Ok(None);
    };
    Ok(Some(DispatchUnit {
        lease_id: claimed.lease_id,
        request: claimed.request,
        resume: true,
        query_index: None,
    }))
}

fn persist_claim_ownership(
    conn: &Connection,
    lease_id: &str,
    config: &DiscoveryConfig,
    ownership: ClaimOwnership,
    cleanup_pending: bool,
) -> Result<(), rusqlite::Error> {
    // Persist ownership for every lease, even when the config has zero or one
    // optional claim fields, so the receipt always carries the authoritative
    // cleanup flags. `unwrap_or_default` keeps empty assignee/label for
    // configs that do not configure them.
    upsert_claim_metadata(
        conn,
        &ClaimMetadataReceipt {
            lease_id: lease_id.to_owned(),
            assignee: config.claim_assignee.clone().unwrap_or_default(),
            label: config.claim_label.clone().unwrap_or_default(),
            assignment_added: ownership.assignment_added,
            label_added: ownership.label_added,
            cleanup_pending,
        },
    )?;
    if get_claim_metadata(conn, lease_id)?.is_none() {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}
fn abandon_failed_claim(
    query: &dyn GithubIssueQuery,
    config: &DiscoveryConfig,
    conn: &Connection,
    claimed: &ClaimedLaunch,
    ownership: ClaimOwnership,
    error: impl std::fmt::Display,
) -> Result<(), rusqlite::Error> {
    let receipt = ClaimMetadataReceipt {
        lease_id: claimed.lease_id.clone(),
        assignee: config.claim_assignee.clone().unwrap_or_default(),
        label: config.claim_label.clone().unwrap_or_default(),
        assignment_added: ownership.assignment_added,
        label_added: ownership.label_added,
        cleanup_pending: true,
    };
    let cleanup_pending = cleanup_remote_claim(
        query,
        &claimed.request.repo,
        claimed.request.issue_number,
        &receipt,
    )
    .is_err();
    persist_claim_ownership(conn, &claimed.lease_id, config, ownership, cleanup_pending)?;
    update_lease_status_conditional_outcome(
        conn,
        &claimed.lease_id,
        LeaseStatus::Abandoned,
        &[LeaseStatus::Claimed, LeaseStatus::Pending],
        Some(&claimed.request.run_id),
        None,
    )?;
    eprintln!(
        "remote claim error for config={} issue={}#{}: {error}",
        claimed.request.config_id, claimed.request.repo, claimed.request.issue_number
    );
    Ok(())
}

fn apply_claim_ownership(request: &mut LaunchRequest, ownership: ClaimOwnership) {
    request.daemon_managed_claim = true;
    request.claim_assignment_added = ownership.assignment_added;
    request.claim_label_added = ownership.label_added;
}

enum PrepareLaunchOutcome {
    Ready(Box<DispatchUnit>),
    Skipped,
    Failed,
}

fn acquire_claimed_launch(
    target: &SchedulerTarget,
    routed: &RoutedIssue,
    discovery: &DiscoveryConfig,
    conn: &Connection,
    config_id: &str,
) -> Result<Result<ClaimedLaunch, PrepareLaunchOutcome>, SchedulerError> {
    let path_bases = path_bases_for(target, config_id);
    match claim_for_launch_pending(
        &routed.issue,
        discovery,
        conn,
        config_id,
        path_bases.as_ref(),
    ) {
        Ok(Ok(claimed)) => Ok(Ok(claimed)),
        Ok(Err(_)) => Ok(Err(PrepareLaunchOutcome::Skipped)),
        Err(error) => {
            eprintln!(
                "claim error for config={} issue={}#{}: {error}",
                config_id,
                target.discovery.repo.as_deref().unwrap_or(""),
                routed.issue.number
            );
            Ok(Err(PrepareLaunchOutcome::Failed))
        }
    }
}

fn prepare_launch_unit(
    target: &SchedulerTarget,
    query: &dyn GithubIssueQuery,
    query_index: usize,
    routed: &RoutedIssue,
    conn: &Connection,
    limits: &CapacityLimits,
    parent_discoveries: &BTreeMap<String, DiscoveryConfig>,
) -> Result<PrepareLaunchOutcome, SchedulerError> {
    let repo = target.discovery.repo.as_deref().unwrap_or("");
    let config_id = routed.config_id.as_deref().unwrap_or(&target.config_id);
    let discovery = launch_discovery_for(target, config_id, limits, parent_discoveries)?;
    if !has_capacity(conn, &discovery, config_id, repo, limits)? {
        return Ok(PrepareLaunchOutcome::Skipped);
    }
    let mut claimed = match acquire_claimed_launch(target, routed, &discovery, conn, config_id)? {
        Ok(claimed) => claimed,
        Err(outcome) => return Ok(outcome),
    };
    let ownership = match inspect_remote_claim(query, &discovery, &routed.issue) {
        Ok(ownership) => ownership,
        Err(error) => {
            abandon_failed_claim(
                query,
                &discovery,
                conn,
                &claimed,
                ClaimOwnership::default(),
                error,
            )?;
            return Ok(PrepareLaunchOutcome::Failed);
        }
    };
    persist_claim_ownership(conn, &claimed.lease_id, &discovery, ownership, true)?;
    let claim_result = apply_remote_claim(query, &discovery, &routed.issue, ownership)
        .and_then(|()| verify_remote_claim(query, &discovery, routed.issue.number));
    if let Err(error) = claim_result {
        abandon_failed_claim(query, &discovery, conn, &claimed, ownership, error)?;
        return Ok(PrepareLaunchOutcome::Failed);
    }
    let transition = update_lease_status_conditional_outcome(
        conn,
        &claimed.lease_id,
        LeaseStatus::Running,
        &[LeaseStatus::Claimed],
        Some(&claimed.request.run_id),
        None,
    )?;
    if transition != ConditionalLeaseStatusOutcome::Applied {
        abandon_failed_claim(
            query,
            &discovery,
            conn,
            &claimed,
            ownership,
            "lease ownership changed during remote claim",
        )?;
        return Ok(PrepareLaunchOutcome::Failed);
    }
    persist_claim_ownership(conn, &claimed.lease_id, &discovery, ownership, false)?;
    apply_claim_ownership(&mut claimed.request, ownership);
    claimed.request.workflow_type_id = routed.workflow_type_id.clone();
    Ok(PrepareLaunchOutcome::Ready(Box::new(DispatchUnit {
        lease_id: claimed.lease_id,
        request: claimed.request,
        resume: false,
        query_index: Some(query_index),
    })))
}

fn collect_launch_units(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    limits: &CapacityLimits,
    units: &mut Vec<DispatchUnit>,
    summary: &mut RunSummary,
) -> Result<(), SchedulerError> {
    let parent_discoveries = parent_launch_discoveries(targets, limits);
    for (query_index, (target, query)) in targets.iter().zip(queries.iter()).enumerate() {
        let active = count_active_leases_for_config(conn, &target.config_id)?;
        let result = match discover(&target.discovery, *query, conn, active) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("discovery error: {e}");
                continue;
            }
        };
        summary.eligible += result.eligible.len();
        for routed in &result.eligible {
            match prepare_launch_unit(
                target,
                *query,
                query_index,
                routed,
                conn,
                limits,
                &parent_discoveries,
            )? {
                PrepareLaunchOutcome::Ready(unit) => units.push(*unit),
                PrepareLaunchOutcome::Skipped => summary.skipped += 1,
                PrepareLaunchOutcome::Failed => summary.failed += 1,
            }
        }
    }
    Ok(())
}

fn launch_discovery_for<'a>(
    target: &'a SchedulerTarget,
    launch_config_id: &str,
    limits: &CapacityLimits,
    parent_discoveries: &'a BTreeMap<String, DiscoveryConfig>,
) -> Result<Cow<'a, DiscoveryConfig>, SchedulerError> {
    if launch_config_id == target.config_id {
        return Ok(Cow::Borrowed(&target.discovery));
    }
    if let Some(discovery) = parent_discoveries.get(launch_config_id) {
        return Ok(Cow::Borrowed(discovery));
    }
    parent_capacity_discovery(&target.discovery, limits).map(Cow::Owned)
}

/// Select the daemon path bases for a routed launch.
///
/// When the launch config id matches the target's own config, use the target's
/// own bases. When the launch config id is a parent config, use the
/// parent-routed bases registered for that parent config id, falling back to
/// empty bases (engine fallbacks apply) with a warning. @plan:issue-117
fn path_bases_for<'a>(
    target: &'a SchedulerTarget,
    launch_config_id: &str,
) -> Cow<'a, DaemonPathBases> {
    if launch_config_id == target.config_id {
        return Cow::Borrowed(&target.path_bases);
    }
    match target.parent_path_bases.get(launch_config_id) {
        Some(bases) => Cow::Borrowed(bases),
        None => {
            tracing::warn!(
                launch_config_id,
                "no daemon path bases found for routed parent config; using empty path bases"
            );
            Cow::Owned(DaemonPathBases::default())
        }
    }
}

fn parent_launch_discoveries(
    targets: &[SchedulerTarget],
    limits: &CapacityLimits,
) -> BTreeMap<String, DiscoveryConfig> {
    let mut discoveries = BTreeMap::new();
    let mut derived = BTreeMap::new();
    for target in targets {
        discoveries.insert(target.config_id.clone(), target.discovery.clone());
        if let Some(parent_config_id) = target.discovery.parent_config_id.as_ref() {
            match parent_capacity_discovery(&target.discovery, limits) {
                Ok(discovery) => {
                    derived.entry(parent_config_id.clone()).or_insert(discovery);
                }
                Err(err) => eprintln!(
                    "parent launch discovery capacity error for config={}: parent_config={parent_config_id}: {err}",
                    target.config_id
                ),
            }
        }
    }
    for (config_id, discovery) in derived {
        discoveries.entry(config_id).or_insert(discovery);
    }
    discoveries
}

fn parent_capacity_discovery(
    discovery: &DiscoveryConfig,
    limits: &CapacityLimits,
) -> Result<DiscoveryConfig, SchedulerError> {
    let mut parent = discovery.clone();
    let limit = discovery
        .max_concurrent_runs_per_config
        .or(discovery.max_concurrent_runs)
        .map_or(limits.per_config, |value| value as usize);
    let limit = u32::try_from(limit).map_err(|_| {
        rusqlite::Error::IntegralValueOutOfRange(0, i64::try_from(limit).unwrap_or(i64::MAX))
    })?;
    parent.max_concurrent_runs = Some(limit);
    parent.max_concurrent_runs_per_config = Some(limit);
    Ok(parent)
}

fn dispatch_parallelism(limits: &CapacityLimits, unit_count: usize) -> usize {
    unit_count.min(limits.global).max(1)
}

fn capacity_limits(targets: &[SchedulerTarget]) -> CapacityLimits {
    CapacityLimits {
        global: targets
            .iter()
            .filter_map(|target| target.discovery.max_concurrent_active_runs)
            .max()
            .unwrap_or(u32::MAX) as usize,
        per_config: targets
            .iter()
            .filter_map(|target| {
                target
                    .discovery
                    .max_concurrent_runs_per_config
                    .or(target.discovery.max_concurrent_runs)
            })
            .max()
            .unwrap_or(1) as usize,
        per_repository: targets
            .iter()
            .filter_map(|target| target.discovery.max_concurrent_runs_per_repository)
            .max()
            .unwrap_or(u32::MAX) as usize,
    }
}

fn has_capacity(
    conn: &Connection,
    cfg: &DiscoveryConfig,
    config_id: &str,
    repo: &str,
    limits: &CapacityLimits,
) -> Result<bool, SchedulerError> {
    let config_limit = cfg
        .max_concurrent_runs_per_config
        .or(cfg.max_concurrent_runs)
        .map_or(limits.per_config, |v| v as usize);
    let repo_limit = cfg
        .max_concurrent_runs_per_repository
        .map_or(limits.per_repository, |v| v as usize);
    Ok(count_active_leases(conn)? < limits.global
        && count_active_leases_for_config(conn, config_id)? < config_limit
        && count_active_leases_for_repository(conn, repo)? < repo_limit)
}

fn poll_due_waits(
    conn: &Connection,
    poller: &dyn ExternalWaitPoller,
) -> Result<RunSummary, PollApplyError> {
    let waits = list_pollable_wait_states(conn, chrono::Utc::now())?;
    let mut summary = RunSummary {
        pollable_waits: waits.len(),
        ..RunSummary::default()
    };
    for wait in waits {
        let decision = poller.poll(&wait);
        match apply_poll_decision(conn, &wait, &decision) {
            Ok(PollApplyOutcome::Committed) => summary.polls_applied += 1,
            Ok(PollApplyOutcome::CommittedWithArtifactWarnings(warnings)) => {
                summary.polls_applied += 1;
                for warning in warnings {
                    summary.record_artifact_warning(ArtifactWarningDetail {
                        run_id: wait.run_id.clone(),
                        phase: warning.phase,
                        error: warning.error,
                    });
                }
            }
            Err(PollApplyError::LeaseTransitionRejected {
                run_id,
                lease_id,
                reason,
            }) => record_lease_transition_skip(
                &mut summary,
                run_id,
                lease_id,
                wait.resume_step.clone(),
                reason,
            ),
            Err(PollApplyError::WaitStateConcurrentTransition(run_id)) => {
                record_wait_state_transition_skip(&mut summary, &wait, run_id);
            }
            Err(PollApplyError::RunStatusConcurrentTransition { run_id, step_id }) => {
                record_run_status_transition_skip(&mut summary, &wait, run_id, step_id);
            }
            Err(PollApplyError::RunMissing { run_id, step_id }) => {
                record_run_missing_skip(&mut summary, &wait, run_id, step_id);
            }
            Err(err @ PollApplyError::Sqlite(_)) => return Err(err),
            Err(err @ PollApplyError::Persistence(_)) => return Err(err),
        }
    }
    Ok(summary)
}

fn record_lease_transition_skip(
    summary: &mut RunSummary,
    run_id: String,
    lease_id: String,
    step_id: String,
    reason: &'static str,
) {
    tracing::warn!(
        run_id = %run_id,
        lease_id = %lease_id,
        step_id = %step_id,
        reason,
        "poll skipped: lease transition rejected"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: Some(lease_id),
        step_id,
        reason: SkippedPollReason::LeaseTransitionRejected,
        lease_transition_reason: Some(reason),
    });
}

fn record_wait_state_transition_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
) {
    tracing::warn!(
        run_id = %run_id,
        step_id = %wait.resume_step,
        "poll skipped: wait-state was concurrently transitioned"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id: wait.resume_step.clone(),
        reason: SkippedPollReason::WaitStateConcurrentTransition,
        lease_transition_reason: None,
    });
}

fn record_run_status_transition_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
    step_id: String,
) {
    tracing::warn!(
        run_id = %run_id,
        step_id = %step_id,
        "poll skipped: run already terminal — stale status update rejected"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id,
        reason: SkippedPollReason::RunStatusConcurrentTransition,
        lease_transition_reason: None,
    });
}

fn record_run_missing_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
    step_id: String,
) {
    tracing::error!(
        run_id = %run_id,
        step_id = %step_id,
        lease_id = ?wait.lease_id,
        integrity_violation = "pollable_wait_missing_run_metadata",
        "poll skipped: orphaned wait-state row"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id,
        reason: SkippedPollReason::RunMissing,
        lease_transition_reason: None,
    });
}

/// Run the scheduler loop until `shutdown` is set.
///
/// Recovers stale leases once at startup (so a crashed previous instance does
/// not permanently block issues), then repeats `run_multi_target_once` and sleeps the
/// configured poll interval, checking the shutdown flag frequently for
/// responsiveness.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-007
pub fn run_loop(
    target: SchedulerTarget,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    shutdown: Arc<AtomicBool>,
    stale_timeout_secs: u64,
) -> Result<(), SchedulerError> {
    let recovered = mark_stale_leases(conn, stale_timeout_secs)?;
    let ready_recovered = mark_stale_ready_to_resume_leases(conn, stale_timeout_secs)?;
    if recovered > 0 || ready_recovered > 0 {
        println!(
            "recovered {recovered} active stale lease(s) and {ready_recovered} ready-to-resume stale lease(s) on startup"
        );
    }

    let poll = target.discovery.poll_interval_secs.unwrap_or(300);
    while !shutdown.load(Ordering::SeqCst) {
        let summary = run_multi_target_once(std::slice::from_ref(&target), &[q], conn, launcher)?;
        if summary.launched > 0
            || summary.resumed > 0
            || summary.suspended > 0
            || summary.failed > 0
            || summary.lease_states_preserved > 0
            || summary.skipped_polls > 0
            || !summary.artifact_warnings.is_empty()
        {
            println!(
                "scheduler pass: {launched} launched, {resumed} resumed, {suspended} suspended, \
                 {failed} failed, {preserved} lease states preserved ({details_dropped} details \
                 dropped), {skipped} skipped, {pollable} pollable waits, {polls_applied} polls \
                 applied, {polls_skipped} polls skipped, {artifact_warnings} artifact warnings",
                launched = summary.launched,
                resumed = summary.resumed,
                suspended = summary.suspended,
                failed = summary.failed,
                preserved = summary.lease_states_preserved,
                details_dropped = summary.lease_state_preserved_details_dropped,
                skipped = summary.skipped,
                pollable = summary.pollable_waits,
                polls_applied = summary.polls_applied,
                polls_skipped = summary.skipped_polls,
                artifact_warnings = summary.artifact_warning_count()
            );
        }
        sleep_with_shutdown(poll, &shutdown);
    }
    Ok(())
}

/// Sleep up to `secs` seconds, waking early if shutdown is requested.
fn sleep_with_shutdown(secs: u64, shutdown: &Arc<AtomicBool>) {
    let ticks = secs.saturating_mul(5); // 200ms granularity
    for _ in 0..ticks {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests;
