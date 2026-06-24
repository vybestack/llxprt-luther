//! Normalized smoke-trace capture/replay model for deterministic engine-routing replay.
//!
//! A `SmokeTrace` records the ordered `(step_id, outcome)` sequence the engine
//! emitted during a run plus the terminal `RunOutcome`. Replaying these recorded
//! per-step outcomes through the real engine re-derives identical routing without
//! any network/`gh` dependency, so live smoke failures become deterministically
//! reproducible offline.
//!
//! @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
//! @requirement:REQ-SMOKE-REPLAY-001,REQ-SMOKE-REPLAY-002,REQ-SMOKE-REPLAY-003

use crate::engine::runner::RunOutcome;
use crate::persistence::checkpoint::{load_events, PersistenceError};
use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current trace schema version. `load_trace` rejects unknown major versions.
pub const SCHEMA_VERSION: u32 = 1;

/// A single recorded engine step: the step that executed and the outcome it
/// produced, in recorded (timestamp) order.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    /// Zero-based sequence index assigned by load order.
    pub seq: u32,
    /// The step id that executed.
    pub step_id: String,
    /// The step outcome string (`success|retryable|fatal|fixable|abandon`).
    pub outcome: String,
}

/// Serde-friendly mirror of [`RunOutcome`] used as the trace's terminal state.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceOutcome {
    /// All steps completed successfully.
    Success,
    /// Run terminated due to fatal error.
    Failure {
        /// Step where the failure occurred.
        step_id: String,
        /// Human-readable failure reason.
        reason: String,
    },
    /// Run was abandoned due to loop limits.
    Abandoned {
        /// Step where the run was abandoned.
        step_id: String,
        /// Human-readable abandon reason.
        reason: String,
    },
    /// Run was interrupted and can be resumed.
    Interrupted {
        /// Step where the run was interrupted.
        step_id: String,
    },
    /// Run paused on a recoverable external wait condition.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    WaitingExternal {
        /// Step where the run paused awaiting external state.
        step_id: String,
        /// Human-readable wait reason.
        reason: String,
    },
}

impl From<&RunOutcome> for TraceOutcome {
    fn from(outcome: &RunOutcome) -> Self {
        match outcome {
            RunOutcome::Success => TraceOutcome::Success,
            RunOutcome::Failure { step_id, reason } => TraceOutcome::Failure {
                step_id: step_id.clone(),
                reason: reason.clone(),
            },
            RunOutcome::Abandoned { step_id, reason } => TraceOutcome::Abandoned {
                step_id: step_id.clone(),
                reason: reason.clone(),
            },
            RunOutcome::Interrupted { step_id } => TraceOutcome::Interrupted {
                step_id: step_id.clone(),
            },
            RunOutcome::WaitingExternal { step_id, reason } => TraceOutcome::WaitingExternal {
                step_id: step_id.clone(),
                reason: reason.clone(),
            },
        }
    }
}

impl TraceOutcome {
    /// Returns true if this recorded outcome matches a replay `RunOutcome`'s
    /// terminal variant. Only the variant kind and `step_id` are compared;
    /// free-form `reason` text is ignored because it can carry non-deterministic
    /// detail (e.g. live error messages).
    pub fn matches_run_outcome(&self, outcome: &RunOutcome) -> bool {
        match (self, outcome) {
            (TraceOutcome::Success, RunOutcome::Success) => true,
            (TraceOutcome::Failure { step_id, .. }, RunOutcome::Failure { step_id: s, .. }) => {
                step_id == s
            }
            (TraceOutcome::Abandoned { step_id, .. }, RunOutcome::Abandoned { step_id: s, .. }) => {
                step_id == s
            }
            (TraceOutcome::Interrupted { step_id }, RunOutcome::Interrupted { step_id: s }) => {
                step_id == s
            }
            (
                TraceOutcome::WaitingExternal { step_id, .. },
                RunOutcome::WaitingExternal { step_id: s, .. },
            ) => step_id == s,
            _ => false,
        }
    }
}

