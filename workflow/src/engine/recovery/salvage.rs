//! Legacy salvage lineage. [C9/B10]
//!
//! Every run WITHOUT a valid pre-execution V1 capsule is salvage-only,
//! regardless of provenance or migration source. Immutable idempotent
//! lineage. Exact recovery refuses. [C9]
//!
//! **[B10]** Historical capsule backfill is PROHIBITED. A capsule may only be
//! written by the fresh-launch path BEFORE any step executes. A run that
//! already executed without one is salvage-only forever — it can never be
//! retroactively given a capsule.
//!
//! A salvage-only run produces an immutable salvage lineage record (audit-only,
//! append-only) and REFUSES exact recovery. [C9/B10]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
//! @requirement:REQ-RP-007

use chrono::Utc;
use rusqlite::{params, Connection, Result as SqliteResult};
use thiserror::Error;

use super::capsule::{verify_envelope_digest, ExecutionCapsuleV1};
use super::protocol::{RecoveryOutcome, RefusalReason};
use crate::persistence::capsule_store::load_capsule_v1;

/// Table name for the immutable salvage lineage records. [C9]
pub const SALVAGE_LINEAGE_TABLE: &str = "salvage_lineage";

/// Initialize the immutable salvage lineage table (idempotent). [C9]
///
/// DDL (salvage pseudocode lines 45–50):
/// ```text
/// CREATE TABLE IF NOT EXISTS salvage_lineage (
///   salvage_id INTEGER PRIMARY KEY AUTOINCREMENT,
///   run_id TEXT NOT NULL,
///   recorded_at TEXT NOT NULL,
///   detail TEXT
/// )
/// ```
///
/// The table is append-only by design: `AUTOINCREMENT` PK guarantees strictly
/// increasing salvage ids and no row reuse. Every salvage attempt records a
/// new immutable lineage row. [C9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub fn init_salvage_lineage_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {SALVAGE_LINEAGE_TABLE} (
                salvage_id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                recorded_at TEXT NOT NULL,
                detail TEXT
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Classification of a run for recovery purposes. [C9/B10]
///
/// A run is either capsule-backed (exact recovery possible) or salvage-only
/// (audit-only, exact recovery refused). [C9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
#[derive(Debug, Clone)]
pub enum RunClassification {
    /// A valid pre-execution V1 capsule exists and its envelope verifies.
    /// Exact recovery is possible via the recovery protocol. [C9]
    CapsuleBacked {
        /// The valid immutable capsule. [C3/C8]
        capsule: Box<ExecutionCapsuleV1>,
    },
    /// No valid pre-execution V1 capsule: salvage-only. [C9/B10]
    ///
    /// This applies regardless of whether the run has a `LaunchProvenance`
    /// (including migrated provenance with sentinel digests) or none. [B10]
    /// Backfill is prohibited; the run remains salvage-only forever.
    SalvageOnly {
        /// The run id that is salvage-only.
        run_id: String,
    },
}

impl RunClassification {
    /// The run id of the classified run.
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::CapsuleBacked { capsule } => &capsule.run_id,
            Self::SalvageOnly { run_id } => run_id,
        }
    }

    /// Whether this classification is salvage-only.
    #[must_use]
    pub fn is_salvage_only(&self) -> bool {
        matches!(self, Self::SalvageOnly { .. })
    }
}

/// Errors produced by the salvage subsystem. [C9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
#[derive(Debug, Error)]
pub enum SalvageError {
    /// Underlying persistence store failure. [C9]
    #[error("salvage persistence error: {0}")]
    Persistence(String),
    /// A capsule-backed run was routed to salvage recovery instead of the
    /// protocol. [C9]
    #[error("unexpected capsule-backed run routed to salvage")]
    UnexpectedCapsuleBackedRun,
}

impl From<rusqlite::Error> for SalvageError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Persistence(err.to_string())
    }
}

/// Classify a run: capsule-backed (exact recovery possible) or salvage-only.
/// [C9/B10]
///
/// Follows salvage pseudocode lines 07–24: load the V1 capsule, verify the
/// envelope digest, and classify. A run with no capsule or a capsule whose
/// envelope fails verification is salvage-only. [C9/B10]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub fn classify_run(conn: &Connection, run_id: &str) -> Result<RunClassification, SalvageError> {
    match load_capsule_v1(conn, run_id) {
        Ok(capsule) => match verify_envelope_digest(&capsule) {
            Ok(()) => Ok(RunClassification::CapsuleBacked {
                capsule: Box::new(capsule),
            }),
            Err(_) => Ok(RunClassification::SalvageOnly {
                run_id: run_id.to_string(),
            }),
        },
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(RunClassification::SalvageOnly {
            run_id: run_id.to_string(),
        }),
        Err(error) => Err(SalvageError::from(error)),
    }
}

/// A salvage-only run produces an immutable salvage lineage record
/// (audit-only) and REFUSES exact recovery. [C9/B10]
///
/// Follows salvage pseudocode lines 28–42: classify the run, append a salvage
/// record if salvage-only, and return [`RecoveryOutcome::Refused`] with
/// [`RefusalReason::SalvageOnly`]. A capsule-backed run routed here is an
/// error (it should have gone to the protocol). [C9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub fn salvage_recover(conn: &Connection, run_id: &str) -> Result<RecoveryOutcome, SalvageError> {
    let classification = classify_run(conn, run_id)?;
    match classification {
        RunClassification::CapsuleBacked { .. } => Err(SalvageError::UnexpectedCapsuleBackedRun),
        RunClassification::SalvageOnly { run_id } => {
            append_salvage_record(conn, &run_id)?;
            Ok(RecoveryOutcome::Refused {
                reason: RefusalReason::SalvageOnly,
            })
        }
    }
}

