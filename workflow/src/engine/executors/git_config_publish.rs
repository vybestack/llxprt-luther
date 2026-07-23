//! Descriptor-anchored publication of the repository-local Git configuration.
//!
//! Every path below the authorized workspace is resolved relative to an open
//! `O_NOFOLLOW` directory descriptor. The config is written to a private temp
//! file, synced, atomically renamed over `.git/config`, the directory is synced,
//! and the final bytes are read back from a newly opened descriptor.

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{fstat, fsync, openat, renameat, unlinkat, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::engine::workspace_ownership::{descriptor_matches_authorization, WorkspaceAnchor};

const STEP_ID: &str = "git_config_publish";
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

/// Publishes the exact repository-local Git configuration required by the
/// production setup workflows.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitConfigPublishExecutor;

impl StepExecutor for GitConfigPublishExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let authorization = context.workspace_authorization().ok_or_else(|| {
            fatal("workspace authorization from workspace_ownership_verify is required")
        })?;
        let origin_template = params
            .get("origin_url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| fatal("origin_url must be a string"))?;
        let origin_url = interpolate_string(origin_template, context);
        validate_origin_url(&origin_url)?;

        let workspace = WorkspaceAnchor::open(context.work_dir())
            .map_err(|error| fatal(&format!("failed to anchor workspace: {error}")))?;
        let authorized = descriptor_matches_authorization(workspace.as_fd(), authorization)
            .map_err(|error| {
                fatal(&format!(
                    "failed to inspect workspace authorization: {error}"
                ))
            })?;
        if !authorized {
            return Err(fatal(
                "workspace identity does not match workspace_ownership_verify authorization",
            ));
        }

        let git = open_directory(workspace.as_fd(), ".git", Path::new(".git"))?;
        remove_alternates(git.as_fd())?;
        let expected = render_config(&origin_url);
        publish_config(git.as_fd(), &expected)?;
        context.set("git_config_published", "true");
        Ok(StepOutcome::Success)
    }
}

fn validate_origin_url(origin_url: &str) -> Result<(), EngineError> {
    if origin_url.is_empty()
        || origin_url.contains(['\0', '\n', '\r'])
        || origin_url.contains('{')
        || origin_url.contains('}')
    {
        return Err(fatal(
            "origin_url is empty, unresolved, or contains invalid bytes",
        ));
    }
    Ok(())
}

fn render_config(origin_url: &str) -> Vec<u8> {
    format!(
        "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n\tlogallrefupdates = true\n\thooksPath = /dev/null\n[remote \"origin\"]\n\turl = {origin_url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
    )
    .into_bytes()
}

fn open_directory(
    parent: BorrowedFd<'_>,
    name: &str,
    display: &Path,
) -> Result<OwnedFd, EngineError> {
    let name = CString::new(name).map_err(|_| fatal("directory name contains a NUL byte"))?;
    openat(parent, &name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| fatal(&format!("failed to open {}: {error}", display.display())))
}

fn remove_alternates(git: BorrowedFd<'_>) -> Result<(), EngineError> {
    let objects = open_optional_directory(git, "objects", Path::new(".git/objects"))?;
    let Some(objects) = objects else {
        return Ok(());
    };
    let info = open_optional_directory(objects.as_fd(), "info", Path::new(".git/objects/info"))?;
    let Some(info) = info else {
        return Ok(());
    };
    let alternates = CString::new("alternates").expect("static C string");
    match unlinkat(info.as_fd(), &alternates, AtFlags::empty()) {
        Ok(()) => fsync(info.as_fd())
            .map_err(|error| fatal(&format!("failed to sync .git/objects/info: {error}"))),
        Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(fatal(&format!(
            "failed to remove .git/objects/info/alternates: {error}"
        ))),
    }
}

fn open_optional_directory(
    parent: BorrowedFd<'_>,
    name: &str,
    display: &Path,
) -> Result<Option<OwnedFd>, EngineError> {
    let name = CString::new(name).expect("static C string");
    match openat(parent, &name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(fatal(&format!(
            "failed to open {}: {error}",
            display.display()
        ))),
    }
}

fn publish_config(git: BorrowedFd<'_>, expected: &[u8]) -> Result<(), EngineError> {
    let temp_name = format!(".luther-config-{}.tmp", uuid::Uuid::new_v4());
    let temp = CString::new(temp_name.as_str()).expect("UUID temp name is a C string");
    let config = CString::new("config").expect("static C string");
    let create_flags =
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat(git, &temp, create_flags, Mode::from_raw_mode(0o600))
        .map_err(|error| fatal(&format!("failed to create temporary Git config: {error}")))?;

    let result = write_and_publish(git, fd, &temp, &config, expected);
    if result.is_err() {
        let _ = unlinkat(git, &temp, AtFlags::empty());
    }
    result
}

fn write_and_publish(
    git: BorrowedFd<'_>,
    fd: OwnedFd,
    temp: &CString,
    config: &CString,
    expected: &[u8],
) -> Result<(), EngineError> {
    let mut file = File::from(fd);
    file.write_all(expected)
        .map_err(|error| fatal(&format!("failed to write temporary Git config: {error}")))?;
    file.sync_all()
        .map_err(|error| fatal(&format!("failed to sync temporary Git config: {error}")))?;
    drop(file);

    renameat(git, temp, git, config)
        .map_err(|error| fatal(&format!("failed to publish .git/config: {error}")))?;
    fsync(git).map_err(|error| fatal(&format!("failed to sync .git directory: {error}")))?;
    let actual = read_regular_file(git, config, Path::new(".git/config"))?;
    if actual != expected {
        return Err(fatal("published .git/config did not read back exactly"));
    }
    Ok(())
}

