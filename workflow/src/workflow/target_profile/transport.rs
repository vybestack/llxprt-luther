//! Git transport resolution and validation.
//!
//! Logical repository identity (owner/name, used for the GitHub API) is
//! separate from the transport Git clones and pushes over. Keeping the two
//! apart lets a harness target a local repository without faking Git.

use super::insert_var;
use crate::workflow::config_loader::{ConfigError, ConfigErrorKind, Result};
use crate::workflow::schema::WorkflowConfig;
use std::path::Path;

/// Variable holding the resolved Git transport URL.
pub const GIT_TRANSPORT_URL_VAR: &str = "git_transport_url";

/// Records how the transport was obtained: `explicit` or `derived`.
///
/// Provenance has to be stored, not inferred. Once a derived URL is written
/// into the variable map it is textually indistinguishable from an explicit
/// one, so a later repository override could not tell whether it was allowed to
/// recompute -- and would leave Git pointing at the previous repository while
/// the GitHub API addressed the new one.
///
/// Recording the value (rather than only its presence) keeps resolution
/// idempotent: resolving an already-resolved config must not promote a derived
/// URL to an explicit one, which would freeze it against later overrides.
pub const GIT_TRANSPORT_SOURCE_VAR: &str = "git_transport_url_source";
pub const TRANSPORT_EXPLICIT: &str = "explicit";
pub(crate) const TRANSPORT_DERIVED: &str = "derived";

/// Production transport for a logical repository.
///
/// The single definition of the default, so the derived value cannot drift from
/// what shipped before the transport seam existed.
#[must_use]
pub fn default_transport_url(target_repo: &str) -> String {
    format!("https://github.com/{target_repo}.git")
}

/// Resolve the Git transport URL, preferring an explicit value over the default
/// derived from logical identity.
///
/// Precedence: an already-set `git_transport_url` variable (from an override or
/// the config) wins; otherwise it is derived from `target_repo`. With no
/// override the result is byte-identical to the previously hardcoded URL.
pub(super) fn resolve_transport_url(config: &mut WorkflowConfig) -> Result<()> {
    // An explicit transport is authoritative and is never recomputed.
    if config
        .variables
        .get(GIT_TRANSPORT_SOURCE_VAR)
        .map(String::as_str)
        == Some(TRANSPORT_EXPLICIT)
    {
        let existing = config
            .variables
            .get(GIT_TRANSPORT_URL_VAR)
            .cloned()
            .unwrap_or_default();
        validate_transport_url(&existing)?;
        return Ok(());
    }
    // A derived transport always tracks current logical identity, so changing
    // the repository moves the push target with it.
    let Some(target_repo) = config.variables.get("target_repo").cloned() else {
        return Ok(());
    };
    let derived = default_transport_url(&target_repo);
    validate_transport_url(&derived)?;
    insert_var(config, GIT_TRANSPORT_URL_VAR, &derived);
    insert_var(config, GIT_TRANSPORT_SOURCE_VAR, TRANSPORT_DERIVED);
    Ok(())
}

/// Reject a transport URL that cannot be used safely.
///
/// Fails closed before any mutation: an unresolved template, embedded control
/// bytes, or a leading dash (which Git would read as an option) is refused
/// rather than passed to Git and discovered mid-run.
pub(super) fn validate_transport_url(url: &str) -> Result<()> {
    let invalid = |detail: &str| {
        Err(ConfigError {
            message: format!("invalid git transport url {url:?}: {detail}"),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })
    };
    if url.is_empty() {
        return invalid("must not be empty");
    }
    if url.contains(['{', '}']) {
        return invalid("contains an unresolved template placeholder");
    }
    if url.chars().any(char::is_control) {
        return invalid("contains control characters");
    }
    if url.starts_with('-') {
        return invalid("must not begin with '-', which Git would read as an option");
    }
    // Local paths are permitted so a harness can push to a bare repository on
    // disk; that is the whole point of separating transport from identity.
    //
    // Each form is checked for an actual authority or path rather than a bare
    // prefix, because "https://" and "git@" are prefixes of themselves and
    // would otherwise be accepted as valid transports.
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.is_empty() {
            return invalid("is missing a host");
        }
        if path.is_empty() {
            return invalid("is missing a repository path");
        }
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("file://") {
        // Only the local form file:///path is supported; a remote authority
        // would not be a local transport at all.
        if !rest.starts_with('/') {
            return invalid("must be of the form file:///absolute/path");
        }
        return validate_local_transport_path(url, Path::new(rest));
    }
    if let Some((user_host, path)) = url.split_once(':') {
        // scp-style: user@host:path
        if let Some((user, host)) = user_host.split_once('@') {
            if user.is_empty() || host.is_empty() {
                return invalid("is missing a user or host");
            }
            if path.is_empty() {
                return invalid("is missing a repository path");
            }
            return Ok(());
        }
    }
    if url.starts_with('/') {
        return validate_local_transport_path(url, Path::new(url));
    }
    invalid("must be an https, ssh, file, scp-style, or absolute-path transport")
}

/// A local transport must exist and look like a Git repository.
///
/// A nonexistent path would otherwise be accepted here and only fail during
/// fetch or push -- after the workspace has already been mutated, which is
/// exactly what "fails closed before any mutation" forbids.
fn validate_local_transport_path(url: &str, path: &Path) -> Result<()> {
    let invalid = |detail: &str| {
        Err(ConfigError {
            message: format!("invalid git transport url {url:?}: {detail}"),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })
    };
    if !path.exists() {
        return invalid("points at a path that does not exist");
    }
    if !path.is_dir() {
        return invalid("points at something that is not a directory");
    }
    // Bare repository (HEAD + objects) or a working tree with .git.
    let bare = path.join("HEAD").exists() && path.join("objects").is_dir();
    if !bare && !path.join(".git").exists() {
        return invalid("does not look like a Git repository");
    }
    Ok(())
}