/// Append an immutable salvage lineage record (never updates existing). [C9]
///
/// Follows salvage pseudocode lines 51–55: a plain `INSERT ... RETURNING` into
/// the append-only `salvage_lineage` table. Every salvage attempt records a
/// new immutable row; the table is never updated. [C9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub fn append_salvage_record(conn: &Connection, run_id: &str) -> Result<i64, SalvageError> {
    init_salvage_lineage_table(conn)?;
    let recorded_at = Utc::now().to_rfc3339();
    let detail = "salvage-only run: no valid pre-execution V1 capsule";
    let salvage_id = conn.query_row(
        &format!(
            "INSERT INTO {SALVAGE_LINEAGE_TABLE} (run_id, recorded_at, detail)
             VALUES (?1, ?2, ?3)
             RETURNING salvage_id"
        ),
        params![run_id, recorded_at, detail],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(salvage_id)
}

/// Count salvage lineage records for a run. [C9]
///
/// Useful for tests asserting that salvage records are appended (and are
/// immutable/idempotent in the sense that each attempt appends a new row).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub fn count_salvage_records(conn: &Connection, run_id: &str) -> Result<i64, SalvageError> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {SALVAGE_LINEAGE_TABLE} WHERE run_id = ?1"),
        params![run_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::capsule_store::init_capsules_table;
    use crate::persistence::sqlite::init_runs_schema;

    fn salvage_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        init_salvage_lineage_table(&conn).expect("init salvage lineage");
        init_capsules_table(&conn).expect("init capsules");
        init_runs_schema(&conn).expect("init runs schema");
        conn
    }

    #[test]
    fn classify_run_without_capsule_is_salvage_only() {
        let conn = salvage_conn();
        let classification = classify_run(&conn, "run-no-capsule").expect("classify");
        assert!(classification.is_salvage_only());
        assert_eq!(classification.run_id(), "run-no-capsule");
    }

    #[test]
    fn salvage_recover_appends_immutable_record_and_refuses() {
        let conn = salvage_conn();
        let run_id = "run-salvage";
        let outcome = salvage_recover(&conn, run_id).expect("salvage recover");
        match outcome {
            RecoveryOutcome::Refused {
                reason: RefusalReason::SalvageOnly,
            } => {}
            other => panic!("expected Refused SalvageOnly, got {other:?}"),
        }
        assert_eq!(count_salvage_records(&conn, run_id).expect("count"), 1);
        let outcome2 = salvage_recover(&conn, run_id).expect("salvage recover 2");
        assert!(matches!(
            outcome2,
            RecoveryOutcome::Refused {
                reason: RefusalReason::SalvageOnly
            }
        ));
        assert_eq!(count_salvage_records(&conn, run_id).expect("count 2"), 2);
    }

    #[test]
    fn salvage_recover_capsule_backed_run_errors() {
        let conn = salvage_conn();
        let run_id = "run-with-capsule";
        let capsule = build_valid_capsule(run_id);
        crate::persistence::capsule_store::persist_capsule_v1(&conn, &capsule)
            .expect("persist capsule");
        let result = salvage_recover(&conn, run_id);
        assert!(matches!(
            result,
            Err(SalvageError::UnexpectedCapsuleBackedRun)
        ));
        assert_eq!(count_salvage_records(&conn, run_id).expect("count"), 0);
    }

    #[test]
    fn classify_run_with_valid_capsule_is_backed() {
        let conn = salvage_conn();
        let run_id = "run-backed";
        let capsule = build_valid_capsule(run_id);
        crate::persistence::capsule_store::persist_capsule_v1(&conn, &capsule)
            .expect("persist capsule");
        let classification = classify_run(&conn, run_id).expect("classify");
        assert!(!classification.is_salvage_only());
    }

    /// Build a valid V1 capsule with a minimal workflow type/config so the
    /// envelope digest verifies in salvage tests.
    fn build_valid_capsule(run_id: &str) -> ExecutionCapsuleV1 {
        use crate::workflow::schema::{
            GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig, RuntimeConfig,
            StepDef, TransitionDef, WorkflowConfig, WorkflowType,
        };
        use std::collections::HashMap;
        let workflow_type = WorkflowType {
            workflow_type_id: "salvage-test".to_string(),
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
        };
        let config = WorkflowConfig {
            config_id: "salvage-test-config".to_string(),
            workflow_type_id: "salvage-test".to_string(),
            runtime: RuntimeConfig {
                timeout_seconds: 60,
                max_retries: 1,
                parallel_steps: None,
                log_level: None,
            },
            repo: RepoConfig {
                workspace_strategy: "temp_clone".to_string(),
                branch_template: "wf-{run_id}".to_string(),
                base_branch: Some("main".to_string()),
                workspace_root: None,
                project_subdir: None,
                artifact_path_base: None,
                diff_path_base: None,
                diff_path_normalization:
                    crate::workflow::schema::DiffPathNormalization::RepoRelative,
            },
            guard_limits: GuardLimits {
                max_iterations: None,
                max_file_changes: None,
                max_tokens: None,
                max_cost: None,
            },
            variables: HashMap::new(),
            discovery: None,
            parent_orchestration: ParentOrchestrationConfig::default(),
            merge_required: false,
            merge_strategy: None,
            command_manifest: None,
            target_profile: None,
        };
        let provenance = crate::persistence::launch_provenance::LaunchProvenance::from_resolved(
            &workflow_type,
            &config,
            std::path::Path::new("."),
        )
        .expect("canonicalize '.'");
        crate::engine::recovery::capsule::build_capsule_v1(
            run_id.to_string(),
            &workflow_type,
            &config,
            std::path::Path::new("."),
            &provenance,
            "main".to_string(),
        )
        .expect("build capsule")
    }
}
