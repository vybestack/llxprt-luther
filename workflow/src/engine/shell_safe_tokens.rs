//! Typed validation of identity-bearing tokens interpolated into shell
//! commands.
//!
//! Certain tokens (`issue_number`, `base_branch`, `artifact_dir`, `work_dir`)
//! carry externally-sourced or attacker-influencable values that are
//! interpolated directly into shell command templates via
//! [`crate::engine::executor::interpolate_string`]. Without validation, a
//! hostile GitHub issue number or branch name carrying shell metacharacters
//! (`;`, `` ` ``, `$()`, `|`) could inject arbitrary commands.
//!
//! [`validate_shell_safe_tokens`] provides a typed safety gate: the shell
//! executor calls it **before** interpolation, rejecting any identity token
//! whose value contains a disallowed character. Tokens absent from the context
//! are skipped (validated only when present).
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

/// Validate that identity-bearing tokens interpolated into shell commands
/// carry only safe characters.
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
