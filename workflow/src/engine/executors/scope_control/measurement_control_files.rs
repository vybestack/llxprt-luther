use std::path::Path;
use std::process::Command;

use crate::engine::continuation::{verify_workspace_ownership_marker, WORKSPACE_OWNER_MARKER};

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
    daemon_managed: bool,
) -> Result<Vec<String>, MeasurementError> {
    if !daemon_managed {
        return Ok(data.untracked_files.clone());
    }

    if let Some(reason) = verify_workspace_ownership_marker(work_dir, run_id) {
        return Err(MeasurementError::ControlMetadata(format!(
            "cannot exclude untrusted workspace ownership marker: {reason}"
        )));
    }
    Ok(data
        .untracked_files
        .iter()
        .filter(|path| path.as_str() != WORKSPACE_OWNER_MARKER)
        .cloned()
        .collect())
}
