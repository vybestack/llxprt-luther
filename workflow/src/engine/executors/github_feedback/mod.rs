//! CodeRabbit feedback collection and remote marker discovery surfaces.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
//! @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-026,REQ-PRFU-034
//! @pseudocode lines 1-49
mod collect;
mod marker_parse;
mod marker_reply;
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
use marker_reply::*;
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