fn read_regular_file(
    parent: BorrowedFd<'_>,
    name: &CString,
    display: &Path,
) -> Result<Vec<u8>, EngineError> {
    let fd = openat(parent, name, READ_FLAGS, Mode::empty())
        .map_err(|error| fatal(&format!("failed to open {}: {error}", display.display())))?;
    let stat = fstat(fd.as_fd())
        .map_err(|error| fatal(&format!("failed to stat {}: {error}", display.display())))?;
    if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(fatal(&format!(
            "{} is not a regular file",
            display.display()
        )));
    }
    let size = usize::try_from(stat.st_size)
        .map_err(|_| fatal(&format!("{} has an invalid size", display.display())))?;
    if size > MAX_CONFIG_BYTES {
        return Err(fatal(&format!("{} is too large", display.display())));
    }
    let mut bytes = Vec::with_capacity(size);
    File::from(fd)
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| fatal(&format!("failed to read {}: {error}", display.display())))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(fatal(&format!("{} is too large", display.display())));
    }
    Ok(bytes)
}

fn fatal(detail: &str) -> EngineError {
    EngineError::StepExecutionError {
        step_id: STEP_ID.to_string(),
        message: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn repository(workspace: &Path) {
        std::fs::create_dir_all(workspace.join(".git/objects/info")).unwrap();
    }

    fn context(workspace: &Path) -> StepContext {
        let authorization =
            crate::engine::workspace_ownership::capture_workspace_authorization(workspace).unwrap();
        let mut context = StepContext::new(workspace.to_path_buf(), "run-A".to_string());
        context.set_workspace_authorization(authorization);
        context.set("target_repo", "owner/repo");
        context.set_current_step_id(STEP_ID);
        context
    }

    fn params() -> serde_json::Value {
        serde_json::json!({"origin_url": "https://github.com/{target_repo}.git"})
    }

    fn execute(context: &mut StepContext) -> Result<StepOutcome, EngineError> {
        GitConfigPublishExecutor.execute(context, &params())
    }

    #[test]
    fn publishes_exact_config_and_is_idempotent() {
        let workspace = tempfile::tempdir().unwrap();
        repository(workspace.path());
        let mut context = context(workspace.path());

        assert_eq!(execute(&mut context).unwrap(), StepOutcome::Success);
        let first = std::fs::read(workspace.path().join(".git/config")).unwrap();
        assert_eq!(first, render_config("https://github.com/owner/repo.git"));
        assert_eq!(execute(&mut context).unwrap(), StepOutcome::Success);
        assert_eq!(
            std::fs::read(workspace.path().join(".git/config")).unwrap(),
            first
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaces_config_symlink_without_writing_its_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        repository(&workspace);
        let target = root.path().join("target");
        std::fs::write(&target, "sentinel").unwrap();
        symlink(&target, workspace.join(".git/config")).unwrap();
        let mut context = context(&workspace);

        execute(&mut context).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "sentinel");
        assert!(!std::fs::symlink_metadata(workspace.join(".git/config"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn removes_alternates_symlink_without_following_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        repository(&workspace);
        let target = root.path().join("target");
        std::fs::write(&target, "sentinel").unwrap();
        let alternates = workspace.join(".git/objects/info/alternates");
        symlink(&target, &alternates).unwrap();
        let mut context = context(&workspace);

        execute(&mut context).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "sentinel");
        assert!(!alternates.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_git_directory_without_touching_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        repository(&workspace);
        let mut context = context(&workspace);
        std::fs::remove_dir_all(workspace.join(".git")).unwrap();
        let target = root.path().join("outside-git");
        repository(&target);
        symlink(target.join(".git"), workspace.join(".git")).unwrap();

        assert!(execute(&mut context).is_err());
        assert!(!target.join(".git/config").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_descriptor_hop_before_config_publication() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/config"), "original").unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(outside.join("info")).unwrap();
        symlink(&outside, workspace.join(".git/objects")).unwrap();
        let mut context = context(&workspace);

        assert!(execute(&mut context).is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.join(".git/config")).unwrap(),
            "original"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_workspace_swap_after_authorization() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        repository(&workspace);
        let mut context = context(&workspace);
        let authorized = root.path().join("authorized");
        std::fs::rename(&workspace, &authorized).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        repository(&workspace);

        assert!(execute(&mut context).is_err());
        assert!(!workspace.join(".git/config").exists());
        assert!(!authorized.join(".git/config").exists());
    }

    #[test]
    fn requires_workspace_authorization() {
        let workspace = tempfile::tempdir().unwrap();
        repository(workspace.path());
        let mut context = StepContext::new(PathBuf::from(workspace.path()), "run-A".to_string());
        context.set("target_repo", "owner/repo");

        assert!(execute(&mut context).is_err());
        assert!(!workspace.path().join(".git/config").exists());
    }
}
