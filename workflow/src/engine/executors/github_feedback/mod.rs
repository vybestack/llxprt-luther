//! CodeRabbit feedback collection and remote marker discovery surfaces.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
//! @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-026,REQ-PRFU-034
//! @pseudocode lines 1-49
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::{
    is_summary_marker_key, value_has_summary_marker_key, PrFollowupBinding,
    PR_FOLLOWUP_SCHEMA_VERSION,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

mod collect;
mod marker_parse;
mod marker_resolve;
mod pending_actions;
mod readiness;
mod reply_record;
mod report;
mod thread_identity;

#[cfg(test)]
mod tests;

// Submodules share their internal `pub(super)` helpers through the parent
// scope. These plain `use` imports keep those helpers reachable from sibling
// modules (via `use super::*`) without re-exporting them into the crate's
// public API surface.
use collect::*;
use marker_parse::*;
use marker_resolve::*;
use pending_actions::*;
use readiness::*;
use reply_record::*;
use report::*;
use thread_identity::*;

// Only the executor entry points, parser, marker type, and clock are part of
// the module's intended public API; re-export exactly those.
pub use collect::{
    FeedbackMarkerParser, GithubCodeRabbitFeedbackExecutor,
    GithubCodeRabbitFeedbackExecutorWithRunner, GithubFeedbackMarkerExecutor,
    GithubFeedbackMarkerExecutorWithRunner, RemoteFeedbackMarker, SystemFeedbackClock,
};
