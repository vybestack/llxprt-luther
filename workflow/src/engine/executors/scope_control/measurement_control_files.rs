use std::path::Path;
use std::process::Command;

use crate::engine::workspace_ownership::{verify_workspace_ownership, WORKSPACE_OWNER_MARKER};

use super::{GitPatchData, MeasurementError};

pub(super) fn collect_untracked_files(work_dir: &Path) -> Result<Vec<String>, MeasurementError> {
    let mut paths = run_ls_files(work_dir, &["--others", "--exclude-standard", "-z"])?;
    paths.extend(run_ls_files(
        work_dir,
        &["--others", "-z", "--", ".luther"],
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn run_ls_files(work_dir: &Path, args: &[&str]) -> Result<Vec<String>, MeasurementError> {
    let output = Command::new("git")
        .arg("ls-files")
        .args(args)
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: format!("ls-files {}", args.join(" ")),
            message: format!("failed to invoke git: {err}"),
        })?;

    if !output.status.success() {
        return Err(MeasurementError::Git {
            command: format!("ls-files {}", args.join(" ")),
            message: format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    parse_z_paths(&output.stdout)
}

pub(super) fn parse_z_paths(data: &[u8]) -> Result<Vec<String>, MeasurementError> {
    super::split_z(data)
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            String::from_utf8(segment.to_vec()).map_err(|err| {
                MeasurementError::Parse(format!("non-UTF-8 path in ls-files output: {err}"))
            })
        })
        .collect()
}

pub(super) fn patch_untracked_files(
    data: &GitPatchData,
    work_dir: &Path,
    run_id: &str,
    ownership_required: bool,
) -> Result<Vec<String>, MeasurementError> {
    // The bootstrap `.luther/workspace-owner` marker is control-plane metadata
    // written by the workflow itself, never by the implementing agent, so it is
    // not part of the measured change set for a run that provably owns the
    // workspace. The durable evidence lives beneath `.git`, which is naturally
    // invisible to scope measurement (Git never reports `.git` contents as
    // untracked files).
    //
    // Verification runs before any inspection of the Git file list so that
    // suppressed or manipulated Git output cannot bypass the check.
    let Some(reason) = verify_workspace_ownership(work_dir, run_id) else {
        // Ownership is proven, so the marker is control-plane state for this
        // run regardless of how the run was launched. Gating exclusion on the
        // launcher made scope measurement report the workflow's own metadata
        // as agent scope creep for every non-daemon run.
        return Ok(data
            .untracked_files
            .iter()
            .filter(|path| path.as_str() != WORKSPACE_OWNER_MARKER)
            .cloned()
            .collect());
    };

    // Ownership could not be verified. Where ownership is mandatory this is a
    // hard failure; otherwise the workspace legitimately carries no Luther
    // ownership evidence and there is simply nothing to exclude.
    if ownership_required {
        return Err(MeasurementError::ControlMetadata(format!(
            "cannot exclude untrusted workspace ownership marker for run '{run_id}': {reason}"
        )));
    }
    if data
        .untracked_files
        .iter()
        .any(|path| path.as_str() == WORKSPACE_OWNER_MARKER)
    {
        return Err(MeasurementError::ControlMetadata(format!(
            "workspace ownership marker present but unverified for run '{run_id}': {reason}"
        )));
    }
    Ok(data.untracked_files.clone())
}
