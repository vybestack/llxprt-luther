//! Descriptor-relative, no-follow access beneath one artifact-store root.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{
    flock, fsync, linkat, mkdirat, open, openat, renameat, statat, unlinkat, AtFlags, Dir,
    FileType, FlockOperation, Mode, OFlags,
};
use rustix::io::Errno;

use crate::engine::runner::EngineError;

use super::artifact_error;

pub const MAX_ARTIFACT_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_ARTIFACT_READ_BYTES: u64 = 8 * MAX_ARTIFACT_FILE_BYTES;
pub(super) const MAX_ARTIFACT_SCAN_DEPTH: usize = 32;
pub(super) const MAX_ARTIFACT_SCAN_FILES: usize = 100_000;

/// Filesystem seam for artifact store root canonicalization.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub trait PrFollowupFilesystem: Send + Sync {
    fn canonicalize_root(&self, path: &Path) -> Result<PathBuf, EngineError>;
}

/// System filesystem used by default artifact store construction.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Debug, Default)]
pub struct SystemPrFollowupFilesystem;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
impl PrFollowupFilesystem for SystemPrFollowupFilesystem {
    fn canonicalize_root(&self, path: &Path) -> Result<PathBuf, EngineError> {
        fs::create_dir_all(path)
            .map_err(|err| artifact_error(format!("create artifact root: {err}")))?;
        path.canonicalize()
            .map_err(|err| artifact_error(format!("canonicalize artifact root: {err}")))
    }
}

/// Encodes a string as one collision-free path segment. Already-safe values
/// retain their readable form; empty, dot, traversal, separator, Unicode, and
/// control-containing values use a reversible hexadecimal encoding.
pub fn sanitize_path_segment(value: &str) -> String {
    let readable = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if readable {
        return value.to_string();
    }
    let mut encoded = String::from("~");
    for byte in value.as_bytes() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[derive(Debug)]
pub(super) struct ReadBudget {
    consumed: u64,
    aggregate_limit: Option<u64>,
}

impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            consumed: 0,
            aggregate_limit: Some(MAX_ARTIFACT_READ_BYTES),
        }
    }
}

impl ReadBudget {
    pub(super) fn without_aggregate_limit() -> Self {
        Self {
            consumed: 0,
            aggregate_limit: None,
        }
    }

