//! Typed merge completion orchestrator: transitions a `ReviewReady`
//! merge-required run to `Merged` via `complete_typed_merge`.
//!
//! This module makes the typed merge production path **reachable**: after a
//! merge-required run's steps complete and the runner writes `ReviewReady`,
//! the daemon scheduler (or recovery path) calls this orchestrator to
//! atomically reach `Merged` with system probes.
//!
//! The orchestrator constructs the `MergeVerifier` with system probes bound to
//! the config-declared `merge_strategy`, derives the exact base commit from
//! the capsule's `base_ref`, and invokes `complete_merge_from_observation`.
//!
//! On failure, the run remains in `ReviewReady` (not `Completed` and not
//! `Merged`), providing a durable diagnostic/retry route. The orchestrator
//! never writes `Completed` and never fabricates success.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
//! @requirement:REQ-RP-010

use std::path::Path;

use rusqlite::Connection;

use crate::engine::recovery::capsule::ExecutionCapsuleV1;
use crate::engine::recovery::typed_merge::{
    complete_merge_from_observation, MergeError, MergeGitProbe, MergeRemoteProbe, MergeStrategy,
    SystemMergeGitProbe, SystemMergeRemoteProbe,
};
use crate::persistence::capsule_store;
use crate::persistence::run_metadata::RunStatus;
use crate::persistence::sqlite;
use crate::workflow::schema::MergeStrategyConfig;

/// Factory for constructing system probes. [P17]
///
/// Production constructs system probes (`SystemMergeGitProbe`,
/// `SystemMergeRemoteProbe` with config-declared strategy). Tests inject a
/// factory that returns deterministic stub probes. This is the injected
/// executor/probe seam that keeps tests no-network.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub trait MergeProbeFactory: Send + Sync {
    /// Create the Git probe.
    fn git_probe(&self) -> Box<dyn MergeGitProbe>;

    /// Create the remote probe bound to the config-declared expected strategy.
    fn remote_probe(&self, expected_strategy: MergeStrategy) -> Box<dyn MergeRemoteProbe>;
}

/// Production probe factory that constructs system probes. [P17]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, Default)]
pub struct SystemMergeProbeFactory;

impl SystemMergeProbeFactory {
    /// Create a new system probe factory.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl MergeProbeFactory for SystemMergeProbeFactory {
    fn git_probe(&self) -> Box<dyn MergeGitProbe> {
        Box::new(SystemMergeGitProbe::new())
    }

    fn remote_probe(&self, expected_strategy: MergeStrategy) -> Box<dyn MergeRemoteProbe> {
        Box::new(SystemMergeRemoteProbe::new(expected_strategy))
    }
}

/// Outcome of a merge completion attempt.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeCompletionOutcome {
    /// The run was transitioned to `Merged` with a typed artifact.
    Merged,
    /// The run is `ReviewReady` but the PR is not yet merged. The run stays
    /// `ReviewReady` and can be retried later.
    NotYetMerged,
    /// The run is not in `ReviewReady` (e.g. already `Merged`, or in another
    /// state). No action taken.
    NotReviewReady(RunStatus),
    /// A merge-required run has no declared `merge_strategy` in config. Fail
    /// closed — never guess.
    StrategyNotDeclared,
    /// An error occurred during completion. The run remains `ReviewReady`
    /// (durable diagnostic/retry route, not fake success).
    Failed(MergeError),
}

/// Check whether a run is a merge-required run in `ReviewReady` that needs
/// typed merge completion.
///
/// Returns `true` only when:
/// - The run exists
/// - The run's status is `ReviewReady`
/// - A capsule exists for the run (merge-required runs always have capsules)
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn needs_merge_completion(conn: &Connection, run_id: &str) -> bool {
    let Ok(Some(md)) = sqlite::get_run_with_conn(conn, run_id) else {
        return false;
    };
    if md.status != RunStatus::ReviewReady {
        return false;
    }
    capsule_store::load_capsule_v1(conn, run_id).is_ok()
}

