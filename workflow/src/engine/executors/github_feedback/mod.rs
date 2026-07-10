//! CodeRabbit feedback collection and remote marker discovery surfaces.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
//! @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-026,REQ-PRFU-034
//! @pseudocode lines 1-49
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
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
pub use collect::*;
mod readiness;
pub use readiness::*;
mod pending_actions;
pub use pending_actions::*;
mod marker_resolve;
pub use marker_resolve::*;
mod thread_identity;
pub use thread_identity::*;
mod reply_record;
pub use reply_record::*;
mod report;
pub use report::*;
mod marker_parse;
pub use marker_parse::*;