/// A normalized, committed record of an engine run's step sequence + outcomes,
/// sufficient to replay engine routing deterministically offline.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SmokeTrace {
    /// Schema version of this trace document.
    pub schema_version: u32,
    /// The engine run id this trace was captured from.
    pub run_id: String,
    /// The workflow type id (e.g. `llxprt-issue-fix-v1`) that was executed.
    pub workflow_type_id: String,
    /// The workflow config id that parameterized the run.
    pub config_id: String,
    /// When the trace was captured (RFC3339, UTC). Stored as a string to match
    /// the persistence layer's timestamp encoding and avoid a chrono serde dep.
    pub captured_at: String,
    /// The terminal run outcome.
    pub final_outcome: TraceOutcome,
    /// The ordered per-step events that drove routing.
    pub events: Vec<TraceEvent>,
}

/// Build a [`SmokeTrace`] from the recorded `events` table for `run_id`.
///
/// Events are read via [`load_events`] (ordered by timestamp ASC) and mapped to
/// [`TraceEvent`]s with `seq` assigned by load order.
///
/// @requirement:REQ-SMOKE-REPLAY-001
pub fn export_trace(
    conn: &Connection,
    run_id: &str,
    workflow_type_id: &str,
    config_id: &str,
    final_outcome: &RunOutcome,
) -> Result<SmokeTrace, PersistenceError> {
    let records = load_events(conn, run_id)?;
    let events = records
        .into_iter()
        .enumerate()
        .map(|(idx, record)| TraceEvent {
            seq: idx as u32,
            step_id: record.step_id,
            outcome: record.outcome,
        })
        .collect();
    Ok(SmokeTrace {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        workflow_type_id: workflow_type_id.to_string(),
        config_id: config_id.to_string(),
        captured_at: Utc::now().to_rfc3339(),
        final_outcome: TraceOutcome::from(final_outcome),
        events,
    })
}

/// Atomically write `trace` as pretty JSON to `path` (temp file + rename),
/// mirroring `pr_followup_artifacts::atomic_write`.
///
/// @requirement:REQ-SMOKE-REPLAY-001
pub fn save_trace(trace: &SmokeTrace, path: &Path) -> Result<(), PersistenceError> {
    let parent = path.parent().ok_or_else(|| {
        PersistenceError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("missing parent for {}", path.display()),
        ))
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(trace)
        .map_err(|e| PersistenceError::Serialization(e.to_string()))?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("trace"),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&temp_path, &bytes)?;
    std::fs::rename(&temp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp_path);
    })?;
    Ok(())
}