/// Resolve the config-declared merge strategy from the capsule's resolved
/// config bytes. [P17]
///
/// Fail closed: if the strategy cannot be resolved (missing field, parse
/// error), return `None`. The orchestrator treats `None` as a hard failure
/// (`StrategyNotDeclared`) — it never guesses.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn resolve_declared_strategy(
    capsule: &ExecutionCapsuleV1,
) -> Result<Option<MergeStrategyConfig>, serde_json::Error> {
    let config: crate::workflow::schema::WorkflowConfig =
        serde_json::from_slice(&capsule.resolved_config_bytes)?;
    Ok(config.merge_strategy)
}

/// Attempt to complete a merge-required run that is in `ReviewReady`.
///
/// This is the **production completion path** that makes the typed merge API
/// reachable. It:
///
/// 1. Loads the run metadata and verifies status is `ReviewReady`.
/// 2. Loads the capsule and resolves the config-declared `merge_strategy`.
/// 3. Constructs system probes via the injected [`MergeProbeFactory`].
/// 4. Calls [`complete_merge_from_observation`], which derives the exact base
///    commit from `capsule.base_ref`, builds the reachability proof via the
///    probes, and commits the artifact + `ReviewReady → Merged` transition
///    atomically.
///
/// On success → [`MergeCompletionOutcome::Merged`].
/// On `NotMerged` → [`MergeCompletionOutcome::NotYetMerged`] (run stays
/// `ReviewReady`, durable retry route).
/// On any other error → [`MergeCompletionOutcome::Failed`] (run stays
/// `ReviewReady`, durable diagnostic/retry route).
///
/// The orchestrator **never** writes `Completed` and **never** fabricates
/// success.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub fn complete_merge_required_run(
    conn: &Connection,
    run_id: &str,
    work_dir: &Path,
    probe_factory: &dyn MergeProbeFactory,
) -> MergeCompletionOutcome {
    // 1. Load run metadata and verify status.
    let md = match sqlite::get_run_with_conn(conn, run_id) {
        Ok(Some(md)) => md,
        Ok(None) => return MergeCompletionOutcome::NotReviewReady(RunStatus::Initialized),
        Err(e) => return MergeCompletionOutcome::Failed(MergeError::Database(e.to_string())),
    };
    if md.status == RunStatus::Merged {
        return MergeCompletionOutcome::NotReviewReady(RunStatus::Merged);
    }
    if md.status != RunStatus::ReviewReady {
        return MergeCompletionOutcome::NotReviewReady(md.status);
    }

    // 2. Load capsule and resolve config-declared strategy.
    let capsule = match capsule_store::load_capsule_v1(conn, run_id) {
        Ok(c) => c,
        Err(e) => {
            return MergeCompletionOutcome::Failed(MergeError::Database(format!(
                "capsule load failed: {e}"
            )))
        }
    };
    let declared = match resolve_declared_strategy(&capsule) {
        Ok(s) => s,
        Err(e) => {
            return MergeCompletionOutcome::Failed(MergeError::Database(format!(
                "config parse failed: {e}"
            )))
        }
    };
    let Some(strategy_config) = declared else {
        return MergeCompletionOutcome::StrategyNotDeclared;
    };
    let expected_strategy = strategy_config.to_merge_strategy();

    // 3. Construct probes via the injected factory.
    let git_probe = probe_factory.git_probe();
    let remote_probe = probe_factory.remote_probe(expected_strategy);

    // 4. Complete the merge from observation.
    match complete_merge_from_observation(conn, run_id, work_dir, git_probe, remote_probe) {
        Ok(()) => MergeCompletionOutcome::Merged,
        Err(MergeError::NotMerged) => MergeCompletionOutcome::NotYetMerged,
        Err(e) => MergeCompletionOutcome::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::recovery::capsule::build_capsule_v1;
    use crate::engine::recovery::typed_merge::{self, MergeObservation, MergeStrategy};
    use crate::persistence::capsule_store::{init_capsules_table, persist_capsule_v1};
    use crate::persistence::recovery_operations::init_operations_table;
    use crate::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
    use crate::persistence::{RunMetadata, RunStatus};
    use crate::workflow::schema::{
        DiffPathNormalization, GuardConfig, GuardLimits, MergeStrategyConfig,
        ParentOrchestrationConfig, RepoConfig, RuntimeConfig, StepDef, TransitionDef,
        WorkflowConfig, WorkflowType,
    };
    use std::path::Path;

    // ---- Test helpers (reused patterns from typed_merge_integration_tests) ----

    fn merge_completion_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        crate::engine::recovery::typed_merge::init_merge_artifacts_table(&conn)
            .expect("init merge artifacts");
        init_capsules_table(&conn).expect("init capsules");
        init_runs_schema(&conn).expect("init runs schema");
        init_operations_table(&conn).expect("init operations");
        conn
    }

    fn sample_workflow_type() -> WorkflowType {
        WorkflowType {
            workflow_type_id: "merge-completion-test".to_string(),
            steps: vec![StepDef {
                step_id: "step1".to_string(),
                step_type: "noop".to_string(),
                description: None,
                parameters: None,
                produces: None,
                consumes: None,
                terminal: None,
                recovery_policy: None,
            }],
            transitions: vec![TransitionDef {
                from: "step1".to_string(),
                to: "step2".to_string(),
                condition: None,
                max_iterations: None,
            }],
            guards: GuardConfig {
                max_retries: None,
                timeout_seconds: None,
                require_approval: None,
            },
        }
    }

    fn sample_config_with_strategy(strategy: Option<MergeStrategyConfig>) -> WorkflowConfig {
        WorkflowConfig {
            config_id: "merge-completion-test-config".to_string(),
            workflow_type_id: "merge-completion-test".to_string(),
            runtime: RuntimeConfig {
                timeout_seconds: 3600,
                max_retries: 3,
                parallel_steps: None,
                log_level: None,
            },
            repo: RepoConfig {
                workspace_strategy: "temp".to_string(),
                branch_template: "test-{run_id}".to_string(),
                base_branch: Some("main".to_string()),
                workspace_root: None,
                project_subdir: None,
                artifact_path_base: None,
                diff_path_base: None,
                diff_path_normalization: DiffPathNormalization::RepoRelative,
            },
            guard_limits: GuardLimits {
                max_iterations: Some(3),
                max_file_changes: Some(50),
                max_tokens: Some(10000),
                max_cost: Some(10.0),
            },
            variables: std::collections::HashMap::new(),
            discovery: None,
            parent_orchestration: ParentOrchestrationConfig::default(),
            merge_required: true,
            merge_strategy: strategy,
            command_manifest: None,
            target_profile: None,
        }
    }

    fn persisted_capsule_with_strategy(
        conn: &Connection,
        run_id: &str,
        strategy: Option<MergeStrategyConfig>,
    ) -> ExecutionCapsuleV1 {
        let workflow = sample_workflow_type();
        let config = sample_config_with_strategy(strategy);
        let provenance = crate::persistence::launch_provenance::LaunchProvenance::from_resolved(
            &workflow,
            &config,
            Path::new("."),
        )
        .expect("canonicalize config_root");
        let capsule = build_capsule_v1(
            run_id.to_string(),
            &workflow,
            &config,
            Path::new("."),
            &provenance,
            "main".to_string(),
        )
        .expect("build capsule");
        persist_capsule_v1(conn, &capsule).expect("persist capsule");
        capsule
    }

    fn seed_run(
        conn: &Connection,
        run_id: &str,
        status: RunStatus,
        repo: &str,
        pr_number: i64,
        head_sha: &str,
    ) {
        let mut md = RunMetadata::new(
            run_id,
            "merge-completion-test",
            "merge-completion-test-config",
        );
        md.status = status;
        md.repository = Some(repo.to_string());
        md.pr_number = Some(pr_number);
        md.head_sha = Some(head_sha.to_string());
        persist_run_with_conn(conn, &md).expect("persist run");
    }

    // ---- Stub probe factory ----

    struct StubProbeFactory {
        git: StubMergeGitProbe,
        remote_observation: MergeObservation,
    }

    impl MergeProbeFactory for StubProbeFactory {
        fn git_probe(&self) -> Box<dyn MergeGitProbe> {
            Box::new(self.git.clone())
        }

        fn remote_probe(&self, expected_strategy: MergeStrategy) -> Box<dyn MergeRemoteProbe> {
            assert_eq!(self.remote_observation.strategy, expected_strategy);
            Box::new(StubMergeRemoteProbe {
                observation: self.remote_observation.clone(),
            })
        }
    }

    #[derive(Clone)]
    struct StubMergeGitProbe {
        ancestors: Vec<(String, String)>,
        base_commits: std::collections::HashMap<String, String>,
    }

    impl StubMergeGitProbe {
        fn new() -> Self {
            Self {
                ancestors: Vec::new(),
                base_commits: std::collections::HashMap::new(),
            }
        }

        fn with_ancestor(mut self, a: &str, d: &str) -> Self {
            self.ancestors.push((a.to_string(), d.to_string()));
            self
        }

        fn with_base_commit(mut self, ref_name: &str, sha: &str) -> Self {
            self.base_commits
                .insert(ref_name.to_string(), sha.to_string());
            self
        }
    }

    impl MergeGitProbe for StubMergeGitProbe {
        fn is_ancestor(
            &self,
            _work_dir: &Path,
            ancestor: &str,
            descendant: &str,
        ) -> Result<(), typed_merge::MergeError> {
            if self
                .ancestors
                .iter()
                .any(|(a, d)| a == ancestor && d == descendant)
            {
                Ok(())
            } else {
                Err(typed_merge::MergeError::ReachabilityFailed(format!(
                    "{ancestor} NOT ancestor of {descendant}"
                )))
            }
        }

        fn compute_tree_content_digest(
            &self,
            _work_dir: &Path,
            _commit: &str,
        ) -> Result<String, typed_merge::MergeError> {
            Ok("digest".to_string())
        }

        fn compute_patch_id(
            &self,
            _work_dir: &Path,
            _base: &str,
            _head: &str,
        ) -> Result<String, typed_merge::MergeError> {
            Ok("patch_id".to_string())
        }

        fn resolve_base_commit(
            &self,
            _work_dir: &Path,
            base_ref: &str,
        ) -> Result<String, typed_merge::MergeError> {
            self.base_commits.get(base_ref).cloned().ok_or_else(|| {
                typed_merge::MergeError::ReachabilityFailed(format!(
                    "no base commit for '{base_ref}'"
                ))
            })
        }
    }

    struct StubMergeRemoteProbe {
        observation: MergeObservation,
    }

    impl MergeRemoteProbe for StubMergeRemoteProbe {
        fn observe_merge(
            &self,
            _repo: &str,
            _pr_number: i64,
        ) -> Result<MergeObservation, typed_merge::MergeError> {
            // The stub trusts the observation's strategy field for testing;
            // the real SystemMergeRemoteProbe cross-checks against
            // expected_strategy.
            Ok(self.observation.clone())
        }
    }

    // ---- Tests ----

    /// GIVEN: a ReviewReady run with a capsule declaring merge_strategy=MergeCommit
    ///        and a stub probe that reports merged=true
    /// WHEN: complete_merge_required_run is called
    /// THEN: it reaches Merged with a typed artifact.
    #[test]
    fn complete_merge_required_run_reaches_merged() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-merged";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::ReviewReady, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            git: StubMergeGitProbe::new()
                .with_ancestor("head123", "merge789")
                .with_ancestor("base456", "merge789")
                .with_base_commit("main", "base456"),
            remote_observation: MergeObservation {
                merged: true,
                strategy: MergeStrategy::MergeCommit,
                result_sha: "merge789".to_string(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert_eq!(outcome, MergeCompletionOutcome::Merged);
        assert!(typed_merge::completion_satisfied(&conn, run_id));
    }

    /// GIVEN: a ReviewReady run where the PR is NOT yet merged
    /// WHEN: complete_merge_required_run is called
    /// THEN: it returns NotYetMerged and the run stays ReviewReady.
    #[test]
    fn complete_merge_required_run_not_yet_merged() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-not-merged";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::ReviewReady, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            git: StubMergeGitProbe::new().with_base_commit("main", "base456"),
            remote_observation: MergeObservation {
                merged: false,
                strategy: MergeStrategy::MergeCommit,
                result_sha: String::new(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert_eq!(outcome, MergeCompletionOutcome::NotYetMerged);
        // Run stays ReviewReady, not Completed.
        let md = sqlite::get_run_with_conn(&conn, run_id).unwrap().unwrap();
        assert_eq!(md.status, RunStatus::ReviewReady);
    }

    /// GIVEN: a ReviewReady run with NO declared merge_strategy
    /// WHEN: complete_merge_required_run is called
    /// THEN: it returns StrategyNotDeclared (fail closed, never guess).
    #[test]
    fn complete_merge_required_run_no_strategy_fails_closed() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-no-strategy";
        let _capsule = persisted_capsule_with_strategy(&conn, run_id, None);
        seed_run(&conn, run_id, RunStatus::ReviewReady, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            git: StubMergeGitProbe::new(),
            remote_observation: MergeObservation {
                merged: true,
                strategy: MergeStrategy::MergeCommit,
                result_sha: "merge789".to_string(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert_eq!(outcome, MergeCompletionOutcome::StrategyNotDeclared);
        // Run stays ReviewReady.
        let md = sqlite::get_run_with_conn(&conn, run_id).unwrap().unwrap();
        assert_eq!(md.status, RunStatus::ReviewReady);
    }

    /// GIVEN: a run that is NOT ReviewReady (e.g. Completed)
    /// WHEN: complete_merge_required_run is called
    /// THEN: it returns NotReviewReady and takes no action.
    #[test]
    fn complete_merge_required_run_wrong_status_no_action() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-wrong-status";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::Completed, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            git: StubMergeGitProbe::new(),
            remote_observation: MergeObservation {
                merged: true,
                strategy: MergeStrategy::MergeCommit,
                result_sha: "merge789".to_string(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert_eq!(
            outcome,
            MergeCompletionOutcome::NotReviewReady(RunStatus::Completed)
        );
    }

    /// GIVEN: a run that is already Merged
    /// WHEN: complete_merge_required_run is called
    /// THEN: it returns NotReviewReady(Merged) (idempotent, no action).
    #[test]
    fn complete_merge_required_run_already_merged() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-already-merged";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::Merged, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            git: StubMergeGitProbe::new(),
            remote_observation: MergeObservation {
                merged: true,
                strategy: MergeStrategy::MergeCommit,
                result_sha: "merge789".to_string(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert_eq!(
            outcome,
            MergeCompletionOutcome::NotReviewReady(RunStatus::Merged)
        );
    }

    /// GIVEN: a ReviewReady run with a valid capsule
    /// WHEN: needs_merge_completion is called
    /// THEN: it returns true.
    #[test]
    fn needs_merge_completion_true_for_review_ready() {
        let conn = merge_completion_conn();
        let run_id = "run-needs-completion";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::ReviewReady, "o/r", 42, "head123");
        assert!(needs_merge_completion(&conn, run_id));
    }

    /// GIVEN: a run that is Completed (not ReviewReady)
    /// WHEN: needs_merge_completion is called
    /// THEN: it returns false.
    #[test]
    fn needs_merge_completion_false_for_completed() {
        let conn = merge_completion_conn();
        let run_id = "run-no-completion";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::Completed, "o/r", 42, "head123");
        assert!(!needs_merge_completion(&conn, run_id));
    }

    /// GIVEN: a ReviewReady run where verification fails (ancestry mismatch)
    /// WHEN: complete_merge_required_run is called
    /// THEN: it returns Failed and the run stays ReviewReady (durable retry).
    #[test]
    fn complete_merge_required_run_failure_leaves_review_ready() {
        let conn = merge_completion_conn();
        let run_id = "run-orch-failure";
        let _capsule =
            persisted_capsule_with_strategy(&conn, run_id, Some(MergeStrategyConfig::MergeCommit));
        seed_run(&conn, run_id, RunStatus::ReviewReady, "o/r", 42, "head123");

        let factory = StubProbeFactory {
            // Missing ancestry config → will fail.
            git: StubMergeGitProbe::new().with_base_commit("main", "base456"),
            remote_observation: MergeObservation {
                merged: true,
                strategy: MergeStrategy::MergeCommit,
                result_sha: "merge789".to_string(),
            },
        };

        let outcome = complete_merge_required_run(&conn, run_id, Path::new("."), &factory);
        assert!(
            matches!(outcome, MergeCompletionOutcome::Failed(_)),
            "got {outcome:?}"
        );
        // Run stays ReviewReady (durable diagnostic/retry route).
        let md = sqlite::get_run_with_conn(&conn, run_id).unwrap().unwrap();
        assert_eq!(md.status, RunStatus::ReviewReady);
    }
}
