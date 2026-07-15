//! Validation for durable post-PR failure terminal artifacts.

use serde::Deserialize;
use serde_json::Value;

use super::{artifact_error, EngineError, PostPrFailureTerminal};

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct TerminalSourceArtifact {
    artifact_family: String,
    artifact_sequence: u64,
    write_sequence: u64,
    failure_sequence: u64,
    producer_step_id: String,
    step_order_index: u64,
    path: String,
    history_path: String,
    failure_reason: Option<String>,
}

impl TerminalSourceArtifact {
    fn validate(&self) -> Result<(), EngineError> {
        let sequences_are_positive = self.artifact_sequence > 0
            && self.write_sequence > 0
            && self.failure_sequence > 0
            && self.step_order_index > 0;
        let identity_is_complete = [
            self.artifact_family.as_str(),
            self.producer_step_id.as_str(),
            self.path.as_str(),
            self.history_path.as_str(),
        ]
        .iter()
        .all(|field| !field.is_empty());
        if !sequences_are_positive || !identity_is_complete {
            return Err(artifact_error(
                "terminal source artifact fields must be exact and non-empty",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TerminalArtifactValidation {
    terminal_state: String,
    terminal_reason: String,
    failure_reason: String,
    failed_step: String,
    source_artifacts: Vec<TerminalSourceArtifact>,
    selected_source_reason: String,
    idempotency_key: String,
    logged_at: String,
    source_failure_sequence: Option<u64>,
    source_artifact_sequence: Option<u64>,
    source_write_sequence: Option<u64>,
    source_step_order_index: Option<u64>,
    source_producer_step_id: Option<String>,
    source_artifact_path: Option<String>,
    source_history_path: Option<String>,
    source_failure_reason: Option<String>,
    source_artifact_family: Option<String>,
}

impl TerminalArtifactValidation {
    fn validate(&self) -> Result<(), EngineError> {
        self.validate_envelope()?;
        let unique_sources = self
            .source_artifacts
            .iter()
            .map(|source| (source.artifact_family.as_str(), source.artifact_sequence))
            .collect::<std::collections::BTreeSet<_>>();
        if unique_sources.len() != self.source_artifacts.len() {
            return Err(artifact_error(
                "terminal source_artifacts contains duplicates",
            ));
        }
        for source in &self.source_artifacts {
            source.validate()?;
        }
        if self.source_artifacts.is_empty() {
            return self.validate_no_source_selection();
        }
        let selected = self.selected_source()?;
        if self.selected_source_reason != "highest_failure_sequence" {
            return Err(artifact_error(
                "terminal with sources must use highest_failure_sequence",
            ));
        }
        if !self.source_artifacts.contains(&selected) {
            return Err(artifact_error(
                "terminal selected source does not exactly match source_artifacts",
            ));
        }
        let expected = self
            .source_artifacts
            .iter()
            .max_by_key(|source| {
                (
                    source.failure_sequence,
                    source.artifact_sequence,
                    source.write_sequence,
                    source.producer_step_id.as_str(),
                )
            })
            .ok_or_else(|| artifact_error("terminal source list unexpectedly empty"))?;
        if expected != &selected {
            return Err(artifact_error(
                "terminal selected source is not the highest failure sequence",
            ));
        }
        if selected.failure_reason.as_deref() != Some(self.failure_reason.as_str())
            || self.source_failure_reason.as_deref() != Some(self.failure_reason.as_str())
        {
            return Err(artifact_error(
                "terminal failure_reason must match the selected source failure reason",
            ));
        }
        Ok(())
    }

    fn validate_envelope(&self) -> Result<(), EngineError> {
        if self.terminal_state != "fatal" {
            return Err(artifact_error("terminal_state must be fatal"));
        }
        let required_text = [
            self.terminal_reason.as_str(),
            self.failure_reason.as_str(),
            self.failed_step.as_str(),
            self.selected_source_reason.as_str(),
            self.idempotency_key.as_str(),
            self.logged_at.as_str(),
        ];
        if required_text.iter().any(|field| field.is_empty()) {
            return Err(artifact_error(
                "terminal envelope string fields must be non-empty",
            ));
        }
        chrono::DateTime::parse_from_rfc3339(&self.logged_at).map_err(|error| {
            artifact_error(format!("terminal logged_at must be RFC3339: {error}"))
        })?;
        Ok(())
    }

    fn validate_no_source_selection(&self) -> Result<(), EngineError> {
        let selection_fields_absent = self.source_failure_sequence.is_none()
            && self.source_artifact_sequence.is_none()
            && self.source_write_sequence.is_none()
            && self.source_step_order_index.is_none()
            && self.source_producer_step_id.is_none()
            && self.source_artifact_path.is_none()
            && self.source_history_path.is_none()
            && self.source_failure_reason.is_none()
            && self.source_artifact_family.is_none();
        if self.selected_source_reason != "no_failure_candidates" {
            return Err(artifact_error(
                "terminal without sources must use no_failure_candidates",
            ));
        }
        if !selection_fields_absent {
            return Err(artifact_error(
                "terminal without sources must not populate selected-source fields",
            ));
        }
        if self.failure_reason != self.terminal_reason {
            return Err(artifact_error(
                "terminal without sources must use terminal_reason as failure_reason",
            ));
        }
        Ok(())
    }

    fn selected_source(&self) -> Result<TerminalSourceArtifact, EngineError> {
        let selected = TerminalSourceArtifact {
            artifact_family: required_selected_text(
                self.source_artifact_family.as_deref(),
                "source_artifact_family",
            )?,
            artifact_sequence: required_selected_sequence(
                self.source_artifact_sequence,
                "source_artifact_sequence",
            )?,
            write_sequence: required_selected_sequence(
                self.source_write_sequence,
                "source_write_sequence",
            )?,
            failure_sequence: required_selected_sequence(
                self.source_failure_sequence,
                "source_failure_sequence",
            )?,
            producer_step_id: required_selected_text(
                self.source_producer_step_id.as_deref(),
                "source_producer_step_id",
            )?,
            step_order_index: required_selected_sequence(
                self.source_step_order_index,
                "source_step_order_index",
            )?,
            path: required_selected_text(
                self.source_artifact_path.as_deref(),
                "source_artifact_path",
            )?,
            history_path: required_selected_text(
                self.source_history_path.as_deref(),
                "source_history_path",
            )?,
            failure_reason: self.source_failure_reason.clone(),
        };
        selected.validate()?;
        Ok(selected)
    }
}

fn required_selected_sequence(value: Option<u64>, field: &str) -> Result<u64, EngineError> {
    value.filter(|sequence| *sequence > 0).ok_or_else(|| {
        artifact_error(format!(
            "terminal selected source field {field} must be positive"
        ))
    })
}

fn required_selected_text(value: Option<&str>, field: &str) -> Result<String, EngineError> {
    value
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| artifact_error(format!("missing or invalid string field {field}")))
}

pub(super) fn validate_terminal_artifact(value: &Value) -> Result<(), EngineError> {
    serde_json::from_value::<PostPrFailureTerminal>(value.clone())
        .map_err(|error| artifact_error(format!("invalid exported terminal schema: {error}")))?;
    serde_json::from_value::<TerminalArtifactValidation>(value.clone())
        .map_err(|error| artifact_error(format!("invalid terminal artifact schema: {error}")))?
        .validate()
}
