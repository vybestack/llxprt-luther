//! Components that speak to GitHub: pull requests, review feedback, PR
//! remediation, and the parent/child orchestration built on them.
//!
//! These were the last domain components inside the engine. With them out,
//! `engine::executors` no longer decides anything about pull requests.
//!
//! `pr_identity_params` stays private: it is shared between the components
//! here and is not part of the package's surface.

pub mod feedback_eval;
pub mod feedback_eval_policy;
pub mod feedback_eval_timeout;
pub mod github_feedback;
pub mod github_pr;
pub mod parent_orchestration;
pub mod pr_check_wait;
pub mod pr_followup_artifacts;
pub mod pr_followup_types;
mod pr_identity_params;
pub mod pr_remediation;
pub mod workflow_auth_preflight;