/// Load and validate a [`SmokeTrace`] from `path`. Rejects unknown/future
/// schema versions.
///
/// @requirement:REQ-SMOKE-REPLAY-002
pub fn load_trace(path: &Path) -> Result<SmokeTrace, PersistenceError> {
    let bytes = std::fs::read(path)?;
    let trace: SmokeTrace = serde_json::from_slice(&bytes)
        .map_err(|e| PersistenceError::Serialization(e.to_string()))?;
    if trace.schema_version > SCHEMA_VERSION {
        return Err(PersistenceError::Serialization(format!(
            "unsupported trace schema_version {} (max supported {})",
            trace.schema_version, SCHEMA_VERSION
        )));
    }
    Ok(trace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::checkpoint::{append_event_with_conn, init_checkpoint_table};
    use chrono::TimeZone;

    fn seed_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        init_checkpoint_table(&conn).expect("init schema");
        conn
    }

    fn sample_trace() -> SmokeTrace {
        SmokeTrace {
            schema_version: SCHEMA_VERSION,
            run_id: "run-123".to_string(),
            workflow_type_id: "llxprt-issue-fix-v1".to_string(),
            config_id: "default".to_string(),
            captured_at: "2026-06-11T12:00:00+00:00".to_string(),
            final_outcome: TraceOutcome::Success,
            events: vec![
                TraceEvent {
                    seq: 0,
                    step_id: "select_issue".to_string(),
                    outcome: "success".to_string(),
                },
                TraceEvent {
                    seq: 1,
                    step_id: "fetch_issue".to_string(),
                    outcome: "success".to_string(),
                },
            ],
        }
    }

    #[test]
    fn export_trace_assigns_seq_in_recorded_order() {
        let conn = seed_conn();
        let base = Utc.with_ymd_and_hms(2026, 6, 11, 0, 0, 0).unwrap();
        append_event_with_conn(&conn, "r1", "step_a", "success", base).unwrap();
        append_event_with_conn(
            &conn,
            "r1",
            "step_b",
            "fatal",
            base + chrono::Duration::seconds(1),
        )
        .unwrap();
        append_event_with_conn(
            &conn,
            "r1",
            "step_c",
            "abandon",
            base + chrono::Duration::seconds(2),
        )
        .unwrap();

        let outcome = RunOutcome::Abandoned {
            step_id: "step_c".to_string(),
            reason: "loop limit".to_string(),
        };
        let trace = export_trace(&conn, "r1", "wf", "cfg", &outcome).unwrap();

        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[0].seq, 0);
        assert_eq!(trace.events[0].step_id, "step_a");
        assert_eq!(trace.events[0].outcome, "success");
        assert_eq!(trace.events[1].seq, 1);
        assert_eq!(trace.events[1].step_id, "step_b");
        assert_eq!(trace.events[1].outcome, "fatal");
        assert_eq!(trace.events[2].seq, 2);
        assert_eq!(trace.events[2].step_id, "step_c");
        assert_eq!(trace.workflow_type_id, "wf");
        assert_eq!(trace.config_id, "cfg");
        assert!(matches!(
            trace.final_outcome,
            TraceOutcome::Abandoned { .. }
        ));
    }

    #[test]
    fn save_then_load_roundtrips_equal() {
        let trace = sample_trace();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.json");
        save_trace(&trace, &path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains('\n'), "expected pretty JSON with newlines");

        let loaded = load_trace(&path).unwrap();
        assert_eq!(loaded, trace);
    }

    #[test]
    fn load_trace_rejects_future_schema_version() {
        let mut trace = sample_trace();
        trace.schema_version = SCHEMA_VERSION + 1;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.json");
        // Write directly to bypass save_trace (which always stamps current version).
        let bytes = serde_json::to_vec_pretty(&trace).unwrap();
        std::fs::write(&path, bytes).unwrap();

        let err = load_trace(&path).unwrap_err();
        assert!(matches!(err, PersistenceError::Serialization(_)));
    }

    #[test]
    fn trace_outcome_from_run_outcome_maps_all_variants() {
        assert_eq!(
            TraceOutcome::from(&RunOutcome::Success),
            TraceOutcome::Success
        );
        assert_eq!(
            TraceOutcome::from(&RunOutcome::Failure {
                step_id: "s".to_string(),
                reason: "r".to_string(),
            }),
            TraceOutcome::Failure {
                step_id: "s".to_string(),
                reason: "r".to_string(),
            }
        );
        assert_eq!(
            TraceOutcome::from(&RunOutcome::Abandoned {
                step_id: "s".to_string(),
                reason: "r".to_string(),
            }),
            TraceOutcome::Abandoned {
                step_id: "s".to_string(),
                reason: "r".to_string(),
            }
        );
        assert_eq!(
            TraceOutcome::from(&RunOutcome::Interrupted {
                step_id: "s".to_string(),
            }),
            TraceOutcome::Interrupted {
                step_id: "s".to_string(),
            }
        );
    }

    #[test]
    fn matches_run_outcome_ignores_reason_text() {
        let recorded = TraceOutcome::Failure {
            step_id: "create_plan".to_string(),
            reason: "recorded reason".to_string(),
        };
        assert!(recorded.matches_run_outcome(&RunOutcome::Failure {
            step_id: "create_plan".to_string(),
            reason: "different live reason".to_string(),
        }));
        assert!(!recorded.matches_run_outcome(&RunOutcome::Failure {
            step_id: "other_step".to_string(),
            reason: "recorded reason".to_string(),
        }));
        assert!(!recorded.matches_run_outcome(&RunOutcome::Success));
    }
}
