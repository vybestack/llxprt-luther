//! Defensive validation of identity-bearing tokens that may be interpolated
//! into shell command templates.
//!
//! ## Actual boundary: positional binding
//!
//! The shell executor's primary injection defense is positional parameter
//! binding ([`bind_shell_template`](crate::engine::executors::shell)): dynamic
//! values are never interpolated into the shell command string. Instead they
//! are passed as `$1`, `$2`, ... positional parameters, so shell parsing only
//! sees the static template. Metacharacters in GitHub or config values are
//! treated as literal data and are never reparsed as shell syntax.
//!
//! ## Supplementary validation
//!
//! [`validate_shell_safe_tokens`] is a **supplementary, defense-in-depth**
//! check. It is not wired into the shell executor's hot path because
//! positional binding is the authoritative boundary. It remains available for
//! callers that want an early rejection of suspicious identity values before
//! they reach any execution path, and it is covered by unit tests so the
//! character policies do not silently regress.
//!
//! # Character policies
//!
//! | Token class | Tokens | Allowed characters |
//! |---|---|---|
//! | Numeric | `issue_number`, `primary_issue_number` | `0-9` |
//! | Refname | `base_branch` | `A-Za-z0-9 - _ . /` |
//! | Path | `artifact_dir`, `work_dir` | any char except shell metacharacters/whitespace |
//!
//! @plan:PLAN-20260722-ISSUE158-SHELL-SAFE-TOKENS

use crate::engine::executor::StepContext;

/// Numeric-only tokens: must be ASCII digits with no sign or whitespace.
const SHELL_SAFE_NUMERIC_TOKENS: &[&str] = &["issue_number", "primary_issue_number"];

/// Git-refname tokens: must contain only git-safe refname characters.
const SHELL_SAFE_REFNAME_TOKENS: &[&str] = &["base_branch"];

/// Filesystem-path tokens: must not contain shell metacharacters or whitespace.
const SHELL_SAFE_PATH_TOKENS: &[&str] = &["artifact_dir", "work_dir"];

/// Validate that identity-bearing tokens carry only safe characters.
///
/// **Not wired into the shell executor:** the authoritative injection defense
/// is positional parameter binding, which passes dynamic values as `$1`, `$2`,
/// ... so they are never reparsed as shell syntax. This function is a
/// supplementary, defense-in-depth check for callers that want to reject
/// suspicious identity values early.
///
/// Returns `Ok(())` when all identity tokens in the context are safe, or
/// `Err(String)` with a diagnostic naming the offending token and character.
/// Tokens absent from the context are skipped (validated only when present).
///
/// # Errors
/// Returns a human-readable `String` naming the offending token, the unsafe
/// character, and the value when a token carries a disallowed character.
///
/// @plan:PLAN-20260722-ISSUE158-SHELL-SAFE-TOKENS
pub fn validate_shell_safe_tokens(context: &StepContext) -> Result<(), String> {
    for &token in SHELL_SAFE_NUMERIC_TOKENS {
        if let Some(value) = context.get(token) {
            validate_numeric_token(token, value)?;
        }
    }
    for &token in SHELL_SAFE_REFNAME_TOKENS {
        if let Some(value) = context.get(token) {
            validate_refname_token(token, value)?;
        }
    }
    for &token in SHELL_SAFE_PATH_TOKENS {
        if let Some(value) = context.get(token) {
            validate_path_token(token, value)?;
        }
    }
    Ok(())
}

/// Validate a numeric-only token (digits, no leading sign or whitespace).
fn validate_numeric_token(token: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "shell-safe token '{token}' is empty; refusing to interpolate"
        ));
    }
    for ch in value.chars() {
        if !ch.is_ascii_digit() {
            return Err(format!(
                "shell-safe token '{token}' contains non-digit character {ch:?} \
                 (value: {value:?}); refusing to interpolate into shell command"
            ));
        }
    }
    Ok(())
}

/// Validate a git refname token.
fn validate_refname_token(token: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "shell-safe token '{token}' is empty; refusing to interpolate"
        ));
    }
    for ch in value.chars() {
        if !is_safe_refname_char(ch) {
            return Err(format!(
                "shell-safe token '{token}' contains unsafe refname character {ch:?} \
                 (value: {value:?}); refusing to interpolate into shell command"
            ));
        }
    }
    Ok(())
}

/// Whether a character is safe in a git refname interpolated into a shell
/// command.
fn is_safe_refname_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')
}

/// Validate a filesystem path token interpolated into a shell command.
fn validate_path_token(token: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "shell-safe token '{token}' is empty; refusing to interpolate"
        ));
    }
    for ch in value.chars() {
        if is_shell_metacharacter(ch) {
            return Err(format!(
                "shell-safe token '{token}' contains shell metacharacter {ch:?} \
                 (value: {value:?}); refusing to interpolate into shell command"
            ));
        }
    }
    Ok(())
}

/// Whether a character is a shell metacharacter or whitespace that must never
/// appear in a path token interpolated into a shell command.
fn is_shell_metacharacter(ch: char) -> bool {
    matches!(
        ch,
        ';' | '|'
            | '&'
            | '$'
            | '`'
            | '<'
            | '>'
            | '{'
            | '}'
            | '('
            | ')'
            | '\''
            | '"'
            | '\\'
            | '\n'
            | '\r'
            | '\t'
            | ' '
            | '*'
            | '?'
            | '['
            | ']'
            | '!'
            | '^'
            | '~'
    )
}
