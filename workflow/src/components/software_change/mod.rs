//! Components that act on a software change: version control, workspace
//! ownership, the coding agent, and verification.
//!
//! Distinct from `generic` in what they know about, not in what they import.
//! Process execution is generic; running `git`, driving LLxprt, or deciding
//! whether a change is in scope is not.
//!
//! `llxprt_diff` stays private, and `llxprt_timeout` and `llxprt_tests` are
//! declared inside `llxprt.rs` (the latter through a `#[path]` attribute), so
//! neither appears here.

pub mod git_config_publish;
pub mod llxprt;
mod llxprt_diff;
pub mod scope_control;
pub mod verify;
pub mod workspace_ownership;
