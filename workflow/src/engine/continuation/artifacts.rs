//! Continuation artifact (JSON) builders and writers.
//!
//! Extracted from the parent continuation module to keep the auditable artifact
//! serialization (request/validation/selection/result bodies) in a single
//! cohesive unit.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};

use crate::persistence::{Checkpoint, RunMetadata};

use super::{checkpoint_identity, ContinuationKind, ContinuationValidation, RewindTarget};

/// Directory under which continuation artifacts for a run are written.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn continuation_artifact_dir(metadata: &RunMetadata, run_id: &str) -> PathBuf {
    let root = metadata.artifact_root.clone().unwrap_or_else(|| {
        crate::runtime_paths::get_artifacts_root()
            .to_string_lossy()
            .to_string()
    });
    Path::new(&root).join("continuation").join(run_id)
}

/// Write a JSON artifact, creating parent directories as needed.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn write_json_artifact(dir: &Path, name: &str, value: &Value) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub(super) fn rewind_target_json(kind: &ContinuationKind) -> Value {
    match kind {
        ContinuationKind::Rewind { target } => match target {
            RewindTarget::ToStep(step) => json!({ "to_step": step }),
            RewindTarget::ToCheckpoint(id) => json!({ "to_checkpoint": id }),
        },
        _ => Value::Null,
    }
}

/// JSON body of `continuation-request.json`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn request_artifact(request: &super::ContinuationRequest) -> Value {
    json!({
        "run_id": request.run_id,
        "kind": request.kind.verb(),
        "from_failed_step": matches!(
            request.kind,
            ContinuationKind::Retry { from_failed_step: true }
        ),
        "rewind_target": rewind_target_json(&request.kind),
        "force": request.force,
        "requested_at": Utc::now().to_rfc3339(),
        "why": "operator-initiated continuation of a failed or waiting run",
    })
}

pub(super) fn validation_artifact(validation: &ContinuationValidation) -> Value {
    json!({
        "ok": validation.ok,
        "checks": validation
            .checks
            .iter()
            .map(|c| json!({ "name": c.name, "passed": c.passed, "detail": c.detail }))
            .collect::<Vec<_>>(),
        "validated_at": Utc::now().to_rfc3339(),
    })
}

pub(super) fn selection_artifact(cp: &Checkpoint) -> Value {
    json!({
        "step_id": cp.step_id,
        "checkpoint_id": checkpoint_identity(cp),
        "status": cp.state_snapshot.status,
        "timestamp": cp.timestamp.to_rfc3339(),
        "loop_count": cp.state_snapshot.loop_count,
        "retry_count": cp.state_snapshot.retry_count,
    })
}

/// The result artifact file name for a continuation kind.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn result_artifact_name(kind: &ContinuationKind) -> &'static str {
    match kind {
        ContinuationKind::Retry { .. } => "retry-result.json",
        _ => "resume-result.json",
    }
}

/// JSON body of the `resume-result.json` / `retry-result.json` artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn result_artifact(
    kind: &ContinuationKind,
    status_label: &str,
    resumed_step: &str,
    external_state: Option<&str>,
) -> Value {
    json!({
        "kind": kind.verb(),
        "resumed_step": resumed_step,
        "status": status_label,
        "external_state_observed": external_state,
        "completed_at": Utc::now().to_rfc3339(),
    })
}