    fn ensure_available(&self, bytes: u64, path: &Path) -> Result<(), EngineError> {
        let total = self
            .consumed
            .checked_add(bytes)
            .ok_or_else(|| artifact_error("artifact aggregate read byte accounting overflowed"))?;
        if self.aggregate_limit.is_some_and(|limit| total > limit) {
            return Err(artifact_error(format!(
                "artifact aggregate reads exceed {MAX_ARTIFACT_READ_BYTES} bytes at {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn consume(&mut self, bytes: u64, path: &Path) -> Result<(), EngineError> {
        self.ensure_available(bytes, path)?;
        self.consumed = self
            .consumed
            .checked_add(bytes)
            .ok_or_else(|| artifact_error("artifact aggregate read byte accounting overflowed"))?;
        Ok(())
    }
}

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const LOCK_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::CREATE)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn canonicalize_root_alias(root: PathBuf) -> PathBuf {
    if fs::create_dir_all(&root).is_ok() {
        fs::canonicalize(&root).unwrap_or(root)
    } else {
        root
    }
}

pub(super) fn validate_contained_directory(
    root: &Path,
    target: &Path,
) -> Result<bool, EngineError> {
    match open_directory(root, target, false) {
        Ok(_) => Ok(true),
        Err(error) if is_not_found(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn validate_contained_file(root: &Path, target: &Path) -> Result<bool, EngineError> {
    match open_regular(root, target, false) {
        Ok(_) => Ok(true),
        Err(error) if is_not_found(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn read_contained_file_with_budget(
    root: &Path,
    path: &Path,
    budget: &mut ReadBudget,
) -> Result<String, EngineError> {
    let file = File::from(open_regular(root, path, false)?);
    read_open_file(file, path, budget)
}

fn read_open_file(
    mut file: File,
    path: &Path,
    budget: &mut ReadBudget,
) -> Result<String, EngineError> {
    let metadata_len = file
        .metadata()
        .map_err(|error| artifact_error(format!("read metadata for {}: {error}", path.display())))?
        .len();
    if metadata_len > MAX_ARTIFACT_FILE_BYTES {
        return Err(artifact_error(format!(
            "artifact file exceeds {MAX_ARTIFACT_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    budget.ensure_available(metadata_len, path)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_ARTIFACT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| artifact_error(format!("read {}: {error}", path.display())))?;
    let actual_len = u64::try_from(bytes.len())
        .map_err(|_| artifact_error("artifact file length exceeds supported range"))?;
    if actual_len > MAX_ARTIFACT_FILE_BYTES {
        return Err(artifact_error(format!(
            "artifact file exceeds {MAX_ARTIFACT_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    budget.consume(actual_len, path)?;
    String::from_utf8(bytes)
        .map_err(|error| artifact_error(format!("read {} as UTF-8: {error}", path.display())))
}

#[derive(Debug)]
pub(super) struct ContainedFile {
    pub(super) path: PathBuf,
    pub(super) content: String,
}

pub(super) fn read_contained_named_files_with_budget(
    root: &Path,
    scan_root: &Path,
    target_name: &OsStr,
    max_depth: usize,
    max_files: usize,
    budget: &mut ReadBudget,
) -> Result<Vec<ContainedFile>, EngineError> {
    let mut files = Vec::new();
    visit_contained_files_after_open(
        root,
        scan_root,
        FileSelection::Named(target_name),
        TraversalLimits {
            max_depth,
            max_files,
            recurse: true,
            symlink_policy: SymlinkPolicy::Skip,
        },
        budget,
        || {},
        |file| {
            files.push(file);
            Ok(())
        },
    )?;
    Ok(files)
}

#[cfg(test)]
pub(super) fn read_contained_json_files_with_budget(
    root: &Path,
    scan_root: &Path,
    budget: &mut ReadBudget,
) -> Result<Vec<ContainedFile>, EngineError> {
    collect_contained_json_files_with_budget(root, scan_root, SymlinkPolicy::Skip, budget)
}

pub(super) fn read_contained_history_candidates_with_budget(
    root: &Path,
    scan_root: &Path,
    budget: &mut ReadBudget,
) -> Result<Vec<ContainedFile>, EngineError> {
    collect_contained_json_files_with_budget(
        root,
        scan_root,
        SymlinkPolicy::RejectMatchingCandidate,
        budget,
    )
}

fn collect_contained_json_files_with_budget(
    root: &Path,
    scan_root: &Path,
    symlink_policy: SymlinkPolicy,
    budget: &mut ReadBudget,
) -> Result<Vec<ContainedFile>, EngineError> {
    let mut files = Vec::new();
    visit_contained_json_files_with_policy(root, scan_root, symlink_policy, budget, |file| {
        files.push(file);
        Ok(())
    })?;
    Ok(files)
}

pub(super) fn read_contained_json_directory_with_budget(
    root: &Path,
    scan_root: &Path,
    budget: &mut ReadBudget,
) -> Result<Vec<ContainedFile>, EngineError> {
    let mut files = Vec::new();
    visit_contained_files_after_open(
        root,
        scan_root,
        FileSelection::Json,
        TraversalLimits {
            max_depth: 0,
            max_files: MAX_ARTIFACT_SCAN_FILES,
            recurse: false,
            symlink_policy: SymlinkPolicy::Skip,
        },
        budget,
        || {},
        |file| {
            files.push(file);
            Ok(())
        },
    )?;
    Ok(files)
}

pub(super) fn visit_contained_json_files_with_budget(
    root: &Path,
    scan_root: &Path,
    budget: &mut ReadBudget,
    visitor: impl FnMut(ContainedFile) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    visit_contained_json_files_with_policy(root, scan_root, SymlinkPolicy::Skip, budget, visitor)
}

fn visit_contained_json_files_with_policy(
    root: &Path,
    scan_root: &Path,
    symlink_policy: SymlinkPolicy,
    budget: &mut ReadBudget,
    visitor: impl FnMut(ContainedFile) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    visit_contained_files_after_open(
        root,
        scan_root,
        FileSelection::Json,
        TraversalLimits {
            max_depth: MAX_ARTIFACT_SCAN_DEPTH,
            max_files: MAX_ARTIFACT_SCAN_FILES,
            recurse: true,
            symlink_policy,
        },
        budget,
        || {},
        visitor,
    )
}

#[derive(Clone, Copy)]
struct TraversalLimits {
    max_depth: usize,
    max_files: usize,
    recurse: bool,
    symlink_policy: SymlinkPolicy,
}

#[derive(Clone, Copy)]
enum SymlinkPolicy {
    Skip,
    RejectMatchingCandidate,
}

#[derive(Clone, Copy)]
enum FileSelection<'a> {
    Named(&'a OsStr),
    Json,
}

impl FileSelection<'_> {
    fn matches(self, path: &Path) -> bool {
        match self {
            Self::Named(name) => path.file_name() == Some(name),
            Self::Json => path.extension() == Some(OsStr::new("json")),
        }
    }
}

#[cfg(test)]
fn read_contained_files_after_open(
    root: &Path,
    scan_root: &Path,
    selection: FileSelection<'_>,
    max_depth: usize,
    max_files: usize,
    budget: &mut ReadBudget,
    after_open: impl FnOnce(),
) -> Result<Vec<ContainedFile>, EngineError> {
    let mut files = Vec::new();
    visit_contained_files_after_open(
        root,
        scan_root,
        selection,
        TraversalLimits {
            max_depth,
            max_files,
            recurse: true,
            symlink_policy: SymlinkPolicy::Skip,
        },
        budget,
        after_open,
        |file| {
            files.push(file);
            Ok(())
        },
    )?;
    Ok(files)
}

#[cfg(test)]
fn visit_current_json_after_open(
    root: &Path,
    scan_root: &Path,
    budget: &mut ReadBudget,
    after_open: impl FnOnce(),
    visitor: impl FnMut(ContainedFile) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    visit_contained_files_after_open(
        root,
        scan_root,
        FileSelection::Json,
        TraversalLimits {
            max_depth: 0,
            max_files: MAX_ARTIFACT_SCAN_FILES,
            recurse: false,
            symlink_policy: SymlinkPolicy::Skip,
        },
        budget,
        after_open,
        visitor,
    )
}

fn visit_contained_files_after_open(
    root: &Path,
    scan_root: &Path,
    selection: FileSelection<'_>,
    limits: TraversalLimits,
    budget: &mut ReadBudget,
    after_open: impl FnOnce(),
    mut visitor: impl FnMut(ContainedFile) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let directory = open_directory(root, scan_root, false)?;
    let mut traversal = ContainedTraversal::new(selection, limits, budget, &mut visitor);
    traversal.remember_directory(&directory, scan_root)?;
    after_open();
    traversal.walk(directory, scan_root, 0)?;
    traversal.verify_unchanged(root)
}

struct DirectoryIdentity {
    path: PathBuf,
    identity: String,
}

struct ContainedTraversal<'a, 'budget, 'visitor> {
    selection: FileSelection<'a>,
    limits: TraversalLimits,
    matching_files: usize,
    budget: &'budget mut ReadBudget,
    visitor: &'visitor mut dyn FnMut(ContainedFile) -> Result<(), EngineError>,
    directories: Vec<DirectoryIdentity>,
}

impl<'a, 'budget, 'visitor> ContainedTraversal<'a, 'budget, 'visitor> {
    fn new(
        selection: FileSelection<'a>,
        limits: TraversalLimits,
        budget: &'budget mut ReadBudget,
        visitor: &'visitor mut dyn FnMut(ContainedFile) -> Result<(), EngineError>,
    ) -> Self {
        Self {
            selection,
            limits,
            matching_files: 0,
            budget,
            visitor,
            directories: Vec::new(),
        }
    }

    fn walk(
        &mut self,
        directory: OwnedFd,
        logical_path: &Path,
        depth: usize,
    ) -> Result<(), EngineError> {
        if depth > self.limits.max_depth {
            return Err(artifact_error(format!(
                "artifact directory nesting exceeds {} at {}",
                self.limits.max_depth,
                logical_path.display()
            )));
        }
        let mut entries = Dir::read_from(&directory).map_err(|error| {
            artifact_error(format!(
                "read artifact directory {}: {error}",
                logical_path.display()
            ))
        })?;
        while let Some(entry) = entries.read() {
            let entry = entry.map_err(|error| {
                artifact_error(format!(
                    "read artifact directory entry in {}: {error}",
                    logical_path.display()
                ))
            })?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let path = logical_path.join(OsStr::from_bytes(name.to_bytes()));
            self.visit_entry(&directory, name, entry.file_type(), &path, depth)?;
        }
        Ok(())
    }

    fn visit_entry(
        &mut self,
        directory: &OwnedFd,
        name: &std::ffi::CStr,
        file_type: FileType,
        path: &Path,
        depth: usize,
    ) -> Result<(), EngineError> {
        let file_type = classify_entry(directory, name, file_type, path)?;
        match file_type {
            FileType::Directory if self.limits.recurse => {
                self.walk_child(directory, name, path, depth)
            }
            FileType::Directory => Ok(()),
            FileType::RegularFile => self.read_file(directory, name, path),
            FileType::Symlink => self.visit_symlink(path),
            _ => Err(artifact_error(format!(
                "artifact traversal encountered unsupported file type: {}",
                path.display()
            ))),
        }
    }

    fn visit_symlink(&self, path: &Path) -> Result<(), EngineError> {
        if matches!(
            self.limits.symlink_policy,
            SymlinkPolicy::RejectMatchingCandidate
        ) && self.selection.matches(path)
        {
            return Err(artifact_error(format!(
                "artifact traversal encountered symbolic link candidate: {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn walk_child(
        &mut self,
        directory: &OwnedFd,
        name: &std::ffi::CStr,
        path: &Path,
        depth: usize,
    ) -> Result<(), EngineError> {
        let child = openat(directory, name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| descriptor_error("open artifact directory", path, error))?;
        self.walk_open_child(child, path, depth)
    }

    fn walk_open_child(
        &mut self,
        child: OwnedFd,
        path: &Path,
        depth: usize,
    ) -> Result<(), EngineError> {
        self.remember_directory(&child, path)?;
        self.walk(child, path, depth + 1)
    }

    fn read_file(
        &mut self,
        directory: &OwnedFd,
        name: &std::ffi::CStr,
        path: &Path,
    ) -> Result<(), EngineError> {
        if !self.selection.matches(path) {
            return Ok(());
        }
        if self.matching_files >= self.limits.max_files {
            return Err(artifact_error(format!(
                "artifact scan exceeds {} matching files under {}",
                self.limits.max_files,
                path.display()
            )));
        }
        self.matching_files += 1;
        let fd = openat(directory, name, READ_FLAGS, Mode::empty())
            .map_err(|error| descriptor_error("open artifact traversal file", path, error))?;
        let file = File::from(fd);
        require_regular(&file, path)?;
        let content = read_open_file(file, path, self.budget)?;
        (self.visitor)(ContainedFile {
            path: path.to_path_buf(),
            content,
        })
    }

    fn remember_directory(&mut self, directory: &OwnedFd, path: &Path) -> Result<(), EngineError> {
        self.directories.push(DirectoryIdentity {
            path: path.to_path_buf(),
            identity: directory_identity(directory, path)?,
        });
        Ok(())
    }

    fn verify_unchanged(&self, root: &Path) -> Result<(), EngineError> {
        for expected in &self.directories {
            let current = open_directory(root, &expected.path, false)?;
            if directory_identity(&current, &expected.path)? != expected.identity {
                return Err(artifact_error(format!(
                    "artifact directory changed during traversal: {}",
                    expected.path.display()
                )));
            }
        }
        Ok(())
    }
}

fn classify_entry(
    directory: &OwnedFd,
    name: &std::ffi::CStr,
    file_type: FileType,
    path: &Path,
) -> Result<FileType, EngineError> {
    if file_type != FileType::Unknown {
        return Ok(file_type);
    }
    let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| descriptor_error("read artifact traversal entry metadata", path, error))?;
    Ok(FileType::from_raw_mode(metadata.st_mode))
}

fn directory_identity(directory: &OwnedFd, path: &Path) -> Result<String, EngineError> {
    let metadata = rustix::fs::fstat(directory)
        .map_err(|error| descriptor_error("read artifact directory identity", path, error))?;
    Ok(format!("{}:{}", metadata.st_dev, metadata.st_ino))
}

pub(super) fn acquire_publication_lock(
    root: &Path,
    binding_root: &Path,
) -> Result<File, EngineError> {
    let directory = open_directory(root, binding_root, true)?;
    let fd = openat(
        &directory,
        ".publication-lock",
        LOCK_FLAGS,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| artifact_error(format!("open artifact publication lock: {error}")))?;
    let file = File::from(fd);
    require_regular(&file, &binding_root.join(".publication-lock"))?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|error| artifact_error(format!("acquire artifact publication lock: {error}")))?;
    Ok(file)
}

pub(super) fn durable_create_new(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), EngineError> {
    publish(root, path, bytes, PublicationKind::CreateNew)
}

pub(super) fn durable_replace(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    publish(root, path, bytes, PublicationKind::Replace)
}

pub(super) struct RetainedPublicationDirectory {
    directory: OwnedFd,
    path: PathBuf,
    identity: String,
}

impl RetainedPublicationDirectory {
    pub(super) fn verify_identity(&self, root: &Path) -> Result<(), EngineError> {
        let current = open_directory(root, &self.path, false)?;
        if directory_identity(&current, &self.path)? != self.identity {
            return Err(artifact_error(format!(
                "artifact publication directory changed during transaction: {}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

pub(super) fn retain_publication_parent(
    root: &Path,
    path: &Path,
) -> Result<RetainedPublicationDirectory, EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", path.display())))?;
    let directory = open_directory(root, parent, true)?;
    let identity = directory_identity(&directory, parent)?;
    Ok(RetainedPublicationDirectory {
        directory,
        path: parent.to_path_buf(),
        identity,
    })
}

pub(super) fn validate_publication_size(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let serialized_size = u64::try_from(bytes.len())
        .map_err(|_| artifact_error("serialized artifact length exceeds supported range"))?;
    if serialized_size > MAX_ARTIFACT_FILE_BYTES {
        return Err(artifact_error(format!(
            "serialized artifact exceeds {MAX_ARTIFACT_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn publish_in_retained_directory(
    retained: &RetainedPublicationDirectory,
    path: &Path,
    bytes: &[u8],
    create_new: bool,
) -> Result<(), EngineError> {
    validate_publication_size(path, bytes)?;
    if path.parent() != Some(retained.path.as_path()) {
        return Err(artifact_error("retained publication parent path mismatch"));
    }
    publish_open_directory(
        &retained.directory,
        &retained.path,
        path,
        bytes,
        if create_new {
            PublicationKind::CreateNew
        } else {
            PublicationKind::Replace
        },
    )
}

pub(super) fn rename_contained_file(
    root: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), EngineError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", source.display())))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", destination.display())))?;
    let source_directory = open_directory(root, source_parent, false)?;
    let destination_directory = open_directory(root, destination_parent, true)?;
    let source_name = file_name(source)?;
    let destination_name = file_name(destination)?;
    let source_fd = openat(&source_directory, source_name, READ_FLAGS, Mode::empty())
        .map_err(|error| artifact_error(format!("open {}: {error}", source.display())))?;
    require_regular(&File::from(source_fd), source)?;
    renameat(
        &source_directory,
        source_name,
        &destination_directory,
        destination_name,
    )
    .map_err(|error| artifact_error(format!("rename {}: {error}", source.display())))?;
    sync_fd(&source_directory, source_parent)?;
    if source_parent != destination_parent {
        sync_fd(&destination_directory, destination_parent)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum PublicationKind {
    CreateNew,
    Replace,
}

fn publish(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    kind: PublicationKind,
) -> Result<(), EngineError> {
    validate_publication_size(path, bytes)?;
    let parent = path
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", path.display())))?;
    let directory = open_directory(root, parent, true)?;
    publish_open_directory(&directory, parent, path, bytes, kind)
}

fn publish_open_directory(
    directory: &OwnedFd,
    parent: &Path,
    path: &Path,
    bytes: &[u8],
    kind: PublicationKind,
) -> Result<(), EngineError> {
    let destination = file_name(path)?;
    let temp_name = format!(
        ".{}.{}.tmp",
        destination.to_string_lossy(),
        uuid::Uuid::new_v4()
    );
    let result = (|| {
        let fd = openat(
            directory,
            temp_name.as_str(),
            CREATE_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| artifact_error(format!("create artifact temp file: {error}")))?;
        let mut file = File::from(fd);
        file.write_all(bytes)
            .map_err(|error| artifact_error(format!("write artifact file: {error}")))?;
        file.sync_all()
            .map_err(|error| artifact_error(format!("sync artifact file: {error}")))?;
        drop(file);
        match kind {
            PublicationKind::CreateNew => {
                linkat(
                    directory,
                    temp_name.as_str(),
                    directory,
                    destination,
                    AtFlags::empty(),
                )
                .map_err(|error| {
                    artifact_error(format!(
                        "publish immutable history {} without replacement: {error}",
                        path.display()
                    ))
                })?;
                sync_fd(directory, parent)?;
                unlinkat(directory, temp_name.as_str(), AtFlags::empty()).map_err(|error| {
                    artifact_error(format!("remove history temp file: {error}"))
                })?;
            }
            PublicationKind::Replace => {
                renameat(directory, temp_name.as_str(), directory, destination).map_err(
                    |error| {
                        artifact_error(format!("atomic rename into {}: {error}", path.display()))
                    },
                )?;
            }
        }
        sync_fd(directory, parent)
    })();
    if result.is_err() {
        let _ = unlinkat(directory, temp_name.as_str(), AtFlags::empty());
    }
    result
}

fn open_regular(root: &Path, path: &Path, create_parent: bool) -> Result<OwnedFd, EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", path.display())))?;
    let directory = open_directory(root, parent, create_parent)?;
    let fd = openat(&directory, file_name(path)?, READ_FLAGS, Mode::empty())
        .map_err(|error| descriptor_error("open artifact file", path, error))?;
    require_regular(
        &File::from(fd.try_clone().map_err(|error| {
            artifact_error(format!("clone descriptor for {}: {error}", path.display()))
        })?),
        path,
    )?;
    Ok(fd)
}

fn open_directory(root: &Path, target: &Path, create: bool) -> Result<OwnedFd, EngineError> {
    ensure_root(root)?;
    let relative = contained_relative(root, target)?;
    let mut directory = open(root, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| descriptor_error("open artifact root", root, error))?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(artifact_error(format!(
                "artifact path contains a non-normal component: {}",
                target.display()
            )));
        };
        directory = match openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) if create => {
                match mkdirat(&directory, name, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => {
                        return Err(descriptor_error("create artifact directory", target, error))
                    }
                }
                fsync(&directory).map_err(|error| {
                    artifact_error(format!("sync artifact parent directory: {error}"))
                })?;
                openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
                    descriptor_error("open created artifact directory", target, error)
                })?
            }
            Err(error) => {
                return Err(descriptor_error("open artifact directory", target, error));
            }
        };
    }
    Ok(directory)
}

fn ensure_root(root: &Path) -> Result<(), EngineError> {
    match fs::create_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) => Err(artifact_error(format!(
            "create artifact store root {}: {error}",
            root.display()
        ))),
    }
}

fn contained_relative<'a>(root: &Path, target: &'a Path) -> Result<&'a Path, EngineError> {
    target.strip_prefix(root).map_err(|_| {
        artifact_error(format!(
            "artifact path {} is outside store root {}",
            target.display(),
            root.display()
        ))
    })
}

fn file_name(path: &Path) -> Result<&OsStr, EngineError> {
    path.file_name()
        .filter(|name| !name.is_empty() && *name != OsStr::new(".") && *name != OsStr::new(".."))
        .ok_or_else(|| artifact_error(format!("invalid artifact filename: {}", path.display())))
}

fn require_regular(file: &File, path: &Path) -> Result<(), EngineError> {
    let metadata = file.metadata().map_err(|error| {
        artifact_error(format!("read metadata for {}: {error}", path.display()))
    })?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(artifact_error(format!(
            "artifact path is not a regular file: {}",
            path.display()
        )))
    }
}

fn sync_fd(fd: &OwnedFd, path: &Path) -> Result<(), EngineError> {
    fsync(fd).map_err(|error| artifact_error(format!("sync directory {}: {error}", path.display())))
}

fn descriptor_error(action: &str, path: &Path, error: Errno) -> EngineError {
    artifact_error(format!("{action} {}: {error}", path.display()))
}

fn is_not_found(error: &EngineError) -> bool {
    error.to_string().contains("No such file or directory")
        || error.to_string().contains("os error 2")
}

#[cfg(test)]
mod path_safety_tests;
