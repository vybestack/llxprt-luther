use super::LlxprtInvocationResult;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug)]
pub(super) struct PreviousResultSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    modified_at: Option<SystemTime>,
}

impl PreviousResultSnapshot {
    pub(super) fn capture(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            bytes: read_optional_bytes(path),
            modified_at: file_modified_at(path),
        }
    }
}

#[derive(Debug)]
pub(super) struct RemediationResultState {
    pub(super) was_updated: bool,
    pub(super) available: bool,
    pub(super) validator_readable: bool,
}

pub(super) fn remediation_result_state(
    invocation: &LlxprtInvocationResult,
    result_path: &Path,
    previous: &PreviousResultSnapshot,
) -> RemediationResultState {
    let was_updated = result_file_is_fresh(result_path, previous);
    let validator_readable = result_file_non_empty(result_path);
    let available = was_updated
        || (invocation.result_file_present
            && invocation.process_class == "success"
            && previous.modified_at.is_none()
            && validator_readable);
    RemediationResultState {
        was_updated,
        available,
        validator_readable,
    }
}

pub(super) fn result_file_non_empty(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.len() > 0)
}

fn read_optional_bytes(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn file_modified_at(path: &Path) -> Option<SystemTime> {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn result_file_is_fresh(path: &Path, previous: &PreviousResultSnapshot) -> bool {
    let Some(current_bytes) = read_optional_bytes(path).filter(|bytes| !bytes.is_empty()) else {
        return false;
    };
    if path != previous.path {
        return true;
    }
    if previous.bytes.as_deref() != Some(current_bytes.as_slice()) {
        return true;
    }
    // Keep this strict: accepting equal mtimes would make an untouched stale result file look fresh.
    file_modified_at(path)
        .zip(previous.modified_at)
        .is_some_and(|(current, previous)| current > previous)
}
